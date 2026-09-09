# Changelog

## Unreleased

## 1.6.1 (2026-09-09)

- Internal refactoring.

## 1.6.0 (2026-09-09)

- **`@flow` and `@task` reject `async def`.** An async body was never awaited: the call returned a coroutine, the body never ran, and the run was recorded `Completed` — a pipeline that fetched nothing and reported success, its only trace a `RuntimeWarning` on stderr after the fact. Both decorators now raise `TypeError` at decoration, naming the function and showing the synchronous wrapper. `async def` with `yield` is rejected too: `inspect.iscoroutinefunction` is false for an async generator function, which failed the same way. Async bodies remain unsupported; `async def` route handlers are unaffected. Anything this breaks was already reporting success without running.
- A guide for fetching from an HTTP API: building the client once so a warm engine reuses its connection pool, `map` over a `ThreadRunner` for concurrency, and where the reuse stops (`isolated=True`, `ProcessRunner`, offline runs). That an engine imports its module once, and so shares module-level state across the runs it serves, is now stated on Engines and the home directory and covered by a test, rather than being an undocumented accident of the implementation.

## 1.5.0 (2026-09-09)

- The runtime home is readable only by the account that created it. It was created with whatever the umask gave — `0755` on a typical machine — so `db.sqlite`, with every run, log, event and variable, was readable by any other account, and `secret.key` was too: it was narrowed to `0600` after being written, leaving a window in which the key that decrypts every secret was world-readable, and that narrowing never ran on Windows at all. Protecting the directory covers everything in it, closes the window, and needs nothing platform-specific for a home under your user profile. A home from an earlier version is narrowed when opened, with a message saying so.
- An engine whose server disappeared could take up to ninety seconds to exit rather than the thirty its documentation promised. It notices only between requests, so the wait was bounded by the client's long-poll timeout — ninety seconds — and not by the thirty second idle threshold that appeared to govern it. The timeout is now forty seconds, ten more than the server ever holds a request, so the worst case is forty rather than ninety. The regression test used to wait exactly ninety seconds and so raced the very timeout that defeated the bound; it now waits sixty against a forty second bound.
- Windows: stopping the server no longer kills the engine executing a run. Engine children were spawned in their own process group on Unix — so a Ctrl-C or a stop aimed at the server never reached them — but there was no Windows equivalent, so a console control event swept up every engine, including the one mid-run that a restarted server is supposed to adopt. A run in flight when the server stopped was lost rather than resumed. Engines now get their own group on both platforms.
- `server.json` records a URL clients can actually use. It carried the address the listener bound, so starting the server on every interface wrote `http://0.0.0.0:<port>` — a bind address, not a destination. Linux and macOS route that to loopback, so it worked by accident; Windows refuses it, and nothing that reads the discovery file could find the server there. `url` now names loopback when the bind address is unspecified, and `host` still records what was bound.
- **Windows: `cereyan serve` can be stopped gracefully.** It handled `SIGTERM`, which Windows never delivers, so anything stopping the server other than an interactive Ctrl-C — a service manager, a script, a supervisor — ended the process before it could shut down: the discovery file was left behind, pending writes were not flushed, the WAL was not checkpointed, and engine processes were orphaned. A console control event arrives as `SIGBREAK` on Windows and is now handled the same way `SIGTERM` is on Unix. Unix behaviour is unchanged. Found by running the Python test suite on Windows for the first time.

## 1.4.0 (2026-09-08)

- The Intel macOS wheel is cross-built from the arm64 runner. GitHub retired the `macos-13` label, so that build queued forever and, because the smoke test and the PyPI upload wait for every wheel, no release could complete at all.
- `cargo bench` runs again. Criterion's flags were being handed to the auto libtest harness of every lib target, which rejects them, so the benchmarks stopped at the first target reached — through `just bench` as much as in CI. Each crate's lib now sets `bench = false`.
- Wall-clock ceilings moved out of the test suite that gates a release. Two Rust timing tests are `#[ignore]` and the Python `performance` marker is deselected there; both run in `just bench` and in CI's benchmark job, which is where the per-platform baselines live. A ceiling calibrated on a developer machine failing on a shared CI runner said nothing about correctness. `benches/e2e.py --no-ceilings` reports an absolute miss instead of failing, while a regression against a baseline still fails.
- Windows: `cereyan-server` now compiles. The Unix socket listener's serve path was never excluded on platforms without Unix sockets, so the crate failed to build and the Windows wheel and test job have been broken since the socket landed in 1.1. Windows had no working wheel for 1.1, 1.2, or 1.3 despite being listed as a supported platform.
- Engines now end with the server. A graceful stop answers every waiting engine with an instruction to exit and signals any that were idle between requests, so `cereyan serve` leaves no engine child behind; an engine executing a run is deliberately left alone, so a restarted server still adopts its run. An engine that loses its server without being told to stop — a kill, a crash — exits by itself after thirty seconds of failing to reach it. **This reverses a documented guarantee:** the server previously exited without terminating any engine process, so anything that relied on the warm pool outliving a graceful stop or restart now sees a cold pool instead.
- Removed `exit_code` from runs. The field was in the `Run` model, the OpenAPI and MCP surfaces, and the run page, but nothing ever wrote it, so it was always null; the store's `set_run_exit` write path had no callers. Clients reading `run.exit_code` should drop it. The SQLite column stays, unread, because migrations are append-only.
- Packaging: the wheel and sdist now carry `LICENSE` and `NOTICE`, project URLs, keywords, an author, and a fuller classifier set, so the PyPI page renders its links, licence, and images. Tagging a release now publishes to PyPI from CI through trusted publishing, which the release checklist already promised.

