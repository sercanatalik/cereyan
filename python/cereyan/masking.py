"""Secret masking for run logs: values handed out by ``Variable.get`` for a secret
are replaced with ``***`` in everything this process records about a run.

The registry is per process and only grows. A warm engine keeps masking a secret
across the runs it executes, which errs on masking more. Only exact substrings
are replaced, as every comparable tool does: a secret that a flow slices, encodes,
or interpolates before logging is not recognised.
"""

from __future__ import annotations

import json
import threading
from typing import Any

MIN_LENGTH = 4
MASK = "***"

_lock = threading.Lock()
_values: list[str] = []


def register(value: Any) -> None:
    """Register a secret's value: its string form, or every string leaf of an object or list."""
    found: list[str] = []
    _collect(value, found)
    with _lock:
        for text in found:
            if len(text) >= MIN_LENGTH and text not in _values:
                _values.append(text)
        _values.sort(key=len, reverse=True)


def _collect(value: Any, out: list[str]) -> None:
    if isinstance(value, str):
        out.append(value)
    elif isinstance(value, dict):
        for item in value.values():
            _collect(item, out)
    elif isinstance(value, (list, tuple)):
        for item in value:
            _collect(item, out)
    elif value is not None and not isinstance(value, bool):
        # A number or another scalar: its JSON text, so a numeric secret is still hidden.
        out.append(json.dumps(value))


def mask(text: str) -> str:
    """``text`` with every registered value replaced by ``***``, longest first."""
    with _lock:
        values = list(_values)
    for value in values:
        if value in text:
            text = text.replace(value, MASK)
    return text


def active() -> bool:
    """Whether any secret has been registered in this process."""
    return bool(_values)


def clear() -> None:
    """Forget every registered value; for tests."""
    with _lock:
        _values.clear()
