"""Targets, caching, retries, timeouts, hooks, futures, runners: offline path."""

import json
import os
import time
from datetime import timedelta

import pytest

from cereyan import INPUTS, SOURCE, LocalTarget, ThreadRunner, exponential, flow, task
from cereyan.runners import UpstreamFailed


def last_run(store):
    run = json.loads(store.list_runs())["items"][0]
    return run, json.loads(store.task_runs(run["id"]))


def test_local_target_atomic_write(tmp_path):
    t = LocalTarget(tmp_path / "out" / "data.txt")
    assert not t.exists()
    with t.open("w") as fh:
        fh.write("hello")
        assert not t.exists()
        assert any(p.name.startswith("data.txt.tmp-") for p in (tmp_path / "out").iterdir())
    assert t.exists()
    assert [p.name for p in (tmp_path / "out").iterdir()] == ["data.txt"]
    assert open(t.path).read() == "hello"
    with pytest.raises(RuntimeError):
        with t.open("w") as fh:
            fh.write("partial")
            raise RuntimeError("boom")
    assert open(t.path).read() == "hello"
    assert [p.name for p in (tmp_path / "out").iterdir()] == ["data.txt"]
    with t.temporary_path() as tmp:
        open(tmp, "w").write("v2")
    assert open(t.path).read() == "v2"
    t.remove()
    assert not t.exists()


def test_output_target_skips_task(store, tmp_path):
    calls = []
    out = LocalTarget(tmp_path / "2026-09-06.parquet")

    @task(output=lambda day: LocalTarget(tmp_path / f"{day}.parquet"))
    def build(day: str):
        calls.append(day)
        with LocalTarget(tmp_path / f"{day}.parquet").open("w") as fh:
            fh.write("x")
        return 1

    @task
    def consume(x):
        return "ok"

    @flow
    def f(day: str = "2026-09-06"):
        return consume(build(day))

    assert f() == "ok"
    assert calls == ["2026-09-06"] and out.exists()
    assert f() == "ok"
    assert calls == ["2026-09-06"]
    run, tasks = last_run(store)
    assert run["state"]["type"] == "Completed"
    assert tasks[0]["state"]["name"] == "Skipped" and tasks[0]["state"]["type"] == "Completed"
    assert tasks[1]["state"]["type"] == "Completed"


def test_cache_inputs_source_and_expiry(store, monkeypatch):
    calls = []

    @task(cache=INPUTS, persist_result=True)
    def compute(n: int):
        calls.append(n)
        return n * 2

    @flow
    def f(n: int = 3):
        return compute(n)

    assert f() == 6 and f() == 6
    assert calls == [3]
    _, tasks = last_run(store)
    assert tasks[0]["state"]["name"] == "Cached"
    assert f(n=4) == 8 and calls == [3, 4]

    # SOURCE policy: a different function body is a miss.
    src_calls = []

    @task(name="src", cache=INPUTS + SOURCE, persist_result=True)
    def src_a(n: int):
        src_calls.append("a")
        return n

    @flow
    def g(n: int = 1):
        return src_a(n)

    g()
    g()
    assert src_calls == ["a"]

    @task(name="src", cache=INPUTS + SOURCE, persist_result=True)
    def src_b(n: int):
        src_calls.append("b")
        return n + 100

    @flow(name="g2")
    def g2(n: int = 1):
        return src_b(n)

    assert g2() == 101 and src_calls == ["a", "b"]

    # Expiry.
    exp_calls = []

    @task(cache=INPUTS, persist_result=True, cache_expires=timedelta(hours=1))
    def expiring(n: int):
        exp_calls.append(n)
        return n

    @flow
    def h(n: int = 1):
        return expiring(n)

    h()
    real_time = time.time
    monkeypatch.setattr(time, "time", lambda: real_time() + 7200)
    h()
    assert exp_calls == [1, 1]


