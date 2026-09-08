"""Large engine reports are delivered in bounded chunks (regression for the 2 MB body stall)."""

from __future__ import annotations

import textwrap
import time

import pytest

from server_helpers import ServerProcess

MODULE = textwrap.dedent(
    """
    from cereyan import App, task, get_run_logger
    app = App("batches")

    @task
    def noop(i: int = 0):
        return i

    @app.flow
    def many_tasks(n: int = 2000):
        for i in range(n):
            noop(i)

    @app.flow
    def many_logs(n: int = 20000):
        log = get_run_logger()
        pad = "x" * 80
        for i in range(n):
            log.info("line %d with padding to grow the report body %s", i, pad)
    """
)


@pytest.fixture
def batches(isolated_home, write_module):
    from cereyan import engine

    engine.close_store()
    srv = ServerProcess(str(isolated_home), str(write_module("batches", MODULE).parent))
    try:
        yield srv
    finally:
        srv.stop()


def _start(server: ServerProcess, name: str, **params) -> dict:
    flow_id = next(f["id"] for f in server.client.flows() if f["name"] == name)
    return server.client._request("POST", f"/api/flows/{flow_id}/runs", body={"parameters": params})


def test_large_task_and_log_reports_complete(batches):
    t0 = time.time()
    run = _start(batches, "many_tasks", n=2000)
    done = batches.wait_run(run["id"], timeout=60)
    assert done["state"]["name"] == "Completed"
    assert time.time() - t0 < 30, "2000 tasks should report in well under the retry ceiling"
    assert len(batches.client.task_runs(run["id"])) == 2000

    run = _start(batches, "many_logs", n=20000)
    done = batches.wait_run(run["id"], timeout=60)
    assert done["state"]["name"] == "Completed"
    page = batches.client.logs(run["id"], after=19990, limit=5)
    assert page["items"], "last log lines were not delivered"
