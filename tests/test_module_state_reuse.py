"""Module-level state persists across the runs one engine serves.

`docs/guides/fetch-from-an-api.md` tells readers to create an HTTP client once at
module level and let the warm engine reuse it. That advice is only true because
`python/cereyan/engine/child.py` imports the module once, outside the work loop.
These tests pin it so a later change cannot quietly reload per run and turn every
documented example into a fresh connection pool.
"""

from __future__ import annotations

import pytest

from server_helpers import ServerProcess

PIPELINE = '''
from cereyan import App, artifacts

app = App("warm")

class _Client:
    """Stands in for the httpx.Client the guide creates at module level."""

    def __init__(self):
        self.requests = 0

    def get(self, path):
        self.requests += 1
        return path

_CLIENT = _Client()  # constructed once per import, not once per run

@app.flow
def reuse():
    _CLIENT.get("orders")
    artifacts.create_progress(_CLIENT.requests, key="requests")
    return _CLIENT.requests

@app.flow(isolated=True)
def fresh():
    _CLIENT.get("orders")
    artifacts.create_progress(_CLIENT.requests, key="requests")
    return _CLIENT.requests
'''


@pytest.fixture
def warm(isolated_home, tmp_path):
    from cereyan import engine

    engine.close_store()
    d = tmp_path / "warm"
    d.mkdir()
    (d / "pipeline.py").write_text(PIPELINE)
    srv = ServerProcess(str(isolated_home), str(d))
    try:
        yield srv
    finally:
        srv.stop()


def start(srv, flow):
    fid = next(f["id"] for f in srv.client.flows() if f["name"] == flow and f["project"] == "warm")
    return srv.client._request("POST", f"/api/flows/{fid}/runs", body={})


def requests_seen(srv):
    page = srv.client._request("GET", "/api/artifacts", params={"key": "requests"})
    return [a["data"]["value"] for a in page["items"]]  # newest first


def test_module_level_client_is_reused_across_runs(warm):
    pids = set()
    for _ in range(3):
        run = warm.wait_run(start(warm, "reuse")["id"])
        assert run["state"]["type"] == "Completed"
        pids.add(run["engine_pid"])

    # One engine, one import, so the client counts every run's request.
    assert None not in pids and len(pids) == 1
    assert requests_seen(warm) == [3, 2, 1]


def test_isolated_flow_gets_a_fresh_import_each_run(warm):
    pids = []
    for _ in range(2):
        run = warm.wait_run(start(warm, "fresh")["id"])
        assert run["state"]["type"] == "Completed"
        pids.append(run["engine_pid"])

    # A fresh process each run means a fresh import and a fresh client.
    assert None not in pids and pids[0] != pids[1]
    assert requests_seen(warm) == [1, 1]
