"""M1.5 ``cereyan check``: import like serve, report findings, never touch the store."""

from __future__ import annotations

import json
import os

CLEAN = '''
from datetime import date
from cereyan import App, flow, task, Cron, Interval

app = App("proj")

@task
def step():
    return 1

@app.flow(schedule=Cron("0 6 * * *", timezone="UTC"))
def nightly(day: date):
    step()

@app.flow(after="nightly", schedule=Interval(3600, timezone="Europe/Istanbul"))
def report(day: date):
    step()

@app.get("/api/ext/ping")
def ping():
    return {"ok": True}

@app.rule(on="run.failed")
def notify(event, run):
    pass
'''

BROKEN_IMPORT = '''
from cereyan import flow
raise RuntimeError("boom at import")
'''

UNKNOWN_UPSTREAM = '''
from cereyan import App
app = App("proj")

@app.flow(after="missing_upstream")
def etl():
    pass
'''

ROUTE_CONFLICT = '''
from cereyan import App
app = App("proj")

@app.get("/api/runs")
def runs():
    return []
'''

BAD_CRON = '''
from cereyan import App, Cron
app = App("proj")

@app.flow(schedule=Cron("0 99 * * *"))
def etl():
    pass
'''

GPU = '''
from cereyan import App
app = App("proj")

@app.flow(resources={"gpu": 1})
def train():
    pass
'''


def report_of(result):
    assert result.stdout.strip().startswith("{"), result.stdout + result.stderr
    return json.loads(result.stdout)


def test_clean_directory_exits_zero_and_leaves_no_store(run_cli, write_module, isolated_home):
    path = write_module("clean", CLEAN)
    result = run_cli("check", str(path.parent), "--now", "2026-09-20T00:00:00Z")
    assert result.returncode == 0, result.stderr
    assert "checked 1 module(s), 2 flow(s), 1 route(s), 1 rule(s), 2 schedule(s)" in result.stdout
    assert "0 error(s), 0 warning(s)" in result.stdout
    assert not os.path.exists(os.path.join(str(isolated_home), "db.sqlite"))
    assert not os.path.exists(os.path.join(str(isolated_home), "server.json"))

    result = run_cli("check", str(path.parent), "--json", "--now", "2026-09-20T00:00:00Z")
    report = report_of(result)
    assert result.returncode == 0 and report["ok"] is True
    assert report["modules"] == ["pipeline"] and report["rules"] == 1
    assert [r["path"] for r in report["routes"]] == ["/api/ext/ping"]
    flows = {f["name"]: f for f in report["flows"]}
    assert flows["nightly"]["schedules"][0]["next"] == [
        "2026-09-20T06:00:00+00:00", "2026-09-21T06:00:00+00:00", "2026-09-22T06:00:00+00:00",
    ]
    # The interval preview is rendered in the schedule's zone, or in UTC where
    # Python has no IANA database (Windows without the `tzdata` package).
    try:
        from zoneinfo import ZoneInfo

        ZoneInfo("Europe/Istanbul")
        offset = "+03:00"
    except Exception:
        offset = "+00:00"
    assert all(t.endswith(offset) for t in flows["report"]["schedules"][0]["next"])
    assert len(flows["report"]["schedules"][0]["next"]) == 3


def test_import_failure_names_the_module(run_cli, write_module):
    path = write_module("broken", BROKEN_IMPORT)
    result = run_cli("check", str(path.parent))
    assert result.returncode == 1
    assert "pipeline: import error: RuntimeError: boom at import" in result.stdout
    report = report_of(run_cli("check", str(path.parent), "--json"))
    assert report["ok"] is False and report["errors"] == 1
    finding = report["findings"][0]
    assert finding["kind"] == "import" and finding["module"] == "pipeline"
    assert "Traceback" in finding["detail"]
    assert report["modules"] == []


def test_upstream_route_and_schedule_errors(run_cli, write_module):
    result = run_cli("check", str(write_module("up", UNKNOWN_UPSTREAM).parent), "--json")
    report = report_of(result)
    assert result.returncode == 1
    assert [f["kind"] for f in report["findings"]] == ["upstream"]
    assert "'missing_upstream'" in report["findings"][0]["message"] and report["findings"][0]["flow"] == "proj/etl"

    result = run_cli("check", str(write_module("route", ROUTE_CONFLICT).parent), "--json")
    report = report_of(result)
    assert result.returncode == 1
    kinds = {f["kind"]: f for f in report["findings"]}
    assert "route" in kinds and "/api/runs" in kinds["route"]["message"]

    result = run_cli("check", str(write_module("cron", BAD_CRON).parent), "--json")
    report = report_of(result)
    assert result.returncode == 1
    errors = [f for f in report["findings"] if f["level"] == "error"]
    assert len(errors) == 1 and errors[0]["kind"] == "schedule" and "cron" in errors[0]["message"]


def test_resource_warning_and_strict(run_cli, write_module):
    directory = write_module("gpu", GPU).parent
    result = run_cli("check", str(directory), "--json")
    report = report_of(result)
    assert result.returncode == 0 and report["ok"] is True and report["warnings"] == 1
    assert report["findings"][0]["kind"] == "resource" and "'gpu'" in report["findings"][0]["message"]
    result = run_cli("check", str(directory), "--json", "--strict")
    assert result.returncode == 1 and report_of(result)["ok"] is False
    (directory / "cereyan.toml").write_text("[resources]\ngpu = 1\n")
    result = run_cli("check", str(directory), "--json", "--strict")
    assert result.returncode == 0 and report_of(result)["warnings"] == 0


def test_missing_directory_and_bad_now(run_cli, tmp_path):
    result = run_cli("check", str(tmp_path / "nope"))
    assert result.returncode == 3 and "is not a directory" in result.stderr
    (tmp_path / "empty").mkdir()
    result = run_cli("check", str(tmp_path / "empty"), "--now", "yesterday")
    assert result.returncode == 3 and "--now" in result.stderr


def test_check_directory_from_python(write_module, isolated_home, capsys):
    from cereyan.check import check_directory

    path = write_module("api", CLEAN)
    report = check_directory(str(path.parent), now="2026-09-20T00:00:00Z")
    assert report["ok"] is True and report["now"] == "2026-09-20T00:00:00+00:00"
    assert {f["name"] for f in report["flows"]} == {"nightly", "report"}
    assert capsys.readouterr().out == ""
    assert not os.path.exists(os.path.join(str(isolated_home), "db.sqlite"))


def test_templated_resource_covered_by_a_pattern_total(run_cli, write_module, tmp_path):
    d = tmp_path / "patterns"
    d.mkdir()
    (d / "pipeline.py").write_text(
        "from cereyan import App\napp = App('pat')\n"
        "@app.flow(resources={'api:{{ tenant }}': 1})\ndef call(tenant: str = 'a'):\n    return tenant\n"
        "@app.flow(resources={'lonely': 1})\ndef other():\n    return 1\n"
    )
    (d / "cereyan.toml").write_text('[resources]\n"api:*" = 2\n')
    result = run_cli("check", str(d), "--json")
    assert result.returncode == 0, result.stderr
    warnings = [f for f in json.loads(result.stdout)["findings"] if f["kind"] == "resource"]
    assert [w["message"] for w in warnings] == ["resource 'lonely' is not in [resources] of cereyan.toml"]
