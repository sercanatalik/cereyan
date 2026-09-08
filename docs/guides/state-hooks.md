# How to run code on state changes

Hooks are plain functions called in the engine when a run or task run reaches a state. Use them for in-process reactions such as closing a connection, writing a marker file, or posting a metric. For reactions that should happen even when the process is gone, or that involve other flows, use [rules](rules.md) instead.

## Flow hooks

```python
from cereyan import flow

events = []

def note(flow, run, state):
    events.append((flow.name, state["type"]))

@flow(on_completion=[note], on_failure=[note], on_crashed=[note], on_cancellation=[note])
def pipeline() -> str:
    return "done"

pipeline()
assert events == [("pipeline", "Completed")]
```

Each hook receives the `Flow`, the run as a dict (`id`, `name`, `parameters`, `state`, ...), and the state dict that triggered it. Hooks run after the state is recorded, in the order given. An exception in a hook is logged with the run and does not change the run's state.

| Option | Called when |
|---|---|
| `on_completion` | The run ends `Completed` |
| `on_failure` | The run ends `Failed`, including `TimedOut` |
| `on_crashed` | The server marks the run `Crashed`; runs on the server side since the engine is gone |
| `on_cancellation` | The run ends `Cancelled` |

## Task hooks

Tasks accept `on_completion`, `on_failure`, and `on_cancellation` with the same signature, receiving the `Task` instead of the flow:

```python
from cereyan import flow, task

seen = []

def audit(task, run, state):
    seen.append((task.name, state["type"], state.get("message")))

@task(on_failure=[audit])
def load() -> None:
    raise ValueError("bad rows")

@flow
def etl() -> None:
    load()

try:
    etl()
except ValueError:
    pass
assert seen[0][:2] == ("load", "Failed")
```

## Choosing between hooks and rules

| | Hooks | Rules |
|---|---|---|
| Run where | In the engine, with your code | In the server |
| Survive a crash | No, except `on_crashed` | Yes |
| Can start other flows | Through the client | Yes, `run_flow` |
| Configurable at runtime | No | Data rules yes |
| Work offline | Yes | Code rules, once registered |

Related: [React to events with rules](rules.md), [Retry, time out and survive crashes](retries-timeouts-crashes.md).
