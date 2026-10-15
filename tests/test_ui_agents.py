"""UI-defined agents: a definition stored through the API becomes a flow on the
built-in runner, runs with a scripted model, asks in the inbox, and keeps its
secrets and spend in check."""

from __future__ import annotations

import json
import os
import time

import pytest

pytest.importorskip("pydantic_ai.durable_exec", reason="needs the pydantic-ai extra")
os.environ.setdefault("PYDANTIC_AI_NO_BANNER", "1")
pytestmark = pytest.mark.filterwarnings("ignore:There is no current event loop:DeprecationWarning")

from cereyan.client import ApiError  # noqa: E402
from server_helpers import ServerProcess  # noqa: E402

# A served directory with no flows of its own, only the scripted models the
# definitions name. `test:<module>:<function>` is honoured when
# CEREYAN_TEST_MODELS is set; the function gets the model id and returns a model.
MODELS = '''
import os
from pydantic_ai.messages import ModelResponse, TextPart, ToolCallPart
from pydantic_ai.models.function import FunctionModel

TRACE = os.environ["AGENT_TRACE"]

def note(line):
    with open(TRACE, "a") as fh:
        fh.write(line + "\\n")

def echo(model_id):
    """Answers with the prompt it was given, after one search."""
    def run(messages, info):
        note("model")
        note("key:" + str(os.environ.get("TEST_MODEL_API_KEY")))
        last = messages[-1].parts[-1]
        if last.part_kind == "user-prompt":
            return ModelResponse(parts=[ToolCallPart("list_runs", {"limit": 2}, tool_call_id="s-1")])
        return ModelResponse(parts=[TextPart(f"answer: {last.content}")])
    return FunctionModel(run)

def refunder(model_id):
    """Calls the self MCP tool that needs approval, then answers."""
    def run(messages, info):
        note("model")
        last = messages[-1].parts[-1]
        if last.part_kind == "user-prompt":
            return ModelResponse(parts=[ToolCallPart("cancel_run", {"run_id": 999999}, tool_call_id="c-1")])
        return ModelResponse(parts=[TextPart(f"done: {last.content}")])
    return FunctionModel(run)

def chatty(model_id):
    """Never stops asking for a tool, to hit the request limit."""
    def run(messages, info):
        note("model")
        return ModelResponse(parts=[ToolCallPart("list_runs", {"limit": 1}, tool_call_id=f"m-{len(messages)}")])
    return FunctionModel(run)

def structured(model_id):
    def run(messages, info):
        note("model")
        return ModelResponse(parts=[TextPart('{"approved": true, "notes": "fine"}')])
    return FunctionModel(run)
'''

def definition(name: str, model: str, **over) -> dict:
    d = {
        "name": name,
        "project": "agents",
        "description": f"test agent {name}",
        "model": f"test:scripted_models:{model}",
        "api_key": None,
        "instructions": "You are a test agent.",
        "prompt": "Say hello to {who}.",
        "tools": [{"kind": "self", "name": "cereyan", "options": {"tools": ["list_runs", "cancel_run"]}}],
        "policy": {"approvals": "ask", "once": []},
        "limits": {"tokens": 10000, "requests": 6, "cost_usd": 1.0, "timeout_seconds": 60},
        "output": {"kind": "text"},
        "crash_retries": 1,
        "enabled": True,
    }
    d.update(over)
    return d


@pytest.fixture
def srv(isolated_home, tmp_path):
    from cereyan import engine

    engine.close_store()
    d = tmp_path / "agents"
    d.mkdir()
    (d / "scripted_models.py").write_text(MODELS)
    (d / "cereyan.toml").write_text('[agents]\nmcp_urls = ["self"]\nmcp_commands = ["npx"]\ncost_per_day = 5.0\n')
    trace = tmp_path / "trace.txt"
    trace.write_text("")
    server = ServerProcess(str(isolated_home), str(d), env={
        "AGENT_TRACE": str(trace), "CEREYAN_TEST_MODELS": "1", "CEREYAN_FAST_CRASH_RERUN": "1",
        "PYDANTIC_AI_NO_BANNER": "1",
    })
    server.trace = trace
    try:
        yield server
    finally:
        server.stop()


