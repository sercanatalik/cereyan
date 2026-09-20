"""1.2 MCP server: HTTP transport, tools, resources, prompts, attribution, and the stdio proxy."""

from __future__ import annotations

import json
import os
import subprocess
import sys
import urllib.error
import urllib.request

import pytest

from server_helpers import ServerProcess

TOKEN = "mcp-token"
SNAPSHOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "mcp_snapshot.json")

PIPELINE = '''
from datetime import date
from cereyan import App, Cron, artifacts, task, wait_for_input

app = App("agent")

@task
def step(day: date):
    return str(day)

@app.flow(mcp_tool=True)
def etl(day: date = date(2026, 9, 6), n: int = 1):
    step(day)
    artifacts.create_markdown(f"ran for {day}", key="etl-note")
    return str(day)

@app.flow
def boom():
    raise ValueError("kaboom")

@app.flow
def ask():
    return wait_for_input("Go?")

@app.flow(schedule=Cron("0 3 1 1 *", timezone="UTC"))
def nightly():
    return "ok"
'''


@pytest.fixture
def agent(isolated_home, tmp_path):
    from cereyan import engine

    engine.close_store()
    d = tmp_path / "agent"
    d.mkdir()
    (d / "pipeline.py").write_text(PIPELINE)
    srv = ServerProcess(str(isolated_home), str(d), env={"CEREYAN_TOKEN": TOKEN})
    from cereyan.client import Client

    srv.client = Client(srv.info["url"], token=TOKEN)
    srv.session = None
    srv.counter = 0
    try:
        yield srv
    finally:
        srv.stop()


def rpc(srv, method, params=None, *, token=TOKEN, notification=False):
    srv.counter += 1
    msg = {"jsonrpc": "2.0", "method": method}
    if params is not None:
        msg["params"] = params
    if not notification:
        msg["id"] = srv.counter
    headers = {"content-type": "application/json"}
    if token:
        headers["authorization"] = f"Bearer {token}"
    if srv.session:
        headers["Mcp-Session-Id"] = srv.session
    req = urllib.request.Request(srv.info["url"] + "/mcp", data=json.dumps(msg).encode(), headers=headers, method="POST")
    with urllib.request.urlopen(req, timeout=10) as resp:
        sid = resp.headers.get("Mcp-Session-Id")
        if sid:
            srv.session = sid
        raw = resp.read()
        return resp.status, (json.loads(raw) if raw else None)


def call(srv, tool, **arguments):
    status, body = rpc(srv, "tools/call", {"name": tool, "arguments": arguments})
    assert status == 200, body
    if "error" in body:
        return body["error"]
    result = body["result"]
    text = result["content"][0]["text"]
    return {"isError": result["isError"], "data": json.loads(text) if not result["isError"] else text}


def test_initialize_token_and_listing(agent):
    with pytest.raises(urllib.error.HTTPError) as err:
        rpc(agent, "initialize", {"clientInfo": {"name": "x"}}, token=None)
    assert err.value.code == 401
    status, body = rpc(agent, "initialize", {"protocolVersion": "2025-06-18", "clientInfo": {"name": "claude-code", "version": "1"}, "capabilities": {}})
    assert status == 200 and body["result"]["protocolVersion"] == "2025-06-18"
    assert set(body["result"]["capabilities"]) == {"tools", "resources", "prompts"}
    assert agent.session
    status, body = rpc(agent, "notifications/initialized", notification=True)
    assert status == 202 and body is None
    assert rpc(agent, "ping")[1]["result"] == {}
    status, body = rpc(agent, "tools/call", {"name": "nope", "arguments": {}})
    assert body["error"]["code"] == -32602
    assert rpc(agent, "no/such")[1]["error"]["code"] == -32601
    req = urllib.request.Request(agent.info["url"] + "/mcp", headers={"authorization": f"Bearer {TOKEN}"})
    with pytest.raises(urllib.error.HTTPError) as err:
        urllib.request.urlopen(req, timeout=5)
    assert err.value.code == 405



