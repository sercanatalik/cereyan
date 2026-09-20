-- Schedule policies: a catch-up window, a deterministic jitter, and a start deadline.
ALTER TABLE schedule ADD COLUMN catchup_window INTEGER;
ALTER TABLE schedule ADD COLUMN jitter INTEGER NOT NULL DEFAULT 0;
ALTER TABLE schedule ADD COLUMN start_deadline INTEGER;
