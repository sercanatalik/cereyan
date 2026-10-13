"""cereyan.pydantic_ai: agent model requests and tool calls as replayable task runs."""

from __future__ import annotations

import json
import os
import subprocess
import sys
from datetime import datetime, timezone

import pytest

pytest.importorskip("pydantic_ai.durable_exec", reason="needs the pydantic-ai extra")
os.environ.setdefault("PYDANTIC_AI_NO_BANNER", "1")
# Pydantic AI's own `run_sync` calls the deprecated `asyncio.get_event_loop()`.
pytestmark = pytest.mark.filterwarnings("ignore:There is no current event loop:DeprecationWarning")

from pydantic_ai import Agent, ModelRetry  # noqa: E402
from pydantic_ai.durable_exec import ModelRequestId, ToolsetCallToolId  # noqa: E402
from pydantic_ai.messages import ModelRequest, ModelResponse, TextPart, ToolCallPart, UserPromptPart  # noqa: E402
from pydantic_ai.models.function import AgentInfo, FunctionModel  # noqa: E402

from cereyan import flow, task  # noqa: E402
from cereyan.engine import resolved_home  # noqa: E402
from cereyan.pydantic_ai import CereyanDurability  # noqa: E402
from cereyan.pydantic_ai._identity import input_hash  # noqa: E402
from cereyan.results import result_store  # noqa: E402

FIXTURE = os.path.join(os.path.dirname(os.path.abspath(__file__)), "fixtures", "pydantic_ai_units.json")


def last_run(store):
    run = json.loads(store.list_runs())["items"][0]
    return run, json.loads(store.task_runs(run["id"]))


def script(messages: list, info: AgentInfo) -> ModelResponse:
    """A model that looks something up, is told to retry once, then answers."""
    last = messages[-1].parts[-1]
    kind = getattr(last, "part_kind", "")
    if kind == "user-prompt":
        return ModelResponse(parts=[ToolCallPart("lookup", {"q": "orders"}, tool_call_id="call-1")])
    if kind == "retry-prompt":
        return ModelResponse(parts=[ToolCallPart("lookup", {"q": "orders-2026"}, tool_call_id="call-2")])
    return ModelResponse(parts=[TextPart(f"answer from {last.content}")])


def make_agent(calls: list):
    model_calls = []

    def counted(messages, info):
        model_calls.append(len(messages))
        return script(messages, info)

    agent = Agent(FunctionModel(counted), name="analyst", capabilities=[CereyanDurability()])

    @agent.tool_plain
    def lookup(q: str) -> str:
        calls.append(q)
        if q == "orders":
            raise ModelRetry("be specific: give a year")
        return f"rows for {q}"

    return agent, model_calls


# ---- identity -----------------------------------------------------------------


def _request(prompt: str, when: datetime) -> list:
    return [ModelRequest(parts=[UserPromptPart(prompt, timestamp=when)], run_id="r1")]


def test_a_model_request_identity_ignores_time_and_run_ids():
    op = ModelRequestId(model_name="m")
    t1 = datetime(2026, 1, 1, tzinfo=timezone.utc)
    t2 = datetime(2026, 1, 2, tzinfo=timezone.utc)
    a = input_hash(op, "x", (None, _request("hi", t1), None, None, None))
    b = input_hash(op, "x", (None, _request("hi", t2), None, None, None))
    c = input_hash(op, "x", (None, _request("bye", t1), None, None, None))
    assert a == b and a != c


def test_identical_tool_calls_differ_by_call_id():
    op = ToolsetCallToolId(toolset_kind="function", toolset_id="t")

    class Ctx:
        def __init__(self, i):
            self.tool_call_id = i

    one = input_hash(op, "lookup", ("lookup", {"q": "x"}, Ctx("a"), None))
    two = input_hash(op, "lookup", ("lookup", {"q": "x"}, Ctx("b"), None))
    again = input_hash(op, "lookup", ("lookup", {"q": "x"}, Ctx("a"), None))
    assert one != two and one == again


# ---- replay -------------------------------------------------------------------


def _units(store):
    """Pass-0 units: name and projected payload, the way the golden fixture records them."""
    _, tasks = last_run(store)
    out = []
    for t in sorted((t for t in tasks if t["pass"] == 0), key=lambda t: t["id"]):
        ref = t["result_ref"]
        hit, payload = result_store(resolved_home()).read(ref) if ref else (False, None)
        out.append({"dynamic_key": t["dynamic_key"], "payload": _project(payload) if hit else None})
    return out


def _project(value):
    """Compare content, not a release's field list: optional fields left at None are
    dropped, so a Pydantic AI release that adds one still matches the recording."""
    if isinstance(value, dict):
        drop = {"timestamp", "usage", "provider_response_id", "provider_details", "run_id", "conversation_id"}
        return {k: _project(v) for k, v in value.items() if k not in drop and v is not None}
    if isinstance(value, list):
        return [_project(v) for v in value]
    return value


