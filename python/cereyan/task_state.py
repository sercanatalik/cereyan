"""A small durable note store for a task's own later attempts.

`set` a value from inside a task, an external job id for instance, and the
same task reads it back with `get` on a retry, a later pass of the run, a
crash rerun, or a retry from failure. Entries are scoped to the run and the
task's dynamic key (or the flow body outside a task), hold JSON up to 64 KB,
and are deleted with the run. Offline and served alike.
"""

from __future__ import annotations

import json
from typing import Any

from . import context
from .exceptions import CereyanError

MAX_BYTES = 64 * 1024
_MISSING = object()


def _scope() -> tuple[Any, str]:
    run = context.current_run()
    if run is None or run.backend is None:
        raise CereyanError("task_state needs a running flow")
    task = context.current_task_run()
    return run.backend, (task.dynamic_key if task is not None else "")


def _check_key(key: str) -> str:
    if not isinstance(key, str) or not key.strip() or len(key) > 200:
        raise CereyanError("task_state keys are non-empty strings of at most 200 characters")
    return key.strip()


def get(key: str, default: Any = None) -> Any:
    """The value stored under ``key`` for this task in this run, or in the run
    it continues (a crash rerun or a retry from failure), else ``default``."""
    backend, scope = _scope()
    raw = backend.task_state_get(scope, _check_key(key))
    return default if raw is None else json.loads(raw)


def set(key: str, value: Any) -> None:  # noqa: A001 - mirrors Variable.set
    """Store ``value`` (any JSON, up to 64 KB) under ``key`` for this task in this run.

    Raises:
        CereyanError: Outside a run, for a bad key, or when the value is too large.
    """
    backend, scope = _scope()
    try:
        text = json.dumps(value)
    except (TypeError, ValueError) as exc:
        raise CereyanError(f"task_state values must be JSON: {exc}") from None
    if len(text.encode()) > MAX_BYTES:
        raise CereyanError(f"task_state value for {key!r} exceeds 64 KB")
    backend.task_state_set(scope, _check_key(key), text)


def delete(key: str) -> bool:
    """Remove ``key`` from this task's entries in this run; returns whether it existed."""
    backend, scope = _scope()
    return bool(backend.task_state_delete(scope, _check_key(key)))


def items() -> dict[str, Any]:
    """Every entry this task stored in this run, as a dict."""
    backend, scope = _scope()
    return {row["key"]: row["value"] for row in backend.task_state_list() if row["scope"] == scope}


def _snooze_count(run) -> int:
    """Increment and return the run's snooze count, kept in the flow-scope state store."""
    raw = run.backend.task_state_get("", "snoozes")
    n = (json.loads(raw) if raw is not None else 0) + 1
    run.backend.task_state_set("", "snoozes", json.dumps(n))
    return n
