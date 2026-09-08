"""1.1 proactive rules: event-armed and clock-armed `unless` rules, offline and served."""

from __future__ import annotations

import json
import os
import time

import pytest

from cereyan import App
from cereyan.client import ApiError
from server_helpers import ServerProcess, free_port

PIPELINE = '''
import os, time
from cereyan import App

app = App("proactive")
HOOK = os.environ.get("RULE_HOOK_FILE")

def note(line):
    with open(HOOK, "a") as fh:
        fh.write(line + "\\n")

@app.flow
def slow(seconds: float = 1.0):
    time.sleep(seconds)
    return seconds

@app.flow
def fails_after(seconds: float = 1.0):
    time.sleep(seconds)
    raise ValueError("late failure")

@app.flow
def quick():
    return 1

@app.rule(on="run.running", flow="slow", unless="run.completed", within=0.6)
def overrun(event, run):
    note(f"overrun:{run['id']}:{event['payload']['expected'][0]}")

@app.rule(on="run.running", flow="fails_after", unless="run.completed", within=0.4)
def failed_not_completed(event, run):
    note(f"failed:{run['id']}")

@app.rule(at="*/2 * * * * *", flow="quick", unless="run.completed", within=3)
def heartbeat(event, run):
    note("heartbeat")
'''


@pytest.fixture(autouse=True)
def _isolated_rule_registry():
    """Code rules registered by these tests must not leak into other files."""
    from cereyan import rules as rules_mod

    saved = dict(rules_mod._registry)
    rules_mod._expectations.clear()
    rules_mod._guards.clear()
    yield
    rules_mod._registry.clear()
    rules_mod._registry.update(saved)
    rules_mod._expectations.clear()
    rules_mod._guards.clear()


@pytest.fixture
def pro(isolated_home, tmp_path):
    from cereyan import engine

    engine.close_store()
    d = tmp_path / "proactive"
    d.mkdir()
    (d / "pipeline.py").write_text(PIPELINE)
    hook = tmp_path / "hook.txt"
    hook.write_text("")
    srv = ServerProcess(str(isolated_home), str(d), env={"RULE_HOOK_FILE": str(hook)})
    srv.hook = hook
    srv.directory_path = d
    try:
        yield srv
    finally:
        srv.stop()


def lines(srv):
    return [ln for ln in srv.hook.read_text().splitlines() if ln]


def rule_id(srv, name):
    return next(r["id"] for r in srv.client._request("GET", "/api/rules") if r["name"] == name)


def start(srv, flow, **params):
    fid = next(f["id"] for f in srv.client.flows() if f["name"] == flow and f["project"] == "proactive")
    return srv.client._request("POST", f"/api/flows/{fid}/runs", body={"parameters": params})


def wait_for(fn, timeout=10, interval=0.05):
    deadline = time.time() + timeout
    while time.time() < deadline:
        v = fn()
        if v:
            return v
        time.sleep(interval)
    raise AssertionError("condition not met in time")


# ---- offline ----------------------------------------------------------------


def test_offline_lapse_fires_when_run_overruns(store):
    calls = []
    app = App("offline_pro")

    @app.rule(on="run.running", flow="crawl", unless="run.completed", within=0.2)
    def late(event, run):
        calls.append((event["name"], event["payload"]["expected"], run["id"]))

    @app.flow
    def crawl(seconds: float = 0.0):
        time.sleep(seconds)
        return 1

    from cereyan.rules import register_with_store

    register_with_store(store)
    crawl(0.0)
    assert calls == []
    crawl(0.4)
    assert len(calls) == 1 and calls[0][0] == "expectation.lapsed" and calls[0][1] == ["run.completed"]
    page = json.loads(store.query_events(json.dumps({"name": "expectation.lapsed", "limit": 5})))
    assert page["items"][0]["resource"]["kind"] == "rule"
    rows = {r["name"]: r for r in json.loads(store.list_rules())}
    assert rows["late"]["fire_count"] == 1 and rows["late"]["unless"]["events"] == ["run.completed"]