def _keys(payload):
    """The top-level keys of a tool result, plus the item fields of every list it holds."""
    assert not payload["isError"], payload
    data = payload["data"]
    shape = {"keys": sorted(data)}
    items = {}
    for key, value in data.items():
        if isinstance(value, list) and value and isinstance(value[0], dict):
            items[key] = sorted(value[0])
    if items:
        shape["items"] = {k: items[k] for k in sorted(items)}
    return shape


def _response_keys(srv):
    """Call every tool against real data and record the top-level keys of what it returns."""
    etl = next(f for f in call(srv, "list_flows")["data"]["flows"] if f["name"] == "etl")
    keys = {}

    started = call(srv, "run_flow", flow="etl", parameters={"day": "2026-09-01"})
    keys["run_flow"] = {"default": _keys(started)}
    srv.wait_run(started["data"]["run"]["id"])

    failed = call(srv, "run_flow", flow="boom")["data"]["run"]
    srv.wait_run(failed["id"])

    keys["list_flows"] = {"default": _keys(call(srv, "list_flows"))}
    keys["list_runs"] = {"default": _keys(call(srv, "list_runs"))}
    keys["get_run"] = {"default": _keys(call(srv, "get_run", run_id=started["data"]["run"]["id"]))}
    keys["run_logs"] = {"default": _keys(call(srv, "run_logs", run_id=failed["id"]))}
    keys["list_events"] = {"default": _keys(call(srv, "list_events"))}
    keys["list_artifacts"] = {"default": _keys(call(srv, "list_artifacts"))}
    keys["list_rules"] = {"default": _keys(call(srv, "list_rules"))}
    keys["explain_failure"] = {"default": _keys(call(srv, "explain_failure", run_id=failed["id"]))}

    created_bf = call(srv, "backfill", flow="etl", parameter="day", start="2026-02-01", end="2026-02-02", dry_run=False)
    keys["backfill"] = {
        "dry run": _keys(call(srv, "backfill", flow="etl", parameter="day", start="2026-01-01", end="2026-01-03")),
        "dry_run false": _keys(created_bf),
    }
    status = created_bf["data"]["backfill"]
    bf_id = status.get("backfill", status)["id"]
    keys["list_backfills"] = {"default": _keys(call(srv, "list_backfills", flow="etl"))}
    keys["get_backfill"] = {"default": _keys(call(srv, "get_backfill", backfill_id=bf_id))}
    keys["cancel_backfill"] = {"default": _keys(call(srv, "cancel_backfill", backfill_id=bf_id))}
    keys["get_flow_source"] = {"default": _keys(call(srv, "get_flow_source", flow="etl"))}
    keys["server_health"] = {"default": _keys(call(srv, "server_health"))}
    keys["list_resources"] = {"default": _keys(call(srv, "list_resources"))}
    keys["flow_dependencies"] = {"default": _keys(call(srv, "flow_dependencies", flow="etl"))}
    keys["check_flows"] = {"default": _keys(call(srv, "check_flows"))}
    rerun = call(srv, "rerun_run", run_id=started["data"]["run"]["id"])
    keys["rerun_run"] = {"default": _keys(rerun)}
    srv.wait_run(rerun["data"]["run"]["id"])
    keys["compare_runs"] = {"default": _keys(call(srv, "compare_runs", run_id=started["data"]["run"]["id"], other_run_id=rerun["data"]["run"]["id"]))}
    keys["flow__agent__etl"] = {"default": _keys(call(srv, "flow__agent__etl", day="2026-09-03"))}

    paused = call(srv, "run_flow", flow="ask")["data"]["run"]
    srv.wait_run(paused["id"], until=lambda r: r["state"]["type"] == "Paused")
    keys["resume_run"] = {"default": _keys(call(srv, "resume_run", run_id=paused["id"], input={"go": True}))}
    srv.wait_run(paused["id"])

    doomed = call(srv, "run_flow", flow="ask")["data"]["run"]
    srv.wait_run(doomed["id"], until=lambda r: r["state"]["type"] == "Paused")
    keys["cancel_run"] = {"default": _keys(call(srv, "cancel_run", run_id=doomed["id"]))}

    sched = srv.client._request("POST", f"/api/flows/{etl['id']}/schedules", body={"kind": "cron", "cron": "0 3 1 1 *"})
    keys["pause_schedule"] = {"default": _keys(call(srv, "pause_schedule", schedule_id=sched["id"]))}
    keys["resume_schedule"] = {"default": _keys(call(srv, "resume_schedule", schedule_id=sched["id"]))}

    keys["list_schedules"] = {"default": _keys(call(srv, "list_schedules"))}
    made = call(srv, "create_schedule", flow="etl", kind="interval", interval=86400)
    keys["create_schedule"] = {"default": _keys(made)}
    made_id = made["data"]["schedule"]["id"]
    keys["edit_schedule"] = {"default": _keys(call(srv, "edit_schedule", schedule_id=made_id, interval=43200))}
    keys["delete_schedule"] = {"default": _keys(call(srv, "delete_schedule", schedule_id=made_id))}

    keys["set_variable"] = {"default": _keys(call(srv, "set_variable", name="snapshot", value="v"))}
    call(srv, "set_variable", name="snapshot_secret", value="hidden", secret=True)
    keys["list_variables"] = {"default": _keys(call(srv, "list_variables"))}
    return {name: keys[name] for name in sorted(keys)}


