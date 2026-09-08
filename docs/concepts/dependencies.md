# Dependencies

```python
from datetime import date
from cereyan import App

app = App("reporting")

@app.flow
def sales(day: date) -> str:
    return f"sales {day}"

@app.flow
def inventory(day: date) -> str:
    return f"inventory {day}"

@app.flow(after=["sales", "inventory"], batch_key="day")
def report(day: date) -> str:
    return f"report {day}"

@app.flow(after=("report", {"for_day": "{{ run.parameters.day }}"}))
def notify(for_day: date) -> str:
    return f"notified {for_day}"

assert report.after["flows"] == ["sales", "inventory"]
assert report.after["key"] == "day"
```

A **dependency** makes one flow run after another. It is declared on the downstream flow with `after=` and evaluated by the server when upstream runs end: a run of the downstream is created with `created_by = run:<upstream id>` and a link to the triggering run in its details. Dependencies need a running server; offline, they are recorded with the flow.

## Single upstream

`after="sales"` creates a downstream run whenever a `sales` run ends `Completed` or `Skipped`. A failed upstream creates nothing. Upstream parameters are copied to the downstream by name, and `after=("sales", {"for_day": "{{ run.parameters.day }}"})` renames or derives them with the same templates rules use.

## Fan-in with a key

`after=["sales", "inventory"], batch_key="day"` runs the downstream once per value of `day`, after *every* listed upstream has a Completed or Skipped run for that value. The rules:

- The batch is identified by the value of `batch_key` in the upstream runs' parameters; different values never mix.
- The downstream run is created when the last upstream completes the batch, with the key value and the usual copied, templated, and default parameters, and a `flow.fan_in` event records the key, the value, and the upstream run ids.
- At most one downstream run exists per key value: an existing run with that value, however it was created, blocks another. Rerunning an upstream for a day that already has a report creates nothing.
- A failed upstream blocks the batch until a rerun of it completes.

`batch_key` is required when `after` lists more than one flow; registration rejects the flow otherwise.

## Visibility

The flow page lists upstreams under *Triggered by* and downstreams under *Triggers*, the dependency graph draws one edge per upstream, and a run created by a dependency links to the run that triggered it. An `after=` naming a flow the server has not registered is reported as a flow error at start without stopping other flows.

Related: [Chain flows](../guides/chain-flows.md), [Events and rules](events-and-rules.md) for the more general way to run a flow when something happens.
