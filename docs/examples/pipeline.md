# Quickstart pipeline

Three tasks and one flow, run offline as a plain script.

Source: [`examples/pipeline.py`](https://github.com/sercanatalik/cereyan/blob/main/examples/pipeline.py). Run it with `python examples/pipeline.py` (no server needed).

The smallest useful pipeline. Run it with `python examples/pipeline.py` and a run
is recorded in the runtime home; run it while `cereyan serve examples/` is up and
the run is handed to the server instead. Parameters come from the type hints, so
`cereyan run examples/pipeline.py:etl --param day=2026-01-02` coerces the string
to a `date` before the flow starts.

```python
from datetime import date

from cereyan import flow, get_run_logger, task
```

## Tasks

A task is a function whose calls inside a flow are recorded as task runs. The
run logger writes lines that are stored with the run and shown live in the UI.

```python
@task
def extract(day: date) -> list[int]:
    get_run_logger().info("extracting %s", day)
    return [1, 2, 3]


@task
def transform(rows: list[int]) -> list[int]:
    return [r * 2 for r in rows]


@task
def load(rows: list[int]) -> int:
    get_run_logger().info("loading %d rows", len(rows))
    return sum(rows)
```

## The flow

The flow calls the tasks in order; `run_name` names each run after its parameter.

```python
@flow(run_name="etl-{day}", tags=["example"])
def etl(day: date = date(2026, 9, 6)) -> int:
    return load(transform(extract(day)))
```

## Run it

A flow is a function: calling it runs the tasks and returns the result.

```python
if __name__ == "__main__":
    total = etl(date(2026, 9, 6))
    print("total:", total)
    assert total == 12
```
