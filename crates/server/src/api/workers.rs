//! Remote workers: registration, heartbeats with commands, the registry the
//! Workers tab shows, and drain, resume, resize and forget.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use cereyan_core::{now_micros, EventName, Worker};
use cereyan_store::WorkerRegistration;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use super::error::{ApiError, ApiResult};
use crate::state::AppState;

/// Seconds between a worker's heartbeats; three missed ones make it offline.
pub const HEARTBEAT_SECS: u64 = 5;

#[derive(Deserialize, Serialize, Clone, utoipa::ToSchema)]
pub struct WorkerFlow {
    pub project: String,
    pub flow: String,
    pub module: String,
    /// The worker's fingerprint of the module (`_core.module_fingerprint`).
    pub module_hash: String,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct RegisterBody {
    pub name: String,
    pub version: String,
    pub cpus: i64,
    #[serde(default = "one")]
    pub processors: i64,
    #[serde(default)]
    #[schema(value_type = Object)]
    pub labels: Map<String, Value>,
    #[serde(default)]
    pub shared_paths: Vec<String>,
    #[serde(default)]
    #[schema(value_type = Object)]
    pub meta: Map<String, Value>,
    #[serde(default)]
    pub flows: Vec<WorkerFlow>,
}

fn one() -> i64 {
    1
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct RegisterResponse {
    pub worker_id: i64,
    pub heartbeat_secs: u64,
    /// `online` or `draining` (a worker drained before it restarted stays drained).
    pub state: String,
    /// Flows the worker has that the server does not: they never run there.
    pub refused: Vec<String>,
    /// Modules whose code differs from the server's: the worker takes no run of them.
    pub drift: Vec<String>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct HeartbeatBody {
    /// Engine ids the worker runs now.
    #[serde(default)]
    pub engines: Vec<String>,
    /// Metadata that changes: free memory, busy processors, git state.
    #[serde(default)]
    #[schema(value_type = Object)]
    pub meta: Map<String, Value>,
    /// Fingerprints again when the checkout changed.
    #[serde(default)]
    pub flows: Option<Vec<WorkerFlow>>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct HeartbeatResponse {
    #[schema(value_type = Vec<Object>)]
    pub commands: Vec<Value>,
    pub state: String,
    pub drift: Vec<String>,
}

/// A worker with what the server knows of it right now.
#[derive(Serialize, utoipa::ToSchema)]
pub struct WorkerView {
    #[serde(flatten)]
    pub worker: Worker,
    /// Engines running a run, and idle or starting.
    pub running: usize,
    pub idle: usize,
    /// Flows it can run (code matches) and modules whose code differs.
    pub flows: usize,
    pub drift: Vec<String>,
}

fn major(version: &str) -> &str {
    version.split('.').next().unwrap_or(version)
}

/// Where the request came from, as a proxy in front reports it.
fn client_address(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-forwarded-for")
        .or_else(|| headers.get("x-real-ip"))
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// What comparing a worker's fingerprints with the server's gives: the rows to
/// store (flow id, the worker's hash), the flows it may run, the modules that
/// differ, and the flows the server does not know.
struct Evaluation {
    rows: Vec<(i64, String)>,
    eligible: HashSet<i64>,
    drift: Vec<String>,
    refused: Vec<String>,
}

/// Compare a worker's module fingerprints with the server's: the flows it may
/// run, and the modules that differ. Flows the server does not know are
/// returned as refused.
fn evaluate(state: &AppState, flows: &[WorkerFlow]) -> ApiResult<Evaluation> {
    let known: HashMap<(String, String), cereyan_core::Flow> = state
        .store
        .list_flows(None)?
        .into_iter()
        .map(|f| ((f.project.clone(), f.name.clone()), f))
        .collect();
    let mut rows = Vec::new();
    let mut eligible = HashSet::new();
    let mut drift: Vec<String> = Vec::new();
    let mut refused = Vec::new();
    for wf in flows {
        let Some(flow) = known.get(&(wf.project.clone(), wf.flow.clone())) else {
            refused.push(format!("{}/{}", wf.project, wf.flow));
            continue;
        };
        rows.push((flow.id, wf.module_hash.clone()));
        let server_hash = state.fingerprints.get(&flow.source_dir, &flow.module);
        if server_hash.as_deref() == Some(wf.module_hash.as_str()) && wf.module == flow.module {
            eligible.insert(flow.id);
        } else if !drift.contains(&flow.module) {
            drift.push(flow.module.clone());
        }
    }
    drift.sort();
    refused.sort();
    Ok(Evaluation {
        rows,
        eligible,
        drift,
        refused,
    })
}

/// The eligible set and drift from the fingerprints a worker last reported,
/// against the server's current code.
fn evaluate_stored(state: &AppState, worker_id: i64) -> ApiResult<(HashSet<i64>, Vec<String>)> {
    let flows: HashMap<i64, cereyan_core::Flow> = state
        .store
        .list_flows(None)?
        .into_iter()
        .map(|f| (f.id, f))
        .collect();
    let mut eligible = HashSet::new();
    let mut drift = Vec::new();
    for (wid, flow_id, hash) in state.store.all_worker_flows()? {
        if wid != worker_id {
            continue;
        }
        let Some(flow) = flows.get(&flow_id) else {
            continue;
        };
        if state
            .fingerprints
            .get(&flow.source_dir, &flow.module)
            .as_deref()
            == Some(hash.as_str())
        {
            eligible.insert(flow_id);
        } else if !drift.contains(&flow.module) {
            drift.push(flow.module.clone());
        }
    }
    drift.sort();
    Ok((eligible, drift))
}

#[utoipa::path(post, path = "/api/workers/register", request_body = RegisterBody,
    responses((status = 200, body = RegisterResponse), (status = 403, description = "The server has no token"), (status = 409, description = "Another major version")))]
pub async fn register(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<RegisterBody>,
) -> ApiResult<Json<RegisterResponse>> {
    if state.config.token.is_none() {
        return Err(ApiError::Forbidden(
            "remote workers need a server token: set [server] token or start serve with --token"
                .into(),
        ));
    }
    if major(&body.version) != major(&state.config.version) {
        return Err(ApiError::Conflict(json!({
            "error": format!(
                "worker version {} and server version {} differ in their major version",
                body.version, state.config.version
            )
        })));
    }
    if body.name.trim().is_empty() {
        return Err(ApiError::Unprocessable("a worker needs a name".into()));
    }
    let st = state.clone();
    let response = tokio::task::spawn_blocking(move || -> ApiResult<RegisterResponse> {
        let Evaluation {
            rows,
            eligible,
            drift,
            refused,
        } = evaluate(&st, &body.flows)?;
        let mut meta = body.meta;
        if let Some(addr) = client_address(&headers) {
            meta.insert("address".into(), json!(addr));
        }
        let cpus = body.cpus.max(1);
        let worker = st.store.register_worker(WorkerRegistration {
            name: body.name.trim().to_string(),
            version: body.version,
            cpus,
            processors: body.processors.clamp(1, cpus),
            labels: body.labels,
            shared_paths: body.shared_paths,
            meta,
        })?;
        st.store.set_worker_flows(worker.id, rows)?;
        st.supervisor.sync_worker(
            worker.id,
            &worker.name,
            worker.processors as usize,
            &worker.state,
            eligible,
        );
        let _ = st.record_engine_event(
            EventName::WorkerRegistered,
            None,
            None,
            json!({"worker_id": worker.id, "name": worker.name, "version": worker.version,
                   "refused": refused, "drift": drift}),
        );
        st.stream.publish(
            "worker.updated",
            worker.id.to_string(),
            json!({"id": worker.id}),
        );
        Ok(RegisterResponse {
            worker_id: worker.id,
            heartbeat_secs: HEARTBEAT_SECS,
            state: worker.state,
            refused,
            drift,
        })
    })
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))??;
    // Queued runs may now fit on the new worker.
    state.supervisor.ensure_capacity(&state);
    Ok(Json(response))
}

#[utoipa::path(post, path = "/api/workers/{id}/heartbeat", params(("id" = i64, Path)), request_body = HeartbeatBody,
    responses((status = 200, body = HeartbeatResponse), (status = 409, description = "Unknown to this server: register again")))]
