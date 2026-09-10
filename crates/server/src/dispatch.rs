//! Run admission and post-transition logic: overlap policies, resource
//! needs, priority inheritance, crash chains, flow timeouts, disable windows,
//! and downstream flow dependencies.

use std::sync::Arc;

use cereyan_core::{now_micros, EventName, Flow, FlowOptions, Run, State, StateName, StateType};
use cereyan_store::CreateRun;
use serde_json::{json, Map, Value};

use crate::state::{AppState, TransitionResult};
use crate::supervisor::{EngineKey, QueuedRun};
use crate::timer::TimerEvent;

pub fn flow_cap_resource(flow: &Flow) -> String {
    format!("flow:{}/{}", flow.project, flow.name)
}

pub fn backfill_resource(backfill_id: i64) -> String {
    format!("backfill:{backfill_id}")
}

/// Resources a run needs before it may be dispatched.
pub fn run_needs(flow: &Flow, options: &FlowOptions, run: &Run) -> Vec<(String, f64)> {
    let mut needs = options.resource_amounts();
    if let Some(cap) = options.max_concurrent {
        if cap > 0 {
            needs.push((flow_cap_resource(flow), 1.0));
        }
    }
    if let Some(b) = run.backfill_id {
        needs.push((backfill_resource(b), 1.0));
    }
    needs
}

/// Effective priority: the run's own or the highest of flows that depend on it.
pub fn effective_priority(state: &AppState, flow: &Flow, run: &Run) -> i64 {
    let mut priority = run.priority;
    if let Ok(flows) = state.store.list_flows(Some(&flow.project)) {
        for other in flows {
            let opts = FlowOptions::from_map(&other.options);
            if opts
                .after
                .as_ref()
                .map(|a| a.depends_on(&flow.name))
                .unwrap_or(false)
            {
                priority = priority.max(opts.priority);
            }
        }
    }
    priority
}

/// Admit a run to the dispatch queue, applying the overlap policy first.
pub fn enqueue_run(state: &Arc<AppState>, run: &Run, flow: &Flow, not_before: Option<i64>) {
    if run.state.is_terminal() {
        return;
    }
    let options = FlowOptions::from_map(&flow.options);
    if let Some(cap) = options.max_concurrent {
        if cap > 0 {
            let name = flow_cap_resource(flow);
            state.supervisor.set_total(&name, cap as f64);
            if !state.supervisor.available(&name, 1.0) {
                match options.on_overlap.as_str() {
                    "skip" => {
                        let mut s = State::named(StateName::Skipped);
                        s.message = Some("previous run still active".into());
                        let _ = state.transition_run(run.id, s, false);
                        let _ = state.record_engine_event(
                            EventName::RunSkipped,
                            Some(run.id),
                            Some(flow.id),
                            json!({"reason": "previous run still active"}),
                        );
                        return;
                    }
                    "cancel_new" => {
                        let s = State::new(StateType::Cancelled)
                            .with_message("previous run still active");
                        let _ = state.transition_run(run.id, s, false);
                        return;
                    }
                    _ => {}
                }
            }
        }
    }
    if let Some(b) = run.backfill_id {
        if let Ok(Some(backfill)) = state.store.get_backfill(b) {
            state
                .supervisor
                .set_total(&backfill_resource(b), backfill.concurrency.max(1) as f64);
        }
    }
    let priority = effective_priority(state, flow, run);
    if priority != run.priority {
        let _ = state.store.set_run_priority(run.id, priority);
    }
    let mut key = EngineKey::from_flow(flow);
    key.nice = crate::supervisor::nice_for(priority);
    state.supervisor.enqueue(QueuedRun {
        run_id: run.id,
        key,
        priority,
        order: run.scheduled_time.unwrap_or(run.created_at),
        needs: run_needs(flow, &options, run),
        not_before,
    });
    state.supervisor.ensure_capacity(state);
}

/// Called after every accepted transition with the run's new state.
pub fn after_transition(state: &Arc<AppState>, run: &Run, previous: Option<&State>) {
    let Ok(Some(flow)) = state.store.get_flow(run.flow_id) else {
        return;
    };
    let options = FlowOptions::from_map(&flow.options);
    match run.state.state_type {
        StateType::Running => {
            if previous
                .map(|p| p.state_type != StateType::Running)
                .unwrap_or(true)
            {
                if let Some(t) = options.timeout_seconds {
                    if t > 0.0 {
                        state.timer.push(
                            now_micros() + (t * 1e6) as i64,
                            TimerEvent::FlowTimeout(run.id),
                        );
                    }
                }
            }
        }
        StateType::Failed => {
            disable_window(state, &flow, &options, run);
        }
        StateType::Completed => {
            trigger_dependents(state, &flow, run);
        }
        _ => {}
    }
}

