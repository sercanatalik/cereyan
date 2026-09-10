//! The rules engine: index rules by event name, apply guards, render
//! templates, execute actions sequentially with a timeout, and record firings.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use cereyan_core::schedule::{from_micros, to_micros, Schedule};
use cereyan_core::{
    now_micros, Event, EventName, Expectation, Flow, FlowOptions, RuleAction, RuleRow, Run, State,
    StateType,
};
use cereyan_rules::{GuardDecision, GuardState, RuleIndex, RunContext};
use cereyan_store::{ArmExpectation, CreateRun};

use crate::timer::TimerEvent;
use serde_json::{json, Value};

use crate::state::{AppState, TransitionResult};
use crate::supervisor::EngineKey;

/// Executes `call` actions of code rules (a Python callable under the GIL).
pub trait RuleDispatcher: Send + Sync {
    fn call(&self, callable: &str, event: &Value, run: &Value) -> Result<Value, String>;
}

#[derive(Default)]
pub struct RulesState {
    rules: RwLock<Vec<RuleRow>>,
    index: RwLock<RuleIndex>,
    guards: Mutex<HashMap<i64, GuardState>>,
    /// Last evaluated tick of each clock-armed rule (microseconds).
    clock_last: Mutex<HashMap<i64, i64>>,
}

/// Cron of a clock-armed rule as a schedule.
fn clock_schedule(rule: &RuleRow) -> Option<Schedule> {
    let clock = rule.spec.at.as_ref()?;
    Some(Schedule::Cron {
        cron: clock.cron.clone(),
        timezone: clock.tz.clone(),
        day_or: true,
    })
}

const CLOCK_LAST_PREFIX: &str = "rules.clock_last:";

impl RulesState {
    pub fn all(&self) -> Vec<RuleRow> {
        self.rules.read().unwrap_or_else(|e| e.into_inner()).clone()
    }
    pub fn get(&self, id: i64) -> Option<RuleRow> {
        self.all().into_iter().find(|r| r.id == id)
    }
}

/// (Re)load rules from the store into memory. Clock-armed rules that are
/// enabled get their next tick pushed if none is pending.
pub fn load(state: &AppState) {
    let rules = state.store.list_rules().unwrap_or_default();
    *state.rules.index.write().unwrap_or_else(|e| e.into_inner()) = RuleIndex::build(&rules);
    *state.rules.rules.write().unwrap_or_else(|e| e.into_inner()) = rules.clone();
    for rule in rules
        .iter()
        .filter(|r| r.enabled && r.spec.is_clock_armed())
    {
        schedule_clock(state, rule);
    }
}

/// Push the next tick of a clock-armed rule unless one is already pending.
fn schedule_clock(state: &AppState, rule: &RuleRow) {
    let Some(schedule) = clock_schedule(rule) else {
        return;
    };
    if state.timer.has_rule_clock(rule.id) {
        return;
    }
    let now = now_micros();
    if let Ok(Some(next)) = schedule.next_after(from_micros(now)) {
        state
            .timer
            .push(to_micros(next), TimerEvent::RuleClock(rule.id));
    }
}

/// Reload open expectations and clock ticks on start. Expectations already
/// past their deadline fire once now; clock ticks missed while down are
/// skipped with a log line.
pub fn start(state: &Arc<AppState>) {
    let now = now_micros();
    if let Ok(open) = state.store.open_expectations() {
        for e in open {
            state
                .timer
                .push(e.deadline.max(now), TimerEvent::Expectation(e.id));
        }
    }
    for rule in state.rules.all() {
        if !rule.enabled || !rule.spec.is_clock_armed() {
            continue;
        }
        let key = format!("{CLOCK_LAST_PREFIX}{}", rule.id);
        if let Ok(Some(last)) = state.store.kv_get(&key) {
            if let Ok(last) = last.parse::<i64>() {
                state
                    .rules
                    .clock_last
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(rule.id, last);
                if let Some(schedule) = clock_schedule(&rule) {
                    if let Ok(Some(missed)) = schedule.next_after(from_micros(last)) {
                        if to_micros(missed) < now {
                            eprintln!(
                                "cereyan: rule {:?} skipped clock tick(s) since {} while the server was down",
                                rule.name,
                                missed.to_rfc3339()
                            );
                        }
                    }
                }
            }
        }
        // `load` already pushed the next tick.
    }
}

