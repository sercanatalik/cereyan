//! The single writer thread. Every write is a `WriteCommand` carrying a reply
//! channel; commands are executed in batches inside one `BEGIN IMMEDIATE`
//! transaction and acknowledged only after the commit.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use cereyan_core::{new_id, now_micros, propose, Id, Proposal, RunPolicy, State, TaskRun};
use crossbeam_channel::{Receiver, Sender};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::StoreError;
use crate::row::{state_from_columns, task_run_from_row, TASK_RUN_COLUMNS, TASK_RUN_FROM};
use crate::Result;

pub type Reply<T> = Sender<Result<T>>;

const GROUP_COMMIT_WINDOW: Duration = Duration::from_millis(5);
const MAX_BATCH: usize = 8_192;

#[derive(Debug, Clone, Default)]
pub struct UpsertFlow {
    pub project: String,
    pub name: String,
    pub module: String,
    pub source_dir: String,
    pub description: Option<String>,
    pub tags: String,
    pub parameter_schema: String,
    pub options: String,
    /// The group declared in Python; `None` writes NULL, which reads as the project.
    pub group: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct CreateRun {
    pub flow_id: i64,
    pub name: String,
    pub parameters: String,
    pub tags: String,
    pub created_by: String,
    pub initial_state: Option<State>,
    pub schedule_id: Option<i64>,
    pub scheduled_time: Option<i64>,
    pub priority: i64,
    pub parent_run_id: Option<i64>,
    pub attempt: i64,
    pub backfill_id: Option<i64>,
}

#[derive(Debug, Clone, Default)]
pub struct ScheduleWrite {
    pub id: Option<i64>,
    pub flow_id: i64,
    pub spec: String,
    pub catchup: String,
    pub catchup_max: i64,
    pub active: bool,
    pub source: String,
    pub code_key: Option<String>,
    pub persist: bool,
}

#[derive(Debug, Clone, Default)]
pub struct SchedulePatch {
    pub spec: Option<String>,
    pub catchup: Option<String>,
    pub catchup_max: Option<i64>,
    pub active: Option<bool>,
    pub paused_reason: Option<Option<String>>,
    pub paused_until: Option<Option<i64>>,
    pub persist: Option<bool>,
}

#[derive(Debug, Clone, Default)]
pub struct CreateBackfill {
    pub flow_id: i64,
    pub parameter: String,
    pub start_value: String,
    pub end_value: String,
    pub interval_secs: f64,
    pub concurrency: i64,
    pub total: i64,
    pub extra_parameters: String,
}

#[derive(Debug, Clone, Default)]
pub struct NewEvent {
    pub name: String,
    pub run_id: Option<i64>,
    pub flow_id: Option<i64>,
    pub payload: Value,
    pub resource: cereyan_core::Resource,
    pub related: Vec<cereyan_core::Resource>,
}

#[derive(Debug, Clone, Default)]
pub struct UpsertArtifact {
    pub run_id: i64,
    pub task_run_id: Option<i64>,
    pub kind: String,
    pub key: Option<String>,
    pub data: String,
    pub external_id: Option<Id>,
}

/// Input for arming an expectation.
#[derive(Clone, Debug)]
pub struct ArmExpectation {
    pub rule_id: i64,
    pub key: String,
    pub run_id: Option<i64>,
    pub flow_id: Option<i64>,
    pub armed_at: i64,
    pub deadline: i64,
}

#[derive(Debug, Clone, Default)]
pub struct RuleWrite {
    pub id: Option<i64>,
    pub name: String,
    pub enabled: bool,
    pub source: String,
    pub module: Option<String>,
    pub spec: String,
}

/// What a database reset deletes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetScope {
    /// Runs and everything they recorded; definitions stay.
    History,
    /// History, stale flows, UI-made schedules and rules, and variables.
    Everything,
}

/// Which rows of a set of flows a batched delete removes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowRows {
    Log,
    Event,
}

/// Rows deleted by a project removal or a reset.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct DeletedCounts {
    pub flows: i64,
    pub runs: i64,
    pub task_runs: i64,
    pub logs: i64,
    pub events: i64,
    pub artifacts: i64,
    pub schedules: i64,
    pub backfills: i64,
    pub rules: i64,
    pub variables: i64,
}

#[derive(Debug, Clone, Default)]
pub struct CreateTaskRun {
    pub run_id: i64,
    pub name: String,
    pub task_key: String,
    pub dynamic_key: String,
    pub external_id: Option<Id>,
    pub parents: Vec<Id>,
    /// Which execution of the run's body this task run belongs to.
    pub pass: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewLog {
    pub run_id: i64,
    #[serde(default)]
    pub task_run_id: Option<i64>,
    #[serde(default)]
    pub task_run_external_id: Option<Id>,
    pub level: i32,
    pub logger: String,
    pub timestamp: i64,
    pub message: String,
}

/// One event of a batched engine report. Events are ordered per run and
/// carry a sequence number so redelivery is harmless.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReportEvent {
    TaskRunCreated {
        seq: i64,
        external_id: Id,
        name: String,
        task_key: String,
        dynamic_key: String,
        #[serde(default)]
        parents: Vec<Id>,
        /// Absent in reports written before passes existed, which behaved as
        /// though every task run belonged to the first execution.
        #[serde(default)]
        pass: i64,
    },
    TaskRunTransition {
        seq: i64,
        external_id: Id,
        state: State,
        #[serde(default)]
        force: bool,
    },
    Logs {
        seq: i64,
        logs: Vec<NewLog>,
    },
    /// A custom event or an artifact reported by an engine.
    Custom {
        seq: i64,
        name: String,
        #[serde(default)]
        payload: Value,
        #[serde(default)]
        task_run_external_id: Option<Id>,
    },
    Artifact {
        seq: i64,
        external_id: Id,
        #[serde(default)]
        task_run_external_id: Option<Id>,
        artifact_kind: String,
        #[serde(default)]
        key: Option<String>,
        data: Value,
    },
}

