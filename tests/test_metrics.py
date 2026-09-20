"""M1.3 Prometheus metrics: the scrape body, its histograms, the history ring, and the public exemption."""

from __future__ import annotations

import json
import re
import urllib.error
import urllib.request

import pytest

from server_helpers import ServerProcess

TOKEN = "metrics-token"


def get(url, headers=None):
    req = urllib.request.Request(url, headers=headers or {})
    try:
        with urllib.request.urlopen(req, timeout=5) as resp:
            return resp.status, resp.headers.get("content-type", ""), resp.read().decode()
    except urllib.error.HTTPError as exc:
        return exc.code, exc.headers.get("content-type", ""), exc.read().decode()


def value(body, series):
    match = re.search(rf"^{re.escape(series)} (\S+)$", body, re.M)
    assert match, f"{series} not in:\n{body}"
    return float(match.group(1))


def test_scrape_after_a_run(server):
    c = server.client
    run = c.run("etl", day="2026-09-06")
    server.wait_run(run["id"])
    status, ctype, body = get(server.info["url"] + "/api/metrics")
    assert status == 200 and ctype == "text/plain; version=0.0.4; charset=utf-8"
    assert value(body, 'cereyan_runs{state="Completed"}') == 1
    assert value(body, 'cereyan_flow_runs{project="proj",flow="etl",state="Completed"}') == 1
    assert value(body, 'cereyan_task_runs{state="Completed"}') >= 1
    assert value(body, "cereyan_queue_depth") == 0
    assert value(body, 'cereyan_engines{status="idle"}') + value(body, 'cereyan_engines{status="busy"}') >= 1
    assert value(body, "cereyan_engines_max") >= 1
    assert value(body, 'cereyan_info{version="%s"}' % server.info["version"]) == 1
    assert value(body, "cereyan_database_bytes") > 0
    commits = value(body, "cereyan_store_commits_total")
    assert commits > 0
    # Histograms: cumulative buckets end at +Inf equal to the count.
    for name in ("cereyan_store_commit_seconds", "cereyan_schedule_start_delay_seconds", "cereyan_resource_wait_seconds"):
        count = value(body, f"{name}_count")
        assert value(body, f'{name}_bucket{{le="+Inf"}}') == count
        assert f"# TYPE {name} histogram" in body
    assert value(body, "cereyan_store_commit_seconds_count") > 0
    assert value(body, "cereyan_schedules") >= 0
    # Every family has HELP and TYPE.
    families = set(re.findall(r"^# TYPE (\S+)", body, re.M))
    assert {"cereyan_runs", "cereyan_flow_runs", "cereyan_rule_firings_total", "cereyan_uptime_seconds", "cereyan_store_write_queue"} <= families
    assert set(re.findall(r"^# HELP (\S+)", body, re.M)) == families

    history = json.loads(get(server.info["url"] + "/api/metrics/history")[2])
    assert history["interval_secs"] == 5 and history["samples"]
    first = history["samples"][0]
    assert set(first) == {"at", "queued", "running", "engines_busy"} and first["queued"] == 0


def test_metrics_public_exemption(isolated_home, project_dir):
    from cereyan import engine

    engine.close_store()
    srv = ServerProcess(str(isolated_home), str(project_dir), env={"CEREYAN_TOKEN": TOKEN})
    try:
        url = srv.info["url"]
        assert get(url + "/api/metrics")[0] == 401
        assert get(url + "/api/metrics", {"authorization": f"Bearer {TOKEN}"})[0] == 200
    finally:
        srv.stop()
    srv = ServerProcess(str(isolated_home), str(project_dir), env={"CEREYAN_TOKEN": TOKEN, "CEREYAN_METRICS_PUBLIC": "yes"})
    try:
        url = srv.info["url"]
        status, ctype, body = get(url + "/api/metrics")
        assert status == 200 and "cereyan_runs" in body
        assert get(url + "/api/metrics/history")[0] == 401
        assert get(url + "/api/runs")[0] == 401
        entries = {(e["table"], e["key"]): e for e in json.loads(get(url + "/api/settings/environment", {"authorization": f"Bearer {TOKEN}"})[2])["configuration"]}
        assert (entries["server", "metrics_public"]["value"], entries["server", "metrics_public"]["source_name"]) == (True, "CEREYAN_METRICS_PUBLIC")
    finally:
        srv.stop()
    with pytest.raises(RuntimeError, match="CEREYAN_METRICS_PUBLIC"):
        ServerProcess(str(isolated_home), str(project_dir), env={"CEREYAN_METRICS_PUBLIC": "sometimes"})
