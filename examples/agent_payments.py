# ---
# title: An agent that must not pay twice
# description: A Pydantic AI agent whose refund and email tools are once tools, so a crash in the middle of either is settled by a probe or a person instead of repeated.
# order: 14
# fixture: served
# requires: pydantic-ai
# ---
#
# Durable agents replay what finished, but a tool call that was *running* when the
# engine died is another matter: did the refund go through before the crash? Running
# it again might pay twice. Naming a tool in `once_tools` makes cereyan refuse to
# repeat it silently. A call that a crash interrupted is settled first: by the tool's
# probe, which looks for the effect, or by a person, through `settle`. This example
# makes the engine die twice on purpose, once in each tool, and shows both routes.
# Run it against `cereyan serve examples/`; each crash rerun starts a few seconds
# after the crash.

import json
import os
import tempfile
import time

from pydantic_ai import Agent, DeferredToolRequests
from pydantic_ai.messages import ModelResponse, TextPart, ToolCallPart
from pydantic_ai.models.function import FunctionModel

from cereyan import NOT_DONE, client, flow, runtime
from cereyan.pydantic_ai import CereyanDurability, settle

# ## The payment system
#
# A JSON file stands in for the payment provider's records. It lives outside the
# engine, so it survives the engine dying, which is the point. Entries are keyed by
# the tool call's `effect_key`: the same call has the same key in every rerun of the
# run, so the probe can ask "did *this* call already refund?".

LEDGER = os.path.join(tempfile.gettempdir(), "cereyan-example-ledger.json")


def ledger() -> dict:
    try:
        with open(LEDGER) as fh:
            return json.load(fh)
    except (OSError, ValueError):
        return {}


def record(key: str, entry: str) -> None:
    entries = ledger()
    entries[key] = entry
    with open(LEDGER, "w") as fh:
        json.dump(entries, fh)


def engine_dies_once(where: str) -> None:
    """Simulate the machine failing: end the engine process the first time this call
    gets here. Not something a real tool does; it is how the example shows a crash."""
    marker = os.path.join(tempfile.gettempdir(), f"cereyan-example-{runtime.task_run.effect_key}.{where}")
    if not os.path.exists(marker):
        open(marker, "w").close()
        os._exit(1)


# ## The tools
#
# `refund` moves money and then the engine dies: its effect happened, its result never
# reached the server. Its probe finds the refund in the ledger, so the rerun recovers
# the result without paying again.
#
# `email_customer` dies before sending anything. It has no probe, because there is no
# way to ask an inbox whether a message went out. Its rerun hands the decision to the
# flow as an approval request instead.


def find_refund(order_id: str, amount: float):
    return ledger().get(runtime.task_run.effect_key, NOT_DONE)


def refund(order_id: str, amount: float) -> str:
    refund_id = f"re_{order_id}_{int(amount * 100)}"
    record(runtime.task_run.effect_key, refund_id)
    engine_dies_once("after-refund")
    return refund_id


def email_customer(order_id: str, text: str) -> str:
    engine_dies_once("before-email")
    return f"emailed the customer of order {order_id}"


# ## The agent
#
# As in [Durable agent](durable_agent.md), a scripted model keeps the example
# repeatable; `AGENT_MODEL` swaps in a real one. Both tools are named in `once_tools`,
# `refund` with its probe and `email_customer` with `None`. A tool without a probe can
# come back as an approval request, so the agent's `output_type` includes
# `DeferredToolRequests`.


def scripted(messages, info):
    last = messages[-1].parts[-1]
    if last.part_kind == "user-prompt":
        return ModelResponse(parts=[ToolCallPart("refund", {"order_id": "1042", "amount": 39.0}, tool_call_id="refund-1")])
    if getattr(last, "tool_name", None) == "refund":
        text = f"Your refund {last.content} is on its way."
        return ModelResponse(parts=[ToolCallPart("email_customer", {"order_id": "1042", "text": text}, tool_call_id="email-1")])
    return ModelResponse(parts=[TextPart(f"Refunded and told the customer: {last.content}")])


