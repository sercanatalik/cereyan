//! The catalogue of engine-emitted event names.
//!
//! One definition for the whole tree: the emit sites in `cereyan-server` name
//! events through [`EventName`], [`check_event_name`] validates user-supplied
//! names in rule match clauses, the Python package exposes the catalogue as
//! `cereyan.events`, and `docs/reference/events.md` is generated from it.
//! Renaming a variant is therefore a compile error at every site that emits it.
//!
//! The catalogue holds *stored* events only. The SSE stream carries messages
//! that share some of these names (`run.updated`, `rule.updated`) and are not
//! events; rules cannot match them, so they are deliberately absent.

use crate::state::{StateName, StateType};

/// Prefixes the engine owns. A name under one of these MUST be a catalogue
/// entry; a name outside them is a custom event and is not checked, which is
/// what keeps `emit_event` open to any vocabulary a pipeline wants.
pub const RESERVED_PREFIXES: [&str; 7] = [
    "run.",
    "task_run.",
    "flow.",
    "schedule.",
    "resource.",
    "rule.",
    "expectation.",
];

/// Every event the engine records.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum EventName {
    RunScheduled,
    RunPending,
    RunRunning,
    RunCompleted,
    RunFailed,
    RunCrashed,
    RunCancelled,
    RunLate,
    RunRetrying,
    RunSkipped,
    RunPaused,
    RunResumed,
    TaskRunRunning,
    TaskRunCompleted,
    TaskRunFailed,
    TaskRunCancelled,
    TaskRunSkipped,
    TaskRunCached,
    FlowRegistered,
    FlowDisabled,
    FlowEnabled,
    FlowFanIn,
    SchedulePaused,
    ScheduleResumed,
    ScheduleCatchup,
    ResourceExhausted,
    RuleFired,
    RuleActionCompleted,
    RuleActionFailed,
    ExpectationArmed,
    ExpectationMet,
    ExpectationLapsed,
}

