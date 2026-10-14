-- Task runs of `once` tasks: their effect must not repeat silently, so a run
-- that continues one whose `once` task started and never finished asks first.
-- A side table rather than a column, so the migration can run again.
CREATE TABLE IF NOT EXISTS task_run_once (
    task_run_id INTEGER PRIMARY KEY REFERENCES task_run(id) ON DELETE CASCADE
);
