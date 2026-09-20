# Configuration

Cereyan reads `cereyan.toml` from the served directory, a handful of environment variables, and the CLI flags. The keys are validated in `python/cereyan/config.py`; unknown keys produce a warning at startup naming the key. A `[store]` table is an error: the home is set by `--home` or `CEREYAN_HOME` only.

## `cereyan.toml`

```toml
[server]
host = "127.0.0.1"
port = 4200
base_path = "/cereyan"        # optional: serve everything under this URL path
token = "change-me"           # optional: require Authorization: Bearer on the API
socket = "/tmp/cereyan.sock"  # optional: also listen on a Unix socket (Unix only)
max_engines = 8
engine_max_runs = 100
cancel_grace_secs = 10
open_browser = true

[defaults]
catchup = "skip"
crash_retries = 5
retain_days = 30

[resources]
db = 4
gpu = 1

[email]
host = "smtp.example.com"
port = 587
tls = "starttls"   # none, starttls, tls
username = "..."
password = "..."
from = "cereyan@example.com"

[ui]
title = "Data Platform"   # optional: shown in the top bar and the browser tab
```

### `[server]`

| Key | Type | Default | Meaning |
|---|---|---|---|
| `host` | string | `127.0.0.1` | Bind address. Bound to a non-loopback address without a token, the server generates one into `<home>/token` and requires it, unless `allow_unauthenticated` is set. |
| `port` | integer | `4200` | TCP port; `0` picks a free port. |
| `base_path` | string | the root | URL path the TCP listener serves the UI, the API, `/mcp`, and custom routes under, for example `/cereyan`. Segments are letters, digits, `-`, `_`, `.`, or `~`. `/` and the bare base path redirect to `{base_path}/`; other paths outside it answer 404. The Unix socket stays at the root. |
| `token` | string | unset | API token required on every `/api/*` route except `/api/health`, and on `/mcp`. Unset beyond loopback, the server generates one; see `host`. |
| `socket` | string | unset | Unix socket path served next to the TCP port; not available on Windows. |
| `enable_auth` | boolean | `false` | Validate credentials with the registered `@app.authenticator`. The server refuses to start when it is true and none is registered. |
| `auth_cookie` | string | unset | Cookie the authenticator reads the credential from when there is no `Authorization: Bearer` header. `cereyan_token` is reserved. |
| `auth_scope` | string | `api` | `api` checks `/api/*` except `/api/health`, and `/mcp`; `all` checks every path except `/api/health`, the UI and custom routes included, and needs `enable_auth`. |
| `login_url` | string | unset | Sign-in page linked from 401 responses and the UI: an `http` or `https` URL, or a path starting with `/`. |
| `allowed_hosts` | array of strings | `[]` | Host names the server answers to and accepts browser pages from, beside IP addresses, `localhost`, and `host`. A request whose `Host` names another host, or whose `Origin` is neither the server's own nor a listed host, answers 403. Entries are host names or IP addresses, without a scheme or port. |
| `allow_unauthenticated` | boolean | `false` | Serve without a token when bound beyond loopback instead of generating one. The server warns at start, reports `exposed: true`, and the UI shows a banner. No effect with a token or `enable_auth`, or on loopback. |
| `mcp_read_only` | boolean | `false` | List and allow only MCP tools that change nothing; every other tool call answers an error. The REST API and the UI are unaffected. |
| `max_engines` | integer | CPU count | Size of the warm engine pool. |
| `engine_max_runs` | integer | `100` | Runs an engine executes before it is recycled. |
| `cancel_grace_secs` | integer | `10` | Seconds between SIGTERM and SIGKILL when cancelling a run. |
| `open_browser` | boolean | `true` | Open the UI when the server starts. |

### `[defaults]`

| Key | Type | Default | Meaning |
|---|---|---|---|
| `catchup` | string | `skip` | Catch-up policy for schedules that do not set one: `skip`, `latest`, or `all`. |
| `crash_retries` | integer | `5` | Reruns of a crashed run before it is marked Failed. |
| `retain_days` | integer | `30` | Days of logs and events kept by retention. |

### `[resources]`

Each key is a resource name and its value the total, for example `db = 4`. Resources are shared by every project on the machine.

### `[email]`

| Key | Type | Meaning |
|---|---|---|
| `host`, `port` | string, integer | SMTP server; `host` is required. |
| `tls` | string | `none`, `starttls`, or `tls`. |
| `username`, `password` | string | SMTP credentials. |
| `from` | string | Sender address; required. |

### `[ui]`

