"""The Environment and Data tabs through a real server: settings with their
sources, the process environment with secrets hidden, removing a project,
and resetting the database."""

import http.client
import json
import sqlite3
import threading
import time
from urllib.parse import urlparse

import pytest
from cereyan.client import ApiError, Client
from server_helpers import WINDOWS, ServerProcess

TOKEN = "s3cret-token-value"

ALPHA = '''
import time
from cereyan import App

app = App("alpha")

@app.flow
def load() -> int:
    return 1

@app.flow
def sleepy(seconds: float = 30.0):
    time.sleep(seconds)
'''

BETA = '''
import time
from cereyan import App, Interval

app = App("beta")

@app.flow(schedule=Interval(3600))
def hourly() -> int:
    return 1

@app.flow
def stubborn(seconds: float = 60.0):
    # Ignores cooperative cancellation; SIGTERM still ends the process.
    end = time.time() + seconds
    while time.time() < end:
        try:
            time.sleep(0.1)
        except KeyboardInterrupt:
            pass

@app.rule(on="run.failed", flow="hourly")
def on_fail(event, run):
    return None
'''

UI_RULE = {
    "name": "alpha-failures",
    "when": {"events": ["run.failed"], "flows": ["load"], "project": "alpha"},
    "do": [{"kind": "set_state", "state_type": "Failed", "message": "noted"}],
}


def project_dir(tmp_path, name: str, source: str, toml: str | None = None):
    d = tmp_path / name
    d.mkdir()
    (d / "pipeline.py").write_text(source)
    if toml is not None:
        (d / "cereyan.toml").write_text(toml)
    return d


def serve(home, directory, env=None):
    from cereyan import engine

    engine.close_store()
    return ServerProcess(str(home), str(directory), env=env)


def status_of(call) -> int:
    with pytest.raises(ApiError) as err:
        call()
    return err.value.status


def leave_stale_alpha(home, tmp_path):
    """Serve project alpha once and run a flow, so the next server sees it stale."""
    alpha = project_dir(tmp_path, "alpha", ALPHA)
    srv = serve(home, alpha)
    try:
        run = srv.client.run("load", project="alpha")
        srv.wait_run(run["id"])
        srv.wait_idle()
    finally:
        srv.stop()
    return alpha


def test_environment_names_sources_and_hides_secrets(isolated_home, tmp_path, monkeypatch):
    monkeypatch.setenv("CEREYAN_TOKEN", TOKEN)
    beta = project_dir(
        tmp_path, "beta", BETA,
        '# ops server\n[server]\n# token = "old-token"\n[defaults]\nretain_days = 9\n'
        '[email]\nhost = "smtp.internal"\nfrom = "ops@example.com"\npassword = "mail-secret"\n',
    )
    srv = serve(isolated_home, beta, env={
        "CEREYAN_HOST": "127.0.0.1",
        "CEREYAN_AUTH_SCOPE": "api",
        "AWS_SECRET_ACCESS_KEY": "aws-hidden-value",
        "DATABASE_URL": "postgres://etl:pw-in-url@db.internal:5432/warehouse",
        "DEPLOY_NOTE": TOKEN,
    })
    try:
        c = srv.client
        env = c._request("GET", "/api/settings/environment")
        text = json.dumps(env, ensure_ascii=False)
        for secret in (TOKEN, "aws-hidden-value", "pw-in-url", "mail-secret", "old-token"):
            assert secret not in text

        entries = {(e["table"], e["key"]): e for e in env["configuration"]}
        assert (entries["server", "port"]["source"], entries["server", "port"]["source_name"]) == ("flag", "--port")
        assert entries["server", "port"]["value"] == urlparse(srv.info["url"]).port
        assert (entries["server", "host"]["source"], entries["server", "host"]["source_name"]) == ("env", "CEREYAN_HOST")
        assert entries["server", "open_browser"]["source_name"] == "CEREYAN_NO_BROWSER"
        token = entries["server", "token"]
        assert (token["value"], token["secret"], token["source"], token["source_name"]) == (None, True, "env", "CEREYAN_TOKEN")
        assert (entries["defaults", "retain_days"]["value"], entries["defaults", "retain_days"]["source"]) == (9, "toml")
        assert entries["defaults", "catchup"]["source"] == "default"
        assert (entries["server", "socket"]["value"], entries["server", "socket"]["source"]) == (None, "default")
        assert entries["email", "password"]["secret"] is True
        assert entries["email", "host"]["value"] == "smtp.internal"

        variables = {v["name"]: v for v in env["variables"]}
        assert variables["AWS_SECRET_ACCESS_KEY"] == {"name": "AWS_SECRET_ACCESS_KEY", "value": None, "hidden": True}
        assert variables["DEPLOY_NOTE"]["hidden"] is True
        assert variables["DATABASE_URL"]["value"] == "postgres://etl:••••••@db.internal:5432/warehouse"
        assert variables["CEREYAN_AUTH_SCOPE"] == {"name": "CEREYAN_AUTH_SCOPE", "value": "api", "hidden": False}
        assert variables["CEREYAN_TOKEN"]["hidden"] is True
        assert env["variables"][0]["name"].startswith("CEREYAN_")
        assert "CEREYAN_PORT" in env["cereyan_unset"] and "CEREYAN_HOME" not in env["cereyan_unset"]

        assert env["runtime"]["config_file"] == str(beta / "cereyan.toml")
        assert env["runtime"]["python_version"]
        assert env["cereyan_toml"].startswith("# ops server\n[server]\n")
        assert '# token = "••••••••"' in env["cereyan_toml"]
        assert 'password = "••••••••"' in env["cereyan_toml"]

        # An edit in Settings becomes the source. Saving rewrites cereyan.toml, so
        # this comes after the file checks above.
        c._request("PATCH", "/api/settings", body={"crash_retries": 2})
        edited = {(e["table"], e["key"]): e for e in c._request("GET", "/api/settings/environment")["configuration"]}
        assert (edited["defaults", "crash_retries"]["value"], edited["defaults", "crash_retries"]["source"]) == (2, "settings")
    finally:
        srv.stop()


