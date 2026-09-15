-- Every event that names a run also names its flow, so a project's events are
-- found through one index when it is previewed or removed.
UPDATE event SET flow_id = (SELECT flow_id FROM run WHERE run.id = event.run_id)
 WHERE flow_id IS NULL AND run_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS event_flow ON event (flow_id);
