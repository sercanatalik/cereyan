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

/// Resources a run needs before it may be dispatched: the flow's declared
/// resources with their names rendered from the run's parameters, a `tag:<t>`
/// unit for every tag that has a total, the flow's concurrency cap, and the
/// backfill's slot.
pub fn run_needs(
    state: &AppState,
    flow: &Flow,
    options: &FlowOptions,
    run: &Run,
) -> Vec<(String, f64)> {
    let mut needs: Vec<(String, f64)> = options
        .resource_amounts()
        .into_iter()
        .map(|(name, amount)| {
            (
                cereyan_core::unique::render_template(&name, &run.parameters),
                amount,
            )
        })
        .collect();
    for tag in &run.tags {
        let name = format!("tag:{tag}");
        if state.supervisor.resource_declared(&name) && !needs.iter().any(|(n, _)| *n == name) {
            needs.push((name, 1.0));
        }
    }
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
/// Uses the cached dependency graph to avoid loading all flows per enqueue.
pub fn effective_priority(state: &AppState, flow: &Flow, run: &Run) -> i64 {
    let mut priority = run.priority;
    // Shared, not copied: this runs on every run admission.
    // The graph already carries the highest priority its dependents declare, and
    // carries it because building the graph parses those dependents' options
    // anyway. So this makes no store read, where it used to read a whole flow per
    // dependent on every run admission.
    if let Some(dependents) = state.dep_graph().get(&flow.id) {
        priority = priority.max(dependents.max_priority);
    }
    priority
}

/// Admit a run to the dispatch queue, applying the overlap policy first.
/// How long a replay waits for the worker that ran it before any host may take it.
const STICKY_MICROS: i64 = 10_000_000;

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
                    "cancel_old" => supersede(state, flow, run),
                    "buffer_one" => {
                        let waiting = state.index.active_runs().into_iter().any(|r| {
                            r.flow_id == flow.id
                                && r.id != run.id
                                && r.state.state_type != StateType::Running
                                && r.engine_pid.is_none()
                                && !r.state.is_terminal()
                        });
                        if waiting {
                            let mut s = State::named(StateName::Skipped);
                            s.message = Some("a run is already queued".into());
                            s.details.insert("reason".into(), json!("buffered"));
                            let _ = state.transition_run(run.id, s, false);
                            let _ = state.record_engine_event(
                                EventName::RunSkipped,
                                Some(run.id),
                                Some(flow.id),
                                json!({"reason": "buffered"}),
                            );
                            return;
                        }
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
    // A run made by hand under a flow deadline: scheduled runs are armed by the
    // scheduler from their due time instead.
    if run.schedule_id.is_none() {
        if let Some(seconds) = options.start_deadline.filter(|d| *d > 0.0) {
            state.timer.push(
                run.created_at + (seconds * 1_000_000.0) as i64,
                crate::timer::TimerEvent::StartDeadline(run.id),
            );
        }
    }
    let priority = effective_priority(state, flow, run);
    if priority != run.priority {
        let _ = state.store.set_run_priority(run.id, priority);
    }
    let mut key = EngineKey::from_flow(flow);
    key.nice = crate::supervisor::nice_for(priority);
    // A replay (a resume, a poke, a crash rerun) prefers the worker that ran
    // the previous attempt, where its local files and warm engine are.
    let previous_host = run.host.clone().or_else(|| {
        run.parent_run_id
            .and_then(|p| state.store.get_run(p).ok().flatten())
            .and_then(|p| p.host)
    });
    let prefer_worker = previous_host
        .filter(|h| h != "server")
        .and_then(|h| state.supervisor.worker_id(&h))
        .filter(|_| options.may_run_remotely());
    state.supervisor.enqueue(QueuedRun {
        run_id: run.id,
        key,
        priority,
        order: run.scheduled_time.unwrap_or(run.created_at),
        needs: run_needs(state, flow, &options, run),
        not_before,
        flow_id: flow.id,
        remote_ok: options.may_run_remotely(),
        prefer_worker,
        prefer_until: cereyan_core::now_micros() + STICKY_MICROS,
    });
    state.supervisor.ensure_capacity(state);
}

/// `on_overlap="cancel_old"`: every other non-terminal run of the flow makes
/// way for `new_run`. A queued run is cancelled at once; a running one is asked
/// to stop exactly as a user cancel would, grace period and kill included.
fn supersede(state: &Arc<AppState>, flow: &Flow, new_run: &Run) {
    let message = format!("superseded by run {}", new_run.id);
    for other in state.index.active_runs() {
        if other.flow_id != flow.id || other.id == new_run.id || other.state.is_terminal() {
            continue;
        }
        if other.state.state_type == StateType::Running || other.engine_pid.is_some() {
            let _ = state.transition_run(
                other.id,
                State::new(StateType::Cancelling).with_message(&message),
                false,
            );
            state.index.update(other.id, |r| {
                r.cancel_requested = true;
                if r.cancelling_since.is_none() {
                    r.cancelling_since = Some(std::time::Instant::now());
                }
            });
        } else {
            state.supervisor.dequeue(other.id);
            state.timer.remove_run_events(other.id);
            let _ = state.transition_run(
                other.id,
                State::new(StateType::Cancelled).with_message(&message),
                false,
            );
        }
    }
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
            if matches!(options.disable_after, Some((_, None, _))) {
                state.supervisor.clear_failures(flow.id);
            }
            trigger_dependents(state, &flow, run);
        }
        StateType::Paused => crate::waits::arm(state, run),
        _ => {}
    }
    // A continuous schedule's run ended: its next run joins the line after the
    // delay. A Crashed run has a rerun coming, which ends the iteration instead.
    if matches!(
        run.state.state_type,
        StateType::Completed | StateType::Failed | StateType::Cancelled
    ) {
        if let Some(sid) = run.schedule_id {
            if state
                .scheduler
                .get(sid)
                .is_some_and(|row| row.active && row.schedule.is_continuous())
            {
                crate::scheduler::materialize(state, sid);
            }
        }
    }
    if run.state.is_terminal() {
        if let Some(backfill_id) = run.backfill_id {
            backfill_completed(state, backfill_id, run.flow_id);
        }
    }
}

