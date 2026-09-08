# Exit codes and the discovery file

## CLI exit codes

| Code | Meaning |
|---|---|
| 0 | Success. For `cereyan run`, the run completed. |
| 1 | `cereyan run` only: the run failed. |
| 2 | `cereyan run` only: nothing ran, because the store was locked and no server took the run. |
| 3 | An error before or outside the run: the flow could not be loaded, a parameter did not coerce, a server was needed but unreachable or required a token, or a backfill argument was invalid. |

The full command reference, generated from the parser, is on the [CLI](cli.md) page.

## The runtime home

One home per machine, resolved from `--home`, then `CEREYAN_HOME`, then `~/.cereyan`. It contains:

| Entry | Meaning |
|---|---|
| `db.sqlite` | The store: flows, runs, task runs, logs, events, rules, artifacts, variables, settings. WAL mode, so `db.sqlite-wal` and `db.sqlite-shm` appear while it is open. |
| `db.lock` | OS advisory lock held by the process that owns the store: a running server, or an offline script while it writes. |
| `server.json` | Written by a running server and removed on shutdown; see below. |
| `secret.key` | Created when the first secret variable is set; encrypts secrets at rest. Mode 0600. |
| `storage/` | Persisted task results and cache entries. |

A corrupted `db.sqlite` is moved aside on open and a fresh store is created; history is a cache and the code plus targets are the source of truth.

## `server.json`

A running server records how to reach it so scripts, the CLI, `cereyan mcp`, and engines find it:

```json
{
  "host": "127.0.0.1",
  "port": 4200,
  "url": "http://127.0.0.1:4200",
  "pid": 12345,
  "started_at": 1788998400000000,
  "version": "1.4.0",
  "auth": true,
  "socket": "/tmp/cereyan.sock"
}
```

| Field | Meaning |
|---|---|
| `host`, `port` | What the listener bound. `host` is `0.0.0.0` when the server was started on every interface. |
| `url` | Where to reach it from this machine. When the server bound an unspecified address, this is loopback rather than the bind address, because `0.0.0.0` is not somewhere a client can connect. |
| `pid` | The server process; clients check it is alive before trusting a stale file. |
| `started_at` | Microseconds since the Unix epoch. |
| `version` | The cereyan version. |
| `auth` | Whether a token is required. The token itself is never written. |
| `socket` | The Unix socket path when one is served, else `null`. |

`cereyan.client.read_discovery()` returns it and `cereyan.client.find_server()` turns it into a client after checking `/api/health`.
