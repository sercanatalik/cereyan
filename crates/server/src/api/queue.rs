//! The queue as the Queue page shows it: the processors (engine pool), the
//! runs in line in dispatch order, and the runs about to join the line.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::State;
use axum::Json;
use serde::Serialize;

use super::error::{ApiError, ApiResult};
use crate::state::AppState;
use crate::supervisor::EngineView;

/// Runs listed in line; the rest are counted in `more`.
const IN_LINE_LIMIT: usize = 500;
/// Runs listed as joining the line.
const JOINING_LIMIT: usize = 50;
/// How far ahead `joining` looks.
const JOINING_HORIZON_SECS: i64 = 3600;

#[derive(Serialize, utoipa::ToSchema)]
pub struct Processors {
    /// Engines that may run at once.
    pub count: usize,
    /// The most `count` may be: this machine's CPU count.
    pub cap: usize,
    /// Where `count` came from: `flag`, `toml`, `settings`, or `default`.
    pub source: String,
    /// Processors per host: the server first, then each worker.
    pub hosts: Vec<HostTotals>,
    /// The one-minute load average over the CPU count (1.0 is every CPU busy);
    /// absent where the platform does not report one.
    pub load: Option<f64>,
    pub items: Vec<EngineView>,
}

/// One-minute load average per CPU, from `getloadavg`.
#[cfg(unix)]
fn load_per_cpu(cpus: usize) -> Option<f64> {
    let mut avg = [0f64; 3];
    // SAFETY: getloadavg writes at most `nelem` doubles into the buffer.
    let n = unsafe { libc::getloadavg(avg.as_mut_ptr(), 1) };
    (n >= 1).then(|| avg[0] / cpus.max(1) as f64)
}

