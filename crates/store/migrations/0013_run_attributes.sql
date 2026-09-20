-- Searchable attributes a run sets on itself at runtime.
ALTER TABLE run ADD COLUMN attributes TEXT NOT NULL DEFAULT '{}';
