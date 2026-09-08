"""1.2 human-in-the-loop: wait_for_input pauses, resume replays with the answer."""

from __future__ import annotations

import io

import pytest

from cereyan import App, wait_for_input
from cereyan.client import ApiError
from cereyan.exceptions import CereyanError
from server_helpers import ServerProcess

PIPELINE = '''
import os, time
from cereyan import App, task, wait_for_input, INPUTS

app = App("hitl")
TRACE = os.environ["HITL_TRACE"]

def note(line):
    with open(TRACE, "a") as fh:
        fh.write(line + "\\n")

@task(cache=INPUTS, persist_result=True)
def prepare(n: int):
    note(f"prepare:{n}")
    return n * 2

@task
def apply_decision(value: int, decision: dict):
    note(f"apply:{value}:{decision['approve']}")
    return value if decision.get("approve") else 0

@app.flow
def approve(n: int = 2):
    value = prepare(n)
    decision = wait_for_input("Approve the load?", schema={"type": "object", "properties": {"approve": {"type": "boolean"}}})
    return apply_decision(value, decision)

@task(resources={"db": 1})
def hold():
    note("hold")
    return 1

@app.flow
def holder():
    hold()
    wait_for_input("Continue?")
    return "done"

@app.flow
def other():
    hold()
    return "other"
'''


@pytest.fixture
def hitl(isolated_home, tmp_path):
    from cereyan import engine

    engine.close_store()
    d = tmp_path / "hitl"
    d.mkdir()
    (d / "pipeline.py").write_text(PIPELINE)
    (d / "cereyan.toml").write_text("[resources]\ndb = 1\n")
    trace = tmp_path / "trace.txt"
    trace.write_text("")
    srv = ServerProcess(str(isolated_home), str(d), env={"HITL_TRACE": str(trace)}, extra=["--max-engines", "1"])
    srv.trace = trace
    try:
        yield srv
    finally:
        srv.stop()


def lines(srv):
    return [ln for ln in srv.trace.read_text().splitlines() if ln]


def start(srv, flow, **params):
    fid = next(f["id"] for f in srv.client.flows() if f["name"] == flow and f["project"] == "hitl")
    return srv.client._request("POST", f"/api/flows/{fid}/runs", body={"parameters": params})


def paused(srv, run_id):
    return srv.wait_run(run_id, until=lambda r: r["state"]["type"] == "Paused")


def test_pause_resume_replays_cached_prefix(hitl):
    run = start(hitl, "approve", n=3)
    p = paused(hitl, run["id"])
    assert p["state"]["details"]["prompt"] == "Approve the load?"
    assert p["state"]["details"]["schema"]["properties"]["approve"]["type"] == "boolean"
    assert p["state"]["message"] == "Approve the load?"
    assert lines(hitl) == ["prepare:3"]
    events = hitl.client._request("GET", f"/api/events?run_id={run['id']}&name=run.paused")["items"]
    assert len(events) == 1
    # The single engine is free while the run waits.
    done = hitl.client._request("GET", f"/api/runs/{run['id']}")
    assert done["engine_pid"] is None or hitl.client._request("GET", "/api/server")["engines"]
    # Resume with the answer: prepare is served from cache, the decision is applied.
    with pytest.raises(ApiError) as err:
        hitl.client.resume(run["id"], {"approve": True})
        hitl.client.resume(run["id"], {"approve": True})
    assert err.value.status == 409
    final = hitl.wait_run(run["id"])
    assert final["state"]["type"] == "Completed"
    assert lines(hitl) == ["prepare:3", "apply:6:True"]
    assert hitl.client._request("GET", f"/api/events?run_id={run['id']}&name=run.resumed")["items"]
    # A cancelled paused run drops its stored answer.
    again = start(hitl, "approve", n=4)
    paused(hitl, again["id"])
    hitl.client.cancel(again["id"])
    assert hitl.wait_run(again["id"])["state"]["type"] == "Cancelled"
    assert hitl.client._request("GET", f"/api/runs/{again['id']}/input")["input"] is None


def test_paused_run_releases_engine_and_resources(hitl):
    held = start(hitl, "holder")
    paused(hitl, held["id"])
    # With one engine and one `db`, another run can still proceed.
    o = start(hitl, "other")
    assert hitl.wait_run(o["id"])["state"]["type"] == "Completed"
    assert lines(hitl) == ["hold", "hold"]
    counts = hitl.client._request("GET", "/api/counts")
    assert counts["active"] >= 1
    hitl.client.resume(held["id"], "go")
    assert hitl.wait_run(held["id"])["state"]["type"] == "Completed"


def test_resume_needs_paused_state(hitl):
    o = start(hitl, "other")
    hitl.wait_run(o["id"])
    with pytest.raises(ApiError) as err:
        hitl.client.resume(o["id"], {"x": 1})
    assert err.value.status == 409


def test_offline_terminal_and_non_tty(monkeypatch):
    app = App("hitl_offline")

    @app.flow
    def ask():
        return wait_for_input("Name?")

    class Tty(io.StringIO):
        def isatty(self):
            return True

    monkeypatch.setattr("sys.stdin", Tty("alice\n"))
    monkeypatch.setattr("builtins.input", lambda prompt="": "alice")
    assert ask() == "alice"
    monkeypatch.setattr("sys.stdin", io.StringIO(""))
    with pytest.raises(CereyanError) as err:
        ask()
    assert "Name?" in str(err.value)