impl ReportEvent {
    pub fn seq(&self) -> i64 {
        match self {
            ReportEvent::TaskRunCreated { seq, .. }
            | ReportEvent::TaskRunTransition { seq, .. }
            | ReportEvent::Logs { seq, .. }
            | ReportEvent::Custom { seq, .. }
            | ReportEvent::Artifact { seq, .. } => *seq,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReportOutcome {
    pub applied: usize,
    pub skipped: usize,
    pub task_runs: Vec<TaskRun>,
    /// (previous state type, new state type) for every task run the batch touched.
    pub task_run_transitions: Vec<(Option<String>, String)>,
    pub log_count: usize,
    pub last_log_id: Option<i64>,
    pub last_seq: i64,
    /// Every accepted task-run transition in this batch: (external id, state).
    pub task_run_states: Vec<(Id, State)>,
    /// Ids of custom events inserted by this batch.
    pub event_ids: Vec<i64>,
    /// Artifacts inserted or updated by this batch.
    pub artifact_ids: Vec<i64>,
    /// Events the store refused while the rest of the batch applied. The server
    /// records one `run.report_rejected` event for each, because a report that
    /// vanished silently is how a whole execution of a run once went missing
    /// from its history.
    pub rejected: Vec<RejectedEvent>,
}

/// One event of a report the store would not apply.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RejectedEvent {
    pub seq: i64,
    pub kind: String,
    pub reason: String,
}

pub enum WriteCommand {
    UpsertFlow(UpsertFlow, Reply<i64>),
    SetFlowError {
        flow_id: i64,
        error: Option<String>,
        reply: Reply<()>,
    },
    DeleteFlow {
        flow_id: i64,
        reply: Reply<bool>,
    },
    CreateRun(CreateRun, Reply<(i64, Id)>),
    CreateRunsBulk(Vec<CreateRun>, Reply<Vec<(i64, Id)>>),
    SetRunPriority {
        run_id: i64,
        priority: i64,
        reply: Reply<()>,
    },
    UpsertSchedule(ScheduleWrite, Reply<i64>),
    PatchSchedule {
        schedule_id: i64,
        patch: SchedulePatch,
        reply: Reply<bool>,
    },
    DeleteSchedule {
        schedule_id: i64,
        reply: Reply<bool>,
    },
    /// Record skipped fires of a schedule; existing ones are left as they are.
    AddSkips {
        schedule_id: i64,
        fires: Vec<i64>,
        created_by: String,
        reply: Reply<usize>,
    },
    DeleteSkips {
        schedule_id: i64,
        fires: Vec<i64>,
        reply: Reply<usize>,
    },
    /// Forget skips whose fire time is at or before `before`.
    DeleteSkipsBefore {
        schedule_id: i64,
        before: i64,
        reply: Reply<usize>,
    },
    /// Mark unstarted runs of skipped fires and unmark the rest; returns the runs changed.
    SyncSkipMarks {
        schedule_id: i64,
        reply: Reply<Vec<i64>>,
    },
    /// Delete Scheduled runs of a schedule that were never picked up; returns their ids.
    DeleteUnstartedRuns {
        schedule_id: i64,
        reply: Reply<Vec<i64>>,
    },
    CreateBackfill(CreateBackfill, Reply<(i64, Id)>),
    SetBackfillCancelled {
        backfill_id: i64,
        reply: Reply<()>,
    },
    KvSet {
        key: String,
        value: String,
        reply: Reply<()>,
    },
    KvDelete {
        key: String,
        reply: Reply<bool>,
    },
    AppendEvent(NewEvent, Reply<(i64, Id)>),
    /// Many events in one transaction (task-run events from a report).
    AppendEvents(Vec<NewEvent>, Reply<Vec<(i64, Id)>>),
    UpsertRule(RuleWrite, Reply<i64>),
    /// Arm an expectation; returns the existing open one for the same key.
    ArmExpectation(ArmExpectation, Reply<i64>),
    /// Mark open expectations of a rule and key whose deadline is not
    /// before `at` as met; returns their ids.
    DisarmExpectations {
        rule_id: i64,
        key: String,
        at: i64,
        reply: Reply<Vec<i64>>,
    },
    /// Move an open expectation to `lapsed` or `cancelled`.
    SettleExpectation {
        id: i64,
        status: String,
        reply: Reply<bool>,
    },
    /// Cancel every open expectation of a rule.
    CancelRuleExpectations {
        rule_id: i64,
        reply: Reply<usize>,
    },
    DeleteRule {
        rule_id: i64,
        reply: Reply<bool>,
    },
    /// Delete code rules not in the keep list (re-registration on start).
    PruneCodeRules {
        keep: Vec<i64>,
        reply: Reply<usize>,
    },
    RecordFiring {
        rule_id: i64,
        event_id: Option<i64>,
        run_id: Option<i64>,
        outcomes: String,
        reply: Reply<i64>,
    },
    UpsertArtifact(UpsertArtifact, Reply<i64>),
    SetVariable {
        name: String,
        value: String,
        tags: String,
        secret: bool,
        reply: Reply<()>,
    },
    DeleteVariable {
        name: String,
        reply: Reply<bool>,
    },
    /// Delete up to `limit` rows of `table` older than `before`; returns the count.
    DeleteExpired {
        table: String,
        before: i64,
        limit: i64,
        reply: Reply<usize>,
    },
    /// Delete up to `limit` terminal runs older than their cutoff (`failed_before`
    /// for Failed and Crashed, `before` otherwise), sparing runs of live backfills
    /// and each flow's newest `keep_per_flow`; returns (id, flow id, state type).
    DeleteExpiredRuns {
        before: i64,
        failed_before: i64,
        keep_per_flow: i64,
        limit: i64,
        reply: Reply<Vec<(i64, i64, String)>>,
    },
    IncrementalVacuum {
        pages: i64,
        reply: Reply<()>,
    },
    TransitionRun {
        run_id: i64,
        state: State,
        force: bool,
        reply: Reply<State>,
    },
    /// Apply one state to many runs in one transaction; rejections are skipped.
    TransitionMany {
        run_ids: Vec<i64>,
        state: State,
        reply: Reply<Vec<i64>>,
    },
    SetRunEngine {
        run_id: i64,
        engine_pid: Option<i64>,
        engine_id: Option<String>,
        reply: Reply<()>,
    },
    CreateTaskRun(CreateTaskRun, Reply<(i64, Id)>),
    TransitionTaskRun {
        task_run_id: i64,
        state: State,
        force: bool,
        reply: Reply<State>,
    },
    AppendLogs(Vec<NewLog>, Reply<usize>),
    ApplyReport {
        run_id: i64,
        events: Vec<ReportEvent>,
        reply: Reply<ReportOutcome>,
    },
    DeleteRun {
        run_id: i64,
        reply: Reply<bool>,
    },
    /// Delete up to `limit` log or event rows of the given flows; returns the count.
    DeleteFlowRows {
        rows: FlowRows,
        flow_ids: Vec<i64>,
        limit: i64,
        reply: Reply<usize>,
    },
    /// Delete the given flows with everything that names them.
    DeleteFlows {
        flow_ids: Vec<i64>,
        reply: Reply<DeletedCounts>,
    },
    /// Write a consistent copy of the database to `path`; runs outside any transaction.
    BackupTo {
        path: String,
        reply: Reply<()>,
    },
    /// Delete the history, or everything but what the live flows registered from code.
    Reset {
        scope: ResetScope,
        live_flows: Vec<i64>,
        reply: Reply<DeletedCounts>,
    },
    /// Rebuild the file to its smallest size; runs outside any transaction.
    Vacuum(Reply<()>),
    Flush(Reply<()>),
    Shutdown,
}

impl WriteCommand {
    /// `VACUUM` in either form cannot run inside a transaction.
    fn outside_transaction(&self) -> bool {
        matches!(
            self,
            WriteCommand::BackupTo { .. } | WriteCommand::Vacuum(_)
        )
    }
}

/// Deferred acknowledgement: the closure is run after the commit.
type Ack = Box<dyn FnOnce() + Send>;

pub fn run(conn: Connection, rx: Receiver<WriteCommand>, commits: Arc<AtomicU64>) {
    let mut last_commit = Instant::now() - GROUP_COMMIT_WINDOW;
    let mut last_was_batch = false;
    loop {
        let first = match rx.recv() {
            Ok(c) => c,
            Err(_) => break,
        };
        let mut batch = vec![first];
        while batch.len() < MAX_BATCH {
            match rx.try_recv() {
                Ok(c) => batch.push(c),
                Err(_) => break,
            }
        }
        // Under load, coalesce into at most one commit per window.
        if batch.len() > 1 || last_was_batch {
            let deadline = last_commit + GROUP_COMMIT_WINDOW;
            while batch.len() < MAX_BATCH && Instant::now() < deadline {
                match rx.recv_deadline(deadline) {
                    Ok(c) => batch.push(c),
                    Err(_) => break,
                }
            }
        }
        last_was_batch = batch.len() > 1;

        let mut acks: Vec<Ack> = Vec::with_capacity(batch.len());
        let mut shutdown = false;
        let mut in_tx = conn.execute_batch("BEGIN IMMEDIATE").is_ok();
        for cmd in batch {
            if let WriteCommand::Shutdown = cmd {
                shutdown = true;
                continue;
            }
            if cmd.outside_transaction() {
                if in_tx {
                    commit(&conn, &commits);
                }
                acks.push(execute(&conn, cmd));
                in_tx = conn.execute_batch("BEGIN IMMEDIATE").is_ok();
                continue;
            }
            acks.push(execute(&conn, cmd));
        }
        if in_tx {
            commit(&conn, &commits);
        }
        last_commit = Instant::now();
        for ack in acks {
            ack();
        }
        if shutdown {
            break;
        }
    }
    let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)");
}

fn commit(conn: &Connection, commits: &AtomicU64) {
    match conn.execute_batch("COMMIT") {
        Ok(()) => {
            commits.fetch_add(1, Ordering::Relaxed);
        }
        Err(e) => {
            eprintln!("cereyan writer: commit failed: {e}");
            let _ = conn.execute_batch("ROLLBACK");
        }
    }
}

fn ack<T: Send + 'static>(reply: Reply<T>, value: Result<T>) -> Ack {
    Box::new(move || {
        let _ = reply.send(value);
    })
}

