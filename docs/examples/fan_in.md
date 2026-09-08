# Fan-in

A report that runs once per day after both of its upstream flows finished that day.

Source: [`examples/fan_in.py`](https://github.com/sercanatalik/cereyan/blob/main/examples/fan_in.py). Run it with `python examples/fan_in.py` (no server needed).

Two independent loads and a report that needs both. With `after=[...]` and
`batch_key="day"`, the server creates one `report` run per day as soon as the last
of `sales` and `inventory` has a completed run for that day. Offline, the three
flows are ordinary functions you call in order.

```python
from datetime import date

from cereyan import App, get_run_logger, task

app = App("reporting")


@task
def load(source: str, day: date) -> int:
    get_run_logger().info("loading %s for %s", source, day)
    return 100
```

## Upstream flows

```python
@app.flow
def sales(day: date) -> int:
    return load("sales", day)


@app.flow
def inventory(day: date) -> int:
    return load("inventory", day)
```

## The downstream flow

`day` is both the flow's parameter and the batch key. A day that already has a
report never gets a second one, and a failed upstream blocks the day until it is
rerun successfully.

```python
@app.flow(after=["sales", "inventory"], batch_key="day", run_name="report-{day}")
def report(day: date) -> str:
    return f"report for {day}"
```

## Offline

Without a server nothing is triggered automatically; call the flows yourself.

```python
if __name__ == "__main__":
    day = date(2026, 2, 1)
    sales(day)
    inventory(day)
    print(report(day))
```
