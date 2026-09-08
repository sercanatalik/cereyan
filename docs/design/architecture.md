# Architecture

```
 ┌──────────────────────────────── cereyan serve dir/ ────────────────────────────────┐
 │  Python process                                                                    │
 │   ├── imports every module under dir/: flows, routes, rules                        │
 │   └── cereyan._core (pyo3)  ──────────────────────────────────────────┐            │
 │        Rust, tokio runtime                                             │            │
 │        ┌─────────────┐  ┌───────────┐  ┌────────────┐  ┌───────────┐  │            │
 │        │ axum HTTP   │  │ scheduler │  │ rules      │  │ supervisor│  │            │
 │        │ /api, /mcp, │  │ timer heap│  │ match +    │  │ engine    │  │            │
 │        │ UI, SSE,    │  │ look-ahead│  │ templates  │  │ pool      │  │            │
 │        │ custom      │  │ catch-up  │  │ expectations│ │ heartbeats│  │            │
 │        │ routes ─────┼──┼──▶ Python │  └────────────┘  └─────┬─────┘  │            │
 │        └──────┬──────┘  └─────┬─────┘        │               │        │            │
 │               └───────────────┴──────────────┴───────────────┘        │            │
 │                                       │  in-memory working set:        │            │
 │                                       │  active runs, counters,        │            │
 │                                       │  schedule heap, resources,     │            │
 │                                       │  rule index                    │            │
 │                                       ▼                                │            │
 │                              ┌─────────────────┐                       │            │
 │                              │ store (SQLite)  │  one writer thread,   │            │
 │                              │ WAL, group      │  read pool,           │            │
 │                              │ commit          │  migrations           │            │
 │                              └─────────────────┘                       │            │
 └─────────────────────────────────────────┬───────────────────────────────────────────┘
                                           │ loopback HTTP: work, report, heartbeat
                          ┌────────────────┴───────────────┐
                          │ engine processes (Python)      │  one module each, warm,
                          │ import module once, run flows  │  recycled on change or
                          │ buffer transitions + logs in   │  after N runs
                          │ embedded core, flush 100 ms    │
                          └────────────────────────────────┘
```

## Crates

| Crate | Role |
|---|---|
| `cereyan-core` | The model (flows, runs, task runs, states, schedules, events, rules), the transition rules, id and time types. No I/O, no async, no dependency on SQLite or Python, so both execution paths share it. |
| `cereyan-store` | SQLite: opening and quarantine, embedded migrations, a single writer thread with group commit, a read pool, secrets encryption, the home directory and its lock. |
| `cereyan-rules` | Matching a rule's `when` and `unless` clauses against events and rendering action templates with minijinja. |
| `cereyan-server` | axum on tokio: the API with an OpenAPI document from utoipa, the SSE stream, the embedded UI, custom-route dispatch into Python, the scheduler, the rules engine and expectations, the engine supervisor, MCP, auth, retention. |
| `cereyan-py` | The pyo3 module `cereyan._core`: the store for the offline path, the server entry point, and the engine's reporting client. |

The Python package is thin: decorators and parameter coercion, targets and results, the CLI, the client, the engine child that executes runs, the route dispatcher, and the MCP stdio proxy.

## The offline path

`python pipeline.py` opens the store through `_core`, takes the advisory lock, and executes the flow in-process. Transitions go through the same `propose` rules as on the server, and the process appends runs, task runs, logs, events, and artifacts directly. If the lock is held by a server, the script submits the run to it instead and streams the logs back.

## The served path

1. The server imports every module under the directory, registers flows (upsert by project and name), routes, and code rules, emits `flow.registered`, and reconciles non-terminal runs from the previous life: engines still alive are adopted, the rest are marked crashed.
2. The scheduler materialises upcoming runs and wakes on a timer heap; rules and expectations use the same heap.
3. A due run is dispatched to an engine keyed by its module and source directory, spawning one when the pool has room. The engine proposes Pending and Running through the API, executes the flow, and buffers task-run transitions and logs in its embedded core, flushing every 100 milliseconds or on flow-level transitions, idempotently by sequence.
4. Every accepted transition updates the in-memory index (active runs, counts, resources), records an event, is pushed to the SSE stream, and is checked against the rule index.
5. Terminal states release resources and engines; crashes are detected by missed heartbeats and a dead PID.

## Identity and time

Rows have integer ids for joins and UUIDv7 external ids for the API. Timestamps are microseconds since the Unix epoch in UTC; schedules evaluate in their IANA timezone.

## The UI

React with Tanstack Router, Query, and Table and shadcn components, built with Vite and embedded in the wheel. It talks to the API through a client generated from the OpenAPI snapshot and updates from the SSE stream; the timeline graph is inline SVG.

Related: [Engines and the home directory](../concepts/engines-and-home.md), [Performance targets](performance.md).
