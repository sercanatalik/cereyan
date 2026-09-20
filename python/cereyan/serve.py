"""``cereyan serve``: discover flows and routes under a directory, register
them, and run the server in this process."""

from __future__ import annotations

import importlib
import ipaddress
import json
import os
import platform
import re
import signal
import sys
import threading
import traceback
import webbrowser
from urllib.parse import urlsplit

from . import _core, apps, engine
from .config import defaults as project_defaults
from .config import email_settings, resource_totals, server_settings, ui_title
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


def _source(kind: str, name: str | None = None) -> dict:
    """Where a setting's value came from: flag, env, app, toml, or default."""
    return {"source": kind, "name": name}


def _describe(source: dict) -> str:
    """A source as error messages name it."""
    return f"{source['name']} in cereyan.toml" if source["source"] == "toml" else str(source["name"])


def _first(candidates: tuple, default=None) -> tuple:
    """The first ``(value, kind, name)`` candidate whose value is set, with its source."""
    for value, kind, name in candidates:
        if value is not None:
            return value, _source(kind, name)
    return default, _source("default")


def _host_port(directory: str, host: str | None, port: int | None, app_host: str | None = None,
               app_port: int | None = None) -> tuple[str, int, dict, dict]:
    settings = server_settings(directory)
    resolved_host, host_source = _first((
        (host or None, "flag", "--host"),
        (os.environ.get("CEREYAN_HOST") or None, "env", "CEREYAN_HOST"),
        (app_host or None, "app", "app.serve(host=)"),
        (settings.get("host") or None, "toml", "[server] host"),
    ), "127.0.0.1")
    resolved_port, port_source = _first((
        (port, "flag", "--port"),
        (os.environ.get("CEREYAN_PORT") or None, "env", "CEREYAN_PORT"),
        (app_port, "app", "app.serve(port=)"),
        (settings.get("port"), "toml", "[server] port"),
    ), 4200)
    return str(resolved_host), int(resolved_port), host_source, port_source


def resolve_host_port(directory: str, host: str | None, port: int | None, app_host: str | None = None,
                      app_port: int | None = None) -> tuple[str, int]:
    """Flag, environment, app.serve(), cereyan.toml, default."""
    resolved_host, resolved_port, _, _ = _host_port(directory, host, port, app_host, app_port)
    return resolved_host, resolved_port


def _token(directory: str, token: str | None = None, app_token: str | None = None) -> tuple[str | None, dict]:
    settings = server_settings(directory)
    value, source = _first((
        (token or None, "flag", "--token"),
        (os.environ.get("CEREYAN_TOKEN") or None, "env", "CEREYAN_TOKEN"),
        (app_token or None, "app", "app.serve(token=)"),
        (settings.get("token") or None, "toml", "[server] token"),
    ))
    return (str(value) if value else None), source


def resolve_token(directory: str, token: str | None = None, app_token: str | None = None) -> str | None:
    """Flag, environment, app.serve(), cereyan.toml. None means unauthenticated."""
    return _token(directory, token, app_token)[0]


def _socket(directory: str, socket: str | None = None, app_socket: str | None = None) -> tuple[str | None, dict]:
    settings = server_settings(directory)
    value, source = _first((
        (socket or None, "flag", "--socket"),
        (os.environ.get("CEREYAN_SOCKET") or None, "env", "CEREYAN_SOCKET"),
        (app_socket or None, "app", "app.serve(socket=)"),
        (settings.get("socket") or None, "toml", "[server] socket"),
    ))
    if not value:
        return None, source
    if sys.platform.startswith("win"):
        raise CereyanError("Unix sockets are not supported on Windows; use --host and --port")
    return os.path.abspath(os.path.expanduser(str(value))), source


def resolve_socket(directory: str, socket: str | None = None, app_socket: str | None = None) -> str | None:
    """Flag, environment, app.serve(), cereyan.toml. None means no Unix socket."""
    return _socket(directory, socket, app_socket)[0]