def test_json_serializer_and_python_version_tag(store):
    from cereyan import engine
    from cereyan.results import ResultStore

    @task(persist_result=True, serializer="json")
    def data():
        return {"a": 1}

    @flow
    def f():
        return data()

    f()
    storage = os.path.join(engine.resolved_home(), "storage")
    files = os.listdir(storage)
    assert len(files) == 1
    with open(os.path.join(storage, files[0]), "rb") as fh:
        header = json.loads(fh.readline())
        body = json.loads(fh.read())
    assert header["serializer"] == "json" and body == {"a": 1}
    assert header["python"]
    # A version mismatch is a miss.
    rs = ResultStore(engine.resolved_home())
    rs.write("k", 1)
    path = rs.path("k")
    raw = open(path, "rb").read().split(b"\n", 1)
    h = json.loads(raw[0])
    h["python"] = "2.7"
    open(path, "wb").write(json.dumps(h).encode() + b"\n" + raw[1])
    assert rs.read("k") == (False, None)


def test_cache_requires_persist_result():
    with pytest.raises(ValueError):
        @task(cache=INPUTS)
        def t():
            pass


def test_task_retries_then_success_and_exhausted(store):
    attempts = []

    @task(retries=2, retry_delay=0)
    def flaky():
        attempts.append(1)
        if len(attempts) < 3:
            raise ValueError("nope")
        return "ok"

    @flow
    def f():
        return flaky()

    assert f() == "ok"
    run, tasks = last_run(store)
    assert tasks[0]["state"]["type"] == "Completed"
    assert tasks[0]["failure_count"] == 2
    assert run["state"]["type"] == "Completed"

    attempts.clear()

    @task(retries=1, retry_delay=[0, 0])
    def always():
        attempts.append(1)
        raise ValueError("still")

    @flow
    def g():
        always()

    with pytest.raises(ValueError):
        g()
    _, tasks = last_run(store)
    assert tasks[0]["state"]["type"] == "Failed" and tasks[0]["failure_count"] == 2
    assert len(attempts) == 2


def test_flow_retries_and_exponential_delay(store):
    tries = []

    @flow(retries=1, retry_delay=exponential(base=0.01, jitter=0.0))
    def f():
        tries.append(1)
        if len(tries) == 1:
            raise RuntimeError("first")
        return "done"

    assert f() == "done"
    run, _ = last_run(store)
    assert run["state"]["type"] == "Completed" and run["failure_count"] == 1
    assert exponential(base=1, jitter=0).delay(3) == 8


def test_task_timeout_thread_and_process(store):
    @task(timeout_seconds=0.3)
    def slow():
        time.sleep(5)

    @flow
    def f():
        slow()

    t0 = time.time()
    with pytest.raises(TimeoutError):
        f()
    assert time.time() - t0 < 3
    _, tasks = last_run(store)
    assert tasks[0]["state"]["name"] == "TimedOut" and tasks[0]["state"]["type"] == "Failed"


def test_flow_timeout_offline(store):
    @flow(timeout_seconds=0.3)
    def f():
        time.sleep(5)

    t0 = time.time()
    with pytest.raises(TimeoutError):
        f()
    assert time.time() - t0 < 3
    run, _ = last_run(store)
    assert run["state"]["name"] == "TimedOut"


def test_hooks_run_after_state_and_errors_are_isolated(store):
    seen = []

    def on_done(owner, run, state):
        seen.append(("done", owner.name, state["type"]))

    def on_fail(owner, run, state):
        seen.append(("fail", owner.name, state["type"], run["name"]))
        raise RuntimeError("hook broke")

    @task(on_completion=[on_done])
    def t():
        return 1

    @flow(on_failure=[on_fail], on_completion=[on_done])
    def f():
        t()
        raise ValueError("bad")

    with pytest.raises(ValueError):
        f()
    run, _ = last_run(store)
    assert run["state"]["type"] == "Failed"
    assert seen[0] == ("done", "t", "Completed")
    assert seen[1][:3] == ("fail", "f", "Failed") and seen[1][3] == run["name"]


