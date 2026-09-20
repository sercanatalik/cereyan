"""Events, rules, artifacts, variables, settings, and retention through a server."""

import http.server
import json
import os
import socket
import sqlite3
import threading
import time

import pytest

from cereyan.client import ApiError
from server_helpers import ServerProcess

PIPELINE = '''
import os, time
from datetime import date
from cereyan import App, flow, task, emit_event, artifacts, Variable, get_run_logger

app = App("obs")

@task
def step(day: date):
    artifacts.create_table([{"day": str(day)}])
    emit_event("orders.checked", {"day": str(day)})
    return 1

@app.flow
def etl(day: date = date(2026, 9, 6)):
    step(day)
    artifacts.create_progress(10, key="load")
    artifacts.update_progress("load", 60)
    return str(day)

@app.flow
def cleanup(day: date = date(2026, 1, 1)):
    return str(day)

@app.flow
def maybe(ok: bool = True):
    if not ok:
        raise RuntimeError("bad day")

@app.flow
def fail():
    raise ValueError("bad")

@app.flow(retries=1, retry_delay=0)
def flaky():
    raise ValueError("again")

_passes = []

@task
def counted(n: int):
    return n

@app.flow(retries=1, retry_delay=0)
def retried_with_tasks():
    counted(1)
    _passes.append(1)
    if len(_passes) == 1:
        raise ValueError("first pass fails")
    counted(2)
    return "ok"

@app.flow
def loop_a():
    return "a"

@app.flow
def reads_secret():
    return Variable.get("api_token")

@app.flow
def fulfil(order: str = "A1", seconds: float = 20.0):
    time.sleep(seconds)

@app.flow
def cancel_order(order: str = "A1"):
    emit_event("orders.cancelled", {"order": order})
    time.sleep(1.0)

HOOK = os.environ.get("RULE_HOOK_FILE")

@app.rule(on="run.failed", flow="fail")
def on_fail(event, run):
    with open(HOOK, "a") as fh:
        fh.write(f"{run['id']}:{event['name']}\\n")
    return {"seen": run["id"]}
'''


class Hook(http.server.BaseHTTPRequestHandler):
    calls = []
    headers_seen = []
    fail_first = 0

    def do_POST(self):
        length = int(self.headers.get("content-length", 0))
        body = self.rfile.read(length)
        Hook.calls.append(body)
        Hook.headers_seen.append({k.lower(): v for k, v in self.headers.items()})
        if Hook.fail_first > 0:
            Hook.fail_first -= 1
            self.send_response(503)
        else:
            self.send_response(200)
        self.end_headers()

    def log_message(self, *a):
        pass


@pytest.fixture
def webhook():
    server = http.server.HTTPServer(("127.0.0.1", 0), Hook)
    Hook.calls = []
    Hook.headers_seen = []
    Hook.fail_first = 0
    t = threading.Thread(target=server.serve_forever, daemon=True)
    t.start()
    yield f"http://127.0.0.1:{server.server_port}/hook"
    server.shutdown()


@pytest.fixture
def obs_dir(tmp_path):
    d = tmp_path / "obs"
    d.mkdir()
    (d / "pipeline.py").write_text(PIPELINE)
    return d


@pytest.fixture
def obs(isolated_home, obs_dir, tmp_path):
    from cereyan import engine

    engine.close_store()
    hook = tmp_path / "rulehook.txt"
    srv = ServerProcess(str(isolated_home), str(obs_dir), env={"RULE_HOOK_FILE": str(hook), "CEREYAN_RETENTION_INTERVAL": "2"})
    srv.hook = hook
    try:
        yield srv
    finally:
        srv.stop()


def fid(server, name):
    return next(f["id"] for f in server.client.flows() if f["name"] == name and f["project"] == "obs")


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


