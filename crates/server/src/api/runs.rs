use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::{Extension, Json};
use cereyan_core::{Flow, FlowOptions, Run, State as RunState, StateType, TaskRun};
use cereyan_store::{CreateRun, ListRunsFilter, RunsPage, UpsertFlow};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::error::{ApiError, ApiResult};
use crate::state::{AppState, TransitionResult};
use crate::supervisor::EngineKey;
use crate::validate::validate_parameters;

#[derive(Deserialize, utoipa::ToSchema)]
pub struct CreateRunForFlowBody {
    #[serde(default)]
    #[schema(value_type = Object)]
    pub parameters: Map<String, Value>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Create a run by flow key, registering the flow when the server does not
/// know it yet (offline handoff from another project).
#[derive(Deserialize, utoipa::ToSchema)]
pub struct CreateRunBody {
    pub project: String,
    pub flow: String,
    #[serde(default)]
    pub module: Option<String>,
    #[serde(default)]
    pub source_dir: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    #[schema(value_type = Object)]
    pub parameter_schema: Option<Value>,
    #[serde(default)]
    #[schema(value_type = Object)]
    pub options: Option<Map<String, Value>>,
    #[serde(default)]
    pub flow_tags: Vec<String>,
    /// The flow's declared group; absent leaves the registered group as it is.
    #[serde(default)]
    pub flow_group: Option<String>,
    #[serde(default)]
    #[schema(value_type = Object)]
    pub parameters: Map<String, Value>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub created_by: Option<String>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct TransitionBody {
    #[serde(rename = "type")]
    pub state_type: StateType,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    #[schema(value_type = Object)]
    pub details: Map<String, Value>,
    #[serde(default)]
    pub force: bool,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct TransitionRejected {
    pub error: String,
    pub reason: String,
    pub current: Option<RunState>,
}

fn generate_name(store: &cereyan_store::Store) -> String {
    const A: &[&str] = &[
        "amber", "brave", "calm", "clever", "cosmic", "crisp", "daring", "eager", "fluent",
        "gentle", "golden", "humble", "jolly", "keen", "lively", "lucky", "merry", "mighty",
        "nimble", "noble", "patient", "polite", "proud", "quick", "quiet", "rapid", "serene",
        "sharp", "silent", "sleek", "smooth", "steady", "sturdy", "sunny", "swift", "tidy",
        "vivid", "witty", "zesty",
    ];
    const N: &[&str] = &[
        "badger", "beetle", "bison", "condor", "cougar", "coyote", "crane", "dolphin", "falcon",
        "ferret", "finch", "gecko", "heron", "ibis", "jackal", "koala", "lemur", "lynx", "macaw",
        "marmot", "meerkat", "narwhal", "newt", "ocelot", "osprey", "otter", "owl", "panda",
        "pelican", "puffin", "quail", "raven", "salmon", "sparrow", "stork", "tapir", "toucan",
        "walrus", "weasel", "wombat", "yak",
    ];
    let mut seed = cereyan_core::now_micros() as u64 ^ (std::process::id() as u64) << 32;
    for _ in 0..64 {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let name = format!(
            "{}-{}",
            A[(seed % A.len() as u64) as usize],
            N[((seed >> 16) % N.len() as u64) as usize]
        );
        if !store.run_name_exists(&name).unwrap_or(true) {
            return name;
        }
    }
    format!("run-{}", cereyan_core::now_micros())
}

pub async fn create_run_inner(
    state: &Arc<AppState>,
    flow: &Flow,
    parameters: Map<String, Value>,
    name: Option<String>,
    tags: Vec<String>,
    created_by: &str,
) -> ApiResult<Run> {
    if state
        .shutting_down
        .load(std::sync::atomic::Ordering::SeqCst)
    {
        return Err(ApiError::Conflict(
            serde_json::json!({"error": "server is shutting down"}),
        ));
    }
    super::database::refuse_while_resetting(state)?;
    validate_parameters(&flow.parameter_schema, &parameters).map_err(ApiError::Unprocessable)?;
    // Fill defaults so the engine and the UI see the effective parameters.
    let mut effective = parameters;
    if let Some(props) = flow
        .parameter_schema
        .get("properties")
        .and_then(|p| p.as_object())
    {
        for (k, prop) in props {
            if !effective.contains_key(k) {
                if let Some(d) = prop.get("default") {
                    effective.insert(k.clone(), d.clone());
                }
            }
        }
    }
    let mut all_tags = flow.tags.clone();
    for t in tags {
        if !all_tags.contains(&t) {
            all_tags.push(t);
        }
    }
    let name = name
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| generate_name(&state.store));
    let st = state.clone();
    let flow_id = flow.id;
    let created_by = created_by.to_string();
    let priority = FlowOptions::from_map(&flow.options).priority;
    let (run_id, _) = tokio::task::spawn_blocking(move || {
        st.store.create_run_full(CreateRun {
            flow_id,
            name,
            parameters: serde_json::to_string(&effective).unwrap_or_else(|_| "{}".into()),
            tags: serde_json::to_string(&all_tags).unwrap_or_else(|_| "[]".into()),
            created_by,
            initial_state: Some(RunState::new(StateType::Scheduled)),
            priority,
            ..Default::default()
        })
    })
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))??;
    let run = state
        .store
        .get_run(run_id)?
        .ok_or_else(|| ApiError::Internal("run vanished".into()))?;
    let key = EngineKey::from_flow(flow);
    state.index.insert_run(&run, key, false);
    state.run_created(&run);
    crate::dispatch::enqueue_run(state, &run, flow, None);
    Ok(run)
}

