"""Remote workers: a second process with its own home and its own copy of the
checkout, registered with a server, taking the runs the server cannot."""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import time

import pytest

from cereyan.client import ApiError, Client
from server_helpers import ServerProcess, own_group, stop_server

TOKEN = "remote-workers-token"

PIPELINE = '''
import os
import time
from cereyan import App, LocalTarget, Variable, task

app = App("remote")

@app.flow
def gated(gate: str, seconds: float = 60.0):
    # Holds its run until the gate file appears.
    end = time.time() + seconds
    while not os.path.exists(gate) and time.time() < end:
        time.sleep(0.05)

@app.flow(runs_on="server")
def pinned(gate: str, seconds: float = 60.0):
    end = time.time() + seconds
    while not os.path.exists(gate) and time.time() < end:
        time.sleep(0.05)

@task(persist_result=True)
def compute(n: int) -> dict:
    return {"n": n, "worker": os.environ.get("CEREYAN_WORKER")}

@app.flow
def uses_host():
    token = Variable.get("remote/secret")
    assert token == "s3cr3t", f"secret was {token!r}"
    compute(1)
    with LocalTarget("out/local.txt").open("w") as fh:
        fh.write("x")
    return os.environ.get("CEREYAN_WORKER")
'''

EXTRA = '''
from cereyan import App

extra = App("remote")

@extra.flow
def only_on_worker():
    pass
'''


def api(srv, method, path, body=None):
    return srv.client._request(method, path, body=body)


def wait_for(check, timeout=40.0, what="condition"):
    deadline = time.time() + timeout
    last = None
    while time.time() < deadline:
        last = check()
        if last:
            return last
        time.sleep(0.2)
    raise AssertionError(f"timed out waiting for {what}; last: {last!r}")


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


class WorkerProcess:
    def __init__(self, tmp_path, srv_url, *, name="w1", extra_files=None, token=TOKEN, processors=2,
                 shared=None, source=None, env=None):
        self.checkout = tmp_path / f"{name}-checkout"
        self.checkout.mkdir()
        shutil.copy(source / "pipeline.py", self.checkout / "pipeline.py")
        for fname, text in (extra_files or {}).items():
            (self.checkout / fname).parent.mkdir(parents=True, exist_ok=True)
            (self.checkout / fname).write_text(text)
        self.home = tmp_path / f"{name}-home"
        self.home.mkdir()
        self.log_path = tmp_path / f"{name}.log"
        extra_env = env or {}
        env = dict(os.environ, CEREYAN_HOME=str(self.home))
        env.pop("CEREYAN_TOKEN", None)
        env.update(extra_env)
        argv = [sys.executable, "-m", "cereyan", "worker", str(self.checkout), "--host", srv_url,
                "--name", name, "--processors", str(processors)]
        if token:
            argv += ["--token", token]
        for p in shared or []:
            argv += ["--shared-path", str(p)]
        self.log = open(self.log_path, "ab")
        self.proc = subprocess.Popen(argv, env=env, stdout=self.log, stderr=subprocess.STDOUT, **own_group())

    def read_log(self) -> str:
        return self.log_path.read_text(errors="replace")

    def stop(self):
        if self.proc.poll() is None:
            stop_server(self.proc, timeout=30)
        self.log.close()


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


def worker_view(srv, name):
    return next((w for w in api(srv, "GET", "/api/workers") if w["name"] == name), None)


def online(srv, name):
    view = worker_view(srv, name)
    return view if view and view["state"] == "online" else None


def test_a_server_without_a_token_refuses_workers(isolated_home, project, tmp_path):
    from cereyan import engine

    engine.close_store()
    server = ServerProcess(str(isolated_home), str(project), max_engines=1)
    try:
        w = WorkerProcess(tmp_path, server.info["url"], source=project, token="anything")
        assert w.proc.wait(timeout=30) == 2
        assert "remote workers need a server token" in w.read_log()
        w.stop()
    finally:
        server.stop()


