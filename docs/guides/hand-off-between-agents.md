# How to hand off between agents

A handoff is a rule: when one agent's run ends, another agent or flow starts with the result in its prompt. The two rules below make a digest, have a reviewer check it, and send the digest back once when the reviewer rejects it.

```{.python fixture:served}
import time

stamp = int(time.time())
digest = served.client._request("POST", "/api/agents", body={
    "name": f"digest_{stamp}", "project": "docs", "model": "openai:gpt-5",
    "instructions": "Summarise what failed overnight.", "prompt": "Write today's digest.",
    "tools": [], "policy": {"approvals": "ask", "once": []},
    "limits": {"tokens": 50000, "requests": 20, "cost_usd": 0.5, "timeout_seconds": 600},
})
reviewer = served.client._request("POST", "/api/agents", body={
    "name": f"reviewer_{stamp}", "project": "docs", "model": "anthropic:claude-sonnet-5-5",
    "instructions": "Check the digest for wrong blame.", "prompt": "Review the digest.",
    "tools": [], "policy": {"approvals": "ask", "once": []},
    "limits": {"tokens": 50000, "requests": 20, "cost_usd": 0.5, "timeout_seconds": 600},
    "output": {"kind": "json", "schema": {"type": "object", "properties": {
        "approved": {"type": "boolean"}, "notes": {"type": "string"}}, "required": ["approved", "notes"]}},
})
forward = served.client.create_rule({
    "name": f"handoff: {digest['name']} → {reviewer['name']}",
    "when": {"events": ["run.completed"], "flows": [digest["name"]], "project": "docs"},
    "do": [{"kind": "run_flow", "flow": reviewer["name"], "copy_parameters": True,
            "parameters": {"prompt": "Check the blame in today's digest."}}],
})
back = served.client.create_rule({
    "name": f"handoff: {reviewer['name']} → {digest['name']}",
    "when": {"events": ["run.completed"], "flows": [reviewer["name"]], "project": "docs"},
    "condition": "not result.approved",
    "max_per_chain": 1,
    "do": [{"kind": "run_flow", "flow": digest["name"],
            "parameters": {"prompt": "Rewrite the digest. The reviewer said: {{ result.notes }}"}}],
})
assert back["condition"] == "not result.approved" and back["max_per_chain"] == 1
for rule in (back, forward):
    served.client._request("DELETE", f"/api/rules/{rule['id']}")
for agent in (reviewer, digest):
    served.client._request("DELETE", f"/api/agents/{agent['id']}")
```

The Handoffs tab on the agent page builds the same rules from a form, and shows them with the chain they last made.

## Say when, and only when

**When this agent ends** picks the events: Completed, Failed or crashed, or any final state. **Only when** is a `condition`, a Jinja expression over the same context the templates see, checked after the match and before the guards. `result` is the run's return value, so `result.approved` reads a field of a JSON output and `not result.approved` is the way back. An undefined value is false, never an error: a run without a result does not fire the rule, and the rule's **Test** button shows what the condition evaluated to against the latest matching event.

## Say what runs, with what

The target is an agent or a flow in the project. For an agent, **With prompt** is a template for its `prompt` parameter: `{{ result }}`, `{{ result.notes }}`, `{{ run.url }}` and `{{ loop.count }}` are in scope. The whole upstream result also reaches the agent as data, inside a `<cereyan-handoff>` block after the task, unless `pass_result` is off, so the prompt says what to do with it rather than repeating it. For a flow, the parameters are templates by name. **Parameters by name** (`copy_parameters`) copies what the two share, such as `day`, before the templates apply.

## Bound the loop

A handoff whose target leads back to this agent closes a loop, and the server refuses it without `max_per_chain`. The bound counts the runs this rule added along the chain's links, so a crash rerun of a round does not count as a new round. At the bound the rule does not fire: the firing is recorded as `limited`, a `rule.loop_limited` event names the rule, the chain's root and the rounds, and `on_limit`, one action such as `run_flow` of a notifier, runs in its place. **Wait between rounds** is the action's `delay`; the next run sits on the Queue page under Starting later until then.

## Read the chain

Every run a rule creates records the run it came from, beside `created_by = rule:<id>`. `GET /api/runs/{id}/chain` (or `client.run_chain`) returns the chain from any of its runs: the root, then every run in creation order with the rule that made it, its round and a preview of its result. The agent page's Handoffs tab shows the last chain as a table with a loop row where a round starts over, and a run page's Details tab names the rule and the run a handoff came from, with the chain beside it.

Related: [Agents](../concepts/agents.md), [React to events with rules](rules.md), [Chain flows](chain-flows.md).