def test_mcp_snapshot(agent):
    """Pin the whole MCP surface; docs/reference/mcp.md is generated from this file."""
    status, body = rpc(agent, "initialize", {"protocolVersion": "2025-06-18", "clientInfo": {"name": "snapshot", "version": "1"}, "capabilities": {}})
    assert status == 200
    handshake = body["result"]
    # The version moves with every release and says nothing about the protocol surface.
    handshake["serverInfo"].pop("version", None)
    snapshot = {
        "handshake": handshake,
        "tools": rpc(agent, "tools/list")[1]["result"]["tools"],
        "resources": rpc(agent, "resources/list")[1]["result"]["resources"],
        "resource_templates": rpc(agent, "resources/templates/list")[1]["result"]["resourceTemplates"],
        "prompts": rpc(agent, "prompts/list")[1]["result"]["prompts"],
        # Rendered with a placeholder id, so the page can show what the model is actually told.
        "prompt_messages": {
            "diagnose_run": rpc(agent, "prompts/get", {"name": "diagnose_run", "arguments": {"run_id": "<run_id>"}})[1][
                "result"
            ]["messages"],
        },
        "response_keys": _response_keys(agent),
    }
    if os.environ.get("CEREYAN_UPDATE_SNAPSHOTS"):
        with open(SNAPSHOT, "w") as fh:
            json.dump(snapshot, fh, indent=2, sort_keys=True)
            fh.write("\n")
    with open(SNAPSHOT) as fh:
        expected = json.load(fh)
    assert snapshot == expected, (
        f"the MCP surface changed; set CEREYAN_UPDATE_SNAPSHOTS=1 to refresh {os.path.relpath(SNAPSHOT)}"
        " and run `just docs` to regenerate docs/reference/mcp.md"
    )