def test_retry_restarts_task_keys_in_a_new_pass(obs):
    run = start(obs, "retried_with_tasks")
    done = obs.wait_run(run["id"], timeout=30)
    assert done["state"]["type"] == "Completed"
    tasks = obs.client._request("GET", f"/api/runs/{run['id']}/tasks")
    by_pass: dict[int, list[str]] = {}
    for t in tasks:
        by_pass.setdefault(t["pass"], []).append(t["dynamic_key"])
    # The execution that failed recorded `counted-0`. The retry is a new pass
    # and numbers its calls from zero again, so the same call keeps its key
    # instead of continuing at `counted-1`.
    assert by_pass[0] == ["counted-0"]
    assert by_pass[1] == ["counted-0", "counted-1"]


def test_engine_events_and_filters(obs):
    c = obs.client
    run = start(obs, "etl")
    obs.wait_run(run["id"])
    evs = c.events(run_id=run["id"], ascending="true", limit=200)
    names = [e["name"] for e in evs]
    for expected in ["run.scheduled", "run.pending", "run.running", "run.completed", "task_run.running", "task_run.completed", "orders.checked"]:
        assert expected in names, names
    assert names.index("run.running") < names.index("run.completed")
    completed = next(e for e in evs if e["name"] == "run.completed")
    assert completed["resource"]["kind"] == "run" and any(r["kind"] == "flow" and r["name"] == "etl" for r in completed["related"])
    custom = next(e for e in evs if e["name"] == "orders.checked")
    assert custom["payload"] == {"day": "2026-09-06"} and custom["resource"]["kind"] == "run"
    only_runs = c.events(kind="run.*", run_id=run["id"], limit=100)
    assert only_runs and all(e["name"].startswith("run.") for e in only_runs)
    assert any(e["name"] == "flow.registered" for e in c.events(kind="flow.*"))
    page = c._request("GET", "/api/events", params={"limit": 2})
    assert len(page["items"]) == 2 and page["next_cursor"]
    posted = c._request("POST", "/api/events", body={"name": "custom.outside", "payload": {"k": 1}})
    assert posted["resource"]["kind"] == "custom"


def test_rule_run_flow_with_template_and_self_trigger(obs):
    c = obs.client
    rule = c.create_rule({
        "name": "cleanup after etl",
        "when": {"events": ["run.*"], "flows": ["etl"], "states": ["Completed"]},
        "do": [{"kind": "run_flow", "flow": "cleanup", "parameters": {"day": "{{ run.parameters.day }}"}}],
    })
    run = start(obs, "etl", day="2026-09-06")
    obs.wait_run(run["id"])
    created = wait_until(lambda: (r := c.runs(flow="cleanup")["items"]) and r[0], timeout=15)
    assert created["parameters"]["day"] == "2026-09-06"
    assert created["created_by"] == f"rule:{rule['id']}"
    obs.wait_run(created["id"])
    firings = c._request("GET", f"/api/rules/{rule['id']}/firings")
    assert len(firings) == 1 and firings[0]["outcomes"][0]["status"] == "completed"
    rules = {r["name"]: r for r in c.rules()}
    assert rules["cleanup after etl"]["fire_count"] == 1
    # A rule that runs its own flow does not loop.
    loop = c.create_rule({
        "name": "loop",
        "when": {"events": ["run.completed"], "flows": ["loop_a"]},
        "do": [{"kind": "run_flow", "flow": "loop_a"}],
    })
    first = start(obs, "loop_a")
    obs.wait_run(first["id"])
    second = wait_until(lambda: (r := [x for x in c.runs(flow="loop_a", limit=50)["items"] if x["created_by"] == f"rule:{loop['id']}"]) and r[0], timeout=15)
    obs.wait_run(second["id"])
    time.sleep(1.5)
    assert len(c.runs(flow="loop_a", limit=50)["items"]) == 2
    assert c._request("GET", f"/api/rules/{loop['id']}")["fire_count"] == 1
    assert any(e["name"] == "rule.fired" for e in c.events(kind="rule.*"))


