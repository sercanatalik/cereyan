"""1.1 Unix socket listener and async custom route handlers."""

from __future__ import annotations

import json
import os
import socket
import subprocess
import sys
import threading
import time
import urllib.request

import pytest

from cereyan.client import Client
from server_helpers import ServerProcess

PIPELINE = '''
import asyncio, time
from cereyan import App

app = App("sock")

@app.flow
def quick():
    return 1

@app.get("/api/ext/slow")
async def slow(delay: float = 0.3):
    await asyncio.sleep(delay)
    return {"ok": True, "delay": delay}

@app.get("/api/ext/aboom")
async def aboom():
    raise RuntimeError("async boom")

@app.get("/api/ext/sync")
def sync():
    return {"sync": True}
'''

unix_only = pytest.mark.skipif(sys.platform.startswith("win"), reason="Unix sockets")


@pytest.fixture
def short_dir():
    """Socket paths must fit SUN_LEN (about 100 bytes); pytest's tmp paths do not."""
    import shutil
    import tempfile

    d = tempfile.mkdtemp(prefix="cy", dir="/tmp")
    try:
        yield d
    finally:
        shutil.rmtree(d, ignore_errors=True)


@pytest.fixture
def sock_dir(tmp_path):
    d = tmp_path / "sock"
    d.mkdir()
    (d / "pipeline.py").write_text(PIPELINE)
    return d


def socket_get(path, target, method="GET"):
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(5)
    s.connect(path)
    s.sendall(f"{method} {target} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n".encode())
    chunks = []
    while True:
        part = s.recv(65536)
        if not part:
            break
        chunks.append(part)
    s.close()
    raw = b"".join(chunks)
    head, _, body = raw.partition(b"\r\n\r\n")
    status = int(head.split(b" ")[1])
    return status, json.loads(body) if body.strip() else None


@unix_only
def test_socket_serves_alongside_tcp_and_bypasses_token(isolated_home, sock_dir, short_dir):
    from cereyan import engine

    engine.close_store()
    path = os.path.join(short_dir, "cereyan.sock")
    # A stale file from a crashed server is replaced.
    with open(path, "w") as fh:
        fh.write("stale")
    srv = ServerProcess(str(isolated_home), str(sock_dir), env={"CEREYAN_TOKEN": "tok", "CEREYAN_SOCKET": path})
    try:
        assert srv.info["socket"] == path and srv.info["auth"] is True
        assert oct(os.stat(path).st_mode & 0o777) == "0o600"
        assert "trusted by file permission" in srv.read_log()
        assert socket_get(path, "/api/health")[0] == 200
        assert socket_get(path, "/api/runs")[0] == 200  # no token needed over the socket
        tcp = urllib.request.Request(srv.info["url"] + "/api/runs")
        with pytest.raises(urllib.error.HTTPError) as err:
            urllib.request.urlopen(tcp, timeout=5)
        assert err.value.code == 401
        # The Python client picks the socket when it has no token.
        env = {k: v for k, v in os.environ.items() if k != "CEREYAN_TOKEN"}
        env["CEREYAN_HOME"] = str(isolated_home)
        proc = subprocess.run([sys.executable, "-m", "cereyan", "runs", "ls", "--json"], env=env, capture_output=True, text=True)
        assert proc.returncode == 0, proc.stderr
        explicit = Client(socket_path=path)
        assert explicit.health() and explicit.server()["version"]
    finally:
        srv.stop()
    assert not os.path.exists(path), "socket file removed on clean shutdown"


@unix_only
def test_live_socket_is_refused(isolated_home, sock_dir, tmp_path, short_dir):
    from cereyan import engine

    engine.close_store()
    path = os.path.join(short_dir, "live.sock")
    srv = ServerProcess(str(isolated_home), str(sock_dir), env={"CEREYAN_SOCKET": path})
    try:
        other_home = tmp_path / "other_home"
        env = dict(os.environ, CEREYAN_HOME=str(other_home), CEREYAN_SOCKET=path, CEREYAN_NO_BROWSER="1")
        proc = subprocess.run([sys.executable, "-m", "cereyan", "serve", str(sock_dir), "--port", "0", "--no-open"],
                              env=env, capture_output=True, text=True, timeout=60)
        assert proc.returncode != 0 and "socket" in proc.stderr.lower()
        assert socket_get(path, "/api/health")[0] == 200
    finally:
        srv.stop()


def test_async_routes(isolated_home, sock_dir):
    from cereyan import engine

    engine.close_store()
    srv = ServerProcess(str(isolated_home), str(sock_dir))
    try:
        c = srv.client
        assert c._request("GET", "/api/ext/slow", params={"delay": 0.05}) == {"ok": True, "delay": 0.05}
        assert c._request("GET", "/api/ext/sync") == {"sync": True}
        # Two concurrent async requests overlap on the shared loop.
        results = []

        def call():
            t0 = time.time()
            c._request("GET", "/api/ext/slow", params={"delay": 0.5})
            results.append(time.time() - t0)

        threads = [threading.Thread(target=call) for _ in range(2)]
        t0 = time.time()
        for t in threads:
            t.start()
        for t in threads:
            t.join()
        assert time.time() - t0 < 0.9, results
        # Failures are isolated.
        from cereyan.client import ApiError

        with pytest.raises(ApiError) as err:
            c._request("GET", "/api/ext/aboom")
        assert err.value.status == 500
        assert c._request("GET", "/api/ext/sync") == {"sync": True}
        assert "async boom" in srv.read_log()
    finally:
        srv.stop()