def test_stale_project_is_removed_and_the_served_one_refused(isolated_home, tmp_path):
    leave_stale_alpha(isolated_home, tmp_path)
    srv = serve(isolated_home, project_dir(tmp_path, "beta", BETA))
    try:
        c = srv.client
        c.create_rule(UI_RULE)
        c._request("POST", "/api/variables", body={"name": "region", "value": "eu"})

        projects = c._request("GET", "/api/projects")
        assert [(p["name"], p["served"]) for p in projects] == [("beta", True), ("alpha", False)]
        assert (projects[1]["flows"], projects[1]["live_flows"], projects[1]["runs"]) == (2, 0, 1)

        preview = c._request("GET", "/api/projects/alpha")
        assert (preview["flows"], preview["runs"], preview["matching_rules"], preview["served"]) == (2, 1, 1, False)
        assert preview["events"] >= 1 and preview["active_runs"] == 0
        assert status_of(lambda: c._request("GET", "/api/projects/nope")) == 404

        with pytest.raises(ApiError) as err:
            c._request("DELETE", "/api/projects/beta")
        assert err.value.status == 409 and err.value.body["reason"] == "served"

        deleted = c._request("DELETE", "/api/projects/alpha")
        assert (deleted["flows"], deleted["runs"]) == (2, 1)
        assert c.flows(project="alpha") == []
        assert c._request("GET", "/api/runs", params={"project": "alpha"})["items"] == []
        assert not any("alpha/" in json.dumps(e) for e in c.events(limit=1000))
        assert [p["name"] for p in c._request("GET", "/api/projects")] == ["beta"]
        assert any(r["name"] == "alpha-failures" for r in c.rules())
        assert [v["name"] for v in c.variables()] == ["region"]
    finally:
        srv.stop()


def test_project_with_a_run_in_progress_is_refused(isolated_home, tmp_path):
    alpha = project_dir(tmp_path, "alpha", ALPHA)
    srv = serve(isolated_home, project_dir(tmp_path, "beta", BETA))
    try:
        c = srv.client
        # Handed off without a parameter schema, so the flow runs with its default 30 s.
        run = c.submit("alpha", "sleepy", module="pipeline", source_dir=str(alpha))
        srv.wait_run(run["id"], until=lambda r: r["state"]["type"] == "Running")
        with pytest.raises(ApiError) as err:
            c._request("DELETE", "/api/projects/alpha")
        assert err.value.status == 409
        assert (err.value.body["reason"], err.value.body["active_runs"]) == ("active_runs", 1)
        c.cancel(run["id"])
        srv.wait_run(run["id"])
        assert c._request("DELETE", "/api/projects/alpha")["runs"] == 1
    finally:
        srv.stop()


