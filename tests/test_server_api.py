import json
import os
import time

import pytest

from cereyan.client import ApiError
from server_helpers import ServerProcess, free_port, is_alive, kill


def flow_id(server, name="etl"):
    return next(f["id"] for f in server.client.flows() if f["name"] == name and f["project"] == "proj")


def test_startup_registers_flows_and_serves_ui(server):
    flows = server.client.flows()
    names = {f["name"] for f in flows}
    assert {"etl", "fail", "sleepy", "isolated_flow"} <= names
    assert all(f["live"] for f in flows)
    assert all(f["project"] == "proj" for f in flows)
    import urllib.request

    with urllib.request.urlopen(server.info["url"] + "/", timeout=5) as r:
        assert r.status == 200
        assert b"<" in r.read()
    assert os.path.exists(os.path.join(server.home, "server.json"))


def test_create_run_executes_in_engine(server):
    run = server.client._request("POST", f"/api/flows/{flow_id(server)}/runs", body={"parameters": {"day": "2026-09-06"}})
    assert run["state"]["type"] in ("Scheduled", "Pending")
    done = server.wait_run(run["id"])
    assert done["state"]["type"] == "Completed", done
    assert done["engine_pid"]
    assert done["parameters"] == {"day": "2026-09-06", "n": 1}
    tasks = server.client.task_runs(run["id"])
    assert [t["dynamic_key"] for t in tasks] == ["step-0"]
    assert tasks[0]["state"]["type"] == "Completed"
    logs = server.client.logs(run["id"])["items"]
    messages = [l["message"] for l in logs]
    assert "day 2026-09-06" in messages
    slow = next(l for l in logs if l["message"] == "slow")
    assert slow["task_run_id"] == tasks[0]["id"]
    assert slow["level"] == 30


def test_invalid_parameters_are_422(server):
    with pytest.raises(ApiError) as info:
        server.client._request("POST", f"/api/flows/{flow_id(server)}/runs", body={"parameters": {"day": "2026-09-06", "n": "abc"}})
    assert info.value.status == 422
    assert "n" in info.value.body["error"]
    with pytest.raises(ApiError) as info:
        server.client._request("POST", f"/api/flows/{flow_id(server)}/runs", body={"parameters": {}})
    assert info.value.status == 422 and "day" in info.value.body["error"]
    with pytest.raises(ApiError) as info:
        server.client.flow(999999)
    assert info.value.status == 404


def test_engine_reused_across_sequential_runs(server):
    pids = set()
    gaps = []
    previous_end = None
    for _ in range(5):
        run = server.client._request("POST", f"/api/flows/{flow_id(server, 'pid_flow')}/runs", body={})
        done = server.wait_run(run["id"])
        assert done["state"]["type"] == "Completed"
        pids.add(done["engine_pid"])
        if previous_end is not None:
            gaps.append(done["start_time"] - previous_end)
        previous_end = done["end_time"]
    assert len(pids) == 1
    engines = server.client.server()["engines"]
    assert len(engines) == 1 and engines[0]["runs_done"] >= 5


def test_isolated_flow_uses_fresh_process(server):
    pids = []
    for _ in range(2):
        run = server.client._request("POST", f"/api/flows/{flow_id(server, 'isolated_flow')}/runs", body={})
        done = server.wait_run(run["id"])
        assert done["state"]["type"] == "Completed"
        pids.append(done["engine_pid"])
    assert pids[0] != pids[1]


def test_failed_run_records_message_and_traceback(server):
    run = server.client._request("POST", f"/api/flows/{flow_id(server, 'fail')}/runs", body={})
    done = server.wait_run(run["id"])
    assert done["state"]["type"] == "Failed"
    assert "ValueError: bad" in done["state"]["message"]
    assert "Traceback" in done["state"]["details"]["traceback"]
    assert done["failure_count"] == 1


