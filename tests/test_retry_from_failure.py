"""M2.2 retry from failure: a linked run replays the finished prefix and
executes from the failure, from a chosen task, or from the start."""

from __future__ import annotations

import json
import os
import time

import pytest

from cereyan.client import ApiError
from server_helpers import ServerProcess

PIPELINE = '''
import os
from cereyan import App, task

app = App("retry")
TRACE = os.environ["RETRY_TRACE"]
FIX = os.environ["RETRY_FIX"]

def note(line):
    with open(TRACE, "a") as fh:
        fh.write(line + "\\n")

@task
def load(day: str):
    note(f"load:{day}")
    return {"day": day, "rows": 3}

@task
def transform(data: dict):
    note(f"transform:{data['day']}")
    if not os.path.exists(FIX):
        raise RuntimeError("bad rows")
    return data["rows"] * 2

@task
def audit(data: dict):
    note(f"audit:{data['day']}")
    return "audited"

@task
def publish(n: int, receipt: str):
    note(f"publish:{n}:{receipt}")
    return n

@app.flow
def pipeline(day: str = "2026-09-20"):
    data = load(day)
    receipt = audit(data)
    n = transform(data)
    return publish(n, receipt)
'''


@pytest.fixture
def srv(isolated_home, tmp_path):
    from cereyan import engine

    engine.close_store()
    d = tmp_path / "retry"
    d.mkdir()
    (d / "pipeline.py").write_text(PIPELINE)
    trace = tmp_path / "trace.txt"
    fix = tmp_path / "fixed"
    server = ServerProcess(str(isolated_home), str(d), env={"RETRY_TRACE": str(trace), "RETRY_FIX": str(fix)})
    server.trace = trace
    server.fix = fix
    try:
        yield server
    finally:
        server.stop()


def lines(srv):
    return srv.trace.read_text().splitlines() if srv.trace.exists() else []


def states(c, run_id):
    return {t["dynamic_key"]: t["state"]["name"] for t in c.task_runs(run_id)}


def test_retry_from_failure_replays_the_finished_prefix(srv, run_cli):
    c = srv.client
    failed = c.run("pipeline", day="2026-09-20")
    assert srv.wait_run(failed["id"])["state"]["type"] == "Failed"
    assert lines(srv) == ["load:2026-09-20", "audit:2026-09-20", "transform:2026-09-20"]
    assert states(c, failed["id"]) == {"load-0": "Completed", "audit-0": "Completed", "transform-0": "Failed"}
    # Not fixed yet: the retry replays load and audit and fails in transform again.
    out = c.retry(failed["id"])
    assert out["retry_of"] == failed["id"] and out["from"] == "failure"
    assert out["replays"] == 2 and out["invalidated"] == ["transform-0"]
    again = out["run"]
    assert again["parent_run_id"] == failed["id"] and again["created_by"] == f"retry:{failed['id']}" and again["attempt"] == 1
    assert srv.wait_run(again["id"])["state"]["type"] == "Failed"
    assert lines(srv)[3:] == ["transform:2026-09-20"]
    assert states(c, again["id"])["load-0"] == "Replayed" and states(c, again["id"])["audit-0"] == "Replayed"
    # Fixed: the retry of the retry finishes, executing transform and publish only.
    srv.fix.write_text("ok")
    result = run_cli("retry", str(again["id"]))
    assert result.returncode == 0, result.stderr
    assert "retries" in result.stdout and "2 tasks replay" in result.stdout
    third_id = int(result.stdout.split()[1])
    done = srv.wait_run(third_id)
    assert done["state"]["type"] == "Completed" and done["parent_run_id"] == again["id"] and done["attempt"] == 2
    assert lines(srv)[4:] == ["transform:2026-09-20", "publish:6:audited"]
    assert states(c, third_id) == {"load-0": "Replayed", "audit-0": "Replayed", "transform-0": "Completed", "publish-0": "Completed"}
    # The originals are untouched.
    assert c.get_run(failed["id"])["state"]["type"] == "Failed"
    events = c._request("GET", f"/api/events?run_id={third_id}&name=task_run.replayed")["items"]
    assert len(events) == 2


def test_retry_from_a_task_and_from_the_start(srv):
    c = srv.client
    srv.fix.write_text("ok")
    good = c.run("pipeline", day="2026-09-21")
    assert srv.wait_run(good["id"])["state"]["type"] == "Completed"
    n = len(lines(srv))
    # From transform: load and audit replay; transform executes, and publish
    # after it, since replay ends at the first task that runs. The tasks were
    # called directly, so the recorded graph names only transform itself.
    out = c.retry(good["id"], from_="transform-0")
    assert out["invalidated"] == ["transform-0"] and out["replays"] == 3
    srv.wait_run(out["run"]["id"])
    assert lines(srv)[n:] == ["transform:2026-09-21", "publish:6:audited"]
    n = len(lines(srv))
    # From audit: only load replays; everything after audit executes.
    out = c.retry(good["id"], from_="audit-0")
    assert out["invalidated"] == ["audit-0"] and out["replays"] == 3
    srv.wait_run(out["run"]["id"])
    assert lines(srv)[n:] == ["audit:2026-09-21", "transform:2026-09-21", "publish:6:audited"]
    n = len(lines(srv))
    # From the start: everything executes and nothing is seeded.
    out = c.retry(good["id"], from_="start")
    assert out["replays"] == 0 and len(out["invalidated"]) == 4
    srv.wait_run(out["run"]["id"])
    assert len(lines(srv)) - n == 4
    with pytest.raises(ApiError) as err:
        c.retry(good["id"], from_="nope-0")
    assert err.value.status == 422
    # Bulk: every Failed run gets a retry.
    srv.fix.unlink()
    bad = c.run("pipeline", day="2026-09-22")
    srv.wait_run(bad["id"])
    srv.fix.write_text("ok")
    count = c._request("POST", "/api/runs/bulk", body={"filter": {"state_type": "Failed", "flow": "pipeline"}, "action": "retry", "dry_run": False})
    assert count["affected"] >= 1
    retries = [r for r in c.runs(flow="pipeline", limit=50)["items"] if r["created_by"] == f"retry:{bad['id']}"]
    assert len(retries) == 1 and srv.wait_run(retries[0]["id"])["state"]["type"] == "Completed"


def test_retry_refuses_an_active_run(srv):
    c = srv.client
    srv.fix.write_text("ok")
    run = c.run("pipeline", day="2026-09-23", delay=60)
    with pytest.raises(ApiError) as err:
        c.retry(run["id"])
    assert err.value.status == 409
    c.cancel(run["id"])
    srv.wait_run(run["id"])
    out = c.retry(run["id"])
    assert out["from"] == "failure" and out["replays"] == 0
    assert srv.wait_run(out["run"]["id"])["state"]["type"] == "Completed"
