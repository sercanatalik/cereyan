"""Custom HTTP routes: FastAPI-shaped sync handlers dispatched from Rust."""

from __future__ import annotations

import asyncio
import concurrent.futures
import dataclasses
import inspect
import json
import sys
import threading
import traceback
import typing
from datetime import date, datetime
from typing import Any, Callable, get_type_hints
from urllib.parse import parse_qs

from . import params as _params
from .exceptions import CereyanError


class HTTPError(CereyanError):
    """Raise inside a handler to send a status with a JSON error body."""

    def __init__(self, status: int, message: str = "") -> None:
        self.status = status
        self.message = message or f"HTTP {status}"
        super().__init__(self.message)


class Request:
    """The raw request handed to a handler parameter annotated ``Request``."""

    def __init__(self, method: str, path: str, path_params: dict[str, str], query: dict[str, list[str]],
                 headers: dict[str, str], body: bytes) -> None:
        self.method = method
        self.path = path
        self.path_params = path_params
        self.query = query
        self.headers = headers
        self.body = body

    def json(self) -> Any:
        """The request body decoded as JSON, or ``None`` when empty."""
        if not self.body:
            return None
        return json.loads(self.body.decode("utf-8"))

    def text(self) -> str:
        """The request body decoded as UTF-8 text."""
        return self.body.decode("utf-8")


class Response:
    """An explicit response: body bytes or str, status, headers."""

    def __init__(self, body: bytes | str = b"", status: int = 200, headers: dict[str, str] | None = None,
                 media_type: str | None = None) -> None:
        self.body = body.encode("utf-8") if isinstance(body, str) else bytes(body)
        self.status = status
        self.headers = dict(headers or {})
        if media_type and "content-type" not in {k.lower() for k in self.headers}:
            self.headers["content-type"] = media_type


@dataclasses.dataclass
class Route:
    """A custom route registered on an App: method, path template, handler, and owning App."""
    method: str
    path: str
    handler: Callable
    app: Any


def _is_body_type(hint: Any) -> bool:
    if hint is Any or hint is inspect.Parameter.empty:
        return False
    if dataclasses.is_dataclass(hint) and inspect.isclass(hint):
        return True
    if typing.is_typeddict(hint):
        return True
    try:
        import pydantic  # type: ignore

        return inspect.isclass(hint) and issubclass(hint, pydantic.BaseModel)
    except ImportError:
        return False


def _bind_body(hint: Any, data: Any, name: str) -> Any:
    if dataclasses.is_dataclass(hint) and inspect.isclass(hint):
        return _params.coerce(data, hint, name)
    if typing.is_typeddict(hint):
        if not isinstance(data, dict):
            raise HTTPError(422, f"parameter {name!r} expects a JSON object")
        hints = get_type_hints(hint)
        out = {}
        for key, sub in hints.items():
            if key in data:
                out[key] = _params.coerce(data[key], sub, f"{name}.{key}")
        missing = set(getattr(hint, "__required_keys__", set())) - set(out)
        if missing:
            raise HTTPError(422, f"parameter {name!r} is missing {sorted(missing)}")
        return out
    try:
        import pydantic  # type: ignore

        if inspect.isclass(hint) and issubclass(hint, pydantic.BaseModel):
            try:
                return hint.model_validate(data)
            except Exception as exc:  # pydantic.ValidationError
                raise HTTPError(422, f"parameter {name!r}: {exc}") from None
    except ImportError:
        pass
    return data


_SCALARS = (str, int, float, bool, date, datetime)


def bind_arguments(route: Route, request: Request) -> dict[str, Any]:
    """Build the handler's keyword arguments from a request.

    Path and query parameters are matched by name and coerced through the handler's
    type hints; a parameter annotated with a dataclass, TypedDict, or pydantic model
    receives the JSON body; a parameter annotated ``Request`` receives the raw request.

    Raises:
        HTTPError: 422 when a required parameter is missing or does not coerce.
    """
    sig = inspect.signature(route.handler)
    try:
        hints = get_type_hints(route.handler)
    except Exception:
        hints = {}
    kwargs: dict[str, Any] = {}
    body_json: Any = ...
    for p in sig.parameters.values():
        hint = hints.get(p.name, p.annotation)
        if hint is Request or (inspect.isclass(hint) and issubclass(hint, Request)):
            kwargs[p.name] = request
            continue
        if _is_body_type(hint):
            if body_json is ...:
                try:
                    body_json = request.json()
                except ValueError:
                    raise HTTPError(422, "request body is not valid JSON") from None
            if body_json is None:
                if p.default is not inspect.Parameter.empty:
                    kwargs[p.name] = p.default
                    continue
                raise HTTPError(422, f"parameter {p.name!r} requires a JSON body")
            kwargs[p.name] = _bind_body(hint, body_json, p.name)
            continue
        raw: Any = ...
        if p.name in request.path_params:
            raw = request.path_params[p.name]
        elif p.name in request.query:
            values = request.query[p.name]
            origin = typing.get_origin(hint)
            raw = values if origin is list else values[-1]
        if raw is ...:
            if p.default is not inspect.Parameter.empty:
                kwargs[p.name] = p.default
                continue
            raise HTTPError(422, f"missing parameter {p.name!r}")
        if hint is inspect.Parameter.empty:
            kwargs[p.name] = raw
            continue
        try:
            kwargs[p.name] = _params.coerce(raw, hint, p.name)
        except _params.ParameterError as exc:
            raise HTTPError(422, str(exc)) from None
    return kwargs


