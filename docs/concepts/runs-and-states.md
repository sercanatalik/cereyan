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

Retries, crash reruns, and resumes after a pause are new attempts of the same run: the run keeps its id and history, `failure_count` or `crash_count` grows, and the task runs of the new attempt are recorded alongside the old ones. Tasks marked `cache=INPUTS` return their stored result on a replay instead of executing again.

## Finding runs

The Runs page, `cereyan runs ls`, `GET /api/runs`, `Client.runs()`, and the MCP `list_runs` tool all filter by flow, project, state type and name, tags, name, and time, and page by keyset cursor. Task runs have the same across `GET /api/task-runs`.

Related: [Tasks](tasks.md), [Events and rules](events-and-rules.md), [Retry, time out and survive crashes](../guides/retries-timeouts-crashes.md).
