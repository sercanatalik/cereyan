//! Rule matching, guards, and sandboxed templating. No I/O: the server and
//! the offline path decide how to execute the rendered actions.

use std::collections::HashMap;

use cereyan_core::{Event, Flow, RuleAction, RuleMatch, RuleRow, RuleSpec, Run};
use minijinja::{Environment, UndefinedBehavior};
use serde_json::{json, Map, Value};

#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error("template error in {field}: {message}")]
    Template { field: String, message: String },
}

/// Does `pattern` (a name or a `prefix*`) match `name`?
pub fn name_matches(pattern: &str, name: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => name.starts_with(prefix),
        None => pattern == name,
    }
}

/// Everything the matcher needs to know about the event's run, if any.
#[derive(Clone, Debug, Default)]
pub struct RunContext {
    pub run: Option<Run>,
    pub flow: Option<Flow>,
}

/// Does the rule's match clause accept this event?
pub fn matches(m: &RuleMatch, event: &Event, ctx: &RunContext) -> bool {
    if !m.events.is_empty() && !m.events.iter().any(|p| name_matches(p, &event.name)) {
        return false;
    }
    let flow_name = ctx
        .flow
        .as_ref()
        .map(|f| f.name.as_str())
        .or_else(|| ctx.run.as_ref().map(|r| r.flow_name.as_str()))
        .or_else(|| {
            event
                .related
                .iter()
                .find(|r| r.kind == "flow")
                .map(|r| r.name.as_str())
        });
    if !m.flows.is_empty() {
        match flow_name {
            Some(name) if m.flows.iter().any(|f| f == name) => {}
            _ => return false,
        }
    }
    if let Some(project) = &m.project {
        let event_project = ctx
            .flow
            .as_ref()
            .map(|f| f.project.clone())
            .or_else(|| ctx.run.as_ref().map(|r| r.project.clone()))
            .or_else(|| {
                event
                    .payload
                    .get("project")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            });
        if event_project.as_deref() != Some(project.as_str()) {
            return false;
        }
    }
    if !m.tags.is_empty() {
        let tags: Vec<String> = ctx.run.as_ref().map(|r| r.tags.clone()).unwrap_or_else(|| {
            event
                .related
                .iter()
                .filter(|r| r.kind == "tag")
                .map(|r| r.name.clone())
                .collect()
        });
        if !m.tags.iter().any(|t| tags.contains(t)) {
            return false;
        }
    }
    if !m.states.is_empty() {
        let state = ctx.run.as_ref().map(|r| r.state.name.clone()).or_else(|| {
            event
                .payload
                .get("state")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        });
        match state {
            Some(s) if m.states.iter().any(|x| x.eq_ignore_ascii_case(&s)) => {}
            _ => return false,
        }
    }
    true
}

