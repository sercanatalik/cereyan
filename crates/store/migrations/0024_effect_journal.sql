-- The effect journal: one row per engine effect a flow body performed (an
-- event, an artifact, published state, attributes), by slot `<kind>:<name>-<n>`
-- and a hash of its content, so a later pass or a crash rerun can skip what
-- already happened. Gone with the run.
CREATE TABLE IF NOT EXISTS effect (
    run_id INTEGER NOT NULL REFERENCES run(id) ON DELETE CASCADE,
    pass INTEGER NOT NULL,
    slot TEXT NOT NULL,
    hash TEXT NOT NULL,
    ref TEXT,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (run_id, slot)
);
