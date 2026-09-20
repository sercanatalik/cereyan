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
use axum::{Extension, Json};
use cereyan_core::{now_micros, ScheduleRow, StateType};
use cereyan_store::{ArtifactFilter, EventFilter, ListRunsFilter, LogFilter};
use serde_json::{json, Map, Value};

use crate::api::backfills::{self, BackfillBody};
use crate::api::error::ApiError;
use crate::api::schedules::{ScheduleBody, SchedulePatchBody};
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

/// Whether the request declares a JSON body, ignoring parameters and case.
fn is_json(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .is_some_and(|media| media.trim().eq_ignore_ascii_case("application/json"))
}

fn api_error_message(e: ApiError) -> String {
    match e {
        ApiError::NotFound(m)
        | ApiError::BadRequest(m)
        | ApiError::Unprocessable(m)
        | ApiError::Unavailable(m) => m,
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
    user: Option<Extension<crate::auth::AuthenticatedUser>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    // A page can post text/plain across sites without a preflight; JSON cannot.
    if !is_json(&headers) {
        return (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Json(rpc_error(
                Value::Null,
                -32600,
                "send the message with Content-Type: application/json",
            )),
        )
            .into_response();
    }
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
            "instructions": if state.config.mcp_read_only {
                "cereyan runs Python pipelines on this machine. This server's MCP endpoint is read-only: only tools that change nothing are listed, and any other tool call is refused. Use list_flows to see what is registered, list_runs, get_run and run_logs to follow work, explain_failure when a run fails, and server_health for the engines and queue."
            } else {
                "cereyan runs Python pipelines on this machine. Use list_flows to see what can run, run_flow to start work, get_run and run_logs to follow it, and explain_failure when a run fails. Flows run on demand: a flow needs no schedule, and run_flow is how work usually starts. For work that should recur, list_schedules shows what is scheduled and the create, edit, delete, pause and resume schedule tools manage it. Writes take effect immediately."
            },
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
        "tools/list" => rpc_result(id, json!({"tools": visible_tools(&state)})),
        "tools/call" => {
            let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let args = params
                .get("arguments")
                .and_then(|a| a.as_object())
                .cloned()
                .unwrap_or_default();
            let user = user.as_ref().map(|Extension(u)| u);
            let refused = state.config.mcp_read_only
                && (tool_list()
                    .iter()
                    .any(|t| t["name"] == name && !is_read_only(t))
                    || name.starts_with("flow__"));
            let outcome = if refused {
                Err(ToolError::Failed(format!(
                    "this server's MCP endpoint is read-only (mcp_read_only): {name} changes state and is not available"
                )))
            } else {
                call_tool(&state, &client, user, name, &args).await
            };
            match outcome {
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
            json!({"prompts": [
                {
                    "name": "diagnose_run",
                    "description": "Explain why a run failed and what to do about it",
                    "arguments": [{"name": "run_id", "description": "The run id", "required": true}]
                },
                {
                    "name": "health_check",
                    "description": "Report what needs attention on this server: engines, queue, failed runs, and schedules",
                    "arguments": []
                },
                {
                    "name": "plan_backfill",
                    "description": "Plan a backfill as a dry run, review the range and count, then create the runs",
                    "arguments": [
                        {"name": "flow", "description": "Flow name, or project/flow", "required": true},
                        {"name": "parameter", "description": "The date or datetime parameter", "required": true},
                        {"name": "start", "description": "First value, YYYY-MM-DD or RFC 3339", "required": true},
                        {"name": "end", "description": "Last value, inclusive", "required": true}
                    ]
                }
            ]}),
        ),
        "prompts/get" => {
            let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let arg = |key: &str| -> String {
                params
                    .get("arguments")
                    .and_then(|a| a.get(key))
                    .map(|v| {
                        v.as_str()
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| v.to_string())
                    })
                    .unwrap_or_default()
            };
            let (description, text) = match name {
                "diagnose_run" => {
                    let run_id = arg("run_id");
                    ("Diagnose a failed run", format!(
                        "Run {run_id} of a cereyan pipeline needs a diagnosis. Call the explain_failure tool with run_id {run_id}, read the failed task runs, the error log lines, and the events, then summarise the root cause in two sentences and suggest one concrete next step (rerun with run_flow, fix the code, or adjust a schedule)."
                    ))
                }
                "health_check" => (
                    "Check the server's health",
                    "Check this cereyan server. Call server_health for the engines, queue, and resources; call list_runs with state_type Failed and again with state_type Crashed for recent problems; call list_schedules and note any schedule that is paused or whose next fire is missing. Report in a few lines what needs attention, most urgent first, and say when nothing does.".to_string(),
                ),
                "plan_backfill" => {
                    let (flow, parameter, start, end) = (arg("flow"), arg("parameter"), arg("start"), arg("end"));
                    ("Plan a backfill", format!(
                        "Plan a backfill of flow {flow} over parameter {parameter} from {start} to {end}. First call the backfill tool with those arguments and dry_run left at its default, which creates nothing, and review the number of runs and the first and last values it reports. If the range and count look right, call backfill again with dry_run false to create the runs, then report the backfill id so progress can be followed with get_backfill. If the count is surprising, stop and ask."
                    ))
                }
                _ => {
                    let reply = rpc_error(id, -32602, format!("unknown prompt {name:?}"));
                    return Json(reply).into_response();
                }
            };
            rpc_result(
                id,
                json!({"description": description, "messages": [{"role": "user", "content": {"type": "text", "text": text}}]}),
            )
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

fn tool_with(
    name: &str,
    description: &str,
    properties: Value,
    required: &[&str],
    read_only: bool,
    destructive: bool,
) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": {"type": "object", "properties": properties, "required": required, "additionalProperties": false},
        "annotations": {"readOnlyHint": read_only, "destructiveHint": destructive}
    })
}

