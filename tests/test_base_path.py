"""Base path, and the fixes in the same code paths: app.serve() precedence,
path-scoped token checks, and the stdio MCP transport's URL path."""

from __future__ import annotations

import http.server
import json
import os
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request

import pytest

from server_helpers import WINDOWS, ServerProcess, stop_server

TOKEN = "s3cret-token"


def raw(url, method="GET", headers=None, body=None):
    """Status, body bytes, and headers (case-insensitive), without following redirects."""

    class NoRedirect(urllib.request.HTTPRedirectHandler):
        def redirect_request(self, *args, **kwargs):
            return None

    opener = urllib.request.build_opener(NoRedirect)
    req = urllib.request.Request(url, data=body, method=method, headers=headers or {})
    try:
        with opener.open(req, timeout=5) as resp:
            return resp.status, resp.read(), resp.headers
    except urllib.error.HTTPError as exc:
        return exc.code, exc.read(), exc.headers


@pytest.fixture
def short_dir():
    """Socket paths must fit SUN_LEN (about 100 bytes); pytest's tmp paths do not."""
    d = tempfile.mkdtemp(prefix="cy", dir=None if WINDOWS else "/tmp")
    try:
        yield d
    finally:
        shutil.rmtree(d, ignore_errors=True)


# -- app.serve() ranks below the environment ---------------------------------

APP_SERVE_SCRIPT = '''
import os
from cereyan import App

app = App("prec")

@app.flow
def quick():
    return 1

app.serve(port=int(os.environ["APP_PORT"]), token="app-token", socket=os.environ.get("APP_SOCKET") or None,
          open_browser=False)
'''


def _wait_discovery(home, proc, timeout=30.0):
    path = os.path.join(str(home), "server.json")
    deadline = time.time() + timeout
    while time.time() < deadline:
        if proc.poll() is not None:
            raise RuntimeError(f"app.serve exited early with {proc.returncode}")
        try:
            with open(path) as fh:
                info = json.load(fh)
            # On Windows a venv's python.exe is a launcher, so the server's PID is its child's.
            if info.get("pid") == proc.pid or ServerProcess._alive(info):
                return info
        except (OSError, ValueError):
            pass
        time.sleep(0.05)
    raise RuntimeError("app.serve did not start")


def test_app_serve_ranks_below_the_environment(isolated_home, tmp_path, short_dir):
    from server_helpers import free_port

    script = tmp_path / "prec" / "pipeline.py"
    script.parent.mkdir()
    script.write_text(APP_SERVE_SCRIPT)
    app_port, env_port = free_port(), free_port()
    env = dict(os.environ, CEREYAN_HOME=str(isolated_home), CEREYAN_NO_BROWSER="1",
               APP_PORT=str(app_port), CEREYAN_PORT=str(env_port), CEREYAN_TOKEN=TOKEN)
    env.pop("CEREYAN_HOST", None)
    env_socket = None
    if not WINDOWS:
        env["APP_SOCKET"] = os.path.join(short_dir, "app.sock")
        env_socket = os.path.join(short_dir, "env.sock")
        env["CEREYAN_SOCKET"] = env_socket
    proc = subprocess.Popen([sys.executable, str(script)], env=env, stdout=subprocess.DEVNULL,
                            stderr=subprocess.DEVNULL,
                            **({"creationflags": subprocess.CREATE_NEW_PROCESS_GROUP} if WINDOWS else {}))
    try:
        info = _wait_discovery(isolated_home, proc)
        assert info["port"] == env_port
        if env_socket:
            assert info["socket"] == env_socket
        url = info["url"]
        assert raw(url + "/api/runs", headers={"authorization": f"Bearer {TOKEN}"})[0] == 200
        assert raw(url + "/api/runs", headers={"authorization": "Bearer app-token"})[0] == 401
    finally:
        stop_server(proc)


# -- the token check is path-scoped -------------------------------------------

@pytest.fixture
def auth_server(isolated_home, project_dir):
    from cereyan import engine

    engine.close_store()
    srv = ServerProcess(str(isolated_home), str(project_dir), env={"CEREYAN_TOKEN": TOKEN})
    try:
        yield srv
    finally:
        srv.stop()


