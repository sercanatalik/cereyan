"""M3.2 unique runs: a flow's unique key, period buckets, replace, the request
idempotency key, rules, dependencies, and the offline path."""

from __future__ import annotations

import json
import time

import pytest

from cereyan.client import ApiError
from server_helpers import ServerProcess

PIPELINE = '''
import time
from cereyan import App, Unique

app = App("uniq")

@app.flow(unique=Unique(key="{day}"))
def load(day: str = "2026-09-20", seconds: float = 2.0):
    time.sleep(seconds)
    return day

@app.flow(unique=Unique(period=3600, states=("Scheduled", "Pending", "Running", "Completed")))
def hourly_report(region: str = "eu"):
    return region

@app.flow(unique=Unique(on_conflict="replace"))
def sync(seconds: float = 3.0):
    time.sleep(seconds)

@app.flow
def plain(n: int = 1):
    return n

@app.flow(after="plain")
def downstream(n: int = 1):
    return n
'''


@pytest.fixture
def srv(isolated_home, tmp_path):
    from cereyan import engine

    engine.close_store()
    d = tmp_path / "uniq"
    d.mkdir()
    (d / "pipeline.py").write_text(PIPELINE)
    server = ServerProcess(str(isolated_home), str(d))
    try:
        yield server
    finally:
        server.stop()


def wait_until(fn, timeout=20):
    deadline = time.time() + timeout
    while time.time() < deadline:
        v = fn()
        if v:
            return v
        time.sleep(0.1)
    raise AssertionError("condition not met in time")


def test_unique_key_holds_while_the_run_is_active(srv):
    c = srv.client
    first = c.run("load", day="2026-09-20", seconds=3)
    assert first.get("conflict") is None and first["unique_key"] == f"flow:{first['flow_id']}|2026-09-20|"
    # Same day: the existing run is answered and nothing is created.
    second = c.run("load", day="2026-09-20", seconds=1)
    assert second["conflict"] is True and second["id"] == first["id"]
    # Another day is another key.
    other = c.run("load", day="2026-09-21", seconds=0.1)
    assert other.get("conflict") is None and other["id"] != first["id"]
    srv.wait_run(first["id"])
    srv.wait_run(other["id"])
    # Terminal runs do not count by default.
    third = c.run("load", day="2026-09-20", seconds=0.1)
    assert third.get("conflict") is None and third["id"] != first["id"]
    srv.wait_run(third["id"])
    assert len([r for r in c.runs(flow="load", limit=50)["items"]]) == 3
    # The raw API answers 200 with the run, 201 when created.
    flow_id = first["flow_id"]
    made = c._request("POST", f"/api/flows/{flow_id}/runs", body={"parameters": {"day": "2026-09-22", "seconds": 2}})
    held = c._request("POST", f"/api/flows/{flow_id}/runs", body={"parameters": {"day": "2026-09-22", "seconds": 2}})
    assert held == {"conflict": True, "run": c.get_run(made["id"])} or held["run"]["id"] == made["id"]
    c.cancel(made["id"])


def test_period_bucket_and_replace(srv):
    c = srv.client
    done = c.run("hourly_report", region="eu")
    assert srv.wait_run(done["id"])["state"]["type"] == "Completed"
    # Completed counts for this flow, so the same hour answers with it.
    again = c.run("hourly_report", region="eu")
    assert again["conflict"] is True and again["id"] == done["id"]
    assert c.run("hourly_report", region="us").get("conflict") is None
    # replace: the waiting run is cancelled and the new one created. Without a
    # key template every parameter is the key, so the two calls must match.
    waiting = c.run("sync", seconds=30, delay=60)
    assert waiting["state"]["type"] == "Scheduled"
    replaced = c.run("sync", seconds=30, delay=60)
    assert replaced.get("conflict") is None and replaced["id"] != waiting["id"]
    assert c.get_run(waiting["id"])["state"]["type"] == "Cancelled"
    assert c.get_run(replaced["id"])["state"]["type"] == "Scheduled"
    c.cancel(replaced["id"])
    srv.wait_run(replaced["id"])


def test_idempotency_key(srv):
    c = srv.client
    first = c.run("plain", n=7, idempotency_key="order-42")
    assert first.get("conflict") is None
    srv.wait_run(first["id"])
    # Whatever the state, the same key within the TTL answers with the first run.
    second = c.run("plain", n=7, idempotency_key="order-42")
    assert second["conflict"] is True and second["id"] == first["id"]
    # A different key, or the same key after its TTL, creates a run.
    assert c.run("plain", n=7, idempotency_key="order-43").get("conflict") is None
    time.sleep(1.2)
    later = c.run("plain", n=7, idempotency_key="order-42", idempotency_ttl=1)
    assert later.get("conflict") is None and later["id"] != first["id"]
    with pytest.raises(ApiError) as err:
        c.run("plain", n=7, idempotency_key="x", idempotency_ttl=-1)
    assert err.value.status == 422
    srv.wait_run(later["id"])


