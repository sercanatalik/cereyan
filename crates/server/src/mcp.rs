//! Built-in MCP server: JSON-RPC 2.0 over Streamable HTTP at `/mcp`. Tools
//! wrap the same internal entry points the REST handlers use; sessions only
//! carry the client name so runs an agent starts are attributable.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use cereyan_core::{now_micros, StateType};
use cereyan_store::{ArtifactFilter, EventFilter, ListRunsFilter, LogFilter};
use serde_json::{json, Map, Value};

use crate::api::backfills::{self, BackfillBody};
use crate::api::error::ApiError;
use crate::api::{observability, runs};
use crate::state::AppState;

pub const PROTOCOL_VERSION: &str = "2025-06-18";
const SESSION_HEADER: &str = "mcp-session-id";
const SESSION_TTL: Duration = Duration::from_secs(3600);
const MAX_SESSIONS: usize = 1000;

/// In-memory MCP sessions: id to client name and last activity.
#[derive(Default)]
pub struct Sessions {
    inner: Mutex<HashMap<String, (String, Instant)>>,
}

impl Sessions {
    fn create(&self, client: String) -> String {
        let id = cereyan_core::new_id().to_string();
        let mut map = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        map.retain(|_, (_, seen)| now.duration_since(*seen) < SESSION_TTL);
        if map.len() >= MAX_SESSIONS {
            let oldest = map
                .iter()
                .min_by_key(|(_, (_, seen))| *seen)
                .map(|(k, _)| k.clone());
            if let Some(k) = oldest {
                map.remove(&k);
            }
        }
        map.insert(id.clone(), (client, now));
        id
    }

    fn client(&self, id: Option<&str>) -> String {
        let Some(id) = id else {
            return "unknown".into();
        };
        let mut map = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        match map.get_mut(id) {
            Some((client, seen)) => {
                *seen = Instant::now();
                client.clone()
            }
            None => "unknown".into(),
        }
    }

    fn remove(&self, id: &str) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id)
            .is_some()
    }
}

fn rpc_error(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message.into()}})
}

fn rpc_result(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn api_error_message(e: ApiError) -> String {
    match e {
        ApiError::NotFound(m) | ApiError::BadRequest(m) | ApiError::Unprocessable(m) => m,
        ApiError::Internal(m) => format!("internal error: {m}"),
        ApiError::Conflict(v) => v
            .get("error")
            .and_then(|e| e.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| v.to_string()),
    }
}

pub async fn handle_get() -> Response {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        Json(json!({"error": "this MCP endpoint answers POST requests with JSON; no server-initiated stream"})),
    )
        .into_response()
}