_BASE_SEGMENT = re.compile(r"[A-Za-z0-9._~-]+")


def normalize_base_path(value: str, source: str) -> str:
    """``""`` for the root, otherwise ``/a/b`` with no trailing slash."""
    stripped = value.strip("/")
    if not stripped:
        return ""
    for segment in stripped.split("/"):
        if segment in (".", "..") or not _BASE_SEGMENT.fullmatch(segment):
            raise CereyanError(
                f"invalid base path {value!r} from {source}: each segment must be letters, digits, "
                "'-', '_', '.', or '~', and not '.' or '..'"
            )
    return "/" + stripped


def _base_path(directory: str, base_path: str | None = None, app_base_path: str | None = None) -> tuple[str, dict]:
    settings = server_settings(directory)
    value, source = _first((
        (base_path, "flag", "--base-path"),
        (os.environ.get("CEREYAN_BASE_PATH") or None, "env", "CEREYAN_BASE_PATH"),
        (app_base_path, "app", "app.serve(base_path=)"),
        (settings.get("base_path"), "toml", "[server] base_path"),
    ))
    if value is None:
        return "", source
    if not isinstance(value, str):
        raise CereyanError(f"invalid base path {value!r} from {_describe(source)}: expected a string")
    return normalize_base_path(value, _describe(source)), source


def resolve_base_path(directory: str, base_path: str | None = None, app_base_path: str | None = None) -> str:
    """Flag, environment, app.serve(), cereyan.toml. ``""`` means the root."""
    return _base_path(directory, base_path, app_base_path)[0]


_TRUE = {"true", "1", "yes"}
_FALSE = {"false", "0", "no"}


def _enable_auth(directory: str, enable_auth: bool | None = None,
                 app_enable_auth: bool | None = None) -> tuple[bool, dict]:
    if enable_auth:
        return True, _source("flag", "--enable-auth")
    env = os.environ.get("CEREYAN_ENABLE_AUTH")
    if env is not None and env.strip():
        value = env.strip().lower()
        if value in _TRUE:
            return True, _source("env", "CEREYAN_ENABLE_AUTH")
        if value in _FALSE:
            return False, _source("env", "CEREYAN_ENABLE_AUTH")
        raise CereyanError(f"invalid CEREYAN_ENABLE_AUTH {env!r}: expected true, false, 1, 0, yes, or no")
    if app_enable_auth is not None:
        if not isinstance(app_enable_auth, bool):
            raise CereyanError(f"invalid app.serve(enable_auth={app_enable_auth!r}): expected True or False")
        return app_enable_auth, _source("app", "app.serve(enable_auth=)")
    value = server_settings(directory).get("enable_auth")
    if value is None:
        return False, _source("default")
    if not isinstance(value, bool):
        raise CereyanError(f"invalid [server] enable_auth {value!r} in cereyan.toml: expected true or false")
    return value, _source("toml", "[server] enable_auth")


def resolve_enable_auth(directory: str, enable_auth: bool | None = None,
                        app_enable_auth: bool | None = None) -> bool:
    """Flag, environment, app.serve(), cereyan.toml. False unless one of them turns auth on."""
    return _enable_auth(directory, enable_auth, app_enable_auth)[0]


