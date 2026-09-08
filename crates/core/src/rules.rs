//! State-transition rules shared by the offline and served execution paths.
//! `propose` is pure: it looks only at the current state, the proposal, and
//! the run policy.

use serde_json::Value;

use crate::state::{State, StateType};
use crate::time::MICROS_PER_SECOND;

/// Per-run policy inputs that later phases extend (retries, timeouts).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RunPolicy {
    pub max_retries: u32,
}

#[derive(Clone, Debug)]
pub struct Proposal {
    pub state: State,
    pub force: bool,
}

impl Proposal {
    pub fn new(state: State) -> Proposal {
        Proposal {
            state,
            force: false,
        }
    }

    pub fn forced(state: State) -> Proposal {
        Proposal { state, force: true }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    Accept,
    Reject(&'static str),
    Rewrite(State),
}

impl Outcome {
    /// The state that will be recorded if the outcome is not a rejection.
    pub fn resolved(self, proposed: State) -> Result<State, &'static str> {
        match self {
            Outcome::Accept => Ok(proposed),
            Outcome::Rewrite(s) => Ok(s),
            Outcome::Reject(reason) => Err(reason),
        }
    }
}

/// Decide whether `proposal` may follow `current`.
pub fn propose(current: Option<&State>, proposal: &Proposal, _policy: &RunPolicy) -> Outcome {
    let proposed = &proposal.state;

    if proposal.force {
        let mut s = proposed.clone();
        s.details.insert("forced".to_string(), Value::Bool(true));
        return Outcome::Rewrite(s);
    }

    if let Some(cur) = current {
        if cur.is_terminal() {
            return Outcome::Reject("terminal");
        }
        if cur.state_type == proposed.state_type
            && cur.name == proposed.name
            && (proposed.timestamp - cur.timestamp).abs() < MICROS_PER_SECOND
        {
            return Outcome::Reject("duplicate");
        }
    }

    let cur_type = current.map(|c| c.state_type);
    match proposed.state_type {
        StateType::Pending => match cur_type {
            None | Some(StateType::Scheduled) => Outcome::Accept,
            _ => Outcome::Reject("invalid-entry"),
        },
        StateType::Running => match cur_type {
            Some(StateType::Pending) | Some(StateType::Scheduled) | Some(StateType::Paused) => {
                Outcome::Accept
            }
            _ => Outcome::Reject("invalid-entry"),
        },
        StateType::Cancelled => match cur_type {
            Some(StateType::Cancelling)
            | Some(StateType::Scheduled)
            | Some(StateType::Pending)
            | Some(StateType::Running)
            | Some(StateType::Paused) => Outcome::Accept,
            _ => Outcome::Reject("invalid-entry"),
        },
        // A run pauses for input only while it is running.
        StateType::Paused => match cur_type {
            Some(StateType::Running) => Outcome::Accept,
            _ => Outcome::Reject("invalid-entry"),
        },
        _ => match cur_type {
            // A cancelling run may only become Cancelled.
            Some(StateType::Cancelling) => Outcome::Reject("cancelling"),
            _ => Outcome::Accept,
        },
    }
}

/// Counters and timing carried by every run, updated on accepted transitions.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RunCounters {
    pub failure_count: u32,
    pub crash_count: u32,
    pub start_time: Option<i64>,
    pub end_time: Option<i64>,
    pub total_run_time: Option<i64>,
}

