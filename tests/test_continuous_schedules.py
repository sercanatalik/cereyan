"""Continuous schedules: one run at a time, the next joining the line after the delay."""

from __future__ import annotations

import time

import pytest

from cereyan.client import ApiError
from server_helpers import ServerProcess

PIPELINE = '''
import os
import time
from cereyan import App, Continuous

app = App("loop")
HERE = os.path.dirname(os.path.abspath(__file__))

@app.flow(schedule=Continuous(delay=1.5))
def quick():
    time.sleep(0.2)

@app.flow(schedule=Continuous(delay="10m"))
def slow_wait():
    pass

@app.flow(schedule=Continuous(delay=0.2), disable_after=(2, None, 3600))
def flaky():
    if os.path.exists(os.path.join(HERE, "fail")):
        raise RuntimeError("told to fail")

@app.flow(schedule=Continuous(delay=0))
def gated():
    # Holds its run until the gate file appears.
    while not os.path.exists(os.path.join(HERE, "gate")):
        time.sleep(0.05)

@app.flow
def plain():
    pass
'''


@pytest.fixture
def loop_dir(tmp_path):
    d = tmp_path / "loop"
    d.mkdir()
    (d / "pipeline.py").write_text(PIPELINE)
    return d


def start(isolated_home, loop_dir) -> ServerProcess:
    from cereyan import engine

    engine.close_store()
    return ServerProcess(str(isolated_home), str(loop_dir))


@pytest.fixture
def srv(isolated_home, loop_dir):
    server = start(isolated_home, loop_dir)
    try:
        yield server
    finally:
        server.stop()


def flow_id(srv, name: str) -> int:
    return next(f["id"] for f in srv.client.flows() if f["name"] == name and f["project"] == "loop")


def schedule(srv, name: str) -> dict:
    return srv.client._request("GET", f"/api/flows/{flow_id(srv, name)}/schedules")[0]


def runs(srv, name: str) -> list[dict]:
    items = srv.client._request("GET", f"/api/runs?flow={name}&limit=100")["items"]
    return sorted(items, key=lambda r: r["id"])


def unfinished(srv, name: str) -> list[dict]:
    return [r for r in runs(srv, name) if r["state"]["type"] not in ("Completed", "Failed", "Cancelled", "Crashed")]


def wait_for(check, timeout: float = 20.0, what: str = "condition"):
    deadline = time.time() + timeout
    while time.time() < deadline:
        value = check()
        if value:
            return value
        time.sleep(0.1)
    raise AssertionError(f"timed out waiting for {what}")


def test_the_next_run_waits_for_the_delay_after_the_end(srv):
    row = schedule(srv, "quick")
    assert row["schedule"] == {"kind": "continuous", "delay": 1.5}
    first = wait_for(lambda: [r for r in runs(srv, "quick") if r["state"]["type"] == "Completed"], what="a completed run")[0]
    successor = wait_for(lambda: [r for r in runs(srv, "quick") if r["id"] > first["id"]], what="the next run")[0]
    # Due one delay after the first ended, and never alongside it.
    assert successor["scheduled_time"] - first["end_time"] >= 1_400_000
    assert successor["created_by"] == "continuous"
    # Keep watching: at no point is more than one run unfinished.
    deadline = time.time() + 5
    while time.time() < deadline:
        assert len(unfinished(srv, "quick")) <= 1
        time.sleep(0.05)
    assert len([r for r in runs(srv, "quick") if r["state"]["type"] == "Completed"]) >= 2


def test_a_waiting_loop_reports_its_state_and_can_join_now(srv):
    first = wait_for(lambda: [r for r in runs(srv, "slow_wait") if r["state"]["type"] == "Completed"], what="first run")[0]
    waiting = wait_for(lambda: unfinished(srv, "slow_wait"), what="the waiting run")[0]
    row = wait_for(lambda: (lambda r: r if r["loop_state"] == "waiting" else None)(schedule(srv, "slow_wait")), what="waiting")
    assert row["next_fire"] >= first["end_time"] + 600_000_000 - 1_000_000
    assert waiting["state"]["name"] == "Scheduled"
    srv.client._request("POST", f"/api/schedules/{row['id']}/now")
    assert srv.wait_run(waiting["id"])["state"]["type"] == "Completed"
    # And the one after it waits the full delay again.
    after = wait_for(lambda: [r for r in unfinished(srv, "slow_wait") if r["id"] > waiting["id"]], what="the next run")[0]
    assert after["scheduled_time"] > time.time() * 1_000_000 + 500_000_000


