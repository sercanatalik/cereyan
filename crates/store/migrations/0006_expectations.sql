-- Proactive rules arm expectations that a later event disarms.
CREATE TABLE expectation (
    id INTEGER PRIMARY KEY,
    rule_id INTEGER NOT NULL REFERENCES rule(id) ON DELETE CASCADE,
    key TEXT NOT NULL,
    run_id INTEGER,
    flow_id INTEGER,
    armed_at INTEGER NOT NULL,
    deadline INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'open'
);
CREATE INDEX expectation_open ON expectation (status, deadline);
CREATE INDEX expectation_rule_key ON expectation (rule_id, key, status);

-- Cross-run artifact browsing by key.
CREATE INDEX artifact_by_key ON artifact (key, id);