/// A supervisor-detected crash: continue the chain or end it.
pub fn crash_run(state: &Arc<AppState>, run_id: i64, message: &str) {
    let Ok(Some(run)) = state.store.get_run(run_id) else {
        return;
    };
    let Ok(Some(flow)) = state.store.get_flow(run.flow_id) else {
        return;
    };
    let options = FlowOptions::from_map(&flow.options);
    let limit = options.crash_retries.unwrap_or(
        state
            .crash_retries_default
            .load(std::sync::atomic::Ordering::Relaxed),
    );
    if run.attempt < limit {
        let s = State::new(StateType::Crashed).with_message(message);
        if let Ok(TransitionResult::Accepted(_)) = state.transition_run(run_id, s, false) {
            let jitter_ms = 5_000 + (now_micros().unsigned_abs() % 10_000) as i64;
            let delay = if state.config.fast_crash_rerun {
                200
            } else {
                jitter_ms
            };
            state
                .timer
                .push(now_micros() + delay * 1_000, TimerEvent::CrashRerun(run_id));
            if options.has_crash_hooks {
                state.supervisor.enqueue_job(
                    EngineKey::from_flow(&flow),
                    json!({"kind": "hooks", "run_id": run_id, "state": "Crashed"}),
                );
                state.supervisor.ensure_capacity(state);
            }
        }
    } else {
        let mut s = State::new(StateType::Failed).with_message("crash limit reached");
        s.details
            .insert("crash".into(), Value::String(message.into()));
        let _ = state.transition_run(run_id, s, false);
    }
}

/// Create the next run of a crash chain.
pub fn crash_rerun(state: &Arc<AppState>, crashed_id: i64) {
    let Ok(Some(crashed)) = state.store.get_run(crashed_id) else {
        return;
    };
    let Ok(Some(flow)) = state.store.get_flow(crashed.flow_id) else {
        return;
    };
    let name = format!(
        "{}-retry-{}",
        crashed
            .name
            .trim_end_matches(|c: char| c.is_ascii_digit() || c == '-')
            .trim_end_matches("-retry"),
        crashed.attempt + 1
    );
    let created = state.store.create_run_full(CreateRun {
        flow_id: flow.id,
        name,
        parameters: serde_json::to_string(&crashed.parameters).unwrap_or_else(|_| "{}".into()),
        tags: serde_json::to_string(&crashed.tags).unwrap_or_else(|_| "[]".into()),
        created_by: format!("crash:{crashed_id}"),
        initial_state: Some(State::new(StateType::Scheduled)),
        schedule_id: crashed.schedule_id,
        scheduled_time: None,
        priority: crashed.priority,
        parent_run_id: Some(crashed_id),
        attempt: crashed.attempt + 1,
        backfill_id: crashed.backfill_id,
    });
    let Ok((run_id, _)) = created else { return };
    let Ok(Some(run)) = state.store.get_run(run_id) else {
        return;
    };
    state
        .index
        .insert_run(&run, EngineKey::from_flow(&flow), false);
    state.run_created(&run);
    enqueue_run(state, &run, &flow, None);
}

pub fn flow_timeout(state: &Arc<AppState>, run_id: i64) {
    let Some(active) = state.index.get(run_id) else {
        return;
    };
    if active.state.state_type != StateType::Running {
        return;
    }
    let Ok(Some(run)) = state.store.get_run(run_id) else {
        return;
    };
    let Ok(Some(flow)) = state.store.get_flow(run.flow_id) else {
        return;
    };
    let options = FlowOptions::from_map(&flow.options);
    let secs = options.timeout_seconds.unwrap_or(0.0);
    let mut s = State::named(StateName::TimedOut);
    s.message = Some(format!("timed out after {secs} s"));
    if let Ok(TransitionResult::Accepted(_)) = state.transition_run(run_id, s, false) {
        if let Some(pid) = active.engine_pid {
            crate::process::terminate(pid);
            let st = state.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(3));
                if crate::process::is_alive(pid) {
                    crate::process::kill(pid);
                }
                st.supervisor.forget_pid(pid);
            });
        }
    }
}

fn disable_window(state: &Arc<AppState>, flow: &Flow, options: &FlowOptions, run: &Run) {
    let Some((count, window, persist)) = options.disable_after else {
        return;
    };
    if count <= 0 {
        return;
    }
    let now = now_micros();
    let failures = state
        .supervisor
        .record_failure(flow.id, now, window.max(1) * 1_000_000);
    if failures >= count as usize {
        let until = now + persist.max(1) * 1_000_000;
        let mut paused_any = false;
        for row in state.scheduler.for_flow(flow.id) {
            if row.active {
                crate::scheduler::pause(state, row.id, Some("disabled"), Some(until));
                paused_any = true;
            }
        }
        if paused_any {
            state.timer.push(until, TimerEvent::ResumeFlow(flow.id));
            let _ = state.record_engine_event(
                EventName::FlowDisabled,
                Some(run.id),
                Some(flow.id),
                json!({"failures": failures, "window_seconds": window, "until": until}),
            );
            state.supervisor.clear_failures(flow.id);
        }
    }
}

