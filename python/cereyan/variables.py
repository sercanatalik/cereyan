"""Variables: small named JSON values, optionally secret."""

from __future__ import annotations

import json
import re
from typing import Any

from . import _core
from .exceptions import CereyanError
from .params import to_json_value

NAME_RE = re.compile(r"^[a-z0-9][a-z0-9_./-]{0,127}$")
MAX_BYTES = 65_536
MASK = "********"


def _validate_name(name: str) -> None:
    if not isinstance(name, str) or not NAME_RE.match(name):
        raise ValueError(
            f"invalid variable name {name!r}: use lowercase letters, digits, '_', '-', '/', and '.'"
        )


def _access():
    """Return ("store", store) or ("server", client)."""
    from . import client as client_module, engine

    try:
        return "store", engine.get_store()
    except _core.StoreLocked:
        server = client_module.find_server(engine.resolved_home())
        if server is None:
            raise
        return "server", server


class Variable:
    """Named JSON values shared by every project on the machine, optionally encrypted.

    Names match ``[a-z0-9][a-z0-9_./-]*`` and values are limited to 64 KB. Offline, values
    go straight to the store; while a server holds the store, they go through its API.
    Secrets are encrypted with the key kept at ``<home>/secret.key`` and are masked in
    the API and the UI.
    """
    @staticmethod
    def get(name: str, default: Any = None) -> Any:
        """Read a variable.

        Args:
            name: The variable name.
            default: Returned when the variable does not exist.

        Returns:
            The stored value, decrypted for secrets.
        """
        _validate_name(name)
        mode, handle = _access()
        if mode == "store":
            try:
                text = handle.get_variable(name)
            except RuntimeError as exc:
                raise CereyanError(str(exc)) from None
            return default if text is None else json.loads(text)
        from .client import ApiError

        try:
            data = handle._request("GET", f"/api/variables/{name}", params={"raw": "true"})
        except ApiError as exc:
            if exc.status == 404:
                return default
            raise
        if data.get("secret"):
            raw = data.get("raw")
            if raw is None:
                raise CereyanError(f"secret variable {name!r} could not be read")
            from . import engine

            try:
                return json.loads(_core.decrypt_secret(engine.resolved_home(), raw))
            except RuntimeError as exc:
                raise CereyanError(str(exc)) from None
        return data.get("value", default)

    @staticmethod
    def set(name: str, value: Any, tags: list[str] | None = None, secret: bool = False, overwrite: bool = True) -> None:
        """Create or update a variable.

        Args:
            name: The variable name.
            value: Any JSON-serialisable value (dates and dataclasses are converted).
            tags: Tags shown on the Variables page.
            secret: Encrypt the value at rest and mask it in the API and the UI.
            overwrite: When ``False``, raise if the variable already exists.

        Raises:
            ValueError: On an invalid name or a value over 64 KB.
            CereyanError: When ``overwrite`` is ``False`` and the variable exists.
        """
        _validate_name(name)
        payload = json.dumps(to_json_value(value))
        if len(payload.encode("utf-8")) > MAX_BYTES:
            raise ValueError("variable value exceeds 64 KB")
        mode, handle = _access()
        if mode == "store":
            if not overwrite and handle.get_variable(name) is not None:
                raise CereyanError(f"variable {name!r} exists and overwrite is False")
            handle.set_variable(name, payload, json.dumps(list(tags or [])), bool(secret))
            return
        handle._request(
            "POST", "/api/variables",
            body={"name": name, "value": json.loads(payload), "tags": list(tags or []), "secret": bool(secret), "overwrite": overwrite},
        )

    @staticmethod
    def unset(name: str) -> bool:
        """Delete a variable.

        Args:
            name: The variable name.

        Returns:
            ``True`` when a variable was deleted, ``False`` when none existed.
        """
        _validate_name(name)
        mode, handle = _access()
        if mode == "store":
            return bool(handle.delete_variable(name))
        from .client import ApiError

        try:
            handle._request("DELETE", f"/api/variables/{name}")
            return True
        except ApiError as exc:
            if exc.status == 404:
                return False
            raise
