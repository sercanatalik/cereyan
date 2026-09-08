# How to backfill a date range

Create one run per day (or hour, or any step) between two dates, with a concurrency limit, and let targets and `bulk_complete` skip what is already done. Backfills need a running server.

## From the CLI

<!-- notest: shell command against a running server -->
```{.python notest}
cereyan backfill daily_etl --param day --start 2026-06-01 --end 2026-08-30 --concurrency 4
cereyan backfill hourly --param at --start 2026-09-01T00:00 --end 2026-09-01T23:00 --interval 1h --reverse
cereyan backfill daily_etl --param day --start 2026-06-01 --end 2026-06-30 --extra region=eu --json
```

The flow is named as `flow` or `project/flow` when the name exists in several projects. The command prints the backfill id, the number of runs, and the tag; `--json` prints the status object.

## From the UI, the API, and an agent

The flow page's **Backfill** button opens a dialog with the same fields. `POST /api/flows/{id}/backfill` and `Client.backfill()` take `parameter`, `start`, `end`, `interval`, `concurrency`, `extra_parameters`, and `reverse`:

```{.python fixture:served}
flow = next(f for f in served.client.flows() if f["name"] == "etl")
status = served.client.backfill(flow["id"], "day", "2026-01-01", "2026-01-03", concurrency=2)
assert status["total"] == 3
assert status["tag"].startswith("backfill:")
runs = served.client.runs(tags=status["tag"])["items"]
for run in runs:
    served.wait_run(run["id"])
assert served.client.backfill_status(status["id"])["total"] == 3
```

The MCP `backfill` tool does the same and dry-runs by default, so an agent sees how many runs it would create before creating them.

## Skip work that is done

Two mechanisms, used together:

- A task with `output=` skips itself when its target exists, so a run over a finished day costs one process dispatch and no work.
- A flow with `bulk_complete=` never creates runs for finished values: the backfill calls it once with every value and records the returned ones as `Skipped` without dispatching them.

```python
from datetime import date
from cereyan import flow, task, LocalTarget

def finished(values: list[date]) -> set[date]:
    return {d for d in values if LocalTarget(f"out/{d}.csv").exists()}

@task(output=lambda day: LocalTarget(f"out/{day}.csv"))
def build(day: date) -> None:
    with LocalTarget(f"out/{day}.csv").open("w") as fh:
        fh.write("...")

@flow(bulk_complete=finished)
def daily_etl(day: date) -> None:
    build(day)
```

## Watch and cancel

Runs carry the tag `backfill:<id>`; filter the Runs page by it. `GET /api/backfills/{id}` reports counts by state, and `POST /api/backfills/{id}/cancel` (or the Cancel button) cancels the remaining Scheduled and Running runs. The backfill's own resource keeps it to `concurrency` runs at once, and every run still respects the flow's `max_concurrent` and resources.

Related: [Backfills](../concepts/backfills.md), [Make reruns idempotent with targets](idempotent-reruns.md).
