//! Endpoints used by engine children: pull work, report batches, heartbeat,
//! and import failures.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::Json;
use cereyan_core::{State as RunState, StateType};
use cereyan_store::ReportEvent;
use serde::{Deserialize, Serialize};

use super::error::{ApiError, ApiResult};
use crate::state::AppState;
use crate::supervisor::{EngineKey, WorkDecision, WorkItem};
use serde_json::json;

#[derive(Deserialize, utoipa::ToSchema)]
pub struct WorkRequest {
    pub engine_id: String,
    pub pid: u32,
    pub source_dir: String,
    pub module: String,
    #[serde(default)]
    pub isolated: bool,
    #[serde(default)]
    pub nice: u8,
    /// Long-poll wait in milliseconds (max 30000).
    #[serde(default)]
    pub wait_ms: u64,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct WorkResponse {
    #[serde(default)]
    pub run: Option<WorkItem>,
    #[serde(default)]
    pub exit: bool,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct ReportRequest {
    pub engine_id: String,
    pub run_id: i64,
    #[schema(value_type = Vec<Object>)]
    pub events: Vec<ReportEvent>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct ReportResponse {
    pub applied: usize,
    pub skipped: usize,
    pub last_seq: i64,
    pub cancel: bool,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct HeartbeatRequest {
    pub engine_id: String,
    pub run_id: i64,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct FailedRequest {
    pub engine_id: String,
    pub source_dir: String,
    pub module: String,
    #[serde(default)]
    pub isolated: bool,
    #[serde(default)]
    pub nice: u8,
    pub traceback: String,
}

async fn build_work_item(state: &Arc<AppState>, run_id: i64) -> ApiResult<Option<WorkItem>> {
    let Some(run) = state.store.get_run(run_id)? else {
        return Ok(None);
    };
    if run.state.is_terminal() {
        return Ok(None);
    }
    let Some(flow) = state.store.get_flow(run.flow_id)? else {
        return Ok(None);
    };
    Ok(Some(WorkItem {
        kind: "run".into(),
        run_id: run.id,
        run_name: run.name.clone(),
        external_id: run.external_id.to_string(),
        project: flow.project,
        flow: flow.name,
        parameters: serde_json::Value::Object(run.parameters.clone()),
        options: serde_json::Value::Object(flow.options.clone()),
        cancel_requested: state
            .index
            .get(run.id)
            .map(|r| r.cancel_requested)
            .unwrap_or(false),
        payload: serde_json::Value::Null,
    }))
}

async fn build_job_item(
    state: &Arc<AppState>,
    payload: serde_json::Value,
) -> ApiResult<Option<WorkItem>> {
    let kind = payload
        .get("kind")
        .and_then(|k| k.as_str())
        .unwrap_or("")
        .to_string();
    let run_id = payload.get("run_id").and_then(|v| v.as_i64()).unwrap_or(0);
    let run = if run_id > 0 {
        state.store.get_run(run_id)?
    } else {
        None
    };
    let flow_id = match (&run, payload.get("flow_id").and_then(|v| v.as_i64())) {
        (Some(r), _) => r.flow_id,
        (None, Some(f)) => f,
        _ => return Ok(None),
    };
    let Some(flow) = state.store.get_flow(flow_id)? else {
        return Ok(None);
    };
    Ok(Some(WorkItem {
        kind,
        run_id,
        run_name: run.as_ref().map(|r| r.name.clone()).unwrap_or_default(),
        external_id: run
            .as_ref()
            .map(|r| r.external_id.to_string())
            .unwrap_or_default(),
        project: flow.project,
        flow: flow.name,
        parameters: run
            .as_ref()
            .map(|r| serde_json::Value::Object(r.parameters.clone()))
            .unwrap_or(serde_json::Value::Object(Default::default())),
        options: serde_json::Value::Object(flow.options.clone()),
        cancel_requested: false,
        payload,
    }))
}

#[utoipa::path(post, path = "/api/engine/work", request_body = WorkRequest, responses((status = 200, body = WorkResponse)))]
pub async fn work(
    State(state): State<Arc<AppState>>,
    Json(req): Json<WorkRequest>,
) -> ApiResult<Json<WorkResponse>> {
    let key = EngineKey {
        source_dir: req.source_dir,
        module: req.module,
        isolated: req.isolated,
        nice: req.nice,
    };
    let deadline = tokio::time::Instant::now() + Duration::from_millis(req.wait_ms.min(30_000));
    let url = format!("http://{}", state.addr);
    loop {
        if state
            .shutting_down
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            // The server is stopping: tell the engine to exit rather than to keep asking.
            // begin_shutdown wakes every parked long-poll so each one lands here.
            return Ok(Json(WorkResponse {
                run: None,
                exit: true,
            }));
        }
        let decision = state
            .supervisor
            .take_work(&req.engine_id, req.pid, &key, &url);
        match decision {
            WorkDecision::Exit => {
                return Ok(Json(WorkResponse {
                    run: None,
                    exit: true,
                }))
            }
            WorkDecision::Job(payload) => match build_job_item(&state, payload).await? {
                Some(item) => {
                    return Ok(Json(WorkResponse {
                        run: Some(item),
                        exit: false,
                    }))
                }
                None => continue,
            },
            WorkDecision::Run(run_id) => {
                let st = state.clone();
                let engine_id = req.engine_id.clone();
                let pid = req.pid as i64;
                tokio::task::spawn_blocking(move || {
                    st.store
                        .set_run_engine(run_id, Some(pid), Some(engine_id.clone()))
                })
                .await
                .map_err(|e| ApiError::Internal(e.to_string()))??;
                state.index.update(run_id, |r| {
                    r.engine_pid = Some(req.pid);
                    r.engine_id = Some(req.engine_id.clone());
                    r.last_heartbeat = std::time::Instant::now();
                });
                match build_work_item(&state, run_id).await? {
                    Some(item) => {
                        return Ok(Json(WorkResponse {
                            run: Some(item),
                            exit: false,
                        }))
                    }
                    None => {
                        state.supervisor.run_finished(run_id);
                        continue;
                    }
                }
            }
            WorkDecision::Wait => {
                let now = tokio::time::Instant::now();
                if now >= deadline {
                    return Ok(Json(WorkResponse {
                        run: None,
                        exit: false,
                    }));
                }
                let notified = state.supervisor.notify.notified();
                let mut shutdown = state.shutdown.clone();
                tokio::select! {
                    _ = tokio::time::timeout_at(deadline, notified) => {}
                    _ = shutdown.changed() => {
                        return Ok(Json(WorkResponse { run: None, exit: false }));
                    }
                }
            }
        }
    }
}

#[utoipa::path(post, path = "/api/engine/report", request_body = ReportRequest, responses((status = 200, body = ReportResponse), (status = 404)))]
pub async fn report(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ReportRequest>,
) -> ApiResult<Json<ReportResponse>> {
    let st = state.clone();
    let run_id = req.run_id;
    // Track previous task-run states for the counters.
    let outcome = tokio::task::spawn_blocking(move || st.store.apply_report(run_id, req.events))
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))??;
    for t in &outcome.task_runs {
        state.publish_task_run(t);
    }
    // One event per accepted transition, not just per final state, appended
    // in a single writer round trip and then published in order.
    let by_ext: std::collections::HashMap<_, _> = outcome
        .task_runs
        .iter()
        .map(|t| (t.external_id, t))
        .collect();
    let task_events: Vec<_> = outcome
        .task_run_states
        .iter()
        .filter_map(|(ext, st)| {
            let mut snapshot = (*by_ext.get(ext)?).clone();
            snapshot.state = st.clone();
            crate::events::task_run_event(&snapshot)
        })
        .collect();
    let st = state.clone();
    let appended = tokio::task::spawn_blocking(move || st.store.append_events(task_events))
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))??;
    let st = state.clone();
    let custom_ids = outcome.event_ids.clone();
    tokio::task::spawn_blocking(move || {
        for (eid, _) in appended {
            st.after_event(eid);
        }
        for eid in custom_ids {
            st.after_event(eid);
        }
    })
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    for aid in &outcome.artifact_ids {
        if let Ok(Some(a)) = state.store.get_artifact(*aid) {
            state.stream.publish(
                "artifact.updated",
                aid.to_string(),
                serde_json::to_value(&a).unwrap_or_default(),
            );
        }
    }
    for (prev, next) in &outcome.task_run_transitions {
        if let Some(next) = StateType::parse(next) {
            state
                .index
                .task_run_transition(prev.as_deref().and_then(StateType::parse), next);
        }
    }
    if outcome.log_count > 0 {
        if let Some(last) = outcome.last_log_id {
            state.publish_logs(run_id, last, outcome.log_count);
        }
    }
    let cancel = state.index.heartbeat(run_id).unwrap_or(false);
    Ok(Json(ReportResponse {
        applied: outcome.applied,
        skipped: outcome.skipped,
        last_seq: outcome.last_seq,
        cancel,
    }))
}

