"""1.1 API token: resolution, middleware, clients, engines, and the CLI."""

from __future__ import annotations

import json
import os
import subprocess
import sys
import urllib.error
import urllib.request

import pytest

from cereyan.client import ApiError, AuthRequired, Client, default_client
from server_helpers import ServerProcess

TOKEN = "s3cret-token"


@pytest.fixture
def auth_server(isolated_home, project_dir):
    from cereyan import engine

    engine.close_store()
    srv = ServerProcess(str(isolated_home), str(project_dir), env={"CEREYAN_TOKEN": TOKEN})
    try:
        yield srv
    finally:
        srv.stop()


def raw_get(url, headers=None):
    req = urllib.request.Request(url, headers=headers or {})
    try:
        with urllib.request.urlopen(req, timeout=5) as resp:
            return resp.status, json.loads(resp.read() or b"null")
    except urllib.error.HTTPError as exc:
        return exc.code, json.loads(exc.read() or b"null")


def test_header_cookie_wrong_and_missing(auth_server):
    url = auth_server.info["url"]
    assert auth_server.info["auth"] is True and "token" not in json.dumps(auth_server.info).lower().replace("auth", "")
    status, body = raw_get(url + "/api/runs")
    assert status == 401 and "token" in body["error"]
    status, body = raw_get(url + "/api/runs", {"authorization": f"Bearer {TOKEN}"})
    assert status == 200
    status, body = raw_get(url + "/api/runs", {"cookie": f"other=1; cereyan_token={TOKEN}"})
    assert status == 200
    status, body = raw_get(url + "/api/runs", {"authorization": "Bearer nope"})
    assert status == 401 and body["error"] == "the API token was rejected"
    status, body = raw_get(url + "/api/health")
    assert status == 200 and body["ok"] is True
    # Static UI assets stay open.
    req = urllib.request.Request(url + "/")
    with urllib.request.urlopen(req, timeout=5) as resp:
        assert resp.status == 200


def test_engines_report_with_token_and_client_sends_it(auth_server):
    client = Client(auth_server.info["url"], token=TOKEN)
    fid = next(f["id"] for f in client.flows() if f["name"] == "etl")
    run = client._request("POST", f"/api/flows/{fid}/runs", body={"parameters": {"day": "2026-09-06", "n": 2}})
    auth_server.client = client
    done = auth_server.wait_run(run["id"])
    assert done["state"]["type"] == "Completed"
    assert client.task_runs(run["id"])


def test_default_client_and_cli_without_token(auth_server, isolated_home, monkeypatch):
    monkeypatch.delenv("CEREYAN_TOKEN", raising=False)
    with pytest.raises(AuthRequired):
        default_client(str(isolated_home))
    env = {k: v for k, v in os.environ.items() if k != "CEREYAN_TOKEN"}
    env["CEREYAN_HOME"] = str(isolated_home)
    proc = subprocess.run([sys.executable, "-m", "cereyan", "runs", "ls"], env=env, capture_output=True, text=True)
    assert proc.returncode != 0 and "CEREYAN_TOKEN" in proc.stderr
    env["CEREYAN_TOKEN"] = TOKEN
    proc = subprocess.run([sys.executable, "-m", "cereyan", "runs", "ls", "--json"], env=env, capture_output=True, text=True)
    assert proc.returncode == 0, proc.stderr
    with pytest.raises(AuthRequired):
        default_client(str(isolated_home), token="wrong")
    assert default_client(str(isolated_home), token=TOKEN).server()["version"]


def test_environment_beats_config_and_no_warning_with_token(isolated_home, project_dir, tmp_path):
    from cereyan import engine

    engine.close_store()
    (project_dir / "cereyan.toml").write_text('[server]\ntoken = "from-file"\n')
    srv = ServerProcess(str(isolated_home), str(project_dir), env={"CEREYAN_TOKEN": "from-env"})
    try:
        url = srv.info["url"]
        assert raw_get(url + "/api/runs", {"authorization": "Bearer from-file"})[0] == 401
        assert raw_get(url + "/api/runs", {"authorization": "Bearer from-env"})[0] == 200
    finally:
        srv.stop()
    (project_dir / "cereyan.toml").write_text('[server]\ntoken = "from-file"\nhost = "0.0.0.0"\n')
    srv = ServerProcess(str(isolated_home), str(project_dir), env={"CEREYAN_TOKEN": "from-env"})
    try:
        # Bound to every interface, but the discovery file has to name somewhere a
        # client can dial. 0.0.0.0 is a bind address: Linux and macOS route it to
        # loopback, Windows refuses it, so recording it would leave every consumer
        # of server.json unable to find the server on Windows.
        assert srv.info["host"] == "0.0.0.0"
        assert srv.info["url"] in (f"http://127.0.0.1:{srv.info['port']}", f"http://[::1]:{srv.info['port']}")
        assert raw_get(srv.info["url"] + "/api/runs", {"authorization": "Bearer from-env"})[0] == 200
        assert "unauthenticated" not in srv.read_log()
    finally:
        srv.stop()


def test_config_file_token_applies(auth_server):
    with pytest.raises(ApiError) as err:
        Client(auth_server.info["url"], token="nope").flows()
    assert err.value.status == 401
