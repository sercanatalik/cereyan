# Schedules

```python
from datetime import datetime, timedelta
from cereyan import flow, Cron, Interval, RRule

@flow(schedule=Cron("0 9 * * 1-5", timezone="Europe/Istanbul"))
def weekday_report(day: str = "today") -> str:
    return day

@flow(schedules=[
    Interval(timedelta(hours=1), anchor=datetime(2026, 1, 1, 0, 30)),
    RRule("DTSTART:20260101T090000\nRRULE:FREQ=MONTHLY;BYMONTHDAY=1", timezone="UTC", catchup="latest"),
])
def hourly_and_monthly() -> None:
    ...

assert len(hourly_and_monthly.schedules) == 2
```

A **schedule** tells the server when to create runs of a flow. It is declared in code as above, or created and edited on the flow page. A flow can have several, each pausable on its own. Schedules only do something while a server runs; offline, they are recorded with the flow and nothing fires.

## Kinds

| Kind | Declaration | Notes |
|---|---|---|
| Cron | `Cron("0 9 * * *", timezone=..., day_or=True)` | Five fields, evaluated by wall clock in the timezone. `day_or` keeps cron's rule that day-of-month and day-of-week are ORed. |
| Interval | `Interval(seconds or timedelta, anchor=..., timezone=...)` | Fires every interval from the anchor. Intervals under a day are elapsed time; longer ones keep their local wall-clock time across DST changes. |
| RRule | `RRule("DTSTART:...\nRRULE:...", timezone=...)` | An iCalendar recurrence rule set; must include `DTSTART`. |

Timezones are IANA names and default to the machine's local zone. Each schedule may carry a `key` so a code declaration and its runtime edits stay matched; keys default to `code-0`, `code-1`, and so on.

## How the scheduler works

- For every active schedule it keeps at least three future runs materialised (and at least one hour of coverage, at most 100 runs), so the Upcoming tab and the dashboard show what comes next. The runs sit in `Scheduled` until their time.
- It wakes on a timer for the earliest due run, not by polling, and dispatches within 50 milliseconds of the scheduled time when an engine is free.
- A run that has not started 15 seconds after its time is renamed `Late` and a `run.late` event is recorded; it still runs as soon as it can.
- Pausing a schedule removes its not-yet-started runs; resuming materialises them again.

## Catch-up

When the server starts after downtime, each schedule's `catchup` policy decides what happens to the fires it missed: `skip` (default) drops them, `latest` creates the most recent one, and `all` creates every one up to `catchup_max` (default 100). Catch-up runs carry `created_by = catchup` and the decision is recorded as a `schedule.catchup` event.

## Parameters and names

A scheduled run gets the flow's default parameter values, and the schedule editor can set overrides. Use `run_name` templates over parameters, or the default scheduled name `flow-YYYYMMDDTHHMMSS`, to tell runs apart.

## Editing at runtime

Schedules declared in code can be edited on the flow page (`PATCH /api/schedules/{id}`); the code declaration wins again on the next restart unless the override is marked `persist`. `POST /api/schedules/preview` returns the next fire times for a declaration, which the editor uses to show them before saving.

An agent can do the same through the [MCP tools](../reference/mcp.md): `list_schedules` shows what is scheduled and `create_schedule`, `edit_schedule`, `delete_schedule`, `pause_schedule` and `resume_schedule` manage it. Two things differ from the flow page. Editing a schedule that was declared in code detaches it from that declaration for good, and the tool says so in its result, because an agent cannot see the badge that tells you the same thing here. Deleting one is refused outright, since the declaration would recreate it at the next restart; pause it instead, or remove the declaration from the flow.

Related: [Schedule a flow](../guides/schedule-a-flow.md), [Backfills](backfills.md), [Resources and concurrency](resources-and-concurrency.md).
