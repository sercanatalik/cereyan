//! Row to model conversion shared by the reader and the writer.

use cereyan_core::schedule::{CatchupPolicy, Schedule};
use cereyan_core::{
    ArtifactListItem, ArtifactRow, Backfill, Event, Expectation, Flow, Id, Resource, RuleFiring,
    RuleRow, RuleSpec, Run, ScheduleRow, State, StateType, TaskRun, VariableRow,
};
use rusqlite::Row;
use serde_json::{Map, Value};

pub fn json_map(text: Option<String>) -> Map<String, Value> {
    text.and_then(|t| serde_json::from_str::<Map<String, Value>>(&t).ok())
        .unwrap_or_default()
}

pub fn json_list(text: Option<String>) -> Vec<String> {
    text.and_then(|t| serde_json::from_str::<Vec<String>>(&t).ok())
        .unwrap_or_default()
}

pub fn state_from_columns(
    state_type: Option<String>,
    name: Option<String>,
    message: Option<String>,
    details: Option<String>,
    timestamp: Option<i64>,
) -> Option<State> {
    let t = StateType::parse(&state_type?)?;
    Some(State {
        state_type: t,
        name: name.unwrap_or_else(|| t.as_str().to_string()),
        message,
        details: json_map(details),
        timestamp: timestamp.unwrap_or(0),
    })
}

/// Column order for `RUN_COLUMNS`.
pub const RUN_COLUMNS: &str =
    "r.id, r.external_id, r.flow_id, f.name, f.project, r.name, r.parameters, r.tags, \
    r.state_type, r.state_name, r.state_message, r.state_details, r.state_timestamp, \
    r.failure_count, r.crash_count, r.created_at, r.start_time, r.end_time, r.total_run_time, \
    r.engine_pid, r.engine_id, r.created_by, r.report_seq, \
    r.schedule_id, r.scheduled_time, r.priority, r.parent_run_id, r.attempt, r.backfill_id, \
    (SELECT json_group_object(st, n) FROM (SELECT COALESCE(t.state_type, 'Pending') AS st, COUNT(*) AS n \
     FROM task_run t WHERE t.run_id = r.id GROUP BY st)) AS task_counts";

pub fn run_from_row(row: &Row<'_>) -> rusqlite::Result<Run> {
    let external: Vec<u8> = row.get(1)?;
    let state = state_from_columns(
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
        row.get(11)?,
        row.get(12)?,
    )
    .unwrap_or_else(|| State::new(StateType::Scheduled).with_timestamp(0));
    Ok(Run {
        id: row.get(0)?,
        external_id: Id::from_bytes(&external).unwrap_or_else(cereyan_core::new_id),
        flow_id: row.get(2)?,
        flow_name: row.get(3)?,
        project: row.get(4)?,
        name: row.get(5)?,
        parameters: json_map(row.get(6)?),
        tags: json_list(row.get(7)?),
        state,
        failure_count: row.get::<_, i64>(13)? as u32,
        crash_count: row.get::<_, i64>(14)? as u32,
        created_at: row.get(15)?,
        start_time: row.get(16)?,
        end_time: row.get(17)?,
        total_run_time: row.get(18)?,
        engine_pid: row.get(19)?,
        engine_id: row.get(20)?,
        created_by: row.get(21)?,
        report_seq: row.get(22)?,
        schedule_id: row.get(23)?,
        scheduled_time: row.get(24)?,
        priority: row.get(25)?,
        parent_run_id: row.get(26)?,
        attempt: row.get(27)?,
        backfill_id: row.get(28)?,
        task_counts: json_counts(row.get(29)?),
    })
}

