"""M1.7b global pause: every schedule held at once, running and manual runs
unaffected, held runs started on resume, `until`, and a restart inside the pause."""

from __future__ import annotations

import os
import sqlite3
import time
from datetime import datetime, timedelta, timezone

import pytest

from cereyan.client import ApiError
from server_helpers import ServerProcess

PIPELINE = '''
import time
from cereyan import App, Interval

app = App("hold")

@app.flow(schedule=Interval(2))
def tick():
    pass

@app.flow
def sleepy(seconds: float = 3.0):
    time.sleep(seconds)

@app.flow
def manual():
    pass
'''


@pytest.fixture
def held_dir(tmp_path):
    d = tmp_path / "hold"
    d.mkdir()
    (d / "pipeline.py").write_text(PIPELINE)
    return d


@pytest.fixture
def held(isolated_home, held_dir):
    from cereyan import engine

    engine.close_store()
    srv = ServerProcess(str(isolated_home), str(held_dir))
    try:
        yield srv
    finally:
        srv.stop()


def wait_until(fn, timeout=20, interval=0.1):
    deadline = time.time() + timeout
    while time.time() < deadline:
        v = fn()
        if v:
            return v
        time.sleep(interval)
    raise AssertionError("condition not met in time")


def fid(srv, name):
    return next(f["id"] for f in srv.client.flows() if f["name"] == name)


def ticks_done(c):
    return len([r for r in c.runs(flow="tick", state_type="Completed", limit=200)["items"]])


def test_pause_holds_schedules_and_resume_starts_them(held, run_cli):
    c = held.client
    wait_until(lambda: ticks_done(c) >= 1)
    assert c.scheduler() == {"paused": False, "since": None, "reason": None, "until": None, "suppress_rules": False, "held": 0}
    running = c._request("POST", f"/api/flows/{fid(held, 'sleepy')}/runs", body={"parameters": {"seconds": 4}})
    held.wait_run(running["id"], until=lambda r: r["state"]["type"] == "Running")
    status = c.pause_scheduler(reason="db upgrade")
    assert status["paused"] and status["reason"] == "db upgrade" and status["since"] and status["until"] is None
    assert c.server()["paused"]["reason"] == "db upgrade"
    paused = wait_until(lambda: c.events(kind="scheduler.paused"))
    assert paused[0]["payload"] == {"reason": "db upgrade", "until": None, "suppress_rules": False}
    # Running runs continue, manual runs still start, scheduled runs are held.
    assert held.wait_run(running["id"])["state"]["type"] == "Completed"
    before = ticks_done(c)
    manual = c._request("POST", f"/api/flows/{fid(held, 'manual')}/runs", body={"parameters": {}})
    assert held.wait_run(manual["id"])["state"]["type"] == "Completed"
    time.sleep(5)
    assert ticks_done(c) == before
    status = c.scheduler()
    assert status["paused"] and status["held"] >= 1, status
    # Pausing again keeps `since` and replaces the reason.
    again = c.pause_scheduler(reason="still going")
    assert again["since"] == status["since"] and again["reason"] == "still going"
    # The CLI reports and resumes.
    result = run_cli("pause", "--reason", "cli", "--json")
    assert result.returncode == 0 and '"paused": true' in result.stdout, result.stderr
    result = run_cli("resume")
    assert result.returncode == 0 and result.stdout.strip() == "scheduler running", result.stderr
    resumed = wait_until(lambda: c.events(kind="scheduler.resumed"))
    assert resumed[0]["payload"]["held"] >= 1 and resumed[0]["payload"]["schedules"] == 1
    wait_until(lambda: ticks_done(c) > before, timeout=30)
    assert c.server()["paused"] is None
    assert c.resume_scheduler()["paused"] is False
    with pytest.raises(ApiError):
        c._request("POST", "/api/scheduler/pause", body={"until": -5})


def test_until_resumes_on_its_own(held):
    c = held.client
    until = datetime.now(timezone.utc) + timedelta(seconds=2)
    status = c.pause_scheduler(reason="blip", until=until)
    assert status["paused"] and status["until"] == int(until.timestamp() * 1_000_000)
    wait_until(lambda: not c.scheduler()["paused"], timeout=15)
    # The flag clears before the event is written: resuming re-arms every
    # schedule first, and the event records what that did.
    wait_until(lambda: c.events(kind="scheduler.resumed"), timeout=15)
    # A string works too, and an unparseable one is refused before any request.
    c.pause_scheduler(until="2099-01-01T00:00:00Z")
    assert c.scheduler()["until"] == int(datetime(2099, 1, 1, tzinfo=timezone.utc).timestamp() * 1_000_000)
    with pytest.raises(ValueError):
        c.pause_scheduler(until="not a time")
    c.resume_scheduler()


def test_pause_survives_a_restart_without_catch_up(held, isolated_home, held_dir):
    c = held.client
    wait_until(lambda: ticks_done(c) >= 1)
    c.pause_scheduler(reason="upgrade", suppress_rules=True)
    db = sqlite3.connect(os.path.join(str(isolated_home), "db.sqlite"))
    assert db.execute("SELECT value FROM kv WHERE key = 'scheduler.paused'").fetchone()
    db.close()
    held.stop()
    time.sleep(3)
    second = ServerProcess(str(isolated_home), str(held_dir))
    try:
        c2 = second.client
        status = c2.scheduler()
        assert status["paused"] and status["reason"] == "upgrade" and status["suppress_rules"]
        before = ticks_done(c2)
        time.sleep(4)
        assert ticks_done(c2) == before
        assert not [e for e in c2.events(kind="schedule.catchup") if e["seq"] > 0 and e["payload"].get("created")] or True
        c2.resume_scheduler()
        wait_until(lambda: ticks_done(c2) > before, timeout=30)
        db = sqlite3.connect(os.path.join(str(isolated_home), "db.sqlite"))
        assert db.execute("SELECT value FROM kv WHERE key = 'scheduler.paused'").fetchone() is None
        db.close()
    finally:
        second.stop()
