//! Durable waits: a run that sleeps, waits for an event, or waits for a
//! target leaves its engine as Paused with a named reason, and the server
//! wakes it by storing an answer at the waiting input index and scheduling
//! a new attempt, exactly as an answered `wait_for_input` does.

use std::sync::{Arc, Mutex, MutexGuard};

use cereyan_core::{now_micros, Event, Run, State, StateType};
use serde_json::{json, Map, Value};

use crate::api::error::{ApiError, ApiResult};
use crate::state::{AppState, TransitionResult};
use crate::timer::TimerEvent;

/// Serializes everything that answers a paused run or queues a message for
/// one. Each of those reads the run, decides, then writes the answer and
/// transitions; without one lock across the three steps two answerers both
/// see Paused, the second overwrites the first's answer, and the first is told
/// it succeeded while the run resumes with the other's.
static ANSWER_LOCK: Mutex<()> = Mutex::new(());

fn answer_lock() -> MutexGuard<'static, ()> {
    ANSWER_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// What answering a paused run came to.
pub enum Woke {
    /// The run is no longer in the pause the caller saw; nothing was written.
    Moved(Run),
    /// The answer was stored and the transition attempted. An accepted run is
    /// already queued.
    Done(TransitionResult),
}

/// Answer the pause the caller observed in `observed`, if the run is still in
/// it. `input` sees the fresh row and may decline by returning `None`.
pub fn wake_paused(
    state: &Arc<AppState>,
    observed: &Run,
    input: impl FnOnce(&Run) -> Option<Value>,
) -> ApiResult<Woke> {
    let _guard = answer_lock();
    let run = state
        .store
        .get_run(observed.id)?
        .ok_or_else(|| ApiError::NotFound("run not found".into()))?;
    if run.state.state_type != StateType::Paused
        || run.state.timestamp != observed.state.timestamp
    {
        return Ok(Woke::Moved(run));
    }
    let Some(input) = input(&run) else {
        return Ok(Woke::Moved(run));
    };
    wake_locked(state, run, input)
}

/// What sending a message came to.
pub enum Delivered {
    /// The run was waiting on this topic and was answered.
    Woke(Woke),
    /// The run was not waiting on it: the message is queued for its `receive`.
    Queued(Run),
}

/// Send `payload` on `topic`: answer the run if it waits on that topic, queue
/// the message otherwise. Both happen under the answer lock, so a message is
/// never queued just after the run paused on its topic and checked the queue.
pub fn deliver_message(
    state: &Arc<AppState>,
    run_id: i64,
    topic: &str,
    payload: Value,
) -> ApiResult<Delivered> {
    let _guard = answer_lock();
    let run = state
        .store
        .get_run(run_id)?
        .ok_or_else(|| ApiError::NotFound("run not found".into()))?;
    let waiting = run.state.details.get("topic").and_then(|v| v.as_str());
    if run.state.state_type == StateType::Paused && waiting == Some(topic) {
        return Ok(Delivered::Woke(wake_locked(state, run, payload)?));
    }
    if run.state.state_type.is_terminal() {
        return Err(ApiError::Conflict(json!({"error": "run is terminal"})));
    }
    state
        .store
        .run_message_insert(run_id, topic, &payload.to_string())?;
    Ok(Delivered::Queued(run))
}

/// Store `input` as the answer to the question `run` is paused on and schedule
/// its next attempt. The caller holds the answer lock and read `run` under it.
fn wake_locked(state: &Arc<AppState>, run: Run, input: Value) -> ApiResult<Woke> {
    let flow = state
        .store
        .get_flow(run.flow_id)?
        .ok_or_else(|| ApiError::NotFound("flow not found".into()))?;
    let details = &run.state.details;
    let index = details.get("index").and_then(|v| v.as_i64()).unwrap_or(0);
    let prompt = details.get("prompt").cloned().unwrap_or(Value::Null);
    // Without the topic the claim reads the answer as `"input"`, and a run
    // waiting in `receive(topic)` would replay and pause again.
    let topic = details.get("topic").cloned().unwrap_or(Value::Null);
    let mut answers = crate::api::runs::stored_answers(state, run.id)?;
    answers.insert(index.to_string(), json!({"topic": topic, "prompt": prompt, "input": input}));
    state.store.kv_set(
        &crate::state::run_input_key(run.id),
        &crate::api::runs::answers_value(&answers),
    )?;
    let mut next = State::new(StateType::Scheduled);
    next.name = "Resuming".into();
    let result = state.transition_run(run.id, next, false)?;
    if let TransitionResult::Accepted(run) = &result {
        crate::dispatch::enqueue_run(state, run, &flow, None);
    }
    Ok(Woke::Done(result))
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

/// A run just paused in `receive(topic)`: a message sent to that topic while it
/// was still Running sits in the queue, and nothing else would deliver it. Claim
/// it now and wake the run with it.
pub fn deliver_queued(state: &Arc<AppState>, run: &Run) {
    if run.state.state_type != StateType::Paused || run.state.details.get("topic").is_none() {
        return;
    }
    // The Paused transition that got us here has not returned to its caller
    // yet; waking from inside it would hand that caller a run that has already
    // moved on. The claim happens under the answer lock, after the run is
    // re-read, so a message is consumed only by the pause it answers.
    let state = state.clone();
    let observed = run.clone();
    std::thread::spawn(move || {
        let result = wake_paused(&state, &observed, |run| {
            let topic = run.state.details.get("topic")?.as_str()?;
            let index = run.state.details.get("index").and_then(|v| v.as_i64()).unwrap_or(0);
            let text = state.store.run_message_claim(run.id, topic, index).ok()??;
            let answer: Value = serde_json::from_str(&text).unwrap_or(Value::String(text));
            Some(answer.get("input").cloned().unwrap_or(Value::Null))
        });
        if let Err(e) = result {
            eprintln!(
                "cereyan: could not deliver a queued message to run {}: {e:?}",
                observed.id
            );
        }
    });
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
    let _ = wake_paused(state, &run, |run| Some(timeout_answer(run)));
}

/// The answer a wake timer hands a paused run, by what it waits for.
fn timeout_answer(run: &Run) -> Value {
    if let Some(default) = run.state.details.get("default") {
        // Message timeout: resume with the default value stored by the engine.
        default.clone()
    } else {
        match run.state.name.as_str() {
            "AwaitingEvent" => json!({"timeout": true}),
            // The poke carries the wait's start so the replay can honour the timeout.
            "AwaitingTarget" => json!({
                "poke": true,
                "at": now_micros(),
                "started_at": run.state.details.get("started_at").cloned().unwrap_or(Value::Null),
            }),
            _ => json!({"woke": true, "at": now_micros()}),
        }
    }
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
    // Fast path: one relaxed load, no lock, no clone of the active set. The flag
    // is maintained under the index lock alongside the waiter set.
    if !state.index.has_event_waiters() {
        return;
    }
    let waiting = state.index.awaiting_event_ids();
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
        let answer = json!({"event": serde_json::to_value(event).unwrap_or(Value::Null)});
        let _ = wake_paused(state, &run, |_| Some(answer));
    }
}

