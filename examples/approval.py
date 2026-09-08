# ---
# title: Approval
# description: A flow that pauses for a human decision and resumes with the answer.
# order: 4
# fixture: served
# ---
#
# `wait_for_input` parks the run in `Paused` with a question. The engine is released
# while the run waits; answering through the UI, the API, the client, or the MCP
# `resume_run` tool schedules a new attempt that replays the flow from the top, where
# tasks marked `cache=INPUTS` return their stored results and `wait_for_input`
# returns the answer. This example runs against a server: start
# `cereyan serve examples/` first.

import time
from datetime import date

from cereyan import INPUTS, flow, get_run_logger, task, wait_for_input
from cereyan import client


@task(cache=INPUTS, persist_result=True)
def prepare(day: date) -> int:
    get_run_logger().info("preparing %s", day)
    return 1200


@task
def release(rows: int) -> str:
    return f"released {rows} rows"


# ## The flow
#
# The question carries a JSON schema, which the run page turns into a form.


@flow
def publish(day: date) -> str:
    rows = prepare(day)
    decision = wait_for_input(
        f"Release {rows} rows for {day}?",
        schema={"type": "object", "properties": {"approve": {"type": "boolean"}}, "required": ["approve"]},
    )
    if not decision["approve"]:
        return "held"
    return release(rows)


# ## Driving it from a script
#
# Start the run, wait until it pauses, read the question, answer it, and wait for
# the result.


def wait_for(run_id: int, predicate, timeout: float = 30.0) -> dict:
    deadline = time.time() + timeout
    while time.time() < deadline:
        run = client.get_run(run_id)
        if predicate(run):
            return run
        time.sleep(0.1)
    raise TimeoutError(f"run {run_id} did not reach the expected state")


if __name__ == "__main__":
    run = client.run("publish", day="2026-03-01")
    paused = wait_for(run["id"], lambda r: r["state"]["type"] == "Paused")
    print("question:", paused["state"]["details"]["prompt"])
    client.default_client().resume(run["id"], {"approve": True})
    done = wait_for(run["id"], lambda r: r["state"]["type"] in ("Completed", "Failed", "Crashed", "Cancelled"))
    assert done["state"]["type"] == "Completed", done["state"]
    print("state:", done["state"]["type"])