def _allow_unauthenticated(directory: str, allow_unauthenticated: bool | None = None,
                           app_allow_unauthenticated: bool | None = None) -> tuple[bool, dict]:
    if allow_unauthenticated:
        return True, _source("flag", "--allow-unauthenticated")
    env = os.environ.get("CEREYAN_ALLOW_UNAUTHENTICATED")
    if env is not None and env.strip():
        value = env.strip().lower()
        if value in _TRUE:
            return True, _source("env", "CEREYAN_ALLOW_UNAUTHENTICATED")
        if value in _FALSE:
            return False, _source("env", "CEREYAN_ALLOW_UNAUTHENTICATED")
        raise CereyanError(f"invalid CEREYAN_ALLOW_UNAUTHENTICATED {env!r}: expected true, false, 1, 0, yes, or no")
    if app_allow_unauthenticated is not None:
        if not isinstance(app_allow_unauthenticated, bool):
            raise CereyanError(
                f"invalid app.serve(allow_unauthenticated={app_allow_unauthenticated!r}): expected True or False"
            )
        return app_allow_unauthenticated, _source("app", "app.serve(allow_unauthenticated=)")
    value = server_settings(directory).get("allow_unauthenticated")
    if value is None:
        return False, _source("default")
    if not isinstance(value, bool):
        raise CereyanError(
            f"invalid [server] allow_unauthenticated {value!r} in cereyan.toml: expected true or false"
        )
    return value, _source("toml", "[server] allow_unauthenticated")


def _mcp_read_only(directory: str, mcp_read_only: bool | None = None,
                           app_mcp_read_only: bool | None = None) -> tuple[bool, dict]:
    if mcp_read_only:
        return True, _source("flag", "--mcp-read-only")
    env = os.environ.get("CEREYAN_MCP_READ_ONLY")
    if env is not None and env.strip():
        value = env.strip().lower()
        if value in _TRUE:
            return True, _source("env", "CEREYAN_MCP_READ_ONLY")
        if value in _FALSE:
            return False, _source("env", "CEREYAN_MCP_READ_ONLY")
        raise CereyanError(f"invalid CEREYAN_MCP_READ_ONLY {env!r}: expected true, false, 1, 0, yes, or no")
    if app_mcp_read_only is not None:
        if not isinstance(app_mcp_read_only, bool):
            raise CereyanError(
                f"invalid app.serve(mcp_read_only={app_mcp_read_only!r}): expected True or False"
            )
        return app_mcp_read_only, _source("app", "app.serve(mcp_read_only=)")
    value = server_settings(directory).get("mcp_read_only")
    if value is None:
        return False, _source("default")
    if not isinstance(value, bool):
        raise CereyanError(
            f"invalid [server] mcp_read_only {value!r} in cereyan.toml: expected true or false"
        )
    return value, _source("toml", "[server] mcp_read_only")


def _metrics_public(directory: str, metrics_public: bool | None = None,
                           app_metrics_public: bool | None = None) -> tuple[bool, dict]:
    if metrics_public:
        return True, _source("flag", "--metrics-public")
    env = os.environ.get("CEREYAN_METRICS_PUBLIC")
    if env is not None and env.strip():
        value = env.strip().lower()
        if value in _TRUE:
            return True, _source("env", "CEREYAN_METRICS_PUBLIC")
        if value in _FALSE:
            return False, _source("env", "CEREYAN_METRICS_PUBLIC")
        raise CereyanError(f"invalid CEREYAN_METRICS_PUBLIC {env!r}: expected true, false, 1, 0, yes, or no")
    if app_metrics_public is not None:
        if not isinstance(app_metrics_public, bool):
            raise CereyanError(
                f"invalid app.serve(metrics_public={app_metrics_public!r}): expected True or False"
            )
        return app_metrics_public, _source("app", "app.serve(metrics_public=)")
    value = server_settings(directory).get("metrics_public")
    if value is None:
        return False, _source("default")
    if not isinstance(value, bool):
        raise CereyanError(
            f"invalid [server] metrics_public {value!r} in cereyan.toml: expected true or false"
        )
    return value, _source("toml", "[server] metrics_public")


def resolve_allow_unauthenticated(directory: str, allow_unauthenticated: bool | None = None,
                                  app_allow_unauthenticated: bool | None = None) -> bool:
    """Flag, environment, app.serve(), cereyan.toml. False unless one of them opts out of the generated token."""
    return _allow_unauthenticated(directory, allow_unauthenticated, app_allow_unauthenticated)[0]