/// A tool that changes nothing.
fn read_tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    tool_with(name, description, properties, required, true, false)
}

/// A tool that adds state: starts, creates, resumes, pauses.
fn write_tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    tool_with(name, description, properties, required, false, false)
}

/// A tool that cancels, deletes, or overwrites.
fn destructive_tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    tool_with(name, description, properties, required, false, true)
}

fn is_read_only(descriptor: &Value) -> bool {
    descriptor["annotations"]["readOnlyHint"]
        .as_bool()
        .unwrap_or(false)
}

fn tool_name_part(text: &str) -> String {
    text.chars()
        .map(|c| if c == '/' || c == '-' { '_' } else { c })
        .collect()
}

/// The `flow__<project>__<name>` tools: one per live flow registered with
/// `mcp_tool=True`. Two flows that map to one name keep the first.
pub fn flow_tools(state: &AppState) -> Vec<(String, cereyan_core::Flow)> {
    let mut out: Vec<(String, cereyan_core::Flow)> = Vec::new();
    for flow in state.store.list_flows(None).unwrap_or_default() {
        let published = flow
            .options
            .get("mcp_tool")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !published || !state.is_live(flow.id) {
            continue;
        }
        let name = format!(
            "flow__{}__{}",
            tool_name_part(&flow.project),
            tool_name_part(&flow.name)
        );
        if out.iter().any(|(n, _)| n == &name) {
            continue;
        }
        out.push((name, flow));
    }
    out
}

fn flow_tool_descriptor(name: &str, flow: &cereyan_core::Flow) -> Value {
    let mut schema = flow.parameter_schema.clone();
    if schema.get("type").is_none() {
        schema = json!({"type": "object", "properties": {}});
    }
    let about = flow
        .description
        .as_deref()
        .map(|d| format!(": {d}"))
        .unwrap_or_default();
    json!({
        "name": name,
        "description": format!("Start a run of flow {}/{}{}. Arguments are the flow's parameters. Returns the run; follow it with get_run.", flow.project, flow.name, about),
        "inputSchema": schema,
        "annotations": {"readOnlyHint": false, "destructiveHint": false}
    })
}

/// Every tool this server lists: the built-ins, then the flow tools, minus
/// everything that changes state when the server is read-only.
pub fn visible_tools(state: &AppState) -> Vec<Value> {
    let mut tools = tool_list();
    tools.extend(
        flow_tools(state)
            .iter()
            .map(|(name, flow)| flow_tool_descriptor(name, flow)),
    );
    if state.config.mcp_read_only {
        tools.retain(is_read_only);
    }
    tools
}

