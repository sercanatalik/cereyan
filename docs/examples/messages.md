# Messages

A flow that waits for a message on a topic and publishes its progress while it runs.

Source: [`examples/messages.py`](https://github.com/sercanatalik/cereyan/blob/main/examples/messages.py). Run it with `python examples/messages.py` while `cereyan serve examples/` is running.

`receive(topic)` parks the run in `Paused` until a message arrives on that topic, and
`publish_state` stores a value that other code can read while the run goes on. The
engine is released while the run waits; a message sent through the API, the client,
or the MCP `send_message` tool schedules a new attempt that replays the flow from the
top, where `receive` returns the message. This example runs against a server: start
`cereyan serve examples/` first.

```python
import time

from cereyan import client, flow, get_run_logger, publish_state, receive, task


@task
def stage(day: str) -> int:
    get_run_logger().info("staging %s", day)
    return 1200


@task
def load(rows: int) -> str:
    return f"loaded {rows} rows"
```

## The flow

The flow stages the day, publishes how far it got, and waits on the `go` topic for
the system that owns the warehouse to say the load may proceed. The wait has a
timeout: after ten minutes with no message, `receive` returns the default and the
run ends as held rather than waiting forever. The flow is listed in the `releases`
group of its project.

```python
@flow(group="releases")
def load_when_ready(day: str) -> str:
    rows = stage(day)
    publish_state("progress", {"step": "staged", "rows": rows})
    go = receive("go", timeout=600, default={"proceed": False})
    if not go["proceed"]:
        return "held"
    loaded = load(rows)
    publish_state("progress", {"step": "loaded", "rows": rows})
    return loaded
```

## Driving it from a script

Start the run, wait for it to pause on the topic, read what it published, send the
message, and wait for the result.

```python
def wait_for(run_id: int, predicate, timeout: float = 30.0) -> dict:
    deadline = time.time() + timeout
    while time.time() < deadline:
        run = client.get_run(run_id)
        if predicate(run):
            return run
        time.sleep(0.1)
    raise TimeoutError(f"run {run_id} did not reach the expected state")


if __name__ == "__main__":
    api = client.default_client()
    run = client.run("load_when_ready", day="2026-03-01")
    paused = wait_for(
        run["id"],
        lambda r: r["state"]["type"] == "Paused" and r["state"]["details"].get("topic") == "go",
    )
    print("waiting on topic:", paused["state"]["details"]["topic"])
    progress = api.run_state(run["id"], key="progress")
    print("progress:", progress["value"])
    assert progress["value"]["step"] == "staged"
    api.send_message(run["id"], "go", {"proceed": True})
    done = wait_for(run["id"], lambda r: r["state"]["type"] in ("Completed", "Failed", "Crashed", "Cancelled"))
    assert done["state"]["type"] == "Completed", done["state"]
    print("state:", done["state"]["type"])
    print("progress:", api.run_state(run["id"], key="progress")["value"])
```
