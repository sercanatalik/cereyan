# Cereyan

[![PyPI](https://img.shields.io/pypi/v/cereyan.svg)](https://pypi.org/project/cereyan/)
[![CI](https://github.com/sercanatalik/cereyan/actions/workflows/ci.yml/badge.svg)](https://github.com/sercanatalik/cereyan/actions/workflows/ci.yml)
[![Python](https://img.shields.io/pypi/pyversions/cereyan.svg)](https://pypi.org/project/cereyan/)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](https://github.com/sercanatalik/cereyan/blob/main/LICENSE)

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

![Run detail with live logs](https://raw.githubusercontent.com/sercanatalik/cereyan/main/docs/images/run-detail.png)

- **Offline first.** A script records runs into a local SQLite file; nothing else needs to run.
- **One process to serve.** The API, the web UI, the scheduler, a warm pool of engine processes, rules, and MCP, in one `cereyan serve`.
- **Data-pipeline semantics.** Targets make reruns idempotent, backfills cover date ranges, resources are named semaphores, flows chain and fan in by key, and rules react to events or to their absence.

## Documentation

The site is at https://sercanatalik.github.io/cereyan/ (built from `docs/` with `just docs`):

- [Quickstart](https://sercanatalik.github.io/cereyan/get-started/quickstart/): from install to a scheduled, retried, backfilled pipeline in ten minutes.
- [Concepts](https://sercanatalik.github.io/cereyan/concepts/app-and-projects/): the model behind flows, runs, states, schedules, targets, resources, backfills, dependencies, events, rules, artifacts, and variables.
- [Guides](https://sercanatalik.github.io/cereyan/guides/retries-timeouts-crashes/): one goal per page, from retries to running the server as a service and using cereyan with an AI agent.
- [Reference](https://sercanatalik.github.io/cereyan/reference/python-api/): the Python API, CLI, HTTP API, MCP tools, events, states, and configuration.
- [Design and limitations](https://sercanatalik.github.io/cereyan/design/limitations/): what cereyan does not do, and why.
- For agents: [llms.txt](https://sercanatalik.github.io/cereyan/llms.txt) and [llms-full.txt](https://sercanatalik.github.io/cereyan/llms-full.txt).
- [Changelog](https://sercanatalik.github.io/cereyan/changelog/).

## Development

Rust stable, Python 3.11 or newer with uv, Node 22, and just. `just ui` builds the UI, `just dev` builds the extension in place, `just test` and `just lint` run everything, `just demo` serves the examples. Details, the repository layout, and the release checklist are in the [contributing guide](https://sercanatalik.github.io/cereyan/contributing/). Design decisions and the phased roadmap live in `roadmap.md`.

## License

MIT, see `LICENSE`. The UI components adapted from Prefect are listed in `NOTICE` and remain under the Apache License 2.0 of their origin.