#[utoipa::path(get, path = "/api/runs", params(ListRunsFilter), responses((status = 200, body = RunsPage)))]
pub async fn list_runs(
    State(state): State<Arc<AppState>>,
    Query(filter): Query<ListRunsFilter>,
) -> ApiResult<Json<RunsPage>> {
    let st = state.clone();
    let page = tokio::task::spawn_blocking(move || st.store.list_runs(&filter))
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))??;
    Ok(Json(page))
}

#[utoipa::path(post, path = "/api/runs", request_body = CreateRunBody, responses((status = 201, body = Run), (status = 404, description = "Unknown flow and no module given"), (status = 422)))]
pub async fn create_run(
    State(state): State<Arc<AppState>>,
    user: Option<Extension<crate::auth::AuthenticatedUser>>,
    Json(body): Json<CreateRunBody>,
) -> ApiResult<(StatusCode, Json<Run>)> {
    // A signed-in user overrides whatever the client claims.
    let created_by = crate::auth::run_creator(
        user.as_ref().map(|Extension(u)| u),
        body.created_by.as_deref().unwrap_or("client"),
    );
    let existing = state.store.get_flow_by_key(&body.project, &body.flow)?;
    let flow = match (existing, body.module.clone(), body.source_dir.clone()) {
        (Some(f), Some(module), Some(source_dir)) if !state.is_live(f.id) => {
            // Refresh metadata from the submitting process.
            let id = state.store.upsert_flow_full(UpsertFlow {
                project: body.project.clone(),
                name: body.flow.clone(),
                module,
                source_dir,
                description: body.description.clone().or(f.description.clone()),
                tags: serde_json::to_string(&if body.flow_tags.is_empty() {
                    f.tags.clone()
                } else {
                    body.flow_tags.clone()
                })
                .unwrap_or_default(),
                parameter_schema: serde_json::to_string(
                    body.parameter_schema
                        .as_ref()
                        .unwrap_or(&f.parameter_schema),
                )
                .unwrap_or_default(),
                options: serde_json::to_string(body.options.as_ref().unwrap_or(&f.options))
                    .unwrap_or_default(),
                group: body.flow_group.clone().or_else(|| f.group.clone()),
            })?;
            let flow = state
                .store
                .get_flow(id)?
                .ok_or_else(|| ApiError::Internal("flow vanished".into()))?;
            state.index.set_flow_project(flow.id, flow.project.clone());
            state.stream.publish(
                "flow.registered",
                flow.id.to_string(),
                serde_json::to_value(super::flows::decorate(&state, flow.clone()))
                    .unwrap_or_default(),
            );
            flow
        }
        (Some(f), _, _) => f,
        (None, Some(module), Some(source_dir)) => {
            let id = state.store.upsert_flow_full(UpsertFlow {
                project: body.project.clone(),
                name: body.flow.clone(),
                module,
                source_dir,
                description: body.description.clone(),
                tags: serde_json::to_string(&body.flow_tags).unwrap_or_default(),
                parameter_schema: serde_json::to_string(
                    body.parameter_schema
                        .as_ref()
                        .unwrap_or(&Value::Object(Map::new())),
                )
                .unwrap_or_default(),
                options: serde_json::to_string(body.options.as_ref().unwrap_or(&Map::new()))
                    .unwrap_or_default(),
                group: body.flow_group.clone(),
            })?;
            let flow = state
                .store
                .get_flow(id)?
                .ok_or_else(|| ApiError::Internal("flow vanished".into()))?;
            state.index.set_flow_project(flow.id, flow.project.clone());
            state.stream.publish(
                "flow.registered",
                flow.id.to_string(),
                serde_json::to_value(super::flows::decorate(&state, flow.clone()))
                    .unwrap_or_default(),
            );
            flow
        }
        (None, _, _) => {
            return Err(ApiError::NotFound(format!(
                "flow {}/{} is not registered; pass module and source_dir to register it",
                body.project, body.flow
            )))
        }
    };
    let run = create_run_inner(
        &state,
        &flow,
        body.parameters,
        body.name,
        body.tags,
        &created_by,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(run)))
}

