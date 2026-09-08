# How to make reruns idempotent with targets

Declare where a task writes, and cereyan skips the task when that output already exists. Rerunning a day, a failed run, or a whole backfill then only does the missing work.

## Declare a target

```python
from datetime import date
from cereyan import flow, task, LocalTarget

writes = []

@task(output=lambda day: LocalTarget(f"data/{day}.csv"))
def export(day: date) -> None:
    writes.append(day)
    with LocalTarget(f"data/{day}.csv").open("w") as fh:
        fh.write("id,amount\n1,10\n")

@flow
def daily(day: date) -> None:
    export(day)

daily(date(2026, 3, 1))
daily(date(2026, 3, 1))
assert writes == [date(2026, 3, 1)]   # the second run skipped export
```

`output=` takes a Target, or a callable over the task's arguments returning one. Only the parameters named in the callable's signature are passed, so `lambda day: ...` works for a task with more arguments. When `exists()` is true the task run ends `Skipped` with the message `output exists`, records `task_run.skipped`, and the body never runs.

## Write atomically

`LocalTarget.open("w")` writes to a temporary file next to the target and renames it into place on close. A crash mid-write leaves no partial file, so the next run does not mistake half an output for a finished one. Use `temporary_path()` when a library insists on writing a path itself:

```python
from cereyan import LocalTarget

target = LocalTarget("data/report.txt")
with target.temporary_path() as tmp:
    with open(tmp, "w") as fh:
        fh.write("done")
assert target.exists()
```

## Custom targets

Anything with an `exists()` method is a target: a row in a table, a key in an object store, a flag in an API.

```python
from datetime import date
from cereyan import flow, task

class TableTarget:
    done: set = set()

    def __init__(self, name: str) -> None:
        self.name = name

    def exists(self) -> bool:
        return self.name in TableTarget.done

@task(output=lambda day: TableTarget(f"sales_{day}"))
def build(day: date) -> None:
    TableTarget.done.add(f"sales_{day}")

@flow
def nightly(day: date) -> None:
    build(day)

nightly(date(2026, 3, 2))
nightly(date(2026, 3, 2))
```

## Skip whole runs

Targets skip tasks, not runs. To avoid scheduling runs for work already done, give the flow `bulk_complete=` and let [backfills](backfill.md) skip those values before they are created.

## Rerun on purpose

To recompute, delete the target (`LocalTarget.remove()`) and run again; there is no force flag, because the file is the truth.

Related: [Targets, caching and results](../concepts/targets-caching-results.md), [Cache task results](cache-results.md).
