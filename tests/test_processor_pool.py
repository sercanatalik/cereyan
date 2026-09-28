"""Processors: the live engine pool size, draining, its cap and default, and the queue view."""

from __future__ import annotations

import os
import time

import pytest

from cereyan.client import ApiError
from server_helpers import ServerProcess

PIPELINE = '''
import os
import time
from cereyan import App

app = App("pool")

@app.flow
def gated(gate: str, seconds: float = 30.0):
    # Runs until the gate file appears, so a test decides when each run ends.
    end = time.time() + seconds
    while not os.path.exists(gate) and time.time() < end:
        time.sleep(0.05)
'''


@pytest.fixture
def pool_dir(tmp_path):
    d = tmp_path / "pool"
    d.mkdir()
    (d / "pipeline.py").write_text(PIPELINE)
    return d


def start(isolated_home, pool_dir, **kwargs) -> ServerProcess:
    from cereyan import engine

    engine.close_store()
    return ServerProcess(str(isolated_home), str(pool_dir), **kwargs)


def queue(srv) -> dict:
    return srv.client._request("GET", "/api/queue")


def patch(srv, n: int) -> dict:
    return srv.client._request("PATCH", "/api/settings", body={"max_engines": n})


def running(srv, run_id: int, timeout: float = 30.0) -> dict:
    return srv.wait_run(run_id, until=lambda r: r["state"]["type"] == "Running", timeout=timeout)


def test_a_fresh_server_runs_one_processor(isolated_home, pool_dir, tmp_path):
    srv = start(isolated_home, pool_dir, max_engines=None)
    try:
        settings = srv.client._request("GET", "/api/settings")
        assert settings["max_engines"] == 1
        assert settings["max_engines_source"] == "default"
        assert settings["cpu_cap"] == os.cpu_count()
        gate = str(tmp_path / "gate")
        first = srv.client.run("gated", gate=gate)
        second = srv.client.run("gated", gate=gate)
        running(srv, first["id"])
        time.sleep(1.0)
        assert srv.client.get_run(second["id"])["state"]["type"] == "Scheduled"
        view = queue(srv)
        assert [r["run_id"] for r in view["in_line"]] == [second["id"]]
        assert view["in_line"][0]["position"] == 1
        open(gate, "w").close()
        assert srv.wait_run(second["id"])["state"]["type"] == "Completed"
    finally:
        srv.stop()


def test_adding_a_processor_starts_a_waiting_run(isolated_home, pool_dir, tmp_path):
    srv = start(isolated_home, pool_dir, max_engines=1)
    gate = str(tmp_path / "gate")
    try:
        first = srv.client.run("gated", gate=gate)
        second = srv.client.run("gated", gate=gate)
        running(srv, first["id"])
        assert patch(srv, 2)["max_engines"] == 2
        running(srv, second["id"])
        assert queue(srv)["processors"]["count"] == 2
    finally:
        open(gate, "w").close()
        srv.stop()


def test_removing_a_busy_processor_drains_it(isolated_home, pool_dir, tmp_path):
    srv = start(isolated_home, pool_dir, max_engines=2)
    gates = [str(tmp_path / "gate-a"), str(tmp_path / "gate-b")]
    try:
        runs = [srv.client.run("gated", gate=g) for g in gates]
        for r in runs:
            running(srv, r["id"])
        patch(srv, 1)
        statuses = sorted(e["status"] for e in queue(srv)["processors"]["items"])
        assert statuses == ["draining", "running"]
        for g in gates:
            open(g, "w").close()
        # Nothing was interrupted: both runs complete.
        assert [srv.wait_run(r["id"])["state"]["type"] for r in runs] == ["Completed", "Completed"]
        deadline = time.time() + 20
        while time.time() < deadline:
            if len(queue(srv)["processors"]["items"]) <= 1:
                break
            time.sleep(0.2)
        assert len(queue(srv)["processors"]["items"]) <= 1
    finally:
        srv.stop()


def test_the_count_is_capped_and_kept_across_a_restart(isolated_home, pool_dir):
    srv = start(isolated_home, pool_dir, max_engines=None)
    try:
        cap = srv.client._request("GET", "/api/settings")["cpu_cap"]
        with pytest.raises(ApiError) as err:
            patch(srv, cap + 1)
        assert err.value.status == 422
        with pytest.raises(ApiError):
            patch(srv, 0)
        target = min(3, cap)
        patch(srv, target)
        assert f"max_engines = {target}" in (pool_dir / "cereyan.toml").read_text()
        assert srv.client._request("GET", "/api/settings")["max_engines_source"] == "settings"
    finally:
        srv.stop()
    srv = start(isolated_home, pool_dir, max_engines=None)
    try:
        settings = srv.client._request("GET", "/api/settings")
        assert settings["max_engines"] == target
        assert settings["max_engines_source"] == "toml"
    finally:
        srv.stop()


def test_a_run_for_later_is_joining_the_line(isolated_home, pool_dir, tmp_path):
    srv = start(isolated_home, pool_dir)
    try:
        later = srv.client.run("gated", gate=str(tmp_path / "gate"), delay=600)
        joining = queue(srv)["joining"]
        assert [(j["run_id"], j["kind"]) for j in joining] == [(later["id"], "delayed")]
        assert queue(srv)["in_line"] == []
    finally:
        srv.stop()
