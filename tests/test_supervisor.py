import os
import subprocess
import sys
import time

import pytest

from server_helpers import ServerProcess, free_port, is_alive, kill


def flow_id(server, name):
    return next(f["id"] for f in server.client.flows() if f["name"] == name and f["project"] == "proj")


def start(server, name, **params):
    return server.client._request("POST", f"/api/flows/{flow_id(server, name)}/runs", body={"parameters": params})


def test_cooperative_cancel(server):
    run = start(server, "sleepy", seconds=30)
    server.wait_run(run["id"], until=lambda r: r["state"]["type"] == "Running")
    pid = server.client.get_run(run["id"])["engine_pid"]
    t0 = time.time()
    server.client.cancel(run["id"])
    done = server.wait_run(run["id"])
    assert done["state"]["type"] == "Cancelled"
    assert time.time() - t0 < 8
    assert is_alive(pid)  # cancelled cooperatively, no signal sent
    # The engine can take another run afterwards.
    again = start(server, "pid_flow")
    assert server.wait_run(again["id"])["state"]["type"] == "Completed"


def test_cancel_scheduled_run_is_immediate(server):
    # Fill the only engine slot for the sleepy module key by queueing two runs;
    # the second is still Scheduled when we cancel it.
    first = start(server, "sleepy", seconds=3)
    second = start(server, "sleepy", seconds=3)
    third = start(server, "sleepy", seconds=3)
    cancelled = server.client.cancel(third["id"])
    assert cancelled["state"]["type"] in ("Cancelled", "Cancelling")
    done = server.wait_run(third["id"], timeout=10)
    assert done["state"]["type"] == "Cancelled"
    for r in (first, second):
        server.wait_run(r["id"])


@pytest.mark.skipif(
    sys.platform == "win32",
    reason="the terminate-then-kill ladder is Unix-only: Windows delivers no SIGTERM for a "
    "process to ignore, and the server's terminate is already TerminateProcess, so a run is "
    "ended in one step and never reports 'killed'. Cooperative cancel is covered separately",
)
def test_stuck_process_is_terminated_then_killed(isolated_home, project_dir):
    (project_dir / "cereyan.toml").write_text("[server]\ncancel_grace_secs = 1\n")
    srv = ServerProcess(str(isolated_home), str(project_dir))
    try:
        run = start(srv, "stubborn", seconds=60)
        srv.wait_run(run["id"], until=lambda r: r["state"]["type"] == "Running")
        srv.client.cancel(run["id"])
        done = srv.wait_run(run["id"], timeout=20)
        assert done["state"]["type"] == "Cancelled"

        run = start(srv, "immortal", seconds=60)
        srv.wait_run(run["id"], until=lambda r: r["state"]["type"] == "Running")
        pid = srv.client.get_run(run["id"])["engine_pid"]
        srv.client.cancel(run["id"])
        done = srv.wait_run(run["id"], timeout=20)
        assert done["state"]["type"] == "Cancelled"
        assert done["state"]["message"] == "killed"
        time.sleep(0.5)
        assert not is_alive(pid)
    finally:
        srv.stop()


def test_killed_engine_marks_run_crashed(server):
    run = start(server, "sleepy", seconds=30)
    running = server.wait_run(run["id"], until=lambda r: r["state"]["type"] == "Running")
    kill(running["engine_pid"])
    done = server.wait_run(run["id"], timeout=20)
    assert done["state"]["type"] == "Crashed"
    assert done["state"]["message"] == "engine process exited unexpectedly"
    assert done["crash_count"] == 1 and done["failure_count"] == 0
    # The pool recovers.
    again = start(server, "pid_flow")
    assert server.wait_run(again["id"])["state"]["type"] == "Completed"


def test_engine_outlives_server_restart(isolated_home, project_dir):
    port = free_port()
    srv = ServerProcess(str(isolated_home), str(project_dir), port=port)
    run = start(srv, "sleepy", seconds=6)
    running = srv.wait_run(run["id"], until=lambda r: r["state"]["type"] == "Running")
    pid = running["engine_pid"]
    assert srv.stop(kill_engines=False) == 0
    assert is_alive(pid)
    srv2 = ServerProcess(str(isolated_home), str(project_dir), port=port)
    try:
        adopted = srv2.client.get_run(run["id"])
        assert adopted["state"]["type"] == "Running"
        done = srv2.wait_run(run["id"], timeout=30)
        assert done["state"]["type"] == "Completed", srv2.read_log()
        assert done["engine_pid"] == pid
        logs = [l["message"] for l in srv2.client.logs(run["id"])["items"]]
        assert "sleeping" in logs
    finally:
        srv2.stop()


