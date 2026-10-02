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
    /// What ran on the worker since it started, for its status page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stats: Option<HeartbeatStats>,
}

/// Runs of one flow that started on a worker since it started.
#[derive(Serialize, utoipa::ToSchema)]
pub struct FlowStats {
    pub flow: String,
    pub completed: i64,
    pub failed: i64,
    pub crashed: i64,
    pub cancelled: i64,
    pub running: i64,
    /// When the latest completed run here ended (microseconds).
    pub last_completed_at: Option<i64>,
}

/// One of the worker's engines and the run it is executing.
#[derive(Serialize, utoipa::ToSchema)]
pub struct EngineStats {
    pub engine_id: String,
    /// The processor slot, from 1.
    pub slot: usize,
    pub module: String,
    /// `starting`, `idle`, `running` or `draining`.
    pub status: String,
    pub run_id: Option<i64>,
    pub flow: Option<String>,
    /// Seconds in the current run, or since the engine started when it has none.
    pub since_secs: u64,
}

/// Counts the server keeps for a worker: what its status page shows.
#[derive(Serialize, utoipa::ToSchema)]
pub struct HeartbeatStats {
    /// The worker's start time (microseconds): runs that started before it are not counted.
    pub since: i64,
    pub by_flow: Vec<FlowStats>,
    pub engines: Vec<EngineStats>,
}

/// At most this many flows in a heartbeat's stats.
const STATS_FLOWS: usize = 200;