fn execute(conn: &Connection, cmd: WriteCommand) -> Ack {
    match cmd {
        WriteCommand::UpsertFlow(f, reply) => ack(reply, upsert_flow(conn, &f)),
        WriteCommand::SetFlowError {
            flow_id,
            error,
            reply,
        } => ack(
            reply,
            conn.execute(
                "UPDATE flow SET error = ?1 WHERE id = ?2",
                params![error, flow_id],
            )
            .map(|_| ())
            .map_err(Into::into),
        ),
        WriteCommand::DeleteFlow { flow_id, reply } => ack(
            reply,
            conn.execute("DELETE FROM flow WHERE id = ?1", params![flow_id])
                .map(|n| n > 0)
                .map_err(Into::into),
        ),
        WriteCommand::CreateRun(r, reply) => ack(reply, create_run(conn, &r)),
        WriteCommand::CreateRunsBulk(rs, reply) => ack(reply, {
            let mut out = Vec::with_capacity(rs.len());
            let mut err = None;
            for r in &rs {
                match create_run(conn, r) {
                    Ok(v) => out.push(v),
                    Err(e) => {
                        err = Some(e);
                        break;
                    }
                }
            }
            match err {
                Some(e) => Err(e),
                None => Ok(out),
            }
        }),
        WriteCommand::SetRunPriority {
            run_id,
            priority,
            reply,
        } => ack(
            reply,
            conn.execute(
                "UPDATE run SET priority = ?1 WHERE id = ?2",
                params![priority, run_id],
            )
            .map(|_| ())
            .map_err(Into::into),
        ),
        WriteCommand::UpsertSchedule(sw, reply) => ack(reply, upsert_schedule(conn, &sw)),
        WriteCommand::PatchSchedule {
            schedule_id,
            patch,
            reply,
        } => ack(reply, patch_schedule(conn, schedule_id, &patch)),
        WriteCommand::AddSkips {
            schedule_id,
            fires,
            created_by,
            reply,
        } => ack(reply, add_skips(conn, schedule_id, &fires, &created_by)),
        WriteCommand::DeleteSkips {
            schedule_id,
            fires,
            reply,
        } => ack(reply, delete_skips(conn, schedule_id, &fires)),
        WriteCommand::DeleteSkipsBefore {
            schedule_id,
            before,
            reply,
        } => ack(
            reply,
            conn.execute(
                "DELETE FROM schedule_skip WHERE schedule_id = ?1 AND fire_time <= ?2",
                params![schedule_id, before],
            )
            .map_err(Into::into),
        ),
        WriteCommand::SyncSkipMarks { schedule_id, reply } => {
            ack(reply, sync_skip_marks(conn, schedule_id))
        }
        WriteCommand::DeleteSchedule { schedule_id, reply } => ack(reply, {
            let _ = conn.execute(
                "DELETE FROM run WHERE schedule_id = ?1 AND state_type = 'Scheduled' AND engine_pid IS NULL",
                params![schedule_id],
            );
            conn.execute("DELETE FROM schedule WHERE id = ?1", params![schedule_id])
                .map(|n| n > 0)
                .map_err(Into::into)
        }),
        WriteCommand::DeleteUnstartedRuns { schedule_id, reply } => ack(reply, {
            let ids: rusqlite::Result<Vec<i64>> = (|| {
                let mut stmt = conn.prepare_cached(
                    "SELECT id FROM run WHERE schedule_id = ?1 AND state_type = 'Scheduled' AND engine_pid IS NULL",
                )?;
                let ids = stmt
                    .query_map(params![schedule_id], |r| r.get(0))?
                    .collect::<rusqlite::Result<Vec<i64>>>()?;
                for id in &ids {
                    conn.execute("DELETE FROM run WHERE id = ?1", params![id])?;
                }
                Ok(ids)
            })();
            ids.map_err(Into::into)
        }),
        WriteCommand::CreateBackfill(b, reply) => ack(reply, {
            let id = new_id();
            conn.execute(
                "INSERT INTO backfill (external_id, flow_id, parameter, start_value, end_value, interval_secs, concurrency, total, extra_parameters, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    id.as_bytes().as_slice(),
                    b.flow_id,
                    b.parameter,
                    b.start_value,
                    b.end_value,
                    b.interval_secs,
                    b.concurrency,
                    b.total,
                    b.extra_parameters,
                    now_micros()
                ],
            )
            .map(|_| (conn.last_insert_rowid(), id))
            .map_err(Into::into)
        }),
        WriteCommand::SetBackfillCancelled { backfill_id, reply } => ack(
            reply,
            conn.execute(
                "UPDATE backfill SET cancelled = 1 WHERE id = ?1",
                params![backfill_id],
            )
            .map(|_| ())
            .map_err(Into::into),
        ),
        WriteCommand::KvDelete { key, reply } => ack(
            reply,
            conn.execute("DELETE FROM kv WHERE key = ?1", params![key])
                .map(|n| n > 0)
                .map_err(Into::into),
        ),
        WriteCommand::KvSet { key, value, reply } => ack(
            reply,
            conn.execute(
                "INSERT INTO kv (key, value, updated_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                params![key, value, now_micros()],
            )
            .map(|_| ())
            .map_err(Into::into),
        ),
        WriteCommand::AppendEvent(e, reply) => ack(reply, append_event(conn, &e)),
        WriteCommand::AppendEvents(events, reply) => ack(
            reply,
            events.iter().map(|e| append_event(conn, e)).collect::<Result<Vec<_>>>(),
        ),
        WriteCommand::UpsertRule(rw, reply) => ack(reply, upsert_rule(conn, &rw)),
        WriteCommand::ArmExpectation(a, reply) => ack(reply, arm_expectation(conn, &a)),
        WriteCommand::DisarmExpectations {
            rule_id,
            key,
            at,
            reply,
        } => ack(reply, {
            (|| {
                let mut stmt = conn.prepare_cached(
                    "SELECT id FROM expectation WHERE rule_id = ?1 AND key = ?2 AND status = 'open' AND deadline >= ?3",
                )?;
                let ids = stmt
                    .query_map(params![rule_id, key, at], |r| r.get::<_, i64>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                for id in &ids {
                    conn.execute(
                        "UPDATE expectation SET status = 'met' WHERE id = ?1",
                        params![id],
                    )?;
                }
                Ok(ids)
            })()
        }),
        WriteCommand::SettleExpectation { id, status, reply } => ack(
            reply,
            conn.execute(
                "UPDATE expectation SET status = ?1 WHERE id = ?2 AND status = 'open'",
                params![status, id],
            )
            .map(|n| n > 0)
            .map_err(Into::into),
        ),
        WriteCommand::CancelRuleExpectations { rule_id, reply } => ack(
            reply,
            conn.execute(
                "UPDATE expectation SET status = 'cancelled' WHERE rule_id = ?1 AND status = 'open'",
                params![rule_id],
            )
            .map_err(Into::into),
        ),
        WriteCommand::DeleteRule { rule_id, reply } => ack(
            reply,
            conn.execute("DELETE FROM rule WHERE id = ?1", params![rule_id])
                .map(|n| n > 0)
                .map_err(Into::into),
        ),
        WriteCommand::PruneCodeRules { keep, reply } => ack(reply, {
            let list = keep
                .iter()
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .join(",");
            let sql = if list.is_empty() {
                "DELETE FROM rule WHERE source = 'code'".to_string()
            } else {
                format!("DELETE FROM rule WHERE source = 'code' AND id NOT IN ({list})")
            };
            conn.execute(&sql, []).map_err(Into::into)
        }),
        WriteCommand::RecordFiring {
            rule_id,
            event_id,
            run_id,
            outcomes,
            reply,
        } => ack(reply, {
            let now = now_micros();
            (|| {
                conn.execute(
                    "INSERT INTO rule_firing (rule_id, event_id, run_id, timestamp, outcomes) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![rule_id, event_id, run_id, now, outcomes],
                )?;
                let id = conn.last_insert_rowid();
                conn.execute(
                    "UPDATE rule SET fire_count = fire_count + 1, last_fired = ?1 WHERE id = ?2",
                    params![now, rule_id],
                )?;
                Ok::<i64, StoreError>(id)
            })()
        }),
        WriteCommand::UpsertArtifact(a, reply) => ack(reply, upsert_artifact(conn, &a)),
        WriteCommand::SetVariable {
            name,
            value,
            tags,
            secret,
            reply,
        } => ack(
            reply,
            conn.execute(
                "INSERT INTO variable (name, value, tags, secret, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?5)
                 ON CONFLICT (name) DO UPDATE SET value = excluded.value, tags = excluded.tags, secret = excluded.secret, updated_at = excluded.updated_at",
                params![name, value, tags, secret as i64, now_micros()],
            )
            .map(|_| ())
            .map_err(Into::into),
        ),
        WriteCommand::DeleteVariable { name, reply } => ack(
            reply,
            conn.execute("DELETE FROM variable WHERE name = ?1", params![name])
                .map(|n| n > 0)
                .map_err(Into::into),
        ),
        WriteCommand::DeleteExpired {
            table,
            before,
            limit,
            reply,
        } => ack(reply, {
            let col = "timestamp";
            let allowed = table == "event" || table == "log";
            if !allowed {
                Err(StoreError::Invalid(format!("retention does not apply to {table}")))
            } else {
                conn.execute(
                    &format!("DELETE FROM {table} WHERE id IN (SELECT id FROM {table} WHERE {col} < ?1 ORDER BY id LIMIT ?2)"),
                    params![before, limit],
                )
                .map_err(Into::into)
            }
        }),
        WriteCommand::DeleteExpiredRuns {
            before,
            failed_before,
            keep_per_flow,
            limit,
            reply,
        } => ack(
            reply,
            delete_expired_runs(conn, before, failed_before, keep_per_flow, limit),
        ),
        WriteCommand::IncrementalVacuum { pages, reply } => ack(
            reply,
            conn.execute_batch(&format!("PRAGMA incremental_vacuum({pages})"))
                .map_err(Into::into),
        ),
        WriteCommand::TransitionRun {
            run_id,
            state,
            force,
            reply,
        } => ack(reply, transition(conn, Table::Run, run_id, state, force)),
        WriteCommand::TransitionMany {
            run_ids,
            state,
            reply,
        } => ack(reply, {
            let mut accepted = Vec::with_capacity(run_ids.len());
            let mut failure = None;
            for id in run_ids {
                match transition(conn, Table::Run, id, state.clone(), false) {
                    Ok(_) => accepted.push(id),
                    Err(StoreError::RejectedWith { .. }) | Err(StoreError::Rejected(_)) | Err(StoreError::NotFound(_)) => {}
                    Err(e) => {
                        failure = Some(e);
                        break;
                    }
                }
            }
            match failure {
                Some(e) => Err(e),
                None => Ok(accepted),
            }
        }),
        WriteCommand::SetRunEngine {
            run_id,
            engine_pid,
            engine_id,
            reply,
        } => ack(
            reply,
            conn.execute(
                "UPDATE run SET engine_pid = ?1, engine_id = ?2 WHERE id = ?3",
                params![engine_pid, engine_id, run_id],
            )
            .map(|_| ())
            .map_err(Into::into),
        ),
        WriteCommand::CreateTaskRun(t, reply) => ack(reply, create_task_run(conn, &t)),
        WriteCommand::TransitionTaskRun {
            task_run_id,
            state,
            force,
            reply,
        } => ack(
            reply,
            transition(conn, Table::TaskRun, task_run_id, state, force),
        ),
        WriteCommand::AppendLogs(logs, reply) => ack(reply, append_logs(conn, &logs)),
        WriteCommand::ApplyReport {
            run_id,
            events,
            reply,
        } => ack(reply, apply_report(conn, run_id, events)),
        WriteCommand::DeleteRun { run_id, reply } => ack(reply, delete_run(conn, run_id)),
        WriteCommand::DeleteFlowRows {
            rows,
            flow_ids,
            limit,
            reply,
        } => ack(reply, delete_flow_rows(conn, rows, &flow_ids, limit)),
        WriteCommand::DeleteFlows { flow_ids, reply } => ack(reply, delete_flows(conn, &flow_ids)),
        WriteCommand::BackupTo { path, reply } => ack(
            reply,
            conn.execute("VACUUM INTO ?1", params![path])
                .map(|_| ())
                .map_err(Into::into),
        ),
        WriteCommand::Reset {
            scope,
            live_flows,
            reply,
        } => ack(reply, reset(conn, scope, &live_flows)),
        WriteCommand::Vacuum(reply) => ack(reply, conn.execute_batch("VACUUM").map_err(Into::into)),
        WriteCommand::Flush(reply) => ack(reply, Ok(())),
        WriteCommand::Shutdown => Box::new(|| {}),
    }
}