impl RunCounters {
    /// Apply an accepted state to the counters and timing fields.
    pub fn apply(&mut self, accepted: &State) {
        match accepted.state_type {
            StateType::Failed => self.failure_count += 1,
            StateType::Crashed => self.crash_count += 1,
            // A retry-pending state means an attempt failed.
            StateType::Scheduled if accepted.name == "AwaitingRetry" => self.failure_count += 1,
            _ => {}
        }
        if accepted.state_type == StateType::Running && self.start_time.is_none() {
            self.start_time = Some(accepted.timestamp);
        }
        if accepted.state_type == StateType::Scheduled && accepted.name == "AwaitingRetry" {
            // A retried run keeps its original start time; TimedOut and
            // others compute timing from it.
        }
        if accepted.is_terminal() {
            self.end_time = Some(accepted.timestamp);
            if let Some(start) = self.start_time {
                self.total_run_time = Some((accepted.timestamp - start).max(0));
            }
        } else {
            self.end_time = None;
            self.total_run_time = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::StateName;

    fn st(t: StateType) -> State {
        State::new(t)
    }

    fn at(t: StateType, ts: i64) -> State {
        State::new(t).with_timestamp(ts)
    }

    #[test]
    fn table() {
        use StateType::*;
        let policy = RunPolicy::default();
        // (current, proposed, force, expected)
        let cases: Vec<(Option<State>, State, bool, Outcome)> = vec![
            (None, st(Pending), false, Outcome::Accept),
            (None, st(Scheduled), false, Outcome::Accept),
            (None, st(Running), false, Outcome::Reject("invalid-entry")),
            (Some(st(Scheduled)), st(Pending), false, Outcome::Accept),
            (
                Some(st(Running)),
                st(Pending),
                false,
                Outcome::Reject("invalid-entry"),
            ),
            (Some(st(Pending)), st(Running), false, Outcome::Accept),
            (Some(st(Scheduled)), st(Running), false, Outcome::Accept),
            (Some(st(Paused)), st(Running), false, Outcome::Accept),
            (Some(st(Running)), st(Completed), false, Outcome::Accept),
            (Some(st(Running)), st(Failed), false, Outcome::Accept),
            (Some(st(Running)), st(Crashed), false, Outcome::Accept),
            (
                Some(st(Completed)),
                st(Running),
                false,
                Outcome::Reject("terminal"),
            ),
            (
                Some(st(Failed)),
                st(Completed),
                false,
                Outcome::Reject("terminal"),
            ),
            (
                Some(st(Cancelling)),
                st(Completed),
                false,
                Outcome::Reject("cancelling"),
            ),
            (Some(st(Cancelling)), st(Cancelled), false, Outcome::Accept),
            (Some(st(Running)), st(Cancelled), false, Outcome::Accept),
            (Some(st(Pending)), st(Cancelled), false, Outcome::Accept),
            (Some(st(Scheduled)), st(Cancelled), false, Outcome::Accept),
            (Some(st(Paused)), st(Cancelled), false, Outcome::Accept),
            (Some(st(Running)), st(Cancelling), false, Outcome::Accept),
            (
                Some(st(Running)),
                State::named(StateName::Skipped),
                false,
                Outcome::Accept,
            ),
        ];
        for (i, (cur, prop, force, expected)) in cases.into_iter().enumerate() {
            let proposal = Proposal {
                state: prop.clone(),
                force,
            };
            let got = propose(cur.as_ref(), &proposal, &policy);
            assert_eq!(got, expected, "case {i}: {:?} -> {:?}", cur, prop);
        }
    }

    #[test]
    fn duplicate_within_a_second_is_rejected() {
        let cur = at(StateType::Running, 1_000_000);
        let dup = at(StateType::Running, 1_500_000);
        assert_eq!(
            propose(Some(&cur), &Proposal::new(dup), &RunPolicy::default()),
            Outcome::Reject("duplicate")
        );
        // Different name with the same type is not a duplicate.
        let cur = at(StateType::Scheduled, 1_000_000);
        let mut late = at(StateType::Scheduled, 1_500_000);
        late.name = "Late".into();
        assert_eq!(
            propose(Some(&cur), &Proposal::new(late), &RunPolicy::default()),
            Outcome::Accept
        );
        // The same state again after more than a second is not a duplicate either.
        let again = at(StateType::Scheduled, 2_500_000);
        assert_eq!(
            propose(Some(&cur), &Proposal::new(again), &RunPolicy::default()),
            Outcome::Accept
        );
    }

    #[test]
    fn force_overrides_terminal_and_records_flag() {
        let cur = st(StateType::Completed);
        let out = propose(
            Some(&cur),
            &Proposal::forced(st(StateType::Failed)),
            &RunPolicy::default(),
        );
        match out {
            Outcome::Rewrite(s) => {
                assert_eq!(s.state_type, StateType::Failed);
                assert_eq!(s.details.get("forced"), Some(&Value::Bool(true)));
            }
            other => panic!("expected rewrite, got {other:?}"),
        }
    }

    #[test]
    fn counters_and_timing() {
        let mut c = RunCounters::default();
        c.apply(&at(StateType::Pending, 10));
        c.apply(&at(StateType::Running, 20));
        c.apply(&at(StateType::Completed, 50));
        assert_eq!(c.start_time, Some(20));
        assert_eq!(c.end_time, Some(50));
        assert_eq!(c.total_run_time, Some(30));
        assert_eq!(c.failure_count, 0);

        let mut c = RunCounters::default();
        c.apply(&at(StateType::Running, 20));
        c.apply(&at(StateType::Crashed, 30));
        assert_eq!(c.crash_count, 1);
        assert_eq!(c.failure_count, 0);

        let mut c = RunCounters::default();
        c.apply(&at(StateType::Running, 20));
        c.apply(&at(StateType::Failed, 30));
        assert_eq!(c.failure_count, 1);
        assert_eq!(c.crash_count, 0);
    }
}
