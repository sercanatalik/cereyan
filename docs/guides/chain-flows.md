# How to chain flows

Run one flow after another with `after=`. The server watches for upstream runs to end and creates the downstream run. Dependencies need a running server.

## One upstream

```python
from datetime import date
from cereyan import App

app = App("sales")

@app.flow
def load_orders(day: date) -> int:
    return 42

@app.flow(after="load_orders")
def build_report(day: date) -> str:
    return f"report for {day}"

assert build_report.after == {"flow": "load_orders", "flows": ["load_orders"], "key": None, "parameters": {}}
```

Every time `load_orders` ends `Completed` or `Skipped`, a `build_report` run is created with the same `day`, `created_by = run:<id>`, and a link to the upstream run in its details. A failed upstream creates nothing.

## Rename or derive parameters

Parameters are copied by name. To map them, give a template for each downstream parameter; the context is the same as for rules, with `run`, `flow`, `state`, `payload`, and `parameters`:

```python
from datetime import date
from cereyan import App

app = App("sales2")

@app.flow
def load_orders(day: date) -> int:
    return 1

@app.flow(after=("load_orders", {"for_day": "{{ run.parameters.day }}", "source": "'orders'"}))
def notify(for_day: date, source: str = "unknown") -> str:
    return f"{source} {for_day}"

assert notify.after["parameters"]["for_day"] == "{{ run.parameters.day }}"
```

## Fan in: wait for several upstreams

When the downstream needs every upstream to have finished the same batch, list them and name the parameter that identifies the batch:

```python
from datetime import date
from cereyan import App

app = App("reporting")

@app.flow
def sales(day: date) -> None: ...

@app.flow
def inventory(day: date) -> None: ...

@app.flow(after=["sales", "inventory"], batch_key="day")
def report(day: date) -> str:
    return f"report {day}"

assert report.after["flows"] == ["sales", "inventory"] and report.after["key"] == "day"
```

`report` runs once per `day`, after both `sales` and `inventory` have a Completed or Skipped run for that day. A day that already has a report never gets a second one, however the first was created, and a failed upstream blocks the batch until it is rerun. Each creation records a `flow.fan_in` event with the key value and the upstream run ids, and the flow page's dependency graph draws one edge per upstream.

## When to use a rule instead

`after=` covers "run B when A finishes". For anything conditional, a [rule](rules.md) with a `run_flow` action gives you the full match clause: only on failure, only for a tag, only in a project, with templated parameters and guards.

## Check the wiring

At start, the server reports an `after=` naming a flow it has not registered as a flow error, visible on the Flows page and in `GET /api/flows`, without stopping other flows. The flow page lists *Triggered by* and *Triggers*.

Related: [Dependencies](../concepts/dependencies.md).
