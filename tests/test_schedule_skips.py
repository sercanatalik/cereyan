"""Skipping upcoming fires of a schedule through a real server: the skip
endpoints, the look-ahead, durability, catch-up, retimes, and how a skip by a
person travels to the flows that run after it."""

import sqlite3
import time
from datetime import datetime, timedelta, timezone

import pytest

from cereyan.client import ApiError
from server_helpers import ServerProcess, free_port

HOUR = 3600 * 1_000_000

# Nothing here fires while a test runs, except what a test schedules itself.
CALM = '''
import time
from cereyan import App, Cron, Interval

app = App("skips")

@app.flow(schedule=Interval(3600, key="hourly"))
def hourly():
    return "hourly"

@app.flow(schedule=Cron("30 6 * * *", timezone="UTC", key="morning"))
def morning():
    return "morning"

@app.flow
def plain():
    return "plain"

@app.flow(max_concurrent=1, on_overlap="skip")
def overlap(seconds: float = 1.0):
    time.sleep(seconds)

@app.flow(after="overlap")
def after_overlap():
    return "after overlap"
'''

# A two-second ticker with a chain after it, and a keyed fan-in.
BUSY = '''
from datetime import date
from cereyan import App, Interval

app = App("skips")

@app.flow(schedule=Interval(2, key="tick"))
def ticker():
    return "tick"

@app.flow(after="ticker")
def after_ticker():
    return "after"

@app.flow(after="after_ticker")
def further():
    return "further"

@app.flow
def sales(day: date = date(2026, 9, 13)):
    return "sales"

@app.flow
def inventory(day: date):
    return "inventory"

@app.flow(after=["sales", "inventory"], batch_key="day")
def report(day: date):
    return "report"
'''


def write_app(tmp_path, name, source):
    d = tmp_path / name
    d.mkdir(exist_ok=True)
    (d / "pipeline.py").write_text(source)
    return d


def serve(home, directory, port=0):
    from cereyan import engine

    engine.close_store()
    return ServerProcess(str(home), str(directory), port=port)


@pytest.fixture
def calm(isolated_home, tmp_path):
    srv = serve(isolated_home, write_app(tmp_path, "calm", CALM))
    try:
        yield srv
    finally:
        srv.stop()


@pytest.fixture
def busy(isolated_home, tmp_path):
    srv = serve(isolated_home, write_app(tmp_path, "busy", BUSY))
    try:
        yield srv
    finally:
        srv.stop()


def flow_of(srv, name):
    return next(f for f in srv.client.flows() if f["name"] == name)


def schedule_of(srv, name):
    return flow_of(srv, name)["schedules"][0]


def upcoming(srv, name, projected=0):
    params = {"projected": projected} if projected else None
    return srv.client._request("GET", f"/api/flows/{flow_of(srv, name)['id']}/upcoming", params=params)


def skip(srv, sid, **body):
    return srv.client._request("POST", f"/api/schedules/{sid}/skips", body=body)


def unskip(srv, sid, fire):
    return srv.client._request("DELETE", f"/api/schedules/{sid}/skips/{fire}")


def skipped_times(items):
    return [i["scheduled_time"] for i in items if i["skipped"]]


def runs_of(srv, name):
    return srv.client.runs(flow=name, limit=500)["items"]


def run_at(srv, name, fire):
    return next((r for r in runs_of(srv, name) if r["scheduled_time"] == fire), None)


def ended(run):
    return run is not None and run["state"]["type"] in ("Completed", "Failed", "Cancelled", "Crashed")


def wait_until(fn, timeout=30, interval=0.1):
    deadline = time.time() + timeout
    while time.time() < deadline:
        value = fn()
        if value:
            return value
        time.sleep(interval)
    raise AssertionError("condition not met in time")


def expect_refusal(status, words, fn, *args, **kwargs):
    with pytest.raises(ApiError) as info:
        fn(*args, **kwargs)
    assert info.value.status == status, info.value.body
    assert words in str(info.value.body), info.value.body


# ---------------------------------------------------------------------------
# the skip endpoints and the look-ahead


