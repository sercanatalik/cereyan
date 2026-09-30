//! Prometheus text exposition, rendered by hand: fixed-bucket histograms over
//! atomics, a five-second sample ring for the dashboard, and the scrape body.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cereyan_core::{now_micros, StateType};
use serde::Serialize;
use tokio::sync::watch;

use crate::state::AppState;

/// Bounds in seconds for delays that run from a second to an hour.
pub const DELAY_BOUNDS: &[f64] = &[1.0, 5.0, 15.0, 60.0, 300.0, 900.0, 3600.0];

/// A cumulative histogram with fixed bounds. `+Inf` is implicit.
pub struct Histogram {
    bounds: &'static [f64],
    counts: Vec<AtomicU64>,
    sum_micros: AtomicU64,
    count: AtomicU64,
}

impl Histogram {
    pub fn new(bounds: &'static [f64]) -> Histogram {
        Histogram {
            bounds,
            counts: bounds.iter().map(|_| AtomicU64::new(0)).collect(),
            sum_micros: AtomicU64::new(0),
            count: AtomicU64::new(0),
        }
    }

    pub fn observe(&self, seconds: f64) {
        let seconds = seconds.max(0.0);
        for (bound, slot) in self.bounds.iter().zip(&self.counts) {
            if seconds <= *bound {
                slot.fetch_add(1, Ordering::Relaxed);
            }
        }
        self.sum_micros
            .fetch_add((seconds * 1_000_000.0) as u64, Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed);
    }

    /// `(bounds with their cumulative counts, sum in seconds, count)`.
    pub fn snapshot(&self) -> (Vec<(f64, u64)>, f64, u64) {
        let buckets = self
            .bounds
            .iter()
            .zip(&self.counts)
            .map(|(b, c)| (*b, c.load(Ordering::Relaxed)))
            .collect();
        (
            buckets,
            self.sum_micros.load(Ordering::Relaxed) as f64 / 1_000_000.0,
            self.count.load(Ordering::Relaxed),
        )
    }
}

/// One point of the dashboard's history.
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct Sample {
    /// Microseconds since the epoch.
    pub at: i64,
    pub queued: usize,
    pub running: i64,
    pub engines_busy: usize,
}

/// The last hour of samples at `SAMPLE_INTERVAL`.
#[derive(Default)]
pub struct Samples {
    inner: Mutex<VecDeque<Sample>>,
}

pub const SAMPLE_INTERVAL: Duration = Duration::from_secs(5);
const SAMPLE_CAPACITY: usize = 720;

impl Samples {
    pub fn push(&self, sample: Sample) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.len() == SAMPLE_CAPACITY {
            inner.pop_front();
        }
        inner.push_back(sample);
    }

    pub fn all(&self) -> Vec<Sample> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .cloned()
            .collect()
    }
}

pub fn sample(state: &AppState) -> Sample {
    // Only the running total is needed here; the projection avoids building the
    // serialised aggregate (and its per-flow map) every sample.
    let runs = state.index.runs_by_state(None);
    Sample {
        at: now_micros(),
        queued: state.supervisor.queue_len(),
        running: runs.get(&StateType::Running).copied().unwrap_or(0),
        // Counted under the lock, not by filtering a serialised snapshot: the
        // supervisor's mutex is contended by every enqueue and poll.
        engines_busy: state.supervisor.engines_busy_count(),
    }
}

/// Take one sample now and then every `SAMPLE_INTERVAL` until shutdown.
pub async fn sample_loop(state: Arc<AppState>, mut shutdown: watch::Receiver<bool>) {
    state.samples.push(sample(&state));
    loop {
        tokio::select! {
            _ = tokio::time::sleep(SAMPLE_INTERVAL) => {}
            _ = shutdown.changed() => { if *shutdown.borrow() { return; } }
        }
        state.samples.push(sample(&state));
    }
}

fn escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

fn header(out: &mut String, name: &str, kind: &str, help: &str) {
    let _ = writeln!(out, "# HELP {name} {help}");
    let _ = writeln!(out, "# TYPE {name} {kind}");
}