#[cfg(not(unix))]
fn load_per_cpu(_cpus: usize) -> Option<f64> {
    None
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct HostTotals {
    pub host: String,
    /// Processors the host may run at once.
    pub count: usize,
    pub busy: usize,
    /// `online`, `draining`, or `offline`; the server is always `online`.
    pub state: String,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct InLine {
    pub run_id: i64,
    pub run_name: String,
    pub flow: String,
    pub project: String,
    pub module: String,
    /// What made the run: `schedule`, `backfill`, `rule`, `dependency`, `crash rerun`,
    /// or the run's `created_by`.
    pub trigger: String,
    pub priority: i64,
    /// 1 for the first run in line.
    pub position: usize,
    /// Microseconds since the run joined the line.
    pub waited_us: i64,
    pub can_start: bool,
    /// Why it cannot start: `resource:<name>`, `max_concurrent`,
    /// `backfill concurrency`, or `no processor`.
    pub reason: Option<String>,
    /// Later runs dispatched while this one could not start.
    pub overtaken_by: u32,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct Joining {
    pub run_id: i64,
    pub run_name: String,
    pub flow: String,
    pub project: String,
    /// `retry`, `continuous`, `schedule`, or `delayed`.
    pub kind: String,
    /// The schedule that made the run, when one did.
    pub schedule_id: Option<i64>,
    /// When it joins the line, microseconds since the epoch.
    pub at: i64,
}

/// A continuous schedule that is paused: its loop has no run waiting.
#[derive(Serialize, utoipa::ToSchema)]
pub struct PausedLoop {
    pub schedule_id: i64,
    pub flow: String,
    pub project: String,
    /// `paused` by a person, or `disabled` by the flow's `disable_after`.
    pub reason: Option<String>,
    /// When a `disabled` loop resumes on its own, microseconds since the epoch.
    pub until: Option<i64>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct QueueView {
    pub processors: Processors,
    pub in_line: Vec<InLine>,
    /// Runs in line past the listed ones.
    pub more: usize,
    pub joining: Vec<Joining>,
    /// Continuous schedules that are paused, so the page can resume them.
    pub paused_loops: Vec<PausedLoop>,
}

/// Why a nearly-due run is where it is.
///
/// Extracted so it can be tested directly: `render` needs an `AppState`, and this
/// classification is the view's semantics rather than its plumbing.
fn joining_kind(run: &cereyan_store::QueueRun) -> &'static str {
    if run.state_name == "AwaitingRetry" {
        "retry"
    } else if run.created_by == "continuous" {
        "continuous"
    } else if run.schedule_id.is_some() {
        "schedule"
    } else {
        "delayed"
    }
}

fn trigger(run: &cereyan_store::QueueRun) -> String {
    if run.backfill_id.is_some() {
        "backfill".into()
    } else if run.schedule_id.is_some() {
        "schedule".into()
    } else if run.created_by.starts_with("rule:") {
        "rule".into()
    } else if run.created_by.starts_with("run:") {
        "dependency".into()
    } else if run.created_by.starts_with("crash:") {
        "crash rerun".into()
    } else {
        run.created_by.clone()
    }
}

#[utoipa::path(get, path = "/api/queue", responses((status = 200, body = QueueView)))]
pub async fn get_queue(State(state): State<Arc<AppState>>) -> ApiResult<Json<QueueView>> {
    let sup = &state.supervisor;
    let (items, line, total) = sup.queue_snapshot(IN_LINE_LIMIT);
    let now = cereyan_core::now_micros();
    let until = now + JOINING_HORIZON_SECS * 1_000_000;
    let soon = sup.joining(until, JOINING_LIMIT);
    let ids: Vec<i64> = line
        .iter()
        .map(|l| l.run_id)
        .chain(soon.iter().map(|(id, _)| *id))
        .collect();
    let st = state.clone();
    let (runs, scheduled) = tokio::task::spawn_blocking(move || {
        Ok::<_, cereyan_store::StoreError>((
            // The nine fields the view shows, not whole runs: up to 550 of these
            // on a page polled every five seconds, and `RUN_COLUMNS` would run a
            // correlated task-count aggregate and decode four JSON columns for
            // each one of them.
            st.store.queue_runs(&ids)?,
            // Runs for later wait on the timer, not in the queue, until they are nearly due.
            st.store.scheduled_queue_runs(now, until, JOINING_LIMIT)?,
        ))
    })
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))??;
    let mut runs: HashMap<i64, cereyan_store::QueueRun> =
        runs.into_iter().map(|r| (r.id, r)).collect();
    let mut soon = soon;
    for run in scheduled {
        if let std::collections::hash_map::Entry::Vacant(slot) = runs.entry(run.id) {
            soon.push((run.id, run.scheduled_time.unwrap_or(until)));
            slot.insert(run);
        }
    }
    soon.sort_by_key(|(id, at)| (*at, *id));
    soon.truncate(JOINING_LIMIT);
    let in_line = line
        .into_iter()
        .filter_map(|l| {
            let run = runs.get(&l.run_id)?;
            Some(InLine {
                run_id: l.run_id,
                run_name: run.name.clone(),
                flow: run.flow_name.clone(),
                project: run.project.clone(),
                module: l.module,
                trigger: trigger(run),
                priority: l.priority,
                position: l.position,
                waited_us: (now - l.order).max(0),
                can_start: l.can_start,
                reason: l.reason,
                overtaken_by: l.overtaken_by,
            })
        })
        .collect::<Vec<_>>();
    let joining = soon
        .into_iter()
        .filter_map(|(id, at)| {
            let run = runs.get(&id)?;
            let kind = joining_kind(run);
            Some(Joining {
                run_id: id,
                run_name: run.name.clone(),
                flow: run.flow_name.clone(),
                project: run.project.clone(),
                kind: kind.into(),
                schedule_id: run.schedule_id,
                at,
            })
        })
        .collect();
    let mut loops: Vec<cereyan_core::ScheduleRow> = state
        .scheduler
        .schedules
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .values()
        .filter(|r| r.schedule.is_continuous() && !r.active)
        .cloned()
        .collect();
    loops.sort_by_key(|r| r.id);
    let flows: HashMap<i64, cereyan_core::Flow> = if loops.is_empty() {
        HashMap::new()
    } else {
        state
            .store
            .list_flows(None)?
            .into_iter()
            .map(|f| (f.id, f))
            .collect()
    };
    let paused_loops = loops
        .into_iter()
        .filter_map(|r| {
            let flow = flows.get(&r.flow_id)?;
            Some(PausedLoop {
                schedule_id: r.id,
                flow: flow.name.clone(),
                project: flow.project.clone(),
                reason: r.paused_reason,
                until: r.paused_until,
            })
        })
        .collect();
    let source = state
        .sources
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get("server.max_engines")
        .map_or_else(|| "default".into(), |s| s.source.clone());
    Ok(Json(QueueView {
        processors: Processors {
            count: sup.max_engines(),
            cap: sup.cpu_cap,
            source,
            hosts: {
                let mut hosts = vec![HostTotals {
                    host: "server".into(),
                    count: sup.max_engines(),
                    busy: items
                        .iter()
                        .filter(|e| e.host == "server" && e.run_id.is_some())
                        .count(),
                    state: "online".into(),
                }];
                for (_, name, st, processors, running, _) in sup.workers_snapshot() {
                    hosts.push(HostTotals {
                        host: name,
                        count: processors,
                        busy: running,
                        state: st,
                    });
                }
                hosts
            },
            load: load_per_cpu(sup.cpu_cap),
            items,
        },
        more: total.saturating_sub(in_line.len()),
        in_line,
        joining,
        paused_loops,
    }))
}

#[cfg(test)]
mod queue_class_tests {
    use super::*;
    use cereyan_store::QueueRun;

    fn q(
        created_by: &str,
        backfill_id: Option<i64>,
        schedule_id: Option<i64>,
        state_name: &str,
    ) -> QueueRun {
        QueueRun {
            id: 1,
            name: "r".into(),
            flow_name: "etl".into(),
            project: "p".into(),
            state_name: state_name.into(),
            created_by: created_by.into(),
            backfill_id,
            schedule_id,
            scheduled_time: Some(1),
        }
    }

    /// `trigger` classifies a run by what created it, in a fixed precedence. It
    /// changed from taking a whole `Run` to taking a projected row, and this is
    /// the invariant that the change had to preserve — every branch, plus the
    /// fall-through that reports an unrecognised creator verbatim.
    #[test]
    fn a_queued_runs_trigger_is_classified_the_same_way() {
        // A backfill wins over everything else.
        assert_eq!(
            trigger(&q("schedule", Some(3), Some(9), "Scheduled")),
            "backfill"
        );
        assert_eq!(trigger(&q("api", Some(3), None, "Scheduled")), "backfill");
        // Then a schedule.
        assert_eq!(
            trigger(&q("schedule", None, Some(9), "Scheduled")),
            "schedule"
        );
        // Then the recognised creator prefixes.
        assert_eq!(trigger(&q("rule:alert", None, None, "Scheduled")), "rule");
        assert_eq!(
            trigger(&q("run:upstream", None, None, "Scheduled")),
            "dependency"
        );
        assert_eq!(
            trigger(&q("crash:42", None, None, "Scheduled")),
            "crash rerun"
        );
        // And anything else is reported as itself.
        for other in ["api", "continuous", "retry:1", "", "Rule:x", "runx"] {
            assert_eq!(
                trigger(&q(other, None, None, "Scheduled")),
                other,
                "an unrecognised creator is reported verbatim"
            );
        }
        // The prefixes are exact: `crash:` is a rerun, `crashed` is not.
        assert_eq!(trigger(&q("crashed", None, None, "Scheduled")), "crashed");
    }

    /// `joining_kind` is a precedence chain of its own, and it reads the
    /// projected `state_name` where it used to read `run.state.name`. A run with
    /// no state name falls back to `Scheduled` in the projection, so it must not
    /// be mistaken for a retry.
    #[test]
    fn a_nearly_due_runs_kind_is_classified_the_same_way() {
        assert_eq!(
            joining_kind(&q("api", None, None, "AwaitingRetry")),
            "retry"
        );
        // A retry beats a continuous creator and a schedule.
        assert_eq!(
            joining_kind(&q("continuous", None, Some(9), "AwaitingRetry")),
            "retry"
        );
        assert_eq!(
            joining_kind(&q("api", None, Some(9), "AwaitingRetry")),
            "retry"
        );
        assert_eq!(
            joining_kind(&q("continuous", None, None, "Scheduled")),
            "continuous"
        );
        assert_eq!(
            joining_kind(&q("schedule", None, Some(9), "Scheduled")),
            "schedule"
        );
        // The distinguishing case: a continuous run *does* carry a schedule id, so
        // only the order of the two checks separates them. Without this, swapping
        // them would pass — an earlier version of this test had no case here.
        assert_eq!(
            joining_kind(&q("continuous", None, Some(9), "Scheduled")),
            "continuous",
            "a continuous run keeps its kind even though it has a schedule"
        );
        assert_eq!(joining_kind(&q("api", None, None, "Scheduled")), "delayed");
        // A backfill id does not affect the joining kind, as before.
        assert_eq!(
            joining_kind(&q("api", Some(3), None, "Scheduled")),
            "delayed"
        );
        // The empty state name a run with no transition carries, projected to
        // `Scheduled`, is not `AwaitingRetry`.
        assert_eq!(joining_kind(&q("api", None, None, "")), "delayed");
        // Any other state name is not the retry state either.
        assert_eq!(joining_kind(&q("api", None, None, "Retrying")), "delayed");
    }
}