/// Cancel the open expectations of a rule (disable, delete, prune).
pub fn cancel_expectations(state: &AppState, rule_id: i64) {
    let _ = state.store.cancel_rule_expectations(rule_id);
    state.timer.remove_rule_clock(rule_id);
}

/// The expectation key of an event: its run, else its flow.
fn expectation_key(event: &Event, ctx: &RunContext) -> Option<(String, Option<i64>, Option<i64>)> {
    if let Some(run) = &ctx.run {
        return Some((format!("run:{}", run.id), Some(run.id), Some(run.flow_id)));
    }
    let flow_id = ctx.flow.as_ref().map(|f| f.id).or(event.flow_id)?;
    Some((format!("flow:{flow_id}"), None, Some(flow_id)))
}

/// Arm and disarm expectations for one event. Runs before the reactive path.
fn track_expectations(state: &Arc<AppState>, event: &Event, ctx: &RunContext, candidates: &[i64]) {
    let Some((key, run_id, flow_id)) = expectation_key(event, ctx) else {
        return;
    };
    for rule in state
        .rules
        .all()
        .into_iter()
        .filter(|r| candidates.contains(&r.id) && r.enabled && r.spec.is_proactive())
    {
        let Some(unless) = &rule.spec.unless else {
            continue;
        };
        if cereyan_rules::matches(unless, event, ctx) {
            if let Ok(met) = state
                .store
                .disarm_expectations(rule.id, &key, event.occurred)
            {
                for id in met {
                    state.stream.publish(
                        EventName::ExpectationMet.as_str(),
                        id.to_string(),
                        json!({"id": id, "rule_id": rule.id, "key": key}),
                    );
                }
            }
        }
        if rule.spec.at.is_some() {
            continue;
        }
        if cereyan_rules::matches(&rule.spec.when, event, ctx) {
            if let Some(run) = &ctx.run {
                if !rule.spec.allow_self && run.created_by == format!("rule:{}", rule.id) {
                    continue;
                }
            }
            let within = (rule.spec.within.unwrap_or(0.0) * 1e6) as i64;
            let deadline = event.occurred + within;
            if let Ok(id) = state.store.arm_expectation(ArmExpectation {
                rule_id: rule.id,
                key: key.clone(),
                run_id,
                flow_id,
                armed_at: event.occurred,
                deadline,
            }) {
                state.timer.push(deadline, TimerEvent::Expectation(id));
                state.stream.publish(
                    EventName::ExpectationArmed.as_str(),
                    id.to_string(),
                    json!({"id": id, "rule_id": rule.id, "key": key, "run_id": run_id, "deadline": deadline}),
                );
            }
        }
    }
}

/// An expectation's deadline arrived: fire the rule if it is still open.
pub fn expectation_due(state: &Arc<AppState>, id: i64) {
    let Some(exp) = state.store.get_expectation(id).ok().flatten() else {
        return;
    };
    if exp.status != "open" {
        return;
    }
    let Some(rule) = state.rules.get(exp.rule_id) else {
        let _ = state.store.settle_expectation(id, "cancelled");
        return;
    };
    if !rule.enabled {
        let _ = state.store.settle_expectation(id, "cancelled");
        return;
    }
    if !matches!(state.store.settle_expectation(id, "lapsed"), Ok(true)) {
        return;
    }
    let run: Option<Run> = exp
        .run_id
        .and_then(|r| state.store.get_run(r).ok().flatten());
    let flow: Option<Flow> = run
        .as_ref()
        .map(|r| r.flow_id)
        .or(exp.flow_id)
        .and_then(|f| state.store.get_flow(f).ok().flatten());
    lapse(state, &rule, RunContext { run, flow }, Some(&exp));
}

