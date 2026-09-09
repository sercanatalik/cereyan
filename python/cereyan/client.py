"""Thin HTTP client for a running cereyan server, used by the CLI, by
custom route handlers, and by the offline handoff. Standard library only."""

from __future__ import annotations

import http.client
import json
import os
import socket
import urllib.error
import urllib.parse
import urllib.request
from typing import Any

from . import _core
from .exceptions import CereyanError


class ServerUnavailable(CereyanError):
    """No live server was found for the home directory."""


class ApiError(CereyanError):
    """The server answered with an error status; ``status`` and the decoded ``body`` are kept."""
    def __init__(self, status: int, body: Any) -> None:
        self.status = status
        self.body = body
        message = body.get("error") if isinstance(body, dict) else str(body)
        super().__init__(f"server returned {status}: {message}")


def discovery_file(home: str | None = None) -> str:
    """Path of ``server.json`` in ``home`` (default: the resolved runtime home)."""
    return os.path.join(home or _core.Store.resolve_home(None), "server.json")


def read_discovery(home: str | None = None) -> dict | None:
    """The parsed ``server.json`` of ``home``, or ``None`` when no server has written one."""
    path = discovery_file(home)
    try:
        with open(path, encoding="utf-8") as fh:
            return json.load(fh)
    except (OSError, ValueError):
        return None


def resolve_client_token(token: str | None = None) -> str | None:
    """The token a client sends: an explicit value, else ``CEREYAN_TOKEN``."""
    return token or os.environ.get("CEREYAN_TOKEN") or None


class _UnixConnection(http.client.HTTPConnection):
    """HTTP/1.1 over a Unix domain socket."""

    def __init__(self, path: str, timeout: float) -> None:
        super().__init__("localhost", timeout=timeout)
        self._path = path

    def connect(self) -> None:
        sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        sock.settimeout(self.timeout)
        sock.connect(self._path)
        self.sock = sock


