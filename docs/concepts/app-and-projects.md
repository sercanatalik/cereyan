# App and projects

```python
from cereyan import App

app = App("warehouse")

@app.flow
def daily_etl(day: str = "2026-09-06") -> str:
    return day

assert daily_etl.project == "warehouse"
```

An **App** is the registry a module's flows, custom routes, and code rules belong to. Its name is the **project** every one of its flows is identified by: a flow's identity is `(project, name)`, so two projects can each have a flow called `etl` without colliding.

The project is also the outer level the UI lists flows and runs under, with each project's [groups](#groups) inside it.

## The default App

You do not have to create an App. `@flow` without one registers on the default App, which is created on first use and named after the directory of the module that defined the first flow, lowercased and sanitised to `[a-z0-9][a-z0-9_-]*`:

```python
from cereyan import flow

@flow
def etl() -> int:
    return 1

print(etl.project)   # the basename of the directory this module lives in
```

Two directories with the same basename that both rely on the default App collapse into one project, and so do the subdirectories of one served directory, which share a process and its one default App. Give each an explicit `App(name=)`; [How to organise flows into projects and groups](../guides/organise-projects-and-groups.md) shows the layout.

## Groups

A group is a level inside a project, for reading the list: it changes no schedule, resource, or dependency. A flow's group is the one it declares with `group=`, else its project:

```python
from cereyan import flow

@flow(group="nightly")
def daily_etl() -> int:
    return 1

assert daily_etl.group == "nightly"
```

A group name used in two projects appears under each of them, summarising only that project's flows. A flow whose group resolves to its project — one that declared none, or declared its project's own name — is listed directly under the project rather than in a section repeating that name, so every flow appears exactly once and there is no ungrouped bucket.

A run is grouped by its flow's group, read through the flow rather than stored on the run, so renaming a group moves the run history with it.

In the UI, the scope sidebar beside Dashboard, Runs, Flows, Events, and Artifacts lists each project with its groups; picking a group narrows Runs and Flows to it, and picking a project narrows every scoped page. The Runs page nests its rows project then group in collapsible sections, and the Flows page bands its rows by group; both are on the [tour](../get-started/tour.md#flows). The API, the CLI, and the MCP tools filter by group too:

```bash
cereyan runs ls --group nightly
```

The filter matches the **resolved** group, the same value the payloads carry, so `?group=warehouse` selects the flows of project `warehouse` that declared no group of their own as well as any flow that declared that name.

## What an App holds

| Registered with | Holds |
|---|---|
| `@app.flow` or `@flow` | Flows, keyed by name; a second flow with the same name raises `FlowRegistrationError` |
| `@app.get`, `@app.post`, `@app.put`, `@app.delete`, `@app.patch`, `@app.route` | Custom HTTP routes served next to the API (see [Add custom HTTP routes](../guides/custom-routes.md)) |
| `@app.rule` | Code rules whose action calls the decorated function (see [Events and rules](events-and-rules.md)) |

`app.serve()` serves the directory of the module that created the App, the same as `cereyan serve <dir>` with the App's options; the CLI is the usual way to serve, and `serve` exists for scripts that want to set host, port, or token in code.

## Registration and liveness

Flows are upserted by `(project, name)` every time a server registers them and are never deleted automatically. The server records each flow's module and source directory so it can start engines for it and so scripts in other projects can hand runs to it. A flow the running server did not register shows as *not live* in the UI with its last-seen time, and can be deleted there.

## What stays global

Resources and variables are shared by every project on the machine, because they describe machine-level things: a resource `db = 4` shared by two projects is the intended reading. Prefix variable names when projects need isolation, for example `warehouse/api_token`. Rules can be scoped to a project with the `project` field of their match clause, and events that reference a flow carry `project` in their payload.

Related: [Flows and parameters](flows-and-parameters.md), [Engines and the home directory](engines-and-home.md).