def _string_setting(directory: str, key: str, env_name: str, flag_name: str, flag: str | None,
                    app_value: str | None) -> tuple[str | None, dict]:
    """The first of flag, environment, app.serve(), cereyan.toml, with its source."""
    value, source = _first((
        (flag, "flag", flag_name),
        (os.environ.get(env_name) or None, "env", env_name),
        (app_value, "app", f"app.serve({key}=)"),
        (server_settings(directory).get(key), "toml", f"[server] {key}"),
    ))
    if value is not None and not isinstance(value, str):
        raise CereyanError(f"invalid {key} {value!r} from {_describe(source)}: expected a string")
    return value, source


def _resolve_string(directory: str, key: str, env_name: str, flag_name: str, flag: str | None,
                    app_value: str | None) -> tuple[str | None, str | None]:
    """The first of flag, environment, app.serve(), cereyan.toml, with where it came from."""
    value, source = _string_setting(directory, key, env_name, flag_name, flag, app_value)
    return value, (None if value is None else _describe(source))


_COOKIE_NAME = re.compile(r"[!#$%&'*+\-.^_`|~0-9A-Za-z]+")


def _auth_cookie(directory: str, auth_cookie: str | None = None,
                 app_auth_cookie: str | None = None) -> tuple[str | None, dict]:
    value, source = _string_setting(directory, "auth_cookie", "CEREYAN_AUTH_COOKIE", "--auth-cookie",
                                    auth_cookie, app_auth_cookie)
    if value is None:
        return None, source
    if value == "cereyan_token":
        raise CereyanError(
            f"auth_cookie from {_describe(source)} cannot be 'cereyan_token': that cookie holds the API token"
        )
    if not _COOKIE_NAME.fullmatch(value):
        raise CereyanError(f"invalid auth_cookie {value!r} from {_describe(source)}: expected a cookie name")
    return value, source


def resolve_auth_cookie(directory: str, auth_cookie: str | None = None,
                        app_auth_cookie: str | None = None) -> str | None:
    """Flag, environment, app.serve(), cereyan.toml. None means bearer credentials only."""
    return _auth_cookie(directory, auth_cookie, app_auth_cookie)[0]


def _auth_scope(directory: str, auth_scope: str | None = None, app_auth_scope: str | None = None) -> tuple[str, dict]:
    value, source = _string_setting(directory, "auth_scope", "CEREYAN_AUTH_SCOPE", "--auth-scope",
                                    auth_scope, app_auth_scope)
    if value is None:
        return "api", source
    if value not in ("api", "all"):
        raise CereyanError(f"invalid auth_scope {value!r} from {_describe(source)}: expected 'api' or 'all'")
    return value, source


def _public_url(directory: str, public_url: str | None = None, app_public_url: str | None = None) -> tuple[str | None, dict]:
    value, source = _string_setting(directory, "public_url", "CEREYAN_PUBLIC_URL", "--public-url", public_url, app_public_url)
    if value is None or not value.strip():
        return None, source
    value = value.strip().rstrip("/")
    if not (value.startswith("http://") or value.startswith("https://")) or urlsplit(value).hostname is None:
        raise CereyanError(f"invalid public_url {value!r} from {_describe(source)}: expected an http or https URL")
    return value, source


def resolve_public_url(directory: str, public_url: str | None = None, app_public_url: str | None = None) -> str | None:
    """Flag, environment, app.serve(), cereyan.toml. None means the server's own address."""
    return _public_url(directory, public_url, app_public_url)[0]


def resolve_auth_scope(directory: str, auth_scope: str | None = None, app_auth_scope: str | None = None) -> str:
    """Flag, environment, app.serve(), cereyan.toml. ``"api"`` unless set."""
    return _auth_scope(directory, auth_scope, app_auth_scope)[0]


