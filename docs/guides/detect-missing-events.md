# How to detect when something did not happen

A reactive rule fires when an event arrives. A proactive rule fires when an expected event does *not* arrive: the nightly load that never finished, the run that has been going for two hours, the daily file that was not produced by nine. Add `unless` to a rule and arm it from an event or from the clock.

## Event-armed: a deadline after something starts

```python
from cereyan import App

app = App("loads")

@app.rule(on="run.running", flow="etl", unless="run.completed", within=2 * 3600)
def etl_overran(event, run):
    print(f"{run['name']} did not complete within two hours")

spec = app.rules[0].spec()
assert spec["unless"]["events"] == ["run.completed"] and spec["within"] == 7200.0
```

`on` arms an expectation when the run starts, `unless` names the event that disarms it, and `within` is the deadline in seconds. The expectation is keyed by the run (or by the flow, for events without a run). If the run completes in time the expectation is met; if it fails, or is still running at the deadline, the rule fires. Expectations are stored, so one whose deadline passed while the server was down fires once on start.

## Clock-armed: something should have happened by now

```python
from cereyan import App

app = App("loads2")

@app.rule(at="0 9 * * *", tz="Europe/Istanbul", flow="daily_load", unless="run.completed")
def daily_load_missing(event, run):
    print("no daily_load completed before nine")

spec = app.rules[0].spec()
assert spec["at"] == {"cron": "0 9 * * *", "tz": "Europe/Istanbul"}
```

At each tick of `at` the rule fires unless a matching event occurred in the look-back window: `within` seconds when given, otherwise the time since the previous tick. Ticks missed while the server was down are skipped with a log line. Clock-armed rules need a running server.

## What the rule sees

A lapse is recorded as an `expectation.lapsed` event with resource `rule/<id>`, the related run and flow, and a payload carrying `rule`, `flow`, `project`, `run`, `run_name`, `expected`, `deadline`, and `armed_at`. The rule's actions execute against that event, so templates can say:

```text
{{ event.payload.expected[0] }} did not happen for {{ flow.name }} by {{ event.payload.deadline }}
```

All guards apply: `once="per_run"` keys on the arming run, and runs created by a lapse's `run_flow` action never re-trigger the same rule unless `allow_self` is set.

## In the UI and the API

The rule form's **Unless** section has the same fields (`unless`, `within`, `at`, `tz`); validation rejects `unless` without either `within` or `at`. The rule page lists open expectations, `GET /api/rules/{id}/expectations` returns them (`?open=false` for history), and **Test** renders a synthetic lapse for a proactive rule.

## Offline

Event-armed expectations are evaluated when the run ends: a run that overruns `within` fires the lapse at completion, provided the rule was registered by a server earlier. Clock-armed rules only work with a server.

Related: [React to events with rules](rules.md), [Events](../reference/events.md).
