# How to make a Pydantic AI agent durable

An agent run makes many model requests and tool calls, and each costs time and money. Add `CereyanDurability` to a [Pydantic AI](https://ai.pydantic.dev) agent and run it from a flow: every model request and tool call becomes a task run with its result checkpointed. A retry, a resume after a pause, or a [crash rerun](retries-timeouts-crashes.md#survive-a-crash) replays the calls that completed instead of making them again.

## Install the extra

<!-- notest: shell command -->
```{.python notest}
pip install "cereyan[pydantic-ai]"
```

This adds `pydantic-ai-slim`; install the model providers you use beside it, such as `pydantic-ai-slim[openai]`. Nothing else in cereyan imports Pydantic AI, so a plain `pip install cereyan` stays free of dependencies.

## Add the capability and run the agent in a flow

```python
from pydantic_ai import Agent
from pydantic_ai.messages import ModelResponse, TextPart, ToolCallPart
from pydantic_ai.models.function import FunctionModel
from cereyan import flow
from cereyan.pydantic_ai import CereyanDurability

def model(messages, info):
    # Stands in for a real model: look something up, then answer.
    last = messages[-1].parts[-1]
    if last.part_kind == "user-prompt":
        return ModelResponse(parts=[ToolCallPart("count_orders", {"day": "2026-03-01"})])
    return ModelResponse(parts=[TextPart(f"There were {last.content} orders.")])

agent = Agent(FunctionModel(model), name="analyst", capabilities=[CereyanDurability()])

@agent.tool_plain
def count_orders(day: str) -> int:
    return 42

@flow(crash_retries=3)
def daily_summary() -> str:
    return agent.run_sync("How many orders were there on 2026-03-01?").output

assert daily_summary() == "There were 42 orders."
```

The agent needs a `name`, which starts the name of each of its task runs. Run it from the flow body, with `run_sync` or with `asyncio.run(agent.run(...))`. On the run page the task runs read `analyst__model.request-0`, `analyst__function_toolset__<agent>.call_tool:count_orders-0`, and so on, numbered in the order the agent made the calls. Outside a flow, and inside a `@task`, the capability does nothing: the agent runs as it would without it, and a task that runs an agent is a single task run.

## What replays and what runs again

A later pass of the run, or its crash rerun, replays a call when its name, its position and its input match the checkpoint:

- a tool call matches on the tool name, its arguments and the id the model gave the call;
- a model request matches on the messages and settings it sends, without their timestamps and run ids.

The first call that does not match ends the replay, and it and everything after it executes again. That happens when:

- you rename the agent, a toolset or the model, so the names no longer match;
- the prompt changed between the attempts;
- the instructions embed something that changes on every pass, such as the current time. Keep such values in the prompt of a single call, or out of the instructions.

A tool that raised `ModelRetry`, needed approval, or failed validation is recorded like any result, so replay continues past it.

```python
from pydantic_ai import Agent
from pydantic_ai.messages import ModelResponse, TextPart, ToolCallPart
from pydantic_ai.models.function import FunctionModel
from cereyan import flow
from cereyan.pydantic_ai import CereyanDurability

calls = []

def model(messages, info):
    last = messages[-1].parts[-1]
    if last.part_kind == "user-prompt":
        return ModelResponse(parts=[ToolCallPart("load", {"table": "orders"})])
    return ModelResponse(parts=[TextPart(str(last.content))])

agent = Agent(FunctionModel(model), name="loader", capabilities=[CereyanDurability()])

@agent.tool_plain
def load(table: str) -> str:
    calls.append(table)
    return f"loaded {table}"

attempts = []

@flow(retries=1, retry_delay=0, checkpoint=True)
def load_then_fail() -> str:
    out = agent.run_sync("Load the orders table.").output
    attempts.append(out)
    if len(attempts) == 1:
        raise RuntimeError("something after the agent failed")
    return out

assert load_then_fail() == "loaded orders"
assert calls == ["orders"]   # the retry replayed the tool call instead of loading again
```

Calls are at least once, as with every durable-execution engine: a crash after a tool's effect and before its checkpoint runs the tool again. Give tools that write elsewhere an idempotency key: inside a tool, `cereyan.runtime.task_run.effect_key` identifies the call and stays the same in a crash rerun and a retry from failure. The run's own id would not, because a crash rerun is a new run. For a tool whose effect cannot take a key, see [Tools that must not repeat](#tools-that-must-not-repeat).

## Ask a person before a tool runs

A Pydantic AI tool can require approval. The agent run then ends with the calls it wants to make, the flow asks with [`wait_for_input`](human-approval.md), and a second agent run continues with the answer:

```python
from pydantic_ai import Agent, DeferredToolRequests, DeferredToolResults
from pydantic_ai.messages import ModelResponse, TextPart, ToolCallPart
from pydantic_ai.models.function import FunctionModel
from cereyan import flow, wait_for_input
from cereyan.pydantic_ai import CereyanDurability

def model(messages, info):
    last = messages[-1].parts[-1]
    if last.part_kind == "user-prompt":
        return ModelResponse(parts=[ToolCallPart("publish", {"table": "orders"})])
    return ModelResponse(parts=[TextPart(str(last.content))])

agent = Agent(
    FunctionModel(model),
    name="publisher",
    output_type=[str, DeferredToolRequests],
    capabilities=[CereyanDurability()],
)

@agent.tool_plain(requires_approval=True)
def publish(table: str) -> str:
    return f"published {table}"

@flow
def publish_with_approval() -> str:
    first = agent.run_sync("Publish the orders table.")
    requests = first.output
    if not isinstance(requests, DeferredToolRequests):
        return requests
    approved = wait_for_input(f"Approve {len(requests.approvals)} call(s)?")
    results = DeferredToolResults(approvals={c.tool_call_id: bool(approved) for c in requests.approvals})
    return agent.run_sync(message_history=first.all_messages(), deferred_tool_results=results).output
```

Served, the run pauses at the question and frees its engine. When it resumes, the first agent run replays without calling the model. After a crash the rerun keeps the answer as well, so the approved tool runs once. A tool cannot ask on its own: `wait_for_input`, `receive`, and the durable waits raise `CereyanError` inside a tool or model call, because pausing works by raising out of the flow body.

## Tools that must not repeat

A tool that sends a message, files a ticket or charges a card should not run twice because the engine died halfway through it. Name it in `once_tools`, the agent counterpart of a [`once` task](side-effects-once.md):

- its start and end reach the server before the agent continues;
- after a crash, a call that started and never finished is *in doubt* and is not executed again on its own.

A probe settles an in-doubt call. It takes the tool's arguments and returns the tool's result when the effect happened, which the model receives as the tool's return, or `NOT_DONE` to execute the tool:

```python
from pydantic_ai import Agent
from pydantic_ai.messages import ModelResponse, TextPart, ToolCallPart
from pydantic_ai.models.function import FunctionModel
from cereyan import NOT_DONE, flow, runtime
from cereyan.pydantic_ai import CereyanDurability

tickets: dict[str, str] = {}   # stands in for a ticketing system

def find_ticket(title: str):
    return tickets.get(runtime.task_run.effect_key, NOT_DONE)

def model(messages, info):
    last = messages[-1].parts[-1]
    if last.part_kind == "user-prompt":
        return ModelResponse(parts=[ToolCallPart("open_ticket", {"title": "orders late"})])
    return ModelResponse(parts=[TextPart(f"Opened {last.content}.")])

agent = Agent(FunctionModel(model), name="oncall",
              capabilities=[CereyanDurability(once_tools={"open_ticket": find_ticket})])

@agent.tool_plain
def open_ticket(title: str) -> str:
    tickets[runtime.task_run.effect_key] = f"T-{len(tickets) + 1}"
    return tickets[runtime.task_run.effect_key]

@flow(crash_retries=3)
def escalate() -> str:
    return agent.run_sync("The orders load is late. Open a ticket.").output

assert escalate() == "Opened T-1."
```

Inside the tool and its probe, `runtime.task_run.effect_key` identifies the call, and a crash rerun keeps it. That is what lets the probe find the ticket the interrupted call opened.

A tool mapped to `None` has no probe. Its in-doubt call comes back from the agent run as an approval request, and `settle` asks a person about it from the flow body, with the same three answers as a `once` task:

- **happened:** the call is denied with a message carrying the result, so the model knows;
- **run again:** the call is approved and executes;
- **fail:** `settle` raises `TaskInDoubt`.

The agent's `output_type` must include `DeferredToolRequests`, and the capability checks that before the first call:

<!-- notest: the in-doubt path needs a crash rerun on a server -->
```{.python notest}
from pydantic_ai import Agent, DeferredToolRequests
from cereyan import flow
from cereyan.pydantic_ai import CereyanDurability, settle

agent = Agent("openai:gpt-5", name="payments", output_type=[str, DeferredToolRequests],
              capabilities=[CereyanDurability(once_tools={"charge": None})])

@flow(crash_retries=3)
def checkout() -> str:
    first = agent.run_sync("Charge order 42.")
    out = first.output
    if isinstance(out, DeferredToolRequests):
        results = settle(out)   # asks about in-doubt calls; add your own approvals to the result
        out = agent.run_sync(message_history=first.all_messages(), deferred_tool_results=results).output
    return out
```

Model requests are never `once`: they change nothing outside, and checkpoints already replay them. A `ModelRetry` makes the model call the tool again under a new call id, which is a new call with a new effect key.

## Tools in parallel

`CereyanDurability(parallel_execution_mode="parallel_ordered_events")` runs the tool calls of one model response concurrently and still replays them in order. The default is `"sequential"`. Plain `"parallel"` is refused, because it orders tool events by completion, which a replay does not repeat.

Related: [Retry, time out and survive crashes](retries-timeouts-crashes.md), [Pause a run for approval](human-approval.md), [Runs and states](../concepts/runs-and-states.md#checkpoints), [Python API](../reference/python-api.md#pydantic-ai).
