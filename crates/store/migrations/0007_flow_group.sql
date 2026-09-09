-- A flow's group, declared in Python. NULL means none was declared and the
-- flow is grouped under its project; the fallback is applied on read so a
-- flow follows its project when no group of its own is set.
ALTER TABLE flow ADD COLUMN flow_group TEXT;
