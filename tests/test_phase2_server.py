"""Schedules, backfills, resources, priority, crash chains, timeouts,
dependencies, and the run graph through a real server."""

import json
import os
import signal
import subprocess
import sys
import time
from datetime import datetime, timedelta, timezone

import pytest

from cereyan.client import ApiError
from server_helpers import ServerProcess, free_port

SCHED_PIPELINE = '''
import os, time
from datetime import date, datetime
from cereyan import App, flow, task, Cron, Interval, get_run_logger, LocalTarget

app = App("sched")

@task
def step():
    return 1

@app.flow(schedule=Interval(3600))
def hourly():
    step()

@app.flow(schedules=[Cron("0 9 * * 1-5", timezone="UTC", key="weekdays"), Interval(1800, key="half")])
def two_schedules():
    pass

@app.flow
def daily(day: date):
    get_run_logger().info("day %s", day)
    return str(day)

@app.flow
def bulk(day: date):
    return str(day)

@app.flow
def slow_day(day: date, seconds: float = 0.5):
    time.sleep(seconds)
    return str(day)

def _bulk_complete(values):
    return {v for v in values if v < "2026-01-04"}

bulk.bulk_complete = _bulk_complete

@app.flow(max_concurrent=1)
def capped(seconds: float = 1.0):
    time.sleep(seconds)

@app.flow(max_concurrent=1, on_overlap="skip")
def skipper(seconds: float = 1.0):
    time.sleep(seconds)

@app.flow(max_concurrent=1, on_overlap="cancel_new")
def canceller(seconds: float = 1.0):
    time.sleep(seconds)

@app.flow(resources={"gpu": 1})
def gpu(seconds: float = 1.0):
    time.sleep(seconds)

@app.flow(resources={"shared_db": 1})
def shared_waiter():
    return "ok"

@app.flow(priority=10)
def urgent():
    return os.getpid()

@app.flow
def normal(seconds: float = 0.0):
    time.sleep(seconds)
    return os.getpid()

@app.flow(timeout_seconds=1)
def slow_flow():
    time.sleep(30)

@app.flow(crash_retries=2)
def crasher():
    os._exit(7)

@app.flow(crash_retries=0)
def crash_once():
    os._exit(7)

@app.flow
def daily_sales(day: date = date(2026, 9, 6)):
    return "sales"

@app.flow(after="daily_sales")
def build_report(day: date):
    get_run_logger().info("report for %s", day)
    return "report"

@app.flow(after=("daily_sales", {"note": "{{ run.parameters.day }}"}))
def renamed(note: str = ""):
    return note

@app.flow(after="daily_sale")
def typo():
    pass

@app.flow(disable_after=(2, 3600, 2), schedule=Interval(3600, key="dis"))
def fragile():
    raise RuntimeError("fail")

@task
def a():
    return 1

@task
def b(x):
    return x

@app.flow
def graphed():
    fa = a.submit()
    fb = b.submit(fa)
    return fb.result()

@app.flow(on_crashed=[lambda f, r, s: open(os.environ["CRASH_HOOK_FILE"], "a").write(f"{r['id']}:{s['type']}\\n")])
def hooked_crash():
    os._exit(7)
'''


@pytest.fixture
def sched_dir(tmp_path):
    d = tmp_path / "sched"
    d.mkdir()
    (d / "pipeline.py").write_text(SCHED_PIPELINE)
    return d


@pytest.fixture
def sched(isolated_home, sched_dir, tmp_path):
    from cereyan import engine

    engine.close_store()
    hook_file = tmp_path / "hooks.txt"
    srv = ServerProcess(str(isolated_home), str(sched_dir), env={"CEREYAN_FAST_CRASH_RERUN": "1", "CRASH_HOOK_FILE": str(hook_file)})
    srv.hook_file = hook_file
    try:
        yield srv
    finally:
        srv.stop()


def fid(server, name):
    return next(f["id"] for f in server.client.flows() if f["name"] == name and f["project"] == "sched")


def start(server, name, **params):
    return server.client._request("POST", f"/api/flows/{fid(server, name)}/runs", body={"parameters": params})


def wait_until(fn, timeout=20, interval=0.05):
    deadline = time.time() + timeout
    while time.time() < deadline:
        v = fn()
        if v:
            return v
        time.sleep(interval)
    raise AssertionError("condition not met in time")


# ---------------------------------------------------------------------------
# schedules


