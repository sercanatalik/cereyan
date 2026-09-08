"""Logging inside runs: records are stored with the run through the active
backend and echoed by the user's own handlers. ``log_prints`` tees stdout
into the run log."""

from __future__ import annotations

import contextlib
import io
import logging
import sys
from typing import TYPE_CHECKING

from . import context

if TYPE_CHECKING:
    from .engine.backends import Backend

RUN_LOGGER = "cereyan.run"


def get_run_logger() -> logging.Logger:
    """A logger that carries the current run and task-run in its records."""
    return logging.getLogger(RUN_LOGGER)


class RunLogHandler(logging.Handler):
    """Forwards records of the current run to its backend. Never blocks on I/O
    beyond what the backend buffers."""

    def __init__(self, backend: Backend, run_id: int) -> None:
        super().__init__()
        self.backend = backend
        self.run_id = run_id

    def emit(self, record: logging.LogRecord) -> None:
        run = context.current_run()
        if run is None or run.id != self.run_id:
            return
        task_run = context.current_task_run()
        try:
            message = record.getMessage()
        except Exception:  # pragma: no cover - defensive against bad format strings
            message = str(record.msg)
        if record.exc_info and record.exc_text is None:
            record.exc_text = logging.Formatter().formatException(record.exc_info)
        if record.exc_text:
            message = f"{message}\n{record.exc_text}"
        try:
            self.backend.log(
                task_run.id if task_run is not None else None,
                int(record.levelno),
                record.name,
                int(record.created * 1_000_000),
                message,
            )
        except Exception:  # pragma: no cover
            self.handleError(record)

    def flush(self) -> None:
        try:
            self.backend.flush_logs()
        except Exception:  # pragma: no cover
            pass


def install(backend: Backend, run_id: int) -> RunLogHandler:
    """Attach a `RunLogHandler` for ``run_id`` to the root logger and return it."""
    handler = RunLogHandler(backend, run_id)
    handler.setLevel(logging.DEBUG)
    logging.getLogger().addHandler(handler)
    cereyan_logger = logging.getLogger("cereyan")
    if cereyan_logger.level == logging.NOTSET:
        cereyan_logger.setLevel(logging.INFO)
    return handler


def uninstall(handler: RunLogHandler) -> None:
    """Flush and detach a handler returned by `install`."""
    handler.flush()
    logging.getLogger().removeHandler(handler)
    handler.close()


class _PrintTee(io.TextIOBase):
    """Writes through to the real stream and logs each complete line."""

    def __init__(self, original, logger: logging.Logger) -> None:
        self.original = original
        self.logger = logger
        self._buffer = ""

    def writable(self) -> bool:
        return True

    def write(self, text: str) -> int:
        self.original.write(text)
        self._buffer += text
        while "\n" in self._buffer:
            line, self._buffer = self._buffer.split("\n", 1)
            if line.strip():
                self.logger.info(line)
        return len(text)

    def flush(self) -> None:
        self.original.flush()
        if self._buffer.strip():
            self.logger.info(self._buffer)
        self._buffer = ""

    @property
    def encoding(self):  # type: ignore[override]
        return getattr(self.original, "encoding", "utf-8")

    def fileno(self) -> int:
        return self.original.fileno()

    def isatty(self) -> bool:
        return False


@contextlib.contextmanager
def capture_prints(enabled: bool):
    """Redirect ``print`` to the run log at INFO while still writing to stdout."""
    if not enabled or isinstance(sys.stdout, _PrintTee):
        yield
        return
    tee = _PrintTee(sys.stdout, logging.getLogger("cereyan.print"))
    logging.getLogger("cereyan.print").setLevel(logging.INFO)
    with contextlib.redirect_stdout(tee):
        try:
            yield
        finally:
            tee.flush()