def test_pause_removes_the_waiting_run_and_resume_seeds_one(srv):
    wait_for(lambda: [r for r in runs(srv, "slow_wait") if r["state"]["type"] == "Completed"], what="first run")
    waiting = wait_for(lambda: unfinished(srv, "slow_wait"), what="the waiting run")[0]
    sid = schedule(srv, "slow_wait")["id"]
    srv.client._request("POST", f"/api/schedules/{sid}/pause")
    assert unfinished(srv, "slow_wait") == []
    assert schedule(srv, "slow_wait")["loop_state"] == "paused"
    assert all(r["id"] != waiting["id"] for r in runs(srv, "slow_wait"))
    srv.client._request("POST", f"/api/schedules/{sid}/resume")
    seeded = wait_for(lambda: unfinished(srv, "slow_wait"), what="a seeded run")
    assert len(seeded) == 1
    # The last run ended moments ago, so the seeded run waits out the delay.
    assert seeded[0]["scheduled_time"] > time.time() * 1_000_000 + 500_000_000


def test_skips_and_catch_up_do_not_apply(srv):
    sid = schedule(srv, "slow_wait")["id"]
    with pytest.raises(ApiError) as err:
        srv.client._request("POST", f"/api/schedules/{sid}/skips", body={"next": 1})
    assert err.value.status == 409
    assert "pause it instead" in err.value.body["error"]
    with pytest.raises(ApiError) as err:
        srv.client._request(
            "POST",
            f"/api/flows/{flow_id(srv, 'plain')}/schedules",
            body={"kind": "continuous", "delay": 60, "catchup": "all"},
        )
    assert err.value.status == 422


def test_a_loop_created_through_the_api_starts_now(srv):
    row = srv.client._request(
        "POST", f"/api/flows/{flow_id(srv, 'plain')}/schedules", body={"kind": "continuous", "delay": 3600}
    )
    assert row["schedule"]["kind"] == "continuous"
    first = wait_for(lambda: runs(srv, "plain"), what="the first run")[0]
    assert srv.wait_run(first["id"])["state"]["type"] == "Completed"
    nxt = wait_for(lambda: unfinished(srv, "plain"), what="the next run")[0]
    assert nxt["scheduled_time"] > time.time() * 1_000_000 + 3_000_000_000


def test_failures_in_a_row_stop_the_loop(srv, loop_dir):
    (loop_dir / "fail").write_text("")
    row = wait_for(
        lambda: (lambda r: r if not r["active"] else None)(schedule(srv, "flaky")), timeout=30, what="the loop to stop"
    )
    assert row["paused_reason"] == "disabled"
    assert unfinished(srv, "flaky") == []
    failed = [r for r in runs(srv, "flaky") if r["state"]["type"] == "Failed"]
    assert len(failed) == 2


def test_a_restart_keeps_the_waiting_run(isolated_home, loop_dir):
    srv = start(isolated_home, loop_dir)
    try:
        wait_for(lambda: [r for r in runs(srv, "slow_wait") if r["state"]["type"] == "Completed"], what="first run")
        waiting = wait_for(lambda: unfinished(srv, "slow_wait"), what="the waiting run")[0]
    finally:
        srv.stop()
    srv = start(isolated_home, loop_dir)
    try:
        time.sleep(1.0)
        left = unfinished(srv, "slow_wait")
        assert [r["id"] for r in left] == [waiting["id"]]
        assert left[0]["scheduled_time"] == waiting["scheduled_time"]
    finally:
        srv.stop()


def test_pausing_while_running_lets_the_run_finish_with_no_successor(srv, loop_dir):
    running = wait_for(
        lambda: [r for r in runs(srv, "gated") if r["state"]["type"] == "Running"], what="a running run"
    )[0]
    row = schedule(srv, "gated")
    assert row["loop_state"] == "running"
    srv.client._request("POST", f"/api/schedules/{row['id']}/pause")
    (loop_dir / "gate").write_text("")
    assert srv.wait_run(running["id"])["state"]["type"] == "Completed"
    time.sleep(1.0)
    assert unfinished(srv, "gated") == []
    assert [r["id"] for r in runs(srv, "gated")] == [running["id"]]