def test_webhook_retries_and_template_error_isolation(obs, webhook):
    c = obs.client
    Hook.fail_first = 2
    rule = c.create_rule({
        "name": "hook",
        "when": {"events": ["run.failed"]},
        "do": [
            {"kind": "webhook", "url": "http://127.0.0.1:1/never", "body": "{{ nope.missing }}"},
            {"kind": "webhook", "url": webhook, "body": '{"run": "{{ run.name }}", "msg": "{{ state.message }}"}'},
        ],
    })
    run = start(obs, "fail")
    obs.wait_run(run["id"])
    # The firing is recorded before its actions run and filled in as they complete.
    firing = wait_until(
        lambda: (f := c._request("GET", f"/api/rules/{rule['id']}/firings"))
        and f[0]
        and all(o["status"] != "pending" for o in f[0]["outcomes"])
        and f[0],
        timeout=20,
    )
    assert firing["outcomes"][0]["status"] == "failed" and "undefined" in firing["outcomes"][0]["error"]
    assert firing["outcomes"][1]["status"] == "completed" and firing["outcomes"][1]["detail"]["attempts"] == 3
    assert len(Hook.calls) == 3
    payload = json.loads(Hook.calls[-1])
    assert payload["run"] == run["name"] and "ValueError: bad" in payload["msg"]
    kinds = [e["name"] for e in c.events(kind="rule.action.*")]
    assert "rule.action.failed" in kinds and "rule.action.completed" in kinds
    tested = c._request("POST", f"/api/rules/{rule['id']}/test")
    assert tested["actions"][1]["rendered"]["body"].startswith("{")
    assert len(Hook.calls) == 3  # dry run sent nothing


def test_once_per_run_and_code_rule_in_server(obs):
    c = obs.client
    rules = {r["name"]: r for r in c.rules()}
    assert rules["on_fail"]["source"] == "code" and rules["on_fail"]["do"][0]["kind"] == "call"
    counted = c.create_rule({"name": "count", "when": {"events": ["run.failed"], "flows": ["flaky"]}, "do": [{"kind": "set_state", "state_type": "Failed", "message": "noted"}]})
    run = start(obs, "flaky")
    obs.wait_run(run["id"])
    time.sleep(1.0)
    assert c._request("GET", f"/api/rules/{counted['id']}")["fire_count"] == 1
    failing = start(obs, "fail")
    obs.wait_run(failing["id"])
    wait_until(lambda: obs.hook.exists() and f"{failing['id']}:run.failed" in obs.hook.read_text(), timeout=15)
    with pytest.raises(ApiError) as info:
        c._request("PATCH", f"/api/rules/{rules['on_fail']['id']}", body={"name": "renamed"})
    assert info.value.status == 409
    toggled = c._request("PATCH", f"/api/rules/{rules['on_fail']['id']}", body={"enabled": False})
    assert toggled["enabled"] is False
    before = obs.hook.read_text()
    again = start(obs, "fail")
    obs.wait_run(again["id"])
    time.sleep(1.0)
    assert obs.hook.read_text() == before
    with pytest.raises(ApiError) as info:
        c.create_rule({"name": "bad", "when": {}, "do": []})
    assert info.value.status == 422
    with pytest.raises(ApiError) as info:
        c.create_rule({"name": "bad", "when": {}, "do": [{"kind": "call", "callable": "x"}]})
    assert info.value.status == 422


def test_rule_project_scope(obs):
    c = obs.client
    scoped = c.create_rule({"name": "scoped", "when": {"events": ["run.failed"], "project": "elsewhere"}, "do": [{"kind": "set_state", "state_type": "Failed", "message": "x"}]})
    run = start(obs, "fail")
    obs.wait_run(run["id"])
    time.sleep(0.8)
    assert c._request("GET", f"/api/rules/{scoped['id']}")["fire_count"] == 0
    ev = c.events(kind="run.failed", run_id=run["id"])[0]
    assert ev["payload"]["project"] == "obs"