/// A clock-armed rule's tick: fire when the window held no expected event.
pub fn clock_tick(state: &Arc<AppState>, rule_id: i64) {
    let Some(rule) = state.rules.get(rule_id) else {
        return;
    };
    let Some(schedule) = clock_schedule(&rule) else {
        return;
    };
    if !rule.enabled || !rule.spec.is_clock_armed() {
        return;
    }
    let now = now_micros();
    let previous = state
        .rules
        .clock_last
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&rule_id)
        .copied();
    let since = match rule.spec.within {
        Some(w) if w > 0.0 => now - (w * 1e6) as i64,
        _ => previous.unwrap_or_else(|| {
            // First tick: look back one interval.
            let next = schedule
                .next_after(from_micros(now))
                .ok()
                .flatten()
                .map(to_micros)
                .unwrap_or(now);
            now - (next - now).max(60_000_000)
        }),
    };
    {
        let mut last = state
            .rules
            .clock_last
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        last.insert(rule_id, now);
    }
    let _ = state
        .store
        .kv_set(&format!("{CLOCK_LAST_PREFIX}{rule_id}"), &now.to_string());
    if let Ok(Some(next)) = schedule.next_after(from_micros(now)) {
        state
            .timer
            .push(to_micros(next), TimerEvent::RuleClock(rule_id));
    }
    // A rule cannot report that something did not happen over a period it was not
    // watching. The window of an early tick reaches back before the rule existed,
    // where the store is empty because nothing could have been recorded yet, not
    // because the expected event was missed. Wait until the rule has been alive for
    // a whole window; the tick above keeps the schedule running meanwhile.
    if since < rule.created_at {
        return;
    }
    let unless = rule.spec.unless.clone().unwrap_or_default();
    let seen = state
        .store
        .events_between(&unless.events, since, now, 10_000)
        .unwrap_or_default()
        .into_iter()
        .any(|e| {
            let ctx = context_for(state, &e);
            cereyan_rules::matches(&unless, &e, &ctx)
        });
    if seen {
        return;
    }
    let flow: Option<Flow> = unless
        .flows
        .first()
        .and_then(|name| find_flow(state, name, unless.project.as_deref()).ok());
    lapse(state, &rule, RunContext { run: None, flow }, None);
}

/// Record an `expectation.lapsed` event and fire the rule against it.
fn lapse(state: &Arc<AppState>, rule: &RuleRow, ctx: RunContext, exp: Option<&Expectation>) {
    let now = now_micros();
    {
        let mut guards = state.rules.guards.lock().unwrap_or_else(|e| e.into_inner());
        let g = guards.entry(rule.id).or_default();
        // Guards need an event; the arming run is the guard key.
        let probe = Event {
            id: 0,
            seq: 0,
            external_id: cereyan_core::new_id(),
            name: EventName::ExpectationLapsed.as_str().into(),
            occurred: now,
            resource: cereyan_core::Resource {
                kind: "rule".into(),
                id: rule.id.to_string(),
                name: rule.name.clone(),
            },
            related: Vec::new(),
            run_id: ctx.run.as_ref().map(|r| r.id),
            flow_id: ctx.flow.as_ref().map(|f| f.id),
            payload: Default::default(),
        };
        match cereyan_rules::check_guards(rule, &probe, &ctx, g, now) {
            GuardDecision::Fire => g.record(ctx.run.as_ref().map(|r| r.id), now),
            GuardDecision::Skip(_) => return,
        }
    }
    let unless = rule.spec.unless.clone().unwrap_or_default();
    let mut related = Vec::new();
    if let Some(run) = &ctx.run {
        related.push(cereyan_core::Resource {
            kind: "run".into(),
            id: run.id.to_string(),
            name: run.name.clone(),
        });
    }
    if let Some(flow) = &ctx.flow {
        related.push(cereyan_core::Resource {
            kind: "flow".into(),
            id: format!("{}/{}", flow.project, flow.name),
            name: flow.name.clone(),
        });
    }
    let event = cereyan_store::NewEvent {
        name: EventName::ExpectationLapsed.as_str().into(),
        run_id: ctx.run.as_ref().map(|r| r.id),
        flow_id: ctx.flow.as_ref().map(|f| f.id),
        payload: json!({
            "rule_id": rule.id,
            "rule": rule.name,
            "flow": ctx.flow.as_ref().map(|f| f.name.clone()),
            "project": ctx.flow.as_ref().map(|f| f.project.clone()),
            "run": ctx.run.as_ref().map(|r| r.id),
            "run_name": ctx.run.as_ref().map(|r| r.name.clone()),
            "expected": unless.events,
            "deadline": exp.map(|e| e.deadline).unwrap_or(now),
            "armed_at": exp.map(|e| e.armed_at),
            "expectation_id": exp.map(|e| e.id),
        }),
        resource: cereyan_core::Resource {
            kind: "rule".into(),
            id: rule.id.to_string(),
            name: rule.name.clone(),
        },
        related,
    };
    let Ok((id, _)) = state.store.append_event(event) else {
        return;
    };
    let Some(event) = state.store.get_event(id).ok().flatten() else {
        return;
    };
    state.stream.publish(
        "event.created",
        id.to_string(),
        serde_json::to_value(&event).unwrap_or_default(),
    );
    if let Some(e) = exp {
        state.stream.publish(
            EventName::ExpectationLapsed.as_str(),
            e.id.to_string(),
            json!({"id": e.id, "rule_id": rule.id, "event_id": id}),
        );
    }
    let st = state.clone();
    let rule = rule.clone();
    std::thread::Builder::new()
        .name("cereyan-rules".into())
        .spawn(move || fire(&st, &rule, &event, &ctx))
        .ok();
}