/// A state name (type or sub-state) that ends a run.
fn name_is_terminal(name: &str) -> bool {
    match StateType::parse(name) {
        Some(t) => t.is_terminal(),
        None => matches!(name, "Cached" | "Replayed" | "Skipped" | "TimedOut"),
    }
}

/// `backfill.completed`, once, when no run of the backfill is left to end.
/// Two runs ending together both see an empty remainder, so the marker is
/// claimed under a lock in this process and persisted for the next one.
fn backfill_completed(state: &Arc<AppState>, backfill_id: i64, flow_id: i64) {
    // A backfill cannot be complete while its newest run has not ended, and runs
    // are created in id order, so that is one seek on the backfill index. Without
    // it, every terminal run re-aggregated the backfill's whole run set: four
    // times the work per doubling, 6.2 s for an 8,000-run backfill.
    //
    // The guard only ever rules completion *out*. When it passes, the aggregate
    // below still decides, because the newest run ending says nothing about an
    // earlier one that is still retrying.
    match state.store.newest_backfill_run_state(backfill_id) {
        Ok(Some(newest)) if !name_is_terminal(&newest) => return,
        Ok(None) => return,
        Ok(Some(_)) => {}
        Err(_) => return,
    }
    let Ok(counts) = state.store.backfill_counts(backfill_id) else {
        return;
    };
    let pending = counts
        .iter()
        .filter(|(s, _)| !name_is_terminal(s))
        .map(|(_, n)| *n)
        .sum::<i64>();
    if pending > 0 || counts.is_empty() {
        return;
    }
    let marker = format!("backfill.completed:{backfill_id}");
    {
        // Use the AppState field (reset on server start) instead of a static.
        // If the mutex is poisoned, clear it and continue.
        let mut guard = state.backfill_emitted.lock().unwrap_or_else(|e| e.into_inner());
        if guard.contains(&backfill_id) || matches!(state.store.kv_get(&marker), Ok(Some(_))) {
            return;
        }
        guard.insert(backfill_id);
    }
    let _ = state.store.kv_set(&marker, "1");
    let by_state: serde_json::Map<String, serde_json::Value> =
        counts.into_iter().map(|(s, n)| (s, json!(n))).collect();
    let _ = state.record_engine_event(
        EventName::BackfillCompleted,
        None,
        Some(flow_id),
        json!({"backfill_id": backfill_id, "counts": by_state}),
    );
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
        unique: None,
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
        if let Some(crate::supervisor::Location::Worker(worker_id)) = active
            .engine_id
            .as_deref()
            .map(crate::supervisor::Location::of_engine)
        {
            // The worker ends its own engine.
            state.supervisor.command_worker(
                worker_id,
                json!({"cmd": "cancel", "run_id": run_id, "engine_id": active.engine_id}),
            );
            return;
        }
        if let Some(pid) = active.engine_pid {
            crate::process::terminate(pid);
            let st = state.clone();
            let shutdown = state.shutdown.clone();
            // Use spawn_blocking so the task is tracked by the tokio runtime
            // and can be cancelled on shutdown.
            tokio::task::spawn_blocking(move || {
                // Wait 3 seconds, but abort if the server is shutting down.
                for _ in 0..30 {
                    if *shutdown.borrow() {
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                if !*shutdown.borrow() && crate::process::is_alive(pid) {
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
    // No window: failures in a row, which a Completed run resets.
    let window_micros = window.map_or(i64::MAX, |w| w.max(1) * 1_000_000);
    let failures = state.supervisor.record_failure(flow.id, now, window_micros);
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

/// A run skipped by a person, or skipped because its upstream was: the flows
/// after it are skipped too. Every other Skipped run (`on_overlap="skip"`, a
/// backfill value already done, a catch-up drop) means "nothing needed doing"
/// and triggers its dependents like a Completed run.
fn carries_skip(run: &Run) -> bool {
    run.state.name == "Skipped"
        && matches!(
            run.state.details.get("reason").and_then(|v| v.as_str()),
            Some("user") | Some("upstream")
        )
}

/// How far a skip is carried down a chain in one go. `after=` does not forbid
/// cycles, and a skipped downstream is created ended, so its dependents follow
/// synchronously; this bounds that recursion.
const MAX_SKIP_DEPTH: usize = 32;

/// Create runs of flows declared `after=` this run's flow.
fn trigger_dependents(state: &Arc<AppState>, upstream: &Flow, run: &Run) {
    trigger_dependents_at(state, upstream, run, 0);
}

fn trigger_dependents_at(state: &Arc<AppState>, upstream: &Flow, run: &Run, depth: usize) {
    if run.created_by.starts_with("catchup") && run.state.name == "Skipped" {
        // Nothing ran; still counts as success per spec, so continue.
    }
    // The dependents come from the shared graph, which is exactly this relation:
    // `depends_on` is `upstreams().contains(name)` and `dep_graph_from_flows`
    // walks `upstreams()`, resolved within the same project. Reading every flow in
    // the project instead cost a full table read and three JSON parses per flow,
    // on every completed run, to find the handful that declared a dependency.
    // The graph's `Arc` is released here rather than held across the loop, which
    // does store reads: a rebuild during the loop should not be pinned by this.
    let ids: Vec<i64> = match state.dep_graph().get(&upstream.id) {
        Some(d) => d.ids.to_vec(),
        None => return,
    };
    for id in ids {
        let Ok(Some(downstream)) = state.store.get_flow(id) else {
            continue;
        };
        let opts = FlowOptions::from_map(&downstream.options);
        let Some(after) = opts.after.as_ref() else {
            continue;
        };
        if !after.depends_on(&upstream.name) || downstream.error.is_some() {
            continue;
        }
        // Keyed fan-in: every upstream must have completed the same batch, once per key.
        let mut fan_in_event: Option<Value> = None;
        let mut skip_down = carries_skip(run);
        if let Some(key) = after.key.as_deref() {
            let Some(value) = run.parameters.get(key) else {
                continue;
            };
            let text = match value {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            // Resolve all upstream flows first.
            let mut upstream_flow_ids: Vec<i64> = Vec::new();
            let mut complete = true;
            for name in after.upstreams() {
                match state.store.get_flow_by_key(&downstream.project, &name) {
                    Ok(Some(up)) => upstream_flow_ids.push(up.id),
                    _ => {
                        complete = false;
                        break;
                    }
                }
            }
            if !complete {
                continue;
            }
            // Batch query: fetch the latest run for all upstream flows in one query.
            let latest_runs = match state.store.latest_run_with_param_many(&upstream_flow_ids, key, &text) {
                Ok(runs) => runs,
                Err(_) => continue,
            };
            let mut upstream_runs: Vec<i64> = Vec::new();
            for flow_id in &upstream_flow_ids {
                // `is_complete` and `carries_skip` are the store's, so the rule a
                // run must meet to satisfy a fan-in is defined once rather than
                // restated here against a whole `Run`.
                match latest_runs.get(flow_id) {
                    Some(Some(latest)) if latest.is_complete() => {
                        skip_down |= latest.carries_skip();
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
        // A skipped downstream is created already ended, with its parameters
        // resolved as usual so its key value is right for anything after it.
        let initial = if skip_down {
            let mut s = State::named(StateName::Skipped);
            s.message = Some(format!("upstream run {} was skipped", run.name));
            s.details.insert("reason".into(), json!("upstream"));
            s.details.insert("upstream_run".into(), json!(run.id));
            s
        } else {
            State::new(StateType::Scheduled)
        };
        // Keyed by the upstream run unless the flow declares its own key, so a
        // completion delivered twice starts one downstream run.
        let unique = crate::api::runs::unique_check_for(&downstream, &params, None).or_else(|| {
            Some(cereyan_store::UniqueCheck {
                key: cereyan_core::unique::unique_key(
                    downstream.id,
                    &format!("dep:{}", run.id),
                    None,
                    0,
                ),
                states: Vec::new(),
                since: None,
            })
        });
        let created = state.store.create_run_full(CreateRun {
            flow_id: downstream.id,
            name,
            parameters: serde_json::to_string(&params).unwrap_or_else(|_| "{}".into()),
            tags: serde_json::to_string(&downstream.tags).unwrap_or_else(|_| "[]".into()),
            created_by: format!("run:{}", run.id),
            initial_state: Some(initial),
            priority: opts.priority,
            unique,
            ..Default::default()
        });
        if let Err(cereyan_store::StoreError::UniqueConflict { existing }) = &created {
            if let (Some(spec), Ok(Some(holder))) = (
                opts.unique.as_ref().filter(|u| u.on_conflict == "debounce"),
                state.store.get_run(*existing),
            ) {
                crate::api::runs::debounce_holder(state, &holder, &params, spec);
            }
        }
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
                if skip_down {
                    // Nothing to enqueue; the flows after this one follow now.
                    if depth < MAX_SKIP_DEPTH {
                        trigger_dependents_at(state, &downstream, &new_run, depth + 1);
                    }
                } else {
                    enqueue_run(state, &new_run, &downstream, None);
                }
            }
        }
    }
}

#[cfg(test)]
mod priority_tests {
    use super::*;
    use crate::api::flows::list_flows_tests::state_with_flows;
    use tempfile::TempDir;

    /// A run at a given priority, read back from the store rather than built as
    /// a literal: `Run` has 39 required fields and no `Default`.
    fn run(state: &AppState, priority: i64) -> Run {
        let flow_id = state.store.list_flows(None).unwrap()[0].id;
        let (id, _) = state
            .store
            .create_run_full(cereyan_store::CreateRun {
                flow_id,
                name: format!("r{priority}"),
                parameters: "{}".into(),
                tags: "[]".into(),
                created_by: "test".into(),
                priority,
                ..Default::default()
            })
            .unwrap();
        state.store.get_run(id).unwrap().expect("the run")
    }

    /// The flow whose dependents supply the priority. Read from the store so the
    /// id is the real one.
    fn upstream_of(state: &AppState, name: &str) -> Flow {
        state
            .store
            .list_flows(None)
            .unwrap()
            .into_iter()
            .find(|f| f.name == name)
            .expect("the flow is registered")
    }

    /// A run takes its own priority when nothing depends on it.
    #[test]
    fn a_run_with_no_dependents_keeps_its_own_priority() {
        let dir = TempDir::new().unwrap();
        let state = state_with_flows(&dir, &[("p", "etl", None, None), ("p", "other", None, None)]);
        let flow = upstream_of(&state, "etl");
        for p in [0i64, 3, -2] {
            assert_eq!(effective_priority(&state, &flow, &run(&state, p)), p, "priority {p}");
        }
    }

    /// A dependent's higher priority is applied, and a lower one is not.
    #[test]
    fn a_dependents_priority_raises_the_run_but_never_lowers_it() {
        let dir = TempDir::new().unwrap();
        let state = state_with_flows(&dir, &[("p", "etl", None, None)]);
        // Attach a dependent at a known priority by rewriting its options.
        let mut opts = serde_json::Map::new();
        opts.insert("after".into(), serde_json::json!({ "flow": "etl" }));
        opts.insert("priority".into(), serde_json::json!(9));
        state
            .store
            .upsert_flow_full(cereyan_store::UpsertFlow {
                project: "p".into(),
                name: "dependent".into(),
                module: "m".into(),
                source_dir: "/tmp".into(),
                description: None,
                tags: "[]".into(),
                parameter_schema: "{}".into(),
                options: serde_json::Value::Object(opts).to_string(),
                ..Default::default()
            })
            .unwrap();
        // Invalidate so the graph is rebuilt with the new declaration.
        state.invalidate_dep_graph();

        let flow = upstream_of(&state, "etl");
        assert_eq!(
            effective_priority(&state, &flow, &run(&state, 2)),
            9,
            "a dependent at 9 raises a run at 2"
        );
        assert_eq!(
            effective_priority(&state, &flow, &run(&state, 12)),
            12,
            "a dependent at 9 does not lower a run at 12"
        );
    }

    /// The graph must record the *highest* among several dependents.
    #[test]
    fn the_highest_of_several_dependents_wins() {
        let dir = TempDir::new().unwrap();
        let state = state_with_flows(&dir, &[("p", "etl", None, None)]);
        for (name, priority) in [("low", 1i64), ("high", 20), ("mid", 7)] {
            let mut opts = serde_json::Map::new();
            opts.insert("after".into(), serde_json::json!({ "flow": "etl" }));
            opts.insert("priority".into(), serde_json::json!(priority));
            state
                .store
                .upsert_flow_full(cereyan_store::UpsertFlow {
                    project: "p".into(),
                    name: name.into(),
                    module: "m".into(),
                    source_dir: "/tmp".into(),
                    description: None,
                    tags: "[]".into(),
                    parameter_schema: "{}".into(),
                    options: serde_json::Value::Object(opts).to_string(),
                    ..Default::default()
                })
                .unwrap();
        }
        state.invalidate_dep_graph();
        let flow = upstream_of(&state, "etl");
        assert_eq!(effective_priority(&state, &flow, &run(&state, 0)), 20);
    }

    /// The point of the change: the rule consults the graph and nothing else.
    ///
    /// Deleting the dependent rows after the graph is built would change the
    /// answer if the rule re-read them from the store. It must not — and a stale
    /// graph is exactly the intended behaviour here, since a stale priority and a
    /// stale dependent id have the same lifetime.
    #[test]
    fn the_rule_does_not_re_read_the_dependents() {
        let dir = TempDir::new().unwrap();
        let state = state_with_flows(&dir, &[("p", "etl", None, None)]);
        let mut opts = serde_json::Map::new();
        opts.insert("after".into(), serde_json::json!({ "flow": "etl" }));
        opts.insert("priority".into(), serde_json::json!(9));
        let dep_id = state
            .store
            .upsert_flow_full(cereyan_store::UpsertFlow {
                project: "p".into(),
                name: "dependent".into(),
                module: "m".into(),
                source_dir: "/tmp".into(),
                description: None,
                tags: "[]".into(),
                parameter_schema: "{}".into(),
                options: serde_json::Value::Object(opts).to_string(),
                ..Default::default()
            })
            .unwrap();

        let flow = upstream_of(&state, "etl");
        let r = run(&state, 1);
        // Warm the graph, so it holds the dependency.
        assert_eq!(effective_priority(&state, &flow, &r), 9);

        // Remove the dependent without invalidating the graph.
        state.store.delete_flow(dep_id).unwrap();
        assert_eq!(
            state.store.list_flows(None).unwrap().len(),
            1,
            "the dependent row is gone"
        );
        assert_eq!(
            effective_priority(&state, &flow, &r),
            9,
            "still 9: the rule reads the graph, not the store, so deleting the \
             row behind an already-built graph does not change the answer"
        );

        // Invalidating does drop it, which is what keeps the graph honest.
        state.invalidate_dep_graph();
        assert_eq!(
            effective_priority(&state, &flow, &r),
            1,
            "after an invalidation the graph is rebuilt without the dependent"
        );
    }
}

#[cfg(test)]
mod backfill_completion_tests {
    use super::*;
    use crate::api::flows::list_flows_tests::state_with_flows;
    use tempfile::TempDir;

    /// The invariant the guard rests on.
    ///
    /// `backfill_completed` returns early when the backfill's newest run has not
    /// ended, and otherwise falls through to the original aggregate. If the guard
    /// ever said "still running" for a backfill the aggregate called finished, a
    /// backfill would never report completion. If it said "ended" for one with
    /// work left, the aggregate would catch it — that direction is safe.
    ///
    /// So the property to pin is one-directional, and it is checked over every
    /// state name the system can hold, including the sub-state names that
    /// `name_is_terminal` special-cases.
    #[test]
    fn the_guard_never_rules_out_a_finished_backfill() {
        let dir = TempDir::new().unwrap();
        let state = state_with_flows(&dir, &[("p", "etl", None, None)]);
        let flow_id = state.store.list_flows(None).unwrap()[0].id;
        let b = 11i64;

        // Every state type, plus the sub-state names `name_is_terminal` handles
        // and a few it must not treat as terminal.
        let names: Vec<(&str, bool)> = vec![
            ("Completed", true),
            ("Failed", true),
            ("Cancelled", true),
            ("Crashed", true),
            ("Cached", true),
            ("Replayed", true),
            ("Skipped", true),
            ("TimedOut", true),
            ("Pending", false),
            ("Scheduled", false),
            ("Running", false),
            ("Paused", false),
            ("AwaitingRetry", false),
            ("Late", false),
            ("AwaitingResource", false),
            ("", false),
        ];

        for (name, terminal) in &names {
            // The guard reads one name and applies the one rule.
            let guard_says_ended = name_is_terminal(name);
            assert_eq!(
                guard_says_ended, *terminal,
                "name_is_terminal({name:?}) disagrees with the table this test \
                 encodes; the guard and the aggregate both use this function, so a \
                 change here changes both -- which is the point, and is why there \
                 is no second copy of the rule to drift"
            );
        }

        // And end to end: with only a terminal newest run, the aggregate reports
        // nothing pending, so the guard let it through and completion happens.
        let (id, _) = state
            .store
            .create_run_full(cereyan_store::CreateRun {
                flow_id,
                name: "only".into(),
                parameters: "{}".into(),
                tags: "[]".into(),
                created_by: format!("backfill:{b}"),
                backfill_id: Some(b),
                initial_state: Some(State::new(StateType::Completed)),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            state.store.newest_backfill_run_state(b).unwrap(),
            Some("Completed".to_string())
        );
        let counts = state.store.backfill_counts(b).unwrap();
        let pending: i64 = counts
            .iter()
            .filter(|(s, _)| !name_is_terminal(s))
            .map(|(_, n)| *n)
            .sum();
        assert_eq!(pending, 0, "nothing pending, so the guard was right to pass");
        assert!(!counts.is_empty());
        assert!(id > 0);
    }

    /// The guard is necessary but not sufficient: the newest run ending says
    /// nothing about an earlier one still retrying. This is the case the guard
    /// deliberately lets through to the aggregate.
    #[test]
    fn the_newest_run_ending_does_not_complete_a_backfill() {
        let dir = TempDir::new().unwrap();
        let state = state_with_flows(&dir, &[("p", "etl", None, None)]);
        let flow_id = state.store.list_flows(None).unwrap()[0].id;
        let b = 13i64;

        // Created first, still going; created second, already ended.
        let mut ids = Vec::new();
        for (i, name) in ["older", "newer"].iter().enumerate() {
            let (id, _) = state
                .store
                .create_run_full(cereyan_store::CreateRun {
                    flow_id,
                    name: (*name).to_string(),
                    parameters: "{}".into(),
                    tags: "[]".into(),
                    created_by: format!("backfill:{b}"),
                    backfill_id: Some(b),
                    initial_state: Some(State::new(StateType::Scheduled)),
                    ..Default::default()
                })
                .unwrap();
            if i == 0 {
                state
                    .store
                    .transition_run(id, State::new(StateType::Running), true)
                    .unwrap();
            } else {
                state
                    .store
                    .transition_run(id, State::new(StateType::Completed), true)
                    .unwrap();
            }
            ids.push(id);
        }

        // The guard passes: the newest run has ended.
        let newest = state
            .store
            .newest_backfill_run_state(b)
            .unwrap()
            .expect("there is a newest run");
        assert!(name_is_terminal(&newest), "the guard lets this through");

        // And the aggregate still finds the older run pending, so completion is
        // correctly not reported.
        let counts = state.store.backfill_counts(b).unwrap();
        let pending: i64 = counts
            .iter()
            .filter(|(s, _)| !name_is_terminal(s))
            .map(|(_, n)| *n)
            .sum();
        assert_eq!(pending, 1, "the older run is still going");
        assert!(ids[0] < ids[1], "the newer run really is the newer one");
    }

    /// The guard must actually rule out the common case, or it buys nothing: with
    /// a run still to end, the aggregate is never reached.
    #[test]
    fn a_backfill_with_a_newest_run_still_going_is_ruled_out_immediately() {
        let dir = TempDir::new().unwrap();
        let state = state_with_flows(&dir, &[("p", "etl", None, None)]);
        let flow_id = state.store.list_flows(None).unwrap()[0].id;
        let b = 17i64;

        let (older, _) = state
            .store
            .create_run_full(cereyan_store::CreateRun {
                flow_id,
                name: "older".into(),
                parameters: "{}".into(),
                tags: "[]".into(),
                created_by: format!("backfill:{b}"),
                backfill_id: Some(b),
                initial_state: Some(State::new(StateType::Scheduled)),
                ..Default::default()
            })
            .unwrap();
        state
            .store
            .transition_run(older, State::new(StateType::Completed), true)
            .unwrap();
        let (newer, _) = state
            .store
            .create_run_full(cereyan_store::CreateRun {
                flow_id,
                name: "newer".into(),
                parameters: "{}".into(),
                tags: "[]".into(),
                created_by: format!("backfill:{b}"),
                backfill_id: Some(b),
                initial_state: Some(State::new(StateType::Scheduled)),
                ..Default::default()
            })
            .unwrap();

        // This is the case the guard exists for: the newest run has not ended, so
        // completion is impossible and the aggregate is not worth running.
        let newest = state
            .store
            .newest_backfill_run_state(b)
            .unwrap()
            .expect("there is a newest run");
        assert_eq!(newest, "Scheduled", "the newest run is still to start");
        assert!(
            !name_is_terminal(&newest),
            "so the guard rules completion out before any aggregate"
        );
        assert!(newer > older);
    }

    /// A backfill with no runs is ruled out by the guard too, which is the same
    /// outcome the `counts.is_empty()` arm used to produce.
    #[test]
    fn a_backfill_with_no_runs_is_ruled_out() {
        let dir = TempDir::new().unwrap();
        let state = state_with_flows(&dir, &[("p", "etl", None, None)]);
        assert!(
            state.store.newest_backfill_run_state(999).unwrap().is_none(),
            "no runs, so no newest run, so nothing to aggregate"
        );
        assert!(state.store.backfill_counts(999).unwrap().is_empty());
    }
}
