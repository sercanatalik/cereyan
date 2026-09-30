//! SQLite store: one writer thread with group commit, a read pool, an OS
//! advisory lock, embedded migrations, and corruption quarantine.

mod error;
mod home;
mod manage;
mod migrations;
mod open;
mod read;
mod row;
pub mod secrets;
mod workers;
mod writer;

pub use error::StoreError;
pub use home::resolve_home;
pub use manage::{ProjectCounts, ProjectRow, TableCounts, BACKUP_DIR};
pub use migrations::latest_version as latest_schema_version;
pub use read::{
    checkpoint_seed_key, ArtifactFilter, ArtifactsPage, Checkpoint, EventFilter, EventsPage,
    FlowLabel, LatestRunMark, QueueRun, RecentRun, RunEventContext, ScheduleRunMark,
    TaskStateRow,
    TimelineRunRow,
};
pub use read::{ListRunsFilter, ListTaskRunsFilter, LogFilter, LogsPage, RunsPage, TaskRunsPage};
pub use workers::{HostFlowCounts, WorkerRegistration};
pub use writer::{
    ArmExpectation, CreateBackfill, CreateRun, CreateTaskRun, DeletedCounts, FlowRows, NewEvent,
    NewLog, ReportEvent, ReportOutcome, ResetScope, RuleWrite, SchedulePatch, ScheduleWrite,
    UniqueCheck, UpsertArtifact, UpsertFlow, WriteCommand,
};

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use cereyan_core::{Event, Id, State};
use crossbeam_channel::{bounded, Receiver, Sender};
use rusqlite::Connection;

pub type Result<T> = std::result::Result<T, StoreError>;

/// Lock-file name inside the home directory.
pub const LOCK_FILE: &str = "db.lock";
/// Database file name inside the home directory.
pub const DB_FILE: &str = "db.sqlite";

/// Read-only connection pool size. Sized to handle concurrent API requests
/// (dashboard polling, SSE streams, rule evaluation) without opening ad-hoc
/// connections. Each connection uses ~2MB of cache; 12 connections ≈ 24MB.
const READ_POOL_SIZE: usize = 12;

/// How long a read waits for a pooled connection before opening one instead.
///
/// Far longer than any read here performs — the slowest holds a connection
/// across three statements — and far shorter than a request timeout, so a
/// saturated pool degrades to opening a connection instead of wedging callers.
const READER_WAIT: std::time::Duration = std::time::Duration::from_millis(250);

pub struct Store {
    home: PathBuf,
    writer: Sender<WriteCommand>,
    readers: Mutex<Vec<Connection>>,
    /// Signalled whenever a read returns a connection, so a waiting read takes
    /// it instead of opening another.
    reader_returned: std::sync::Condvar,
    _lock: std::fs::File,
    writer_thread: Mutex<Option<std::thread::JoinHandle<()>>>,
    commits: Arc<AtomicU64>,
    commit_stats: Arc<writer::CommitStats>,
}

impl Store {
    /// Open the store at `home`, taking the advisory lock and applying
    /// migrations. The directory is created when missing.
    pub fn open(home: &Path) -> Result<Store> {
        open::ensure_home(home)?;
        let lock = open::take_lock(home)?;
        let db_path = home.join(DB_FILE);
        let write_conn = open::open_writer(&db_path)?;
        migrations::backup_before_upgrade(&write_conn, home)?;
        migrations::apply(&write_conn)?;

        let mut readers = Vec::with_capacity(READ_POOL_SIZE);
        for _ in 0..READ_POOL_SIZE {
            readers.push(open::open_reader(&db_path)?);
        }
        let reader_returned = std::sync::Condvar::new();

        let (tx, rx) = bounded::<WriteCommand>(16_384);
        let commits = Arc::new(AtomicU64::new(0));
        let counter = commits.clone();
        let commit_stats = Arc::new(writer::CommitStats::default());
        let stats = commit_stats.clone();
        let handle = std::thread::Builder::new()
            .name("cereyan-writer".into())
            .spawn(move || writer::run(write_conn, rx, counter, stats))?;

        Ok(Store {
            home: home.to_path_buf(),
            writer: tx,
            readers: Mutex::new(readers),
            reader_returned,
            _lock: lock,
            writer_thread: Mutex::new(Some(handle)),
            commits,
            commit_stats,
        })
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    /// Queue a write without waiting. The returned receiver yields the result
    /// once the containing transaction has committed.
    pub fn submit<T: Send + 'static>(
        &self,
        build: impl FnOnce(writer::Reply<T>) -> WriteCommand,
    ) -> Result<Receiver<Result<T>>> {
        let (reply_tx, reply_rx) = bounded(1);
        self.writer
            .send(build(reply_tx))
            .map_err(|_| StoreError::WriterGone)?;
        Ok(reply_rx)
    }

