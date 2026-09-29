-- Remote workers (3.0). A worker is another machine with its own checkout that
-- adds processors to the server's queue. The server keeps the flow definitions;
-- a worker declares which of them it can run and a fingerprint per module, so a
-- run only goes where the code matches.
CREATE TABLE worker (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    version TEXT NOT NULL,
    cpus INTEGER NOT NULL,
    processors INTEGER NOT NULL,
    labels TEXT NOT NULL DEFAULT '{}',
    shared_paths TEXT NOT NULL DEFAULT '[]',
    meta TEXT NOT NULL DEFAULT '{}',
    -- online, draining, offline
    state TEXT NOT NULL DEFAULT 'online',
    registered_at INTEGER NOT NULL,
    last_seen_at INTEGER NOT NULL
);

CREATE TABLE worker_flow (
    worker_id INTEGER NOT NULL REFERENCES worker(id) ON DELETE CASCADE,
    flow_id INTEGER NOT NULL,
    module_hash TEXT NOT NULL,
    PRIMARY KEY (worker_id, flow_id)
);

-- Where a run executed: 'server' or a worker's name, and the processor slot.
ALTER TABLE run ADD COLUMN host TEXT;
ALTER TABLE run ADD COLUMN processor INTEGER;
-- Incremented on every hand-off to an engine; reports under an older lease are
-- refused, so a run rerun after a network split is not also finished elsewhere.
ALTER TABLE run ADD COLUMN lease INTEGER NOT NULL DEFAULT 0;
-- The fingerprint of the module that executed the run.
ALTER TABLE run ADD COLUMN source_hash TEXT;
CREATE INDEX run_host_start ON run (host, start_time) WHERE host IS NOT NULL;
