# How to secure the server

By default the server listens on loopback with no authentication: anyone on the machine can use it, and nobody off the machine can reach it. Add a token before binding to another address or exposing MCP, and use the Unix socket to let trusted local processes in without the token.

## Require a token

```toml
[server]
token = "change-me"
```

Or `cereyan serve --token change-me`, `CEREYAN_TOKEN`, or `app.serve(token=...)`, in that precedence. With a token set, every `/api/*` route except `/api/health`, and the `/mcp` endpoint, require `Authorization: Bearer <token>`. Requests without it get 401.

Clients pick the token up from `CEREYAN_TOKEN` or `cereyan --token`; engine children receive it in their environment; the UI prompts for it once and stores it in a `cereyan_token` cookie scoped to `/api`. `server.json` records `auth: true` and never the token itself.

```python
from cereyan import client

api = client.Client("http://127.0.0.1:4200", token="change-me")
assert api.token == "change-me"
```

Generate the token with something like `python -c "import secrets; print(secrets.token_urlsafe(32))"` and keep it out of the repository: `cereyan.toml` is usually committed, so prefer the environment variable on shared machines.

## Bind beyond loopback

```toml
[server]
host = "0.0.0.0"
port = 4200
token = "..."
```

The server warns at start when bound to a non-loopback address without a token. There is no TLS: put a reverse proxy in front if the network is not trusted.

## Trust local processes through the socket

```toml
[server]
socket = "/tmp/cereyan.sock"
```

Or `--socket`, `CEREYAN_SOCKET`, or `app.serve(socket=...)`. The server also listens on the Unix socket, created with mode 0600 and removed on shutdown; a stale file from a crashed server is replaced. Requests over the socket skip the token check, because the file permissions are the authentication. `server.json` records the path, the Python client uses it when the server needs a token the client does not have (`Client(socket_path=...)` selects it explicitly), and `cereyan mcp --socket` connects through it. Keep the path short; the OS limits socket paths to about 100 bytes. Not available on Windows.

## What a token holder can do

Everything the API allows, including starting runs, backfilling, setting variables, and, through MCP, the same for an agent. There is one permission level; there are no read-only tokens.

## Custom routes

Routes registered with `@app.get` and friends under `/api/` are behind the token like the built-in API; routes outside `/api/` are open. Put anything that changes state under `/api/`.

Related: [Configuration](../reference/configuration.md), [Use cereyan with an AI agent](agents.md).
