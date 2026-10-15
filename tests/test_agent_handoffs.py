"""Handoffs: a rule carries one run's result into the next agent's prompt,
conditions and loop bounds keep a chain finite, and the chain is walkable."""

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

MODELS = '''
import os
from pydantic_ai.messages import ModelResponse, TextPart
from pydantic_ai.models.function import FunctionModel

TRACE = os.environ["AGENT_TRACE"]

def note(line):
    with open(TRACE, "a") as fh:
        fh.write(line + "\\n")

def prompt_of(messages):
    for part in messages[0].parts:
        if part.part_kind == "user-prompt":
            return part.content
    return ""

def digest(model_id):
    def run(messages, info):
        text = prompt_of(messages)
        note("digest:" + text.replace("\\n", " "))
        return ModelResponse(parts=[TextPart("2 failures overnight, blamed on the warehouse")])
    return FunctionModel(run)

def reviewer(model_id):
    def run(messages, info):
        text = prompt_of(messages)
        note("reviewer:" + text.replace("\\n", " "))
        return ModelResponse(parts=[TextPart('{"approved": false, "notes": "cause is the row limit"}')])
    return FunctionModel(run)
'''

PIPELINE = '''
from cereyan import App

app = App("agents")

@app.flow
def produce(day: str = "2026-10-01") -> dict:
    return {"rows": 3, "day": day}

@app.flow
def consume(day: str = "", summary: str = "") -> str:
    return f"{summary} for {day}"

@app.flow
def notifier(reason: str = "") -> str:
    return reason

@app.flow
def frame() -> object:
    class NotJson:
        pass
    return NotJson()
'''