pub async fn heartbeat(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Json(body): Json<HeartbeatBody>,
) -> ApiResult<Json<HeartbeatResponse>> {
    let st = state.clone();
    let out = tokio::task::spawn_blocking(move || -> ApiResult<HeartbeatResponse> {
        let Some(worker) = st.store.get_worker(id)? else {
            return Err(ApiError::Conflict(
                json!({"error": "unknown worker", "register": true}),
            ));
        };
        if let Some(flows) = &body.flows {
            st.store.set_worker_flows(id, evaluate(&st, flows)?.rows)?;
            st.supervisor.clear_broken(id);
        }
        let (eligible, drift) = evaluate_stored(&st, id)?;
        let state_now = if worker.state == "offline" {
            "online".to_string()
        } else {
            worker.state.clone()
        };
        st.supervisor.sync_worker(
            id,
            &worker.name,
            worker.processors as usize,
            &state_now,
            eligible,
        );
        let Some(commands) = st.supervisor.worker_heartbeat(id, &body.engines) else {
            return Err(ApiError::Conflict(
                json!({"error": "unknown worker", "register": true}),
            ));
        };
        let mut meta = body.meta;
        meta.insert("drift".into(), json!(drift));
        st.store.touch_worker(id, meta)?;
        if worker.state == "offline" {
            st.stream
                .publish("worker.updated", id.to_string(), json!({"id": id}));
        }
        Ok(HeartbeatResponse {
            commands,
            state: state_now,
            drift,
        })
    })
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))??;
    Ok(Json(out))
}

