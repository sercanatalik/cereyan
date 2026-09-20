-- A completed task's result is persisted as a checkpoint of its run so a crash
-- rerun or a resumed attempt can replay it instead of executing the task again.
-- `result_ref` is the ResultStore key of the stored value; `input_hash` is the
-- hash of the bound arguments, compared on replay so changed inputs run anew.
ALTER TABLE task_run ADD COLUMN result_ref TEXT;
ALTER TABLE task_run ADD COLUMN input_hash TEXT;
