# Events and rules

```python
from cereyan import App, emit_event, events, states

app = App("shop")

@app.rule(on="orders.*", flow="check_orders", once="per_run")
def alert(event, run):
    print("empty table:", event["payload"]["table"], "in run", run["name"])

@app.rule(on=events.run.failed, states=[states.Failed])
def on_failure(event, run):
    print("failed:", run["name"])

@app.flow
def check_orders() -> None:
    emit_event("orders.table_empty", {"table": "orders"})

spec = app.rules[0].spec()
assert spec["when"]["events"] == ["orders.*"]
assert spec["do"][0]["kind"] == "call"
assert events.run.failed == "run.failed"
```

An **event** is a recorded fact: a name such as `run.failed`, a resource it is about, related resources, a payload, and a sequence number. The engine records one for every meaningful change (run and task-run transitions, schedule changes, rule firings, flow registration, resource exhaustion, expectations), and `emit_event` records custom ones. The [events catalogue](../reference/events.md) lists them all.

A **rule** is `when` plus `do`: a match clause over events and an ordered list of actions, with guards. Rules are how cereyan reacts: run a cleanup flow when an ETL completes, page someone when a nightly flow fails, cancel a run that was started by mistake.

## Matching

`when` names event names or prefixes (`run.*`), flows, tags, states, and a project. A rule fires for an event that matches every clause it sets.

A value in `states` matches the run's state *type* or its sub-state *name*, so `states=["Scheduled"]` covers a run that is `Late` or `AwaitingRetry` and `states=["Late"]` narrows to just that one. See [States and transitions](../reference/states.md).

### Names are checked

The engine owns the prefixes `run.`, `task_run.`, `flow.`, `schedule.`, `resource.`, `rule.`, and `expectation.`. A name under one of them that the engine never emits — `run.failure` for `run.failed` — is rejected when the rule is declared, rather than sitting silent forever:

```python
import pytest
from cereyan import App

with pytest.raises(ValueError, match='did you mean "run.failed"'):

    @App("typo").rule(on="run.failure")
    def never(event, run):
        ...
```

Every other name is yours and is never checked, so `on="orders.table_empty"` needs no registration. `cereyan.events` and `cereyan.states` carry the catalogue if you would rather not type the strings: `events.run.failed` *is* `"run.failed"`, and `events.run.any` is `"run.*"`.

## Actions

| Action | Effect |
|---|---|
| `run_flow` | Create a run of a flow with templated parameters |
| `cancel_run` | Cancel the run the event is about |
| `set_state` | Force the run into a state |
| `pause_schedule`, `resume_schedule` | Stop or restart a schedule |
| `webhook` | HTTP request with a templated body; three attempts with backoff |
| `email` | SMTP through `[email]` in `cereyan.toml` |
| `call` | A code rule's function, `fn(event, run)` |

Templates use Jinja syntax rendered in Rust with `event`, `run`, `flow`, `state`, `payload`, and `parameters` in scope. An undefined variable is an error; a failing template fails only its action and is recorded as `rule.action.failed`.

## Guards

Disabled rules never fire. `once=per_run` (the default) fires at most once per run and `once=never` drops that limit; `cooldown_seconds` and `max_per_minute` throttle; and a rule never fires on events of runs it created unless `allow_self` is set, so a rule that runs a flow cannot trigger itself forever. A keyword that is not one of these is rejected rather than ignored, so a misspelled guard cannot leave a rule running on defaults.

## Two kinds of rule

**Data rules** are created on the Rules page or through `POST /api/rules`, stored in the database, and editable at runtime. **Code rules** come from `@app.rule(...)`, run their function as a `call` action, are re-registered on every server start, and show read-only in the UI. Once a server has registered them in the store they also fire on the offline path for a script's own events; a script whose rules have never been served records the events but does not fire the rules.

## Proactive rules

A reactive rule fires when something happens. A proactive rule fires when something expected does *not* happen: add `unless` naming the expected event, and either `within` seconds of the arming `on` event (event-armed) or `at` a cron expression in timezone `tz` (clock-armed). A lapse is recorded as an `expectation.lapsed` event and the rule's actions run against it. See [Detect when something did not happen](../guides/detect-missing-events.md).

## Where events go

The Events page and `GET /api/events` filter by name prefix, resource, flow, run, and time with keyset pagination; the SSE stream delivers new ones as `event.created`; the MCP `list_events` tool reads them; and retention deletes events older than `retain_days`.

Related: [React to events with rules](../guides/rules.md), [Artifacts](artifacts.md).