fn views(state: &AppState) -> ApiResult<Vec<WorkerView>> {
    let live: HashMap<i64, (String, usize, usize)> = state
        .supervisor
        .workers_snapshot()
        .into_iter()
        .map(|(id, _, st, _, running, idle)| (id, (st, running, idle)))
        .collect();
    let mut flows_per: HashMap<i64, usize> = HashMap::new();
    for (wid, _, _) in state.store.all_worker_flows()? {
        *flows_per.entry(wid).or_default() += 1;
    }
    let mut out = Vec::new();
    for mut worker in state.store.list_workers()? {
        let (running, idle) = match live.get(&worker.id) {
            Some((st, running, idle)) => {
                worker.state = st.clone();
                (*running, *idle)
            }
            None => (0, 0),
        };
        let drift: Vec<String> = worker
            .meta
            .get("drift")
            .and_then(|d| serde_json::from_value(d.clone()).ok())
            .unwrap_or_default();
        let flows = flows_per
            .get(&worker.id)
            .copied()
            .unwrap_or(0)
            .saturating_sub(drift.len());
        out.push(WorkerView {
            worker,
            running,
            idle,
            flows,
            drift,
        });
    }
    Ok(out)
}

#[utoipa::path(get, path = "/api/workers", responses((status = 200, body = Vec<WorkerView>)))]
pub async fn list(State(state): State<Arc<AppState>>) -> ApiResult<Json<Vec<WorkerView>>> {
    Ok(Json(views(&state)?))
}

#[derive(Deserialize, utoipa::IntoParams)]
pub struct TimelineQuery {
    /// Hours back from now (default 6, at most 48).
    #[serde(default)]
    pub hours: Option<u32>,
}

/// One run on a processor lane.
#[derive(Serialize, utoipa::ToSchema)]
pub struct TimelineRun {
    pub run_id: i64,
    pub flow: String,
    pub processor: Option<i64>,
    /// The run's state type; `null` for a run not started yet.
    pub state: Option<String>,
    pub start: Option<i64>,
    pub end: Option<i64>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct Timeline {
    pub host: String,
    pub since: i64,
    pub runs: Vec<TimelineRun>,
    /// A forecast, not an assignment: runs in line or due within the hour that
    /// this host can take, soonest first.
    pub next: Vec<TimelineRun>,
}

#[utoipa::path(get, path = "/api/workers/{id}/timeline", params(("id" = i64, Path, description = "A worker id, or 0 for the server"), TimelineQuery),
    responses((status = 200, body = Timeline), (status = 404)))]
