//! Schedules: materialize upcoming runs, dispatch them when due, mark late
//! runs, apply catch-up on start, and persist the wake-up time.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use cereyan_core::schedule::{from_micros, to_micros, CatchupPolicy, Schedule};
use cereyan_core::{
    now_micros, EventName, Flow, FlowOptions, Run, ScheduleDecl, ScheduleRow, State, StateType,
};
use cereyan_store::{CreateRun, ScheduleWrite};
use chrono::Utc;
use serde_json::json;
use tokio::sync::watch;

use crate::state::AppState;
use crate::timer::TimerEvent;

pub const LOOKAHEAD_RUNS: usize = 3;
pub const LOOKAHEAD_MIN_SECS: i64 = 3600;
pub const LOOKAHEAD_MAX: usize = 100;
pub const LATE_AFTER_SECS: i64 = 15;
pub const PERSIST_EVERY_SECS: i64 = 60;
pub const LAST_WAKEUP_KEY: &str = "scheduler.last_wakeup";
/// Engines are warmed this long before a scheduled run is due.
pub const PREWARM_SECS: i64 = 5;
/// The mark a waiting run carries in its state details when a person skipped
/// its fire: `details.skip = "user"`. `schedule_skip` is the durable record;
/// the mark only saves the Due handler a lookup and never outlives the table.
pub const SKIP_MARK: &str = "skip";
const SKIP_BY_PERSON: &str = "user";

/// Whether a waiting run belongs to a fire a person skipped.
pub fn is_marked(run: &Run) -> bool {
    run.state.details.get(SKIP_MARK).and_then(|v| v.as_str()) == Some(SKIP_BY_PERSON)
}

#[derive(Default)]
pub struct Scheduler {
    pub schedules: RwLock<HashMap<i64, ScheduleRow>>,
}

impl Scheduler {
    pub fn new() -> Scheduler {
        Scheduler::default()
    }

    /// Forget every schedule; a restart loads them again from the store.
    pub fn clear(&self) {
        self.schedules
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }

    pub fn get(&self, id: i64) -> Option<ScheduleRow> {
        self.schedules
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&id)
            .cloned()
    }

    pub fn for_flow(&self, flow_id: i64) -> Vec<ScheduleRow> {
        let mut v: Vec<ScheduleRow> = self
            .schedules
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .filter(|s| s.flow_id == flow_id)
            .cloned()
            .collect();
        v.sort_by_key(|s| s.id);
        v
    }

    fn put(&self, row: ScheduleRow) {
        self.schedules
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(row.id, row);
    }

    pub fn remove(&self, id: i64) {
        self.schedules
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id);
    }
}

/// Bring code-declared schedules of a flow in line with its declarations.
pub fn sync_code_schedules(
    state: &AppState,
    flow: &Flow,
    decls: &[ScheduleDecl],
) -> Result<(), String> {
    let existing = state
        .store
        .list_schedules(Some(flow.id))
        .map_err(|e| e.to_string())?;
    let mut seen: Vec<String> = Vec::new();
    for (i, decl) in decls.iter().enumerate() {
        let key = decl.key.clone().unwrap_or_else(|| format!("code-{i}"));
        seen.push(key.clone());
        decl.schedule.validate().map_err(|e| e.to_string())?;
        let pinned = decl.schedule.clone().with_anchor_if_missing(now_micros());
        let spec = serde_json::to_string(&pinned).map_err(|e| e.to_string())?;
        let current = existing
            .iter()
            .find(|s| s.source == "code" && s.code_key.as_deref() == Some(&key));
        match current {
            Some(row) if row.persist => {}
            Some(row) => {
                let spec = match (&decl.schedule, &row.schedule) {
                    (
                        Schedule::Interval {
                            anchor: None,
                            interval,
                            timezone,
                        },
                        Schedule::Interval {
                            anchor: Some(a), ..
                        },
                    ) => serde_json::to_string(&Schedule::Interval {
                        interval: *interval,
                        anchor: Some(*a),
                        timezone: timezone.clone(),
                    })
                    .map_err(|e| e.to_string())?,
                    _ => spec,
                };
                state
                    .store
                    .upsert_schedule(ScheduleWrite {
                        id: Some(row.id),
                        flow_id: flow.id,
                        spec,
                        catchup: decl.catchup.as_str().into(),
                        catchup_max: decl.catchup_max,
                        active: row.active,
                        source: "code".into(),
                        code_key: Some(key),
                        persist: false,
                    })
                    .map_err(|e| e.to_string())?;
            }
            None => {
                state
                    .store
                    .upsert_schedule(ScheduleWrite {
                        id: None,
                        flow_id: flow.id,
                        spec,
                        catchup: decl.catchup.as_str().into(),
                        catchup_max: decl.catchup_max,
                        active: true,
                        source: "code".into(),
                        code_key: Some(key),
                        persist: false,
                    })
                    .map_err(|e| e.to_string())?;
            }
        }
    }
    for row in existing.iter().filter(|s| s.source == "code" && !s.persist) {
        if !row
            .code_key
            .as_ref()
            .map(|k| seen.contains(k))
            .unwrap_or(false)
        {
            let _ = state.store.delete_schedule(row.id);
        }
    }
    Ok(())
}