#[utoipa::path(get, path = "/api/runs/{id}", params(("id" = i64, Path)), responses((status = 200, body = Run), (status = 404)))]
pub async fn get_run(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> ApiResult<Json<Run>> {
    let run = state
        .store
        .get_run(id)?
        .ok_or_else(|| ApiError::NotFound("run not found".into()))?;
    Ok(Json(run))
}

#[utoipa::path(delete, path = "/api/runs/{id}", params(("id" = i64, Path)), responses((status = 204), (status = 404)))]
pub async fn delete_run(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    let run = state
        .store
        .get_run(id)?
        .ok_or_else(|| ApiError::NotFound("run not found".into()))?;
    delete_inner(&state, &run).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Cancel a run that is still going, then delete it and everything it recorded.
pub async fn delete_inner(state: &Arc<AppState>, run: &Run) -> ApiResult<()> {
    if !run.state.is_terminal() {
        cancel_inner(state, run).await?;
    }
    state.supervisor.dequeue(run.id);
    state.store.delete_run(run.id)?;
    state
        .index
        .remove_run(run.id, Some(&run.state), run.flow_id);
    state.stream.publish(
        "run.updated",
        run.id.to_string(),
        serde_json::json!({"id": run.id, "deleted": true}),
    );
    Ok(())
}

/// Also served on POST, which is what the engine client speaks.
#[utoipa::path(patch, path = "/api/runs/{id}/attributes", params(("id" = i64, Path)), request_body = Object, responses((status = 200, body = Run), (status = 404), (status = 422)))]
pub async fn patch_attributes(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Json(body): Json<serde_json::Map<String, serde_json::Value>>,
) -> ApiResult<Json<Run>> {
    if body
        .keys()
        .any(|k| k.is_empty() || !k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
    {
        return Err(ApiError::Unprocessable(
            "attribute names are letters, digits, and underscores".into(),
        ));
    }
    let patch = serde_json::Value::Object(body).to_string();
    if patch.len() > 16 * 1024 {
        return Err(ApiError::Unprocessable("attributes exceed 16 KB".into()));
    }
    if !state.store.merge_run_attributes(id, &patch)? {
        return Err(ApiError::NotFound("run not found".into()));
    }
    let run = state
        .store
        .get_run(id)?
        .ok_or_else(|| ApiError::NotFound("run not found".into()))?;
    state.stream.publish(
        "run.updated",
        id.to_string(),
        serde_json::to_value(&run).unwrap_or_default(),
    );
    Ok(Json(run))
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct BulkBody {
    /// The same filters as `GET /api/runs`, as an object; empty matches every run.
    #[serde(default)]
    #[schema(value_type = serde_json::Value)]
    pub filter: ListRunsFilter,
    /// `cancel`, `rerun`, or `delete`.
    pub action: String,
    /// Count only; defaults to true.
    #[serde(default)]
    pub dry_run: Option<bool>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct BulkResult {
    pub action: String,
    pub dry_run: bool,
    /// Runs the filter matched, capped at 10,000.
    pub matched: usize,
    /// Runs the action applied to: every match for delete, non-terminal ones for cancel, terminal ones for rerun.
    pub affected: usize,
}

const BULK_CAP: usize = 10_000;

#[utoipa::path(post, path = "/api/runs/bulk", request_body = BulkBody, responses((status = 200, body = BulkResult), (status = 422)))]
pub async fn bulk_runs(
    State(state): State<Arc<AppState>>,
    user: Option<Extension<crate::auth::AuthenticatedUser>>,
    Json(body): Json<BulkBody>,
) -> ApiResult<Json<BulkResult>> {
    let action = body.action.as_str();
    if !matches!(action, "cancel" | "rerun" | "delete") {
        return Err(ApiError::Unprocessable(
            "action must be cancel, rerun, or delete".into(),
        ));
    }
    let dry_run = body.dry_run.unwrap_or(true);
    let mut filter = body.filter.clone();
    filter.sort = Some("created_asc".into());
    filter.limit = Some(500);
    filter.cursor = None;
    let mut runs: Vec<Run> = Vec::new();
    loop {
        let st = state.clone();
        let f = filter.clone();
        let page = tokio::task::spawn_blocking(move || st.store.list_runs(&f))
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))??;
        runs.extend(page.items);
        match page.next_cursor {
            Some(c) if runs.len() < BULK_CAP => filter.cursor = Some(c),
            _ => break,
        }
    }
    runs.truncate(BULK_CAP);
    let targets: Vec<&Run> = runs
        .iter()
        .filter(|r| match action {
            "cancel" => !r.state.is_terminal(),
            "rerun" => r.state.is_terminal(),
            _ => true,
        })
        .collect();
    let matched = runs.len();
    let affected = targets.len();
    if !dry_run {
        let creator = crate::auth::run_creator(user.as_ref().map(|Extension(u)| u), "bulk");
        for run in targets {
            match action {
                "cancel" => {
                    cancel_inner(&state, run).await?;
                }
                "delete" => delete_inner(&state, run).await?,
                _ => {
                    let Some(flow) = state.store.get_flow(run.flow_id)? else {
                        continue;
                    };
                    create_run_inner(
                        &state,
                        &flow,
                        run.parameters.clone(),
                        None,
                        run.tags.clone(),
                        &creator,
                    )
                    .await?;
                }
            }
        }
    }
    Ok(Json(BulkResult {
        action: action.to_string(),
        dry_run,
        matched,
        affected,
    }))
}

#[derive(Deserialize, utoipa::IntoParams)]
pub struct PassQuery {
    /// One execution of the run's body: 0 the first time, the next after a
    /// resume or an in-process flow retry.
    #[serde(default)]
    pub pass: Option<i64>,
}

#[utoipa::path(get, path = "/api/runs/{id}/tasks", params(("id" = i64, Path), PassQuery), responses((status = 200, body = Vec<TaskRun>)))]
pub async fn run_tasks(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Query(q): Query<PassQuery>,
) -> ApiResult<Json<Vec<TaskRun>>> {
    Ok(Json(state.store.task_runs_by_run(id, q.pass)?))
}

#[utoipa::path(post, path = "/api/runs/{id}/transition", params(("id" = i64, Path)), request_body = TransitionBody, responses((status = 200, body = Run), (status = 409, body = TransitionRejected), (status = 404)))]
pub async fn transition_run(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Json(body): Json<TransitionBody>,
) -> ApiResult<Json<Run>> {
    let proposed = RunState::from_parts(
        body.state_type,
        body.name.as_deref(),
        body.message,
        body.details,
    );
    let st = state.clone();
    let result = tokio::task::spawn_blocking(move || st.transition_run(id, proposed, body.force))
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))??;
    match result {
        TransitionResult::Accepted(run) => Ok(Json(*run)),
        TransitionResult::Rejected { reason, current } => Err(ApiError::Conflict(
            serde_json::to_value(TransitionRejected {
                error: format!("transition rejected: {reason}"),
                reason: reason.to_string(),
                current,
            })
            .unwrap_or_default(),
        )),
    }
}

pub async fn cancel_inner(state: &Arc<AppState>, run: &Run) -> ApiResult<Run> {
    if run.state.is_terminal() {
        return Ok(run.clone());
    }
    let queued = state.supervisor.queued(run.id);
    let has_engine = state.supervisor.engine_for_run(run.id).is_some()
        || state.index.get(run.id).and_then(|r| r.engine_pid).is_some();
    let immediate = queued
        || !has_engine
        || matches!(
            run.state.state_type,
            StateType::Scheduled | StateType::Pending
        );
    let st = state.clone();
    let run_id = run.id;
    let result = tokio::task::spawn_blocking(move || {
        if immediate {
            st.supervisor.dequeue(run_id);
            st.transition_run(run_id, RunState::new(StateType::Cancelled), false)
        } else {
            st.transition_run(run_id, RunState::new(StateType::Cancelling), false)
        }
    })
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))??;
    match result {
        TransitionResult::Accepted(run) => {
            let run = *run;
            state.index.update(run.id, |r| {
                r.cancel_requested = true;
                if r.cancelling_since.is_none() {
                    r.cancelling_since = Some(std::time::Instant::now());
                }
            });
            Ok(run)
        }
        TransitionResult::Rejected { reason, current } => Err(ApiError::Conflict(
            serde_json::to_value(TransitionRejected {
                error: format!("cannot cancel: {reason}"),
                reason: reason.to_string(),
                current,
            })
            .unwrap_or_default(),
        )),
    }
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct ResumeBody {
    /// The answer handed to `wait_for_input` on the resumed attempt (any JSON).
    #[schema(value_type = Value)]
    pub input: Value,
}

