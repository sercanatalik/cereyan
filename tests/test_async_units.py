"""Async units: one task run per coroutine, on the caller's event loop, replayed like a task."""

from __future__ import annotations

import asyncio
import json

import pytest

from cereyan import flow
from cereyan.engine.runner import run_async_unit


def last_run(store):
    run = json.loads(store.list_runs())["items"][0]
    return run, json.loads(store.task_runs(run["id"]))


def unit(name, value, calls, *, input_hash="h", fail=None, delay=0.0, **kw):
    async def body():
        calls.append(name)
        if delay:
            await asyncio.sleep(delay)
        if fail is not None and fail():
            raise RuntimeError(f"{name} failed")
        return value

    return run_async_unit(name, f"test:{name}", body, input_hash=input_hash, **kw)


def test_units_replay_on_a_later_pass(store):
    calls: list[str] = []
    seen = []

    @flow(retries=1, retry_delay=0, checkpoint=True)
    def agent_like():
        async def main():
            a = await unit("model", {"text": "hi"}, calls)
            b = await unit("tool", [1, 2], calls)
            return a, b

        out = asyncio.run(main())
        if not seen:
            seen.append(1)
            raise RuntimeError("fail after the units")
        return out

    assert agent_like() == ({"text": "hi"}, [1, 2])
    assert calls == ["model", "tool"]
    _, tasks = last_run(store)
    by = {(t["pass"], t["dynamic_key"]): t for t in tasks}
    assert by[(0, "model-0")]["result_ref"].startswith("ckpt-")
    assert by[(1, "model-0")]["state"]["name"] == "Replayed"
    assert by[(1, "tool-0")]["state"]["name"] == "Replayed"


def test_a_changed_input_hash_ends_replay(store):
    calls: list[str] = []
    seen = []

    @flow(retries=1, retry_delay=0, checkpoint=True)
    def diverging():
        async def main():
            await unit("model", 1, calls, input_hash=f"prompt-{len(seen)}")
            await unit("tool", 2, calls)

        asyncio.run(main())
        if not seen:
            seen.append(1)
            raise RuntimeError("again")

    diverging()
    # The model request asked something else on the retry, so it and everything after it ran again.
    assert calls == ["model", "tool", "model", "tool"]


def test_retries_and_timeouts_are_recorded_like_tasks(store):
    calls: list[str] = []
    tries = []

    def flaky():
        tries.append(1)
        return len(tries) == 1

    @flow
    def retrying():
        return asyncio.run(unit("flaky", "ok", calls, fail=flaky, retries=1))

    assert retrying() == "ok"
    _, tasks = last_run(store)
    assert tasks[0]["state"]["type"] == "Completed" and calls == ["flaky", "flaky"]
    assert tasks[0]["failure_count"] == 1

    @flow
    def slow():
        return asyncio.run(unit("slow", 1, calls, delay=5, timeout=0.05))

    with pytest.raises(Exception):
        slow()
    _, tasks = last_run(store)
    assert tasks[0]["state"]["type"] == "Failed" and tasks[0]["state"]["name"] == "TimedOut"


def test_gathered_units_are_numbered_in_call_order(store):
    calls: list[str] = []

    @flow
    def fan_out():
        async def main():
            # The first call finishes last; numbering follows the calls, not completion.
            return await asyncio.gather(
                unit("lookup", "a", calls, delay=0.05),
                unit("lookup", "b", calls, delay=0.01),
                unit("lookup", "c", calls),
            )

        return asyncio.run(main())

    assert fan_out() == ["a", "b", "c"]
    _, tasks = last_run(store)
    assert sorted(t["dynamic_key"] for t in tasks) == ["lookup-0", "lookup-1", "lookup-2"]


def test_a_cancelled_unit_is_recorded_cancelled(store):
    calls: list[str] = []

    @flow
    def cancelling():
        async def main():
            pending = asyncio.ensure_future(unit("waits", 1, calls, delay=5))
            await asyncio.sleep(0.05)
            pending.cancel()
            with pytest.raises(asyncio.CancelledError):
                await pending

        asyncio.run(main())

    cancelling()
    _, tasks = last_run(store)
    assert tasks[0]["state"]["type"] == "Cancelled"


def test_a_unit_needs_a_run():
    with pytest.raises(Exception, match="needs a run"):
        asyncio.run(unit("alone", 1, []))


# ---- once units --------------------------------------------------------------------


def _in_doubt_flow(name, **kw):
    """A flow whose run says `name-0` was left in doubt by an earlier run."""
    from cereyan import context

    calls: list[str] = []

    @flow
    def f():
        context.current_run().in_doubt.add(f"{name}-0")
        return asyncio.run(unit(name, "executed", calls, once=True, **kw))

    return f, calls


def test_an_in_doubt_unit_with_a_probe_value_is_recovered(store):
    async def probe():
        return "found"

    f, calls = _in_doubt_flow("charge", probe=probe)
    assert f() == "found" and calls == []
    _, tasks = last_run(store)
    assert tasks[0]["state"]["name"] == "Recovered" and tasks[0]["once"] is True


def test_an_in_doubt_unit_whose_probe_finds_nothing_executes(store):
    from cereyan import NOT_DONE

    f, calls = _in_doubt_flow("charge", probe=lambda: NOT_DONE)
    assert f() == "executed" and calls == ["charge"]


def test_a_raising_probe_defers(store):
    def probe():
        raise RuntimeError("cannot tell")

    f, calls = _in_doubt_flow("charge", probe=probe, defer=lambda: {"deferred": True})
    assert f() == {"deferred": True} and calls == []
    _, tasks = last_run(store)
    assert tasks[0]["state"]["type"] == "Completed" and tasks[0]["state"]["details"]["in_doubt"] == "deferred"


def test_an_in_doubt_unit_nothing_can_settle_fails(store):
    from cereyan import TaskInDoubt

    f, calls = _in_doubt_flow("charge")
    with pytest.raises(TaskInDoubt):
        f()
    assert calls == []


def test_a_once_unit_not_in_doubt_runs_and_records_once(store):
    calls: list[str] = []

    @flow
    def f():
        return asyncio.run(unit("charge", "executed", calls, once=True))

    assert f() == "executed" and calls == ["charge"]
    _, tasks = last_run(store)
    assert tasks[0]["once"] is True
