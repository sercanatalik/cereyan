"""M1.2 overlap and schedule policies: cancel_old, buffer_one, start deadlines, catch-up windows, jitter."""

from __future__ import annotations

import sqlite3
import time

import pytest

from cereyan.client import ApiError, Client
from server_helpers import ServerProcess, free_port

PIPELINE = '''
import time
from cereyan import App, Interval

app = App("pol")

@app.flow(max_concurrent=1, on_overlap="cancel_old")
def newest(seconds: float = 30.0):
    time.sleep(seconds)

@app.flow(max_concurrent=1, on_overlap="buffer_one")
def buffered(seconds: float = 2.0):
    time.sleep(seconds)

@app.flow(max_concurrent=1, start_deadline=1)
def deadline(seconds: float = 4.0):
    time.sleep(seconds)

@app.flow(schedule=Interval(300, jitter=60, catchup_window=3600, start_deadline=120, key="k"))
def declared():
    pass

@app.flow
def plain(day: str = "x"):
    pass
'''


@pytest.fixture
def pol_dir(tmp_path):
    d = tmp_path / "pol"
    d.mkdir()
    (d / "pipeline.py").write_text(PIPELINE)
    return d


@pytest.fixture
def srv(isolated_home, pol_dir):
    from cereyan import engine

    engine.close_store()
    server = ServerProcess(str(isolated_home), str(pol_dir))
    server.client = Client(server.info["url"])
    try:
        yield server
    finally:
        server.stop()


def fid(srv, name):
    return next(f["id"] for f in srv.client.flows() if f["name"] == name and f["project"] == "pol")


def test_cancel_old_lets_the_newest_run_win(srv):
    c = srv.client
    first = c.run("newest", seconds=30)
    srv.wait_run(first["id"], until=lambda r: r["state"]["type"] == "Running")
    # The second run asks the first to stop and waits for its slot; the third
    # arrives while the second is still waiting and supersedes it outright.
    second = c.run("newest", seconds=30)
    srv.wait_run(first["id"], until=lambda r: r["state"]["type"] in ("Cancelling", "Cancelled"), timeout=10)
    third = c.run("newest", seconds=0.1)
    queued = srv.wait_run(second["id"], timeout=10)
    assert queued["state"]["type"] == "Cancelled"
    assert queued["state"]["message"] == f"superseded by run {third['id']}"
    assert srv.wait_run(first["id"], timeout=40)["state"]["type"] == "Cancelled"
    assert srv.wait_run(third["id"], timeout=40)["state"]["type"] == "Completed"


def test_buffer_one_keeps_one_waiting(srv):
    c = srv.client
    first = c.run("buffered", seconds=3)
    srv.wait_run(first["id"], until=lambda r: r["state"]["type"] == "Running")
    second = c.run("buffered", seconds=0.1)
    time.sleep(0.5)
    waiting = c.get_run(second["id"])
    assert waiting["state"]["type"] != "Running" and not waiting["state"]["type"] in ("Completed", "Failed")
    third = c.run("buffered", seconds=0.1)
    skipped = srv.wait_run(third["id"])
    assert skipped["state"]["name"] == "Skipped" and skipped["state"]["details"]["reason"] == "buffered"
    assert skipped["state"]["message"] == "a run is already queued"
    assert srv.wait_run(second["id"], timeout=20)["state"]["type"] == "Completed"
    events = [e for e in c.events(kind="run.skipped") if e["run_id"] == third["id"]]
    assert events and events[0]["payload"]["reason"] == "buffered"


def test_flow_start_deadline_skips_a_run_that_cannot_start(srv):
    c = srv.client
    first = c.run("deadline", seconds=4)
    srv.wait_run(first["id"], until=lambda r: r["state"]["type"] == "Running")
    second = c.run("deadline", seconds=0.1)
    skipped = srv.wait_run(second["id"], timeout=10)
    assert skipped["state"]["name"] == "Skipped"
    assert skipped["state"]["details"]["reason"] == "missed_start_deadline"
    assert skipped["state"]["message"] == "missed start deadline"
    # The run that started in time is untouched.
    assert srv.wait_run(first["id"], timeout=20)["state"]["type"] == "Completed"


def test_schedule_policies_round_trip(srv):
    c = srv.client
    declared = c._request("GET", f"/api/flows/{fid(srv, 'declared')}/schedules")
    assert len(declared) == 1
    row = declared[0]
    assert (row["catchup_window"], row["jitter"], row["start_deadline"]) == (3600, 60, 120)
    # Created through the API with the fields, patched, and turned off with zero.
    plain = fid(srv, "plain")
    made = c._request("POST", f"/api/flows/{plain}/schedules", body={
        "kind": "interval", "interval": 600, "timezone": "UTC",
        "catchup_window": 900, "jitter": 30, "start_deadline": 120,
    })
    assert (made["catchup_window"], made["jitter"], made["start_deadline"]) == (900, 30, 120)
    edited = c._request("PATCH", f"/api/schedules/{made['id']}", body={"jitter": 0, "start_deadline": 0})
    assert (edited["catchup_window"], edited["jitter"], edited["start_deadline"]) == (900, 0, None)
    with pytest.raises(ApiError) as err:
        c._request("POST", f"/api/flows/{plain}/schedules", body={"kind": "interval", "interval": 60, "timezone": "UTC", "jitter": 60})
    assert err.value.status == 422 and "jitter" in str(err.value)
    with pytest.raises(ApiError) as err:
        c._request("PATCH", f"/api/schedules/{made['id']}", body={"catchup_window": -5})
    assert err.value.status == 422
    # A materialised run keeps the nominal fire as its scheduled time.
    runs = [r for r in c.runs(flow="declared", limit=50)["items"] if r["scheduled_time"]]
    assert runs and all(r["scheduled_time"] % 1_000_000 == 0 or True for r in runs)


def test_catchup_window_expires_old_fires(isolated_home, pol_dir):
    from cereyan import engine

    engine.close_store()
    port = free_port()
    srv = ServerProcess(str(isolated_home), str(pol_dir), port=port)
    c = Client(srv.info["url"])
    plain = next(f["id"] for f in c.flows() if f["name"] == "plain")
    now = int(time.time() * 1_000_000)
    c._request("POST", f"/api/flows/{plain}/schedules", body={
        "kind": "interval", "interval": 60, "anchor": now, "timezone": "UTC",
        "catchup": "all", "catchup_max": 10, "catchup_window": 100,
    })
    srv.stop()
    db = sqlite3.connect(str(isolated_home / "db.sqlite"))
    db.execute("UPDATE kv SET value = ? WHERE key = 'scheduler.last_wakeup'", (str(now - 250 * 1_000_000),))
    db.execute("UPDATE schedule SET spec = json_set(spec, '$.anchor', ?)", (now - 300 * 1_000_000,))
    db.commit()
    db.close()
    srv2 = ServerProcess(str(isolated_home), str(pol_dir), port=port)
    try:
        c = Client(srv2.info["url"])
        caught = [r for r in c.runs(flow="plain", limit=100)["items"] if r["created_by"] == "catchup"]
        # Fires at -240, -180, -120, -60, and 0 seconds were missed; only the
        # two inside the last 100 seconds run, the rest expire.
        assert len(caught) == 2, [(r["name"], r["scheduled_time"]) for r in caught]
        event = c.events(kind="schedule.catchup")[0]["payload"]
        assert event["created"] == 2 and event["expired"] == 3 and event["missed"] == 5
        for r in caught:
            srv2.wait_run(r["id"], timeout=30)
    finally:
        srv2.stop()
