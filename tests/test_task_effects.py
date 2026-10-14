"""E3 task effects: an effect key per call that survives crash reruns, and
`once` tasks that report ahead, never repeat silently, and recover through a
probe or a person."""

from __future__ import annotations

import time

import pytest

from server_helpers import ServerProcess

PIPELINE = '''
import os
from cereyan import App, task, NOT_DONE, runtime

app = App("effects")
TRACE = os.environ["EFFECT_TRACE"]
MARK = os.environ["EFFECT_MARK"]

def note(line):
    with open(TRACE, "a") as fh:
        fh.write(line + "\\n")

def first_time(name):
    path = f"{MARK}.{name}"
    if os.path.exists(path):
        return False
    open(path, "w").close()
    return True

@task
def charge_plain(n: int):
    note(f"plain:{n}")
    return n

@app.flow(crash_retries=1)
def plain_complete_then_die():
    charge_plain(1)
    if first_time("plain"):
        os._exit(7)
    return "ok"

@task(once=True)
def charge_once(n: int):
    note(f"once:{n}:{runtime.task_run.effect_key}")
    return n

@app.flow(crash_retries=1)
def once_complete_then_die():
    charge_once(1)
    if first_time("once_done"):
        os._exit(7)
    return "ok"

@task(once=True)
def interrupted(n: int):
    note(f"start:{n}:{runtime.task_run.effect_key}")
    if first_time(f"interrupted{n}"):
        os._exit(7)
    note(f"done:{n}")
    return f"ch_{n}"

@app.flow(crash_retries=2)
def die_during_once(n: int = 1):
    out = interrupted(n)
    note(f"result:{out}")
    return out

def found(n: int):
    note(f"probe:{n}")
    return f"probed_{n}"

@task(once=True, probe=found)
def interrupted_probed(n: int):
    note(f"start:{n}")
    if first_time(f"probed{n}"):
        os._exit(7)
    return f"ch_{n}"

@app.flow(crash_retries=1)
def die_during_probed(n: int = 2):
    out = interrupted_probed(n)
    note(f"result:{out}")
    return out

def nothing(n: int):
    note(f"probe:{n}")
    return NOT_DONE

@task(once=True, probe=nothing)
def interrupted_unprobed(n: int):
    note(f"start:{n}")
    if first_time(f"unprobed{n}"):
        os._exit(7)
    return f"ch_{n}"

@app.flow(crash_retries=1)
def die_during_not_done(n: int = 3):
    out = interrupted_unprobed(n)
    note(f"result:{out}")
    return out

@app.flow(crash_retries=2)
def doubt_then_crash(n: int = 6):
    out = interrupted(n)
    if first_time(f"after{n}"):
        os._exit(7)
    note(f"result:{out}")
    return out

@task
def keyed_then_boom():
    note(f"key:{runtime.task_run.effect_key}")
    if first_time("keyed_boom"):
        raise RuntimeError("fails the first time")
    return 1

@app.flow
def key_retry():
    return keyed_then_boom()

@task
def keyed():
    note(f"key:{runtime.task_run.effect_key}")
    return runtime.task_run.effect_key

@task
def boom():
    if first_time("boom"):
        raise RuntimeError("fails the first time")
    return 1

@app.flow
def key_then_fail():
    keyed()
    keyed()
    boom()
    return "ok"
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
        "EFFECT_TRACE": str(trace), "EFFECT_MARK": str(tmp_path / "mark"), "CEREYAN_FAST_CRASH_RERUN": "1",
    })
    server.trace = trace
    try:
        yield server
    finally:
        server.stop()


def lines(srv, prefix=""):
    found = srv.trace.read_text().splitlines() if srv.trace.exists() else []
    return [line for line in found if line.startswith(prefix)]


def wait_until(fn, timeout=40):
    deadline = time.time() + timeout
    while time.time() < deadline:
        v = fn()
        if v:
            return v
        time.sleep(0.2)
    raise AssertionError("condition not met in time")


def rerun_of(c, flow, run_id):
    return wait_until(lambda: next((r for r in c.runs(flow=flow, limit=20)["items"]
                                    if r["created_by"] == f"crash:{run_id}"), None))


def test_a_plain_task_completed_just_before_a_crash_runs_again(srv):
    # Today's at-least-once: the completion was still in the report buffer.
    c = srv.client
    run = c.run("plain_complete_then_die")
    assert srv.wait_run(rerun_of(c, "plain_complete_then_die", run["id"])["id"])["state"]["type"] == "Completed"
    assert lines(srv, "plain:") == ["plain:1", "plain:1"]


def test_a_once_task_completed_just_before_a_crash_is_replayed(srv):
    c = srv.client
    run = c.run("once_complete_then_die")
    rerun = rerun_of(c, "once_complete_then_die", run["id"])
    assert srv.wait_run(rerun["id"])["state"]["type"] == "Completed"
    assert len(lines(srv, "once:")) == 1
    tasks = c.task_runs(run["id"])
    assert tasks[0]["once"] is True
    # The server reports the key the task saw.
    detail = c._request("GET", f"/api/task-runs/{tasks[0]['id']}")
    assert detail["effect_key"] == lines(srv, "once:")[0].split(":")[2]


def test_an_interrupted_once_task_pauses_in_doubt(srv):
    c = srv.client
    run = c.run("die_during_once", n=1)
    rerun = rerun_of(c, "die_during_once", run["id"])
    paused = srv.wait_run(rerun["id"], until=lambda r: r["state"]["type"] == "Paused")
    assert paused["state"]["name"] == "InDoubt"
    assert "interrupted-0" in paused["state"]["message"]
    assert len(lines(srv, "start:")) == 1


def task_named(c, run_id, key):
    return next(t for t in c.task_runs(run_id) if t["dynamic_key"] == key)


def in_doubt(srv, flow, **params):
    c = srv.client
    run = c.run(flow, **params)
    rerun = rerun_of(c, flow, run["id"])
    srv.wait_run(rerun["id"], until=lambda r: r["state"]["type"] == "Paused")
    return run, rerun


def test_answered_happened_recovers_without_running(srv):
    c = srv.client
    _, rerun = in_doubt(srv, "die_during_once", n=1)
    c.resume(rerun["id"], {"action": "happened", "result": "ch_1"})
    assert srv.wait_run(rerun["id"])["state"]["type"] == "Completed"
    assert lines(srv, "result:") == ["result:ch_1"]
    assert len(lines(srv, "start:1")) == 1
    recovered = [t for t in c.task_runs(rerun["id"]) if t["dynamic_key"] == "interrupted-0"][-1]
    assert recovered["state"]["name"] == "Recovered"
    assert any(e["name"] == "task_run.recovered" for e in c.events("task_run.recovered", limit=50))


def test_answered_run_again_executes_with_the_same_key(srv):
    c = srv.client
    _, rerun = in_doubt(srv, "die_during_once", n=4)
    c.resume(rerun["id"], {"action": "run_again"})
    assert srv.wait_run(rerun["id"])["state"]["type"] == "Completed"
    starts = lines(srv, "start:4")
    assert len(starts) == 2 and starts[0] == starts[1], starts
    assert lines(srv, "result:") == ["result:ch_4"]


def test_answered_fail_fails_the_task(srv):
    c = srv.client
    _, rerun = in_doubt(srv, "die_during_once", n=5)
    c.resume(rerun["id"], {"action": "fail"})
    done = srv.wait_run(rerun["id"])
    assert done["state"]["type"] == "Failed" and "TaskInDoubt" in done["state"]["message"]
    assert len(lines(srv, "start:5")) == 1


def test_a_settled_doubt_stays_settled_after_another_crash(srv):
    c = srv.client
    run, rerun = in_doubt(srv, "doubt_then_crash", n=6)
    c.resume(rerun["id"], {"action": "happened", "result": "ch_6"})
    third = rerun_of(c, "doubt_then_crash", rerun["id"])
    done = srv.wait_run(third["id"])
    assert done["state"]["type"] == "Completed", done["state"]
    assert lines(srv, "result:") == ["result:ch_6"]
    assert len(lines(srv, "start:6")) == 1


def test_a_probe_that_finds_the_effect_recovers_it(srv):
    c = srv.client
    run = c.run("die_during_probed", n=2)
    rerun = rerun_of(c, "die_during_probed", run["id"])
    assert srv.wait_run(rerun["id"])["state"]["type"] == "Completed"
    assert lines(srv, "probe:") == ["probe:2"]
    assert len(lines(srv, "start:2")) == 1
    assert lines(srv, "result:") == ["result:probed_2"]
    assert task_named(c, rerun["id"], "interrupted_probed-0")["state"]["name"] == "Recovered"


def test_a_probe_that_finds_nothing_executes(srv):
    c = srv.client
    run = c.run("die_during_not_done", n=3)
    rerun = rerun_of(c, "die_during_not_done", run["id"])
    assert srv.wait_run(rerun["id"])["state"]["type"] == "Completed"
    assert lines(srv, "probe:") == ["probe:3"]
    assert len(lines(srv, "start:3")) == 2
    assert lines(srv, "result:") == ["result:ch_3"]


def test_effect_keys_survive_a_retry_from_failure_but_not_from_the_start(srv):
    c = srv.client
    run = c.run("key_then_fail")
    assert srv.wait_run(run["id"])["state"]["type"] == "Failed"
    first, second = lines(srv, "key:")
    assert first != second  # two calls, two keys
    run = c.run("key_retry")
    assert srv.wait_run(run["id"])["state"]["type"] == "Failed"
    retry = c.retry(run["id"])
    assert srv.wait_run(retry["run"]["id"])["state"]["type"] == "Completed"
    again = c.retry(run["id"], from_="start")
    assert srv.wait_run(again["run"]["id"])["state"]["type"] == "Completed"
    original, from_failure, from_start = lines(srv, "key:")[2:]
    assert original == from_failure
    assert from_start != original


def test_once_options_are_checked_at_decoration():
    from cereyan import task

    with pytest.raises(ValueError, match="probe"):
        @task(once=True, retries=2)
        def charge():
            pass
    with pytest.raises(ValueError, match="once=True"):
        @task(probe=lambda: None)
        def look():
            pass


def test_the_catalogue_names_recovered_and_in_doubt():
    from cereyan import events, states

    assert states.Recovered == "Recovered" and states.Recovered.state_type == "Completed"
    assert states.InDoubt == "InDoubt" and states.InDoubt.state_type == "Paused"
    assert events.task_run.recovered == "task_run.recovered"


def test_offline_once_tasks_run_and_a_probe_settles_a_retry(store):
    from cereyan import NOT_DONE, flow, runtime, task

    calls, keys = [], []

    def charged():
        return "ch_9" if calls else NOT_DONE

    @task(once=True, retries=1, retry_delay=0, probe=charged)
    def charge():
        keys.append(runtime.task_run.effect_key)
        calls.append(1)
        raise TimeoutError("the response never came, but the charge went through")

    @flow
    def pay():
        return charge()

    assert pay() == "ch_9"
    assert len(calls) == 1 and len(keys[0]) == 32
    import json

    run = json.loads(store.list_runs())["items"][0]
    rows = json.loads(store.task_runs(run["id"]))
    assert rows[0]["state"]["name"] == "Recovered" and rows[0]["once"] is True