- UI redesign: a top bar replaces the sidebar and breadcrumb bar, with the eight sections as tabs, a project switcher that scopes every list, a ⌘K palette that jumps to sections, flows, runs, and artifacts, and a warm neutral palette in light and dark with Geist bundled. Controls are shadcn components. The dashboard leads with a Needs attention list (paused, failed, crashed, and late runs with inline actions), Running now with task progress, and the histogram with an axis; the runs page has popover filters, a task-state bar per run, and a floating selection bar; the run page is a workbench with a tasks rail that filters the logs to one task run and shows retry countdowns; the flows page groups by project with the schedule in words, a run-history sparkline, and a Dependencies panel of `after=` chains and fan-in groups.
- API: run list items and `GET /api/runs/{id}` carry `task_counts` by task state; `recent_runs` entries on flows gain the run's duration; `AwaitingRetry` details record `retries`.

- Documentation authoring guide: `docs/AGENTS.md` now exists, covering the layout, the generated pages and their generators, the page kinds and section budgets, the vocabulary, the style rules, the tested-block markers and fixtures, the redirect rule, and the commands. `just lint` fails when it is missing or stops naming a generated page, which is what let the documentation-restructure entry below promise a file that was never written.
- The MCP reference is generated: `scripts/gen_mcp_reference.py` renders `docs/reference/mcp.md` from `tests/mcp_snapshot.json`, a snapshot of the server's own handshake, tools, resource templates, prompts, and response keys, checked in `just lint` like the CLI and HTTP references. The page now carries the protocol version, the instructions the server gives the model, each argument's type, default, and range, the keys every tool returns (including `next_cursor` and `verdict`), the fields of the lists they hold, and the prompt text.
- Documentation: `cereyan mcp --token` never worked; `--token` is global and goes before the subcommand. The agent guide says so, and calls the two resources templates, which is what they are.

- Overlap soak (`just soak`, `benches/soak_overlap.py`): fifteen scheduled flows across `enqueue`, `skip`, and `cancel_new` run for an hour against a real server and are checked against nine overlap invariants; `--quick` for twelve minutes, `--keep` to browse the UI afterwards, manual `workflow_dispatch` job in CI.
- Documentation restructured into Get started, Concepts, Guides, Reference, Examples, and Design on a Material for MkDocs site with `llms.txt` output; every Python block in the docs and every file under `examples/` is executed by the test suite; the CLI and HTTP API references are generated from the parser and the OpenAPI snapshot; a docstring check covers the public API; `docs/AGENTS.md` and an OpenSpec rule keep future changes documented. Old page URLs redirect.
- Docstrings for every public class, function, and method, and help text for every CLI option (`cereyan <command> --help`). No behaviour changes.
- Seven literate examples under `examples/`: quickstart pipeline, daily ETL, fan-in, approval, alerting, webhook route, agent diagnosis.

## 1.3.0 (2026-09-06)

- Fan-in dependencies: `@flow(after=["a", "b"], batch_key="day")` runs the downstream once per key value after every upstream completed it; `flow.fan_in` events, upstream lists in the flow summary and graph.

## 1.2.0 (2026-09-06)

- Built-in MCP server: `POST /mcp` (Streamable HTTP, JSON replies) and `cereyan mcp` (stdio proxy) with fifteen curated tools, two resources, and a `diagnose_run` prompt; runs started by agents record `created_by = mcp:<client>`.
- Human-in-the-loop: `wait_for_input(prompt, schema=None)` pauses a run, `POST /api/runs/{id}/resume` answers it, the run page shows the question with a form; the engine and resources are released while a run waits. `run.paused` and `run.resumed` events.
- `Paused` is now entered only from `Running`.

## 1.1.0 (2026-09-06)

- Proactive rules: `unless` with `within` (event-armed) or `at` cron with `tz` (clock-armed) fires when an expected event does not happen; lapses are `expectation.lapsed` events, expectations survive restarts, `GET /api/rules/{id}/expectations`, and an "Unless" section in the rule form.
- API token: `--token`, `CEREYAN_TOKEN`, `app.serve(token=)`, or `[server] token` protects every API route except health; the UI prompts for it, clients and engines send it, and `server.json` records `auth`.
- Unix socket listener (`--socket`, `CEREYAN_SOCKET`, `[server] socket`) trusted by file permission, recorded in `server.json`, and used by the Python client.
- Async custom route handlers on one shared event loop.
- Artifacts page and `GET /api/artifacts` with filters, keyset pagination, and per-key history.
- Negative `priority` lowers engine niceness on Unix.
- Downgrading to 1.0.x: delete rules that use `unless` first; 1.0 does not load them.

## 1.0.0 (2026-09-06)

First release. One wheel, no runtime dependencies, Python 3.11 or newer.

- Flows and tasks with parameters from type hints, offline execution into a local SQLite store, and the `cereyan run` and `cereyan runs ls` commands.
- `cereyan serve`: HTTP API with an OpenAPI document, server-sent events, a warm pool of engine processes that survive server restarts, custom routes, and the embedded React UI.
- One runtime home per machine (`--home`, `CEREYAN_HOME`, `~/.cereyan`); flows identified by project and name; cross-project handoff from scripts to a running server.
- Schedules (cron, interval, RRule) with timezones and catch-up policies; retries, timeouts, hooks, and crash chains; Targets with atomic writes; input and source caching; backfills; resources, priority, overlap policies, and disable windows; concurrent tasks with futures, map, thread and process runners; single-upstream flow dependencies; timeline graph.
- Events, rules with seven actions and templating, artifacts, variables with encrypted secrets, settings and retention, benchmarks against the performance targets, docs, and the release wheel matrix.
- Performance: engine reports are chunked and accepted up to 64 MB, task-run events are appended in one batch per report, and opening a large database no longer reads the whole file when the previous shutdown was clean.
