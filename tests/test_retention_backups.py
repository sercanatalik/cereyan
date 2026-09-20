"""M1.4 run retention and backups: expired runs go, the newest and the live stay,
copies are scheduled, on demand, and written before a migration."""

from __future__ import annotations

import json
import os
import sqlite3
import subprocess
import sys
import time

import pytest

from cereyan.client import Client
from server_helpers import ServerProcess

DAY = 86_400 * 1_000_000

PIPELINE = '''
import time
from cereyan import App, task

app = App("keep")

@task
def step(n: int):
    return n

@app.flow
def quick(n: int = 1):
    step(n)

@app.flow
def boom():
    raise ValueError("kaboom")

@app.flow
def sleepy(seconds: float = 30.0):
    time.sleep(seconds)
'''


@pytest.fixture
def srv(isolated_home, tmp_path):
    from cereyan import engine

    engine.close_store()
    d = tmp_path / "keep"
    d.mkdir()
    (d / "pipeline.py").write_text(PIPELINE)
    server = ServerProcess(str(isolated_home), str(d), env={"CEREYAN_RETENTION_INTERVAL": "2"})
    server.client = Client(server.info["url"])
    server.project_dir = d
    try:
        yield server
    finally:
        server.stop()


def age(home, run_ids, days):
    old = int(time.time() * 1_000_000) - days * DAY
    db = sqlite3.connect(os.path.join(str(home), "db.sqlite"))
    marks = ",".join("?" for _ in run_ids)
    db.execute(f"UPDATE run SET created_at = ?, end_time = CASE WHEN end_time IS NULL THEN NULL ELSE ? END, state_timestamp = ? WHERE id IN ({marks})", (old, old, old, *run_ids))
    db.commit()
    db.close()


def run_ids(client):
    return {r["id"] for r in client.runs(limit=200)["items"]}


def wait_for(predicate, timeout=15.0):
    deadline = time.time() + timeout
    while time.time() < deadline:
        if predicate():
            return True
        time.sleep(0.2)
    return False


def test_run_retention_keeps_newest_failed_and_running(srv, isolated_home):
    c = srv.client
    flows = {f["name"]: f["id"] for f in c.flows()}
    done = [c.run("quick", n=i) for i in range(3)]
    for r in done:
        srv.wait_run(r["id"])
    failed = c.run("boom")
    srv.wait_run(failed["id"])
    running = c.run("sleepy", seconds=60)
    srv.wait_run(running["id"], until=lambda r: r["state"]["type"] == "Running")
    age(isolated_home, [r["id"] for r in done] + [failed["id"], running["id"]], 10)
    before = c.counts()
    # Off by default: nothing happens even with old runs.
    time.sleep(3)
    assert run_ids(c) == {r["id"] for r in done} | {failed["id"], running["id"]}

    saved = c._request("PATCH", "/api/settings", body={"retain_runs_days": 1, "retain_failed_runs_days": 30, "keep_last_runs_per_flow": 1})
    assert (saved["retain_runs_days"], saved["retain_failed_runs_days"], saved["keep_last_runs_per_flow"]) == (1, 30, 1)
    toml = (srv.project_dir / "cereyan.toml").read_text()
    assert "retain_runs_days = 1" in toml and "keep_last_runs_per_flow = 1" in toml
    assert wait_for(lambda: run_ids(c) == {done[-1]["id"], failed["id"], running["id"]}, timeout=20), run_ids(c)
    # The survivor keeps its task runs; the deleted ones took theirs along.
    assert c.task_runs(done[-1]["id"])
    db = sqlite3.connect(os.path.join(str(isolated_home), "db.sqlite"))
    assert db.execute("SELECT COUNT(*) FROM task_run WHERE run_id IN (?, ?)", (done[0]["id"], done[1]["id"])).fetchone()[0] == 0
    db.close()
    # Counts moved with the deletion.
    after = c.counts()
    assert after["runs"]["Completed"] == before["runs"]["Completed"] - 2
    assert after["runs"]["Failed"] == before["runs"]["Failed"]
    # A negative value is refused.
    with pytest.raises(Exception):
        c._request("PATCH", "/api/settings", body={"retain_runs_days": -1})
    c.cancel(running["id"])
    srv.wait_run(running["id"])
    env = {(e["table"], e["key"]): e for e in c._request("GET", "/api/settings/environment")["configuration"]}
    assert env["defaults", "retain_runs_days"]["source"] == "settings" and env["defaults", "backup_keep"]["value"] == 7
    assert flows["quick"]