def test_custom_routes_outside_api_are_open(auth_server):
    url = auth_server.info["url"]
    status, body, _ = raw(url + "/health")
    assert (status, body) == (200, b"fine")
    assert raw(url + "/api/ext/ping")[0] == 401
    mcp = json.dumps({"jsonrpc": "2.0", "id": 1, "method": "ping"}).encode()
    assert raw(url + "/mcp", method="POST", body=mcp, headers={"content-type": "application/json"})[0] == 401


# -- the stdio MCP transport keeps the URL path --------------------------------

@pytest.fixture
def fake_mcp():
    seen: list[str] = []

    class Handler(http.server.BaseHTTPRequestHandler):
        def do_POST(self):
            self.rfile.read(int(self.headers.get("content-length") or 0))
            seen.append(self.path)
            body = json.dumps({"jsonrpc": "2.0", "id": 1, "result": {}}).encode()
            self.send_response(200)
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *args):
            pass

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield f"http://127.0.0.1:{server.server_address[1]}", seen
    finally:
        server.shutdown()
        server.server_close()


# -- resolving the base path ---------------------------------------------------

@pytest.mark.parametrize("given, expected", [
    ("cereyan/", "/cereyan"), ("/cereyan", "/cereyan"), ("/", ""), ("", ""), ("/a/b.c~d_e-f", "/a/b.c~d_e-f"),
])
def test_base_path_is_normalised(given, expected):
    from cereyan.serve import normalize_base_path

    assert normalize_base_path(given, "test") == expected


@pytest.mark.parametrize("given", ["/a/../b", "/./a", "/a b", "/a?x", "/a#x", "/a%20b", "/a//b", "/{x}", " /a"])
def test_bad_base_path_names_value_and_source(given):
    from cereyan.exceptions import CereyanError
    from cereyan.serve import normalize_base_path

    with pytest.raises(CereyanError) as info:
        normalize_base_path(given, "CEREYAN_BASE_PATH")
    assert repr(given) in str(info.value) and "CEREYAN_BASE_PATH" in str(info.value)


def test_base_path_precedence(tmp_path, monkeypatch):
    from cereyan.serve import resolve_base_path

    project = tmp_path / "with_toml"
    project.mkdir()
    (project / "cereyan.toml").write_text('[server]\nbase_path = "/toml"\n')
    monkeypatch.delenv("CEREYAN_BASE_PATH", raising=False)
    assert resolve_base_path(str(tmp_path / "no_toml")) == ""
    assert resolve_base_path(str(project)) == "/toml"
    assert resolve_base_path(str(project), app_base_path="/app") == "/app"
    monkeypatch.setenv("CEREYAN_BASE_PATH", "/env")
    assert resolve_base_path(str(project), app_base_path="/app") == "/env"
    assert resolve_base_path(str(project), "/flag", "/app") == "/flag"
    assert resolve_base_path(str(project), "/") == ""  # an explicit root flag still wins


@pytest.mark.parametrize("suffix, expected", [("", "/mcp"), ("/prefix", "/prefix/mcp"), ("/prefix/", "/prefix/mcp")])
def test_mcp_transport_posts_under_the_url_path(fake_mcp, suffix, expected):
    from cereyan.mcp import Transport

    base, seen = fake_mcp
    reply = Transport(base + suffix, None, None).send({"jsonrpc": "2.0", "id": 1, "method": "ping"})
    assert reply == {"jsonrpc": "2.0", "id": 1, "result": {}}
    assert seen == [expected]


# -- serving under a base path -------------------------------------------------

BASE = "/cereyan"


def _serve_under_base(home, directory, *, env=None, extra=()):
    from cereyan import engine

    engine.close_store()
    return ServerProcess(str(home), str(directory), env=env, extra=["--base-path", BASE, *extra])


@pytest.fixture
def base_server(isolated_home, project_dir):
    srv = _serve_under_base(isolated_home, project_dir)
    try:
        yield srv
    finally:
        srv.stop()


