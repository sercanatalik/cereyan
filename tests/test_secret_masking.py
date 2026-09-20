"""M1.9 secrets read with Variable.get are masked in log rows, captured prints,
exception text, and failure records, offline and served."""

from __future__ import annotations

import json
import os
import subprocess
import sys

from cereyan import masking
from cereyan.client import Client
from server_helpers import ServerProcess

TOKEN = "hunter2-token"

PIPELINE = '''
from cereyan import App, Variable, get_run_logger

app = App("mask")

@app.flow(log_prints=True)
def leaky(fail: bool = False):
    token = Variable.get("token")
    get_run_logger().info("token is %s", token)
    print(f"printed {token}")
    if fail:
        raise RuntimeError(f"bad {token}")

@app.flow
def short():
    get_run_logger().info("pin %s", Variable.get("pin"))

@app.flow
def nested():
    creds = Variable.get("creds")
    get_run_logger().info("user %s pass %s port %s", creds["user"], creds["pass"], creds["port"])
'''


def test_mask_registry_orders_longest_first_and_skips_short():
    masking.clear()
    try:
        masking.register("abc")
        masking.register({"user": "alice-user", "pass": "p@ss-word-1", "port": 5432, "flag": True})
        masking.register("alice")
        assert masking.mask("alice-user:p@ss-word-1@db:5432 abc alice") == "***:***@db:*** abc ***"
        assert masking.active()
    finally:
        masking.clear()
    assert masking.mask("alice-user") == "alice-user"


def set_secrets(env):
    code = (
        "from cereyan import Variable; "
        f"Variable.set('token', {TOKEN!r}, secret=True); "
        "Variable.set('pin', 'ab1', secret=True); "
        "Variable.set('creds', {'user': 'alice-user', 'pass': 'p@ss-word-1', 'port': 5432}, secret=True)"
    )
    subprocess.run([sys.executable, "-c", code], env=env, check=True, capture_output=True, text=True)


def test_offline_logs_and_failure_are_masked(isolated_home, write_module, run_cli):
    from cereyan import engine

    engine.close_store()
    path = write_module("mask", PIPELINE)
    env = dict(os.environ, CEREYAN_HOME=str(isolated_home))
    set_secrets(env)
    ok = run_cli("run", f"{path}:leaky")
    assert ok.returncode == 0, ok.stderr
    # The engine's own terminal keeps the plain text; the store does not.
    assert TOKEN in ok.stdout + ok.stderr
    failed = run_cli("run", f"{path}:leaky", "--param", "fail=true")
    assert failed.returncode == 1
    run_cli("run", f"{path}:short")
    run_cli("run", f"{path}:nested")
    store = engine.get_store()
    try:
        runs = json.loads(store.list_runs(json.dumps({"limit": 10})))["items"]
        by_flow = {}
        for r in runs:
            by_flow.setdefault(r["flow_name"], []).append(r)
        texts = {}
        for name, items in by_flow.items():
            for r in items:
                logs = json.loads(store.query_logs(json.dumps({"run_id": r["id"], "limit": 100})))["items"]
                texts.setdefault(name, []).append("\n".join(l["message"] for l in logs))
        leaky_logs = "\n".join(texts["leaky"])
        assert "token is ***" in leaky_logs and "printed ***" in leaky_logs and TOKEN not in leaky_logs
        assert "pin ab1" in "\n".join(texts["short"])
        nested_logs = "\n".join(texts["nested"])
        assert "user *** pass *** port ***" in nested_logs and "alice-user" not in nested_logs
        failed_run = next(r for r in by_flow["leaky"] if r["state"]["type"] == "Failed")
        assert failed_run["state"]["message"] == "RuntimeError: bad ***"
        assert TOKEN not in failed_run["state"]["details"]["traceback"]
        assert "bad ***" in failed_run["state"]["details"]["traceback"]
    finally:
        engine.close_store()


def test_served_logs_and_failure_are_masked(isolated_home, tmp_path):
    from cereyan import Variable, engine

    engine.close_store()
    d = tmp_path / "mask"
    d.mkdir()
    (d / "pipeline.py").write_text(PIPELINE)
    srv = ServerProcess(str(isolated_home), str(d))
    c = Client(srv.info["url"])
    try:
        Variable.set("token", TOKEN, secret=True)
        Variable.set("creds", {"user": "alice-user", "pass": "p@ss-word-1", "port": 5432}, secret=True)
        run = c.run("leaky", fail=True)
        done = srv.wait_run(run["id"])
        assert done["state"]["type"] == "Failed" and done["state"]["message"] == "RuntimeError: bad ***"
        assert TOKEN not in json.dumps(done["state"]["details"])
        lines = [l["message"] for l in c.logs(run["id"])["items"]]
        assert any(l == "token is ***" for l in lines) and any(l == "printed ***" for l in lines)
        assert TOKEN not in "\n".join(lines)
        # A run that leaks nothing new in the same warm engine is still masked.
        again = c.run("nested")
        srv.wait_run(again["id"])
        text = "\n".join(l["message"] for l in c.logs(again["id"])["items"])
        assert "user *** pass *** port ***" in text
    finally:
        srv.stop()
