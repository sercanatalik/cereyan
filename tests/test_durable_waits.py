"""M2.3 durable waits: sleep, wait for an event, wait for a target, and snooze
free the engine, wake on time, survive a restart, and work offline."""

from __future__ import annotations

import os
import time

import pytest

from server_helpers import ServerProcess

PIPELINE = '''
import os, time
from cereyan import App, task, sleep, wait_for_event, wait_for_target, Snooze, WaitTimeout, LocalTarget, emit_event

app = App("waits")
TRACE = os.environ["WAIT_TRACE"]
TARGET = os.environ["WAIT_TARGET"]

def note(line):
    with open(TRACE, "a") as fh:
        fh.write(line + "\\n")

@task
def prepare():
    note("prepare")
    return 1

@app.flow
def napper(seconds: float = 3.0):
    prepare()
    note("before")
    sleep(seconds, min_durable=1)
    note("after")
    return "rested"

@app.flow
def listener(day: str = "2026-09-20", within: float = 30.0):
    prepare()
    try:
        ev = wait_for_event("orders.ready", match={"day": day}, within=within)
    except WaitTimeout:
        note("timeout")
        return "timed out"
    note(f"got:{ev['payload']['day']}")
    return ev["payload"]["day"]

@app.flow
def announce(day: str = "2026-09-20"):
    emit_event("orders.ready", {"day": day, "rows": 3})

@app.flow
def self_announce(day: str = "2026-09-21"):
    # The run emits the event and only then waits for it: offline that returns at
    # once, and served it must too.
    emit_event("orders.ready", {"day": day, "rows": 1})
    ev = wait_for_event("orders.ready", match={"day": day}, within=20)
    note(f"self:{ev['payload']['day']}")
    return ev["payload"]["day"]

@task
def kick_announce(day: str):
    # Another run records the event while this one is still on its way to the wait.
    from cereyan import client
    other = client.run("announce", day=day)
    while client.get_run(other["id"])["state"]["type"] not in ("Completed", "Failed", "Crashed", "Cancelled"):
        time.sleep(0.05)
    return other["id"]

@app.flow
def early_listener(day: str = "2026-09-22", within: float = 20.0):
    kick_announce(day)
    try:
        ev = wait_for_event("orders.ready", match={"day": day}, within=within)
    except WaitTimeout:
        note("early:timeout")
        return "timed out"
    note(f"early:{ev['payload']['day']}")
    return ev["payload"]["day"]

@app.flow
def waits_twice(day: str = "2026-09-23", within: float = 4.0):
    first = wait_for_event("orders.ready", match={"day": day}, within=within)
    note(f"first:{first['id']}")
    try:
        second = wait_for_event("orders.ready", match={"day": day}, within=within)
        note(f"second:{second['id']}")
    except WaitTimeout:
        note("second:timeout")

@app.flow
def watcher(poke: float = 1.0, timeout: float | None = None):
    prepare()
    t = wait_for_target(LocalTarget(TARGET), poke=poke, timeout=timeout)
    note("target")
    return str(t.path) if hasattr(t, "path") else "ok"

@app.flow
def dozer():
    prepare()
    if not os.path.exists(TARGET):
        note("snooze")
        raise Snooze(2)
    note("done")
    return "done"
'''


@pytest.fixture
def srv(isolated_home, tmp_path):
    from cereyan import engine

    engine.close_store()
    d = tmp_path / "waits"
    d.mkdir()
    (d / "pipeline.py").write_text(PIPELINE)
    trace = tmp_path / "trace.txt"
    target = tmp_path / "target.txt"
    env = {"WAIT_TRACE": str(trace), "WAIT_TARGET": str(target)}
    server = ServerProcess(str(isolated_home), str(d), env=env)
    server.trace = trace
    server.target = target
    server.env = env
    server.dir = d
    try:
        yield server
    finally:
        server.stop()


def lines(srv):
    return srv.trace.read_text().splitlines() if srv.trace.exists() else []


def test_sleep_frees_the_engine_and_wakes_on_time(srv, isolated_home):
    c = srv.client
    t0 = time.time()
    run = c.run("napper", seconds=3)
    paused = srv.wait_run(run["id"], timeout=15, until=lambda r: r["state"]["type"] == "Paused")
    assert paused["state"]["name"] == "Sleeping"
    # The engine is free while the run sleeps: no engine names it as its current run.
    assert all(e.get("current_run") != run["id"] for e in c.server()["engines"])
    assert paused["state"]["details"]["wake_at"] >= int(t0 * 1_000_000) + 2_500_000
    assert lines(srv) == ["prepare", "before"]
    done = srv.wait_run(run["id"], timeout=30)
    assert done["state"]["type"] == "Completed" and time.time() - t0 >= 3
    # The task replayed from its checkpoint; body code between tasks runs again,
    # which is why side effects belong in tasks.
    assert lines(srv) == ["prepare", "before", "before", "after"]
    tasks = c.task_runs(run["id"])
    assert [t["state"]["name"] for t in tasks if t["pass"] == 1] == ["Replayed"]
    assert c._request("GET", f"/api/events?run_id={run['id']}&name=run.resumed")["items"]
    # A longer sleep survives a restart of the server.
    long = c.run("napper", seconds=6)
    srv.wait_run(long["id"], timeout=15, until=lambda r: r["state"]["name"] == "Sleeping")
    srv.stop()
    second = ServerProcess(str(isolated_home), str(srv.dir), env=srv.env)
    try:
        assert second.wait_run(long["id"], timeout=30)["state"]["type"] == "Completed"
    finally:
        second.stop()
        srv.proc = second.proc