def _valid_login_url(url: str) -> bool:
    if any(c.isspace() or ord(c) < 32 or ord(c) == 127 for c in url):
        return False
    if url.startswith("/"):
        # `//host` and `/\host` leave the origin in a browser.
        return not url.startswith("//") and not url.startswith("/\\")
    parts = urlsplit(url)
    return parts.scheme.lower() in ("http", "https") and bool(parts.netloc)


def _login_url(directory: str, login_url: str | None = None,
               app_login_url: str | None = None) -> tuple[str | None, dict]:
    value, source = _string_setting(directory, "login_url", "CEREYAN_LOGIN_URL", "--login-url",
                                    login_url, app_login_url)
    if value is None:
        return None, source
    if not _valid_login_url(value):
        raise CereyanError(
            f"invalid login_url {value!r} from {_describe(source)}: "
            "expected an http or https URL, or a path starting with /"
        )
    return value, source


def resolve_login_url(directory: str, login_url: str | None = None, app_login_url: str | None = None) -> str | None:
    """Flag, environment, app.serve(), cereyan.toml. None means no sign-in link."""
    return _login_url(directory, login_url, app_login_url)[0]


_HOST_LABEL = re.compile(r"[a-z0-9-]+")


def _valid_allowed_host(host: str) -> bool:
    bare = host[1:-1] if host.startswith("[") and host.endswith("]") else host
    try:
        ipaddress.ip_address(bare)
        return True
    except ValueError:
        return all(_HOST_LABEL.fullmatch(label) for label in host.split("."))


def _allowed_hosts(directory: str, allowed_hosts: list[str] | None = None,
                   app_allowed_hosts: list[str] | tuple[str, ...] | None = None) -> tuple[list[str], dict]:
    env = [item for item in os.environ.get("CEREYAN_ALLOWED_HOSTS", "").split(",") if item.strip()]
    value, source = _first((
        (list(allowed_hosts) if allowed_hosts else None, "flag", "--allowed-host"),
        (env or None, "env", "CEREYAN_ALLOWED_HOSTS"),
        (app_allowed_hosts, "app", "app.serve(allowed_hosts=)"),
        (server_settings(directory).get("allowed_hosts"), "toml", "[server] allowed_hosts"),
    ), [])
    if not isinstance(value, (list, tuple)) or not all(isinstance(item, str) for item in value):
        raise CereyanError(f"invalid allowed_hosts {value!r} from {_describe(source)}: expected a list of host names")
    hosts = [item.strip().lower() for item in value]
    for host in hosts:
        if not _valid_allowed_host(host):
            raise CereyanError(
                f"invalid allowed_hosts entry {host!r} from {_describe(source)}: "
                "expected a host name or an IP address, without a scheme, port, or path"
            )
    return hosts, source


def resolve_allowed_hosts(directory: str, allowed_hosts: list[str] | None = None,
                          app_allowed_hosts: list[str] | tuple[str, ...] | None = None) -> list[str]:
    """Flag, environment, app.serve(), cereyan.toml. Empty unless set; the first source wins whole."""
    return _allowed_hosts(directory, allowed_hosts, app_allowed_hosts)[0]


def _file_sources(directory: str, settings: dict, toml_defaults: dict) -> dict[str, dict]:
    """Sources of the settings only cereyan.toml can set."""

    def from_file(table: str, key: str, present: bool) -> dict:
        return _source("toml", f"[{table}] {key}") if present else _source("default")

    out = {
        "server.cancel_grace_secs": from_file("server", "cancel_grace_secs", "cancel_grace_secs" in settings),
        "defaults.catchup": from_file("defaults", "catchup", "catchup" in toml_defaults),
        "defaults.retain_days": from_file("defaults", "retain_days", "retain_days" in toml_defaults),
        **{f"defaults.{key}": from_file("defaults", key, key in toml_defaults)
           for key in ("retain_runs_days", "retain_failed_runs_days", "keep_last_runs_per_flow",
                       "backup_every", "backup_keep", "retain_checkpoints_days")},
        "ui.title": from_file("ui", "title", ui_title(directory) is not None),
    }
    for key in resource_totals(directory):
        out[f"resources.{key}"] = _source("toml", f"[resources] {key}")
    for key in email_settings(directory) or {}:
        out[f"email.{key}"] = _source("toml", f"[email] {key}")
    return out


