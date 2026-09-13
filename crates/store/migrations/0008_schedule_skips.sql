-- Fires of a schedule a person chose to skip. The run materialized for such a
-- fire is marked and ends Skipped at its time instead of starting; the table,
-- not the mark, is the durable record, so a rebuild re-marks from here.
CREATE TABLE schedule_skip (
    schedule_id INTEGER NOT NULL REFERENCES schedule(id) ON DELETE CASCADE,
    fire_time   INTEGER NOT NULL,
    created_at  INTEGER NOT NULL,
    created_by  TEXT NOT NULL,
    PRIMARY KEY (schedule_id, fire_time)
) WITHOUT ROWID;
