//! Schedule CRUD, pause and resume, upcoming runs, and previews.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use cereyan_core::schedule::{CatchupPolicy, Schedule};
use cereyan_core::{now_micros, Run, ScheduleRow};
use cereyan_store::{ListRunsFilter, SchedulePatch, ScheduleWrite};
use serde::{Deserialize, Serialize};

use super::error::{ApiError, ApiResult};
use crate::scheduler;
use crate::state::AppState;

#[derive(Deserialize, utoipa::ToSchema)]
pub struct ScheduleBody {
    #[serde(flatten)]
    pub schedule: Schedule,
    #[serde(default)]
    pub catchup: Option<CatchupPolicy>,
    #[serde(default)]
    pub catchup_max: Option<i64>,
    #[serde(default)]
    pub active: Option<bool>,
    #[serde(default)]
    pub persist: Option<bool>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct SchedulePatchBody {
    #[serde(default)]
    pub cron: Option<String>,
    #[serde(default)]
    pub timezone: Option<String>,
    #[serde(default)]
    pub day_or: Option<bool>,
    #[serde(default)]
    pub interval: Option<f64>,
    #[serde(default)]
    pub anchor: Option<i64>,
    #[serde(default)]
    pub rrule: Option<String>,
    #[serde(default)]
    pub catchup: Option<CatchupPolicy>,
    #[serde(default)]
    pub catchup_max: Option<i64>,
    #[serde(default)]
    pub persist: Option<bool>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct PreviewResponse {
    pub next: Vec<i64>,
    pub timezone: String,
}

pub(crate) fn decorate(state: &AppState, mut row: ScheduleRow) -> ScheduleRow {
    if let Some(cached) = state.scheduler.get(row.id) {
        row.next_fire = cached.next_fire;
    }
    row
}

#[utoipa::path(get, path = "/api/flows/{id}/schedules", params(("id" = i64, Path)), responses((status = 200, body = Vec<ScheduleRow>)))]
pub async fn list_schedules(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> ApiResult<Json<Vec<ScheduleRow>>> {
    let rows = state.store.list_schedules(Some(id))?;
    Ok(Json(
        rows.into_iter().map(|r| decorate(&state, r)).collect(),
    ))
}

#[utoipa::path(post, path = "/api/flows/{id}/schedules", params(("id" = i64, Path)), request_body = ScheduleBody, responses((status = 201, body = ScheduleRow), (status = 422)))]
pub async fn create_schedule(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Json(body): Json<ScheduleBody>,
) -> ApiResult<(StatusCode, Json<ScheduleRow>)> {
    let row = create_schedule_inner(&state, id, body, "ui").await?;
    Ok((StatusCode::CREATED, Json(row)))
}

/// Store a new schedule for a flow. `source` records who made it: `ui` for the
/// interface, `mcp` for an agent. Reconciliation only ever singles out `code`,
/// so any other value is left alone when the flow re-registers.
pub async fn create_schedule_inner(
    state: &Arc<AppState>,
    flow_id: i64,
    body: ScheduleBody,
    source: &str,
) -> ApiResult<ScheduleRow> {
    state
        .store
        .get_flow(flow_id)?
        .ok_or_else(|| ApiError::NotFound("flow not found".into()))?;
    body.schedule
        .validate()
        .map_err(|e| ApiError::Unprocessable(e.to_string()))?;
    let pinned = body.schedule.clone().with_anchor_if_missing(now_micros());
    let st = state.clone();
    let source = source.to_string();
    let schedule_id = tokio::task::spawn_blocking(move || {
        let sid = st.store.upsert_schedule(ScheduleWrite {
            id: None,
            flow_id,
            spec: serde_json::to_string(&pinned).unwrap_or_default(),
            catchup: body.catchup.unwrap_or_default().as_str().into(),
            catchup_max: body.catchup_max.unwrap_or(100),
            active: body.active.unwrap_or(true),
            source,
            code_key: None,
            persist: body.persist.unwrap_or(true),
        })?;
        scheduler::rebuild(&st, sid);
        st.publish_schedule(sid);
        Ok::<i64, cereyan_store::StoreError>(sid)
    })
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))??;
    let row = state
        .store
        .get_schedule(schedule_id)?
        .ok_or_else(|| ApiError::Internal("schedule vanished".into()))?;
    Ok(decorate(state, row))
}

fn apply_patch(current: &Schedule, body: &SchedulePatchBody) -> Schedule {
    match current.clone() {
        Schedule::Cron {
            cron,
            timezone,
            day_or,
        } => Schedule::Cron {
            cron: body.cron.clone().unwrap_or(cron),
            timezone: body.timezone.clone().or(timezone),
            day_or: body.day_or.unwrap_or(day_or),
        },
        Schedule::Interval {
            interval,
            anchor,
            timezone,
        } => Schedule::Interval {
            interval: body.interval.unwrap_or(interval),
            anchor: body.anchor.or(anchor),
            timezone: body.timezone.clone().or(timezone),
        },
        Schedule::RRule { rrule, timezone } => Schedule::RRule {
            rrule: body.rrule.clone().unwrap_or(rrule),
            timezone: body.timezone.clone().or(timezone),
        },
    }
}

#[utoipa::path(patch, path = "/api/schedules/{sid}", params(("sid" = i64, Path)), request_body = SchedulePatchBody, responses((status = 200, body = ScheduleRow), (status = 422)))]
pub async fn patch_schedule(
    State(state): State<Arc<AppState>>,
    Path(sid): Path<i64>,
    Json(body): Json<SchedulePatchBody>,
) -> ApiResult<Json<ScheduleRow>> {
    Ok(Json(patch_schedule_inner(&state, sid, body).await?))
}

/// Retime a schedule. A spec change marks the row `persist`, after which the
/// flow's own declaration no longer governs it: `scheduler::register` skips
/// every persisted row. Callers that can explain that to a person should.
pub async fn patch_schedule_inner(
    state: &Arc<AppState>,
    sid: i64,
    body: SchedulePatchBody,
) -> ApiResult<ScheduleRow> {
    let row = state
        .store
        .get_schedule(sid)?
        .ok_or_else(|| ApiError::NotFound("schedule not found".into()))?;
    let schedule = if let Some(cron) = &body.cron {
        Schedule::Cron {
            cron: cron.clone(),
            timezone: body.timezone.clone().or_else(|| match &row.schedule {
                Schedule::Cron { timezone, .. } => timezone.clone(),
                _ => None,
            }),
            day_or: body.day_or.unwrap_or(true),
        }
    } else if let Some(rrule) = &body.rrule {
        Schedule::RRule {
            rrule: rrule.clone(),
            timezone: body.timezone.clone(),
        }
    } else if body.interval.is_some() && !matches!(row.schedule, Schedule::Interval { .. }) {
        Schedule::Interval {
            interval: body.interval.unwrap_or(3600.0),
            anchor: body.anchor.or(Some(now_micros())),
            timezone: body.timezone.clone(),
        }
    } else {
        apply_patch(&row.schedule, &body)
    };
    schedule
        .validate()
        .map_err(|e| ApiError::Unprocessable(e.to_string()))?;
    let schedule = schedule.with_anchor_if_missing(now_micros());
    let st = state.clone();
    tokio::task::spawn_blocking(move || {
        st.store.patch_schedule(
            sid,
            SchedulePatch {
                spec: Some(serde_json::to_string(&schedule).unwrap_or_default()),
                catchup: body.catchup.map(|c| c.as_str().to_string()),
                catchup_max: body.catchup_max,
                persist: body.persist.or(Some(true)),
                ..Default::default()
            },
        )?;
        scheduler::rebuild(&st, sid);
        st.publish_schedule(sid);
        Ok::<(), cereyan_store::StoreError>(())
    })
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))??;
    let row = state
        .store
        .get_schedule(sid)?
        .ok_or_else(|| ApiError::NotFound("schedule not found".into()))?;
    Ok(decorate(state, row))
}

