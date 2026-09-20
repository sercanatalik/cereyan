# How to wait without holding an engine

A flow often has to wait: an hour for a rate limit to reset, for a file another system writes, for an event another flow emits. A plain `time.sleep` or a polling loop holds the engine and the flow's resources for the whole wait, and a crash in the middle loses it. The durable waits end the attempt instead, free the engine, and bring the run back when there is something to do; the new attempt replays the tasks that already completed from their [checkpoints](../concepts/runs-and-states.md#checkpoints) and continues after the wait.

## Sleep

```python
from cereyan import flow, sleep, task

@task
def request_export() -> str:
    return "export-42"

@flow
def export() -> str:
    job = request_export()
    sleep(900)          # the run shows Sleeping; the engine is free
    return job
```

`sleep(seconds)` blocks for short waits and pauses the run as `Sleeping` from `min_durable` seconds (60 by default) with a wake time the server's timer keeps, also across a restart. `sleep_until(when)` names the time. Offline both block.

## Wait for an event

```python
from cereyan import WaitTimeout, flow, wait_for_event

@flow
def after_orders(day: str) -> str:
    try:
        event = wait_for_event("orders.ready", match={"day": day}, within=3600)
    except WaitTimeout:
        return "no orders today"
    return event["payload"]["day"]
```

The run pauses as `AwaitingEvent` until an event named `orders.ready` (or matching `orders.*`) whose payload carries the `match` keys is recorded, by a flow's `emit_event`, a custom route, or `POST /api/events`; the event comes back from the call. `within` bounds the wait and raises `WaitTimeout` on replay. Offline the local store is polled for `within` seconds, which is required there.

## Wait for a target

```python
from cereyan import LocalTarget, flow, wait_for_target

@flow
def load(day: str) -> str:
    wait_for_target(LocalTarget(f"/drop/{day}.csv"), poke=300, timeout=6 * 3600)
    return day
```

`wait_for_target` returns at once when the [target](idempotent-reruns.md) exists; otherwise the run pauses as `AwaitingTarget` and is poked every `poke` seconds, each poke replaying the body to this call, until the target exists or `timeout` passes.

## Snooze

```python
from cereyan import Snooze, flow, task

@task
def warehouse_ready() -> bool:
    return True

@flow
def nightly() -> str:
    if not warehouse_ready():
        raise Snooze(600)   # not a failure: the body runs again in ten minutes
    return "loaded"
```

`raise Snooze(seconds)` from a flow or a task ends the attempt as `Sleeping` without touching `failure_count` and reruns the body after the wait; `details.snoozes` on the run counts how often it did. Offline the run sleeps in process and reruns.

## What the run shows

While it waits the run page's banner says what it waits for and offers **Wake now**, which resumes the run at once; `run.paused` and `run.resumed` events mark both moments, and the same rules that page on a paused question apply. Waits belong in the flow body: from a task submitted to a worker thread they end the task, not the run.

Related: [Pause a run for approval](human-approval.md), [Runs and states](../concepts/runs-and-states.md).