def test_run_flow_attribution_and_explain_failure(agent):
    rpc(agent, "initialize", {"clientInfo": {"name": "claude-code"}})
    flows = call(agent, "list_flows")["data"]["flows"]
    assert {f["name"] for f in flows} == {"etl", "boom", "ask", "nightly"}
    created = call(agent, "run_flow", flow="etl", parameters={"day": "2026-09-01", "n": 2}, tags=["agent"])
    assert not created["isError"]
    run = created["data"]["run"]
    assert run["created_by"] == "mcp:claude-code" and "agent" in run["tags"]
    done = agent.wait_run(run["id"])
    assert done["state"]["type"] == "Completed"
    got = call(agent, "get_run", run_id=run["id"])["data"]
    assert got["run"]["id"] == run["id"] and len(got["task_runs"]) == 1
    logs = call(agent, "run_logs", run_id=run["id"], min_level=20)["data"]["logs"]
    assert any("started" in l["message"] for l in logs)
    failed = call(agent, "run_flow", flow="boom")["data"]["run"]
    agent.wait_run(failed["id"])
    explained = call(agent, "explain_failure", run_id=failed["id"])["data"]
    assert "kaboom" in explained["verdict"]
    assert any("kaboom" in l["message"] for l in explained["error_logs"])
    assert any(e["name"] == "run.failed" for e in explained["events"])
    listed = call(agent, "list_runs", state_type="Failed")["data"]["runs"]
    assert [r["id"] for r in listed] == [failed["id"]]
    bad = call(agent, "run_flow", flow="missing")
    assert bad["isError"] and "missing" in bad["data"]
    # Without a session the run is still attributed, as unknown.
    agent.session = None
    anon = call(agent, "run_flow", flow="etl")["data"]["run"]
    assert anon["created_by"] == "mcp:unknown"


def test_backfill_dry_run_resume_and_variables(agent):
    rpc(agent, "initialize", {"clientInfo": {"name": "cursor"}})
    plan = call(agent, "backfill", flow="etl", parameter="day", start="2026-01-01", end="2026-01-05")["data"]
    assert plan["dry_run"] is True and plan["runs"] == 5
    assert call(agent, "list_runs", flow="etl")["data"]["runs"] == []
    real = call(agent, "backfill", flow="etl", parameter="day", start="2026-01-01", end="2026-01-03", dry_run=False)["data"]
    assert real["backfill"]["total"] == 3
    paused = call(agent, "run_flow", flow="ask")["data"]["run"]
    agent.wait_run(paused["id"], until=lambda r: r["state"]["type"] == "Paused")
    resumed = call(agent, "resume_run", run_id=paused["id"], input={"go": True})["data"]["run"]
    assert resumed["state"]["type"] == "Scheduled" and resumed["state"]["name"] == "Resuming"
    assert agent.wait_run(paused["id"])["state"]["type"] == "Completed"
    var = call(agent, "set_variable", name="region", value="eu", tags=["infra"])["data"]["variable"]
    assert var["name"] == "region" and var["value"] == "eu"
    secret = call(agent, "set_variable", name="api_key", value="s3", secret=True)["data"]["variable"]
    assert secret["secret"] is True and secret["value"] != "s3"
    cancelled = call(agent, "cancel_run", run_id=real["backfill"]["id"])
    assert cancelled is not None
    status, body = rpc(agent, "resources/templates/list")
    assert [t["uriTemplate"] for t in body["result"]["resourceTemplates"]] == ["cereyan://runs/{id}/logs", "cereyan://runs/{id}/artifacts"]
    status, body = rpc(agent, "resources/read", {"uri": f"cereyan://runs/{paused['id']}/logs"})
    contents = body["result"]["contents"][0]
    assert contents["mimeType"] == "application/json" and json.loads(contents["text"])["run_id"] == paused["id"]
    status, body = rpc(agent, "prompts/get", {"name": "diagnose_run", "arguments": {"run_id": "7"}})
    assert "explain_failure" in body["result"]["messages"][0]["content"]["text"]
    assert rpc(agent, "prompts/list")[1]["result"]["prompts"][0]["name"] == "diagnose_run"