/// Load every schedule, apply catch-up, materialize, and arm timers.
pub fn start(state: &Arc<AppState>) {
    start_inner(state, true);
}

/// After a database reset: reload every schedule and arm it from now, with
/// no catch-up for fires before the reset.
pub fn restart(state: &Arc<AppState>) {
    state.scheduler.clear();
    start_inner(state, false);
}

fn start_inner(state: &Arc<AppState>, with_catch_up: bool) {
    let rows = match state.store.list_schedules(None) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("scheduler: cannot load schedules: {e}");
            return;
        }
    };
    let last_wakeup: Option<i64> = state
        .store
        .kv_get(LAST_WAKEUP_KEY)
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok())
        .filter(|_| with_catch_up);
    let now = now_micros();
    // A disable window's resume timer lives only in memory, so it is derived
    // again from `paused_until`: ended windows resume now, open ones re-arm.
    let mut ended: Vec<(ScheduleRow, i64)> = Vec::new();
    let mut open: HashMap<i64, i64> = HashMap::new();
    for row in rows {
        state.scheduler.put(row.clone());
        if !row.active {
            if let (Some("disabled"), Some(until)) =
                (row.paused_reason.as_deref(), row.paused_until)
            {
                if until <= now {
                    ended.push((row, until));
                } else {
                    let at = open.entry(row.flow_id).or_insert(until);
                    *at = (*at).max(until);
                }
            }
            continue;
        }
        if let Some(last) = last_wakeup {
            catch_up(state, &row, last, now);
        }
        drop_lost_skips(state, &row);
        materialize(state, row.id);
    }
    let mut enabled: Vec<i64> = Vec::new();
    for (row, until) in &ended {
        resume(state, row.id);
        // Fires inside the window were suppressed, not missed: catch up only
        // what the outage missed after it ended.
        if let Some(last) = last_wakeup {
            catch_up(state, row, last.max(*until), now);
        }
        if !enabled.contains(&row.flow_id) {
            enabled.push(row.flow_id);
        }
    }
    for flow_id in enabled {
        let _ = state.record_engine_event(EventName::FlowEnabled, None, Some(flow_id), json!({}));
    }
    for (flow_id, until) in open {
        state.timer.push(until, TimerEvent::ResumeFlow(flow_id));
    }
    // Existing Scheduled runs with a scheduled time (materialized earlier) need timers.
    for run in state.index.active_runs() {
        if run.state.state_type == StateType::Scheduled {
            if let Some(stored) = state.store.get_run(run.id).ok().flatten() {
                if let Some(due) = stored.scheduled_time {
                    arm_run(state, run.id, due, is_marked(&stored));
                }
            }
        }
    }
    state
        .timer
        .push(now + PERSIST_EVERY_SECS * 1_000_000, TimerEvent::Persist);
    let _ = state.store.kv_set(LAST_WAKEUP_KEY, &now.to_string());
}

