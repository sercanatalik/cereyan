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
    assert auth_server.info["auth"] is True and auth_server.info["token_file"] is None
    assert TOKEN not in json.dumps(auth_server.info)
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


# -- Beyond loopback: a generated token unless the operator opts out ---------


def mcp_initialize(url, headers=None):
    """Status of an MCP ``initialize`` over HTTP."""
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                       "params": {"clientInfo": {"name": "token-test"}}}).encode()
    req = urllib.request.Request(url + "/mcp", data=body, method="POST",
                                 headers={"content-type": "application/json", **(headers or {})})
    try:
        with urllib.request.urlopen(req, timeout=5) as resp:
            return resp.status
    except urllib.error.HTTPError as exc:
        return exc.code


def start_beyond_loopback(isolated_home, project_dir, env=None, extra=None):
    from cereyan import engine
    from server_helpers import ServerProcess

    engine.close_store()
    fresh_log(isolated_home)
    full = {"CEREYAN_HOST": "0.0.0.0", **(env or {})}
    return ServerProcess(str(isolated_home), str(project_dir), env=full, extra=extra)


def fresh_log(isolated_home):
    """Empty the home's serve.log, which every server of the home appends to."""
    os.makedirs(str(isolated_home), exist_ok=True)
    open(os.path.join(str(isolated_home), "serve.log"), "wb").close()


def test_generated_token_beyond_loopback(isolated_home, project_dir, monkeypatch):
    from server_helpers import WINDOWS

    monkeypatch.delenv("CEREYAN_TOKEN", raising=False)
    token_path = isolated_home / "token"
    srv = start_beyond_loopback(isolated_home, project_dir)
    try:
        assert token_path.exists()
        token = token_path.read_text().strip()
        assert len(token) == 64
        if not WINDOWS:
            assert os.stat(token_path).st_mode & 0o777 == 0o600
        assert srv.info["auth"] is True and srv.info["exposed"] is False
        assert srv.info["token_file"] == str(token_path)
        assert token not in json.dumps(srv.info)
        url = srv.info["url"]
        bearer = {"authorization": f"Bearer {token}"}
        assert raw_get(url + "/api/health")[0] == 200
        assert raw_get(url + "/api/runs")[0] == 401
        assert raw_get(url + "/api/runs", bearer)[0] == 200
        assert mcp_initialize(url) == 401
        assert mcp_initialize(url, bearer) == 200
        server = raw_get(url + "/api/server", bearer)[1]
        assert (server["auth"], server["exposed"], server["token_file"]) == (True, False, str(token_path))
        log = srv.read_log()
        assert str(token_path) in log and "newly generated" in log
        assert token not in log and "reachable from the network" not in log
        # The Environment tab names the file as the token's source, value hidden.
        entries = {(e["table"], e["key"]): e for e in raw_get(url + "/api/settings/environment", bearer)[1]["configuration"]}
        tok = entries["server", "token"]
        assert (tok["value"], tok["secret"], tok["source"], tok["source_name"]) == (None, True, "generated", str(token_path))
        # Engines get the generated token like a configured one.
        client = Client(url, token=token)
        fid = next(f["id"] for f in client.flows() if f["name"] == "etl")
        run = client._request("POST", f"/api/flows/{fid}/runs", body={"parameters": {"day": "2026-09-06", "n": 1}})
        srv.client = client
        assert srv.wait_run(run["id"])["state"]["type"] == "Completed"
    finally:
        srv.stop()

    # A restart reuses the file rather than minting a new token.
    srv = start_beyond_loopback(isolated_home, project_dir)
    try:
        assert token_path.read_text().strip() == token
        assert raw_get(srv.info["url"] + "/api/runs", {"authorization": f"Bearer {token}"})[0] == 200
        assert "the generated API token" in srv.read_log()
    finally:
        srv.stop()

    # A configured token wins, and the file is left alone but not read.
    srv = start_beyond_loopback(isolated_home, project_dir, env={"CEREYAN_TOKEN": "explicit"})
    try:
        assert srv.info["auth"] is True and srv.info["token_file"] is None
        assert raw_get(srv.info["url"] + "/api/runs", {"authorization": f"Bearer {token}"})[0] == 401
        assert raw_get(srv.info["url"] + "/api/runs", {"authorization": "Bearer explicit"})[0] == 200
        assert token_path.read_text().strip() == token
    finally:
        srv.stop()

    # Loopback is unchanged: nothing generated, nothing required.
    token_path.unlink()
    from cereyan import engine
    from server_helpers import ServerProcess

    engine.close_store()
    fresh_log(isolated_home)
    srv = ServerProcess(str(isolated_home), str(project_dir))
    try:
        assert not token_path.exists()
        assert (srv.info["auth"], srv.info["exposed"], srv.info["token_file"]) == (False, False, None)
        assert raw_get(srv.info["url"] + "/api/runs")[0] == 200
        log = srv.read_log()
        assert "API token" not in log and "reachable from the network" not in log
    finally:
        srv.stop()


