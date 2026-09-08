//! Backfills: create a range of runs in one transaction, prefilter through
//! the flow's bulk_complete hook, report status, and cancel.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use cereyan_core::{Backfill, Flow, FlowOptions, State as RunState, StateName, StateType};
use cereyan_store::{CreateBackfill, CreateRun, ListRunsFilter};
use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use super::error::{ApiError, ApiResult};
use crate::state::AppState;
use crate::supervisor::EngineKey;

#[derive(Deserialize, utoipa::ToSchema, Default, Clone)]
pub struct BackfillBody {
    pub parameter: String,
    pub start: String,
    pub end: String,
    /// Seconds, or a shorthand like `1d`, `12h`, `30m`. Default one day.
    #[serde(default)]
    pub interval: Option<Value>,
    #[serde(default)]
    pub concurrency: Option<i64>,
    #[serde(default)]
    #[schema(value_type = Object)]
    pub extra_parameters: Map<String, Value>,
    #[serde(default)]
    pub reverse: bool,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct BackfillStatus {
    #[serde(flatten)]
    pub backfill: Backfill,
    pub counts: std::collections::HashMap<String, i64>,
    pub tag: String,
}

pub fn parse_interval(v: Option<&Value>) -> Result<f64, String> {
    let Some(v) = v else { return Ok(86_400.0) };
    if let Some(n) = v.as_f64() {
        return if n > 0.0 {
            Ok(n)
        } else {
            Err("interval must be positive".into())
        };
    }
    let text = v
        .as_str()
        .ok_or("interval must be a number of seconds or a duration like 1d")?
        .trim();
    let (num, unit) = text.split_at(
        text.trim_end_matches(|c: char| c.is_ascii_alphabetic())
            .len(),
    );
    let n: f64 = num
        .trim()
        .parse()
        .map_err(|_| format!("invalid interval {text:?}"))?;
    let mult = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "s" | "sec" | "secs" => 1.0,
        "m" | "min" | "mins" => 60.0,
        "h" | "hr" | "hour" | "hours" => 3600.0,
        "d" | "day" | "days" => 86_400.0,
        "w" | "week" | "weeks" => 604_800.0,
        other => return Err(format!("unknown interval unit {other:?}")),
    };
    if n <= 0.0 {
        return Err("interval must be positive".into());
    }
    Ok(n * mult)
}

enum Kind {
    Date,
    DateTime,
}

fn parameter_kind(flow: &Flow, name: &str) -> Result<Kind, ApiError> {
    let prop = flow
        .parameter_schema
        .get("properties")
        .and_then(|p| p.get(name))
        .ok_or_else(|| ApiError::Unprocessable(format!("flow has no parameter {name:?}")))?;
    let format = prop.get("format").and_then(|f| f.as_str()).or_else(|| {
        prop.get("anyOf").and_then(|a| a.as_array()).and_then(|a| {
            a.iter()
                .find_map(|p| p.get("format").and_then(|f| f.as_str()))
        })
    });
    match format {
        Some("date") => Ok(Kind::Date),
        Some("date-time") => Ok(Kind::DateTime),
        _ => Err(ApiError::Unprocessable(format!(
            "parameter {name:?} is not a date or datetime"
        ))),
    }
}

pub fn generate_values(
    kind_date: bool,
    start: &str,
    end: &str,
    interval_secs: f64,
    reverse: bool,
) -> Result<Vec<String>, ApiError> {
    let parse = |s: &str| -> Result<DateTime<Utc>, ApiError> {
        if let Ok(d) = NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d") {
            let dt: DateTime<Utc> = d.and_hms_opt(0, 0, 0).unwrap().and_utc();
            return Ok(dt);
        }
        if let Ok(dt) = DateTime::parse_from_rfc3339(s.trim()) {
            let dt: DateTime<Utc> = dt.with_timezone(&Utc);
            return Ok(dt);
        }
        if let Ok(dt) = NaiveDateTime::parse_from_str(s.trim(), "%Y-%m-%dT%H:%M:%S") {
            let dt: DateTime<Utc> = dt.and_utc();
            return Ok(dt);
        }
        Err(ApiError::Unprocessable(format!(
            "cannot parse {s:?} as a date or datetime"
        )))
    };
    let start_dt = parse(start)?;
    let end_dt = parse(end)?;
    if end_dt < start_dt {
        return Err(ApiError::Unprocessable("end is before start".into()));
    }
    let step = Duration::microseconds((interval_secs * 1e6) as i64);
    let mut values = Vec::new();
    let mut cursor = start_dt;
    while cursor <= end_dt && values.len() < 100_000 {
        values.push(if kind_date {
            cursor.format("%Y-%m-%d").to_string()
        } else {
            cursor.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        });
        cursor += step;
    }
    if reverse {
        values.reverse();
    }
    Ok(values)
}