pub fn tool_list() -> Vec<Value> {
    let flow_prop = json!({"type": "string", "description": "Flow name, or project/flow when the name exists in several projects"});
    vec![
        read_tool("list_flows", "List the registered flows with their project, parameters schema, tags, and any registration error. Read-only.",
            json!({
                "project": {"type": "string", "description": "Only flows of this project"},
                "group": {"type": "string", "description": "Only flows of this group: the one a flow declared, else its project"}
            }), &[]),
        read_tool("list_runs", "List runs, newest first, with optional filters. Read-only.",
            json!({
                "flow": {"type": "string"}, "project": {"type": "string"},
                "group": {"type": "string", "description": "Only runs of flows in this group: the one a flow declared, else its project"},
                "state_type": {"type": "string", "description": "Scheduled, Pending, Running, Completed, Failed, Cancelled, Crashed, Paused, Cancelling"},
                "state_name": {"type": "string", "description": "A named sub-state such as Late or AwaitingRetry"},
                "name": {"type": "string", "description": "Exact run name"},
                "params": {"type": "string", "description": "Comma-separated key=value pairs the run's parameters must match, e.g. day=2026-09-01; searches the last 30 days unless start bounds are given"},
                "attributes": {"type": "string", "description": "Comma-separated key=value pairs the run's attributes (set_attributes) must match"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 200, "default": 20}
            }), &[]),
        read_tool("get_run", "One run with its state, parameters, timing, and task runs. Read-only.",
            json!({"run_id": {"type": "integer"}}), &["run_id"]),
        read_tool("compare_runs", "What changed between two runs: parameters and attributes, duration, each task's state and duration, the first task that diverged, error lines and the failure message new on the second run, and artifact differences. The first run is the baseline. Read-only.",
            json!({"run_id": {"type": "integer", "description": "The baseline, usually the last good run"}, "other_run_id": {"type": "integer", "description": "The run in question"}}), &["run_id", "other_run_id"]),
        read_tool("run_logs", "Log lines of a run, oldest first. Filter by minimum level (10 debug, 20 info, 30 warning, 40 error) or a search string. Read-only.",
            json!({"run_id": {"type": "integer"}, "min_level": {"type": "integer"}, "search": {"type": "string"}, "limit": {"type": "integer", "minimum": 1, "maximum": 1000, "default": 200}}), &["run_id"]),
        read_tool("list_events", "Recent events (run and task transitions, schedule changes, rule firings, custom events), newest first. Read-only.",
            json!({"name": {"type": "string", "description": "Exact name or a prefix ending in * such as run.*"}, "run_id": {"type": "integer"}, "flow_id": {"type": "integer"}, "limit": {"type": "integer", "minimum": 1, "maximum": 500, "default": 50}}), &[]),
        read_tool("list_artifacts", "Artifacts across runs, newest first, with their run, flow, and project. Read-only.",
            json!({"kind": {"type": "string"}, "key": {"type": "string"}, "flow": {"type": "string"}, "project": {"type": "string"}, "run_id": {"type": "integer"}, "limit": {"type": "integer", "minimum": 1, "maximum": 200, "default": 50}}), &[]),
        read_tool("list_rules", "The rules (reactive and proactive) with their match, actions, guards, and fire counts. Read-only.",
            json!({}), &[]),
        read_tool("list_schedules", "The schedules of one flow or of every flow: the spec, whether it is active, when it next fires, and whether it was declared in the flow's code, created in the interface, or created by an agent. Read-only.",
            json!({"flow": flow_prop, "project": {"type": "string", "description": "Only schedules of this project"}}), &[]),
        read_tool("explain_failure", "Everything needed to diagnose a run in one call: the run, its failed or crashed task runs, the last warning-or-above log lines, and the run's events. Read-only.",
            json!({"run_id": {"type": "integer"}}), &["run_id"]),
        write_tool("run_flow", "Start a run of a flow now, or at a later time with `at` or `delay_seconds`. Creates the run immediately and returns without waiting; follow it with get_run. Parameters are validated against the flow's schema.",
            json!({"flow": flow_prop, "parameters": {"type": "object", "description": "Flow parameters as JSON"}, "name": {"type": "string", "description": "Optional run name"}, "tags": {"type": "array", "items": {"type": "string"}},
                   "at": {"type": "string", "description": "Start at this time, ISO 8601, instead of now"},
                   "delay_seconds": {"type": "number", "minimum": 0, "description": "Start this many seconds from now; not with at"}}), &["flow"]),
        destructive_tool("cancel_run", "Cancel a run. A queued run is cancelled at once; a running run is asked to stop and killed after the grace period.",
            json!({"run_id": {"type": "integer"}}), &["run_id"]),
        write_tool("resume_run", "Answer a Paused run's wait_for_input question and schedule its next attempt. The answer can be any JSON.",
            json!({"run_id": {"type": "integer"}, "input": {"description": "The answer, any JSON"}}), &["run_id", "input"]),
        write_tool("backfill", "Create one run per value of a date or datetime parameter between start and end. Defaults to a dry run that only reports how many runs would be created; pass dry_run false to create them. Can create thousands of runs.",
            json!({"flow": flow_prop, "parameter": {"type": "string"}, "start": {"type": "string", "description": "YYYY-MM-DD or RFC 3339"}, "end": {"type": "string"}, "interval": {"type": "string", "description": "Seconds or a duration such as 1d or 12h (default 1d)"}, "concurrency": {"type": "integer", "default": 1}, "extra_parameters": {"type": "object"}, "reverse": {"type": "boolean"}, "dry_run": {"type": "boolean", "default": true}}), &["flow", "parameter", "start", "end"]),
        write_tool("create_schedule", "Make a flow run repeatedly. To run a flow once, now, use run_flow instead: a flow needs no schedule, and running on demand is the normal case. Returns the schedule and the next few times it will fire.",
            json!({
                "flow": flow_prop, "project": {"type": "string"},
                "kind": {"type": "string", "enum": ["cron", "interval", "rrule"], "description": "Which kind of schedule"},
                "cron": {"type": "string", "description": "Five-field cron expression, for kind cron"},
                "interval": {"type": "number", "description": "Seconds between fires, for kind interval"},
                "anchor": {"type": "integer", "description": "Microseconds UTC the interval counts from; defaults to now"},
                "rrule": {"type": "string", "description": "RFC 5545 RRULE, for kind rrule"},
                "timezone": {"type": "string", "description": "IANA name such as Europe/Istanbul; UTC when unset"},
                "day_or": {"type": "boolean", "description": "For cron, OR day-of-month with day-of-week (default true)"},
                "catchup": {"type": "string", "enum": ["skip", "latest", "all"], "default": "skip"},
                "catchup_max": {"type": "integer", "default": 100},
                "catchup_window": {"type": "integer", "description": "Seconds; missed fires older than this are not caught up (0 is off)"},
                "jitter": {"type": "integer", "description": "Seconds; each run is due up to this long after its fire time, deterministically (0 is off)"},
                "start_deadline": {"type": "integer", "description": "Seconds; a run not started this long after it was due is skipped (0 is off)"}
            }), &["flow", "kind"]),
        write_tool("edit_schedule", "Retime an existing schedule. Editing one that was declared in the flow's code lasts until the server restarts, when the declaration in the Python source applies again, and the result says so. Returns the schedule and the next few times it will fire.",
            json!({
                "schedule_id": {"type": "integer"},
                "cron": {"type": "string"}, "interval": {"type": "number"}, "anchor": {"type": "integer"},
                "rrule": {"type": "string"}, "timezone": {"type": "string"}, "day_or": {"type": "boolean"},
                "catchup": {"type": "string", "enum": ["skip", "latest", "all"]},
                "catchup_max": {"type": "integer"},
                "catchup_window": {"type": "integer", "description": "Seconds; 0 turns it off"},
                "jitter": {"type": "integer", "description": "Seconds; 0 turns it off"},
                "start_deadline": {"type": "integer", "description": "Seconds; 0 turns it off"}
            }), &["schedule_id"]),
        destructive_tool("delete_schedule", "Remove a schedule that was created in the interface or by an agent. A schedule declared in the flow's code cannot be removed this way, because the next restart recreates it from the declaration; pause_schedule stops that one durably.",
            json!({"schedule_id": {"type": "integer"}}), &["schedule_id"]),
        write_tool("pause_schedule", "Pause a schedule so it stops creating runs until resumed.",
            json!({"schedule_id": {"type": "integer"}}), &["schedule_id"]),
        write_tool("resume_schedule", "Resume a paused schedule.",
            json!({"schedule_id": {"type": "integer"}}), &["schedule_id"]),
        write_tool("pause_scheduler", "Pause every schedule at once, for maintenance or an incident. Nothing scheduled starts until resume_scheduler or `until`; running runs, manual runs and backfills continue. With suppress_rules, rules that would fire are recorded as suppressed instead of acting.",
            json!({
                "reason": {"type": "string", "description": "Shown in the UI banner and recorded on the scheduler.paused event"},
                "until": {"type": "string", "description": "When to resume on its own, ISO 8601; omit to hold until resume_scheduler"},
                "suppress_rules": {"type": "boolean", "default": false}
            }), &[]),
        write_tool("resume_scheduler", "End the global pause: held runs start and each schedule catches up the fires it missed under its own policy.",
            json!({}), &[]),
        destructive_tool("set_variable", "Create or overwrite a variable. Secrets are encrypted at rest and never returned in plain text.",
            json!({"name": {"type": "string"}, "value": {"description": "Any JSON"}, "tags": {"type": "array", "items": {"type": "string"}}, "secret": {"type": "boolean", "default": false}}), &["name", "value"]),
        read_tool("list_backfills", "Backfills, newest first, each with its counts of runs by state. Read-only.",
            json!({"flow": flow_prop, "project": {"type": "string"}}), &[]),
        read_tool("get_backfill", "One backfill with its counts of runs by state. Read-only.",
            json!({"backfill_id": {"type": "integer"}}), &["backfill_id"]),
        destructive_tool("cancel_backfill", "Cancel a backfill: its queued runs are cancelled at once and its running runs are asked to stop.",
            json!({"backfill_id": {"type": "integer"}}), &["backfill_id"]),
        read_tool("get_flow_source", "The Python source of the module that registered a flow, read from the flow's own source directory and cut at 64 KB. Read-only.",
            json!({"flow": flow_prop, "project": {"type": "string"}}), &["flow"]),
        read_tool("server_health", "The server's state in one call: engines and what they are running, queue length, resource usage, schedule count, whether the scheduler is paused, and whether a token is required or the server is exposed. Read-only.",
            json!({}), &[]),
        read_tool("list_variables", "Every variable's name, tags, and timestamps; the value only when it is not a secret. Read-only.",
            json!({}), &[]),
        read_tool("list_resources", "Resource totals from [resources] and what is in use. Read-only.",
            json!({}), &[]),
        read_tool("flow_dependencies", "What a flow runs after (its upstreams and batch key) and which flows run after it. Read-only.",
            json!({"flow": flow_prop, "project": {"type": "string"}}), &["flow"]),
        read_tool("check_flows", "Run `cereyan check --json` on the served directory in a child process and return its report: import failures, unknown upstreams, invalid schedules with previews, unlisted resources, and route conflicts. Takes a few seconds. Read-only.",
            json!({}), &[]),
        write_tool("rerun_run", "Start a new run of the same flow with the original run's parameters and tags. With `from`, the new run is a retry linked to the original that replays its completed tasks from their checkpoints and executes from the failure, from the start, or from the named task and everything after it. Returns the new run; follow it with get_run.",
            json!({"run_id": {"type": "integer"}, "from": {"type": "string", "description": "failure, start, or a task's dynamic key such as transform-0; omit for a fresh run"}}), &["run_id"]),
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

/// An ISO 8601 instant (`2026-09-20T03:00:00Z`, an offset, or naive UTC) as microseconds.
fn parse_instant(text: &str) -> Result<i64, ToolError> {
    let t = text.trim();
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(t) {
        return Ok(dt.timestamp_micros());
    }
    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(t, "%Y-%m-%dT%H:%M:%S") {
        return Ok(dt.and_utc().timestamp_micros());
    }
    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(t, "%Y-%m-%dT%H:%M") {
        return Ok(dt.and_utc().timestamp_micros());
    }
    Err(ToolError::Failed(format!(
        "until: cannot parse {text:?} as ISO 8601"
    )))
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

/// The schedule arguments as `ScheduleBody` expects them on the wire. `Schedule`
/// is `#[serde(tag = "kind")]`, so the variant fields sit alongside `kind`; the
/// tool takes them flat and only the keys belonging to a schedule are forwarded.
fn schedule_body_json(args: &Map<String, Value>) -> Value {
    let mut body = Map::new();
    for key in [
        "kind",
        "cron",
        "interval",
        "anchor",
        "rrule",
        "timezone",
        "day_or",
        "catchup",
        "catchup_max",
        "catchup_window",
        "jitter",
        "start_deadline",
    ] {
        if let Some(v) = args.get(key) {
            body.insert(key.to_string(), v.clone());
        }
    }
    Value::Object(body)
}

/// The next few times a schedule fires, so an agent can check a spec means what
/// it intended before it fires unattended. An unparseable spec yields nothing
/// rather than failing the call: the schedule is already stored by this point.
fn preview_of(row: &ScheduleRow) -> Vec<i64> {
    crate::scheduler::preview(&row.schedule, 3).unwrap_or_default()
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
    user: Option<&crate::auth::AuthenticatedUser>,
    name: &str,
    args: &Map<String, Value>,
) -> Result<Value, ToolError> {
    if name.starts_with("flow__") {
        let Some((_, flow)) = flow_tools(state).into_iter().find(|(n, _)| n == name) else {
            return Err(ToolError::Unknown(name.to_string()));
        };
        let run = runs::create_run_inner(
            state,
            &flow,
            args.clone(),
            None,
            Vec::new(),
            &crate::auth::run_creator(user, &format!("mcp:{client}")),
            None,
            None,
        )
        .await?;
        return Ok(json!({"run": run, "note": "created; follow it with get_run"}));
    }
    match name {
        "list_backfills" => {
            let flow_id = match arg_str(args, "flow") {
                Some(n) => Some(resolve_flow(state, &n, arg_str(args, "project").as_deref())?.id),
                None => None,
            };
            let mut items = Vec::new();
            for b in state.store.list_backfills(flow_id)? {
                items.push(
                    serde_json::to_value(backfills::status_of(state, b.id)?).unwrap_or(Value::Null),
                );
            }
            Ok(json!({"backfills": items}))
        }
        "get_backfill" => {
            let id = arg_i64(args, "backfill_id")?;
            Ok(json!({"backfill": backfills::status_of(state, id)?}))
        }
        "cancel_backfill" => {
            let id = arg_i64(args, "backfill_id")?;
            let status = backfills::cancel_backfill_inner(state, id).await?;
            Ok(
                json!({"backfill": status, "note": "cancelled; queued runs are cancelled and running ones asked to stop"}),
            )
        }
        "get_flow_source" => {
            let name = arg_str(args, "flow")
                .ok_or_else(|| ToolError::Failed("flow is required".into()))?;
            let flow = resolve_flow(state, &name, arg_str(args, "project").as_deref())?;
            flow_source(&flow)
        }
        "server_health" => Ok(json!({
            "version": state.config.version,
            "engines": state.supervisor.engines_snapshot(),
            "queued": state.supervisor.queue_len(),
            "resources": state.supervisor.resources_snapshot(),
            "schedules": state.store.list_schedules(None)?.len(),
            "auth": state.config.token.is_some(),
            "exposed": state.exposed(),
            "paused": state.pause(),
            "read_only": state.config.mcp_read_only,
            "served_dir": state.config.served_dir.as_ref().map(|p| p.display().to_string()),
            "as_of": now_micros(),
        })),
        "list_variables" => {
            let items: Vec<Value> = state
                .store
                .list_variables()?
                .into_iter()
                .map(|v| {
                    let mut row = json!({
                        "name": v.name, "tags": v.tags, "secret": v.secret,
                        "created_at": v.created_at, "updated_at": v.updated_at,
                    });
                    if !v.secret {
                        row["value"] = v.value;
                    }
                    row
                })
                .collect();
            Ok(json!({"variables": items}))
        }
        "list_resources" => Ok(json!({"resources": state.supervisor.resources_snapshot()})),
        "flow_dependencies" => {
            let name = arg_str(args, "flow")
                .ok_or_else(|| ToolError::Failed("flow is required".into()))?;
            let flow = resolve_flow(state, &name, arg_str(args, "project").as_deref())?;
            let options = cereyan_core::FlowOptions::from_map(&flow.options);
            let triggers: Vec<String> = state
                .store
                .list_flows(None)?
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
            Ok(json!({
                "flow": flow.name, "project": flow.project,
                "upstreams": options.after.as_ref().map(|a| a.upstreams()).unwrap_or_default(),
                "batch_key": options.after.as_ref().and_then(|a| a.key.clone()),
                "triggers": triggers,
            }))
        }
        "check_flows" => {
            let Some(dir) = state.config.served_dir.clone() else {
                return Err(ToolError::Failed(
                    "this server was started from a script without a served directory, so there is no directory to check".into(),
                ));
            };
            let python = state.config.python.clone();
            let home = state.config.home.clone();
            tokio::task::spawn_blocking(move || run_check(&python, &dir, &home))
                .await
                .map_err(|e| ToolError::Failed(e.to_string()))?
        }
        "rerun_run" => {
            let id = arg_i64(args, "run_id")?;
            let original = state
                .store
                .get_run(id)?
                .ok_or_else(|| ToolError::Failed(format!("run {id} not found")))?;
            if let Some(from) = arg_str(args, "from").filter(|f| !f.trim().is_empty()) {
                let out = runs::retry_inner(
                    state,
                    &original,
                    from.trim(),
                    &crate::auth::run_creator(user, &format!("mcp:{client}")),
                )
                .await?;
                return Ok(json!({
                    "run": out.run, "rerun_of": id, "from": out.from, "replays": out.replays,
                    "invalidated": out.invalidated,
                    "note": "created as a retry of the original: kept tasks replay from their checkpoints; follow it with get_run"
                }));
            }
            let flow = state
                .store
                .get_flow(original.flow_id)?
                .ok_or_else(|| ToolError::Failed(format!("flow {} not found", original.flow_id)))?;
            let run = runs::create_run_inner(
                state,
                &flow,
                original.parameters.clone(),
                None,
                original.tags.clone(),
                &crate::auth::run_creator(user, &format!("mcp:{client}")),
                None,
                None,
            )
            .await?;
            Ok(
                json!({"run": run, "rerun_of": id, "note": "created with the original run's parameters; follow it with get_run"}),
            )
        }
        "list_flows" => {
            let project = arg_str(args, "project");
            let group = arg_str(args, "group");
            let flows = state
                .store
                .list_flows_filtered(project.as_deref(), group.as_deref())?;
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
                group: arg_str(args, "group"),
                state_type: arg_str(args, "state_type"),
                state_name: arg_str(args, "state_name"),
                name: arg_str(args, "name"),
                params: arg_str(args, "params")
                    .map(|s| {
                        s.split(',')
                            .map(|p| p.trim().to_string())
                            .filter(|p| !p.is_empty())
                            .collect()
                    })
                    .unwrap_or_default(),
                attributes: arg_str(args, "attributes")
                    .map(|s| {
                        s.split(',')
                            .map(|p| p.trim().to_string())
                            .filter(|p| !p.is_empty())
                            .collect()
                    })
                    .unwrap_or_default(),
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
            let tasks = state.store.task_runs_by_run(id, None)?;
            Ok(json!({"run": run, "task_runs": tasks}))
        }
        "compare_runs" => {
            let a = arg_i64(args, "run_id")?;
            let b = arg_i64(args, "other_run_id")?;
            if a == b {
                return Err(ToolError::Failed(
                    "run_id and other_run_id must differ".into(),
                ));
            }
            let st = state.clone();
            let comparison = tokio::task::spawn_blocking(move || {
                let left = runs::bundle(&st, a)?;
                let right = runs::bundle(&st, b)?;
                Ok::<_, ApiError>(crate::compare::compare(&left, &right))
            })
            .await
            .map_err(|e| ToolError::Failed(e.to_string()))??;
            Ok(serde_json::to_value(comparison).unwrap_or(Value::Null))
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
                group: None,
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
                .task_runs_by_run(id, None)?
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
            let at = match arg_str(args, "at").filter(|a| !a.trim().is_empty()) {
                Some(text) => Some(parse_instant(&text)?),
                None => None,
            };
            let delay = args.get("delay_seconds").and_then(|d| d.as_f64());
            let starts = runs::not_before(at, delay)?;
            let (run, conflict) = runs::create_run_checked(
                state,
                &flow,
                parameters,
                arg_str(args, "name"),
                tags,
                &crate::auth::run_creator(user, &format!("mcp:{client}")),
                starts,
                None,
                None,
            )
            .await?;
            let note = if conflict {
                "not created: this run already holds the flow's unique key; follow it with get_run"
            } else if starts.is_some_and(|t| t > now_micros()) {
                "created for later; it starts at scheduled_time"
            } else {
                "created; follow it with get_run"
            };
            Ok(json!({"run": run, "conflict": conflict, "note": note}))
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
        "list_schedules" => {
            let flow_id = match arg_str(args, "flow") {
                Some(n) => Some(resolve_flow(state, &n, arg_str(args, "project").as_deref())?.id),
                None => None,
            };
            let project = arg_str(args, "project");
            let rows = state.store.list_schedules(flow_id)?;
            let flows = state.store.list_flows(None)?;
            let schedules: Vec<Value> = rows
                .into_iter()
                .filter_map(|row| {
                    let flow = flows.iter().find(|f| f.id == row.flow_id)?;
                    if let Some(p) = &project {
                        if &flow.project != p {
                            return None;
                        }
                    }
                    let row = crate::api::schedules::decorate(state, row);
                    Some(json!({
                        "id": row.id, "flow": flow.name, "project": flow.project,
                        "schedule": row.schedule, "catchup": row.catchup, "catchup_max": row.catchup_max,
                        "active": row.active, "source": row.source, "next_fire": row.next_fire,
                        "paused_reason": row.paused_reason, "paused_until": row.paused_until,
                    }))
                })
                .collect();
            Ok(json!({"schedules": schedules}))
        }
        "create_schedule" => {
            let name = arg_str(args, "flow")
                .ok_or_else(|| ToolError::Failed("flow is required".into()))?;
            let flow = resolve_flow(state, &name, arg_str(args, "project").as_deref())?;
            let body: ScheduleBody = serde_json::from_value(schedule_body_json(args))
                .map_err(|e| ToolError::Failed(format!("schedule is not valid: {e}")))?;
            let row =
                crate::api::schedules::create_schedule_inner(state, flow.id, body, "mcp").await?;
            let mut out = json!({"schedule": row, "next_fires": preview_of(&row)});
            // Two schedules on one flow is legal and occasionally meant; more often
            // the agent has not noticed the flow already declares its own.
            let existing = state.store.list_schedules(Some(flow.id))?;
            if existing.iter().any(|s| s.source == "code") {
                out["note"] = json!(format!(
                    "{} already has a schedule declared in its code; it now has more than one.",
                    flow.name
                ));
            }
            Ok(out)
        }
        "edit_schedule" => {
            let sid = arg_i64(args, "schedule_id")?;
            let before = state
                .store
                .get_schedule(sid)?
                .ok_or_else(|| ToolError::Failed(format!("schedule {sid} not found")))?;
            let body: SchedulePatchBody = serde_json::from_value(Value::Object(args.clone()))
                .map_err(|e| ToolError::Failed(format!("patch is not valid: {e}")))?;
            let row = crate::api::schedules::patch_schedule_inner(state, sid, body).await?;
            let mut out = json!({"schedule": row, "next_fires": preview_of(&row)});
            if before.source == "code" {
                out["note"] = json!(
                    "This schedule is declared in the flow's code. The change lasts until the \
                     server restarts; then the flow's declaration applies again."
                );
            }
            Ok(out)
        }
        "delete_schedule" => {
            let sid = arg_i64(args, "schedule_id")?;
            let row = state
                .store
                .get_schedule(sid)?
                .ok_or_else(|| ToolError::Failed(format!("schedule {sid} not found")))?;
            if row.source == "code" {
                // Deleting would not last: register() recreates every declared
                // schedule it does not find, so a success here would be a lie.
                return Err(ToolError::Failed(format!(
                    "schedule {sid} is declared in the flow's code and would be recreated at the \
                     next restart; use pause_schedule to stop it, or remove the declaration from \
                     the flow"
                )));
            }
            let st = state.clone();
            let ok = tokio::task::spawn_blocking(move || crate::scheduler::delete(&st, sid))
                .await
                .map_err(|e| ToolError::Failed(e.to_string()))?;
            Ok(json!({"deleted": ok, "schedule_id": sid}))
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
        "pause_scheduler" => {
            let reason = arg_str(args, "reason").filter(|r| !r.trim().is_empty());
            let until = match arg_str(args, "until").filter(|u| !u.trim().is_empty()) {
                Some(text) => Some(parse_instant(&text)?),
                None => None,
            };
            let suppress = args
                .get("suppress_rules")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let st = state.clone();
            let pause = tokio::task::spawn_blocking(move || {
                crate::scheduler::pause_all(&st, reason, until, suppress)
            })
            .await
            .map_err(|e| ToolError::Failed(e.to_string()))?;
            Ok(
                json!({"paused": true, "since": pause.since, "reason": pause.reason, "until": pause.until, "suppress_rules": pause.suppress_rules}),
            )
        }
        "resume_scheduler" => {
            let st = state.clone();
            let resumed = tokio::task::spawn_blocking(move || crate::scheduler::resume_all(&st))
                .await
                .map_err(|e| ToolError::Failed(e.to_string()))?;
            Ok(match resumed {
                Some((held, schedules)) => {
                    json!({"paused": false, "held": held, "schedules": schedules})
                }
                None => json!({"paused": false, "already": "the scheduler was not paused"}),
            })
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

const SOURCE_CAP: usize = 64 * 1024;
const CHECK_TIMEOUT: Duration = Duration::from_secs(60);
const CHECK_OUTPUT_CAP: u64 = 1024 * 1024;

/// The module file that registered `flow`, read only from under the flow's
/// own source directory and cut at `SOURCE_CAP`. Nothing here comes from the
/// request: the directory and module were recorded at registration.
fn flow_source(flow: &cereyan_core::Flow) -> Result<Value, ToolError> {
    let base = std::fs::canonicalize(&flow.source_dir)
        .map_err(|e| ToolError::Failed(format!("source directory {}: {e}", flow.source_dir)))?;
    let mut path = base.clone();
    for part in flow.module.split('.') {
        path.push(part);
    }
    path.set_extension("py");
    let path = std::fs::canonicalize(&path)
        .map_err(|e| ToolError::Failed(format!("{}: {e}", path.display())))?;
    if !path.starts_with(&base) {
        return Err(ToolError::Failed(
            "the flow's module is outside its source directory".into(),
        ));
    }
    let bytes =
        std::fs::read(&path).map_err(|e| ToolError::Failed(format!("{}: {e}", path.display())))?;
    let truncated = bytes.len() > SOURCE_CAP;
    let text = String::from_utf8_lossy(&bytes[..bytes.len().min(SOURCE_CAP)]).to_string();
    Ok(json!({
        "flow": flow.name, "project": flow.project, "module": flow.module,
        "path": path.display().to_string(), "bytes": bytes.len(), "truncated": truncated,
        "source": text,
    }))
}

/// `python -m cereyan check --json <dir>` in a child process, bounded in time
/// and output. Exit 1 still carries a report (findings); exit 3 is a failure
/// to check at all and its stderr becomes the error.
fn run_check(
    python: &str,
    dir: &std::path::Path,
    home: &std::path::Path,
) -> Result<Value, ToolError> {
    use std::io::Read;
    use std::process::{Command, Stdio};

    let mut child = Command::new(python)
        .args(["-m", "cereyan", "check", "--json"])
        .arg(dir)
        .env("CEREYAN_HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| ToolError::Failed(format!("could not start {python}: {e}")))?;
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let out_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = (&mut stdout).take(CHECK_OUTPUT_CAP).read_to_end(&mut buf);
        let mut sink = [0u8; 8192];
        while matches!(stdout.read(&mut sink), Ok(n) if n > 0) {}
        buf
    });
    let err_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = (&mut stderr).take(64 * 1024).read_to_end(&mut buf);
        let mut sink = [0u8; 8192];
        while matches!(stderr.read(&mut sink), Ok(n) if n > 0) {}
        buf
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() > CHECK_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ToolError::Failed(format!(
                    "cereyan check did not finish within {} s",
                    CHECK_TIMEOUT.as_secs()
                )));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(ToolError::Failed(e.to_string())),
        }
    };
    let out = out_reader.join().unwrap_or_default();
    let err = String::from_utf8_lossy(&err_reader.join().unwrap_or_default()).to_string();
    match serde_json::from_slice::<Value>(&out) {
        Ok(report) => Ok(report),
        Err(_) => Err(ToolError::Failed(format!(
            "cereyan check exited with {} and produced no report: {}",
            status.code().unwrap_or(-1),
            err.trim()
        ))),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_json_bodies_are_read() {
        let with = |value: &str| {
            let mut headers = HeaderMap::new();
            headers.insert(header::CONTENT_TYPE, value.parse().unwrap());
            is_json(&headers)
        };
        assert!(with("application/json"));
        assert!(with("Application/JSON; charset=utf-8"));
        assert!(!with("text/plain"));
        assert!(!with("text/plain; charset=application/json"));
        assert!(!with("application/json-patch+json"));
        assert!(!is_json(&HeaderMap::new()));
    }
}