def test_a_retry_re_executes_no_unit_and_matches_the_recording(store):
    calls: list[str] = []
    agent, model_calls = make_agent(calls)
    seen = []

    @flow(retries=1, retry_delay=0, checkpoint=True)
    def analyse() -> str:
        out = agent.run_sync("how many orders?").output
        if not seen:
            seen.append(1)
            raise RuntimeError("engine died after the agent")
        return out

    assert analyse() == "answer from rows for orders-2026"
    # One execution of every unit: the retry replayed them all.
    assert calls == ["orders", "orders-2026"] and len(model_calls) == 3
    _, tasks = last_run(store)
    second = [t for t in tasks if t["pass"] == 1]
    assert second and all(t["state"]["name"] == "Replayed" for t in second)
    # The ModelRetry was recorded like any result, so replay went straight past it.
    units = _units(store)
    retried = next(u for u in units if u["dynamic_key"].endswith("call_tool:lookup-0"))
    assert retried["payload"]["kind"] == "model_retry"
    if os.environ.get("CEREYAN_UPDATE_SNAPSHOTS") or not os.path.exists(FIXTURE):
        os.makedirs(os.path.dirname(FIXTURE), exist_ok=True)
        with open(FIXTURE, "w") as fh:
            json.dump(units, fh, indent=2, sort_keys=True)
            fh.write("\n")
    with open(FIXTURE) as fh:
        assert units == json.load(fh), "unit names or payloads changed; set CEREYAN_UPDATE_SNAPSHOTS=1 if intended"


def test_a_changed_prompt_asks_the_model_again(store):
    calls: list[str] = []
    agent, model_calls = make_agent(calls)
    prompts = ["how many orders?", "how many refunds?"]

    @flow(retries=1, retry_delay=0, checkpoint=True)
    def analyse() -> str:
        out = agent.run_sync(prompts[0]).output
        if len(prompts) == 2:
            prompts.pop(0)
            raise RuntimeError("again, with another question")
        return out

    analyse()
    # The first request diverged, so the whole conversation ran again.
    assert len(model_calls) == 6


# ---- scope --------------------------------------------------------------------


def test_outside_a_run_nothing_is_recorded(store):
    agent, _ = make_agent([])
    assert agent.run_sync("how many orders?").output.startswith("answer")
    assert json.loads(store.list_runs())["items"] == []


def test_inside_a_task_only_the_task_is_recorded(store):
    agent, _ = make_agent([])

    @task
    def ask() -> str:
        return agent.run_sync("how many orders?").output

    @flow
    def wrapper() -> str:
        return ask()

    wrapper()
    _, tasks = last_run(store)
    assert [t["dynamic_key"] for t in tasks] == ["ask-0"]


def test_plain_parallel_is_refused():
    with pytest.raises(ValueError, match="sequential"):
        CereyanDurability(parallel_execution_mode="parallel")


def test_the_module_names_the_extra_when_pydantic_ai_is_missing(tmp_path):
    # A stub that hides pydantic_ai from the import system, in a fresh interpreter.
    code = (
        "import sys\n"
        "class Hide:\n"
        "    def find_spec(self, name, path=None, target=None):\n"
        "        if name == 'pydantic_ai' or name.startswith('pydantic_ai.'):\n"
        "            raise ImportError('hidden')\n"
        "sys.meta_path.insert(0, Hide())\n"
        "import cereyan\n"
        "try:\n"
        "    import cereyan.pydantic_ai\n"
        "except ImportError as e:\n"
        "    print(e)\n"
    )
    out = subprocess.run([sys.executable, "-c", code], capture_output=True, text=True, check=True)
    assert 'pip install "cereyan[pydantic-ai]"' in out.stdout


# ---- served: crash reruns and approvals ------------------------------------------