#[utoipa::path(delete, path = "/api/schedules/{sid}", params(("sid" = i64, Path)), responses((status = 204), (status = 404)))]
pub async fn delete_schedule(
    State(state): State<Arc<AppState>>,
    Path(sid): Path<i64>,
) -> ApiResult<StatusCode> {
    let st = state.clone();
    let ok = tokio::task::spawn_blocking(move || scheduler::delete(&st, sid))
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    if ok {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound("schedule not found".into()))
    }
}

#[utoipa::path(post, path = "/api/schedules/{sid}/pause", params(("sid" = i64, Path)), responses((status = 200, body = ScheduleRow)))]
pub async fn pause_schedule(
    State(state): State<Arc<AppState>>,
    Path(sid): Path<i64>,
) -> ApiResult<Json<ScheduleRow>> {
    state
        .store
        .get_schedule(sid)?
        .ok_or_else(|| ApiError::NotFound("schedule not found".into()))?;
    let st = state.clone();
    tokio::task::spawn_blocking(move || scheduler::pause(&st, sid, Some("paused"), None))
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let row = state
        .store
        .get_schedule(sid)?
        .ok_or_else(|| ApiError::NotFound("schedule not found".into()))?;
    Ok(Json(decorate(&state, row)))
}

#[utoipa::path(post, path = "/api/schedules/{sid}/resume", params(("sid" = i64, Path)), responses((status = 200, body = ScheduleRow)))]
pub async fn resume_schedule(
    State(state): State<Arc<AppState>>,
    Path(sid): Path<i64>,
) -> ApiResult<Json<ScheduleRow>> {
    state
        .store
        .get_schedule(sid)?
        .ok_or_else(|| ApiError::NotFound("schedule not found".into()))?;
    let st = state.clone();
    tokio::task::spawn_blocking(move || scheduler::resume(&st, sid))
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let row = state
        .store
        .get_schedule(sid)?
        .ok_or_else(|| ApiError::NotFound("schedule not found".into()))?;
    Ok(Json(decorate(&state, row)))
}

