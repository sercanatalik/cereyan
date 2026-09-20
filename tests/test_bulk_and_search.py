"""M1.6 parameter and attribute search, set_attributes, and filter-wide bulk actions."""

from __future__ import annotations

import json
import os
import subprocess
import sys
import time

import pytest

from cereyan.client import ApiError, Client
from server_helpers import ServerProcess

PIPELINE = '''
import time
from cereyan import App, set_attributes, task

app = App("srch")

@task
def note(region: str, n: int):
    set_attributes(region=region, size=n)

@app.flow
def etl(day: str, n: int = 1, region: str = "eu"):
    note(region, n)
    set_attributes(size=n + 1)
    return day

@app.flow
def sleepy(seconds: float = 30.0):
    time.sleep(seconds)

@app.flow
def boom():
    raise ValueError("no")
'''


@pytest.fixture
def srv(isolated_home, tmp_path):
    from cereyan import engine

    engine.close_store()
    d = tmp_path / "srch"
    d.mkdir()
    (d / "pipeline.py").write_text(PIPELINE)
    server = ServerProcess(str(isolated_home), str(d))
    server.client = Client(server.info["url"])
    try:
        yield server
    finally:
        server.stop()


def test_parameter_and_attribute_search(srv):
    c = srv.client
    runs = [c.run("etl", day=f"2026-01-0{i}", n=i, region="eu" if i < 3 else "us") for i in (1, 2, 3)]
    for r in runs:
        srv.wait_run(r["id"])
    by_day = c.runs(params="day=2026-01-02")["items"]
    assert [r["id"] for r in by_day] == [runs[1]["id"]]
    assert [r["id"] for r in c.runs(params="day=2026-01-02,n=2")["items"]] == [runs[1]["id"]]
    assert c.runs(params="day=2026-01-02,n=3")["items"] == []
    # Attributes set from a task and then merged from the flow.
    one = c.get_run(runs[0]["id"])
    assert one["attributes"] == {"region": "eu", "size": 2}
    eu = {r["id"] for r in c.runs(attributes="region=eu")["items"]}
    assert eu == {runs[0]["id"], runs[1]["id"]}
    assert [r["id"] for r in c.runs(attributes="region=us,size=4")["items"]] == [runs[2]["id"]]
    # A key that is not searchable is ignored rather than injected.
    assert len(c.runs(params="day' OR 1=1 --=x")["items"]) == 3
    # The endpoint merges and validates.
    patched = c._request("PATCH", f"/api/runs/{runs[2]['id']}/attributes", body={"owner": "ops", "size": None})
    assert patched["attributes"] == {"region": "us", "owner": "ops"}
    with pytest.raises(ApiError) as err:
        c._request("PATCH", f"/api/runs/{runs[2]['id']}/attributes", body={"bad name": 1})
    assert err.value.status == 422
    with pytest.raises(ApiError) as err:
        c._request("PATCH", "/api/runs/999999/attributes", body={"a": 1})
    assert err.value.status == 404


def test_bulk_actions_count_then_act(srv):
    c = srv.client
    done = [c.run("etl", day=f"2026-02-0{i}", n=i) for i in (1, 2)]
    for r in done:
        srv.wait_run(r["id"])
    failed = c.run("boom")
    srv.wait_run(failed["id"])
    sleeping = [c.run("sleepy", seconds=30) for _ in range(2)]
    for r in sleeping:
        srv.wait_run(r["id"], until=lambda x: x["state"]["type"] in ("Running", "Scheduled", "Pending"))
    # Dry run by default, and cancel counts only what is still going.
    count = c._request("POST", "/api/runs/bulk", body={"filter": {"flow": "sleepy"}, "action": "cancel"})
    assert (count["dry_run"], count["matched"], count["affected"]) == (True, 2, 2)
    assert all(not c.get_run(r["id"])["state"]["type"] in ("Cancelled",) for r in sleeping)
    applied = c._request("POST", "/api/runs/bulk", body={"filter": {"flow": "sleepy"}, "action": "cancel", "dry_run": False})
    assert applied["affected"] == 2
    for r in sleeping:
        assert srv.wait_run(r["id"], timeout=30)["state"]["type"] == "Cancelled"
    # Rerun creates copies of the terminal etl runs with their parameters.
    rerun = c._request("POST", "/api/runs/bulk", body={"filter": {"flow": "etl"}, "action": "rerun", "dry_run": False})
    assert (rerun["matched"], rerun["affected"]) == (2, 2)
    copies = [r for r in c.runs(flow="etl", limit=50)["items"] if r["created_by"] == "bulk"]
    assert sorted(r["parameters"]["day"] for r in copies) == ["2026-02-01", "2026-02-02"]
    for r in copies:
        srv.wait_run(r["id"])
    # Delete everything failed.
    gone = c._request("POST", "/api/runs/bulk", body={"filter": {"state_type": "Failed"}, "action": "delete", "dry_run": False})
    assert gone["affected"] == 1 and c.runs(state_type="Failed")["items"] == []
    with pytest.raises(ApiError) as err:
        c._request("POST", "/api/runs/bulk", body={"action": "explode"})
    assert err.value.status == 422


def test_set_attributes_offline_and_outside_a_run(isolated_home, write_module, run_cli):
    from cereyan import engine, set_attributes
    from cereyan.exceptions import CereyanError

    engine.close_store()
    path = write_module("srch", PIPELINE)
    result = run_cli("run", f"{path}:etl", "--param", "day=2026-03-01", "--param", "n=5")
    assert result.returncode == 0, result.stderr
    store = engine.get_store()
    try:
        run = json.loads(store.list_runs(json.dumps({"limit": 1})))["items"][0]
        assert run["attributes"] == {"region": "eu", "size": 6}
        assert json.loads(store.list_runs(json.dumps({"attributes": ["size=6"]})))["items"][0]["id"] == run["id"]
        assert json.loads(store.list_runs(json.dumps({"params": ["day=2026-03-01"]})))["items"]
        assert json.loads(store.list_runs(json.dumps({"params": ["day=2026-03-02"]})))["items"] == []
    finally:
        engine.close_store()
    with pytest.raises(CereyanError, match="running flow"):
        set_attributes(x=1)