fn line(out: &mut String, name: &str, labels: &[(&str, &str)], value: impl std::fmt::Display) {
    if labels.is_empty() {
        let _ = writeln!(out, "{name} {value}");
        return;
    }
    let rendered: Vec<String> = labels
        .iter()
        .map(|(k, v)| format!("{k}=\"{}\"", escape(v)))
        .collect();
    let _ = writeln!(out, "{name}{{{}}} {value}", rendered.join(","));
}

fn histogram(
    out: &mut String,
    name: &str,
    help: &str,
    buckets: &[(f64, u64)],
    sum: f64,
    count: u64,
) {
    header(out, name, "histogram", help);
    for (bound, n) in buckets {
        let le = if bound.fract() == 0.0 {
            format!("{}", *bound as i64)
        } else {
            format!("{bound}")
        };
        let _ = writeln!(out, "{name}_bucket{{le=\"{le}\"}} {n}");
    }
    let _ = writeln!(out, "{name}_bucket{{le=\"+Inf\"}} {count}");
    let _ = writeln!(out, "{name}_sum {sum}");
    let _ = writeln!(out, "{name}_count {count}");
}

/// The whole scrape body.
/// One `cereyan_flow_runs` series: a flow's project and name, a state, and the
/// count for that pair.
///
/// The counts are keyed by flow id as a string, so each key is parsed and looked
/// up in `by_id` — a map built once, rather than a scan of `flows` per counted
/// flow, which made this quadratic in the number of flows.
///
/// Ordered by the string key then by state, which is the order the series have
/// always been emitted in. A key that is not an id, or an id with no row, is
/// skipped: a count can outlive the flow it names.
fn flow_run_series(
    flows: &[cereyan_store::FlowLabel],
    counts_flows: &std::collections::HashMap<String, std::collections::HashMap<String, i64>>,
) -> Vec<(String, String, String, i64)> {
    let by_id: std::collections::HashMap<i64, &cereyan_store::FlowLabel> =
        flows.iter().map(|f| (f.id, f)).collect();
    let mut per_flow: Vec<(&String, &std::collections::HashMap<String, i64>)> =
        counts_flows.iter().collect();
    per_flow.sort_by(|a, b| a.0.cmp(b.0));
    let mut out = Vec::new();
    for (flow_id, per_state) in per_flow {
        let Some(flow) = flow_id.parse::<i64>().ok().and_then(|id| by_id.get(&id)) else {
            continue;
        };
        let mut entries: Vec<(&String, &i64)> = per_state.iter().collect();
        entries.sort();
        for (state_type, n) in entries {
            out.push((
                flow.project.clone(),
                flow.name.clone(),
                state_type.clone(),
                *n,
            ));
        }
    }
    out
}

/// The two resource gauge lines, one pair per resource row.
///
/// Takes the rows rather than the supervisor so it can be checked against the
/// serialised-object read-back it replaced — including that a total of zero here
/// is a resource's own zero and not a failed lookup.
fn resource_lines(out: &mut String, rows: &[crate::supervisor::ResourceRow]) {
    for r in rows {
        line(out, "cereyan_resource_total", &[("resource", &r.name)], r.total);
        line(out, "cereyan_resource_used", &[("resource", &r.name)], r.used);
    }
}