def test_runs_spill_over_to_a_worker_and_record_where_they_ran(srv, workers, tmp_path):
    # The extra flow sits in a package of its own: a file beside pipeline.py
    # would change pipeline's fingerprint, which is drift.
    w = workers(name="w1", extra_files={"extra/flows.py": EXTRA})
    view = wait_for(lambda: online(srv, "w1"), what="w1 online")
    assert view["processors"] == 2
    assert view["drift"] == []
    registered = api(srv, "GET", "/api/events?name=worker.registered&limit=5")["items"]
    assert registered[0]["payload"]["refused"] == ["remote/only_on_worker"]
    gate = str(tmp_path / "gate")
    first = srv.client.run("gated", gate=gate)
    srv.wait_run(first["id"], until=lambda r: r["state"]["type"] == "Running")
    second = srv.client.run("gated", gate=gate)
    running = srv.wait_run(second["id"], until=lambda r: r["state"]["type"] == "Running", timeout=40)
    assert running["host"] == "w1", w.read_log()
    assert srv.client.get_run(first["id"])["host"] == "server"
    queue = api(srv, "GET", "/api/queue")
    hosts = {h["host"]: h for h in queue["processors"]["hosts"]}
    assert hosts["w1"]["busy"] == 1 and hosts["server"]["busy"] == 1
    open(gate, "w").close()
    assert srv.wait_run(second["id"])["state"]["type"] == "Completed"
    timeline = api(srv, "GET", f"/api/workers/{view['id']}/timeline")
    assert [r["run_id"] for r in timeline["runs"]] == [second["id"]]


def test_a_flow_pinned_to_the_server_never_goes_to_a_worker(srv, workers, tmp_path):
    workers(name="w1")
    wait_for(lambda: online(srv, "w1"), what="w1 online")
    gate = str(tmp_path / "gate")
    first = srv.client.run("pinned", gate=gate)
    srv.wait_run(first["id"], until=lambda r: r["state"]["type"] == "Running")
    second = srv.client.run("pinned", gate=gate)
    time.sleep(12)
    assert srv.client.get_run(second["id"])["state"]["type"] == "Scheduled"
    open(gate, "w").close()
    done = srv.wait_run(second["id"])
    assert done["host"] == "server"


def test_a_worker_with_older_code_takes_none_of_that_module(srv, workers, tmp_path, project):
    w = workers(name="w1")
    wait_for(lambda: online(srv, "w1"), what="w1 online")
    with open(w.checkout / "pipeline.py", "a") as fh:
        fh.write("\n# changed on the worker only\n")
    view = wait_for(lambda: (worker_view(srv, "w1") or {}).get("drift") and worker_view(srv, "w1"), what="drift")
    assert view["drift"] == ["pipeline"]
    gate = str(tmp_path / "gate")
    first = srv.client.run("gated", gate=gate)
    srv.wait_run(first["id"], until=lambda r: r["state"]["type"] == "Running")
    second = srv.client.run("gated", gate=gate)
    time.sleep(8)
    assert srv.client.get_run(second["id"])["state"]["type"] == "Scheduled"
    reasons = [r["reason"] for r in api(srv, "GET", "/api/queue")["in_line"]]
    assert reasons == ["no processor with matching code"]
    open(gate, "w").close()
    assert srv.wait_run(second["id"])["host"] == "server"


def test_secrets_results_and_local_files_on_a_worker(srv, workers, tmp_path):
    from cereyan import Variable

    api(srv, "POST", "/api/variables", {"name": "remote/secret", "value": "s3cr3t", "secret": True})
    w = workers(name="w1", shared=[tmp_path / "lake"])
    wait_for(lambda: online(srv, "w1"), what="w1 online")
    gate = str(tmp_path / "gate")
    blocker = srv.client.run("gated", gate=gate)
    srv.wait_run(blocker["id"], until=lambda r: r["state"]["type"] == "Running")
    run = srv.client.run("uses_host")
    done = srv.wait_run(run["id"], timeout=60)
    open(gate, "w").close()
    assert done["state"]["type"] == "Completed", (done["state"].get("message"), w.read_log())
    assert done["host"] == "w1"
    # The result went to the server's storage, not the worker's home.
    storage = os.path.join(srv.home, "storage")
    assert any(not n.startswith("ckpt-") for n in os.listdir(storage))
    assert not (w.home / "storage").exists()
    assert not (w.home / "db.sqlite").exists(), "the worker's engine opened a store of its own"
    # The relative LocalTarget landed in the worker's checkout, and was reported.
    assert (w.checkout / "out" / "local.txt").exists()
    events = api(srv, "GET", f"/api/events?name=run.local_path_on_worker&run_id={run['id']}")["items"]
    assert events and events[0]["payload"]["host"] == "w1"
    assert events[0]["payload"]["path"].endswith(os.path.join("out", "local.txt"))