#[utoipa::path(post, path = "/api/flows/{id}/backfill", params(("id" = i64, Path)), request_body = BackfillBody, responses((status = 201, body = BackfillStatus), (status = 422)))]
pub async fn create_backfill(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Json(body): Json<BackfillBody>,
) -> ApiResult<(StatusCode, Json<BackfillStatus>)> {
    let flow = state
        .store
        .get_flow(id)?
        .ok_or_else(|| ApiError::NotFound("flow not found".into()))?;
    let status = create_backfill_inner(&state, &flow, &body).await?;
    Ok((StatusCode::CREATED, Json(status)))
}

/// The parameter values a backfill request would create, without creating anything.
pub fn plan_backfill(flow: &Flow, body: &BackfillBody) -> ApiResult<(Vec<String>, f64)> {
    let kind = parameter_kind(flow, &body.parameter)?;
    let interval = parse_interval(body.interval.as_ref()).map_err(ApiError::Unprocessable)?;
    let values = generate_values(
        matches!(kind, Kind::Date),
        &body.start,
        &body.end,
        interval,
        body.reverse,
    )?;
    if values.is_empty() {
        return Err(ApiError::Unprocessable("the range produces no runs".into()));
    }
    Ok((values, interval))
}

/// Create the backfill row and its runs, then index and dispatch them in the background.
pub async fn create_backfill_inner(
    state: &Arc<AppState>,
    flow: &Flow,
    body: &BackfillBody,
) -> ApiResult<BackfillStatus> {
    let (values, interval) = plan_backfill(flow, body)?;
    let concurrency = body.concurrency.unwrap_or(1).max(1);
    let options = FlowOptions::from_map(&flow.options);
    let st = state.clone();
    let flow_clone = flow.clone();
    let parameter = body.parameter.clone();
    let extra = body.extra_parameters.clone();
    let start_v = body.start.clone();
    let end_v = body.end.clone();
    let values_clone = values.clone();
    let (backfill_id, created) = tokio::task::spawn_blocking(move || {
        let (backfill_id, _) = st.store.create_backfill(CreateBackfill {
            flow_id: flow_clone.id,
            parameter: parameter.clone(),
            start_value: start_v,
            end_value: end_v,
            interval_secs: interval,
            concurrency,
            total: values_clone.len() as i64,
            extra_parameters: serde_json::to_string(&extra).unwrap_or_else(|_| "{}".into()),
        })?;
        let tag = format!("backfill:{backfill_id}");
        let mut tags = flow_clone.tags.clone();
        tags.push(tag);
        let tags_json = serde_json::to_string(&tags).unwrap_or_else(|_| "[]".into());
        let defaults: Map<String, Value> = flow_clone
            .parameter_schema
            .get("properties")
            .and_then(|p| p.as_object())
            .map(|o| {
                o.iter()
                    .filter_map(|(k, p)| p.get("default").map(|d| (k.clone(), d.clone())))
                    .collect()
            })
            .unwrap_or_default();
        let cmds: Vec<CreateRun> = values_clone
            .iter()
            .map(|v| {
                let mut params = defaults.clone();
                for (k, val) in &extra {
                    params.insert(k.clone(), val.clone());
                }
                params.insert(parameter.clone(), Value::String(v.clone()));
                CreateRun {
                    flow_id: flow_clone.id,
                    name: format!("{}-{}", flow_clone.name, v.replace(':', "")),
                    parameters: serde_json::to_string(&params).unwrap_or_else(|_| "{}".into()),
                    tags: tags_json.clone(),
                    created_by: format!("backfill:{backfill_id}"),
                    initial_state: Some(RunState::new(StateType::Scheduled)),
                    priority: options.priority,
                    backfill_id: Some(backfill_id),
                    ..Default::default()
                }
            })
            .collect();
        let created = st.store.create_runs_bulk(cmds)?;
        Ok::<(i64, Vec<(i64, cereyan_core::Id)>), cereyan_store::StoreError>((backfill_id, created))
    })
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))??;

    state.supervisor.set_total(
        &crate::dispatch::backfill_resource(backfill_id),
        concurrency as f64,
    );
    let key = EngineKey::from_flow(flow);
    let st = state.clone();
    let flow_for_dispatch = flow.clone();
    let has_bulk = options.has_bulk_complete;
    let count = created.len();
    // Index insertion and dispatch happen in the background so the creation
    // call returns as soon as the transaction commits.
    tokio::task::spawn_blocking(move || {
        let mut cursor = None;
        loop {
            let page = match st.store.list_runs(&ListRunsFilter {
                backfill_id: Some(backfill_id),
                limit: Some(500),
                sort: Some("created_asc".into()),
                cursor,
                ..Default::default()
            }) {
                Ok(p) => p,
                Err(_) => break,
            };
            for run in &page.items {
                st.index.insert_run(run, key.clone(), false);
            }
            match page.next_cursor {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }
        st.stream.publish(
            "backfill.created",
            backfill_id.to_string(),
            json!({"backfill_id": backfill_id, "flow_id": flow_for_dispatch.id, "count": count}),
        );
        if has_bulk {
            st.supervisor.enqueue_job(
                key.clone(),
                json!({"kind": "bulk_complete", "flow_id": flow_for_dispatch.id, "backfill_id": backfill_id}),
            );
            st.supervisor.ensure_capacity(&st);
        } else {
            enqueue_all(&st, backfill_id, &flow_for_dispatch, &[]);
        }
    });
    status_of(state, backfill_id)
}

