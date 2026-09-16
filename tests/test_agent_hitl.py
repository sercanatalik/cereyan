"""1.2 human-in-the-loop: wait_for_input pauses, resume replays with the answer."""

from __future__ import annotations

import io
import os

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

@app.flow
def two_questions():
    a = wait_for_input("First?")
    note(f"first:{a}")
    b = wait_for_input("Second?")
    note(f"second:{b}")
    return [a, b]

@app.flow
def same_prompt():
    a = wait_for_input("Are you sure?")
    b = wait_for_input("Are you sure?")
    return [a, b]

@task
def asks():
    return wait_for_input("From a task?")

@app.flow
def asks_from_a_task():
    return asks.submit().result()
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


def test_resume_records_the_task_runs_of_its_pass(hitl):
    run = start(hitl, "approve", n=3)
    paused(hitl, run["id"])
    hitl.client.resume(run["id"], {"approve": True})
    assert hitl.wait_run(run["id"])["state"]["type"] == "Completed"
    tasks = hitl.client._request("GET", f"/api/runs/{run['id']}/tasks")
    by_pass: dict[int, list[str]] = {}
    for t in tasks:
        by_pass.setdefault(t["pass"], []).append(t["dynamic_key"])
    # The first execution recorded `prepare-0`. The resumed one is a new pass:
    # it records its replay of that same call and the work that follows, which
    # used to be dropped because the engine restarted the report sequence.
    assert by_pass[0] == ["prepare-0"]
    assert sorted(by_pass[1]) == ["apply_decision-0", "prepare-0"]
    first = hitl.client._request("GET", f"/api/runs/{run['id']}/tasks?pass=0")
    assert [t["dynamic_key"] for t in first] == ["prepare-0"]
    events = hitl.client._request("GET", f"/api/events?run_id={run['id']}&name=task_run.*")["items"]
    assert any(e["payload"].get("dynamic_key") == "apply_decision-0" for e in events)
    # The graph is one pass, so no edge joins task runs that never ran together.
    graph = hitl.client._request("GET", f"/api/runs/{run['id']}/graph")
    assert sorted(n["dynamic_key"] for n in graph["nodes"]) == ["apply_decision-0", "prepare-0"]


def test_resume_needs_paused_state(hitl):
    o = start(hitl, "other")
    hitl.wait_run(o["id"])
    with pytest.raises(ApiError) as err:
        hitl.client.resume(o["id"], {"x": 1})
    assert err.value.status == 409


def test_two_questions_each_wait_for_their_own_answer(hitl):
    run = start(hitl, "two_questions")
    first = paused(hitl, run["id"])
    assert first["state"]["message"] == "First?"
    assert first["state"]["details"]["index"] == 0
    hitl.client.resume(run["id"], "A")
    second = hitl.wait_run(
        run["id"],
        until=lambda r: r["state"]["type"] == "Paused" and r["state"]["message"] == "Second?",
    )
    assert second["state"]["details"]["index"] == 1
    hitl.client.resume(run["id"], "B")
    assert hitl.wait_run(run["id"])["state"]["type"] == "Completed"
    # The body replays from the top on each resume, so the first question is
    # reached again and answered from the store; the second gets its own
    # answer instead of the first one.
    assert lines(hitl) == ["first:A", "first:A", "second:B"]


def test_two_questions_with_the_same_prompt_both_pause(hitl):
    run = start(hitl, "same_prompt")
    paused(hitl, run["id"])
    hitl.client.resume(run["id"], "yes")
    again = hitl.wait_run(
        run["id"],
        until=lambda r: r["state"]["type"] == "Paused" and r["state"]["details"].get("index") == 1,
    )
    assert again["state"]["message"] == "Are you sure?"
    hitl.client.resume(run["id"], "yes again")
    assert hitl.wait_run(run["id"])["state"]["type"] == "Completed"


def test_asking_from_a_task_is_refused(hitl):
    run = start(hitl, "asks_from_a_task")
    done = hitl.wait_run(run["id"])
    assert done["state"]["type"] == "Failed"
    assert "flow body" in (done["state"]["message"] or "")


def test_answers_are_dropped_when_the_run_ends(hitl):
    run = start(hitl, "two_questions")
    paused(hitl, run["id"])
    hitl.client.resume(run["id"], "A")
    hitl.wait_run(
        run["id"],
        until=lambda r: r["state"]["type"] == "Paused" and r["state"]["details"].get("index") == 1,
    )
    hitl.client.resume(run["id"], "B")
    assert hitl.wait_run(run["id"])["state"]["type"] == "Completed"
    stored = hitl.client._request("GET", f"/api/runs/{run['id']}/input")
    assert stored["answers"] == {}
    assert stored["input"] is None and stored["pending"] is None


def test_a_changed_prompt_at_the_same_position_asks_again():
    from cereyan import context
    from cereyan.exceptions import RunPaused
    from cereyan.inputs import wait_for_input

    class Stub:
        offline = False

        def get_input(self, index):
            return {"prompt": "Approve the load?", "input": {"approve": True}}

    ctx = context.RunContext(
        id=1, external_id="x", name="r", flow=None, parameters={}, backend=Stub()
    )
    token = context.set_run(ctx)
    try:
        assert wait_for_input("Approve the load?") == {"approve": True}
        # The same position, a different question: the stored answer is not
        # handed to it.
        ctx._input_index = 0
        with pytest.raises(RunPaused) as err:
            wait_for_input("Publish to production?")
        assert err.value.index == 0
    finally:
        context.reset_run(token)


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