    /// Submit a write and wait for its acknowledgement, which arrives only
    /// after the containing transaction has committed.
    pub fn write<T: Send + 'static>(
        &self,
        build: impl FnOnce(writer::Reply<T>) -> WriteCommand,
    ) -> Result<T> {
        self.submit(build)?
            .recv()
            .map_err(|_| StoreError::WriterGone)?
    }

    /// Number of write transactions committed since open.
    pub fn commit_count(&self) -> u64 {
        self.commits.load(Ordering::Relaxed)
    }

    /// Commit latency: `(bounds with cumulative counts, sum in seconds, count)`.
    pub fn commit_stats(&self) -> (Vec<(f64, u64)>, f64, u64) {
        self.commit_stats.snapshot()
    }

    /// Writes queued for the writer thread.
    pub fn write_queue_len(&self) -> usize {
        self.writer.len()
    }

    /// Borrow a read-only connection from the pool.
    ///
    /// When the pool is empty the read waits for a connection to come back
    /// rather than opening one. Opening is not free — the four PRAGMAs and the
    /// schema parse cost far more than a small query, and the connection would
    /// then be closed rather than pooled, so the next read pays it again. The
    /// wait is bounded, so a saturated pool degrades to opening a connection
    /// instead of blocking the caller indefinitely.
    pub fn with_reader<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let conn = {
            let mut pool = self.readers.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if let Some(c) = pool.pop() {
                    break c;
                }
                let (guard, _timed_out) = self
                    .reader_returned
                    .wait_timeout(pool, READER_WAIT)
                    .unwrap_or_else(|e| e.into_inner());
                pool = guard;
                if _timed_out.timed_out() {
                    // Nobody returned one in time. Degrade to an ad-hoc
                    // connection rather than wait without bound.
                    break open::open_reader(&self.home.join(DB_FILE))?;
                }
            }
        };
        // Guard returns the connection to the pool on drop, even if `f` panics,
        // and wakes a waiting read either way.
        struct ConnGuard<'a> {
            conn: Option<Connection>,
            store: &'a Store,
        }
        impl Drop for ConnGuard<'_> {
            fn drop(&mut self) {
                if let Some(conn) = self.conn.take() {
                    let mut pool = self.store.readers.lock().unwrap_or_else(|e| e.into_inner());
                    if pool.len() < READ_POOL_SIZE {
                        pool.push(conn);
                    }
                    // Signalled after the push, and outside the `if` above: a
                    // connection opened as an overflow is not pooled, but the
                    // waiter should still re-check in case room appeared.
                    self.store.reader_returned.notify_one();
                }
            }
        }
        let mut guard = ConnGuard {
            conn: Some(conn),
            store: self,
        };
        let result = f(guard.conn.as_ref().expect("conn is Some"));
        // On success, return the connection to the pool explicitly.
        let conn = guard.conn.take().expect("conn is Some");
        let mut pool = self.readers.lock().unwrap_or_else(|e| e.into_inner());
        if pool.len() < READ_POOL_SIZE {
            pool.push(conn);
        }
        drop(pool);
        self.reader_returned.notify_one();
        result
    }

    // ---- write helpers -----------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub fn upsert_flow(
        &self,
        project: &str,
        name: &str,
        module: &str,
        source_dir: &str,
        description: Option<&str>,
        tags: &str,
        parameter_schema: &str,
    ) -> Result<i64> {
        self.upsert_flow_full(writer::UpsertFlow {
            project: project.into(),
            name: name.into(),
            module: module.into(),
            source_dir: source_dir.into(),
            description: description.map(Into::into),
            tags: tags.into(),
            parameter_schema: parameter_schema.into(),
            options: "{}".into(),
            group: None,
        })
    }

    pub fn upsert_flow_full(&self, cmd: writer::UpsertFlow) -> Result<i64> {
        self.write(|reply| WriteCommand::UpsertFlow(cmd, reply))
    }

    pub fn set_flow_error(&self, flow_id: i64, error: Option<String>) -> Result<()> {
        self.write(|reply| WriteCommand::SetFlowError {
            flow_id,
            error,
            reply,
        })
    }

    pub fn delete_flow(&self, flow_id: i64) -> Result<bool> {
        self.write(|reply| WriteCommand::DeleteFlow { flow_id, reply })
    }

    pub fn create_run(
        &self,
        flow_id: i64,
        name: &str,
        parameters: &str,
        tags: &str,
    ) -> Result<(i64, Id)> {
        self.create_run_full(writer::CreateRun {
            flow_id,
            name: name.into(),
            parameters: parameters.into(),
            tags: tags.into(),
            created_by: "script".into(),
            ..Default::default()
        })
    }

    pub fn create_run_full(&self, cmd: writer::CreateRun) -> Result<(i64, Id)> {
        self.write(|reply| WriteCommand::CreateRun(cmd, reply))
    }

    /// Create many runs in one transaction.
    pub fn create_runs_bulk(&self, cmds: Vec<writer::CreateRun>) -> Result<Vec<(i64, Id)>> {
        self.write(|reply| WriteCommand::CreateRunsBulk(cmds, reply))
    }

    /// Replace the outcomes of a firing recorded by `record_firing`.
    pub fn update_firing(&self, id: i64, outcomes: &str) -> Result<()> {
        self.write(|reply| WriteCommand::UpdateFiring {
            id,
            outcomes: outcomes.to_string(),
            reply,
        })
    }

    /// Merge a JSON object into a run's searchable attributes; false when the run is unknown.
    pub fn merge_run_attributes(&self, run_id: i64, patch: &str) -> Result<bool> {
        self.write(|reply| WriteCommand::MergeRunAttributes {
            run_id,
            patch: patch.to_string(),
            reply,
        })
    }

    pub fn set_run_priority(&self, run_id: i64, priority: i64) -> Result<()> {
        self.write(|reply| WriteCommand::SetRunPriority {
            run_id,
            priority,
            reply,
        })
    }

    pub fn upsert_schedule(&self, sw: writer::ScheduleWrite) -> Result<i64> {
        self.write(|reply| WriteCommand::UpsertSchedule(sw, reply))
    }

    pub fn patch_schedule(&self, schedule_id: i64, patch: writer::SchedulePatch) -> Result<bool> {
        self.write(|reply| WriteCommand::PatchSchedule {
            schedule_id,
            patch,
            reply,
        })
    }

    pub fn delete_schedule(&self, schedule_id: i64) -> Result<bool> {
        self.write(|reply| WriteCommand::DeleteSchedule { schedule_id, reply })
    }

    pub fn delete_unstarted_runs(&self, schedule_id: i64) -> Result<Vec<i64>> {
        self.write(|reply| WriteCommand::DeleteUnstartedRuns { schedule_id, reply })
    }

    /// Record skipped fires of a schedule; returns how many were new.
    pub fn add_skips(&self, schedule_id: i64, fires: Vec<i64>, created_by: &str) -> Result<usize> {
        self.write(|reply| WriteCommand::AddSkips {
            schedule_id,
            fires,
            created_by: created_by.into(),
            reply,
        })
    }

    /// Forget skipped fires of a schedule; returns how many existed.
    pub fn delete_skips(&self, schedule_id: i64, fires: Vec<i64>) -> Result<usize> {
        self.write(|reply| WriteCommand::DeleteSkips {
            schedule_id,
            fires,
            reply,
        })
    }

    pub fn delete_skips_before(&self, schedule_id: i64, before: i64) -> Result<usize> {
        self.write(|reply| WriteCommand::DeleteSkipsBefore {
            schedule_id,
            before,
            reply,
        })
    }

    /// Mark the unstarted runs of skipped fires and unmark the others.
    pub fn sync_skip_marks(&self, schedule_id: i64) -> Result<Vec<i64>> {
        self.write(|reply| WriteCommand::SyncSkipMarks { schedule_id, reply })
    }

    pub fn create_backfill(&self, b: writer::CreateBackfill) -> Result<(i64, Id)> {
        self.write(|reply| WriteCommand::CreateBackfill(b, reply))
    }

    pub fn set_backfill_cancelled(&self, backfill_id: i64) -> Result<()> {
        self.write(|reply| WriteCommand::SetBackfillCancelled { backfill_id, reply })
    }

    /// Forget checkpoint references of terminal runs that ended before `before`
    /// (microseconds); the files are the caller's to remove. Returns the rows cleared.
    pub fn clear_checkpoints_before(&self, before: i64) -> Result<usize> {
        self.write(|reply| WriteCommand::ClearCheckpointsBefore { before, reply })
    }

    /// Set one task state entry; false when the run does not exist.
    pub fn task_state_set(&self, run_id: i64, scope: &str, key: &str, value: &str) -> Result<bool> {
        self.write(|reply| WriteCommand::TaskStateSet {
            run_id,
            scope: scope.into(),
            key: key.into(),
            value: value.into(),
            reply,
        })
    }

    pub fn task_state_delete(&self, run_id: i64, scope: &str, key: &str) -> Result<bool> {
        self.write(|reply| WriteCommand::TaskStateDelete {
            run_id,
            scope: scope.into(),
            key: key.into(),
            reply,
        })
    }

    pub fn kv_delete(&self, key: &str) -> Result<bool> {
        self.write(|reply| WriteCommand::KvDelete {
            key: key.into(),
            reply,
        })
    }

    pub fn kv_set(&self, key: &str, value: &str) -> Result<()> {
        self.write(|reply| WriteCommand::KvSet {
            key: key.into(),
            value: value.into(),
            reply,
        })
    }

    /// Insert a pending message for a run.
    pub fn run_message_insert(&self, run_id: i64, topic: &str, payload: &str) -> Result<()> {
        self.write(|reply| WriteCommand::RunMessageInsert {
            run_id,
            topic: topic.into(),
            payload: payload.into(),
            reply,
        })
    }

    /// Claim the oldest pending message for a run and topic, storing the
    /// answer in the run's input KV so replay finds it at the same ordinal.
    /// Returns the payload string when a message was claimed, None otherwise.
    pub fn run_message_claim(&self, run_id: i64, topic: &str, index: i64) -> Result<Option<String>> {
        self.write(|reply| WriteCommand::RunMessageClaim {
            run_id,
            topic: topic.into(),
            index,
            reply,
        })
    }

    /// List unconsumed messages for a run.
    pub fn run_message_list(&self, run_id: i64) -> Result<Vec<(String, String)>> {
        self.write(|reply| WriteCommand::RunMessageList {
            run_id,
            reply,
        })
    }

    pub fn append_event(&self, event: writer::NewEvent) -> Result<(Event, Id)> {
        self.write(|reply| WriteCommand::AppendEvent(event, reply))
    }

    /// Append many events in one writer round trip.
    pub fn append_events(&self, events: Vec<writer::NewEvent>) -> Result<Vec<(Event, Id)>> {
        if events.is_empty() {
            return Ok(Vec::new());
        }
        self.write(|reply| WriteCommand::AppendEvents(events, reply))
    }

    pub fn arm_expectation(&self, a: writer::ArmExpectation) -> Result<i64> {
        self.write(|reply| WriteCommand::ArmExpectation(a, reply))
    }

    /// Met expectations: open ones of the rule and key whose deadline is at
    /// or after the event time `at`.
    pub fn disarm_expectations(&self, rule_id: i64, key: &str, at: i64) -> Result<Vec<i64>> {
        self.write(|reply| WriteCommand::DisarmExpectations {
            rule_id,
            key: key.into(),
            at,
            reply,
        })
    }

    pub fn settle_expectation(&self, id: i64, status: &str) -> Result<bool> {
        self.write(|reply| WriteCommand::SettleExpectation {
            id,
            status: status.into(),
            reply,
        })
    }

    pub fn cancel_rule_expectations(&self, rule_id: i64) -> Result<usize> {
        self.write(|reply| WriteCommand::CancelRuleExpectations { rule_id, reply })
    }

    pub fn upsert_rule(&self, rw: writer::RuleWrite) -> Result<i64> {
        self.write(|reply| WriteCommand::UpsertRule(rw, reply))
    }

    pub fn delete_rule(&self, rule_id: i64) -> Result<bool> {
        self.write(|reply| WriteCommand::DeleteRule { rule_id, reply })
    }

    pub fn prune_code_rules(&self, keep: Vec<i64>) -> Result<usize> {
        self.write(|reply| WriteCommand::PruneCodeRules { keep, reply })
    }

    pub fn record_firing(
        &self,
        rule_id: i64,
        event_id: Option<i64>,
        run_id: Option<i64>,
        outcomes: &str,
    ) -> Result<i64> {
        self.write(|reply| WriteCommand::RecordFiring {
            rule_id,
            event_id,
            run_id,
            outcomes: outcomes.into(),
            reply,
        })
    }

    pub fn upsert_artifact(&self, a: writer::UpsertArtifact) -> Result<i64> {
        self.write(|reply| WriteCommand::UpsertArtifact(a, reply))
    }

    pub fn set_variable(&self, name: &str, value: &str, tags: &str, secret: bool) -> Result<()> {
        self.write(|reply| WriteCommand::SetVariable {
            name: name.into(),
            value: value.into(),
            tags: tags.into(),
            secret,
            reply,
        })
    }

    pub fn delete_variable(&self, name: &str) -> Result<bool> {
        self.write(|reply| WriteCommand::DeleteVariable {
            name: name.into(),
            reply,
        })
    }

    pub fn delete_expired(&self, table: &str, before: i64, limit: i64) -> Result<usize> {
        self.write(|reply| WriteCommand::DeleteExpired {
            table: table.into(),
            before,
            limit,
            reply,
        })
    }

    /// Delete up to `limit` expired terminal runs; see `WriteCommand::DeleteExpiredRuns`.
    pub fn delete_expired_runs(
        &self,
        before: i64,
        failed_before: i64,
        keep_per_flow: i64,
        limit: i64,
    ) -> Result<Vec<(i64, i64, String)>> {
        self.write(|reply| WriteCommand::DeleteExpiredRuns {
            before,
            failed_before,
            keep_per_flow,
            limit,
            reply,
        })
    }

    pub fn incremental_vacuum(&self, pages: i64) -> Result<()> {
        self.write(|reply| WriteCommand::IncrementalVacuum { pages, reply })
    }

    /// Database and WAL file sizes in bytes.
    pub fn file_sizes(&self) -> (u64, u64) {
        let db = std::fs::metadata(self.home.join(DB_FILE))
            .map(|m| m.len())
            .unwrap_or(0);
        let wal = std::fs::metadata(self.home.join(format!("{DB_FILE}-wal")))
            .map(|m| m.len())
            .unwrap_or(0);
        (db, wal)
    }

    pub fn transition_run(&self, run_id: i64, state: State, force: bool) -> Result<State> {
        self.write(|reply| WriteCommand::TransitionRun {
            run_id,
            state,
            force,
            reply,
        })
    }

    /// Transition many runs to the same state in one transaction. Returns the ids accepted.
    pub fn transition_many(&self, run_ids: Vec<i64>, state: State) -> Result<Vec<i64>> {
        self.write(|reply| WriteCommand::TransitionMany {
            run_ids,
            state,
            reply,
        })
    }

    /// Move a Scheduled run's start and replace its parameters; false when it
    /// is not Scheduled any more.
    pub fn reschedule_run(
        &self,
        run_id: i64,
        scheduled_time: i64,
        parameters: &str,
    ) -> Result<bool> {
        self.write(|reply| WriteCommand::RescheduleRun {
            run_id,
            scheduled_time,
            parameters: parameters.to_string(),
            reply,
        })
    }

    pub fn set_run_engine(
        &self,
        run_id: i64,
        engine_pid: Option<i64>,
        engine_id: Option<String>,
    ) -> Result<()> {
        self.write(|reply| WriteCommand::SetRunEngine {
            run_id,
            engine_pid,
            engine_id,
            reply,
        })
    }

    pub fn create_task_run(
        &self,
        run_id: i64,
        name: &str,
        task_key: &str,
        dynamic_key: &str,
        pass: i64,
    ) -> Result<(i64, Id)> {
        self.create_task_run_full(writer::CreateTaskRun {
            run_id,
            name: name.into(),
            task_key: task_key.into(),
            dynamic_key: dynamic_key.into(),
            external_id: None,
            parents: Vec::new(),
            pass,
        })
    }

    pub fn create_task_run_full(&self, cmd: writer::CreateTaskRun) -> Result<(i64, Id)> {
        self.write(|reply| WriteCommand::CreateTaskRun(cmd, reply))
    }

    pub fn transition_task_run(
        &self,
        task_run_id: i64,
        state: State,
        force: bool,
    ) -> Result<State> {
        self.write(|reply| WriteCommand::TransitionTaskRun {
            task_run_id,
            state,
            force,
            reply,
        })
    }

    pub fn append_logs(&self, logs: Vec<NewLog>) -> Result<usize> {
        self.write(|reply| WriteCommand::AppendLogs(logs, reply))
    }

    /// Apply an engine report batch idempotently.
    pub fn apply_report(&self, run_id: i64, events: Vec<ReportEvent>) -> Result<ReportOutcome> {
        self.write(|reply| WriteCommand::ApplyReport {
            run_id,
            events,
            reply,
        })
    }

    pub fn delete_run(&self, run_id: i64) -> Result<bool> {
        self.write(|reply| WriteCommand::DeleteRun { run_id, reply })
    }

    /// Flush: wait until everything queued before this call has committed.
    pub fn flush(&self) -> Result<()> {
        self.write(WriteCommand::Flush)
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        // Ask the writer to stop, then wait for it so the WAL is checkpointed
        // before the lock file is released.
        let _ = self.writer.send(WriteCommand::Shutdown);
        if let Some(handle) = self
            .writer_thread
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod reader_pool_tests {
    use super::*;
    use std::sync::Arc;

    fn store() -> (tempfile::TempDir, Arc<Store>) {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        (dir, Arc::new(s))
    }

    /// Take every pooled connection out, so the next read has to wait or open.
    fn drain(store: &Store) -> Vec<Connection> {
        let mut pool = store.readers.lock().unwrap_or_else(|e| e.into_inner());
        (0..pool.len()).filter_map(|_| pool.pop()).collect()
    }

    fn pooled(store: &Store) -> usize {
        store
            .readers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    fn seed(store: &Store) -> i64 {
        let f = store
            .upsert_flow("p", "etl", "m", "/tmp", None, "[]", "{}")
            .unwrap();
        store
            .write(|reply| {
                crate::writer::WriteCommand::CreateRun(
                    crate::writer::CreateRun {
                        flow_id: f,
                        name: "r1".into(),
                        parameters: "{}".into(),
                        tags: "[]".into(),
                        created_by: "test".into(),
                        ..Default::default()
                    },
                    reply,
                )
            })
            .map(|(id, _)| id)
            .unwrap()
    }

    /// The point of waiting: reads far above the pool size must not each open a
    /// connection, and the pool must be whole afterwards.
    #[test]
    fn reads_above_the_pool_size_leave_the_pool_intact() {
        let (_d, store) = store();
        let run = seed(&store);
        let threads: Vec<_> = (0..64)
            .map(|_| {
                let store = Arc::clone(&store);
                std::thread::spawn(move || {
                    for _ in 0..5 {
                        assert_eq!(store.get_run(run).unwrap().unwrap().id, run);
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().expect("reader thread");
        }
        assert_eq!(pooled(&store), READ_POOL_SIZE, "connections were lost");
    }

    /// A read that finds the pool empty waits: it cannot finish while every
    /// connection is held elsewhere.
    #[test]
    fn a_read_waits_for_a_connection_rather_than_proceeding() {
        let (_d, store) = store();
        let run = seed(&store);
        let mut held = drain(&store);
        assert_eq!(held.len(), READ_POOL_SIZE);

        let reader = {
            let store = Arc::clone(&store);
            std::thread::spawn(move || store.get_run(run).unwrap().map(|r| r.id))
        };
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(!reader.is_finished(), "the read did not wait");

        // Hand one back and signal it.
        {
            let mut pool = store.readers.lock().unwrap_or_else(|e| e.into_inner());
            pool.push(held.pop().unwrap());
        }
        store.reader_returned.notify_one();
        assert_eq!(reader.join().expect("reader thread"), Some(run));
        drop(held);
    }

    /// The signal has to be on the panic path too, or the next reader waits out
    /// the whole timeout for a connection that is already free.
    ///
    /// One connection is left in the pool so the panicking read takes a *pooled*
    /// one; if the panic path failed to return it, the pool would be left empty.
    #[test]
    fn a_panicking_read_returns_its_connection() {
        let (_d, store) = store();
        let mut held = drain(&store);
        {
            let mut pool = store.readers.lock().unwrap_or_else(|e| e.into_inner());
            pool.push(held.pop().unwrap());
        }
        assert_eq!(pooled(&store), 1);

        let panicking = {
            let store = Arc::clone(&store);
            std::thread::spawn(move || {
                store.with_reader(|_| -> Result<()> {
                    panic!("boom")
                });
            })
        };
        assert!(panicking.join().is_err(), "the read was supposed to panic");
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert_eq!(
            pooled(&store),
            1,
            "a panicking read did not return its pooled connection"
        );
        drop(held);
    }

    /// A read must not block forever: with the pool drained and nothing to
    /// return a connection, it proceeds on one of its own.
    #[test]
    fn the_wait_is_bounded() {
        let (_d, store) = store();
        let run = seed(&store);
        let _held = drain(&store);
        let started = std::time::Instant::now();
        let got = store.get_run(run).unwrap().map(|r| r.id);
        let elapsed = started.elapsed();
        assert_eq!(got, Some(run), "the read did not produce its result");
        assert!(
            elapsed >= READER_WAIT,
            "returned after {elapsed:?}, before the wait could expire"
        );
        assert!(
            elapsed < READER_WAIT * 4,
            "waited {elapsed:?}, far beyond the bound"
        );
    }

    /// Results must not depend on whether a read waited or opened.
    #[test]
    fn results_are_the_same_whether_waiting_or_opening() {
        let (_d, store) = store();
        let run = seed(&store);
        store.create_task_run(run, "orders", "t", "orders", 0).unwrap();
        let expected = store.get_run(run).unwrap().expect("row");

        let _held = drain(&store);
        let threads: Vec<_> = (0..24)
            .map(|_| {
                let store = Arc::clone(&store);
                std::thread::spawn(move || store.get_run(run).unwrap())
            })
            .collect();
        for t in threads {
            assert_eq!(t.join().expect("reader thread"), Some(expected.clone()));
        }
    }
}