payments = Agent(
    os.environ.get("AGENT_MODEL") or FunctionModel(scripted),
    name="payments",
    instructions="Refund the order, then email the customer.",
    output_type=[str, DeferredToolRequests],
    capabilities=[CereyanDurability(once_tools={"refund": find_refund, "email_customer": None})],
)
payments.tool_plain(refund)
payments.tool_plain(email_customer)

# ## The flow
#
# `crash_retries=2` lets the server rerun the run after each of the two crashes. When
# an agent run ends with deferred requests, `settle` asks a person about every in-doubt
# call, pausing the run, and returns the results for the next agent run.


@flow(group="agents", crash_retries=2)
def refund_ticket(order_id: str = "1042") -> str:
    first = payments.run_sync(f"Order {order_id} arrived broken: refund it and tell the customer.")
    out = first.output
    if isinstance(out, DeferredToolRequests):
        out = payments.run_sync(message_history=first.all_messages(), deferred_tool_results=settle(out)).output
    return out


# ## What you see
#
# The first run crashes while `refund` executes. The tasks rail marks the call `once`
# and reads "Interrupted": the server knew it had started, because a `once` call
# reports its start before it runs.
#
# ![The first refund_ticket run Crashed, engine process exited unexpectedly; its tasks rail shows the model request Completed and the refund tool call, marked once, Interrupted](../images/agent-crashed.png)
#
# Its rerun replays the model request, finds the refund in doubt, and asks the probe,
# which finds it in the ledger: the call completes as `Recovered` without paying
# again. Then `email_customer` starts, and the engine dies again.
#
# ![The second run Crashed: model request Replayed, the refund tool call Recovered with its once badge, a second model request Completed, and the email tool call Interrupted; the log reads recovered by its probe without executing](../images/agent-recovered.png)
#
# The third run replays everything up to the email, which has no probe, so the agent
# run ends with an approval request and `settle` asks. The form offers `happened`,
# `run_again` and `fail`, with an optional result.
#
# ![The third run Paused, asking whether email_customer (email-1) took effect, with the action set to happened and an empty result field, over a rail of replayed units and the deferred email call](../images/agent-in-doubt.png)
#
# ## Driving it from a script
#
# The run crashes in `refund`. Its rerun recovers the refund through the probe and
# crashes in `email_customer`. The rerun after that asks whether the email went out.
# Nothing was sent, but this script answers "happened" to show the model being told,
# then checks the ledger: one refund, however many times the engine died.


def wait_for(run_id: int, predicate, timeout: float = 90.0) -> dict:
    deadline = time.time() + timeout
    while time.time() < deadline:
        run = client.get_run(run_id)
        if predicate(run):
            return run
        time.sleep(0.2)
    raise TimeoutError(f"run {run_id} did not reach the expected state")


def rerun_of(run_id: int) -> dict:
    deadline = time.time() + 90
    while time.time() < deadline:
        for run in client.list_runs(flow="refund_ticket", limit=20):
            if run["created_by"] == f"crash:{run_id}":
                return run
        time.sleep(0.2)
    raise TimeoutError(f"run {run_id} was not rerun")


if __name__ == "__main__":
    api = client.default_client()
    first = client.run("refund_ticket")
    second = rerun_of(first["id"])
    third = rerun_of(second["id"])
    paused = wait_for(third["id"], lambda r: r["state"]["type"] == "Paused")
    print("question:", paused["state"]["details"]["prompt"])
    api.resume(third["id"], {"action": "happened", "result": "emailed the customer of order 1042"})
    done = wait_for(third["id"], lambda r: r["state"]["type"] in ("Completed", "Failed", "Crashed", "Cancelled"))
    print("runs:", first["name"], "crashed,", second["name"], "crashed,", done["name"], done["state"]["type"])
    assert done["state"]["type"] == "Completed", done["state"]
    recovered = [t for t in api.task_runs(second["id"]) if "call_tool:refund" in t["dynamic_key"]]
    assert recovered and recovered[-1]["state"]["name"] == "Recovered", recovered
    key = api._request("GET", f"/api/task-runs/{recovered[-1]['id']}")["effect_key"]
    print("refund", ledger()[key], "recorded once under effect key", key)
    assert ledger()[key] == "re_1042_3900"