def test_list_runs_filters_cursor_and_counts(server):
    ids = []
    for i in range(3):
        run = server.client._request("POST", f"/api/flows/{flow_id(server)}/runs", body={"parameters": {"day": "2026-01-0%d" % (i + 1)}, "tags": ["t%d" % i]})
        ids.append(run["id"])
    for i in ids:
        server.wait_run(i)
    fail = server.client._request("POST", f"/api/flows/{flow_id(server, 'fail')}/runs", body={})
    server.wait_run(fail["id"])
    page = server.client.runs(limit=2)
    assert len(page["items"]) == 2 and page["next_cursor"]
    page2 = server.client.runs(limit=2, cursor=page["next_cursor"])
    assert page2["items"][0]["id"] < page["items"][-1]["id"]
    failed = server.client.runs(state_type="Failed")["items"]
    assert [r["flow_name"] for r in failed] == ["fail"]
    tagged = server.client.runs(tags="t1")["items"]
    assert len(tagged) == 1
    assert server.client.runs(project="nope")["items"] == []
    assert len(server.client.runs(project="proj", flow="etl")["items"]) == 3
    counts = server.client.counts()
    assert counts["runs"]["Completed"] == 3
    assert counts["runs"]["Failed"] == 1
    assert counts["active"] == 0
    assert counts["task_runs"].get("Completed", 0) >= 4
    assert server.client.counts(project="other")["runs"]["Completed"] == 0
    task_runs = server.client._request("GET", "/api/task-runs", params={"limit": 2})
    assert len(task_runs["items"]) == 2 and task_runs["items"][0]["flow_name"]
    one = server.client._request("GET", f"/api/task-runs/{task_runs['items'][0]['id']}")
    assert one["run_name"]


def test_transition_endpoint_applies_rules(server):
    run = server.client._request("POST", f"/api/flows/{flow_id(server, 'sleepy')}/runs", body={"parameters": {"seconds": 5}})
    server.wait_run(run["id"], until=lambda r: r["state"]["type"] == "Running")
    with pytest.raises(ApiError) as info:
        server.client._request("POST", f"/api/runs/{run['id']}/transition", body={"type": "Running"})
    assert info.value.status == 409
    assert info.value.body["reason"] == "duplicate"
    assert info.value.body["current"]["type"] == "Running"
    forced = server.client._request("POST", f"/api/runs/{run['id']}/transition", body={"type": "Completed", "force": True, "message": "forced"})
    assert forced["state"]["type"] == "Completed"
    assert forced["state"]["details"]["forced"] is True


def test_delete_run(server):
    run = server.client._request("POST", f"/api/flows/{flow_id(server)}/runs", body={"parameters": {"day": "2026-09-06"}})
    server.wait_run(run["id"])
    server.client.delete_run(run["id"])
    with pytest.raises(ApiError) as info:
        server.client.get_run(run["id"])
    assert info.value.status == 404


def test_openapi_document_and_snapshot(server):
    doc = server.client._request("GET", "/api/openapi.json")
    paths = sorted(doc["paths"])
    snapshot = os.path.join(os.path.dirname(__file__), "openapi_paths.json")
    if os.environ.get("CEREYAN_UPDATE_SNAPSHOTS"):
        with open(snapshot, "w") as fh:
            json.dump(paths, fh, indent=2)
    with open(snapshot) as fh:
        expected = json.load(fh)
    assert paths == expected, "OpenAPI paths changed; set CEREYAN_UPDATE_SNAPSHOTS=1 to refresh"
    ui_snapshot = os.path.join(os.path.dirname(os.path.dirname(__file__)), "ui", "openapi.snapshot.json")
    if os.environ.get("CEREYAN_UPDATE_SNAPSHOTS"):
        with open(ui_snapshot, "w") as fh:
            json.dump(doc, fh, indent=2, sort_keys=True)
            fh.write("\n")
    with open(ui_snapshot) as fh:
        assert doc == json.load(fh), "OpenAPI document changed; refresh ui/openapi.snapshot.json and run service-sync"
    assert "Run" in doc["components"]["schemas"]
    assert "Flow" in doc["components"]["schemas"]
    run_props = doc["components"]["schemas"]["Run"]["properties"]
    assert "engine_pid" in run_props and "project" in run_props


def test_runs_ls_uses_http_when_server_is_up(server, run_cli):
    run = server.client._request("POST", f"/api/flows/{flow_id(server)}/runs", body={"parameters": {"day": "2026-09-06"}})
    server.wait_run(run["id"])
    result = run_cli("runs", "ls", "--json", home=server.home)
    assert result.returncode == 0, result.stderr
    assert [r["id"] for r in json.loads(result.stdout)] == [run["id"]]


