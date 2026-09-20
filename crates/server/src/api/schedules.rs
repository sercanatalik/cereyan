//! Schedule CRUD, pause and resume, upcoming runs, and previews.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use cereyan_core::schedule::{CatchupPolicy, Schedule};
use cereyan_core::{now_micros, Flow, FlowOptions, Run, ScheduleRow};
use cereyan_store::{ListRunsFilter, SchedulePatch, ScheduleWrite, StoreError};
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
    /// Seconds; missed fires older than this are not caught up. 0 or absent is off.
    #[serde(default)]
    pub catchup_window: Option<i64>,
    /// Seconds; each run is due up to this long after its fire time.
    #[serde(default)]
    pub jitter: Option<i64>,
    /// Seconds; a run not started this long after it was due is skipped. 0 or absent is off.
    #[serde(default)]
    pub start_deadline: Option<i64>,
    #[serde(default)]
    pub active: Option<bool>,
    #[serde(default)]
    pub persist: Option<bool>,
}

/// Reject a negative policy value, and a jitter that reaches an interval's period.
fn check_policy(
    schedule: &Schedule,
    window: Option<i64>,
    jitter: Option<i64>,
    deadline: Option<i64>,
) -> ApiResult<()> {
    for (name, value) in [
        ("catchup_window", window),
        ("jitter", jitter),
        ("start_deadline", deadline),
    ] {
        if value.is_some_and(|v| v < 0) {
            return Err(ApiError::Unprocessable(format!(
                "{name} must be zero or more"
            )));
        }
    }
    if let (Some(j), Schedule::Interval { interval, .. }) = (jitter, schedule) {
        if j > 0 && (j as f64) >= *interval {
            return Err(ApiError::Unprocessable(format!(
                "jitter ({j} s) must be shorter than the interval ({interval} s)"
            )));
        }
    }
    Ok(())
}

