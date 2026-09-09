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

A flow's timeout is enforced by the server, which records `TimedOut` and ends the engine, and offline by an alarm in the running process. That alarm does not exist on Windows: a flow run by `python pipeline.py` or `cereyan run` there is not timed out at all, and `timeout_seconds` on it is accepted and ignored. Run the flow through `cereyan serve` for a timeout that holds on every platform. Task timeouts are unaffected.

## Survive a crash

A **crash** is the engine process dying while a run executes: an out-of-memory kill, a segfault in a C extension, a machine reboot. The server notices through missed heartbeats and a dead PID, marks the run `Crashed`, and reruns it up to `crash_retries` times (the decorator, then `[defaults] crash_retries` in `cereyan.toml`, then 5) before it stays `Failed`. Crash reruns carry `created_by = crash:<n>`.

```python
from cereyan import flow

@flow(crash_retries=2)
def fragile() -> None:
    ...
```

A crash is not a failure of your code, so it does not consume `retries`. Design flows so a rerun is safe: write outputs through [targets](../concepts/targets-caching-results.md) and cache expensive steps, and a crashed run picks up where the files say it left off.

## See what happened

The run page's Details tab shows every state with its message; the failing exception's type, message, and traceback are in the state details and the log. The MCP `explain_failure` tool and the `diagnose_run` prompt collect the same for an agent.

Related: [Run code on state changes](state-hooks.md), [Runs and states](../concepts/runs-and-states.md).
