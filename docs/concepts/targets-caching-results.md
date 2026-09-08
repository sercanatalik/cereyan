# Targets, caching and results

```python
from datetime import date, timedelta
from cereyan import flow, task, LocalTarget, INPUTS

calls = []

@task(output=lambda day: LocalTarget(f"out/{day}.csv"))
def export(day: date) -> None:
    calls.append("export")
    with LocalTarget(f"out/{day}.csv").open("w") as fh:
        fh.write("a,b\n1,2\n")

@task(cache=INPUTS, persist_result=True, cache_expires=timedelta(hours=1))
def summarize(day: date) -> dict:
    calls.append("summarize")
    return {"day": str(day), "rows": 1}

@flow
def daily(day: date) -> dict:
    export(day)
    return summarize(day)

first = daily(date(2026, 1, 1))
second = daily(date(2026, 1, 1))
assert first == second
assert calls == ["export", "summarize"]   # the second run skipped export and hit the cache
```

Cereyan treats your code and the files it writes as the source of truth, and its own database as a cache of history. Two mechanisms make reruns cheap and safe: **targets** for outputs that live outside cereyan, and **result caching** for return values.

## Targets

A **Target** is anything with `exists()`. `LocalTarget(path)` is a file: it writes through a temporary path that is renamed onto the target on close, so a reader never sees a partial file and a crash leaves no half-written output.

Declare a task's target with `output=`: a Target, or a callable over the task's arguments returning one. When the target exists the task run ends `Skipped` with the message `output exists` and its body does not execute. That is what makes rerunning a day, or a whole backfill, idempotent.

Targets say nothing about *how* the file is written: the task body still has to write it, usually through the same `LocalTarget`'s `open("w")`.

## Result caching

With `persist_result=True` a task's return value is stored under `<home>/storage/<key>` (pickle by default, `serializer="json"` for JSON) along with the Python version. `cache=` then reuses it:

| Policy | Hit when |
|---|---|
| `INPUTS` | The arguments are unchanged |
| `SOURCE` | The task's source code is unchanged |
| `INPUTS + SOURCE` | Both |

A hit ends the task run `Cached` with the stored result and records `task_run.cached`. `cache_expires=timedelta(...)` makes entries stale after that long, and a result written by a different Python minor version counts as a miss. Caching requires `persist_result=True`; the decorator rejects the combination otherwise.

## Replay after a pause

A run resumed after `wait_for_input` starts a new attempt from the top of the flow. Tasks marked `cache=INPUTS` return their stored results on the replay, so work done before the question is not repeated. See [Pause a run for approval](../guides/human-approval.md).

## Where results live

`storage/` is inside the runtime home, is never cleaned by retention, and is safe to delete: a missing entry is a cache miss. Results are not the way to pass data between flows; write a target and read it downstream.

Related: [Make reruns idempotent with targets](../guides/idempotent-reruns.md), [Cache task results](../guides/cache-results.md), [Backfills](backfills.md).