| Key | Type | Default | Meaning |
|---|---|---|---|
| `title` | string | `cereyan` | Name shown next to the mark in the top bar, cut to 240 px with an ellipsis, and in full as the browser tab title. It is trimmed and may have at most 80 characters and no control characters; an invalid title logs a warning at startup and `cereyan` shows instead. |

The Settings page (`PATCH /api/settings`) updates resource totals, retention, the crash retry default, and the UI title and writes them back to `cereyan.toml`. Writing back rewrites the file, so comments in it are not kept. A server with no served directory has no file to write: a title saved there lasts until the server restarts.

The Settings page's Environment tab (`GET /api/settings/environment`) lists every setting in effect with where its value came from, following the precedence below: the flag, the environment variable, `app.serve()`, `cereyan.toml`, an edit in Settings, or the default. It shows `cereyan.toml` with `[server] token` and `[email] password` replaced, and never sends either value.

## Precedence

| Setting | Order, highest first |
|---|---|
| Home | `--home`, `CEREYAN_HOME`, `~/.cereyan` |
| Host and port | `--host`/`--port`, `CEREYAN_HOST`/`CEREYAN_PORT`, `app.serve(host, port)`, `[server]`, `127.0.0.1:4200` |
| Base path | `--base-path`, `CEREYAN_BASE_PATH`, `app.serve(base_path=)`, `[server] base_path`, the root |
| Token | `--token`, `CEREYAN_TOKEN`, `app.serve(token=)`, `[server] token` |
| Socket | `--socket`, `CEREYAN_SOCKET`, `app.serve(socket=)`, `[server] socket` |
| `enable_auth` | `--enable-auth`, `CEREYAN_ENABLE_AUTH`, `app.serve(enable_auth=)`, `[server] enable_auth`, `false` |
| `auth_cookie`, `auth_scope`, `login_url` | `--auth-cookie`/`--auth-scope`/`--login-url`, `CEREYAN_AUTH_COOKIE`/`CEREYAN_AUTH_SCOPE`/`CEREYAN_LOGIN_URL`, the same-named `app.serve()` arguments, `[server]` |
| `allowed_hosts` | `--allowed-host` (repeatable), `CEREYAN_ALLOWED_HOSTS` (comma-separated), `app.serve(allowed_hosts=)`, `[server] allowed_hosts`, empty; the first that sets it wins whole |
| `allow_unauthenticated` | `--allow-unauthenticated`, `CEREYAN_ALLOW_UNAUTHENTICATED`, `app.serve(allow_unauthenticated=)`, `[server] allow_unauthenticated`, `false` |
| `mcp_read_only` | `--mcp-read-only`, `CEREYAN_MCP_READ_ONLY`, `app.serve(mcp_read_only=)`, `[server] mcp_read_only`, `false` |
| `crash_retries` | the flow decorator, `[defaults]`, `--crash-retries`, `5` |

## Environment variables

| Variable | Meaning |
|---|---|
| `CEREYAN_HOME` | Runtime home directory. Engine children inherit it. |
| `CEREYAN_HOST`, `CEREYAN_PORT` | Server bind address. |
| `CEREYAN_BASE_PATH` | URL path to serve under. |
| `CEREYAN_TOKEN` | API token required by the server and sent by the CLI, the Python client, `cereyan mcp`, and engine children. |
| `CEREYAN_SOCKET` | Unix socket path served next to the TCP port. |
| `CEREYAN_ENABLE_AUTH` | `true`, `false`, `1`, `0`, `yes`, or `no`: call the registered authenticator. |
| `CEREYAN_AUTH_COOKIE`, `CEREYAN_AUTH_SCOPE`, `CEREYAN_LOGIN_URL` | The authenticator's cookie, the paths checked, and the sign-in page; as the `[server]` keys. |
| `CEREYAN_ALLOWED_HOSTS` | Comma-separated host names the server answers to; as `[server] allowed_hosts`. |
| `CEREYAN_ALLOW_UNAUTHENTICATED` | `true`, `false`, `1`, `0`, `yes`, or `no`: serve without a token beyond loopback; as `[server] allow_unauthenticated`. |
| `CEREYAN_MCP_READ_ONLY` | `true`, `false`, `1`, `0`, `yes`, or `no`: hide and refuse MCP tools that change state; as `[server] mcp_read_only`. |
| `CEREYAN_NO_BROWSER` | Do not open the UI on `serve`. |

Set by the server for its engine children, not for users: `CEREYAN_ENGINE_ID`. An engine child finds the server through the `url` in `server.json`, base path included; `CEREYAN_SERVER` overrides that URL for an engine started by hand. Two knobs exist for the test suite and benchmarks only: `CEREYAN_RETENTION_INTERVAL` (seconds between retention passes, default hourly) and `CEREYAN_FAST_CRASH_RERUN`.