def test_skip_one_in_the_middle_then_undo(calm):
    runs = upcoming(calm, "hourly")
    assert len(runs) == 3 and not skipped_times(runs) and not any(r["projected"] for r in runs)
    first, second, third = (r["scheduled_time"] for r in runs)
    sid = runs[0]["schedule_id"]

    out = skip(calm, sid, fires=[second], by="ui")
    assert out["skipped"] == [second] and out["schedule"]["skipped"] == 1
    assert out["downstream"] == []
    runs = upcoming(calm, "hourly")
    assert skipped_times(runs) == [second]
    marked = next(r for r in runs if r["skipped"])
    assert marked["state"]["type"] == "Scheduled" and marked["state"]["details"]["skip"] == "user"
    assert marked["skipped_by"] == "ui" and marked["skipped_at"] > 0
    assert all(r["skipped_by"] is None for r in runs if not r["skipped"])
    # The look-ahead keeps three runs that will start, past the skipped one.
    assert len([r for r in runs if not r["skipped"]]) == 3
    assert schedule_of(calm, "hourly")["next_fire"] == first

    skip(calm, sid, fires=[first])
    assert schedule_of(calm, "hourly")["next_fire"] == third

    undone = unskip(calm, sid, second)
    assert undone["skipped"] == 1
    assert skipped_times(upcoming(calm, "hourly")) == [first]
    unskip(calm, sid, first)
    assert schedule_of(calm, "hourly")["next_fire"] == first
    assert schedule_of(calm, "hourly")["skipped"] == 0


def test_skip_next_n_and_the_times_refused(calm):
    runs = upcoming(calm, "hourly")
    first, second, third = (r["scheduled_time"] for r in runs)
    sid = runs[0]["schedule_id"]
    assert skip(calm, sid, next=2)["skipped"] == [first, second]
    # "Next" passes over fires that are already skipped.
    assert skip(calm, sid, next=1)["skipped"] == [third]
    assert schedule_of(calm, "hourly")["skipped"] == 3

    expect_refusal(422, "has passed", skip, calm, sid, fires=[first - HOUR])
    expect_refusal(422, "not a fire of this schedule", skip, calm, sid, fires=[first + 1])
    expect_refusal(422, "more than 100 fires ahead", skip, calm, sid, fires=[first + 200 * HOUR])
    expect_refusal(422, "at least 1", skip, calm, sid, next=0)
    expect_refusal(422, "name the fires", skip, calm, sid)
    expect_refusal(422, "by must be ui or api", skip, calm, sid, fires=[first], by="robot")
    expect_refusal(404, "no skip", unskip, calm, sid, first + 1)
    expect_refusal(422, "has passed", unskip, calm, sid, first - HOUR)
    expect_refusal(404, "schedule not found", skip, calm, 999_999, next=1)


def test_projected_fires_past_the_look_ahead(calm):
    items = upcoming(calm, "hourly", projected=5)
    runs = [i for i in items if not i["projected"]]
    projected = [i for i in items if i["projected"]]
    assert len(runs) == 3 and len(projected) == 5
    times = [i["scheduled_time"] for i in items]
    assert all(b - a == HOUR for a, b in zip(times, times[1:])), times
    assert all("id" not in p and p["skipped"] is False for p in projected)

    before = len(runs_of(calm, "hourly"))
    target = projected[2]["scheduled_time"]
    skip(calm, projected[2]["schedule_id"], fires=[target])
    items = upcoming(calm, "hourly", projected=5)
    assert skipped_times(items) == [target]
    assert next(i for i in items if i["skipped"])["skipped_by"] == "api"
    assert len(runs_of(calm, "hourly")) == before, "skipping a projected fire creates no run"

    capped = [i for i in upcoming(calm, "hourly", projected=500) if i["projected"]]
    assert len(capped) == 100


def test_skips_survive_pause_resume_and_restart(isolated_home, tmp_path):
    directory = write_app(tmp_path, "calm", CALM)
    port = free_port()
    srv = serve(isolated_home, directory, port)
    try:
        runs = upcoming(srv, "hourly")
        sid = runs[0]["schedule_id"]
        second = runs[1]["scheduled_time"]
        skip(srv, sid, fires=[second])
        srv.client._request("POST", f"/api/schedules/{sid}/pause")
        assert upcoming(srv, "hourly") == []
        assert schedule_of(srv, "hourly")["skipped"] == 1, "a paused schedule keeps its skips"
        srv.client._request("POST", f"/api/schedules/{sid}/resume")
        assert skipped_times(upcoming(srv, "hourly")) == [second]
    finally:
        srv.stop()
    srv = serve(isolated_home, directory, port)
    try:
        assert skipped_times(upcoming(srv, "hourly")) == [second]
        assert schedule_of(srv, "hourly")["skipped"] == 1
    finally:
        srv.stop()


# ---------------------------------------------------------------------------
# fire times arriving