def test_artifacts_api(obs):
    c = obs.client
    run = start(obs, "etl")
    obs.wait_run(run["id"])
    rows = wait_until(lambda: (a := c.artifacts(run["id"])) and len(a) == 2 and a, timeout=10)
    kinds = {r["kind"]: r for r in rows}
    assert kinds["progress"]["data"]["value"] == 60
    tasks = c.task_runs(run["id"])
    task_rows = c._request("GET", f"/api/task-runs/{tasks[0]['id']}/artifacts")
    assert task_rows[0]["kind"] == "table" and task_rows[0]["data"]["rows"] == [{"day": "2026-09-06"}]


def test_variables_api_and_secret_read_in_engine(obs):
    c = obs.client
    c._request("POST", "/api/variables", body={"name": "region", "value": "eu", "tags": ["infra"]})
    c._request("POST", "/api/variables", body={"name": "api_token", "value": "hunter2", "secret": True})
    listed = {v["name"]: v for v in c.variables()}
    assert listed["region"]["value"] == "eu" and listed["api_token"]["value"] == "********"
    assert c._request("GET", "/api/variables/api_token")["value"] == "********"
    raw = c._request("GET", "/api/variables/api_token", params={"raw": "true"})["raw"]
    assert raw.startswith("v1:")
    with pytest.raises(ApiError) as info:
        c._request("POST", "/api/variables", body={"name": "Bad Name", "value": 1})
    assert info.value.status == 422
    run = start(obs, "reads_secret")
    done = obs.wait_run(run["id"])
    assert done["state"]["type"] == "Completed"
    patched = c._request("PATCH", "/api/variables/region", body={"value": "us"})
    assert patched["value"] == "us"
    c._request("DELETE", "/api/variables/region")
    with pytest.raises(ApiError):
        c._request("GET", "/api/variables/region")
    assert c.settings()["secret_key_present"] is True
    os.remove(os.path.join(obs.home, "secret.key"))
    assert c.settings()["secret_key_missing"] is True


def test_settings_persist_and_retention(obs, obs_dir):
    c = obs.client
    s = c.settings()
    import cereyan

    assert s["retain_days"] == 30 and s["database_bytes"] > 0 and s["version"] == cereyan.__version__
    assert s["custom_routes"] == [] and s["catchup_default"] == "skip"
    patched = c._request("PATCH", "/api/settings", body={"retain_days": 7, "resources": {"db": 4}})
    assert patched["retain_days"] == 7 and patched["resources"]["db"]["total"] == 4.0
    toml_text = (obs_dir / "cereyan.toml").read_text()
    assert "retain_days = 7" in toml_text and "db = 4.0" in toml_text
    run = start(obs, "etl")
    obs.wait_run(run["id"])
    # Wait for the engine to finish reporting: a log line that lands after the update
    # below carries a fresh timestamp, and retention would rightly keep it forever.
    obs.wait_idle()
    # Age the run's logs and events beyond retention and wait for the pass.
    old = int((time.time() - 40 * 86400) * 1_000_000)
    db = sqlite3.connect(os.path.join(obs.home, "db.sqlite"))
    db.execute("UPDATE log SET timestamp = ?", (old,))
    db.execute("UPDATE event SET timestamp = ?", (old,))
    db.commit()
    db.close()
    wait_until(lambda: c.logs(run["id"])["items"] == [], timeout=15)
    assert c.get_run(run["id"])["state"]["type"] == "Completed"
    assert c.task_runs(run["id"])
    # Logs and events are aged together but not necessarily swept together, so wait
    # for the events the same way rather than assuming one pass cleared both. The
    # run itself and its task runs survive retention; only its logs and events go.
    wait_until(lambda: c.events(kind="run.*", limit=5) == [], timeout=15)


def test_unknown_config_key_warning(isolated_home, obs_dir):
    (obs_dir / "cereyan.toml").write_text("[server]\nprot = 4200\n")
    from cereyan import engine

    engine.close_store()
    srv = ServerProcess(str(isolated_home), str(obs_dir))
    try:
        assert "unknown key 'prot' in [server]" in srv.read_log()
    finally:
        srv.stop()


