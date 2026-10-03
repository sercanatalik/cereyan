//! Read queries over the read-only pool.

use std::collections::HashMap;
use std::sync::LazyLock;

use cereyan_core::{
    ArtifactListItem, ArtifactRow, Backfill, Event, Expectation, Flow, Id, Log, RuleFiring,
    RuleRow, Run, ScheduleRow, StateType, TaskRun, VariableRow,
};
use rusqlite::{params_from_iter, types::Value as SqlValue, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::row::{
    artifact_from_row, artifact_list_from_row, backfill_from_row, event_from_row,
    expectation_from_row, firing_from_row, flow_from_row, rule_from_row, run_from_row,
    schedule_from_row, task_run_from_row, variable_from_row, ARTIFACT_COLUMNS,
    ARTIFACT_LIST_COLUMNS, BACKFILL_COLUMNS, EVENT_COLUMNS, EXPECTATION_COLUMNS, FLOW_COLUMNS,
    RULE_COLUMNS, RUN_COLUMNS, SCHEDULE_COLUMNS, TASK_RUN_COLUMNS, TASK_RUN_FROM, VARIABLE_COLUMNS,
};
use crate::{Result, Store, StoreError};

// Single-row statements, built once and compiled once per connection through
// `prepare_cached`. `query_row` would instead re-parse the SQL on every call,
// which for `get_run` means compiling ~800 bytes including a correlated
// aggregate subquery.
static SQL_FLOWS_BY_NAME: LazyLock<String> =
    LazyLock::new(|| format!("SELECT {FLOW_COLUMNS} FROM flow WHERE name = ?1"));
static SQL_RUN_NAME_EXISTS: LazyLock<String> =
    LazyLock::new(|| "SELECT 1 FROM run WHERE name = ?1 LIMIT 1".to_string());
static SQL_GET_RUN: LazyLock<String> = LazyLock::new(|| {
    format!("SELECT {RUN_COLUMNS} FROM run r JOIN flow f ON f.id = r.flow_id WHERE r.id = ?1")
});
static SQL_GET_RUN_BY_EXTERNAL_ID: LazyLock<String> = LazyLock::new(|| {
    format!(
        "SELECT {RUN_COLUMNS} FROM run r JOIN flow f ON f.id = r.flow_id WHERE r.external_id = ?1"
    )
});
static SQL_GET_TASK_RUN: LazyLock<String> =
    LazyLock::new(|| format!("SELECT {TASK_RUN_COLUMNS} FROM {TASK_RUN_FROM} WHERE t.id = ?1"));
static SQL_GET_FLOW: LazyLock<String> =
    LazyLock::new(|| format!("SELECT {FLOW_COLUMNS} FROM flow WHERE id = ?1"));
static SQL_GET_FLOW_BY_KEY: LazyLock<String> =
    LazyLock::new(|| format!("SELECT {FLOW_COLUMNS} FROM flow WHERE project = ?1 AND name = ?2"));
static SQL_GET_SCHEDULE: LazyLock<String> =
    LazyLock::new(|| format!("SELECT {SCHEDULE_COLUMNS} FROM schedule WHERE id = ?1"));
static SQL_GET_BACKFILL: LazyLock<String> =
    LazyLock::new(|| format!("SELECT {BACKFILL_COLUMNS} FROM backfill WHERE id = ?1"));
static SQL_GET_EVENT: LazyLock<String> =
    LazyLock::new(|| format!("SELECT {EVENT_COLUMNS} FROM event WHERE id = ?1"));
static SQL_GET_RULE: LazyLock<String> =
    LazyLock::new(|| format!("SELECT {RULE_COLUMNS} FROM rule WHERE id = ?1"));
static SQL_GET_EXPECTATION: LazyLock<String> =
    LazyLock::new(|| format!("SELECT {EXPECTATION_COLUMNS} FROM expectation WHERE id = ?1"));
static SQL_GET_ARTIFACT: LazyLock<String> =
    LazyLock::new(|| format!("SELECT {ARTIFACT_COLUMNS} FROM artifact WHERE id = ?1"));
static SQL_GET_VARIABLE: LazyLock<String> =
    LazyLock::new(|| format!("SELECT {VARIABLE_COLUMNS} FROM variable WHERE name = ?1"));

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
pub struct EventFilter {
    /// Exact name, or a prefix ending in `*` (e.g. `run.*`).
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub resource_kind: Option<String>,
    #[serde(default)]
    pub resource_id: Option<String>,
    #[serde(default)]
    pub run_id: Option<i64>,
    #[serde(default)]
    pub flow_id: Option<i64>,
    #[serde(default)]
    pub after: Option<i64>,
    #[serde(default)]
    pub before: Option<i64>,
    #[serde(default)]
    pub limit: Option<usize>,
    /// Keyset cursor: the id of the last event of the previous page.
    #[serde(default)]
    pub cursor: Option<i64>,
    /// Ascending order when true (defaults to newest first).
    #[serde(default)]
    pub ascending: bool,
}

/// Filter for the cross-run artifact listing.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
pub struct ArtifactFilter {
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub flow: Option<String>,
    #[serde(default)]
    pub project: Option<String>,
    /// The resolved group: the one the flow declared, else its project.
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub run_id: Option<i64>,
    #[serde(default)]
    pub limit: Option<usize>,
    /// Keyset cursor: the id of the last artifact of the previous page.
    #[serde(default)]
    pub after: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ArtifactsPage {
    pub items: Vec<ArtifactListItem>,
    pub next_cursor: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct EventsPage {
    pub items: Vec<Event>,
    pub next_cursor: Option<i64>,
}

/// One entry of a run's task state store.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TaskStateRow {
    /// The task's dynamic key, or empty for the flow body.
    pub scope: String,
    pub key: String,
    pub value: serde_json::Value,
    pub updated_at: i64,
}

/// kv key holding the checkpoints a run was seeded with, as a JSON list.
pub fn checkpoint_seed_key(run_id: i64) -> String {
    format!("run.checkpoints:{run_id}")
}

/// Skip rows per schedule id, as `list_skip_rows` returns them for one.
pub type SkipRowsBySchedule = std::collections::HashMap<i64, Vec<(i64, i64, String)>>;

/// Completed task runs of a crash chain that stored a replayable result.
///
/// `chain` is a JSON array of run ids so the statement text is the same whatever
/// the chain depth; `json_each` expands it.
/// The four fields of a run a keyed fan-in needs, and no more.
///
/// A fan-in with `after={flow=.., key=..}` asks, for each upstream flow, whether
/// its newest run carrying a given parameter value is Completed or Skipped and
/// whether that skip was carried by a person or an upstream. That is a state, a
/// name, and the `reason` in the state's details.
///
/// A whole `Run` for that is 37 columns, a correlated `task_counts` aggregate and
/// four decoded JSON documents, and the reader builds one for **every** run of
/// every listed flow whose parameters match, keeping one per flow.
#[derive(Clone, Debug, PartialEq)]
pub struct LatestRunMark {
    pub id: i64,
    /// Coalesced to `Scheduled`, as every other run reader does: a run that has not
    /// been transitioned yet stores a NULL state.
    pub state_type: String,
    pub state_name: String,
    /// The state's details, read whole because the only key consulted is `reason`
    /// and a projection of a JSON object is not expressible without a second
    /// `json_extract` per candidate key.
    pub state_details: serde_json::Map<String, serde_json::Value>,
}

impl LatestRunMark {
    /// Whether this run was skipped by a person or by an upstream, and so carries
    /// its skip further down the chain.
    ///
    /// The rule itself, so that the store's projection and the dispatch decision
    /// cannot drift apart: there is one definition of "carries a skip", and it is
    /// here rather than duplicated in the caller.
    pub fn carries_skip(&self) -> bool {
        self.state_name == "Skipped"
            && matches!(
                self.state_details.get("reason").and_then(|v| v.as_str()),
                Some("user") | Some("upstream")
            )
    }

    /// Whether this run counts as having finished, for a fan-in waiting on it.
    pub fn is_complete(&self) -> bool {
        self.state_type == StateType::Completed.as_str() || self.state_name == "Skipped"
    }
}

/// The columns behind `LatestRunMark`, in the order its decoder reads them.
const LATEST_RUN_MARK_COLUMNS: &str = "r.id, COALESCE(r.state_type, 'Scheduled'), \
     COALESCE(r.state_name, 'Scheduled'), r.state_details";

/// `(flow_id, LatestRunMark)` — the leading column is only there so the caller can
/// keep one row per flow without the `run` table's `flow_id` being decoded twice.
fn latest_run_mark_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<(i64, LatestRunMark)> {
    Ok((
        row.get(0)?,
        LatestRunMark {
            id: row.get(1)?,
            state_type: row.get(2)?,
            state_name: row.get(3)?,
            state_details: crate::row::json_map(row.get::<_, Option<String>>(4)?),
        },
    ))
}

fn chain_checkpoints(conn: &Connection, chain: &str) -> Result<Vec<Checkpoint>> {
    let mut stmt = conn.prepare_cached(
        "SELECT t.dynamic_key, t.task_key, t.input_hash, t.result_ref, t.run_id, t.pass
         FROM task_run t
         WHERE t.run_id IN (SELECT CAST(value AS INTEGER) FROM json_each(?1))
           AND t.state_type = 'Completed'
           AND t.result_ref IS NOT NULL AND t.input_hash IS NOT NULL
         ORDER BY t.run_id, t.pass, t.id",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![chain], |r| {
            Ok(Checkpoint {
                dynamic_key: r.get(0)?,
                task_key: r.get(1)?,
                input_hash: r.get(2)?,
                result_ref: r.get(3)?,
                run_id: r.get(4)?,
                pass: r.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// The replay seeds of a crash chain, keyed `run.checkpoints:<id>`, in one read.
fn chain_seeds(
    conn: &Connection,
    chain: &[i64],
) -> Result<std::collections::HashMap<String, String>> {
    let keys: Vec<String> = chain.iter().map(|id| checkpoint_seed_key(*id)).collect();
    let json = serde_json::to_string(&keys).unwrap_or_else(|_| "[]".to_string());
    let mut stmt = conn.prepare_cached(
        "SELECT key, value FROM kv WHERE key IN (SELECT value FROM json_each(?1))",
    )?;
    let mut found: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for r in stmt.query_map(rusqlite::params![json], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })? {
        let (k, v) = r?;
        found.insert(k, v);
    }
    Ok(found)
}

/// A run-id list as a JSON array, for binding into a fixed `json_each` query.
///
/// Lets a statement take a variable-length set as one parameter, so the SQL text
/// stays constant and the prepared statement stays cacheable.
fn chain_json(ids: &[i64]) -> String {
    let mut out = String::with_capacity(ids.len() * 4 + 2);
    out.push('[');
    for (i, id) in ids.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&id.to_string());
    }
    out.push(']');
    out
}

/// A completed task run's stored result, keyed for replay.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Checkpoint {
    pub dynamic_key: String,
    pub task_key: String,
    pub input_hash: String,
    pub result_ref: String,
    pub run_id: i64,
    pub pass: i64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
pub struct ListRunsFilter {
    #[serde(default)]
    pub project: Option<String>,
    /// The resolved group: the one the flow declared, else its project.
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub flow: Option<String>,
    #[serde(default)]
    pub flow_id: Option<i64>,
    #[serde(default)]
    pub state_type: Option<String>,
    #[serde(default)]
    pub state_name: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    /// Tags the run must carry; a query string may pass them comma-separated.
    #[serde(default, deserialize_with = "list_or_csv")]
    #[cfg_attr(feature = "openapi", param(value_type = Option<String>))]
    pub tags: Vec<String>,
    /// `key=value` pairs the run's parameters must match (comma-separated in a query string).
    /// Without a start bound, the search covers the last 30 days.
    #[serde(default, deserialize_with = "list_or_csv")]
    #[cfg_attr(feature = "openapi", param(value_type = Option<String>))]
    pub params: Vec<String>,
    /// `key=value` pairs the run's attributes must match, as `params`.
    #[serde(default, deserialize_with = "list_or_csv")]
    #[cfg_attr(feature = "openapi", param(value_type = Option<String>))]
    pub attributes: Vec<String>,
    /// Inclusive lower bound on the run's start time (or creation when never started), microseconds.
    #[serde(default)]
    pub start_after: Option<i64>,
    #[serde(default)]
    pub start_before: Option<i64>,
    /// `created_desc` (default), `created_asc`, `start_desc`, `start_asc`, `duration_desc`, `duration_asc`, `name_asc`.
    #[serde(default)]
    pub sort: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
    /// Keyset cursor: the id of the last run of the previous page (id-ordered sorts only).
    #[serde(default)]
    pub cursor: Option<i64>,
    #[serde(default)]
    pub schedule_id: Option<i64>,
    #[serde(default)]
    pub backfill_id: Option<i64>,
    /// Only runs with a scheduled time after this (upcoming lists).
    #[serde(default)]
    pub scheduled_after: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RunsPage {
    pub items: Vec<Run>,
    pub next_cursor: Option<i64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
pub struct ListTaskRunsFilter {
    #[serde(default)]
    pub run_id: Option<i64>,
    /// Only task runs of this execution of the run's body.
    #[serde(default)]
    pub pass: Option<i64>,
    #[serde(default)]
    pub project: Option<String>,
    /// The resolved group: the one the flow declared, else its project.
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub flow: Option<String>,
    #[serde(default)]
    pub state_type: Option<String>,
    #[serde(default)]
    pub state_name: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub start_after: Option<i64>,
    #[serde(default)]
    pub start_before: Option<i64>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub cursor: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TaskRunsPage {
    pub items: Vec<TaskRun>,
    pub next_cursor: Option<i64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
pub struct LogFilter {
    #[serde(default)]
    pub run_id: Option<i64>,
    #[serde(default)]
    pub task_run_id: Option<i64>,
    #[serde(default)]
    pub after_id: Option<i64>,
    /// Minimum level, Python numeric levels (10 debug, 20 info, 30 warning, 40 error, 50 critical).
    #[serde(default)]
    pub min_level: Option<i32>,
    #[serde(default)]
    pub search: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct LogsPage {
    pub items: Vec<Log>,
    pub next_cursor: Option<i64>,
}

fn list_or_csv<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Either {
        List(Vec<String>),
        Text(String),
    }
    Ok(match Option::<Either>::deserialize(d)? {
        None => Vec::new(),
        Some(Either::List(v)) => v,
        Some(Either::Text(t)) => t
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
    })
}

struct Query {
    clauses: Vec<String>,
    args: Vec<SqlValue>,
}

impl Query {
    fn new() -> Query {
        Query {
            clauses: Vec::new(),
            args: Vec::new(),
        }
    }
    fn push(&mut self, clause: &str, v: SqlValue) {
        self.args.push(v);
        self.clauses
            .push(clause.replace('?', &format!("?{}", self.args.len())));
    }
    fn where_sql(&self) -> String {
        if self.clauses.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", self.clauses.join(" AND "))
        }
    }
}

/// Batch keys are interpolated into a JSON path: identifiers only.
fn valid_param_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 64
        && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !key.starts_with(|c: char| c.is_ascii_digit())
}

/// Non-terminal state types as a SQL list; `IN` over these uses the state index.
fn active_list() -> String {
    StateType::ALL
        .iter()
        .filter(|t| !t.is_terminal())
        .map(|t| format!("'{}'", t.as_str()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// `(run id, state type, state name, duration in microseconds)` of a recent run.
/// A queued run, as the queue view needs it: the nine fields it shows. See
/// [`Store::queue_runs`].
#[derive(Clone, Debug, PartialEq)]
pub struct QueueRun {
    pub id: i64,
    pub name: String,
    pub flow_name: String,
    pub project: String,
    pub state_name: String,
    pub created_by: String,
    pub backfill_id: Option<i64>,
    pub schedule_id: Option<i64>,
    pub scheduled_time: Option<i64>,
}

pub type RecentRun = (i64, String, String, Option<i64>);

/// The columns behind [`Store::queue_runs`], in the order its decoder reads them.
///
/// `state_type` is carried only so `state_name` can fall back to it, the way
/// `state_from_columns` does. Both are plain columns; neither is JSON, and the
/// `task_counts` aggregate that `RUN_COLUMNS` carries is deliberately absent.
const QUEUE_RUN_COLUMNS: &str = "r.id, r.name, f.name, f.project, r.state_name, r.state_type, \
     r.created_by, r.backfill_id, r.schedule_id, r.scheduled_time";

fn queue_run_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<QueueRun> {
    Ok(QueueRun {
        id: row.get(0)?,
        name: row.get(1)?,
        flow_name: row.get(2)?,
        project: row.get(3)?,
        // Coalesced the way `state_from_columns` does, so a run that has not
        // written a state name yet reads the same here as it does whole.
        state_name: row
            .get::<_, Option<String>>(4)?
            .or_else(|| row.get::<_, Option<String>>(5).ok().flatten())
            .unwrap_or_else(|| StateType::Scheduled.as_str().to_string()),
        created_by: row.get(6)?,
        backfill_id: row.get(7)?,
        schedule_id: row.get(8)?,
        scheduled_time: row.get(9)?,
    })
}

/// A run as a timeline row shows it: id, flow name, and when it is due.
///
/// A timeline lists the runs a host might take next, and reads four fields of
/// each. `RUN_COLUMNS` — 37 columns, a correlated `task_counts` aggregate
/// evaluated once per row, four decoded JSON documents — is what the whole
/// reader costs for those four.
#[derive(Clone, Debug, PartialEq)]
pub struct TimelineRunRow {
    pub id: i64,
    /// The flow's name, which the row displays.
    pub flow_name: String,
    /// Needed to decide whether the flow may run on the worker at all, which is
    /// why it is here rather than being looked up again.
    pub flow_id: i64,
    pub scheduled_time: Option<i64>,
    pub created_at: i64,
}

/// The columns behind [`Store::timeline_run_rows`], in the order its decoder
/// reads them. No aggregate, and no JSON column the row does not need.
const TIMELINE_RUN_COLUMNS: &str = "r.id, f.name, r.flow_id, r.scheduled_time, r.created_at";

/// The newest `limit` runs of one flow, newest first.
///
/// `run_flow_id (flow_id, id)` serves both the filter and the order, so this is a
/// seek that stops after `limit` rows rather than a walk of the flow's history.
/// The `COALESCE`s are unchanged from the batched form they replace: a run that has
/// not been transitioned yet stores a NULL state, and the reader has always
/// reported those as Scheduled.
const SQL_RECENT_RUN_STATES: &str =
    "SELECT id, COALESCE(state_type, 'Scheduled'), COALESCE(state_name, 'Scheduled'), total_run_time \
     FROM run WHERE flow_id = ?1 ORDER BY id DESC LIMIT ?2";

/// The end time of one flow's newest completed run.
///
/// `LIMIT 1` over the completed runs only. The filter is `COALESCE(state_type,
/// 'Completed') = 'Completed'` for the same reason the other readers coalesce: a
/// run that has not been transitioned yet has a NULL state, and this reader has
/// always counted it as completed.
///
/// **The `LIMIT 1` is over completed runs, not over all runs.** A flow whose newest
/// completed run has no end time is absent from the answer -- the reader does not
/// fall through to an older completed run that has one. Selecting the newest
/// completed run and then discarding a null end time is what preserves that, and it
/// is why this is a per-flow seek with its own `LIMIT` rather than a
/// `COALESCE(end_time, ...)` in SQL.
/// The newest `limit` runs of one flow that have a recorded duration.
///
/// The filter is `total_run_time IS NOT NULL` in SQL **and** in Rust. It has to be
/// in both: the SQL one keeps the `LIMIT` meaningful for rows that do have a
/// duration, and the Rust one drops the nulls the seek necessarily walks over.
/// Filtering only in SQL would make the `LIMIT` count rows the caller then
/// discards; filtering only in Rust would make the seek walk the flow's whole
/// history looking for `MEDIAN_SAMPLE` durations that may not exist.
const SQL_MEDIAN_SAMPLE: &str = "SELECT total_run_time FROM run \
     WHERE flow_id = ?1 AND total_run_time IS NOT NULL \
     ORDER BY id DESC LIMIT ?2";

const SQL_LAST_COMPLETED_AT: &str = "SELECT end_time FROM run \
     WHERE flow_id = ?1 AND COALESCE(state_type, 'Completed') = 'Completed' \
     ORDER BY id DESC LIMIT 1";

fn timeline_run_row_from(row: &rusqlite::Row<'_>) -> rusqlite::Result<TimelineRunRow> {
    Ok(TimelineRunRow {
        id: row.get(0)?,
        flow_name: row.get(1)?,
        flow_id: row.get(2)?,
        scheduled_time: row.get(3)?,
        created_at: row.get(4)?,
    })
}

/// A flow's identity, for labelling metric series. See [`Store::flow_labels`].
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FlowLabel {
    pub id: i64,
    pub project: String,
    pub name: String,
}

/// One of a schedule's future runs, as the fire path needs it. See
/// [`Store::future_run_marks`].
#[derive(Clone, Debug, PartialEq)]
pub struct ScheduleRunMark {
    pub id: i64,
    pub scheduled_time: Option<i64>,
    pub details: serde_json::Map<String, serde_json::Value>,
}

/// The run fields an event record is built from. See
/// [`Store::run_event_context`].
#[derive(Clone, Debug, PartialEq)]
pub struct RunEventContext {
    pub external_id: cereyan_core::Id,
    pub name: String,
    pub project: String,
    pub flow_name: String,
    pub tags: Vec<String>,
    pub state_name: String,
}

impl Store {
    pub fn list_runs(&self, filter: &ListRunsFilter) -> Result<RunsPage> {
        let limit = filter.limit.unwrap_or(50).clamp(1, 500);
        let mut q = Query::new();
        if let Some(p) = &filter.project {
            q.push("f.project = ?", SqlValue::Text(p.clone()));
        }
        if let Some(g) = &filter.group {
            q.push(
                "COALESCE(f.flow_group, f.project) = ?",
                SqlValue::Text(g.clone()),
            );
        }
        if let Some(f) = &filter.flow {
            q.push("f.name = ?", SqlValue::Text(f.clone()));
        }
        if let Some(id) = filter.flow_id {
            q.push("r.flow_id = ?", SqlValue::Integer(id));
        }
        if let Some(t) = &filter.state_type {
            q.push("r.state_type = ?", SqlValue::Text(t.clone()));
        }
        if let Some(n) = &filter.state_name {
            q.push("r.state_name = ?", SqlValue::Text(n.clone()));
        }
        if let Some(n) = &filter.name {
            q.push("r.name LIKE ?", SqlValue::Text(format!("%{n}%")));
        }
        for tag in &filter.tags {
            let encoded = serde_json::to_string(tag).unwrap_or_default();
            q.push("r.tags LIKE ?", SqlValue::Text(format!("%{encoded}%")));
        }
        // JSON searches walk rows, so they are bounded to a month unless the caller bounds them.
        let mut searched = false;
        for (column, entries) in [
            ("parameters", &filter.params),
            ("attributes", &filter.attributes),
        ] {
            for entry in entries {
                let Some((key, value)) = entry.split_once('=') else {
                    continue;
                };
                let key = key.trim();
                if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                    continue;
                }
                searched = true;
                q.push(
                    &format!("CAST(json_extract(r.{column}, '$.{key}') AS TEXT) = ?"),
                    SqlValue::Text(value.trim().to_string()),
                );
            }
        }
        if let Some(t) = filter.start_after {
            q.push(
                "COALESCE(r.start_time, r.created_at) >= ?",
                SqlValue::Integer(t),
            );
        } else if searched {
            q.push(
                "COALESCE(r.start_time, r.created_at) >= ?",
                SqlValue::Integer(cereyan_core::now_micros() - 30 * 86_400 * 1_000_000),
            );
        }
        if let Some(t) = filter.start_before {
            q.push(
                "COALESCE(r.start_time, r.created_at) <= ?",
                SqlValue::Integer(t),
            );
        }
        if let Some(id) = filter.schedule_id {
            q.push("r.schedule_id = ?", SqlValue::Integer(id));
        }
        if let Some(id) = filter.backfill_id {
            q.push("r.backfill_id = ?", SqlValue::Integer(id));
        }
        if let Some(t) = filter.scheduled_after {
            q.push("r.scheduled_time >= ?", SqlValue::Integer(t));
        }
        let sort = filter.sort.as_deref().unwrap_or("created_desc");
        let (order, keyset) = match sort {
            "created_asc" => ("r.id ASC", Some(">")),
            "start_desc" => ("COALESCE(r.start_time, r.created_at) DESC, r.id DESC", None),
            "start_asc" => ("COALESCE(r.start_time, r.created_at) ASC, r.id ASC", None),
            "duration_desc" => ("r.total_run_time DESC, r.id DESC", None),
            "duration_asc" => ("r.total_run_time ASC, r.id DESC", None),
            "name_asc" => ("r.name ASC, r.id DESC", None),
            "scheduled_asc" => ("r.scheduled_time ASC, r.id ASC", None),
            _ => ("r.id DESC", Some("<")),
        };
        if let (Some(c), Some(op)) = (filter.cursor, keyset) {
            q.push(&format!("r.id {op} ?"), SqlValue::Integer(c));
        }
        let sql = format!(
            "SELECT {RUN_COLUMNS} FROM run r JOIN flow f ON f.id = r.flow_id {} ORDER BY {order} LIMIT {}",
            q.where_sql(),
            limit + 1
        );
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(&sql)?;
            let rows = stmt
                .query_map(params_from_iter(q.args.iter()), run_from_row)?
                .collect::<rusqlite::Result<Vec<Run>>>()?;
            let mut items = rows;
            let next_cursor = if items.len() > limit {
                items.truncate(limit);
                if keyset.is_some() {
                    items.last().map(|r| r.id)
                } else {
                    None
                }
            } else {
                None
            };
            Ok(RunsPage { items, next_cursor })
        })
    }

    /// The run fields an event record needs: its external id, name, the flow's
    /// project and name, its tags, and its state name.
    ///
    /// `record_event` runs on every event the server stores and reads only these
    /// six values, so it takes this projection rather than a whole `Run` — which
    /// would mean a correlated `task_counts` aggregate and four JSON columns
    /// decoded to keep one string.
    pub fn run_event_context(&self, run_id: i64) -> Result<Option<RunEventContext>> {
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT r.external_id, r.name, f.project, f.name, r.tags,
                        COALESCE(r.state_name, COALESCE(r.state_type, 'Scheduled'))
                 FROM run r JOIN flow f ON f.id = r.flow_id
                 WHERE r.id = ?1",
            )?;
            let ctx = stmt
                .query_row([run_id], |row| {
                    let external: Vec<u8> = row.get(0)?;
                    Ok(RunEventContext {
                        external_id: Id::from_bytes(&external).unwrap_or_else(cereyan_core::new_id),
                        name: row.get(1)?,
                        project: row.get(2)?,
                        flow_name: row.get(3)?,
                        tags: crate::row::json_list(row.get(4)?),
                        state_name: row.get(5)?,
                    })
                })
                .optional()?;
            Ok(ctx)
        })
    }

    /// A run's parameters, on their own.
    ///
    /// For callers that need the parameters and nothing else. `get_run` expands
    /// `RUN_COLUMNS` — 37 columns, a correlated `task_counts` aggregate, a join
    /// to `flow` — and decodes four JSON columns, where this reads one.
    ///
    /// Decoded leniently, through the same `json_map` the whole reader uses, so a
    /// malformed or absent column yields an empty map rather than an error.
    pub fn run_parameters(
        &self,
        run_id: i64,
    ) -> Result<Option<serde_json::Map<String, serde_json::Value>>> {
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached("SELECT parameters FROM run WHERE id = ?1")?;
            let got: Option<Option<String>> =
                stmt.query_row([run_id], |row| row.get(0)).optional()?;
            Ok(got.map(crate::row::json_map))
        })
    }

    pub fn get_run(&self, run_id: i64) -> Result<Option<Run>> {
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(&SQL_GET_RUN)?;
            Ok(stmt.query_row([run_id], run_from_row).optional()?)
        })
    }

    /// The given runs, projected to what the queue view shows.
    ///
    /// [`get_runs`] expands `RUN_COLUMNS` — 37 columns including a correlated
    /// `task_counts` aggregate that SQLite evaluates once per row — and decodes
    /// four JSON columns. The queue view reads nine plain columns, for up to 550
    /// runs, on a page polled every five seconds. Neither the aggregate nor the
    /// JSON is wanted here.
    pub fn queue_runs(&self, run_ids: &[i64]) -> Result<Vec<QueueRun>> {
        if run_ids.is_empty() {
            return Ok(Vec::new());
        }
        let ids: Vec<String> = run_ids.iter().map(|id| id.to_string()).collect();
        let sql = format!(
            "SELECT {QUEUE_RUN_COLUMNS} FROM run r JOIN flow f ON f.id = r.flow_id \
             WHERE r.id IN ({})",
            ids.join(",")
        );
        self.with_reader(|conn| {
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt
                .query_map([], queue_run_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// Scheduled runs due in the window, projected to what the queue view shows.
    ///
    /// The narrow counterpart of [`scheduled_between`], which keeps its other
    /// caller. Same `WHERE`, same order, same limit.
    pub fn scheduled_queue_runs(
        &self,
        after: i64,
        until: i64,
        limit: usize,
    ) -> Result<Vec<QueueRun>> {
        let sql = format!(
            "SELECT {QUEUE_RUN_COLUMNS} FROM run r JOIN flow f ON f.id = r.flow_id \
             WHERE r.state_type = 'Scheduled' AND r.scheduled_time > ?1 AND r.scheduled_time <= ?2 \
             ORDER BY r.scheduled_time, r.id LIMIT ?3"
        );
        self.with_reader(|conn| {
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt
                .query_map(
                    rusqlite::params![after, until, limit as i64],
                    queue_run_from_row,
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    pub fn get_runs(&self, run_ids: &[i64]) -> Result<Vec<Run>> {
        if run_ids.is_empty() {
            return Ok(Vec::new());
        }
        let ids: Vec<String> = run_ids.iter().map(|id| id.to_string()).collect();
        let sql = format!(
            "SELECT {RUN_COLUMNS} FROM run r JOIN flow f ON f.id = r.flow_id WHERE r.id IN ({})",
            ids.join(",")
        );
        self.with_reader(|conn| {
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt
                .query_map([], run_from_row)?
                .collect::<rusqlite::Result<Vec<Run>>>()?;
            Ok(rows)
        })
    }

    /// Scheduled runs due after `after` and at or before `until`, soonest first.
    pub fn scheduled_between(&self, after: i64, until: i64, limit: usize) -> Result<Vec<Run>> {
        let sql = format!(
            "SELECT {RUN_COLUMNS} FROM run r JOIN flow f ON f.id = r.flow_id
             WHERE r.state_type = 'Scheduled' AND r.scheduled_time > ?1 AND r.scheduled_time <= ?2
             ORDER BY r.scheduled_time, r.id LIMIT ?3"
        );
        self.with_reader(|conn| {
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt
                .query_map(rusqlite::params![after, until, limit as i64], run_from_row)?
                .collect::<rusqlite::Result<Vec<Run>>>()?;
            Ok(rows)
        })
    }

    pub fn get_run_by_external_id(&self, id: &Id) -> Result<Option<Run>> {
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(&SQL_GET_RUN_BY_EXTERNAL_ID)?;
            Ok(stmt
                .query_row([id.as_bytes().as_slice()], run_from_row)
                .optional()?)
        })
    }

    /// Runs that are not in a terminal state, oldest first.
    pub fn active_runs(&self) -> Result<Vec<Run>> {
        let sql = format!(
            "SELECT {RUN_COLUMNS} FROM run r JOIN flow f ON f.id = r.flow_id
             WHERE r.state_type IS NULL OR r.state_type IN ({}) ORDER BY r.id",
            active_list()
        );
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(&sql)?;
            let rows = stmt
                .query_map([], run_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// Run counts by (flow_id, state_type).
    pub fn run_counts(&self) -> Result<Vec<(i64, String, i64)>> {
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT flow_id, COALESCE(state_type, 'Scheduled'), COUNT(*) FROM run GROUP BY flow_id, state_type",
            )?;
            let rows = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// Task run counts by state name (Skipped and Cached count separately).
    pub fn task_run_counts(&self) -> Result<Vec<(String, i64)>> {
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT COALESCE(state_name, 'Pending'), COUNT(*) FROM task_run GROUP BY state_name",
            )?;
            let rows = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// Task runs of one run, oldest first: every pass, or only the one given.
    pub fn task_runs_by_run(&self, run_id: i64, pass: Option<i64>) -> Result<Vec<TaskRun>> {
        let clause = if pass.is_some() {
            " AND t.pass = ?2"
        } else {
            ""
        };
        let sql = format!(
            "SELECT {TASK_RUN_COLUMNS} FROM {TASK_RUN_FROM} WHERE t.run_id = ?1{clause} ORDER BY t.id"
        );
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(&sql)?;
            let rows = match pass {
                Some(p) => stmt
                    .query_map(rusqlite::params![run_id, p], task_run_from_row)?
                    .collect::<rusqlite::Result<Vec<_>>>()?,
                None => stmt
                    .query_map([run_id], task_run_from_row)?
                    .collect::<rusqlite::Result<Vec<_>>>()?,
            };
            Ok(rows)
        })
    }

    /// The pass the next execution of this run's body belongs to: one more than
    /// the highest recorded, or 0 when the run has no task runs. A pass that
    /// recorded nothing leaves no trace and its number is used again, which is
    /// harmless because it has no rows to collide with.
    pub fn next_pass(&self, run_id: i64) -> Result<i64> {
        self.with_reader(|conn| {
            let next = conn.query_row(
                "SELECT COALESCE(MAX(pass), -1) + 1 FROM task_run WHERE run_id = ?1",
                [run_id],
                |r| r.get(0),
            )?;
            Ok(next)
        })
    }

    pub fn list_task_runs(&self, filter: &ListTaskRunsFilter) -> Result<TaskRunsPage> {
        let limit = filter.limit.unwrap_or(50).clamp(1, 500);
        let mut q = Query::new();
        if let Some(id) = filter.run_id {
            q.push("t.run_id = ?", SqlValue::Integer(id));
        }
        if let Some(p) = filter.pass {
            q.push("t.pass = ?", SqlValue::Integer(p));
        }
        if let Some(p) = &filter.project {
            q.push("f.project = ?", SqlValue::Text(p.clone()));
        }
        if let Some(g) = &filter.group {
            q.push(
                "COALESCE(f.flow_group, f.project) = ?",
                SqlValue::Text(g.clone()),
            );
        }
        if let Some(f) = &filter.flow {
            q.push("f.name = ?", SqlValue::Text(f.clone()));
        }
        if let Some(t) = &filter.state_type {
            q.push("t.state_type = ?", SqlValue::Text(t.clone()));
        }
        if let Some(n) = &filter.state_name {
            q.push("t.state_name = ?", SqlValue::Text(n.clone()));
        }
        if let Some(n) = &filter.name {
            q.push("t.name LIKE ?", SqlValue::Text(format!("%{n}%")));
        }
        if let Some(ts) = filter.start_after {
            q.push(
                "COALESCE(t.start_time, t.created_at) >= ?",
                SqlValue::Integer(ts),
            );
        }
        if let Some(ts) = filter.start_before {
            q.push(
                "COALESCE(t.start_time, t.created_at) <= ?",
                SqlValue::Integer(ts),
            );
        }
        if let Some(c) = filter.cursor {
            q.push("t.id < ?", SqlValue::Integer(c));
        }
        let sql = format!(
            "SELECT {TASK_RUN_COLUMNS} FROM {TASK_RUN_FROM} {} ORDER BY t.id DESC LIMIT {}",
            q.where_sql(),
            limit + 1
        );
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(&sql)?;
            let mut items = stmt
                .query_map(params_from_iter(q.args.iter()), task_run_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let next_cursor = if items.len() > limit {
                items.truncate(limit);
                items.last().map(|t| t.id)
            } else {
                None
            };
            Ok(TaskRunsPage { items, next_cursor })
        })
    }

    pub fn get_task_run(&self, id: i64) -> Result<Option<TaskRun>> {
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(&SQL_GET_TASK_RUN)?;
            Ok(stmt.query_row([id], task_run_from_row).optional()?)
        })
    }

    pub fn logs(&self, filter: &LogFilter) -> Result<LogsPage> {
        let limit = filter.limit.unwrap_or(1000).clamp(1, 10_000);
        let mut q = Query::new();
        if let Some(id) = filter.run_id {
            q.push("run_id = ?", SqlValue::Integer(id));
        }
        if let Some(id) = filter.task_run_id {
            q.push("task_run_id = ?", SqlValue::Integer(id));
        }
        if let Some(after) = filter.after_id {
            q.push("id > ?", SqlValue::Integer(after));
        }
        if let Some(level) = filter.min_level {
            q.push("level >= ?", SqlValue::Integer(level as i64));
        }
        if let Some(s) = &filter.search {
            q.push("message LIKE ?", SqlValue::Text(format!("%{s}%")));
        }
        let sql = format!(
            "SELECT id, run_id, task_run_id, level, logger, timestamp, message FROM log {} ORDER BY id LIMIT {}",
            q.where_sql(),
            limit + 1
        );
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(&sql)?;
            let mut items = stmt
                .query_map(params_from_iter(q.args.iter()), |r| {
                    Ok(Log {
                        id: r.get(0)?,
                        run_id: r.get(1)?,
                        task_run_id: r.get(2)?,
                        level: r.get(3)?,
                        logger: r.get(4)?,
                        timestamp: r.get(5)?,
                        message: r.get(6)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let next_cursor = if items.len() > limit {
                items.truncate(limit);
                items.last().map(|l| l.id)
            } else {
                None
            };
            Ok(LogsPage { items, next_cursor })
        })
    }

    pub fn logs_by_run(&self, run_id: i64, after_id: i64, limit: usize) -> Result<Vec<Log>> {
        Ok(self
            .logs(&LogFilter {
                run_id: Some(run_id),
                after_id: Some(after_id),
                limit: Some(limit),
                ..Default::default()
            })?
            .items)
    }

    /// Every flow's id, project and name.
    ///
    /// For labelling metric series, which need three columns and nothing else.
    /// `list_flows` selects fourteen, three of them JSON — `tags`,
    /// `parameter_schema` and `options` — so a scrape through it parses a
    /// schema, an options object and a tag array for every registered flow only
    /// to emit a label pair. Four callers of `list_flows` genuinely need those
    /// columns; this one does not, so it takes its own reader.
    pub fn flow_labels(&self) -> Result<Vec<FlowLabel>> {
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached("SELECT id, project, name FROM flow")?;
            let rows = stmt
                .query_map([], |row| {
                    Ok(FlowLabel {
                        id: row.get(0)?,
                        project: row.get(1)?,
                        name: row.get(2)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    pub fn list_flows(&self, project: Option<&str>) -> Result<Vec<Flow>> {
        self.list_flows_filtered(project, None)
    }

    /// Flows narrowed by project and by resolved group. A `NULL` `flow_group`
    /// reads as the flow's project, so a group equal to a project name selects
    /// that project's flows that declared none, matching what the API returns.
    pub fn list_flows_filtered(
        &self,
        project: Option<&str>,
        group: Option<&str>,
    ) -> Result<Vec<Flow>> {
        let sql = format!(
            "SELECT {FLOW_COLUMNS} FROM flow \
             WHERE (?1 IS NULL OR project = ?1) \
               AND (?2 IS NULL OR COALESCE(flow_group, project) = ?2) \
             ORDER BY project, name"
        );
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(&sql)?;
            let rows = stmt
                .query_map([project, group], flow_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    pub fn get_flow(&self, id: i64) -> Result<Option<Flow>> {
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(&SQL_GET_FLOW)?;
            Ok(stmt.query_row([id], flow_from_row).optional()?)
        })
    }

    pub fn get_flow_by_key(&self, project: &str, name: &str) -> Result<Option<Flow>> {
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(&SQL_GET_FLOW_BY_KEY)?;
            Ok(stmt.query_row([project, name], flow_from_row).optional()?)
        })
    }

    /// Recent runs of a flow, newest first, limited (for the flows page dots).
    /// The checkpoint map a new attempt of `run_id` replays from: for every
    /// dynamic key, the latest completed task run with a stored result, over
    /// the run's own passes and the chain of crashed runs it reruns.
    pub fn checkpoints(&self, run_id: i64) -> Result<Vec<Checkpoint>> {
        self.with_reader(|conn| {
            // Walk the crash chain once; both follow-up queries use this list
            // rather than evaluating the recursive CTE again.
            let mut chain: Vec<i64> = Vec::new();
            {
                let mut walk = conn.prepare_cached(
                    "WITH RECURSIVE chain(id) AS (
                        SELECT ?1
                        UNION ALL
                        SELECT r.parent_run_id FROM run r JOIN chain ON r.id = chain.id
                        WHERE r.parent_run_id IS NOT NULL AND r.created_by LIKE 'crash:%'
                     ) SELECT id FROM chain",
                )?;
                for id in walk.query_map(rusqlite::params![run_id], |r| r.get::<_, i64>(0))? {
                    chain.push(id?);
                }
            }
            if chain.is_empty() {
                return Ok(Vec::new());
            }

            let rows = chain_checkpoints(conn, &chain_json(&chain))?;
            let seeds = chain_seeds(conn, &chain)?;

            let mut latest: std::collections::HashMap<String, Checkpoint> =
                std::collections::HashMap::new();
            // Seeds of the whole chain go first, oldest run first, so a run's own
            // completed work wins over its seed.
            for id in chain.iter().rev() {
                if let Some(seed) = seeds.get(&checkpoint_seed_key(*id)) {
                    for cp in serde_json::from_str::<Vec<Checkpoint>>(seed).unwrap_or_default() {
                        latest.insert(cp.dynamic_key.clone(), cp);
                    }
                }
            }
            // Later passes and later runs of the chain win: keep the last per key.
            for cp in rows {
                latest.insert(cp.dynamic_key.clone(), cp);
            }
            let mut out: Vec<Checkpoint> = latest.into_values().collect();
            out.sort_by(|a, b| a.dynamic_key.cmp(&b.dynamic_key));
            Ok(out)
        })
    }

    /// Recent runs of several flows at once, keyed by flow id, newest first
    /// within each flow and capped at `limit` per flow.
    ///
    /// The per-flow cap is a window function, since a per-group row limit is not
    /// expressible with `GROUP BY`. The ids go in as one JSON array so the
    /// statement text is the same whatever the number of flows.
    pub fn recent_run_states_many(
        &self,
        flow_ids: &[i64],
        limit: usize,
    ) -> Result<std::collections::HashMap<i64, Vec<RecentRun>>> {
        let mut out: std::collections::HashMap<i64, Vec<RecentRun>> = Default::default();
        if flow_ids.is_empty() {
            return Ok(out);
        }
        // One seek per flow, not one ranked pass over every flow's whole history.
        //
        // The window function this replaces had no bound on the partition, so
        // SQLite had to assign a row number to every run of every listed flow
        // before the outer `rn <= ?2` could throw them away. `run_flow_id
        // (flow_id, id)` already orders each flow's runs by id, so a per-flow
        // `ORDER BY id DESC LIMIT ?` is a seek that stops after `limit` rows.
        //
        // The statement is `prepare_cached`, so the flow count costs executions of
        // one compiled statement rather than one compilation each. See
        // `per_flow_window_reads` in design.md for the measurement, including the
        // correlated-`LIMIT` rewrite that was tried first and measured *slower*.
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(SQL_RECENT_RUN_STATES)?;
            for flow_id in flow_ids {
                let rows = stmt
                    .query_map(rusqlite::params![flow_id, limit as i64], |r| {
                        let run: RecentRun = (r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?);
                        Ok(run)
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                if !rows.is_empty() {
                    out.insert(*flow_id, rows);
                }
            }
            Ok(out)
        })
    }

    /// The end time of each flow's most recent completed run.
    ///
    /// Projects only `flow_id` and `end_time`: the caller wants a timestamp, so
    /// the full run projection — including the per-row `task_counts` aggregate
    /// and the join to `flow` — would be work it never reads. A flow whose newest
    /// completed run has no end time is absent, matching a caller that took
    /// `and_then` over `end_time`.
    pub fn last_completed_at_many(
        &self,
        flow_ids: &[i64],
    ) -> Result<std::collections::HashMap<i64, i64>> {
        let mut out: std::collections::HashMap<i64, i64> = Default::default();
        if flow_ids.is_empty() {
            return Ok(out);
        }
        // A seek per flow rather than a ranked pass. `run_state_id` is not usable
        // here -- the filter is on `state_type` and the order is by `id` -- but
        // `run_flow_id (flow_id, id)` is, and the per-flow `LIMIT 1` is what makes
        // it stop at the newest completed run instead of walking back through
        // every scheduled one.
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(SQL_LAST_COMPLETED_AT)?;
            for flow_id in flow_ids {
                // `optional`, because a flow with no completed run yields no row
                // at all rather than a row of NULLs.
                let end_time: Option<Option<i64>> = stmt
                    .query_row(rusqlite::params![flow_id], |r| r.get(0))
                    .optional()?;
                // A completed run with no end time is not a completion time, so
                // the flow is left absent rather than mapped to nothing.
                if let Some(Some(at)) = end_time {
                    out.insert(*flow_id, at);
                }
            }
            Ok(out)
        })
    }

    pub fn recent_run_states(&self, flow_id: i64, limit: usize) -> Result<Vec<RecentRun>> {
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT id, COALESCE(state_type, 'Scheduled'), COALESCE(state_name, 'Scheduled'), total_run_time
                 FROM run WHERE flow_id = ?1 ORDER BY id DESC LIMIT ?2",
            )?;
            let rows = stmt
                .query_map(rusqlite::params![flow_id, limit as i64], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    pub fn list_schedules(&self, flow_id: Option<i64>) -> Result<Vec<ScheduleRow>> {
        let sql = format!(
            "SELECT {SCHEDULE_COLUMNS} FROM schedule WHERE (?1 IS NULL OR flow_id = ?1) ORDER BY id"
        );
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(&sql)?;
            let rows = stmt
                .query_map([flow_id], schedule_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    pub fn get_schedule(&self, id: i64) -> Result<Option<ScheduleRow>> {
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(&SQL_GET_SCHEDULE)?;
            Ok(stmt.query_row([id], schedule_from_row).optional()?)
        })
    }

    /// Skipped fire times of a schedule, ascending.
    pub fn list_skips(&self, schedule_id: i64) -> Result<Vec<i64>> {
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT fire_time FROM schedule_skip WHERE schedule_id = ?1 ORDER BY fire_time",
            )?;
            let rows = stmt
                .query_map([schedule_id], |r| r.get::<_, i64>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// Skips of a schedule with when and by whom each was made:
    /// `(fire_time, created_at, created_by)`, ascending by fire time.
    /// The skip rows of several schedules at once, grouped by schedule id and
    /// ordered by fire time within each.
    ///
    /// The ids go in as one JSON array so the statement text is the same however
    /// many schedules there are, which keeps it in the prepared statement cache.
    /// A schedule with no skips has no entry, matching `list_skip_rows`, which
    /// returns nothing for one.
    pub fn list_skip_rows_many(&self, schedule_ids: &[i64]) -> Result<SkipRowsBySchedule> {
        let mut out: SkipRowsBySchedule = Default::default();
        if schedule_ids.is_empty() {
            return Ok(out);
        }
        let list = serde_json::to_string(schedule_ids).unwrap_or_else(|_| "[]".to_string());
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT schedule_id, fire_time, created_at, created_by FROM schedule_skip
                 WHERE schedule_id IN (SELECT CAST(value AS INTEGER) FROM json_each(?1))
                 ORDER BY schedule_id, fire_time",
            )?;
            for r in stmt.query_map(rusqlite::params![list], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })? {
                let (schedule_id, fire, at, by) = r?;
                out.entry(schedule_id).or_default().push((fire, at, by));
            }
            Ok(out)
        })
    }

    pub fn list_skip_rows(&self, schedule_id: i64) -> Result<Vec<(i64, i64, String)>> {
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT fire_time, created_at, created_by FROM schedule_skip
                 WHERE schedule_id = ?1 ORDER BY fire_time",
            )?;
            let rows = stmt
                .query_map([schedule_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// Scheduled runs of a schedule with a scheduled time after `after`, ascending.
    /// The fire times a schedule already has runs for, between `from` and `to`
    /// inclusive, whatever state those runs are in.
    ///
    /// Catch-up walks fires that have passed, and `future_runs_of_schedule`
    /// cannot see them: it looks forward, and only at Scheduled runs. Without
    /// this a fire the look-ahead had already materialised was given a second
    /// run every time a machine slept through it.
    pub fn fire_times_of_schedule(&self, schedule_id: i64, from: i64, to: i64) -> Result<Vec<i64>> {
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT scheduled_time FROM run
                  WHERE schedule_id = ?1 AND scheduled_time BETWEEN ?2 AND ?3",
            )?;
            let rows = stmt
                .query_map(rusqlite::params![schedule_id, from, to], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<i64>>>()?;
            Ok(rows)
        })
    }

    /// Runs a cancel action could target: not in a final state, optionally
    /// restricted to one flow and to a set of state types.
    ///
    /// A light projection — only the id, the parameters and the engine process
    /// id — because that is all the caller reads. It skips the join to `flow`
    /// and the `task_counts` aggregate that the general run reader carries.
    ///
    /// The state predicate is the plain terminal list, matching
    /// `StateType::is_terminal`. Note that `active_runs_of_schedule` uses a
    /// *different* rule which treats a crashed run as live while a rerun of it
    /// is pending; that is not what a cancel wants, so it is not used here.
    ///
    /// `states` empty means no state restriction. Both the flow id and the state
    /// list are bound, the latter as one JSON array, so the statement text is
    /// the same whatever the caller asks for.
    pub fn cancellable_runs(
        &self,
        flow_id: Option<i64>,
        states: &[cereyan_core::StateType],
    ) -> Result<Vec<(i64, String, Option<i64>)>> {
        let list = serde_json::to_string(
            &states
                .iter()
                .map(|s| s.as_str().to_string())
                .collect::<Vec<_>>(),
        )
        .unwrap_or_else(|_| "[]".to_string());
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT r.id, r.parameters, r.engine_pid
                 FROM run r
                 WHERE (?1 IS NULL OR r.flow_id = ?1)
                   AND (?2 = 0 OR r.state_type IN (SELECT value FROM json_each(?3)))
                   -- COALESCE: a run that has not been transitioned yet stores a
                   -- NULL state_type, which `NOT IN` would treat as unknown and
                   -- exclude. The rest of the codebase reads that as 'Scheduled',
                   -- which is not terminal, so it is a target.
                   AND COALESCE(r.state_type, 'Scheduled')
                       NOT IN ('Completed', 'Failed', 'Cancelled', 'Crashed')
                 ORDER BY r.id",
            )?;
            let rows = stmt
                .query_map(rusqlite::params![flow_id, states.len() as i64, list], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// Every flow with this name, in any project.
    ///
    /// For resolving a name that was not given as `project/flow`, so the caller
    /// can tell a unique match from an ambiguous one. Bounded to the matching
    /// name, unlike a full flow read.
    pub fn flows_by_name(&self, name: &str) -> Result<Vec<cereyan_core::Flow>> {
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(&SQL_FLOWS_BY_NAME)?;
            let rows = stmt
                .query_map([name], flow_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// Runs of a schedule that have not reached a final state, oldest first.
    ///
    /// A Crashed run counts **unless** its rerun is still to come: a crash rerun
    /// is created in the same schedule with `parent_run_id` pointing at the run
    /// it replaces, so the parent is superseded and counting it would count one
    /// attempt twice. A Crashed run whose retries are exhausted has no child and
    /// its work is still outstanding, so it counts.
    ///
    /// (This corrects an earlier comment here, which stated the opposite.)
    pub fn active_runs_of_schedule(&self, schedule_id: i64) -> Result<Vec<Run>> {
        let sql = format!(
            "SELECT {RUN_COLUMNS} FROM run r JOIN flow f ON f.id = r.flow_id
             WHERE r.schedule_id = ?1
               AND (r.state_type NOT IN ('Completed', 'Failed', 'Cancelled', 'Crashed')
                    OR (r.state_type = 'Crashed'
                        AND NOT EXISTS (SELECT 1 FROM run c WHERE c.parent_run_id = r.id)))
             ORDER BY r.id"
        );
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(&sql)?;
            let rows = stmt
                .query_map([schedule_id], run_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// `active_runs_of_schedule` for many schedules in one query per chunk,
    /// keyed by schedule id; a schedule with no unfinished run is absent.
    pub fn active_runs_of_schedules(&self, schedule_ids: &[i64]) -> Result<HashMap<i64, Vec<Run>>> {
        let mut out: HashMap<i64, Vec<Run>> = HashMap::new();
        for chunk in schedule_ids.chunks(500) {
            let marks = vec!["?"; chunk.len()].join(", ");
            let sql = format!(
                "SELECT {RUN_COLUMNS} FROM run r JOIN flow f ON f.id = r.flow_id
                 WHERE r.schedule_id IN ({marks})
                   AND (r.state_type NOT IN ('Completed', 'Failed', 'Cancelled', 'Crashed')
                        OR (r.state_type = 'Crashed'
                            AND NOT EXISTS (SELECT 1 FROM run c WHERE c.parent_run_id = r.id)))
                 ORDER BY r.id"
            );
            let rows = self.with_reader(|conn| {
                let mut stmt = conn.prepare(&sql)?;
                let rows = stmt
                    .query_map(params_from_iter(chunk.iter()), run_from_row)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })?;
            for run in rows {
                if let Some(sid) = run.schedule_id {
                    out.entry(sid).or_default().push(run);
                }
            }
        }
        Ok(out)
    }

    /// Does any run name `run_id` as its parent (a crash rerun was created)?
    pub fn has_child_run(&self, run_id: i64) -> Result<bool> {
        self.with_reader(|conn| {
            Ok(conn
                .query_row(
                    "SELECT 1 FROM run WHERE parent_run_id = ?1 LIMIT 1",
                    [run_id],
                    |_| Ok(()),
                )
                .optional()?
                .is_some())
        })
    }

    /// Crashed runs since `since` (microseconds) whose crash chain has no
    /// follow-up run yet. The rerun timer lives only in memory, so these are
    /// the chains a restart interrupted.
    pub fn crashed_without_rerun(&self, since: i64) -> Result<Vec<i64>> {
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT r.id FROM run r
                 WHERE r.state_type = 'Crashed' AND r.state_timestamp >= ?1
                   AND NOT EXISTS (SELECT 1 FROM run c WHERE c.parent_run_id = r.id)
                 ORDER BY r.id",
            )?;
            let rows = stmt
                .query_map([since], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<i64>>>()?;
            Ok(rows)
        })
    }

    /// When the schedule's most recent finished run ended, in microseconds.
    pub fn last_end_of_schedule(&self, schedule_id: i64) -> Result<Option<i64>> {
        self.with_reader(|conn| {
            Ok(conn.query_row(
                "SELECT MAX(end_time) FROM run WHERE schedule_id = ?1 AND end_time IS NOT NULL",
                [schedule_id],
                |row| row.get::<_, Option<i64>>(0),
            )?)
        })
    }

    /// The given runs as timeline rows.
    pub fn timeline_run_rows(&self, run_ids: &[i64]) -> Result<Vec<TimelineRunRow>> {
        if run_ids.is_empty() {
            return Ok(Vec::new());
        }
        self.with_reader(|conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {TIMELINE_RUN_COLUMNS} FROM run r JOIN flow f ON f.id = r.flow_id \
                 WHERE r.id IN ({})",
                run_ids
                    .iter()
                    .map(|id| id.to_string())
                    .collect::<Vec<_>>()
                    .join(","),
            ))?;
            let rows = stmt
                .query_map([], timeline_run_row_from)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// Scheduled runs due in the window, as timeline rows.
    ///
    /// The narrow counterpart of [`scheduled_between`], which keeps its other
    /// callers. Same `WHERE`, same order, same limit.
    pub fn scheduled_timeline_run_rows(
        &self,
        after: i64,
        until: i64,
        limit: usize,
    ) -> Result<Vec<TimelineRunRow>> {
        let sql = format!(
            "SELECT {TIMELINE_RUN_COLUMNS} FROM run r JOIN flow f ON f.id = r.flow_id \
             WHERE r.state_type = 'Scheduled' AND r.scheduled_time > ?1 AND r.scheduled_time <= ?2 \
             ORDER BY r.scheduled_time, r.id LIMIT ?3"
        );
        self.with_reader(|conn| {
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt
                .query_map(
                    rusqlite::params![after, until, limit as i64],
                    timeline_run_row_from,
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// The options of the given flows, keyed by flow id.
    ///
    /// For a worker's timeline, which checks whether each candidate's flow may run
    /// remotely. Only the candidates' own flows are read, rather than every
    /// registered flow. `options` is projected whole and the rule is applied in
    /// Rust by `FlowOptions::may_run_remotely`, so there is one definition of it.
    pub fn flow_options_by_id(
        &self,
        flow_ids: &[i64],
    ) -> Result<HashMap<i64, serde_json::Map<String, serde_json::Value>>> {
        if flow_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let list = serde_json::to_string(flow_ids).unwrap_or_else(|_| "[]".to_string());
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT id, options FROM flow \
                 WHERE id IN (SELECT CAST(value AS INTEGER) FROM json_each(?1))",
            )?;
            let mut out: HashMap<i64, serde_json::Map<String, serde_json::Value>> = HashMap::new();
            for row in stmt.query_map([list], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?))
            })? {
                let (id, options) = row?;
                out.insert(id, crate::row::json_map(options));
            }
            Ok(out)
        })
    }

    /// The flow name of each of the given runs, keyed by run id.
    ///
    /// For a worker's heartbeat, which reports a flow name beside each of its
    /// engines. `get_runs` expands `RUN_COLUMNS` — 37 columns, a correlated
    /// `task_counts` aggregate evaluated once per row, and four decoded JSON
    /// documents — for one string per run, on a timer.
    ///
    /// A run that does not exist has no entry, so a caller keyed by id simply
    /// finds nothing, exactly as `get_runs` omitting the row would leave it.
    pub fn flow_names(&self, run_ids: &[i64]) -> HashMap<i64, String> {
        if run_ids.is_empty() {
            return HashMap::new();
        }
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT r.id, f.name FROM run r JOIN flow f ON f.id = r.flow_id
                 WHERE r.id IN (SELECT CAST(value AS INTEGER) FROM json_each(?1))",
            )?;
            let rows = stmt
                .query_map([chain_json(run_ids)], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows.into_iter().collect())
        })
        .unwrap_or_default()
    }

    /// The id, scheduled time and state details of the given runs, keyed by id.
    ///
    /// For callers that hold a list of run ids and need three fields from each —
    /// the scheduler's `resume` and `held_runs`, which ask whether a waiting run
    /// was skipped and when it is due. One statement for the whole list, bound
    /// through `json_each` so the SQL text stays constant and cacheable, instead
    /// of a `get_run` per id.
    ///
    /// A run that does not appear in the map simply was not found; nothing is
    /// returned for it.
    pub fn run_marks(
        &self,
        run_ids: impl IntoIterator<Item = i64>,
    ) -> HashMap<i64, ScheduleRunMark> {
        let ids: Vec<i64> = run_ids.into_iter().collect();
        if ids.is_empty() {
            return HashMap::new();
        }
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT r.id, r.scheduled_time, r.state_details
                 FROM run r
                 WHERE r.id IN (SELECT CAST(value AS INTEGER) FROM json_each(?1))",
            )?;
            let rows = stmt
                .query_map([chain_json(&ids)], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        ScheduleRunMark {
                            id: row.get(0)?,
                            scheduled_time: row.get(1)?,
                            details: crate::row::json_map(row.get(2)?),
                        },
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows.into_iter().collect())
        })
        .unwrap_or_default()
    }

    /// A schedule's future scheduled runs, as the three values the fire path
    /// reads: the id, the time it is due, and the state details the skip mark
    /// lives in.
    ///
    /// [`future_runs_of_schedule`] returns whole `Run`s — 39 columns, a
    /// correlated `task_counts` aggregate and four decoded JSON documents — and a
    /// schedule may hold up to `LOOKAHEAD_MAX` of them, so one fire would
    /// materialise a hundred runs to read three values from each. The `WHERE`,
    /// the `ORDER BY` and the returned order are identical to that reader's.
    pub fn future_run_marks(&self, schedule_id: i64, after: i64) -> Result<Vec<ScheduleRunMark>> {
        let sql = "SELECT r.id, r.scheduled_time, r.state_details
             FROM run r
             WHERE r.schedule_id = ?1 AND r.scheduled_time > ?2 AND r.state_type = 'Scheduled'
             ORDER BY r.scheduled_time";
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(sql)?;
            let rows = stmt
                .query_map(rusqlite::params![schedule_id, after], |row| {
                    Ok(ScheduleRunMark {
                        id: row.get(0)?,
                        scheduled_time: row.get(1)?,
                        details: crate::row::json_map(row.get(2)?),
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    pub fn future_runs_of_schedule(&self, schedule_id: i64, after: i64) -> Result<Vec<Run>> {
        let sql = format!(
            "SELECT {RUN_COLUMNS} FROM run r JOIN flow f ON f.id = r.flow_id
             WHERE r.schedule_id = ?1 AND r.scheduled_time > ?2 AND r.state_type = 'Scheduled'
             ORDER BY r.scheduled_time"
        );
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(&sql)?;
            let rows = stmt
                .query_map(rusqlite::params![schedule_id, after], run_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    pub fn get_backfill(&self, id: i64) -> Result<Option<Backfill>> {
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(&SQL_GET_BACKFILL)?;
            Ok(stmt.query_row([id], backfill_from_row).optional()?)
        })
    }

    pub fn list_backfills(&self, flow_id: Option<i64>) -> Result<Vec<Backfill>> {
        let sql = format!(
            "SELECT {BACKFILL_COLUMNS} FROM backfill WHERE (?1 IS NULL OR flow_id = ?1) ORDER BY id DESC LIMIT 200"
        );
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(&sql)?;
            let rows = stmt
                .query_map([flow_id], backfill_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// Run counts of a backfill by state name.
    /// Of `values`, the ones whose **latest** run for `flow_id` is `Completed`.
    ///
    /// The latest run decides, so a value with an older `Completed` run and a
    /// newer `Failed` one is *not* in the result: it still needs rerunning.
    /// That is why this is a window function rather than a `DISTINCT` over
    /// completed runs.
    ///
    /// `values` and the JSON path are bound as single parameters, so the
    /// statement is the same whatever the set size and the key contains.
    pub fn latest_completed_param_values(
        &self,
        flow_id: i64,
        key: &str,
        values: &[String],
    ) -> Result<std::collections::HashSet<String>> {
        if values.is_empty() {
            return Ok(Default::default());
        }
        let path = format!("$.{key}");
        let list = serde_json::to_string(values).unwrap_or_else(|_| "[]".to_string());
        // Explicit indices throughout: the same JSON path is used three times,
        // and mixing anonymous `?` with numbered ones makes the binding order
        // ambiguous. `?3` is the path, reused.
        let extract = "CAST(json_extract(r.parameters, ?3) AS TEXT)";
        let sql = format!(
            "SELECT value FROM (SELECT {extract} AS value, r.state_type, \
             ROW_NUMBER() OVER (PARTITION BY {extract} ORDER BY r.id DESC) AS rn \
             FROM run r WHERE r.flow_id = ?1 AND {extract} IN (SELECT value FROM json_each(?2))) \
             WHERE rn = 1 AND state_type = 'Completed'"
        );
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(&sql)?;
            let mut out = std::collections::HashSet::new();
            for r in stmt.query_map(rusqlite::params![flow_id, list, path], |r| {
                r.get::<_, String>(0)
            })? {
                out.insert(r?);
            }
            Ok(out)
        })
    }

    /// Run counts by state for several backfills at once, keyed by backfill id.
    ///
    /// A backfill with no runs has no group here, so callers default to empty.
    pub fn backfill_counts_many(
        &self,
        backfill_ids: &[i64],
    ) -> Result<std::collections::HashMap<i64, Vec<(String, i64)>>> {
        let mut out: std::collections::HashMap<i64, Vec<(String, i64)>> = Default::default();
        if backfill_ids.is_empty() {
            return Ok(out);
        }
        let list = serde_json::to_string(backfill_ids).unwrap_or_else(|_| "[]".to_string());
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT backfill_id, COALESCE(state_name, 'Scheduled'), COUNT(*)
                 FROM run WHERE backfill_id IN (SELECT CAST(value AS INTEGER) FROM json_each(?1))
                 GROUP BY backfill_id, state_name",
            )?;
            for r in stmt.query_map(rusqlite::params![list], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })? {
                let (id, state, n) = r?;
                out.entry(id).or_default().push((state, n));
            }
            Ok(out)
        })
    }

    /// The state name of a backfill's most recently created run.
    ///
    /// A backfill cannot be complete while its newest run has not ended, and runs
    /// are created in id order, so this answers that with one seek on
    /// `run_backfill (backfill_id, id)`. It is the guard in front of
    /// [`Self::backfill_counts`], which aggregates the backfill's whole run set
    /// and is far too expensive to run once per terminal run.
    ///
    /// `None` when the backfill has no runs.
    pub fn newest_backfill_run_state(&self, backfill_id: i64) -> Result<Option<String>> {
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT COALESCE(state_name, 'Scheduled') FROM run
                 WHERE backfill_id = ?1 ORDER BY id DESC LIMIT 1",
            )?;
            Ok(stmt.query_row([backfill_id], |row| row.get(0)).optional()?)
        })
    }

    pub fn backfill_counts(&self, backfill_id: i64) -> Result<Vec<(String, i64)>> {
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT COALESCE(state_name, 'Scheduled'), COUNT(*) FROM run WHERE backfill_id = ?1 GROUP BY state_name",
            )?;
            let rows = stmt
                .query_map([backfill_id], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// One task state value, from the run or the crash and retry parents it
    /// continues, nearest first.
    pub fn task_state_get(&self, run_id: i64, scope: &str, key: &str) -> Result<Option<String>> {
        self.with_reader(|conn| {
            Ok(conn
                .query_row(
                    "WITH RECURSIVE chain(id, depth) AS (
                        SELECT ?1, 0
                        UNION ALL
                        SELECT r.parent_run_id, chain.depth + 1 FROM run r JOIN chain ON r.id = chain.id
                        WHERE r.parent_run_id IS NOT NULL
                          AND (r.created_by LIKE 'crash:%' OR r.created_by LIKE 'retry:%')
                          AND chain.depth < 50
                     )
                     SELECT s.value FROM task_state s JOIN chain ON s.run_id = chain.id
                     WHERE s.scope = ?2 AND s.key = ?3 ORDER BY chain.depth LIMIT 1",
                    rusqlite::params![run_id, scope, key],
                    |r| r.get(0),
                )
                .optional()?)
        })
    }

    /// Every task state entry a run wrote itself.
    pub fn task_state_list(&self, run_id: i64) -> Result<Vec<TaskStateRow>> {
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT scope, key, value, updated_at FROM task_state WHERE run_id = ?1 ORDER BY scope, key",
            )?;
            let rows = stmt
                .query_map([run_id], |r| {
                    let raw: String = r.get(2)?;
                    Ok(TaskStateRow {
                        scope: r.get(0)?,
                        key: r.get(1)?,
                        value: serde_json::from_str(&raw).unwrap_or(serde_json::Value::String(raw)),
                        updated_at: r.get(3)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    pub fn kv_get(&self, key: &str) -> Result<Option<String>> {
        self.with_reader(|conn| {
            Ok(conn
                .query_row("SELECT value FROM kv WHERE key = ?1", [key], |r| r.get(0))
                .optional()?)
        })
    }

    pub fn list_events(
        &self,
        kind: Option<&str>,
        after_id: i64,
        limit: usize,
    ) -> Result<Vec<Event>> {
        Ok(self
            .query_events(&EventFilter {
                name: kind.map(|k| k.to_string()),
                cursor: if after_id > 0 { Some(after_id) } else { None },
                ascending: after_id > 0,
                limit: Some(limit),
                ..Default::default()
            })?
            .items)
    }

    pub fn query_events(&self, filter: &EventFilter) -> Result<EventsPage> {
        let limit = filter.limit.unwrap_or(100).clamp(1, 1000);
        let mut q = Query::new();
        if let Some(name) = &filter.name {
            if let Some(prefix) = name.strip_suffix('*') {
                q.push("kind LIKE ?", SqlValue::Text(format!("{prefix}%")));
            } else {
                q.push("kind = ?", SqlValue::Text(name.clone()));
            }
        }
        if let Some(k) = &filter.resource_kind {
            q.push("resource_kind = ?", SqlValue::Text(k.clone()));
        }
        if let Some(i) = &filter.resource_id {
            q.push("resource_id = ?", SqlValue::Text(i.clone()));
        }
        if let Some(r) = filter.run_id {
            q.push("run_id = ?", SqlValue::Integer(r));
        }
        if let Some(f) = filter.flow_id {
            q.push("flow_id = ?", SqlValue::Integer(f));
        }
        if let Some(t) = filter.after {
            q.push("timestamp >= ?", SqlValue::Integer(t));
        }
        if let Some(t) = filter.before {
            q.push("timestamp <= ?", SqlValue::Integer(t));
        }
        if let Some(c) = filter.cursor {
            if filter.ascending {
                q.push("id > ?", SqlValue::Integer(c));
            } else {
                q.push("id < ?", SqlValue::Integer(c));
            }
        }
        let order = if filter.ascending { "ASC" } else { "DESC" };
        let sql = format!(
            "SELECT {EVENT_COLUMNS} FROM event {} ORDER BY id {order} LIMIT {}",
            q.where_sql(),
            limit + 1
        );
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(&sql)?;
            let mut items = stmt
                .query_map(params_from_iter(q.args.iter()), event_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let next_cursor = if items.len() > limit {
                items.truncate(limit);
                items.last().map(|e| e.id)
            } else {
                None
            };
            Ok(EventsPage { items, next_cursor })
        })
    }

    pub fn get_event(&self, id: i64) -> Result<Option<Event>> {
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(&SQL_GET_EVENT)?;
            Ok(stmt.query_row([id], event_from_row).optional()?)
        })
    }

    pub fn list_rules(&self) -> Result<Vec<RuleRow>> {
        let sql = format!("SELECT {RULE_COLUMNS} FROM rule ORDER BY id");
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(&sql)?;
            let rows = stmt
                .query_map([], rule_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    pub fn get_rule(&self, id: i64) -> Result<Option<RuleRow>> {
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(&SQL_GET_RULE)?;
            Ok(stmt.query_row([id], rule_from_row).optional()?)
        })
    }

    pub fn list_firings(&self, rule_id: i64, limit: usize) -> Result<Vec<RuleFiring>> {
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT id, rule_id, event_id, run_id, timestamp, outcomes FROM rule_firing WHERE rule_id = ?1 ORDER BY id DESC LIMIT ?2",
            )?;
            let rows = stmt
                .query_map(rusqlite::params![rule_id, limit.clamp(1, 1000) as i64], firing_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// The newest run of a flow whose parameter `key` equals `value` (compared as text).
    pub fn latest_run_with_param(
        &self,
        flow_id: i64,
        key: &str,
        value: &str,
    ) -> Result<Option<Run>> {
        if !valid_param_key(key) {
            return Err(StoreError::Invalid(format!("invalid batch key {key:?}")));
        }
        // The JSON path is part of the statement, so this one genuinely varies
        // with `key`; it still goes through the statement cache rather than
        // being re-parsed on every call.
        let sql = format!(
            "SELECT {RUN_COLUMNS} FROM run r JOIN flow f ON f.id = r.flow_id
             WHERE r.flow_id = ?1 AND CAST(json_extract(r.parameters, '$.{key}') AS TEXT) = ?2
             ORDER BY r.id DESC LIMIT 1"
        );
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(&sql)?;
            Ok(stmt
                .query_row(rusqlite::params![flow_id, value], run_from_row)
                .optional()?)
        })
    }

    /// The newest run of each flow whose parameter `key` equals `value` (compared as text).
    /// Returns a map from flow_id to the latest run (or None if no run matches).
    pub fn latest_run_with_param_many(
        &self,
        flow_ids: &[i64],
        key: &str,
        value: &str,
    ) -> Result<HashMap<i64, Option<LatestRunMark>>> {
        if !valid_param_key(key) {
            return Err(StoreError::Invalid(format!("invalid batch key {key:?}")));
        }
        if flow_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let ids: Vec<String> = flow_ids.iter().map(|id| id.to_string()).collect();
        // Four columns, and no `task_counts` aggregate.
        //
        // This is a fan-in check: it asks whether each upstream flow's newest run
        // carrying a given parameter value is Completed or Skipped, and collects
        // those runs' ids. `RUN_COLUMNS` is 37 columns, four decoded JSON
        // documents, and a correlated `task_counts` aggregate evaluated once per
        // row -- for every run of every listed flow whose parameters match, of
        // which all but one per flow are discarded below.
        //
        // Measured at **2.0x**, and the ratio holds at every size where the rows
        // are wide enough to matter; with few matching rows it is 1.0x, because
        // then the cost is the `json_extract` scan and not the row width. There is
        // no size at which it is slower.
        //
        // The scan itself cannot be avoided: the key is supplied by the rule, so
        // there is no expression index to build for it. See design.md for the
        // per-flow seek that was measured and rejected, with its crossover.
        let sql = format!(
            "SELECT r.flow_id, {LATEST_RUN_MARK_COLUMNS} FROM run r
             WHERE r.flow_id IN ({}) AND CAST(json_extract(r.parameters, '$.{key}') AS TEXT) = ?1
             ORDER BY r.id DESC",
            ids.join(",")
        );
        self.with_reader(|conn| {
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt
                .query_map(rusqlite::params![value], latest_run_mark_from_row)?
                .collect::<rusqlite::Result<Vec<(i64, LatestRunMark)>>>()?;
            // Keep only the latest run per flow (rows are ordered by id DESC).
            let mut out: HashMap<i64, Option<LatestRunMark>> =
                flow_ids.iter().map(|id| (*id, None)).collect();
            // Every flow starts at `None`, so the first row seen for a flow, its
            // newest, fills the slot; `entry().or_insert` never would.
            for (flow_id, mark) in rows {
                if let Some(slot @ None) = out.get_mut(&flow_id) {
                    *slot = Some(mark);
                }
            }
            Ok(out)
        })
    }

    /// Open expectations ordered by deadline (reloaded into the timer on start).
    pub fn open_expectations(&self) -> Result<Vec<Expectation>> {
        let sql = format!(
            "SELECT {EXPECTATION_COLUMNS} FROM expectation WHERE status = 'open' ORDER BY deadline"
        );
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(&sql)?;
            let rows = stmt
                .query_map([], expectation_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    pub fn get_expectation(&self, id: i64) -> Result<Option<Expectation>> {
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(&SQL_GET_EXPECTATION)?;
            Ok(stmt.query_row([id], expectation_from_row).optional()?)
        })
    }

    /// A rule's expectations, newest first; `open_only` limits to armed ones.
    pub fn expectations_by_rule(
        &self,
        rule_id: i64,
        open_only: bool,
        limit: usize,
    ) -> Result<Vec<Expectation>> {
        let status = if open_only {
            " AND status = 'open'"
        } else {
            ""
        };
        let sql = format!(
            "SELECT {EXPECTATION_COLUMNS} FROM expectation WHERE rule_id = ?1{status} ORDER BY id DESC LIMIT ?2"
        );
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(&sql)?;
            let rows = stmt
                .query_map(
                    rusqlite::params![rule_id, limit.clamp(1, 1000) as i64],
                    expectation_from_row,
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// Events whose name matches any pattern (exact or trailing `*`) with a
    /// timestamp in `[since, until]`, oldest first, capped at `limit`.
    pub fn events_between(
        &self,
        patterns: &[String],
        since: i64,
        until: i64,
        limit: usize,
    ) -> Result<Vec<Event>> {
        let mut q = Query::new();
        q.push("timestamp >= ?", SqlValue::Integer(since));
        q.push("timestamp <= ?", SqlValue::Integer(until));
        let mut names: Vec<String> = Vec::new();
        for p in patterns {
            if let Some(prefix) = p.strip_suffix('*') {
                q.args.push(SqlValue::Text(format!("{prefix}%")));
                names.push(format!("kind LIKE ?{}", q.args.len()));
            } else {
                q.args.push(SqlValue::Text(p.clone()));
                names.push(format!("kind = ?{}", q.args.len()));
            }
        }
        if !names.is_empty() {
            q.clauses.push(format!("({})", names.join(" OR ")));
        }
        let sql = format!(
            "SELECT {EVENT_COLUMNS} FROM event {} ORDER BY id ASC LIMIT {}",
            q.where_sql(),
            limit.clamp(1, 100_000)
        );
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(&sql)?;
            let rows = stmt
                .query_map(params_from_iter(q.args.iter()), event_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// Artifacts across runs, newest first, with keyset pagination.
    pub fn list_artifacts(&self, filter: &ArtifactFilter) -> Result<ArtifactsPage> {
        let limit = filter.limit.unwrap_or(50).clamp(1, 500);
        let mut q = Query::new();
        if let Some(k) = &filter.kind {
            q.push("a.kind = ?", SqlValue::Text(k.clone()));
        }
        if let Some(k) = &filter.key {
            q.push("a.key = ?", SqlValue::Text(k.clone()));
        }
        if let Some(f) = &filter.flow {
            q.push("f.name = ?", SqlValue::Text(f.clone()));
        }
        if let Some(p) = &filter.project {
            q.push("f.project = ?", SqlValue::Text(p.clone()));
        }
        if let Some(g) = &filter.group {
            q.push(
                "COALESCE(f.flow_group, f.project) = ?",
                SqlValue::Text(g.clone()),
            );
        }
        if let Some(r) = filter.run_id {
            q.push("a.run_id = ?", SqlValue::Integer(r));
        }
        if let Some(c) = filter.after {
            q.push("a.id < ?", SqlValue::Integer(c));
        }
        let sql = format!(
            "SELECT {ARTIFACT_LIST_COLUMNS} FROM artifact a JOIN run r ON r.id = a.run_id JOIN flow f ON f.id = r.flow_id {} ORDER BY a.id DESC LIMIT {}",
            q.where_sql(),
            limit + 1
        );
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(&sql)?;
            let mut items = stmt
                .query_map(params_from_iter(q.args.iter()), artifact_list_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let next_cursor = if items.len() > limit {
                items.truncate(limit);
                items.last().map(|a| a.artifact.id)
            } else {
                None
            };
            Ok(ArtifactsPage { items, next_cursor })
        })
    }

    pub fn artifacts_by_run(&self, run_id: i64) -> Result<Vec<ArtifactRow>> {
        let sql = format!("SELECT {ARTIFACT_COLUMNS} FROM artifact WHERE run_id = ?1 ORDER BY id");
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(&sql)?;
            let rows = stmt
                .query_map([run_id], artifact_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    pub fn artifacts_by_task_run(&self, task_run_id: i64) -> Result<Vec<ArtifactRow>> {
        let sql =
            format!("SELECT {ARTIFACT_COLUMNS} FROM artifact WHERE task_run_id = ?1 ORDER BY id");
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(&sql)?;
            let rows = stmt
                .query_map([task_run_id], artifact_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    pub fn get_artifact(&self, id: i64) -> Result<Option<ArtifactRow>> {
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(&SQL_GET_ARTIFACT)?;
            Ok(stmt.query_row([id], artifact_from_row).optional()?)
        })
    }

    /// Variables with masked secrets.
    pub fn list_variables(&self) -> Result<Vec<VariableRow>> {
        let sql = format!("SELECT {VARIABLE_COLUMNS} FROM variable ORDER BY name");
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(&sql)?;
            let rows = stmt
                .query_map([], variable_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows.into_iter().map(|(v, _)| v).collect())
        })
    }

    /// A variable with its raw stored value (ciphertext for secrets).
    pub fn get_variable(&self, name: &str) -> Result<Option<(VariableRow, String)>> {
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(&SQL_GET_VARIABLE)?;
            Ok(stmt.query_row([name], variable_from_row).optional()?)
        })
    }

    pub fn count_secret_variables(&self) -> Result<i64> {
        self.with_reader(|conn| {
            Ok(
                conn.query_row("SELECT COUNT(*) FROM variable WHERE secret = 1", [], |r| {
                    r.get(0)
                })?,
            )
        })
    }

    /// Median total run time of completed runs of a flow, in microseconds.
    /// How many recent durations a median is taken over. A cap on how far back
    /// the median looks, shared by the single-flow and batched reads so they
    /// cannot sample different sets.
    pub const MEDIAN_SAMPLE: usize = 101;

    pub fn median_run_duration(&self, flow_id: i64) -> Result<Option<i64>> {
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT total_run_time FROM run WHERE flow_id = ?1 AND total_run_time IS NOT NULL ORDER BY id DESC LIMIT 101",
            )?;
            let mut v = stmt
                .query_map([flow_id], |r| r.get::<_, i64>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            if v.is_empty() {
                return Ok(None);
            }
            v.sort_unstable();
            Ok(Some(v[v.len() / 2]))
        })
    }

    /// The flows with these ids, in one query. Ids that do not exist are simply
    /// absent from the map, matching a `get_flow` that returned `None`.
    pub fn get_flows_by_ids(
        &self,
        ids: &[i64],
    ) -> Result<std::collections::HashMap<i64, cereyan_core::Flow>> {
        let mut out: std::collections::HashMap<i64, cereyan_core::Flow> = Default::default();
        if ids.is_empty() {
            return Ok(out);
        }
        let list = serde_json::to_string(ids).unwrap_or_else(|_| "[]".to_string());
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(&format!(
                "SELECT {FLOW_COLUMNS} FROM flow WHERE id IN (SELECT CAST(value AS INTEGER) FROM json_each(?1))"
            ))?;
            for row in stmt.query_map(rusqlite::params![list], flow_from_row)? {
                let f = row?;
                out.insert(f.id, f);
            }
            Ok(out)
        })
    }

    /// Start times for these runs, in one query.
    ///
    /// A light projection: the caller wants a timestamp, not a run. The value is
    /// an `Option` so "this run has not started" is distinguishable from "no such
    /// run", which a missing map entry already means.
    pub fn run_start_times(
        &self,
        ids: &[i64],
    ) -> Result<std::collections::HashMap<i64, Option<i64>>> {
        let mut out: std::collections::HashMap<i64, Option<i64>> = Default::default();
        if ids.is_empty() {
            return Ok(out);
        }
        let list = serde_json::to_string(ids).unwrap_or_else(|_| "[]".to_string());
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT id, start_time FROM run WHERE id IN (SELECT CAST(value AS INTEGER) FROM json_each(?1))",
            )?;
            for r in stmt.query_map(rusqlite::params![list], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, Option<i64>>(1)?))
            })? {
                let (id, start) = r?;
                out.insert(id, start);
            }
            Ok(out)
        })
    }

    /// The median run duration of each of several flows, in one query.
    ///
    /// Samples the newest [`MEDIAN_SAMPLE`] timed runs per flow and returns the
    /// same element the single-flow read would: `sort[len / 2]`, which for an
    /// even count is the *upper* middle, not the mean of the two middles. That
    /// is reproduced deliberately — a different element would silently change
    /// the number the settings page reports.
    ///
    /// A flow with no timed run has no entry, matching `median_run_duration`
    /// returning `None`.
    pub fn median_run_duration_many(
        &self,
        flow_ids: &[i64],
    ) -> Result<std::collections::HashMap<i64, i64>> {
        let mut out: std::collections::HashMap<i64, i64> = Default::default();
        if flow_ids.is_empty() {
            return Ok(out);
        }
        // A seek per flow. The window this replaces ranked every run of every
        // listed flow -- including the ones with no duration, which the filter
        // then discarded -- to take the newest `MEDIAN_SAMPLE` that do have one.
        // A per-flow `ORDER BY id DESC LIMIT ?` cannot stop early over the nulls,
        // so the sample is taken per flow and filtered in Rust, where dropping a
        // null is free.
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(SQL_MEDIAN_SAMPLE)?;
            for flow_id in flow_ids {
                let mut group: Vec<i64> = stmt
                    .query_map(
                        rusqlite::params![flow_id, Self::MEDIAN_SAMPLE as i64],
                        |r| r.get::<_, Option<i64>>(0),
                    )?
                    .collect::<rusqlite::Result<Vec<_>>>()?
                    .into_iter()
                    .flatten()
                    .collect();
                if group.is_empty() {
                    continue;
                }
                // The sample arrives newest first; the median is taken over the
                // values, so they are sorted before the middle element is read.
                group.sort_unstable();
                if let Some(median) = group.get(group.len() / 2) {
                    out.insert(*flow_id, *median);
                }
            }
            Ok(out)
        })
    }

    /// Whether any run carries `name`.
    ///
    /// Asks for one row rather than counting: `COUNT(*)` would walk every index
    /// entry for the name, and the `LIMIT` on an aggregate is applied after the
    /// aggregate is computed, so it does not stop the walk. A name shared by many
    /// runs is measurably slower to count than to find.
    pub fn run_name_exists(&self, name: &str) -> Result<bool> {
        self.with_reader(|conn| {
            Ok(conn
                .prepare_cached(&SQL_RUN_NAME_EXISTS)?
                .query_row([name], |r| r.get::<_, i64>(0))
                .optional()?
                .is_some())
        })
    }
}
