//! Events, rules, artifacts, and variables endpoints.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use cereyan_core::{ArtifactRow, Event, Expectation, RuleFiring, RuleRow, RuleSpec, VariableRow};
use cereyan_store::{
    secrets, ArtifactFilter, ArtifactsPage, EventFilter, EventsPage, RuleWrite, UpsertArtifact,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::error::{ApiError, ApiResult};
use crate::rules;
use crate::state::AppState;

// ---- events ---------------------------------------------------------------

#[utoipa::path(get, path = "/api/events", params(EventFilter), responses((status = 200, body = EventsPage)))]
pub async fn list_events(
    State(state): State<Arc<AppState>>,
    Query(filter): Query<EventFilter>,
) -> ApiResult<Json<EventsPage>> {
    let st = state.clone();
    let page = tokio::task::spawn_blocking(move || st.store.query_events(&filter))
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))??;
    Ok(Json(page))
}

#[utoipa::path(get, path = "/api/events/{id}", params(("id" = i64, Path)), responses((status = 200, body = Event), (status = 404)))]
pub async fn get_event(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> ApiResult<Json<Event>> {
    let e = state
        .store
        .get_event(id)?
        .ok_or_else(|| ApiError::NotFound("event not found".into()))?;
    Ok(Json(e))
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct EmitEventBody {
    pub name: String,
    #[serde(default)]
    #[schema(value_type = Object)]
    pub payload: serde_json::Map<String, Value>,
    #[serde(default)]
    pub run_id: Option<i64>,
    #[serde(default)]
    pub flow_id: Option<i64>,
    #[serde(default)]
    pub resource: Option<cereyan_core::Resource>,
}

#[utoipa::path(post, path = "/api/events", request_body = EmitEventBody, responses((status = 201, body = Event)))]
pub async fn emit_event(
    State(state): State<Arc<AppState>>,
    Json(body): Json<EmitEventBody>,
) -> ApiResult<(StatusCode, Json<Event>)> {
    if body.name.trim().is_empty() {
        return Err(ApiError::Unprocessable("event name is required".into()));
    }
    let st = state.clone();
    let id = tokio::task::spawn_blocking(move || {
        if let Some(resource) = body.resource {
            st.record_event_full(cereyan_store::NewEvent {
                name: body.name,
                run_id: body.run_id,
                flow_id: body.flow_id,
                payload: Value::Object(body.payload),
                resource,
                related: Vec::new(),
            })
        } else {
            st.record_event(
                &body.name,
                body.run_id,
                body.flow_id,
                Value::Object(body.payload),
            )
        }
    })
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))??;
    let e = state
        .store
        .get_event(id)?
        .ok_or_else(|| ApiError::Internal("event vanished".into()))?;
    Ok((StatusCode::CREATED, Json(e)))
}

// ---- rules ----------------------------------------------------------------

#[derive(Deserialize, utoipa::ToSchema)]
pub struct RuleBody {
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(flatten)]
    pub spec: RuleSpec,
}