pub async fn handle_delete(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let Some(id) = headers.get(SESSION_HEADER).and_then(|v| v.to_str().ok()) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    if state.mcp.remove(id) {
        StatusCode::NO_CONTENT.into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

pub async fn handle_post(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let message: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return Json(rpc_error(Value::Null, -32700, format!("parse error: {e}")))
                .into_response()
        }
    };
    if message.is_array() {
        return Json(rpc_error(
            Value::Null,
            -32600,
            "batches are not supported; send one message per request",
        ))
        .into_response();
    }
    let session = headers
        .get(SESSION_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let method = message
        .get("method")
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    let id = message.get("id").cloned();
    let Some(id) = id else {
        // A notification: acknowledge without a body.
        return StatusCode::ACCEPTED.into_response();
    };
    if method == "initialize" {
        let client = params
            .get("clientInfo")
            .and_then(|c| c.get("name"))
            .and_then(|n| n.as_str())
            .unwrap_or("unknown")
            .to_string();
        let sid = state.mcp.create(client);
        let result = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {"tools": {}, "resources": {}, "prompts": {}},
            "serverInfo": {"name": "cereyan", "version": state.config.version},
            "instructions": "cereyan runs Python pipelines on this machine. Use list_flows to see what can run, run_flow to start work, get_run and run_logs to follow it, and explain_failure when a run fails. Writes (run_flow, cancel_run, resume_run, backfill, pause_schedule, resume_schedule, set_variable) take effect immediately.",
        });
        let mut resp = Json(rpc_result(id, result)).into_response();
        if let Ok(v) = header::HeaderValue::from_str(&sid) {
            resp.headers_mut().insert(SESSION_HEADER, v);
        }
        return resp;
    }
    let client = state.mcp.client(session.as_deref());
    let reply = match method.as_str() {
        "ping" => rpc_result(id, json!({})),
        "tools/list" => rpc_result(id, json!({"tools": tool_list()})),
        "tools/call" => {
            let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let args = params
                .get("arguments")
                .and_then(|a| a.as_object())
                .cloned()
                .unwrap_or_default();
            match call_tool(&state, &client, name, &args).await {
                Ok(v) => rpc_result(
                    id,
                    json!({"content": [{"type": "text", "text": pretty(&v)}], "isError": false}),
                ),
                Err(ToolError::Unknown(n)) => rpc_error(id, -32602, format!("unknown tool {n:?}")),
                Err(ToolError::Failed(msg)) => rpc_result(
                    id,
                    json!({"content": [{"type": "text", "text": msg}], "isError": true}),
                ),
            }
        }
        "resources/list" => rpc_result(id, json!({"resources": []})),
        "resources/templates/list" => rpc_result(
            id,
            json!({"resourceTemplates": [
                {"uriTemplate": "cereyan://runs/{id}/logs", "name": "Run logs", "description": "Log lines of a run as JSON", "mimeType": "application/json"},
                {"uriTemplate": "cereyan://runs/{id}/artifacts", "name": "Run artifacts", "description": "Artifacts of a run as JSON", "mimeType": "application/json"}
            ]}),
        ),
        "resources/read" => {
            let uri = params.get("uri").and_then(|u| u.as_str()).unwrap_or("");
            match read_resource(&state, uri) {
                Ok(text) => rpc_result(
                    id,
                    json!({"contents": [{"uri": uri, "mimeType": "application/json", "text": text}]}),
                ),
                Err(msg) => rpc_error(id, -32002, msg),
            }
        }
        "prompts/list" => rpc_result(
            id,
            json!({"prompts": [{
                "name": "diagnose_run",
                "description": "Explain why a run failed and what to do about it",
                "arguments": [{"name": "run_id", "description": "The run id", "required": true}]
            }]}),
        ),
        "prompts/get" => {
            let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
            if name != "diagnose_run" {
                rpc_error(id, -32602, format!("unknown prompt {name:?}"))
            } else {
                let run_id = params
                    .get("arguments")
                    .and_then(|a| a.get("run_id"))
                    .map(|v| {
                        v.as_str()
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| v.to_string())
                    })
                    .unwrap_or_default();
                let text = format!(
                    "Run {run_id} of a cereyan pipeline needs a diagnosis. Call the explain_failure tool with run_id {run_id}, read the failed task runs, the error log lines, and the events, then summarise the root cause in two sentences and suggest one concrete next step (rerun with run_flow, fix the code, or adjust a schedule)."
                );
                rpc_result(
                    id,
                    json!({"description": "Diagnose a failed run", "messages": [{"role": "user", "content": {"type": "text", "text": text}}]}),
                )
            }
        }
        other => rpc_error(id, -32601, format!("method {other:?} is not supported")),
    };
    Json(reply).into_response()
}

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
}

enum ToolError {
    Unknown(String),
    Failed(String),
}

impl From<ApiError> for ToolError {
    fn from(e: ApiError) -> Self {
        ToolError::Failed(api_error_message(e))
    }
}

impl From<cereyan_store::StoreError> for ToolError {
    fn from(e: cereyan_store::StoreError) -> Self {
        ToolError::Failed(api_error_message(ApiError::from(e)))
    }
}

fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": {"type": "object", "properties": properties, "required": required, "additionalProperties": false}
    })
}

