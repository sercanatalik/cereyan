//! Projects: flows grouped by project, what removing one deletes, and
//! removing it.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::Json;
use cereyan_core::StateType;
use cereyan_store::{DeletedCounts, ProjectRow};
use serde::Serialize;
use serde_json::json;

use super::error::{ApiError, ApiResult};
use crate::state::AppState;

/// Rows per transaction when a project's logs and events are deleted.
const DELETE_BATCH: i64 = 5_000;

#[derive(Serialize, utoipa::ToSchema)]
pub struct ProjectSummary {
    pub name: String,
    pub flows: usize,
    /// Flows this server registered from code.
    pub live_flows: usize,
    pub runs: i64,
    /// Start time of the project's latest run.
    pub last_run_at: Option<i64>,
    /// True when any of its flows is live, so it cannot be removed.
    pub served: bool,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct ProjectPreview {
    pub name: String,
    pub flows: i64,
    pub runs: i64,
    pub schedules: i64,
    pub backfills: i64,
    pub events: i64,
    /// Rules whose match names the project; they are kept.
    pub matching_rules: usize,
    pub served: bool,
    /// Runs that are Pending, Running, Paused or Cancelling.
    pub active_runs: i64,
}

fn summarize(state: &AppState, project: ProjectRow) -> ProjectSummary {
    let live_flows = project
        .flow_ids
        .iter()
        .filter(|id| state.is_live(**id))
        .count();
    ProjectSummary {
        runs: state.index.counts(Some(&project.name)).runs.values().sum(),
        flows: project.flow_ids.len(),
        live_flows,
        last_run_at: project.last_run_at,
        served: live_flows > 0,
        name: project.name,
    }
}

fn find(state: &AppState, name: &str) -> ApiResult<ProjectRow> {
    state
        .store
        .list_projects()?
        .into_iter()
        .find(|p| p.name == name)
        .ok_or_else(|| ApiError::NotFound(format!("project {name} not found")))
}

fn matching_rules(state: &AppState, name: &str) -> usize {
    state
        .rules
        .all()
        .iter()
        .filter(|r| {
            r.spec.when.project.as_deref() == Some(name)
                || r.spec.unless.as_ref().and_then(|u| u.project.as_deref()) == Some(name)
        })
        .count()
}

#[utoipa::path(get, path = "/api/projects", responses((status = 200, body = [ProjectSummary])))]
pub async fn list_projects(
    State(state): State<Arc<AppState>>,
) -> ApiResult<Json<Vec<ProjectSummary>>> {
    let mut out: Vec<ProjectSummary> = state
        .store
        .list_projects()?
        .into_iter()
        .map(|p| summarize(&state, p))
        .collect();
    out.sort_by(|a, b| b.served.cmp(&a.served).then_with(|| a.name.cmp(&b.name)));
    Ok(Json(out))
}

#[utoipa::path(get, path = "/api/projects/{name}", params(("name" = String, Path)), responses((status = 200, body = ProjectPreview), (status = 404)))]
pub async fn get_project(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> ApiResult<Json<ProjectPreview>> {
    let project = find(&state, &name)?;
    let counts = state.store.project_counts(&name)?;
    Ok(Json(ProjectPreview {
        flows: counts.flows,
        runs: counts.runs,
        schedules: counts.schedules,
        backfills: counts.backfills,
        events: counts.events,
        active_runs: counts.active_runs,
        matching_rules: matching_rules(&state, &name),
        served: project.flow_ids.iter().any(|id| state.is_live(*id)),
        name,
    }))
}

#[utoipa::path(
    delete,
    path = "/api/projects/{name}",
    params(("name" = String, Path)),
    responses(
        (status = 200, body = DeletedCounts),
        (status = 404),
        (status = 409, description = "The project is served, or a run of it is in progress"),
        (status = 503, description = "The database is being reset")
    )
)]
pub async fn delete_project(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> ApiResult<Json<DeletedCounts>> {
    super::database::refuse_while_resetting(&state)?;
    let project = find(&state, &name)?;
    if project.flow_ids.iter().any(|id| state.is_live(*id)) {
        return Err(ApiError::Conflict(json!({
            "error": format!("project {name} is served by this server; serve another directory before removing it"),
            "reason": "served",
        })));
    }
    let active: Vec<_> = state
        .index
        .active_runs()
        .into_iter()
        .filter(|r| project.flow_ids.contains(&r.flow_id))
        .collect();
    let busy = active
        .iter()
        .filter(|r| r.state.state_type != StateType::Scheduled)
        .count();
    if busy > 0 {
        return Err(ApiError::Conflict(json!({
            "error": format!("project {name} has {busy} run(s) in progress; cancel them or let them finish first"),
            "reason": "active_runs",
            "active_runs": busy,
        })));
    }
    // Scheduled runs go with the project: take them off the queue and the timer.
    let scheduled: Vec<i64> = active.iter().map(|r| r.id).collect();
    state.supervisor.dequeue_many(&scheduled);
    for id in &scheduled {
        state.timer.remove_run_events(*id);
    }
    for flow_id in &project.flow_ids {
        for row in state.scheduler.for_flow(*flow_id) {
            state.scheduler.remove(row.id);
            state.timer.remove_schedule_events(row.id);
        }
    }
    let st = state.clone();
    let ids = project.flow_ids.clone();
    let deleted =
        tokio::task::spawn_blocking(move || st.store.delete_flows_batched(&ids, DELETE_BATCH))
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))??;
    for flow_id in &project.flow_ids {
        state.index.remove_flow(*flow_id);
        state.stream.publish(
            "flow.registered",
            flow_id.to_string(),
            json!({"id": flow_id, "deleted": true}),
        );
    }
    Ok(Json(deleted))
}