pub const ACTION_TIMEOUT: Duration = Duration::from_secs(10);

fn context_for(state: &AppState, event: &Event) -> RunContext {
    let run: Option<Run> = event
        .run_id
        .and_then(|id| state.store.get_run(id).ok().flatten());
    let flow: Option<Flow> = run
        .as_ref()
        .map(|r| r.flow_id)
        .or(event.flow_id)
        .and_then(|id| state.store.get_flow(id).ok().flatten());
    RunContext { run, flow }
}

/// Evaluate every candidate rule against a freshly recorded event.
pub fn on_event(state: &Arc<AppState>, event: Event) {
    if event.name.starts_with("rule.") {
        return;
    }
    let candidates = state
        .rules
        .index
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .candidates(&event.name);
    if candidates.is_empty() {
        return;
    }
    let rules = state.rules.all();
    let ctx = context_for(state, &event);
    let now = now_micros();
    if !event.name.starts_with("expectation.") {
        track_expectations(state, &event, &ctx, &candidates);
    }
    let mut to_fire: Vec<RuleRow> = Vec::new();
    {
        let mut guards = state.rules.guards.lock().unwrap_or_else(|e| e.into_inner());
        for rule in rules
            .into_iter()
            .filter(|r| candidates.contains(&r.id) && !r.spec.is_proactive())
        {
            if !cereyan_rules::matches(&rule.spec.when, &event, &ctx) {
                continue;
            }
            let g = guards.entry(rule.id).or_default();
            match cereyan_rules::check_guards(&rule, &event, &ctx, g, now) {
                GuardDecision::Fire => {
                    g.record(ctx.run.as_ref().map(|r| r.id), now);
                    to_fire.push(rule);
                }
                GuardDecision::Skip(_) => {}
            }
        }
    }
    if to_fire.is_empty() {
        return;
    }
    let st = state.clone();
    std::thread::Builder::new()
        .name("cereyan-rules".into())
        .spawn(move || {
            for rule in to_fire {
                fire(&st, &rule, &event, &ctx);
            }
        })
        .ok();
}