impl EventName {
    pub const ALL: [EventName; 32] = [
        EventName::RunScheduled,
        EventName::RunPending,
        EventName::RunRunning,
        EventName::RunCompleted,
        EventName::RunFailed,
        EventName::RunCrashed,
        EventName::RunCancelled,
        EventName::RunLate,
        EventName::RunRetrying,
        EventName::RunSkipped,
        EventName::RunPaused,
        EventName::RunResumed,
        EventName::TaskRunRunning,
        EventName::TaskRunCompleted,
        EventName::TaskRunFailed,
        EventName::TaskRunCancelled,
        EventName::TaskRunSkipped,
        EventName::TaskRunCached,
        EventName::FlowRegistered,
        EventName::FlowDisabled,
        EventName::FlowEnabled,
        EventName::FlowFanIn,
        EventName::SchedulePaused,
        EventName::ScheduleResumed,
        EventName::ScheduleCatchup,
        EventName::ResourceExhausted,
        EventName::RuleFired,
        EventName::RuleActionCompleted,
        EventName::RuleActionFailed,
        EventName::ExpectationArmed,
        EventName::ExpectationMet,
        EventName::ExpectationLapsed,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            EventName::RunScheduled => "run.scheduled",
            EventName::RunPending => "run.pending",
            EventName::RunRunning => "run.running",
            EventName::RunCompleted => "run.completed",
            EventName::RunFailed => "run.failed",
            EventName::RunCrashed => "run.crashed",
            EventName::RunCancelled => "run.cancelled",
            EventName::RunLate => "run.late",
            EventName::RunRetrying => "run.retrying",
            EventName::RunSkipped => "run.skipped",
            EventName::RunPaused => "run.paused",
            EventName::RunResumed => "run.resumed",
            EventName::TaskRunRunning => "task_run.running",
            EventName::TaskRunCompleted => "task_run.completed",
            EventName::TaskRunFailed => "task_run.failed",
            EventName::TaskRunCancelled => "task_run.cancelled",
            EventName::TaskRunSkipped => "task_run.skipped",
            EventName::TaskRunCached => "task_run.cached",
            EventName::FlowRegistered => "flow.registered",
            EventName::FlowDisabled => "flow.disabled",
            EventName::FlowEnabled => "flow.enabled",
            EventName::FlowFanIn => "flow.fan_in",
            EventName::SchedulePaused => "schedule.paused",
            EventName::ScheduleResumed => "schedule.resumed",
            EventName::ScheduleCatchup => "schedule.catchup",
            EventName::ResourceExhausted => "resource.exhausted",
            EventName::RuleFired => "rule.fired",
            EventName::RuleActionCompleted => "rule.action.completed",
            EventName::RuleActionFailed => "rule.action.failed",
            EventName::ExpectationArmed => "expectation.armed",
            EventName::ExpectationMet => "expectation.met",
            EventName::ExpectationLapsed => "expectation.lapsed",
        }
    }

    /// The resource kind the event hangs off, which is also how the generated
    /// reference page groups the catalogue.
    pub fn resource(self) -> &'static str {
        match self {
            EventName::RunScheduled
            | EventName::RunPending
            | EventName::RunRunning
            | EventName::RunCompleted
            | EventName::RunFailed
            | EventName::RunCrashed
            | EventName::RunCancelled
            | EventName::RunLate
            | EventName::RunRetrying
            | EventName::RunSkipped
            | EventName::RunPaused
            | EventName::RunResumed => "run",
            EventName::TaskRunRunning
            | EventName::TaskRunCompleted
            | EventName::TaskRunFailed
            | EventName::TaskRunCancelled
            | EventName::TaskRunSkipped
            | EventName::TaskRunCached => "task_run",
            EventName::FlowRegistered
            | EventName::FlowDisabled
            | EventName::FlowEnabled
            | EventName::FlowFanIn => "flow",
            EventName::SchedulePaused | EventName::ScheduleResumed | EventName::ScheduleCatchup => {
                "schedule"
            }
            EventName::ResourceExhausted => "resource",
            EventName::RuleFired
            | EventName::RuleActionCompleted
            | EventName::RuleActionFailed
            | EventName::ExpectationArmed
            | EventName::ExpectationMet
            | EventName::ExpectationLapsed => "rule",
        }
    }

    /// When the event fires, as one sentence. This is the prose the generated
    /// reference page prints, so it lives here rather than in the page.
    pub fn when(self) -> &'static str {
        match self {
            EventName::RunScheduled => "The run is created, or re-enters Scheduled for a crash rerun",
            EventName::RunPending => "An engine accepted the run",
            EventName::RunRunning => "User code started",
            EventName::RunCompleted => "The run finished without error",
            EventName::RunFailed => "The run raised, timed out, or was set failed",
            EventName::RunCrashed => "The engine died while the run was executing",
            EventName::RunCancelled => "The run was cancelled",
            EventName::RunLate => "The scheduled time passed 15 seconds ago and the run has not started",
            EventName::RunRetrying => "A retry attempt started",
            EventName::RunSkipped => "The run ended Skipped: `on_overlap=\"skip\"`, a backfill value already done, or a catch-up drop",
            EventName::RunPaused => "The run is waiting on `wait_for_input`",
            EventName::RunResumed => "The run was answered and its next attempt scheduled",
            EventName::TaskRunRunning => "The task started",
            EventName::TaskRunCompleted => "The task returned",
            EventName::TaskRunFailed => "The task raised or timed out (after its retries)",
            EventName::TaskRunCancelled => "The task was cancelled with its run",
            EventName::TaskRunSkipped => "The task's `output=` target already existed",
            EventName::TaskRunCached => "The task returned a persisted result",
            EventName::FlowRegistered => "The server registered the flow at start or on handoff",
            EventName::FlowDisabled => "`disable_after` tripped; the flow's schedules are paused until `until`",
            EventName::FlowEnabled => "The disable window ended and the schedules resumed",
            EventName::FlowFanIn => "Every upstream completed a run for the key value and the downstream run was created",
            EventName::SchedulePaused => "A schedule was paused from the UI, the API, an MCP tool, a rule, or a disable window",
            EventName::ScheduleResumed => "A schedule was resumed",
            EventName::ScheduleCatchup => "The server started and applied the catch-up policy to fires missed while it was down",
            EventName::ResourceExhausted => "A run waited for a resource that had no capacity; recorded once per wait",
            EventName::RuleFired => "A rule matched an event and its actions started",
            EventName::RuleActionCompleted => "One action finished",
            EventName::RuleActionFailed => "One action failed, including a template that did not render",
            EventName::ExpectationArmed => "A proactive rule's `when` event armed an expectation",
            EventName::ExpectationMet => "The expected event arrived before the deadline",
            EventName::ExpectationLapsed => "The deadline passed, or a clock-armed rule's tick found no matching event; the rule's actions run against this event",
        }
    }

    /// The payload keys the emit site sets, in the order the reference lists them.
    pub fn payload_fields(self) -> &'static [&'static str] {
        match self {
            EventName::RunLate => &[
                "state",
                "state_type",
                "message",
                "flow",
                "project",
                "parameters",
                "created_by",
                "scheduled_time",
                "name",
            ],
            EventName::RunScheduled
            | EventName::RunPending
            | EventName::RunRunning
            | EventName::RunCompleted
            | EventName::RunFailed
            | EventName::RunCrashed
            | EventName::RunCancelled
            | EventName::RunRetrying
            | EventName::RunPaused
            | EventName::RunResumed => &[
                "state",
                "state_type",
                "message",
                "flow",
                "project",
                "parameters",
                "created_by",
            ],
            EventName::RunSkipped => &[
                "state",
                "state_type",
                "message",
                "flow",
                "project",
                "parameters",
                "created_by",
                "reason",
            ],
            EventName::TaskRunRunning
            | EventName::TaskRunCompleted
            | EventName::TaskRunFailed
            | EventName::TaskRunCancelled
            | EventName::TaskRunSkipped
            | EventName::TaskRunCached => {
                &["task", "dynamic_key", "state", "message", "flow", "project"]
            }
            EventName::FlowRegistered => &["flow", "project", "module"],
            EventName::FlowDisabled => &["failures", "window_seconds", "until"],
            EventName::FlowEnabled => &[],
            EventName::FlowFanIn => &["key", "value", "upstream", "run_id"],
            EventName::SchedulePaused => &["schedule_id", "reason"],
            EventName::ScheduleResumed => &["schedule_id"],
            EventName::ScheduleCatchup => {
                &["schedule_id", "policy", "missed", "created", "dropped"]
            }
            EventName::ResourceExhausted => &["resource"],
            EventName::RuleFired => &["rule_id", "rule", "event", "event_id"],
            EventName::RuleActionCompleted => &["rule_id", "action", "index", "detail"],
            EventName::RuleActionFailed => &["rule_id", "action", "index", "error"],
            EventName::ExpectationArmed => &["id", "rule_id", "key", "run_id", "deadline"],
            EventName::ExpectationMet => &["id", "rule_id", "key"],
            EventName::ExpectationLapsed => &[
                "rule_id",
                "rule",
                "flow",
                "project",
                "run",
                "run_name",
                "expected",
                "deadline",
                "armed_at",
                "expectation_id",
            ],
        }
    }

    pub fn parse(text: &str) -> Option<EventName> {
        EventName::ALL.into_iter().find(|e| e.as_str() == text)
    }
}

