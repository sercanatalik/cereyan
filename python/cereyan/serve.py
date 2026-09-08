"""``cereyan serve``: discover flows and routes under a directory, register
them, and run the server in this process."""

from __future__ import annotations

import importlib
import json
import os
import signal
import sys
import traceback
import webbrowser

from . import _core, apps, engine
from .config import defaults as project_defaults
from .config import email_settings, resource_totals, server_settings
from .rules import dispatch as rule_dispatch
from .rules import register_with_store as register_code_rules
from .exceptions import CereyanError
from .routes import Dispatcher

SKIP_DIRS = {"node_modules", "venv", ".venv", "build", "dist", "tests", "test", "__pycache__", "site-packages"}


def discover_modules(directory: str) -> list[str]:
    """Importable module names for every ``.py`` file under ``directory``."""
    modules: list[str] = []
    for root, dirs, files in os.walk(directory):
        dirs[:] = sorted(
            d for d in dirs if not d.startswith(".") and not d.startswith("_") and d not in SKIP_DIRS
        )
        rel_root = os.path.relpath(root, directory)
        for f in sorted(files):
            if not f.endswith(".py") or f.startswith("_") or f.startswith("test_") or f == "setup.py":
                continue
            stem = f[:-3]
            parts = [] if rel_root == "." else rel_root.split(os.sep)
            modules.append(".".join(parts + [stem]))
    return modules


def import_modules(directory: str, modules: list[str]) -> list[tuple[str, str]]:
    """Import modules with ``directory`` first on sys.path; returns failures."""
    if directory not in sys.path:
        sys.path.insert(0, directory)
    failures: list[tuple[str, str]] = []
    for name in modules:
        try:
            importlib.import_module(name)
        except BaseException:
            failures.append((name, traceback.format_exc()))
    return failures


def resolve_host_port(directory: str, host: str | None, port: int | None, app_host: str | None = None,
                      app_port: int | None = None) -> tuple[str, int]:
    """Flag, environment, app.serve(), cereyan.toml, default."""
    settings = server_settings(directory)
    env_host = os.environ.get("CEREYAN_HOST") or None
    env_port = os.environ.get("CEREYAN_PORT")
    resolved_host = host or env_host or app_host or settings.get("host") or "127.0.0.1"
    if port is not None:
        resolved_port = port
    elif env_port:
        resolved_port = int(env_port)
    elif app_port is not None:
        resolved_port = app_port
    elif "port" in settings:
        resolved_port = int(settings["port"])
    else:
        resolved_port = 4200
    return str(resolved_host), int(resolved_port)


def resolve_token(directory: str, token: str | None = None, app_token: str | None = None) -> str | None:
    """Flag, environment, app.serve(), cereyan.toml. None means unauthenticated."""
    settings = server_settings(directory)
    value = token or os.environ.get("CEREYAN_TOKEN") or app_token or settings.get("token")
    return str(value) if value else None


def resolve_socket(directory: str, socket: str | None = None, app_socket: str | None = None) -> str | None:
    """Flag, environment, app.serve(), cereyan.toml. None means no Unix socket."""
    settings = server_settings(directory)
    value = socket or os.environ.get("CEREYAN_SOCKET") or app_socket or settings.get("socket")
    if not value:
        return None
    if sys.platform.startswith("win"):
        raise CereyanError("Unix sockets are not supported on Windows; use --host and --port")
    return os.path.abspath(os.path.expanduser(str(value)))


