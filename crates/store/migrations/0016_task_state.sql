-- Small durable notes a task keeps for its own later attempts: an external job
-- id, a cursor. Scoped to the run and the task's dynamic key ('' for the flow
-- body), JSON values, gone with the run.
CREATE TABLE task_state (
    run_id INTEGER NOT NULL REFERENCES run(id) ON DELETE CASCADE,
    scope TEXT NOT NULL,
    key TEXT NOT NULL,
    value TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (run_id, scope, key)
);
