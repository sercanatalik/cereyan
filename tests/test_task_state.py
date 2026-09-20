"""M2.6 task state store: a task's notes survive retries, passes, crash reruns
and retries from failure; entries go with the run."""

from __future__ import annotations

import json
import os
import time

import pytest

from cereyan.client import ApiError
from server_helpers import ServerProcess

PIPELINE = '''
import os
from cereyan import App, task, task_state

app = App("notes")
TRACE = os.environ["NOTES_TRACE"]

def note(line):
    with open(TRACE, "a") as fh:
        fh.write(line + "\\n")

@task(retries=1, retry_delay=0)
def submit_job(day: str):
    job = task_state.get("job_id")
    if job is None:
        job = f"job-{day}"
        task_state.set("job_id", job)
        note(f"started:{job}")
        raise RuntimeError("lost the connection after starting")
    note(f"resumed:{job}")
    return job

@app.flow
def pipeline(day: str = "2026-09-20"):
    task_state.set("flow_note", {"day": day})
    return submit_job(day)

@task
def remember_then_die():
    if task_state.get("job_id") is None:
        task_state.set("job_id", "job-7")
        note("started:job-7")
        os._exit(7)
    note("resumed:job-7")
    return "ok"

@app.flow(crash_retries=1)
def crashy():
    return remember_then_die()

@task
def keep_then_fail():
    seen = task_state.get("attempts", 0)
    task_state.set("attempts", seen + 1)
    if seen == 0:
        raise RuntimeError("first time fails")
    return seen

@app.flow
def failing():
    return keep_then_fail()
'''


@pytest.fixture
def srv(isolated_home, tmp_path):
    from cereyan import engine

    engine.close_store()
    d = tmp_path / "notes"
    d.mkdir()
    (d / "pipeline.py").write_text(PIPELINE)
    trace = tmp_path / "trace.txt"
    server = ServerProcess(str(isolated_home), str(d), env={"NOTES_TRACE": str(trace), "CEREYAN_FAST_CRASH_RERUN": "1"})
    server.trace = trace
    try:
        yield server
    finally:
        server.stop()


def lines(srv):
    return srv.trace.read_text().splitlines() if srv.trace.exists() else []


def wait_until(fn, timeout=40):
    deadline = time.time() + timeout
    while time.time() < deadline:
        v = fn()
        if v:
            return v
        time.sleep(0.2)
    raise AssertionError("condition not met in time")


def test_offline_notes_survive_retries_and_flow_scope(isolated_home):
    from cereyan import engine, flow, task, task_state
    from cereyan.exceptions import CereyanError

    engine.close_store()
    starts = []

    @task(retries=2, retry_delay=0)
    def submit():
        job = task_state.get("job_id")
        if job is None:
            task_state.set("job_id", "j-1")
            starts.append(1)
            raise RuntimeError("boom")
        assert task_state.items() == {"job_id": "j-1"}
        return job

    @flow
    def f():
        task_state.set("cursor", 42)
        assert task_state.get("cursor") == 42
        assert task_state.get("missing", "dflt") == "dflt"
        out = submit()
        assert task_state.delete("cursor") and task_state.get("cursor") is None
        return out

    assert f() == "j-1" and starts == [1]
    with pytest.raises(CereyanError):
        task_state.get("x")

    @task
    def too_big():
        task_state.set("blob", "x" * 70_000)

    @flow
    def g():
        too_big()

    with pytest.raises(Exception):
        g()
    store = engine.get_store()
    run = json.loads(store.list_runs())["items"][-1]
    rows = json.loads(store.task_state_list(run["id"]))
    assert [(r["scope"], r["key"], r["value"]) for r in rows] == [("submit-0", "job_id", "j-1")]


def test_served_notes_list_retry_crash_and_cascade(srv):
    c = srv.client
    run = c.run("pipeline", day="2026-09-20")
    assert srv.wait_run(run["id"])["state"]["type"] == "Completed"
    assert lines(srv) == ["started:job-2026-09-20", "resumed:job-2026-09-20"]
    rows = c._request("GET", f"/api/runs/{run['id']}/state")
    assert [(r["scope"], r["key"], r["value"]) for r in rows] == [("", "flow_note", {"day": "2026-09-20"}), ("submit_job-0", "job_id", "job-2026-09-20")]
    one = c._request("GET", f"/api/runs/{run['id']}/state", params={"scope": "submit_job-0", "key": "job_id"})
    assert one == {"found": True, "value": "job-2026-09-20"}
    assert c._request("GET", f"/api/runs/{run['id']}/state", params={"scope": "nope", "key": "job_id"})["found"] is False
    with pytest.raises(ApiError) as err:
        c._request("POST", f"/api/runs/{run['id']}/state", body={"scope": "", "key": "", "value": 1})
    assert err.value.status == 422
    # A crash rerun reads what the original attempt noted.
    crashed = c.run("crashy")
    rerun = wait_until(lambda: next((r for r in c.runs(flow="crashy", limit=20)["items"] if r["created_by"] == f"crash:{crashed['id']}"), None))
    assert srv.wait_run(rerun["id"])["state"]["type"] == "Completed"
    assert lines(srv)[2:] == ["started:job-7", "resumed:job-7"]
    assert c._request("GET", f"/api/runs/{rerun['id']}/state") == []
    # ... and so does a retry from failure.
    failed = c.run("failing")
    assert srv.wait_run(failed["id"])["state"]["type"] == "Failed"
    out = c.retry(failed["id"])
    assert srv.wait_run(out["run"]["id"])["state"]["type"] == "Completed"
    own = c._request("GET", f"/api/runs/{out['run']['id']}/state")
    assert [(r["key"], r["value"]) for r in own] == [("attempts", 2)]
    # Deleting the run takes its entries along.
    c._request("DELETE", f"/api/runs/{run['id']}")
    with pytest.raises(ApiError) as err:
        c._request("GET", f"/api/runs/{run['id']}/state")
    assert err.value.status == 404
