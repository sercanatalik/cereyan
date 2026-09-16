# How to organise flows into projects and groups

Name a project with an App, and sort its flows into groups with `group=`. The UI's scope sidebar lists each project with its groups beneath it.

```python
from cereyan import App

app = App("warehouse")

@app.flow(group="nightly")
def load_orders() -> int:
    return 120

@app.flow
def compact_tables() -> str:
    return "compacted"

assert (load_orders.project, load_orders.group) == ("warehouse", "nightly")
assert compact_tables.group == "warehouse"
```

`load_orders` is listed under `warehouse › nightly`. `compact_tables` declares no group, so its group is its project and it is listed as one of the project's own flows, shown as `(project)` in the sidebar. What a project and a group are is on [App and projects](../concepts/app-and-projects.md). The runnable [Projects and groups](../examples/projects_and_groups.md) example lays out a whole project.

## Name the project

A flow is identified by `(project, name)`, so give each project a name that will not change: renaming it later registers new flows (see [Rename a group or a project](#rename-a-group-or-a-project)).

| You write | Project |
|---|---|
| `app = App("warehouse")`, then `@app.flow` | `warehouse` |
| `@flow` with no App | The default App's name: the directory of the module that defined the first flow in the process, lowercased and sanitised |

Relying on the default App is fine for a single directory of flows. It is not for more than one: two directories with the same basename collapse into one project, and a server imports every module under its directory into one process with one default App.

## Serve several projects from one directory

`cereyan serve` imports every `.py` file under the directory it is given, subdirectories included, and every App it finds is a project. Give each subdirectory its own App:

```text
pipelines/
├─ ingest/load.py       app = App("ingest")
└─ report/build.py      app = App("report")
```

```bash
cereyan serve pipelines
```

Without the Apps, both files register on the default App and both flows land in one project named `ingest`, the first directory imported. `cereyan serve examples` serves five projects this way: `examples`, `reporting`, `shop`, `intake`, and `warehouse`.

## Group flows inside a project

A group is only a heading in the list. It changes no schedule, resource, dependency, or rule, and a flow is in exactly one group. A group name is local to its project: `nightly` in `warehouse` and `nightly` in `ingest` are two groups, each under its own project.

```python
from cereyan import App

ingest = App("ingest")

@ingest.flow(group="nightly")
def pull_orders() -> int:
    return 1

assert pull_orders.group == "nightly" and pull_orders.project == "ingest"
```

Declaring the project's own name as the group is the same as declaring none.

## Scope the UI and filter by group

Pick a project, or a group under it, in the sidebar beside Dashboard, Runs, Flows, Events, and Artifacts. The choice is kept across reloads. Runs and Flows narrow to the group; Dashboard, Events, and Artifacts narrow to its project. A link carrying `?project=` or `?group=` overrides the sidebar for that page.

Outside the UI, filter by the same names:

| Where | Filter |
|---|---|
| CLI | `cereyan runs ls --project warehouse --group nightly` |
| HTTP API | `?project=` and `?group=` on `GET /api/flows`, `/api/runs`, `/api/task-runs`, and `/api/artifacts` |
| MCP | `project` and `group` on `list_flows` and `list_runs` |

The filter matches the resolved group, so `group=warehouse` selects the flows of `warehouse` that declared no group.

## Rename a group or a project

| Rename | What happens |
|---|---|
| A group | Change `group=` and restart the server. A run's group is read through its flow, so the flow's history moves to the new group |
| A project | Change the App's name and restart. The flows register as new flows with no history; the old ones stay listed, dimmed, as not registered by this server, with their runs |

To drop the old project's flows and runs, delete each flow from its row on the Flows page, or remove the whole project in **Settings → Data**, which lists what it deletes first (see [How to clean up the store](clean-up-the-store.md)).

Related: [App and projects](../concepts/app-and-projects.md), [How to chain flows](chain-flows.md), [Tour of the UI](../get-started/tour.md).