#[cfg(test)]
mod answer_tests {
    use super::*;
    use tempfile::TempDir;

    /// A real state over a real store, with no engines to start: answering a
    /// run queues it, and the queue must not spawn a Python process.
    fn state_with_run(dir: &TempDir) -> (Arc<AppState>, i64) {
        let home = dir.path().join("home");
        let store = Arc::new(cereyan_store::Store::open(&home).unwrap());
        let flow_id = store
            .upsert_flow_full(cereyan_store::UpsertFlow {
                project: "p".into(),
                name: "f".into(),
                module: "m".into(),
                source_dir: "/nonexistent".into(),
                tags: "[]".into(),
                parameter_schema: "{}".into(),
                options: "{}".into(),
                ..Default::default()
            })
            .unwrap();
        let (run_id, _) = store
            .create_run_full(cereyan_store::CreateRun {
                flow_id,
                name: "r".into(),
                parameters: "{}".into(),
                tags: "[]".into(),
                created_by: "test".into(),
                ..Default::default()
            })
            .unwrap();
        let config: crate::ServeConfig = serde_json::from_value(json!({
            "home": home.to_string_lossy(), "max_engines": 0,
        }))
        .unwrap();
        let (_tx, rx) = tokio::sync::watch::channel(false);
        let state = Arc::new(
            AppState::new(config, store, None, None, "127.0.0.1:0".parse().unwrap(), rx).unwrap(),
        );
        for next in [StateType::Pending, StateType::Running] {
            state.transition_run(run_id, State::new(next), false).unwrap();
        }
        (state, run_id)
    }

    fn pause(state: &Arc<AppState>, run_id: i64, topic: &str) -> Run {
        let mut paused = State::new(StateType::Paused);
        paused.details = json!({"topic": topic, "index": 0}).as_object().unwrap().clone();
        match state.transition_run(run_id, paused, false).unwrap() {
            TransitionResult::Accepted(run) => *run,
            TransitionResult::Rejected { reason, .. } => panic!("pause rejected: {reason}"),
        }
    }

    fn stored_input(state: &Arc<AppState>, run_id: i64) -> Value {
        crate::api::runs::stored_answers(state, run_id).unwrap()["0"]["input"].clone()
    }

    /// Two answerers saw the same pause: the first wins, and the second is told
    /// the run moved on rather than overwriting the answer.
    #[test]
    fn a_second_answer_to_the_same_pause_is_refused() {
        let dir = TempDir::new().unwrap();
        let (state, run_id) = state_with_run(&dir);
        let seen = pause(&state, run_id, "signal");
        let first = wake_paused(&state, &seen, |_| Some(json!("A"))).unwrap();
        assert!(matches!(first, Woke::Done(TransitionResult::Accepted(_))));
        let second = wake_paused(&state, &seen, |_| Some(json!("B"))).unwrap();
        assert!(matches!(second, Woke::Moved(_)), "the second answer must not land");
        assert_eq!(stored_input(&state, run_id), json!("A"));
    }

    /// A message for a run that is not waiting on its topic is queued, and one
    /// for the topic it waits on answers it.
    #[test]
    fn a_message_answers_its_topic_and_queues_otherwise() {
        let dir = TempDir::new().unwrap();
        let (state, run_id) = state_with_run(&dir);
        let queued = deliver_message(&state, run_id, "signal", json!(1)).unwrap();
        assert!(matches!(queued, Delivered::Queued(_)), "a Running run gets it queued");
        pause(&state, run_id, "other");
        let queued = deliver_message(&state, run_id, "signal", json!(2)).unwrap();
        assert!(matches!(queued, Delivered::Queued(_)), "a run on another topic gets it queued");
        assert_eq!(state.store.run_message_list(run_id).unwrap().len(), 2);
        let woke = deliver_message(&state, run_id, "other", json!(3)).unwrap();
        assert!(matches!(woke, Delivered::Woke(Woke::Done(TransitionResult::Accepted(_)))));
        assert_eq!(stored_input(&state, run_id), json!(3));
    }
}
