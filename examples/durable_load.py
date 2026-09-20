# ---
# title: Durable load
# description: A task remembers the job it started, a durable sleep frees the engine, and a retry replays the finished prefix.
# order: 9
# fixture: served
# ---
#
# Three things keep a long load honest across attempts. `task_state` is a small note
# store scoped to the task and the run: a task writes the id of the job it submitted
# and finds it again on a retry, a crash rerun, or a retry from failure, so the job is
# never submitted twice. `sleep` above a minute pauses the run as `Sleeping` and
# frees the engine until the wake time (here the threshold is lowered so the
# example finishes quickly). And every completed task leaves a checkpoint, so a
# retry from failure replays what already finished and executes from the point you
# choose. This example runs against a server: start `cereyan serve examples/` first.

import time
from datetime import date

from cereyan import client, flow, get_run_logger, sleep, task, task_state


@task(retries=2, retry_delay=0)
def submit_job(day: date) -> str:
    """Submit the warehouse job once, whatever happens to this attempt."""
    job = task_state.get("job_id")
    if job is None:
        job = f"job-{day}"
        task_state.set("job_id", job)
        get_run_logger().info("submitted %s", job)
        raise ConnectionError("lost the connection right after submitting")
    get_run_logger().info("resuming with %s", job)
    return job


@task
def collect(job: str) -> str:
    return f"{job}: 1200 rows"


# ## The flow
#
# The first attempt of `submit_job` notes the job id and fails; the retry finds the
# note and returns it. `sleep(2, min_durable=1)` then parks the run as `Sleeping`
# with no engine attached, and the wake replays `submit_job` from its checkpoint
# rather than running it a third time.


@flow(group="loads")
def load_warehouse(day: date) -> str:
    job = submit_job(day)
    sleep(2, min_durable=1)
    return collect(job)


# ## Driving it from a script
#
# Run it, then retry the finished run from `collect-0`: the new run replays
# `submit_job-0` as `Replayed` and executes only `collect`.


def wait_for(run_id: int, predicate, timeout: float = 60.0) -> dict:
    deadline = time.time() + timeout
    while time.time() < deadline:
        run = client.get_run(run_id)
        if predicate(run):
            return run
        time.sleep(0.2)
    raise TimeoutError(f"run {run_id} did not reach the expected state")


TERMINAL = ("Completed", "Failed", "Crashed", "Cancelled")

if __name__ == "__main__":
    api = client.default_client()
    run = client.run("load_warehouse", day="2026-03-01")
    slept = wait_for(run["id"], lambda r: r["state"]["name"] == "Sleeping" or r["state"]["type"] in TERMINAL)
    print("while sleeping, engine attached:", slept["engine_pid"] is not None)
    done = wait_for(run["id"], lambda r: r["state"]["type"] in TERMINAL)
    assert done["state"]["type"] == "Completed", done["state"]
    notes = api._request("GET", f"/api/runs/{run['id']}/state")
    print("task state:", [(n["scope"], n["key"], n["value"]) for n in notes])

    again = api.retry(run["id"], from_="collect-0")
    print("replays:", again["replays"], "executes:", again["invalidated"])
    retried = wait_for(again["run"]["id"], lambda r: r["state"]["type"] in TERMINAL)
    assert retried["state"]["type"] == "Completed", retried["state"]
    names = {t["dynamic_key"]: t["state"]["name"] for t in api.task_runs(retried["id"])}
    print("retried task runs:", names)
    assert names["submit_job-0"] == "Replayed" and names["collect-0"] == "Completed"