def test_skipped_fires_end_skipped_and_the_next_one_runs(busy):
    sid = schedule_of(busy, "ticker")["id"]
    out = skip(busy, sid, next=2, by="ui")
    a, b = out["skipped"]
    assert {d["flow"] for d in out["downstream"]} == {"after_ticker", "further"}
    assert all(d["fires"] == [a, b] for d in out["downstream"])

    for fire in (a, b):
        run = wait_until(lambda: (r := run_at(busy, "ticker", fire)) and ended(r) and r)
        assert run["state"]["name"] == "Skipped" and run["state"]["details"]["reason"] == "user"
        assert run["start_time"] is None, "a skipped fire never starts"
    following = wait_until(lambda: (r := run_at(busy, "ticker", b + 2_000_000)) and ended(r) and r)
    assert following["state"]["name"] == "Completed"

    skipped_run = run_at(busy, "ticker", a)
    events = busy.client.events(kind="run.skipped", limit=500)
    assert any(e["run_id"] == skipped_run["id"] and e["payload"].get("reason") == "user" for e in events)


def test_a_skip_travels_down_the_chain(busy):
    sid = schedule_of(busy, "ticker")["id"]
    fire = skip(busy, sid, next=1)["skipped"][0]
    up = wait_until(lambda: (r := run_at(busy, "ticker", fire)) and ended(r) and r)

    def made_by(name, run_id):
        return next((r for r in runs_of(busy, name) if r["created_by"] == f"run:{run_id}"), None)

    down = wait_until(lambda: made_by("after_ticker", up["id"]))
    assert down["state"]["name"] == "Skipped" and down["start_time"] is None
    assert down["state"]["details"] == {"reason": "upstream", "upstream_run": up["id"]}
    further = wait_until(lambda: made_by("further", down["id"]))
    assert further["state"]["name"] == "Skipped"
    assert further["state"]["details"]["reason"] == "upstream"

    # The next ticker run completes and its downstream runs as usual.
    nxt = wait_until(lambda: (r := run_at(busy, "ticker", fire + 2_000_000)) and ended(r) and r)
    normal = wait_until(lambda: (r := made_by("after_ticker", nxt["id"])) and ended(r) and r)
    assert normal["state"]["name"] == "Completed"


def test_fan_in_with_one_upstream_skipped_by_a_person(busy):
    # One fire only, so the skipped run is the upstream's latest for the key.
    at = datetime.now(timezone.utc) + timedelta(seconds=3)
    made = busy.client._request(
        "POST",
        f"/api/flows/{flow_of(busy, 'sales')['id']}/schedules",
        body={"kind": "rrule", "rrule": f"DTSTART:{at:%Y%m%dT%H%M%SZ}\nRRULE:FREQ=DAILY;COUNT=1", "timezone": "UTC"},
    )
    out = skip(busy, made["id"], next=1)
    assert [d["flow"] for d in out["downstream"]] == ["report"]
    fire = out["skipped"][0]
    sales = wait_until(lambda: (r := run_at(busy, "sales", fire)) and ended(r) and r)
    assert sales["state"]["name"] == "Skipped"
    assert runs_of(busy, "report") == [], "inventory has not finished the day yet"

    inventory = busy.client._request(
        "POST", f"/api/flows/{flow_of(busy, 'inventory')['id']}/runs", body={"parameters": {"day": "2026-09-13"}}
    )
    busy.wait_run(inventory["id"])
    report = wait_until(lambda: next(iter(runs_of(busy, "report")), None))
    assert report["state"]["name"] == "Skipped" and report["state"]["details"]["reason"] == "upstream"
    assert report["parameters"]["day"] == "2026-09-13"


def test_an_overlap_skip_still_triggers_downstream(calm):
    fid = flow_of(calm, "overlap")["id"]
    first = calm.client._request("POST", f"/api/flows/{fid}/runs", body={"parameters": {"seconds": 2}})
    calm.wait_run(first["id"], until=lambda r: r["state"]["type"] == "Running")
    second = calm.client._request("POST", f"/api/flows/{fid}/runs", body={"parameters": {"seconds": 0.1}})
    done = calm.wait_run(second["id"], timeout=10)
    assert done["state"]["name"] == "Skipped" and "reason" not in done["state"]["details"]
    after = wait_until(
        lambda: next((r for r in runs_of(calm, "after_overlap") if r["created_by"] == f"run:{second['id']}"), None)
    )
    assert calm.wait_run(after["id"])["state"]["name"] == "Completed"


def test_a_skip_made_before_a_restart_still_ends_skipped(isolated_home, tmp_path):
    directory = write_app(tmp_path, "busy", BUSY)
    port = free_port()
    srv = serve(isolated_home, directory, port)
    try:
        sid = schedule_of(srv, "ticker")["id"]
        soon = int(time.time() * 1_000_000) + 8_000_000
        fire = next(r["scheduled_time"] for r in upcoming(srv, "ticker") if r["scheduled_time"] > soon)
        skip(srv, sid, fires=[fire])
    finally:
        srv.stop()
    srv = serve(isolated_home, directory, port)
    try:
        run = wait_until(lambda: (r := run_at(srv, "ticker", fire)) and ended(r) and r, timeout=40)
        assert run["state"]["name"] == "Skipped" and run["state"]["details"]["reason"] == "user"
    finally:
        srv.stop()


