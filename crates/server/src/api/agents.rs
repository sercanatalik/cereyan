//! Agents defined in the UI: a stored definition, registered as a flow on the
//! built-in runner, with a catalogue of what it may be given and an allowlist
//! that bounds it.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use cereyan_core::now_micros;
use cereyan_store::{AgentRow, AgentWrite, ApprovalRow, UpsertFlow};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use super::error::{ApiError, ApiResult};
use crate::state::AppState;

/// The module every UI agent's flow is registered on.
pub const RUNNER_MODULE: &str = "cereyan.pydantic_ai.runner";

const TOOL_KINDS: &[&str] = &["capability", "common", "mcp", "self", "code"];
const APPROVALS: &[&str] = &["ask", "approve", "deny"];

/// `[agents]` in cereyan.toml: what a UI-defined agent may start or reach.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AgentsConfig {
    /// Executables a stdio MCP server may start; empty allows none.
    #[serde(default)]
    pub mcp_commands: Vec<String>,
    /// URL patterns (`*` on the host) an HTTP MCP server may use, and `self`.
    #[serde(default)]
    pub mcp_urls: Vec<String>,
    /// A cap on the estimated cost of all agent runs in a UTC day; 0 is none.
    #[serde(default)]
    pub cost_per_day: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ToolSpec {
    pub kind: String,
    pub name: String,
    #[serde(default)]
    #[schema(value_type = Object)]
    pub options: Map<String, Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Policy {
    #[serde(default = "default_ask")]
    pub approvals: String,
    #[serde(default)]
    pub once: Vec<String>,
}

fn default_ask() -> String {
    "ask".into()
}

impl Default for Policy {
    fn default() -> Self {
        Policy {
            approvals: default_ask(),
            once: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Limits {
    #[serde(default)]
    pub tokens: Option<i64>,
    #[serde(default)]
    pub requests: Option<i64>,
    #[serde(default)]
    pub cost_usd: Option<f64>,
    #[serde(default)]
    pub timeout_seconds: Option<f64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Output {
    #[serde(default = "default_text")]
    pub kind: String,
    #[serde(default)]
    #[schema(value_type = Object)]
    pub schema: Option<Value>,
}

fn default_text() -> String {
    "text".into()
}

impl Default for Output {
    fn default() -> Self {
        Output {
            kind: default_text(),
            schema: None,
        }
    }
}

/// What the UI stores for an agent and the runner builds from.
#[derive(Clone, Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct AgentDefinition {
    pub name: String,
    #[serde(default = "default_project")]
    pub project: String,
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub description: String,
    pub model: String,
    /// A secret Variable's name; never the key itself.
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub instructions: String,
    #[serde(default)]
    pub prompt: String,
    #[serde(default)]
    pub tools: Vec<ToolSpec>,
    #[serde(default)]
    pub policy: Policy,
    #[serde(default)]
    pub limits: Limits,
    #[serde(default)]
    pub output: Output,
    #[serde(default)]
    pub crash_retries: i64,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_project() -> String {
    "agents".into()
}

fn default_true() -> bool {
    true
}

/// An agent as the API returns it: the row, plus what the runner checks at start.
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct AgentResponse {
    #[serde(flatten)]
    pub row: AgentRow,
    /// Why the allowlist refuses it now, if it does.
    pub allow_error: Option<String>,
    /// The flow's parameter schema derived from the templates.
    #[schema(value_type = Object)]
    pub parameter_schema: Value,
    /// Estimated cost of today's agent runs and the daily cap (0 is none).
    pub cost_today: f64,
    pub cost_cap: f64,
    /// This agent's runs, tokens and estimated cost over the past 24 hours.
    pub usage_24h: Usage24h,
}

#[derive(Clone, Debug, Default, Serialize, utoipa::ToSchema)]
pub struct Usage24h {
    pub runs: i64,
    pub tokens: i64,
    pub cost_usd: f64,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct ApprovalBody {
    pub tool: String,
    #[serde(default)]
    #[schema(value_type = Object)]
    pub args: Value,
    #[serde(default)]
    pub run_id: Option<i64>,
}

// ---- validation and the allowlist -----------------------------------------------

/// `{name}` holes of a template, in first-seen order, without duplicates.
pub fn template_variables(texts: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for text in texts {
        let chars: Vec<char> = text.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            if chars[i] == '{' {
                if i + 1 < chars.len() && chars[i + 1] == '{' {
                    i += 2;
                    continue;
                }
                let start = i + 1;
                let mut j = start;
                while j < chars.len() && (chars[j].is_ascii_alphanumeric() || chars[j] == '_') {
                    j += 1;
                }
                if j > start
                    && j < chars.len()
                    && chars[j] == '}'
                    && (chars[start].is_ascii_alphabetic() || chars[start] == '_')
                    && !(j + 1 < chars.len() && chars[j + 1] == '}')
                {
                    let name: String = chars[start..j].iter().collect();
                    if !out.contains(&name) {
                        out.push(name);
                    }
                    i = j + 1;
                    continue;
                }
            }
            i += 1;
        }
    }
    out
}

/// The flow's parameters: `prompt` with the template as its default, and
/// every other template variable as an optional string.
pub fn parameter_schema(d: &AgentDefinition) -> Value {
    let mut properties = Map::new();
    properties.insert(
        "prompt".into(),
        json!({"type": "string", "default": d.prompt}),
    );
    for name in template_variables(&[&d.prompt, &d.instructions]) {
        if name != "prompt" {
            properties.insert(name, json!({"type": "string", "default": ""}));
        }
    }
    json!({"type": "object", "properties": properties})
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 100
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
}

/// The shape checks the store does not make; a failure names the field.
fn validate(state: &AppState, d: &AgentDefinition) -> Result<(), String> {
    if !valid_name(&d.name) {
        return Err("name: letters, digits, '_', '-' and '.', up to 100 characters".into());
    }
    if !valid_name(&d.project) {
        return Err("project: letters, digits, '_', '-' and '.', up to 100 characters".into());
    }
    if d.model.trim().is_empty() {
        return Err("model: required".into());
    }
    if let Some(key) = &d.api_key {
        match state.store.get_variable(key) {
            Ok(Some((row, _))) if row.secret => {}
            Ok(Some(_)) => return Err(format!("api_key: variable {key:?} is not secret")),
            Ok(None) => return Err(format!("api_key: no variable named {key:?}")),
            Err(e) => return Err(format!("api_key: {e}")),
        }
    }
    for (i, tool) in d.tools.iter().enumerate() {
        if !TOOL_KINDS.contains(&tool.kind.as_str()) {
            return Err(format!(
                "tools[{i}].kind: must be one of {}",
                TOOL_KINDS.join(", ")
            ));
        }
        if tool.kind == "code" {
            return Err(format!("tools[{i}].kind: code tools are not available yet"));
        }
        if tool.name.is_empty() {
            return Err(format!("tools[{i}].name: required"));
        }
        if tool.kind == "mcp" {
            let url = tool.options.get("url").and_then(|v| v.as_str());
            let command = tool.options.get("command").and_then(|v| v.as_str());
            if url.is_none() && command.is_none() {
                return Err(format!(
                    "tools[{i}].options: an MCP server needs url or command"
                ));
            }
        }
    }
    if !APPROVALS.contains(&d.policy.approvals.as_str()) {
        return Err("policy.approvals: must be ask, approve or deny".into());
    }
    for (name, value) in [
        ("tokens", d.limits.tokens.map(|v| v as f64)),
        ("requests", d.limits.requests.map(|v| v as f64)),
        ("cost_usd", d.limits.cost_usd),
        ("timeout_seconds", d.limits.timeout_seconds),
    ] {
        if value.is_some_and(|v| v < 0.0) {
            return Err(format!("limits.{name}: must be zero or more"));
        }
    }
    match d.output.kind.as_str() {
        "text" => {}
        "json" => {
            if !d.output.schema.as_ref().is_some_and(|s| s.is_object()) {
                return Err("output.schema: required for json output".into());
            }
        }
        _ => return Err("output.kind: must be text or json".into()),
    }
    if d.crash_retries < 0 {
        return Err("crash_retries: must be zero or more".into());
    }
    Ok(())
}

/// Whether `url` matches a pattern with `*` standing for any run of characters
/// other than `/`.
fn url_matches(pattern: &str, url: &str) -> bool {
    fn go(p: &[char], u: &[char]) -> bool {
        match (p.first(), u.first()) {
            (None, None) => true,
            (None, Some(_)) => false,
            (Some('*'), _) => (0..=u.len())
                .take_while(|&k| k == 0 || u[k - 1] != '/')
                .any(|k| go(&p[1..], &u[k..])),
            (Some(a), Some(b)) if a == b => go(&p[1..], &u[1..]),
            _ => false,
        }
    }
    let p: Vec<char> = pattern.chars().collect();
    let u: Vec<char> = url.chars().collect();
    go(&p, &u)
}

/// Why the allowlist refuses the definition's tools, if it does.
pub fn allow_error(cfg: &AgentsConfig, d: &AgentDefinition) -> Option<String> {
    for tool in &d.tools {
        match tool.kind.as_str() {
            "self" => {
                if !cfg.mcp_urls.iter().any(|u| u == "self") {
                    return Some(
                        "the server's own MCP tools are not allowed: add \"self\" to [agents] mcp_urls in cereyan.toml"
                            .into(),
                    );
                }
            }
            "mcp" => {
                if let Some(command) = tool.options.get("command").and_then(|v| v.as_str()) {
                    let base = command.rsplit('/').next().unwrap_or(command);
                    let allowed = cfg
                        .mcp_commands
                        .iter()
                        .any(|c| c == command || (!c.contains('/') && c == base));
                    if !allowed {
                        return Some(format!(
                            "command {command:?} is not in [agents] mcp_commands in cereyan.toml"
                        ));
                    }
                } else if let Some(url) = tool.options.get("url").and_then(|v| v.as_str()) {
                    let allowed = cfg
                        .mcp_urls
                        .iter()
                        .any(|p| p != "self" && url_matches(p, url));
                    if !allowed {
                        return Some(format!(
                            "url {url:?} matches nothing in [agents] mcp_urls in cereyan.toml"
                        ));
                    }
                }
            }
            _ => {}
        }
    }
    None
}

// ---- registration as a flow -------------------------------------------------------

fn flow_options(row: &AgentRow, d: &AgentDefinition) -> Value {
    json!({
        "agent": true,
        "agent_id": row.id,
        "runs_on": "server",
        "crash_retries": if d.crash_retries > 0 { Some(d.crash_retries) } else { None },
        "timeout_seconds": d.limits.timeout_seconds,
        "priority": 0,
        "on_overlap": "enqueue",
        "schedules": [],
    })
}

/// Register, or refresh, the flow an agent runs as; the flow is live for this server.
pub fn register_flow(state: &AppState, row: &AgentRow, d: &AgentDefinition) -> ApiResult<i64> {
    let source_dir = state
        .config
        .served_dir
        .clone()
        .unwrap_or_else(|| state.config.home.clone());
    let flow_id = state.store.upsert_flow_full(UpsertFlow {
        project: row.project.clone(),
        name: row.name.clone(),
        module: RUNNER_MODULE.into(),
        source_dir: source_dir.display().to_string(),
        description: if d.description.is_empty() {
            None
        } else {
            Some(d.description.clone())
        },
        tags: "[\"agent\"]".into(),
        parameter_schema: parameter_schema(d).to_string(),
        options: flow_options(row, d).to_string(),
        group: d.group.clone(),
    })?;
    state.store.set_agent_flow(row.id, Some(flow_id))?;
    state
        .live_flows
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .insert(flow_id);
    state.invalidate_dep_graph();
    state.stream.publish(
        "flow.registered",
        flow_id.to_string(),
        json!({"id": flow_id, "agent": row.id}),
    );
    Ok(flow_id)
}

/// Register every stored agent's flow at start, so the flows exist without a
/// module to import them from.
pub fn register_all(state: &AppState) {
    let rows = match state.store.list_agents() {
        Ok(rows) => rows,
        Err(e) => {
            eprintln!("warning: agents could not be read: {e}");
            return;
        }
    };
    for row in rows {
        let Ok(d) = serde_json::from_value::<AgentDefinition>(row.definition.clone()) else {
            eprintln!(
                "warning: agent {}/{} has a definition this server cannot read",
                row.project, row.name
            );
            continue;
        };
        if let Err(e) = register_flow(state, &row, &d) {
            eprintln!(
                "warning: agent {}/{} could not be registered: {e:?}",
                row.project, row.name
            );
        }
    }
}

fn day_start() -> i64 {
    let now = now_micros();
    now - now.rem_euclid(86_400_000_000)
}

fn respond(state: &AppState, row: AgentRow) -> ApiResult<AgentResponse> {
    let d: AgentDefinition = serde_json::from_value(row.definition.clone())
        .map_err(|e| ApiError::Internal(format!("stored definition: {e}")))?;
    let cost_today = state.store.agent_cost_since(day_start())?;
    let usage_24h = match row.flow_id {
        Some(flow_id) => {
            let (runs, tokens, cost_usd) = state
                .store
                .agent_usage_since(flow_id, now_micros() - 86_400_000_000)?;
            Usage24h {
                runs,
                tokens,
                cost_usd,
            }
        }
        None => Usage24h::default(),
    };
    Ok(AgentResponse {
        allow_error: allow_error(&state.config.agents, &d),
        parameter_schema: parameter_schema(&d),
        cost_today,
        cost_cap: state.config.agents.cost_per_day,
        usage_24h,
        row,
    })
}

// ---- routes ------------------------------------------------------------------------

#[utoipa::path(get, path = "/api/agents", responses((status = 200, body = Vec<AgentResponse>)))]
pub async fn list_agents(
    State(state): State<Arc<AppState>>,
) -> ApiResult<Json<Vec<AgentResponse>>> {
    let rows = state.store.list_agents()?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(respond(&state, row)?);
    }
    Ok(Json(out))
}

#[utoipa::path(post, path = "/api/agents", request_body = AgentDefinition, responses((status = 201, body = AgentResponse), (status = 422)))]
pub async fn create_agent(
    State(state): State<Arc<AppState>>,
    Json(d): Json<AgentDefinition>,
) -> ApiResult<(StatusCode, Json<AgentResponse>)> {
    validate(&state, &d).map_err(ApiError::Unprocessable)?;
    if let Some(reason) = allow_error(&state.config.agents, &d) {
        return Err(ApiError::Unprocessable(reason));
    }
    let row = state.store.upsert_agent(AgentWrite {
        id: None,
        project: d.project.clone(),
        name: d.name.clone(),
        source: "ui".into(),
        enabled: d.enabled,
        definition: serde_json::to_string(&d).unwrap_or_default(),
    })?;
    register_flow(&state, &row, &d)?;
    let row = state
        .store
        .get_agent(row.id)?
        .ok_or_else(|| ApiError::Internal("agent vanished".into()))?;
    Ok((StatusCode::CREATED, Json(respond(&state, row)?)))
}

#[utoipa::path(get, path = "/api/agents/catalogue", responses((status = 200, description = "What agents on this server may be given", body = Object)))]
pub async fn catalogue(State(state): State<Arc<AppState>>) -> Json<Value> {
    let mut out = state.config.agent_catalogue.clone();
    if !out.is_object() {
        out = json!({"installed": false});
    }
    let self_tools: Vec<Value> = crate::mcp::tool_list()
        .into_iter()
        .map(|t| {
            json!({
                "name": t["name"],
                "description": t["description"],
                "read_only": t["annotations"]["readOnlyHint"].as_bool().unwrap_or(false),
            })
        })
        .collect();
    if let Some(map) = out.as_object_mut() {
        map.insert(
            "self".into(),
            json!({
                "ready": state.config.agents.mcp_urls.iter().any(|u| u == "self"),
                "tools": self_tools,
            }),
        );
        map.insert(
            "allowlist".into(),
            serde_json::to_value(&state.config.agents).unwrap_or_default(),
        );
        map.insert(
            "features".into(),
            Value::Object(state.config.features.clone()),
        );
    }
    Json(out)
}

#[utoipa::path(get, path = "/api/agents/{id}", params(("id" = i64, Path)), responses((status = 200, body = AgentResponse), (status = 404)))]
pub async fn get_agent(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> ApiResult<Json<AgentResponse>> {
    let row = state
        .store
        .get_agent(id)?
        .ok_or_else(|| ApiError::NotFound("agent not found".into()))?;
    Ok(Json(respond(&state, row)?))
}

#[utoipa::path(patch, path = "/api/agents/{id}", params(("id" = i64, Path)), request_body = AgentDefinition, responses((status = 200, body = AgentResponse), (status = 404), (status = 422)))]
pub async fn patch_agent(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Json(d): Json<AgentDefinition>,
) -> ApiResult<Json<AgentResponse>> {
    let current = state
        .store
        .get_agent(id)?
        .ok_or_else(|| ApiError::NotFound("agent not found".into()))?;
    validate(&state, &d).map_err(ApiError::Unprocessable)?;
    if let Some(reason) = allow_error(&state.config.agents, &d) {
        return Err(ApiError::Unprocessable(reason));
    }
    if (current.project != d.project || current.name != d.name) && current.flow_id.is_some() {
        // The identity moves: the old flow stays as history, no longer live.
        let old = current.flow_id.unwrap_or(0);
        state
            .live_flows
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&old);
    }
    let row = state.store.upsert_agent(AgentWrite {
        id: Some(id),
        project: d.project.clone(),
        name: d.name.clone(),
        source: current.source.clone(),
        enabled: d.enabled,
        definition: serde_json::to_string(&d).unwrap_or_default(),
    })?;
    register_flow(&state, &row, &d)?;
    let row = state
        .store
        .get_agent(id)?
        .ok_or_else(|| ApiError::Internal("agent vanished".into()))?;
    Ok(Json(respond(&state, row)?))
}

#[utoipa::path(delete, path = "/api/agents/{id}", params(("id" = i64, Path)), responses((status = 204), (status = 404)))]
pub async fn delete_agent(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    let row = state
        .store
        .get_agent(id)?
        .ok_or_else(|| ApiError::NotFound("agent not found".into()))?;
    if let Some(flow_id) = row.flow_id {
        // The flow stays as history with its runs, as a flow does when its
        // module stops being served: it is no longer live, so nothing starts it.
        for run in state.index.active_runs() {
            if run.flow_id == flow_id {
                state.supervisor.dequeue(run.id);
            }
        }
        state
            .live_flows
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&flow_id);
        state.invalidate_dep_graph();
        state.stream.publish(
            "flow.registered",
            flow_id.to_string(),
            json!({"id": flow_id, "agent": id, "unregistered": true}),
        );
    }
    state.store.delete_agent(id)?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(get, path = "/api/agents/{id}/approvals", params(("id" = i64, Path)), responses((status = 200, body = Vec<ApprovalRow>), (status = 404)))]
pub async fn list_approvals(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> ApiResult<Json<Vec<ApprovalRow>>> {
    state
        .store
        .get_agent(id)?
        .ok_or_else(|| ApiError::NotFound("agent not found".into()))?;
    Ok(Json(state.store.list_approvals(id)?))
}

/// The hash a remembered approval is matched on: the tool and its arguments as canonical JSON.
pub fn approval_hash(tool: &str, args: &Value) -> String {
    let text = canonical(args);
    let digest = Sha256::digest(format!("{tool}\0{text}").as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn canonical(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let inner: Vec<String> = keys
                .into_iter()
                .map(|k| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(k).unwrap_or_default(),
                        canonical(&map[k])
                    )
                })
                .collect();
            format!("{{{}}}", inner.join(","))
        }
        Value::Array(items) => {
            let inner: Vec<String> = items.iter().map(canonical).collect();
            format!("[{}]", inner.join(","))
        }
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

#[utoipa::path(post, path = "/api/agents/{id}/approvals", params(("id" = i64, Path)), request_body = ApprovalBody, responses((status = 201, body = ApprovalRow), (status = 404)))]
pub async fn add_approval(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Json(body): Json<ApprovalBody>,
) -> ApiResult<(StatusCode, Json<ApprovalRow>)> {
    state
        .store
        .get_agent(id)?
        .ok_or_else(|| ApiError::NotFound("agent not found".into()))?;
    let row = state.store.add_approval(
        id,
        body.tool.clone(),
        approval_hash(&body.tool, &body.args),
        body.args.to_string(),
        body.run_id,
    )?;
    Ok((StatusCode::CREATED, Json(row)))
}

#[utoipa::path(delete, path = "/api/agents/{id}/approvals/{aid}", params(("id" = i64, Path), ("aid" = i64, Path)), responses((status = 204), (status = 404)))]
pub async fn delete_approval(
    State(state): State<Arc<AppState>>,
    Path((id, aid)): Path<(i64, i64)>,
) -> ApiResult<StatusCode> {
    if state.store.delete_approval(id, aid)? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound("approval not found".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_variables_skip_doubled_braces_and_repeat_names() {
        assert_eq!(
            template_variables(&["Digest for {day} since {since}", "{{json}} {day} {1bad}"]),
            vec!["day".to_string(), "since".to_string()]
        );
    }

    #[test]
    fn url_patterns_wildcard_a_host_segment() {
        assert!(url_matches(
            "https://*.internal.example.com/mcp",
            "https://docs.internal.example.com/mcp"
        ));
        assert!(!url_matches(
            "https://*.internal.example.com/mcp",
            "https://evil.com/internal.example.com/mcp"
        ));
        assert!(url_matches("https://host/mcp", "https://host/mcp"));
        assert!(!url_matches("https://host/mcp", "https://host/mcp2"));
    }

    #[test]
    fn the_allowlist_names_the_offending_entry() {
        let cfg = AgentsConfig {
            mcp_commands: vec!["npx".into()],
            mcp_urls: vec!["self".into()],
            cost_per_day: 0.0,
        };
        let mut d: AgentDefinition =
            serde_json::from_value(json!({"name": "a", "model": "m"})).unwrap();
        d.tools.push(ToolSpec {
            kind: "mcp".into(),
            name: "crm".into(),
            options: json!({"command": "bash"}).as_object().unwrap().clone(),
        });
        let reason = allow_error(&cfg, &d).unwrap();
        assert!(reason.contains("bash") && reason.contains("mcp_commands"));
        d.tools[0].options = json!({"command": "/usr/local/bin/npx"})
            .as_object()
            .unwrap()
            .clone();
        assert!(allow_error(&cfg, &d).is_none());
        d.tools.push(ToolSpec {
            kind: "self".into(),
            name: "cereyan".into(),
            options: Map::new(),
        });
        assert!(allow_error(&cfg, &d).is_none());
        let approval = approval_hash("post", &json!({"b": 1, "a": [1, 2]}));
        assert_eq!(
            approval,
            approval_hash("post", &json!({"a": [1, 2], "b": 1}))
        );
    }
}
