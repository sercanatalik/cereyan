//! The queue as the Queue page shows it: the processors (engine pool), the
//! runs in line in dispatch order, and the runs about to join the line.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::State;
use axum::Json;
use cereyan_core::Run;
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

fn trigger(run: &Run) -> String {
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
            st.store.get_runs(&ids)?,
            // Runs for later wait on the timer, not in the queue, until they are nearly due.
            st.store.scheduled_between(now, until, JOINING_LIMIT)?,
        ))
    })
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))??;
    let mut runs: HashMap<i64, Run> = runs.into_iter().map(|r| (r.id, r)).collect();
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
            let kind = if run.state.name == "AwaitingRetry" {
                "retry"
            } else if run.created_by == "continuous" {
                "continuous"
            } else if run.schedule_id.is_some() {
                "schedule"
            } else {
                "delayed"
            };
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
            load: load_per_cpu(sup.cpu_cap),
            items,
        },
        more: total.saturating_sub(in_line.len()),
        in_line,
        joining,
        paused_loops,
    }))
}