pub fn tool_list() -> Vec<Value> {
    let flow_prop = json!({"type": "string", "description": "Flow name, or project/flow when the name exists in several projects"});
    vec![
        tool("list_flows", "List the registered flows with their project, parameters schema, tags, and any registration error. Read-only.",
            json!({"project": {"type": "string", "description": "Only flows of this project"}}), &[]),
        tool("list_runs", "List runs, newest first, with optional filters. Read-only.",
            json!({
                "flow": {"type": "string"}, "project": {"type": "string"},
                "state_type": {"type": "string", "description": "Scheduled, Pending, Running, Completed, Failed, Cancelled, Crashed, Paused, Cancelling"},
                "state_name": {"type": "string", "description": "A named sub-state such as Late or AwaitingRetry"},
                "name": {"type": "string", "description": "Exact run name"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 200, "default": 20}
            }), &[]),
        tool("get_run", "One run with its state, parameters, timing, and task runs. Read-only.",
            json!({"run_id": {"type": "integer"}}), &["run_id"]),
        tool("run_logs", "Log lines of a run, oldest first. Filter by minimum level (10 debug, 20 info, 30 warning, 40 error) or a search string. Read-only.",
            json!({"run_id": {"type": "integer"}, "min_level": {"type": "integer"}, "search": {"type": "string"}, "limit": {"type": "integer", "minimum": 1, "maximum": 1000, "default": 200}}), &["run_id"]),
        tool("list_events", "Recent events (run and task transitions, schedule changes, rule firings, custom events), newest first. Read-only.",
            json!({"name": {"type": "string", "description": "Exact name or a prefix ending in * such as run.*"}, "run_id": {"type": "integer"}, "flow_id": {"type": "integer"}, "limit": {"type": "integer", "minimum": 1, "maximum": 500, "default": 50}}), &[]),
        tool("list_artifacts", "Artifacts across runs, newest first, with their run, flow, and project. Read-only.",
            json!({"kind": {"type": "string"}, "key": {"type": "string"}, "flow": {"type": "string"}, "project": {"type": "string"}, "run_id": {"type": "integer"}, "limit": {"type": "integer", "minimum": 1, "maximum": 200, "default": 50}}), &[]),
        tool("list_rules", "The rules (reactive and proactive) with their match, actions, guards, and fire counts. Read-only.",
            json!({}), &[]),
        tool("explain_failure", "Everything needed to diagnose a run in one call: the run, its failed or crashed task runs, the last warning-or-above log lines, and the run's events. Read-only.",
            json!({"run_id": {"type": "integer"}}), &["run_id"]),
        tool("run_flow", "Start a run of a flow now. Creates the run immediately and returns without waiting; follow it with get_run. Parameters are validated against the flow's schema.",
            json!({"flow": flow_prop, "parameters": {"type": "object", "description": "Flow parameters as JSON"}, "name": {"type": "string", "description": "Optional run name"}, "tags": {"type": "array", "items": {"type": "string"}}}), &["flow"]),
        tool("cancel_run", "Cancel a run. A queued run is cancelled at once; a running run is asked to stop and killed after the grace period.",
            json!({"run_id": {"type": "integer"}}), &["run_id"]),
        tool("resume_run", "Answer a Paused run's wait_for_input question and schedule its next attempt. The answer can be any JSON.",
            json!({"run_id": {"type": "integer"}, "input": {"description": "The answer, any JSON"}}), &["run_id", "input"]),
        tool("backfill", "Create one run per value of a date or datetime parameter between start and end. Defaults to a dry run that only reports how many runs would be created; pass dry_run false to create them. Can create thousands of runs.",
            json!({"flow": flow_prop, "parameter": {"type": "string"}, "start": {"type": "string", "description": "YYYY-MM-DD or RFC 3339"}, "end": {"type": "string"}, "interval": {"type": "string", "description": "Seconds or a duration such as 1d or 12h (default 1d)"}, "concurrency": {"type": "integer", "default": 1}, "extra_parameters": {"type": "object"}, "reverse": {"type": "boolean"}, "dry_run": {"type": "boolean", "default": true}}), &["flow", "parameter", "start", "end"]),
        tool("pause_schedule", "Pause a schedule so it stops creating runs until resumed.",
            json!({"schedule_id": {"type": "integer"}}), &["schedule_id"]),
        tool("resume_schedule", "Resume a paused schedule.",
            json!({"schedule_id": {"type": "integer"}}), &["schedule_id"]),
        tool("set_variable", "Create or overwrite a variable. Secrets are encrypted at rest and never returned in plain text.",
            json!({"name": {"type": "string"}, "value": {"description": "Any JSON"}, "tags": {"type": "array", "items": {"type": "string"}}, "secret": {"type": "boolean", "default": false}}), &["name", "value"]),
    ]
}