def test_catch_up_leaves_a_skipped_fire_skipped(isolated_home, tmp_path):
    directory = write_app(tmp_path, "calm", CALM)
    port = free_port()
    srv = serve(isolated_home, directory, port)
    now = int(time.time() * 1_000_000)
    try:
        made = srv.client._request(
            "POST",
            f"/api/flows/{flow_of(srv, 'plain')['id']}/schedules",
            body={"kind": "interval", "interval": 60, "anchor": now, "timezone": "UTC", "catchup": "all", "catchup_max": 10},
        )
        sid = made["id"]
    finally:
        srv.stop()
    # Pretend the server slept through four fires, one of which had been skipped.
    skipped_fire = now - 180 * 1_000_000
    db = sqlite3.connect(str(isolated_home / "db.sqlite"))
    db.execute("UPDATE kv SET value = ? WHERE key = 'scheduler.last_wakeup'", (str(now - 250 * 1_000_000),))
    db.execute("UPDATE schedule SET spec = json_set(spec, '$.anchor', ?) WHERE id = ?", (now - 300 * 1_000_000, sid))
    db.execute(
        "INSERT INTO schedule_skip (schedule_id, fire_time, created_at, created_by) VALUES (?, ?, ?, 'api')",
        (sid, skipped_fire, now),
    )
    db.commit()
    db.close()
    srv = serve(isolated_home, directory, port)
    try:
        catchup = [r for r in runs_of(srv, "plain") if r["created_by"] == "catchup"]
        times = {r["scheduled_time"] for r in catchup}
        assert skipped_fire not in times
        assert {now - k * 60 * 1_000_000 for k in (4, 2, 1)} <= times, sorted(times)
        for r in catchup:
            srv.wait_run(r["id"])
    finally:
        srv.stop()


# ---------------------------------------------------------------------------
# retimes and the persist default


def test_skips_survive_an_edit_that_keeps_their_fire(calm):
    sid = schedule_of(calm, "morning")["id"]
    skip(calm, sid, next=2)
    calm.client._request("PATCH", f"/api/schedules/{sid}", body={"catchup": "latest"})
    assert schedule_of(calm, "morning")["skipped"] == 2
    assert not calm.client.events(kind="schedule.skips_dropped")


def test_a_retime_drops_skips_it_no_longer_produces(calm):
    sid = schedule_of(calm, "morning")["id"]
    fires = skip(calm, sid, next=2)["skipped"]
    calm.client._request("PATCH", f"/api/schedules/{sid}", body={"cron": "15 7 * * *"})
    assert schedule_of(calm, "morning")["skipped"] == 0
    events = calm.client.events(kind="schedule.skips_dropped")
    assert len(events) == 1
    payload = events[0]["payload"]
    assert payload["schedule_id"] == sid and payload["dropped"] == fires


def test_an_edit_lasts_until_restart_unless_persisted(isolated_home, tmp_path):
    directory = write_app(tmp_path, "calm", CALM)
    port = free_port()
    srv = serve(isolated_home, directory, port)
    try:
        sid = schedule_of(srv, "morning")["id"]
        patched = srv.client._request("PATCH", f"/api/schedules/{sid}", body={"cron": "15 7 * * *"})
        assert patched["schedule"]["cron"] == "15 7 * * *" and patched["persist"] is False
        # A skip on the edited time, which the declaration will not produce.
        fire = skip(srv, sid, next=1)["skipped"][0]
    finally:
        srv.stop()
    srv = serve(isolated_home, directory, port)
    try:
        row = schedule_of(srv, "morning")
        assert row["schedule"]["cron"] == "30 6 * * *", "the declaration applies again"
        assert row["skipped"] == 0
        dropped = srv.client.events(kind="schedule.skips_dropped")
        assert [e["payload"]["dropped"] for e in dropped] == [[fire]]
        kept = srv.client._request("PATCH", f"/api/schedules/{sid}", body={"cron": "15 7 * * *", "persist": True})
        assert kept["persist"] is True
    finally:
        srv.stop()
    srv = serve(isolated_home, directory, port)
    try:
        row = schedule_of(srv, "morning")
        assert row["schedule"]["cron"] == "15 7 * * *" and row["persist"] is True
    finally:
        srv.stop()