def test_local_clients_read_the_generated_token(isolated_home, project_dir, monkeypatch):
    from cereyan.client import find_server

    monkeypatch.delenv("CEREYAN_TOKEN", raising=False)
    srv = start_beyond_loopback(isolated_home, project_dir)
    try:
        token = (isolated_home / "token").read_text().strip()
        # The Python client and the CLI on this machine need nothing.
        assert default_client(str(isolated_home)).server()["auth"] is True
        assert find_server(str(isolated_home)).flows()
        env = {k: v for k, v in os.environ.items() if k != "CEREYAN_TOKEN"}
        env["CEREYAN_HOME"] = str(isolated_home)
        proc = subprocess.run([sys.executable, "-m", "cereyan", "runs", "ls", "--json"], env=env, capture_output=True, text=True)
        assert proc.returncode == 0, proc.stderr
        # The stdio MCP proxy goes through the same discovery.
        messages = [
            {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"clientInfo": {"name": "stdio-host"}}},
            {"jsonrpc": "2.0", "method": "notifications/initialized"},
            {"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "list_flows", "arguments": {}}},
        ]
        proc = subprocess.run([sys.executable, "-m", "cereyan", "mcp"], env=env,
                              input="".join(json.dumps(m) + "\n" for m in messages),
                              capture_output=True, text=True, timeout=60)
        replies = [json.loads(line) for line in proc.stdout.splitlines() if line.strip()]
        assert proc.returncode == 0 and [r["id"] for r in replies] == [1, 2], proc.stderr
        assert "etl" in {f["name"] for f in json.loads(replies[1]["result"]["content"][0]["text"])["flows"]}
        # An explicit value still wins over the file, even a wrong one.
        monkeypatch.setenv("CEREYAN_TOKEN", "wrong")
        with pytest.raises(AuthRequired, match="rejected"):
            default_client(str(isolated_home))
        # A client built for an address by hand never reads the file.
        monkeypatch.delenv("CEREYAN_TOKEN")
        assert Client(srv.info["url"]).token is None
        assert Client(srv.info["url"], token=token).server()["token_file"] == str(isolated_home / "token")
    finally:
        srv.stop()


def test_allow_unauthenticated_opts_out(isolated_home, project_dir, monkeypatch):
    monkeypatch.delenv("CEREYAN_TOKEN", raising=False)
    monkeypatch.delenv("CEREYAN_ALLOW_UNAUTHENTICATED", raising=False)
    token_path = isolated_home / "token"
    cases = [
        ({}, ["--allow-unauthenticated"], None, ("flag", "--allow-unauthenticated")),
        ({"CEREYAN_ALLOW_UNAUTHENTICATED": "yes"}, None, None, ("env", "CEREYAN_ALLOW_UNAUTHENTICATED")),
        ({}, None, "[server]\nallow_unauthenticated = true\n", ("toml", "[server] allow_unauthenticated")),
    ]
    for env, extra, toml, source in cases:
        if toml:
            (project_dir / "cereyan.toml").write_text(toml)
        srv = start_beyond_loopback(isolated_home, project_dir, env=env, extra=extra)
        try:
            assert not token_path.exists()
            assert (srv.info["auth"], srv.info["exposed"], srv.info["token_file"]) == (False, True, None)
            url = srv.info["url"]
            assert raw_get(url + "/api/runs")[0] == 200
            assert mcp_initialize(url) == 200
            assert raw_get(url + "/api/server")[1]["exposed"] is True
            assert "reachable from the network" in srv.read_log()
            entries = {(e["table"], e["key"]): e for e in raw_get(url + "/api/settings/environment")[1]["configuration"]}
            entry = entries["server", "allow_unauthenticated"]
            assert (entry["value"], entry["source"], entry["source_name"]) == (True, *source)
        finally:
            srv.stop()
        (project_dir / "cereyan.toml").unlink(missing_ok=True)

    # A value that is not a boolean stops the server, naming the variable.
    with pytest.raises(RuntimeError, match="CEREYAN_ALLOW_UNAUTHENTICATED"):
        start_beyond_loopback(isolated_home, project_dir, env={"CEREYAN_ALLOW_UNAUTHENTICATED": "maybe"})

    # With a token it has no effect, and the server says so.
    srv = start_beyond_loopback(isolated_home, project_dir, env={"CEREYAN_TOKEN": TOKEN}, extra=["--allow-unauthenticated"])
    try:
        assert (srv.info["auth"], srv.info["exposed"]) == (True, False)
        assert raw_get(srv.info["url"] + "/api/runs")[0] == 401
        assert "no effect" in srv.read_log()
    finally:
        srv.stop()

    # On loopback it has no effect and nothing is said.
    from cereyan import engine
    from server_helpers import ServerProcess

    engine.close_store()
    fresh_log(isolated_home)
    srv = ServerProcess(str(isolated_home), str(project_dir), extra=["--allow-unauthenticated"])
    try:
        assert (srv.info["auth"], srv.info["exposed"]) == (False, False)
        log = srv.read_log()
        assert "no effect" not in log and "reachable from the network" not in log
    finally:
        srv.stop()
