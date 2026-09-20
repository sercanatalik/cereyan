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
| RRule | `RRule("DTSTART:...\nRRULE:...", timezone=...)` | An iCalendar recurrence rule set; must include `DTSTART`. A `DTSTART` that ends in `Z` or carries `TZID=` keeps that zone and `timezone` only affects how fires are shown; one with neither is read in `timezone`. |

Timezones are IANA names and default to the machine's local zone. Each schedule may carry a `key` so a code declaration and its runtime edits stay matched; keys default to `code-0`, `code-1`, and so on.

## How the scheduler works

- For every active schedule it keeps at least three future runs materialised (and at least one hour of coverage, at most 100 runs), so the Upcoming tab and the dashboard show what comes next. The runs sit in `Scheduled` until their time.
- It wakes on a timer for the earliest due run, not by polling, and dispatches within 50 milliseconds of the scheduled time when an engine is free.
- A run that has not started 15 seconds after its time is renamed `Late` and a `run.late` event is recorded; it still runs as soon as it can.
- Pausing a schedule removes its not-yet-started runs; resuming materialises them again.

## Skipping fires

Skip a fire when one run should not happen but the schedule should stay on: from the Flows page menu (**Skip next run**, **Skip runs…**), the flow page's **Upcoming** tab, or `POST /api/schedules/{id}/skips` with a list of `fires` or `{"next": N}`. A skip names one fire time of one schedule, at most 100 fires ahead, and lasts until that time passes: it survives a restart, a pause and resume, and an edit that still produces the time. At its time the fire's run ends `Skipped` with `details.reason = "user"` without starting, shows in Runs, and records `run.skipped`; until then **Undo** or `DELETE /api/schedules/{id}/skips/{fire}` takes it back. The look-ahead keeps three runs that will start, so it reaches past skipped fires, and the flows that run after this one are skipped for that fire too (see [Dependencies](dependencies.md)).

## Daylight saving time

Twice a year a timezone loses an hour and gains one back, so a wall-clock time can be missing or can happen twice. A schedule that fires by wall clock resolves both the same way.

| Schedule | A time the clocks skipped | A time that happens twice |
|---|---|---|
| Cron | Fires at the first instant after the gap, on the same day | Fires once, at the earlier instant |
| Interval of a day or more | Fires at the first instant after the gap, on the same day | Fires once, at the earlier instant |
| Interval under a day | Not affected: elapsed time, not wall clock | Not affected |
| RRule | Follows the recurrence rule's own instants | Follows the recurrence rule's own instants |

A daily schedule at 02:30 `America/New_York` on the day the clocks jump from 02:00 to 03:00 therefore fires at 03:00, and the day is never missing from its history. The gap is resolved by the zone's own shift, which is not always an hour: `Australia/Lord_Howe` moves thirty minutes, and a 02:15 schedule there fires at 02:30.

One fire has at most one run. A fire the look-ahead already materialised is not created again by catch-up, and the store refuses a second run for the same schedule and time, so a machine that sleeps through its own look-ahead comes back with the runs it had rather than two of each. A database that already held duplicates keeps both runs — they happened — but only the earlier one stays attached to its schedule.

## Catch-up

When the server starts after downtime, each schedule's `catchup` policy decides what happens to the fires it missed: `skip` (default) drops them, `latest` creates the most recent one, and `all` creates every one up to `catchup_max` (default 100). `catchup_window`, in seconds, drops missed fires older than that before the policy applies: a nightly report is worth catching up a day later, not a month later. A skipped fire is never caught up. Catch-up runs carry `created_by = catchup` and the decision is recorded as a `schedule.catchup` event with `missed`, `created`, `dropped`, and `expired` counts.

## Jitter and start deadlines

`jitter`, in seconds, spreads a schedule's runs: each run becomes due at its fire time plus an offset in `[0, jitter)` computed from the schedule and the fire time, so the offset is the same after a restart and the run's `scheduled_time` stays the nominal fire. Ten flows on `0 * * * *` with `jitter=300` start across five minutes instead of together. An interval schedule's jitter must be shorter than its interval.

`start_deadline`, in seconds, skips a run that has not started that long after it was due, with reason `missed_start_deadline`: useful for a poll whose value is gone once the next one is due. It can also be set on the flow for every run, including ones started by hand; the schedule's value wins. Both are set in Python (`Cron("0 * * * *", jitter=300, start_deadline=900)`), in the schedule editor, through the API, and by an agent.

## Parameters and names

A scheduled run gets the flow's default parameter values, and the schedule editor can set overrides. Use `run_name` templates over parameters, or the default scheduled name `flow-YYYYMMDDTHHMMSS`, to tell runs apart.

## Editing at runtime

Schedules declared in code can be edited on the flow page or with **Reschedule…** (`PATCH /api/schedules/{id}`). The edit lasts until the server restarts, when the code declaration applies again; send `persist: true` to keep it and detach the schedule from its declaration for good. Skips whose fire time the edited schedule no longer produces are dropped and recorded as a `schedule.skips_dropped` event, as are those a restored declaration no longer produces. `POST /api/schedules/preview` returns the next fire times for a declaration, which the editors use to show them before saving.

An agent can do the same through the [MCP tools](../reference/mcp.md): `list_schedules` shows what is scheduled and `create_schedule`, `edit_schedule`, `delete_schedule`, `pause_schedule` and `resume_schedule` manage it. Two things differ from the flow page. An edit to a schedule declared in code lasts until the next restart, and the tool says so in its result, because an agent cannot see the note the flow page shows. Deleting one is refused outright, since the declaration would recreate it at the next restart; pause it instead, or remove the declaration from the flow.

Related: [Schedule a flow](../guides/schedule-a-flow.md), [Backfills](backfills.md), [Resources and concurrency](resources-and-concurrency.md).