fn catch_up(state: &Arc<AppState>, row: &ScheduleRow, last: i64, now: i64) {
    let start = from_micros(last);
    let end = from_micros(now);
    let max = (row.catchup_max.max(1) as usize).saturating_add(1000);
    let fires = match row.schedule.fires_between(start, end, max) {
        Ok(f) => f,
        Err(_) => return,
    };
    // A skipped fire stays skipped whatever the policy.
    let skips: HashSet<i64> = state
        .store
        .list_skips(row.id)
        .unwrap_or_default()
        .into_iter()
        .collect();
    // A fire the look-ahead already materialised is not missed, whatever state
    // its run reached. Without this a machine that slept through its own
    // look-ahead gave every one of those fires a second run.
    let taken: HashSet<i64> = state
        .store
        .fire_times_of_schedule(row.id, last, now)
        .unwrap_or_default()
        .into_iter()
        .collect();
    let fires: Vec<_> = fires
        .into_iter()
        .filter(|f| {
            let at = to_micros(*f);
            !skips.contains(&at) && !taken.contains(&at)
        })
        .collect();
    if fires.is_empty() {
        return;
    }
    let (chosen, dropped): (Vec<_>, usize) = match row.catchup {
        CatchupPolicy::Skip => (Vec::new(), fires.len()),
        CatchupPolicy::Latest => (vec![*fires.last().unwrap()], fires.len() - 1),
        CatchupPolicy::All => {
            let keep = (row.catchup_max.max(0) as usize).min(fires.len());
            let start_idx = fires.len() - keep;
            (fires[start_idx..].to_vec(), start_idx)
        }
    };
    let Some(flow) = state.store.get_flow(row.flow_id).ok().flatten() else {
        return;
    };
    for fire in &chosen {
        let scheduled = to_micros(*fire);
        if let Some(run) = create_scheduled_run(state, &flow, row, scheduled, "catchup", false) {
            crate::dispatch::enqueue_run(state, &run, &flow, None);
        }
    }
    let _ = state.record_engine_event(
        EventName::ScheduleCatchup,
        None,
        Some(row.flow_id),
        json!({
            "schedule_id": row.id, "policy": row.catchup.as_str(), "missed": fires.len(),
            "created": chosen.len(), "dropped": dropped,
        }),
    );
}

fn create_scheduled_run(
    state: &Arc<AppState>,
    flow: &Flow,
    row: &ScheduleRow,
    scheduled: i64,
    created_by: &str,
    skip: bool,
) -> Option<Run> {
    let options = FlowOptions::from_map(&flow.options);
    let mut params = serde_json::Map::new();
    if let Some(props) = flow
        .parameter_schema
        .get("properties")
        .and_then(|p| p.as_object())
    {
        for (k, prop) in props {
            if let Some(d) = prop.get("default") {
                params.insert(k.clone(), d.clone());
            }
        }
    }
    let name = format!(
        "{}-{}",
        flow.name,
        from_micros(scheduled).format("%Y%m%dT%H%M%S")
    );
    let name = if state.store.run_name_exists(&name).unwrap_or(false) {
        format!("{name}-{}", row.id)
    } else {
        name
    };
    let mut initial = State::new(StateType::Scheduled).with_timestamp(now_micros());
    if skip {
        initial
            .details
            .insert(SKIP_MARK.into(), json!(SKIP_BY_PERSON));
    }
    let (run_id, _) = state
        .store
        .create_run_full(CreateRun {
            flow_id: flow.id,
            name,
            parameters: serde_json::to_string(&params).unwrap_or_else(|_| "{}".into()),
            tags: serde_json::to_string(&flow.tags).unwrap_or_else(|_| "[]".into()),
            created_by: created_by.into(),
            initial_state: Some(initial),
            schedule_id: Some(row.id),
            scheduled_time: Some(scheduled),
            priority: options.priority,
            ..Default::default()
        })
        .ok()?;
    let run = state.store.get_run(run_id).ok().flatten()?;
    state
        .index
        .insert_run(&run, crate::supervisor::EngineKey::from_flow(flow), false);
    state.run_created(&run);
    Some(run)
}