#[utoipa::path(get, path = "/api/flows/{id}/upcoming", params(("id" = i64, Path)), responses((status = 200, body = Vec<Run>)))]
pub async fn upcoming_runs(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> ApiResult<Json<Vec<Run>>> {
    let page = state.store.list_runs(&ListRunsFilter {
        flow_id: Some(id),
        state_type: Some("Scheduled".into()),
        scheduled_after: Some(now_micros()),
        sort: Some("scheduled_asc".into()),
        limit: Some(200),
        ..Default::default()
    })?;
    Ok(Json(page.items))
}

#[utoipa::path(post, path = "/api/flows/{id}/pause", params(("id" = i64, Path)), responses((status = 200, description = "All schedules paused")))]
pub async fn pause_flow(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> ApiResult<Json<serde_json::Value>> {
    let st = state.clone();
    let n = tokio::task::spawn_blocking(move || {
        let rows = st.scheduler.for_flow(id);
        for r in &rows {
            scheduler::pause(&st, r.id, Some("paused"), None);
        }
        rows.len()
    })
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(serde_json::json!({"paused": n})))
}

#[utoipa::path(post, path = "/api/flows/{id}/resume", params(("id" = i64, Path)), responses((status = 200, description = "All schedules resumed")))]
pub async fn resume_flow(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> ApiResult<Json<serde_json::Value>> {
    let st = state.clone();
    let n = tokio::task::spawn_blocking(move || {
        let rows = st.scheduler.for_flow(id);
        for r in &rows {
            scheduler::resume(&st, r.id);
        }
        rows.len()
    })
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(serde_json::json!({"resumed": n})))
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct PreviewBody {
    #[serde(flatten)]
    pub schedule: Schedule,
    #[serde(default)]
    pub count: Option<usize>,
}

#[utoipa::path(post, path = "/api/schedules/preview", request_body = PreviewBody, responses((status = 200, body = PreviewResponse), (status = 422)))]
pub async fn preview_schedule(Json(body): Json<PreviewBody>) -> ApiResult<Json<PreviewResponse>> {
    let next = scheduler::preview(&body.schedule, body.count.unwrap_or(3))
        .map_err(ApiError::Unprocessable)?;
    Ok(Json(PreviewResponse {
        next,
        timezone: body.schedule.timezone_name(),
    }))
}
