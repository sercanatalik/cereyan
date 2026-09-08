"""Engines must not outlive a graceful server shutdown."""

from __future__ import annotations

import time

import pytest

from server_helpers import ServerProcess, is_alive, kill

PIPELINE = '''
from cereyan import App, task

app = App("shutdown")

@task
def step():
    return 1

@app.flow
def work():
    step()
    return "done"
'''



def _gone(pids: list[int], timeout: float = 5.0) -> list[int]:
    deadline = time.time() + timeout
    while time.time() < deadline:
        left = [p for p in pids if is_alive(p)]
        if not left:
            return []
        time.sleep(0.1)
    return [p for p in pids if is_alive(p)]


@pytest.fixture
def served(isolated_home, tmp_path):
    from cereyan import engine

    engine.close_store()
    d = tmp_path / "shutdown"
    d.mkdir()
    (d / "pipeline.py").write_text(PIPELINE)
    srv = ServerProcess(str(isolated_home), str(d))
    from cereyan.client import Client

    srv.client = Client(srv.info["url"])
    yield srv
    # The test stops the server itself; clean up anything it left behind.
    for pid in srv.engine_pids() if srv.proc.poll() is None else []:
        kill(pid)
    if srv.proc.poll() is None:
        srv.stop()


def test_graceful_stop_leaves_no_engine(served):
    flows = served.client._request("GET", "/api/flows")
    flow_id = (flows if isinstance(flows, list) else flows["items"])[0]["id"]
    run = served.client._request("POST", f"/api/flows/{flow_id}/runs", body={"parameters": {}})
    served.wait_run(run["id"])
    pids = served.engine_pids()
    assert pids, "the run should have warmed an engine"

    served.stop(kill_engines=False)

    leaked = _gone(pids)
    assert not leaked, f"engines outlived the server: {leaked}"


def test_idle_engine_gives_up_when_the_server_is_killed(served):
    """A SIGKILLed server never says exit, so the engine's own idle timeout must end it.

    This is existing behaviour: `get_work` in crates/py/src/client.rs returns a synthetic
    ``{"exit": true}`` once IDLE_RETRY (30 s) has passed with the server unreachable. It is
    pinned here so the graceful path above cannot become the only thing keeping engines from
    outliving their server.
    """
    flows = served.client._request("GET", "/api/flows")
    flow_id = (flows if isinstance(flows, list) else flows["items"])[0]["id"]
    run = served.client._request("POST", f"/api/flows/{flow_id}/runs", body={"parameters": {}})
    served.wait_run(run["id"])
    pids = served.engine_pids()
    assert pids, "the run should have warmed an engine"

    served.proc.kill()
    served.proc.wait()

    leaked = _gone(pids, timeout=90.0)
    assert not leaked, f"idle engines never gave up: {leaked}"
    assert "server unreachable" in served.read_log()