def test_decorator_validation():
    app = App("bad_rules")
    with pytest.raises(ValueError):
        @app.rule(on="run.running", unless="run.completed")
        def no_within(event, run):
            pass
    with pytest.raises(ValueError):
        @app.rule(at="0 9 * * *")
        def no_unless(event, run):
            pass


# ---- served ---------------------------------------------------------------


def test_overrun_fires_once_and_in_time_does_not(pro):
    fast = start(pro, "slow", seconds=0.05)
    pro.wait_run(fast["id"])
    long = start(pro, "slow", seconds=1.5)
    rid = rule_id(pro, "overrun")
    wait_for(lambda: pro.client._request("GET", f"/api/rules/{rid}/expectations"))
    pro.wait_run(long["id"])
    wait_for(lambda: any(ln.startswith(f"overrun:{long['id']}:run.completed") for ln in lines(pro)))
    time.sleep(0.8)
    assert [ln for ln in lines(pro) if ln.startswith("overrun:")] == [f"overrun:{long['id']}:run.completed"]
    history = pro.client._request("GET", f"/api/rules/{rid}/expectations?open=false")
    statuses = {e["run_id"]: e["status"] for e in history}
    assert statuses[fast["id"]] == "met" and statuses[long["id"]] == "lapsed"
    events = pro.client._request("GET", f"/api/events?name=expectation.lapsed&run_id={long['id']}")["items"]
    assert len(events) == 1 and events[0]["payload"]["rule"] == "overrun" and events[0]["resource"]["kind"] == "rule"
    firings = pro.client._request("GET", f"/api/rules/{rid}/firings")
    assert len(firings) == 1 and firings[0]["run_id"] == long["id"]


def test_failure_is_not_completion(pro):
    run = start(pro, "fails_after", seconds=0.05)
    pro.wait_run(run["id"])
    wait_for(lambda: f"failed:{run['id']}" in lines(pro), timeout=5)


def test_cooldown_across_lapses_and_disable_cancels(pro):
    # A data rule (code rules are read-only): lapse cancels the run.
    created = pro.client._request("POST", "/api/rules", body={
        "name": "cancel-overrun", "when": {"events": ["run.running"], "flows": ["slow"]},
        "unless": {"events": ["run.completed"]}, "within": 0.6, "cooldown_seconds": 60,
        "do": [{"kind": "cancel_run"}],
    })
    rid = created["id"]
    a = start(pro, "slow", seconds=1.2)
    b = start(pro, "slow", seconds=1.2)
    done_a = pro.wait_run(a["id"])
    done_b = pro.wait_run(b["id"])
    time.sleep(0.5)
    firings = pro.client._request("GET", f"/api/rules/{rid}/firings")
    assert len(firings) == 1, firings
    states = sorted([done_a["state"]["type"], done_b["state"]["type"]])
    assert states == ["Cancelled", "Completed"]
    # Disable: open expectations are cancelled and nothing fires.
    pro.client._request("PATCH", f"/api/rules/{rid}", body={"cooldown_seconds": 0})
    c = start(pro, "slow", seconds=1.2)
    wait_for(lambda: pro.client._request("GET", f"/api/rules/{rid}/expectations"))
    pro.client._request("PATCH", f"/api/rules/{rid}", body={"enabled": False})
    done_c = pro.wait_run(c["id"])
    assert done_c["state"]["type"] == "Completed"
    assert len(pro.client._request("GET", f"/api/rules/{rid}/firings")) == 1
    history = {e["run_id"]: e["status"] for e in pro.client._request("GET", f"/api/rules/{rid}/expectations?open=false")}
    assert history[c["id"]] == "cancelled"