def test_dead_engine_after_restart_is_crashed(isolated_home, project_dir):
    port = free_port()
    srv = ServerProcess(str(isolated_home), str(project_dir), port=port)
    run = start(srv, "sleepy", seconds=30)
    running = srv.wait_run(run["id"], until=lambda r: r["state"]["type"] == "Running")
    # Stop the server first so it cannot observe the engine dying.
    assert srv.stop(kill_engines=False) == 0
    kill(running["engine_pid"])
    time.sleep(0.5)
    srv2 = ServerProcess(str(isolated_home), str(project_dir), port=port)
    try:
        crashed = srv2.wait_run(run["id"], timeout=10)
        assert crashed["state"]["type"] == "Crashed"
        assert crashed["state"]["message"] == "server restarted while run was in progress"
    finally:
        srv2.stop()


def test_module_edit_recycles_engine(server, project_dir):
    first = server.wait_run(start(server, "pid_flow")["id"])
    time.sleep(1.1)
    path = project_dir / "pipeline.py"
    path.write_text(path.read_text() + "\n# edited\n")
    os.utime(path, None)
    second = server.wait_run(start(server, "pid_flow")["id"])
    assert first["engine_pid"] != second["engine_pid"]


def test_import_failure_fails_runs_and_marks_flow(server, tmp_path):
    broken_dir = tmp_path / "broken"
    broken_dir.mkdir()
    (broken_dir / "broken.py").write_text("from cereyan import flow\n@flow\ndef b():\n    pass\ndef (broken\n")
    run = server.client.submit(
        "broken", "b", {}, module="broken", source_dir=str(broken_dir), parameter_schema={"type": "object", "properties": {}}
    )
    t0 = time.time()
    done = server.wait_run(run["id"], timeout=10)
    assert done["state"]["type"] == "Failed"
    assert "SyntaxError" in done["state"]["details"]["traceback"]
    assert time.time() - t0 < 5.5
    flow = next(f for f in server.client.flows() if f["name"] == "b")
    assert flow["error"]
    assert flow["live"] is False


def test_offline_handoff_from_another_project(server, tmp_path):
    other = tmp_path / "other"
    other.mkdir()
    (other / "script.py").write_text(
        "from cereyan import flow, task, get_run_logger\n"
        "@task\ndef t():\n    get_run_logger().info('task log line')\n    return 1\n"
        "@flow\ndef daily_etl():\n    t()\n    return 42\n"
        "if __name__ == '__main__':\n    print('result', daily_etl())\n"
    )
    env = dict(os.environ, CEREYAN_HOME=server.home)
    result = subprocess.run([sys.executable, str(other / "script.py")], env=env, capture_output=True, text=True, timeout=60)
    assert result.returncode == 0, result.stderr
    assert "task log line" in result.stderr
    assert "result None" in result.stdout
    runs = server.client.runs(project="other")["items"]
    assert len(runs) == 1 and runs[0]["state"]["type"] == "Completed"
    assert runs[0]["created_by"] == "script"
    flow = next(f for f in server.client.flows() if f["project"] == "other")
    assert flow["live"] is False and flow["source_dir"] == str(other)

    (other / "bad.py").write_text("from cereyan import flow\n@flow\ndef boom():\n    raise ValueError('nope')\nboom()\n")
    result = subprocess.run([sys.executable, str(other / "bad.py")], env=env, capture_output=True, text=True, timeout=60)
    assert result.returncode == 1
    assert "ValueError: nope" in result.stderr

    result = subprocess.run(
        [sys.executable, "-m", "cereyan", "run", f"{other / 'script.py'}:daily_etl"], env=env, capture_output=True, text=True, timeout=60
    )
    assert result.returncode == 0, result.stderr
    assert "run completed" in result.stdout
    result = subprocess.run(
        [sys.executable, "-m", "cereyan", "run", f"{other / 'bad.py'}:boom"], env=env, capture_output=True, text=True, timeout=60
    )
    assert result.returncode == 1

    missing = tmp_path / "gone"
    missing.mkdir()
    (missing / "m.py").write_text("from cereyan import flow\n@flow\ndef g():\n    pass\n")
    run = server.client.submit("gone", "g", {}, module="m", source_dir=str(missing), parameter_schema={"type": "object", "properties": {}})
    import shutil

    shutil.rmtree(missing)
    done = server.wait_run(run["id"], timeout=15)
    assert done["state"]["type"] == "Failed"
    assert str(missing) in done["state"]["details"]["traceback"] or "No module named" in done["state"]["details"]["traceback"]


