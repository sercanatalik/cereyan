"""Host and Origin checks: a rebound name or another site's page is refused before anything else."""

from __future__ import annotations

import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import urllib.error
import urllib.request
from urllib.parse import urlsplit

import pytest

from server_helpers import ServerProcess

PIPELINE = '''
import time
from cereyan import App

app = App("guard")

@app.flow
def slow():
    time.sleep(60)

@app.post("/hook")
def hook():
    return {"hooked": True}
'''

EVIL = "https://evil.example"

unix_only = pytest.mark.skipif(sys.platform.startswith("win"), reason="Unix sockets")


def start(isolated_home, tmp_path, env: dict | None = None) -> ServerProcess:
    from cereyan import engine

    engine.close_store()
    d = tmp_path / "guard"
    d.mkdir(exist_ok=True)
    (d / "pipeline.py").write_text(PIPELINE)
    return ServerProcess(str(isolated_home), str(d), env=env)


@pytest.fixture
def srv(isolated_home, tmp_path):
    s = start(isolated_home, tmp_path)
    try:
        yield s
    finally:
        s.stop()


def send(srv, method: str, path: str, headers: dict | None = None, body=None):
    data = json.dumps(body).encode() if body is not None else (None if method == "GET" else b"")
    req = urllib.request.Request(srv.info["url"] + path, data=data, headers=headers or {}, method=method)
    try:
        with urllib.request.urlopen(req, timeout=10) as resp:
            status, raw = resp.status, resp.read()
    except urllib.error.HTTPError as exc:
        status, raw = exc.code, exc.read()
    try:
        return status, (json.loads(raw) if raw else None)
    except ValueError:
        return status, raw.decode("utf-8", "replace")


def own(srv) -> str:
    """The server's own origin, as the UI's requests carry it."""
    return "http://" + urlsplit(srv.info["url"]).netloc


def tool(tool_name: str, /, **arguments) -> dict:
    return {"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": tool_name, "arguments": arguments}}


def environment_entry(srv, key: str) -> dict:
    env = srv.client._request("GET", "/api/settings/environment")
    return next(e for e in env["configuration"] if (e["table"], e["key"]) == ("server", key))


def test_rebound_host_is_refused_everywhere(srv):
    port = urlsplit(srv.info["url"]).port
    for path in ("/api/runs", "/api/health", "/", "/runs/1", "/hook"):
        status, body = send(srv, "GET", path, {"Host": f"evil.example:{port}"})
        assert status == 403, path
        assert body["host"] == "evil.example" and "allowed_hosts" in body["error"], path
    assert send(srv, "GET", "/api/runs", {"Host": f"127.0.0.1:{port}"})[0] == 200
    assert send(srv, "GET", "/api/runs", {"Host": "localhost:9999"})[0] == 200
    assert send(srv, "GET", "/api/runs", {"Host": "app.localhost"})[0] == 403
    entry = environment_entry(srv, "allowed_hosts")
    assert (entry["value"], entry["source"]) == ([], "default")


def test_cross_site_requests_are_refused(srv):
    run_id = srv.client.submit("guard", "slow")["id"]
    srv.wait_run(run_id, until=lambda r: r["state"]["type"] == "Running")
    status, body = send(srv, "POST", f"/api/runs/{run_id}/cancel", {"Origin": EVIL})
    assert status == 403 and body["origin"] == EVIL
    # text/plain is what a page can send without a preflight; JSON is refused by origin too.
    for content_type in ("text/plain", "application/json"):
        status, _ = send(srv, "POST", "/mcp", {"Origin": EVIL, "Content-Type": content_type},
                         tool("set_variable", name="stolen", value="x"))
        assert status == 403, content_type
    assert send(srv, "POST", "/mcp", {"Origin": "null", "Content-Type": "application/json"},
                tool("cancel_run", run_id=run_id))[0] == 403
    assert send(srv, "POST", f"/api/runs/{run_id}/cancel", {"Origin": "http://localhost:3000"})[0] == 403
    assert "stolen" not in [v["name"] for v in srv.client.variables()]
    assert srv.client.get_run(run_id)["state"]["type"] == "Running"
    # Without an Origin, as the CLI and agents send it, the same call works.
    status, body = send(srv, "POST", "/mcp", {"Content-Type": "application/json"},
                        tool("set_variable", name="allowed", value="x"))
    assert status == 200 and not body["result"]["isError"], body
    assert "allowed" in [v["name"] for v in srv.client.variables()]
    # The UI's requests carry its own origin and pass.
    assert send(srv, "POST", f"/api/runs/{run_id}/cancel", {"Origin": own(srv)})[0] == 200
    assert srv.wait_run(run_id)["state"]["type"] == "Cancelled"