def test_wait_for_event_matches_and_times_out(srv):
    c = srv.client
    run = c.run("listener", day="2026-09-20", within=30)
    waiting = srv.wait_run(run["id"], timeout=15, until=lambda r: r["state"]["type"] == "Paused")
    assert waiting["state"]["name"] == "AwaitingEvent" and waiting["state"]["details"]["event"] == "orders.ready"
    # Another day does not wake it; the right one does.
    other = c.run("announce", day="2026-09-19")
    srv.wait_run(other["id"])
    time.sleep(1)
    assert c.get_run(run["id"])["state"]["name"] == "AwaitingEvent"
    right = c.run("announce", day="2026-09-20")
    srv.wait_run(right["id"])
    done = srv.wait_run(run["id"], timeout=30)
    assert done["state"]["type"] == "Completed" and lines(srv)[-1] == "got:2026-09-20"
    # Nothing arrives within two seconds: the call raises WaitTimeout on replay.
    late = c.run("listener", day="never", within=2)
    done = srv.wait_run(late["id"], timeout=30)
    assert done["state"]["type"] == "Completed" and lines(srv)[-1] == "timeout"


def test_wait_for_target_and_snooze(srv):
    c = srv.client
    run = c.run("watcher", poke=1)
    waiting = srv.wait_run(run["id"], timeout=15, until=lambda r: r["state"]["type"] == "Paused")
    assert waiting["state"]["name"] == "AwaitingTarget"
    time.sleep(2)
    srv.target.write_text("here")
    done = srv.wait_run(run["id"], timeout=30)
    assert done["state"]["type"] == "Completed" and lines(srv)[-1] == "target"
    srv.target.unlink()
    # Snooze: the first attempt ends as Sleeping without a failure; the wake reruns the body.
    doze = c.run("dozer")
    paused = srv.wait_run(doze["id"], timeout=15, until=lambda r: r["state"]["type"] == "Paused")
    assert paused["state"]["name"] == "Sleeping" and paused["state"]["details"]["snoozes"] == 1
    assert paused["state"]["details"]["reason"] == "snooze"
    srv.target.write_text("here")
    done = srv.wait_run(doze["id"], timeout=30)
    assert done["state"]["type"] == "Completed" and done["failure_count"] == 0
    assert lines(srv)[-2:] == ["snooze", "done"]
    # A short target timeout raises WaitTimeout and fails the run.
    srv.target.unlink()
    out = c.run("watcher", poke=1, timeout=2)
    failed = srv.wait_run(out["id"], timeout=30)
    assert failed["state"]["type"] == "Failed" and "did not appear" in failed["state"]["message"]


def test_offline_waits_block_and_snooze_reruns(store, tmp_path):
    from cereyan import LocalTarget, Snooze, WaitTimeout, emit_event, flow, sleep, task, wait_for_event, wait_for_target

    calls = []
    path = tmp_path / "t.txt"

    @task
    def make():
        path.write_text("x")

    @flow
    def f():
        t0 = time.time()
        sleep(0.2)
        assert time.time() - t0 >= 0.2
        emit_event("orders.ready", {"day": "d"})
        ev = wait_for_event("orders.*", match={"day": "d"}, within=5)
        assert ev["payload"]["day"] == "d"
        with pytest.raises(WaitTimeout):
            wait_for_event("never.happens", within=0.3)
        make()
        assert wait_for_target(LocalTarget(path), poke=1, timeout=2).exists()
        calls.append(1)
        if len(calls) == 1:
            raise Snooze(0.2)
        return "done"

    assert f() == "done" and calls == [1, 1]
    from cereyan import states

    assert states.Sleeping.state_type == "Paused" and states.AwaitingEvent.state_type == "Paused"


def test_the_runs_own_event_answers_its_wait(srv):
    run = srv.client.run("self_announce", day="2026-09-21")
    done = srv.wait_run(run["id"], timeout=30)
    assert done["state"]["type"] == "Completed", done["state"]
    assert lines(srv)[-1] == "self:2026-09-21"


def test_an_event_recorded_just_before_the_pause_answers_the_wait(srv):
    run = srv.client.run("early_listener", day="2026-09-22", within=20)
    done = srv.wait_run(run["id"], timeout=40)
    assert done["state"]["type"] == "Completed", done["state"]
    assert lines(srv)[-1] == "early:2026-09-22"


def test_an_older_event_from_another_run_is_not_taken(srv):
    c = srv.client
    before = c.run("announce", day="2026-09-24")
    assert srv.wait_run(before["id"])["state"]["type"] == "Completed"
    run = c.run("listener", day="2026-09-24", within=2)
    done = srv.wait_run(run["id"], timeout=30)
    # Recorded before this run's pass started: the wait times out instead.
    assert done["state"]["type"] == "Completed" and lines(srv)[-1] == "timeout"


def test_one_event_answers_one_wait(srv):
    c = srv.client
    run = c.run("waits_twice", day="2026-09-23", within=4)
    srv.wait_run(run["id"], timeout=15, until=lambda r: r["state"]["name"] == "AwaitingEvent")
    sent = c.run("announce", day="2026-09-23")
    srv.wait_run(sent["id"])
    done = srv.wait_run(run["id"], timeout=30)
    assert done["state"]["type"] == "Completed", done["state"]
    first = [ln for ln in lines(srv) if ln.startswith("first:")]
    assert len(set(first)) == 1
    assert lines(srv)[-1] == "second:timeout"
