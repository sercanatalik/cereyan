# How to limit concurrency and overlap

Three controls, from narrow to wide: a cap on one flow, named resources shared by many, and the engine pool.

## Cap one flow

```python
from cereyan import flow

@flow(max_concurrent=1, on_overlap="skip")
def every_ten_minutes() -> None:
    ...

@flow(max_concurrent=2)
def exports() -> None:
    ...

assert every_ten_minutes.options["on_overlap"] == "skip"
```

`max_concurrent` limits how many runs of the flow are Pending or Running at once. When the cap is reached, `on_overlap` decides: `enqueue` (default) waits as `AwaitingResource`, `skip` ends the new run `Skipped`, `cancel_new` ends it `Cancelled`. Pick `skip` for polling flows where a missed tick does not matter and `enqueue` for flows that must not lose work.

A run that finds no slot is still created for its tick, so every scheduled time appears in history. Under `skip` and `cancel_new` it ends at once with the message "previous run still active" (and a `run.skipped` event for `skip`); under `enqueue` it stays Scheduled, turns Late once its time passes, and starts in scheduled-time order as soon as a run of the flow ends. The reference for this behaviour is the overlap soak, `just soak` in [Contributing](../contributing.md), which runs fifteen such flows for an hour and checks that caps hold, controls never skip, and queues drain in order.

## Share a resource

Declare totals in `cereyan.toml` or on the Settings page, and claim units on flows or tasks:

```toml
[resources]
db = 4
gpu = 1
```

```python
from cereyan import flow, task

@task(resources={"db": 1})
def query(sql: str) -> str:
    return sql

@flow(resources={"gpu": 1})
def train() -> str:
    return query("select 1")

assert train() == "select 1"
```

A run or task run waits as `AwaitingResource` until every resource it declares has capacity, and a `resource.exhausted` event records the first wait. Units are released on every terminal state, on pause, and when an engine dies. Resources are global to the machine, so two projects declaring `db` share the same four units. A resource that is not declared in configuration has no limit.

## Order the queue

`priority` (higher first) decides which waiting run gets the next free engine or unit. It never preempts a running run. A negative priority also lowers the engine's OS scheduling priority on Linux and macOS, useful for background reprocessing:

```python
from cereyan import flow

@flow(priority=-10)
def reprocess_history() -> None:
    ...
```

## Bound the pool

`max_engines` in `[server]` or `--max-engines` caps engine processes, and therefore runs executing at once, machine-wide. The default is the CPU count.

## Stop a failing flow

```python
from cereyan import flow

@flow(disable_after=(3, 3600, 86400))
def nightly() -> None:
    ...
```

Three failures within an hour pause the flow's schedules for a day, with `flow.disabled` and `flow.enabled` events marking the window. Manual runs still work.

## Check what is waiting

The Runs page filter `AwaitingResource`, the run's Details tab (which names the resource), and `GET /api/counts` show what is blocked. The Settings page shows each resource's total and in-use count.

Related: [Resources and concurrency](../concepts/resources-and-concurrency.md), [Schedule a flow](schedule-a-flow.md).
