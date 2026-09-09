# Design and limitations

Cereyan is built for one machine, one wheel, and pipelines that can always be rerun. Those three choices explain most of what it does and everything it refuses to do. This page says what those choices mean for you.

## What it is for

- A data engineer with pipelines on a laptop, a workstation, or one server, who wants schedules, retries, backfills, a UI, and alerts without running a platform.
- Batch work measured in seconds to hours, tens of thousands of runs a day at most, where files and tables written by the flow are the real output.
- Teams that keep flows in one repository and prefer code over configuration.

## Decisions

**One wheel, no runtime dependencies.** `pip install cereyan` is the whole install: the Rust core, the server, the UI, and the MCP server are inside the wheel. Nothing is pulled in at run time, so a pipeline environment gains no transitive dependencies from its orchestrator. Docs tooling and tests use dependency groups that never reach the wheel.

**One process serves everything.** `cereyan serve` is the API, the UI, the scheduler, the rules engine, the MCP endpoint, and the engine supervisor. There are no separate agents, workers, or queues to run.

**Two execution paths, one store.** A plain `python pipeline.py` writes the same SQLite store the server uses, under the same state rules, so a script and a served run look identical in history. When a server holds the store, scripts hand their runs to it.

**History is a cache.** The database records what happened; your code and your targets are the source of truth. Losing `db.sqlite` loses the run list, not your data, and a corrupted file is quarantined rather than blocking startup. Crashed runs are rerun, up to a limit, on the assumption that a rerun is safe because targets make it so.

**A fixed vocabulary.** App, flow, task, run, task run, state, schedule, parameter, target, resource, backfill, artifact, variable, event, rule. There is no "deployment": a served flow with a schedule is the deployed thing.

**Performance is a feature.** The store is SQLite in WAL mode with one writer, group commit, a read pool, and an in-memory working set; engines report in batches serialised in Rust. The [targets](performance.md) are enforced by benchmarks in CI.

## What it does not do, and why

| Not provided | Reason | Instead |
|---|---|---|
| Remote workers, work pools, Kubernetes or Docker execution | Cereyan runs on the machine it is installed on; distributing execution would need a broker, a scheduler that knows about hosts, and a rollout story, which is the platform it set out not to be. | Run a server per machine, or call remote systems from inside tasks. |
| Postgres or any other database | SQLite gives one file, no service, and the performance the targets need; a second backend would double the store and change the operational shape. | Retention keeps the file bounded; back it up like any file. |
| Remote or object-store targets (S3, GCS, HDFS) | A `Target` is anything with `exists()`, so you can write one in a few lines with the client library you already use; shipping them would add dependencies to the wheel. | Write a small class with `exists()`. |
| Integrations and connector packages | Tasks are plain Python; the library for your warehouse or API works unchanged inside one. | Import it in the task. |
| Multi-user accounts, roles, SSO, audit trails | One token, one permission level, one machine. | Put a reverse proxy in front for network access; use the token or the socket. |
| Cloud-style extras: assets, SLAs, incident management, metric triggers, incoming webhooks as event sources | Each is a product on its own; rules with `unless` and custom routes cover the local versions of the common cases. | A proactive rule for "did not happen by"; a custom route that calls `emit_event` for incoming webhooks. |
| TLS | The server is loopback-only by default and a reverse proxy does TLS better. | nginx, Caddy, or an SSH tunnel. |
| Versioned flows, code storage, image builds | The code in the served directory is the version; engines reload it when it changes. | Git. |
| Real-time or streaming pipelines | Runs are units of work with a beginning and an end; the scheduler is a timer heap, not an event loop over streams. | A long-running flow per stream partition, or another tool. |
| Preemption, priority-based killing | A running run is never interrupted to make room; priority only orders the queue. | Resources and caps to keep heavy work from starting. |

## Limits worth knowing

- One server per machine, because the home and its advisory lock are global.
- The engine pool bounds concurrent runs at `max_engines`, default the CPU count; a run occupies an engine for its whole duration, including time spent waiting on I/O.
- Artifacts are limited to 1 MB and variables to 64 KB.
- Retention deletes logs and events older than `retain_days`; runs and task runs stay.
- Unix sockets and engine niceness do not exist on Windows. Three behaviours are also weaker there: a flow's `timeout_seconds` does nothing on the offline path, cancelling a flow blocked in a call ends the engine instead of raising inside the flow, and a clock-armed proactive rule can lapse while the events it waits for are still arriving.
- Schedules, dependencies, backfills, data rules, clock-armed proactive rules, and pausing need a running server; offline scripts record runs and fire code rules that a server has registered.

Related: [Architecture](architecture.md), [Migrate from Prefect or Luigi](../guides/migrate.md).
