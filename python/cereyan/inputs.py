"""Human-in-the-loop: pause a run until someone answers."""

from __future__ import annotations

import json
import sys
import threading
import time
from typing import Any

from . import context
from .exceptions import CereyanError, RunPaused


def wait_for_input(prompt: str, schema: dict | None = None) -> Any:
    """Return the answer given to this question, pausing the run when there is none.

    Inside a served run the first call transitions the run to ``Paused`` with
    the prompt and ends the attempt; the engine is free while the run waits.
    ``POST /api/runs/{id}/resume`` (or the Resume button, or the MCP
    ``resume_run`` tool) stores the answer and schedules a new attempt, which
    reruns the flow from the top and gets the answer from this call. Tasks
    marked ``cache=INPUTS`` are skipped on the replay.

    Questions are numbered in the order the body reaches them, and an answer
    belongs to the question it answered, so a flow can ask, resume, and ask
    again. An answer is used only when the prompt at that position still
    matches the one it was given for; a body that changed asks afresh rather
    than handing an old answer to a new question.

    Outside a served run the answer is read from the terminal, or an error is
    raised when stdin is not interactive.

    Raises:
        CereyanError: When called outside the thread executing the flow body,
            such as from a task submitted with ``submit`` or ``map``.
    """
    run = context.current_run()
    if run is None or run.backend.offline:
        return _prompt_terminal(prompt, schema)
    if threading.get_ident() != run.body_thread:
        raise CereyanError(
            f"wait_for_input({prompt!r}) belongs in the flow body: pausing works by raising out of it, "
            "and from a task on another thread that ends the task instead of the run"
        )
    index = run.next_input_index()
    stored = run.backend.get_input(index)
    if stored is not None:
        answered = stored.get("prompt")
        # An answer stored before questions were numbered has no prompt to
        # match, and answers the first question.
        if answered is None or answered == prompt:
            return stored.get("input")
    task = context.current_task_run()
    raise RunPaused(prompt, schema, task.id if task else None, index)


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


def pause_details(exc: RunPaused) -> dict:
    """The state details recorded when a run pauses: the prompt, its schema, the time asked, the asking task run, and which question is waiting."""
    return {
        "prompt": exc.prompt,
        "schema": exc.schema,
        "asked_at": int(time.time() * 1_000_000),
        "task_run": exc.task_run,
        "index": exc.index,
    }
