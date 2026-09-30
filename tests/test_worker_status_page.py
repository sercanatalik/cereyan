"""The worker's read-only status page: where it listens, what status.json and
healthz say while the server answers and after it stops, the embedded page
under a proxy prefix, and a port that cannot be bound."""

from __future__ import annotations

import http.server
import json
import re
import socket
import threading
import urllib.error
import urllib.request

import pytest

from cereyan.client import Client
from server_helpers import ServerProcess
from test_remote_workers import TOKEN, WorkerProcess, online, wait_for

PIPELINE = '''
import os
import time
from cereyan import App

app = App("status")

@app.flow(runs_on="server")
def hold(gate: str, seconds: float = 60.0):
    # Keeps the server's one processor busy so other runs go to the worker.
    end = time.time() + seconds
    while not os.path.exists(gate) and time.time() < end:
        time.sleep(0.05)

@app.flow
def ok():
    return 1

@app.flow
def boom():
    raise ValueError("boom")
'''


@pytest.fixture
def project(tmp_path):
    d = tmp_path / "server-checkout"
    d.mkdir()
    (d / "pipeline.py").write_text(PIPELINE)
    return d


@pytest.fixture
def srv(isolated_home, project):
    from cereyan import engine

    engine.close_store()
    server = ServerProcess(str(isolated_home), str(project), max_engines=1, extra=["--token", TOKEN])
    server.client = Client(server.info["url"], token=TOKEN)
    try:
        yield server
    finally:
        server.stop()


@pytest.fixture
def workers(tmp_path, srv, project):
    started = []

    def start(**kwargs):
        w = WorkerProcess(tmp_path, srv.info["url"], source=project, **kwargs)
        started.append(w)
        return w

    yield start
    for w in started:
        w.stop()


def page_url(w) -> str:
    """The status page address the worker logged at startup."""
    found = wait_for(lambda: re.search(r"status page on (http://\S+)", w.read_log()), timeout=30,
                     what="the status page line")
    return found.group(1)


def get(url: str, method: str = "GET") -> tuple[int, bytes, dict]:
    req = urllib.request.Request(url, method=method)
    try:
        with urllib.request.urlopen(req, timeout=10) as r:
            return r.status, r.read(), dict(r.headers)
    except urllib.error.HTTPError as exc:
        return exc.code, exc.read(), dict(exc.headers)


def status(url: str) -> dict:
    code, body, _ = get(f"{url}/status.json")
    assert code == 200
    return json.loads(body)


def test_counts_engines_and_health_while_the_server_answers(srv, workers, tmp_path):
    w = workers(name="w1", processors=1)
    url = page_url(w)
    assert url.startswith("http://127.0.0.1:") and not url.endswith(":0")
    wait_for(lambda: online(srv, "w1"), what="w1 online")
    gate = str(tmp_path / "gate")
    held = srv.client.run("hold", gate=gate)
    srv.wait_run(held["id"], until=lambda r: r["state"]["type"] == "Running")
    done = srv.client.run("ok")
    assert srv.wait_run(done["id"], timeout=40)["host"] == "w1", w.read_log()
    failed = srv.client.run("boom")
    assert srv.wait_run(failed["id"], timeout=40)["state"]["type"] == "Failed"
    open(gate, "w").close()

    def counted():
        s = status(url)
        by = {f["flow"]: f for f in (s["stats"] or {}).get("by_flow", [])}
        return s if by.get("ok", {}).get("completed") == 1 and by.get("boom", {}).get("failed") == 1 else None

    s = wait_for(counted, what="counts in status.json")
    assert s["name"] == "w1" and s["state"] == "online" and s["server_reachable"] is True
    assert s["worker_id"] is not None and s["processors"] == 1
    assert {f["flow"] for f in s["flows"]} == {"hold", "ok", "boom"}
    assert s["host"]["meta"]["status_url"] == url
    assert s["stats"]["by_flow"][0]["last_completed_at"] or s["stats"]["by_flow"][1]["last_completed_at"]
    assert all(e["slot"] >= 1 for e in s["stats"]["engines"])
    assert any("registered as w1" in e["message"] for e in s["events"])
    code, body, _ = get(f"{url}/healthz")
    assert code == 200 and json.loads(body)["ok"] is True
    # The server lists the page's address among the worker's host details.
    view = online(srv, "w1")
    assert view["meta"]["status_url"] == url