def lines(srv):
    return [ln for ln in srv.trace.read_text().splitlines() if ln]


def wait_until(fn, timeout=40):
    deadline = time.time() + timeout
    while time.time() < deadline:
        v = fn()
        if v:
            return v
        time.sleep(0.2)
    raise AssertionError("condition not met in time")


def create(srv, **over):
    return srv.client._request("POST", "/api/agents", body=definition(over.pop("name", "echo"), over.pop("model", "echo"), **over))


def run_agent(srv, agent, **params):
    body = {"parameters": {"who": "world", **params}}
    return srv.client._request("POST", f"/api/flows/{agent['flow_id']}/runs", body=body)


def test_a_ui_agent_runs_as_a_flow_and_records_its_output(srv):
    c = srv.client
    agent = create(srv)
    assert agent["source"] == "ui" and agent["enabled"] is True and agent["flow_id"]
    flow = c.flow(agent["flow_id"])
    assert flow["project"] == "agents" and flow["name"] == "echo" and flow["options"]["agent"] is True
    assert sorted(flow["parameter_schema"]["properties"]) == ["prompt", "who"]
    run = run_agent(srv, agent)
    done = srv.wait_run(run["id"], timeout=60)
    assert done["state"]["type"] == "Completed", done["state"]
    assert c.run_state(run["id"], key="outcome")["value"].startswith("answer:")
    assert c.run_state(run["id"], key="usage")["value"]["requests"] == 2
    assert lines(srv).count("model") == 2
    units = c.task_runs(run["id"])
    model_units = [t for t in units if "model.request" in t["dynamic_key"]]
    assert model_units and model_units[0]["state"]["details"]["summary"]["kind"] == "model"
    arts = c.artifacts(run["id"])
    assert any(a["kind"] == "markdown" for a in arts)


def test_a_definition_is_validated_and_its_key_must_be_secret(srv):
    c = srv.client
    c._request("POST", "/api/variables", body={"name": "plain_key", "value": "x"})
    c._request("POST", "/api/variables", body={"name": "openai/api_key", "value": "sk-very-secret-key", "secret": True})
    with pytest.raises(ApiError) as err:
        create(srv, name="bad", api_key="plain_key")
    assert err.value.status == 422 and "api_key" in str(err.value)
    with pytest.raises(ApiError) as err:
        create(srv, name="bad", tools=[{"kind": "code", "name": "x", "options": {}}])
    assert err.value.status == 422 and "code" in str(err.value)
    with pytest.raises(ApiError) as err:
        create(srv, name="bad", output={"kind": "json"})
    assert err.value.status == 422 and "schema" in str(err.value)
    agent = create(srv, name="keyed", api_key="openai/api_key")
    assert agent["definition"]["api_key"] == "openai/api_key"
    assert c._request("GET", f"/api/agents/{agent['id']}")["name"] == "keyed"
    assert len(c._request("GET", "/api/agents")) == 1
    # The key reaches the model's environment for the run and nowhere else.
    run = run_agent(srv, agent)
    assert srv.wait_run(run["id"], timeout=60)["state"]["type"] == "Completed"
    assert "key:sk-very-secret-key" in lines(srv)
    log_text = " ".join(row["message"] for row in c.logs(run["id"])["items"])
    assert "sk-very-secret-key" not in log_text
    assert "sk-very-secret-key" not in json.dumps(c.run_state(run["id"]))
    assert "sk-very-secret-key" not in json.dumps(c._request("GET", "/api/settings/environment"))
    assert "sk-very-secret-key" not in json.dumps(c.task_runs(run["id"]))


