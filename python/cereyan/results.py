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


def encode(value: Any, serializer: str = "pickle") -> bytes:
    """The bytes `ResultStore.write` would store for ``value``; raises when it cannot be encoded."""
    if serializer == "json":
        return json.dumps(_params.to_json_value(value)).encode()
    return pickle.dumps(value, protocol=pickle.HIGHEST_PROTOCOL)


def checkpoint_key(run_external_id: str, task_run_external_id: str) -> str:
    """The storage key of a task run's checkpoint, in the run's own namespace."""
    return f"ckpt-{run_external_id}-{task_run_external_id}"


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

    def _write_bytes(self, key: str, data: bytes) -> str:
        """Store ``data`` under ``key`` atomically; returns where it went."""
        path = self.path(key)
        tmp = f"{path}.tmp-{os.getpid()}"
        with open(tmp, "wb") as fh:
            fh.write(data)
        os.replace(tmp, path)
        return path

    def _read_bytes(self, key: str) -> bytes | None:
        """The bytes stored under ``key``, or ``None``."""
        try:
            with open(self.path(key), "rb") as fh:
                return fh.read()
        except OSError:
            return None

    def write(self, key: str, value: Any, serializer: str = "pickle", expires: timedelta | None = None) -> str:
        """Persist ``value`` under ``key`` atomically and return where it went."""
        header = {
            "python": PYTHON_VERSION,
            "serializer": serializer,
            "created_at": time.time(),
            "expires_at": (time.time() + expires.total_seconds()) if expires else None,
        }
        if serializer == "json":
            body = json.dumps(_params.to_json_value(value)).encode()
        else:
            body = pickle.dumps(value, protocol=pickle.HIGHEST_PROTOCOL)
        return self._write_bytes(key, json.dumps(header).encode() + b"\n" + body)

    def write_encoded(self, key: str, payload: bytes, serializer: str = "pickle") -> str:
        """Persist an already encoded value (see `encode`) under ``key`` atomically."""
        header = {"python": PYTHON_VERSION, "serializer": serializer, "created_at": time.time(), "expires_at": None}
        return self._write_bytes(key, json.dumps(header).encode() + b"\n" + payload)

    def read(self, key: str) -> tuple[bool, Any]:
        """Return (hit, value). Version mismatch and expiry count as misses."""
        data = self._read_bytes(key)
        if data is None:
            return False, None
        first, _, body = data.partition(b"\n")
        try:
            header = json.loads(first.decode())
        except ValueError:
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


#: Upload chunk size: small enough for a proxy's default body limit to be raised
#: once, not per result.
CHUNK_BYTES = 4 * 1024 * 1024


class ServerResultStore(ResultStore):
    """Results kept by the server, for an engine on a remote worker: the same keys
    and format, stored under the server's ``<home>/storage`` through its API, so a
    rerun on any host finds the checkpoints and cache entries.
    """
    def __init__(self, client) -> None:  # noqa: D107 - documented on the class
        self.client = client
        self.dir = ""

    def path(self, key: str) -> str:
        """Where the entry lives, as the server names it."""
        return f"/api/results/{key}"

    def _write_bytes(self, key: str, data: bytes) -> str:
        import uuid

        # The upload id keeps two uploads of one key apart; the offset makes a
        # retried chunk land where it belongs instead of being appended again.
        upload = uuid.uuid4().hex
        chunks = [data[i:i + CHUNK_BYTES] for i in range(0, len(data), CHUNK_BYTES)] or [b""]
        for n, chunk in enumerate(chunks):
            self.client._request_bytes(
                "PUT", self.path(key), chunk,
                params={"part": n, "last": "true" if n == len(chunks) - 1 else "false",
                        "upload": upload, "offset": n * CHUNK_BYTES},
            )
        return self.path(key)

    def _read_bytes(self, key: str) -> bytes | None:
        from .client import ApiError

        try:
            return self.client._request_bytes("GET", self.path(key))
        except ApiError as exc:
            if exc.status == 404:
                return None
            raise


def result_store(home: str) -> ResultStore:
    """The result store for this process: the server's, through its API, in an engine
    a remote worker started; the local ``<home>/storage`` everywhere else."""
    from .client import served

    client = served()
    if client is not None and os.environ.get("CEREYAN_WORKER"):
        return ServerResultStore(client)
    return ResultStore(home)