#[utoipa::path(post, path = "/api/engine/heartbeat", request_body = HeartbeatRequest, responses((status = 200, description = "{cancel: bool, active: bool}")))]
pub async fn heartbeat(
    State(state): State<Arc<AppState>>,
    Json(req): Json<HeartbeatRequest>,
) -> Json<serde_json::Value> {
    match state.index.heartbeat(req.run_id) {
        Some(cancel) => Json(serde_json::json!({"cancel": cancel, "active": true})),
        None => Json(serde_json::json!({"cancel": false, "active": false})),
    }
}

#[utoipa::path(post, path = "/api/engine/failed", request_body = FailedRequest, responses((status = 200, description = "Queued runs of the module failed")))]
pub async fn failed(
    State(state): State<Arc<AppState>>,
    Json(req): Json<FailedRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let key = EngineKey {
        source_dir: req.source_dir.clone(),
        module: req.module.clone(),
        isolated: req.isolated,
        nice: req.nice,
    };
    let runs = state.supervisor.take_queued_for_key(&key, &req.engine_id);
    let summary = req
        .traceback
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("import failed")
        .to_string();
    let st = state.clone();
    let traceback = req.traceback.clone();
    let failed_runs = runs.clone();
    tokio::task::spawn_blocking(move || {
        for run_id in failed_runs {
            let mut s = RunState::new(StateType::Failed).with_message(summary.clone());
            s.details.insert(
                "traceback".into(),
                serde_json::Value::String(traceback.clone()),
            );
            let _ = st.transition_run(run_id, s, true);
        }
        if let Ok(flows) = st.store.list_flows(None) {
            for f in flows {
                if f.module == req.module && f.source_dir == req.source_dir {
                    let _ = st.store.set_flow_error(f.id, Some(summary.clone()));
                    if let Ok(Some(updated)) = st.store.get_flow(f.id) {
                        st.stream.publish(
                            "flow.registered",
                            f.id.to_string(),
                            serde_json::to_value(super::flows::decorate(&st, updated))
                                .unwrap_or_default(),
                        );
                    }
                }
            }
        }
    })
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(serde_json::json!({"failed_runs": runs})))
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct AcquireRequest {
    pub run_id: i64,
    #[schema(value_type = Object)]
    pub resources: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    pub wait_ms: u64,
}

