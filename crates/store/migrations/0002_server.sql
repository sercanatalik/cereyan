ALTER TABLE run ADD COLUMN engine_pid INTEGER;
ALTER TABLE run ADD COLUMN engine_id TEXT;
ALTER TABLE run ADD COLUMN exit_code INTEGER;
ALTER TABLE run ADD COLUMN report_seq INTEGER NOT NULL DEFAULT 0;
ALTER TABLE run ADD COLUMN created_by TEXT NOT NULL DEFAULT 'script';
ALTER TABLE flow ADD COLUMN error TEXT;
ALTER TABLE flow ADD COLUMN options TEXT NOT NULL DEFAULT '{}';
CREATE INDEX task_run_state_id ON task_run (state_type, id);
CREATE INDEX log_task_run ON log (task_run_id, id);
