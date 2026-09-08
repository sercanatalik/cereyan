import json
import os

import pytest

from cereyan import CereyanError, StoreLocked, flow, task
from cereyan import _core, engine


def test_successful_script_records_completed_run(store):
    @task
    def a():
        return 1

    @task
    def b():
        return 2

    @flow
    def f():
        return a() + b()

    assert f() == 3
    runs = json.loads(store.list_runs())["items"]
    assert len(runs) == 1
    run = runs[0]
    assert run["state"]["type"] == "Completed"
    assert run["failure_count"] == 0
    assert run["start_time"] is not None and run["end_time"] is not None
    tasks = json.loads(store.task_runs(run["id"]))
    assert [t["name"] for t in tasks] == ["a", "b"]


def test_exception_in_flow_records_failed_and_propagates(store):
    @flow
    def f():
        raise ValueError("bad")

    with pytest.raises(ValueError, match="bad"):
        f()
    run = json.loads(store.list_runs())["items"][0]
    assert run["state"]["type"] == "Failed"
    assert "ValueError: bad" in run["state"]["message"]
    assert "Traceback" in run["state"]["details"]["traceback"]
    assert run["failure_count"] == 1
    assert run["crash_count"] == 0


def test_failed_task_is_recorded_before_flow_fails(store):
    @task
    def boom():
        raise RuntimeError("task broke")

    @flow
    def f():
        boom()

    with pytest.raises(RuntimeError):
        f()
    run = json.loads(store.list_runs())["items"][0]
    tasks = json.loads(store.task_runs(run["id"]))
    assert tasks[0]["state"]["type"] == "Failed"
    assert "task broke" in tasks[0]["state"]["message"]


def test_logs_are_stored_with_run(store):
    from cereyan import get_run_logger

    @task
    def t():
        get_run_logger().warning("inside task")

    @flow
    def f():
        get_run_logger().info("hello %s", "world")
        t()

    f()
    run = json.loads(store.list_runs())["items"][0]
    logs = json.loads(store.logs(run["id"]))
    messages = [l["message"] for l in logs]
    assert "hello world" in messages
    assert "inside task" in messages
    task_log = next(l for l in logs if l["message"] == "inside task")
    assert task_log["task_run_id"] is not None
    assert task_log["level"] == 30


def test_store_lock_conflict_fails_before_user_code(isolated_home):
    holder = _core.Store.open(str(isolated_home))
    calls = []

    @flow
    def f():
        calls.append(1)

    with pytest.raises(CereyanError) as info:
        f()
    assert str(os.getpid()) in str(info.value)
    assert "server" in str(info.value)
    assert calls == []
    del holder


def test_second_store_open_reports_holder_pid(store):
    with pytest.raises(StoreLocked) as info:
        _core.Store.open(str(engine.resolved_home()))
    assert str(os.getpid()) in str(info.value)


def test_home_created_on_first_run(isolated_home):
    assert not isolated_home.exists()

    @flow
    def f():
        pass

    f()
    assert (isolated_home / "db.sqlite").exists()
    assert (isolated_home / "db.lock").exists()
    names = {p.name for p in isolated_home.iterdir()}
    assert names <= {"db.sqlite", "db.sqlite-wal", "db.sqlite-shm", "db.lock"}


def test_transition_rules_shared_with_python(store):
    flow_id = store.upsert_flow("p", "f", "m", "/d")
    run_id, _ = store.create_run(flow_id, "r")
    with pytest.raises(_core.TransitionRejected, match="invalid-entry"):
        store.transition(run_id, "Running")
    store.transition(run_id, "Pending")
    store.transition(run_id, "Running")
    with pytest.raises(_core.TransitionRejected, match="duplicate"):
        store.transition(run_id, "Running")
    accepted = json.loads(store.transition(run_id, "Completed", "Skipped"))
    assert accepted["type"] == "Completed" and accepted["name"] == "Skipped"
    with pytest.raises(_core.TransitionRejected, match="terminal"):
        store.transition(run_id, "Running")
    forced = json.loads(store.transition(run_id, "Failed", force=True))
    assert forced["details"]["forced"] is True
