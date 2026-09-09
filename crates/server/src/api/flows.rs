use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use cereyan_core::{Flow, Run};
use serde::{Deserialize, Serialize};

use super::error::{ApiError, ApiResult};
use super::runs::{create_run_inner, CreateRunForFlowBody};
use crate::state::AppState;

#[derive(Deserialize, utoipa::IntoParams)]
pub struct FlowsQuery {
    pub project: Option<String>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct FlowSummary {
    #[serde(flatten)]
    pub flow: Flow,
    /// Newest first: (run id, state type, state name, duration in microseconds
    /// or null while the run has none) of the last ten runs.
    pub recent_runs: Vec<(i64, String, String, Option<i64>)>,
    /// Flows that run after this one.
    pub triggers: Vec<String>,
    /// The flow this one runs after, if any (the first upstream).
    pub triggered_by: Option<String>,
    /// Every upstream of a fan-in flow (empty without `after=`).
    #[serde(default)]
    pub upstreams: Vec<String>,
    /// The batch key of a keyed fan-in.
    #[serde(default)]
    pub batch_key: Option<String>,
    pub schedules: Vec<cereyan_core::ScheduleRow>,
}

fn summarize(
    state: &AppState,
    flow: Flow,
    all: &[Flow],
) -> Result<FlowSummary, cereyan_store::StoreError> {
    let recent = state.store.recent_run_states(flow.id, 10)?;
    let options = cereyan_core::FlowOptions::from_map(&flow.options);
    let triggers = all
        .iter()
        .filter(|f| f.project == flow.project && f.id != flow.id)
        .filter(|f| {
            cereyan_core::FlowOptions::from_map(&f.options)
                .after
                .map(|a| a.depends_on(&flow.name))
                .unwrap_or(false)
        })
        .map(|f| f.name.clone())
        .collect();
    let schedules = state.scheduler.for_flow(flow.id);
    Ok(FlowSummary {
        triggered_by: options.after.as_ref().map(|a| a.flow.clone()),
        upstreams: options
            .after
            .as_ref()
            .map(|a| a.upstreams())
            .unwrap_or_default(),
        batch_key: options.after.as_ref().and_then(|a| a.key.clone()),
        flow: decorate(state, flow),
        recent_runs: recent,
        triggers,
        schedules,
    })
}

/// Fill the read-time fields the store does not hold: whether the flow is live,
/// and its group resolved to the project when it declared none, so that clients
/// never re-apply the fallback.
pub fn decorate(state: &AppState, mut flow: Flow) -> Flow {
    flow.live = state.is_live(flow.id);
    flow.group = Some(flow.group_or_project().to_string());
    flow
}

#[utoipa::path(get, path = "/api/flows", params(FlowsQuery), responses((status = 200, body = Vec<FlowSummary>)))]
pub async fn list_flows(
    State(state): State<Arc<AppState>>,
    Query(q): Query<FlowsQuery>,
) -> ApiResult<Json<Vec<FlowSummary>>> {
    let all = state.store.list_flows(None)?;
    let flows = state.store.list_flows(q.project.as_deref())?;
    let mut out = Vec::with_capacity(flows.len());
    for flow in flows {
        out.push(summarize(&state, flow, &all)?);
    }
    Ok(Json(out))
}

#[utoipa::path(get, path = "/api/flows/{id}", params(("id" = i64, Path)), responses((status = 200, body = FlowSummary), (status = 404)))]
pub async fn get_flow(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> ApiResult<Json<FlowSummary>> {
    let flow = state
        .store
        .get_flow(id)?
        .ok_or_else(|| ApiError::NotFound("flow not found".into()))?;
    let all = state.store.list_flows(None)?;
    Ok(Json(summarize(&state, flow, &all)?))
}

#[utoipa::path(delete, path = "/api/flows/{id}", params(("id" = i64, Path)), responses((status = 204), (status = 409, description = "Flow is live"), (status = 404)))]
pub async fn delete_flow(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    let flow = state
        .store
        .get_flow(id)?
        .ok_or_else(|| ApiError::NotFound("flow not found".into()))?;
    if state.is_live(flow.id) {
        return Err(ApiError::Conflict(serde_json::json!({
            "error": "flow is registered by the running server; stop serving it before deleting"
        })));
    }
    for run in state.index.active_runs() {
        if run.flow_id == id {
            state.supervisor.dequeue(run.id);
        }
    }
    state.store.delete_flow(id)?;
    state.index.remove_flow(id);
    state.stream.publish(
        "flow.registered",
        id.to_string(),
        serde_json::json!({"id": id, "deleted": true}),
    );
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(post, path = "/api/flows/{id}/runs", params(("id" = i64, Path)), request_body = CreateRunForFlowBody, responses((status = 201, body = Run), (status = 422, description = "Invalid parameters")))]
pub async fn create_run_for_flow(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Json(body): Json<CreateRunForFlowBody>,
) -> ApiResult<(StatusCode, Json<Run>)> {
    let flow = state
        .store
        .get_flow(id)?
        .ok_or_else(|| ApiError::NotFound("flow not found".into()))?;
    let run = create_run_inner(&state, &flow, body.parameters, body.name, body.tags, "api").await?;
    Ok((StatusCode::CREATED, Json(run)))
}