def definition(name: str, **over) -> dict:
    d = {
        "name": name,
        "project": "agents",
        "description": f"test agent {name}",
        "model": f"test:scripted_models:{name}",
        "api_key": None,
        "instructions": "You are a test agent.",
        "prompt": "Handle {topic}.",
        "tools": [],
        "policy": {"approvals": "approve", "once": []},
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
    (d / "pipeline.py").write_text(PIPELINE)
    (d / "cereyan.toml").write_text('[agents]\nmcp_urls = ["self"]\n')
    trace = tmp_path / "trace.txt"
    trace.write_text("")
    server = ServerProcess(str(isolated_home), str(d), env={
        "AGENT_TRACE": str(trace), "CEREYAN_TEST_MODELS": "1", "PYDANTIC_AI_NO_BANNER": "1",
    })
    server.trace = trace
    try:
        yield server
    finally:
        server.stop()


def lines(srv):
    return [ln for ln in srv.trace.read_text().splitlines() if ln]


def wait_until(fn, timeout=60):
    deadline = time.time() + timeout
    while time.time() < deadline:
        v = fn()
        if v:
            return v
        time.sleep(0.2)
    raise AssertionError("condition not met in time")


def runs_of(c, flow: str) -> list[dict]:
    return sorted(c.runs(flow=flow, limit=50)["items"], key=lambda r: r["id"])


def test_a_chain_of_two_agents_loops_once_and_hands_the_result_on(srv):
    c = srv.client
    digest = c._request("POST", "/api/agents", body=definition("digest"))
    reviewer = c._request("POST", "/api/agents", body=definition(
        "reviewer",
        output={"kind": "json", "schema": {"type": "object", "properties": {
            "approved": {"type": "boolean"}, "notes": {"type": "string"}}, "required": ["approved", "notes"]}},
    ))
    forward = c.create_rule({
        "name": "handoff: digest → reviewer",
        "when": {"events": ["run.completed"], "flows": ["digest"], "project": "agents"},
        "do": [{"kind": "run_flow", "flow": "reviewer", "copy_parameters": True,
                "parameters": {"prompt": "Check the blame in: {{ result }}"}}],
    })
    # The way back closes a loop, so it needs a bound.
    with pytest.raises(ApiError) as err:
        c.create_rule({
            "name": "handoff: reviewer → digest",
            "when": {"events": ["run.completed"], "flows": ["reviewer"], "project": "agents"},
            "condition": "not result.approved",
            "do": [{"kind": "run_flow", "flow": "digest", "parameters": {"prompt": "Redo it: {{ result.notes }}"}}],
        })
    assert err.value.status == 422 and "max_per_chain" in str(err.value) and "reviewer → digest → reviewer" in str(err.value)
    back = c.create_rule({
        "name": "handoff: reviewer → digest",
        "when": {"events": ["run.completed"], "flows": ["reviewer"], "project": "agents"},
        "condition": "not result.approved",
        "max_per_chain": 1,
        "on_limit": {"kind": "run_flow", "flow": "notifier", "parameters": {"reason": "still rejected after {{ loop.count }} rounds"}},
        "do": [{"kind": "run_flow", "flow": "digest", "copy_parameters": True,
                "parameters": {"prompt": "Redo it: {{ result.notes }}"}}],
    })
    assert back["condition"] == "not result.approved" and back["max_per_chain"] == 1

    first = c._request("POST", f"/api/flows/{digest['flow_id']}/runs", body={"parameters": {"topic": "overnight"}})
    assert srv.wait_run(first["id"], timeout=60)["state"]["type"] == "Completed"
    assert c.run_state(first["id"], key="result") == {"found": True, "value": "2 failures overnight, blamed on the warehouse"}

    # digest → reviewer (rejects) → digest again → reviewer (rejects) → limit → notifier.
    notified = wait_until(lambda: runs_of(c, "notifier"), timeout=90)[0]
    assert srv.wait_run(notified["id"], timeout=60)["state"]["type"] == "Completed"
    digests, reviews = runs_of(c, "digest"), runs_of(c, "reviewer")
    assert len(digests) == 2 and len(reviews) == 2
    assert reviews[0]["created_by"] == f"rule:{forward['id']}" and reviews[0]["parent_run_id"] == first["id"]
    assert reviews[0]["parameters"]["topic"] == "overnight", "copy_parameters carries shared parameters"
    assert reviews[0]["parameters"]["prompt"] == "Check the blame in: 2 failures overnight, blamed on the warehouse"
    assert digests[1]["created_by"] == f"rule:{back['id']}" and digests[1]["parent_run_id"] == reviews[0]["id"]
    assert digests[1]["parameters"]["prompt"] == "Redo it: cause is the row limit"
    assert notified["parameters"]["reason"] == "still rejected after 1 rounds"
    assert c.run_state(notified["id"], key="result")["value"] == "still rejected after 1 rounds"

    # The upstream result reached each agent as data, after the task.
    seen = lines(srv)
    review_prompts = [ln for ln in seen if ln.startswith("reviewer:")]
    assert len(review_prompts) == 2
    assert "<cereyan-task>" in review_prompts[0] and '<cereyan-handoff from="digest"' in review_prompts[0]
    assert review_prompts[0].index("</cereyan-task>") < review_prompts[0].index("<cereyan-handoff")
    assert "2 failures overnight" in review_prompts[0].split("<cereyan-handoff")[1]
    redo = [ln for ln in seen if ln.startswith("digest:")][1]
    assert '<cereyan-handoff from="reviewer"' in redo and '"approved": false' in redo

    # The limit was recorded, not run.
    firings = sorted(c._request("GET", f"/api/rules/{back['id']}/firings"), key=lambda f: f["id"])
    assert [f["outcomes"][0]["status"] for f in firings] == ["completed", "limited"]
    assert firings[1]["outcomes"][-1]["kind"] == "run_flow" and firings[1]["outcomes"][-1]["status"] == "completed"
    limited = wait_until(lambda: c.events(kind="rule.loop_limited"))
    assert limited[0]["payload"]["rounds"] == 1 and limited[0]["payload"]["limit"] == 1
    assert limited[0]["payload"]["rule_id"] == back["id"] and limited[0]["payload"]["chain_root"] == first["id"]

    # The chain is walkable from any of its runs, with rounds.
    chain = c.run_chain(reviews[1]["id"])
    assert chain["root"] == first["id"]
    assert [(r["flow_name"], r["round"]) for r in chain["runs"]] == [
        ("digest", 1), ("reviewer", 1), ("digest", 2), ("reviewer", 2), ("notifier", 1)]
    assert chain["runs"][1]["rule"] == "handoff: digest → reviewer" and chain["runs"][1]["rule_id"] == forward["id"]
    assert chain["runs"][0]["result"].startswith("2 failures") and chain["runs"][1]["result"].startswith("{")
    assert c.run_chain(first["id"])["runs"] == chain["runs"]

    # The dry run shows what the condition evaluated to.
    dry = c._request("POST", f"/api/rules/{back['id']}/test")
    assert dry["condition"] == {"expression": "not result.approved", "value": True}


def test_a_plain_flow_result_is_recorded_and_templated(srv):
    c = srv.client
    flows = {f["name"]: f for f in c.flows()}
    rule = c.create_rule({
        "name": "consume after produce",
        "when": {"events": ["run.completed"], "flows": ["produce"]},
        "condition": "result.rows > 2",
        "do": [{"kind": "run_flow", "flow": "consume", "copy_parameters": True,
                "parameters": {"summary": "{{ result.rows }} rows"}}],
    })
    run = c._request("POST", f"/api/flows/{flows['produce']['id']}/runs", body={"parameters": {"day": "2026-10-02"}})
    assert srv.wait_run(run["id"], timeout=60)["state"]["type"] == "Completed"
    assert c.run_state(run["id"], key="result")["value"] == {"rows": 3, "day": "2026-10-02"}
    consumed = wait_until(lambda: runs_of(c, "consume"))[0]
    assert consumed["parameters"] == {"day": "2026-10-02", "summary": "3 rows"}
    assert consumed["parent_run_id"] == run["id"] and consumed["created_by"] == f"rule:{rule['id']}"
    assert srv.wait_run(consumed["id"], timeout=60)["state"]["type"] == "Completed"
    assert c.run_state(consumed["id"], key="result")["value"] == "3 rows for 2026-10-02"
    # A result that is not JSON is simply not recorded.
    frame = c._request("POST", f"/api/flows/{flows['frame']['id']}/runs", body={"parameters": {}})
    assert srv.wait_run(frame["id"], timeout=60)["state"]["type"] == "Completed"
    assert c.run_state(frame["id"], key="result")["found"] is False
    # A condition over a missing result is false, quietly.
    c.create_rule({
        "name": "never",
        "when": {"events": ["run.completed"], "flows": ["frame"]},
        "condition": "result.rows > 2",
        "do": [{"kind": "run_flow", "flow": "notifier"}],
    })
    again = c._request("POST", f"/api/flows/{flows['frame']['id']}/runs", body={"parameters": {}})
    assert srv.wait_run(again["id"], timeout=60)["state"]["type"] == "Completed"
    time.sleep(1.0)
    assert runs_of(c, "notifier") == []
    assert not [e for e in c.events(kind="rule.*") if e["name"] == "rule.action.failed"]
    rules = {r["name"]: r for r in c.rules()}
    assert rules["never"]["fire_count"] == 0
    # A bad expression is refused at save.
    with pytest.raises(ApiError) as err:
        c.create_rule({"name": "bad", "when": {"events": ["run.completed"]}, "condition": "result.(",
                       "do": [{"kind": "run_flow", "flow": "notifier"}]})
    assert err.value.status == 422 and "condition" in str(err.value)
