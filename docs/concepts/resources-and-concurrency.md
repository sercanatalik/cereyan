# Resources and concurrency

```python
from cereyan import flow, task

@task(resources={"db": 1})
def load(rows: int) -> int:
    return rows

@flow(max_concurrent=1, on_overlap="skip", priority=5, resources={"gpu": 1})
def nightly(rows: int = 10) -> int:
    return load(rows)

assert nightly() == 10
assert nightly.options["max_concurrent"] == 1
```

A **resource** is a named counting semaphore with a total set in `cereyan.toml` (`[resources]`) or on the Settings page. Flows and tasks declare how much of a resource each run holds; a run whose resources are not available waits as `AwaitingResource` and is dispatched as soon as they are. Resources are released on every terminal state, on pause, and when an engine dies, so a crash cannot leak capacity. They are shared by every project on the machine.

Offline, a resource is a local semaphore of size one, which serialises tasks holding the same name inside a single process.

## Per-flow cap and overlap

`max_concurrent=N` is a resource named after the flow with total `N`: at most `N` runs of the flow are Pending or Running at once. The default is unlimited. `on_overlap` decides what a new run does when the cap is reached:

| Value | Behaviour |
|---|---|
| `enqueue` (default) | Wait as `AwaitingResource` and start when a slot frees |
| `skip` | End `Skipped` at once with the message `previous run still active` and a `run.skipped` event |
| `cancel_new` | End `Cancelled` with the same message |

## Priority

`priority` orders dispatch among runs waiting for an engine or a resource, higher first. It never preempts: a running run is never paused or killed to make room. A negative priority also raises the engine's OS niceness (`min(19, -priority)`) on Linux and macOS, so heavy background flows yield CPU to interactive work; such engines are pooled by their niceness and count toward `max_engines`.

## Disable windows

`disable_after=(count, window_seconds, persist_seconds)` pauses every schedule of the flow for `persist_seconds` once it has failed `count` times within `window_seconds`, records `flow.disabled`, and resumes automatically with `flow.enabled`. Use it to stop a broken nightly flow from filling the run list until someone looks.

## Engines

The number of runs executing at once is also bounded by the engine pool: `max_engines` (default: CPU count) engine processes, each running one run at a time. Resources and caps decide *which* runs may proceed; the pool decides *how many*. See [Engines and the home directory](engines-and-home.md).

## Backfills

A backfill has its own resource, sized by its `concurrency`, so a large date range never floods the pool; see [Backfills](backfills.md).

Related: [Limit concurrency and overlap](../guides/resources-and-overlap.md), [Schedules](schedules.md).