/// Enqueue every not-yet-dispatched run of a backfill, skipping listed values.
pub fn enqueue_all(state: &Arc<AppState>, backfill_id: i64, flow: &Flow, skip_values: &[String]) {
    let Ok(Some(backfill)) = state.store.get_backfill(backfill_id) else {
        return;
    };
    let options = FlowOptions::from_map(&flow.options);
    let key = EngineKey::from_flow(flow);
    state.supervisor.set_total(
        &crate::dispatch::backfill_resource(backfill_id),
        backfill.concurrency.max(1) as f64,
    );
    if let Some(cap) = options.max_concurrent {
        if cap > 0 {
            state
                .supervisor
                .set_total(&crate::dispatch::flow_cap_resource(flow), cap as f64);
        }
    }
    let mut cursor: Option<i64> = None;
    loop {
        let page = match state.store.list_runs(&ListRunsFilter {
            backfill_id: Some(backfill_id),
            state_type: Some("Scheduled".into()),
            limit: Some(500),
            sort: Some("created_asc".into()),
            cursor,
            ..Default::default()
        }) {
            Ok(p) => p,
            Err(_) => return,
        };
        let mut batch = Vec::with_capacity(page.items.len());
        for run in &page.items {
            let value = run
                .parameters
                .get(&backfill.parameter)
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if skip_values.iter().any(|s| s == value) {
                let mut s = RunState::named(StateName::Skipped);
                s.message = Some("already complete (bulk_complete)".into());
                let _ = state.transition_run(run.id, s, false);
                continue;
            }
            batch.push(crate::supervisor::QueuedRun {
                run_id: run.id,
                key: key.clone(),
                priority: run.priority,
                order: run.scheduled_time.unwrap_or(run.created_at),
                needs: crate::dispatch::run_needs(flow, &options, run),
                not_before: None,
            });
        }
        state.supervisor.enqueue_many(batch);
        match page.next_cursor {
            Some(c) => cursor = Some(c),
            None => break,
        }
    }
    state.supervisor.ensure_capacity(state);
}

pub fn status_of(state: &AppState, backfill_id: i64) -> ApiResult<BackfillStatus> {
    let backfill = state
        .store
        .get_backfill(backfill_id)?
        .ok_or_else(|| ApiError::NotFound("backfill not found".into()))?;
    let counts = state
        .store
        .backfill_counts(backfill_id)?
        .into_iter()
        .collect();
    Ok(BackfillStatus {
        tag: format!("backfill:{backfill_id}"),
        backfill,
        counts,
    })
}

