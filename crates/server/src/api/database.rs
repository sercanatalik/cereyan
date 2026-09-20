//! Database statistics and the reset.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::Json;
use cereyan_core::{State as RunState, StateType};
use cereyan_store::{DeletedCounts, ResetScope, StoreError, TableCounts};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::error::{ApiError, ApiResult};
use crate::state::AppState;

#[derive(Serialize, utoipa::ToSchema)]
pub struct DatabaseInfo {
    pub path: String,
    pub bytes: u64,
    pub wal_bytes: u64,
    pub counts: TableCounts,
    /// Projects none of whose flows this server serves.
    pub stale_projects: usize,
    /// Where a reset writes its copy.
    pub backup_dir: String,
    /// `db-*.sqlite` copies in it.
    pub backups: usize,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct BackupResult {
    /// The copy just written.
    pub path: String,
    /// `db-*.sqlite` copies after pruning.
    pub backups: usize,
}

#[utoipa::path(post, path = "/api/database/backup", responses((status = 200, body = BackupResult), (status = 500)))]
pub async fn backup_database(State(state): State<Arc<AppState>>) -> ApiResult<Json<BackupResult>> {
    let st = state.clone();
    let path = tokio::task::spawn_blocking(move || crate::retention::run_backup(&st))
        .await
        .map_err(join)?
        .map_err(|e| ApiError::Internal(format!("could not copy the database: {e}")))?;
    Ok(Json(BackupResult {
        path: path.display().to_string(),
        backups: state.store.list_backups().map(|v| v.len()).unwrap_or(0),
    }))
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct ResetBody {
    /// `history` or `everything`; required.
    #[serde(default)]
    pub scope: Option<String>,
    /// Write a copy to `<home>/backups/` first; defaults to true.
    #[serde(default)]
    pub backup: Option<bool>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct ResetResult {
    pub scope: String,
    /// The copy written before the reset, when one was asked for.
    pub backup_path: Option<String>,
    pub deleted: DeletedCounts,
}

/// Answer 503 while a reset runs, for routes that would create runs.
pub fn refuse_while_resetting(state: &AppState) -> ApiResult<()> {
    if state.is_resetting() {
        return Err(ApiError::Unavailable(
            "the database is being reset; try again in a moment".into(),
        ));
    }
    Ok(())
}

fn join(e: tokio::task::JoinError) -> ApiError {
    ApiError::Internal(e.to_string())
}

#[utoipa::path(get, path = "/api/database", responses((status = 200, body = DatabaseInfo)))]
pub async fn get_database(State(state): State<Arc<AppState>>) -> ApiResult<Json<DatabaseInfo>> {
    let st = state.clone();
    let (counts, projects) = tokio::task::spawn_blocking(move || {
        Ok::<_, StoreError>((st.store.table_counts()?, st.store.list_projects()?))
    })
    .await
    .map_err(join)??;
    let (bytes, wal_bytes) = state.store.file_sizes();
    Ok(Json(DatabaseInfo {
        path: state
            .config
            .home
            .join(cereyan_store::DB_FILE)
            .display()
            .to_string(),
        bytes,
        wal_bytes,
        counts,
        stale_projects: projects
            .iter()
            .filter(|p| !p.flow_ids.iter().any(|id| state.is_live(*id)))
            .count(),
        backup_dir: state
            .config
            .home
            .join(cereyan_store::BACKUP_DIR)
            .display()
            .to_string(),
        backups: state.store.list_backups().map(|v| v.len()).unwrap_or(0),
    }))
}

/// Clears the reset flag however the reset ends, and wakes parked engine polls.
struct ResetGuard<'a>(&'a AppState);

impl Drop for ResetGuard<'_> {
    fn drop(&mut self) {
        self.0.resetting.store(false, Ordering::SeqCst);
        self.0.supervisor.notify.notify_waiters();
    }
}

#[utoipa::path(
    post,
    path = "/api/database/reset",
    request_body = ResetBody,
    responses(
        (status = 200, body = ResetResult),
        (status = 409, description = "A reset is already running"),
        (status = 422, description = "Missing or unknown scope"),
        (status = 500, description = "The copy failed; nothing was deleted")
    )
)]
pub async fn reset_database(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ResetBody>,
) -> ApiResult<Json<ResetResult>> {
    let (scope, name) = match body.scope.as_deref() {
        Some("history") => (ResetScope::History, "history"),
        Some("everything") => (ResetScope::Everything, "everything"),
        Some(other) => {
            return Err(ApiError::Unprocessable(format!(
                "unknown scope {other:?}: expected \"history\" or \"everything\""
            )))
        }
        None => {
            return Err(ApiError::Unprocessable(
                "scope is required: \"history\" or \"everything\"".into(),
            ))
        }
    };
    if state
        .resetting
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err(ApiError::Conflict(
            json!({"error": "a reset is already running"}),
        ));
    }
    let guard = ResetGuard(&state);
    let outcome = wipe(&state, scope, body.backup.unwrap_or(true)).await;
    // The scheduler creates upcoming runs as it reloads, so the flag goes first.
    drop(guard);
    let reloaded = reload(&state);
    let (backup_path, deleted) = outcome?;
    reloaded?;
    state.stream.publish(
        crate::stream::DATABASE_RESET,
        "database",
        json!({"scope": name}),
    );
    Ok(Json(ResetResult {
        scope: name.into(),
        backup_path,
        deleted,
    }))
}