/// Execute a rule's actions against an event and record the firing.
pub fn fire(state: &Arc<AppState>, rule: &RuleRow, event: &Event, ctx: &RunContext) {
    let env = cereyan_rules::environment();
    let template_ctx = cereyan_rules::template_context(event, ctx);
    let mut outcomes: Vec<Value> = Vec::new();
    let _ = state.record_engine_event(
        EventName::RuleFired,
        ctx.run.as_ref().map(|r| r.id),
        ctx.flow.as_ref().map(|f| f.id),
        json!({"rule_id": rule.id, "rule": rule.name, "event": event.name, "event_id": event.id}),
    );
    for (i, action) in rule.spec.actions.iter().enumerate() {
        let rendered = match cereyan_rules::render_action(&env, action, &template_ctx) {
            Ok(a) => a,
            Err(e) => {
                let msg = e.to_string();
                outcomes.push(
                    json!({"index": i, "kind": action.kind, "status": "failed", "error": msg}),
                );
                let _ = state.record_engine_event(
                    EventName::RuleActionFailed,
                    ctx.run.as_ref().map(|r| r.id),
                    ctx.flow.as_ref().map(|f| f.id),
                    json!({"rule_id": rule.id, "action": action.kind, "index": i, "error": msg}),
                );
                continue;
            }
        };
        let result = run_with_timeout(
            state.clone(),
            rule.clone(),
            rendered.clone(),
            ctx.clone(),
            event.clone(),
        );
        match result {
            Ok(detail) => {
                outcomes.push(json!({"index": i, "kind": action.kind, "status": "completed", "detail": detail}));
                let _ = state.record_engine_event(
                    EventName::RuleActionCompleted,
                    ctx.run.as_ref().map(|r| r.id),
                    ctx.flow.as_ref().map(|f| f.id),
                    json!({"rule_id": rule.id, "action": action.kind, "index": i, "detail": detail}),
                );
            }
            Err(msg) => {
                outcomes.push(
                    json!({"index": i, "kind": action.kind, "status": "failed", "error": msg}),
                );
                let _ = state.record_engine_event(
                    EventName::RuleActionFailed,
                    ctx.run.as_ref().map(|r| r.id),
                    ctx.flow.as_ref().map(|f| f.id),
                    json!({"rule_id": rule.id, "action": action.kind, "index": i, "error": msg}),
                );
            }
        }
    }
    let _ = state.store.record_firing(
        rule.id,
        Some(event.id),
        ctx.run.as_ref().map(|r| r.id),
        &serde_json::to_string(&outcomes).unwrap_or_else(|_| "[]".into()),
    );
    if let Ok(Some(updated)) = state.store.get_rule(rule.id) {
        let mut rules = state.rules.rules.write().unwrap_or_else(|e| e.into_inner());
        if let Some(slot) = rules.iter_mut().find(|r| r.id == rule.id) {
            slot.fire_count = updated.fire_count;
            slot.last_fired = updated.last_fired;
        }
        state.stream.publish(
            "rule.updated",
            rule.id.to_string(),
            serde_json::to_value(&updated).unwrap_or_default(),
        );
    }
}

fn run_with_timeout(
    state: Arc<AppState>,
    rule: RuleRow,
    action: RuleAction,
    ctx: RunContext,
    event: Event,
) -> Result<Value, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("cereyan-rule-action".into())
        .spawn(move || {
            let result = execute(&state, &rule, &action, &ctx, &event);
            let _ = tx.send(result);
        })
        .map_err(|e| e.to_string())?;
    match rx.recv_timeout(ACTION_TIMEOUT) {
        Ok(r) => r,
        Err(_) => Err(format!(
            "action timed out after {} s",
            ACTION_TIMEOUT.as_secs()
        )),
    }
}