fn default_true() -> bool {
    true
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct RulePatch {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    #[schema(value_type = Object)]
    pub when: Option<cereyan_core::RuleMatch>,
    #[serde(default, rename = "do")]
    pub actions: Option<Vec<cereyan_core::RuleAction>>,
    #[serde(default)]
    pub once: Option<String>,
    #[serde(default)]
    pub cooldown_seconds: Option<f64>,
    #[serde(default)]
    pub max_per_minute: Option<i64>,
    #[serde(default)]
    pub allow_self: Option<bool>,
    #[serde(default)]
    #[schema(value_type = Object)]
    pub unless: Option<Option<cereyan_core::RuleMatch>>,
    #[serde(default)]
    pub within: Option<Option<f64>>,
    #[serde(default)]
    #[schema(value_type = Object)]
    pub at: Option<Option<cereyan_core::RuleClock>>,
}

#[utoipa::path(get, path = "/api/rules", responses((status = 200, body = Vec<RuleRow>)))]
pub async fn list_rules(State(state): State<Arc<AppState>>) -> Json<Vec<RuleRow>> {
    Json(state.rules.all())
}

#[utoipa::path(post, path = "/api/rules", request_body = RuleBody, responses((status = 201, body = RuleRow), (status = 422)))]
pub async fn create_rule(
    State(state): State<Arc<AppState>>,
    Json(body): Json<RuleBody>,
) -> ApiResult<(StatusCode, Json<RuleRow>)> {
    cereyan_rules::validate_spec(&body.spec).map_err(ApiError::Unprocessable)?;
    if body.spec.actions.iter().any(|a| a.kind == "call") {
        return Err(ApiError::Unprocessable(
            "call actions are only available to code rules".into(),
        ));
    }
    let st = state.clone();
    let id = tokio::task::spawn_blocking(move || {
        let id = st.store.upsert_rule(RuleWrite {
            id: None,
            name: body.name,
            enabled: body.enabled,
            source: "ui".into(),
            module: None,
            spec: serde_json::to_string(&body.spec).unwrap_or_default(),
        })?;
        rules::load(&st);
        Ok::<i64, cereyan_store::StoreError>(id)
    })
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))??;
    let row = state
        .rules
        .get(id)
        .ok_or_else(|| ApiError::Internal("rule vanished".into()))?;
    state.stream.publish(
        "rule.updated",
        id.to_string(),
        serde_json::to_value(&row).unwrap_or_default(),
    );
    Ok((StatusCode::CREATED, Json(row)))
}

#[utoipa::path(get, path = "/api/rules/{id}", params(("id" = i64, Path)), responses((status = 200, body = RuleRow), (status = 404)))]
pub async fn get_rule(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> ApiResult<Json<RuleRow>> {
    state
        .rules
        .get(id)
        .map(Json)
        .ok_or_else(|| ApiError::NotFound("rule not found".into()))
}

#[utoipa::path(patch, path = "/api/rules/{id}", params(("id" = i64, Path)), request_body = RulePatch, responses((status = 200, body = RuleRow), (status = 404), (status = 422)))]
pub async fn patch_rule(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Json(body): Json<RulePatch>,
) -> ApiResult<Json<RuleRow>> {
    let current = state
        .rules
        .get(id)
        .ok_or_else(|| ApiError::NotFound("rule not found".into()))?;
    let structural = body.name.is_some()
        || body.when.is_some()
        || body.actions.is_some()
        || body.once.is_some()
        || body.cooldown_seconds.is_some()
        || body.max_per_minute.is_some()
        || body.allow_self.is_some()
        || body.unless.is_some()
        || body.within.is_some()
        || body.at.is_some();
    if current.source == "code" && structural {
        return Err(ApiError::Conflict(
            json!({"error": "code rules are read-only; only enabled can change"}),
        ));
    }
    let mut spec = current.spec.clone();
    if let Some(w) = body.when {
        spec.when = w;
    }
    if let Some(a) = body.actions {
        spec.actions = a;
    }
    if let Some(o) = body.once {
        spec.once = o;
    }
    if let Some(c) = body.cooldown_seconds {
        spec.cooldown_seconds = c;
    }
    if let Some(m) = body.max_per_minute {
        spec.max_per_minute = m;
    }
    if let Some(s) = body.allow_self {
        spec.allow_self = s;
    }
    if let Some(u) = body.unless {
        spec.unless = u;
    }
    if let Some(w) = body.within {
        spec.within = w;
    }
    if let Some(a) = body.at {
        spec.at = a;
    }
    cereyan_rules::validate_spec(&spec).map_err(ApiError::Unprocessable)?;
    let disabling = body.enabled == Some(false) && current.enabled;
    let st = state.clone();
    tokio::task::spawn_blocking(move || {
        st.store.upsert_rule(RuleWrite {
            id: Some(id),
            name: body.name.unwrap_or(current.name),
            enabled: body.enabled.unwrap_or(current.enabled),
            source: current.source,
            module: current.module,
            spec: serde_json::to_string(&spec).unwrap_or_default(),
        })?;
        if disabling {
            rules::cancel_expectations(&st, id);
        }
        rules::load(&st);
        Ok::<(), cereyan_store::StoreError>(())
    })
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))??;
    let row = state
        .rules
        .get(id)
        .ok_or_else(|| ApiError::NotFound("rule not found".into()))?;
    state.stream.publish(
        "rule.updated",
        id.to_string(),
        serde_json::to_value(&row).unwrap_or_default(),
    );
    Ok(Json(row))
}

