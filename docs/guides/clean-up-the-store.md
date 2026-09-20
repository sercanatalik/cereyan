# How to clean up the store

Remove a project nothing serves any more, or start the history over, from **Settings → Data** or the HTTP API. Both need API access; with a token set, send it as for any other `/api/*` route.

```bash
curl -X DELETE http://127.0.0.1:4200/api/projects/etl
```

## Remove a project

The Projects table lists every project in the store with its flows, runs, and last run. **Remove…** opens a dialog that counts what goes, from `GET /api/projects/{name}`; type the project's name to confirm.

| Goes | Stays |
|---|---|
| The project's flows, their runs with task runs, logs and artifacts, their schedules and backfills, and the events about them | Rules that match the project, which stop firing until it is served again, and the variables and resources every project shares |

The removal answers 409, and deletes nothing, while this server serves the project or a run of it is Pending, Running, Paused, or Cancelling. Serve another directory, or cancel the runs, first. Scheduled runs go with the project.

Logs and events are deleted 5,000 rows at a time, so removing a large project takes a while but the server keeps answering meanwhile.

## Reset the database

```bash
curl -X POST http://127.0.0.1:4200/api/database/reset \
  -H 'content-type: application/json' -d '{"scope": "history"}'
```

`scope` is required.

| Scope | Deletes | Keeps |
|---|---|---|
| `history` | Every run with its task runs, logs and artifacts, every event, backfill, rule firing and expectation; rule fire counts go back to 0 | Flows, schedules, rules, variables |
| `everything` | The history, flows this server does not serve, schedules and rules made in the UI, and every variable, secrets included | The flows, schedules and rules this server registered from code |

Both scopes cancel runs in progress first: an engine gets the usual cancel, and is killed if it has not stopped after twice `cancel_grace_secs`. Neither touches `cereyan.toml`, settings saved from the Settings page, or `secret.key`. Schedules start again from the time of the reset, without catching up. While a reset runs, anything that would create a run answers 503. Every open UI reloads when it ends.

## Let retention delete old runs

By default retention deletes only logs and events older than `retain_days`. Set `retain_runs_days` to delete terminal runs too, and the pass, which runs hourly, removes each expired run with its task runs, states, logs, events, artifacts, and the answer it was resumed with:

```toml
[defaults]
retain_runs_days = 30          # Completed and Cancelled runs
retain_failed_runs_days = 90   # Failed and Crashed runs, kept longer for diagnosis
keep_last_runs_per_flow = 10   # a flow's newest finished runs always stay
```

Or set them on the Settings page, which writes them back to `cereyan.toml`. A run's age is its end time. Running runs, and runs of a backfill that is not cancelled and still has queued runs, are never deleted. Deletes go in batches of 500 runs per transaction with the same pauses as log retention, so the API stays responsive. Persisted results under `<home>/storage/` are a cache keyed by task inputs, shared across runs, and are not deleted with a run.

## Keep a copy

`backup` defaults to `true`: before deleting anything the reset writes a consistent copy to `<home>/backups/db-<YYYYMMDD-HHMMSS>.sqlite`, in UTC. If the copy fails, the reset stops there and nothing is deleted. The copy needs as much free disk as the database.

The same copy can be scheduled and made on demand:

```toml
[defaults]
backup_every = 24   # hours; the hourly retention pass writes the copy when one is due
backup_keep = 7     # older db-*.sqlite copies are deleted
```

```bash
cereyan backup            # through the running server, or directly when none is up
```

`POST /api/database/backup` does the same over HTTP, the Data tab has a *Back up now* button, and Settings reports the last copy's time and path. Copies are written with `VACUUM INTO`, so they are compact and consistent while the server keeps running.

Before a schema upgrade, the store writes `<home>/backups/pre-migration-v<old>-<time>.sqlite` on its own and refuses to migrate if the copy cannot be written; `CEREYAN_NO_MIGRATION_BACKUP=1` skips it. Those copies are never pruned.

To go back to a copy, stop the server, replace `db.sqlite` in the home with it, delete `db.sqlite-wal` and `db.sqlite-shm`, and start the server.

## Without a running server

There is no reset command. Stop the server and delete `db.sqlite`, `db.sqlite-wal`, and `db.sqlite-shm` from the home; the next start creates an empty store. `cereyan backup` works without a server and writes the copy directly.

Related: [Configuration](../reference/configuration.md) · [How to secure the server](secure-the-server.md) · [How to run the server as a service](run-as-a-service.md)