def test_backups_scheduled_on_demand_and_cli(srv, isolated_home, run_cli):
    c = srv.client
    c.run("quick", n=1)
    home = str(isolated_home)
    backups = os.path.join(home, "backups")
    assert c._request("GET", "/api/database")["backups"] == 0
    # On demand through the API.
    made = c._request("POST", "/api/database/backup")
    assert made["path"].startswith(backups) and made["backups"] == 1
    copy = sqlite3.connect(made["path"])
    assert copy.execute("SELECT COUNT(*) FROM run").fetchone()[0] >= 1
    copy.close()
    settings = c.settings()
    assert settings["last_backup_path"] == made["path"] and settings["last_backup_at"] and settings["backups"] == 1
    # Through the CLI, which finds the server.
    result = run_cli("backup", "--json")
    assert result.returncode == 0, result.stderr
    second = json.loads(result.stdout)["path"]
    assert second != made["path"] and os.path.exists(second)
    # Pruning keeps the newest.
    c._request("PATCH", "/api/settings", body={"backup_keep": 1})
    third = c._request("POST", "/api/database/backup")
    names = sorted(os.listdir(backups))
    assert names == [os.path.basename(third["path"])], names
    # A pre-migration copy in the same directory is never pruned.
    with open(os.path.join(backups, "pre-migration-v9-20260101-000000.sqlite"), "wb"):
        pass
    c._request("POST", "/api/database/backup")
    assert "pre-migration-v9-20260101-000000.sqlite" in os.listdir(backups)
    assert len([n for n in os.listdir(backups) if n.startswith("db-")]) == 1
    # Scheduled: with backup_every set and the last copy made just now, nothing
    # new appears; with the last-backup marker cleared, the next pass writes one.
    c._request("PATCH", "/api/settings", body={"backup_every": 1, "backup_keep": 5})
    time.sleep(3)
    assert len([n for n in os.listdir(backups) if n.startswith("db-")]) == 1
    db = sqlite3.connect(os.path.join(home, "db.sqlite"))
    db.execute("DELETE FROM kv WHERE key = 'backup.last_at'")
    db.commit()
    db.close()
    assert wait_for(lambda: len([n for n in os.listdir(backups) if n.startswith("db-")]) == 2, timeout=10)


def test_cli_backup_without_a_server(isolated_home, run_cli, write_module):
    path = write_module("solo", PIPELINE)
    env = dict(os.environ, CEREYAN_HOME=str(isolated_home))
    proc = subprocess.run([sys.executable, "-m", "cereyan", "run", f"{path}:quick"], env=env, capture_output=True, text=True)
    assert proc.returncode == 0, proc.stderr
    result = run_cli("backup")
    assert result.returncode == 0, result.stderr
    copy = result.stdout.strip()
    assert copy.startswith(os.path.join(str(isolated_home), "backups"))
    db = sqlite3.connect(copy)
    assert db.execute("SELECT COUNT(*) FROM run").fetchone()[0] == 1
    db.close()


def test_pre_migration_copy_is_written_before_upgrading(isolated_home, run_cli, write_module):
    path = write_module("mig", PIPELINE)
    env = dict(os.environ, CEREYAN_HOME=str(isolated_home))
    subprocess.run([sys.executable, "-m", "cereyan", "run", f"{path}:quick"], env=env, capture_output=True, text=True, check=True)
    db_path = os.path.join(str(isolated_home), "db.sqlite")
    db = sqlite3.connect(db_path)
    (version,) = db.execute("PRAGMA user_version").fetchone()
    # Pretend the store is one schema behind; the last migration is idempotent enough to re-run.
    db.execute(f"PRAGMA user_version = {version - 1}")
    db.commit()
    db.close()
    result = run_cli("runs", "ls")
    backups = os.path.join(str(isolated_home), "backups")
    copies = [n for n in os.listdir(backups)] if os.path.isdir(backups) else []
    assert any(n.startswith(f"pre-migration-v{version - 1}-") for n in copies), (result.stdout, result.stderr, copies)
    assert "pre-migration" in result.stderr
    db = sqlite3.connect(db_path)
    assert db.execute("PRAGMA user_version").fetchone()[0] == version
    db.close()