def serve(directory: str | None = None, *, host: str | None = None, port: int | None = None,
          max_engines: int | None = None, engine_max_runs: int | None = None, open_browser: bool | None = None,
          discover: bool = True, quiet: bool = False, ready=None, crash_retries: int | None = None,
          token: str | None = None, socket: str | None = None) -> int:
    directory = os.path.abspath(directory or os.getcwd())
    if not os.path.isdir(directory):
        raise CereyanError(f"{directory} is not a directory")
    settings = server_settings(directory)
    resolved_token = resolve_token(directory, token)
    resolved_socket = resolve_socket(directory, socket)
    if discover:
        modules = discover_modules(directory)
        engine.runner.suppress_top_level_runs(True, "cereyan serve is importing modules")
        try:
            for name, tb in import_modules(directory, modules):
                print(f"warning: could not import {name}:\n{tb}", file=sys.stderr)
        finally:
            engine.runner.suppress_top_level_runs(False)
    resolved_host, resolved_port = resolve_host_port(directory, host, port)

    registered = apps.all_apps()
    flows = [f for app in registered for f in app.flows.values()]
    routes = [r for app in registered for r in app.routes]
    # Unknown `after=` upstreams are flow errors, not fatal.
    flow_errors: dict[tuple[str, str], str] = {}
    for f in flows:
        if f.after:
            names = f.after.get("flows") or [f.after["flow"]]
            unknown = [n for n in names if not any(o.project == f.project and o.name == n for o in flows)]
            if unknown:
                flow_errors[(f.project, f.name)] = "unknown upstream flow " + ", ".join(f"'{n}'" for n in unknown)
    if not flows and not routes:
        print(f"warning: no flows or routes found under {directory}", file=sys.stderr)

    try:
        store = engine.get_store()
    except _core.StoreLocked as exc:
        raise CereyanError(f"{exc} Only one cereyan server can run per home directory.") from None
    live_ids = [engine.runner.register_flow(store, f) for f in flows]
    register_code_rules(store)
    for f, fid in zip(flows, live_ids):
        err = flow_errors.get((f.project, f.name))
        if err:
            store.set_flow_error(fid, err)
            print(f"warning: flow {f.project}/{f.name}: {err}", file=sys.stderr)
    toml_defaults = project_defaults(directory)
    # Precedence: flow decorator (server side), then cereyan.toml, then the CLI flag.
    if "crash_retries" in toml_defaults:
        crash_default = int(toml_defaults["crash_retries"])
    elif crash_retries is not None:
        crash_default = int(crash_retries)
    else:
        crash_default = 5

    dispatcher = Dispatcher(routes)
    email = email_settings(directory)
    config = {
        "home": engine.resolved_home(),
        "host": resolved_host,
        "port": resolved_port,
        "served_dir": directory,
        "python": sys.executable,
        "max_engines": int(max_engines or settings.get("max_engines") or (os.cpu_count() or 4)),
        "engine_max_runs": int(engine_max_runs or settings.get("engine_max_runs") or 100),
        "cancel_grace_secs": int(settings.get("cancel_grace_secs", 10)),
        "custom_routes": dispatcher.specs(),
        "live_flows": live_ids,
        "version": _core.__version__,
        "resources": resource_totals(directory),
        "crash_retries_default": crash_default,
        "fast_crash_rerun": bool(os.environ.get("CEREYAN_FAST_CRASH_RERUN")),
        "email": email,
        "retain_days": int(toml_defaults.get("retain_days", 30)),
        "catchup_default": str(toml_defaults.get("catchup", "skip")),
        "retention_interval_secs": int(os.environ["CEREYAN_RETENTION_INTERVAL"]) if os.environ.get("CEREYAN_RETENTION_INTERVAL") else None,
        "token": resolved_token,
        "socket": resolved_socket,
    }
    if "max_engines" in toml_defaults and max_engines is None and "max_engines" not in settings:
        config["max_engines"] = int(toml_defaults["max_engines"])
    if "engine_max_runs" in toml_defaults and engine_max_runs is None and "engine_max_runs" not in settings:
        config["engine_max_runs"] = int(toml_defaults["engine_max_runs"])
    try:
        server = _core.Server.start(store, json.dumps(config), dispatcher if routes else None, rule_dispatch)
    except RuntimeError as exc:
        engine.close_store()
        raise CereyanError(str(exc)) from None

    if not quiet:
        print(f"cereyan serving {len(flows)} flow(s) from {directory} at {server.url}", file=sys.stderr)
    should_open = open_browser if open_browser is not None else settings.get("open_browser", True)
    if should_open and not os.environ.get("CEREYAN_NO_BROWSER"):
        try:
            webbrowser.open(server.url)
        except Exception:
            pass
    if ready is not None:
        ready(server)

    def _terminate(signum, frame):
        raise KeyboardInterrupt

    previous = signal.signal(signal.SIGTERM, _terminate)
    try:
        while not server.wait(0.5):
            pass
    except KeyboardInterrupt:
        if not quiet:
            print("cereyan: shutting down", file=sys.stderr)
    finally:
        signal.signal(signal.SIGTERM, previous)
        server.stop()
        engine.close_store()
    return 0