/// Guard state kept per rule between firings.
#[derive(Clone, Debug, Default)]
pub struct GuardState {
    pub last_fired: Option<i64>,
    /// Firing timestamps within the last minute (microseconds).
    pub recent: Vec<i64>,
    /// Runs this rule already fired for (once = per_run).
    pub fired_runs: Vec<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum GuardDecision {
    Fire,
    Skip(&'static str),
}

/// Apply the guards: enabled, once-per-run, cooldown, rate cap, self-trigger.
pub fn check_guards(
    rule: &RuleRow,
    event: &Event,
    ctx: &RunContext,
    state: &GuardState,
    now: i64,
) -> GuardDecision {
    if !rule.enabled {
        return GuardDecision::Skip("disabled");
    }
    if let Some(run) = &ctx.run {
        if !rule.spec.allow_self && run.created_by == format!("rule:{}", rule.id) {
            return GuardDecision::Skip("self-trigger");
        }
        if rule.spec.once == "per_run" && state.fired_runs.contains(&run.id) {
            return GuardDecision::Skip("once per run");
        }
    }
    if rule.spec.cooldown_seconds > 0.0 {
        if let Some(last) = state.last_fired {
            if (now - last) as f64 / 1e6 < rule.spec.cooldown_seconds {
                return GuardDecision::Skip("cooldown");
            }
        }
    }
    let minute_ago = now - 60_000_000;
    let recent = state.recent.iter().filter(|t| **t > minute_ago).count() as i64;
    if rule.spec.max_per_minute > 0 && recent >= rule.spec.max_per_minute {
        return GuardDecision::Skip("rate cap");
    }
    let _ = event;
    GuardDecision::Fire
}

impl GuardState {
    pub fn record(&mut self, run_id: Option<i64>, now: i64) {
        self.last_fired = Some(now);
        self.recent.push(now);
        let minute_ago = now - 60_000_000;
        self.recent.retain(|t| *t > minute_ago);
        if let Some(r) = run_id {
            if !self.fired_runs.contains(&r) {
                self.fired_runs.push(r);
                if self.fired_runs.len() > 10_000 {
                    self.fired_runs.drain(..5_000);
                }
            }
        }
    }
}

/// A sandboxed template environment: no loaders, strict undefined.
pub fn environment() -> Environment<'static> {
    let mut env = Environment::new();
    env.set_undefined_behavior(UndefinedBehavior::Strict);
    env
}

/// The fixed template context.
pub fn template_context(event: &Event, ctx: &RunContext) -> Value {
    let run = ctx
        .run
        .as_ref()
        .map(|r| serde_json::to_value(r).unwrap_or(Value::Null))
        .unwrap_or(Value::Null);
    let flow = ctx
        .flow
        .as_ref()
        .map(|f| serde_json::to_value(f).unwrap_or(Value::Null))
        .unwrap_or(Value::Null);
    let state = ctx
        .run
        .as_ref()
        .map(|r| serde_json::to_value(&r.state).unwrap_or(Value::Null))
        .unwrap_or(Value::Null);
    let parameters = ctx
        .run
        .as_ref()
        .map(|r| Value::Object(r.parameters.clone()))
        .unwrap_or(Value::Object(Map::new()));
    json!({
        "event": serde_json::to_value(event).unwrap_or(Value::Null),
        "run": run,
        "flow": flow,
        "state": state,
        "payload": Value::Object(event.payload.clone()),
        "parameters": parameters,
    })
}

pub fn render(
    env: &Environment<'_>,
    field: &str,
    template: &str,
    context: &Value,
) -> Result<String, RenderError> {
    if !template.contains("{{") && !template.contains("{%") {
        return Ok(template.to_string());
    }
    env.render_str(template, context)
        .map_err(|e| RenderError::Template {
            field: field.to_string(),
            message: e.to_string(),
        })
}

fn render_value(
    env: &Environment<'_>,
    field: &str,
    value: &Value,
    context: &Value,
) -> Result<Value, RenderError> {
    match value {
        Value::String(s) => {
            let rendered = render(env, field, s, context)?;
            Ok(Value::String(rendered))
        }
        Value::Object(map) => {
            let mut out = Map::new();
            for (k, v) in map {
                out.insert(
                    k.clone(),
                    render_value(env, &format!("{field}.{k}"), v, context)?,
                );
            }
            Ok(Value::Object(out))
        }
        Value::Array(items) => Ok(Value::Array(
            items
                .iter()
                .enumerate()
                .map(|(i, v)| render_value(env, &format!("{field}[{i}]"), v, context))
                .collect::<Result<Vec<_>, _>>()?,
        )),
        other => Ok(other.clone()),
    }
}

/// The default failure email body, in the spirit of Luigi's failure email.
pub const DEFAULT_EMAIL_BODY: &str =
    "Run {{ run.name }} of flow {{ run.project }}/{{ run.flow_name }} ended {{ state.name }}.\n\n\
Parameters:\n{% for k, v in parameters | dictsort %}  {{ k }} = {{ v }}\n{% endfor %}\n\
Message: {{ state.message }}\n\n{{ state.details.traceback | default('') }}";

pub const DEFAULT_EMAIL_SUBJECT: &str =
    "[cereyan] {{ run.flow_name }} {{ state.name }}: {{ run.name }}";

/// Render every template field of an action against the context.
pub fn render_action(
    env: &Environment<'_>,
    action: &RuleAction,
    context: &Value,
) -> Result<RuleAction, RenderError> {
    let mut out = action.clone();
    if let Value::Object(map) = render_value(
        env,
        "parameters",
        &Value::Object(action.parameters.clone()),
        context,
    )? {
        out.parameters = map;
    }
    if let Some(m) = &action.message {
        out.message = Some(render(env, "message", m, context)?);
    }
    if let Some(u) = &action.url {
        out.url = Some(render(env, "url", u, context)?);
    }
    if let Value::Object(map) = render_value(
        env,
        "headers",
        &Value::Object(action.headers.clone()),
        context,
    )? {
        out.headers = map;
    }
    if action.kind == "webhook" {
        let body = action
            .body
            .clone()
            .unwrap_or_else(|| "{{ event | tojson }}".into());
        out.body = Some(render(env, "body", &body, context)?);
    } else if action.kind == "email" {
        let subject = action
            .subject
            .clone()
            .unwrap_or_else(|| DEFAULT_EMAIL_SUBJECT.into());
        let body = action
            .body
            .clone()
            .unwrap_or_else(|| DEFAULT_EMAIL_BODY.into());
        out.subject = Some(render(env, "subject", &subject, context)?);
        out.body = Some(render(env, "body", &body, context)?);
    } else if let Some(b) = &action.body {
        out.body = Some(render(env, "body", b, context)?);
    }
    Ok(out)
}

/// An index of rules by the first segment of their event patterns, so an
/// event only visits rules that can match it.
#[derive(Default, Debug)]
pub struct RuleIndex {
    by_prefix: HashMap<String, Vec<i64>>,
    all: Vec<i64>,
}

impl RuleIndex {
    pub fn build(rules: &[RuleRow]) -> RuleIndex {
        let mut idx = RuleIndex::default();
        for r in rules {
            // Clock-armed rules never match events by `when`; index their
            // `unless` events so the disarm path sees them. Event-armed
            // proactive rules index both clauses.
            if let Some(unless) = &r.spec.unless {
                for p in &unless.events {
                    let key = p
                        .split('.')
                        .next()
                        .unwrap_or("")
                        .trim_end_matches('*')
                        .to_string();
                    if key.is_empty() {
                        idx.all.push(r.id);
                    } else {
                        idx.by_prefix.entry(key).or_default().push(r.id);
                    }
                }
                if r.spec.at.is_some() {
                    continue;
                }
            }
            if r.spec.when.events.is_empty() {
                idx.all.push(r.id);
                continue;
            }
            for p in &r.spec.when.events {
                let key = p
                    .split('.')
                    .next()
                    .unwrap_or("")
                    .trim_end_matches('*')
                    .to_string();
                if key.is_empty() {
                    idx.all.push(r.id);
                } else {
                    idx.by_prefix.entry(key).or_default().push(r.id);
                }
            }
        }
        idx
    }

