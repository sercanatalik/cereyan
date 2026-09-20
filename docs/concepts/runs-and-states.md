# Runs and states

```python
from cereyan import flow, task

attempts = []

@task(retries=1)
def flaky() -> str:
    attempts.append(1)
    if len(attempts) == 1:
        raise RuntimeError("first try fails")
    return "ok"

@flow
def resilient() -> str:
    return flaky()

assert resilient() == "ok"
assert len(attempts) == 2   # task run: Running, AwaitingRetry, Retrying, Completed
```

A **run** is one execution of a flow. A **task run** is one execution of a task inside it. Both carry a **state** with a type, an optional sub-state name, a message, details, and a timestamp, and both keep the whole history of states they went through.

## State types

```
Scheduled ──▶ Pending ──▶ Running ──▶ Completed
    │            │           │  ▲         Failed
    │            │           │  │         Crashed
    │            │           ▼  │
    │            │         Paused
    │            │           │
    └────────────┴───────────┴──▶ Cancelling ──▶ Cancelled
```

| Type | Meaning |
|---|---|
| `Scheduled` | Created; waiting for its time, a retry delay, or a resource |
| `Pending` | Dispatched to an engine |
| `Running` | Executing |
| `Paused` | Waiting for a person to answer `wait_for_input`; the engine is free |
| `Cancelling` | Asked to stop |
| `Completed`, `Failed`, `Cancelled`, `Crashed` | Terminal |

Named sub-states refine a type: `Late` and `AwaitingRetry` and `AwaitingResource` (Scheduled), `Retrying` (Running), `TimedOut` (Failed), `Cached` and `Skipped` (Completed). The exact transition rules, shared by the offline and served paths, are on the [States and transitions](../reference/states.md) page.

## What a run records

- The flow, project, parameters, tags, and name.
- `created_by`: `script` for a plain call, `client` for the API and CLI, `schedule:<id>`, `backfill:<id>`, `rule:<id>`, `run:<id>` for a dependency, `catchup`, `crash:<n>` for a crash rerun, and `mcp:<client>` for an agent.
- Timing: created, scheduled, start, end, total run time; and counters for failures and crashes.
- Every log line, task run, artifact, and event.

Inside the run, `cereyan.runtime.run`, `cereyan.runtime.task_run`, and `cereyan.runtime.flow` read the run in progress — its id, name, and parameters — and are `None` outside one; see the [Python API](../reference/python-api.md).

## Attempts

A run's body can execute more than once, and each execution is a **pass**. The first is pass 0; a retry and a resume after a pause are the next. Every pass numbers its task runs from zero, so the same call keeps its dynamic key in each one — `fetch-0` in pass 0 is the same call as `fetch-0` in pass 1 — and the task runs of a later pass are recorded alongside the earlier ones rather than replacing them. `failure_count` grows with retries. Tasks marked `cache=INPUTS` return their stored result on a replay instead of executing again.

A **crash rerun** is not a pass: the supervisor creates a new run linked to the crashed one through `parent_run_id`, with `created_by` of `crash:<id>` and `attempt` one higher, so a crash chain is a chain of runs rather than one run executed twice.

`GET /api/runs/{id}/tasks` returns every pass, and `?pass=` narrows it to one. The run page shows the latest pass and offers a switcher when there is more than one.

## Attributes

A run can describe itself while it runs: `set_attributes(region="eu", rows=n)` from a flow or task merges key-values into the run's `attributes`, offline and served, and later calls merge over earlier ones. Names are letters, digits, and underscores; values are any JSON. They are the run's own searchable notes, distinct from parameters (what it was asked to do) and tags (labels given at creation).

## Finding runs

The Runs page, `cereyan runs ls`, `GET /api/runs`, `Client.runs()`, and the MCP `list_runs` tool all filter by flow, project, state type and name, tags, name, and time, and page by keyset cursor. `GET /api/runs`, the Runs page, and `list_runs` also search parameters and attributes by exact value, `?params=day=2026-09-01` or `?attributes=region=eu`, comparing the stored value as text; such a search covers the last 30 days unless a start bound is given, because it reads the JSON rather than an index. `POST /api/runs/bulk` takes a `filter` object with the same fields and an `action` of `cancel`, `rerun`, or `delete` and counts what it would touch before doing it (`dry_run` defaults to true). Task runs have the same across `GET /api/task-runs`.

## Comparing runs

When a run fails or slows down, the question is what changed since the last good one. `GET /api/runs/compare?ids=a,b` answers it in one document: the parameters and attributes that differ, the duration delta, every task of each run's latest pass matched by key with its state and duration on both sides, the first task that diverged, the error lines and failure message that only the second run has, and the artifacts that differ by key. The first id is the baseline, so "new" means present on the second run only; the two runs can be of different flows, and then fewer rows match. `Client.compare_runs(a, b)`, `cereyan runs compare A B`, and the MCP `compare_runs` tool return the same document; the Runs page offers Compare when two runs are selected, and a run's overflow menu compares it with the previous run of its flow.

Related: [Tasks](tasks.md), [Events and rules](events-and-rules.md), [Retry, time out and survive crashes](../guides/retries-timeouts-crashes.md).
