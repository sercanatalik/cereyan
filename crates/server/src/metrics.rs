//! Prometheus text exposition, rendered by hand: fixed-bucket histograms over
//! atomics, a five-second sample ring for the dashboard, and the scrape body.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cereyan_core::now_micros;
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
    let counts = state.index.counts(None);
    let engines = state.supervisor.engines_snapshot();
    Sample {
        at: now_micros(),
        queued: state.supervisor.queue_len(),
        running: counts.runs.get("Running").copied().unwrap_or(0),
        engines_busy: engines
            .iter()
            .filter(|e| !e["current_run"].is_null())
            .count(),
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
pub fn render(state: &AppState) -> String {
    let mut out = String::with_capacity(4096);
    let counts = state.index.counts(None);
    let flows = state.store.list_flows(None).unwrap_or_default();

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
    let mut per_flow: Vec<(&String, &std::collections::HashMap<String, i64>)> =
        counts.flows.iter().collect();
    per_flow.sort_by(|a, b| a.0.cmp(b.0));
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
            line(
                &mut out,
                "cereyan_flow_runs",
                &[
                    ("project", &flow.project),
                    ("flow", &flow.name),
                    ("state", state_type),
                ],
                n,
            );
        }
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
        state.supervisor.max_engines,
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
    if let Some(resources) = state.supervisor.resources_snapshot().as_object() {
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

    header(
        &mut out,
        "cereyan_rule_firings_total",
        "counter",
        "Times each rule has fired.",
    );
    for rule in state.rules.all() {
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
