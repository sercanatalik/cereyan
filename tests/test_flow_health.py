"""M4.4 freshness and deadlines: PASS, WARN or FAIL per flow from recorded runs,
and one run.overdue event per run that outlasts its expectation."""

from __future__ import annotations

import time

import pytest

from server_helpers import ServerProcess

PIPELINE = '''
import time
from datetime import timedelta
from cereyan import App

app = App("health")

@app.flow(fresh_within=timedelta(seconds=3))
def load():
    return 1

@app.flow(expect_by="0 0 1 1 *", expect_by_tz="UTC")
def yearly():
    return 1

@app.flow(expected_duration=1)
def slow(seconds: float = 3.0):
    time.sleep(seconds)

@app.flow(overdue_factor=2.0)
def usually_quick(seconds: float = 0.1):
    time.sleep(seconds)

@app.flow
def plain():
    return 1
'''


@pytest.fixture
def srv(isolated_home, tmp_path):
    from cereyan import engine

    engine.close_store()
    d = tmp_path / "health"
    d.mkdir()
    (d / "pipeline.py").write_text(PIPELINE)
    server = ServerProcess(str(isolated_home), str(d))
    try:
        yield server
    finally:
        server.stop()


def health(c, name):
    return next(f for f in c.flows() if f["name"] == name)["health"]


def wait_until(fn, timeout=60):
    deadline = time.time() + timeout
    while time.time() < deadline:
        v = fn()
        if v:
            return v
        time.sleep(0.5)
    raise AssertionError("condition not met in time")


def test_freshness_and_deadline(srv):
    c = srv.client
    assert health(c, "plain") is None
    # Fresh: nothing completed yet and the flow is younger than the window.
    assert health(c, "load")["status"] == "PASS"
    run = c.run("load")
    srv.wait_run(run["id"])
    h = health(c, "load")
    assert h["status"] == "PASS" and h["last_completed_at"] and h["reasons"] == []
    time.sleep(2.5)
    h = health(c, "load")
    assert h["status"] in ("WARN", "FAIL")
    time.sleep(1.5)
    h = health(c, "load")
    assert h["status"] == "FAIL" and "fresh_within" in h["reasons"][0]
    # A deadline that passed with no completed run since the previous one.
    h = health(c, "yearly")
    assert h["status"] == "FAIL" and h["deadline"] and "deadline" in h["reasons"][0]
    run = c.run("yearly")
    srv.wait_run(run["id"])
    assert health(c, "yearly")["status"] == "PASS"


def test_overdue_runs_warn_and_record_one_event(srv):
    c = srv.client
    run = c.run("slow", seconds=12)
    srv.wait_run(run["id"], until=lambda r: r["state"]["type"] == "Running")
    events = wait_until(lambda: c.events(kind="run.overdue") or None, timeout=30)
    assert [e["run_id"] for e in events] == [run["id"]]
    assert events[0]["payload"]["basis"] == "expected_duration" and events[0]["payload"]["expected_seconds"] == 1
    h = health(c, "slow")
    assert h["status"] == "WARN" and "expected" in h["reasons"][0]
    srv.wait_run(run["id"])
    time.sleep(1)
    assert health(c, "slow")["status"] == "PASS"
    assert len(c.events(kind="run.overdue")) == 1
    # A factor against the median needs three completed runs first.
    for _ in range(3):
        srv.wait_run(c.run("usually_quick", seconds=0.1)["id"])
    assert health(c, "usually_quick")["status"] == "PASS"
    long = c.run("usually_quick", seconds=12)
    srv.wait_run(long["id"], until=lambda r: r["state"]["type"] == "Running")
    overdue = wait_until(lambda: [e for e in c.events(kind="run.overdue") if e["run_id"] == long["id"]] or None, timeout=30)
    assert overdue[0]["payload"]["basis"] == "median"
    srv.wait_run(long["id"])


def test_options_validate():
    from datetime import timedelta

    from cereyan import flow

    f = flow(name="healthy", fresh_within=timedelta(hours=26), expect_by="0 9 * * *", expected_duration=90, overdue_factor=2)(lambda: None)
    opts = f.options() if callable(f.options) else f.options
    assert opts["fresh_within"] == 26 * 3600 and opts["expected_duration"] == 90
    with pytest.raises(ValueError):
        flow(name="bad_window", fresh_within=0)(lambda: None)
    with pytest.raises(ValueError):
        flow(name="bad_cron", expect_by="not a cron")(lambda: None)
    with pytest.raises(ValueError):
        flow(name="bad_factor", overdue_factor=-1)(lambda: None)
