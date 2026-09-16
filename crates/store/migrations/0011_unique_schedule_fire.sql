-- One fire of a schedule, one run.
--
-- `run_schedule` was an index, not a constraint, and two paths created a second
-- run for a fire that already had one: catch-up recreating a fire the
-- look-ahead had already materialised (a machine that sleeps through its own
-- look-ahead does this every time), and two `materialize` calls for one
-- schedule racing on the blocking pool.
--
-- Duplicates already recorded are kept but unlinked: the runs happened, and
-- deleting them would destroy a record of work that really ran. Only the
-- earliest run of each fire goes on claiming to be that fire.
UPDATE run
   SET schedule_id = NULL
 WHERE schedule_id IS NOT NULL
   AND scheduled_time IS NOT NULL
   AND id NOT IN (
       SELECT MIN(id) FROM run
        WHERE schedule_id IS NOT NULL AND scheduled_time IS NOT NULL
        GROUP BY schedule_id, scheduled_time
   );

-- Replaces the non-unique index: the same lookup, now a constraint. Partial, so
-- a run with no schedule is untouched and ad-hoc runs may share a timestamp.
DROP INDEX IF EXISTS run_schedule;
CREATE UNIQUE INDEX run_schedule ON run (schedule_id, scheduled_time)
    WHERE schedule_id IS NOT NULL AND scheduled_time IS NOT NULL;