/// Execute one rendered action. Returns a JSON detail on success.
pub fn execute(
    state: &Arc<AppState>,
    rule: &RuleRow,
    action: &RuleAction,
    ctx: &RunContext,
    event: &Event,
) -> Result<Value, String> {
    match action.kind.as_str() {
        "run_flow" => {
            let name = action.flow.clone().unwrap_or_default();
            let project = ctx.flow.as_ref().map(|f| f.project.clone());
            let flow = find_flow(state, &name, project.as_deref())?;
            let options = FlowOptions::from_map(&flow.options);
            let mut params = serde_json::Map::new();
            if let Some(props) = flow
                .parameter_schema
                .get("properties")
                .and_then(|p| p.as_object())
            {
                for (k, prop) in props {
                    if let Some(d) = prop.get("default") {
                        params.insert(k.clone(), d.clone());
                    }
                }
            }
            for (k, v) in &action.parameters {
                params.insert(k.clone(), v.clone());
            }
            crate::validate::validate_parameters(&flow.parameter_schema, &params)?;
            let (run_id, _) = state
                .store
                .create_run_full(CreateRun {
                    flow_id: flow.id,
                    name: format!("{}-rule-{}", flow.name, rule.id),
                    parameters: serde_json::to_string(&params).unwrap_or_else(|_| "{}".into()),
                    tags: serde_json::to_string(&flow.tags).unwrap_or_else(|_| "[]".into()),
                    created_by: format!("rule:{}", rule.id),
                    initial_state: Some(State::new(StateType::Scheduled)),
                    priority: options.priority,
                    ..Default::default()
                })
                .map_err(|e| e.to_string())?;
            let run = state
                .store
                .get_run(run_id)
                .map_err(|e| e.to_string())?
                .ok_or("run vanished")?;
            state
                .index
                .insert_run(&run, EngineKey::from_flow(&flow), false);
            state.run_created(&run);
            crate::dispatch::enqueue_run(state, &run, &flow, None);
            Ok(json!({"run_id": run.id, "name": run.name}))
        }
        "cancel_run" => {
            let run = ctx.run.as_ref().ok_or("event has no run to cancel")?;
            if run.state.is_terminal() {
                return Ok(json!({"run_id": run.id, "already": run.state.name}));
            }
            let immediate = state.supervisor.dequeue(run.id) || run.engine_pid.is_none();
            let next = if immediate {
                State::new(StateType::Cancelled)
                    .with_message(format!("cancelled by rule {}", rule.name))
            } else {
                State::new(StateType::Cancelling)
            };
            match state
                .transition_run(run.id, next, false)
                .map_err(|e| e.to_string())?
            {
                TransitionResult::Accepted(r) => Ok(json!({"run_id": r.id, "state": r.state.name})),
                TransitionResult::Rejected { reason, .. } => {
                    Err(format!("cancel rejected: {reason}"))
                }
            }
        }
        "set_state" => {
            let run = ctx.run.as_ref().ok_or("event has no run")?;
            let t = StateType::parse(action.state_type.as_deref().unwrap_or(""))
                .ok_or_else(|| format!("unknown state type {:?}", action.state_type))?;
            let mut s = State::new(t);
            s.message = action.message.clone();
            match state
                .transition_run(run.id, s, true)
                .map_err(|e| e.to_string())?
            {
                TransitionResult::Accepted(r) => Ok(json!({"run_id": r.id, "state": r.state.name})),
                TransitionResult::Rejected { reason, .. } => {
                    Err(format!("set_state rejected: {reason}"))
                }
            }
        }
        "pause_schedule" | "resume_schedule" => {
            let flow = match &action.flow {
                Some(name) if !name.is_empty() => {
                    find_flow(state, name, ctx.flow.as_ref().map(|f| f.project.as_str()))?
                }
                _ => ctx.flow.clone().ok_or("event has no flow")?,
            };
            let rows = state.scheduler.for_flow(flow.id);
            for r in &rows {
                if action.kind == "pause_schedule" {
                    crate::scheduler::pause(
                        state,
                        r.id,
                        Some(&format!("rule:{}", rule.name)),
                        None,
                    );
                } else {
                    crate::scheduler::resume(state, r.id);
                }
            }
            Ok(json!({"flow": flow.name, "schedules": rows.len()}))
        }
        "webhook" => {
            let url = action.url.clone().unwrap_or_default();
            let method = action
                .method
                .clone()
                .unwrap_or_else(|| "POST".into())
                .to_ascii_uppercase();
            let body = action.body.clone().unwrap_or_default();
            let agent: ureq::Agent = ureq::Agent::config_builder()
                .http_status_as_error(false)
                .timeout_global(Some(Duration::from_secs(5)))
                .build()
                .into();
            let mut last = String::new();
            for attempt in 0..3 {
                if attempt > 0 {
                    std::thread::sleep(Duration::from_millis(200 * (1 << attempt)));
                }
                let mut req = match method.as_str() {
                    "GET" => agent.get(&url).force_send_body(),
                    "PUT" => agent.put(&url),
                    "PATCH" => agent.patch(&url),
                    "DELETE" => agent.delete(&url).force_send_body(),
                    _ => agent.post(&url),
                };
                let mut has_ct = false;
                for (k, v) in &action.headers {
                    let val = v
                        .as_str()
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| v.to_string());
                    if k.eq_ignore_ascii_case("content-type") {
                        has_ct = true;
                    }
                    req = req.header(k.as_str(), val.as_str());
                }
                if !has_ct {
                    req = req.header("content-type", "application/json");
                }
                match req.send(body.as_bytes()) {
                    Ok(resp) => {
                        let status = resp.status().as_u16();
                        if status < 300 {
                            return Ok(json!({"status": status, "attempts": attempt + 1}));
                        }
                        last = format!("status {status}");
                    }
                    Err(e) => last = e.to_string(),
                }
            }
            Err(format!("webhook failed after 3 attempts: {last}"))
        }
        "email" => {
            let cfg = state
                .config
                .email
                .clone()
                .ok_or("no [email] configuration in cereyan.toml")?;
            send_email(
                &cfg,
                &action.to,
                action.subject.as_deref().unwrap_or(""),
                action.body.as_deref().unwrap_or(""),
            )?;
            Ok(json!({"to": action.to, "subject": action.subject}))
        }
        "call" => {
            let callable = action.callable.clone().unwrap_or_default();
            let dispatcher = state
                .rule_dispatcher
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
                .ok_or("no code rule dispatcher registered")?;
            let ev = serde_json::to_value(event).unwrap_or(Value::Null);
            let run = ctx
                .run
                .as_ref()
                .map(|r| serde_json::to_value(r).unwrap_or(Value::Null))
                .unwrap_or(Value::Null);
            dispatcher.call(&callable, &ev, &run)
        }
        other => Err(format!("unknown action kind {other:?}")),
    }
}