def test_email_action_over_local_smtp(obs):
    """A tiny SMTP responder proves the email action goes through lettre."""
    received = []

    def smtp_server(sock):
        conn, _ = sock.accept()
        f = conn.makefile("rb")
        conn.sendall(b"220 local ESMTP\r\n")
        data_mode = False
        while True:
            line = f.readline()
            if not line:
                break
            if data_mode:
                if line.strip() == b".":
                    data_mode = False
                    conn.sendall(b"250 OK\r\n")
                else:
                    received.append(line)
                continue
            cmd = line.split(b" ")[0].strip().upper()
            if cmd in (b"EHLO", b"HELO"):
                conn.sendall(b"250-local\r\n250 SIZE 1000000\r\n")
            elif cmd == b"DATA":
                data_mode = True
                conn.sendall(b"354 go\r\n")
            elif cmd == b"QUIT":
                conn.sendall(b"221 bye\r\n")
                break
            else:
                conn.sendall(b"250 OK\r\n")
        conn.close()

    sock = socket.socket()
    sock.bind(("127.0.0.1", 0))
    sock.listen(1)
    port = sock.getsockname()[1]
    threading.Thread(target=smtp_server, args=(sock,), daemon=True).start()
    obs.stop()
    from cereyan import engine

    engine.close_store()
    import pathlib

    pathlib.Path(obs.directory, "cereyan.toml").write_text(f'[email]\nhost = "127.0.0.1"\nport = {port}\ntls = "none"\nfrom = "cereyan@example.com"\n')
    srv = ServerProcess(obs.home, obs.directory, env={"RULE_HOOK_FILE": str(obs.hook)})
    try:
        c = srv.client
        assert c.settings()["email_configured"] is True
        rule = c.create_rule({"name": "mail", "when": {"events": ["run.failed"]}, "do": [{"kind": "email", "to": ["ops@example.com"]}]})
        run = c._request("POST", f"/api/flows/{fid(srv, 'fail')}/runs", body={"parameters": {}})
        srv.wait_run(run["id"])
        firing = wait_until(lambda: (f := c._request("GET", f"/api/rules/{rule['id']}/firings")) and f[0], timeout=20)
        assert firing["outcomes"][0]["status"] == "completed", firing
        body = b"".join(received).decode(errors="replace")
        assert "ValueError: bad" in body and "Subject:" in body
    finally:
        srv.stop()


def test_failure_streak_recovery_and_consecutive_guard(obs, webhook):
    c = obs.client
    rule = c._request("POST", "/api/rules", body={
        "name": "third strike", "when": {"events": ["run.failed"], "flows": ["maybe"]},
        "do": [{"kind": "webhook", "url": webhook, "body": "{{ payload.failures_in_a_row }}"}],
        "once": "never", "after_consecutive": 2,
    })
    failures = []
    for _ in range(3):
        run = start(obs, "maybe", ok=False)
        obs.wait_run(run["id"])
        failures.append(run["id"])
    ok = start(obs, "maybe", ok=True)
    obs.wait_run(ok["id"])
    failed = [e for e in c.events(kind="run.failed") if e["run_id"] in failures]
    assert sorted(e["payload"]["failures_in_a_row"] for e in failed) == [1, 2, 3]
    recovered = [e for e in c.events(kind="flow.recovered") if e["run_id"] == ok["id"]]
    assert len(recovered) == 1
    assert {k: recovered[0]["payload"][k] for k in ("flow", "project", "failures", "run_id")} == {"flow": "maybe", "project": "obs", "failures": 3, "run_id": ok["id"]}
    firings = wait_until(lambda: (lambda f: f if len(f) == 2 else None)(c._request("GET", f"/api/rules/{rule['id']}/firings")))
    assert {f["run_id"] for f in firings} == set(failures[1:])
    wait_until(lambda: all(o["status"] == "completed" for f in c._request("GET", f"/api/rules/{rule['id']}/firings") for o in f["outcomes"]))
    assert sorted(b.decode() for b in Hook.calls) == ["2", "3"]
    again = start(obs, "maybe", ok=True)
    obs.wait_run(again["id"])
    assert not [e for e in c.events(kind="flow.recovered") if e["run_id"] == again["id"]]