def test_clean_shutdown_ends_idle_engines_and_keeps_busy_ones(server):
    """Shutdown reclaims idle engines; one executing a run survives for restart adoption."""
    # Occupy one engine first, so the second run has to warm a different one.
    busy = server.client._request(
        "POST", f"/api/flows/{flow_id(server, 'sleepy')}/runs", body={"parameters": {"seconds": 30}}
    )
    running = server.wait_run(busy["id"], until=lambda r: r["state"]["type"] == "Running")
    busy_pid = running["engine_pid"]
    done = server.wait_run(
        server.client._request("POST", f"/api/flows/{flow_id(server, 'pid_flow')}/runs", body={})["id"]
    )
    idle_pid = done["engine_pid"]
    assert busy_pid != idle_pid, "the second run should have warmed its own engine"

    code = server.stop(kill_engines=False)
    assert code == 0
    assert not os.path.exists(os.path.join(server.home, "server.json"))

    deadline = time.time() + 5
    while time.time() < deadline and is_alive(idle_pid):
        time.sleep(0.1)
    assert not is_alive(idle_pid), "an idle engine outlived the server"
    assert is_alive(busy_pid), "an engine executing a run was signalled"  # restart adoption needs it
    kill(busy_pid)


def test_host_port_precedence(isolated_home, project_dir):
    (project_dir / "cereyan.toml").write_text("[server]\nport = %d\n" % free_port())
    port = free_port()
    srv = ServerProcess(str(isolated_home), str(project_dir), port=port)
    try:
        assert srv.info["port"] == port
    finally:
        srv.stop()
    env_port = free_port()
    srv = ServerProcess(str(isolated_home), str(project_dir), port=0, env={"CEREYAN_PORT": str(env_port)})
    try:
        # A --port flag of 0 is explicit and wins over the environment.
        assert srv.info["port"] != env_port
    finally:
        srv.stop()


def test_second_server_is_refused(server, isolated_home, project_dir):
    import subprocess, sys

    env = dict(os.environ, CEREYAN_HOME=str(isolated_home), CEREYAN_NO_BROWSER="1")
    result = subprocess.run(
        [sys.executable, "-m", "cereyan", "serve", str(project_dir), "--port", "0", "--no-open"],
        env=env, capture_output=True, text=True, timeout=60,
    )
    assert result.returncode == 3
    assert "locked" in result.stderr or "server" in result.stderr


def test_live_stream_delivers_run_and_log_events(server):
    import http.client
    from urllib.parse import urlparse

    u = urlparse(server.info["url"])
    conn = http.client.HTTPConnection(u.hostname, u.port, timeout=10)
    conn.request("GET", "/api/stream", headers={"accept": "text/event-stream"})
    resp = conn.getresponse()
    assert resp.status == 200
    first = resp.readline().decode()
    assert "hello" in first or first.startswith("id:")
    run = server.client._request("POST", f"/api/flows/{flow_id(server)}/runs", body={"parameters": {"day": "2026-09-06"}})
    events = []
    deadline = time.time() + 15
    while time.time() < deadline:
        line = resp.readline().decode()
        if line.startswith("event:"):
            events.append(line.split(":", 1)[1].strip())
        if "run.updated" in events and "log.appended" in events and "task_run.updated" in events:
            break
    conn.close()
    assert "run.updated" in events
    assert "task_run.updated" in events
    assert "log.appended" in events
    server.wait_run(run["id"])
    # Replay from the beginning yields the same events with ids.
    conn = http.client.HTTPConnection(u.hostname, u.port, timeout=10)
    conn.request("GET", "/api/stream?since=0")
    resp = conn.getresponse()
    seen_ids = []
    deadline = time.time() + 5
    while time.time() < deadline and len(seen_ids) < 3:
        line = resp.readline().decode()
        if line.startswith("id:"):
            seen_ids.append(int(line.split(":", 1)[1].strip()))
    conn.close()
    assert seen_ids and seen_ids == sorted(seen_ids)


def test_task_counts_and_recent_run_durations(server):
    fid = flow_id(server)
    created = server.client._request("POST", f"/api/flows/{fid}/runs", body={"parameters": {"day": "2026-09-06"}})
    assert isinstance(created["task_counts"], dict)
    done = server.wait_run(created["id"])
    assert done["task_counts"] == {"Completed": 1}
    listed = next(r for r in server.client._request("GET", "/api/runs", params={"flow": "etl"})["items"] if r["id"] == created["id"])
    assert listed["task_counts"] == {"Completed": 1}
    flow = server.client.flow(fid)
    newest = flow["recent_runs"][0]
    assert newest[0] == created["id"] and newest[1] == "Completed"
    assert isinstance(newest[3], int) and newest[3] > 0
    scheduled = server.client._request("POST", f"/api/flows/{flow_id(server, 'sleepy')}/runs", body={})
    entry = next(e for e in server.client.flow(flow_id(server, "sleepy"))["recent_runs"] if e[0] == scheduled["id"])
    assert entry[3] is None
    server.client._request("POST", f"/api/runs/{scheduled['id']}/cancel")
