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
    /// Start at this time (microseconds since the epoch) instead of now.
    #[serde(default)]
    pub scheduled_time: Option<i64>,
    /// Start this many seconds from now instead of now; not with `scheduled_time`.
    #[serde(default)]
    pub delay: Option<f64>,
    /// The same key for this flow within `idempotency_ttl` answers with the run first created.
    #[serde(default)]
    pub idempotency_key: Option<String>,
    /// Seconds the idempotency key stays valid (default 86400).
    #[serde(default)]
    pub idempotency_ttl: Option<f64>,
}

/// A request-level idempotency key: the same key for the same flow within
/// `ttl` seconds answers with the run first created.
#[derive(Clone, Debug)]
pub struct Idempotency {
    pub key: String,
    pub ttl: Option<f64>,
}

impl Idempotency {
    pub fn from_body(key: Option<String>, ttl: Option<f64>) -> ApiResult<Option<Idempotency>> {
        let key = key.map(|k| k.trim().to_string()).filter(|k| !k.is_empty());
        if ttl.is_some_and(|t| t <= 0.0 || !t.is_finite()) {
            return Err(ApiError::Unprocessable(
                "idempotency_ttl must be a positive number of seconds".into(),
            ));
        }
        Ok(key.map(|key| Idempotency { key, ttl }))
    }
}

/// What makes this creation unique: the request's idempotency key when given,
/// else the flow's own `unique` declaration, else nothing.
pub fn unique_check_for(
    flow: &Flow,
    parameters: &Map<String, Value>,
    idempotency: Option<&Idempotency>,
) -> Option<cereyan_store::UniqueCheck> {
    let now = cereyan_core::now_micros();
    if let Some(idem) = idempotency {
        let ttl = idem.ttl.unwrap_or(86_400.0);
        return Some(cereyan_store::UniqueCheck {
            key: cereyan_core::unique::idempotency_key(flow.id, &idem.key),
            states: Vec::new(),
            since: Some(now - (ttl * 1_000_000.0) as i64),
        });
    }
    let spec = FlowOptions::from_map(&flow.options).unique?;
    let rendered = cereyan_core::unique::render_key(spec.key.as_deref(), parameters);
    Some(cereyan_store::UniqueCheck {
        key: cereyan_core::unique::unique_key(flow.id, &rendered, spec.period, now),
        states: spec.counting_states(),
        since: None,
    })
}

async fn insert_run(
    state: &Arc<AppState>,
    cmd: CreateRun,
) -> ApiResult<Result<(i64, cereyan_core::Id), cereyan_store::StoreError>> {
    let st = state.clone();
    tokio::task::spawn_blocking(move || st.store.create_run_full(cmd))
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))
}

/// The run that holds a unique key already, answered instead of a new one.
#[derive(Serialize, utoipa::ToSchema)]
pub struct RunConflict {
    pub conflict: bool,
    pub run: Run,
}

/// 201 with the run when created, 200 with `{conflict: true, run}` when a run
/// already held the key.
pub fn created_response(run: Run, conflict: bool) -> axum::response::Response {
    use axum::response::IntoResponse;
    if conflict {
        (
            StatusCode::OK,
            Json(RunConflict {
                conflict: true,
                run,
            }),
        )
            .into_response()
    } else {
        (StatusCode::CREATED, Json(run)).into_response()
    }
}

