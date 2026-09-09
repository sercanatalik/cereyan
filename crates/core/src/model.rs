use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::id::Id;
use crate::state::State;
use crate::time::Micros;

/// A registered flow. Identity is `(project, name)`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Flow {
    pub id: i64,
    pub external_id: Id,
    pub project: String,
    pub name: String,
    pub module: String,
    pub source_dir: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// The group the flow is listed under, declared in Python with `group=`.
    /// Null means none was declared and the flow is grouped under its project;
    /// API responses carry the resolved value, so clients never apply that
    /// fallback themselves.
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    #[cfg_attr(feature = "openapi", schema(value_type = Object))]
    pub parameter_schema: Value,
    #[serde(default)]
    #[cfg_attr(feature = "openapi", schema(value_type = Object))]
    pub options: Map<String, Value>,
    #[serde(default)]
    pub error: Option<String>,
    pub created_at: Micros,
    pub last_seen_at: Micros,
    /// Whether the running server has this flow registered from code.
    #[serde(default)]
    pub live: bool,
}

impl Flow {
    /// The group this flow belongs to: the one it declared, else its project.
    /// Resolved on read so a flow with no group of its own follows its project.
    pub fn group_or_project(&self) -> &str {
        self.group.as_deref().unwrap_or(&self.project)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Run {
    pub id: i64,
    pub external_id: Id,
    pub flow_id: i64,
    pub flow_name: String,
    pub project: String,
    /// The flow's group, read through the flow: never stored on the run.
    #[serde(default)]
    pub group: String,
    pub name: String,
    #[serde(default)]
    #[cfg_attr(feature = "openapi", schema(value_type = Object))]
    pub parameters: Map<String, Value>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub state: State,
    pub failure_count: u32,
    pub crash_count: u32,
    pub created_at: Micros,
    #[serde(default)]
    pub start_time: Option<Micros>,
    #[serde(default)]
    pub end_time: Option<Micros>,
    #[serde(default)]
    pub total_run_time: Option<Micros>,
    #[serde(default)]
    pub engine_pid: Option<i64>,
    #[serde(default)]
    pub engine_id: Option<String>,
    #[serde(default)]
    pub created_by: String,
    #[serde(default)]
    pub report_seq: i64,
    #[serde(default)]
    pub schedule_id: Option<i64>,
    #[serde(default)]
    pub scheduled_time: Option<Micros>,
    #[serde(default)]
    pub priority: i64,
    #[serde(default)]
    pub parent_run_id: Option<i64>,
    #[serde(default)]
    pub attempt: i64,
    #[serde(default)]
    pub backfill_id: Option<i64>,
    /// Task runs of this run counted by state type; empty until tasks exist.
    #[serde(default)]
    pub task_counts: std::collections::BTreeMap<String, i64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TaskRun {
    pub id: i64,
    pub external_id: Id,
    pub run_id: i64,
    pub name: String,
    pub task_key: String,
    pub dynamic_key: String,
    pub state: State,
    pub failure_count: u32,
    pub crash_count: u32,
    pub created_at: Micros,
    #[serde(default)]
    pub start_time: Option<Micros>,
    #[serde(default)]
    pub end_time: Option<Micros>,
    #[serde(default)]
    pub total_run_time: Option<Micros>,
    #[serde(default)]
    pub run_name: String,
    #[serde(default)]
    pub flow_id: i64,
    #[serde(default)]
    pub flow_name: String,
    #[serde(default)]
    pub project: String,
    /// External ids of task runs this one waited on (futures and wait_for).
    #[serde(default)]
    pub parents: Vec<Id>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Log {
    pub id: i64,
    pub run_id: i64,
    #[serde(default)]
    pub task_run_id: Option<i64>,
    pub level: i32,
    pub logger: String,
    pub timestamp: Micros,
    pub message: String,
}

/// Placeholders for later phases; defined now so the schema and API types
/// are stable from the start.
/// A stored schedule row: the schedule itself plus policy and bookkeeping.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ScheduleRow {
    pub id: i64,
    pub external_id: Id,
    pub flow_id: i64,
    pub schedule: crate::schedule::Schedule,
    pub catchup: crate::schedule::CatchupPolicy,
    pub catchup_max: i64,
    pub active: bool,
    #[serde(default)]
    pub paused_reason: Option<String>,
    #[serde(default)]
    pub paused_until: Option<Micros>,
    /// `code` for schedules declared on the flow, `ui` for ones created in the
    /// interface, `mcp` for ones created by an agent. Startup reconciliation
    /// singles out `code` alone; every other value is left as it is.
    pub source: String,
    #[serde(default)]
    pub code_key: Option<String>,
    pub persist: bool,
    pub created_at: Micros,
    pub updated_at: Micros,
    /// Next fire time computed by the scheduler (not stored).
    #[serde(default)]
    pub next_fire: Option<Micros>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Backfill {
    pub id: i64,
    pub external_id: Id,
    pub flow_id: i64,
    pub parameter: String,
    pub start_value: String,
    pub end_value: String,
    pub interval_secs: f64,
    pub concurrency: i64,
    pub total: i64,
    #[serde(default)]
    #[cfg_attr(feature = "openapi", schema(value_type = Object))]
    pub extra_parameters: Map<String, Value>,
    pub cancelled: bool,
    pub created_at: Micros,
}

/// Flow-level options declared in Python and stored as JSON on the flow row.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct FlowOptions {
    pub isolated: bool,
    pub log_prints: bool,
    pub priority: i64,
    pub max_concurrent: Option<i64>,
    /// `enqueue`, `skip`, or `cancel_new`.
    pub on_overlap: String,
    #[cfg_attr(feature = "openapi", schema(value_type = Object))]
    pub resources: Map<String, Value>,
    pub crash_retries: Option<i64>,
    pub timeout_seconds: Option<f64>,
    pub retries: i64,
    pub after: Option<AfterSpec>,
    /// (count, window_seconds, persist_seconds)
    pub disable_after: Option<(i64, i64, i64)>,
    pub schedules: Vec<ScheduleDecl>,
    pub has_bulk_complete: bool,
    pub has_crash_hooks: bool,
}

impl FlowOptions {
    pub fn from_map(map: &Map<String, Value>) -> FlowOptions {
        serde_json::from_value(Value::Object(map.clone())).unwrap_or_default()
    }

    pub fn resource_amounts(&self) -> Vec<(String, f64)> {
        self.resources
            .iter()
            .filter_map(|(k, v)| v.as_f64().map(|n| (k.clone(), n)))
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AfterSpec {
    /// The first upstream (kept for 1.0 payloads and the run details link).
    pub flow: String,
    /// Every upstream; empty for rows written before fan-in existed.
    #[serde(default)]
    pub flows: Vec<String>,
    /// Parameter that identifies a batch: the downstream runs once per value.
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    #[cfg_attr(feature = "openapi", schema(value_type = Object))]
    pub parameters: Map<String, Value>,
}

impl AfterSpec {
    /// The effective upstream list.
    pub fn upstreams(&self) -> Vec<String> {
        if self.flows.is_empty() {
            vec![self.flow.clone()]
        } else {
            self.flows.clone()
        }
    }

    pub fn depends_on(&self, flow_name: &str) -> bool {
        self.upstreams().iter().any(|f| f == flow_name)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ScheduleDecl {
    #[serde(flatten)]
    pub schedule: crate::schedule::Schedule,
    #[serde(default)]
    pub catchup: crate::schedule::CatchupPolicy,
    #[serde(default = "default_catchup_max")]
    pub catchup_max: i64,
    #[serde(default)]
    pub key: Option<String>,
}

fn default_catchup_max() -> i64 {
    100
}

/// What an event is about.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Resource {
    /// `run`, `task_run`, `flow`, `schedule`, `rule`, or `custom`.
    pub kind: String,
    pub id: String,
    #[serde(default)]
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Event {
    pub id: i64,
    /// Sequence number: the writer assigns ids in commit order.
    pub seq: i64,
    pub external_id: Id,
    pub name: String,
    pub occurred: Micros,
    pub resource: Resource,
    #[serde(default)]
    pub related: Vec<Resource>,
    #[serde(default)]
    pub run_id: Option<i64>,
    #[serde(default)]
    pub flow_id: Option<i64>,
    #[serde(default)]
    #[cfg_attr(feature = "openapi", schema(value_type = Object))]
    pub payload: Map<String, Value>,
}

/// A stored artifact attached to a run and optionally a task run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ArtifactRow {
    pub id: i64,
    pub external_id: Id,
    pub run_id: i64,
    #[serde(default)]
    pub task_run_id: Option<i64>,
    /// `markdown`, `table`, `progress`, `link`, or `image`.
    pub kind: String,
    #[serde(default)]
    pub key: Option<String>,
    #[cfg_attr(feature = "openapi", schema(value_type = Object))]
    pub data: Value,
    pub created_at: Micros,
    pub updated_at: Micros,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct VariableRow {
    pub name: String,
    /// Plain JSON value, or `"********"` for secrets in API responses.
    #[cfg_attr(feature = "openapi", schema(value_type = Object))]
    pub value: Value,
    #[serde(default)]
    pub tags: Vec<String>,
    pub secret: bool,
    pub created_at: Micros,
    pub updated_at: Micros,
}

/// Match clause of a rule.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct RuleMatch {
    /// Event names, or prefixes ending in `*`.
    pub events: Vec<String>,
    pub flows: Vec<String>,
    pub tags: Vec<String>,
    pub states: Vec<String>,
    pub project: Option<String>,
}

/// One rule action. `kind` selects the variant; other fields are optional.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct RuleAction {
    /// `run_flow`, `cancel_run`, `set_state`, `pause_schedule`, `resume_schedule`, `webhook`, `email`, `call`.
    pub kind: String,
    pub flow: Option<String>,
    #[cfg_attr(feature = "openapi", schema(value_type = Object))]
    pub parameters: Map<String, Value>,
    pub state_type: Option<String>,
    pub message: Option<String>,
    pub url: Option<String>,
    pub method: Option<String>,
    #[cfg_attr(feature = "openapi", schema(value_type = Object))]
    pub headers: Map<String, Value>,
    pub body: Option<String>,
    pub to: Vec<String>,
    pub subject: Option<String>,
    /// Name of the Python callable for `call` actions (code rules).
    pub callable: Option<String>,
}

/// Clock of a clock-armed proactive rule: a cron expression in a timezone.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct RuleClock {
    pub cron: String,
    pub tz: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct RuleSpec {
    #[serde(rename = "when")]
    pub when: RuleMatch,
    #[serde(rename = "do")]
    pub actions: Vec<RuleAction>,
    /// `per_run` or `never`.
    pub once: String,
    pub cooldown_seconds: f64,
    pub max_per_minute: i64,
    pub allow_self: bool,
    /// Proactive rules: the expected event that disarms an expectation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unless: Option<RuleMatch>,
    /// Seconds after the arming event (or look-back before a clock tick).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub within: Option<f64>,
    /// Clock-armed rules: evaluate at each cron tick instead of on an event.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<RuleClock>,
}

impl Default for RuleSpec {
    fn default() -> Self {
        RuleSpec {
            when: RuleMatch::default(),
            actions: Vec::new(),
            once: "per_run".into(),
            cooldown_seconds: 0.0,
            max_per_minute: 60,
            allow_self: false,
            unless: None,
            within: None,
            at: None,
        }
    }
}

impl RuleSpec {
    /// Has an `unless` clause: fires on a missing event, not a present one.
    pub fn is_proactive(&self) -> bool {
        self.unless.is_some()
    }
    /// Evaluated on cron ticks rather than armed by an event.
    pub fn is_clock_armed(&self) -> bool {
        self.unless.is_some() && self.at.is_some()
    }
}

/// An armed expectation of a proactive rule.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Expectation {
    pub id: i64,
    pub rule_id: i64,
    /// `run:<id>` or `flow:<id>`.
    pub key: String,
    #[serde(default)]
    pub run_id: Option<i64>,
    #[serde(default)]
    pub flow_id: Option<i64>,
    pub armed_at: Micros,
    pub deadline: Micros,
    /// `open`, `met`, `lapsed`, or `cancelled`.
    pub status: String,
}

/// One artifact in the cross-run listing, with its run's identity.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ArtifactListItem {
    #[serde(flatten)]
    pub artifact: ArtifactRow,
    pub run_name: String,
    pub flow_name: String,
    pub project: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RuleRow {
    pub id: i64,
    pub external_id: Id,
    pub name: String,
    pub enabled: bool,
    /// `ui` or `code`.
    pub source: String,
    #[serde(default)]
    pub module: Option<String>,
    #[serde(flatten)]
    pub spec: RuleSpec,
    pub fire_count: i64,
    #[serde(default)]
    pub last_fired: Option<Micros>,
    pub created_at: Micros,
    pub updated_at: Micros,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RuleFiring {
    pub id: i64,
    pub rule_id: i64,
    #[serde(default)]
    pub event_id: Option<i64>,
    #[serde(default)]
    pub run_id: Option<i64>,
    pub timestamp: Micros,
    #[cfg_attr(feature = "openapi", schema(value_type = Vec<Object>))]
    pub outcomes: Vec<Value>,
}