def test_an_approval_is_asked_remembered_and_revoked(srv):
    c = srv.client
    agent = create(srv, name="refunder", model="refunder")
    run = run_agent(srv, agent)
    paused = srv.wait_run(run["id"], timeout=60, until=lambda r: r["state"]["type"] in ("Paused", "Completed", "Failed"))
    assert paused["state"]["type"] == "Paused", paused["state"]
    assert "cancel_run" in paused["state"]["details"]["prompt"]
    c.resume(run["id"], {"approve": True, "always_allow": True})
    assert srv.wait_run(run["id"], timeout=60)["state"]["type"] == "Completed"
    remembered = c._request("GET", f"/api/agents/{agent['id']}/approvals")
    assert [r["tool"] for r in remembered] == ["cancel_run"] and remembered[0]["args"] == {"run_id": 999999}
    # The same call again is approved without a question.
    again = run_agent(srv, agent)
    assert srv.wait_run(again["id"], timeout=60)["state"]["type"] == "Completed"
    assert c.get_run(again["id"])["state"]["type"] == "Completed"
    # Revoked, it asks again.
    c._request("DELETE", f"/api/agents/{agent['id']}/approvals/{remembered[0]['id']}")
    third = run_agent(srv, agent)
    paused = srv.wait_run(third["id"], timeout=60, until=lambda r: r["state"]["type"] in ("Paused", "Completed", "Failed"))
    assert paused["state"]["type"] == "Paused"
    c.resume(third["id"], {"approve": False})
    done = srv.wait_run(third["id"], timeout=60)
    assert done["state"]["type"] == "Completed"
    assert "denied" in c.run_state(third["id"], key="outcome")["value"]


def test_policy_deny_and_approve_skip_the_question(srv):
    c = srv.client
    deny = create(srv, name="denier", model="refunder", policy={"approvals": "deny", "once": []})
    run = run_agent(srv, deny)
    done = srv.wait_run(run["id"], timeout=60)
    assert done["state"]["type"] == "Completed", done["state"]
    assert "not allowed" in c.run_state(run["id"], key="outcome")["value"]
    allow = create(srv, name="approver", model="refunder", policy={"approvals": "approve", "once": []})
    run = run_agent(srv, allow)
    done = srv.wait_run(run["id"], timeout=60)
    assert done["state"]["type"] == "Completed", done["state"]
    assert "not allowed" not in c.run_state(run["id"], key="outcome")["value"]


def test_the_request_limit_fails_the_run(srv):
    agent = create(srv, name="chatty", model="chatty", limits={"requests": 3, "tokens": None, "cost_usd": None, "timeout_seconds": 60})
    run = run_agent(srv, agent)
    done = srv.wait_run(run["id"], timeout=60)
    assert done["state"]["type"] == "Failed" and "request" in done["state"]["message"].lower()
    assert lines(srv).count("model") <= 3


def test_json_output_is_validated_and_stored(srv):
    c = srv.client
    schema = {"type": "object", "properties": {"approved": {"type": "boolean"}, "notes": {"type": "string"}}, "required": ["approved"]}
    agent = create(srv, name="reviewer", model="structured", tools=[], output={"kind": "json", "schema": schema})
    run = run_agent(srv, agent)
    done = srv.wait_run(run["id"], timeout=60)
    assert done["state"]["type"] == "Completed", done["state"]
    assert c.run_state(run["id"], key="outcome")["value"].startswith("approved=True")
    assert any(a["kind"] == "markdown" and "approved" in json.dumps(a["data"]) for a in c.artifacts(run["id"]))


def test_a_disabled_agent_refuses_to_run(srv):
    c = srv.client
    agent = create(srv, name="sleepy")
    body = {**agent["definition"], "enabled": False}
    c._request("PATCH", f"/api/agents/{agent['id']}", body=body)
    run = run_agent(srv, agent)
    done = srv.wait_run(run["id"], timeout=60)
    assert done["state"]["type"] == "Failed" and "disabled" in done["state"]["message"]