#[utoipa::path(post, path = "/api/runs/{id}/resume", params(("id" = i64, Path)), request_body = ResumeBody, responses((status = 200, body = Run), (status = 404), (status = 409)))]
pub async fn resume_run(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Json(body): Json<ResumeBody>,
) -> ApiResult<Json<Run>> {
    Ok(Json(resume_inner(&state, id, body.input).await?))
}

/// Store the answer for a Paused run and schedule its next attempt.
pub async fn resume_inner(state: &Arc<AppState>, id: i64, input: Value) -> ApiResult<Run> {
    let run = state
        .store
        .get_run(id)?
        .ok_or_else(|| ApiError::NotFound("run not found".into()))?;
    if run.state.state_type != StateType::Paused {
        return Err(ApiError::Conflict(serde_json::json!({
            "error": format!("run is {}, not Paused", run.state.state_type.as_str()),
            "current": run.state,
        })));
    }
    let flow = state
        .store
        .get_flow(run.flow_id)?
        .ok_or_else(|| ApiError::NotFound("flow not found".into()))?;
    // The answer belongs to the question the run is actually waiting on, which
    // its Paused state names. The caller sends only the answer, so a stale
    // client cannot answer a question that has already moved on.
    let index = run
        .state
        .details
        .get("index")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let prompt = run
        .state
        .details
        .get("prompt")
        .cloned()
        .unwrap_or(Value::Null);
    let mut answers = stored_answers(state, id)?;
    answers.insert(
        index.to_string(),
        serde_json::json!({"prompt": prompt, "input": input}),
    );
    let answer = answers_value(&answers);
    let st = state.clone();
    let result = tokio::task::spawn_blocking(move || {
        st.store.kv_set(&crate::state::run_input_key(id), &answer)?;
        let mut next = RunState::new(StateType::Scheduled);
        next.name = "Resuming".into();
        st.transition_run(id, next, false)
    })
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))??;
    match result {
        TransitionResult::Accepted(run) => {
            let run = *run;
            crate::dispatch::enqueue_run(state, &run, &flow, None);
            Ok(run)
        }
        TransitionResult::Rejected { reason, current } => Err(ApiError::Conflict(
            serde_json::to_value(TransitionRejected {
                error: format!("cannot resume: {reason}"),
                reason: reason.to_string(),
                current,
            })
            .unwrap_or_default(),
        )),
    }
}

