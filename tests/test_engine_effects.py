"""E2 engine effects: a flow body's events, artifacts, published state and
attributes happen once per run, and once per crash chain, however many passes
the body takes."""

from __future__ import annotations

import json
import time

import pytest

from server_helpers import ServerProcess

PIPELINE = '''
import os, time
from cereyan import App, task, emit_event, publish_state, wait_for_input, set_attributes
from cereyan.artifacts import create_markdown

app = App("effects")
TRACE = os.environ["EFFECTS_TRACE"]
MARK = os.environ["EFFECTS_MARK"]

def note(line):
    with open(TRACE, "a") as fh:
        fh.write(line + "\\n")

@app.rule(on="orders.ready")
def on_ready(event, run):
    note(f"rule:{event['payload']['day']}")

@app.flow
def ask_once(day: str = "d1"):
    emit_event("orders.ready", {"day": day})
    create_markdown("# before the question")
    publish_state("progress", 1)
    set_attributes(stage="asked")
    answer = wait_for_input("go?")
    publish_state("progress", 2)
    return answer

@app.rule(on="run.crashed", flow="dies_twice")
def on_crash(event, run):
    note(f"crashed:{run['id']}")

@app.flow(crash_retries=2)
def dies_twice():
    deaths = len(open(MARK + ".twice").read()) if os.path.exists(MARK + ".twice") else 0
    if deaths < 2:
        with open(MARK + ".twice", "a") as fh:
            fh.write("x")
        os._exit(7)
    return "third time"

@task
def flaky(day: str):
    if not os.path.exists(MARK + "." + day):
        open(MARK + "." + day, "w").close()
        raise RuntimeError("first time fails")
    return day

@app.flow
def emit_then_fail(day: str = "d3"):
    emit_event("orders.ready", {"day": day})
    return flaky(day)

@app.flow(crash_retries=1)
def die_after_emit(day: str = "d2"):
    emit_event("orders.ready", {"day": day})
    create_markdown("# before the crash")
    publish_state("progress", 1)
    if not os.path.exists(MARK):
        open(MARK, "w").close()
        time.sleep(1.5)  # let the report carrying the effects go out
        os._exit(7)
    return "survived"
'''


@pytest.fixture
def srv(isolated_home, tmp_path):
    from cereyan import engine

    engine.close_store()
    d = tmp_path / "effects"
    d.mkdir()
    (d / "pipeline.py").write_text(PIPELINE)
    trace = tmp_path / "trace.txt"
    server = ServerProcess(str(isolated_home), str(d), env={
        "EFFECTS_TRACE": str(trace), "EFFECTS_MARK": str(tmp_path / "died"), "CEREYAN_FAST_CRASH_RERUN": "1",
    })
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


def day_events(c, day):
    return [e for e in c.events("orders.ready", limit=500) if e["payload"].get("day") == day]


def offline_events(store, name):
    return json.loads(store.query_events(json.dumps({"name": name, "limit": 500, "ascending": True})))["items"]


# --- offline -----------------------------------------------------------------


def test_offline_snooze_does_not_emit_again(store):
    from cereyan import Snooze, emit_event, flow

    passes = []

    @flow
    def poll():
        emit_event("poll.started", {"n": 1})
        passes.append(1)
        if len(passes) < 3:
            raise Snooze(0.05)
        return "done"

    assert poll() == "done" and len(passes) == 3
    assert len(offline_events(store, "poll.started")) == 1


def test_offline_published_state_does_not_move_backwards(store):
    from cereyan import Snooze, flow, publish_state

    passes = []

    @flow
    def progress():
        publish_state("progress", 1)
        passes.append(1)
        if len(passes) == 1:
            raise Snooze(0.05)
        publish_state("progress", 2)
        return "done"

    writes = []
    real = store.task_state_set

    class Spy:
        def __getattr__(self, name):
            return getattr(store, name)

        def task_state_set(self, *args, **kwargs):
            if args[2] == "progress":
                writes.append(args[3])
            return real(*args, **kwargs)

    from cereyan.engine import backends

    original = backends.StoreBackend.__init__

    def spying_init(self, s, *args, **kwargs):
        original(self, Spy(), *args, **kwargs)

    backends.StoreBackend.__init__ = spying_init
    try:
        assert progress() == "done"
    finally:
        backends.StoreBackend.__init__ = original
    assert writes == ["1", "2"]


# --- served ------------------------------------------------------------------


def test_effects_before_a_question_happen_once(srv):
    c = srv.client
    run = c.run("ask_once", day="d1")
    srv.wait_run(run["id"], until=lambda r: r["state"]["type"] == "Paused")
    c.resume(run["id"], "yes")
    done = srv.wait_run(run["id"], timeout=30)
    assert done["state"]["type"] == "Completed", done["state"]
    assert len(day_events(c, "d1")) == 1
    assert len([a for a in c.artifacts(run["id"]) if a["kind"] == "markdown"]) == 1
    assert c.run_state(run["id"], key="progress") == {"found": True, "value": 2}
    wait_until(lambda: "rule:d1" in lines(srv))
    time.sleep(1)
    assert lines(srv).count("rule:d1") == 1


