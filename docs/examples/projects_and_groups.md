# Projects and groups

One App as a project, with flows in two groups and flows of its own.

Source: [`examples/projects_and_groups.py`](https://github.com/sercanatalik/cereyan/blob/main/examples/projects_and_groups.py). Run it with `python examples/projects_and_groups.py` (no server needed).

A project is the name of an App; a group is a level inside that project. The UI's
scope sidebar lists every project with its groups beneath it, and picking one narrows
the Dashboard, Runs, Flows, Events, and Artifacts to it. This file is one project,
`warehouse`, laid out as the sidebar draws it:

```text
warehouse
├─ (project)  compact_tables
├─ adhoc      reprocess_day
└─ nightly    load_customers, load_orders
```

```python
from datetime import date

from cereyan import App, get_run_logger
```

## The project

`App("warehouse")` names the project. Every flow registered on it is identified by
`(warehouse, name)`, so another project can have its own `load_orders` without a
collision. Without an App, flows belong to a project named after their directory.

```python
app = App("warehouse")
```

## Two groups

`group=` puts a flow in a group inside its project. Groups are for reading the list,
not for running: a group changes no schedule, resource, or dependency.

```python
@app.flow(group="nightly")
def load_orders(day: date) -> int:
    get_run_logger().info("loading orders for %s", day)
    return 120


@app.flow(group="nightly")
def load_customers(day: date) -> int:
    return 40


@app.flow(group="adhoc")
def reprocess_day(day: date) -> int:
    get_run_logger().info("reprocessing %s", day)
    return 160
```

## The project's own flows

A flow that declares no group resolves to its project. It is listed directly under
`warehouse`, shown as `(project)` in the sidebar, rather than in a group repeating
the project's name.

```python
@app.flow
def compact_tables() -> str:
    return "compacted"
```

## Check the layout

`project` and `group` are what the API returns for each flow and each of its runs.
The same names drive the `group` filter everywhere:

```bash
cereyan runs ls --group nightly
curl 'http://127.0.0.1:4200/api/flows?project=warehouse&group=nightly'
```

```python
if __name__ == "__main__":
    for fl, group in [
        (load_orders, "nightly"),
        (load_customers, "nightly"),
        (reprocess_day, "adhoc"),
        (compact_tables, "warehouse"),
    ]:
        assert fl.project == "warehouse", fl.project
        assert fl.group == group, (fl.name, fl.group)
    print("orders:", load_orders(date(2026, 3, 1)))
```