def test_code_schedules_registered_and_upcoming(sched):
    c = sched.client
    flows = {f["name"]: f for f in c.flows("sched")}
    assert len(flows["hourly"]["schedules"]) == 1
    s = flows["hourly"]["schedules"][0]
    assert s["schedule"]["kind"] == "interval" and s["source"] == "code" and s["active"]
    assert s["next_fire"]
    upcoming = c.upcoming(flows["hourly"]["id"])
    assert len(upcoming) == 3
    times = [u["scheduled_time"] for u in upcoming]
    assert times == sorted(times) and all(u["state"]["type"] == "Scheduled" for u in upcoming)
    assert times[1] - times[0] == 3600 * 1_000_000
    assert upcoming[0]["created_by"] == "schedule"
    two = flows["two_schedules"]["schedules"]
    assert {s["code_key"] for s in two} == {"weekdays", "half"}


def test_schedule_api_create_patch_pause_resume_delete(sched):
    c = sched.client
    flow_id = fid(sched, "daily")
    created = c._request("POST", f"/api/flows/{flow_id}/schedules", body={"kind": "cron", "cron": "*/5 * * * *", "timezone": "UTC", "catchup": "latest"})
    assert created["source"] == "ui" and created["catchup"] == "latest"
    sid = created["id"]
    upcoming = c.upcoming(flow_id)
    assert len(upcoming) >= 3
    first = upcoming[0]["scheduled_time"]
    patched = c._request("PATCH", f"/api/schedules/{sid}", body={"cron": "*/10 * * * *"})
    assert patched["schedule"]["cron"] == "*/10 * * * *"
    upcoming2 = c.upcoming(flow_id)
    assert all((u["scheduled_time"] // 1_000_000) % 600 == 0 for u in upcoming2)
    assert upcoming2[0]["scheduled_time"] >= first
    paused = c._request("POST", f"/api/schedules/{sid}/pause")
    assert paused["active"] is False
    assert c.upcoming(flow_id) == []
    resumed = c._request("POST", f"/api/schedules/{sid}/resume")
    assert resumed["active"] is True and len(c.upcoming(flow_id)) >= 3
    with pytest.raises(ApiError) as info:
        c._request("POST", f"/api/flows/{flow_id}/schedules", body={"kind": "cron", "cron": "not a cron"})
    assert info.value.status == 422
    with pytest.raises(ApiError) as info:
        c._request("PATCH", f"/api/schedules/{sid}", body={"timezone": "Mars/Olympus"})
    assert info.value.status == 422
    c._request("DELETE", f"/api/schedules/{sid}")
    assert c.upcoming(flow_id) == []
    assert c.schedules(flow_id) == []
    preview = c._request("POST", "/api/schedules/preview", body={"kind": "cron", "cron": "0 9 * * 1-5", "timezone": "UTC", "count": 3})
    assert len(preview["next"]) == 3 and preview["timezone"] == "UTC"


def test_schedule_fires_on_time_with_low_drift(sched):
    c = sched.client
    flow_id = fid(sched, "normal")
    now = datetime.now(timezone.utc)
    anchor = now + timedelta(seconds=3)
    created = c._request(
        "POST", f"/api/flows/{flow_id}/schedules",
        body={"kind": "interval", "interval": 600, "anchor": int(anchor.timestamp() * 1_000_000) - 600 * 1_000_000, "timezone": "UTC"},
    )
    upcoming = c.upcoming(flow_id)
    target = min(u["scheduled_time"] for u in upcoming)
    assert abs(target - int(anchor.timestamp() * 1_000_000)) < 1_000
    run_id = next(u["id"] for u in upcoming if u["scheduled_time"] == target)
    done = sched.wait_run(run_id, timeout=30)
    assert done["state"]["type"] == "Completed", done
    # start_time is the Running transition, proposed right after Pending.
    drift_us = done["start_time"] - target
    assert drift_us >= -50_000, drift_us
    assert drift_us < 250_000, f"drift {drift_us / 1000:.1f} ms"
    c._request("DELETE", f"/api/schedules/{created['id']}")


def test_catchup_policies_on_restart(isolated_home, sched_dir):
    from cereyan import engine

    engine.close_store()
    port = free_port()
    srv = ServerProcess(str(isolated_home), str(sched_dir), port=port)
    c = srv.client
    flow_id = fid(srv, "daily")
    now = int(time.time() * 1_000_000)
    for policy in ("skip", "latest", "all"):
        c._request("POST", f"/api/flows/{flow_id}/schedules", body={"kind": "interval", "interval": 60, "anchor": now, "timezone": "UTC", "catchup": policy, "catchup_max": 2})
    srv.stop()
    # Pretend the server slept for four fires.
    import sqlite3

    db = sqlite3.connect(str(isolated_home / "db.sqlite"))
    db.execute("UPDATE kv SET value = ? WHERE key = 'scheduler.last_wakeup'", (str(now - 250 * 1_000_000),))
    db.execute("UPDATE schedule SET spec = json_set(spec, '$.anchor', ?)", (now - 300 * 1_000_000,))
    db.commit()
    db.close()
    srv2 = ServerProcess(str(isolated_home), str(sched_dir), port=port)
    try:
        c = srv2.client
        catchup = [r for r in c.runs(flow="daily", limit=500)["items"] if r["created_by"] == "catchup"]
        # latest: 1 run, all with max 2: 2 runs, skip: none.
        assert len(catchup) == 3, [(r["name"], r["schedule_id"]) for r in catchup]
        events = c.events(kind="schedule.catchup")
        assert len(events) == 3
        dropped = {e["payload"]["policy"]: e["payload"]["dropped"] for e in events}
        assert dropped["skip"] >= 4 and dropped["latest"] >= 3 and dropped["all"] >= 2
        for r in catchup:
            srv2.wait_run(r["id"], timeout=30)
    finally:
        srv2.stop()


def test_late_marking_when_engines_busy(isolated_home, sched_dir):
    from cereyan import engine

    engine.close_store()
    srv = ServerProcess(str(isolated_home), str(sched_dir), extra=["--max-engines", "1"])
    try:
        c = srv.client
        busy = start(srv, "normal", seconds=40)
        srv.wait_run(busy["id"], until=lambda r: r["state"]["type"] == "Running")
        flow_id = fid(srv, "normal")
        anchor = int(time.time() * 1_000_000) + 1_000_000
        c._request("POST", f"/api/flows/{flow_id}/schedules", body={"kind": "interval", "interval": 600, "anchor": anchor - 600 * 1_000_000, "timezone": "UTC"})
        run_id = min(c.upcoming(flow_id), key=lambda u: u["scheduled_time"])["id"]
        late = srv.wait_run(run_id, timeout=30, until=lambda r: r["state"]["name"] == "Late")
        assert late["state"]["type"] == "Scheduled"
        assert c.events(kind="run.late")
        c.cancel(busy["id"])
        done = srv.wait_run(run_id, timeout=40)
        assert done["state"]["type"] == "Completed", done
    finally:
        srv.stop()


# ---------------------------------------------------------------------------
# resilience


def test_crash_chain_and_limit(sched):
    start(sched, "crasher")
    c = sched.client

    def chain_done():
        runs = [r for r in c.runs(flow="crasher", limit=50)["items"]]
        return runs if len(runs) == 3 and all(r["state"]["type"] in ("Failed",) or r["state"]["type"] == "Crashed" for r in runs) and any(r["state"]["type"] == "Failed" for r in runs) else None

    runs = wait_until(chain_done, timeout=60)
    runs.sort(key=lambda r: r["id"])
    assert [r["state"]["type"] for r in runs] == ["Crashed", "Crashed", "Failed"]
    assert runs[-1]["state"]["message"] == "crash limit reached"
    assert runs[1]["parent_run_id"] == runs[0]["id"] and runs[2]["parent_run_id"] == runs[1]["id"]
    assert runs[1]["created_by"] == f"crash:{runs[0]['id']}"
    assert [r["attempt"] for r in runs] == [0, 1, 2]

    once = start(sched, "crash_once")
    done = sched.wait_run(once["id"], timeout=30)
    assert done["state"]["type"] == "Failed" and done["state"]["message"] == "crash limit reached"
    assert len(c.runs(flow="crash_once")["items"]) == 1


def test_crash_hooks_run_in_fresh_engine(sched):
    run = start(sched, "hooked_crash")
    wait_until(lambda: sched.hook_file.exists() and sched.hook_file.read_text().strip(), timeout=40)
    assert f"{run['id']}:Crashed" in sched.hook_file.read_text()


def test_flow_timeout_kills_engine(sched):
    run = start(sched, "slow_flow")
    t0 = time.time()
    done = sched.wait_run(run["id"], timeout=30)
    assert done["state"]["name"] == "TimedOut" and done["state"]["type"] == "Failed"
    assert 0.8 < time.time() - t0 < 6
    again = start(sched, "normal")
    assert sched.wait_run(again["id"])["state"]["type"] == "Completed"


def test_crash_retries_precedence(isolated_home, sched_dir):
    from cereyan import engine

    engine.close_store()
    (sched_dir / "cereyan.toml").write_text("[defaults]\ncrash_retries = 1\n")
    srv = ServerProcess(str(isolated_home), str(sched_dir), extra=["--crash-retries", "3"])
    try:
        assert srv.client.settings()["crash_retries_default"] == 1
    finally:
        srv.stop()
    (sched_dir / "cereyan.toml").unlink()
    srv = ServerProcess(str(isolated_home), str(sched_dir), extra=["--crash-retries", "3"])
    try:
        assert srv.client.settings()["crash_retries_default"] == 3
    finally:
        srv.stop()


# ---------------------------------------------------------------------------
# backfill


def test_backfill_create_status_cancel(sched):
    c = sched.client
    flow_id = fid(sched, "slow_day")
    t0 = time.time()
    # End is inclusive: June 1 through August 30 is 91 days.
    status = c.backfill(flow_id, "day", "2026-06-01", "2026-08-30", interval="1d", concurrency=2, extra_parameters={"seconds": 0.3})
    assert time.time() - t0 < 1.5
    assert status["total"] == 91 and status["tag"] == f"backfill:{status['id']}"
    page = c.runs(backfill_id=status["id"], limit=500)["items"]
    assert len(page) == 91
    assert all(status["tag"] in r["tags"] for r in page)
    days = sorted(r["parameters"]["day"] for r in page)
    assert days[0] == "2026-06-01" and days[-1] == "2026-08-30"
    # Concurrency cap: never more than two Running.
    peak = 0
    for _ in range(40):
        running = [r for r in c.runs(backfill_id=status["id"], state_type="Running", limit=500)["items"]]
        peak = max(peak, len(running))
        time.sleep(0.05)
    assert peak <= 2
    cancelled = c.cancel_backfill(status["id"])
    assert cancelled["cancelled"] is True

    def all_terminal():
        counts = c.backfill_status(status["id"])["counts"]
        active = sum(v for k, v in counts.items() if k in ("Scheduled", "Pending", "Running", "Cancelling", "AwaitingResource", "Late"))
        return counts if active == 0 else None

    counts = wait_until(all_terminal, timeout=40)
    assert counts.get("Cancelled", 0) >= 1
    assert sum(counts.values()) == 91
    with pytest.raises(ApiError) as info:
        c.backfill(fid(sched, "normal"), "seconds", "2026-06-01", "2026-06-02")
    assert info.value.status == 422
    fast = c.backfill(fid(sched, "daily"), "day", "2026-06-01", "2026-06-10")
    assert fast["total"] == 10
    listed = c._request("GET", f"/api/flows/{flow_id}/backfills")
    assert listed and listed[0]["id"] == status["id"]


def test_backfill_bulk_complete_prefilter(sched):
    c = sched.client
    status = c.backfill(fid(sched, "bulk"), "day", "2026-01-01", "2026-01-06", concurrency=3)
    assert status["total"] == 6

    def settled():
        counts = c.backfill_status(status["id"])["counts"]
        return counts if counts.get("Skipped", 0) == 3 and counts.get("Completed", 0) == 3 else None

    counts = wait_until(settled, timeout=40)
    assert counts == {"Skipped": 3, "Completed": 3}
    skipped = [r for r in c.runs(backfill_id=status["id"], state_name="Skipped", limit=50)["items"]]
    assert sorted(r["parameters"]["day"] for r in skipped) == ["2026-01-01", "2026-01-02", "2026-01-03"]


def test_backfill_cli(sched):
    env = dict(os.environ, CEREYAN_HOME=sched.home)
    result = subprocess.run(
        [sys.executable, "-m", "cereyan", "backfill", "sched/daily", "--param", "day", "--start", "2026-03-01", "--end", "2026-03-03", "--json"],
        env=env, capture_output=True, text=True, timeout=60,
    )
    assert result.returncode == 0, result.stderr
    payload = json.loads(result.stdout)
    assert payload["total"] == 3


# ---------------------------------------------------------------------------
# resources, priority, overlap


def test_max_concurrent_enqueues_and_other_flows_unaffected(sched):
    c = sched.client
    first = start(sched, "capped", seconds=2)
    sched.wait_run(first["id"], until=lambda r: r["state"]["type"] == "Running")
    second = start(sched, "capped", seconds=0.2)
    waiting = sched.wait_run(second["id"], timeout=10, until=lambda r: r["state"]["name"] == "AwaitingResource")
    assert waiting["state"]["type"] == "Scheduled"
    other = start(sched, "normal")
    assert sched.wait_run(other["id"], timeout=10)["state"]["type"] == "Completed"
    assert c.get_run(second["id"])["state"]["name"] == "AwaitingResource"
    done = sched.wait_run(second["id"], timeout=15)
    assert done["state"]["type"] == "Completed"
    assert done["start_time"] >= c.get_run(first["id"])["end_time"]


def test_overlap_skip_and_cancel_new(sched):
    c = sched.client
    first = start(sched, "skipper", seconds=2)
    sched.wait_run(first["id"], until=lambda r: r["state"]["type"] == "Running")
    second = start(sched, "skipper", seconds=0.1)
    done = sched.wait_run(second["id"], timeout=10)
    assert done["state"]["name"] == "Skipped" and done["state"]["message"] == "previous run still active"
    assert c.events(kind="run.skipped")
    sched.wait_run(first["id"])
    third = start(sched, "skipper", seconds=0.1)
    assert sched.wait_run(third["id"])["state"]["name"] == "Completed"

    first = start(sched, "canceller", seconds=2)
    sched.wait_run(first["id"], until=lambda r: r["state"]["type"] == "Running")
    second = start(sched, "canceller", seconds=0.1)
    done = sched.wait_run(second["id"], timeout=10)
    assert done["state"]["type"] == "Cancelled" and done["state"]["message"] == "previous run still active"


def test_flow_resources_wait_and_release_on_crash(sched):
    c = sched.client
    c._request("PATCH", "/api/settings", body={"resources": {"gpu": 1}})
    holder = start(sched, "gpu", seconds=30)
    sched.wait_run(holder["id"], until=lambda r: r["state"]["type"] == "Running")
    waiter = start(sched, "gpu", seconds=0.1)
    w = sched.wait_run(waiter["id"], timeout=10, until=lambda r: r["state"]["name"] == "AwaitingResource")
    assert "gpu" in w["state"]["message"]
    os.kill(c.get_run(holder["id"])["engine_pid"], signal.SIGKILL)
    done = sched.wait_run(waiter["id"], timeout=30)
    assert done["state"]["type"] == "Completed"
    settings = c.settings()
    assert settings["resources"]["gpu"]["total"] == 1.0


def test_priority_orders_dispatch_without_preemption(isolated_home, sched_dir):
    from cereyan import engine

    engine.close_store()
    srv = ServerProcess(str(isolated_home), str(sched_dir), extra=["--max-engines", "1"])
    try:
        c = srv.client
        busy = start(srv, "normal", seconds=3)
        srv.wait_run(busy["id"], until=lambda r: r["state"]["type"] == "Running")
        low = start(srv, "normal")
        high = start(srv, "urgent")
        waiting = srv.wait_run(high["id"], timeout=10, until=lambda r: r["state"]["name"] == "AwaitingResource")
        assert waiting["state"]["message"] == "no engine slot"
        assert c.get_run(busy["id"])["state"]["type"] == "Running"
        high_done = srv.wait_run(high["id"], timeout=30)
        low_done = srv.wait_run(low["id"], timeout=30)
        assert high_done["start_time"] < low_done["start_time"]
        assert c.get_run(busy["id"])["state"]["type"] == "Completed"
    finally:
        srv.stop()


def test_disable_window_pauses_schedules_and_resumes(sched):
    c = sched.client
    flow_id = fid(sched, "fragile")
    assert c.schedules(flow_id)[0]["active"] is True
    for _ in range(2):
        r = start(sched, "fragile")
        sched.wait_run(r["id"], timeout=20)
    paused = wait_until(lambda: (s := c.schedules(flow_id)[0]) and not s["active"] and s, timeout=10)
    assert paused["paused_reason"] == "disabled"
    assert c.events(kind="flow.disabled")
    resumed = wait_until(lambda: (s := c.schedules(flow_id)[0]) and s["active"] and s, timeout=15)
    assert resumed["paused_reason"] is None


def test_settings_saturation_risk(isolated_home, sched_dir):
    from cereyan import engine

    engine.close_store()
    srv = ServerProcess(str(isolated_home), str(sched_dir), extra=["--max-engines", "3"])
    try:
        s = srv.client.settings()
        assert s["engine_saturation_risk"] is True
        assert set(s["saturation_flows"]) == {"capped", "skipper", "canceller"}
    finally:
        srv.stop()
    srv = ServerProcess(str(isolated_home), str(sched_dir), extra=["--max-engines", "8"])
    try:
        assert srv.client.settings()["engine_saturation_risk"] is False
    finally:
        srv.stop()


# ---------------------------------------------------------------------------
# dependencies and graph


def test_flow_dependencies(sched):
    c = sched.client
    flows = {f["name"]: f for f in c.flows("sched")}
    assert set(flows["daily_sales"]["triggers"]) == {"build_report", "renamed"}
    assert flows["build_report"]["triggered_by"] == "daily_sales"
    assert flows["typo"]["error"] == "unknown upstream flow 'daily_sale'"
    up = start(sched, "daily_sales", day="2026-09-06")
    sched.wait_run(up["id"])
    report = wait_until(lambda: (r := c.runs(flow="build_report")["items"]) and r[0], timeout=15)
    assert report["created_by"] == f"run:{up['id']}"
    assert report["parameters"]["day"] == "2026-09-06"
    renamed = wait_until(lambda: (r := c.runs(flow="renamed")["items"]) and r[0], timeout=15)
    assert renamed["parameters"]["note"] == "2026-09-06"
    assert sched.wait_run(report["id"])["state"]["type"] == "Completed"
    failing = start(sched, "fragile")
    sched.wait_run(failing["id"])
    time.sleep(0.5)
    assert len(c.runs(flow="build_report")["items"]) == 1


def test_run_graph_edges(sched):
    run = start(sched, "graphed")
    sched.wait_run(run["id"])
    graph = sched.client._request("GET", f"/api/runs/{run['id']}/graph")
    names = {n["id"]: n["dynamic_key"] for n in graph["nodes"]}
    assert set(names.values()) == {"a-0", "b-0"}
    assert len(graph["edges"]) == 1
    e = graph["edges"][0]
    assert names[e["from"]] == "a-0" and names[e["to"]] == "b-0"
    counts = sched.client.counts()
    assert counts["task_runs"].get("Completed", 0) >= 2


@pytest.mark.performance
def test_backfill_10k_runs_under_one_second(sched):
    """Task 4.5: 10k runs created in one transaction in under a second."""
    c = sched.client
    flow_id = fid(sched, "daily")
    t0 = time.perf_counter()
    status = c.backfill(flow_id, "day", "2000-01-01", "2027-05-18", interval="1d", concurrency=1)
    elapsed = time.perf_counter() - t0
    assert status["total"] == 10_000
    assert elapsed < 1.0, f"backfill creation took {elapsed:.2f}s"
    cancelled = c.cancel_backfill(status["id"])
    assert cancelled["cancelled"] is True


def test_resources_are_shared_across_projects(sched, tmp_path):
    """Project identity task 9.1: a resource name is global, not per project."""
    c = sched.client
    c._request("PATCH", "/api/settings", body={"resources": {"shared_db": 1}})
    other = tmp_path / "otherproj"
    other.mkdir()
    (other / "pipe.py").write_text("import time\nfrom cereyan import flow\n@flow(resources={'shared_db': 1})\ndef hold(seconds: float = 3.0):\n    time.sleep(seconds)\n")
    holder = c.submit(
        "otherproj", "hold", {"seconds": 3}, module="pipe", source_dir=str(other),
        parameter_schema={"type": "object", "properties": {"seconds": {"type": "number", "default": 3.0}}},
        options={"resources": {"shared_db": 1}},
    )
    sched.wait_run(holder["id"], until=lambda r: r["state"]["type"] == "Running")
    # A flow in project "sched" needing the same bare name waits.
    fl = fid(sched, "gpu")
    c._request("PATCH", "/api/settings", body={"resources": {"gpu": 5}})
    waiter = c.submit("sched", "shared_waiter", {}, module="pipeline", source_dir=str(sched.directory),
                      parameter_schema={"type": "object", "properties": {}}, options={"resources": {"shared_db": 1}})
    w = sched.wait_run(waiter["id"], timeout=10, until=lambda r: r["state"]["name"] in ("AwaitingResource", "Failed", "Completed"))
    assert w["state"]["name"] == "AwaitingResource" and "shared_db" in w["state"]["message"]
    done = sched.wait_run(waiter["id"], timeout=30)
    assert done["start_time"] >= c.get_run(holder["id"])["end_time"]
    assert fl