def test_drain_offline_and_forget(srv, workers):
    w = workers(name="w1")
    view = wait_for(lambda: online(srv, "w1"), what="w1 online")
    api(srv, "POST", f"/api/workers/{view['id']}/drain")
    assert worker_view(srv, "w1")["state"] == "draining"
    with pytest.raises(ApiError) as err:
        api(srv, "DELETE", f"/api/workers/{view['id']}")
    assert err.value.status == 409
    w.proc.kill()
    w.proc.wait()
    wait_for(lambda: worker_view(srv, "w1")["state"] == "offline", timeout=40, what="offline")
    offline = api(srv, "GET", "/api/events?name=worker.offline&limit=5")["items"]
    assert offline and offline[0]["payload"]["name"] == "w1"
    api(srv, "DELETE", f"/api/workers/{view['id']}")
    assert worker_view(srv, "w1") is None


def test_cancel_a_run_on_a_worker(srv, workers, tmp_path):
    workers(name="w1")
    wait_for(lambda: online(srv, "w1"), what="w1 online")
    gate = str(tmp_path / "gate")
    blocker = srv.client.run("gated", gate=gate)
    srv.wait_run(blocker["id"], until=lambda r: r["state"]["type"] == "Running")
    remote = srv.client.run("gated", gate=gate)
    srv.wait_run(remote["id"], until=lambda r: r["state"]["type"] == "Running" and r.get("host") == "w1", timeout=40)
    srv.client.cancel(remote["id"])
    done = srv.wait_run(remote["id"], timeout=60)
    open(gate, "w").close()
    assert done["state"]["type"] == "Cancelled"


def test_a_stale_lease_is_refused(srv):
    gate = "/nonexistent-gate"
    run = srv.client.run("gated", gate=gate, seconds=0.2)
    done = srv.wait_run(run["id"])
    assert done["lease"] >= 1
    import urllib.request

    req = urllib.request.Request(
        srv.info["url"] + "/api/engine/heartbeat",
        data=json.dumps({"engine_id": "old", "run_id": run["id"]}).encode(),
        method="POST",
        headers={"content-type": "application/json", "authorization": f"Bearer {TOKEN}",
                 "x-cereyan-lease": str(done["lease"] - 1) if done["lease"] > 1 else "999"},
    )
    with pytest.raises(urllib.error.HTTPError) as err:
        urllib.request.urlopen(req)
    assert err.value.code == 409
    assert json.loads(err.value.read())["stale_lease"] is True


def test_results_upload_in_chunks_and_read_back_whole(srv):
    import urllib.request

    base = srv.info["url"]

    def put(part, last, data):
        req = urllib.request.Request(
            f"{base}/api/results/ckpt-a-b?part={part}&last={'true' if last else 'false'}", data=data, method="PUT",
            headers={"authorization": f"Bearer {TOKEN}", "content-type": "application/octet-stream"},
        )
        return urllib.request.urlopen(req).status

    assert put(0, False, b"abc") == 204
    assert put(1, False, b"def") == 204
    # Not visible until the last part lands.
    with pytest.raises(ApiError) as missing:
        srv.client._request_bytes("GET", "/api/results/ckpt-a-b")
    assert missing.value.status == 404
    assert put(2, True, b"ghi") == 204
    assert srv.client._request_bytes("GET", "/api/results/ckpt-a-b") == b"abcdefghi"
    with pytest.raises(ApiError) as bad:
        srv.client._request_bytes("PUT", "/api/results/..%2Fsecret.key", b"x", params={"last": "true"})
    assert bad.value.status in (400, 404)


def test_the_engine_side_store_splits_a_large_result(monkeypatch):
    from cereyan import results

    sent = []

    class Recorder:
        def _request_bytes(self, method, path, data=None, params=None):
            sent.append((method, path, len(data or b""), params))
            return b""

    monkeypatch.setattr(results, "CHUNK_BYTES", 10)
    store = results.ServerResultStore(Recorder())
    store.write_encoded("k1", b"x" * 25)
    parts = [p for p in sent if p[0] == "PUT"]
    # A one-line header travels before the 25 bytes, so there are more parts than three.
    assert [p[3]["part"] for p in parts] == list(range(len(parts)))
    assert [p[3]["last"] for p in parts] == ["false"] * (len(parts) - 1) + ["true"]
    assert all(p[2] <= 10 for p in parts)
    assert sum(p[2] for p in parts) > 25