def test_submit_map_wait_for_and_graph_parents(store):
    order = []

    @task
    def extract():
        order.append("extract")
        return [1, 2, 3]

    @task
    def load(rows):
        order.append("load")
        return sum(rows)

    @task
    def double(x):
        return x * 2

    @task
    def notify(wait_for=None):
        order.append("notify")
        return "sent"

    @flow
    def f():
        e = extract.submit()
        b = load.submit(e)
        doubled = double.map([1, 2, 3])
        n = notify.submit(wait_for=[e, b])
        assert b.result() == 6
        assert [d.result() for d in doubled] == [2, 4, 6]
        return n.result()

    assert f() == "sent"
    assert order[:2] == ["extract", "load"]
    run, tasks = last_run(store)
    by_key = {t["dynamic_key"]: t for t in tasks}
    assert sorted(k for k in by_key if k.startswith("double")) == ["double-0", "double-1", "double-2"]
    assert by_key["load-0"]["parents"] == [by_key["extract-0"]["external_id"]]
    assert set(by_key["notify-0"]["parents"]) == {by_key["extract-0"]["external_id"], by_key["load-0"]["external_id"]}
    assert all(t["state"]["type"] == "Completed" for t in tasks)


def test_upstream_failure_propagates(store):
    @task
    def bad():
        raise ValueError("upstream")

    @task
    def downstream(x):
        return x

    @flow
    def f():
        d = downstream.submit(bad.submit())
        return d.result()

    with pytest.raises(UpstreamFailed):
        f()
    _, tasks = last_run(store)
    by_key = {t["dynamic_key"]: t for t in tasks}
    assert by_key["bad-0"]["state"]["type"] == "Failed"
    assert by_key["downstream-0"]["state"]["type"] == "Failed"
    assert by_key["downstream-0"]["state"]["message"] == "upstream task failed"


def test_process_runner_parallel_pids(store):
    from server_helpers import PIDS_MODULE  # noqa: F401  (module-level task for pickling)
    import importlib

    mod = importlib.import_module("pid_tasks")
    result = mod.parallel_flow()
    assert len(set(result)) == 4
    assert os.getpid() not in result


def test_generator_task_and_nested_deadlock_warning(store, caplog):
    @task
    def fetch(i):
        return i * 10

    @task
    def combine():
        results = yield [fetch.submit(1), fetch.submit(2)]
        one = yield fetch(3)
        return sum(results) + one

    @flow
    def f():
        return combine()

    assert f() == 60
    _, tasks = last_run(store)
    assert len(tasks) == 4

    @task
    def child():
        return 1

    @task
    def parent():
        return child.submit().result()

    @flow(runner=ThreadRunner(max_workers=1))
    def g():
        return parent.submit().result()

    import logging

    with caplog.at_level(logging.WARNING, logger="cereyan.run"):
        assert g() == 1
    assert any("deadlock risk" in r.message for r in caplog.records)


def test_forgotten_wait_finishes_before_run_ends(store):
    done = []

    @task
    def slow():
        time.sleep(0.3)
        done.append(1)

    @flow
    def f():
        slow.submit()
        return "returned early"

    assert f() == "returned early"
    assert done == [1]
    run, tasks = last_run(store)
    assert run["state"]["type"] == "Completed" and tasks[0]["state"]["type"] == "Completed"
    assert run["end_time"] >= tasks[0]["end_time"]


def test_offline_task_resources_serialize(store):
    active = []
    peak = []

    @task(resources={"db": 1})
    def use_db(i):
        active.append(i)
        peak.append(len(active))
        time.sleep(0.05)
        active.remove(i)
        return i

    @flow
    def f():
        return [fut.result() for fut in use_db.map([1, 2, 3])]

    assert f() == [1, 2, 3]
    assert max(peak) == 1


def test_schedule_declarations_validate():
    from cereyan import Cron, Interval, RRule
    from cereyan.schedules import normalize

    decls = normalize([Cron("0 9 * * *", timezone="Europe/Istanbul"), Interval(timedelta(hours=1)), RRule("DTSTART:20260101T000000Z\nRRULE:FREQ=DAILY")])
    assert [d["kind"] for d in decls] == ["cron", "interval", "rrule"]
    assert decls[1]["interval"] == 3600.0
    with pytest.raises(ValueError):
        normalize(Interval(0))
    with pytest.raises(ValueError):
        normalize(Cron("* * * * *", catchup="sometimes"))

    @flow(schedule=Cron("0 9 * * *"), max_concurrent=1, on_overlap="skip", priority=5, after="upstream", resources={"gpu": 1})
    def f():
        pass

    o = f.options
    assert o["schedules"][0]["cron"] == "0 9 * * *" and o["max_concurrent"] == 1 and o["after"]["flow"] == "upstream"
    with pytest.raises(ValueError):
        flow(after=["a", "b"])(lambda: None)
