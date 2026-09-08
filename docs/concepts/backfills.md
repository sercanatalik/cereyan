# Backfills

```python
from datetime import date
from cereyan import flow, task, LocalTarget

def already_done(values: list[date]) -> set[date]:
    return {d for d in values if LocalTarget(f"out/{d}.csv").exists()}

@task(output=lambda day: LocalTarget(f"out/{day}.csv"))
def build(day: date) -> None:
    with LocalTarget(f"out/{day}.csv").open("w") as fh:
        fh.write("...")

@flow(bulk_complete=already_done)
def daily(day: date) -> None:
    build(day)

assert daily.options["has_bulk_complete"]
```

A **backfill** creates one run of a flow per step of a date or datetime parameter over a range, in one transaction, and runs them under its own concurrency limit. It is how you compute history for a new pipeline, or recompute a range after a bug fix. Backfills need a running server.

## Creating one

<!-- notest: shell command against a running server -->
```{.python notest}
cereyan backfill daily --param day --start 2026-06-01 --end 2026-08-30 --interval 1d --concurrency 4
```

The same is available from the flow page's **Backfill** dialog, `POST /api/flows/{id}/backfill`, `Client.backfill()`, and the MCP `backfill` tool (which dry-runs by default and reports the count). `--interval` accepts seconds or a duration such as `1d` or `12h`; `--reverse` creates the newest value first; `--extra name=value` fixes other parameters.

## What happens

1. The values from start to end (inclusive) are enumerated.
2. If the flow defines `bulk_complete(values) -> set`, it is called once and the values it returns are recorded as `Skipped` runs without dispatching them, so a rerun over a range that is half done only executes the missing half.
3. One run per remaining value is created, tagged `backfill:<id>` and with `created_by = backfill:<id>`, all in one transaction. Ten thousand runs take under a second.
4. The runs execute through a resource named after the backfill with total `concurrency`, so they never take more engines than allowed, and each run also respects the flow's own cap and resources.

`GET /api/backfills/{id}` reports counts by state; `POST /api/backfills/{id}/cancel` cancels the remaining Scheduled and Running runs. The Runs page filters by the backfill's tag.

## Idempotency

Backfills pair with [targets](targets-caching-results.md): a task with `output=` skips days whose file exists, and `bulk_complete` avoids even scheduling them. Together they let you rerun a backfill over the same range as often as you like.

Related: [Backfill a date range](../guides/backfill.md), [Resources and concurrency](resources-and-concurrency.md).