pub fn render(state: &AppState) -> String {
    let mut out = String::with_capacity(4096);
    let counts = state.index.counts(None);
    // Labels only: three plain columns. `list_flows` would also parse every
    // flow's tags, parameter schema and options, and a scrape needs none of them.
    let flows = state.store.flow_labels().unwrap_or_default();

    header(&mut out, "cereyan_info", "gauge", "Build information.");
    line(
        &mut out,
        "cereyan_info",
        &[("version", &state.config.version)],
        1,
    );
    header(
        &mut out,
        "cereyan_uptime_seconds",
        "gauge",
        "Seconds since the server started.",
    );
    line(
        &mut out,
        "cereyan_uptime_seconds",
        &[],
        (now_micros() - state.started_at) / 1_000_000,
    );

    header(&mut out, "cereyan_runs", "gauge", "Runs by state type.");
    let mut states: Vec<(&String, &i64)> = counts.runs.iter().collect();
    states.sort();
    for (state_type, n) in states {
        line(&mut out, "cereyan_runs", &[("state", state_type)], n);
    }
    header(
        &mut out,
        "cereyan_flow_runs",
        "gauge",
        "Runs by flow and state type.",
    );
    for (project, name, state_type, n) in flow_run_series(&flows, &counts.flows) {
        line(
            &mut out,
            "cereyan_flow_runs",
            &[("project", &project), ("flow", &name), ("state", &state_type)],
            n,
        );
    }
    header(
        &mut out,
        "cereyan_task_runs",
        "gauge",
        "Task runs by state type.",
    );
    let mut tasks: Vec<(&String, &i64)> = counts.task_runs.iter().collect();
    tasks.sort();
    for (state_type, n) in tasks {
        line(&mut out, "cereyan_task_runs", &[("state", state_type)], n);
    }
    header(
        &mut out,
        "cereyan_active_runs",
        "gauge",
        "Non-terminal runs held in memory.",
    );
    line(&mut out, "cereyan_active_runs", &[], counts.active);

    header(
        &mut out,
        "cereyan_queue_depth",
        "gauge",
        "Runs waiting for an engine.",
    );
    line(
        &mut out,
        "cereyan_queue_depth",
        &[],
        state.supervisor.queue_len(),
    );
    let engines = state.supervisor.engines_snapshot();
    let busy = engines
        .iter()
        .filter(|e| !e["current_run"].is_null())
        .count();
    header(
        &mut out,
        "cereyan_engines",
        "gauge",
        "Engine processes by status.",
    );
    line(&mut out, "cereyan_engines", &[("status", "busy")], busy);
    line(
        &mut out,
        "cereyan_engines",
        &[("status", "idle")],
        engines.len() - busy,
    );
    header(
        &mut out,
        "cereyan_engines_max",
        "gauge",
        "Size of the engine pool.",
    );
    line(
        &mut out,
        "cereyan_engines_max",
        &[],
        state.supervisor.max_engines(),
    );

    header(
        &mut out,
        "cereyan_resource_total",
        "gauge",
        "Resource totals from [resources].",
    );
    header(
        &mut out,
        "cereyan_resource_used",
        "gauge",
        "Resource units in use by running runs.",
    );
    resource_lines(&mut out, &state.supervisor.resource_rows());

    header(
        &mut out,
        "cereyan_rule_firings_total",
        "counter",
        "Times each rule has fired.",
    );
    for rule in state.rules.all().iter() {
        line(
            &mut out,
            "cereyan_rule_firings_total",
            &[("rule", &rule.name)],
            rule.fire_count,
        );
    }
    header(
        &mut out,
        "cereyan_schedules",
        "gauge",
        "Schedules across every flow.",
    );
    line(
        &mut out,
        "cereyan_schedules",
        &[],
        state
            .store
            .list_schedules(None)
            .map(|s| s.len())
            .unwrap_or(0),
    );

    let (db, wal) = state.store.file_sizes();
    header(
        &mut out,
        "cereyan_database_bytes",
        "gauge",
        "Size of db.sqlite.",
    );
    line(&mut out, "cereyan_database_bytes", &[], db);
    header(
        &mut out,
        "cereyan_wal_bytes",
        "gauge",
        "Size of the write-ahead log.",
    );
    line(&mut out, "cereyan_wal_bytes", &[], wal);
    header(
        &mut out,
        "cereyan_store_commits_total",
        "counter",
        "Writer transactions committed.",
    );
    line(
        &mut out,
        "cereyan_store_commits_total",
        &[],
        state.store.commit_count(),
    );
    header(
        &mut out,
        "cereyan_store_write_queue",
        "gauge",
        "Writes waiting for the writer thread.",
    );
    line(
        &mut out,
        "cereyan_store_write_queue",
        &[],
        state.store.write_queue_len(),
    );

    let (b, s, c) = state.index.start_delay.snapshot();
    histogram(
        &mut out,
        "cereyan_schedule_start_delay_seconds",
        "Seconds from a run's scheduled time to its start.",
        &b,
        s,
        c,
    );
    let (b, s, c) = state.index.resource_wait.snapshot();
    histogram(
        &mut out,
        "cereyan_resource_wait_seconds",
        "Seconds runs spent waiting for a resource.",
        &b,
        s,
        c,
    );
    let (b, s, c) = state.store.commit_stats();
    histogram(
        &mut out,
        "cereyan_store_commit_seconds",
        "Writer commit latency.",
        &b,
        s,
        c,
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn histogram_is_cumulative() {
        let h = Histogram::new(&[1.0, 10.0]);
        h.observe(0.5);
        h.observe(5.0);
        h.observe(50.0);
        let (buckets, sum, count) = h.snapshot();
        assert_eq!(buckets, vec![(1.0, 1), (10.0, 2)]);
        assert_eq!(count, 3);
        assert!((sum - 55.5).abs() < 1e-6);
        let mut out = String::new();
        histogram(&mut out, "x_seconds", "help", &buckets, sum, count);
        assert!(out.contains("x_seconds_bucket{le=\"1\"} 1\n"));
        assert!(out.contains("x_seconds_bucket{le=\"+Inf\"} 3\n"));
        assert!(out.contains("x_seconds_count 3\n"));
    }

    #[test]
    fn labels_are_escaped() {
        let mut out = String::new();
        line(&mut out, "m", &[("a", "q\"x\\y\nz")], 1);
        assert_eq!(out, "m{a=\"q\\\"x\\\\y\\nz\"} 1\n");
    }
}

#[cfg(test)]
mod flow_series_tests {
    use super::*;
    use std::collections::HashMap;

    fn flow(id: i64, project: &str, name: &str) -> cereyan_store::FlowLabel {
        cereyan_store::FlowLabel {
            id,
            project: project.into(),
            name: name.into(),
        }
    }

    fn counts(entries: &[(&str, &[(&str, i64)])]) -> HashMap<String, HashMap<String, i64>> {
        entries
            .iter()
            .map(|(id, states)| {
                (
                    id.to_string(),
                    states
                        .iter()
                        .map(|(s, n)| (s.to_string(), *n))
                        .collect::<HashMap<String, i64>>(),
                )
            })
            .collect()
    }

    /// The pre-change algorithm, kept as the oracle: a scan per counted flow.
    fn series_the_old_way(
        flows: &[cereyan_store::FlowLabel],
        counts_flows: &HashMap<String, HashMap<String, i64>>,
    ) -> Vec<(String, String, String, i64)> {
        let mut per_flow: Vec<(&String, &HashMap<String, i64>)> = counts_flows.iter().collect();
        per_flow.sort_by(|a, b| a.0.cmp(b.0));
        let mut out = Vec::new();
        for (flow_id, per_state) in per_flow {
            let Some(flow) = flow_id
                .parse::<i64>()
                .ok()
                .and_then(|id| flows.iter().find(|f| f.id == id))
            else {
                continue;
            };
            let mut entries: Vec<(&String, &i64)> = per_state.iter().collect();
            entries.sort();
            for (state_type, n) in entries {
                out.push((
                    flow.project.clone(),
                    flow.name.clone(),
                    state_type.clone(),
                    *n,
                ));
            }
        }
        out
    }

    #[test]
    fn the_series_match_the_previous_implementation() {
        let flows = vec![
            flow(1, "p", "etl"),
            flow(2, "p", "billing"),
            flow(3, "q", "rollup"),
            flow(10, "p", "ten"),
        ];
        // Several states per flow, out of order, plus a flow with one.
        let c = counts(&[
            ("1", &[("Failed", 2), ("Completed", 5)]),
            ("2", &[("Running", 1)]),
            ("10", &[("Pending", 7)]),
        ]);
        assert_eq!(flow_run_series(&flows, &c), series_the_old_way(&flows, &c));
    }

    #[test]
    fn a_flows_series_carries_its_project_and_name() {
        let flows = vec![flow(7, "warehouse", "load")];
        let c = counts(&[("7", &[("Completed", 3)])]);
        assert_eq!(
            flow_run_series(&flows, &c),
            vec![(
                "warehouse".to_string(),
                "load".to_string(),
                "Completed".to_string(),
                3
            )]
        );
    }

    #[test]
    fn a_flow_with_several_states_gets_one_series_each() {
        let flows = vec![flow(1, "p", "etl")];
        let c = counts(&[("1", &[("Failed", 2), ("Completed", 5), ("Running", 1)])]);
        let got = flow_run_series(&flows, &c);
        assert_eq!(got.len(), 3);
        // Ordered by state name.
        let states: Vec<&str> = got.iter().map(|r| r.2.as_str()).collect();
        assert_eq!(states, vec!["Completed", "Failed", "Running"]);
        let counts: Vec<i64> = got.iter().map(|r| r.3).collect();
        assert_eq!(counts, vec![5, 2, 1]);
    }

    /// A count can outlive the flow it names, and a non-numeric key is possible
    /// if the index's key type ever changes. Both must be skipped, not panic.
    #[test]
    fn a_count_with_no_flow_row_is_skipped() {
        let flows = vec![flow(1, "p", "etl")];
        let c = counts(&[("1", &[("Completed", 1)]), ("999", &[("Completed", 4)])]);
        let got = flow_run_series(&flows, &c);
        assert_eq!(got.len(), 1, "the orphan count should be skipped");
        assert_eq!(got[0].1, "etl");
    }

    #[test]
    fn a_key_that_is_not_an_id_is_skipped() {
        let flows = vec![flow(1, "p", "etl")];
        let c = counts(&[("1", &[("Completed", 1)]), ("not-a-number", &[("Failed", 9)])]);
        let got = flow_run_series(&flows, &c);
        assert_eq!(got.len(), 1, "a non-numeric key should be skipped");
        assert_eq!(got[0].2, "Completed");
    }

    #[test]
    fn no_counts_yields_no_series() {
        assert!(flow_run_series(&[flow(1, "p", "etl")], &HashMap::new()).is_empty());
    }

    #[test]
    fn counts_with_no_flows_yield_no_series() {
        let c = counts(&[("1", &[("Completed", 1)])]);
        assert!(flow_run_series(&[], &c).is_empty());
    }

    /// The ordering is by the *string* key, so "10" precedes "9". That is
    /// pre-existing and pinned here so a well-meaning reordering shows up.
    #[test]
    fn series_are_ordered_by_the_string_key() {
        let flows = vec![flow(2, "p", "b"), flow(10, "p", "j")];
        let c = counts(&[("2", &[("Completed", 1)]), ("10", &[("Completed", 1)])]);
        let got = flow_run_series(&flows, &c);
        let names: Vec<&str> = got.iter().map(|r| r.1.as_str()).collect();
        assert_eq!(names, vec!["j", "b"], "string order puts 10 before 2");
    }
}

#[cfg(test)]
mod resource_line_tests {
    use super::*;
    use crate::supervisor::ResourceRow;

    fn row(name: &str, total: f64, used: f64, pattern: Option<&str>) -> ResourceRow {
        ResourceRow {
            name: name.into(),
            total,
            used,
            pattern: pattern.map(|p| p.into()),
        }
    }

    /// The lines the render emitted before this change: it built a JSON object,
    /// navigated it by string key, and fell back to `0.0` for anything it could
    /// not read. This is the oracle.
    fn lines_the_old_way(rows: &[ResourceRow]) -> String {
        let v: serde_json::Value = serde_json::Value::Object(
            rows.iter()
                .map(|r| {
                    let mut e = serde_json::json!({"total": r.total, "used": r.used});
                    if let Some(p) = &r.pattern {
                        e["pattern"] = serde_json::Value::String(p.clone());
                    }
                    (r.name.clone(), e)
                })
                .collect(),
        );
        let mut out = String::new();
        if let Some(resources) = v.as_object() {
            for (name, v) in resources {
                line(
                    &mut out,
                    "cereyan_resource_total",
                    &[("resource", name)],
                    v["total"].as_f64().unwrap_or(0.0),
                );
                line(
                    &mut out,
                    "cereyan_resource_used",
                    &[("resource", name)],
                    v["used"].as_f64().unwrap_or(0.0),
                );
            }
        }
        out
    }

    #[test]
    fn the_lines_match_the_previous_implementation() {
        // Sorted by name, which is what `resource_rows` guarantees. The old path
        // sorted the names itself; the new one inherits the order, so this is the
        // only order the two can actually meet in.
        let rows = vec![
            row("cpu", 4.0, 1.5, None),
            row("exact", 7.0, 0.0, None),
            row("gpu-0", 2.0, 2.0, Some("gpu-*")),
            row("undeclared", 1.0, 0.25, None),
        ];
        let mut out = String::new();
        resource_lines(&mut out, &rows);
        assert_eq!(out, lines_the_old_way(&rows));
    }

    #[test]
    fn a_zero_total_is_emitted_as_zero() {
        // The point of the change: this must be a real zero on the wire, and the
        // old fallback also produced a zero — so the test asserts the value is
        // present and correctly labelled, not merely that it is absent.
        let mut out = String::new();
        resource_lines(&mut out, &[row("idle", 0.0, 0.0, None)]);
        assert!(
            out.contains("cereyan_resource_total{resource=\"idle\"} 0"),
            "a zero total must be emitted, not skipped: {out}"
        );
        assert!(
            out.contains("cereyan_resource_used{resource=\"idle\"} 0"),
            "a zero usage must be emitted: {out}"
        );
    }

    #[test]
    fn each_resource_gets_a_total_and_a_usage_line() {
        let mut out = String::new();
        resource_lines(
            &mut out,
            &[
                row("cpu", 4.0, 1.5, None),
                row("memory", 1024.0, 512.0, None),
            ],
        );
        for name in ["cpu", "memory"] {
            assert!(
                out.contains(&format!("cereyan_resource_total{{resource=\"{name}\"}}")),
                "no total line for {name}: {out}"
            );
            assert!(
                out.contains(&format!("cereyan_resource_used{{resource=\"{name}\"}}")),
                "no used line for {name}: {out}"
            );
        }
        assert!(out.contains("cereyan_resource_total{resource=\"cpu\"} 4"), "{out}");
        assert!(out.contains("cereyan_resource_used{resource=\"cpu\"} 1.5"), "{out}");
        assert!(out.contains("cereyan_resource_total{resource=\"memory\"} 1024"), "{out}");
    }

    /// `resource_lines` emits in the order it is given; the sorted guarantee lives
    /// in `resource_rows`, which sorts the union of declared and in-use names.
    /// The defect this change exists for, made visible.
    ///
    /// The old path read each value back out of a serialised object with
    /// `as_f64().unwrap_or(0.0)`. A value it could not read became a **zero** —
    /// and for a resource gauge a zero reads as an idle resource, so the failure
    /// looked like data rather than like a bug. `NaN` stands in for "a value the
    /// lookup could not read"; a resource total is not normally NaN, but the
    /// shape of the failure is what matters, and both paths can be compared on it.
    #[test]
    fn a_value_the_old_path_could_not_read_reported_zero() {
        let rows = [row("broken", f64::NAN, 0.0, None)];
        let mut new_out = String::new();
        resource_lines(&mut new_out, &rows);
        let old_out = lines_the_old_way(&rows);

        // The old path substituted a zero.
        assert!(
            old_out.contains("cereyan_resource_total{resource=\"broken\"} 0\n"),
            "the old path should have reported a zero: {old_out}"
        );
        // The new path reports the row's own value, whatever it is.
        assert_ne!(
            new_out, old_out,
            "the new path must not substitute a value it was handed"
        );
        // Scoped to the total line: the *usage* of this row really is zero, and
        // emitting that is correct.
        let total_line = new_out
            .lines()
            .find(|l| l.starts_with("cereyan_resource_total"))
            .expect("a total line");
        assert!(
            total_line.contains("NaN"),
            "the new path must report the value it was handed: {total_line}"
        );
        assert_eq!(
            total_line, "cereyan_resource_total{resource=\"broken\"} NaN",
            "the total line carries the row's own value"
        );
    }

    #[test]
    fn the_lines_follow_the_order_of_the_rows() {
        let rows = vec![row("b", 2.0, 0.0, None), row("a", 1.0, 0.0, None)];
        let mut out = String::new();
        resource_lines(&mut out, &rows);
        let order: Vec<&str> = out
            .lines()
            .filter_map(|l| l.split('"').nth(1))
            .step_by(2)
            .collect();
        assert_eq!(order, vec!["b", "a"], "the given order is preserved");
    }

    #[test]
    fn no_resources_emits_no_lines() {
        let mut out = String::new();
        resource_lines(&mut out, &[]);
        assert!(out.is_empty());
    }
}