/// The answers stored for a run: question index to `{prompt, input}`. Anything
/// else under the key, including a value without the `v` marker, holds no answers.
fn stored_answers(state: &AppState, id: i64) -> ApiResult<Map<String, Value>> {
    let Some(raw) = state.store.kv_get(&crate::state::run_input_key(id))? else {
        return Ok(Map::new());
    };
    let value: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
    if value.get("v").and_then(|v| v.as_i64()) != Some(1) {
        return Ok(Map::new());
    }
    Ok(value
        .get("answers")
        .and_then(|a| a.as_object())
        .cloned()
        .unwrap_or_default())
}

fn answers_value(answers: &Map<String, Value>) -> String {
    serde_json::json!({"v": 1, "answers": answers}).to_string()
}

#[derive(Deserialize, utoipa::IntoParams)]
pub struct InputQuery {
    /// Which question: 0 is the first `wait_for_input` call of the body.
    #[serde(default)]
    pub index: Option<i64>,
}

#[utoipa::path(get, path = "/api/runs/{id}/input", params(("id" = i64, Path), InputQuery), responses((status = 200, description = "With index: {answer: {prompt, input}} or {answer: null}. Without: the pending question, every answer given, and {input} for the first"), (status = 404)))]
pub async fn run_input(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Query(q): Query<InputQuery>,
) -> ApiResult<Json<Value>> {
    let run = state
        .store
        .get_run(id)?
        .ok_or_else(|| ApiError::NotFound("run not found".into()))?;
    let answers = stored_answers(&state, id)?;
    if let Some(index) = q.index {
        let answer = answers
            .get(&index.to_string())
            .cloned()
            .unwrap_or(Value::Null);
        return Ok(Json(serde_json::json!({ "answer": answer })));
    }
    let pending = if run.state.state_type == StateType::Paused {
        serde_json::json!({
            "prompt": run.state.details.get("prompt").cloned().unwrap_or(Value::Null),
            "schema": run.state.details.get("schema").cloned().unwrap_or(Value::Null),
            "index": run.state.details.get("index").cloned().unwrap_or(serde_json::json!(0)),
        })
    } else {
        Value::Null
    };
    // `input` stays the first question's answer, for callers written before
    // questions were numbered.
    let first = answers
        .get("0")
        .and_then(|a| a.get("input"))
        .cloned()
        .unwrap_or(Value::Null);
    Ok(Json(
        serde_json::json!({"pending": pending, "answers": answers, "input": first}),
    ))
}

