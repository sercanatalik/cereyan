# Cereyan roadmap

Cereyan is a minimal, local-first orchestrator for Python data pipelines: a Rust core (tokio, axum, SQLite) exposed through PyO3, a thin Python authoring layer, and an embedded React UI, shipped as one wheel with no runtime dependencies. This page records the phases it was built in, what each delivered, and the decisions that shaped it.

All phases and the 1.1, 1.2, and 1.3 releases are implemented, followed by the documentation restructure and the UI redesign. Each phase ended with something runnable, and each later phase built on the previous one's artifacts without changing its specs. One cross-cutting piece, global home and project identity, refined phases 0 and 1 and landed inside them.

## Phase 0: core skeleton

Deliverable: `python pipeline.py` records runs into `~/.cereyan/db.sqlite`; `cereyan run` and `cereyan runs ls` work; wheels build on three platforms.

Capabilities: `packaging`, `flow-authoring`, `run-state-machine`, `local-store`, `offline-execution`.

Key decisions: maturin mixed project with a Cargo workspace; abi3 wheels for Python 3.11+; integer rowids with UUIDv7 external ids; microsecond UTC timestamps; state rules as pure Rust functions shared by both execution paths; SQLite in WAL with `synchronous=NORMAL`, one writer thread with group commit, read pool, OS advisory lock, corruption quarantine, embedded migrations; in-house parameter coercion (no pydantic).

## Cross-cutting: global home and project identity

Deliverable: one runtime home per machine resolved from `--home`, then `CEREYAN_HOME`, then `~/.cereyan`; flows keyed by `(project, name)` where the project is the `App` name, defaulting to the directory of the defining module; stale flows shown, not hidden; handoff works across projects.

Capabilities: `runtime-home`, `project-identity`; modifies `flow-authoring` and `local-store` from phase 0.

Key decisions: `cereyan.toml` cannot move the home; engines inherit `CEREYAN_HOME`; flow row records `module` and `source_dir`; engines keyed by `(source_dir, module)`; registration upserts and never deletes; resources and variables stay global; project filter on runs, flows, counts, and the UI. Tasks 1 and 2 land with phase 0, tasks 3 and 4 with phase 1, task 5 with phases 2 and 3.

## Phase 1: server and first UI

Deliverable: `cereyan serve dir/` opens a UI with live runs and logs; runs execute in warm engine child processes; users can add custom routes.

Capabilities: `http-api`, `engine-supervisor`, `run-logs`, `custom-routes`, `web-ui`.

Key decisions: axum on a tokio runtime inside the Python process; OpenAPI from utoipa feeding a generated TypeScript client; SSE stream with ring buffer and coalescing; in-memory active index and counters; warm engine pool with recycle-on-change and `isolated=True`; batched reporting serialized in Rust; SIGTERM then SIGKILL cancellation; engines survive server restarts and are adopted; offline runs hand off to a live server; FastAPI-shaped sync route handlers; React, Tanstack, shadcn, desktop only.

## Phase 2: schedules and backfill

Deliverable: schedules fire on time with a catch-up policy; retries, timeouts, hooks, and the crash policy work; Targets make reruns idempotent; backfills, resources, concurrent tasks, single-upstream flow dependencies; flow detail, schedule editor, run form, backfill dialog, timeline graph in the UI.

Capabilities: `schedules`, `run-resilience`, `targets-and-caching`, `backfill`, `resources`, `concurrent-tasks`, `flow-dependencies`, `web-ui-scheduling`.

Key decisions: heap-and-timer scheduler with bounded look-ahead; catch-up `skip` by default; `crash_retries` default 5 then Failed; `LocalTarget` atomic writes; `output=` produces Skipped; cache policies require result persistence; backfill is a single transaction with a per-backfill concurrency resource; resources are in-memory named semaphores released on any terminal state; `max_concurrent` caps a flow (default unlimited) with `on_overlap` enqueue, skip, or cancel_new; priority orders dispatch only, never preempts; `after=` is a code rule with one upstream.

## Phase 3: rules, observability, release

Deliverable: event feed, rules with seven actions, artifacts, variables with secrets, settings and retention, benchmarks enforcing the performance targets, wheels and docs for 1.0.0.

Capabilities: `events`, `rules`, `artifacts`, `variables`, `settings-and-retention`, `performance-targets`, `web-ui-observability`.

Key decisions: reactive rules only (`unless` deferred); minijinja templates in Rust; data rules from the UI and code rules from `@app.rule`; self-trigger and rate guards; SMTP via lettre only; secrets encrypted with a local key; 30-day retention in small batches; benchmark regression gate at 20 percent.

