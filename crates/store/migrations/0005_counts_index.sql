-- Per-flow state counts at startup group by (flow_id, state_type); this
-- covering index turns that into an ordered index scan.
CREATE INDEX run_flow_state ON run (flow_id, state_type);