#[utoipa::path(post, path = "/api/runs/{id}/cancel", params(("id" = i64, Path)), responses((status = 200, body = Run), (status = 404)))]
pub async fn cancel_run(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> ApiResult<Json<Run>> {
    let run = state
        .store
        .get_run(id)?
        .ok_or_else(|| ApiError::NotFound("run not found".into()))?;
    Ok(Json(cancel_inner(&state, &run).await?))
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct GraphNode {
    pub id: i64,
    pub external_id: String,
    pub name: String,
    pub dynamic_key: String,
    pub state: RunState,
    pub start_time: Option<i64>,
    pub end_time: Option<i64>,
    pub created_at: i64,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct GraphEdge {
    pub from: i64,
    pub to: i64,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct RunGraph {
    pub run_id: i64,
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
}

#[utoipa::path(get, path = "/api/runs/{id}/graph", params(("id" = i64, Path), PassQuery), responses((status = 200, body = RunGraph)))]
pub async fn run_graph(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Query(q): Query<PassQuery>,
) -> ApiResult<Json<RunGraph>> {
    // One pass at a time: an edge between passes would join task runs that
    // never ran together. The latest is the one the run page opens on.
    let pass = match q.pass {
        Some(p) => p,
        None => (state.store.next_pass(id)? - 1).max(0),
    };
    let tasks = state.store.task_runs_by_run(id, Some(pass))?;
    let by_ext: std::collections::HashMap<String, i64> = tasks
        .iter()
        .map(|t| (t.external_id.to_string(), t.id))
        .collect();
    let mut edges = Vec::new();
    for t in &tasks {
        for p in &t.parents {
            if let Some(from) = by_ext.get(&p.to_string()) {
                edges.push(GraphEdge {
                    from: *from,
                    to: t.id,
                });
            }
        }
    }
    let nodes = tasks
        .into_iter()
        .map(|t| GraphNode {
            id: t.id,
            external_id: t.external_id.to_string(),
            name: t.name,
            dynamic_key: t.dynamic_key,
            state: t.state,
            start_time: t.start_time,
            end_time: t.end_time,
            created_at: t.created_at,
        })
        .collect();
    Ok(Json(RunGraph {
        run_id: id,
        nodes,
        edges,
    }))
}
