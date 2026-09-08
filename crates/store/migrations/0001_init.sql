CREATE TABLE flow (
    id INTEGER PRIMARY KEY,
    external_id BLOB NOT NULL UNIQUE,
    project TEXT NOT NULL,
    name TEXT NOT NULL,
    module TEXT NOT NULL,
    source_dir TEXT NOT NULL,
    description TEXT,
    tags TEXT NOT NULL DEFAULT '[]',
    parameter_schema TEXT NOT NULL DEFAULT '{}',
    created_at INTEGER NOT NULL,
    last_seen_at INTEGER NOT NULL,
    UNIQUE (project, name)
);

CREATE TABLE run (
    id INTEGER PRIMARY KEY,
    external_id BLOB NOT NULL UNIQUE,
    flow_id INTEGER NOT NULL REFERENCES flow(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    parameters TEXT NOT NULL DEFAULT '{}',
    tags TEXT NOT NULL DEFAULT '[]',
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
    total_run_time INTEGER
);
CREATE INDEX run_flow_start ON run (flow_id, start_time);
CREATE INDEX run_state_start ON run (state_type, start_time);
CREATE INDEX run_flow_id ON run (flow_id, id);
CREATE INDEX run_state_id ON run (state_type, id);
CREATE INDEX run_name ON run (name);

CREATE TABLE run_state (
    id INTEGER PRIMARY KEY,
    run_id INTEGER NOT NULL REFERENCES run(id) ON DELETE CASCADE,
    type TEXT NOT NULL,
    name TEXT NOT NULL,
    message TEXT,
    details TEXT NOT NULL DEFAULT '{}',
    timestamp INTEGER NOT NULL
);
CREATE INDEX run_state_run ON run_state (run_id, id);

CREATE TABLE task_run (
    id INTEGER PRIMARY KEY,
    external_id BLOB NOT NULL UNIQUE,
    run_id INTEGER NOT NULL REFERENCES run(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    task_key TEXT NOT NULL,
    dynamic_key TEXT NOT NULL,
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
    UNIQUE (run_id, dynamic_key)
);

CREATE TABLE task_run_state (
    id INTEGER PRIMARY KEY,
    task_run_id INTEGER NOT NULL REFERENCES task_run(id) ON DELETE CASCADE,
    type TEXT NOT NULL,
    name TEXT NOT NULL,
    message TEXT,
    details TEXT NOT NULL DEFAULT '{}',
    timestamp INTEGER NOT NULL
);
CREATE INDEX task_run_state_task ON task_run_state (task_run_id, id);

CREATE TABLE log (
    id INTEGER PRIMARY KEY,
    run_id INTEGER NOT NULL REFERENCES run(id) ON DELETE CASCADE,
    task_run_id INTEGER,
    level INTEGER NOT NULL,
    logger TEXT NOT NULL,
    timestamp INTEGER NOT NULL,
    message TEXT NOT NULL
);
CREATE INDEX log_run ON log (run_id, id);