/// Dry run of a proactive rule: render against a synthetic lapse built from
/// the most recent arming event (or now, for clock-armed rules).
fn test_proactive_rule(state: &Arc<AppState>, rule: &RuleRow) -> Result<Value, String> {
    let unless = rule.spec.unless.clone().unwrap_or_default();
    let now = now_micros();
    let mut ctx = RunContext {
        run: None,
        flow: None,
    };
    let mut armed_at = now;
    if rule.spec.at.is_none() {
        let page = state
            .store
            .query_events(&cereyan_store::EventFilter {
                limit: Some(200),
                ..Default::default()
            })
            .map_err(|e| e.to_string())?;
        for e in page.items {
            let c = context_for(state, &e);
            if cereyan_rules::matches(&rule.spec.when, &e, &c) {
                armed_at = e.occurred;
                ctx = c;
                break;
            }
        }
    } else if let Some(name) = unless.flows.first() {
        ctx.flow = find_flow(state, name, unless.project.as_deref()).ok();
    }
    let deadline = armed_at + (rule.spec.within.unwrap_or(0.0) * 1e6) as i64;
    let event = Event {
        id: 0,
        seq: 0,
        external_id: cereyan_core::new_id(),
        name: EventName::ExpectationLapsed.as_str().into(),
        occurred: now,
        resource: cereyan_core::Resource {
            kind: "rule".into(),
            id: rule.id.to_string(),
            name: rule.name.clone(),
        },
        related: Vec::new(),
        run_id: ctx.run.as_ref().map(|r| r.id),
        flow_id: ctx.flow.as_ref().map(|f| f.id),
        payload: json!({
            "rule_id": rule.id, "rule": rule.name,
            "flow": ctx.flow.as_ref().map(|f| f.name.clone()),
            "project": ctx.flow.as_ref().map(|f| f.project.clone()),
            "run": ctx.run.as_ref().map(|r| r.id),
            "run_name": ctx.run.as_ref().map(|r| r.name.clone()),
            "expected": unless.events, "deadline": deadline, "armed_at": armed_at,
            "synthetic": true,
        })
        .as_object()
        .cloned()
        .unwrap_or_default(),
    };
    let env = cereyan_rules::environment();
    let template_ctx = cereyan_rules::template_context(&event, &ctx);
    let actions: Vec<Value> = rule
        .spec
        .actions
        .iter()
        .map(
            |a| match cereyan_rules::render_action(&env, a, &template_ctx) {
                Ok(r) => json!({"kind": a.kind, "rendered": r}),
                Err(e) => json!({"kind": a.kind, "error": e.to_string()}),
            },
        )
        .collect();
    Ok(json!({"event": event, "actions": actions, "note": "synthetic lapse; nothing was armed"}))
}