fn render_template(template: &str, run: &Run) -> Value {
    let trimmed = template.trim();
    if let Some(inner) = trimmed
        .strip_prefix("{{")
        .and_then(|s| s.strip_suffix("}}"))
    {
        let path = inner.trim();
        if let Some(key) = path.strip_prefix("run.parameters.") {
            return run.parameters.get(key).cloned().unwrap_or(Value::Null);
        }
        match path {
            "run.id" => return Value::from(run.id),
            "run.name" => return Value::String(run.name.clone()),
            "run.external_id" => return Value::String(run.external_id.to_string()),
            _ => {}
        }
    }
    Value::String(template.to_string())
}

/// Create runs of flows declared `after=` this run's flow.
fn trigger_dependents(state: &Arc<AppState>, upstream: &Flow, run: &Run) {
    if run.created_by.starts_with("catchup") && run.state.name == "Skipped" {
        // Nothing ran; still counts as success per spec, so continue.
    }
    let Ok(flows) = state.store.list_flows(Some(&upstream.project)) else {
        return;
    };
    for downstream in flows {
        let opts = FlowOptions::from_map(&downstream.options);
        let Some(after) = opts.after.as_ref() else {
            continue;
        };
        if !after.depends_on(&upstream.name) || downstream.error.is_some() {
            continue;
        }
        // Keyed fan-in: every upstream must have completed the same batch, once per key.
        let mut fan_in_event: Option<Value> = None;
        if let Some(key) = after.key.as_deref() {
            let Some(value) = run.parameters.get(key) else {
                continue;
            };
            let text = match value {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            let mut upstream_runs: Vec<i64> = Vec::new();
            let mut complete = true;
            for name in after.upstreams() {
                let Ok(Some(up)) = state.store.get_flow_by_key(&downstream.project, &name) else {
                    complete = false;
                    break;
                };
                match state.store.latest_run_with_param(up.id, key, &text) {
                    Ok(Some(latest))
                        if latest.state.state_type == StateType::Completed
                            || latest.state.name == "Skipped" =>
                    {
                        upstream_runs.push(latest.id);
                    }
                    _ => {
                        complete = false;
                        break;
                    }
                }
            }
            if !complete {
                continue;
            }
            if let Ok(Some(_)) = state.store.latest_run_with_param(downstream.id, key, &text) {
                continue;
            }
            fan_in_event = Some(json!({
                "key": key, "value": value, "upstream_runs": upstream_runs,
                "flow": downstream.name, "project": downstream.project,
            }));
        }
        let mut params: Map<String, Value> = Map::new();
        let declared: Vec<String> = downstream
            .parameter_schema
            .get("properties")
            .and_then(|p| p.as_object())
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default();
        for name in &declared {
            if let Some(v) = run.parameters.get(name) {
                params.insert(name.clone(), v.clone());
            }
        }
        for (name, template) in &after.parameters {
            if let Some(t) = template.as_str() {
                params.insert(name.clone(), render_template(t, run));
            } else {
                params.insert(name.clone(), template.clone());
            }
        }
        if let Some(props) = downstream
            .parameter_schema
            .get("properties")
            .and_then(|p| p.as_object())
        {
            for (k, prop) in props {
                if !params.contains_key(k) {
                    if let Some(d) = prop.get("default") {
                        params.insert(k.clone(), d.clone());
                    }
                }
            }
        }
        let name = match (&after.key, fan_in_event.as_ref()) {
            (Some(key), Some(ev)) => format!(
                "{}-{}-{}",
                downstream.name,
                key,
                ev.get("value")
                    .map(|v| v
                        .as_str()
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| v.to_string()))
                    .unwrap_or_default()
                    .replace([':', ' ', '/'], "-")
            ),
            _ => format!("{}-after-{}", downstream.name, run.name),
        };
        let created = state.store.create_run_full(CreateRun {
            flow_id: downstream.id,
            name,
            parameters: serde_json::to_string(&params).unwrap_or_else(|_| "{}".into()),
            tags: serde_json::to_string(&downstream.tags).unwrap_or_else(|_| "[]".into()),
            created_by: format!("run:{}", run.id),
            initial_state: Some(State::new(StateType::Scheduled)),
            priority: opts.priority,
            ..Default::default()
        });
        if let Ok((run_id, _)) = created {
            if let Ok(Some(new_run)) = state.store.get_run(run_id) {
                state
                    .index
                    .insert_run(&new_run, EngineKey::from_flow(&downstream), false);
                state.run_created(&new_run);
                if let Some(mut ev) = fan_in_event {
                    if let Some(obj) = ev.as_object_mut() {
                        obj.insert("run_id".into(), json!(run_id));
                    }
                    let _ = state.record_engine_event(
                        EventName::FlowFanIn,
                        Some(run_id),
                        Some(downstream.id),
                        ev,
                    );
                }
                enqueue_run(state, &new_run, &downstream, None);
            }
        }
    }
}