class Client:
    """HTTP client for a running server, on the standard library only.

    Args:
        base_url: The server URL, for example ``http://127.0.0.1:4200``.
        timeout: Seconds to wait for a response.
        token: API token; defaults to ``CEREYAN_TOKEN``.
        socket_path: Connect over this Unix socket instead of TCP.

    Most methods return the decoded JSON of the matching endpoint (see the HTTP API
    reference) and raise `ApiError` on an error status or
    `ServerUnavailable` when the server cannot be reached.
    """
    def __init__(self, base_url: str = "", timeout: float = 30.0, token: str | None = None,
                 socket_path: str | None = None) -> None:
        self.base_url = base_url.rstrip("/")
        self.timeout = timeout
        self.token = resolve_client_token(token)
        self.socket_path = socket_path

    def _request(self, method: str, path: str, body: Any = None, params: dict | None = None) -> Any:
        target = path
        if params:
            clean = {k: v for k, v in params.items() if v is not None and v != ""}
            if clean:
                target += "?" + urllib.parse.urlencode(clean, doseq=True)
        data = None
        headers = {"accept": "application/json"}
        if body is not None:
            data = json.dumps(body).encode("utf-8")
            headers["content-type"] = "application/json"
        if self.token:
            headers["authorization"] = f"Bearer {self.token}"
        if self.socket_path:
            return self._request_unix(method, target, data, headers)
        req = urllib.request.Request(self.base_url + target, data=data, method=method, headers=headers)
        try:
            with urllib.request.urlopen(req, timeout=self.timeout) as resp:
                raw = resp.read()
                if not raw:
                    return None
                return json.loads(raw.decode("utf-8"))
        except urllib.error.HTTPError as exc:
            raw = exc.read()
            try:
                payload = json.loads(raw.decode("utf-8")) if raw else {}
            except ValueError:
                payload = {"error": raw.decode("utf-8", "replace")}
            raise ApiError(exc.code, payload) from None
        except urllib.error.URLError as exc:
            raise ServerUnavailable(f"cannot reach {self.base_url}: {exc.reason}") from None

    def _request_unix(self, method: str, target: str, data: bytes | None, headers: dict) -> Any:
        conn = _UnixConnection(self.socket_path, self.timeout)
        try:
            conn.request(method, target, body=data, headers=headers)
            resp = conn.getresponse()
            raw = resp.read()
        except OSError as exc:
            raise ServerUnavailable(f"cannot reach unix socket {self.socket_path}: {exc}") from None
        finally:
            conn.close()
        if resp.status >= 400:
            try:
                payload = json.loads(raw.decode("utf-8")) if raw else {}
            except ValueError:
                payload = {"error": raw.decode("utf-8", "replace")}
            raise ApiError(resp.status, payload)
        return json.loads(raw.decode("utf-8")) if raw else None

    # -- endpoints --------------------------------------------------------

    def health(self) -> bool:
        """``True`` when ``GET /api/health`` answers ``ok``; never raises."""
        try:
            return bool(self._request("GET", "/api/health").get("ok"))
        except CereyanError:
            return False

    def server(self) -> dict:
        """``GET /api/server``: version, home, engines, auth state, and listeners."""
        return self._request("GET", "/api/server")

    def flows(self, project: str | None = None) -> list[dict]:
        """``GET /api/flows``, optionally filtered by ``project``."""
        return self._request("GET", "/api/flows", params={"project": project})

    def flow(self, flow_id: int) -> dict:
        """``GET /api/flows/{id}``: one flow with its schedules and summary."""
        return self._request("GET", f"/api/flows/{flow_id}")

    def runs(self, **filters: Any) -> dict:
        """``GET /api/runs`` with the given filters (``flow``, ``project``, ``state_type``, ``tags``, ``limit``, ``cursor``, ...); returns the page."""
        return self._request("GET", "/api/runs", params=filters)

    def get_run(self, run_id: int) -> dict:
        """``GET /api/runs/{id}``: one run with its state and parameters."""
        return self._request("GET", f"/api/runs/{run_id}")

    def task_runs(self, run_id: int) -> list[dict]:
        """``GET /api/runs/{id}/tasks``: the run's task runs."""
        return self._request("GET", f"/api/runs/{run_id}/tasks")

    def logs(self, run_id: int, after: int = 0, level: str | None = None, search: str | None = None,
             limit: int = 1000) -> dict:
        """``GET /api/runs/{id}/logs`` after sequence ``after``, optionally filtered by ``level`` and ``search``."""
        return self._request(
            "GET", f"/api/runs/{run_id}/logs", params={"after": after, "level": level, "search": search, "limit": limit}
        )

    def resume(self, run_id: int, input: Any) -> dict:
        """``POST /api/runs/{id}/resume``: answer a Paused run with ``input`` and schedule its next attempt."""
        return self._request("POST", f"/api/runs/{run_id}/resume", body={"input": input})

    def cancel(self, run_id: int) -> dict:
        """``POST /api/runs/{id}/cancel``: cancel a queued run at once or ask a running one to stop."""
        return self._request("POST", f"/api/runs/{run_id}/cancel")

    def delete_run(self, run_id: int) -> None:
        """``DELETE /api/runs/{id}``: remove a finished run and its task runs, logs, and artifacts."""
        self._request("DELETE", f"/api/runs/{run_id}")

    def counts(self, project: str | None = None) -> dict:
        """``GET /api/counts``: run counts by state for the dashboard, optionally per ``project``."""
        return self._request("GET", "/api/counts", params={"project": project})

    def backfill(self, flow_id: int, parameter: str, start: str, end: str, *, interval: Any = "1d",
                 concurrency: int = 1, extra_parameters: dict | None = None, reverse: bool = False) -> dict:
        """``POST /api/flows/{id}/backfill``: create one run per step of ``parameter`` from ``start`` to ``end``.

        Args:
            flow_id: The flow's id.
            parameter: The date or datetime parameter to step.
            start: First value, ISO formatted.
            end: Last value, inclusive.
            interval: Step as seconds or a duration such as ``"1d"`` or ``"12h"``.
            concurrency: How many of the backfill's runs may execute at once.
            extra_parameters: Fixed values for other parameters.
            reverse: Create the newest value first.

        Returns:
            The backfill status with its id, tag, and counts.
        """
        return self._request(
            "POST",
            f"/api/flows/{flow_id}/backfill",
            body={"parameter": parameter, "start": start, "end": end, "interval": interval, "concurrency": concurrency,
                  "extra_parameters": extra_parameters or {}, "reverse": reverse},
        )

    def backfill_status(self, backfill_id: int) -> dict:
        """``GET /api/backfills/{id}``: counts of the backfill's runs by state."""
        return self._request("GET", f"/api/backfills/{backfill_id}")

    def cancel_backfill(self, backfill_id: int) -> dict:
        """``POST /api/backfills/{id}/cancel``: cancel the backfill's remaining runs."""
        return self._request("POST", f"/api/backfills/{backfill_id}/cancel")

    def schedules(self, flow_id: int) -> list[dict]:
        """``GET /api/flows/{id}/schedules``: the flow's schedules with their next fire times."""
        return self._request("GET", f"/api/flows/{flow_id}/schedules")

    def upcoming(self, flow_id: int) -> list[dict]:
        """``GET /api/flows/{id}/upcoming``: the runs materialised ahead for the flow."""
        return self._request("GET", f"/api/flows/{flow_id}/upcoming")

    def settings(self) -> dict:
        """``GET /api/settings``: resource totals, retention, and defaults."""
        return self._request("GET", "/api/settings")

    def events(self, kind: str | None = None, after: int = 0, limit: int = 100, **filters: Any) -> list[dict]:
        """``GET /api/events`` newest first, filtered by name prefix ``kind`` and other filters; with ``after`` returns events following that sequence number in ascending order."""
        params = {"name": kind, "limit": limit, **filters}
        if after:
            params["cursor"] = after
            params["ascending"] = "true"
        return self._request("GET", "/api/events", params=params)["items"]

    def rules(self) -> list[dict]:
        """``GET /api/rules``: every rule with its match clause, actions, guards, and fire counts."""
        return self._request("GET", "/api/rules")

    def create_rule(self, body: dict) -> dict:
        """``POST /api/rules``: create a data rule from a rule body."""
        return self._request("POST", "/api/rules", body=body)

    def variables(self) -> list[dict]:
        """``GET /api/variables``: every variable with secrets masked."""
        return self._request("GET", "/api/variables")

    def artifacts(self, run_id: int) -> list[dict]:
        """``GET /api/runs/{id}/artifacts``: the run's artifacts."""
        return self._request("GET", f"/api/runs/{run_id}/artifacts")

    def submit(self, project: str, flow: str, parameters: dict | None = None, *, name: str | None = None,
               tags: list[str] | None = None, module: str | None = None, source_dir: str | None = None,
               description: str | None = None, parameter_schema: dict | None = None,
               options: dict | None = None, flow_tags: list[str] | None = None,
               flow_group: str | None = None, created_by: str = "client") -> dict:
        """``POST /api/runs``: create a run with explicit project and flow, registering the flow when needed.

        This is the low-level call used by the offline handoff and by `run`; pass
        ``module`` and ``source_dir`` so the server can import a flow it has not seen.
        """
        body: dict[str, Any] = {
            "project": project,
            "flow": flow,
            "parameters": parameters or {},
            "name": name,
            "tags": tags or [],
            "created_by": created_by,
        }
        if module is not None:
            body["module"] = module
        if source_dir is not None:
            body["source_dir"] = source_dir
        if description is not None:
            body["description"] = description
        if parameter_schema is not None:
            body["parameter_schema"] = parameter_schema
        if options is not None:
            body["options"] = options
        if flow_tags:
            body["flow_tags"] = flow_tags
        if flow_group is not None:
            body["flow_group"] = flow_group
        return self._request("POST", "/api/runs", body=body)

    def run(self, flow: str, project: str | None = None, *, name: str | None = None,
            tags: list[str] | None = None, **parameters: Any) -> dict:
        """Create a run of a registered flow by name. Returns the run."""
        from .params import to_json_value

        if project is None:
            matches = [f for f in self.flows() if f["name"] == flow]
            if not matches:
                raise CereyanError(f"no flow named {flow!r} is registered on the server")
            if len(matches) > 1:
                projects = sorted(m["project"] for m in matches)
                raise CereyanError(f"flow {flow!r} exists in several projects {projects}; pass project=")
            project = matches[0]["project"]
        return self.submit(project, flow, to_json_value(parameters), name=name, tags=tags)