def test_everything_is_served_under_the_base_path(base_server):
    info = base_server.info
    origin = f"http://127.0.0.1:{info['port']}"
    url = origin + BASE
    assert (info["base_path"], info["url"]) == (BASE, url)
    assert raw(url + "/api/health")[0] == 200
    assert raw(origin + "/api/health")[0] == 404
    assert raw(origin + "/elsewhere")[0] == 404
    for target in (origin + "/", url):
        status, _, headers = raw(target)
        assert (status, headers["location"]) == (307, BASE + "/"), target
    # A deep link gets index.html with the base tag, and its relative assets load under the base.
    status, body, _ = raw(url + "/runs/42")
    html = body.decode()
    assert status == 200 and f'<base href="{BASE}/" />' in html
    script = re.search(r'src="\./(assets/[^"]+\.js)"', html).group(1)
    assert raw(f"{url}/{script}")[0] == 200
    assert raw(url + "/health")[:2] == (200, b"fine")
    server = json.loads(raw(url + "/api/server")[1])
    assert (server["base_path"], server["url"]) == (BASE, url)


def test_cli_and_engines_find_the_server_under_the_base_path(base_server, run_cli):
    fid = next(f["id"] for f in base_server.client.flows() if f["name"] == "etl")
    run = base_server.client._request("POST", f"/api/flows/{fid}/runs", body={"parameters": {"day": "2026-09-06"}})
    assert base_server.wait_run(run["id"])["state"]["type"] == "Completed"
    result = run_cli("runs", "ls", "--json", home=base_server.home)
    assert result.returncode == 0, result.stderr
    assert [r["id"] for r in json.loads(result.stdout)] == [run["id"]]


def test_token_check_under_the_base_path(isolated_home, project_dir):
    srv = _serve_under_base(isolated_home, project_dir, env={"CEREYAN_TOKEN": TOKEN})
    try:
        url = srv.info["url"]
        assert raw(url + "/api/health")[0] == 200
        assert raw(url + "/api/runs")[0] == 401
        assert raw(url + "/api/runs", headers={"authorization": f"Bearer {TOKEN}"})[0] == 200
        assert raw(url + "/health")[:2] == (200, b"fine")
    finally:
        srv.stop()


def _socket_status(path, target):
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(5)
    s.connect(path)
    s.sendall(f"GET {target} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n".encode())
    head = s.recv(65536)
    s.close()
    return int(head.split(b" ")[1])


@pytest.mark.skipif(WINDOWS, reason="Unix sockets")
def test_socket_serves_at_the_root(isolated_home, project_dir, short_dir):
    sock = os.path.join(short_dir, "c.sock")
    srv = _serve_under_base(isolated_home, project_dir, extra=["--socket", sock])
    try:
        assert srv.info["socket"] == sock
        assert raw(srv.info["url"] + "/api/health")[0] == 200
        assert _socket_status(sock, "/api/health") == 200
    finally:
        srv.stop()


def test_bad_base_path_stops_serve_before_binding(isolated_home, project_dir, run_cli):
    result = run_cli("serve", str(project_dir), "--port", "0", "--no-open", "--base-path", "/a/../b")
    assert result.returncode != 0
    assert "'/a/../b'" in result.stderr and "--base-path" in result.stderr
    assert not os.path.exists(os.path.join(str(isolated_home), "server.json"))


def test_mcp_stdio_under_the_base_path(isolated_home, project_dir):
    srv = _serve_under_base(isolated_home, project_dir, env={"CEREYAN_TOKEN": TOKEN})
    try:
        env = dict(os.environ, CEREYAN_HOME=str(isolated_home), CEREYAN_TOKEN=TOKEN)
        initialize = {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"clientInfo": {"name": "t"}}}
        for args in ([], ["--url", srv.info["url"]]):  # through server.json, then explicitly
            proc = subprocess.run([sys.executable, "-m", "cereyan", "mcp", *args], env=env,
                                  input=json.dumps(initialize) + "\n", capture_output=True, text=True, timeout=60)
            assert proc.returncode == 0, proc.stderr
            reply = json.loads(proc.stdout.splitlines()[0])
            assert reply["result"]["serverInfo"]["name"] == "cereyan", (args, reply)
    finally:
        srv.stop()
