"""Task effects: the effect key every task call carries, and how an interrupted
``once`` call is settled, by its probe or by a person, instead of executing again."""

from __future__ import annotations

import hashlib
import threading
import zlib
from typing import Any

from . import context
from .exceptions import RunPaused, TaskInDoubt


class _NotDone:
    """What a ``probe`` returns when the effect did not happen."""

    _instance: "_NotDone | None" = None

    def __new__(cls) -> "_NotDone":
        if cls._instance is None:
            cls._instance = super().__new__(cls)
        return cls._instance

    def __repr__(self) -> str:
        return "NOT_DONE"

    def __reduce__(self):
        return (_NotDone, ())


NOT_DONE = _NotDone()
"""Returned by a ``once`` task's ``probe`` when the effect did not happen, so the
task executes. Any other return value is the task's result."""

#: The schema of the in-doubt question's answer.
IN_DOUBT_SCHEMA: dict = {
    "type": "object",
    "required": ["action"],
    "properties": {
        "action": {"enum": ["happened", "run_again", "fail"]},
        "result": {"description": "The task's return value, when its effect happened"},
    },
}

#: The topic in-doubt questions are asked and answered on.
TOPIC = "in_doubt"


def effect_key(lineage: str, dynamic_key: str) -> str:
    """The key an outside system can deduplicate a task call's effect on."""
    digest = hashlib.sha256(f"cereyan.effect\0{lineage}\0{dynamic_key}".encode())
    return digest.hexdigest()[:32]


def question_index(dynamic_key: str) -> int:
    """Where a call's in-doubt question is answered: a position of its own,
    derived from the dynamic key, so asking it does not move the other
    questions of the body and settling it does not either."""
    return 1_000_000_000 + zlib.crc32(dynamic_key.encode()) % 1_000_000_000


def settle(run: context.RunContext, task: Any, dynamic_key: str) -> tuple[str, Any]:
    """The answer to an in-doubt call: ``("happened", result)`` or ``("run_again", None)``.

    Asks when nobody has answered yet, by pausing the run as ``Paused/InDoubt``.

    Raises:
        RunPaused: No answer yet; the run waits for one.
        TaskInDoubt: The answer was ``fail``, or the call cannot ask, because it is
            offline or off the flow body's thread.
    """
    off_body = threading.get_ident() != run.body_thread or context.in_async_unit()
    if run.backend.offline or off_body:
        where = "offline" if run.backend.offline else "off the flow body's thread"
        raise TaskInDoubt(
            f"{dynamic_key} was interrupted in an earlier run and may have taken effect; called {where}, "
            f"it cannot ask. Give {task.name!r} a probe, or call it from the flow body."
        )
    index = question_index(dynamic_key)
    claimed = run.backend.claim_message(TOPIC, index)
    if claimed is not None and claimed.get("claimed"):
        answer = claimed.get("answer")
        value = answer.get("input") if isinstance(answer, dict) and "input" in answer else answer
        action, result = _action(value)
        if action == "fail":
            raise TaskInDoubt(f"{dynamic_key} was interrupted and may have taken effect; the answer was to fail")
        if action in ("happened", "run_again"):
            return action, result
    prompt = (
        f"{dynamic_key} ({task.name}) was interrupted in an earlier run and may have taken effect. "
        "Did its effect happen?"
    )
    raise RunPaused(prompt, IN_DOUBT_SCHEMA, None, index, name="InDoubt",
                    details={"topic": TOPIC, "dynamic_key": dynamic_key})


def _action(value: Any) -> tuple[str | None, Any]:
    if isinstance(value, str):
        return value, None
    if isinstance(value, dict):
        return value.get("action"), value.get("result")
    return None, None