pub(crate) fn find_flow(
    state: &AppState,
    name: &str,
    project: Option<&str>,
) -> Result<Flow, String> {
    let (proj, flow_name) = match name.split_once('/') {
        Some((p, n)) => (Some(p.to_string()), n.to_string()),
        None => (project.map(|p| p.to_string()), name.to_string()),
    };
    if let Some(p) = &proj {
        if let Ok(Some(f)) = state.store.get_flow_by_key(p, &flow_name) {
            return Ok(f);
        }
    }
    let flows = state.store.list_flows(None).map_err(|e| e.to_string())?;
    let matches: Vec<Flow> = flows.into_iter().filter(|f| f.name == flow_name).collect();
    match matches.len() {
        1 => Ok(matches.into_iter().next().unwrap()),
        0 => Err(format!("flow {name:?} is not registered")),
        _ => Err(format!(
            "flow {name:?} exists in several projects; use project/flow"
        )),
    }
}

pub fn send_email(
    cfg: &crate::EmailConfig,
    to: &[String],
    subject: &str,
    body: &str,
) -> Result<(), String> {
    use lettre::message::header::ContentType;
    use lettre::transport::smtp::authentication::Credentials;
    use lettre::{Message, SmtpTransport, Transport};

    let mut builder = Message::builder()
        .from(
            cfg.from
                .parse()
                .map_err(|e| format!("bad from address: {e}"))?,
        )
        .subject(subject)
        .header(ContentType::TEXT_PLAIN);
    for addr in to {
        builder = builder.to(addr
            .parse()
            .map_err(|e| format!("bad recipient {addr}: {e}"))?);
    }
    let message = builder.body(body.to_string()).map_err(|e| e.to_string())?;
    let mut transport = match cfg.tls.as_str() {
        "tls" => SmtpTransport::relay(&cfg.host)
            .map_err(|e| e.to_string())?
            .port(cfg.port),
        "starttls" => SmtpTransport::starttls_relay(&cfg.host)
            .map_err(|e| e.to_string())?
            .port(cfg.port),
        _ => SmtpTransport::builder_dangerous(&cfg.host).port(cfg.port),
    };
    if let (Some(u), Some(p)) = (&cfg.username, &cfg.password) {
        transport = transport.credentials(Credentials::new(u.clone(), p.clone()));
    }
    transport
        .timeout(Some(Duration::from_secs(8)))
        .build()
        .send(&message)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Dry run: render every action of a rule against the latest matching event.
pub fn test_rule(state: &Arc<AppState>, rule: &RuleRow) -> Result<Value, String> {
    if rule.spec.is_proactive() {
        return test_proactive_rule(state, rule);
    }
    let page = state
        .store
        .query_events(&cereyan_store::EventFilter {
            limit: Some(200),
            ..Default::default()
        })
        .map_err(|e| e.to_string())?;
    let mut chosen: Option<(Event, RunContext)> = None;
    for e in page.items {
        let ctx = context_for(state, &e);
        if cereyan_rules::matches(&rule.spec.when, &e, &ctx) {
            chosen = Some((e, ctx));
            break;
        }
    }
    let Some((event, ctx)) = chosen else {
        return Ok(
            json!({"event": Value::Null, "actions": [], "note": "no recent event matches this rule"}),
        );
    };
    let env = cereyan_rules::environment();
    let template_ctx = cereyan_rules::template_context(&event, &ctx);
    let actions: Vec<Value> = rule
        .spec
        .actions
        .iter()
        .map(
            |a| match cereyan_rules::render_action(&env, a, &template_ctx) {
                Ok(r) => json!({"kind": a.kind, "rendered": r}),
                Err(e) => json!({"kind": a.kind, "error": e.to_string()}),
            },
        )
        .collect();
    Ok(json!({"event": event, "actions": actions}))
}