def to_response(value: Any) -> tuple[int, list[tuple[str, str]], bytes]:
    """Turn a handler's return value into ``(status, headers, body)``.

    Accepts a `Response`, ``(value, status)``, a dict or list (JSON), a str
    (text/plain), bytes (octet-stream), or ``None`` (JSON ``null``).
    """
    status = 200
    if isinstance(value, tuple) and len(value) == 2 and isinstance(value[1], int):
        value, status = value
    if isinstance(value, Response):
        headers = list(value.headers.items())
        if not any(k.lower() == "content-type" for k, _ in headers):
            headers.append(("content-type", "application/octet-stream"))
        return value.status, headers, value.body
    if value is None:
        return status, [("content-type", "application/json")], b"null"
    if isinstance(value, (dict, list)):
        body = json.dumps(_params.to_json_value(value)).encode("utf-8")
        return status, [("content-type", "application/json")], body
    if isinstance(value, str):
        return status, [("content-type", "text/plain; charset=utf-8")], value.encode("utf-8")
    if isinstance(value, (bytes, bytearray)):
        return status, [("content-type", "application/octet-stream")], bytes(value)
    body = json.dumps(_params.to_json_value(value)).encode("utf-8")
    return status, [("content-type", "application/json")], body


def _error(status: int, message: str) -> tuple[int, list[tuple[str, str]], bytes]:
    return status, [("content-type", "application/json")], json.dumps({"error": message}).encode("utf-8")


class _Loop:
    """One asyncio event loop on a daemon thread, started on first use."""

    def __init__(self) -> None:
        self._loop = None
        self._lock = threading.Lock()

    def get(self):
        with self._lock:
            if self._loop is None:
                loop = asyncio.new_event_loop()
                thread = threading.Thread(target=loop.run_forever, name="cereyan-routes-loop", daemon=True)
                thread.start()
                self._loop = loop
            return self._loop


class Dispatcher:
    """Called from Rust with the raw request; returns (status, headers, body).

    ``async def`` handlers run on one shared event loop (see ``_Loop``); a
    blocking call inside one blocks every other async handler, so keep them
    non-blocking or use a sync handler instead.
    """

    #: Seconds an async handler may take before the route answers 504.
    async_timeout = 30.0

    def __init__(self, routes: list[Route]) -> None:
        self.routes = routes
        self._loop = _Loop()

    def specs(self) -> list[dict[str, Any]]:
        """The route table handed to the Rust router at start: id, method, path, and handler source."""
        return [
            {"id": i, "method": r.method, "path": r.path, "source": f"{r.handler.__module__}:{r.handler.__qualname__}"}
            for i, r in enumerate(self.routes)
        ]

    def __call__(self, route_id: int, method: str, path: str, path_params: list[tuple[str, str]],
                 query: str, headers: list[tuple[str, str]], body: bytes):
        try:
            route = self.routes[route_id]
        except IndexError:
            return _error(404, "route not found")
        request = Request(
            method=method,
            path=path,
            path_params=dict(path_params),
            query=parse_qs(query, keep_blank_values=True),
            headers={k.lower(): v for k, v in headers},
            body=bytes(body),
        )
        try:
            kwargs = bind_arguments(route, request)
            if inspect.iscoroutinefunction(route.handler):
                future = asyncio.run_coroutine_threadsafe(route.handler(**kwargs), self._loop.get())
                try:
                    result = future.result(timeout=self.async_timeout)
                except concurrent.futures.TimeoutError:
                    future.cancel()
                    return _error(504, "route handler timed out")
            else:
                result = route.handler(**kwargs)
            return to_response(result)
        except HTTPError as exc:
            return _error(exc.status, exc.message)
        except Exception:
            print(f"error in route {route.method} {route.path}:\n{traceback.format_exc()}", file=sys.stderr)
            return _error(500, "internal error in route handler")
