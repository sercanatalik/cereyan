-- Agents defined in the UI: a stored definition that the server registers as a
-- flow on the built-in runner, and the tool calls a person approved for good.
CREATE TABLE IF NOT EXISTS agent (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    external_id BLOB NOT NULL UNIQUE,
    project TEXT NOT NULL,
    name TEXT NOT NULL,
    source TEXT NOT NULL DEFAULT 'ui',
    enabled INTEGER NOT NULL DEFAULT 1,
    definition TEXT NOT NULL,
    flow_id INTEGER REFERENCES flow(id) ON DELETE SET NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    UNIQUE (project, name)
);
CREATE TABLE IF NOT EXISTS remembered_approval (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    agent_id INTEGER NOT NULL REFERENCES agent(id) ON DELETE CASCADE,
    tool TEXT NOT NULL,
    args_hash TEXT NOT NULL,
    args TEXT NOT NULL,
    first_run_id INTEGER,
    created_at INTEGER NOT NULL,
    UNIQUE (agent_id, tool, args_hash)
);
