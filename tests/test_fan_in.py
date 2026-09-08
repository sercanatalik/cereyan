"""1.3 keyed fan-in: a downstream runs once every upstream completed the same batch."""

from __future__ import annotations

import pytest

from cereyan import App
from server_helpers import ServerProcess

PIPELINE = '''
from datetime import date
from cereyan import App

app = App("fanin")

@app.flow
def sales(day: date, fail: bool = False):
    if fail:
        raise ValueError("sales broke")
    return f"sales {day}"

@app.flow
def inventory(day: date):
    return f"inventory {day}"

@app.flow(after=["sales", "inventory"], batch_key="day")
def report(day: date):
    return f"report {day}"

@app.flow(after="sales")
def mirror(day: date):
    return f"mirror {day}"

@app.flow(after=["sales", "ghost"], batch_key="day")
def broken(day: date):
    return "never"
'''


@pytest.fixture
def fanin(isolated_home, tmp_path):
    from cereyan import engine

    engine.close_store()
    d = tmp_path / "fanin"
    d.mkdir()
    (d / "pipeline.py").write_text(PIPELINE)
    srv = ServerProcess(str(isolated_home), str(d))
    try:
        yield srv
    finally:
        srv.stop()


def fid(srv, name):
    return next(f["id"] for f in srv.client.flows() if f["name"] == name and f["project"] == "fanin")


def start(srv, name, **params):
    run = srv.client._request("POST", f"/api/flows/{fid(srv, name)}/runs", body={"parameters": params})
    return srv.wait_run(run["id"])


def runs_of(srv, name):
    return srv.client._request("GET", "/api/runs", params={"flow": name, "project": "fanin", "limit": 50})["items"]


def wait_runs(srv, name, count, timeout=10):
    import time

    deadline = time.time() + timeout
    while time.time() < deadline:
        items = runs_of(srv, name)
        if len(items) >= count:
            return items
        time.sleep(0.05)
    return runs_of(srv, name)


def test_batch_completes_once_per_key(fanin):
    assert start(fanin, "sales", day="2026-09-06")["state"]["type"] == "Completed"
    import time

    time.sleep(0.5)
    assert runs_of(fanin, "report") == []  # inventory still missing
    assert len(wait_runs(fanin, "mirror", 1)) == 1  # single-upstream dependency unchanged
    inv = start(fanin, "inventory", day="2026-09-06")
    reports = wait_runs(fanin, "report", 1)
    assert len(reports) == 1
    report = reports[0]
    assert report["parameters"]["day"] == "2026-09-06" and report["created_by"] == f"run:{inv['id']}"
    assert report["name"].startswith("report-day-2026-09-06")
    assert fanin.wait_run(report["id"])["state"]["type"] == "Completed"
    events = fanin.client._request("GET", "/api/events", params={"name": "flow.fan_in"})["items"]
    assert len(events) == 1 and events[0]["payload"]["value"] == "2026-09-06" and len(events[0]["payload"]["upstream_runs"]) == 2
    # Repeating an upstream for the same day does not create a second report.
    start(fanin, "sales", day="2026-09-06")
    time.sleep(0.5)
    assert len(runs_of(fanin, "report")) == 1
    assert len(wait_runs(fanin, "mirror", 2)) == 2
    # Different days do not mix.
    start(fanin, "inventory", day="2026-09-07")
    time.sleep(0.5)
    assert len(runs_of(fanin, "report")) == 1


def test_failed_upstream_blocks_until_it_succeeds(fanin):
    start(fanin, "inventory", day="2026-09-08")
    failed = start(fanin, "sales", day="2026-09-08", fail=True)
    assert failed["state"]["type"] == "Failed"
    import time

    time.sleep(0.5)
    assert runs_of(fanin, "report") == []
    start(fanin, "sales", day="2026-09-08")
    reports = wait_runs(fanin, "report", 1)
    assert len(reports) == 1 and reports[0]["parameters"]["day"] == "2026-09-08"


def test_summary_and_unknown_upstream(fanin):
    summary = next(f for f in fanin.client.flows() if f["name"] == "report")
    assert summary["upstreams"] == ["sales", "inventory"] and summary["batch_key"] == "day"
    assert summary["triggered_by"] == "sales"
    sales = next(f for f in fanin.client.flows() if f["name"] == "sales")
    assert set(sales["triggers"]) >= {"report", "mirror"}
    broken = next(f for f in fanin.client.flows() if f["name"] == "broken")
    assert broken["error"] == "unknown upstream flow 'ghost'"


def test_registration_errors():
    app = App("fanin_errors")
    with pytest.raises(ValueError) as err:
        @app.flow(after=["a", "b"])
        def no_key(day: str):
            pass
    assert "batch_key" in str(err.value)
    with pytest.raises(ValueError):
        @app.flow(after="a", batch_key="bad key")
        def bad_key(day: str):
            pass
    with pytest.raises(ValueError):
        @app.flow(batch_key="day")
        def key_alone(day: str):
            pass

    @app.flow(after=("a", {"day": "{{ run.parameters.day }}"}))
    def legacy(day: str):
        pass

    assert legacy.after == {"flow": "a", "flows": ["a"], "key": None, "parameters": {"day": "{{ run.parameters.day }}"}}
