# How to run tasks concurrently

Call a task to run it now and wait. Submit it to run it in the background and get a `Future`. Map it to submit one task run per element.

## Submit and map

```python
from cereyan import flow, task

@task
def fetch(source: str) -> int:
    return len(source)

@task
def combine(sizes: list[int]) -> int:
    return sum(sizes)

@flow
def gather() -> int:
    futures = fetch.map(["orders", "customers", "products"])
    return combine([f.result() for f in futures])

assert gather() == 6 + 9 + 8
```

`submit(*args, **kwargs)` returns a `Future` at once; `result()` blocks until the task run ends and re-raises its exception on failure, `done()` polls, `wait()` blocks without returning the value, and `exception()` returns what was raised. `map(iterable, **static)` submits one run per element and passes the static keyword arguments to each.

## Pass futures between tasks

A future given as an argument, at any depth inside lists, tuples, sets, or dicts, is resolved before the receiving task starts, and the dependency is recorded for the timeline:

```python
from cereyan import flow, task

@task
def extract() -> list[int]:
    return [1, 2, 3]

@task
def load(rows: list[int]) -> int:
    return sum(rows)

@flow
def pipeline() -> int:
    rows = extract.submit()
    return load.submit(rows).result()

assert pipeline() == 6
```

Use `wait_for=[future, ...]` on a call or submission to order tasks that share no data:

```python
from cereyan import flow, task

order = []

@task
def first() -> None:
    order.append("first")

@task
def second() -> None:
    order.append("second")

@flow
def ordered() -> None:
    f = first.submit()
    second(wait_for=[f])

ordered()
assert order == ["first", "second"]
```

## Choose a runner

The flow's `runner` decides where submitted tasks execute.

| Runner | Use for | Notes |
|---|---|---|
| `ThreadRunner(max_workers)` (default) | I/O-bound work: HTTP ([fetching from an API](fetch-from-an-api.md)), databases, files | Shares the process; the run context propagates. A task that waits on a child while every worker is busy gets a warning and a temporary extra worker, so nested waits cannot deadlock. |
| `ProcessRunner(max_workers)` | CPU-bound work | Each task run is a spawned process; arguments and results must be picklable and the task must be importable from a module (not defined in `__main__` or a notebook). A timeout terminates the worker. |

```python
from cereyan import flow, task, ThreadRunner

@task
def work(i: int) -> int:
    return i * 2

@flow(runner=ThreadRunner(max_workers=8))
def wide() -> list[int]:
    return [f.result() for f in work.map(range(10))]

assert wide() == [i * 2 for i in range(10)]
```

`max_workers` defaults to the CPU count. Concurrency inside a run is separate from concurrency between runs, which [resources and caps](resources-and-overlap.md) govern.

## Timeouts and retries still apply

Each submitted task run has its own `retries` and `timeout_seconds`; a failure surfaces when you call `result()`.

Related: [Tasks](../concepts/tasks.md).