def test_the_catalogue_names_what_each_tool_needs(srv):
    cat = srv.client._request("GET", "/api/agents/catalogue")
    assert cat["installed"] is True and cat["version"]
    by_name = {t["name"]: t for t in cat["common"]}
    assert "secret:tavily/api_key" in by_name["tavily_search"]["needs"]
    assert any(t["name"] == "WebSearch" for t in cat["capabilities"])
    self_tools = {t["name"]: t for t in cat["self"]["tools"]}
    assert self_tools["list_runs"]["read_only"] is True and self_tools["cancel_run"]["read_only"] is False
    assert cat["allowlist"]["mcp_commands"] == ["npx"] and cat["self"]["ready"] is True
    assert cat["features"]["pydantic_ai"]
    assert srv.client.server()["features"]["pydantic_ai"]


def test_the_allowlist_refuses_at_save_and_disables_later(srv, isolated_home, tmp_path):
    c = srv.client
    with pytest.raises(ApiError) as err:
        create(srv, name="shell", tools=[{"kind": "mcp", "name": "sh", "options": {"command": "bash", "args": ["-c", "id"]}}])
    assert err.value.status == 422 and "bash" in str(err.value) and "mcp_commands" in str(err.value)
    with pytest.raises(ApiError) as err:
        create(srv, name="remote", tools=[{"kind": "mcp", "name": "r", "options": {"url": "https://evil.example/mcp"}}])
    assert err.value.status == 422 and "mcp_urls" in str(err.value)
    agent = create(srv, name="npxer", tools=[{"kind": "mcp", "name": "crm", "options": {"command": "npx", "args": ["-y", "x"]}}])
    assert agent["allow_error"] is None
    # The list shrinks; the agent is disabled rather than run.
    srv.stop()
    directory = tmp_path / "agents"
    (directory / "cereyan.toml").write_text('[agents]\nmcp_urls = ["self"]\n')
    again = ServerProcess(str(isolated_home), str(directory), env={
        "AGENT_TRACE": str(srv.trace), "CEREYAN_TEST_MODELS": "1", "PYDANTIC_AI_NO_BANNER": "1"})
    try:
        got = again.client._request("GET", f"/api/agents/{agent['id']}")
        assert "npx" in got["allow_error"] and "mcp_commands" in got["allow_error"]
        run = again.client._request("POST", f"/api/flows/{got['flow_id']}/runs", body={"parameters": {}})
        done = again.wait_run(run["id"], timeout=60)
        assert done["state"]["type"] == "Failed" and "npx" in done["state"]["message"]
    finally:
        again.stop()


def test_the_daily_cost_cap_stops_new_runs(srv):
    c = srv.client
    agent = create(srv, name="spender")
    run = run_agent(srv, agent)
    assert srv.wait_run(run["id"], timeout=60)["state"]["type"] == "Completed"
    # A scripted model has no price; record what a priced one would have cost today.
    c._request("POST", f"/api/runs/{run['id']}/state", body={"scope": "", "key": "usage", "value": {"requests": 2, "cost_usd": 5.0}})
    got = c._request("GET", f"/api/agents/{agent['id']}")
    assert got["cost_today"] == 5.0 and got["cost_cap"] == 5.0
    capped = run_agent(srv, agent)
    done = srv.wait_run(capped["id"], timeout=60)
    assert done["state"]["type"] == "Failed" and "cost_per_day" in done["state"]["message"]


def test_deleting_an_agent_removes_its_flow_and_keeps_its_runs(srv):
    c = srv.client
    agent = create(srv, name="gone")
    run = run_agent(srv, agent)
    assert srv.wait_run(run["id"], timeout=60)["state"]["type"] == "Completed"
    c._request("DELETE", f"/api/agents/{agent['id']}")
    # The flow stays as history, no longer live, and its runs with it.
    assert c.flow(agent["flow_id"])["live"] is False
    assert c.get_run(run["id"])["state"]["type"] == "Completed"
    assert c._request("GET", "/api/agents") == []
