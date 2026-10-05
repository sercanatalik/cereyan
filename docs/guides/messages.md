# How to send a message to a running flow

Call `receive(topic)` where the flow needs something from outside: a go-ahead from another system, the id of a job it is waiting on, a batch of work. The run pauses with the topic visible in the UI and the API, the engine is released, and a message on that topic resumes it with the payload. Pausing needs a running server.

## Wait for a message

```python
from cereyan import flow, receive, task

@task
def stage(day: str) -> int:
    return 1200

@flow
def load_when_ready(day: str) -> str:
    rows = stage(day)
    go = receive("go")
    return f"loaded {rows} rows" if go["proceed"] else "held"
```

The first call to `receive` moves the run to `Paused` with `topic` in its state details, records `run.paused`, and ends the attempt. The tasks before the call are checkpointed, so the resumed attempt does not redo their work. `wait_for_input(prompt)` is `receive("input", prompt=prompt)`: a question for a person is a message on the `input` topic. See [Pause a run for approval](human-approval.md) for the form the run page builds from a question's schema.

## Send one

Against the [Messages example](../examples/messages.md), whose `load_when_ready` waits on `go`:

```{.python fixture:served}
run = served.client.run("load_when_ready", day="2026-03-01")
paused = served.wait_run(
    run["id"],
    until=lambda r: r["state"]["type"] == "Paused" and r["state"]["details"].get("topic") == "go",
)
served.client.send_message(run["id"], "go", {"proceed": True})
done = served.wait_run(run["id"])
assert done["state"]["type"] == "Completed"
```

`POST /api/runs/{id}/messages/{topic}` with `{"payload": ...}`, `Client.send_message`, and the MCP `send_message` tool do the same. The payload is any JSON. A run that is Paused on exactly that topic resumes at once; otherwise the message is queued and handed over when the run next reaches `receive` for that topic, so sending before the flow gets there is safe. Sending to a run that has ended answers 409. On the run page the band shows the topic, and **Resume** sends what you type as the message.

## Time out

```python
from cereyan import flow, receive

@flow
def nightly() -> str:
    go = receive("go", timeout=600, default={"proceed": False})
    return "loaded" if go["proceed"] else "held"
```

With `timeout`, the run resumes with `default` when nothing arrives in time. The timer is the server's, so the run holds no engine while it waits and the deadline survives a restart. The default comes back as a value, not as its JSON text: `default=None` is `None`, `default=42` is `42`.

## Publish state while the run goes on

```python
from cereyan import flow, publish_state

@flow
def load(day: str) -> str:
    publish_state("progress", {"step": "staged", "rows": 1200})
    return "loaded"
```

`publish_state(key, value)` stores a JSON value (up to 64 KB) under a key for the run. It is flow-level state, so it survives retries and resumes and is not scoped to a task. `GET /api/runs/{id}/state` lists every value the run stored as `{scope, key, value}`, with `scope` empty for the flow body and a task's dynamic key for [task state](../reference/python-api.md#task-state); `?key=progress` returns that one value as `{found, value}`. `Client.run_state` wraps both. The system that will send the message can read how far the run got before it decides.

## Several topics

Each `receive` call in a flow pauses in turn, and a flow can receive, resume, and receive again. Calls are numbered in the order the body reaches them, and a message belongs to the call it answered, so a second `receive` waits for its own message rather than taking the first one. A message is used only while the topic at that position still matches the one it was sent for; edit the flow so that a different topic sits there and the run waits again.

`receive` belongs in the flow body. Pausing works by raising out of the body, so a call from a task submitted with `submit` or `map` raises `CereyanError` instead of pausing the run. Code between the top of the flow and the call that is not in a task runs again on resume, so keep side effects inside tasks.

## Find the runs that wait

`GET /api/runs?state_type=Paused` lists them with their details, and the MCP `list_waiting_runs` tool returns each paused run's topic and prompt so an agent can answer the ones it owns. A paused run counts as active, appears under **Needs attention** on the dashboard, can be cancelled, and holds no engine and no resources. A proactive rule on `run.paused` unless `run.resumed` notices a run that waits too long; see [Pause a run for approval](human-approval.md#while-it-waits).

## Offline

Without a server, `receive("input", ...)` reads from the terminal as `wait_for_input` does, and any other topic raises `CereyanError`, so a script under cron fails fast instead of hanging.

Related: [Pause a run for approval](human-approval.md), [Wait without holding an engine](wait-durably.md), [Use cereyan with an AI agent](agents.md).