/// Ensure the schedule has enough future Scheduled runs and arm their timers.
pub fn materialize(state: &Arc<AppState>, schedule_id: i64) {
    let Some(row) = state.scheduler.get(schedule_id) else {
        return;
    };
    if !row.active {
        return;
    }
    let Some(flow) = state.store.get_flow(row.flow_id).ok().flatten() else {
        return;
    };
    let now = now_micros();
    let all_skips = state.store.list_skips(row.id).unwrap_or_default();
    if all_skips.first().is_some_and(|t| *t <= now) {
        // A passed skip is history: its run, if one was made, already ended Skipped.
        let _ = state.store.delete_skips_before(row.id, now);
    }
    let skips: HashSet<i64> = all_skips.into_iter().filter(|t| *t > now).collect();
    let mut existing = state
        .store
        .future_runs_of_schedule(row.id, now)
        .unwrap_or_default();
    let mut cursor = existing
        .iter()
        .filter_map(|r| r.scheduled_time)
        .max()
        .map(from_micros)
        .unwrap_or_else(Utc::now);
    let horizon = Utc::now() + chrono::Duration::seconds(LOOKAHEAD_MIN_SECS);
    // Skipped fires do not count: the look-ahead keeps LOOKAHEAD_RUNS runs that
    // will start and extends past skips, bounded by LOOKAHEAD_MAX in all.
    let mut starting = existing.iter().filter(|r| !is_marked(r)).count();
    let mut created = 0;
    while existing.len() < LOOKAHEAD_MAX && (starting < LOOKAHEAD_RUNS || cursor < horizon) {
        let next = match row.schedule.next_after(cursor) {
            Ok(Some(n)) => n,
            _ => break,
        };
        if next <= cursor {
            break;
        }
        cursor = next;
        let scheduled = to_micros(next);
        let skip = skips.contains(&scheduled);
        if let Some(run) = create_scheduled_run(state, &flow, &row, scheduled, "schedule", skip) {
            if !skip {
                starting += 1;
            }
            existing.push(run);
            created += 1;
        } else {
            break;
        }
        if created > LOOKAHEAD_MAX {
            break;
        }
    }
    for run in &existing {
        if let Some(due) = run.scheduled_time {
            arm_run(state, run.id, due, is_marked(run));
        }
    }
    // Wake again when the earliest future run fires so the look-ahead is kept.
    if let Some(first) = existing.iter().filter_map(|r| r.scheduled_time).min() {
        state.timer.remove_schedule_events(row.id);
        state.timer.push(first + 1_000, TimerEvent::Fire(row.id));
    }
    let next_fire = existing
        .iter()
        .filter(|r| !is_marked(r))
        .filter_map(|r| r.scheduled_time)
        .min();
    let mut updated = row.clone();
    updated.next_fire = next_fire;
    updated.skipped = skips.len() as i64;
    state.scheduler.put(updated);
}

fn arm_run(state: &Arc<AppState>, run_id: i64, due: i64, skipped: bool) {
    state.timer.remove_run_events(run_id);
    state.timer.push(due, TimerEvent::Due(run_id));
    // A skipped fire's run never starts: nothing to mark late, no engine to warm.
    if skipped {
        return;
    }
    state.timer.push(
        due + LATE_AFTER_SECS * 1_000_000,
        TimerEvent::LateCheck(run_id),
    );
    // Pre-warm an engine shortly before the run is due.
    let prewarm = due - PREWARM_SECS * 1_000_000;
    if prewarm > now_micros() {
        state.timer.push(prewarm, TimerEvent::Wake);
    }
}

/// Handle a schedule edit or resume: drop unstarted runs and rebuild.
pub fn rebuild(state: &Arc<AppState>, schedule_id: i64) {
    if let Ok(Some(row)) = state.store.get_schedule(schedule_id) {
        state.scheduler.put(row);
    } else {
        state.scheduler.remove(schedule_id);
        state.timer.remove_schedule_events(schedule_id);
        return;
    }
    drop_unstarted(state, schedule_id);
    if let Some(row) = state.scheduler.get(schedule_id) {
        drop_lost_skips(state, &row);
    }
    materialize(state, schedule_id);
}