fn upsert_flow(conn: &Connection, f: &UpsertFlow) -> Result<i64> {
    let now = now_micros();
    let id = new_id();
    let options = if f.options.is_empty() {
        "{}"
    } else {
        &f.options
    };
    conn.execute(
        "INSERT INTO flow (external_id, project, name, module, source_dir, description, tags, parameter_schema, created_at, last_seen_at, options, flow_group)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9, ?10, ?11)
         ON CONFLICT (project, name) DO UPDATE SET
            module = excluded.module,
            source_dir = excluded.source_dir,
            description = excluded.description,
            tags = excluded.tags,
            parameter_schema = excluded.parameter_schema,
            options = excluded.options,
            flow_group = excluded.flow_group,
            error = NULL,
            last_seen_at = excluded.last_seen_at",
        params![
            id.as_bytes().as_slice(),
            f.project,
            f.name,
            f.module,
            f.source_dir,
            f.description,
            f.tags,
            f.parameter_schema,
            now,
            options,
            f.group
        ],
    )?;
    let flow_id: i64 = conn.query_row(
        "SELECT id FROM flow WHERE project = ?1 AND name = ?2",
        params![f.project, f.name],
        |r| r.get(0),
    )?;
    Ok(flow_id)
}

fn create_run(conn: &Connection, r: &CreateRun) -> Result<(i64, Id)> {
    let id = new_id();
    let mut stmt = conn.prepare_cached(
        "INSERT INTO run (external_id, flow_id, name, parameters, tags, created_at, created_by,
                          schedule_id, scheduled_time, priority, parent_run_id, attempt, backfill_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
    )?;
    stmt.execute(params![
        id.as_bytes().as_slice(),
        r.flow_id,
        r.name,
        r.parameters,
        r.tags,
        now_micros(),
        r.created_by,
        r.schedule_id,
        r.scheduled_time,
        r.priority,
        r.parent_run_id,
        r.attempt,
        r.backfill_id
    ])?;
    drop(stmt);
    let run_id = conn.last_insert_rowid();
    if let Some(state) = &r.initial_state {
        transition(conn, Table::Run, run_id, state.clone(), false)?;
    }
    Ok((run_id, id))
}

fn create_task_run(conn: &Connection, t: &CreateTaskRun) -> Result<(i64, Id)> {
    let id = t.external_id.unwrap_or_else(new_id);
    conn.execute(
        "INSERT INTO task_run (external_id, run_id, name, task_key, dynamic_key, created_at, parents, pass)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT (external_id) DO NOTHING",
        params![
            id.as_bytes().as_slice(),
            t.run_id,
            t.name,
            t.task_key,
            t.dynamic_key,
            now_micros(),
            serde_json::to_string(&t.parents).unwrap_or_else(|_| "[]".into()),
            t.pass
        ],
    )?;
    let row_id: i64 = conn.query_row(
        "SELECT id FROM task_run WHERE external_id = ?1",
        params![id.as_bytes().as_slice()],
        |r| r.get(0),
    )?;
    Ok((row_id, id))
}

#[derive(Clone, Copy)]
enum Table {
    Run,
    TaskRun,
}

impl Table {
    fn main(self) -> &'static str {
        match self {
            Table::Run => "run",
            Table::TaskRun => "task_run",
        }
    }
    fn history(self) -> &'static str {
        match self {
            Table::Run => "run_state",
            Table::TaskRun => "task_run_state",
        }
    }
    fn fk(self) -> &'static str {
        match self {
            Table::Run => "run_id",
            Table::TaskRun => "task_run_id",
        }
    }
    fn not_found(self) -> &'static str {
        match self {
            Table::Run => "run",
            Table::TaskRun => "task run",
        }
    }
}

