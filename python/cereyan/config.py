"""Project-level ``cereyan.toml`` loading."""

from __future__ import annotations

import os
import sys
import tomllib
from functools import lru_cache

from .exceptions import ConfigError

CONFIG_FILE = "cereyan.toml"

HOME_ONLY_MESSAGE = (
    "cereyan.toml cannot set the store location: a [store] table is not allowed. "
    "The home is set by --home or CEREYAN_HOME only."
)


@lru_cache(maxsize=64)
def load_project_config(directory: str) -> dict:
    """Load and validate ``<directory>/cereyan.toml``; an absent file is ``{}``."""
    path = os.path.join(directory, CONFIG_FILE)
    if not os.path.isfile(path):
        return {}
    with open(path, "rb") as fh:
        try:
            data = tomllib.load(fh)
        except tomllib.TOMLDecodeError as exc:
            raise ConfigError(f"{path}: {exc}") from exc
    if "store" in data:
        raise ConfigError(f"{path}: {HOME_ONLY_MESSAGE}")
    server = data.get("server", {})
    if not isinstance(server, dict):
        raise ConfigError(f"{path}: [server] must be a table")
    for warning in unknown_keys(data):
        print(f"warning: {path}: {warning}", file=sys.stderr)
    return data


KNOWN_KEYS = {
    "server": {"host", "port", "max_engines", "engine_max_runs", "cancel_grace_secs", "open_browser", "token", "socket"},
    "defaults": {"catchup", "crash_retries", "retain_days", "max_engines", "engine_max_runs"},
    "email": {"host", "port", "tls", "username", "password", "from"},
}


def unknown_keys(data: dict) -> list[str]:
    out = []
    for table, keys in data.items():
        if table == "resources":
            continue
        if table not in KNOWN_KEYS:
            out.append(f"unknown table [{table}]")
            continue
        if isinstance(keys, dict):
            for k in keys:
                if k not in KNOWN_KEYS[table]:
                    out.append(f"unknown key {k!r} in [{table}]")
    return out


def email_settings(directory: str) -> dict | None:
    table = load_project_config(directory).get("email")
    if not table:
        return None
    if "host" not in table or "from" not in table:
        raise ConfigError("[email] needs host and from")
    return dict(table)


def server_settings(directory: str) -> dict:
    """The ``[server]`` table (host, port, max_engines, engine_max_runs, open_browser)."""
    return dict(load_project_config(directory).get("server", {}))


def resource_totals(directory: str) -> dict[str, float]:
    table = load_project_config(directory).get("resources", {})
    out = {}
    for k, v in (table or {}).items():
        try:
            out[str(k)] = float(v)
        except (TypeError, ValueError):
            raise ConfigError(f"[resources] {k} must be a number") from None
    return out


def defaults(directory: str) -> dict:
    """The ``[defaults]`` table (crash_retries)."""
    return dict(load_project_config(directory).get("defaults", {}))
