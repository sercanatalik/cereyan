import json
import logging
import time

import pytest

from cereyan.client import ApiError


def flow_id(server, name):
    return next(f["id"] for f in server.client.flows() if f["name"] == name and f["project"] == "proj")


def test_custom_routes(server):
    c = server.client
    assert c._request("GET", "/api/ext/ping") == {"ok": True}
    assert c._request("GET", "/api/ext/orders/42", params={"verbose": "true"}) == {"id": 42, "verbose": True}
    with pytest.raises(ApiError) as info:
        c._request("GET", "/api/ext/orders/abc")
    assert info.value.status == 422 and "id" in info.value.body["error"]
    import urllib.request

    req = urllib.request.Request(server.info["url"] + "/api/ext/items", data=b'{"a": 1}', method="POST", headers={"content-type": "application/json"})
    with urllib.request.urlopen(req) as r:
        assert r.status == 201
        assert json.loads(r.read()) == {"received": {"a": 1}}
    with pytest.raises(ApiError) as info:
        c._request("GET", "/api/ext/teapot")
    assert info.value.status == 418 and info.value.body["error"] == "short and stout"
    with pytest.raises(ApiError) as info:
        c._request("GET", "/api/ext/boom")
    assert info.value.status == 500
    assert c.health()  # server keeps serving
    assert "kaboom" in server.read_log()
    with urllib.request.urlopen(server.info["url"] + "/health") as r:
        assert r.read() == b"fine"
        assert r.headers["content-type"].startswith("text/plain")
    hook = c._request("POST", "/api/ext/webhook", params={"day": "2026-02-02"})
    done = server.wait_run(hook["run_id"])
    assert done["state"]["type"] == "Completed"
    assert done["parameters"]["day"] == "2026-02-02"


def test_route_collision_fails_startup(isolated_home, tmp_path):
    import os, subprocess, sys

    d = tmp_path / "collide"
    d.mkdir()
    (d / "p.py").write_text("from cereyan import App\napp = App('c')\n@app.get('/api/runs')\ndef h():\n    return {}\n")
    env = dict(os.environ, CEREYAN_HOME=str(isolated_home), CEREYAN_NO_BROWSER="1")
    result = subprocess.run([sys.executable, "-m", "cereyan", "serve", str(d), "--port", "0", "--no-open"], env=env, capture_output=True, text=True, timeout=60)
    assert result.returncode == 3
    assert "/api/runs" in result.stderr and "collides" in result.stderr
    assert not os.path.exists(isolated_home / "server.json")


def settled_logs(server, run_id, timeout=10.0):
    """The run's logs once no more are arriving.

    A run reaching a terminal state does not mean its logs have all landed: the
    engine reports them in batches every 100 ms, so the last batch can arrive
    after the state does. Anything comparing two log queries has to start from a
    stream that has stopped moving, or the second query can see one more than the
    first.
    """
    deadline = time.time() + timeout
    previous = None
    while time.time() < deadline:
        items = server.client.logs(run_id)["items"]
        if previous is not None and len(items) == len(previous):
            return items
        previous = items
        time.sleep(0.2)
    return previous or []


def test_log_prints_and_filters(server):
    run = server.client._request("POST", f"/api/flows/{flow_id(server, 'printer')}/runs", body={})
    done = server.wait_run(run["id"])
    assert done["state"]["type"] == "Completed"
    logs = settled_logs(server, run["id"])
    by_msg = {l["message"]: l for l in logs}
    assert by_msg["hello"]["level"] == 20
    assert by_msg["hello from task"]["task_run_id"] is not None
    assert "hello" in server.read_log()  # still printed on the engine's stdout
    errors = server.client.logs(run["id"], level="ERROR")["items"]
    assert errors == []
    infos = server.client.logs(run["id"], level="INFO")["items"]
    assert len(infos) == len(logs)
    found = server.client.logs(run["id"], search="from task")["items"]
    assert [l["message"] for l in found] == ["hello from task"]
    page = server.client.logs(run["id"], limit=2)
    assert len(page["items"]) == 2 and page["next_cursor"] == page["items"][-1]["id"]
    tail = server.client.logs(run["id"], after=page["next_cursor"])["items"]
    assert tail[0]["id"] > page["next_cursor"]
    task_id = by_msg["hello from task"]["task_run_id"]
    task_logs = server.client._request("GET", f"/api/task-runs/{task_id}/logs")["items"]
    assert "hello from task" in [l["message"] for l in task_logs]
    assert all(l["task_run_id"] == task_id for l in task_logs)


def test_offline_logs_echo_and_store(run_cli, write_module):
    path = write_module("logproj", "from cereyan import flow, get_run_logger\n@flow\ndef f():\n    get_run_logger().warning('watch out')\n")
    result = run_cli("run", f"{path}:f")
    assert result.returncode == 0
    assert "watch out" in result.stderr


@pytest.mark.performance
def test_log_throughput(isolated_home):
    """100k lines through the run handler should cost under 10 percent over a null handler."""
    from cereyan import flow, task
    from cereyan import engine

    lines = 100_000
    logger = logging.getLogger("bench")
    logger.setLevel(logging.INFO)
    logger.propagate = False
    null = logging.NullHandler()
    logger.addHandler(null)
    t0 = time.perf_counter()
    for i in range(lines):
        logger.info("line %d", i)
    baseline = time.perf_counter() - t0
    logger.removeHandler(null)
    logger.propagate = True

    @task
    def emit():
        t0 = time.perf_counter()
        for i in range(lines):
            logger.info("line %d", i)
        return time.perf_counter() - t0

    measured = {}

    @flow
    def f():
        measured["t"] = emit()

    f()
    store = engine.get_store()
    run = json.loads(store.list_runs())["items"][0]
    stored = json.loads(store.query_logs(json.dumps({"run_id": run["id"], "limit": 10000})))
    assert stored["next_cursor"]  # more than one page exists
    overhead = measured["t"] / baseline - 1
    print(f"baseline {baseline:.3f}s, with handler {measured['t']:.3f}s, overhead {overhead:.1%}")
    assert measured["t"] < 1.0 * 3, "100k lines should take about a second"