class AuthRequired(CereyanError):
    """The server requires a token the client does not have (or a wrong one)."""


def default_client(home: str | None = None, token: str | None = None) -> Client:
    """A client for the server recorded in ``server.json``; raises when none is live.

    Raises ``AuthRequired`` when the server needs a token this client cannot
    present, so callers never fall back to opening the store by mistake.
    """
    info = read_discovery(home)
    if not info:
        raise ServerUnavailable("no cereyan server is running (no server.json in the home directory)")
    url = info.get("url") or f"http://{info['host']}:{info['port']}"
    socket_path = info.get("socket")
    resolved = resolve_client_token(token)
    # Prefer the socket when the server needs a token we do not have.
    use_socket = bool(socket_path) and not resolved and os.path.exists(socket_path)
    client = Client(url, token=token, socket_path=socket_path if use_socket else None)
    if not client.health():
        raise ServerUnavailable(f"the server recorded in server.json ({client.base_url}) is not responding")
    if info.get("auth") and not use_socket:
        try:
            client.server()
        except ApiError as exc:
            if exc.status == 401:
                raise AuthRequired(
                    "the running server requires an API token: set CEREYAN_TOKEN or pass --token"
                    if not client.token else "the server rejected the API token"
                ) from None
            raise
    return client


def find_server(home: str | None = None) -> Client | None:
    """A client for the live server of ``home``, or ``None`` when there is none; raises ``AuthRequired`` when a token is needed."""
    try:
        return default_client(home)
    except ServerUnavailable:
        return None


# Module-level convenience API for route handlers and scripts.


def run(flow: str, project: str | None = None, **parameters: Any) -> dict:
    """Create a run of a registered flow on the live server and return it.

    Args:
        flow: Flow name.
        project: Project name; required when the name exists in several projects.
        **parameters: Flow parameters.

    Raises:
        ServerUnavailable: When no server is running for the home.
        CereyanError: When the flow is unknown or ambiguous.
    """
    return default_client().run(flow, project, **parameters)


def get_run(run_id: int) -> dict:
    """``GET /api/runs/{id}`` on the live server."""
    return default_client().get_run(run_id)


def list_runs(**filters: Any) -> list[dict]:
    """The items of ``GET /api/runs`` on the live server with the given filters."""
    return default_client().runs(**filters)["items"]


def list_flows(project: str | None = None) -> list[dict]:
    """``GET /api/flows`` on the live server, optionally filtered by ``project``."""
    return default_client().flows(project)


def cancel(run_id: int) -> dict:
    """``POST /api/runs/{id}/cancel`` on the live server."""
    return default_client().cancel(run_id)
