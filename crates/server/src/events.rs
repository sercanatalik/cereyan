//! Engine-emitted events: one per accepted run or task-run state, plus
//! schedule, flow, resource, and rule events emitted where they happen.

use cereyan_core::{EventName, Run, StateType, TaskRun};
use serde_json::json;

use crate::state::AppState;

/// The event for a run state, or None when the state emits nothing.
pub fn run_event_name(run: &Run) -> Option<EventName> {
    let name = run.state.name.as_str();
    Some(match (run.state.state_type, name) {
        (_, "Late") => EventName::RunLate,
        (_, "Retrying") => EventName::RunRetrying,
        (_, "Skipped") => EventName::RunSkipped,
        (StateType::Scheduled, "Resuming") => EventName::RunResumed,
        (StateType::Paused, _) => EventName::RunPaused,
        (_, "AwaitingRetry") | (_, "AwaitingResource") => return None,
        (StateType::Scheduled, _) => EventName::RunScheduled,
        (StateType::Pending, _) => EventName::RunPending,
        (StateType::Running, _) => EventName::RunRunning,
        (StateType::Completed, _) => EventName::RunCompleted,
        (StateType::Failed, _) => EventName::RunFailed,
        (StateType::Crashed, _) => EventName::RunCrashed,
        (StateType::Cancelled, _) => EventName::RunCancelled,
        (StateType::Cancelling, _) => return None,
    })
}

pub fn emit_run_event(state: &AppState, run: &Run) {
    let Some(name) = run_event_name(run) else {
        return;
    };
    let mut payload = json!({
        "state": run.state.name,
        "state_type": run.state.state_type,
        "message": run.state.message,
        "flow": run.flow_name,
        "project": run.project,
        "parameters": run.parameters,
        "created_by": run.created_by,
    });
    // A skip by a person, or one carried down from an upstream, says so.
    if name == EventName::RunSkipped {
        if let Some(reason) = run.state.details.get("reason") {
            payload["reason"] = reason.clone();
        }
    }
    if matches!(name, EventName::RunFailed | EventName::RunCrashed) {
        payload["failures_in_a_row"] = json!(1 + failure_streak_before(state, run));
    }
    let _ = state.record_engine_event(name, Some(run.id), Some(run.flow_id), payload);
    if name == EventName::RunCompleted {
        let ended = failure_streak_before(state, run);
        if ended > 0 {
            let _ = state.record_engine_event(
                EventName::FlowRecovered,
                Some(run.id),
                Some(run.flow_id),
                json!({"flow": run.flow_name, "project": run.project, "failures": ended, "run_id": run.id}),
            );
        }
    }
}

/// Consecutive Failed or Crashed runs of the flow that ended before `run`,
/// newest first, stopping at the first other terminal run. Non-terminal runs
/// and runs newer than `run` are skipped; the count is capped by the window.
fn failure_streak_before(state: &AppState, run: &Run) -> i64 {
    let Ok(recent) = state.store.recent_run_states(run.flow_id, 50) else {
        return 0;
    };
    let mut streak = 0;
    for (id, state_type, _, _) in recent {
        if id >= run.id {
            continue;
        }
        match state_type.as_str() {
            "Failed" | "Crashed" => streak += 1,
            "Completed" | "Cancelled" => break,
            _ => continue,
        }
    }
    streak
}

pub fn task_run_event_name(t: &TaskRun) -> Option<EventName> {
    Some(match (t.state.state_type, t.state.name.as_str()) {
        (_, "Skipped") => EventName::TaskRunSkipped,
        (_, "Cached") => EventName::TaskRunCached,
        (_, "Replayed") => EventName::TaskRunReplayed,
        (StateType::Running, _) => EventName::TaskRunRunning,
        (StateType::Completed, _) => EventName::TaskRunCompleted,
        (StateType::Failed, _) => EventName::TaskRunFailed,
        (StateType::Cancelled, _) => EventName::TaskRunCancelled,
        _ => return None,
    })
}

/// The stored event for a task-run transition, if that transition has one.
pub fn task_run_event(t: &TaskRun) -> Option<cereyan_store::NewEvent> {
    let name = task_run_event_name(t)?;
    Some(cereyan_store::NewEvent {
        name: name.as_str().into(),
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
            let _ = state.record_engine_event(
                EventName::FlowRegistered,
                None,
                Some(f.id),
                json!({"flow": f.name, "project": f.project, "module": f.module}),
            );
        }
    }
}
