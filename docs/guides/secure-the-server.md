# How to secure the server

By default the server listens on loopback with no authentication: any process on the machine can use it. Web pages open in your browser cannot, because the server answers only to its own addresses and refuses requests from other sites' pages; see [Reach the server under another name](#reach-the-server-under-another-name). Add a token before binding to another address or exposing MCP, and use the Unix socket to let trusted local processes in without the token.

## Require a token

```toml
[server]
token = "change-me"
```

Or `cereyan serve --token change-me`, `CEREYAN_TOKEN`, or `app.serve(token=...)`, in that precedence. With a token set, every `/api/*` route except `/api/health`, and the `/mcp` endpoint, require `Authorization: Bearer <token>`. Requests without it get 401.

Clients pick the token up from `CEREYAN_TOKEN` or `cereyan --token`; engine children receive it in their environment; the UI prompts for it once and stores it in a `cereyan_token` cookie scoped to `/api` under the base path. `server.json` records `auth: true` and never the token itself.

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

The server warns at start when bound to a non-loopback address without a token. There is no TLS: put a reverse proxy in front if the network is not trusted, as below.

## Put it behind nginx at a sub-path

```toml
[server]
base_path = "/cereyan"
allowed_hosts = ["cereyan.example.com"]
```

Or `--base-path /cereyan`, `CEREYAN_BASE_PATH`, or `app.serve(base_path=...)`. The server then answers everything under that path, custom routes included, and the proxy forwards the path unchanged. `allowed_hosts` names the public host, because the UI's requests carry that origin; nginx sends `Host: 127.0.0.1:4200` upstream, which the server accepts:

```nginx
location /cereyan/ {
    proxy_pass http://127.0.0.1:4200;   # no trailing slash: the path passes through unchanged
    proxy_http_version 1.1;
    proxy_buffering off;                # live updates on /api/stream
    proxy_read_timeout 1h;
}
```

A `proxy_pass` ending in `/` strips `/cereyan`, and every request then answers 404. The same URLs work without the proxy, at `http://127.0.0.1:4200/cereyan/`, and `/` redirects there. The CLI, the Python client, engines, and `cereyan mcp` read the base path from `server.json`; the Unix socket stays at the root. Keep the server on loopback so the proxy, which terminates TLS, is the only way in.

## Reach the server under another name

```toml
[server]
allowed_hosts = ["cereyan.example.com", "build-box.lan"]
```

Or `--allowed-host` (repeat it for more), `CEREYAN_ALLOWED_HOSTS` as a comma-separated list, or `app.serve(allowed_hosts=[...])`; the first that sets it wins whole. Before the token is checked, the server answers two kinds of request with 403, on every path, the UI and custom routes included:

| Refused | Why | Accepted without configuration |
|---|---|---|
| A `Host` naming another host | A site whose name is re-pointed at 127.0.0.1 (DNS rebinding) could read and change everything | IP addresses, `localhost`, the configured `host` |
| An `Origin` from another page | A page on any site could post to the API and `/mcp` from your browser without seeing a reply | The server's own origin |

Names in `allowed_hosts` are accepted in both. Add one when you open the server through it: a LAN name, a tunnel, or a reverse proxy. Entries are host names or IP addresses without a scheme or port; the 403 names the host or origin it refused. The CLI, the Python client, engines, and `cereyan mcp` dial an address and send no `Origin`, so they need nothing, and the Unix socket skips both checks.

## Trust local processes through the socket

```toml
[server]
socket = "/tmp/cereyan.sock"
```

Or `--socket`, `CEREYAN_SOCKET`, or `app.serve(socket=...)`. The server also listens on the Unix socket, created with mode 0600 and removed on shutdown; a stale file from a crashed server is replaced. Requests over the socket skip the token check, because the file permissions are the authentication. `server.json` records the path, the Python client uses it when the server needs a token the client does not have (`Client(socket_path=...)` selects it explicitly), and `cereyan mcp --socket` connects through it. Keep the path short; the OS limits socket paths to about 100 bytes. Not available on Windows.

## Sign in with your identity provider

To accept your organisation's single sign-on instead of, or beside, the token, register an `@app.authenticator` and set `enable_auth`; see [How to sign in with your identity provider](sign-in-with-your-identity-provider.md). The token keeps working, and engines keep using it.

## What a token holder can do

Everything the API allows, including starting runs, backfilling, setting variables, removing projects, resetting the database, and, through MCP, the same for an agent. A user the authenticator signs in can do the same. There is one permission level; there are no read-only tokens.

That includes the Settings page's Environment tab, which lists the server's settings and its process environment. The server hides a value before sending it when the variable's name contains `KEY`, `SECRET`, `TOKEN`, `PASS`, `PWD`, `CREDENTIAL`, `PRIVATE`, `AUTH`, `COOKIE`, or `SESSION`, when it equals the token or the `[email]` password, and for the password inside a `user:password@` URL. A secret under any other name, such as a webhook URL with its key in the path, is shown, so keep such values out of the server's environment on a shared server.

## Custom routes

Routes registered with `@app.get` and friends under `/api/` are behind the token like the built-in API; routes outside `/api/` are open unless `auth_scope = "all"`, which needs the authenticator. Put anything that changes state under `/api/`.

Related: [Configuration](../reference/configuration.md), [Use cereyan with an AI agent](agents.md).
