CREATE TABLE schedule (
    id INTEGER PRIMARY KEY,
    external_id BLOB NOT NULL UNIQUE,
    flow_id INTEGER NOT NULL REFERENCES flow(id) ON DELETE CASCADE,
    spec TEXT NOT NULL,
    catchup TEXT NOT NULL DEFAULT 'skip',
    catchup_max INTEGER NOT NULL DEFAULT 100,
    active INTEGER NOT NULL DEFAULT 1,
    paused_reason TEXT,
    paused_until INTEGER,
    source TEXT NOT NULL DEFAULT 'ui',
    code_key TEXT,
    persist INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE INDEX schedule_flow ON schedule (flow_id);

ALTER TABLE run ADD COLUMN schedule_id INTEGER;
ALTER TABLE run ADD COLUMN scheduled_time INTEGER;
ALTER TABLE run ADD COLUMN priority INTEGER NOT NULL DEFAULT 0;
ALTER TABLE run ADD COLUMN parent_run_id INTEGER;
ALTER TABLE run ADD COLUMN attempt INTEGER NOT NULL DEFAULT 0;
ALTER TABLE run ADD COLUMN backfill_id INTEGER;
CREATE INDEX run_schedule ON run (schedule_id, scheduled_time);
CREATE INDEX run_backfill ON run (backfill_id, id);

CREATE TABLE backfill (
    id INTEGER PRIMARY KEY,
    external_id BLOB NOT NULL UNIQUE,
    flow_id INTEGER NOT NULL REFERENCES flow(id) ON DELETE CASCADE,
    parameter TEXT NOT NULL,
    start_value TEXT NOT NULL,
    end_value TEXT NOT NULL,
    interval_secs REAL NOT NULL,
    concurrency INTEGER NOT NULL,
    total INTEGER NOT NULL,
    extra_parameters TEXT NOT NULL DEFAULT '{}',
    cancelled INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL
);

ALTER TABLE task_run ADD COLUMN parents TEXT NOT NULL DEFAULT '[]';

CREATE TABLE kv (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE event (
    id INTEGER PRIMARY KEY,
    external_id BLOB NOT NULL UNIQUE,
    kind TEXT NOT NULL,
    timestamp INTEGER NOT NULL,
    run_id INTEGER,
    flow_id INTEGER,
    payload TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX event_time ON event (timestamp, id);
CREATE INDEX event_kind ON event (kind, id);