def test_delete_non_live_flow_only(server, tmp_path):
    other = tmp_path / "other2"
    other.mkdir()
    (other / "s.py").write_text("from cereyan import flow\n@flow\ndef f():\n    pass\nf()\n")
    env = dict(os.environ, CEREYAN_HOME=server.home)
    subprocess.run([sys.executable, str(other / "s.py")], env=env, capture_output=True, text=True, timeout=60, check=True)
    flows = server.client.flows()
    stale = next(f for f in flows if f["project"] == "other2")
    live = next(f for f in flows if f["project"] == "proj")
    from cereyan.client import ApiError
    import pytest

    with pytest.raises(ApiError) as info:
        server.client._request("DELETE", f"/api/flows/{live['id']}")
    assert info.value.status == 409
    server.client._request("DELETE", f"/api/flows/{stale['id']}")
    assert all(f["project"] != "other2" for f in server.client.flows())
    assert server.client.runs(project="other2")["items"] == []


def test_engine_inherits_home_from_server_flag(tmp_path, project_dir):
    """A server started with --home while CEREYAN_HOME points elsewhere: engines
    must still find the server through the flag home's server.json."""
    flag_home = tmp_path / "flag-home"
    env_home = tmp_path / "env-home"
    srv = ServerProcess(str(env_home), str(project_dir), env={"CEREYAN_HOME": str(env_home)}, extra=[])
    srv.stop()
    import subprocess, sys, json, time

    proc = subprocess.Popen(
        [sys.executable, "-m", "cereyan", "--home", str(flag_home), "serve", str(project_dir), "--port", "0", "--no-open"],
        env=dict(os.environ, CEREYAN_HOME=str(env_home), CEREYAN_NO_BROWSER="1"),
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True,
    )
    try:
        deadline = time.time() + 30
        info = None
        while time.time() < deadline and info is None:
            try:
                info = json.load(open(flag_home / "server.json"))
            except (OSError, ValueError):
                time.sleep(0.05)
        assert info, "server.json not written into the --home directory"
        assert not (env_home / "server.json").exists()
        from cereyan.client import Client

        c = Client(info["url"])
        fid = next(f["id"] for f in c.flows() if f["name"] == "pid_flow")
        run = c._request("POST", f"/api/flows/{fid}/runs", body={})
        deadline = time.time() + 30
        while time.time() < deadline:
            r = c.get_run(run["id"])
            if r["state"]["type"] in ("Completed", "Failed", "Crashed"):
                break
            time.sleep(0.05)
        assert r["state"]["type"] == "Completed", r
        for e in c.server()["engines"]:
            kill(e["pid"])
    finally:
        proc.terminate()
        proc.wait(timeout=15)


def test_same_module_name_in_two_projects_uses_two_engines(server, tmp_path):
    other = tmp_path / "b"
    other.mkdir()
    (other / "pipeline.py").write_text("import os\nfrom cereyan import flow\n@flow\ndef pid_flow():\n    return os.getpid()\n")
    run_b = server.client.submit(
        "b", "pid_flow", {}, module="pipeline", source_dir=str(other), parameter_schema={"type": "object", "properties": {}}
    )
    run_a = start(server, "pid_flow")
    done_a = server.wait_run(run_a["id"])
    done_b = server.wait_run(run_b["id"])
    assert done_a["state"]["type"] == "Completed" and done_b["state"]["type"] == "Completed"
    assert done_a["engine_pid"] != done_b["engine_pid"]
    modules = {(e["source_dir"], e["module"]) for e in server.client.server()["engines"]}
    assert len(modules) == 2, server.read_log() + repr(server.client.server()["engines"])