/// Zero means off for the optional policies.
fn off_when_zero(value: Option<i64>) -> Option<i64> {
    value.filter(|v| *v > 0)
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
    pub catchup_window: Option<i64>,
    #[serde(default)]
    pub jitter: Option<i64>,
    #[serde(default)]
    pub start_deadline: Option<i64>,
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
    check_policy(
        &body.schedule,
        body.catchup_window,
        body.jitter,
        body.start_deadline,
    )?;
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
            catchup_window: off_when_zero(body.catchup_window),
            jitter: body.jitter.unwrap_or(0),
            start_deadline: off_when_zero(body.start_deadline),
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

/// Retime a schedule. The row keeps its `persist` flag unless the body sets
/// it, so an edit to a code-declared schedule lasts until the next start, when
/// `sync_code_schedules` applies the declaration again; `persist: true` keeps
/// the edit and detaches the row from the declaration for good.
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
    check_policy(
        &schedule,
        body.catchup_window,
        Some(body.jitter.unwrap_or(row.jitter)),
        body.start_deadline,
    )?;
    let schedule = schedule.with_anchor_if_missing(now_micros());
    let st = state.clone();
    tokio::task::spawn_blocking(move || {
        st.store.patch_schedule(
            sid,
            SchedulePatch {
                spec: Some(serde_json::to_string(&schedule).unwrap_or_default()),
                catchup: body.catchup.map(|c| c.as_str().to_string()),
                catchup_max: body.catchup_max,
                catchup_window: body.catchup_window.map(|v| off_when_zero(Some(v))),
                jitter: body.jitter,
                start_deadline: body.start_deadline.map(|v| off_when_zero(Some(v))),
                persist: body.persist,
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

#[derive(Deserialize, utoipa::IntoParams)]
pub struct UpcomingQuery {
    /// Also list this many fires of each active schedule past its
    /// materialized runs, computed without creating runs (at most 100).
    #[serde(default)]
    pub projected: Option<usize>,
}

/// A materialized run in the upcoming list.
#[derive(Serialize, utoipa::ToSchema)]
pub struct UpcomingRun {
    #[serde(flatten)]
    pub run: Run,
    /// A person skipped this fire: the run ends Skipped at its time instead of starting.
    pub skipped: bool,
    /// Who skipped it (`ui`, `api`), when `skipped`.
    pub skipped_by: Option<String>,
    /// When it was skipped, in microseconds, when `skipped`.
    pub skipped_at: Option<i64>,
    /// Always false: this fire has a run.
    pub projected: bool,
}

/// A fire past the look-ahead, computed from the schedule; no run exists for it yet.
#[derive(Serialize, utoipa::ToSchema)]
pub struct ProjectedFire {
    pub schedule_id: i64,
    pub scheduled_time: i64,
    pub skipped: bool,
    pub skipped_by: Option<String>,
    pub skipped_at: Option<i64>,
    /// Always true.
    pub projected: bool,
}

/// One entry of the upcoming list: a run, or with `projected=N` a fire without one.
#[derive(Serialize, utoipa::ToSchema)]
#[serde(untagged)]
pub enum UpcomingItem {
    Run(Box<UpcomingRun>),
    Projected(ProjectedFire),
}

impl UpcomingItem {
    fn scheduled_time(&self) -> i64 {
        match self {
            UpcomingItem::Run(r) => r.run.scheduled_time.unwrap_or(r.run.created_at),
            UpcomingItem::Projected(p) => p.scheduled_time,
        }
    }
}

#[utoipa::path(get, path = "/api/flows/{id}/upcoming", params(("id" = i64, Path), UpcomingQuery), responses((status = 200, body = Vec<UpcomingItem>)))]
pub async fn upcoming_runs(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Query(q): Query<UpcomingQuery>,
) -> ApiResult<Json<Vec<UpcomingItem>>> {
    let now = now_micros();
    let page = state.store.list_runs(&ListRunsFilter {
        flow_id: Some(id),
        state_type: Some("Scheduled".into()),
        scheduled_after: Some(now),
        sort: Some("scheduled_asc".into()),
        limit: Some(200),
        ..Default::default()
    })?;
    let schedules = state.scheduler.for_flow(id);
    // (schedule, fire) -> (when it was skipped, by whom)
    let mut skips: HashMap<(i64, i64), (i64, String)> = HashMap::new();
    for row in &schedules {
        for (fire, at, by) in state.store.list_skip_rows(row.id)? {
            skips.insert((row.id, fire), (at, by));
        }
    }
    let mut last: HashMap<i64, i64> = HashMap::new();
    let mut out: Vec<UpcomingItem> = Vec::with_capacity(page.items.len());
    for run in page.items {
        if let (Some(sid), Some(at)) = (run.schedule_id, run.scheduled_time) {
            let latest = last.entry(sid).or_insert(at);
            *latest = (*latest).max(at);
        }
        let skipped = scheduler::is_marked(&run);
        let made = run
            .schedule_id
            .zip(run.scheduled_time)
            .and_then(|key| skips.get(&key))
            .filter(|_| skipped);
        out.push(UpcomingItem::Run(Box::new(UpcomingRun {
            skipped_by: made.map(|(_, by)| by.clone()),
            skipped_at: made.map(|(at, _)| *at),
            run,
            skipped,
            projected: false,
        })));
    }
    let count = q.projected.unwrap_or(0).min(scheduler::LOOKAHEAD_MAX);
    if count > 0 {
        for row in state
            .scheduler
            .for_flow(id)
            .into_iter()
            .filter(|r| r.active)
        {
            let from = last.get(&row.id).copied().unwrap_or(now).max(now);
            for fire in scheduler::fires_after(&row.schedule, from, count) {
                let made = skips.get(&(row.id, fire));
                out.push(UpcomingItem::Projected(ProjectedFire {
                    schedule_id: row.id,
                    scheduled_time: fire,
                    skipped: made.is_some(),
                    skipped_by: made.map(|(_, by)| by.clone()),
                    skipped_at: made.map(|(at, _)| *at),
                    projected: true,
                }));
            }
        }
        out.sort_by_key(UpcomingItem::scheduled_time);
    }
    Ok(Json(out))
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct SkipBody {
    /// Fire times to skip, in microseconds, as the upcoming list gives them.
    #[serde(default)]
    pub fires: Vec<i64>,
    /// Skip the next N fires not already skipped instead.
    #[serde(default)]
    pub next: Option<usize>,
    /// Who asked: `ui` or `api` (the default).
    #[serde(default)]
    pub by: Option<String>,
}

/// A flow that runs after the skipped one, directly or further down its chain.
#[derive(Serialize, utoipa::ToSchema)]
pub struct DownstreamSkip {
    pub flow: String,
    pub project: String,
    /// The skipped fires whose runs of this flow will be created Skipped.
    pub fires: Vec<i64>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct SkipResponse {
    pub schedule: ScheduleRow,
    /// The fires this request skipped.
    pub skipped: Vec<i64>,
    pub downstream: Vec<DownstreamSkip>,
}

/// Flows that run after `flow`, directly or further down, within its project.
fn downstream_of(state: &AppState, flow: &Flow) -> Result<Vec<Flow>, StoreError> {
    let flows = state.store.list_flows(Some(&flow.project))?;
    let mut out: Vec<Flow> = Vec::new();
    let mut frontier = vec![flow.name.clone()];
    while let Some(name) = frontier.pop() {
        for f in &flows {
            let after_it = FlowOptions::from_map(&f.options)
                .after
                .map(|a| a.depends_on(&name))
                .unwrap_or(false);
            if after_it && f.id != flow.id && !out.iter().any(|o| o.id == f.id) {
                frontier.push(f.name.clone());
                out.push(f.clone());
            }
        }
    }
    Ok(out)
}

#[utoipa::path(post, path = "/api/schedules/{sid}/skips", params(("sid" = i64, Path)), request_body = SkipBody, responses((status = 200, body = SkipResponse), (status = 404), (status = 422)))]
pub async fn add_skips(
    State(state): State<Arc<AppState>>,
    Path(sid): Path<i64>,
    Json(body): Json<SkipBody>,
) -> ApiResult<Json<SkipResponse>> {
    let row = state
        .store
        .get_schedule(sid)?
        .ok_or_else(|| ApiError::NotFound("schedule not found".into()))?;
    let by = match body.by.as_deref() {
        None | Some("api") => "api",
        Some("ui") => "ui",
        Some(other) => {
            return Err(ApiError::Unprocessable(format!(
                "by must be ui or api, not {other}"
            )))
        }
    };
    let st = state.clone();
    let target = row.clone();
    let skipped = tokio::task::spawn_blocking(move || {
        scheduler::skip_fires(&st, &target, &body.fires, body.next, by)
    })
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?
    .map_err(ApiError::Unprocessable)?;
    let downstream = match state.store.get_flow(row.flow_id)? {
        Some(flow) => downstream_of(&state, &flow)?
            .into_iter()
            .map(|f| DownstreamSkip {
                flow: f.name,
                project: f.project,
                fires: skipped.clone(),
            })
            .collect(),
        None => Vec::new(),
    };
    let row = state
        .store
        .get_schedule(sid)?
        .ok_or_else(|| ApiError::NotFound("schedule not found".into()))?;
    Ok(Json(SkipResponse {
        schedule: decorate(&state, row),
        skipped,
        downstream,
    }))
}

#[utoipa::path(delete, path = "/api/schedules/{sid}/skips/{fire}", params(("sid" = i64, Path), ("fire" = i64, Path, description = "The skipped fire time, in microseconds")), responses((status = 200, body = ScheduleRow), (status = 404), (status = 422)))]
pub async fn delete_skip(
    State(state): State<Arc<AppState>>,
    Path((sid, fire)): Path<(i64, i64)>,
) -> ApiResult<Json<ScheduleRow>> {
    state
        .store
        .get_schedule(sid)?
        .ok_or_else(|| ApiError::NotFound("schedule not found".into()))?;
    let st = state.clone();
    let found = tokio::task::spawn_blocking(move || scheduler::unskip_fire(&st, sid, fire))
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .map_err(ApiError::Unprocessable)?;
    if !found {
        return Err(ApiError::NotFound("no skip at that time".into()));
    }
    let row = state
        .store
        .get_schedule(sid)?
        .ok_or_else(|| ApiError::NotFound("schedule not found".into()))?;
    Ok(Json(decorate(&state, row)))
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
