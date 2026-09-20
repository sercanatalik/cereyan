//! Flow health: PASS, WARN or FAIL derived from recorded runs against a flow's
//! `fresh_within`, `expect_by`, `expected_duration` and `overdue_factor`, and
//! the `run.overdue` event a sweep records once per run that outlasts its
//! expectation. A read model: nothing here is a second state machine.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cereyan_core::schedule::{from_micros, Schedule};
use cereyan_core::{now_micros, EventName, Flow, FlowOptions, StateType};
use cereyan_store::ListRunsFilter;
use serde::Serialize;
use serde_json::json;
use tokio::sync::watch;

use crate::state::AppState;

/// How often Running runs are checked against their expected duration.
const SWEEP_SECS: u64 = 5;

#[derive(Clone, Debug, Serialize, utoipa::ToSchema, PartialEq)]
pub struct FlowHealth {
    /// `PASS`, `WARN`, or `FAIL`.
    pub status: String,
    /// Why, one sentence per finding; empty for a passing flow.
    pub reasons: Vec<String>,
    /// When the last Completed run ended, microseconds.
    pub last_completed_at: Option<i64>,
    /// The `expect_by` deadline in force, microseconds.
    pub deadline: Option<i64>,
}

fn worst(a: &str, b: &str) -> &'static str {
    match (a, b) {
        ("FAIL", _) | (_, "FAIL") => "FAIL",
        ("WARN", _) | (_, "WARN") => "WARN",
        _ => "PASS",
    }
}

fn human(secs: f64) -> String {
    if secs < 90.0 {
        format!("{secs:.0}s")
    } else if secs < 5400.0 {
        format!("{:.0}m", secs / 60.0)
    } else if secs < 172_800.0 {
        format!("{:.1}h", secs / 3600.0)
    } else {
        format!("{:.1}d", secs / 86_400.0)
    }
}

/// The expected duration of a run of `flow`, in seconds, and what it rests on.
pub fn expected_duration(
    state: &AppState,
    flow: &Flow,
    options: &FlowOptions,
) -> Option<(f64, &'static str)> {
    if let Some(d) = options.expected_duration.filter(|d| *d > 0.0) {
        return Some((d, "expected_duration"));
    }
    let factor = options.overdue_factor.filter(|f| *f > 0.0)?;
    let mut durations: Vec<i64> = state
        .store
        .recent_run_states(flow.id, 40)
        .ok()?
        .into_iter()
        .filter(|(_, t, _, d)| t == "Completed" && d.is_some())
        .filter_map(|(_, _, _, d)| d)
        .take(20)
        .collect();
    if durations.len() < 3 {
        return None;
    }
    durations.sort();
    let median = durations[durations.len() / 2] as f64 / 1_000_000.0;
    Some((median * factor, "median"))
}

/// The latest `expect_by` fire at or before `now` and the one before it.
fn deadline_window(cron: &str, tz: Option<&str>, now: i64) -> Option<(i64, i64)> {
    let schedule = Schedule::Cron {
        cron: cron.to_string(),
        timezone: tz.map(|t| t.to_string()),
        day_or: true,
    };
    schedule.validate().ok()?;
    let end = from_micros(now);
    for days in [1i64, 7, 31, 366, 800] {
        let start = end - chrono::Duration::days(days);
        let fires = schedule.fires_between(start, end, 100_000).ok()?;
        if fires.len() >= 2 {
            let n = fires.len();
            return Some((
                cereyan_core::schedule::to_micros(fires[n - 2]),
                cereyan_core::schedule::to_micros(fires[n - 1]),
            ));
        }
    }
    None
}