impl std::fmt::Display for EventName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The reserved prefix `name` falls under, if any.
pub fn reserved_prefix(name: &str) -> Option<&'static str> {
    RESERVED_PREFIXES
        .into_iter()
        .find(|p| name.starts_with(p) || name == p.trim_end_matches('.'))
}

/// A name a rule may not use, with the nearest catalogue entry when there is one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NameError {
    pub value: String,
    pub suggestion: Option<String>,
    message: String,
}

impl NameError {
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for NameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for NameError {}

/// Is this a name a rule may match on?
///
/// A trailing `*` matches a prefix and is always allowed: the wildcard is how a
/// rule says "every run event", and `runs.*` is a custom namespace, not a typo
/// we can prove. A name under a [reserved prefix](RESERVED_PREFIXES) must be a
/// catalogue entry, because nothing else will ever emit it. Everything else is
/// a custom event and passes untouched.
pub fn check_event_name(name: &str) -> Result<(), NameError> {
    if name.ends_with('*') {
        return Ok(());
    }
    let Some(prefix) = reserved_prefix(name) else {
        return Ok(());
    };
    if EventName::parse(name).is_some() {
        return Ok(());
    }
    let known: Vec<&str> = EventName::ALL
        .iter()
        .filter(|e| e.as_str().starts_with(prefix))
        .map(|e| e.as_str())
        .collect();
    let suggestion = nearest(name, &known);
    let message = match &suggestion {
        Some(s) => format!(
            "unknown event name {name:?}: {prefix:?} is a reserved prefix and nothing emits {name:?}; did you mean {s:?}?"
        ),
        None => format!(
            "unknown event name {name:?}: {prefix:?} is a reserved prefix and nothing emits {name:?}; known names are {}, or use \"{prefix}*\" to match them all",
            known.join(", ")
        ),
    };
    Err(NameError {
        value: name.to_string(),
        suggestion,
        message,
    })
}

/// Is this a name a rule's `states` may hold? Either a state type or a named
/// sub-state; a rule matching a type also matches that type's sub-states.
pub fn check_state_name(name: &str) -> Result<(), NameError> {
    if StateType::parse(name).is_some() || StateName::parse(name).is_some() {
        return Ok(());
    }
    let known: Vec<&str> = StateType::ALL
        .iter()
        .map(|t| t.as_str())
        .chain(StateName::ALL.iter().map(|n| n.as_str()))
        .collect();
    let suggestion = nearest(name, &known);
    let message = match &suggestion {
        Some(s) => format!("unknown state {name:?}; did you mean {s:?}?"),
        None => format!(
            "unknown state {name:?}; expected a state type ({}) or a sub-state name ({})",
            StateType::ALL
                .iter()
                .map(|t| t.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            StateName::ALL
                .iter()
                .map(|n| n.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        ),
    };
    Err(NameError {
        value: name.to_string(),
        suggestion,
        message,
    })
}

/// The closest candidate within a third of the name's length, case-insensitively.
/// The ratio keeps `run.failure` -> `run.failed` while refusing to guess for a
/// name that simply is not in the catalogue.
fn nearest(name: &str, candidates: &[&str]) -> Option<String> {
    let lowered = name.to_ascii_lowercase();
    let budget = (name.chars().count() / 3).max(1);
    candidates
        .iter()
        .map(|c| (levenshtein(&lowered, &c.to_ascii_lowercase()), *c))
        .filter(|(d, _)| *d <= budget)
        .min_by_key(|(d, c)| (*d, c.len()))
        .map(|(_, c)| c.to_string())
}

fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, ca) in a.chars().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != *cb);
            cur[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique_and_prefixed() {
        let mut seen = std::collections::HashSet::new();
        for e in EventName::ALL {
            assert!(seen.insert(e.as_str()), "duplicate name {}", e.as_str());
            assert!(
                reserved_prefix(e.as_str()).is_some(),
                "{} is not under a reserved prefix",
                e.as_str()
            );
            assert_eq!(EventName::parse(e.as_str()), Some(e));
        }
    }

    /// The catalogue is the `events` capability's list; this test is what makes
    /// adding an event to the spec and not to the code fail.
    #[test]
    fn catalogue_matches_the_spec() {
        let expected = [
            "run.scheduled",
            "run.pending",
            "run.running",
            "run.completed",
            "run.failed",
            "run.crashed",
            "run.cancelled",
            "run.late",
            "run.retrying",
            "run.skipped",
            "run.paused",
            "run.resumed",
            "task_run.running",
            "task_run.completed",
            "task_run.failed",
            "task_run.cancelled",
            "task_run.skipped",
            "task_run.cached",
            "flow.registered",
            "flow.disabled",
            "flow.enabled",
            "flow.fan_in",
            "schedule.paused",
            "schedule.resumed",
            "schedule.catchup",
            "resource.exhausted",
            "rule.fired",
            "rule.action.completed",
            "rule.action.failed",
            "expectation.armed",
            "expectation.met",
            "expectation.lapsed",
        ];
        let actual: Vec<&str> = EventName::ALL.iter().map(|e| e.as_str()).collect();
        assert_eq!(actual, expected);
    }

    #[test]
    fn every_event_has_prose_and_a_resource() {
        for e in EventName::ALL {
            assert!(!e.when().is_empty(), "{e} has no prose");
            assert!(
                e.as_str().starts_with(&format!("{}.", e.resource())) || e.resource() == "rule",
                "{e} is grouped under {}",
                e.resource()
            );
        }
    }

    #[test]
    fn known_names_pass() {
        for e in EventName::ALL {
            assert!(check_event_name(e.as_str()).is_ok(), "{e} rejected");
        }
    }

    #[test]
    fn wildcards_pass() {
        for name in ["run.*", "task_run.*", "runs.*", "*"] {
            assert!(check_event_name(name).is_ok(), "{name} rejected");
        }
    }

    #[test]
    fn custom_names_pass() {
        for name in ["orders.table_empty", "vendor.x.y", "runs.failed", "ping"] {
            assert!(check_event_name(name).is_ok(), "{name} rejected");
        }
    }

    #[test]
    fn typo_under_a_reserved_prefix_is_rejected_with_a_suggestion() {
        let err = check_event_name("run.failure").unwrap_err();
        assert_eq!(err.suggestion.as_deref(), Some("run.failed"));
        assert!(err.message().contains("run.failure"));
        assert!(err.message().contains("run.failed"));

        let err = check_event_name("task_run.complete").unwrap_err();
        assert_eq!(err.suggestion.as_deref(), Some("task_run.completed"));
    }

    #[test]
    fn unknown_name_under_a_reserved_prefix_lists_the_alternatives() {
        let err = check_event_name("run.quiesced").unwrap_err();
        assert_eq!(err.suggestion, None);
        assert!(err.message().contains("run.*"));
        assert!(err.message().contains("run.completed"));
    }

    /// `run.updated` is an SSE message, not an event. A rule naming it would
    /// never fire, so the catalogue has to reject it rather than shrug.
    #[test]
    fn stream_message_names_are_not_events() {
        for name in [
            "run.updated",
            "task_run.updated",
            "rule.updated",
            "schedule.updated",
        ] {
            assert!(check_event_name(name).is_err(), "{name} accepted");
        }
    }

    #[test]
    fn state_names_pass() {
        for t in StateType::ALL {
            assert!(check_state_name(t.as_str()).is_ok());
        }
        for n in StateName::ALL {
            assert!(check_state_name(n.as_str()).is_ok());
        }
    }

    #[test]
    fn misspelled_state_is_rejected_with_a_suggestion() {
        let err = check_state_name("Faild").unwrap_err();
        assert_eq!(err.suggestion.as_deref(), Some("Failed"));
        let err = check_state_name("Cancelled ").unwrap_err();
        assert_eq!(err.suggestion.as_deref(), Some("Cancelled"));
    }

    #[test]
    fn unrelated_state_lists_the_alternatives() {
        let err = check_state_name("Zzzzzzzz").unwrap_err();
        assert_eq!(err.suggestion, None);
        assert!(err.message().contains("Scheduled"));
        assert!(err.message().contains("AwaitingRetry"));
    }

    #[test]
    fn levenshtein_basics() {
        assert_eq!(levenshtein("", ""), 0);
        assert_eq!(levenshtein("abc", "abc"), 0);
        assert_eq!(levenshtein("abc", "abd"), 1);
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("kitten", "sitting"), 3);
    }
}
