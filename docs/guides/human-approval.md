# How to pause a run for approval

Call `wait_for_input` where the flow needs a decision. The run pauses with the question visible in the UI and the API, the engine is released, and the answer resumes it. Pausing needs a running server; offline, the question is asked on the terminal.

## Ask the question

```python
from datetime import date
from cereyan import flow, task, wait_for_input, INPUTS

@task(cache=INPUTS, persist_result=True)
def prepare(day: date) -> int:
    return 1200

@task
def release(rows: int) -> str:
    return f"released {rows}"

@flow
def publish(day: date) -> str:
    rows = prepare(day)
    decision = wait_for_input(
        f"Release {rows} rows for {day}?",
        schema={"type": "object", "properties": {"approve": {"type": "boolean"}}, "required": ["approve"]},
    )
    return release(rows) if decision["approve"] else "held"
```

The first call to `wait_for_input` moves the run to `Paused` with the prompt and schema in its state details, records `run.paused`, and ends the attempt. Mark the tasks before the question with `cache=INPUTS` so the resumed attempt does not redo their work.

## Answer it

On the run page, the Details tab shows the question with a form built from the schema and a **Resume** button. Programmatically:

```{.python fixture:served}
run = served.client.run("publish", day="2026-03-01")
paused = served.wait_run(run["id"], until=lambda r: r["state"]["type"] == "Paused")
assert "Release" in paused["state"]["details"]["prompt"]

served.client.resume(run["id"], {"approve": True})
done = served.wait_run(run["id"])
assert done["state"]["type"] == "Completed"
```

`POST /api/runs/{id}/resume` with `{"input": ...}`, `Client.resume`, and the MCP `resume_run` tool do the same. `GET /api/runs/{id}/input` returns the stored answer. Resuming a run that is not paused answers 409.

## What happens on resume

The answer is stored, `run.resumed` is recorded, and a new attempt of the same run is scheduled. It reruns the flow from the top: tasks with `cache=INPUTS` return their cached results as `Cached` task runs, `wait_for_input` returns the answer instead of pausing, and the rest of the flow executes. Any code between the top of the flow and the question that is not in a cached task runs again, so keep side effects inside tasks.

## While it waits

A paused run counts as active, appears on the dashboard, can be cancelled, and holds no engine and no resources. Use a proactive rule to notice a run that waits too long:

```python
from cereyan import App

app = App("approvals")

@app.rule(on="run.paused", flow="publish", unless="run.resumed", within=4 * 3600)
def nobody_answered(event, run):
    print(f"{run['name']} has been waiting for four hours")
```

## Several questions

Each `wait_for_input` call in a flow pauses in turn; the stored answer belongs to the attempt that asked, so a flow can ask, resume, and ask again.

## Offline

Without a server, `wait_for_input` reads the answer from the terminal (as JSON when a schema is given) and raises `CereyanError` when there is no interactive terminal, so scripts under cron fail fast instead of hanging.

Related: [Use cereyan with an AI agent](agents.md) for answering questions from an agent, [Cache task results](cache-results.md).
