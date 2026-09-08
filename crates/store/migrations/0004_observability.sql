ALTER TABLE event ADD COLUMN resource_kind TEXT NOT NULL DEFAULT 'custom';
ALTER TABLE event ADD COLUMN resource_id TEXT NOT NULL DEFAULT '';
ALTER TABLE event ADD COLUMN resource_name TEXT NOT NULL DEFAULT '';
ALTER TABLE event ADD COLUMN related TEXT NOT NULL DEFAULT '[]';
CREATE INDEX event_resource ON event (resource_kind, resource_id, id);

CREATE TABLE rule (
    id INTEGER PRIMARY KEY,
    external_id BLOB NOT NULL UNIQUE,
    name TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1,
    source TEXT NOT NULL DEFAULT 'ui',
    module TEXT,
    spec TEXT NOT NULL,
    fire_count INTEGER NOT NULL DEFAULT 0,
    last_fired INTEGER,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE rule_firing (
    id INTEGER PRIMARY KEY,
    rule_id INTEGER NOT NULL REFERENCES rule(id) ON DELETE CASCADE,
    event_id INTEGER,
    run_id INTEGER,
    timestamp INTEGER NOT NULL,
    outcomes TEXT NOT NULL DEFAULT '[]'
);
CREATE INDEX rule_firing_rule ON rule_firing (rule_id, id);

CREATE TABLE artifact (
    id INTEGER PRIMARY KEY,
    external_id BLOB NOT NULL UNIQUE,
    run_id INTEGER NOT NULL REFERENCES run(id) ON DELETE CASCADE,
    task_run_id INTEGER,
    kind TEXT NOT NULL,
    key TEXT,
    data TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE INDEX artifact_run ON artifact (run_id, id);
CREATE INDEX artifact_task ON artifact (task_run_id, id);
CREATE UNIQUE INDEX artifact_key ON artifact (run_id, COALESCE(task_run_id, 0), key) WHERE key IS NOT NULL;

CREATE TABLE variable (
    name TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    tags TEXT NOT NULL DEFAULT '[]',
    secret INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
