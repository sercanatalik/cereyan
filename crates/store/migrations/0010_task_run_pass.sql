-- Which execution of a run's body created a task run: the first is pass 0, and
-- a resume or an in-process flow retry is the next. Every pass numbers its task
-- runs from zero, so the same call keeps its dynamic key across passes and the
-- pass tells them apart. Before this, a resumed run asked for `prepare-0` a
-- second time, the insert hit UNIQUE (run_id, dynamic_key), and the whole
-- report carrying it was discarded, losing the rest of that execution.
--
-- SQLite cannot drop a table constraint, so the table is rebuilt. The runner
-- disables foreign keys around the migration transaction: dropping the old
-- table with them enforced deletes it row by row and cascades into
-- task_run_state, which would take every recorded task state with it.
CREATE TABLE task_run_new (
    id INTEGER PRIMARY KEY,
    external_id BLOB NOT NULL UNIQUE,
    run_id INTEGER NOT NULL REFERENCES run(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    task_key TEXT NOT NULL,
    dynamic_key TEXT NOT NULL,
    pass INTEGER NOT NULL DEFAULT 0,
    state_type TEXT,
    state_name TEXT,
    state_message TEXT,
    state_details TEXT NOT NULL DEFAULT '{}',
    state_timestamp INTEGER,
    failure_count INTEGER NOT NULL DEFAULT 0,
    crash_count INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    start_time INTEGER,
    end_time INTEGER,
    total_run_time INTEGER,
    parents TEXT NOT NULL DEFAULT '[]',
    UNIQUE (run_id, pass, dynamic_key)
);

-- Ids are carried over: task_run_state, log and artifact all point at them.
INSERT INTO task_run_new (
    id, external_id, run_id, name, task_key, dynamic_key, pass,
    state_type, state_name, state_message, state_details, state_timestamp,
    failure_count, crash_count, created_at, start_time, end_time, total_run_time, parents)
SELECT
    id, external_id, run_id, name, task_key, dynamic_key, 0,
    state_type, state_name, state_message, state_details, state_timestamp,
    failure_count, crash_count, created_at, start_time, end_time, total_run_time, parents
FROM task_run;

-- Refuse the upgrade rather than lose task runs: when the copy did not take
-- every row, the CHECK fails and that aborts the migration transaction.
CREATE TABLE task_run_rebuild_guard (copied INTEGER NOT NULL CHECK (copied = 1));
INSERT INTO task_run_rebuild_guard (copied)
SELECT CASE
    WHEN (SELECT COUNT(*) FROM task_run_new) = (SELECT COUNT(*) FROM task_run) THEN 1
    ELSE 0
END;
DROP TABLE task_run_rebuild_guard;

DROP TABLE task_run;
ALTER TABLE task_run_new RENAME TO task_run;
CREATE INDEX task_run_state_id ON task_run (state_type, id);
