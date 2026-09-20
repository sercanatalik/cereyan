//! The global pause: hold every schedule at once, with a reason and an end.

use std::sync::Arc;

use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};

use super::error::{ApiError, ApiResult};
use crate::scheduler::{self, Pause};
use crate::state::AppState;

#[derive(Deserialize, utoipa::ToSchema, Default)]
#[serde(default)]
pub struct PauseBody {
    /// Why, shown in the UI banner and recorded on the event.
    pub reason: Option<String>,
    /// When to resume on its own, microseconds; absent means until resumed.
    pub until: Option<i64>,
    /// Record rules that would fire as suppressed instead of acting.
    pub suppress_rules: bool,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct SchedulerStatus {
    pub paused: bool,
    /// When the pause began, microseconds; null when running.
    pub since: Option<i64>,
    pub reason: Option<String>,
    /// When the scheduler resumes on its own, microseconds.
    pub until: Option<i64>,
    pub suppress_rules: bool,
    /// Scheduled runs whose time has passed and that the pause is holding.
    pub held: usize,
}

fn status(state: &Arc<AppState>) -> SchedulerStatus {
    let pause: Option<Pause> = state.pause();
    SchedulerStatus {
        paused: pause.is_some(),
        since: pause.as_ref().map(|p| p.since),
        reason: pause.as_ref().and_then(|p| p.reason.clone()),
        until: pause.as_ref().and_then(|p| p.until),
        suppress_rules: pause.as_ref().is_some_and(|p| p.suppress_rules),
        held: scheduler::held_runs(state),
    }
}

#[utoipa::path(get, path = "/api/scheduler", responses((status = 200, body = SchedulerStatus)))]
pub async fn get_scheduler(State(state): State<Arc<AppState>>) -> Json<SchedulerStatus> {
    let st = state.clone();
    Json(
        tokio::task::spawn_blocking(move || status(&st))
            .await
            .unwrap_or_else(|_| status(&state)),
    )
}

#[utoipa::path(post, path = "/api/scheduler/pause", request_body = PauseBody, responses((status = 200, body = SchedulerStatus), (status = 422)))]
pub async fn pause_scheduler(
    State(state): State<Arc<AppState>>,
    body: Option<Json<PauseBody>>,
) -> ApiResult<Json<SchedulerStatus>> {
    let body = body.map(|b| b.0).unwrap_or_default();
    if body.until.is_some_and(|u| u <= 0) {
        return Err(ApiError::Unprocessable(
            "until must be a time in microseconds".into(),
        ));
    }
    let reason = body
        .reason
        .map(|r| r.trim().to_string())
        .filter(|r| !r.is_empty());
    let st = state.clone();
    tokio::task::spawn_blocking(move || {
        scheduler::pause_all(&st, reason, body.until, body.suppress_rules);
        status(&st)
    })
    .await
    .map(Json)
    .map_err(|e| ApiError::Internal(e.to_string()))
}

#[utoipa::path(post, path = "/api/scheduler/resume", responses((status = 200, body = SchedulerStatus)))]
pub async fn resume_scheduler(
    State(state): State<Arc<AppState>>,
) -> ApiResult<Json<SchedulerStatus>> {
    let st = state.clone();
    tokio::task::spawn_blocking(move || {
        scheduler::resume_all(&st);
        status(&st)
    })
    .await
    .map(Json)
    .map_err(|e| ApiError::Internal(e.to_string()))
}
