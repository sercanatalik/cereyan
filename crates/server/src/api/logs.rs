use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::Json;
use cereyan_store::{LogFilter, LogsPage};
use serde::Deserialize;

use super::error::{ApiError, ApiResult};
use crate::state::AppState;

#[derive(Deserialize, utoipa::IntoParams)]
pub struct LogsQuery {
    /// Return rows with id greater than this (keyset cursor).
    pub after: Option<i64>,
    /// Minimum level: a Python numeric level or DEBUG, INFO, WARNING, ERROR, CRITICAL.
    pub level: Option<String>,
    pub search: Option<String>,
    pub limit: Option<usize>,
}

fn level_number(text: &str) -> Option<i32> {
    match text.trim().to_ascii_uppercase().as_str() {
        "" => None,
        "DEBUG" => Some(10),
        "INFO" => Some(20),
        "WARNING" | "WARN" => Some(30),
        "ERROR" => Some(40),
        "CRITICAL" | "FATAL" => Some(50),
        n => n.parse().ok(),
    }
}

fn to_filter(q: LogsQuery) -> LogFilter {
    LogFilter {
        after_id: q.after,
        min_level: q.level.as_deref().and_then(level_number),
        search: q.search.filter(|s| !s.is_empty()),
        limit: q.limit,
        ..Default::default()
    }
}

#[utoipa::path(get, path = "/api/runs/{id}/logs", params(("id" = i64, Path), LogsQuery), responses((status = 200, body = LogsPage)))]
pub async fn run_logs(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Query(q): Query<LogsQuery>,
) -> ApiResult<Json<LogsPage>> {
    let mut filter = to_filter(q);
    filter.run_id = Some(id);
    let st = state.clone();
    let page = tokio::task::spawn_blocking(move || st.store.logs(&filter))
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))??;
    Ok(Json(page))
}

#[utoipa::path(get, path = "/api/task-runs/{id}/logs", params(("id" = i64, Path), LogsQuery), responses((status = 200, body = LogsPage)))]
pub async fn task_run_logs(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Query(q): Query<LogsQuery>,
) -> ApiResult<Json<LogsPage>> {
    let mut filter = to_filter(q);
    filter.task_run_id = Some(id);
    let st = state.clone();
    let page = tokio::task::spawn_blocking(move || st.store.logs(&filter))
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))??;
    Ok(Json(page))
}
