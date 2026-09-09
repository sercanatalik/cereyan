//! SQLite store: one writer thread with group commit, a read pool, an OS
//! advisory lock, embedded migrations, and corruption quarantine.

mod error;
mod home;
mod migrations;
mod open;
mod read;
mod row;
pub mod secrets;
mod writer;

pub use error::StoreError;
pub use home::resolve_home;
pub use migrations::latest_version as latest_schema_version;
pub use read::{ArtifactFilter, ArtifactsPage, EventFilter, EventsPage};
pub use read::{ListRunsFilter, ListTaskRunsFilter, LogFilter, LogsPage, RunsPage, TaskRunsPage};
pub use writer::{
    ArmExpectation, CreateBackfill, CreateRun, CreateTaskRun, NewEvent, NewLog, ReportEvent,
    ReportOutcome, RuleWrite, SchedulePatch, ScheduleWrite, UpsertArtifact, UpsertFlow,
    WriteCommand,
};

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use cereyan_core::{Id, State};
use crossbeam_channel::{bounded, Receiver, Sender};
use rusqlite::Connection;

pub type Result<T> = std::result::Result<T, StoreError>;

/// Lock-file name inside the home directory.
pub const LOCK_FILE: &str = "db.lock";
/// Database file name inside the home directory.
pub const DB_FILE: &str = "db.sqlite";

const READ_POOL_SIZE: usize = 4;

pub struct Store {
    home: PathBuf,
    writer: Sender<WriteCommand>,
    readers: Mutex<Vec<Connection>>,
    _lock: std::fs::File,
    writer_thread: Mutex<Option<std::thread::JoinHandle<()>>>,
    commits: Arc<AtomicU64>,
}

impl Store {
    /// Open the store at `home`, taking the advisory lock and applying
    /// migrations. The directory is created when missing.
    pub fn open(home: &Path) -> Result<Store> {
        open::ensure_home(home)?;
        let lock = open::take_lock(home)?;
        let db_path = home.join(DB_FILE);
        let write_conn = open::open_writer(&db_path)?;
        migrations::apply(&write_conn)?;

        let mut readers = Vec::with_capacity(READ_POOL_SIZE);
        for _ in 0..READ_POOL_SIZE {
            readers.push(open::open_reader(&db_path)?);
        }

        let (tx, rx) = bounded::<WriteCommand>(16_384);
        let commits = Arc::new(AtomicU64::new(0));
        let counter = commits.clone();
        let handle = std::thread::Builder::new()
            .name("cereyan-writer".into())
            .spawn(move || writer::run(write_conn, rx, counter))?;

        Ok(Store {
            home: home.to_path_buf(),
            writer: tx,
            readers: Mutex::new(readers),
            _lock: lock,
            writer_thread: Mutex::new(Some(handle)),
            commits,
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

    /// Borrow a read-only connection from the pool.
    pub fn with_reader<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let conn = {
            let mut pool = self.readers.lock().unwrap_or_else(|e| e.into_inner());
            match pool.pop() {
                Some(c) => c,
                None => open::open_reader(&self.home.join(DB_FILE))?,
            }
        };
        let out = f(&conn);
        let mut pool = self.readers.lock().unwrap_or_else(|e| e.into_inner());
        if pool.len() < READ_POOL_SIZE {
            pool.push(conn);
        }
        out
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

    pub fn create_backfill(&self, b: writer::CreateBackfill) -> Result<(i64, Id)> {
        self.write(|reply| WriteCommand::CreateBackfill(b, reply))
    }

    pub fn set_backfill_cancelled(&self, backfill_id: i64) -> Result<()> {
        self.write(|reply| WriteCommand::SetBackfillCancelled { backfill_id, reply })
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

    pub fn append_event(&self, event: writer::NewEvent) -> Result<(i64, Id)> {
        self.write(|reply| WriteCommand::AppendEvent(event, reply))
    }

    /// Append many events in one writer round trip.
    pub fn append_events(&self, events: Vec<writer::NewEvent>) -> Result<Vec<(i64, Id)>> {
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
    ) -> Result<(i64, Id)> {
        self.create_task_run_full(writer::CreateTaskRun {
            run_id,
            name: name.into(),
            task_key: task_key.into(),
            dynamic_key: dynamic_key.into(),
            external_id: None,
            parents: Vec::new(),
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
