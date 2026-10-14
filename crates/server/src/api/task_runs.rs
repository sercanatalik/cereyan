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
    let mut t = state
        .store
        .get_task_run(id)?
        .ok_or_else(|| ApiError::NotFound("task run not found".into()))?;
    let (_, lineage) = state.store.lineage(t.run_id)?;
    t.effect_key = Some(effect_key(&lineage.to_string(), &t.dynamic_key));
    Ok(Json(t))
}

/// The effect key of a task call, as the engine computes it
/// (`cereyan.effects.effect_key`): the lineage root's external id and the
/// call's dynamic key, hashed.
pub fn effect_key(lineage: &str, dynamic_key: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(format!("cereyan.effect\0{lineage}\0{dynamic_key}").as_bytes());
    digest.iter().take(16).map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn effect_key_matches_the_engine() {
        // hashlib.sha256(b"cereyan.effect\0root\0charge-0").hexdigest()[:32]
        assert_eq!(
            super::effect_key("root", "charge-0"),
            "863baa9d2d304d8662912c63718c3ec4"
        );
    }
}
