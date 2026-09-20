"""M3.4 keyed resources: templated names, pattern totals, tag limits, and the
snapshot, served and offline."""

from __future__ import annotations

import time

import pytest

from server_helpers import ServerProcess

PIPELINE = '''
import time
from cereyan import App, task

app = App("keyed")

@app.flow(resources={"api:{{ tenant }}": 1})
def call_api(tenant: str = "acme", seconds: float = 2.0):
    time.sleep(seconds)
    return tenant

@app.flow(tags=["gpu"])
def train(seconds: float = 2.0):
    time.sleep(seconds)

@app.flow(tags=["gpu"])
def score(seconds: float = 0.2):
    time.sleep(seconds)

@task(resources={"db:{shard}": 1})
def touch(shard: str, seconds: float):
    time.sleep(seconds)
    return shard

@app.flow
def shards(shard: str = "a", seconds: float = 0.2):
    return touch(shard, seconds)
'''

TOML = '''
[resources]
"api:*" = 1
"tag:gpu" = 1
"db:*" = 1
'''


@pytest.fixture
def srv(isolated_home, tmp_path):
    from cereyan import engine

    engine.close_store()
    d = tmp_path / "keyed"
    d.mkdir()
    (d / "pipeline.py").write_text(PIPELINE)
    (d / "cereyan.toml").write_text(TOML)
    server = ServerProcess(str(isolated_home), str(d))
    try:
        yield server
    finally:
        server.stop()


def test_tenant_keys_serialize_per_tenant_only(srv):
    c = srv.client
    first = c.run("call_api", tenant="acme", seconds=3)
    srv.wait_run(first["id"], until=lambda r: r["state"]["type"] == "Running")
    second = c.run("call_api", tenant="acme", seconds=0.1)
    other = c.run("call_api", tenant="globex", seconds=0.1)
    waiting = srv.wait_run(second["id"], timeout=10, until=lambda r: r["state"]["name"] == "AwaitingResource")
    assert waiting["state"]["details"]["resource"] == "api:acme"
    assert srv.wait_run(other["id"])["state"]["type"] == "Completed"
    assert c.get_run(second["id"])["state"]["name"] == "AwaitingResource"
    snapshot = c.settings()["resources"]
    assert snapshot["api:*"]["total"] == 1.0
    assert snapshot["api:acme"] == {"total": 1.0, "used": 1.0, "pattern": "api:*"}
    srv.wait_run(first["id"])
    assert srv.wait_run(second["id"])["state"]["type"] == "Completed"
    events = c.events(kind="resource.exhausted")
    assert any(e["run_id"] == second["id"] for e in events)


def test_tag_limit_spans_flows(srv):
    c = srv.client
    training = c.run("train", seconds=3)
    srv.wait_run(training["id"], until=lambda r: r["state"]["type"] == "Running")
    scoring = c.run("score")
    waiting = srv.wait_run(scoring["id"], timeout=10, until=lambda r: r["state"]["name"] == "AwaitingResource")
    assert waiting["state"]["details"]["resource"] == "tag:gpu"
    srv.wait_run(training["id"])
    assert srv.wait_run(scoring["id"])["state"]["type"] == "Completed"


def test_task_level_template_served(srv):
    c = srv.client
    a1 = c.run("shards", shard="a", seconds=2)
    srv.wait_run(a1["id"], until=lambda r: r["state"]["type"] == "Running")
    time.sleep(0.5)
    a2 = c.run("shards", shard="a", seconds=0.1)
    b = c.run("shards", shard="b", seconds=0.1)
    assert srv.wait_run(b["id"])["state"]["type"] == "Completed"
    assert c.get_run(a2["id"])["state"]["type"] != "Completed" or c.get_run(a2["id"])["start_time"] >= a1["start_time"] + 1_500_000
    assert srv.wait_run(a2["id"])["state"]["type"] == "Completed"
    assert c.get_run(a2["id"])["start_time"] is not None
    snapshot = c.settings()["resources"]
    assert "db:a" in snapshot and snapshot["db:a"]["pattern"] == "db:*"


def test_offline_task_template_keys_by_parameter(store):
    from cereyan import flow, task

    peak = {}
    active = {}

    # Task names render from the run's parameters, so `lane` is the flow's.
    @task(resources={"lane:{{ lane }}": 1})
    def work(i: int):
        active["x"] = active.get("x", 0) + 1
        peak["x"] = max(peak.get("x", 0), active["x"])
        time.sleep(0.05)
        active["x"] -= 1
        return i

    @flow
    def f(lane: str = "x"):
        return [fut.result() for fut in work.map([1, 2, 3])]

    assert f(lane="x") == [1, 2, 3]
    assert peak == {"x": 1}