def test_rule_mcp_and_dependency_honour_the_key(srv):
    c = srv.client
    rule = c._request("POST", "/api/rules", body={
        "name": "kick", "when": {"events": ["run.completed"], "flows": ["plain"]},
        "do": [{"kind": "run_flow", "flow": "load", "parameters": {"day": "rule-day", "seconds": 4}}],
        "once": "never",
    })
    a = c.run("plain", n=1)
    srv.wait_run(a["id"])
    b = c.run("plain", n=2)
    srv.wait_run(b["id"])
    firings = wait_until(lambda: (lambda f: f if len(f) == 2 else None)(c._request("GET", f"/api/rules/{rule['id']}/firings")))
    outcomes = sorted((f["outcomes"][0]["detail"] for f in firings), key=lambda d: "conflict" in d)
    assert "run_id" in outcomes[0] and outcomes[1]["conflict"] is True and outcomes[1]["run_id"] == outcomes[0]["run_id"]
    rule_runs = [r for r in c.runs(flow="load", limit=50)["items"] if r["created_by"] == f"rule:{rule['id']}"]
    assert len(rule_runs) == 1
    c.cancel(rule_runs[0]["id"])
    # A dependency-triggered run is keyed by its upstream run.
    down = wait_until(lambda: [r for r in c.runs(flow="downstream", limit=50)["items"] if r["created_by"] == f"run:{a['id']}"])
    assert down[0]["unique_key"] == f"flow:{down[0]['flow_id']}|dep:{a['id']}|"


def test_offline_unique_flow(isolated_home):
    from cereyan import Unique, engine, flow

    engine.close_store()
    calls = []

    @flow(unique=Unique(key="{day}", period=3600, states=("Completed", "Running")))
    def report(day: str = "2026-09-20"):
        calls.append(day)
        return day

    assert report(day="2026-09-20") == "2026-09-20"
    assert report(day="2026-09-20") is None
    assert report(day="2026-09-21") == "2026-09-21"
    assert calls == ["2026-09-20", "2026-09-21"]
    store = engine.get_store()
    assert len(json.loads(store.list_runs())["items"]) == 2
    with pytest.raises(ValueError):
        Unique(on_conflict="maybe")


DEBOUNCE_PIPELINE = '''
import time
from cereyan import App, Unique

app = App("burst")

@app.flow(unique=Unique(key="{doc}", on_conflict="debounce", debounce=3, max_wait=8))
def render(doc: str = "a", version: int = 1):
    return f"{doc}:{version}"

@app.flow(unique=Unique(key="every-ping", on_conflict="throttle", period=3))
def ping(n: int = 0):
    return n
'''


@pytest.fixture
def burst(isolated_home, tmp_path):
    from cereyan import engine

    engine.close_store()
    d = tmp_path / "burst"
    d.mkdir()
    (d / "pipeline.py").write_text(DEBOUNCE_PIPELINE)
    server = ServerProcess(str(isolated_home), str(d))
    try:
        yield server
    finally:
        server.stop()


def test_debounce_moves_one_run_along_with_the_latest_parameters(burst):
    c = burst.client
    t0 = time.time()
    first = c.run("render", doc="a", version=1)
    assert first.get("conflict") is None and first["state"]["type"] == "Scheduled"
    assert first["scheduled_time"] >= int(t0 * 1_000_000) + 2_500_000
    time.sleep(0.5)
    second = c.run("render", doc="a", version=2)
    time.sleep(0.5)
    third = c.run("render", doc="a", version=3)
    assert second["conflict"] is True and third["conflict"] is True
    assert second["id"] == first["id"] == third["id"]
    assert third["parameters"]["version"] == 3 and third["scheduled_time"] > first["scheduled_time"]
    other = c.run("render", doc="b", version=9)
    assert other.get("conflict") is None and other["id"] != first["id"]
    done = burst.wait_run(first["id"], timeout=30)
    assert done["state"]["type"] == "Completed" and done["parameters"]["version"] == 3
    assert done["start_time"] >= third["scheduled_time"] - 300_000
    doc_a = [r for r in c.runs(flow="render", limit=20)["items"] if r["parameters"]["doc"] == "a"]
    assert len(doc_a) == 1, [(r["id"], r["created_by"], r["state"], r["scheduled_time"], r["unique_key"]) for r in doc_a]
    # A started run opens a new window: the next submission is a new run.
    again = c.run("render", doc="a", version=4)
    assert again.get("conflict") is None and again["id"] != first["id"]
    burst.wait_run(other["id"])
    burst.wait_run(again["id"])


def test_debounce_cap_and_throttle(burst):
    c = burst.client
    t0 = time.time()
    run = c.run("render", doc="cap", version=0)
    for i in range(1, 8):
        time.sleep(1)
        moved = c.run("render", doc="cap", version=i)
        assert moved["id"] == run["id"]
    # max_wait=8: the run started about eight seconds after its creation despite the burst.
    done = burst.wait_run(run["id"], timeout=40)
    assert done["state"]["type"] == "Completed"
    started = done["start_time"] / 1_000_000 - t0
    assert 7.5 <= started <= 11.5, started
    # throttle: the first run in a three-second window is kept, the others answer with it.
    a = c.run("ping", n=1)
    b = c.run("ping", n=2)
    d = c.run("ping", n=3)
    assert a.get("conflict") is None and b["conflict"] is True and d["conflict"] is True
    assert b["id"] == a["id"] == d["id"]
    burst.wait_run(a["id"])
    time.sleep(3.5)
    later = c.run("ping", n=4)
    assert later.get("conflict") is None and later["id"] != a["id"]
    burst.wait_run(later["id"])
    with pytest.raises(ValueError):
        __import__("cereyan").Unique(on_conflict="debounce")
    with pytest.raises(ValueError):
        __import__("cereyan").Unique(on_conflict="throttle")