/// When a new run should start: `scheduled_time` or now plus `delay`, or none for now.
pub fn not_before(scheduled_time: Option<i64>, delay: Option<f64>) -> ApiResult<Option<i64>> {
    match (scheduled_time, delay) {
        (Some(_), Some(_)) => Err(ApiError::Unprocessable(
            "give scheduled_time or delay, not both".into(),
        )),
        (Some(t), None) => Ok(Some(t)),
        (None, Some(d)) if d < 0.0 || !d.is_finite() => Err(ApiError::Unprocessable(
            "delay must be zero or more seconds".into(),
        )),
        (None, Some(d)) => Ok(Some(cereyan_core::now_micros() + (d * 1_000_000.0) as i64)),
        (None, None) => Ok(None),
    }
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
    /// Start at this time (microseconds since the epoch) instead of now.
    #[serde(default)]
    pub scheduled_time: Option<i64>,
    /// Start this many seconds from now instead of now; not with `scheduled_time`.
    #[serde(default)]
    pub delay: Option<f64>,
    /// The same key for this flow within `idempotency_ttl` answers with the run first created.
    #[serde(default)]
    pub idempotency_key: Option<String>,
    /// Seconds the idempotency key stays valid (default 86400).
    #[serde(default)]
    pub idempotency_ttl: Option<f64>,
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

#[allow(clippy::too_many_arguments)]
pub async fn create_run_inner(
    state: &Arc<AppState>,
    flow: &Flow,
    parameters: Map<String, Value>,
    name: Option<String>,
    tags: Vec<String>,
    created_by: &str,
    not_before: Option<i64>,
    parent: Option<(i64, i64)>,
) -> ApiResult<Run> {
    create_run_checked(
        state, flow, parameters, name, tags, created_by, not_before, parent, None,
    )
    .await
    .map(|(run, _)| run)
}

/// Create a run under the flow's `unique` declaration or a request's
/// idempotency key. Returns the run and whether it was one that already held
/// the key (`skip`), in which case nothing was created.
#[allow(clippy::too_many_arguments)]
pub async fn create_run_checked(
    state: &Arc<AppState>,
    flow: &Flow,
    parameters: Map<String, Value>,
    name: Option<String>,
    tags: Vec<String>,
    created_by: &str,
    not_before: Option<i64>,
    parent: Option<(i64, i64)>,
    idempotency: Option<Idempotency>,
) -> ApiResult<(Run, bool)> {
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
    let options = FlowOptions::from_map(&flow.options);
    let priority = options.priority;
    let unique = unique_check_for(flow, &effective, idempotency.as_ref());
    let replace = idempotency.is_none()
        && options
            .unique
            .as_ref()
            .is_some_and(|u| u.on_conflict == "replace");
    let make = |unique: Option<cereyan_store::UniqueCheck>| CreateRun {
        flow_id,
        name: name.clone(),
        parameters: serde_json::to_string(&effective).unwrap_or_else(|_| "{}".into()),
        tags: serde_json::to_string(&all_tags).unwrap_or_else(|_| "[]".into()),
        created_by: created_by.clone(),
        initial_state: Some(RunState::new(StateType::Scheduled)),
        priority,
        scheduled_time: not_before,
        parent_run_id: parent.map(|(id, _)| id),
        attempt: parent.map(|(_, attempt)| attempt).unwrap_or(0),
        unique,
        ..Default::default()
    };
    let holder_of = |existing: i64| -> ApiResult<Run> {
        state
            .store
            .get_run(existing)?
            .ok_or_else(|| ApiError::Internal("run vanished".into()))
    };
    let run_id = match insert_run(&st, make(unique.clone())).await? {
        Ok((run_id, _)) => run_id,
        Err(cereyan_store::StoreError::UniqueConflict { existing }) => {
            let holder = holder_of(existing)?;
            // `replace`: cancel the holder and try once more; a holder that is
            // still winding down keeps the key, and the caller gets it.
            if replace && !holder.state.is_terminal() {
                let cancelled = cancel_inner(state, &holder).await?;
                if !cancelled.state.is_terminal() {
                    return Ok((cancelled, true));
                }
                match insert_run(&st, make(unique.clone())).await? {
                    Ok((run_id, _)) => run_id,
                    Err(cereyan_store::StoreError::UniqueConflict { existing }) => {
                        return Ok((holder_of(existing)?, true))
                    }
                    Err(e) => return Err(e.into()),
                }
            } else {
                return Ok((holder, true));
            }
        }
        Err(e) => return Err(e.into()),
    };
    let run = state
        .store
        .get_run(run_id)?
        .ok_or_else(|| ApiError::Internal("run vanished".into()))?;
    let key = EngineKey::from_flow(flow);
    state.index.insert_run(&run, key, false);
    state.run_created(&run);
    // A run for later is armed on the timer like a schedule fire: it waits in
    // Scheduled, pre-warms an engine, can be marked Late, and is held by the
    // global pause. A time already past starts now.
    match not_before {
        Some(due) if due > cereyan_core::now_micros() => {
            crate::scheduler::arm_run(state, run.id, due, false)
        }
        _ => crate::dispatch::enqueue_run(state, &run, flow, None),
    }
    Ok((run, false))
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

#[utoipa::path(post, path = "/api/runs", request_body = CreateRunBody, responses((status = 201, body = Run), (status = 200, body = RunConflict, description = "A run already holds the unique or idempotency key"), (status = 404, description = "Unknown flow and no module given"), (status = 422)))]
pub async fn create_run(
    State(state): State<Arc<AppState>>,
    user: Option<Extension<crate::auth::AuthenticatedUser>>,
    Json(body): Json<CreateRunBody>,
) -> ApiResult<axum::response::Response> {
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
    let starts = not_before(body.scheduled_time, body.delay)?;
    let idempotency = Idempotency::from_body(body.idempotency_key, body.idempotency_ttl)?;
    let (run, conflict) = create_run_checked(
        &state,
        &flow,
        body.parameters,
        body.name,
        body.tags,
        &created_by,
        starts,
        None,
        idempotency,
    )
    .await?;
    Ok(created_response(run, conflict))
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

#[derive(Deserialize, utoipa::IntoParams)]
pub struct CompareQuery {
    /// Two run ids, comma separated: the baseline first, the run in question second.
    pub ids: String,
}

pub(crate) fn bundle(state: &AppState, id: i64) -> ApiResult<crate::compare::RunBundle> {
    let run = state
        .store
        .get_run(id)?
        .ok_or_else(|| ApiError::NotFound(format!("run {id} not found")))?;
    let tasks = state.store.task_runs_by_run(id, None)?;
    let errors = state
        .store
        .logs(&cereyan_store::LogFilter {
            run_id: Some(id),
            min_level: Some(40),
            limit: Some(500),
            ..Default::default()
        })?
        .items;
    let artifacts = state.store.artifacts_by_run(id)?;
    Ok(crate::compare::RunBundle {
        run,
        tasks,
        errors,
        artifacts,
    })
}

#[utoipa::path(get, path = "/api/runs/compare", params(CompareQuery), responses((status = 200, body = crate::compare::RunComparison), (status = 404), (status = 422)))]
pub async fn compare_runs(
    State(state): State<Arc<AppState>>,
    Query(q): Query<CompareQuery>,
) -> ApiResult<Json<crate::compare::RunComparison>> {
    let ids: Vec<i64> = q
        .ids
        .split(',')
        .map(|s| s.trim().parse::<i64>())
        .collect::<Result<_, _>>()
        .map_err(|_| ApiError::Unprocessable("ids must be two run ids, comma separated".into()))?;
    let [a, b] = ids[..] else {
        return Err(ApiError::Unprocessable(
            "ids must name exactly two runs".into(),
        ));
    };
    if a == b {
        return Err(ApiError::Unprocessable(
            "ids must be two different runs".into(),
        ));
    }
    let st = state.clone();
    tokio::task::spawn_blocking(move || {
        let left = bundle(&st, a)?;
        let right = bundle(&st, b)?;
        Ok(Json(crate::compare::compare(&left, &right)))
    })
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?
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
    /// `cancel`, `rerun`, `retry` (from failure), or `delete`.
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
    if !matches!(action, "cancel" | "rerun" | "retry" | "delete") {
        return Err(ApiError::Unprocessable(
            "action must be cancel, rerun, retry, or delete".into(),
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
            "rerun" | "retry" => r.state.is_terminal(),
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
                "retry" => {
                    let by = crate::auth::run_creator(
                        user.as_ref().map(|Extension(u)| u),
                        &format!("retry:{}", run.id),
                    );
                    retry_inner(&state, run, "failure", &by).await?;
                }
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
                        None,
                        None,
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

#[derive(Deserialize, utoipa::ToSchema, Default)]
#[serde(default)]
pub struct RetryBody {
    /// `failure` (default), `start`, or a dynamic key such as `transform-0`.
    pub from: Option<String>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct RetryResponse {
    pub run: Run,
    pub retry_of: i64,
    pub from: String,
    /// Checkpoints the new run replays.
    pub replays: usize,
    /// Dynamic keys that will execute again.
    pub invalidated: Vec<String>,
}

/// Fork a terminal run: a new run seeded with the original's checkpoints
/// except `from` and everything downstream of it.
pub async fn retry_inner(
    state: &Arc<AppState>,
    original: &Run,
    from: &str,
    created_by: &str,
) -> ApiResult<RetryResponse> {
    if !original.state.is_terminal() {
        return Err(ApiError::Conflict(serde_json::json!({
            "error": format!("run is {}, not finished", original.state.state_type.as_str()),
            "current": original.state,
        })));
    }
    let flow = state
        .store
        .get_flow(original.flow_id)?
        .ok_or_else(|| ApiError::NotFound("flow not found".into()))?;
    let tasks = state.store.task_runs_by_run(original.id, None)?;
    let last = tasks.iter().map(|t| t.pass).max().unwrap_or(0);
    let latest: Vec<&TaskRun> = tasks.iter().filter(|t| t.pass == last).collect();
    let key_of: std::collections::HashMap<String, &str> = latest
        .iter()
        .map(|t| (t.external_id.to_string(), t.dynamic_key.as_str()))
        .collect();
    // Everything that waited on a key, transitively, through the recorded parents.
    let downstream = |roots: Vec<String>| -> std::collections::BTreeSet<String> {
        let mut out: std::collections::BTreeSet<String> = roots.iter().cloned().collect();
        loop {
            let before = out.len();
            for t in &latest {
                if out.contains(&t.dynamic_key) {
                    continue;
                }
                let waits_on_hit = t
                    .parents
                    .iter()
                    .filter_map(|p| key_of.get(&p.to_string()))
                    .any(|k| out.contains(*k));
                if waits_on_hit {
                    out.insert(t.dynamic_key.clone());
                }
            }
            if out.len() == before {
                break;
            }
        }
        out
    };
    let invalidated: std::collections::BTreeSet<String> = match from {
        "start" => latest.iter().map(|t| t.dynamic_key.clone()).collect(),
        "failure" => downstream(
            latest
                .iter()
                .filter(|t| t.state.state_type != StateType::Completed)
                .map(|t| t.dynamic_key.clone())
                .collect(),
        ),
        key => {
            if !latest.iter().any(|t| t.dynamic_key == key) {
                return Err(ApiError::Unprocessable(format!(
                    "run {} recorded no task {key:?} in its last pass",
                    original.id
                )));
            }
            downstream(vec![key.to_string()])
        }
    };
    let seed: Vec<cereyan_store::Checkpoint> = if from == "start" {
        Vec::new()
    } else {
        state
            .store
            .checkpoints(original.id)?
            .into_iter()
            .filter(|c| !invalidated.contains(&c.dynamic_key))
            .collect()
    };
    let run = create_run_inner(
        state,
        &flow,
        original.parameters.clone(),
        None,
        original.tags.clone(),
        created_by,
        None,
        Some((original.id, original.attempt + 1)),
    )
    .await?;
    if !seed.is_empty() {
        state.store.kv_set(
            &cereyan_store::checkpoint_seed_key(run.id),
            &serde_json::to_string(&seed).unwrap_or_else(|_| "[]".into()),
        )?;
    }
    Ok(RetryResponse {
        replays: seed.len(),
        run,
        retry_of: original.id,
        from: from.to_string(),
        invalidated: invalidated.into_iter().collect(),
    })
}

#[utoipa::path(post, path = "/api/runs/{id}/retry", params(("id" = i64, Path)), request_body = RetryBody, responses((status = 201, body = RetryResponse), (status = 404), (status = 409), (status = 422)))]
pub async fn retry_run(
    State(state): State<Arc<AppState>>,
    user: Option<Extension<crate::auth::AuthenticatedUser>>,
    Path(id): Path<i64>,
    body: Option<Json<RetryBody>>,
) -> ApiResult<(StatusCode, Json<RetryResponse>)> {
    let original = state
        .store
        .get_run(id)?
        .ok_or_else(|| ApiError::NotFound("run not found".into()))?;
    let from = body
        .and_then(|b| b.0.from)
        .map(|f| f.trim().to_string())
        .filter(|f| !f.is_empty())
        .unwrap_or_else(|| "failure".into());
    let created_by =
        crate::auth::run_creator(user.as_ref().map(|Extension(u)| u), &format!("retry:{id}"));
    let out = retry_inner(&state, &original, &from, &created_by).await?;
    Ok((StatusCode::CREATED, Json(out)))
}

#[derive(Deserialize, utoipa::IntoParams)]
pub struct StateQuery {
    /// The task's dynamic key, or empty for the flow body; with `key`, reads one value.
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub key: Option<String>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct StateValue {
    pub found: bool,
    pub value: Value,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct StateBody {
    #[serde(default)]
    pub scope: String,
    pub key: String,
    #[serde(default)]
    pub value: Value,
    /// Remove the entry instead of setting it.
    #[serde(default)]
    pub delete: bool,
}

/// The largest JSON value a task may keep in its state store.
pub const TASK_STATE_MAX_BYTES: usize = 64 * 1024;

#[utoipa::path(get, path = "/api/runs/{id}/state", params(("id" = i64, Path), StateQuery), responses((status = 200, body = Vec<cereyan_store::TaskStateRow>, description = "Every entry, or with scope and key one StateValue"), (status = 404)))]
pub async fn run_state(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Query(q): Query<StateQuery>,
) -> ApiResult<axum::response::Response> {
    use axum::response::IntoResponse;
    state
        .store
        .get_run(id)?
        .ok_or_else(|| ApiError::NotFound("run not found".into()))?;
    if let Some(key) = q.key.filter(|k| !k.is_empty()) {
        let scope = q.scope.unwrap_or_default();
        let st = state.clone();
        let raw = tokio::task::spawn_blocking(move || st.store.task_state_get(id, &scope, &key))
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))??;
        let out = match raw {
            Some(text) => StateValue {
                found: true,
                value: serde_json::from_str(&text).unwrap_or(Value::String(text)),
            },
            None => StateValue {
                found: false,
                value: Value::Null,
            },
        };
        return Ok(Json(out).into_response());
    }
    let st = state.clone();
    let rows = tokio::task::spawn_blocking(move || st.store.task_state_list(id))
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))??;
    Ok(Json(rows).into_response())
}

#[utoipa::path(post, path = "/api/runs/{id}/state", params(("id" = i64, Path)), request_body = StateBody, responses((status = 200, description = "ok"), (status = 404), (status = 422)))]
pub async fn set_run_state(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Json(body): Json<StateBody>,
) -> ApiResult<Json<Value>> {
    let key = body.key.trim().to_string();
    if key.is_empty() || key.len() > 200 {
        return Err(ApiError::Unprocessable(
            "key must be 1 to 200 characters".into(),
        ));
    }
    let ok = if body.delete {
        state.store.task_state_delete(id, &body.scope, &key)?;
        state.store.get_run(id)?.is_some()
    } else {
        let text = body.value.to_string();
        if text.len() > TASK_STATE_MAX_BYTES {
            return Err(ApiError::Unprocessable("value exceeds 64 KB".into()));
        }
        state.store.task_state_set(id, &body.scope, &key, &text)?
    };
    if !ok {
        return Err(ApiError::NotFound("run not found".into()));
    }
    Ok(Json(serde_json::json!({"ok": true})))
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
