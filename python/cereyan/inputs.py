"""Human-in-the-loop: pause a run until someone answers, and receive messages."""

from __future__ import annotations

import json
import sys
import threading
import time
from typing import Any

from . import context
from .exceptions import CereyanError, RunPaused


def receive(topic: str, *, prompt: str | None = None, schema: dict | None = None,
            timeout: float | None = None, default: Any = None) -> Any:
    """Return the next message on ``topic``, pausing the run when none is available.

    ``timeout`` seconds: resume with ``default`` if no message arrives in time.
    The timer is server-side, so the run stays Paused and frees its engine.

    Inside a served run the first call transitions the run to ``Paused`` with
    ``topic`` in its state details and ends the attempt; the engine is free
    while the run waits. ``POST /api/runs/{id}/messages/{topic}`` stores a
    message and resumes the run, which reruns the flow from the top and
    gets the message from this call.

    Calls are numbered in the order the body reaches them, and a message
    belongs to the call it answered, so a flow can receive, resume, and
    receive again. A message is used only when the topic at that position
    still matches the one it was given for.

    ``wait_for_input(prompt, schema)`` is ``receive('input', prompt=prompt,
    schema=schema)``.

    Outside a served run ``topic='input'`` reads the answer from the terminal;
    other topics raise an error. Timeouts are not supported offline.

    Raises:
        CereyanError: When called outside the thread executing the flow body.
    """
    run = context.current_run()
    if run is None or run.backend.offline:
        if topic == "input":
            if timeout is not None:
                raise CereyanError("receive() timeout is not supported offline")
            return _prompt_terminal(prompt or topic, schema)
        raise CereyanError(
            f"receive({topic!r}) needs a running server; offline only 'input' is supported"
        )
    if threading.get_ident() != run.body_thread:
        raise CereyanError(
            f"receive({topic!r}) belongs in the flow body: pausing works by raising out of it, "
            "and from a task on another thread that ends the task instead of the run"
        )
    index = run.next_input_index()
    # First try an atomic claim: checks stored answers and pending messages.
    claimed = run.backend.claim_message(topic, index)
    if claimed is not None and claimed.get("claimed"):
        answer = claimed.get("answer")
        # For wait_for_input (topic="input"), verify the prompt matches so a
        # changed body asks afresh rather than handing an old answer to a new
        # question.
        if (
            topic == "input"
            and prompt is not None
            and isinstance(answer, dict)
            and answer.get("prompt") not in (None, prompt)
        ):
            pass  # prompt mismatch: pause again
        else:
            return answer.get("input") if isinstance(answer, dict) else answer
    # No matching message: pause. The prompt is shown in the inbox when
    # topic is "input"; for other topics the prompt defaults to the topic.
    display = prompt or topic
    details: dict[str, Any] = {"topic": topic}
    if timeout is not None and timeout > 0:
        details["wake_at"] = int((time.time() + timeout) * 1_000_000)
        # The details travel as JSON already: the default goes in as a value, so
        # the timeout hands it back as one, not as its encoding.
        try:
            json.dumps(default)
            details["default"] = default
        except (TypeError, ValueError):
            details["default"] = str(default)
    task = context.current_task_run()
    raise RunPaused(display, schema, task.id if task else None, index,
                    details=details)


def wait_for_input(prompt: str, schema: dict | None = None) -> Any:
    """Return the answer given to this question, pausing the run when there is none.

    This is ``receive('input', prompt=prompt, schema=schema)``; see
    :func:`receive` for full semantics.
    """
    return receive("input", prompt=prompt, schema=schema)


def _prompt_terminal(prompt: str, schema: dict | None) -> Any:
    if not sys.stdin or not sys.stdin.isatty():
        raise CereyanError(
            f"wait_for_input({prompt!r}) needs a running server (cereyan serve) or an interactive terminal"
        )
    hint = f" (JSON matching {json.dumps(schema)})" if schema else ""
    raw = input(f"{prompt}{hint}: ")
    if schema:
        try:
            return json.loads(raw)
        except ValueError:
            return raw
    return raw


def publish_state(key: str, value: Any) -> None:
    """Store ``value`` (any JSON, up to 64 KB) under ``key`` for this run,
    readable with ``GET /api/runs/{id}/state`` outside the flow.

    This is flow-level state: it is not scoped to the current task, and it
    survives retries and resumes.
    """
    run = context.current_run()
    if run is None or run.backend is None:
        raise CereyanError("publish_state needs a running flow")
    try:
        text = json.dumps(value)
    except (TypeError, ValueError) as exc:
        raise CereyanError(f"publish_state values must be JSON: {exc}") from None
    if len(text.encode()) > 64 * 1024:
        raise CereyanError(f"publish_state value for {key!r} exceeds 64 KB")
    run.backend.task_state_set("", key, text)


def pause_details(exc: RunPaused) -> dict:
    """The state details recorded when a run pauses: the prompt, its schema, the time asked, the asking task run, and which question is waiting."""
    details = {
        "prompt": exc.prompt,
        "schema": exc.schema,
        "asked_at": int(time.time() * 1_000_000),
        "task_run": exc.task_run,
        "index": exc.index,
    }
    details.update(getattr(exc, "details", None) or {})
    return details
