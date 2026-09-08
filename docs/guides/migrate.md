# How to migrate from Prefect or Luigi

Cereyan borrows its vocabulary from Prefect and its file-oriented idempotency from Luigi, so most concepts map directly. The tables below say what to reach for and what has no equivalent, with the reasons on the [Design and limitations](../design/limitations.md) page.

## From Prefect

| Prefect | cereyan | Notes |
|---|---|---|
| `@flow`, `@task` | `@flow`, `@task` | Same shape. Parameters are typed by hints; pydantic is not used. |
| Deployment | A served flow with `schedule=` | There is no deployment object. `cereyan serve dir/` registers every flow in the directory; schedules live on the flow. |
| Work pool, worker | Engine pool | The server runs engine processes itself; there are no remote workers. |
| `prefect.yaml`, profiles, settings | `cereyan.toml`, `CEREYAN_*` | One file next to the flows. |
| Blocks, secrets | Variables with `secret=True` | Small JSON values, encrypted locally. No typed blocks, no cloud secret stores. |
| Automations, triggers | Rules (`when` / `do`) and `unless` | Same reactive model plus proactive rules; templates use the same Jinja syntax. |
| Artifacts | Artifacts | Markdown, table, progress, link, image; keyed history across runs. |
| Results, caching | `persist_result`, `cache=INPUTS`, `SOURCE` | Local storage only. |
| Task runners | `ThreadRunner`, `ProcessRunner` | No Dask or Ray. |
| `pause_flow_run`, `wait_for_input` | `wait_for_input` | Same idea; the resumed attempt replays cached tasks. |
| States | Same names, plus `Cancelling` and named sub-states | The transition rules are on the [states page](../reference/states.md). |
| Events | Events | Names are `run.completed` rather than `prefect.flow-run.Completed`. |
| Flow run retries, `retry_delay_seconds` | `retries`, `retry_delay` | `exponential(...)` replaces `exponential_backoff`. |
| Global concurrency limits, tag limits | Resources, `max_concurrent` | Named semaphores declared in configuration. |
| Prefect Cloud, workspaces, RBAC, SSO | none | One machine, one user level, one token. |
| Assets, SLAs, incident management | none | Not planned. |
| `prefect-*` integration packages | none | Use the library directly inside tasks. |
| Prefect MCP server | Built-in MCP server | `cereyan mcp` for stdio hosts, `POST /mcp` over HTTP. |

What changes in practice: delete the deployment step and the worker, keep the decorators, move settings into `cereyan.toml`, and replace `Secret.load` with `Variable.get`. Flows that used `prefect.runtime` read the run through `get_run_logger()` and the hooks' `run` argument.

## From Luigi

| Luigi | cereyan | Notes |
|---|---|---|
| `luigi.Task` with `requires`, `output`, `run` | `@task(output=...)` called from a `@flow` | Ordering is the order of calls in the flow body instead of a `requires` graph; the target still decides whether the task runs. |
| `luigi.Target`, `LocalTarget` | `Target`, `LocalTarget` | Same protocol: anything with `exists()`. Atomic writes are built in. No HDFS, S3, or database targets. |
| `luigi.Parameter`, `DateParameter` | Type hints | `day: date` instead of `luigi.DateParameter()`. |
| `luigi --module x Task --param` | `cereyan run x.py:flow --param name=value` | |
| `luigi.build([...])` | Call the flow | A flow is a function. |
| Central scheduler (`luigid`) | `cereyan serve` | Also the UI, the API, the scheduler, rules, and MCP. |
| Workers, `--workers N` | Engine pool, `max_engines` | Managed by the server. |
| `RangeDaily`, `RangeHourly` | Backfills | `cereyan backfill flow --param day --start ... --end ...` |
| `bulk_complete` | `bulk_complete=` on the flow | Same purpose: skip values already done before scheduling. |
| `resources` | `resources=` | Named semaphores; totals in `cereyan.toml` instead of `luigi.cfg`. |
| `retry_count`, `retry_delay` | `retries`, `retry_delay` | Per task or per flow. |
| `luigi.cfg` | `cereyan.toml` | |
| Event handlers (`@Task.event_handler`) | Hooks (`on_failure=`) and rules | Hooks for in-process reactions, rules for everything else. |
| Visualiser | The UI | Runs, flows, timeline, events, artifacts. |
| Cron to trigger recurring work | Schedules | Built in; catch-up policies replace "run the task for every missed date". |
| Task history database | The store | SQLite, with retention for logs and events. |

What changes in practice: turn each `requires` chain into a flow that calls the tasks in order (or several flows chained with `after=`), keep the `output` targets, replace parameters with type hints, and move `luigi.cfg` resources and retry settings into `cereyan.toml` and the decorators.

Related: [Design and limitations](../design/limitations.md), [Concepts](../concepts/app-and-projects.md).