/// The first `count` fire times of a schedule after `after`, in microseconds.
pub fn fires_after(schedule: &Schedule, after: i64, count: usize) -> Vec<i64> {
    let mut out = Vec::new();
    let mut cursor = from_micros(after);
    while out.len() < count {
        match schedule.next_after(cursor) {
            Ok(Some(next)) if next > cursor => {
                out.push(to_micros(next));
                cursor = next;
            }
            _ => break,
        }
    }
    out
}

/// Forget skips whose fire time the schedule no longer produces, as after an
/// edit or a restart that restored a code declaration, and record which.
fn drop_lost_skips(state: &Arc<AppState>, row: &ScheduleRow) {
    let now = now_micros();
    let skips: Vec<i64> = state
        .store
        .list_skips(row.id)
        .unwrap_or_default()
        .into_iter()
        .filter(|t| *t > now)
        .collect();
    if skips.is_empty() {
        return;
    }
    let produced: HashSet<i64> = fires_after(&row.schedule, now, LOOKAHEAD_MAX)
        .into_iter()
        .collect();
    let lost: Vec<i64> = skips
        .into_iter()
        .filter(|t| !produced.contains(t))
        .collect();
    if lost.is_empty() {
        return;
    }
    let _ = state.store.delete_skips(row.id, lost.clone());
    let _ = state.record_engine_event(
        EventName::ScheduleSkipsDropped,
        None,
        Some(row.flow_id),
        json!({"schedule_id": row.id, "dropped": lost}),
    );
}