/// Apply the shared transition rules and record the accepted state.
fn transition(
    conn: &Connection,
    table: Table,
    id: i64,
    state: State,
    force: bool,
) -> Result<State> {
    let sql = format!(
        "SELECT state_type, state_name, state_message, state_details, state_timestamp,
                failure_count, crash_count, start_time, end_time, total_run_time
         FROM {} WHERE id = ?1",
        table.main()
    );
    let row = conn
        .query_row(&sql, params![id], |r| {
            Ok((
                state_from_columns(r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?),
                cereyan_core::rules::RunCounters {
                    failure_count: r.get::<_, i64>(5)? as u32,
                    crash_count: r.get::<_, i64>(6)? as u32,
                    start_time: r.get(7)?,
                    end_time: r.get(8)?,
                    total_run_time: r.get(9)?,
                },
            ))
        })
        .optional()?;
    let (current, mut counters) = row.ok_or(StoreError::NotFound(table.not_found()))?;

    let proposal = Proposal { state, force };
    let outcome = propose(current.as_ref(), &proposal, &RunPolicy::default());
    let accepted = outcome
        .resolved(proposal.state)
        .map_err(|reason| StoreError::RejectedWith {
            reason,
            current: current.clone(),
        })?;
    counters.apply(&accepted);

    let details = serde_json::to_string(&accepted.details)?;
    let sql = format!(
        "UPDATE {} SET state_type = ?1, state_name = ?2, state_message = ?3, state_details = ?4,
                state_timestamp = ?5, failure_count = ?6, crash_count = ?7,
                start_time = ?8, end_time = ?9, total_run_time = ?10
         WHERE id = ?11",
        table.main()
    );
    conn.execute(
        &sql,
        params![
            accepted.state_type.as_str(),
            accepted.name,
            accepted.message,
            details,
            accepted.timestamp,
            counters.failure_count as i64,
            counters.crash_count as i64,
            counters.start_time,
            counters.end_time,
            counters.total_run_time,
            id
        ],
    )?;
    let sql = format!(
        "INSERT INTO {} ({}, type, name, message, details, timestamp) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        table.history(),
        table.fk()
    );
    conn.execute(
        &sql,
        params![
            id,
            accepted.state_type.as_str(),
            accepted.name,
            accepted.message,
            details,
            accepted.timestamp
        ],
    )?;
    Ok(accepted)
}

fn task_run_id_by_external(conn: &Connection, ext: &Id) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT id FROM task_run WHERE external_id = ?1",
            params![ext.as_bytes().as_slice()],
            |r| r.get(0),
        )
        .optional()?)
}

fn append_logs(conn: &Connection, logs: &[NewLog]) -> Result<usize> {
    append_logs_with_map(conn, logs, &mut HashMap::new())
}

fn append_logs_with_map(
    conn: &Connection,
    logs: &[NewLog],
    ext_cache: &mut HashMap<Id, Option<i64>>,
) -> Result<usize> {
    let mut stmt = conn.prepare_cached(
        "INSERT INTO log (run_id, task_run_id, level, logger, timestamp, message) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    for l in logs {
        let task_run_id = match (l.task_run_id, l.task_run_external_id) {
            (Some(id), _) => Some(id),
            (None, Some(ext)) => match ext_cache.get(&ext) {
                Some(v) => *v,
                None => {
                    let v = task_run_id_by_external(conn, &ext)?;
                    ext_cache.insert(ext, v);
                    v
                }
            },
            (None, None) => None,
        };
        stmt.execute(params![
            l.run_id,
            task_run_id,
            l.level,
            l.logger,
            l.timestamp,
            l.message
        ])?;
    }
    Ok(logs.len())
}

/// Apply a batch of engine events in order, skipping any whose sequence
/// number is not greater than the run's stored `report_seq`.
/// Which event a rejection came from, for the message the server records.
fn event_kind(event: &ReportEvent) -> &'static str {
    match event {
        ReportEvent::TaskRunCreated { .. } => "task_run_created",
        ReportEvent::TaskRunTransition { .. } => "task_run_transition",
        ReportEvent::Logs { .. } => "logs",
        ReportEvent::Custom { .. } => "custom",
        ReportEvent::Artifact { .. } => "artifact",
    }
}

/// Whether one event can be dropped while the rest of its report applies. A
/// constraint the engine broke concerns that event alone; a database that
/// cannot be read or written concerns every event, and still fails the report.
fn is_event_rejection(e: &StoreError) -> bool {
    match e {
        StoreError::Sqlite(rusqlite::Error::SqliteFailure(err, _)) => {
            matches!(err.code, rusqlite::ErrorCode::ConstraintViolation)
        }
        StoreError::Invalid(_) | StoreError::NotFound(_) => true,
        _ => false,
    }
}

fn apply_report(conn: &Connection, run_id: i64, events: Vec<ReportEvent>) -> Result<ReportOutcome> {
    let mut last_seq: i64 = conn
        .query_row(
            "SELECT report_seq FROM run WHERE id = ?1",
            params![run_id],
            |r| r.get(0),
        )
        .optional()?
        .ok_or(StoreError::NotFound("run"))?;
    let mut out = ReportOutcome {
        last_seq,
        ..Default::default()
    };
    let mut ext_cache: HashMap<Id, Option<i64>> = HashMap::new();
    let mut touched: Vec<Id> = Vec::new();
    let mut before: HashMap<Id, Option<String>> = HashMap::new();
    let remember_before =
        |conn: &Connection, ext: &Id, before: &mut HashMap<Id, Option<String>>| {
            if !before.contains_key(ext) {
                let prev: Option<String> = conn
                    .query_row(
                        "SELECT state_type FROM task_run WHERE external_id = ?1",
                        params![ext.as_bytes().as_slice()],
                        |r| r.get(0),
                    )
                    .optional()
                    .ok()
                    .flatten()
                    .flatten();
                before.insert(*ext, prev);
            }
        };
    for event in events {
        if event.seq() <= last_seq {
            out.skipped += 1;
            continue;
        }
        // One rejected event is not a rejected report. A whole batch used to be
        // discarded for a single bad row, which is how an entire execution of a
        // run went missing from its history without a word anywhere.
        let step = (|| -> Result<bool> {
            match &event {
                ReportEvent::TaskRunCreated {
                    external_id,
                    name,
                    task_key,
                    dynamic_key,
                    parents,
                    pass,
                    ..
                } => {
                    remember_before(conn, external_id, &mut before);
                    let (row_id, _) = create_task_run(
                        conn,
                        &CreateTaskRun {
                            run_id,
                            name: name.clone(),
                            task_key: task_key.clone(),
                            dynamic_key: dynamic_key.clone(),
                            external_id: Some(*external_id),
                            parents: parents.clone(),
                            pass: *pass,
                        },
                    )?;
                    ext_cache.insert(*external_id, Some(row_id));
                    if !touched.contains(external_id) {
                        touched.push(*external_id);
                    }
                }
                ReportEvent::TaskRunTransition {
                    external_id,
                    state,
                    force,
                    ..
                } => {
                    remember_before(conn, external_id, &mut before);
                    let row_id = match ext_cache.get(external_id) {
                        Some(Some(id)) => *id,
                        _ => match task_run_id_by_external(conn, external_id)? {
                            Some(id) => {
                                ext_cache.insert(*external_id, Some(id));
                                id
                            }
                            None => {
                                // Unknown task run: the create event was lost; skip.
                                out.skipped += 1;
                                return Ok(false);
                            }
                        },
                    };
                    match transition(conn, Table::TaskRun, row_id, state.clone(), *force) {
                        Ok(accepted) => out.task_run_states.push((*external_id, accepted)),
                        Err(StoreError::RejectedWith { .. }) | Err(StoreError::Rejected(_)) => {
                            // Rules already applied once (redelivery or a stale
                            // proposal); the stored state wins.
                        }
                        Err(e) => return Err(e),
                    }
                    if !touched.contains(external_id) {
                        touched.push(*external_id);
                    }
                }
                ReportEvent::Logs { logs, .. } => {
                    let n = append_logs_with_map(conn, logs, &mut ext_cache)?;
                    out.log_count += n;
                    if n > 0 {
                        out.last_log_id = Some(conn.last_insert_rowid());
                    }
                }
                ReportEvent::Custom {
                    name,
                    payload,
                    task_run_external_id,
                    ..
                } => {
                    let (resource, run_row) = run_resource(conn, run_id)?;
                    let mut related = run_related(conn, run_id)?;
                    if let Some(ext) = task_run_external_id {
                        related.push(cereyan_core::Resource {
                            kind: "task_run".into(),
                            id: ext.to_string(),
                            name: String::new(),
                        });
                    }
                    let (eid, _) = append_event(
                        conn,
                        &NewEvent {
                            name: name.clone(),
                            run_id: Some(run_id),
                            flow_id: run_row,
                            payload: payload.clone(),
                            resource,
                            related,
                        },
                    )?;
                    out.event_ids.push(eid);
                }
                ReportEvent::Artifact {
                    external_id,
                    task_run_external_id,
                    artifact_kind,
                    key,
                    data,
                    ..
                } => {
                    let task_run_id = match task_run_external_id {
                        Some(ext) => match ext_cache.get(ext) {
                            Some(v) => *v,
                            None => {
                                let v = task_run_id_by_external(conn, ext)?;
                                ext_cache.insert(*ext, v);
                                v
                            }
                        },
                        None => None,
                    };
                    let aid = upsert_artifact(
                        conn,
                        &UpsertArtifact {
                            run_id,
                            task_run_id,
                            kind: artifact_kind.clone(),
                            key: key.clone(),
                            data: data.to_string(),
                            external_id: Some(*external_id),
                        },
                    )?;
                    out.artifact_ids.push(aid);
                }
            }
            Ok(true)
        })();
        match step {
            Ok(true) => {
                last_seq = event.seq();
                out.applied += 1;
            }
            Ok(false) => last_seq = event.seq(),
            Err(e) if is_event_rejection(&e) => {
                out.rejected.push(RejectedEvent {
                    seq: event.seq(),
                    kind: event_kind(&event).into(),
                    reason: e.to_string(),
                });
                last_seq = event.seq();
            }
            Err(e) => return Err(e),
        }
    }
    if last_seq != out.last_seq {
        conn.execute(
            "UPDATE run SET report_seq = ?1 WHERE id = ?2",
            params![last_seq, run_id],
        )?;
        out.last_seq = last_seq;
    }
    let sql = format!("SELECT {TASK_RUN_COLUMNS} FROM {TASK_RUN_FROM} WHERE t.external_id = ?1");
    for ext in touched {
        if let Some(t) = conn
            .query_row(&sql, params![ext.as_bytes().as_slice()], task_run_from_row)
            .optional()?
        {
            let prev = before.get(&ext).cloned().flatten();
            let next = t.state.state_type.as_str().to_string();
            if prev.as_deref() != Some(next.as_str()) {
                out.task_run_transitions.push((prev, next));
            }
            out.task_runs.push(t);
        }
    }
    Ok(out)
}