AGENTS = '''
import os, time
os.environ.setdefault("PYDANTIC_AI_NO_BANNER", "1")
from cereyan import App, wait_for_input
from cereyan.pydantic_ai import CereyanDurability
from pydantic_ai import Agent, DeferredToolRequests, DeferredToolResults
from pydantic_ai.messages import ModelResponse, TextPart, ToolCallPart
from pydantic_ai.models.function import FunctionModel

app = App("agents")
TRACE = os.environ["AGENT_TRACE"]

def note(line):
    with open(TRACE, "a") as fh:
        fh.write(line + "\\n")

def crash_once(tag):
    marker = TRACE + "." + tag
    if not os.path.exists(marker):
        open(marker, "w").close()
        # Let the engine's batched task updates reach the server first.
        time.sleep(0.5)
        os._exit(7)

def script(messages, info):
    note("model")
    last = messages[-1].parts[-1]
    if last.part_kind == "user-prompt":
        return ModelResponse(parts=[ToolCallPart("lookup", {"q": "orders"}, tool_call_id="call-1")])
    return ModelResponse(parts=[TextPart(f"answer: {last.content}")])

analyst = Agent(FunctionModel(script), name="analyst", capabilities=[CereyanDurability()])

@analyst.tool_plain
def lookup(q: str) -> str:
    note(f"lookup:{q}")
    return f"rows for {q}"

@app.flow(crash_retries=1)
def analyse() -> str:
    out = analyst.run_sync("how many orders?").output
    crash_once("analyse")
    return out

def publish_script(messages, info):
    note("model")
    last = messages[-1].parts[-1]
    if last.part_kind == "user-prompt":
        return ModelResponse(parts=[ToolCallPart("publish", {"table": "orders"}, tool_call_id="pub-1")])
    return ModelResponse(parts=[TextPart(f"done: {last.content}")])

publisher = Agent(
    FunctionModel(publish_script),
    name="publisher",
    output_type=[str, DeferredToolRequests],
    capabilities=[CereyanDurability()],
)

@publisher.tool_plain(requires_approval=True)
def publish(table: str) -> str:
    note(f"publish:{table}")
    return f"published {table}"

@app.flow(crash_retries=1)
def publish_with_approval() -> str:
    first = publisher.run_sync("publish the orders table")
    requests = first.output
    assert isinstance(requests, DeferredToolRequests), requests
    answer = wait_for_input(f"Approve {len(requests.approvals)} call(s)?")
    results = DeferredToolResults(approvals={c.tool_call_id: bool(answer) for c in requests.approvals})
    out = publisher.run_sync(message_history=first.all_messages(), deferred_tool_results=results).output
    crash_once("publish")
    return out
'''


@pytest.fixture
def served(isolated_home, tmp_path):
    from cereyan import engine
    from server_helpers import ServerProcess

    engine.close_store()
    d = tmp_path / "agents"
    d.mkdir()
    (d / "pipeline.py").write_text(AGENTS)
    trace = tmp_path / "trace.txt"
    trace.write_text("")
    srv = ServerProcess(
        str(isolated_home), str(d),
        env={"AGENT_TRACE": str(trace), "CEREYAN_FAST_CRASH_RERUN": "1", "PYDANTIC_AI_NO_BANNER": "1"},
        extra=["--max-engines", "1"],
    )
    srv.trace = trace
    try:
        yield srv
    finally:
        srv.stop()


def _lines(srv):
    return [ln for ln in srv.trace.read_text().splitlines() if ln]


def _start(srv, name):
    fid = next(f["id"] for f in srv.client.flows() if f["name"] == name and f["project"] == "agents")
    return srv.client._request("POST", f"/api/flows/{fid}/runs", body={"parameters": {}})


def _rerun_of(srv, run_id):
    import time

    deadline = time.time() + 40
    while time.time() < deadline:
        for r in srv.client._request("GET", "/api/runs", params={"limit": 50})["items"]:
            if r["created_by"] == f"crash:{run_id}":
                return r
        time.sleep(0.1)
    raise AssertionError(f"no crash rerun of {run_id}\n{srv.read_log()}")


def _settled(srv, run_id):
    return srv.wait_run(run_id, timeout=40, until=lambda r: r["state"]["type"] in ("Completed", "Failed", "Crashed", "Paused"))


def test_a_crash_rerun_replays_the_agent(served):
    run = _start(served, "analyse")
    assert served.wait_run(run["id"], timeout=40)["state"]["type"] == "Crashed"
    rerun = _rerun_of(served, run["id"])
    final = _settled(served, rerun["id"])
    assert final["state"]["type"] == "Completed", final["state"]
    # Two model requests and one tool call, all before the crash; none again after it.
    assert _lines(served) == ["model", "lookup:orders", "model"]
    tasks = served.client._request("GET", f"/api/runs/{rerun['id']}/tasks")
    assert tasks and all(t["state"]["name"] == "Replayed" for t in tasks)


def test_approve_crash_and_continue(served):
    run = _start(served, "publish_with_approval")
    asked = served.wait_run(run["id"], timeout=40, until=lambda r: r["state"]["type"] == "Paused")
    assert asked["state"]["message"] == "Approve 1 call(s)?"
    served.client.resume(run["id"], True)
    assert served.wait_run(run["id"], timeout=40)["state"]["type"] == "Crashed"
    rerun = _rerun_of(served, run["id"])
    final = _settled(served, rerun["id"])
    # The rerun replays both agent runs, reuses the approval and publishes nothing again.
    assert final["state"]["type"] == "Completed", final["state"]
    assert _lines(served) == ["model", "publish:orders", "model"]
    tasks = served.client._request("GET", f"/api/runs/{rerun['id']}/tasks")
    # The second agent run rebuilt its message history with new timestamps and still matched.
    assert [t["state"]["name"] for t in tasks] == ["Replayed"] * 3