def test_stdio_proxy_round_trip_and_no_server(agent, isolated_home, tmp_path):
    env = dict(os.environ, CEREYAN_HOME=str(isolated_home), CEREYAN_TOKEN=TOKEN)
    messages = [
        {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"clientInfo": {"name": "stdio-host"}}},
        {"jsonrpc": "2.0", "method": "notifications/initialized"},
        {"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "list_flows", "arguments": {}}},
    ]
    proc = subprocess.run([sys.executable, "-m", "cereyan", "mcp"], env=env, input="".join(json.dumps(m) + "\n" for m in messages),
                          capture_output=True, text=True, timeout=60)
    assert proc.returncode == 0, proc.stderr
    replies = [json.loads(line) for line in proc.stdout.splitlines() if line.strip()]
    assert [r["id"] for r in replies] == [1, 2]
    assert replies[0]["result"]["serverInfo"]["name"] == "cereyan"
    flows = json.loads(replies[1]["result"]["content"][0]["text"])["flows"]
    assert {f["name"] for f in flows} == {"etl", "boom", "ask", "nightly"}
    # No server: every request gets a -32000 error, and the process stays up until EOF.
    empty_home = tmp_path / "empty_home"
    env2 = dict(os.environ, CEREYAN_HOME=str(empty_home))
    env2.pop("CEREYAN_TOKEN", None)
    proc = subprocess.run([sys.executable, "-m", "cereyan", "mcp"], env=env2, input=json.dumps(messages[0]) + "\n" + json.dumps(messages[2]) + "\n",
                          capture_output=True, text=True, timeout=60)
    replies = [json.loads(line) for line in proc.stdout.splitlines() if line.strip()]
    assert proc.returncode == 0 and len(replies) == 2
    assert all(r["error"]["code"] == -32000 and "server" in r["error"]["message"] for r in replies)


def test_schedule_tools_reach_and_guard(agent):
    """The dead end this closes, plus the two guards on code-declared schedules."""
    rpc(agent, "initialize", {"clientInfo": {"name": "claude-code"}})

    # A schedule declared in code that has never fired is still discoverable, and
    # the id it yields is the one pause_schedule wants. This was the dead end:
    # no tool returned a schedule id, so pause_schedule could not be reached.
    listed = call(agent, "list_schedules")["data"]["schedules"]
    nightly = next(s for s in listed if s["flow"] == "nightly")
    assert nightly["source"] == "code" and nightly["active"]
    assert nightly["next_fire"], "a schedule that has never fired still reports its next fire"
    paused = call(agent, "pause_schedule", schedule_id=nightly["id"])
    assert not paused["isError"] and not paused["data"]["schedule"]["active"]
    call(agent, "resume_schedule", schedule_id=nightly["id"])

    # Filters narrow to a flow, and the tool answers for one flow or for all.
    only = call(agent, "list_schedules", flow="nightly")["data"]["schedules"]
    assert [s["flow"] for s in only] == ["nightly"]

    # A schedule an agent creates is attributed to it and carries its fire times.
    made = call(agent, "create_schedule", flow="etl", kind="cron", cron="0 4 1 1 *")
    assert not made["isError"], made
    assert made["data"]["schedule"]["source"] == "mcp"
    assert len(made["data"]["next_fires"]) == 3
    assert "note" not in made["data"], "etl declares no schedule of its own"
    made_id = made["data"]["schedule"]["id"]

    # Creating a second schedule on a flow that declares one in code is legal,
    # and says so rather than failing.
    second = call(agent, "create_schedule", flow="nightly", kind="interval", interval=86400)
    assert "already has a schedule declared in its code" in second["data"]["note"]
    call(agent, "delete_schedule", schedule_id=second["data"]["schedule"]["id"])

    # Editing a code-declared schedule lasts only until the declaration applies
    # again at the next restart, which an agent has to be told.
    edited = call(agent, "edit_schedule", schedule_id=nightly["id"], cron="0 5 1 1 *")
    assert "until the server restarts" in edited["data"]["note"]
    # Editing one the agent made says nothing, because it has no declaration.
    quiet = call(agent, "edit_schedule", schedule_id=made_id, cron="0 6 1 1 *")
    assert "note" not in quiet["data"]

    # Deleting a code-declared schedule is refused: startup would recreate it, so
    # reporting success would be untrue.
    refused = call(agent, "delete_schedule", schedule_id=nightly["id"])
    assert refused["isError"] and "pause_schedule" in refused["data"]
    assert call(agent, "list_schedules", flow="nightly")["data"]["schedules"], "still there"

    # One the agent made deletes normally.
    gone = call(agent, "delete_schedule", schedule_id=made_id)
    assert not gone["isError"] and gone["data"]["deleted"]
    assert made_id not in [s["id"] for s in call(agent, "list_schedules")["data"]["schedules"]]


