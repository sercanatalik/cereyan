-- A run may carry a unique key (flow, rendered key template, period bucket, or
-- a request's idempotency key). Run creation looks the key up in the states
-- that count before inserting, inside the single writer, so two creations with
-- the same key cannot both succeed. Terminal runs stay in the index: which
-- states count is the flow's choice, checked at creation.
ALTER TABLE run ADD COLUMN unique_key TEXT;
CREATE INDEX run_unique_key ON run (unique_key) WHERE unique_key IS NOT NULL;
