//! Durable waits: a run that sleeps, waits for an event, or waits for a
//! target leaves its engine as Paused with a named reason, and the server
//! wakes it by storing an answer at the waiting input index and scheduling
//! a new attempt, exactly as an answered `wait_for_input` does.

use std::sync::Arc;

use cereyan_core::{now_micros, Event, Run, State, StateType};
use serde_json::{json, Map, Value};

use crate::state::{AppState, TransitionResult};
use crate::timer::TimerEvent;

/// Wake a paused run with `input` as the answer to the wait it is in.
pub fn wake_run(state: &Arc<AppState>, run_id: i64, input: Value) -> Result<Run, String> {
    let run = state
        .store
        .get_run(run_id)
        .map_err(|e| e.to_string())?
        .ok_or("run not found")?;
    if run.state.state_type != StateType::Paused {
        return Err(format!(
            "run is {}, not Paused",
            run.state.state_type.as_str()
        ));
    }
    let flow = state
        .store
        .get_flow(run.flow_id)
        .map_err(|e| e.to_string())?
        .ok_or("flow not found")?;
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
    let mut answers =
        crate::api::runs::stored_answers(state, run_id).map_err(|e| format!("{e:?}"))?;
    answers.insert(index.to_string(), json!({"prompt": prompt, "input": input}));
    state
        .store
        .kv_set(
            &crate::state::run_input_key(run_id),
            &crate::api::runs::answers_value(&answers),
        )
        .map_err(|e| e.to_string())?;
    let mut next = State::new(StateType::Scheduled);
    next.name = "Resuming".into();
    match state
        .transition_run(run_id, next, false)
        .map_err(|e| e.to_string())?
    {
        TransitionResult::Accepted(run) => {
            let run = *run;
            crate::dispatch::enqueue_run(state, &run, &flow, None);
            Ok(run)
        }
        TransitionResult::Rejected { reason, .. } => Err(format!("cannot wake: {reason}")),
    }
}

/// Arm the wake timer a Paused run asked for (`details.wake_at`).
pub fn arm(state: &Arc<AppState>, run: &Run) {
    if run.state.state_type != StateType::Paused {
        return;
    }
    if let Some(at) = run.state.details.get("wake_at").and_then(|v| v.as_i64()) {
        state
            .timer
            .push(at.max(now_micros()), TimerEvent::WakeRun(run.id));
    }
}

/// After a restart: every paused run with a wake time gets its timer back.
pub fn rearm_all(state: &Arc<AppState>) {
    for active in state.index.active_runs() {
        if active.state.state_type != StateType::Paused {
            continue;
        }
        if let Ok(Some(run)) = state.store.get_run(active.id) {
            arm(state, &run);
        }
    }
}

/// The wake timer fired: what it means depends on what the run waits for.
pub fn on_timer(state: &Arc<AppState>, run_id: i64) {
    let Ok(Some(run)) = state.store.get_run(run_id) else {
        return;
    };
    if run.state.state_type != StateType::Paused {
        return;
    }
    // The timer that fired must be the one this pause asked for.
    let due = run.state.details.get("wake_at").and_then(|v| v.as_i64());
    if due.is_some_and(|at| at > now_micros() + 1_000) {
        return;
    }
    let answer = match run.state.name.as_str() {
        "AwaitingEvent" => json!({"timeout": true}),
        // The poke carries the wait's start so the replay can honour the timeout.
        "AwaitingTarget" => json!({
            "poke": true,
            "at": now_micros(),
            "started_at": run.state.details.get("started_at").cloned().unwrap_or(Value::Null),
        }),
        _ => json!({"woke": true, "at": now_micros()}),
    };
    let _ = wake_run(state, run_id, answer);
}

fn payload_matches(wanted: &Map<String, Value>, payload: &Map<String, Value>) -> bool {
    wanted.iter().all(|(k, want)| match payload.get(k) {
        Some(have) => {
            have == want
                || matches!(want, Value::String(s) if !have.is_string() && *have.to_string() == *s)
        }
        None => false,
    })
}

/// An event arrived: wake every run waiting for one like it.
pub fn on_event(state: &Arc<AppState>, event: &Event) {
    let waiting: Vec<i64> = state
        .index
        .active_runs()
        .into_iter()
        .filter(|r| r.state.state_type == StateType::Paused && r.state.name == "AwaitingEvent")
        .map(|r| r.id)
        .collect();
    if waiting.is_empty() {
        return;
    }
    let payload = event.payload.clone();
    for run_id in waiting {
        let Ok(Some(run)) = state.store.get_run(run_id) else {
            continue;
        };
        let Some(pattern) = run.state.details.get("event").and_then(|v| v.as_str()) else {
            continue;
        };
        if !cereyan_rules::name_matches(pattern, &event.name) {
            continue;
        }
        let wanted = run
            .state
            .details
            .get("match")
            .and_then(|m| m.as_object())
            .cloned()
            .unwrap_or_default();
        if !payload_matches(&wanted, &payload) {
            continue;
        }
        let _ = wake_run(
            state,
            run_id,
            json!({"event": serde_json::to_value(event).unwrap_or(Value::Null)}),
        );
    }
}
