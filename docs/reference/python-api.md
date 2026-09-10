# Python API

Everything below is importable from `cereyan` unless a module path is shown. This page is rendered from the docstrings in `python/cereyan/` by mkdocstrings; edit the docstrings, not this page.

## Decorators and the App

::: cereyan.flow

::: cereyan.Flow
    options:
      members: false

::: cereyan.task

::: cereyan.Task
    options:
      members: [submit, map]

::: cereyan.App
    options:
      members: [flow, task, register, route, get, post, put, delete, patch, rule, serve]

::: cereyan.get_default_app

## The current run

`cereyan.runtime` is a live view of the run executing in this thread. Each attribute is `None` outside a run, so a task called directly from a test reads `None` rather than raising.

| Attribute | Type | Carries |
|---|---|---|
| `cereyan.runtime.run` | `RunContext` | `id`, `external_id`, `name`, `flow`, `parameters`, and `flow_name` and `project` |
| `cereyan.runtime.task_run` | `TaskRunContext` | `id` (the external UUID), `name`, `task_key`, `dynamic_key` |
| `cereyan.runtime.flow` | `Flow` | The flow of the current run |

```python
from cereyan import flow, task, runtime

@task
def record() -> str:
    return f"{runtime.run.name}/{runtime.task_run.dynamic_key}"

@flow
def report() -> str:
    return f"{runtime.flow.name}#{runtime.run.id}: {record()}"

assert runtime.run is None
assert report().endswith("/record-0")   # "report#1: <run name>/record-0"
```

## Schedules and retry delays

::: cereyan.Cron

::: cereyan.Interval

::: cereyan.RRule

::: cereyan.exponential

## Targets, caching and results

::: cereyan.Target

::: cereyan.LocalTarget
    options:
      members: [exists, open, remove, temporary_path]

::: cereyan.CachePolicy

`INPUTS` and `SOURCE` are the members of `CachePolicy`, exported at the top level so `cache=INPUTS + SOURCE` reads naturally.

## Concurrency

::: cereyan.Future
    options:
      members: [done, wait, result, exception]

::: cereyan.ThreadRunner
    options:
      members: false

::: cereyan.ProcessRunner
    options:
      members: false

## Logging, events and artifacts

::: cereyan.get_run_logger

::: cereyan.emit_event

::: cereyan.events.EventName

::: cereyan.events.EventGroup

::: cereyan.states.StateName

::: cereyan.artifacts.create_markdown

::: cereyan.artifacts.create_table

::: cereyan.artifacts.create_progress

::: cereyan.artifacts.update_progress

::: cereyan.artifacts.create_link

::: cereyan.artifacts.create_image

## Variables

::: cereyan.Variable
    options:
      members: [get, set, unset]

## Human input

::: cereyan.wait_for_input

## Client

The module-level functions use the server recorded in `server.json` of the runtime home; `Client` targets any server explicitly.

::: cereyan.client.run

::: cereyan.client.get_run

::: cereyan.client.list_runs

::: cereyan.client.list_flows

::: cereyan.client.cancel

::: cereyan.client.Client
    options:
      members: [health, server, flows, flow, runs, get_run, task_runs, logs, resume, cancel, delete_run, counts, backfill, backfill_status, cancel_backfill, schedules, upcoming, settings, events, rules, create_rule, variables, artifacts, submit, run]

::: cereyan.client.find_server

## Custom routes

::: cereyan.Request
    options:
      members: [json, text]

::: cereyan.Response

::: cereyan.HTTPError

## Errors

::: cereyan.CereyanError

::: cereyan.ParameterError

::: cereyan.StoreLocked

::: cereyan.TransitionRejected

::: cereyan.client.ServerUnavailable

::: cereyan.client.ApiError

::: cereyan.client.AuthRequired