fn describe_fire(fire: i64) -> String {
    from_micros(fire).format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// Skip fires of a schedule: the ones listed, or the next `next` not yet
/// skipped. Refuses past fires, times the schedule does not produce, and
/// anything more than LOOKAHEAD_MAX fires ahead. Returns the fires skipped.
pub fn skip_fires(
    state: &Arc<AppState>,
    row: &ScheduleRow,
    fires: &[i64],
    next: Option<usize>,
    created_by: &str,
) -> Result<Vec<i64>, String> {
    let now = now_micros();
    let ahead = fires_after(&row.schedule, now, LOOKAHEAD_MAX);
    let chosen: Vec<i64> = match next {
        Some(0) => return Err("next must be at least 1".into()),
        Some(n) => {
            let skipped: HashSet<i64> = state
                .store
                .list_skips(row.id)
                .map_err(|e| e.to_string())?
                .into_iter()
                .collect();
            let open: Vec<i64> = ahead
                .iter()
                .copied()
                .filter(|t| !skipped.contains(t))
                .take(n)
                .collect();
            if open.len() < n {
                return Err(format!(
                    "only {} of the next {LOOKAHEAD_MAX} fires can still be skipped",
                    open.len()
                ));
            }
            open
        }
        None => {
            if fires.is_empty() {
                return Err("name the fires to skip, or pass next".into());
            }
            let produced: HashSet<i64> = ahead.iter().copied().collect();
            for &fire in fires {
                if fire <= now {
                    return Err(format!("{} has passed", describe_fire(fire)));
                }
                if !produced.contains(&fire) {
                    return Err(match ahead.last() {
                        Some(last) if fire > *last => format!(
                            "{} is more than {LOOKAHEAD_MAX} fires ahead",
                            describe_fire(fire)
                        ),
                        _ => format!("{} is not a fire of this schedule", describe_fire(fire)),
                    });
                }
            }
            let mut chosen = fires.to_vec();
            chosen.sort_unstable();
            chosen.dedup();
            chosen
        }
    };
    state
        .store
        .add_skips(row.id, chosen.clone(), created_by)
        .map_err(|e| e.to_string())?;
    apply_skips(state, row.id);
    Ok(chosen)
}

/// Undo a skip before its fire time. `Ok(false)` when there was no such skip.
pub fn unskip_fire(state: &Arc<AppState>, schedule_id: i64, fire: i64) -> Result<bool, String> {
    if fire <= now_micros() {
        return Err(format!(
            "{} has passed; its Skipped run is history",
            describe_fire(fire)
        ));
    }
    let removed = state
        .store
        .delete_skips(schedule_id, vec![fire])
        .map_err(|e| e.to_string())?;
    if removed == 0 {
        return Ok(false);
    }
    apply_skips(state, schedule_id);
    Ok(true)
}

/// Bring the waiting runs of a schedule in line with its skips after a change,
/// then top the look-ahead up past any new ones.
fn apply_skips(state: &Arc<AppState>, schedule_id: i64) {
    if let Ok(changed) = state.store.sync_skip_marks(schedule_id) {
        for id in changed {
            if let Ok(Some(run)) = state.store.get_run(id) {
                let st = run.state.clone();
                state.index.update(id, |r| r.state = st);
                if is_marked(&run) {
                    // Due already enqueued it: take it back before an engine does.
                    state.supervisor.dequeue(id);
                }
                if let Some(due) = run.scheduled_time {
                    arm_run(state, id, due, is_marked(&run));
                }
                state.publish_run(&run);
            }
        }
    }
    materialize(state, schedule_id);
    // A paused schedule is not materialized; its count still changes.
    if let (Some(mut cached), Ok(Some(stored))) = (
        state.scheduler.get(schedule_id),
        state.store.get_schedule(schedule_id),
    ) {
        cached.skipped = stored.skipped;
        state.scheduler.put(cached);
    }
    state.publish_schedule(schedule_id);
}

/// The time of a skipped fire arrived: its run ends Skipped without starting.
fn end_skipped(state: &Arc<AppState>, run: &Run) {
    let mut skipped = State::named(cereyan_core::StateName::Skipped);
    skipped.message = Some("skipped by a person".into());
    skipped
        .details
        .insert("reason".into(), json!(SKIP_BY_PERSON));
    let _ = state.transition_run(run.id, skipped, false);
    if let (Some(schedule_id), Some(fire)) = (run.schedule_id, run.scheduled_time) {
        let _ = state.store.delete_skips(schedule_id, vec![fire]);
    }
}

pub fn drop_unstarted(state: &Arc<AppState>, schedule_id: i64) {
    if let Ok(ids) = state.store.delete_unstarted_runs(schedule_id) {
        for id in ids {
            let flow_id = state.index.get(id).map(|r| r.flow_id).unwrap_or(0);
            state
                .index
                .remove_run(id, Some(&State::new(StateType::Scheduled)), flow_id);
            state.supervisor.dequeue(id);
            state.timer.remove_run_events(id);
            state.stream.publish(
                "run.updated",
                id.to_string(),
                json!({"id": id, "deleted": true}),
            );
        }
    }
    state.timer.remove_schedule_events(schedule_id);
}

pub fn pause(state: &Arc<AppState>, schedule_id: i64, reason: Option<&str>, until: Option<i64>) {
    let _ = state.store.patch_schedule(
        schedule_id,
        cereyan_store::SchedulePatch {
            active: Some(false),
            paused_reason: Some(reason.map(|s| s.to_string())),
            paused_until: Some(until),
            ..Default::default()
        },
    );
    if let Ok(Some(row)) = state.store.get_schedule(schedule_id) {
        state.scheduler.put(row);
    }
    drop_unstarted(state, schedule_id);
    state.publish_schedule(schedule_id);
    if let Some(row) = state.scheduler.get(schedule_id) {
        let _ = state.record_engine_event(
            EventName::SchedulePaused,
            None,
            Some(row.flow_id),
            json!({"schedule_id": schedule_id, "reason": reason}),
        );
    }
}

pub fn resume(state: &Arc<AppState>, schedule_id: i64) {
    let _ = state.store.patch_schedule(
        schedule_id,
        cereyan_store::SchedulePatch {
            active: Some(true),
            paused_reason: Some(None),
            paused_until: Some(None),
            ..Default::default()
        },
    );
    rebuild(state, schedule_id);
    state.publish_schedule(schedule_id);
    if let Some(row) = state.scheduler.get(schedule_id) {
        let _ = state.record_engine_event(
            EventName::ScheduleResumed,
            None,
            Some(row.flow_id),
            json!({"schedule_id": schedule_id}),
        );
    }
}

pub fn delete(state: &Arc<AppState>, schedule_id: i64) -> bool {
    drop_unstarted(state, schedule_id);
    let ok = state.store.delete_schedule(schedule_id).unwrap_or(false);
    state.scheduler.remove(schedule_id);
    state.publish_schedule(schedule_id);
    ok
}

/// Preview the next fires of a schedule definition.
pub fn preview(schedule: &Schedule, count: usize) -> Result<Vec<i64>, String> {
    schedule.validate().map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    let mut cursor = Utc::now();
    for _ in 0..count.min(20) {
        match schedule.next_after(cursor).map_err(|e| e.to_string())? {
            Some(n) => {
                out.push(to_micros(n));
                cursor = n;
            }
            None => break,
        }
    }
    Ok(out)
}

/// The scheduler task: sleep until the earliest timer, then handle every due event.
pub async fn run_loop(state: Arc<AppState>, mut shutdown: watch::Receiver<bool>) {
    loop {
        let now = now_micros();
        let wait = state
            .timer
            .peek_at()
            .map(|t| Duration::from_micros((t - now).max(0) as u64))
            .unwrap_or(Duration::from_secs(3600));
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = state.timer.notify.notified() => { continue; }
            _ = shutdown.changed() => { if *shutdown.borrow() { return; } }
        }
        let due = state.timer.pop_due(now_micros());
        if due.is_empty() {
            continue;
        }
        let st = state.clone();
        let _ = tokio::task::spawn_blocking(move || {
            for (_, event) in due {
                handle(&st, event);
            }
        })
        .await;
    }
}