def test_expectation_survives_restart(isolated_home, tmp_path):
    from cereyan import engine

    engine.close_store()
    d = tmp_path / "proactive"
    d.mkdir()
    (d / "pipeline.py").write_text(PIPELINE)
    hook = tmp_path / "hook.txt"
    hook.write_text("")
    port = free_port()
    srv = ServerProcess(str(isolated_home), str(d), port=port, env={"RULE_HOOK_FILE": str(hook)})
    srv.hook = hook
    rid = rule_id(srv, "overrun")
    run = start(srv, "slow", seconds=4.0)
    wait_for(lambda: srv.client._request("GET", f"/api/rules/{rid}/expectations"))
    clock_rule = rule_id(srv, "heartbeat")
    assert srv.stop(kill_engines=False) == 0
    # Down across the 0.6 s deadline; pretend the clock rule last ticked an hour ago.
    time.sleep(2.5)
    import sqlite3

    db = sqlite3.connect(os.path.join(str(isolated_home), "db.sqlite"))
    db.execute("INSERT OR REPLACE INTO kv (key, value, updated_at) VALUES (?, ?, ?)",
               (f"rules.clock_last:{clock_rule}", str(int((time.time() - 3600) * 1e6)), int(time.time() * 1e6)))
    db.commit()
    db.close()
    again = ServerProcess(str(isolated_home), str(d), port=port, env={"RULE_HOOK_FILE": str(hook)})
    again.hook = hook
    try:
        # The deadline passed while down: the lapse fires once on start.
        wait_for(lambda: any(ln.startswith(f"overrun:{run['id']}") for ln in lines(again)), timeout=10)
        history = {e["run_id"]: e["status"] for e in again.client._request("GET", f"/api/rules/{rid}/expectations?open=false")}
        assert history[run["id"]] == "lapsed"
        again.wait_run(run["id"], timeout=30)
        assert "skipped clock tick" in again.read_log()
    finally:
        again.stop()


def test_clock_armed_rule_fires_only_when_window_is_empty(pro):
    rid = rule_id(pro, "heartbeat")
    row = pro.client._request("GET", f"/api/rules/{rid}")
    assert row["at"]["cron"] == "*/2 * * * * *" and row["within"] == 3
    # Keep `quick` completing inside every 3 s window for a while: no lapse.
    t0 = time.time()
    while time.time() - t0 < 4.5:
        pro.wait_run(start(pro, "quick")["id"])
        time.sleep(0.5)
    assert "heartbeat" not in lines(pro)
    # Then stop producing: the next tick after the window empties fires.
    wait_for(lambda: "heartbeat" in lines(pro), timeout=8)


def test_validation_and_dry_run(pro):
    with pytest.raises(ApiError) as err:
        pro.client._request("POST", "/api/rules", body={
            "name": "bad", "when": {"events": ["run.running"]}, "unless": {"events": ["run.completed"]},
            "do": [{"kind": "cancel_run"}],
        })
    assert err.value.status == 422 and "within" in str(err.value)
    with pytest.raises(ApiError) as err:
        pro.client._request("POST", "/api/rules", body={
            "name": "bad2", "when": {"events": []}, "unless": {"events": ["run.completed"]},
            "at": {"cron": "not a cron"}, "do": [{"kind": "cancel_run"}],
        })
    assert err.value.status == 422
    created = pro.client._request("POST", "/api/rules", body={
        "name": "data-proactive", "when": {"events": ["run.running"], "flows": ["slow"]},
        "unless": {"events": ["run.completed"]}, "within": 30,
        "do": [{"kind": "webhook", "url": "http://127.0.0.1:9/x", "body": "{{ event.payload.expected[0] }} missing for {{ flow.name }}"}],
    })
    pro.wait_run(start(pro, "slow", seconds=0.05)["id"])
    result = pro.client._request("POST", f"/api/rules/{created['id']}/test")
    assert result["event"]["name"] == "expectation.lapsed" and result["event"]["payload"]["synthetic"] is True
    assert result["actions"][0]["rendered"]["body"] == "run.completed missing for slow"
    assert pro.client._request("GET", f"/api/rules/{created['id']}/expectations?open=false") == [] or all(
        e["status"] != "lapsed" for e in pro.client._request("GET", f"/api/rules/{created['id']}/expectations?open=false"))