def test_custom_routes_are_guarded(srv):
    assert send(srv, "POST", "/hook", {"Origin": EVIL})[0] == 403
    assert send(srv, "POST", "/hook", {"Origin": own(srv)}) == (200, {"hooked": True})
    assert send(srv, "POST", "/hook") == (200, {"hooked": True})


def test_allowed_hosts_accepts_host_and_origin(isolated_home, tmp_path):
    srv = start(isolated_home, tmp_path, env={"CEREYAN_ALLOWED_HOSTS": "Cereyan.Example.com"})
    try:
        assert send(srv, "GET", "/api/runs", {"Host": "cereyan.example.com"})[0] == 200
        assert send(srv, "GET", "/api/runs", {"Host": "other.example.com"})[0] == 403
        # Behind a proxy: Host is the upstream address, Origin the public page.
        assert send(srv, "POST", "/hook", {"Origin": "https://cereyan.example.com"}) == (200, {"hooked": True})
        assert send(srv, "POST", "/hook", {"Origin": "https://other.example.com"})[0] == 403
        entry = environment_entry(srv, "allowed_hosts")
        assert entry["value"] == ["cereyan.example.com"]
        assert (entry["source"], entry["source_name"]) == ("env", "CEREYAN_ALLOWED_HOSTS")
    finally:
        srv.stop()


def test_refused_before_the_token_and_under_a_base_path(isolated_home, tmp_path):
    srv = start(isolated_home, tmp_path, env={"CEREYAN_TOKEN": "tok", "CEREYAN_BASE_PATH": "/cereyan"})
    try:
        assert srv.info["url"].endswith("/cereyan")
        status, body = send(srv, "GET", "/api/runs", {"Host": "evil.example"})
        assert status == 403 and body["host"] == "evil.example"
        assert send(srv, "GET", "/api/runs")[0] == 401
        assert send(srv, "GET", "/api/runs", {"Authorization": "Bearer tok"})[0] == 200
        assert send(srv, "GET", "/api/runs", {"Authorization": "Bearer tok", "Origin": EVIL})[0] == 403
    finally:
        srv.stop()


def test_invalid_allowed_hosts_stops_the_server(isolated_home, tmp_path):
    d = tmp_path / "bad"
    d.mkdir()
    (d / "pipeline.py").write_text(PIPELINE)
    env = dict(os.environ, CEREYAN_HOME=str(isolated_home), CEREYAN_NO_BROWSER="1",
               CEREYAN_ALLOWED_HOSTS="cereyan.example.com:443")
    proc = subprocess.run([sys.executable, "-m", "cereyan", "serve", str(d), "--port", "0", "--no-open"],
                          env=env, capture_output=True, text=True, timeout=60)
    assert proc.returncode != 0
    assert "allowed_hosts" in proc.stderr and "CEREYAN_ALLOWED_HOSTS" in proc.stderr


@unix_only
def test_socket_skips_the_guard(isolated_home, tmp_path):
    # Socket paths must fit SUN_LEN (about 100 bytes); pytest's tmp paths do not.
    short = tempfile.mkdtemp(prefix="cy", dir="/tmp")
    path = os.path.join(short, "guard.sock")
    srv = start(isolated_home, tmp_path, env={"CEREYAN_SOCKET": path})
    try:
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.settimeout(5)
        s.connect(path)
        s.sendall(f"GET /api/runs HTTP/1.1\r\nHost: evil.example\r\nOrigin: {EVIL}\r\nConnection: close\r\n\r\n".encode())
        raw = b""
        while chunk := s.recv(65536):
            raw += chunk
        s.close()
        assert raw.split(b" ")[1] == b"200", raw[:200]
    finally:
        srv.stop()
        shutil.rmtree(short, ignore_errors=True)
