//! Flow health: PASS, WARN or FAIL derived from recorded runs against a flow's
//! `fresh_within`, `expect_by`, `expected_duration` and `overdue_factor`, and
//! the `run.overdue` event a sweep records once per run that outlasts its
//! expectation. A read model: nothing here is a second state machine.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use cereyan_core::schedule::{from_micros, Schedule};
use cereyan_core::{now_micros, EventName, Flow, FlowOptions, StateType};
use serde::Serialize;
use serde_json::json;
use tokio::sync::watch;

use crate::state::AppState;

/// How often Running runs are checked against their expected duration.
const SWEEP_SECS: u64 = 5;

/// Recent run states read per flow, for deriving a median duration. Matches the
/// flows list's read so both derive from the same window.
const RECENT_RUNS_READ: usize = 40;

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
/// The expectation for a flow, and where it came from.
///
/// `recent` is passed in because the caller already read it: fetching the last
/// forty run states here meant one query per *run* being checked rather than one
/// per flow, on a sweep that runs every five seconds.
pub fn expected_duration(
    options: &FlowOptions,
    recent: &[cereyan_store::RecentRun],
) -> Option<(f64, &'static str)> {
    if let Some(d) = options.expected_duration.filter(|d| *d > 0.0) {
        return Some((d, "expected_duration"));
    }
    let factor = options.overdue_factor.filter(|f| *f > 0.0)?;
    let mut durations: Vec<i64> = recent
        .iter()
        .filter(|(_, t, _, d)| t == "Completed" && d.is_some())
        .filter_map(|(_, _, _, d)| *d)
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
///
/// Walks **backwards** from `now` and stops at two. It used to walk forwards from
/// the start of each of five widening windows, taking the last two of whatever it
/// found — so a per-minute `expect_by` visited 1,440 fires, re-parsing the cron and
/// re-reading `/etc/localtime` on every step, to return two timestamps. Measured at
/// 23.6 ms per flow per `/api/flows` request before, 2 steps after.
///
/// The 800-day floor is kept because it was the widest window tried, and it is
/// still the answer to "has this flow fired at all recently enough to judge". It is
/// applied per step rather than by walking forward to it, so a schedule that last
/// fired nine years ago still reports nothing.
fn deadline_window(cron: &str, tz: Option<&str>, now: i64) -> Option<(i64, i64)> {
    let schedule = Schedule::Cron {
        cron: cron.to_string(),
        timezone: tz.map(|t| t.to_string()),
        day_or: true,
    };
    schedule.validate().ok()?;
    let end = from_micros(now);
    let floor = end - chrono::Duration::days(800);
    let fires = schedule.fires_before(end, floor, 2).ok()?;
    if fires.len() < 2 {
        return None;
    }
    // Most recent first, so the older one is second.
    Some((
        cereyan_core::schedule::to_micros(fires[1]),
        cereyan_core::schedule::to_micros(fires[0]),
    ))
}

/// The flow's health, or none when it declares no expectation.
/// Health of one flow against its expectations.
/// The reasons for running runs that have exceeded their flow's expectation.
///
/// Pure, and takes `now` explicitly, so the comparison at the boundary is
/// testable: a caller that reads its own clock cannot be pinned to an exact
/// elapsed value, which made `>` against `>=` invisible to any test.
///
/// `start_times` is the batched read the caller made — `Store::run_start_times`.
/// A run with no entry, or an entry of `None`, is skipped: it has not started, or
/// the row is gone.
fn overdue_reasons(
    active: &[crate::index::ActiveRun],
    start_times: &std::collections::HashMap<i64, Option<i64>>,
    now: i64,
    expected: f64,
    basis: &str,
) -> Vec<String> {
    let mut out = Vec::new();
    for run in active {
        if run.state.state_type != StateType::Running {
            continue;
        }
        let Some(start) = start_times.get(&run.id).copied().flatten() else {
            continue;
        };
        let elapsed = (now - start) as f64 / 1_000_000.0;
        if elapsed > expected {
            out.push(format!(
                "run {} has been running {} against {} expected ({basis})",
                run.id,
                human(elapsed),
                human(expected)
            ));
        }
    }
    out
}

///
/// `options`, `active`, `recent`, `last_completed_at` and `start_times` are all
/// passed in because the caller already has them: parsing the options again,
/// cloning the whole active set per flow, and reading a run per running run are
/// what made the flows list expensive.
#[allow(clippy::too_many_arguments)]
pub fn flow_health(
    flow: &Flow,
    options: &FlowOptions,
    active: &[crate::index::ActiveRun],
    recent: &[cereyan_store::RecentRun],
    last_completed_at: Option<i64>,
    start_times: &std::collections::HashMap<i64, Option<i64>>,
) -> Option<FlowHealth> {
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
    // `last_completed_at` is passed in: reading it here meant a full run
    // projection — including the per-row task_counts aggregate and the join to
    // `flow` — to extract a single timestamp, once per flow.

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

    if let Some((expected, basis)) = expected_duration(options, recent) {
        for reason in overdue_reasons(active, start_times, now, expected, basis) {
            status = worst(status, "WARN");
            reasons.push(reason);
        }
    }

    Some(FlowHealth {
        status: status.into(),
        reasons,
        last_completed_at,
        deadline,
    })
}

/// One pass: every Running run past its flow's expectation gets `run.overdue` once.
pub fn sweep(state: &Arc<AppState>) -> usize {
    let now = now_micros();
    let mut recorded = 0;
    // One snapshot of the active set, projected twice. The previous code cloned
    // the whole set separately for the running runs and for the alive ids.
    let all = state.index.active_runs();
    {
        // Forget runs that are no longer active so the set stays small.
        let alive: HashSet<i64> = all.iter().map(|r| r.id).collect();
        let mut guard = state.overdue_reported.lock().unwrap_or_else(|e| e.into_inner());
        guard.retain(|id| alive.contains(id));
    }
    let running: Vec<crate::index::ActiveRun> = all
        .into_iter()
        .filter(|r| r.state.state_type == StateType::Running)
        .collect();
    if running.is_empty() {
        return 0;
    }

    // One read per distinct flow, not per run. A flow with twenty running runs
    // used to be fetched twenty times, and its forty recent run states read
    // twenty times over.
    let flow_ids: Vec<i64> = {
        let mut ids: Vec<i64> = running.iter().map(|r| r.flow_id).collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    };
    let flows = state.store.get_flows_by_ids(&flow_ids).unwrap_or_default();
    let recent_by_flow = state
        .store
        .recent_run_states_many(&flow_ids, RECENT_RUNS_READ)
        .unwrap_or_default();
    let start_times = state
        .store
        .run_start_times(&running.iter().map(|r| r.id).collect::<Vec<_>>())
        .unwrap_or_default();

    for active in running {
        let Some(flow) = flows.get(&active.flow_id) else {
            continue;
        };
        let options = FlowOptions::from_map(&flow.options);
        let empty: Vec<cereyan_store::RecentRun> = Vec::new();
        let Some((expected, basis)) = expected_duration(
            &options,
            recent_by_flow
                .get(&active.flow_id)
                .unwrap_or(&empty),
        ) else {
            continue;
        };
        // A light projection of `start_time`, rather than a whole run.
        let Some(start) = start_times.get(&active.id).copied().flatten() else {
            continue;
        };
        let elapsed = (now - start) as f64 / 1_000_000.0;
        if elapsed <= expected {
            continue;
        }
        {
            let mut guard = state.overdue_reported.lock().unwrap_or_else(|e| e.into_inner());
            if !guard.insert(active.id) {
                continue;
            }
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

#[cfg(test)]
pub(crate) mod flow_health_tests {
    use super::*;
    use cereyan_core::State;
    use std::collections::HashMap;
    use std::time::Instant;

    fn flow(id: i64) -> Flow {
        Flow {
            id,
            external_id: cereyan_core::new_id(),
            project: "p".into(),
            name: "etl".into(),
            module: "m".into(),
            source_dir: "/tmp".into(),
            description: None,
            tags: vec![],
            group: None,
            parameter_schema: serde_json::json!({}),
            options: serde_json::Map::new(),
            error: None,
            created_at: 0,
            last_seen_at: 0,
            live: false,
        }
    }

    /// A running run `elapsed_secs` ago, as the index would hold it.
    pub(crate) fn running(id: i64, _elapsed_secs: f64) -> crate::index::ActiveRun {
        crate::index::ActiveRun {
            id,
            flow_id: 1,
            name: format!("r{id}"),
            state: State::new(StateType::Running),
            engine_pid: Some(1),
            engine_id: Some("e".into()),
            last_heartbeat: Instant::now(),
            cancel_requested: false,
            cancelling_since: None,
            terminated_at: None,
            key: crate::supervisor::EngineKey {
                source_dir: "/tmp".into(),
                module: "m".into(),
                isolated: false,
                nice: 0,
            },
            adopted: false,
        }
    }

    /// A run the index holds as running, with an arbitrary flow.
    pub(crate) fn running_pub(id: i64) -> crate::index::ActiveRun {
        running(id, 0.0)
    }

    fn queued(id: i64) -> crate::index::ActiveRun {
        let mut r = running(id, 0.0);
        r.state = State::new(StateType::Scheduled);
        r
    }

    /// The expectations options: a fixed expected duration, so the rule reduces
    /// to "running longer than this".
    fn expecting(secs: f64) -> FlowOptions {
        FlowOptions {
            expected_duration: Some(secs),
            ..Default::default()
        }
    }

    /// Start times as `run_start_times` returns them: a map from id to an
    /// optional timestamp, with no entry at all for a run that does not exist.
    fn starts(pairs: &[(i64, Option<f64>)], now: i64) -> HashMap<i64, Option<i64>> {
        pairs
            .iter()
            .map(|(id, secs)| {
                (*id, secs.map(|s| now - (s * 1_000_000.0) as i64))
            })
            .collect()
    }

    #[test]
    fn a_run_past_its_expectation_is_reported() {
        let now = now_micros();
        let active = [running(1, 600.0)];
        let h = flow_health(
            &flow(1),
            &expecting(60.0),
            &active,
            &[],
            None,
            &starts(&[(1, Some(600.0))], now),
        )
        .expect("a flow with an expectation has health");
        assert_eq!(h.status, "WARN", "{:?}", h.reasons);
        assert_eq!(h.reasons.len(), 1, "{:?}", h.reasons);
        assert!(h.reasons[0].contains('1'), "names the run: {:?}", h.reasons);
        assert!(
            h.reasons[0].contains("expected_duration"),
            "names the basis: {:?}",
            h.reasons
        );
    }

    #[test]
    fn a_run_within_its_expectation_is_not_reported() {
        let now = now_micros();
        let active = [running(1, 10.0)];
        let h = flow_health(
            &flow(1),
            &expecting(60.0),
            &active,
            &[],
            None,
            &starts(&[(1, Some(10.0))], now),
        )
        .expect("health");
        assert_eq!(h.status, "PASS", "{:?}", h.reasons);
        assert!(h.reasons.is_empty(), "{:?}", h.reasons);
    }

    /// A run whose start time the batched read did not return — because it has
    /// not started, or because the row is gone. The old code reached the same
    /// `continue` when `get_run` returned a run with no `start_time`, or `None`.
    #[test]
    fn a_run_with_no_start_time_is_skipped() {
        let now = now_micros();
        for map in [
            starts(&[(1, None)], now),         // known, not started
            HashMap::new(),                      // not in the map at all
        ] {
            let active = [running(1, 600.0)];
            let h = flow_health(&flow(1), &expecting(60.0), &active, &[], None, &map)
                .expect("health");
            assert!(
                h.reasons.is_empty(),
                "a run with no start time is skipped, got {:?}",
                h.reasons
            );
        }
    }

    #[test]
    fn only_running_runs_contribute_a_start_time() {
        let now = now_micros();
        // A Scheduled run with an old start time must not be reported as overdue.
        let active = [queued(1)];
        let h = flow_health(
            &flow(1),
            &expecting(60.0),
            &active,
            &[],
            None,
            &starts(&[(1, Some(9_999.0))], now),
        )
        .expect("health");
        assert!(h.reasons.is_empty(), "a non-Running run is not overdue: {:?}", h.reasons);
    }

    /// Several flows, each with its own runs. The map is shared, so the risk this
    /// covers is one flow reading another's runs.
    #[test]
    fn each_flow_reads_only_its_own_runs() {
        let now = now_micros();
        let mut one = running(1, 600.0);
        one.flow_id = 1;
        let mut two = running(2, 600.0);
        two.flow_id = 2;
        let map = starts(&[(1, Some(600.0)), (2, Some(600.0))], now);

        for (f, active, expected_warn) in [
            (1, &[one][..], true),
            (2, &[two][..], true),
        ] {
            let h = flow_health(&flow(f), &expecting(60.0), active, &[], None, &map)
                .expect("health");
            assert_eq!(h.reasons.len(), 1, "flow {f}: {:?}", h.reasons);
            assert!(
                h.reasons[0].contains(&active[0].id.to_string()),
                "flow {f} reported another flow's run: {:?}",
                h.reasons
            );
            assert_eq!(h.status == "WARN", expected_warn);
        }
        // And a flow with no running run reports nothing even though the map has
        // entries for other flows' runs.
        let h = flow_health(&flow(3), &expecting(60.0), &[], &[], None, &map).expect("health");
        assert!(h.reasons.is_empty(), "{:?}", h.reasons);
    }

    #[test]
    fn a_flow_with_no_expectation_has_no_health() {
        let now = now_micros();
        let active = [running(1, 600.0)];
        let none = FlowOptions::default();
        assert!(
            flow_health(
                &flow(1),
                &none,
                &active,
                &[],
                None,
                &starts(&[(1, Some(600.0))], now),
            )
            .is_none(),
            "a flow with nothing to compare against has no health, and reads nothing"
        );
    }

    /// The reasons must be what the previous implementation produced, which read
    /// a whole run per running run and took `start_time` from it.
    ///
    /// Compared on the **set of run ids named and their order**, not on the full
    /// text. `flow_health` reads its own `now_micros()` internally, so a test
    /// cannot pin the elapsed figure: by the time it runs, the run is a few
    /// microseconds older than when the fixture was built, and `human()` rounds
    /// that. The full wording *is* pinned, by the tests above, at margins where
    /// rounding is stable — 600 s against 60 s, not 60.5 s against 60 s.
    ///
    /// What this test covers is the thing the edit could have broken: which runs
    /// are considered, and which are reported.
    #[test]
    fn the_reports_match_the_previous_implementation() {
        let now = now_micros();
        let cases: Vec<(Vec<crate::index::ActiveRun>, Vec<(i64, Option<f64>)>, f64)> = vec![
            (vec![running(1, 600.0)], vec![(1, Some(600.0))], 60.0),
            (vec![running(1, 10.0)], vec![(1, Some(10.0))], 60.0),
            (
                vec![running(1, 600.0), running(2, 120.0), queued(3)],
                vec![(1, Some(600.0)), (2, Some(120.0)), (3, Some(9_000.0))],
                60.0,
            ),
            (vec![running(1, 600.0)], vec![(1, None)], 60.0),
            (vec![running(1, 600.0)], vec![], 60.0),
            (vec![], vec![], 60.0),
            // A run in another flow, whose start time is in the map.
            (
                {
                    let mut r = running(1, 600.0);
                    r.flow_id = 7;
                    vec![r]
                },
                vec![(1, Some(600.0))],
                60.0,
            ),
        ];
        // The run id a reason names: the second whitespace-separated token of
        // "run <id> has been running ...".
        let named = |r: &str| -> String {
            r.split_whitespace().nth(1).unwrap_or("").to_string()
        };
        for (active, pairs, expected) in cases {
            let map = starts(&pairs, now);
            let got = flow_health(&flow(1), &expecting(expected), &active, &[], None, &map)
                .expect("health");

            // The previous implementation, reconstructed.
            let mut want_ids = Vec::new();
            let mut want_warn = false;
            for r in active.iter().filter(|r| r.state.state_type == StateType::Running) {
                let Some(start) = map.get(&r.id).copied().flatten() else {
                    continue;
                };
                let elapsed = (now - start) as f64 / 1_000_000.0;
                if elapsed > expected {
                    want_ids.push(r.id.to_string());
                    want_warn = true;
                }
            }
            let got_ids: Vec<String> = got.reasons.iter().map(|r| named(r)).collect();
            assert_eq!(
                got_ids, want_ids,
                "the reported runs differ for {active:?}"
            );
            assert_eq!(
                got.status == "WARN",
                want_warn,
                "the status differs for {active:?}"
            );
            // Every reason is well formed, whatever the elapsed figure rounds to.
            for r in &got.reasons {
                assert!(r.starts_with("run "), "malformed reason: {r}");
                assert!(r.contains(" expected ("), "malformed reason: {r}");
            }
        }
    }
}

#[cfg(test)]
mod overdue_rule_tests {
    use super::overdue_reasons;
    use std::collections::HashMap;

    fn running_at(id: i64, flow_id: i64) -> crate::index::ActiveRun {
        let mut r = super::flow_health_tests::running_pub(id);
        r.flow_id = flow_id;
        r
    }

    /// The boundary, made testable by the rule taking `now`.
    ///
    /// Before the extraction this comparison could not be pinned by any test: the
    /// caller read its own `now_micros()`, so a run started exactly `expected`
    /// ago was already a few microseconds over by the time the assertion ran, and
    /// `>` and `>=` were indistinguishable. With `now` supplied, they are not.
    #[test]
    fn a_run_exactly_at_its_expectation_is_not_yet_overdue() {
        let now = 1_000_000_000i64;
        let active = [running_at(1, 1)];
        let at_expectation: HashMap<i64, Option<i64>> =
            [(1i64, Some(now - 60_000_000))].into_iter().collect();

        assert!(
            overdue_reasons(&active, &at_expectation, now, 60.0, "expected_duration").is_empty(),
            "elapsed == expected is not greater than expected"
        );
        // One microsecond over is over.
        let just_over: HashMap<i64, Option<i64>> =
            [(1i64, Some(now - 60_000_000 - 1))].into_iter().collect();
        assert_eq!(
            overdue_reasons(&active, &just_over, now, 60.0, "expected_duration").len(),
            1,
            "one microsecond past the expectation is over it"
        );
        // And one microsecond under is not.
        let just_under: HashMap<i64, Option<i64>> =
            [(1i64, Some(now - 60_000_000 + 1))].into_iter().collect();
        assert!(
            overdue_reasons(&active, &just_under, now, 60.0, "expected_duration").is_empty(),
            "one microsecond short of the expectation is not over it"
        );
    }

    #[test]
    fn the_elapsed_and_expected_figures_are_rendered() {
        let now = 1_000_000_000i64;
        let active = [running_at(7, 1)];
        let map: HashMap<i64, Option<i64>> = [(7i64, Some(now - 600_000_000))].into_iter().collect();
        assert_eq!(
            overdue_reasons(&active, &map, now, 60.0, "overdue_factor"),
            vec!["run 7 has been running 10m against 60s expected (overdue_factor)"],
            "the wording and the basis are part of what this rule produces"
        );
    }

    #[test]
    fn a_run_from_another_flow_is_not_considered() {
        let now = 1_000_000_000i64;
        let map: HashMap<i64, Option<i64>> = [(1i64, Some(now - 600_000_000))].into_iter().collect();
        // The rule is handed a flow's own active slice, so a run belonging to
        // another flow is simply not in it.
        assert!(
            overdue_reasons(&[], &map, now, 60.0, "expected_duration").is_empty(),
            "an empty slice reports nothing, whatever the map holds"
        );
    }
}
