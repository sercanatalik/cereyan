"""Human-in-the-loop: pause a run until someone answers."""

from __future__ import annotations

import json
import sys
import time
from typing import Any

from . import context
from .exceptions import CereyanError, RunPaused


def wait_for_input(prompt: str, schema: dict | None = None) -> Any:
    """Return the answer given to this run, pausing it when there is none yet.

    Inside a served run the first call transitions the run to ``Paused`` with
    the prompt and ends the attempt; the engine is free while the run waits.
    ``POST /api/runs/{id}/resume`` (or the Resume button, or the MCP
    ``resume_run`` tool) stores the answer and schedules a new attempt, which
    reruns the flow from the top and gets the answer from this call. Tasks
    marked ``cache=INPUTS`` are skipped on the replay.

    Outside a served run the answer is read from the terminal, or an error is
    raised when stdin is not interactive.
    """
    run = context.current_run()
    if run is None or run.backend.offline:
        return _prompt_terminal(prompt, schema)
    stored = run.backend.get_input()
    if stored is not None:
        return stored
    task = context.current_task_run()
    raise RunPaused(prompt, schema, task.id if task else None)


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
    """The state details recorded when a run pauses: the prompt, its schema, the time asked, and the asking task run."""
    return {
        "prompt": exc.prompt,
        "schema": exc.schema,
        "asked_at": int(time.time() * 1_000_000),
        "task_run": exc.task_run,
    }