## 1.1: proactive rules and a hardened server

Deliverable: rules can fire when an expected event does not happen by a deadline; the server can be reached safely from CI or another process on the same machine; artifacts are browsable across runs; custom routes can be async.

Scope, decided 2026-09-06:

- Proactive `unless` rules: an armed expectation (for example `flow.completed` for a flow within a window of its scheduled time, or by a wall-clock time in a timezone) that a matching event disarms and the timer heap fires when it lapses. Reuses the schedule vocabulary and the existing rule actions, guards, and firings history.
- Bearer-token auth: one static token in settings, checked by middleware on every API route when set; the Python client, the CLI, and the UI pass it. Unset keeps today's localhost-only behaviour.
- Unix socket listener as an alternative to a TCP port, with discovery through `server.json` and the client accepting a socket path.
- Async custom route handlers alongside the sync ones.
- Global artifacts page: artifacts across runs, grouped by key with history.
- Negative `priority` lowering engine OS niceness.

Constraints carried over: one wheel, no runtime dependencies; SQLite stays the only store; no remote targets.

## 1.2: agent ready

Deliverable: an AI agent can operate cereyan through a built-in MCP server over stdio and HTTP, including starting work; a flow can pause for a human decision and resume with the answer.

Scope, decided 2026-09-06:

- Built-in MCP server (JSON-RPC 2.0) with no new dependencies: a Streamable HTTP endpoint (`/mcp`) inside the Rust server, and `cereyan mcp` as a stdio process in Python that proxies to the running server for hosts such as Claude Code and Claude Desktop. Authentication is the 1.1 bearer token or the Unix socket; MCP adds no second permission model.
- A curated tool set, not the raw API. Reads: `list_flows`, `list_runs`, `get_run`, `run_logs`, `list_events`, `list_artifacts`, `list_rules`, `explain_failure` (run, failed tasks, last error logs, related events in one call). Writes: `run_flow`, `cancel_run`, `backfill` (with a dry run returning the count), `pause_schedule`, `resume_schedule`, `set_variable`. Rule creation stays out. Run logs and artifacts are also MCP resources; a "diagnose this run" prompt ships with the server.
- Tool descriptions state effects plainly, and runs created through MCP record `created_by = mcp:<client name>` from the initialise handshake so agent activity is attributable on the runs page and in events.
- Human-in-the-loop: `wait_for_input(schema)` inside a task parks the run in the existing `Paused` state with the pending question visible in the UI and the API; a resume endpoint carries the answer. The engine releases its slot on pause; resume starts a new attempt that replays cached tasks and reads the answer, so no process is held open while waiting.

Constraints carried over: one wheel, no runtime dependencies; the MCP transports are implemented on the standard library and axum.

## 1.3: fan-in dependencies

Fan-in flow dependencies with keyed batches: a downstream flow runs once every upstream has a completed run for the same key (a named parameter such as a date). Needs a key model over run parameters, likely an indexed run key column, and the dependency graph in the UI. Deferred so the key model gets its own design.

## Documentation

Deliverable: a Material for MkDocs site (Get started, Concepts, Guides, Reference, Examples, Design) with tested code blocks, generated CLI and HTTP references, literate examples, `llms.txt`, and `docs/AGENTS.md`; every future change carries a documentation task.

Capabilities: `docs-site`, `docs-tested-examples`, `docs-reference-generation`, `docs-authoring-rules`; modifies `packaging`.

## UI redesign

Deliverable: a console shell in place of the Prefect-shaped sidebar: a top bar with the eight sections, a project switcher, and a ⌘K palette; shadcn components on a warm neutral palette with ink as the only brand colour and Geist bundled; the dashboard leads with what needs attention, the runs page gets popover filters and a task-state bar, the run page becomes a workbench with a tasks rail, and the flows page groups by project with a Dependencies panel. Run list items carry `task_counts` and flow `recent_runs` carry durations.

Capabilities: `ui-design-system`; modifies `web-ui`, `web-ui-observability`, `http-api`.

## Dropped

Postgres and remote or object-store targets are not planned. Both would break the single-machine, single-wheel, no-dependency shape that the rest of the design relies on, and there is no shared-server use case to justify them.

## Performance targets

| Operation | Target |
|---|---|
| Server start with 1M historical runs | under 200 ms |
| Task transitions ingested | 20k per second |
| Log lines ingested | 100k per second |
| Runs list query at 1M runs | under 10 ms |
| Schedule wake-up drift | under 50 ms |
| Backfill create of 10k runs | under 1 s |
| Warm-pool overhead Scheduled to user code | under 5 ms |
| Counts endpoint | under 5 ms |
