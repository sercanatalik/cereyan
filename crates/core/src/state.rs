use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::time::{now_micros, Micros};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum StateType {
    Scheduled,
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
    Crashed,
    Paused,
    Cancelling,
}

impl StateType {
    pub const ALL: [StateType; 9] = [
        StateType::Scheduled,
        StateType::Pending,
        StateType::Running,
        StateType::Completed,
        StateType::Failed,
        StateType::Cancelled,
        StateType::Crashed,
        StateType::Paused,
        StateType::Cancelling,
    ];

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            StateType::Completed | StateType::Failed | StateType::Cancelled | StateType::Crashed
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            StateType::Scheduled => "Scheduled",
            StateType::Pending => "Pending",
            StateType::Running => "Running",
            StateType::Completed => "Completed",
            StateType::Failed => "Failed",
            StateType::Cancelled => "Cancelled",
            StateType::Crashed => "Crashed",
            StateType::Paused => "Paused",
            StateType::Cancelling => "Cancelling",
        }
    }

    pub fn parse(text: &str) -> Option<StateType> {
        StateType::ALL.into_iter().find(|t| t.as_str() == text)
    }
}

impl std::fmt::Display for StateType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Named sub-states with a fixed type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StateName {
    Late,
    AwaitingRetry,
    AwaitingResource,
    Retrying,
    TimedOut,
    Cached,
    Skipped,
}

impl StateName {
    pub const ALL: [StateName; 7] = [
        StateName::Late,
        StateName::AwaitingRetry,
        StateName::AwaitingResource,
        StateName::Retrying,
        StateName::TimedOut,
        StateName::Cached,
        StateName::Skipped,
    ];

    pub fn state_type(self) -> StateType {
        match self {
            StateName::Late | StateName::AwaitingRetry | StateName::AwaitingResource => {
                StateType::Scheduled
            }
            StateName::Retrying => StateType::Running,
            StateName::TimedOut => StateType::Failed,
            StateName::Cached | StateName::Skipped => StateType::Completed,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            StateName::Late => "Late",
            StateName::AwaitingRetry => "AwaitingRetry",
            StateName::AwaitingResource => "AwaitingResource",
            StateName::Retrying => "Retrying",
            StateName::TimedOut => "TimedOut",
            StateName::Cached => "Cached",
            StateName::Skipped => "Skipped",
        }
    }

    pub fn parse(text: &str) -> Option<StateName> {
        StateName::ALL.into_iter().find(|n| n.as_str() == text)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct State {
    #[serde(rename = "type")]
    pub state_type: StateType,
    pub name: String,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    #[cfg_attr(feature = "openapi", schema(value_type = Object))]
    pub details: Map<String, Value>,
    pub timestamp: Micros,
}

impl State {
    /// A state whose name equals its type.
    pub fn new(state_type: StateType) -> State {
        State {
            state_type,
            name: state_type.as_str().to_string(),
            message: None,
            details: Map::new(),
            timestamp: now_micros(),
        }
    }

    /// A named sub-state; the type is fixed by the name.
    pub fn named(name: StateName) -> State {
        let mut s = State::new(name.state_type());
        s.name = name.as_str().to_string();
        s
    }

    /// Build a state from strings as they arrive from Python or the API.
    /// An empty or missing name defaults to the type name. A recognised
    /// sub-state name forces its type.
    pub fn from_parts(
        state_type: StateType,
        name: Option<&str>,
        message: Option<String>,
        details: Map<String, Value>,
    ) -> State {
        let (state_type, name) = match name.filter(|n| !n.is_empty()) {
            None => (state_type, state_type.as_str().to_string()),
            Some(n) => match StateName::parse(n) {
                Some(sub) => (sub.state_type(), sub.as_str().to_string()),
                None => (state_type, n.to_string()),
            },
        };
        State {
            state_type,
            name,
            message,
            details,
            timestamp: now_micros(),
        }
    }

    pub fn with_message(mut self, message: impl Into<String>) -> State {
        self.message = Some(message.into());
        self
    }

    pub fn with_timestamp(mut self, ts: Micros) -> State {
        self.timestamp = ts;
        self
    }

    pub fn is_terminal(&self) -> bool {
        self.state_type.is_terminal()
    }

    pub fn is_completed(&self) -> bool {
        self.state_type == StateType::Completed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_name_equals_type() {
        assert_eq!(State::new(StateType::Running).name, "Running");
    }

    #[test]
    fn sub_states_have_fixed_types() {
        assert_eq!(StateName::Skipped.state_type(), StateType::Completed);
        assert_eq!(StateName::TimedOut.state_type(), StateType::Failed);
        assert_eq!(StateName::Retrying.state_type(), StateType::Running);
        assert_eq!(StateName::Late.state_type(), StateType::Scheduled);
    }

    #[test]
    fn skipped_counts_as_completed() {
        let s = State::named(StateName::Skipped);
        assert!(s.is_completed());
        assert!(s.is_terminal());
    }

    #[test]
    fn from_parts_forces_type_of_named_state() {
        let s = State::from_parts(StateType::Running, Some("Skipped"), None, Map::new());
        assert_eq!(s.state_type, StateType::Completed);
        let s = State::from_parts(StateType::Running, Some(""), None, Map::new());
        assert_eq!(s.name, "Running");
    }

    #[test]
    fn parse_round_trip() {
        for t in StateType::ALL {
            assert_eq!(StateType::parse(t.as_str()), Some(t));
        }
        assert_eq!(StateType::parse("Nope"), None);
    }
}