/// The flow's health, or none when it declares no expectation.
pub fn flow_health(state: &AppState, flow: &Flow) -> Option<FlowHealth> {
    let options = FlowOptions::from_map(&flow.options);
    if options.fresh_within.is_none()
        && options.expect_by.is_none()
        && options.expected_duration.is_none()
        && options.overdue_factor.is_none()
    {
        return None;
    }
    let now = now_micros();
    let mut status = "PASS";
    let mut reasons = Vec::new();
    let last_completed = state
        .store
        .list_runs(&ListRunsFilter {
            flow_id: Some(flow.id),
            state_type: Some("Completed".into()),
            limit: Some(1),
            sort: Some("created_desc".into()),
            ..Default::default()
        })
        .ok()
        .and_then(|p| p.items.into_iter().next());
    let last_completed_at = last_completed.as_ref().and_then(|r| r.end_time);
    let active: Vec<_> = state
        .index
        .active_runs()
        .into_iter()
        .filter(|r| r.flow_id == flow.id)
        .collect();

    if let Some(window) = options.fresh_within.filter(|w| *w > 0.0) {
        match last_completed_at {
            Some(ended) => {
                let age = (now - ended) as f64 / 1_000_000.0;
                if age > window {
                    status = worst(status, "FAIL");
                    reasons.push(format!(
                        "last completed {} ago, more than fresh_within {}",
                        human(age),
                        human(window)
                    ));
                } else if age > window * 0.75 {
                    status = worst(status, "WARN");
                    reasons.push(format!(
                        "last completed {} ago, close to fresh_within {}",
                        human(age),
                        human(window)
                    ));
                }
            }
            None => {
                let existed = (now - flow.created_at) as f64 / 1_000_000.0;
                if existed > window {
                    status = worst(status, "FAIL");
                    reasons.push(format!("never completed in fresh_within {}", human(window)));
                }
            }
        }
    }

    let mut deadline = None;
    if let Some(cron) = options
        .expect_by
        .as_deref()
        .filter(|c| !c.trim().is_empty())
    {
        if let Some((previous, latest)) =
            deadline_window(cron, options.expect_by_tz.as_deref(), now)
        {
            deadline = Some(latest);
            let done_in_window = last_completed_at.is_some_and(|t| t > previous);
            if !done_in_window {
                if !active.is_empty() {
                    status = worst(status, "WARN");
                    reasons.push(format!("deadline {cron} passed with a run still active"));
                } else {
                    status = worst(status, "FAIL");
                    reasons.push(format!(
                        "no completed run since the {cron} deadline before this one"
                    ));
                }
            }
        }
    }

    if let Some((expected, basis)) = expected_duration(state, flow, &options) {
        for run in &active {
            if run.state.state_type != StateType::Running {
                continue;
            }
            let Some(start) = state
                .store
                .get_run(run.id)
                .ok()
                .flatten()
                .and_then(|r| r.start_time)
            else {
                continue;
            };
            let elapsed = (now - start) as f64 / 1_000_000.0;
            if elapsed > expected {
                status = worst(status, "WARN");
                reasons.push(format!(
                    "run {} has been running {} against {} expected ({basis})",
                    run.id,
                    human(elapsed),
                    human(expected)
                ));
            }
        }
    }

    Some(FlowHealth {
        status: status.into(),
        reasons,
        last_completed_at,
        deadline,
    })
}

/// Runs already reported overdue, so the event is recorded once per run.
static OVERDUE_REPORTED: Mutex<Option<HashSet<i64>>> = Mutex::new(None);

fn already_reported(run_id: i64) -> bool {
    let mut guard = OVERDUE_REPORTED.lock().unwrap_or_else(|e| e.into_inner());
    let set = guard.get_or_insert_with(HashSet::new);
    !set.insert(run_id)
}

/// One pass: every Running run past its flow's expectation gets `run.overdue` once.
pub fn sweep(state: &Arc<AppState>) -> usize {
    let now = now_micros();
    let mut recorded = 0;
    let running: Vec<_> = state
        .index
        .active_runs()
        .into_iter()
        .filter(|r| r.state.state_type == StateType::Running)
        .collect();
    {
        // Forget runs that are no longer active so the set stays small.
        let alive: HashSet<i64> = state
            .index
            .active_runs()
            .into_iter()
            .map(|r| r.id)
            .collect();
        let mut guard = OVERDUE_REPORTED.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(set) = guard.as_mut() {
            set.retain(|id| alive.contains(id));
        }
    }
    for active in running {
        let Ok(Some(flow)) = state.store.get_flow(active.flow_id) else {
            continue;
        };
        let options = FlowOptions::from_map(&flow.options);
        let Some((expected, basis)) = expected_duration(state, &flow, &options) else {
            continue;
        };
        let Some(start) = state
            .store
            .get_run(active.id)
            .ok()
            .flatten()
            .and_then(|r| r.start_time)
        else {
            continue;
        };
        let elapsed = (now - start) as f64 / 1_000_000.0;
        if elapsed <= expected || already_reported(active.id) {
            continue;
        }
        let _ = state.record_engine_event(
            EventName::RunOverdue,
            Some(active.id),
            Some(flow.id),
            json!({"expected_seconds": expected, "elapsed_seconds": elapsed, "basis": basis}),
        );
        recorded += 1;
    }
    recorded
}

pub async fn sweep_loop(state: Arc<AppState>, mut shutdown: watch::Receiver<bool>) {
    loop {
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(SWEEP_SECS)) => {}
            _ = shutdown.changed() => { if *shutdown.borrow() { return; } }
        }
        let st = state.clone();
        let _ = tokio::task::spawn_blocking(move || sweep(&st)).await;
    }
}