fn arg_i64(args: &Map<String, Value>, key: &str) -> Result<i64, ToolError> {
    args.get(key)
        .and_then(|v| {
            v.as_i64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
        .ok_or_else(|| ToolError::Failed(format!("{key} must be an integer")))
}

fn arg_str(args: &Map<String, Value>, key: &str) -> Option<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
}

fn arg_usize(args: &Map<String, Value>, key: &str, default: usize, max: usize) -> usize {
    args.get(key)
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .unwrap_or(default)
        .clamp(1, max)
}

fn resolve_flow(
    state: &AppState,
    name: &str,
    project: Option<&str>,
) -> Result<cereyan_core::Flow, ToolError> {
    crate::rules::find_flow(state, name, project).map_err(ToolError::Failed)
}

async fn call_tool(
    state: &Arc<AppState>,
    client: &str,
    name: &str,
    args: &Map<String, Value>,
) -> Result<Value, ToolError> {
    match name {
        "list_flows" => {
            let project = arg_str(args, "project");
            let flows = state.store.list_flows(project.as_deref())?;
            let items: Vec<Value> = flows
                .into_iter()
                .map(|f| {
                    json!({
                        "id": f.id, "project": f.project, "name": f.name, "description": f.description,
                        "tags": f.tags, "parameter_schema": f.parameter_schema, "live": state.is_live(f.id),
                        "error": f.error, "options": f.options,
                    })
                })
                .collect();
            Ok(json!({"flows": items}))
        }
        "list_runs" => {
            let filter = ListRunsFilter {
                flow: arg_str(args, "flow"),
                project: arg_str(args, "project"),
                state_type: arg_str(args, "state_type"),
                state_name: arg_str(args, "state_name"),
                name: arg_str(args, "name"),
                limit: Some(arg_usize(args, "limit", 20, 200)),
                ..Default::default()
            };
            let page = state.store.list_runs(&filter)?;
            Ok(json!({"runs": page.items, "next_cursor": page.next_cursor}))
        }
        "get_run" => {
            let id = arg_i64(args, "run_id")?;
            let run = state
                .store
                .get_run(id)?
                .ok_or_else(|| ToolError::Failed(format!("run {id} not found")))?;
            let tasks = state.store.task_runs_by_run(id)?;
            Ok(json!({"run": run, "task_runs": tasks}))
        }
        "run_logs" => {
            let id = arg_i64(args, "run_id")?;
            let page = state.store.logs(&LogFilter {
                run_id: Some(id),
                min_level: args
                    .get("min_level")
                    .and_then(|v| v.as_i64())
                    .map(|v| v as i32),
                search: arg_str(args, "search"),
                limit: Some(arg_usize(args, "limit", 200, 1000)),
                ..Default::default()
            })?;
            Ok(json!({"logs": page.items, "next_cursor": page.next_cursor}))
        }
        "list_events" => {
            let page = state.store.query_events(&EventFilter {
                name: arg_str(args, "name"),
                run_id: args.get("run_id").and_then(|v| v.as_i64()),
                flow_id: args.get("flow_id").and_then(|v| v.as_i64()),
                limit: Some(arg_usize(args, "limit", 50, 500)),
                ..Default::default()
            })?;
            Ok(json!({"events": page.items, "next_cursor": page.next_cursor}))
        }
        "list_artifacts" => {
            let page = state.store.list_artifacts(&ArtifactFilter {
                kind: arg_str(args, "kind"),
                key: arg_str(args, "key"),
                flow: arg_str(args, "flow"),
                project: arg_str(args, "project"),
                run_id: args.get("run_id").and_then(|v| v.as_i64()),
                limit: Some(arg_usize(args, "limit", 50, 200)),
                after: None,
            })?;
            Ok(json!({"artifacts": page.items, "next_cursor": page.next_cursor}))
        }
        "list_rules" => Ok(json!({"rules": state.rules.all()})),
        "explain_failure" => {
            let id = arg_i64(args, "run_id")?;
            let run = state
                .store
                .get_run(id)?
                .ok_or_else(|| ToolError::Failed(format!("run {id} not found")))?;
            let failed: Vec<_> = state
                .store
                .task_runs_by_run(id)?
                .into_iter()
                .filter(|t| matches!(t.state.state_type, StateType::Failed | StateType::Crashed))
                .collect();
            let mut logs = state
                .store
                .logs(&LogFilter {
                    run_id: Some(id),
                    min_level: Some(30),
                    limit: Some(1000),
                    ..Default::default()
                })?
                .items;
            if logs.len() > 50 {
                logs = logs.split_off(logs.len() - 50);
            }
            let events = state
                .store
                .query_events(&EventFilter {
                    run_id: Some(id),
                    limit: Some(100),
                    ..Default::default()
                })?
                .items;
            let verdict = match run.state.state_type {
                StateType::Failed | StateType::Crashed => run
                    .state
                    .message
                    .clone()
                    .unwrap_or_else(|| "no failure message".into()),
                other => format!("the run is {}, not failed", other.as_str()),
            };
            Ok(json!({
                "run": run, "verdict": verdict, "failed_task_runs": failed,
                "error_logs": logs, "events": events,
            }))
        }
        "run_flow" => {
            let name = arg_str(args, "flow")
                .ok_or_else(|| ToolError::Failed("flow is required".into()))?;
            let flow = resolve_flow(state, &name, arg_str(args, "project").as_deref())?;
            let parameters = args
                .get("parameters")
                .and_then(|p| p.as_object())
                .cloned()
                .unwrap_or_default();
            let tags: Vec<String> = args
                .get("tags")
                .and_then(|t| t.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            let run = runs::create_run_inner(
                state,
                &flow,
                parameters,
                arg_str(args, "name"),
                tags,
                &format!("mcp:{client}"),
            )
            .await?;
            Ok(json!({"run": run, "note": "created; follow it with get_run"}))
        }
        "cancel_run" => {
            let id = arg_i64(args, "run_id")?;
            let run = state
                .store
                .get_run(id)?
                .ok_or_else(|| ToolError::Failed(format!("run {id} not found")))?;
            let run = runs::cancel_inner(state, &run).await?;
            Ok(json!({"run": run}))
        }
        "resume_run" => {
            let id = arg_i64(args, "run_id")?;
            let input = args.get("input").cloned().unwrap_or(Value::Null);
            let run = runs::resume_inner(state, id, input).await?;
            Ok(json!({"run": run}))
        }
        "backfill" => {
            let name = arg_str(args, "flow")
                .ok_or_else(|| ToolError::Failed("flow is required".into()))?;
            let flow = resolve_flow(state, &name, arg_str(args, "project").as_deref())?;
            let body = BackfillBody {
                parameter: arg_str(args, "parameter").unwrap_or_default(),
                start: arg_str(args, "start").unwrap_or_default(),
                end: arg_str(args, "end").unwrap_or_default(),
                interval: args.get("interval").cloned(),
                concurrency: args.get("concurrency").and_then(|v| v.as_i64()),
                extra_parameters: args
                    .get("extra_parameters")
                    .and_then(|v| v.as_object())
                    .cloned()
                    .unwrap_or_default(),
                reverse: args
                    .get("reverse")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
            };
            let dry_run = args
                .get("dry_run")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            if dry_run {
                let (values, interval) = backfills::plan_backfill(&flow, &body)?;
                return Ok(json!({
                    "dry_run": true, "flow": flow.name, "parameter": body.parameter,
                    "runs": values.len(), "first": values.first(), "last": values.last(),
                    "interval_seconds": interval,
                    "note": "nothing was created; call again with dry_run false to create these runs",
                }));
            }
            let status = backfills::create_backfill_inner(state, &flow, &body).await?;
            Ok(json!({"dry_run": false, "backfill": status}))
        }
        "pause_schedule" | "resume_schedule" => {
            let sid = arg_i64(args, "schedule_id")?;
            state
                .store
                .get_schedule(sid)?
                .ok_or_else(|| ToolError::Failed(format!("schedule {sid} not found")))?;
            let st = state.clone();
            let pausing = name == "pause_schedule";
            tokio::task::spawn_blocking(move || {
                if pausing {
                    crate::scheduler::pause(&st, sid, Some("paused"), None)
                } else {
                    crate::scheduler::resume(&st, sid)
                }
            })
            .await
            .map_err(|e| ToolError::Failed(e.to_string()))?;
            let row = state.store.get_schedule(sid)?;
            Ok(json!({"schedule": row}))
        }
        "set_variable" => {
            let vname = arg_str(args, "name")
                .ok_or_else(|| ToolError::Failed("name is required".into()))?;
            let value = args.get("value").cloned().unwrap_or(Value::Null);
            let tags: Vec<String> = args
                .get("tags")
                .and_then(|t| t.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            let secret = args
                .get("secret")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let row =
                observability::set_variable_inner(state, &vname, &value, &tags, secret).await?;
            Ok(json!({"variable": row}))
        }
        other => Err(ToolError::Unknown(other.to_string())),
    }
}

fn read_resource(state: &AppState, uri: &str) -> Result<String, String> {
    let rest = uri
        .strip_prefix("cereyan://runs/")
        .ok_or_else(|| format!("unknown resource {uri:?}"))?;
    let (id, what) = rest
        .split_once('/')
        .ok_or_else(|| format!("unknown resource {uri:?}"))?;
    let id: i64 = id.parse().map_err(|_| format!("bad run id in {uri:?}"))?;
    match what {
        "logs" => {
            let page = state
                .store
                .logs(&LogFilter {
                    run_id: Some(id),
                    limit: Some(1000),
                    ..Default::default()
                })
                .map_err(|e| e.to_string())?;
            Ok(pretty(
                &json!({"run_id": id, "logs": page.items, "as_of": now_micros()}),
            ))
        }
        "artifacts" => {
            let items = state
                .store
                .artifacts_by_run(id)
                .map_err(|e| e.to_string())?;
            Ok(pretty(&json!({"run_id": id, "artifacts": items})))
        }
        _ => Err(format!("unknown resource {uri:?}")),
    }
}