def test_messages_must_be_json(agent):
    run_id = agent.client.submit("agent", "ask")["id"]
    agent.wait_run(run_id, until=lambda r: r["state"]["type"] == "Paused")

    def post(content_type, msg):
        headers = {"content-type": content_type, "authorization": f"Bearer {TOKEN}"}
        req = urllib.request.Request(agent.info["url"] + "/mcp", data=json.dumps(msg).encode(), headers=headers, method="POST")
        try:
            with urllib.request.urlopen(req, timeout=10) as resp:
                return resp.status, json.loads(resp.read())
        except urllib.error.HTTPError as exc:
            return exc.code, json.loads(exc.read())

    # text/plain is what a page on another site can send without a preflight.
    cancel = {"jsonrpc": "2.0", "id": 1, "method": "tools/call",
              "params": {"name": "cancel_run", "arguments": {"run_id": run_id}}}
    status, body = post("text/plain", cancel)
    assert status == 415 and body["id"] is None and "application/json" in body["error"]["message"]
    assert agent.client.get_run(run_id)["state"]["type"] == "Paused"
    # Case and parameters in the media type do not matter.
    status, body = post("Application/JSON; charset=utf-8", {"jsonrpc": "2.0", "id": 2, "method": "ping"})
    assert (status, body["result"]) == (200, {})


def tool_map(srv):
    return {t["name"]: t for t in rpc(srv, "tools/list")[1]["result"]["tools"]}


def test_annotations_and_flow_tools(agent):
    rpc(agent, "initialize", {"clientInfo": {"name": "annot"}})
    tools = tool_map(agent)
    assert tools["get_run"]["annotations"] == {"readOnlyHint": True, "destructiveHint": False}
    assert tools["run_flow"]["annotations"] == {"readOnlyHint": False, "destructiveHint": False}
    assert tools["cancel_run"]["annotations"]["destructiveHint"] is True
    assert tools["set_variable"]["annotations"]["destructiveHint"] is True
    assert all("annotations" in t for t in tools.values())
    # etl opted in; boom did not.
    flow_tool = tools["flow__agent__etl"]
    assert "day" in flow_tool["inputSchema"]["properties"] and flow_tool["annotations"]["readOnlyHint"] is False
    assert "flow__agent__boom" not in tools
    started = call(agent, "flow__agent__etl", day="2026-09-02")
    assert started["isError"] is False and started["data"]["run"]["parameters"]["day"] == "2026-09-02"
    assert agent.wait_run(started["data"]["run"]["id"])["state"]["type"] == "Completed"
    missing = call(agent, "flow__agent__boom")
    assert missing["code"] == -32602


