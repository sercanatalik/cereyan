# Agents

```{.python fixture:served}
import time

agent = served.client._request("POST", "/api/agents", body={
    "name": f"digest_{int(time.time())}",
    "project": "docs",
    "model": "openai:gpt-5",
    "instructions": "Summarise what failed overnight for {team}.",
    "prompt": "Write today's digest.",
    "tools": [{"kind": "capability", "name": "WebSearch", "options": {}}],
    "policy": {"approvals": "ask", "once": []},
    "limits": {"tokens": 50000, "requests": 20, "cost_usd": 0.5, "timeout_seconds": 600},
})
flow = served.client.flow(agent["flow_id"])
assert flow["options"]["agent"] is True
assert sorted(flow["parameter_schema"]["properties"]) == ["prompt", "team"]
served.client._request("DELETE", f"/api/agents/{agent['id']}")
```

An **agent** is a flow you define without writing Python: a model, instructions, a prompt, the tools it may use, and a policy for what it may do on its own. The server stores the definition and registers a flow for it on the built-in runner, so everything that works on a flow works on an agent: schedules, rules, backfills, dependencies, the queue, and the inbox. The flow's parameters are `prompt` and every `{brace}` in the prompt or instructions.

## Running

A run of an agent fetches its definition, builds a [Pydantic AI](https://ai.pydantic.dev) agent from it with the durability capability, and runs it. Every model request and tool call is a task run with its result checkpointed, so a resume after a question or a crash rerun replays what finished instead of paying for it again. When the agent finishes, its output is the run's result and a markdown artifact, and the run's `outcome`, `usage` and attributes carry the summary and the tokens, so lists show them without opening the run. The run page's Conversation tab shows the turns.

An agent runs on the server, with the key named in its definition exported to the provider for that run only. The key is a secret [Variable](variables.md); the definition stores its name.

## Tools

The catalogue (`GET /api/agents/catalogue`) lists what an agent on this server may be given:

| Kind | What it is | Needs |
|---|---|---|
| Capability | A Pydantic AI capability: `WebSearch`, `WebFetch`, `Thinking`, `ToolSearch`, and others | often a model that supports it |
| Common tool | A Pydantic AI common tool: `duckduckgo_search`, `tavily_search`, `web_fetch`, and others | a package, and for some a secret Variable with the key |
| MCP server | An HTTP server by URL, or a stdio command | `pydantic-ai-slim[mcp]`, and the [allowlist](../reference/configuration.md#agents) |
| Self | The server's own MCP tools, read-only ones by default | `self` in the allowlist |

Each need is named on the entry, so an editor shows why a tool is not ready yet.

## Policy and limits

A tool call that requires approval, such as a `self` tool that changes state, follows the agent's policy: `ask` pauses the run with a question in the inbox, `approve` lets it through, `deny` tells the model it is not allowed. An approval answered with *always allow* is remembered for the agent, by tool and arguments, and listed on the agent page where it can be revoked.

Tools named under `once` are [once tools](../guides/side-effects-once.md): a call a crash interrupted is not repeated silently. UI tools have no probe, so such a call asks in the inbox.

Limits cap each run's tokens, model requests, estimated cost and time; `[agents] cost_per_day` caps all agents together. A definition that no longer passes the allowlist is disabled, not run.

## Handoffs

A **handoff** runs the next agent or flow when this one ends, with the result in its prompt. It is a [rule](events-and-rules.md): the match names the agent's flow, the action is `run_flow` of the target, and the rule may carry a `condition` over the result (`not result.approved`) and, when the target points back up the chain, `max_per_chain`, the most rounds the loop may take. Every run a rule creates records the run it came from, so a chain is walkable from any of its runs (`GET /api/runs/{id}/chain`), and the agent page's Handoffs tab shows the links, the rounds used, and the last chain with each run's result, tokens and time.

The upstream result reaches an agent inside a `<cereyan-handoff from="reviewer" run="tidy-moose">` block after the task, marked as data, so a tool that read a hostile page cannot steer the next agent through its output. A chain is finite by construction: the server refuses a rule that closes a loop without a bound, and at the bound the rule records `rule.loop_limited` and runs its `on_limit` action instead.

Related: [Define an agent in the UI](../guides/define-an-agent.md), [Hand off between agents](../guides/hand-off-between-agents.md), [Make a Pydantic AI agent durable](../guides/durable-pydantic-ai-agent.md), [Configuration](../reference/configuration.md#agents).
