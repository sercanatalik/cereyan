//! Domain model, identifiers, time helpers, and state-transition rules.
//! This crate performs no I/O and depends on neither SQLite, tokio, nor pyo3.

pub mod id;
pub mod model;
pub mod rules;
pub mod schedule;
pub mod state;
pub mod time;

pub use id::{new_id, Id};
pub use model::*;
pub use rules::{propose, Outcome, Proposal, RunPolicy};
pub use schedule::{CatchupPolicy, Schedule, ScheduleError};
pub use state::{State, StateName, StateType};
pub use time::now_micros;
