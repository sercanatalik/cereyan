# Cereyan

[![PyPI](https://img.shields.io/pypi/v/cereyan.svg)](https://pypi.org/project/cereyan/)
[![CI](https://github.com/sercanatalik/cereyan/actions/workflows/ci.yml/badge.svg)](https://github.com/sercanatalik/cereyan/actions/workflows/ci.yml)
[![Python](https://img.shields.io/pypi/pyversions/cereyan.svg)](https://pypi.org/project/cereyan/)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](https://github.com/sercanatalik/cereyan/blob/main/LICENSE)
[![Status](https://img.shields.io/badge/status-production%20ready-brightgreen.svg)](#status)

Cereyan is a minimal, local-first orchestrator for Python data pipelines. A Rust core (SQLite store, state machine, scheduler, HTTP server) sits behind a thin layer of Python decorators, and the whole thing ships as one wheel with no runtime dependencies. Decorate functions as flows and tasks, run them as plain scripts, and every run is recorded locally; `cereyan serve` adds live monitoring, schedules, retries, backfills, rules, and a built-in MCP server for agents on top of the same store.

```bash
pip install cereyan
```

```python
# pipeline.py
from datetime import date
from cereyan import flow, task, get_run_logger

@task
def extract(day: date) -> list[int]:
    get_run_logger().info("extracting %s", day)
    return [1, 2, 3]

@task
def load(rows: list[int]) -> int:
    return sum(rows)

@flow(run_name="etl-{day}")
def etl(day: date = date(2026, 9, 6)) -> int:
    return load(extract(day))

if __name__ == "__main__":
    print(etl())
```

```bash
python pipeline.py                                    # records a run in ~/.cereyan/db.sqlite
cereyan run pipeline.py:etl --param day=2026-01-02    # same, with parameters and a summary
cereyan serve .                                       # API, UI, scheduler, engines at http://127.0.0.1:4200
```

[![The cereyan dashboard: counts by state, a Needs attention list, Running now with task progress, and the live event feed](https://raw.githubusercontent.com/sercanatalik/cereyan/main/docs/images/dashboard.png)](https://sercanatalik.github.io/cereyan/get-started/tour/#dashboard)

<p align="center"><sub>The dashboard after <code>cereyan serve</code> &mdash; <a href="https://sercanatalik.github.io/cereyan/get-started/tour/">take the full tour</a></sub></p>

- **Offline first.** A script records runs into a local SQLite file; nothing else needs to run.
- **One process to serve.** The API, the web UI, the scheduler, a warm pool of engine processes, rules, and MCP, in one `cereyan serve`.
- **Data-pipeline semantics.** Targets make reruns idempotent, backfills cover date ranges, resources are named semaphores, flows chain and fan in by key, and rules react to events or to their absence.

## Screenshots

Every page below is described in the [tour](https://sercanatalik.github.io/cereyan/get-started/tour/); click a shot to jump to it.

| | |
|:--:|:--:|
| [![Runs list in collapsible groups, each header rolling up its runs' states, over rows with popover filters and a task-state bar per run](https://raw.githubusercontent.com/sercanatalik/cereyan/main/docs/images/runs.png)](https://sercanatalik.github.io/cereyan/get-started/tour/#runs) | [![Run detail: the tasks rail, live logs filtered to one task run, and retry countdowns](https://raw.githubusercontent.com/sercanatalik/cereyan/main/docs/images/run-detail.png)](https://sercanatalik.github.io/cereyan/get-started/tour/#run-detail) |
| **[Runs](https://sercanatalik.github.io/cereyan/get-started/tour/#runs)** &mdash; group, filter, select and act in bulk | **[Run detail](https://sercanatalik.github.io/cereyan/get-started/tour/#run-detail)** &mdash; logs, tasks and retries |
| [![Flows in collapsible groups, each header rolling up the next fire, recent runs, last-run states and tags of the flows beneath it, over rows with schedules in words and a run-history sparkline](https://raw.githubusercontent.com/sercanatalik/cereyan/main/docs/images/flows.png)](https://sercanatalik.github.io/cereyan/get-started/tour/#flows) | [![Task timeline: an SVG graph of task runs with dependencies and a selection panel](https://raw.githubusercontent.com/sercanatalik/cereyan/main/docs/images/run-timeline.png)](https://sercanatalik.github.io/cereyan/get-started/tour/#run-detail) |
| **[Flows](https://sercanatalik.github.io/cereyan/get-started/tour/#flows)** &mdash; groups, schedules, dependencies, history | **[Timeline](https://sercanatalik.github.io/cereyan/get-started/tour/#run-detail)** &mdash; the task graph of a run |
| [![Events page with the live feed and a JSON payload viewer](https://raw.githubusercontent.com/sercanatalik/cereyan/main/docs/images/events.png)](https://sercanatalik.github.io/cereyan/get-started/tour/#events) | [![Rules page listing when/do rules and their firing counts](https://raw.githubusercontent.com/sercanatalik/cereyan/main/docs/images/rules.png)](https://sercanatalik.github.io/cereyan/get-started/tour/#rules) |
| **[Events](https://sercanatalik.github.io/cereyan/get-started/tour/#events)** &mdash; what happened, as it happens | **[Rules](https://sercanatalik.github.io/cereyan/get-started/tour/#rules)** &mdash; react to events, or to their absence |

The UI follows your system theme; the documentation shows the [dark variants](https://sercanatalik.github.io/cereyan/get-started/tour/) too.

## Documentation

The site is at https://sercanatalik.github.io/cereyan/ (built from `docs/` with `just docs`):

- [Quickstart](https://sercanatalik.github.io/cereyan/get-started/quickstart/): from install to a scheduled, retried, backfilled pipeline in ten minutes.
- [Concepts](https://sercanatalik.github.io/cereyan/concepts/app-and-projects/): the model behind flows, runs, states, schedules, targets, resources, backfills, dependencies, events, rules, artifacts, and variables.
- [Guides](https://sercanatalik.github.io/cereyan/guides/retries-timeouts-crashes/): one goal per page, from retries to running the server as a service and using cereyan with an AI agent.
- [Reference](https://sercanatalik.github.io/cereyan/reference/python-api/): the Python API, CLI, HTTP API, MCP tools, events, states, and configuration.
- [Design and limitations](https://sercanatalik.github.io/cereyan/design/limitations/): what cereyan does not do, and why.
- For agents: [llms.txt](https://sercanatalik.github.io/cereyan/llms.txt) and [llms-full.txt](https://sercanatalik.github.io/cereyan/llms-full.txt).
- [Changelog](https://sercanatalik.github.io/cereyan/changelog/).

## Status

Production ready, and published to [PyPI](https://pypi.org/project/cereyan/). The pieces
described here work and are tested on macOS, Linux and Windows, and the interfaces —
names, signatures, defaults, routes, event payloads and the database schema — are settled.
Read the [changelog](https://sercanatalik.github.io/cereyan/changelog/) before upgrading.
Issues and questions are welcome.

## Development

Rust stable, Python 3.11 or newer with uv, Node 22, and just. `just ui` builds the UI, `just dev` builds the extension in place, `just test` and `just lint` run everything, `just demo` serves the examples. Details, the repository layout, and the release checklist are in the [contributing guide](https://sercanatalik.github.io/cereyan/contributing/). Design decisions and the phased roadmap live in `roadmap.md`.

## Credits

Cereyan owes its shape to two projects that came first.

[**Luigi**](https://github.com/spotify/luigi) contributed the idea the data-pipeline semantics rest on: a task declares the target it produces, and work is skipped when that target already exists. `Target` and `LocalTarget` keep Luigi's names because they are Luigi's concept, and idempotent reruns and backfills follow from it.

[**Prefect**](https://github.com/PrefectHQ/prefect) contributed the authoring model — flows and tasks as decorated functions, runs carrying explicit states, a server watching them — and the look of the UI. Six UI components were adapted from Prefect's, rewritten in React from the Vue originals; they are marked in `ui/src/components/ported/` and listed in `NOTICE`, and remain under the Apache License 2.0 of their origin.

Neither is a dependency, and cereyan deliberately does far less than either: one machine, one process, one wheel, no remote workers and no database to run. If you need what they do, use them. [Migrating from Prefect or Luigi](https://sercanatalik.github.io/cereyan/guides/migrate/) says what carries over and what does not.

## License

MIT, see `LICENSE`.
