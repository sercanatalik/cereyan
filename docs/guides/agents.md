# How to use cereyan with an AI agent

Cereyan has a built-in [MCP](https://modelcontextprotocol.io) server, so an agent can list flows, start runs, follow and diagnose them, backfill, manage schedules, and answer a paused run's question. Nothing extra to install; it ships in the wheel and runs inside `cereyan serve`.

## Connect Claude Code or Claude Desktop

Hosts that speak stdio start `cereyan mcp`, which proxies every message to the running server. Add to the host's MCP configuration:

```json
{
  "mcpServers": {
    "cereyan": {
      "command": "cereyan",
      "args": ["mcp"],
      "env": {"CEREYAN_TOKEN": "your-token"}
    }
  }
}
```

For Claude Code that is `claude mcp add cereyan -- cereyan mcp` or the same block in `.mcp.json`. `cereyan mcp` finds the server through `server.json` in the runtime home, and `--url` and `--socket` override it. The API token comes from `CEREYAN_TOKEN`, as in the block above; `--token` is a global flag, so on the command line it goes before the subcommand (`cereyan --token <token> mcp`, not `cereyan mcp --token <token>`). With a Unix socket and no token the proxy connects over the socket. Start `cereyan serve` first; the proxy reports an error to the host when no server is running.

## Connect over HTTP

Web agents and custom clients use Streamable HTTP: `POST /mcp` with one JSON-RPC message per request, `Content-Type: application/json` (any other type answers 415), and `Authorization: Bearer <token>` when the server has a token. A client running in a web page on another origin also needs that page's host in [`allowed_hosts`](secure-the-server.md#reach-the-server-under-another-name). `initialize` returns an `Mcp-Session-Id` header to send on later requests; `DELETE /mcp` ends the session.

```{.python fixture:served}
import json, urllib.request

session = {}

def rpc(method, params=None, *, notification=False):
    msg = {"jsonrpc": "2.0", "method": method, "params": params or {}}
    if not notification:
        msg["id"] = 1
    headers = {"content-type": "application/json", **session}
    req = urllib.request.Request(served.url + "/mcp", data=json.dumps(msg).encode(), headers=headers, method="POST")
    with urllib.request.urlopen(req) as resp:
        if resp.headers.get("Mcp-Session-Id"):
            session["Mcp-Session-Id"] = resp.headers["Mcp-Session-Id"]
        raw = resp.read()
    return json.loads(raw) if raw else None

rpc("initialize", {"protocolVersion": "2025-06-18", "clientInfo": {"name": "docs-example", "version": "1"}, "capabilities": {}})
rpc("notifications/initialized", notification=True)
tools = {t["name"] for t in rpc("tools/list")["result"]["tools"]}
assert {"list_flows", "run_flow", "explain_failure", "resume_run"} <= tools
```

## What the agent can do

The tool set is curated: seventeen read-only tools (`list_flows`, `list_runs`, `get_run`, `run_logs`, `list_events`, `list_artifacts`, `list_rules`, `list_schedules`, `explain_failure`, `list_backfills`, `get_backfill`, `get_flow_source`, `server_health`, `list_variables`, `list_resources`, `flow_dependencies`, `check_flows`) and twelve that change state (`run_flow`, `rerun_run`, `cancel_run`, `resume_run`, `backfill`, `cancel_backfill`, `create_schedule`, `edit_schedule`, `delete_schedule`, `pause_schedule`, `resume_schedule`, `set_variable`). Every description states its effect, every tool carries the MCP `readOnlyHint` and `destructiveHint` annotations so a client can gate approval on them, `backfill` dry-runs unless told otherwise, `list_variables` never returns a secret's value, `get_flow_source` reads only from the flow's own source directory, `check_flows` runs [`cereyan check`](test-a-pipeline.md#check-the-directory-before-serving) on the served directory in a child process, and rule creation is not exposed. The full list with each argument's type, default, and range, and the keys every tool returns, is on the [MCP reference](../reference/mcp.md) page.

A flow can publish itself as a tool: `@flow(mcp_tool=True)` lists it as `flow__<project>__<name>` with the flow's parameters as the tool's arguments, so an agent starts it without knowing `run_flow`. Nothing else changes; the run is the same run.

Schedules are the one place where an agent has less room than the flow page: editing a schedule that was declared in code detaches it from that declaration for good and the result says so, and deleting such a schedule is refused, because the declaration would recreate it at the next restart. See [Schedules](../concepts/schedules.md).

Two resource templates, `cereyan://runs/{id}/logs` and `cereyan://runs/{id}/artifacts`, expose a run's logs and artifacts as JSON — they are listed by `resources/templates/list`, not `resources/list`. Three prompts steer common tasks: `diagnose_run` tells the model to call `explain_failure` and summarise the cause, `health_check` walks `server_health`, the failed and crashed runs, and the schedules, and `plan_backfill` dry-runs a backfill and creates it only after reviewing the count.

## Know what the agent did

Runs created through MCP record `created_by = mcp:<client name>` from the `initialize` handshake, so the Runs page, the events feed, and `list_runs` show which agent started what. Filter the Runs page by that value to audit agent activity.

## Authentication and safety

MCP uses the API token or the Unix socket; there is no second permission model. Anyone holding the token, or a local user on the socket, can do through MCP what the API allows, including starting work. Give an agent the socket rather than the token when it runs on the same machine, and see [Secure the server](secure-the-server.md).

To hand an agent a server it can only look at, start the server read-only:

```toml
[server]
mcp_read_only = true
```

Or `--mcp-read-only`, `CEREYAN_MCP_READ_ONLY`, or `app.serve(mcp_read_only=True)`. `tools/list` then returns only the tools whose `readOnlyHint` is true (flow tools included in what is hidden), any other tool call answers an error naming read-only mode, and the `initialize` instructions say so. The REST API and the UI are unaffected; to offer both an agent that reads and one that acts, run a second server on the same home and another port.

## Let the agent answer questions

A flow paused with `wait_for_input` shows its question in `get_run`; the agent answers with `resume_run`. Combine this with a proactive rule on `run.paused` so a human is paged when neither an agent nor a person has answered in time. See [Pause a run for approval](human-approval.md).

## Point the agent at the docs

The whole site is available as one file at [llms-full.txt](https://sercanatalik.github.io/cereyan/llms-full.txt), with [llms.txt](https://sercanatalik.github.io/cereyan/llms.txt) as the index.

Related: [MCP tools, resources and prompts](../reference/mcp.md), the [agent diagnosis example](../examples/agent_diagnosis.md).
