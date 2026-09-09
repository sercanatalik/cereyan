"""Result persistence and the task cache under ``<home>/storage``."""

from __future__ import annotations

import enum
import hashlib
import inspect
import json
import os
import pickle
import sys
import time
from datetime import timedelta
from typing import Any

from . import params as _params


class CachePolicy(enum.Flag):
    NONE = 0
    INPUTS = enum.auto()
    SOURCE = enum.auto()

    def __add__(self, other: "CachePolicy") -> "CachePolicy":
        return self | other


INPUTS = CachePolicy.INPUTS
SOURCE = CachePolicy.SOURCE

PYTHON_VERSION = f"{sys.version_info.major}.{sys.version_info.minor}"


def storage_dir(home: str) -> str:
    """The ``storage`` directory under ``home`` where persisted results live, created on first use.

    Only the subdirectory. The home itself is created by the store, which is what
    makes it readable by its owner alone; ``makedirs`` here would create it too, at
    whatever the umask gives, and quietly bypass that. Results are written during a
    run, so the store has already opened the home — if it has not, that is worth an
    error rather than a directory anyone can read.
    """
    path = os.path.join(home, "storage")
    try:
        os.mkdir(path)
    except FileExistsError:
        pass
    return path


def _hash_inputs(values: dict[str, Any]) -> str:
    try:
        payload = json.dumps(_params.to_json_value(values), sort_keys=True, default=repr).encode()
    except TypeError:
        payload = repr(sorted(values.items())).encode()
    return hashlib.sha256(payload).hexdigest()


def _hash_source(fn) -> str:
    try:
        src = inspect.getsource(fn)
    except (OSError, TypeError):
        src = getattr(fn, "__code__", None) and fn.__code__.co_code.hex() or repr(fn)
    return hashlib.sha256(src.encode() if isinstance(src, str) else src).hexdigest()


def cache_key(task_key: str, fn, policy: CachePolicy, values: dict[str, Any]) -> str:
    """The storage key for a task's result under ``policy``: a hash of the task key plus the inputs and/or the source, as the policy selects."""
    parts = [task_key]
    if policy & CachePolicy.INPUTS:
        parts.append(_hash_inputs(values))
    if policy & CachePolicy.SOURCE:
        parts.append(_hash_source(fn))
    return hashlib.sha256("|".join(parts).encode()).hexdigest()


class ResultStore:
    """Persisted task results under ``<home>/storage``.

    Each entry is a one-line JSON header (Python version, serializer, timestamps) followed
    by the pickled or JSON-encoded value. Reads treat a different Python version or an
    expired entry as a miss.
    """
    def __init__(self, home: str) -> None:
        self.dir = storage_dir(home)

    def path(self, key: str) -> str:
        """The file path for ``key``."""
        return os.path.join(self.dir, key)

    def write(self, key: str, value: Any, serializer: str = "pickle", expires: timedelta | None = None) -> str:
        """Persist ``value`` under ``key`` atomically and return the file path."""
        header = {
            "python": PYTHON_VERSION,
            "serializer": serializer,
            "created_at": time.time(),
            "expires_at": (time.time() + expires.total_seconds()) if expires else None,
        }
        path = self.path(key)
        tmp = f"{path}.tmp-{os.getpid()}"
        with open(tmp, "wb") as fh:
            fh.write(json.dumps(header).encode() + b"\n")
            if serializer == "json":
                fh.write(json.dumps(_params.to_json_value(value)).encode())
            else:
                fh.write(pickle.dumps(value, protocol=pickle.HIGHEST_PROTOCOL))
        os.replace(tmp, path)
        return path

    def read(self, key: str) -> tuple[bool, Any]:
        """Return (hit, value). Version mismatch and expiry count as misses."""
        path = self.path(key)
        try:
            with open(path, "rb") as fh:
                header = json.loads(fh.readline().decode())
                body = fh.read()
        except (OSError, ValueError):
            return False, None
        if header.get("python") != PYTHON_VERSION:
            return False, None
        expires_at = header.get("expires_at")
        if expires_at is not None and time.time() > expires_at:
            return False, None
        try:
            if header.get("serializer") == "json":
                return True, json.loads(body.decode())
            return True, pickle.loads(body)
        except Exception:
            return False, None