/// A JSON object of integer counts, as produced by `json_group_object`.
pub fn json_counts(text: Option<String>) -> std::collections::BTreeMap<String, i64> {
    text.and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub const TASK_RUN_COLUMNS: &str =
    "t.id, t.external_id, t.run_id, t.name, t.task_key, t.dynamic_key, \
    t.state_type, t.state_name, t.state_message, t.state_details, t.state_timestamp, \
    t.failure_count, t.crash_count, t.created_at, t.start_time, t.end_time, t.total_run_time, \
    r.name, r.flow_id, f.name, f.project, t.parents";

pub const TASK_RUN_FROM: &str =
    "task_run t JOIN run r ON r.id = t.run_id JOIN flow f ON f.id = r.flow_id";

pub fn task_run_from_row(row: &Row<'_>) -> rusqlite::Result<TaskRun> {
    let external: Vec<u8> = row.get(1)?;
    let state = state_from_columns(
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
    )
    .unwrap_or_else(|| State::new(StateType::Pending).with_timestamp(0));
    Ok(TaskRun {
        id: row.get(0)?,
        external_id: Id::from_bytes(&external).unwrap_or_else(cereyan_core::new_id),
        run_id: row.get(2)?,
        name: row.get(3)?,
        task_key: row.get(4)?,
        dynamic_key: row.get(5)?,
        state,
        failure_count: row.get::<_, i64>(11)? as u32,
        crash_count: row.get::<_, i64>(12)? as u32,
        created_at: row.get(13)?,
        start_time: row.get(14)?,
        end_time: row.get(15)?,
        total_run_time: row.get(16)?,
        run_name: row.get(17)?,
        flow_id: row.get(18)?,
        flow_name: row.get(19)?,
        project: row.get(20)?,
        parents: row
            .get::<_, Option<String>>(21)?
            .and_then(|t| serde_json::from_str::<Vec<Id>>(&t).ok())
            .unwrap_or_default(),
    })
}

pub const SCHEDULE_COLUMNS: &str =
    "id, external_id, flow_id, spec, catchup, catchup_max, active, paused_reason, \
    paused_until, source, code_key, persist, created_at, updated_at";

pub fn schedule_from_row(row: &Row<'_>) -> rusqlite::Result<ScheduleRow> {
    let blob: Vec<u8> = row.get(1)?;
    let spec: String = row.get(3)?;
    let catchup: String = row.get(4)?;
    Ok(ScheduleRow {
        id: row.get(0)?,
        external_id: Id::from_bytes(&blob).unwrap_or_else(cereyan_core::new_id),
        flow_id: row.get(2)?,
        schedule: serde_json::from_str::<Schedule>(&spec).unwrap_or(Schedule::Interval {
            interval: 3600.0,
            anchor: None,
            timezone: None,
        }),
        catchup: CatchupPolicy::parse(&catchup).unwrap_or_default(),
        catchup_max: row.get(5)?,
        active: row.get::<_, i64>(6)? != 0,
        paused_reason: row.get(7)?,
        paused_until: row.get(8)?,
        source: row.get(9)?,
        code_key: row.get(10)?,
        persist: row.get::<_, i64>(11)? != 0,
        created_at: row.get(12)?,
        updated_at: row.get(13)?,
        next_fire: None,
    })
}

pub const BACKFILL_COLUMNS: &str =
    "id, external_id, flow_id, parameter, start_value, end_value, interval_secs, \
    concurrency, total, extra_parameters, cancelled, created_at";

pub fn backfill_from_row(row: &Row<'_>) -> rusqlite::Result<Backfill> {
    let blob: Vec<u8> = row.get(1)?;
    Ok(Backfill {
        id: row.get(0)?,
        external_id: Id::from_bytes(&blob).unwrap_or_else(cereyan_core::new_id),
        flow_id: row.get(2)?,
        parameter: row.get(3)?,
        start_value: row.get(4)?,
        end_value: row.get(5)?,
        interval_secs: row.get(6)?,
        concurrency: row.get(7)?,
        total: row.get(8)?,
        extra_parameters: json_map(row.get(9)?),
        cancelled: row.get::<_, i64>(10)? != 0,
        created_at: row.get(11)?,
    })
}

pub const EVENT_COLUMNS: &str = "id, external_id, kind, timestamp, run_id, flow_id, payload, \
    resource_kind, resource_id, resource_name, related";

pub fn event_from_row(row: &Row<'_>) -> rusqlite::Result<Event> {
    let blob: Vec<u8> = row.get(1)?;
    let id: i64 = row.get(0)?;
    let related: Vec<Resource> = row
        .get::<_, Option<String>>(10)?
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    Ok(Event {
        id,
        seq: id,
        external_id: Id::from_bytes(&blob).unwrap_or_else(cereyan_core::new_id),
        name: row.get(2)?,
        occurred: row.get(3)?,
        run_id: row.get(4)?,
        flow_id: row.get(5)?,
        payload: json_map(row.get(6)?),
        resource: Resource {
            kind: row.get(7)?,
            id: row.get(8)?,
            name: row.get(9)?,
        },
        related,
    })
}

pub const ARTIFACT_COLUMNS: &str =
    "id, external_id, run_id, task_run_id, kind, key, data, created_at, updated_at";

pub fn artifact_from_row(row: &Row<'_>) -> rusqlite::Result<ArtifactRow> {
    let blob: Vec<u8> = row.get(1)?;
    let data: String = row.get(6)?;
    Ok(ArtifactRow {
        id: row.get(0)?,
        external_id: Id::from_bytes(&blob).unwrap_or_else(cereyan_core::new_id),
        run_id: row.get(2)?,
        task_run_id: row.get(3)?,
        kind: row.get(4)?,
        key: row.get(5)?,
        data: serde_json::from_str(&data).unwrap_or(Value::Null),
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

pub const EXPECTATION_COLUMNS: &str =
    "id, rule_id, key, run_id, flow_id, armed_at, deadline, status";

pub fn expectation_from_row(row: &Row<'_>) -> rusqlite::Result<Expectation> {
    Ok(Expectation {
        id: row.get(0)?,
        rule_id: row.get(1)?,
        key: row.get(2)?,
        run_id: row.get(3)?,
        flow_id: row.get(4)?,
        armed_at: row.get(5)?,
        deadline: row.get(6)?,
        status: row.get(7)?,
    })
}

/// Artifact columns joined with the run and flow identity (see `list_artifacts`).
pub const ARTIFACT_LIST_COLUMNS: &str =
    "a.id, a.external_id, a.run_id, a.task_run_id, a.kind, a.key, a.data, a.created_at, a.updated_at, r.name, f.name, f.project";

pub fn artifact_list_from_row(row: &Row<'_>) -> rusqlite::Result<ArtifactListItem> {
    Ok(ArtifactListItem {
        artifact: artifact_from_row(row)?,
        run_name: row.get(9)?,
        flow_name: row.get(10)?,
        project: row.get(11)?,
    })
}

pub const VARIABLE_COLUMNS: &str = "name, value, tags, secret, created_at, updated_at";

/// Variable rows come back with the raw stored value (ciphertext for secrets).
pub fn variable_from_row(row: &Row<'_>) -> rusqlite::Result<(VariableRow, String)> {
    let raw: String = row.get(1)?;
    let secret = row.get::<_, i64>(3)? != 0;
    Ok((
        VariableRow {
            name: row.get(0)?,
            value: if secret {
                Value::String("********".into())
            } else {
                serde_json::from_str(&raw).unwrap_or(Value::Null)
            },
            tags: json_list(row.get(2)?),
            secret,
            created_at: row.get(4)?,
            updated_at: row.get(5)?,
        },
        raw,
    ))
}

pub const RULE_COLUMNS: &str = "id, external_id, name, enabled, source, module, spec, fire_count, last_fired, created_at, updated_at";

pub fn rule_from_row(row: &Row<'_>) -> rusqlite::Result<RuleRow> {
    let blob: Vec<u8> = row.get(1)?;
    let spec: String = row.get(6)?;
    Ok(RuleRow {
        id: row.get(0)?,
        external_id: Id::from_bytes(&blob).unwrap_or_else(cereyan_core::new_id),
        name: row.get(2)?,
        enabled: row.get::<_, i64>(3)? != 0,
        source: row.get(4)?,
        module: row.get(5)?,
        spec: serde_json::from_str::<RuleSpec>(&spec).unwrap_or_default(),
        fire_count: row.get(7)?,
        last_fired: row.get(8)?,
        created_at: row.get(9)?,
        updated_at: row.get(10)?,
    })
}

pub fn firing_from_row(row: &Row<'_>) -> rusqlite::Result<RuleFiring> {
    let outcomes: String = row.get(5)?;
    Ok(RuleFiring {
        id: row.get(0)?,
        rule_id: row.get(1)?,
        event_id: row.get(2)?,
        run_id: row.get(3)?,
        timestamp: row.get(4)?,
        outcomes: serde_json::from_str(&outcomes).unwrap_or_default(),
    })
}

pub const FLOW_COLUMNS: &str =
    "id, external_id, project, name, module, source_dir, description, tags, \
    parameter_schema, created_at, last_seen_at, error, options";

pub fn flow_from_row(row: &Row<'_>) -> rusqlite::Result<Flow> {
    let blob: Vec<u8> = row.get(1)?;
    let schema: String = row.get(8)?;
    Ok(Flow {
        id: row.get(0)?,
        external_id: Id::from_bytes(&blob).unwrap_or_else(cereyan_core::new_id),
        project: row.get(2)?,
        name: row.get(3)?,
        module: row.get(4)?,
        source_dir: row.get(5)?,
        description: row.get(6)?,
        tags: json_list(row.get(7)?),
        parameter_schema: serde_json::from_str(&schema).unwrap_or(Value::Null),
        created_at: row.get(9)?,
        last_seen_at: row.get(10)?,
        error: row.get(11)?,
        options: json_map(row.get(12)?),
        live: false,
    })
}
