"""1.1 cross-run artifacts listing and engine niceness for negative priority."""

from __future__ import annotations

import os
import sys

import pytest

from server_helpers import ServerProcess

PIPELINE = '''
import os
from cereyan import App, artifacts

app = App("arts")

@app.flow
def etl(n: int = 1):
    artifacts.create_progress(n, key="rows")
    artifacts.create_markdown(f"# run {n}", key="report")
    return n

@app.flow(priority=-10)
def lazy():
    return os.getpriority(os.PRIO_PROCESS, 0) if hasattr(os, "getpriority") else 0

@app.flow
def eager():
    return os.getpriority(os.PRIO_PROCESS, 0) if hasattr(os, "getpriority") else 0
'''


@pytest.fixture
def arts(isolated_home, tmp_path):
    from cereyan import engine

    engine.close_store()
    d = tmp_path / "arts"
    d.mkdir()
    (d / "pipeline.py").write_text(PIPELINE)
    srv = ServerProcess(str(isolated_home), str(d))
    try:
        yield srv
    finally:
        srv.stop()


def start(srv, flow, **params):
    fid = next(f["id"] for f in srv.client.flows() if f["name"] == flow and f["project"] == "arts")
    return srv.client._request("POST", f"/api/flows/{fid}/runs", body={"parameters": params})


def test_key_history_and_pagination(arts):
    runs = []
    for i in range(1, 6):  # sequential, so artifact ids follow n
        r = start(arts, "etl", n=i)
        assert arts.wait_run(r["id"])["state"]["type"] == "Completed"
        runs.append(r)
    page = arts.client._request("GET", "/api/artifacts", params={"key": "rows"})
    assert [a["data"]["value"] for a in page["items"]] == [5, 4, 3, 2, 1]
    assert page["items"][0]["run_name"] == runs[-1]["name"] and page["items"][0]["flow_name"] == "etl"
    assert page["items"][0]["project"] == "arts" and page["next_cursor"] is None
    first = arts.client._request("GET", "/api/artifacts", params={"limit": 2})
    assert len(first["items"]) == 2 and first["next_cursor"]
    second = arts.client._request("GET", "/api/artifacts", params={"limit": 2, "after": first["next_cursor"]})
    ids_first = {a["id"] for a in first["items"]}
    assert len(second["items"]) == 2 and not ids_first & {a["id"] for a in second["items"]}
    assert all(a["kind"] == "markdown" for a in arts.client._request("GET", "/api/artifacts", params={"kind": "markdown"})["items"])
    assert arts.client._request("GET", "/api/artifacts", params={"flow": "nope"})["items"] == []
    by_run = arts.client._request("GET", "/api/artifacts", params={"run_id": runs[0]["id"]})
    assert {a["key"] for a in by_run["items"]} == {"rows", "report"}


@pytest.mark.skipif(not hasattr(os, "getpriority") or sys.platform.startswith("win"), reason="Unix niceness")
def test_negative_priority_lowers_engine_niceness(arts):
    lazy = arts.wait_run(start(arts, "lazy")["id"])
    eager = arts.wait_run(start(arts, "eager")["id"])
    assert lazy["state"]["type"] == "Completed" and eager["state"]["type"] == "Completed"
    assert lazy["priority"] == -10
    assert lazy["engine_pid"] != eager["engine_pid"]
    # The engine that ran `lazy` reported niceness 10; `eager` ran at the default.
    lazy_result = arts.client._request("GET", f"/api/runs/{lazy['id']}")
    engines = arts.client._request("GET", "/api/server")["engines"]
    nices = {e["pid"]: e.get("nice", 0) for e in engines}
    assert nices.get(lazy["engine_pid"]) == 10 and nices.get(eager["engine_pid"], 0) == 0
    assert lazy_result["state"]["type"] == "Completed"