/// Runs by flow on the worker since its reported start, and its engines now.
fn stats(state: &AppState, worker: &Worker) -> ApiResult<HeartbeatStats> {
    let since = worker
        .meta
        .get("started_at")
        .and_then(Value::as_i64)
        .unwrap_or(worker.registered_at);
    let by_flow = state
        .store
        .runs_by_flow_on_host(&worker.name, since, STATS_FLOWS)?
        .into_iter()
        .map(|c| FlowStats {
            flow: c.flow,
            completed: c.completed,
            failed: c.failed,
            crashed: c.crashed,
            cancelled: c.cancelled,
            running: c.running,
            last_completed_at: c.last_completed_at,
        })
        .collect();
    let views = state.supervisor.worker_engines(worker.id);
    let run_ids: Vec<i64> = views.iter().filter_map(|e| e.run_id).collect();
    let flows: HashMap<i64, String> = if run_ids.is_empty() {
        HashMap::new()
    } else {
        // Two columns per engine's run. `get_runs` would read a whole run --
        // 37 columns, a correlated task-count aggregate and four decoded JSON
        // documents -- for one name, every five seconds per worker.
        state.store.flow_names(&run_ids)
    };
    let engines = views
        .into_iter()
        .map(|e| EngineStats {
            flow: e.run_id.and_then(|id| flows.get(&id).cloned()),
            engine_id: e.id,
            slot: e.slot,
            module: e.module,
            status: e.status.to_string(),
            run_id: e.run_id,
            since_secs: e.since_secs,
        })
        .collect();
    Ok(HeartbeatStats {
        since,
        by_flow,
        engines,
    })
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
    // One read of this worker's rows, joined to the two flow columns the
    // fingerprint needs. The heartbeat runs every few seconds per worker, so
    // reading the whole flow and worker_flow tables here was the cost.
    let mut eligible = HashSet::new();
    let mut drift: Vec<String> = Vec::new();
    for (flow_id, source_dir, module, hash) in state.store.worker_flow_details(worker_id)? {
        if state
            .fingerprints
            .get(&source_dir, &module)
            .as_deref()
            == Some(hash.as_str())
        {
            eligible.insert(flow_id);
        } else if !drift.contains(&module) {
            drift.push(module);
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
    // The queue, the timeline and the UI name the server's own processors
    // "server"; a worker by that name would merge into its row.
    if body.name.trim().eq_ignore_ascii_case("server") {
        return Err(ApiError::Unprocessable(
            "\"server\" names the server's own processors; start the worker with another --name".into(),
        ));
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
        // The state is read again under the lock a drain or resume takes, so
        // one landing since `worker` was read is not overwritten with the old
        // state until the next heartbeat.
        let state_lock = worker_state_lock();
        let stored = st
            .store
            .get_worker(id)?
            .map(|w| w.state)
            .unwrap_or_else(|| worker.state.clone());
        let state_now = if stored == "offline" {
            "online".to_string()
        } else {
            stored
        };
        st.supervisor.sync_worker(
            id,
            &worker.name,
            worker.processors as usize,
            &state_now,
            eligible,
        );
        drop(state_lock);
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
            stats: Some(stats(&st, &worker)?),
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
    let flows_per: HashMap<i64, i64> = state.store.worker_flow_counts()?.into_iter().collect();
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
        // A flow in drift is claimed but not eligible, so it is subtracted: the
        // number shown is the eligible count.
        let flows = flows_per
            .get(&worker.id)
            .copied()
            .unwrap_or(0)
            .saturating_sub(drift.len() as i64)
            .max(0) as usize;
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
#[derive(Serialize, PartialEq, Eq, Debug, utoipa::ToSchema)]
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

/// The runs a host might take next: queued and nearly-due, filtered by
/// eligibility, capped at twenty.
///
/// Extracted so both branches can be tested. The flow rows are read **only when
/// the eligibility filter will consult them** — the server's own timeline passes
/// `None`, whose filter arm is `true`, so reading them there was 2 ms per request
/// with no effect on the response.
fn timeline_next(
    state: &AppState,
    eligible: Option<&HashSet<i64>>,
) -> Result<Vec<TimelineRun>, cereyan_store::StoreError> {
    // A host with no eligibility filter is offered everything.
    let (_, line, _) = state.supervisor.queue_snapshot(200);
    let queued: HashSet<i64> = line.iter().map(|l| l.run_id).collect();
    let until = now_micros() + 3_600_000_000;
    // Five columns per candidate: the id, the flow name, the flow it belongs to
    // and when it is due. Whole runs would mean `RUN_COLUMNS` -- 37 columns and a
    // correlated task-count aggregate each -- for at most twenty rows kept out of
    // a few hundred.
    let mut candidates = state
        .store
        .timeline_run_rows(&queued.iter().copied().collect::<Vec<_>>())?;
    candidates.extend(
        state
            .store
            .scheduled_timeline_run_rows(now_micros(), until, 100)?,
    );
    // Read the flows only when the filter will consult them. The server's own
    // timeline (`id == 0`) filters on `None => true` and never looks at them, so
    // reading the whole flow table there was 2 ms of work per request with no
    // effect on the response. Even for a worker, only the candidates' own flows
    // can be looked up.
    let flows: HashMap<i64, serde_json::Map<String, serde_json::Value>> = if eligible.is_some() {
        let mut flow_ids: Vec<i64> = candidates.iter().map(|r| r.flow_id).collect();
        flow_ids.sort_unstable();
        flow_ids.dedup();
        state.store.flow_options_by_id(&flow_ids)?
    } else {
        HashMap::new()
    };
    Ok(timeline_next_from(candidates, eligible, &flows))
}

/// The forecast from a set of candidates, an eligibility filter and the flows
/// read for it.
///
/// Split out from the reads so the filter can be exercised with a *populated*
/// flow map and an empty one. The point of `eligible == None` is that the map is
/// never consulted, and that is only checkable if a test can hand this function a
/// full map and still get the same answer.
///
/// The ordering and the dedup live here rather than at the call site, so that the
/// whole forecast — sort key, duplicate removal, eligibility, cap — is one thing
/// with one set of tests.
fn timeline_next_from(
    mut candidates: Vec<cereyan_store::TimelineRunRow>,
    eligible: Option<&HashSet<i64>>,
    flows: &HashMap<i64, serde_json::Map<String, serde_json::Value>>,
) -> Vec<TimelineRun> {
    candidates.sort_by_key(|r| (r.scheduled_time.unwrap_or(r.created_at), r.id));
    candidates.dedup_by_key(|r| r.id);
    candidates
        .into_iter()
        .filter(|r| match &eligible {
            None => true,
            Some(set) => {
                set.contains(&r.flow_id)
                    && flows.get(&r.flow_id).is_some_and(|options| {
                        cereyan_core::FlowOptions::from_map(options).may_run_remotely()
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
        .collect()
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
    let next = timeline_next(&state, eligible.as_ref())?;
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

/// Held while a worker's state is written to the store and the supervisor, and
/// while a heartbeat reads it and syncs the supervisor, so the two never
/// interleave.
static WORKER_STATE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn worker_state_lock() -> std::sync::MutexGuard<'static, ()> {
    WORKER_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner())
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
    {
        let _state_lock = worker_state_lock();
        state.store.set_worker_state(id, to)?;
        state.supervisor.set_worker_state(id, to);
    }
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

/// The worker is stopping cleanly: it turns offline now. Its own shutdown
/// drain is not left behind, so it registers online when it comes back; a
/// drain an operator set before the shutdown is kept by the worker not
/// calling this.
#[utoipa::path(post, path = "/api/workers/{id}/leave", params(("id" = i64, Path)), responses((status = 204), (status = 404)))]
pub async fn leave(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    state
        .store
        .get_worker(id)?
        .ok_or_else(|| ApiError::NotFound("worker not found".into()))?;
    {
        let _state_lock = worker_state_lock();
        state.store.set_worker_state(id, "offline")?;
        state.supervisor.worker_left(id);
    }
    let _ = state.record_engine_event(
        EventName::WorkerOffline,
        None,
        None,
        json!({"worker_id": id, "name": state.supervisor.worker_name(id), "left": true}),
    );
    state
        .stream
        .publish("worker.updated", id.to_string(), json!({"id": id}));
    state.supervisor.ensure_capacity(&state);
    Ok(StatusCode::NO_CONTENT)
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

#[cfg(test)]
pub(crate) mod timeline_tests {
    use super::*;
    use cereyan_core::State;
    use cereyan_core::StateType;
    use cereyan_store::CreateRun;
    use cereyan_store::TimelineRunRow;
    use serde_json::json;
    use tempfile::TempDir;

    /// `created_at` is given explicitly so a run with no scheduled time can be
    /// placed *between* two that have one -- which is the only way to show the
    /// fallback interleaves by time rather than being appended.
    fn row_at(id: i64, flow_id: i64, flow_name: &str, due: Option<i64>, created: i64) -> TimelineRunRow {
        TimelineRunRow {
            id,
            flow_name: flow_name.to_string(),
            flow_id,
            scheduled_time: due,
            created_at: created,
        }
    }

    fn row(id: i64, flow_id: i64, flow_name: &str, due: Option<i64>) -> TimelineRunRow {
        row_at(id, flow_id, flow_name, due, 100 + id)
    }

    fn options(pairs: &[(&str, &str)]) -> serde_json::Map<String, serde_json::Value> {
        let mut m = serde_json::Map::new();
        for (k, v) in pairs {
            m.insert((*k).to_string(), json!(v));
        }
        m
    }

    /// The load-bearing claim of this change: with no eligibility filter the flow
    /// map is **not consulted**, so a full map and an empty one give the same
    /// forecast. Before the change the whole flow table was read to build that map
    /// and then thrown away — 2 ms per request on 2,000 flows.
    #[test]
    fn the_servers_own_forecast_does_not_consult_the_flows() {
        let candidates = vec![
            row_at(1, 10, "local", Some(1_000), 50),
            row_at(2, 11, "elsewhere", None, 1_500),
            row_at(3, 12, "server-only", Some(2_000), 50),
        ];
        let full: HashMap<i64, serde_json::Map<String, serde_json::Value>> = [
            (10, options(&[])),
            (11, options(&[("runs_on", "worker")])),
            // Would be excluded if the map were consulted.
            (12, options(&[("runs_on", "server")])),
        ]
        .into_iter()
        .collect();

        let ids = |rows: Vec<TimelineRun>| -> Vec<(i64, String, Option<i64>)> {
            rows.into_iter()
                .map(|r| (r.run_id, r.flow, r.start))
                .collect()
        };
        assert_eq!(
            ids(timeline_next_from(candidates.clone(), None, &HashMap::new())),
            ids(timeline_next_from(candidates.clone(), None, &full)),
            "a populated flow map must not change the server's forecast"
        );
        let without_flows = ids(timeline_next_from(candidates.clone(), None, &HashMap::new()));
        assert_eq!(
            without_flows.iter().map(|r| r.0).collect::<Vec<_>>(),
            vec![1, 2, 3],
            "every candidate is offered, in due order, including the server-only flow"
        );
    }

    /// The same candidates, seen by a worker: now the map *is* consulted, and the
    /// flow that may not run remotely drops out. This is what makes the previous
    /// test meaningful — the map is not inert, it is simply unused in one branch.
    #[test]
    fn a_workers_forecast_drops_a_flow_that_may_not_run_remotely() {
        let candidates = vec![
            row(1, 10, "local", Some(1_000)),
            row(2, 12, "server-only", Some(2_000)),
            row(3, 13, "not-eligible", Some(3_000)),
        ];
        let flows: HashMap<i64, serde_json::Map<String, serde_json::Value>> = [
            (10, options(&[])),
            (12, options(&[("runs_on", "server")])),
            (13, options(&[])),
        ]
        .into_iter()
        .collect();
        let eligible: HashSet<i64> = [10, 12, 13].into_iter().collect();

        let shown: Vec<i64> = timeline_next_from(candidates.clone(), Some(&eligible), &flows)
            .iter()
            .map(|r| r.run_id)
            .collect();
        assert_eq!(
            shown,
            vec![1, 3],
            "flow 12 is server-only, and flow 13's run is not eligible"
        );
        // And a flow absent from the map is treated as ineligible, not as allowed.
        let mut without_13 = flows.clone();
        without_13.remove(&13);
        assert_eq!(
            timeline_next_from(candidates.clone(), Some(&eligible), &without_13)
                .iter()
                .map(|r| r.run_id)
                .collect::<Vec<_>>(),
            vec![1],
            "an unread flow offers nothing"
        );
        // A worker eligible for nothing is offered nothing.
        assert!(
            timeline_next_from(candidates, Some(&HashSet::new()), &flows).is_empty(),
            "no eligibility, no forecast"
        );
    }

    #[test]
    fn the_forecast_is_ordered_deduplicated_and_capped() {
        let flows = HashMap::new();
        // Due out of order, run 2 appearing twice (it is both queued and due), and
        // run 3 with no scheduled time at all -- due, by its creation time, between
        // the other two.
        let candidates = vec![
            row_at(1, 10, "c", Some(3_000), 10),
            row_at(2, 10, "a", Some(1_000), 10),
            row_at(3, 10, "b", None, 2_000),
            row_at(2, 10, "a", Some(1_000), 10),
        ];
        let shown = timeline_next_from(candidates.clone(), None, &flows);
        let ids: Vec<i64> = shown.iter().map(|r| r.run_id).collect();
        assert_eq!(
            ids,
            vec![2, 3, 1],
            "by due time; run 3's creation time places it between, and the duplicate is dropped"
        );

        // 25 candidates, cap 20.
        let many: Vec<TimelineRunRow> = (0..25).map(|i| row(i, 10, "f", Some(i))).collect();
        assert_eq!(
            timeline_next_from(many, None, &flows).len(),
            20,
            "the cap is unchanged"
        );
    }

    // ---- over a real store ------------------------------------------------

    fn state_with_scheduled_runs(dir: &TempDir) -> (Arc<AppState>, i64, i64) {
        let home = dir.path().join("home");
        let store = Arc::new(cereyan_store::Store::open(&home).unwrap());
        let mut upserts = Vec::new();
        for (name, runs_on) in [("remote-ok", None), ("server-only", Some("server"))] {
            let mut options = serde_json::Map::new();
            if let Some(r) = runs_on {
                options.insert("runs_on".into(), json!(r));
            }
            upserts.push(cereyan_store::UpsertFlow {
                project: "p".into(),
                name: name.into(),
                module: "m".into(),
                source_dir: "/tmp".into(),
                description: None,
                tags: "[]".into(),
                parameter_schema: "{}".into(),
                options: serde_json::Value::Object(options).to_string(),
                group: None,
                ..Default::default()
            });
        }
        let mut ids = Vec::new();
        for u in upserts {
            ids.push(store.upsert_flow_full(u).unwrap());
        }
        // Due inside the hour, so `scheduled_timeline_run_rows` finds them.
        let now = now_micros();
        for (i, flow_id) in ids.iter().enumerate() {
            store
                .create_run_full(CreateRun {
                    flow_id: *flow_id,
                    name: format!("run-{i}"),
                    parameters: "{}".into(),
                    tags: "[]".into(),
                    created_by: "test".into(),
                    initial_state: Some(State::new(StateType::Scheduled)),
                    scheduled_time: Some(now + 60_000_000 * (i as i64 + 1)),
                    ..Default::default()
                })
                .unwrap();
        }
        let config: crate::ServeConfig = serde_json::from_value(json!({
            "home": home.to_string_lossy(),
        }))
        .unwrap();
        let (_tx, rx) = tokio::sync::watch::channel(false);
        (
            Arc::new(
                AppState::new(config, store, None, None, "127.0.0.1:0".parse().unwrap(), rx)
                    .unwrap(),
            ),
            ids[0],
            ids[1],
        )
    }

    /// End to end over a real store: the server's own forecast offers both runs,
    /// including the one whose flow is server-only.
    #[test]
    fn the_servers_own_forecast_over_a_real_store_offers_everything() {
        let dir = TempDir::new().unwrap();
        let (state, remote_ok, _server_only) = state_with_scheduled_runs(&dir);
        let next = timeline_next(&state, None).unwrap();
        let names: Vec<&str> = next.iter().map(|r| r.flow.as_str()).collect();
        assert_eq!(
            names,
            vec!["remote-ok", "server-only"],
            "with no filter, both flows are offered, soonest first"
        );
    }

    /// The same store, seen by a worker eligible for both flows: the server-only
    /// one drops out. If this passes while the previous one fails to see a
    /// difference, the map is being consulted in exactly one branch.
    #[test]
    fn a_workers_forecast_over_a_real_store_drops_the_server_only_flow() {
        let dir = TempDir::new().unwrap();
        let (state, remote_ok, server_only) = state_with_scheduled_runs(&dir);
        let eligible: HashSet<i64> = [remote_ok, server_only].into_iter().collect();
        let next = timeline_next(&state, Some(&eligible)).unwrap();
        let names: Vec<&str> = next.iter().map(|r| r.flow.as_str()).collect();
        assert_eq!(names, vec!["remote-ok"], "a server-only flow is not remote work");

        // The runs the server sees are also the runs the worker draws from, so the
        // only difference between the two forecasts is the filter.
        let server = timeline_next(&state, None).unwrap();
        assert_eq!(server.len(), 2, "both are candidates");
        assert_eq!(next.len(), 1, "one survives the filter");
    }
}
