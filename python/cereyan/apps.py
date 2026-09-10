"""The App: registry of flows, custom routes, and the project name."""

from __future__ import annotations

import os
import re
import sys
from typing import TYPE_CHECKING, Callable

from .exceptions import FlowRegistrationError

if TYPE_CHECKING:
    from .flows import Flow
    from .routes import Route

NAME_PATTERN = re.compile(r"^[a-z0-9][a-z0-9_-]*$")


def _caller_file(depth: int = 2) -> str | None:
    frame = sys._getframe(depth)
    while frame is not None:
        file = frame.f_globals.get("__file__")
        module = frame.f_globals.get("__name__", "")
        if file and not module.startswith("cereyan"):
            return file
        frame = frame.f_back
    return None


def sanitize_name(text: str) -> str:
    """Turn an arbitrary directory name into a valid project name."""
    cleaned = re.sub(r"[^a-z0-9_-]+", "-", text.lower()).strip("-")
    if not cleaned or not NAME_PATTERN.match(cleaned):
        return "default"
    return cleaned


def project_name_for_file(file: str | None) -> str:
    """The default project name: basename of the directory holding ``file``."""
    if file:
        directory = os.path.dirname(os.path.abspath(file))
    else:
        directory = os.getcwd()
    return sanitize_name(os.path.basename(directory) or "default")


_all_apps: list[App] = []


class App:
    """Registry of flows and routes. Its name is the project every flow belongs to."""

    def __init__(self, name: str | None = None, *, source_file: str | None = None) -> None:
        if name is None:
            name = project_name_for_file(source_file or _caller_file())
        elif not isinstance(name, str) or not NAME_PATTERN.match(name):
            raise ValueError(
                f"invalid App name {name!r}: must match {NAME_PATTERN.pattern}"
            )
        self.name = name
        self.flows: dict[str, Flow] = {}
        self.routes: list[Route] = []
        self.rules: list = []
        self.source_file = source_file or _caller_file()
        _all_apps.append(self)

    def __repr__(self) -> str:
        return f"App({self.name!r}, flows={sorted(self.flows)})"

    # -- registration -----------------------------------------------------

    def register(self, flow: Flow) -> None:
        """Add a flow to this App; raises ``FlowRegistrationError`` when another flow of the same name is registered."""
        existing = self.flows.get(flow.name)
        if existing is not None and existing is not flow:
            raise FlowRegistrationError(
                f"flow {flow.name!r} is already registered in project {self.name!r} "
                f"at {existing.source_location}; second definition at {flow.source_location}"
            )
        self.flows[flow.name] = flow
        flow.app = self

    def flow(self, fn: Callable | None = None, **options):
        """Decorator equivalent to ``@flow`` bound to this App."""
        from .flows import flow as flow_decorator

        return flow_decorator(fn, app=self, **options)

    def task(self, fn: Callable | None = None, **options):
        """Decorator equivalent to ``@task``; tasks are not tied to an App, this exists for symmetry."""
        from .tasks import task as task_decorator

        return task_decorator(fn, **options)

    # -- custom routes ----------------------------------------------------

    def route(self, method: str, path: str):
        """Register a custom HTTP route handled by the decorated function.

        Args:
            method: HTTP method.
            path: Path template with ``{name}`` placeholders, for example
                ``"/api/ext/orders/{id}"``.
        """
        from .routes import Route

        def decorate(fn: Callable) -> Callable:
            self.routes.append(Route(method.upper(), path, fn, self))
            return fn

        return decorate

    def get(self, path: str):
        """Register a ``GET`` route; see `route`."""
        return self.route("GET", path)

    def post(self, path: str):
        """Register a ``POST`` route; see `route`."""
        return self.route("POST", path)

    def put(self, path: str):
        """Register a ``PUT`` route; see `route`."""
        return self.route("PUT", path)

    def delete(self, path: str):
        """Register a ``DELETE`` route; see `route`."""
        return self.route("DELETE", path)

    def patch(self, path: str):
        """Register a ``PATCH`` route; see `route`."""
        return self.route("PATCH", path)

    # -- rules ------------------------------------------------------------

    def rule(self, on=None, flow=None, tags=None, states=None, unless=None, within=None, at=None, tz=None, **guards):
        """Register a code rule whose action calls the decorated ``fn(event, run)``.

        A reactive rule fires when an event matching ``on`` happens. A
        proactive rule fires when the event named by ``unless`` does *not*
        happen: either ``within`` seconds of the ``on`` event (event-armed),
        or by each tick of the cron expression ``at`` in timezone ``tz``
        (clock-armed, no ``on``).

        Event and state names are checked here, so a rule that could never fire
        raises at import rather than sitting silent. Use the constants in
        `cereyan.events` and `cereyan.states` to get them right the first time;
        a custom event name outside the reserved prefixes is always accepted.
        A value in ``states`` matches the run's state type or its sub-state
        name, so ``states=["Scheduled"]`` also covers Late and AwaitingRetry.

        See `cereyan.rules.register` for the full argument list.
        """
        from .rules import register

        def decorate(fn: Callable) -> Callable:
            register(self, fn, on, flow=flow, tags=tags, states=states, unless=unless, within=within, at=at, tz=tz, **guards)
            return fn

        return decorate

    # -- serving ----------------------------------------------------------

    def serve(self, host: str | None = None, port: int | None = None, **options) -> int:
        """Serve the flows and routes registered so far, blocking until stopped.

        Host and port given here rank below the CLI flags and environment and
        above ``cereyan.toml``.
        """
        from .serve import serve

        directory = os.path.dirname(os.path.abspath(self.source_file)) if self.source_file else os.getcwd()
        return serve(directory, host=host, port=port, discover=False, **options)


def all_apps() -> list[App]:
    return list(_all_apps)


_default_app: App | None = None


def get_default_app(source_file: str | None = None) -> App:
    """The default App, created on first use and named after the directory of
    the module that first needed it."""
    global _default_app
    if _default_app is None:
        _default_app = App(source_file=source_file)
    return _default_app


def reset_default_app() -> None:
    """Testing helper: forget the default App and every registered App."""
    global _default_app
    _default_app = None
    _all_apps.clear()