def test_new_read_tools_and_rerun(agent, tmp_path):
    rpc(agent, "initialize", {"clientInfo": {"name": "reader"}})
    source = call(agent, "get_flow_source", flow="etl")["data"]
    assert "def etl(" in source["source"] and source["truncated"] is False
    assert source["path"].endswith("pipeline.py") and source["module"] == "pipeline"

    call(agent, "set_variable", name="plain", value={"a": 1})
    call(agent, "set_variable", name="hidden", value="s3cret", secret=True)
    rows = {v["name"]: v for v in call(agent, "list_variables")["data"]["variables"]}
    assert rows["plain"]["value"] == {"a": 1} and rows["plain"]["secret"] is False
    assert rows["hidden"]["secret"] is True and "value" not in rows["hidden"]
    assert "s3cret" not in json.dumps(rows)

    health = call(agent, "server_health")["data"]
    assert {"engines", "queued", "resources", "schedules", "auth", "exposed", "read_only", "served_dir"} <= set(health)
    assert health["auth"] is True and health["read_only"] is False and health["schedules"] >= 1
    assert "resources" in call(agent, "list_resources")["data"]
    deps = call(agent, "flow_dependencies", flow="etl")["data"]
    assert deps == {"flow": "etl", "project": "agent", "upstreams": [], "batch_key": None, "triggers": []}

    original = call(agent, "run_flow", flow="etl", parameters={"day": "2026-09-04", "n": 3})["data"]["run"]
    agent.wait_run(original["id"])
    again = call(agent, "rerun_run", run_id=original["id"])["data"]
    assert again["rerun_of"] == original["id"] and again["run"]["id"] != original["id"]
    assert again["run"]["parameters"] == {"day": "2026-09-04", "n": 3}
    assert again["run"]["created_by"] == "mcp:reader"

    report = call(agent, "check_flows")["data"]
    assert report["ok"] is True and report["errors"] == 0 and "findings" in report
    assert {f["name"] for f in report["flows"]} >= {"etl", "boom", "ask", "nightly"}
    assert call(agent, "rerun_run", run_id=999999)["isError"] is True


def test_prompts(agent):
    names = [p["name"] for p in rpc(agent, "prompts/list")[1]["result"]["prompts"]]
    assert names == ["diagnose_run", "health_check", "plan_backfill"]
    text = rpc(agent, "prompts/get", {"name": "health_check"})[1]["result"]["messages"][0]["content"]["text"]
    assert "server_health" in text and "list_runs" in text and "list_schedules" in text
    plan = rpc(agent, "prompts/get", {"name": "plan_backfill", "arguments": {"flow": "etl", "parameter": "day", "start": "2026-01-01", "end": "2026-01-31"}})
    text = plan[1]["result"]["messages"][0]["content"]["text"]
    assert "etl" in text and "2026-01-31" in text and "dry_run" in text
    assert rpc(agent, "prompts/get", {"name": "nope"})[1]["error"]["code"] == -32602


def test_read_only_mode(isolated_home, tmp_path):
    from cereyan import engine

    engine.close_store()
    d = tmp_path / "ro"
    d.mkdir()
    (d / "pipeline.py").write_text(PIPELINE)
    with pytest.raises(RuntimeError, match="CEREYAN_MCP_READ_ONLY"):
        ServerProcess(str(isolated_home), str(d), env={"CEREYAN_TOKEN": TOKEN, "CEREYAN_MCP_READ_ONLY": "maybe"})
    srv = ServerProcess(str(isolated_home), str(d), env={"CEREYAN_TOKEN": TOKEN, "CEREYAN_MCP_READ_ONLY": "yes"})
    from cereyan.client import Client

    srv.client = Client(srv.info["url"], token=TOKEN)
    srv.session = None
    srv.counter = 0
    try:
        status, body = rpc(srv, "initialize", {"clientInfo": {"name": "ro"}})
        assert status == 200 and "read-only" in body["result"]["instructions"]
        tools = tool_map(srv)
        assert all(t["annotations"]["readOnlyHint"] for t in tools.values())
        assert "list_runs" in tools and "server_health" in tools
        assert not {"run_flow", "cancel_run", "set_variable", "rerun_run"} & set(tools)
        assert not any(name.startswith("flow__") for name in tools)
        refused = call(srv, "run_flow", flow="etl")
        assert refused["isError"] is True and "read-only" in refused["data"]
        refused = call(srv, "flow__agent__etl", day="2026-09-02")
        assert refused["isError"] is True and "read-only" in refused["data"]
        assert call(srv, "list_runs")["isError"] is False
        assert call(srv, "server_health")["data"]["read_only"] is True
        assert call(srv, "no_such_tool")["code"] == -32602
        entries = {(e["table"], e["key"]): e for e in srv.client._request("GET", "/api/settings/environment")["configuration"]}
        assert (entries["server", "mcp_read_only"]["value"], entries["server", "mcp_read_only"]["source"]) == (True, "env")
    finally:
        srv.stop()
