# Performance targets

These targets shaped the design (a Rust core, batched reporting, an in-memory working set) and are enforced by the benchmarks in CI. They are measured on a developer laptop; a small server does better.

| Operation | Target |
|---|---|
| Server start with 1M historical runs | under 200 ms |
| Task transitions ingested | 20k per second |
| Log lines ingested | 100k per second |
| Runs list query at 1M runs | under 10 ms |
| Schedule wake-up drift | under 50 ms |
| Backfill create of 10k runs | under 1 s |
| Warm-pool overhead, Scheduled to user code | under 5 ms |
| Counts endpoint | under 5 ms |

## How they are checked

`just bench` runs two suites:

- **Criterion benchmarks** in the Rust crates for the store's write path, the transition rules, and matching.
- **The end-to-end benchmark** `benches/e2e.py`, which starts a server on a temporary home, generates its own fixtures (a million-run history, wide and log-heavy flows, a long backfill, an interval schedule), and measures each operation through the public API.

The end-to-end benchmark fails when a target is missed or when a number regresses more than 20 percent against the checked-in baseline for the platform, `benches/baseline.<platform>.json`. CI runs it on every push with `--quick`. When a regression is intentional, refresh the baseline in the same change with `just bench-baseline`.

## What makes the numbers

- SQLite in WAL mode with `synchronous=NORMAL`, one writer thread with group commit, and a read pool, so reads never wait for writes.
- Integer rowids for joins and keyset pagination for every list endpoint, so the runs list costs the same at a million rows as at ten.
- An in-memory index of active runs, counters, the schedule heap, resources, and the rule index, so dispatch, counts, and matching do not query the database.
- Engines buffer transitions and logs in their embedded Rust core and report in batches serialised there; reports are accepted up to 64 MB and appended in one write per report.
- Opening a large database after a clean shutdown does not read the whole file.

Related: [Architecture](architecture.md).
