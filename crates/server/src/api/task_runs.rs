use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::Json;
use cereyan_core::TaskRun;
use cereyan_store::{ListTaskRunsFilter, TaskRunsPage};

use super::error::{ApiError, ApiResult};
use crate::state::AppState;

#[utoipa::path(get, path = "/api/task-runs", params(ListTaskRunsFilter), responses((status = 200, body = TaskRunsPage)))]
pub async fn list_task_runs(
    State(state): State<Arc<AppState>>,
    Query(filter): Query<ListTaskRunsFilter>,
) -> ApiResult<Json<TaskRunsPage>> {
    let st = state.clone();
    let page = tokio::task::spawn_blocking(move || st.store.list_task_runs(&filter))
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))??;
    Ok(Json(page))
}

#[utoipa::path(get, path = "/api/task-runs/{id}", params(("id" = i64, Path)), responses((status = 200, body = TaskRun), (status = 404)))]
pub async fn get_task_run(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> ApiResult<Json<TaskRun>> {
    let t = state
        .store
        .get_task_run(id)?
        .ok_or_else(|| ApiError::NotFound("task run not found".into()))?;
    Ok(Json(t))
}
