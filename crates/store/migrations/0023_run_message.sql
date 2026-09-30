-- Message queue per run: external messages sent to a running flow before
-- it reaches the matching `receive(topic)` call.
CREATE TABLE IF NOT EXISTS run_message (
    run_id INTEGER NOT NULL REFERENCES run(id) ON DELETE CASCADE,
    topic TEXT NOT NULL,
    payload TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    ordinal INTEGER PRIMARY KEY AUTOINCREMENT,
    consumed_at INTEGER
);
CREATE INDEX IF NOT EXISTS run_message_run_topic ON run_message(run_id, topic, consumed_at);
