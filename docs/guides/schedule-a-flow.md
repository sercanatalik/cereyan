# How to schedule a flow

Add `schedule=` to the flow and serve it. The server creates runs at each fire time; nothing fires offline.

## Declare the schedule in code

```python
from datetime import date, timedelta
from cereyan import flow, Cron, Interval

@flow(schedule=Cron("30 6 * * *", timezone="Europe/Istanbul"))
def morning_load(day: date = date.today()) -> str:
    return str(day)

@flow(schedule=Interval(timedelta(minutes=15)), max_concurrent=1, on_overlap="skip")
def poll_queue() -> None:
    ...

assert morning_load.schedules[0]["kind"] == "cron"
assert poll_queue.schedules[0]["interval"] == 900
```

Then:

<!-- notest: shell command that blocks -->
```{.python notest}
cereyan serve pipelines/
```

The flow page shows the schedule summary and next fire time, the **Upcoming** tab lists the runs materialised ahead, and the dashboard's Upcoming panel shows the next fires across flows.

## Pick the kind

| Want | Declare |
|---|---|
| Wall-clock times | `Cron("0 9 * * 1-5", timezone="Europe/Istanbul")` |
| Every N seconds or minutes | `Interval(timedelta(minutes=15))`; add `anchor=datetime(...)` to align the grid |
| Calendar rules | `RRule("DTSTART:20260101T090000\nRRULE:FREQ=MONTHLY;BYDAY=-1FR", timezone="UTC")` |

Several schedules can be given as `schedules=[...]`. Each takes `catchup` (`skip`, `latest`, `all`) and `catchup_max` for what happens to fires missed while the server was down.

## Keep runs from piling up

A slow flow on a fast schedule needs a policy. `max_concurrent=1` with `on_overlap="skip"` drops a fire while the previous run is still going; `"enqueue"` (the default) lets it wait; `"cancel_new"` records it as cancelled. See [Limit concurrency and overlap](resources-and-overlap.md).

## Pause and resume

Pause from the flow page, `POST /api/schedules/{id}/pause`, the MCP `pause_schedule` tool, or a rule's `pause_schedule` action. Pausing removes the schedule's not-yet-started runs; resuming materialises them again. `disable_after=(count, window, persist)` pauses automatically after repeated failures.

## Inspect through the API

```{.python fixture:served}
flow = next(f for f in served.client.flows() if f["name"] == "daily_etl")
schedules = served.client.schedules(flow["id"])
assert schedules and schedules[0]["schedule"]["kind"] == "cron"
upcoming = served.client.upcoming(flow["id"])
assert len(upcoming) >= 1
```

## Edit at runtime

The flow page's schedule editor changes a code-declared schedule in place and previews the next fire times (`POST /api/schedules/preview`). Edits last until the next restart re-applies the code declaration, unless marked *persist*.

Related: [Schedules](../concepts/schedules.md), [Backfill a date range](backfill.md).
