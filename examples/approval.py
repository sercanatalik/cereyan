# ---
# title: Approval
# description: A flow that pauses for a human decision and resumes with the answer.
# order: 5
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
# Each question carries a JSON schema, which the run page turns into a form. The flow
# asks twice: whether to load the day, then whether to publish it. Every answer is
# stored against the question it answered, so the replay after the second answer gets
# `approve` for the first question and `publish` for the second. The flow is listed in
# the `releases` group of its project.

APPROVE = {"type": "object", "properties": {"approve": {"type": "boolean"}}, "required": ["approve"]}


@flow(group="releases")
def publish(day: date) -> str:
    rows = prepare(day)
    if not wait_for_input(f"Release {rows} rows for {day}?", schema=APPROVE)["approve"]:
        return "held"
    released = release(rows)
    if not wait_for_input(f"Publish {day} to the dashboard?", schema=APPROVE)["approve"]:
        return f"{released}, not published"
    return f"{released}, published"


# ## Driving it from a script
#
# Start the run, answer each question as the run pauses on it, and wait for the
# result.


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
    for question in ("Release", "Publish"):
        paused = wait_for(
            run["id"],
            lambda r: r["state"]["type"] == "Paused" and r["state"]["details"]["prompt"].startswith(question),
        )
        print("question:", paused["state"]["details"]["prompt"])
        client.default_client().resume(run["id"], {"approve": True})
    done = wait_for(run["id"], lambda r: r["state"]["type"] in ("Completed", "Failed", "Crashed", "Cancelled"))
    assert done["state"]["type"] == "Completed", done["state"]
    print("state:", done["state"]["type"])