def _qualname(fn) -> str:
    return f"{getattr(fn, '__module__', '?')}.{getattr(fn, '__qualname__', repr(fn))}"


def choose_authenticator(authenticators: list, enabled: bool, failed_modules: list[str]):
    """The authenticator the server calls: the one registered when auth is enabled, else None."""
    if len(authenticators) > 1:
        raise CereyanError(
            "only one @app.authenticator may be registered per process; found "
            + " and ".join(_qualname(fn) for fn in authenticators)
        )
    if not enabled:
        return None
    if not authenticators:
        message = "enable_auth is true but no @app.authenticator is registered"
        if failed_modules:
            message += "; these modules failed to import: " + ", ".join(failed_modules)
        raise CereyanError(message)
    return authenticators[0]


def serve(directory: str | None = None, *, host: str | None = None, port: int | None = None,
          max_engines: int | None = None, engine_max_runs: int | None = None, open_browser: bool | None = None,
          discover: bool = True, quiet: bool = False, ready=None, crash_retries: int | None = None,
          token: str | None = None, socket: str | None = None, app_host: str | None = None,
          app_port: int | None = None, app_token: str | None = None, app_socket: str | None = None,
          base_path: str | None = None, app_base_path: str | None = None,
          enable_auth: bool | None = None, app_enable_auth: bool | None = None,
          auth_cookie: str | None = None, app_auth_cookie: str | None = None,
          auth_scope: str | None = None, app_auth_scope: str | None = None,
          login_url: str | None = None, app_login_url: str | None = None,
          allowed_hosts: list[str] | None = None,
          app_allowed_hosts: list[str] | tuple[str, ...] | None = None,
          allow_unauthenticated: bool | None = None, app_allow_unauthenticated: bool | None = None,
          mcp_read_only: bool | None = None, app_mcp_read_only: bool | None = None,
          metrics_public: bool | None = None, app_metrics_public: bool | None = None,
          public_url: str | None = None, app_public_url: str | None = None) -> int:
    """Serve ``directory``. ``host``, ``port``, ``token``, ``socket``, ``base_path``,
    ``enable_auth``, ``auth_cookie``, ``auth_scope``, ``login_url``,
    ``allowed_hosts``, ``allow_unauthenticated``, and ``mcp_read_only`` are the CLI flags; the
    ``app_*`` values come from ``app.serve()`` and rank below the environment."""
    directory = os.path.abspath(directory or os.getcwd())
    if not os.path.isdir(directory):
        raise CereyanError(f"{directory} is not a directory")
    settings = server_settings(directory)
    # Where each setting came from, keyed `table.key`, for the Environment tab.
    sources: dict[str, dict] = {}
    resolved_token, sources["server.token"] = _token(directory, token, app_token)
    resolved_socket, sources["server.socket"] = _socket(directory, socket, app_socket)
    resolved_base_path, sources["server.base_path"] = _base_path(directory, base_path, app_base_path)
    resolved_enable_auth, sources["server.enable_auth"] = _enable_auth(directory, enable_auth, app_enable_auth)
    resolved_auth_cookie, sources["server.auth_cookie"] = _auth_cookie(directory, auth_cookie, app_auth_cookie)
    resolved_auth_scope, sources["server.auth_scope"] = _auth_scope(directory, auth_scope, app_auth_scope)
    resolved_login_url, sources["server.login_url"] = _login_url(directory, login_url, app_login_url)
    resolved_allowed_hosts, sources["server.allowed_hosts"] = _allowed_hosts(
        directory, allowed_hosts, app_allowed_hosts
    )
    resolved_allow_unauthenticated, sources["server.allow_unauthenticated"] = _allow_unauthenticated(
        directory, allow_unauthenticated, app_allow_unauthenticated
    )
    resolved_mcp_read_only, sources["server.mcp_read_only"] = _mcp_read_only(directory, mcp_read_only, app_mcp_read_only)
    resolved_metrics_public, sources["server.metrics_public"] = _metrics_public(directory, metrics_public, app_metrics_public)
    resolved_public_url, sources["server.public_url"] = _public_url(directory, public_url, app_public_url)
    if resolved_auth_scope == "all" and not resolved_enable_auth:
        raise CereyanError(
            "auth_scope 'all' requires enable_auth: without an authenticator the UI could not load its token prompt"
        )
    failed_modules: list[str] = []
    if discover:
        modules = discover_modules(directory)
        engine.runner.suppress_top_level_runs(True, "cereyan serve is importing modules")
        try:
            for name, tb in import_modules(directory, modules):
                failed_modules.append(name)
                print(f"warning: could not import {name}:\n{tb}", file=sys.stderr)
        finally:
            engine.runner.suppress_top_level_runs(False)
    resolved_host, resolved_port, sources["server.host"], sources["server.port"] = _host_port(
        directory, host, port, app_host, app_port
    )

    registered = apps.all_apps()
    flows = [f for app in registered for f in app.flows.values()]
    routes = [r for app in registered for r in app.routes]
    authenticators = [fn for app in registered for fn in app.authenticators]
    authenticator = choose_authenticator(authenticators, resolved_enable_auth, failed_modules)
    if authenticator is not None:
        print(f"cereyan: auth enabled; {_qualname(authenticator)} validates credentials", file=sys.stderr)
        if resolved_auth_cookie is None:
            print("cereyan: auth_cookie is not set, so only bearer credentials reach the authenticator "
                  "and browsers cannot sign in", file=sys.stderr)
    else:
        if authenticators:
            print(f"cereyan: authenticator {_qualname(authenticators[0])} is registered but disabled; "
                  "set enable_auth to use it", file=sys.stderr)
        for key, value in (("auth_cookie", resolved_auth_cookie), ("login_url", resolved_login_url)):
            if value is not None:
                print(f"warning: {key} has no effect while enable_auth is false", file=sys.stderr)
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
    crash_default, sources["defaults.crash_retries"] = _first((
        (toml_defaults.get("crash_retries"), "toml", "[defaults] crash_retries"),
        (crash_retries, "flag", "--crash-retries"),
    ), 5)
    crash_default = int(crash_default)
    # max_engines and engine_max_runs: the argument, then [server].
    engines, sources["server.max_engines"] = _first((
        (max_engines or None, "flag", "--max-engines or app.serve(max_engines=)"),
        (settings.get("max_engines") or None, "toml", "[server] max_engines"),
    ), os.cpu_count() or 4)
    engine_runs, sources["server.engine_max_runs"] = _first((
        (engine_max_runs or None, "flag", "--engine-max-runs or app.serve(engine_max_runs=)"),
        (settings.get("engine_max_runs") or None, "toml", "[server] engine_max_runs"),
    ), 100)
    should_open, sources["server.open_browser"] = _first((
        (False if os.environ.get("CEREYAN_NO_BROWSER") else None, "env", "CEREYAN_NO_BROWSER"),
        (open_browser, "flag", "--no-open or app.serve(open_browser=)"),
        (settings.get("open_browser"), "toml", "[server] open_browser"),
    ), True)
    sources.update(_file_sources(directory, settings, toml_defaults))

    dispatcher = Dispatcher(routes)
    email = email_settings(directory)
    config = {
        "home": engine.resolved_home(),
        "host": resolved_host,
        "port": resolved_port,
        "served_dir": directory,
        "python": sys.executable,
        "max_engines": int(engines),
        "engine_max_runs": int(engine_runs),
        "cancel_grace_secs": int(settings.get("cancel_grace_secs", 10)),
        "custom_routes": dispatcher.specs(),
        "live_flows": live_ids,
        "version": _core.__version__,
        "resources": resource_totals(directory),
        "crash_retries_default": crash_default,
        "fast_crash_rerun": bool(os.environ.get("CEREYAN_FAST_CRASH_RERUN")),
        "email": email,
        "retain_days": int(toml_defaults.get("retain_days", 30)),
        "retain_runs_days": int(toml_defaults.get("retain_runs_days", 0)),
        "retain_failed_runs_days": int(toml_defaults.get("retain_failed_runs_days", 0)),
        "keep_last_runs_per_flow": int(toml_defaults.get("keep_last_runs_per_flow", 10)),
        "backup_every": int(toml_defaults.get("backup_every", 0)),
        "backup_keep": int(toml_defaults.get("backup_keep", 7)),
        "retain_checkpoints_days": int(toml_defaults.get("retain_checkpoints_days", 7)),
        "title": ui_title(directory),
        "catchup_default": str(toml_defaults.get("catchup", "skip")),
        "retention_interval_secs": int(os.environ["CEREYAN_RETENTION_INTERVAL"]) if os.environ.get("CEREYAN_RETENTION_INTERVAL") else None,
        "token": resolved_token,
        "socket": resolved_socket,
        "base_path": resolved_base_path,
        "auth_cookie": resolved_auth_cookie,
        "auth_scope": resolved_auth_scope,
        "login_url": resolved_login_url,
        "allowed_hosts": resolved_allowed_hosts,
        "allow_unauthenticated": resolved_allow_unauthenticated,
        "mcp_read_only": resolved_mcp_read_only,
        "metrics_public": resolved_metrics_public,
        "public_url": resolved_public_url,
        "open_browser": bool(should_open),
        "sources": sources,
        "python_version": platform.python_version(),
        "platform": f"{platform.system().lower()} {platform.machine()}",
    }
    try:
        server = _core.Server.start(store, json.dumps(config), dispatcher if routes else None, rule_dispatch,
                                    authenticator=authenticator)
    except RuntimeError as exc:
        engine.close_store()
        raise CereyanError(str(exc)) from None

    if not quiet:
        print(f"cereyan serving {len(flows)} flow(s) from {directory} at {server.url}", file=sys.stderr)
    if should_open:
        try:
            webbrowser.open(server.url + "/")
        except Exception:
            pass
    if ready is not None:
        ready(server)

    def _terminate(signum, frame):
        raise KeyboardInterrupt

    # SIGTERM is the cooperative stop on Unix. Windows never delivers it, and a
    # console control event arrives as SIGBREAK, whose default handler ends the
    # process before any of the cleanup below runs: the discovery file survives,
    # the store is not flushed, and engines are left behind. Handle whichever the
    # platform has, so stopping the server means the same thing everywhere.
    # Interactive Ctrl-C needs nothing extra; it arrives as SIGINT.
    # Handlers can only be installed on the main thread. Served from another
    # thread, the host stops the server through the handle `ready` received.
    stop_signals = [signal.SIGTERM]
    if hasattr(signal, "SIGBREAK"):  # Windows
        stop_signals.append(signal.SIGBREAK)
    on_main_thread = threading.current_thread() is threading.main_thread()
    previous = [(sig, signal.signal(sig, _terminate)) for sig in stop_signals] if on_main_thread else []
    try:
        while not server.wait(0.5):
            pass
    except KeyboardInterrupt:
        if not quiet:
            print("cereyan: shutting down", file=sys.stderr)
    finally:
        for sig, handler in previous:
            signal.signal(sig, handler)
        server.stop()
        engine.close_store()
    return 0