def test_webhook_presets_signature_and_run_url(obs, webhook):
    import base64
    import hashlib
    import hmac

    c = obs.client
    c._request("POST", "/api/rules", body={
        "name": "slack", "when": {"events": ["run.failed"], "flows": ["maybe"]},
        "do": [{"kind": "webhook", "url": webhook, "preset": "slack", "secret": "s3cret"}],
    })
    c._request("POST", "/api/rules", body={
        "name": "pager", "when": {"events": ["run.failed", "flow.recovered"], "flows": ["maybe"]},
        "do": [{"kind": "webhook", "url": webhook, "preset": "pagerduty", "routing_key": "rk-1"}],
        "once": "never",
    })
    with pytest.raises(Exception):
        c._request("POST", "/api/rules", body={
            "name": "bad", "when": {"events": ["run.failed"]},
            "do": [{"kind": "webhook", "url": webhook, "preset": "ntfy"}],
        })
    failed = start(obs, "maybe", ok=False)
    obs.wait_run(failed["id"])
    wait_until(lambda: len(Hook.calls) >= 2)
    bodies = [json.loads(b) for b in Hook.calls]
    slack = next(b for b in bodies if "text" in b)
    assert "maybe: run.failed" in slack["text"] and "bad day" in slack["text"]
    assert f"{obs.info['url']}/runs/{failed['id']}" in slack["text"]
    index = next(i for i, h in enumerate(Hook.headers_seen) if "webhook-signature" in h)
    signed, body = Hook.headers_seen[index], Hook.calls[index]
    expected = hmac.new(b"s3cret", f"{signed['webhook-id']}.{signed['webhook-timestamp']}.".encode() + body, hashlib.sha256).digest()
    assert signed["webhook-signature"] == "v1," + base64.b64encode(expected).decode()
    trigger = next(b for b in bodies if b.get("event_action") == "trigger")
    assert trigger["routing_key"] == "rk-1" and trigger["dedup_key"] == "cereyan-obs-maybe"
    assert trigger["payload"]["custom_details"]["run_url"].endswith(f"/runs/{failed['id']}")
    ok = start(obs, "maybe", ok=True)
    obs.wait_run(ok["id"])
    wait_until(lambda: any(json.loads(b).get("event_action") == "resolve" for b in Hook.calls))
    resolve = next(json.loads(b) for b in Hook.calls if json.loads(b).get("event_action") == "resolve")
    assert resolve["dedup_key"] == "cereyan-obs-maybe"


def test_backfill_completed_event(obs):
    c = obs.client
    made = c._request("POST", f"/api/flows/{fid(obs, 'etl')}/backfill", body={"parameter": "day", "start": "2026-03-01", "end": "2026-03-02", "concurrency": 2})
    bf = made.get("backfill", made)["id"]
    wait_until(lambda: (lambda items: items and all(r["state"]["type"] in ("Completed", "Failed", "Cancelled", "Crashed") for r in items))(c.runs(backfill_id=bf, limit=50)["items"]), timeout=60)
    events = wait_until(lambda: [e for e in c.events(kind="backfill.completed") if e["payload"]["backfill_id"] == bf])
    assert len(events) == 1 and sum(events[0]["payload"]["counts"].values()) == 2