def test_a_crash_rerun_does_not_emit_again(srv):
    c = srv.client
    run = c.run("die_after_emit", day="d2")
    rerun = wait_until(lambda: next((r for r in c.runs(flow="die_after_emit", limit=20)["items"]
                                     if r["created_by"] == f"crash:{run['id']}"), None))
    assert srv.wait_run(rerun["id"])["state"]["type"] == "Completed"
    assert len(day_events(c, "d2")) == 1
    # Artifacts are the run's own records: the rerun shows its copy.
    assert len([a for a in c.artifacts(run["id"]) if a["kind"] == "markdown"]) == 1
    assert len([a for a in c.artifacts(rerun["id"]) if a["kind"] == "markdown"]) == 1
    # State is read through the chain, so the rerun lists the crashed run's value.
    assert sorted((r["key"], r["value"]) for r in c.run_state(rerun["id"])) == [("progress", 1), ("result", "survived")]


def test_offline_identical_emits_stay_distinct_and_changed_ones_repeat(store):
    from cereyan import Snooze, emit_event, flow

    passes = []

    @flow
    def ticks():
        emit_event("tick", {"n": 1})
        emit_event("tick", {"n": 1})
        emit_event("report", {"pass": len(passes)})  # changes every pass
        emit_event("after", {})
        passes.append(1)
        if len(passes) < 2:
            raise Snooze(0.05)
        return "done"

    assert ticks() == "done"
    assert len(offline_events(store, "tick")) == 2
    assert len(offline_events(store, "report")) == 2
    assert len(offline_events(store, "after")) == 1


def test_offline_effects_repeat_where_tasks_execute_again(store):
    from cereyan import Snooze, emit_event, flow

    attempts = []

    @flow(retries=1, retry_delay=0)
    def retried():
        emit_event("retried.start", {})
        attempts.append(1)
        if len(attempts) == 1:
            raise RuntimeError("boom")
        return "ok"

    assert retried() == "ok"
    assert len(offline_events(store, "retried.start")) == 2

    attempts.clear()

    @flow(retries=1, retry_delay=0, checkpoint=True)
    def replayed():
        emit_event("replayed.start", {})
        attempts.append(1)
        if len(attempts) == 1:
            raise RuntimeError("boom")
        return "ok"

    assert replayed() == "ok"
    assert len(offline_events(store, "replayed.start")) == 1

    passes = []

    @flow(checkpoint=False)
    def unjournaled():
        emit_event("unjournaled.start", {})
        passes.append(1)
        if len(passes) == 1:
            raise Snooze(0.05)
        return "ok"

    assert unjournaled() == "ok"
    assert len(offline_events(store, "unjournaled.start")) == 2


def test_offline_task_effects_are_not_journaled_and_artifacts_return_their_id(store):
    from cereyan import Snooze, emit_event, flow, task
    from cereyan.artifacts import create_markdown

    @task
    def inside():
        emit_event("from.task", {})
        return 1

    ids = []

    @flow
    def mixed():
        inside()
        ids.append(create_markdown("# once"))
        if len(ids) == 1:
            raise Snooze(0.05)
        return "ok"

    assert mixed() == "ok"
    run = json.loads(store.list_runs())["items"][0]
    slots = [e["slot"] for e in json.loads(store.effects(run["id"]))]
    # The task's emit is not journaled; the artifact and the result note are.
    assert slots == ["artifact:~markdown-0", "state:result-0"]
    assert ids[0] == ids[1] and ids[0]
    assert len(json.loads(store.artifacts(run["id"]))) == 1


def test_a_retry_from_failure_does_not_emit_again_but_one_from_the_start_does(srv):
    c = srv.client
    run = c.run("emit_then_fail", day="d3")
    assert srv.wait_run(run["id"])["state"]["type"] == "Failed"
    retry = c.retry(run["id"])
    assert srv.wait_run(retry["run"]["id"])["state"]["type"] == "Completed"
    assert len(day_events(c, "d3")) == 1
    again = c.retry(run["id"], from_="start")
    assert srv.wait_run(again["run"]["id"])["state"]["type"] == "Completed"
    assert len(day_events(c, "d3")) == 2


def test_a_per_run_rule_alerts_once_for_a_crash_chain(srv):
    c = srv.client
    run = c.run("dies_twice")
    last = wait_until(lambda: next((r for r in c.runs(flow="dies_twice", limit=20)["items"]
                                    if r["attempt"] == 2), None))
    assert srv.wait_run(last["id"])["state"]["type"] == "Completed"
    crashed = [r for r in c.runs(flow="dies_twice", limit=20)["items"] if r["state"]["type"] == "Crashed"]
    assert len(crashed) == 2
    time.sleep(1.5)
    assert [line for line in lines(srv) if line.startswith("crashed:")] == [f"crashed:{run['id']}"]
