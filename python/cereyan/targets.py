"""Targets: idempotent outputs. A Target is anything with ``exists()``."""

from __future__ import annotations

import contextlib
import io
import os
import uuid
from typing import Any, Protocol, runtime_checkable


@runtime_checkable
class Target(Protocol):
    """Anything with ``exists()``: the protocol a task's ``output=`` must satisfy."""
    def exists(self) -> bool:
        """``True`` when the output this target stands for is already present."""


class LocalTarget:
    """A file on the local filesystem written atomically."""

    def __init__(self, path: str | os.PathLike, *, mkdir: bool = True) -> None:
        self.path = os.fspath(path)
        self.mkdir = mkdir

    def __repr__(self) -> str:
        return f"LocalTarget({self.path!r})"

    def __fspath__(self) -> str:
        return self.path

    def exists(self) -> bool:
        """``True`` when the file exists."""
        return os.path.exists(self.path)

    def remove(self) -> None:
        """Delete the file if it exists."""
        try:
            os.remove(self.path)
        except FileNotFoundError:
            pass

    def _tmp_path(self) -> str:
        return f"{self.path}.tmp-{uuid.uuid4().hex[:12]}"

    @contextlib.contextmanager
    def temporary_path(self):
        """Yield a temporary path; on success it is renamed onto the target."""
        if self.mkdir:
            os.makedirs(os.path.dirname(os.path.abspath(self.path)) or ".", exist_ok=True)
        tmp = self._tmp_path()
        try:
            yield tmp
        except BaseException:
            with contextlib.suppress(FileNotFoundError):
                os.remove(tmp)
            raise
        os.replace(tmp, self.path)

    def open(self, mode: str = "r", **kwargs: Any):
        """Open the file; write modes write to a temporary path that replaces the target on close, so readers never see a partial file."""
        if "w" in mode or "a" in mode or "x" in mode:
            return _AtomicWriter(self, mode, **kwargs)
        return open(self.path, mode, **kwargs)


class _AtomicWriter:
    """File object writing to a temporary path, renamed on close."""

    def __init__(self, target: LocalTarget, mode: str, **kwargs: Any) -> None:
        self.target = target
        if target.mkdir:
            os.makedirs(os.path.dirname(os.path.abspath(target.path)) or ".", exist_ok=True)
        self.tmp = target._tmp_path()
        self._file = open(self.tmp, mode, **kwargs)
        self._closed = False

    def __getattr__(self, name: str):
        return getattr(self._file, name)

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc, tb):
        if exc_type is None:
            self.close()
        else:
            self.abort()
        return False

    def __iter__(self):
        return iter(self._file)

    def write(self, data):
        return self._file.write(data)

    def close(self) -> None:
        if self._closed:
            return
        self._closed = True
        self._file.flush()
        try:
            os.fsync(self._file.fileno())
        except (OSError, io.UnsupportedOperation):
            pass
        self._file.close()
        os.replace(self.tmp, self.target.path)

    def abort(self) -> None:
        if self._closed:
            return
        self._closed = True
        self._file.close()
        with contextlib.suppress(FileNotFoundError):
            os.remove(self.tmp)

    def __del__(self):
        if not self._closed:
            with contextlib.suppress(Exception):
                self._file.close()


def resolve_output(spec: Any, values: dict[str, Any]) -> Any:
    """Turn an ``output=`` declaration into a Target for the given arguments."""
    if spec is None:
        return None
    if callable(spec) and not hasattr(spec, "exists"):
        import inspect

        params = inspect.signature(spec).parameters
        if any(p.kind == p.VAR_KEYWORD for p in params.values()):
            return spec(**values)
        return spec(**{k: v for k, v in values.items() if k in params})
    return spec
