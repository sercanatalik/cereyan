# Tasks

```python
from cereyan import flow, task

@task(retries=2)
def fetch(n: int) -> list[int]:
    return list(range(n))

@task
def total(rows: list[int]) -> int:
    return sum(rows)

@flow
def pipeline(n: int = 4) -> int:
    return total(fetch(n))

assert pipeline() == 6
```

A **task** is a decorated function called inside a flow. Each call becomes a **task run** recorded with the run: its state, timing, retries, logs, and result. Outside a flow a task is an ordinary function call.

## Task runs and dynamic keys

Every call gets a dynamic key made of the task name and a counter, such as `fetch-0` and `fetch-1`, so the same task called twice in one run is two task runs. Task runs record which earlier task runs produced their arguments, which is what the run page's timeline and dependency views draw.

## What a task can do

| Option | Effect | Guide |
|---|---|---|
| `retries`, `retry_delay` | Rerun on failure, waiting a fixed, per-attempt, or exponential delay | [Retry, time out and survive crashes](../guides/retries-timeouts-crashes.md) |
| `timeout_seconds` | Fail the task run as `TimedOut` after the limit | same |
| `output=` | Skip the task when its target already exists | [Make reruns idempotent with targets](../guides/idempotent-reruns.md) |
| `cache=`, `cache_expires`, `persist_result`, `serializer` | Reuse a persisted result when inputs or source are unchanged | [Cache task results](../guides/cache-results.md) |
| `resources` | Hold named resources while the task runs | [Limit concurrency and overlap](../guides/resources-and-overlap.md) |
| `on_completion`, `on_failure`, `on_cancellation` | Call hooks with `(task, run, state)` | [Run code on state changes](../guides/state-hooks.md) |
| `log_prints` | Tee `print` into the run log | [Tasks](#logging) below |
| `name`, `description`, `tags` | What the UI shows | |

## Concurrency

`task.submit(...)` returns a `Future` instead of waiting, and `task.map(iterable)` submits one task run per element. Futures passed as arguments to another task are resolved before it starts, and `wait_for=` adds ordering without passing data. The flow's `runner` decides where submitted tasks execute: a `ThreadRunner` by default, or a `ProcessRunner` for CPU-bound work. See [Run tasks concurrently](../guides/concurrent-tasks.md).

```python
from cereyan import flow, task

@task
def square(x: int) -> int:
    return x * x

@flow
def squares() -> list[int]:
    return [f.result() for f in square.map([1, 2, 3])]

assert squares() == [1, 4, 9]
```

## Logging

Inside a task, `get_run_logger()` returns a logger whose records are stored with the run and tagged with the task run; standard `logging` calls from any logger are captured the same way. With `log_prints=True` on the task or the flow, `print` output is logged at INFO as well.

## Results

A task's return value flows back to the caller as usual. With `persist_result=True` it is also written under `<home>/storage`, which is what caching and replay after a pause read from.

Related: [Flows and parameters](flows-and-parameters.md), [Runs and states](runs-and-states.md).
