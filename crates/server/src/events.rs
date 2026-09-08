//! Engine-emitted events: one per accepted run or task-run state, plus
//! schedule, flow, resource, and rule events emitted where they happen.

use cereyan_core::{Run, StateType, TaskRun};
use serde_json::json;

use crate::state::AppState;

/// The event name for a run state, or None when the state emits nothing.
pub fn run_event_name(run: &Run) -> Option<&'static str> {
    let name = run.state.name.as_str();
    Some(match (run.state.state_type, name) {
        (_, "Late") => "run.late",
        (_, "Retrying") => "run.retrying",
        (_, "Skipped") => "run.skipped",
        (StateType::Scheduled, "Resuming") => "run.resumed",
        (StateType::Paused, _) => "run.paused",
        (_, "AwaitingRetry") | (_, "AwaitingResource") => return None,
        (StateType::Scheduled, _) => "run.scheduled",
        (StateType::Pending, _) => "run.pending",
        (StateType::Running, _) => "run.running",
        (StateType::Completed, _) => "run.completed",
        (StateType::Failed, _) => "run.failed",
        (StateType::Crashed, _) => "run.crashed",
        (StateType::Cancelled, _) => "run.cancelled",
        (StateType::Cancelling, _) => return None,
    })
}

pub fn emit_run_event(state: &AppState, run: &Run) {
    let Some(name) = run_event_name(run) else {
        return;
    };
    let _ = state.record_event(
        name,
        Some(run.id),
        Some(run.flow_id),
        json!({
            "state": run.state.name,
            "state_type": run.state.state_type,
            "message": run.state.message,
            "flow": run.flow_name,
            "project": run.project,
            "parameters": run.parameters,
            "created_by": run.created_by,
        }),
    );
}

pub fn task_run_event_name(t: &TaskRun) -> Option<&'static str> {
    Some(match (t.state.state_type, t.state.name.as_str()) {
        (_, "Skipped") => "task_run.skipped",
        (_, "Cached") => "task_run.cached",
        (StateType::Running, _) => "task_run.running",
        (StateType::Completed, _) => "task_run.completed",
        (StateType::Failed, _) => "task_run.failed",
        (StateType::Cancelled, _) => "task_run.cancelled",
        _ => return None,
    })
}

/// The stored event for a task-run transition, if that transition has one.
pub fn task_run_event(t: &TaskRun) -> Option<cereyan_store::NewEvent> {
    let name = task_run_event_name(t)?;
    Some(cereyan_store::NewEvent {
        name: name.into(),
        run_id: Some(t.run_id),
        flow_id: Some(t.flow_id),
        payload: json!({
            "task": t.name, "dynamic_key": t.dynamic_key, "state": t.state.name,
            "message": t.state.message, "flow": t.flow_name, "project": t.project,
        }),
        resource: cereyan_core::Resource {
            kind: "task_run".into(),
            id: t.external_id.to_string(),
            name: t.dynamic_key.clone(),
        },
        related: vec![
            cereyan_core::Resource {
                kind: "run".into(),
                id: t.run_id.to_string(),
                name: t.run_name.clone(),
            },
            cereyan_core::Resource {
                kind: "flow".into(),
                id: format!("{}/{}", t.project, t.flow_name),
                name: t.flow_name.clone(),
            },
        ],
    })
}

/// `flow.registered` for every flow the server registered from code.
pub fn emit_registered_flows(state: &AppState) {
    if let Ok(flows) = state.store.list_flows(None) {
        for f in flows.iter().filter(|f| state.is_live(f.id)) {
            let _ = state.record_event(
                "flow.registered",
                None,
                Some(f.id),
                json!({"flow": f.name, "project": f.project, "module": f.module}),
            );
        }
    }
}