pub async fn timeline(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Query(q): Query<TimelineQuery>,
) -> ApiResult<Json<Timeline>> {
    let host = if id == 0 {
        "server".to_string()
    } else {
        state
            .store
            .get_worker(id)?
            .ok_or_else(|| ApiError::NotFound("worker not found".into()))?
            .name
    };
    let since = now_micros() - i64::from(q.hours.unwrap_or(6).min(48)) * 3_600_000_000;
    let runs = state
        .store
        .runs_on_host(&host, since, 2000)?
        .into_iter()
        .map(|(run_id, flow, processor, st, start, end)| TimelineRun {
            run_id,
            flow,
            processor,
            state: st,
            start,
            end,
        })
        .collect();
    // What it may take next: remote-eligible runs whose code it has, or, for
    // the server, anything.
    let eligible: Option<HashSet<i64>> = if id == 0 {
        None
    } else {
        Some(evaluate_stored(&state, id)?.0)
    };
    let (_, line, _) = state.supervisor.queue_snapshot(200);
    let queued: HashSet<i64> = line.iter().map(|l| l.run_id).collect();
    let until = now_micros() + 3_600_000_000;
    let mut candidates: Vec<cereyan_core::Run> = state
        .store
        .get_runs(&queued.iter().copied().collect::<Vec<_>>())?;
    candidates.extend(state.store.scheduled_between(now_micros(), until, 100)?);
    candidates.sort_by_key(|r| (r.scheduled_time.unwrap_or(r.created_at), r.id));
    candidates.dedup_by_key(|r| r.id);
    let flows: HashMap<i64, cereyan_core::Flow> = state
        .store
        .list_flows(None)?
        .into_iter()
        .map(|f| (f.id, f))
        .collect();
    let next = candidates
        .into_iter()
        .filter(|r| match &eligible {
            None => true,
            Some(set) => {
                set.contains(&r.flow_id)
                    && flows.get(&r.flow_id).is_some_and(|f| {
                        cereyan_core::FlowOptions::from_map(&f.options).may_run_remotely()
                    })
            }
        })
        .take(20)
        .map(|r| TimelineRun {
            run_id: r.id,
            flow: r.flow_name,
            processor: None,
            state: None,
            start: r.scheduled_time.or(Some(r.created_at)),
            end: None,
        })
        .collect();
    Ok(Json(Timeline {
        host,
        since,
        runs,
        next,
    }))
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct WorkerPatch {
    /// Engines the worker may run at once, 1 up to its CPU count.
    pub processors: i64,
}

#[utoipa::path(patch, path = "/api/workers/{id}", params(("id" = i64, Path)), request_body = WorkerPatch,
    responses((status = 200, body = Vec<WorkerView>), (status = 404), (status = 422)))]
pub async fn patch(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Json(body): Json<WorkerPatch>,
) -> ApiResult<Json<Vec<WorkerView>>> {
    let worker = state
        .store
        .get_worker(id)?
        .ok_or_else(|| ApiError::NotFound("worker not found".into()))?;
    if body.processors < 1 || body.processors > worker.cpus {
        return Err(ApiError::Unprocessable(format!(
            "processors must be between 1 and {}, the worker's CPU count",
            worker.cpus
        )));
    }
    state.store.set_worker_processors(id, body.processors)?;
    state
        .supervisor
        .set_worker_processors(id, body.processors as usize);
    state.supervisor.ensure_capacity(&state);
    Ok(Json(views(&state)?))
}

async fn set_state(state: Arc<AppState>, id: i64, to: &str) -> ApiResult<Json<Vec<WorkerView>>> {
    let worker = state
        .store
        .get_worker(id)?
        .ok_or_else(|| ApiError::NotFound("worker not found".into()))?;
    if worker.state == "offline" {
        return Err(ApiError::Conflict(
            json!({"error": "the worker is offline"}),
        ));
    }
    state.store.set_worker_state(id, to)?;
    state.supervisor.set_worker_state(id, to);
    state.supervisor.command_worker(
        id,
        json!({"cmd": if to == "draining" { "drain" } else { "resume" }}),
    );
    state
        .stream
        .publish("worker.updated", id.to_string(), json!({"id": id}));
    state.supervisor.ensure_capacity(&state);
    Ok(Json(views(&state)?))
}

#[utoipa::path(post, path = "/api/workers/{id}/drain", params(("id" = i64, Path)), responses((status = 200, body = Vec<WorkerView>), (status = 404), (status = 409)))]
pub async fn drain(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> ApiResult<Json<Vec<WorkerView>>> {
    set_state(state, id, "draining").await
}

#[utoipa::path(post, path = "/api/workers/{id}/resume", params(("id" = i64, Path)), responses((status = 200, body = Vec<WorkerView>), (status = 404), (status = 409)))]
pub async fn resume(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> ApiResult<Json<Vec<WorkerView>>> {
    set_state(state, id, "online").await
}

#[utoipa::path(delete, path = "/api/workers/{id}", params(("id" = i64, Path)), responses((status = 204), (status = 404), (status = 409, description = "The worker is not offline")))]
pub async fn forget(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    let worker = state
        .store
        .get_worker(id)?
        .ok_or_else(|| ApiError::NotFound("worker not found".into()))?;
    let live_state = state
        .supervisor
        .workers_snapshot()
        .into_iter()
        .find(|w| w.0 == id)
        .map(|w| w.2)
        .unwrap_or(worker.state);
    if live_state != "offline" {
        return Err(ApiError::Conflict(json!({
            "error": "only an offline worker can be forgotten; drain it and stop its process first"
        })));
    }
    state.supervisor.forget_worker(id);
    state.store.delete_worker(id)?;
    state.stream.publish(
        "worker.updated",
        id.to_string(),
        json!({"id": id, "deleted": true}),
    );
    Ok(StatusCode::NO_CONTENT)
}