def test_unreachable_keeps_the_last_counts_and_fails_health(srv, workers):
    w = workers(name="w1")
    url = page_url(w)
    wait_for(lambda: status(url)["stats"] is not None, what="first stats")
    srv.stop()
    s = wait_for(lambda: (lambda s: s if not s["server_reachable"] else None)(status(url)), what="unreachable")
    assert s["failing_since"] is not None and s["stats"] is not None and s["stats_as_of"] is not None
    # Failures repeat every heartbeat; they collapse into one event with a count.
    s = wait_for(lambda: (lambda s: s if s["events"][-1]["count"] >= 2 else None)(status(url)), what="a repeat")
    assert s["events"][-1]["level"] == "error" and "heartbeat failed" in s["events"][-1]["message"]
    interval = s["heartbeat_secs"]
    wait_for(lambda: get(f"{url}/healthz")[0] == 503, timeout=interval * 3 + 20, what="healthz 503")


def test_page_routes_methods_and_a_proxy_prefix(srv, workers):
    w = workers(name="w1")
    url = page_url(w)
    code, html, headers = get(f"{url}/")
    assert code == 200 and headers["Content-Type"].startswith("text/html")
    assets = re.findall(r'(?:src|href)="\./(assets/[^"]+)"', html.decode())
    assert assets and all(get(f"{url}/{a}")[0] == 200 for a in assets)
    assert get(f"{url}/status.json", method="POST")[0] == 405
    assert get(f"{url}/nothing-here")[0] == 404
    assert get(f"{url}/healthz", method="HEAD")[0] in (200, 503)

    # A proxy that serves the worker under /workers/w1/ and strips the prefix.
    class Proxy(http.server.BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def do_GET(self):  # noqa: N802
            path = self.path.removeprefix("/workers/w1")
            code, body, headers = get(url + path)
            self.send_response(code)
            self.send_header("Content-Type", headers.get("Content-Type", "text/plain"))
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

    proxy = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Proxy)
    threading.Thread(target=proxy.serve_forever, daemon=True).start()
    try:
        base = f"http://127.0.0.1:{proxy.server_address[1]}/workers/w1/"
        code, html, _ = get(base)
        assert code == 200
        for a in re.findall(r'(?:src|href)="\./(assets/[^"]+)"', html.decode()):
            assert get(base + a)[0] == 200
        assert json.loads(get(base + "status.json")[1])["name"] == "w1"
    finally:
        proxy.shutdown()
        proxy.server_close()


def test_a_taken_port_stops_the_worker_before_it_registers(srv, workers):
    with socket.socket() as taken:
        taken.bind(("127.0.0.1", 0))
        taken.listen()
        port = taken.getsockname()[1]
        w = workers(name="w1", extra_args=["--status-port", str(port)])
        assert w.proc.wait(timeout=30) != 0
    log = w.read_log()
    assert "--status-port" in log, log
    assert "registered as" not in log


def test_heartbeat_stats_count_from_the_workers_start(srv):
    from cereyan import __version__

    ran = srv.client.run("ok")
    srv.wait_run(ran["id"])
    started = int(__import__("time").time() * 1_000_000)
    body = {"name": "fake", "version": __version__, "cpus": 2, "processors": 1,
            "meta": {"started_at": started}, "flows": []}
    reg = srv.client._request("POST", "/api/workers/register", body=body)
    answer = srv.client._request("POST", f"/api/workers/{reg['worker_id']}/heartbeat", body={"engines": []})
    # What a 3.0 worker reads is unchanged; stats ride alongside.
    assert set(answer) >= {"commands", "state", "drift", "stats"}
    assert answer["stats"]["since"] == started
    assert answer["stats"]["by_flow"] == [] and answer["stats"]["engines"] == []
