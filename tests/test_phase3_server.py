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
def fail():
    raise ValueError("bad")

@app.flow(retries=1, retry_delay=0)
def flaky():
    raise ValueError("again")

@app.flow
def loop_a():
    return "a"

@app.flow
def reads_secret():
    return Variable.get("api_token")

HOOK = os.environ.get("RULE_HOOK_FILE")

@app.rule(on="run.failed", flow="fail")
def on_fail(event, run):
    with open(HOOK, "a") as fh:
        fh.write(f"{run['id']}:{event['name']}\\n")
    return {"seen": run["id"]}
'''


class Hook(http.server.BaseHTTPRequestHandler):
    calls = []
    fail_first = 0

    def do_POST(self):
        length = int(self.headers.get("content-length", 0))
        body = self.rfile.read(length)
        Hook.calls.append(body)
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
    firing = wait_until(lambda: (f := c._request("GET", f"/api/rules/{rule['id']}/firings")) and f[0], timeout=20)
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
    assert c.events(kind="run.*", limit=5) == []


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