/// Stop everything in flight, copy, then delete.
async fn wipe(
    state: &Arc<AppState>,
    scope: ResetScope,
    backup: bool,
) -> ApiResult<(Option<String>, DeletedCounts)> {
    state.timer.clear();
    // Parked engine polls wake and are told to exit.
    state.supervisor.notify.notify_waiters();
    stop_runs(state).await;
    let backup_path = if backup {
        let st = state.clone();
        let path = tokio::task::spawn_blocking(move || st.store.backup())
            .await
            .map_err(join)?
            .map_err(|e| {
                ApiError::Internal(format!(
                    "could not copy the database, so nothing was deleted: {e}"
                ))
            })?;
        Some(path.display().to_string())
    } else {
        None
    };
    let live: Vec<i64> = state
        .live_flows
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .copied()
        .collect();
    let st = state.clone();
    let deleted = tokio::task::spawn_blocking(move || st.store.reset(scope, &live))
        .await
        .map_err(join)??;
    Ok((backup_path, deleted))
}

/// Cancel runs that hold an engine and wait for them, killing what is left
/// after twice the grace period. Runs that never started are only taken off
/// the queue: the reset deletes them, and cancelling each would fire rules.
async fn stop_runs(state: &Arc<AppState>) {
    let active = state.index.active_runs();
    let ids: Vec<i64> = active.iter().map(|r| r.id).collect();
    state.supervisor.dequeue_many(&ids);
    let running: Vec<i64> = active
        .iter()
        .filter(|r| r.engine_pid.is_some() || state.supervisor.engine_for_run(r.id).is_some())
        .map(|r| r.id)
        .collect();
    for id in &running {
        if let Ok(Some(run)) = state.store.get_run(*id) {
            let _ = super::runs::cancel_inner(state, &run).await;
        }
    }
    let deadline = Instant::now() + state.supervisor.cancel_grace * 2 + Duration::from_secs(1);
    let still_running = |state: &AppState| {
        running
            .iter()
            .filter_map(|id| state.index.get(*id))
            .collect::<Vec<_>>()
    };
    while Instant::now() < deadline && !still_running(state).is_empty() {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    for run in still_running(state) {
        if let Some(pid) = run.engine_pid {
            crate::process::kill(pid);
        }
        let st = state.clone();
        let _ = tokio::task::spawn_blocking(move || {
            st.transition_run(
                run.id,
                RunState::new(StateType::Cancelled).with_message("database reset"),
                true,
            )
        })
        .await;
    }
}

/// Rebuild the in-memory working set from the store after a reset.
fn reload(state: &Arc<AppState>) -> ApiResult<()> {
    state.index.clear_active();
    state
        .reconcile()
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    crate::rules::load(state);
    crate::scheduler::restart(state);
    Ok(())
}