fn handle(state: &Arc<AppState>, event: TimerEvent) {
    match event {
        TimerEvent::Fire(schedule_id) => materialize(state, schedule_id),
        TimerEvent::Due(run_id) => {
            let Some(run) = state.store.get_run(run_id).ok().flatten() else {
                return;
            };
            if run.state.state_type != StateType::Scheduled || run.engine_pid.is_some() {
                return;
            }
            if is_marked(&run) {
                end_skipped(state, &run);
                return;
            }
            let Some(flow) = state.store.get_flow(run.flow_id).ok().flatten() else {
                return;
            };
            crate::dispatch::enqueue_run(state, &run, &flow, None);
        }
        TimerEvent::LateCheck(run_id) => {
            let Some(run) = state.store.get_run(run_id).ok().flatten() else {
                return;
            };
            if run.state.state_type == StateType::Scheduled
                && run.engine_pid.is_none()
                && run.state.name != "Late"
                && !is_marked(&run)
            {
                let mut late = State::named(cereyan_core::StateName::Late);
                late.message = run.state.message.clone();
                if let Ok(crate::state::TransitionResult::Accepted(_)) =
                    state.transition_run(run_id, late, false)
                {
                    let _ = state.record_engine_event(
                        EventName::RunLate,
                        Some(run_id),
                        Some(run.flow_id),
                        json!({"scheduled_time": run.scheduled_time, "name": run.name}),
                    );
                }
            }
        }
        TimerEvent::CrashRerun(run_id) => crate::dispatch::crash_rerun(state, run_id),
        TimerEvent::FlowTimeout(run_id) => crate::dispatch::flow_timeout(state, run_id),
        TimerEvent::ResumeFlow(flow_id) => {
            for row in state.scheduler.for_flow(flow_id) {
                if !row.active && row.paused_reason.as_deref() == Some("disabled") {
                    resume(state, row.id);
                }
            }
            let _ =
                state.record_engine_event(EventName::FlowEnabled, None, Some(flow_id), json!({}));
        }
        TimerEvent::Persist => {
            let now = now_micros();
            let _ = state.store.kv_set(LAST_WAKEUP_KEY, &now.to_string());
            state
                .timer
                .push(now + PERSIST_EVERY_SECS * 1_000_000, TimerEvent::Persist);
        }
        TimerEvent::Wake => {
            state.supervisor.ensure_capacity(state);
        }
        TimerEvent::Expectation(id) => crate::rules::expectation_due(state, id),
        TimerEvent::RuleClock(rule_id) => crate::rules::clock_tick(state, rule_id),
    }
}