#[utoipa::path(post, path = "/api/resources/acquire", request_body = AcquireRequest, responses((status = 200, description = "{lease: id} or {waiting: name}")))]
pub async fn acquire(
    State(state): State<Arc<AppState>>,
    Json(req): Json<AcquireRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let needs: Vec<(String, f64)> = req
        .resources
        .iter()
        .filter_map(|(k, v)| v.as_f64().map(|n| (k.clone(), n)))
        .collect();
    let deadline = tokio::time::Instant::now() + Duration::from_millis(req.wait_ms.min(30_000));
    loop {
        match state.supervisor.try_acquire(req.run_id, &needs) {
            Ok(lease) => return Ok(Json(json!({"lease": lease}))),
            Err(blocking) => {
                if tokio::time::Instant::now() >= deadline {
                    return Ok(Json(json!({"waiting": blocking})));
                }
                let notified = state.supervisor.notify.notified();
                let _ = tokio::time::timeout_at(deadline, notified).await;
            }
        }
    }
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct ReleaseRequest {
    pub run_id: i64,
    pub lease: u64,
}

#[utoipa::path(post, path = "/api/resources/release", request_body = ReleaseRequest, responses((status = 200)))]
pub async fn release(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ReleaseRequest>,
) -> Json<serde_json::Value> {
    state.supervisor.release_lease(req.run_id, req.lease);
    Json(json!({"ok": true}))
}
