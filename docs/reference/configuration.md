# Configuration

Cereyan reads `cereyan.toml` from the served directory, a handful of environment variables, and the CLI flags. The keys are validated in `python/cereyan/config.py`; unknown keys produce a warning at startup naming the key. A `[store]` table is an error: the home is set by `--home` or `CEREYAN_HOME` only.

## `cereyan.toml`

```toml
[server]
host = "127.0.0.1"
port = 4200
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
```

### `[server]`

| Key | Type | Default | Meaning |
|---|---|---|---|
| `host` | string | `127.0.0.1` | Bind address. The server warns when bound to a non-loopback address without a token. |
| `port` | integer | `4200` | TCP port; `0` picks a free port. |
| `token` | string | unset | API token required on every `/api/*` route except `/api/health`. |
| `socket` | string | unset | Unix socket path served next to the TCP port; not available on Windows. |
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
| `max_engines`, `engine_max_runs` | integer | as `[server]` | Accepted here for compatibility; `[server]` takes precedence. |

### `[resources]`

Each key is a resource name and its value the total, for example `db = 4`. Resources are shared by every project on the machine.

### `[email]`

| Key | Type | Meaning |
|---|---|---|
| `host`, `port` | string, integer | SMTP server; `host` is required. |
| `tls` | string | `none`, `starttls`, or `tls`. |
| `username`, `password` | string | SMTP credentials. |
| `from` | string | Sender address; required. |

The Settings page (`PATCH /api/settings`) updates resource totals, retention, and the crash retry default and writes them back to `cereyan.toml`.

## Precedence

| Setting | Order, highest first |
|---|---|
| Home | `--home`, `CEREYAN_HOME`, `~/.cereyan` |
| Host and port | `--host`/`--port`, `CEREYAN_HOST`/`CEREYAN_PORT`, `app.serve(host, port)`, `[server]`, `127.0.0.1:4200` |
| Token | `--token`, `CEREYAN_TOKEN`, `app.serve(token=)`, `[server] token` |
| Socket | `--socket`, `CEREYAN_SOCKET`, `app.serve(socket=)`, `[server] socket` |
| `crash_retries` | the flow decorator, `[defaults]`, `--crash-retries`, `5` |

## Environment variables

| Variable | Meaning |
|---|---|
| `CEREYAN_HOME` | Runtime home directory. Engine children inherit it. |
| `CEREYAN_HOST`, `CEREYAN_PORT` | Server bind address. |
| `CEREYAN_TOKEN` | API token required by the server and sent by the CLI, the Python client, `cereyan mcp`, and engine children. |
| `CEREYAN_SOCKET` | Unix socket path served next to the TCP port. |
| `CEREYAN_NO_BROWSER` | Do not open the UI on `serve`. |

Set by the server for its engine children, not for users: `CEREYAN_SERVER` (the server URL) and `CEREYAN_ENGINE_ID`. Two knobs exist for the test suite and benchmarks only: `CEREYAN_RETENTION_INTERVAL` (seconds between retention passes, default hourly) and `CEREYAN_FAST_CRASH_RERUN`.
