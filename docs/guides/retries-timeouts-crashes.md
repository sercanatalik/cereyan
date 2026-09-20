# How to retry, time out, and survive crashes

Failures come in three shapes: the code raised, the code ran too long, or the process died. Each has its own option.

## Retry a task or a flow

Set `retries` on the task. The task run goes through `AwaitingRetry`, waits, then executes again as `Retrying`; only after the last retry does it fail.

```python
from cereyan import flow, task, exponential

calls = []

@task(retries=3, retry_delay=exponential(base=0.01, jitter=0.01, maximum=1))
def fetch() -> str:
    calls.append(1)
    if len(calls) < 3:
        raise ConnectionError("try again")
    return "data"

@flow
def sync() -> str:
    return fetch()

assert sync() == "data"
assert len(calls) == 3
```

`retry_delay` takes seconds, a list of per-attempt seconds (`[1, 5, 30]`, the last value repeats), or `exponential(base, jitter, maximum)` for `base * 2**attempt` plus jitter, capped at `maximum`.

`retries` on a flow retries the whole run: a new attempt of the same run, with `failure_count` increased. Tasks that already completed run again unless they are cached or have a target.

```python
from cereyan import flow, task

seen = []

@flow(retries=1, retry_delay=0)
def whole_run() -> int:
    seen.append(1)
    if len(seen) == 1:
        raise RuntimeError("transient")
    return len(seen)

assert whole_run() == 2
```

## Retry only what a retry can fix

A retry helps when the failure was luck: a connection refused, a lock held, a rate limit. It does not help when the input is malformed or the credentials are wrong, and three attempts at a doomed call cost three times as much and fail three times as slowly.

`retry_on` names the failures worth retrying. Anything else ends the attempt at once.

```python
from cereyan import flow, task

attempts = []

@task(retries=3, retry_delay=0, retry_on=(ConnectionError,))
def load(value: str) -> str:
    attempts.append(value)
    raise ValueError("malformed row")

@flow
def ingest() -> None:
    load("bad")

try:
    ingest()
except Exception:
    pass
assert len(attempts) == 1
```

`retry_when` decides case by case. It receives the exception and the attempt that just failed, and returning `False` stops the retries; use it when the type alone does not say enough, such as an HTTP error whose status code does.

```python
from cereyan import flow, task

def worth_retrying(exc: BaseException, attempt: int) -> bool:
    return "429" in str(exc)

@task(retries=5, retry_delay=0, retry_when=worth_retrying)
def call_api() -> None:
    raise RuntimeError("403 forbidden")
```

Both apply together when both are set, and each can veto. A `retry_when` that raises stops the retry too, and the failure recorded is the original one, not the predicate's.

`Abort` says it at the point of failure, whatever `retries` allows:

```python
from cereyan import Abort, flow, task

@task(retries=5, retry_delay=0)
def parse(row: str) -> int:
    if not row.isdigit():
        raise Abort(f"not a number: {row!r}")
    return int(row)

@flow
def read() -> None:
    parse("twelve")
```

The run fails on its first attempt with `abort` in its state details, so a refused retry is distinguishable from an exhausted one in the UI and in rules. `crash_retries` is unaffected: a crash has no exception to judge.

## Retry a finished run from where it failed

When a run has failed and the cause is fixed, retry it rather than run it again from scratch. `POST /api/runs/{id}/retry`, `Client.retry(run_id)`, `cereyan retry <run>`, the MCP `rerun_run` tool's `from`, and **Retry from failure** on the run page all create a new run of the same flow and parameters, linked to the original (`created_by = retry:<id>`, the next `attempt`), seeded with the original's [checkpoints](../concepts/runs-and-states.md#checkpoints). The new run replays every task that had completed as `Replayed` and executes from the task that failed, plus everything that waited on it. The original keeps its history.

`from` picks the point: `failure` (the default), `start` for a clean rerun that is still linked, or a task's dynamic key such as `transform-0`, which the run page offers as **Rerun from here** on each task. That task executes, and so does everything after it, because replay ends at the first task that runs; `invalidated` in the response lists the point and the tasks recorded as waiting on it. The Runs page's **Retry** and the bulk action `retry` do this for every finished run a filter matches.

```{.python fixture:served}
run = served.client.run("etl", day="2026-03-03")
served.wait_run(run["id"])
out = served.client.retry(run["id"], from_="start")
assert out["retry_of"] == run["id"] and out["run"]["parent_run_id"] == run["id"]
```

Automatic in-process retries can resume the same way: `@flow(retries=2, checkpoint=True)` replays the completed prefix on each retry instead of running it again.

## Time out

`timeout_seconds` on a task fails the task run as `TimedOut` after the limit; on a flow it fails the run. A timed-out task still counts toward `retries`, so `retries=2, timeout_seconds=30` gives three attempts of thirty seconds each.

```python
import time
from cereyan import flow, task

@task(timeout_seconds=0.2)
def slow() -> None:
    time.sleep(5)

@flow
def limited() -> None:
    slow()

try:
    limited()
except TimeoutError as exc:
    assert "exceeded" in str(exc)
```

With the default thread runner a timed-out task's thread is abandoned; with a `ProcessRunner` the worker process is terminated.

A flow's timeout is enforced by the server when the flow is served, which records `TimedOut` and ends the engine, and offline by interrupting the flow in its own process, which raises `TimeoutError`. The interrupt reaches Python code and `time.sleep` on every platform, and any blocking system call on Linux and macOS. A flow stuck in a call the platform cannot interrupt, such as a loop inside a C extension or most blocking I/O on Windows, is timed out when the call returns offline, and ended by the server when served. Only the main thread can be interrupted: a flow called from another thread runs without its timeout and logs a warning saying so.

## Survive a crash

A **crash** is the engine process dying while a run executes: an out-of-memory kill, a segfault in a C extension, a machine reboot. The server notices through missed heartbeats and a dead PID, marks the run `Crashed`, and reruns it up to `crash_retries` times (the decorator, then `[defaults] crash_retries` in `cereyan.toml`, then 5) before it stays `Failed`. Crash reruns carry `created_by = crash:<n>`.

```python
from cereyan import flow

@flow(crash_retries=2)
def fragile() -> None:
    ...
```

A crash is not a failure of your code, so it does not consume `retries`. The rerun starts with the crashed run's [checkpoints](../concepts/runs-and-states.md#checkpoints): every task that completed before the crash is `Replayed` from its stored result, and execution resumes at the first task that did not finish. That is at-least-once, since a task that completed in the moments before the crash may run again, so still write outputs through [targets](../concepts/targets-caching-results.md) when a repeat would be harmful. In-process retries replay too when the flow declares `checkpoint=True`.

## See what happened

The run page's Details tab shows every state with its message; the failing exception's type, message, and traceback are in the state details and the log. The MCP `explain_failure` tool and the `diagnose_run` prompt collect the same for an agent.

Related: [Run code on state changes](state-hooks.md), [Runs and states](../concepts/runs-and-states.md).
