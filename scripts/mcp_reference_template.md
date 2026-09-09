# MCP tools, resources and prompts

The server implements the [Model Context Protocol](https://modelcontextprotocol.io) (JSON-RPC 2.0) so an agent can operate cereyan. The tool set is curated rather than a mirror of the HTTP API. Definitions live in `crates/server/src/mcp.rs`; this page is generated from a snapshot of what a client actually receives. Setup for Claude Code, Claude Desktop, and HTTP clients is in [Use cereyan with an AI agent](../guides/agents.md).

## Transports

| Transport | How | Authentication |
|---|---|---|
| stdio | `cereyan mcp`, started by the host; proxies every message to the running server found through `server.json` (`--url` and `--socket` override) | `CEREYAN_TOKEN`, or `--token` before the subcommand (`cereyan --token <token> mcp`); with a Unix socket and no token it connects over the socket |
| Streamable HTTP | `POST /mcp` with one JSON-RPC message per request; requests get a JSON reply, notifications get 202; `initialize` returns `Mcp-Session-Id` to send on later requests; `DELETE /mcp` ends the session; `GET /mcp` answers 405 | `Authorization: Bearer <token>` when the server has a token |

Runs created through MCP record `created_by = mcp:<client name>` from the `initialize` handshake.

## Handshake

<!-- generated: handshake -->

## Tools

Every tool description states its effect so a model can decide before calling. A tool returns one text content block holding the JSON whose top-level keys are listed as its response; a failure returns `isError: true` and a message instead. Rule creation is not exposed, and a schedule declared in a flow's code cannot be deleted through MCP: the next restart recreates it from the declaration, so pausing it is what lasts.

<!-- generated: tools -->

## Resources

<!-- generated: resources -->

## Prompts

<!-- generated: prompts -->