#[utoipa::path(get, path = "/api/backfills/{id}", params(("id" = i64, Path)), responses((status = 200, body = BackfillStatus), (status = 404)))]
pub async fn get_backfill(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> ApiResult<Json<BackfillStatus>> {
    Ok(Json(status_of(&state, id)?))
}

#[utoipa::path(get, path = "/api/flows/{id}/backfills", params(("id" = i64, Path)), responses((status = 200, body = Vec<BackfillStatus>)))]
pub async fn list_backfills(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> ApiResult<Json<Vec<BackfillStatus>>> {
    let rows = state.store.list_backfills(Some(id))?;
    let mut out = Vec::new();
    for b in rows {
        out.push(status_of(&state, b.id)?);
    }
    Ok(Json(out))
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct PrefilterBody {
    pub skip: Vec<String>,
}

#[utoipa::path(post, path = "/api/backfills/{id}/prefilter", params(("id" = i64, Path)), request_body = PrefilterBody, responses((status = 200)))]
pub async fn prefilter_backfill(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Json(body): Json<PrefilterBody>,
) -> ApiResult<Json<serde_json::Value>> {
    let backfill = state
        .store
        .get_backfill(id)?
        .ok_or_else(|| ApiError::NotFound("backfill not found".into()))?;
    let flow = state
        .store
        .get_flow(backfill.flow_id)?
        .ok_or_else(|| ApiError::NotFound("flow not found".into()))?;
    let st = state.clone();
    let skipped = body.skip.len();
    tokio::task::spawn_blocking(move || enqueue_all(&st, id, &flow, &body.skip))
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(json!({"skipped": skipped})))
}

#[utoipa::path(post, path = "/api/backfills/{id}/cancel", params(("id" = i64, Path)), responses((status = 200, body = BackfillStatus)))]
pub async fn cancel_backfill(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> ApiResult<Json<BackfillStatus>> {
    state
        .store
        .get_backfill(id)?
        .ok_or_else(|| ApiError::NotFound("backfill not found".into()))?;
    let st = state.clone();
    tokio::task::spawn_blocking(move || {
        let _ = st.store.set_backfill_cancelled(id);
        let mut cursor = None;
        let mut bulk: Vec<(i64, StateType)> = Vec::new();
        let mut flow_id = 0;
        loop {
            let page = match st.store.list_runs(&ListRunsFilter {
                backfill_id: Some(id),
                limit: Some(500),
                sort: Some("created_asc".into()),
                cursor,
                ..Default::default()
            }) {
                Ok(p) => p,
                Err(_) => break,
            };
            for run in &page.items {
                if run.state.is_terminal() {
                    continue;
                }
                flow_id = run.flow_id;
                let immediate = matches!(
                    run.state.state_type,
                    StateType::Scheduled | StateType::Pending
                ) || run.engine_pid.is_none();
                if immediate {
                    bulk.push((run.id, run.state.state_type));
                } else {
                    let _ = st.transition_run(run.id, RunState::new(StateType::Cancelling), false);
                    st.index.update(run.id, |r| {
                        r.cancel_requested = true;
                        if r.cancelling_since.is_none() {
                            r.cancelling_since = Some(std::time::Instant::now());
                        }
                    });
                }
            }
            match page.next_cursor {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }
        if !bulk.is_empty() {
            let ids: Vec<i64> = bulk.iter().map(|(i, _)| *i).collect();
            st.supervisor.dequeue_many(&ids);
            for id in &ids {
                st.timer.remove_run_events(*id);
            }
            let accepted = st
                .store
                .transition_many(
                    ids.clone(),
                    RunState::new(StateType::Cancelled).with_message("backfill cancelled"),
                )
                .unwrap_or_default();
            for prev in [StateType::Scheduled, StateType::Pending] {
                let group: Vec<i64> = bulk
                    .iter()
                    .filter(|(i, p)| *p == prev && accepted.contains(i))
                    .map(|(i, _)| *i)
                    .collect();
                if !group.is_empty() {
                    st.index
                        .bulk_terminal(flow_id, &group, prev, StateType::Cancelled);
                }
            }
            for id in &accepted {
                st.supervisor.run_finished(*id);
            }
            st.stream.publish(
                "backfill.updated",
                id.to_string(),
                json!({"backfill_id": id, "cancelled": accepted.len()}),
            );
            st.supervisor.ensure_capacity(&st);
        }
    })
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(status_of(&state, id)?))
}