def test_run_url_uses_public_url(isolated_home, obs_dir, webhook):
    from cereyan import engine

    engine.close_store()
    srv = ServerProcess(str(isolated_home), str(obs_dir), env={"CEREYAN_PUBLIC_URL": "https://cereyan.example.com/"})
    try:
        c = srv.client
        c._request("POST", "/api/rules", body={
            "name": "link", "when": {"events": ["run.completed"], "flows": ["etl"]},
            "do": [{"kind": "webhook", "url": webhook, "body": "{{ run.url }}"}],
        })
        run = c._request("POST", f"/api/flows/{fid(srv, 'etl')}/runs", body={"parameters": {}})
        srv.wait_run(run["id"])
        wait_until(lambda: Hook.calls)
        assert Hook.calls[0].decode() == f"https://cereyan.example.com/runs/{run['id']}"
        env = {(e["table"], e["key"]): e for e in c._request("GET", "/api/settings/environment")["configuration"]}
        assert env["server", "public_url"]["value"] == "https://cereyan.example.com"
    finally:
        srv.stop()


def test_cancel_runs_selects_by_parameter_and_spares_the_events_run(obs):
    c = obs.client
    rule = c._request("POST", "/api/rules", body={
        "name": "stop the order", "when": {"events": ["orders.cancelled"]},
        "do": [
            {"kind": "cancel_runs", "flow": "fulfil", "parameters": {"order": "{{ payload.order }}"}},
            {"kind": "cancel_runs"},
        ],
        "once": "never",
    })
    with pytest.raises(Exception):
        c._request("POST", "/api/rules", body={
            "name": "bad states", "when": {"events": ["orders.cancelled"]},
            "do": [{"kind": "cancel_runs", "states": ["Completed"]}],
        })
    a1 = [start(obs, "fulfil", order="A1") for _ in range(2)]
    b2 = start(obs, "fulfil", order="B2")
    for r in a1 + [b2]:
        obs.wait_run(r["id"], until=lambda run: run["state"]["type"] == "Running")
    announce = start(obs, "cancel_order", order="A1")
    for r in a1:
        obs.wait_run(r["id"])
    assert {c.get_run(r["id"])["state"]["type"] for r in a1} == {"Cancelled"}
    assert c.get_run(b2["id"])["state"]["type"] == "Running"
    # The second action had no flow: it selects the event's own flow but never the event's own run.
    assert obs.wait_run(announce["id"])["state"]["type"] == "Completed"
    firing = wait_until(lambda: (f := c._request("GET", f"/api/rules/{rule['id']}/firings")) and f[0])
    outcomes = {o["index"]: o for o in firing["outcomes"]}
    assert sorted(outcomes[0]["detail"]["cancelled"]) == sorted(r["id"] for r in a1)
    assert outcomes[1]["detail"]["cancelled"] == []
    c.cancel(b2["id"])
    obs.wait_run(b2["id"])


def test_maintenance_window_suppresses_rule_actions(obs, webhook):
    c = obs.client
    rule = c._request("POST", "/api/rules", body={
        "name": "page", "when": {"events": ["run.failed"], "flows": ["fail"]},
        "do": [{"kind": "webhook", "url": webhook}],
        "once": "never",
    })
    status = c.pause_scheduler(reason="db upgrade", suppress_rules=True)
    assert status["paused"] and status["reason"] == "db upgrade" and status["suppress_rules"]
    quiet = start(obs, "fail")
    obs.wait_run(quiet["id"])
    firing = wait_until(lambda: (f := c._request("GET", f"/api/rules/{rule['id']}/firings")) and f[0])
    assert firing["outcomes"] == [{"index": 0, "kind": "webhook", "status": "suppressed"}]
    assert not any(e["run_id"] == quiet["id"] for e in c.events(kind="rule.fired"))
    assert Hook.calls == []
    assert c.resume_scheduler()["paused"] is False
    loud = start(obs, "fail")
    obs.wait_run(loud["id"])
    wait_until(lambda: len(Hook.calls) == 1)
    firings = c._request("GET", f"/api/rules/{rule['id']}/firings")
    assert {f["run_id"] for f in firings} == {quiet["id"], loud["id"]}