fn delete_run(conn: &Connection, run_id: i64) -> Result<bool> {
    let n = conn.execute("DELETE FROM run WHERE id = ?1", params![run_id])?;
    Ok(n > 0)
}

/// Terminal state types as the `run` table spells them.
const TERMINAL: &str = "'Completed', 'Failed', 'Cancelled', 'Crashed'";

/// The retention pass for runs. A run's age is its end time, else its state
/// time, else its creation time. A backfill is live while it is not cancelled
/// and still has a non-terminal run. `keep_per_flow` counts terminal runs, so
/// a flow always keeps that much finished history whatever is running. The schema cascades from `run` to task
/// runs, states, logs, events, and artifacts; the `run.input` answer is a kv
/// row and goes explicitly.
fn delete_expired_runs(
    conn: &Connection,
    before: i64,
    failed_before: i64,
    keep_per_flow: i64,
    limit: i64,
) -> Result<Vec<(i64, i64, String)>> {
    let sql = format!(
        "SELECT r.id, r.flow_id, r.state_type FROM run r
         WHERE r.state_type IN ({TERMINAL})
           AND COALESCE(r.end_time, r.state_timestamp, r.created_at)
               < CASE WHEN r.state_type IN ('Failed', 'Crashed') THEN ?2 ELSE ?1 END
           AND (r.backfill_id IS NULL OR NOT EXISTS (
                 SELECT 1 FROM backfill b
                 WHERE b.id = r.backfill_id AND b.cancelled = 0
                   AND EXISTS (SELECT 1 FROM run x WHERE x.backfill_id = b.id
                               AND x.state_type NOT IN ({TERMINAL}))))
           AND r.id NOT IN (SELECT n.id FROM run n
                            WHERE n.flow_id = r.flow_id AND n.state_type IN ({TERMINAL})
                            ORDER BY n.id DESC LIMIT ?3)
         ORDER BY r.id LIMIT ?4"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows: Vec<(i64, i64, String)> = stmt
        .query_map(
            params![before, failed_before, keep_per_flow.max(0), limit],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?
        .collect::<std::result::Result<_, _>>()?;
    if rows.is_empty() {
        return Ok(rows);
    }
    let ids = id_list(&rows.iter().map(|r| r.0).collect::<Vec<_>>());
    conn.execute_batch(&format!(
        "DELETE FROM kv WHERE key IN (SELECT 'run.input:' || id FROM run WHERE id IN ({ids}));
         DELETE FROM run WHERE id IN ({ids});"
    ))?;
    Ok(rows)
}

/// A comma-separated id list for `IN (...)`; SQLite reads `IN ()` as empty.
fn id_list(ids: &[i64]) -> String {
    ids.iter()
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(",")
}

fn count(conn: &Connection, sql: &str) -> Result<i64> {
    Ok(conn.query_row(sql, [], |r| r.get(0))?)
}

fn delete_flow_rows(
    conn: &Connection,
    rows: FlowRows,
    flow_ids: &[i64],
    limit: i64,
) -> Result<usize> {
    let ids = id_list(flow_ids);
    let sql = match rows {
        FlowRows::Log => format!(
            "DELETE FROM log WHERE id IN (SELECT l.id FROM log l JOIN run r ON r.id = l.run_id WHERE r.flow_id IN ({ids}) LIMIT ?1)"
        ),
        FlowRows::Event => format!(
            "DELETE FROM event WHERE id IN (SELECT id FROM event WHERE flow_id IN ({ids}) LIMIT ?1)"
        ),
    };
    Ok(conn.execute(&sql, params![limit])?)
}

/// Delete flows; the cascades take their runs, task runs, logs, artifacts,
/// schedules and backfills. Events, expectations and stored answers have no
/// foreign key and are deleted here.
fn delete_flows(conn: &Connection, flow_ids: &[i64]) -> Result<DeletedCounts> {
    let ids = id_list(flow_ids);
    let runs = format!("SELECT id FROM run WHERE flow_id IN ({ids})");
    let out = DeletedCounts {
        flows: count(
            conn,
            &format!("SELECT COUNT(*) FROM flow WHERE id IN ({ids})"),
        )?,
        runs: count(
            conn,
            &format!("SELECT COUNT(*) FROM run WHERE flow_id IN ({ids})"),
        )?,
        task_runs: count(
            conn,
            &format!("SELECT COUNT(*) FROM task_run WHERE run_id IN ({runs})"),
        )?,
        logs: count(
            conn,
            &format!("SELECT COUNT(*) FROM log WHERE run_id IN ({runs})"),
        )?,
        events: count(
            conn,
            &format!("SELECT COUNT(*) FROM event WHERE flow_id IN ({ids})"),
        )?,
        artifacts: count(
            conn,
            &format!("SELECT COUNT(*) FROM artifact WHERE run_id IN ({runs})"),
        )?,
        schedules: count(
            conn,
            &format!("SELECT COUNT(*) FROM schedule WHERE flow_id IN ({ids})"),
        )?,
        backfills: count(
            conn,
            &format!("SELECT COUNT(*) FROM backfill WHERE flow_id IN ({ids})"),
        )?,
        ..Default::default()
    };
    conn.execute(
        &format!("DELETE FROM kv WHERE key IN (SELECT 'run.input:' || id FROM run WHERE flow_id IN ({ids}))"),
        [],
    )?;
    conn.execute(
        &format!("DELETE FROM expectation WHERE flow_id IN ({ids}) OR run_id IN ({runs})"),
        [],
    )?;
    conn.execute(&format!("DELETE FROM event WHERE flow_id IN ({ids})"), [])?;
    conn.execute(&format!("DELETE FROM flow WHERE id IN ({ids})"), [])?;
    Ok(out)
}

/// Children are deleted before parents, so the cascades find nothing left to do.
fn reset(conn: &Connection, scope: ResetScope, live_flows: &[i64]) -> Result<DeletedCounts> {
    let mut out = DeletedCounts {
        runs: count(conn, "SELECT COUNT(*) FROM run")?,
        task_runs: count(conn, "SELECT COUNT(*) FROM task_run")?,
        logs: count(conn, "SELECT COUNT(*) FROM log")?,
        events: count(conn, "SELECT COUNT(*) FROM event")?,
        artifacts: count(conn, "SELECT COUNT(*) FROM artifact")?,
        backfills: count(conn, "SELECT COUNT(*) FROM backfill")?,
        ..Default::default()
    };
    conn.execute_batch(
        "DELETE FROM log;
         DELETE FROM task_run_state;
         DELETE FROM artifact;
         DELETE FROM task_run;
         DELETE FROM run_state;
         DELETE FROM rule_firing;
         DELETE FROM expectation;
         DELETE FROM event;
         DELETE FROM run;
         DELETE FROM backfill;
         DELETE FROM kv WHERE key LIKE 'run.input:%';
         UPDATE rule SET fire_count = 0, last_fired = NULL;",
    )?;
    if scope == ResetScope::Everything {
        let live = id_list(live_flows);
        out.flows = count(
            conn,
            &format!("SELECT COUNT(*) FROM flow WHERE id NOT IN ({live})"),
        )?;
        out.schedules = count(
            conn,
            &format!(
                "SELECT COUNT(*) FROM schedule WHERE source != 'code' OR flow_id NOT IN ({live})"
            ),
        )?;
        out.variables = count(conn, "SELECT COUNT(*) FROM variable")?;
        conn.execute(&format!("DELETE FROM flow WHERE id NOT IN ({live})"), [])?;
        conn.execute("DELETE FROM schedule WHERE source != 'code'", [])?;
        conn.execute(
            "DELETE FROM kv WHERE key IN (SELECT 'rules.clock_last:' || id FROM rule WHERE source != 'code')",
            [],
        )?;
        out.rules = conn.execute("DELETE FROM rule WHERE source != 'code'", [])? as i64;
        conn.execute("DELETE FROM variable", [])?;
    }
    Ok(out)
}

fn upsert_schedule(conn: &Connection, sw: &ScheduleWrite) -> Result<i64> {
    let now = now_micros();
    if let Some(id) = sw.id {
        conn.execute(
            "UPDATE schedule SET spec = ?1, catchup = ?2, catchup_max = ?3, active = ?4, source = ?5, code_key = ?6, persist = ?7, updated_at = ?8 WHERE id = ?9",
            params![sw.spec, sw.catchup, sw.catchup_max, sw.active as i64, sw.source, sw.code_key, sw.persist as i64, now, id],
        )?;
        return Ok(id);
    }
    let ext = new_id();
    conn.execute(
        "INSERT INTO schedule (external_id, flow_id, spec, catchup, catchup_max, active, source, code_key, persist, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)",
        params![
            ext.as_bytes().as_slice(),
            sw.flow_id,
            sw.spec,
            sw.catchup,
            sw.catchup_max,
            sw.active as i64,
            sw.source,
            sw.code_key,
            sw.persist as i64,
            now
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

fn patch_schedule(conn: &Connection, id: i64, p: &SchedulePatch) -> Result<bool> {
    let mut sets: Vec<String> = Vec::new();
    let mut args: Vec<rusqlite::types::Value> = Vec::new();
    let push = |col: &str,
                v: rusqlite::types::Value,
                sets: &mut Vec<String>,
                args: &mut Vec<rusqlite::types::Value>| {
        args.push(v);
        sets.push(format!("{col} = ?{}", args.len()));
    };
    if let Some(v) = &p.spec {
        push(
            "spec",
            rusqlite::types::Value::Text(v.clone()),
            &mut sets,
            &mut args,
        );
    }
    if let Some(v) = &p.catchup {
        push(
            "catchup",
            rusqlite::types::Value::Text(v.clone()),
            &mut sets,
            &mut args,
        );
    }
    if let Some(v) = p.catchup_max {
        push(
            "catchup_max",
            rusqlite::types::Value::Integer(v),
            &mut sets,
            &mut args,
        );
    }
    if let Some(v) = p.active {
        push(
            "active",
            rusqlite::types::Value::Integer(v as i64),
            &mut sets,
            &mut args,
        );
    }
    if let Some(v) = &p.paused_reason {
        let val = match v {
            Some(t) => rusqlite::types::Value::Text(t.clone()),
            None => rusqlite::types::Value::Null,
        };
        push("paused_reason", val, &mut sets, &mut args);
    }
    if let Some(v) = &p.paused_until {
        let val = match v {
            Some(t) => rusqlite::types::Value::Integer(*t),
            None => rusqlite::types::Value::Null,
        };
        push("paused_until", val, &mut sets, &mut args);
    }
    if let Some(v) = p.persist {
        push(
            "persist",
            rusqlite::types::Value::Integer(v as i64),
            &mut sets,
            &mut args,
        );
    }
    args.push(rusqlite::types::Value::Integer(now_micros()));
    sets.push(format!("updated_at = ?{}", args.len()));
    args.push(rusqlite::types::Value::Integer(id));
    let sql = format!(
        "UPDATE schedule SET {} WHERE id = ?{}",
        sets.join(", "),
        args.len()
    );
    let n = conn.execute(&sql, rusqlite::params_from_iter(args.iter()))?;
    Ok(n > 0)
}

fn add_skips(
    conn: &Connection,
    schedule_id: i64,
    fires: &[i64],
    created_by: &str,
) -> Result<usize> {
    let now = now_micros();
    let mut stmt = conn.prepare_cached(
        "INSERT OR IGNORE INTO schedule_skip (schedule_id, fire_time, created_at, created_by)
         VALUES (?1, ?2, ?3, ?4)",
    )?;
    let mut added = 0;
    for fire in fires {
        added += stmt.execute(params![schedule_id, fire, now, created_by])?;
    }
    Ok(added)
}

fn delete_skips(conn: &Connection, schedule_id: i64, fires: &[i64]) -> Result<usize> {
    let mut stmt =
        conn.prepare_cached("DELETE FROM schedule_skip WHERE schedule_id = ?1 AND fire_time = ?2")?;
    let mut deleted = 0;
    for fire in fires {
        deleted += stmt.execute(params![schedule_id, fire])?;
    }
    Ok(deleted)
}

/// The mark is `state_details.skip = "user"` on a run still waiting in Scheduled.
/// A mark is only ever derived from `schedule_skip`, so this is the one place
/// that sets or clears it.
fn sync_skip_marks(conn: &Connection, schedule_id: i64) -> Result<Vec<i64>> {
    let mut changed = Vec::new();
    for sql in [
        "UPDATE run SET state_details = json_set(state_details, '$.skip', 'user')
         WHERE schedule_id = ?1 AND state_type = 'Scheduled' AND engine_pid IS NULL
           AND json_extract(state_details, '$.skip') IS NULL
           AND scheduled_time IN (SELECT fire_time FROM schedule_skip WHERE schedule_id = ?1)
         RETURNING id",
        "UPDATE run SET state_details = json_remove(state_details, '$.skip')
         WHERE schedule_id = ?1 AND state_type = 'Scheduled'
           AND json_extract(state_details, '$.skip') IS NOT NULL
           AND scheduled_time NOT IN (SELECT fire_time FROM schedule_skip WHERE schedule_id = ?1)
         RETURNING id",
    ] {
        let mut stmt = conn.prepare_cached(sql)?;
        let ids = stmt
            .query_map(params![schedule_id], |r| r.get::<_, i64>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        changed.extend(ids);
    }
    Ok(changed)
}

fn append_event(conn: &Connection, e: &NewEvent) -> Result<(i64, Id)> {
    let id = new_id();
    // An event about a run always names its flow too, so a project's events
    // can be found through `event_flow`.
    let flow_id = match (e.flow_id, e.run_id) {
        (None, Some(run_id)) => conn
            .prepare_cached("SELECT flow_id FROM run WHERE id = ?1")?
            .query_row(params![run_id], |r| r.get(0))
            .optional()?,
        (flow_id, _) => flow_id,
    };
    conn.execute(
        "INSERT INTO event (external_id, kind, timestamp, run_id, flow_id, payload, resource_kind, resource_id, resource_name, related)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            id.as_bytes().as_slice(),
            e.name,
            now_micros(),
            e.run_id,
            flow_id,
            e.payload.to_string(),
            e.resource.kind,
            e.resource.id,
            e.resource.name,
            serde_json::to_string(&e.related).unwrap_or_else(|_| "[]".into())
        ],
    )?;
    Ok((conn.last_insert_rowid(), id))
}

/// The run resource and its flow id, for engine-reported custom events.
fn run_resource(conn: &Connection, run_id: i64) -> Result<(cereyan_core::Resource, Option<i64>)> {
    let row = conn
        .query_row(
            "SELECT external_id, name, flow_id FROM run WHERE id = ?1",
            params![run_id],
            |r| {
                Ok((
                    r.get::<_, Vec<u8>>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            },
        )
        .optional()?;
    match row {
        Some((blob, name, flow_id)) => Ok((
            cereyan_core::Resource {
                kind: "run".into(),
                id: Id::from_bytes(&blob)
                    .map(|i| i.to_string())
                    .unwrap_or_default(),
                name,
            },
            Some(flow_id),
        )),
        None => Ok((cereyan_core::Resource::default(), None)),
    }
}

/// Related resources of a run: its flow and its tags.
fn run_related(conn: &Connection, run_id: i64) -> Result<Vec<cereyan_core::Resource>> {
    let row = conn
        .query_row(
            "SELECT f.name, f.project, r.tags FROM run r JOIN flow f ON f.id = r.flow_id WHERE r.id = ?1",
            params![run_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)),
        )
        .optional()?;
    let mut out = Vec::new();
    if let Some((flow, project, tags)) = row {
        out.push(cereyan_core::Resource {
            kind: "flow".into(),
            id: format!("{project}/{flow}"),
            name: flow,
        });
        for tag in serde_json::from_str::<Vec<String>>(&tags).unwrap_or_default() {
            out.push(cereyan_core::Resource {
                kind: "tag".into(),
                id: tag.clone(),
                name: tag,
            });
        }
    }
    Ok(out)
}

fn arm_expectation(conn: &Connection, a: &ArmExpectation) -> Result<i64> {
    let existing: Option<i64> = conn
        .query_row(
            "SELECT id FROM expectation WHERE rule_id = ?1 AND key = ?2 AND status = 'open'",
            params![a.rule_id, a.key],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(id) = existing {
        return Ok(id);
    }
    conn.execute(
        "INSERT INTO expectation (rule_id, key, run_id, flow_id, armed_at, deadline, status) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'open')",
        params![a.rule_id, a.key, a.run_id, a.flow_id, a.armed_at, a.deadline],
    )?;
    Ok(conn.last_insert_rowid())
}

fn upsert_rule(conn: &Connection, rw: &RuleWrite) -> Result<i64> {
    let now = now_micros();
    if let Some(id) = rw.id {
        conn.execute(
            "UPDATE rule SET name = ?1, enabled = ?2, source = ?3, module = ?4, spec = ?5, updated_at = ?6 WHERE id = ?7",
            params![rw.name, rw.enabled as i64, rw.source, rw.module, rw.spec, now, id],
        )?;
        return Ok(id);
    }
    let ext = new_id();
    conn.execute(
        "INSERT INTO rule (external_id, name, enabled, source, module, spec, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
        params![ext.as_bytes().as_slice(), rw.name, rw.enabled as i64, rw.source, rw.module, rw.spec, now],
    )?;
    Ok(conn.last_insert_rowid())
}

fn upsert_artifact(conn: &Connection, a: &UpsertArtifact) -> Result<i64> {
    let now = now_micros();
    if let Some(key) = &a.key {
        let existing: Option<i64> = conn
            .query_row(
                "SELECT id FROM artifact WHERE run_id = ?1 AND COALESCE(task_run_id, 0) = COALESCE(?2, 0) AND key = ?3",
                params![a.run_id, a.task_run_id, key],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = existing {
            conn.execute(
                "UPDATE artifact SET kind = ?1, data = ?2, updated_at = ?3 WHERE id = ?4",
                params![a.kind, a.data, now, id],
            )?;
            return Ok(id);
        }
    }
    let ext = a.external_id.unwrap_or_else(new_id);
    conn.execute(
        "INSERT INTO artifact (external_id, run_id, task_run_id, kind, key, data, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
         ON CONFLICT (external_id) DO UPDATE SET data = excluded.data, updated_at = excluded.updated_at",
        params![ext.as_bytes().as_slice(), a.run_id, a.task_run_id, a.kind, a.key, a.data, now],
    )?;
    Ok(conn.last_insert_rowid())
}

#[cfg(test)]
mod retention_tests {
    use super::*;

    fn store() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON").unwrap();
        crate::migrations::apply(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO flow (id, external_id, project, name, module, source_dir, created_at, last_seen_at)
             VALUES (1, X'01', 'p', 'f', 'm', '/d', 0, 0), (2, X'02', 'p', 'g', 'm', '/d', 0, 0);
             INSERT INTO backfill (id, external_id, flow_id, parameter, start_value, end_value, interval_secs, concurrency, total, created_at)
             VALUES (7, X'07', 1, 'day', 'a', 'b', 1, 1, 2, 0);",
        )
        .unwrap();
        conn
    }

    fn run(conn: &Connection, id: i64, flow: i64, state: &str, end: i64, backfill: Option<i64>) {
        conn.execute(
            "INSERT INTO run (id, external_id, flow_id, name, state_type, created_at, end_time, backfill_id)
             VALUES (?1, ?2, ?3, 'r', ?4, ?5, ?5, ?6)",
            params![id, vec![id as u8], flow, state, end, backfill],
        )
        .unwrap();
    }

    fn ids(conn: &Connection) -> Vec<i64> {
        let mut s = conn.prepare("SELECT id FROM run ORDER BY id").unwrap();
        s.query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }

    #[test]
    fn keeps_running_backfill_and_newest_runs() {
        let conn = store();
        run(&conn, 1, 1, "Completed", 10, None);
        run(&conn, 2, 1, "Completed", 20, None);
        run(&conn, 3, 1, "Completed", 30, None);
        run(&conn, 4, 1, "Running", 5, None);
        run(&conn, 5, 2, "Completed", 10, Some(7));
        run(&conn, 6, 2, "Pending", 10, Some(7));
        run(&conn, 8, 2, "Failed", 10, None);
        run(&conn, 9, 2, "Completed", 15, None);
        conn.execute(
            "INSERT INTO kv (key, value, updated_at) VALUES ('run.input:1', 'x', 0)",
            [],
        )
        .unwrap();
        // Completed older than 100, failed older than 5: run 8 (failed at 10) survives.
        let gone = delete_expired_runs(&conn, 100, 5, 1, 500).unwrap();
        assert_eq!(gone.iter().map(|g| g.0).collect::<Vec<_>>(), vec![1, 2]);
        assert_eq!(gone[0].2, "Completed");
        // 3 is flow 1's newest, 4 is running, 5 is in a live backfill, 8 is a
        // young failure, 9 is flow 2's newest.
        assert_eq!(ids(&conn), vec![3, 4, 5, 6, 8, 9]);
        let kv: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM kv WHERE key = 'run.input:1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(kv, 0);
        // Once the backfill's last run ends, its runs age out too.
        conn.execute("UPDATE run SET state_type = 'Completed' WHERE id = 6", [])
            .unwrap();
        let gone = delete_expired_runs(&conn, 100, 100, 1, 500).unwrap();
        assert_eq!(gone.iter().map(|g| g.0).collect::<Vec<_>>(), vec![5, 6, 8]);
        assert_eq!(ids(&conn), vec![3, 4, 9]);
    }
}
