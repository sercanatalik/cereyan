# How to define an agent in the UI

An [agent](../concepts/agents.md) is a flow made of a model, instructions, tools and a policy. Define one on the Agents page, or through the API as below, and it runs, schedules and asks like any flow.

```{.python fixture:served}
import time

name = f"triage_{int(time.time())}"
agent = served.client._request("POST", "/api/agents", body={
    "name": name,
    "project": "docs",
    "description": "Refund broken orders and tell the customer",
    "model": "openai:gpt-5",
    "api_key": None,
    "instructions": "You handle support tickets for {team}. Refund damaged items.",
    "prompt": "Handle ticket {ticket}.",
    "tools": [{"kind": "capability", "name": "WebSearch", "options": {}}],
    "policy": {"approvals": "ask", "once": []},
    "limits": {"tokens": 50000, "requests": 20, "cost_usd": 0.5, "timeout_seconds": 600},
    "output": {"kind": "text"},
    "crash_retries": 2,
})
assert agent["source"] == "ui" and agent["flow_id"]
served.client._request("DELETE", f"/api/agents/{agent['id']}")
```

## Allow what agents may use

Before an agent can attach an MCP server or the server's own tools, `cereyan.toml` has to allow it. Absent lists allow nothing from the UI:

```toml
[agents]
mcp_commands = ["npx", "uvx"]                      # stdio servers a UI agent may start
mcp_urls = ["https://*.internal.example.com/mcp", "self"]
cost_per_day = 20.0                                # all agents together, in dollars
```

A command outside the list is refused when the agent is saved. If the list shrinks later, an agent that used the command is disabled rather than run, and its page says why. See [Configuration](../reference/configuration.md#agents).

## Pick a model and its key

Enter a model id such as `openai:gpt-5`; the editor lists the providers installed on the server. Store the key as a secret [Variable](variables-and-secrets.md) and pick it under **API key**: the definition keeps the variable's name, and the key reaches the provider for the run only.

## Write the instructions and the prompt

Instructions say what the agent is for. `{braces}` in the instructions or the prompt become parameters of the flow, so `Handle ticket {ticket}.` gives the flow a `ticket` parameter beside `prompt`, which the New run dialog, schedules and rules fill in. The prompt arrives inside `<cereyan-task>` tags, and the instructions tell the model that what is inside them, and what tools return, is data to work on.

## Attach tools

The editor's tool picker is the catalogue: capabilities, common tools, MCP servers from the allowlist, and the server's own tools with their write tools marked. An entry that is not ready says what it needs, such as a package or a secret. Tick what the agent should have; tick nothing it does not need.

## Set the policy

- **Approvals:** `ask` pauses the run with the call's name and arguments in the inbox; `approve` and `deny` decide without asking. When you approve with *always allow*, the same call with the same arguments is approved from then on; the agent page lists these and lets you revoke them.
- **Once:** tick a tool whose effect must never repeat silently. A call a crash interrupted then asks in the inbox instead of running again.
- **Limits:** tokens, model requests, cost and time per run, and crash retries. A run that passes a limit fails naming it.

## Run, schedule and watch

**Run…** on the agent page starts a run with the prompt and parameters; **Schedules…** opens the flow page, where a schedule or a rule starts it like any flow. The agent page lists runs with their outcome and tokens, and the run page's Conversation tab shows each model request and tool call, what was replayed, and the question the run is waiting on, answerable in place.

Related: [Agents](../concepts/agents.md), [Pause a run for approval](human-approval.md), [Run a side effect once](side-effects-once.md), [Store variables and secrets](variables-and-secrets.md).