#[utoipa::path(delete, path = "/api/rules/{id}", params(("id" = i64, Path)), responses((status = 204), (status = 404), (status = 409)))]
pub async fn delete_rule(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    let current = state
        .rules
        .get(id)
        .ok_or_else(|| ApiError::NotFound("rule not found".into()))?;
    if current.source == "code" {
        return Err(ApiError::Conflict(
            json!({"error": "code rules are removed by deleting the decorator"}),
        ));
    }
    let st = state.clone();
    tokio::task::spawn_blocking(move || {
        rules::cancel_expectations(&st, id);
        st.store.delete_rule(id)?;
        rules::load(&st);
        Ok::<(), cereyan_store::StoreError>(())
    })
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))??;
    state.stream.publish(
        "rule.updated",
        id.to_string(),
        json!({"id": id, "deleted": true}),
    );
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(get, path = "/api/rules/{id}/firings", params(("id" = i64, Path)), responses((status = 200, body = Vec<RuleFiring>)))]
pub async fn rule_firings(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> ApiResult<Json<Vec<RuleFiring>>> {
    Ok(Json(state.store.list_firings(id, 200)?))
}

#[derive(Deserialize, utoipa::IntoParams)]
pub struct ExpectationQuery {
    /// Only open (armed) expectations; default true.
    #[serde(default)]
    pub open: Option<bool>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[utoipa::path(get, path = "/api/rules/{id}/expectations", params(("id" = i64, Path), ExpectationQuery), responses((status = 200, body = Vec<Expectation>), (status = 404)))]
pub async fn rule_expectations(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Query(q): Query<ExpectationQuery>,
) -> ApiResult<Json<Vec<Expectation>>> {
    state
        .rules
        .get(id)
        .ok_or_else(|| ApiError::NotFound("rule not found".into()))?;
    Ok(Json(state.store.expectations_by_rule(
        id,
        q.open.unwrap_or(true),
        q.limit.unwrap_or(200),
    )?))
}

#[utoipa::path(post, path = "/api/rules/{id}/test", params(("id" = i64, Path)), responses((status = 200, description = "Rendered actions against the latest matching event")))]
pub async fn test_rule(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> ApiResult<Json<Value>> {
    let rule = state
        .rules
        .get(id)
        .ok_or_else(|| ApiError::NotFound("rule not found".into()))?;
    let st = state.clone();
    let result = tokio::task::spawn_blocking(move || rules::test_rule(&st, &rule))
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .map_err(ApiError::Internal)?;
    Ok(Json(result))
}

// ---- artifacts -------------------------------------------------------------

#[utoipa::path(get, path = "/api/artifacts", params(ArtifactFilter), responses((status = 200, body = ArtifactsPage)))]
pub async fn list_artifacts(
    State(state): State<Arc<AppState>>,
    Query(filter): Query<ArtifactFilter>,
) -> ApiResult<Json<ArtifactsPage>> {
    let st = state.clone();
    let page = tokio::task::spawn_blocking(move || st.store.list_artifacts(&filter))
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))??;
    Ok(Json(page))
}

#[utoipa::path(get, path = "/api/runs/{id}/artifacts", params(("id" = i64, Path)), responses((status = 200, body = Vec<ArtifactRow>)))]
pub async fn run_artifacts(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> ApiResult<Json<Vec<ArtifactRow>>> {
    Ok(Json(state.store.artifacts_by_run(id)?))
}

#[utoipa::path(get, path = "/api/task-runs/{id}/artifacts", params(("id" = i64, Path)), responses((status = 200, body = Vec<ArtifactRow>)))]
pub async fn task_run_artifacts(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> ApiResult<Json<Vec<ArtifactRow>>> {
    Ok(Json(state.store.artifacts_by_task_run(id)?))
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct ArtifactBody {
    pub run_id: i64,
    #[serde(default)]
    pub task_run_id: Option<i64>,
    pub kind: String,
    #[serde(default)]
    pub key: Option<String>,
    #[schema(value_type = Object)]
    pub data: Value,
}

pub const ARTIFACT_MAX_BYTES: usize = 1_048_576;

#[utoipa::path(post, path = "/api/artifacts", request_body = ArtifactBody, responses((status = 201, body = ArtifactRow), (status = 422)))]
pub async fn create_artifact(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ArtifactBody>,
) -> ApiResult<(StatusCode, Json<ArtifactRow>)> {
    let data = body.data.to_string();
    if data.len() > ARTIFACT_MAX_BYTES {
        return Err(ApiError::Unprocessable("artifact exceeds 1 MB".into()));
    }
    let st = state.clone();
    let id = tokio::task::spawn_blocking(move || {
        st.store.upsert_artifact(UpsertArtifact {
            run_id: body.run_id,
            task_run_id: body.task_run_id,
            kind: body.kind,
            key: body.key,
            data,
            external_id: None,
        })
    })
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))??;
    let row = state
        .store
        .get_artifact(id)?
        .ok_or_else(|| ApiError::Internal("artifact vanished".into()))?;
    state.stream.publish(
        "artifact.updated",
        id.to_string(),
        serde_json::to_value(&row).unwrap_or_default(),
    );
    Ok((StatusCode::CREATED, Json(row)))
}

// ---- variables -------------------------------------------------------------

#[derive(Deserialize, utoipa::ToSchema)]
pub struct VariableBody {
    pub name: String,
    #[schema(value_type = Object)]
    pub value: Value,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub secret: bool,
    #[serde(default = "default_true")]
    pub overwrite: bool,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct VariablePatch {
    #[serde(default)]
    #[schema(value_type = Object)]
    pub value: Option<Value>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    #[serde(default)]
    pub secret: Option<bool>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct VariableWithRaw {
    #[serde(flatten)]
    pub variable: VariableRow,
    /// Ciphertext of a secret, present only when `raw=true` was requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw: Option<String>,
}

#[derive(Deserialize, utoipa::IntoParams)]
pub struct VariableQuery {
    /// Return the stored ciphertext of a secret so a local client can decrypt it.
    pub raw: Option<bool>,
}

pub const VARIABLE_MAX_BYTES: usize = 65_536;

pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name.chars().all(|c| {
            c.is_ascii_lowercase()
                || c.is_ascii_digit()
                || c == '_'
                || c == '-'
                || c == '/'
                || c == '.'
        })
        && !name.starts_with(['-', '/', '.'])
}

fn store_value(state: &AppState, value: &Value, secret: bool) -> Result<String, ApiError> {
    let text = value.to_string();
    if text.len() > VARIABLE_MAX_BYTES {
        return Err(ApiError::Unprocessable(
            "variable value exceeds 64 KB".into(),
        ));
    }
    if secret {
        secrets::encrypt(&state.config.home, &text).map_err(|e| ApiError::Internal(e.to_string()))
    } else {
        Ok(text)
    }
}

#[utoipa::path(get, path = "/api/variables", responses((status = 200, body = Vec<VariableRow>)))]
pub async fn list_variables(
    State(state): State<Arc<AppState>>,
) -> ApiResult<Json<Vec<VariableRow>>> {
    Ok(Json(state.store.list_variables()?))
}

#[utoipa::path(post, path = "/api/variables", request_body = VariableBody, responses((status = 201, body = VariableRow), (status = 409), (status = 422)))]
pub async fn create_variable(
    State(state): State<Arc<AppState>>,
    Json(body): Json<VariableBody>,
) -> ApiResult<(StatusCode, Json<VariableRow>)> {
    if !valid_name(&body.name) {
        return Err(ApiError::Unprocessable(
            "variable names use lowercase letters, digits, '_', '-', '/', and '.'".into(),
        ));
    }
    if !body.overwrite && state.store.get_variable(&body.name)?.is_some() {
        return Err(ApiError::Conflict(
            json!({"error": format!("variable {} exists", body.name)}),
        ));
    }
    let row = set_variable_inner(&state, &body.name, &body.value, &body.tags, body.secret).await?;
    Ok((StatusCode::CREATED, Json(row)))
}

/// Write a variable (encrypting secrets) and publish the change.
pub async fn set_variable_inner(
    state: &Arc<AppState>,
    name: &str,
    value: &Value,
    tags: &[String],
    secret: bool,
) -> ApiResult<VariableRow> {
    if !valid_name(name) {
        return Err(ApiError::Unprocessable(
            "variable names use lowercase letters, digits, '_', '-', '/', and '.'".into(),
        ));
    }
    let stored = store_value(state, value, secret)?;
    let tags = serde_json::to_string(tags).unwrap_or_else(|_| "[]".into());
    let st = state.clone();
    let owned = name.to_string();
    tokio::task::spawn_blocking(move || st.store.set_variable(&owned, &stored, &tags, secret))
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))??;
    let (row, _) = state
        .store
        .get_variable(name)?
        .ok_or_else(|| ApiError::Internal("variable vanished".into()))?;
    state.stream.publish(
        "variable.updated",
        name.to_string(),
        serde_json::to_value(&row).unwrap_or_default(),
    );
    Ok(row)
}

#[utoipa::path(get, path = "/api/variables/{name}", params(("name" = String, Path), VariableQuery), responses((status = 200, body = VariableWithRaw), (status = 404)))]
pub async fn get_variable(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Query(q): Query<VariableQuery>,
) -> ApiResult<Json<VariableWithRaw>> {
    let (row, raw) = state
        .store
        .get_variable(&name)?
        .ok_or_else(|| ApiError::NotFound("variable not found".into()))?;
    let raw = if row.secret && q.raw.unwrap_or(false) {
        Some(raw)
    } else {
        None
    };
    Ok(Json(VariableWithRaw { variable: row, raw }))
}

#[utoipa::path(patch, path = "/api/variables/{name}", params(("name" = String, Path)), request_body = VariablePatch, responses((status = 200, body = VariableRow), (status = 404)))]
pub async fn patch_variable(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Json(body): Json<VariablePatch>,
) -> ApiResult<Json<VariableRow>> {
    let (current, raw) = state
        .store
        .get_variable(&name)?
        .ok_or_else(|| ApiError::NotFound("variable not found".into()))?;
    let secret = body.secret.unwrap_or(current.secret);
    let stored = match body.value {
        Some(v) => store_value(&state, &v, secret)?,
        None if secret == current.secret => raw,
        None => {
            // Toggling secrecy re-encodes the existing value.
            let plain = if current.secret {
                secrets::decrypt(&state.config.home, &raw)
                    .map_err(|e| ApiError::Internal(e.to_string()))?
            } else {
                raw
            };
            store_value(
                &state,
                &serde_json::from_str(&plain).unwrap_or(Value::String(plain.clone())),
                secret,
            )?
        }
    };
    let tags =
        serde_json::to_string(&body.tags.unwrap_or(current.tags)).unwrap_or_else(|_| "[]".into());
    let st = state.clone();
    let n = name.clone();
    tokio::task::spawn_blocking(move || st.store.set_variable(&n, &stored, &tags, secret))
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))??;
    let (row, _) = state
        .store
        .get_variable(&name)?
        .ok_or_else(|| ApiError::Internal("variable vanished".into()))?;
    state.stream.publish(
        "variable.updated",
        name,
        serde_json::to_value(&row).unwrap_or_default(),
    );
    Ok(Json(row))
}

#[utoipa::path(delete, path = "/api/variables/{name}", params(("name" = String, Path)), responses((status = 204), (status = 404)))]
pub async fn delete_variable(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> ApiResult<StatusCode> {
    if !state.store.delete_variable(&name)? {
        return Err(ApiError::NotFound("variable not found".into()));
    }
    state.stream.publish(
        "variable.updated",
        name.clone(),
        json!({"name": name, "deleted": true}),
    );
    Ok(StatusCode::NO_CONTENT)
}
