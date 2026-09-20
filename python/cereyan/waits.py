"""Durable waits: sleep, wait for an event, wait for a target, or snooze
without holding an engine.

Each wait works like `wait_for_input`: the first time the body reaches it the
run pauses with a named reason and the engine is freed; the server wakes the
run when the time, the event, or the poke comes; the new attempt replays the
body from checkpoints, reaches the same call, and continues with the answer.
Offline the calls block in process.
"""

from __future__ import annotations

import json
import threading
import time
from datetime import datetime, timezone
from typing import Any

from . import context
from .exceptions import CereyanError, RunPaused, Snooze, WaitTimeout

__all__ = ["sleep", "sleep_until", "wait_for_event", "wait_for_target", "Snooze", "WaitTimeout"]

#: Sleeps shorter than this block in the engine instead of pausing the run.
DEFAULT_MIN_DURABLE = 60.0


def _now_micros() -> int:
    return int(time.time() * 1_000_000)


def _served_run():
    run = context.current_run()
    if run is None or run.backend is None or run.backend.offline:
        return None
    if threading.get_ident() != run.body_thread:
        raise CereyanError(
            "a durable wait belongs in the flow body: pausing works by raising out of it, "
            "and from a task on another thread that ends the task instead of the run"
        )
    return run


def _ask(run, prompt: str, name: str, details: dict) -> dict | None:
    """The stored answer for this wait, or pause the run (raises)."""
    index = run.next_input_index()
    stored = run.backend.get_input(index)
    if stored is not None and stored.get("prompt") == prompt:
        answer = stored.get("input")
        return answer if isinstance(answer, dict) else {}
    task = context.current_task_run()
    raise RunPaused(prompt, None, task.id if task else None, index, name=name, details=details)


def sleep(seconds: float, *, min_durable: float = DEFAULT_MIN_DURABLE) -> None:
    """Wait ``seconds``. Inside a served run a wait of ``min_durable`` seconds
    or more pauses the run as ``Sleeping`` and frees the engine until the wake
    time; shorter waits, and every wait offline, block in process.

    Raises:
        CereyanError: From a task on another thread than the flow body.
    """
    seconds = max(0.0, float(seconds))
    run = _served_run()
    if run is None or seconds < float(min_durable):
        time.sleep(seconds)
        return
    prompt = f"sleep:{seconds:g}"
    answer = _ask(run, prompt, "Sleeping", {"wake_at": _now_micros() + int(seconds * 1_000_000), "seconds": seconds})
    del answer  # woke: nothing to return


def sleep_until(when: datetime, *, min_durable: float = DEFAULT_MIN_DURABLE) -> None:
    """Wait until ``when`` (a naive datetime is UTC); as `sleep`, durable from ``min_durable`` seconds away."""
    if when.tzinfo is None:
        when = when.replace(tzinfo=timezone.utc)
    remaining = max(0.0, when.timestamp() - time.time())
    run = _served_run()
    if run is None or remaining < float(min_durable):
        time.sleep(remaining)
        return
    prompt = f"sleep_until:{when.isoformat()}"
    _ask(run, prompt, "Sleeping", {"wake_at": int(when.timestamp() * 1_000_000), "seconds": remaining})


def wait_for_event(name: str, match: dict | None = None, within: float | None = None) -> dict:
    """Wait for an event named ``name`` (exact, or ``prefix.*``) whose payload
    carries every key of ``match`` with an equal value, and return it.

    Inside a served run the run pauses as ``AwaitingEvent`` and the server wakes
    it when such an event is recorded. ``within`` seconds bound the wait; past
    them the call raises `WaitTimeout`. Offline the local store is polled for
    ``within`` seconds, which is required there.

    Raises:
        WaitTimeout: When ``within`` passes first.
        CereyanError: Offline without ``within``, or from a task thread.
    """
    if not name or not isinstance(name, str):
        raise CereyanError("wait_for_event needs an event name")
    wanted = dict(match or {})
    run = _served_run()
    if run is None:
        return _poll_events_offline(name, wanted, within)
    prompt = f"event:{name}"
    details: dict[str, Any] = {"event": name, "match": wanted}
    if within is not None:
        details["wake_at"] = _now_micros() + int(float(within) * 1_000_000)
        details["within"] = float(within)
    answer = _ask(run, prompt, "AwaitingEvent", details)
    if answer.get("timeout"):
        raise WaitTimeout(f"no event {name!r} matched within {within:g}s")
    return answer.get("event") or {}


def _poll_events_offline(name: str, wanted: dict, within: float | None) -> dict:
    from . import engine

    if within is None:
        raise CereyanError("wait_for_event offline needs `within`: nothing else can emit while this process waits")
    store = engine.get_store()
    prefix = name[:-1] if name.endswith("*") else None
    deadline = time.time() + float(within)
    since = _now_micros()
    run = context.current_run()
    own = run.id if run is not None else None
    while True:
        page = json.loads(store.query_events(json.dumps({"limit": 200})))
        for ev in page.get("items", []):
            # Events this run emitted itself count whenever they were recorded;
            # anything else has to arrive after the wait began.
            if ev.get("timestamp", 0) < since and ev.get("run_id") != own:
                continue
            ok = ev.get("name") == name or (prefix is not None and str(ev.get("name", "")).startswith(prefix))
            payload = ev.get("payload") or {}
            if ok and all(payload.get(k) == v or (isinstance(v, str) and str(payload.get(k)) == v) for k, v in wanted.items()):
                return ev
        if time.time() >= deadline:
            raise WaitTimeout(f"no event {name!r} matched within {within:g}s")
        time.sleep(min(0.2, max(0.0, deadline - time.time())))


def wait_for_target(target, poke: float = 60.0, timeout: float | None = None):
    """Wait until ``target.exists()`` and return the target.

    Inside a served run the run pauses as ``AwaitingTarget`` and is poked every
    ``poke`` seconds: each poke replays the body to this call, which checks
    again. ``timeout`` seconds bound the wait, after which the call raises
    `WaitTimeout`. Offline the target is polled in process.
    """
    poke = max(1.0, float(poke))
    if target.exists():
        return target
    run = _served_run()
    if run is None:
        deadline = time.time() + float(timeout) if timeout is not None else None
        while not target.exists():
            if deadline is not None and time.time() >= deadline:
                raise WaitTimeout(f"target {target!r} did not appear within {timeout:g}s")
            time.sleep(min(poke, 0.5))
        return target
    prompt = f"target:{target!r}"
    index = run.next_input_index()
    stored = run.backend.get_input(index)
    now = _now_micros()
    if stored is not None and stored.get("prompt") == prompt:
        # A poke brought us back; the target is still absent (checked above).
        answer = stored.get("input") if isinstance(stored.get("input"), dict) else {}
        started = answer.get("started_at") or now
    else:
        started = now
    if timeout is not None and now >= started + int(float(timeout) * 1_000_000):
        raise WaitTimeout(f"target {target!r} did not appear within {timeout:g}s")
    wake_at = now + int(poke * 1_000_000)
    if timeout is not None:
        wake_at = min(wake_at, started + int(float(timeout) * 1_000_000))
    task = context.current_task_run()
    raise RunPaused(prompt, None, task.id if task else None, index, name="AwaitingTarget",
                    details={"target": repr(target), "poke": poke, "wake_at": wake_at, "started_at": started,
                             "timeout": timeout})