def test_history_reset_keeps_definitions(isolated_home, tmp_path):
    srv = serve(isolated_home, project_dir(tmp_path, "beta", BETA))
    try:
        c = srv.client
        run = c.run("hourly", project="beta")
        srv.wait_run(run["id"])
        srv.wait_idle()
        c.create_rule(UI_RULE)
        c._request("POST", "/api/variables", body={"name": "region", "value": "eu"})
        rules = sorted(r["name"] for r in c.rules())

        assert status_of(lambda: c._request("POST", "/api/database/reset", body={})) == 422
        assert status_of(lambda: c._request("POST", "/api/database/reset", body={"scope": "all"})) == 422
        assert c._request("GET", "/api/database")["counts"]["runs"] >= 1

        started = time.time()
        result = c._request("POST", "/api/database/reset", body={"scope": "history"})
        assert result["scope"] == "history" and result["deleted"]["runs"] >= 1
        with sqlite3.connect(result["backup_path"]) as copy:
            done = copy.execute("SELECT COUNT(*) FROM run WHERE state_type = 'Completed'").fetchone()[0]
        assert done == 1
        assert result["backup_path"].startswith(str(isolated_home / "backups"))

        items = c._request("GET", "/api/runs")["items"]
        assert all(r["state"]["type"] == "Scheduled" for r in items)
        # The hourly schedule is armed again from now: nothing for fires before the reset.
        assert items and all(r["scheduled_time"] >= started * 1_000_000 for r in items)
        assert sorted(r["name"] for r in c.rules()) == rules
        assert all(r["fire_count"] == 0 for r in c.rules())
        assert [v["name"] for v in c.variables()] == ["region"]
    finally:
        srv.stop()


def test_everything_reset_keeps_what_code_registered(isolated_home, tmp_path):
    leave_stale_alpha(isolated_home, tmp_path)
    srv = serve(isolated_home, project_dir(tmp_path, "beta", BETA))
    try:
        c = srv.client
        hourly = next(f for f in c.flows(project="beta") if f["name"] == "hourly")
        c.create_rule(UI_RULE)
        c._request("POST", f"/api/flows/{hourly['id']}/schedules", body={"kind": "cron", "cron": "0 3 1 1 *"})
        c._request("POST", "/api/variables", body={"name": "region", "value": "eu"})

        result = c._request("POST", "/api/database/reset", body={"scope": "everything", "backup": False})
        assert result["backup_path"] is None
        deleted = result["deleted"]
        assert (deleted["flows"], deleted["rules"], deleted["variables"]) == (2, 1, 1)

        assert sorted(f["name"] for f in c.flows()) == ["hourly", "stubborn"]
        assert [r["name"] for r in c.rules()] == ["on_fail"] or all(r["source"] == "code" for r in c.rules())
        assert all(s["source"] == "code" for s in c.schedules(hourly["id"]))
        assert c.variables() == []
        assert [p["name"] for p in c._request("GET", "/api/projects")] == ["beta"]
    finally:
        srv.stop()


def test_failed_copy_deletes_nothing(isolated_home, tmp_path):
    srv = serve(isolated_home, project_dir(tmp_path, "beta", BETA))
    try:
        c = srv.client
        run = c.run("hourly", project="beta")
        srv.wait_run(run["id"])
        (isolated_home / "backups").write_text("a file where the directory should be")
        assert status_of(lambda: c._request("POST", "/api/database/reset", body={"scope": "history"})) == 500
        assert c.get_run(run["id"])["state"]["type"] == "Completed"
        # The failed attempt leaves the server able to create runs.
        assert c.run("hourly", project="beta")["id"]
    finally:
        srv.stop()


@pytest.mark.skipif(WINDOWS, reason="the stubborn flow relies on SIGTERM")
def test_reset_announces_itself_and_refuses_new_runs_meanwhile(isolated_home, tmp_path):
    beta = project_dir(tmp_path, "beta", BETA, "[server]\ncancel_grace_secs = 1\n")
    srv = serve(isolated_home, beta)
    try:
        c = srv.client
        busy = c.run("stubborn", project="beta", seconds=60)
        srv.wait_run(busy["id"], until=lambda r: r["state"]["type"] == "Running")

        u = urlparse(srv.info["url"])
        conn = http.client.HTTPConnection(u.hostname, u.port, timeout=30)
        conn.request("GET", "/api/stream", headers={"accept": "text/event-stream"})
        stream = conn.getresponse()

        results: list = []
        resetter = Client(srv.info["url"])
        worker = threading.Thread(
            target=lambda: results.append(
                resetter._request("POST", "/api/database/reset", body={"scope": "history", "backup": False})
            )
        )
        worker.start()
        refused = False
        deadline = time.time() + 20
        while worker.is_alive() and time.time() < deadline and not refused:
            try:
                c.run("hourly", project="beta")
            except ApiError as exc:
                refused = exc.status == 503
            time.sleep(0.05)
        worker.join(30)
        assert refused, "no run creation was refused while the reset ran"
        assert results and results[0]["scope"] == "history"

        seen = []
        deadline = time.time() + 10
        while time.time() < deadline and "database.reset" not in seen:
            line = stream.readline().decode()
            if line.startswith("event:"):
                seen.append(line.split(":", 1)[1].strip())
        conn.close()
        assert seen.count("database.reset") == 1
        assert status_of(lambda: c.get_run(busy["id"])) == 404
    finally:
        srv.stop()
