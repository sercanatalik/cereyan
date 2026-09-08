//! Read queries over the read-only pool.

use cereyan_core::{
    ArtifactListItem, ArtifactRow, Backfill, Event, Expectation, Flow, Id, Log, RuleFiring,
    RuleRow, Run, ScheduleRow, StateType, TaskRun, VariableRow,
};
use rusqlite::{params_from_iter, types::Value as SqlValue, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::row::{
    artifact_from_row, artifact_list_from_row, backfill_from_row, event_from_row,
    expectation_from_row, firing_from_row, flow_from_row, rule_from_row, run_from_row,
    schedule_from_row, task_run_from_row, variable_from_row, ARTIFACT_COLUMNS,
    ARTIFACT_LIST_COLUMNS, BACKFILL_COLUMNS, EVENT_COLUMNS, EXPECTATION_COLUMNS, FLOW_COLUMNS,
    RULE_COLUMNS, RUN_COLUMNS, SCHEDULE_COLUMNS, TASK_RUN_COLUMNS, TASK_RUN_FROM, VARIABLE_COLUMNS,
};
use crate::{Result, Store, StoreError};

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

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
pub struct ListRunsFilter {
    #[serde(default)]
    pub project: Option<String>,
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
    #[serde(default)]
    pub project: Option<String>,
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
pub type RecentRun = (i64, String, String, Option<i64>);

impl Store {
    pub fn list_runs(&self, filter: &ListRunsFilter) -> Result<RunsPage> {
        let limit = filter.limit.unwrap_or(50).clamp(1, 500);
        let mut q = Query::new();
        if let Some(p) = &filter.project {
            q.push("f.project = ?", SqlValue::Text(p.clone()));
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
        if let Some(t) = filter.start_after {
            q.push(
                "COALESCE(r.start_time, r.created_at) >= ?",
                SqlValue::Integer(t),
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

    pub fn get_run(&self, run_id: i64) -> Result<Option<Run>> {
        let sql = format!(
            "SELECT {RUN_COLUMNS} FROM run r JOIN flow f ON f.id = r.flow_id WHERE r.id = ?1"
        );
        self.with_reader(|conn| Ok(conn.query_row(&sql, [run_id], run_from_row).optional()?))
    }

    pub fn get_run_by_external_id(&self, id: &Id) -> Result<Option<Run>> {
        let sql = format!(
            "SELECT {RUN_COLUMNS} FROM run r JOIN flow f ON f.id = r.flow_id WHERE r.external_id = ?1"
        );
        self.with_reader(|conn| {
            Ok(conn
                .query_row(&sql, [id.as_bytes().as_slice()], run_from_row)
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

    pub fn task_runs_by_run(&self, run_id: i64) -> Result<Vec<TaskRun>> {
        let sql = format!(
            "SELECT {TASK_RUN_COLUMNS} FROM {TASK_RUN_FROM} WHERE t.run_id = ?1 ORDER BY t.id"
        );
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(&sql)?;
            let rows = stmt
                .query_map([run_id], task_run_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    pub fn list_task_runs(&self, filter: &ListTaskRunsFilter) -> Result<TaskRunsPage> {
        let limit = filter.limit.unwrap_or(50).clamp(1, 500);
        let mut q = Query::new();
        if let Some(id) = filter.run_id {
            q.push("t.run_id = ?", SqlValue::Integer(id));
        }
        if let Some(p) = &filter.project {
            q.push("f.project = ?", SqlValue::Text(p.clone()));
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
        let sql = format!("SELECT {TASK_RUN_COLUMNS} FROM {TASK_RUN_FROM} WHERE t.id = ?1");
        self.with_reader(|conn| Ok(conn.query_row(&sql, [id], task_run_from_row).optional()?))
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

    pub fn list_flows(&self, project: Option<&str>) -> Result<Vec<Flow>> {
        let sql = format!(
            "SELECT {FLOW_COLUMNS} FROM flow WHERE (?1 IS NULL OR project = ?1) ORDER BY project, name"
        );
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(&sql)?;
            let rows = stmt
                .query_map([project], flow_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    pub fn get_flow(&self, id: i64) -> Result<Option<Flow>> {
        let sql = format!("SELECT {FLOW_COLUMNS} FROM flow WHERE id = ?1");
        self.with_reader(|conn| Ok(conn.query_row(&sql, [id], flow_from_row).optional()?))
    }

    pub fn get_flow_by_key(&self, project: &str, name: &str) -> Result<Option<Flow>> {
        let sql = format!("SELECT {FLOW_COLUMNS} FROM flow WHERE project = ?1 AND name = ?2");
        self.with_reader(|conn| {
            Ok(conn
                .query_row(&sql, [project, name], flow_from_row)
                .optional()?)
        })
    }

    /// Recent runs of a flow, newest first, limited (for the flows page dots).
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
        let sql = format!("SELECT {SCHEDULE_COLUMNS} FROM schedule WHERE id = ?1");
        self.with_reader(|conn| Ok(conn.query_row(&sql, [id], schedule_from_row).optional()?))
    }

    /// Scheduled runs of a schedule with a scheduled time after `after`, ascending.
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
        let sql = format!("SELECT {BACKFILL_COLUMNS} FROM backfill WHERE id = ?1");
        self.with_reader(|conn| Ok(conn.query_row(&sql, [id], backfill_from_row).optional()?))
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
        let sql = format!("SELECT {EVENT_COLUMNS} FROM event WHERE id = ?1");
        self.with_reader(|conn| Ok(conn.query_row(&sql, [id], event_from_row).optional()?))
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
        let sql = format!("SELECT {RULE_COLUMNS} FROM rule WHERE id = ?1");
        self.with_reader(|conn| Ok(conn.query_row(&sql, [id], rule_from_row).optional()?))
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
        let sql = format!(
            "SELECT {RUN_COLUMNS} FROM run r JOIN flow f ON f.id = r.flow_id
             WHERE r.flow_id = ?1 AND CAST(json_extract(r.parameters, '$.{key}') AS TEXT) = ?2
             ORDER BY r.id DESC LIMIT 1"
        );
        self.with_reader(|conn| {
            Ok(conn
                .query_row(&sql, rusqlite::params![flow_id, value], run_from_row)
                .optional()?)
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
        let sql = format!("SELECT {EXPECTATION_COLUMNS} FROM expectation WHERE id = ?1");
        self.with_reader(|conn| {
            Ok(conn
                .query_row(&sql, [id], expectation_from_row)
                .optional()?)
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
        let sql = format!("SELECT {ARTIFACT_COLUMNS} FROM artifact WHERE id = ?1");
        self.with_reader(|conn| Ok(conn.query_row(&sql, [id], artifact_from_row).optional()?))
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
        let sql = format!("SELECT {VARIABLE_COLUMNS} FROM variable WHERE name = ?1");
        self.with_reader(|conn| Ok(conn.query_row(&sql, [name], variable_from_row).optional()?))
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

    pub fn run_name_exists(&self, name: &str) -> Result<bool> {
        self.with_reader(|conn| {
            let n: i64 = conn.query_row(
                "SELECT COUNT(*) FROM run WHERE name = ?1 LIMIT 1",
                [name],
                |r| r.get(0),
            )?;
            Ok(n > 0)
        })
    }
}
