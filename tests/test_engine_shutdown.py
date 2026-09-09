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
    served.wait_idle()

    served.stop(kill_engines=False)

    leaked = _gone(pids)
    assert not leaked, f"engines outlived the server: {leaked}"


def _kill_the_server_under(served) -> list[int]:
    """Warm an engine, let it go idle, then SIGKILL the server so it never says exit."""
    flows = served.client._request("GET", "/api/flows")
    flow_id = (flows if isinstance(flows, list) else flows["items"])[0]["id"]
    run = served.client._request("POST", f"/api/flows/{flow_id}/runs", body={"parameters": {}})
    served.wait_run(run["id"])
    pids = served.engine_pids()
    assert pids, "the run should have warmed an engine"
    served.wait_idle()
    served.proc.kill()
    served.proc.wait()
    return pids


def test_idle_engine_gives_up_when_the_server_is_killed(served):
    """A SIGKILLed server never says exit, so the engine's own idle timeout must end it.

    This is existing behaviour: `get_work` in crates/py/src/client.rs returns a synthetic
    ``{"exit": true}`` once IDLE_RETRY (30 s) has passed with the server unreachable. It is
    pinned here so the graceful path above cannot become the only thing keeping engines from
    outliving their server.

    Correctness only: that the engine gives up at all. The ceiling it gives up *within* is
    asserted by the performance test below, because a wall-clock bound with 15 s of headroom
    fails on a loaded shared runner without saying anything about the product.
    """
    pids = _kill_the_server_under(served)

    leaked = _gone(pids, timeout=150.0)
    assert not leaked, f"idle engines never gave up: {leaked}"
    assert "server unreachable" in served.read_log()


@pytest.mark.performance
def test_idle_engine_gives_up_within_the_documented_bound(served):
    """The give-up is bounded by LONG_POLL_TIMEOUT (40 s), not by IDLE_RETRY.

    60 s against a 40 s bound. This waited 90 s, which was exactly the client timeout that
    used to defeat the bound, so the test raced the bug instead of catching it. Margin is
    what makes this an assertion rather than a coin flip — and it holds only on calibrated
    hardware, which is why `just bench` runs it and `just test` does not.
    """
    pids = _kill_the_server_under(served)

    leaked = _gone(pids, timeout=60.0)
    assert not leaked, f"idle engines outran the 40 s bound: {leaked}"