    pub fn candidates(&self, event_name: &str) -> Vec<i64> {
        let key = event_name.split('.').next().unwrap_or("");
        let mut out = self.all.clone();
        if let Some(v) = self.by_prefix.get(key) {
            out.extend(v.iter().copied());
        }
        out.sort_unstable();
        out.dedup();
        out
    }
}

/// Validate a spec before storing it.
pub fn validate_spec(spec: &RuleSpec) -> Result<(), String> {
    if spec.actions.is_empty() {
        return Err("a rule needs at least one action".into());
    }
    if !matches!(spec.once.as_str(), "per_run" | "never") {
        return Err("once must be per_run or never".into());
    }
    if let Some(unless) = &spec.unless {
        if unless.events.is_empty() {
            return Err("unless needs at least one expected event".into());
        }
        match (&spec.at, spec.within) {
            (Some(clock), within) => {
                if !spec.when.events.is_empty() {
                    return Err("a clock-armed rule (at) cannot also have when events".into());
                }
                if let Some(w) = within {
                    if w <= 0.0 {
                        return Err("within must be positive".into());
                    }
                }
                let schedule = cereyan_core::Schedule::Cron {
                    cron: clock.cron.clone(),
                    timezone: clock.tz.clone(),
                    day_or: true,
                };
                schedule.validate().map_err(|e| format!("at: {e}"))?;
            }
            (None, Some(w)) => {
                if w <= 0.0 {
                    return Err("within must be positive".into());
                }
                if spec.when.events.is_empty() {
                    return Err("unless needs when events to arm on, or an at clock".into());
                }
            }
            (None, None) => {
                return Err("unless needs within (event-armed) or at (clock-armed)".into());
            }
        }
    } else if spec.at.is_some() {
        return Err("at is only valid together with unless".into());
    }
    for (i, a) in spec.actions.iter().enumerate() {
        match a.kind.as_str() {
            "run_flow" => {
                if a.flow.as_deref().unwrap_or("").is_empty() {
                    return Err(format!("action {i}: run_flow needs a flow"));
                }
            }
            "cancel_run" | "pause_schedule" | "resume_schedule" => {}
            "set_state" => {
                if a.state_type.as_deref().unwrap_or("").is_empty() {
                    return Err(format!("action {i}: set_state needs a state_type"));
                }
            }
            "webhook" => {
                if a.url.as_deref().unwrap_or("").is_empty() {
                    return Err(format!("action {i}: webhook needs a url"));
                }
            }
            "email" => {
                if a.to.is_empty() {
                    return Err(format!("action {i}: email needs recipients"));
                }
            }
            "call" => {
                if a.callable.as_deref().unwrap_or("").is_empty() {
                    return Err(format!("action {i}: call needs a callable"));
                }
            }
            other => return Err(format!("action {i}: unknown kind {other:?}")),
        }
    }
    let env = environment();
    for (i, a) in spec.actions.iter().enumerate() {
        for (field, text) in [
            ("body", &a.body),
            ("subject", &a.subject),
            ("message", &a.message),
            ("url", &a.url),
        ] {
            if let Some(t) = text {
                if let Err(e) = env.template_from_str(t) {
                    return Err(format!("action {i}: {field} template: {e}"));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cereyan_core::{new_id, Resource, State, StateType};

    fn event(name: &str) -> Event {
        Event {
            id: 1,
            seq: 1,
            external_id: new_id(),
            name: name.into(),
            occurred: 0,
            resource: Resource {
                kind: "run".into(),
                id: "x".into(),
                name: "run-1".into(),
            },
            related: vec![Resource {
                kind: "flow".into(),
                id: "p/etl".into(),
                name: "etl".into(),
            }],
            run_id: Some(1),
            flow_id: Some(1),
            payload: Map::new(),
        }
    }

    fn run(created_by: &str) -> Run {
        Run {
            id: 1,
            external_id: new_id(),
            flow_id: 1,
            flow_name: "etl".into(),
            project: "p".into(),
            name: "run-1".into(),
            parameters: serde_json::from_value(json!({"day": "2026-09-06"})).unwrap(),
            tags: vec!["prod".into()],
            state: State::new(StateType::Failed).with_message("boom"),
            failure_count: 1,
            crash_count: 0,
            created_at: 0,
            start_time: None,
            end_time: None,
            total_run_time: None,
            engine_pid: None,
            engine_id: None,
            created_by: created_by.into(),
            report_seq: 0,
            schedule_id: None,
            scheduled_time: None,
            priority: 0,
            parent_run_id: None,
            attempt: 0,
            backfill_id: None,
            task_counts: Default::default(),
        }
    }

    fn rule(spec: RuleSpec) -> RuleRow {
        RuleRow {
            id: 7,
            external_id: new_id(),
            name: "r".into(),
            enabled: true,
            source: "ui".into(),
            module: None,
            spec,
            fire_count: 0,
            last_fired: None,
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn wildcard_and_flow_match() {
        let m = RuleMatch {
            events: vec!["run.*".into()],
            flows: vec!["etl".into()],
            ..Default::default()
        };
        let ctx = RunContext {
            run: Some(run("api")),
            flow: None,
        };
        assert!(matches(&m, &event("run.failed"), &ctx));
        assert!(!matches(&m, &event("task_run.failed"), &ctx));
        let other = RuleMatch {
            events: vec!["run.failed".into()],
            flows: vec!["other".into()],
            ..Default::default()
        };
        assert!(!matches(&other, &event("run.failed"), &ctx));
        let tagged = RuleMatch {
            tags: vec!["prod".into()],
            states: vec!["Failed".into()],
            ..Default::default()
        };
        assert!(matches(&tagged, &event("run.failed"), &ctx));
        let project = RuleMatch {
            project: Some("q".into()),
            ..Default::default()
        };
        assert!(!matches(&project, &event("run.failed"), &ctx));
    }

    #[test]
    fn guards() {
        let spec = RuleSpec {
            cooldown_seconds: 10.0,
            max_per_minute: 2,
            ..Default::default()
        };
        let r = rule(spec);
        let mut st = GuardState::default();
        let ctx = RunContext {
            run: Some(run("api")),
            flow: None,
        };
        assert_eq!(
            check_guards(&r, &event("run.failed"), &ctx, &st, 0),
            GuardDecision::Fire
        );
        st.record(Some(1), 0);
        assert_eq!(
            check_guards(&r, &event("run.failed"), &ctx, &st, 1_000_000),
            GuardDecision::Skip("once per run")
        );
        let ctx2 = RunContext {
            run: Some(Run {
                id: 2,
                ..run("api")
            }),
            flow: None,
        };
        assert_eq!(
            check_guards(&r, &event("run.failed"), &ctx2, &st, 1_000_000),
            GuardDecision::Skip("cooldown")
        );
        assert_eq!(
            check_guards(&r, &event("run.failed"), &ctx2, &st, 11_000_000),
            GuardDecision::Fire
        );
        st.record(Some(2), 11_000_000);
        let ctx3 = RunContext {
            run: Some(Run {
                id: 3,
                ..run("api")
            }),
            flow: None,
        };
        assert_eq!(
            check_guards(&r, &event("run.failed"), &ctx3, &st, 22_000_000),
            GuardDecision::Skip("rate cap")
        );
        let self_ctx = RunContext {
            run: Some(Run {
                id: 9,
                ..run("rule:7")
            }),
            flow: None,
        };
        assert_eq!(
            check_guards(
                &r,
                &event("run.completed"),
                &self_ctx,
                &GuardState::default(),
                0
            ),
            GuardDecision::Skip("self-trigger")
        );
        let mut disabled = rule(RuleSpec::default());
        disabled.enabled = false;
        assert_eq!(
            check_guards(
                &disabled,
                &event("run.failed"),
                &ctx,
                &GuardState::default(),
                0
            ),
            GuardDecision::Skip("disabled")
        );
    }

    #[test]
    fn templates_render_and_fail_strictly() {
        let env = environment();
        let ctx = RunContext {
            run: Some(run("api")),
            flow: None,
        };
        let context = template_context(&event("run.failed"), &ctx);
        let action = RuleAction {
            kind: "run_flow".into(),
            flow: Some("cleanup".into()),
            parameters: serde_json::from_value(json!({"day": "{{ run.parameters.day }}"})).unwrap(),
            ..Default::default()
        };
        let rendered = render_action(&env, &action, &context).unwrap();
        assert_eq!(rendered.parameters.get("day").unwrap(), "2026-09-06");
        let email = RuleAction {
            kind: "email".into(),
            to: vec!["a@b".into()],
            ..Default::default()
        };
        let rendered = render_action(&env, &email, &context).unwrap();
        assert!(rendered.subject.unwrap().contains("etl Failed"));
        assert!(rendered.body.unwrap().contains("day = 2026-09-06"));
        let bad = RuleAction {
            kind: "webhook".into(),
            url: Some("http://x".into()),
            body: Some("{{ nope.missing }}".into()),
            ..Default::default()
        };
        assert!(render_action(&env, &bad, &context).is_err());
    }

    #[test]
    fn index_and_validation() {
        let rules = vec![
            rule(RuleSpec {
                when: RuleMatch {
                    events: vec!["run.*".into()],
                    ..Default::default()
                },
                actions: vec![RuleAction {
                    kind: "cancel_run".into(),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            RuleRow {
                id: 8,
                ..rule(RuleSpec {
                    when: RuleMatch {
                        events: vec!["task_run.failed".into()],
                        ..Default::default()
                    },
                    ..Default::default()
                })
            },
        ];
        let idx = RuleIndex::build(&rules);
        assert_eq!(idx.candidates("run.failed"), vec![7]);
        assert_eq!(idx.candidates("task_run.failed"), vec![8]);
        assert!(validate_spec(&RuleSpec::default()).is_err());
        assert!(validate_spec(&RuleSpec {
            actions: vec![RuleAction {
                kind: "webhook".into(),
                ..Default::default()
            }],
            ..Default::default()
        })
        .is_err());
        assert!(validate_spec(&RuleSpec {
            actions: vec![RuleAction {
                kind: "webhook".into(),
                url: Some("http://x".into()),
                body: Some("{% if %}".into()),
                ..Default::default()
            }],
            ..Default::default()
        })
        .is_err());
    }
}
