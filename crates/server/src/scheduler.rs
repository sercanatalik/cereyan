//! Schedules: materialize upcoming runs, dispatch them when due, mark late
//! runs, apply catch-up on start, and persist the wake-up time.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use cereyan_core::schedule::{from_micros, to_micros, CatchupPolicy, Schedule};
use cereyan_core::{
    now_micros, EventName, Flow, FlowOptions, Run, ScheduleDecl, ScheduleRow, State, StateType,
};
use cereyan_store::{CreateRun, ScheduleRunMark, ScheduleWrite};
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
/// kv key holding the global pause as JSON while the scheduler is paused.
pub const PAUSE_KEY: &str = "scheduler.paused";

/// The global pause: every schedule held at once, with a reason and an end.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct Pause {
    /// When the pause began, microseconds.
    pub since: i64,
    pub reason: Option<String>,
    /// When the scheduler resumes on its own, microseconds; none means until resumed.
    pub until: Option<i64>,
    /// Rules that would fire are recorded as suppressed instead of acting.
    pub suppress_rules: bool,
}

/// Pause every schedule: nothing materialises or starts until `resume_all`.
/// Pausing again replaces the reason and the end.
pub fn pause_all(
    state: &Arc<AppState>,
    reason: Option<String>,
    until: Option<i64>,
    suppress_rules: bool,
) -> Pause {
    let now = now_micros();
    let since = state.pause().map(|p| p.since).unwrap_or(now);
    let pause = Pause {
        since,
        reason,
        until,
        suppress_rules,
    };
    *state.pause.write().unwrap_or_else(|e| e.into_inner()) = Some(pause.clone());
    let _ = state.store.kv_set(
        PAUSE_KEY,
        &serde_json::to_string(&pause).unwrap_or_else(|_| "{}".into()),
    );
    if let Some(at) = until {
        state
            .timer
            .push(at.max(now), TimerEvent::SchedulerResume(since));
    }
    let _ = state.record_engine_event(
        EventName::SchedulerPaused,
        None,
        None,
        json!({"reason": pause.reason, "until": pause.until, "suppress_rules": pause.suppress_rules}),
    );
    pause
}

/// End the global pause: each schedule catches up the fires it missed under
/// its own policy and materialises again, and every held run starts. Returns
/// the runs started and the schedules re-armed, or none when not paused.
pub fn resume_all(state: &Arc<AppState>) -> Option<(usize, usize)> {
    let pause = state
        .pause
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .take()?;
    let _ = state.store.kv_delete(PAUSE_KEY);
    let now = now_micros();
    let rows = state.store.list_schedules(None).unwrap_or_default();
    let mut schedules = 0;
    for row in rows.into_iter().filter(|r| r.active) {
        state.scheduler.put(row.clone());
        catch_up(state, &row, pause.since, now);
        materialize(state, row.id);
        schedules += 1;
    }
    // One read of the marks for the runs the index says are waiting, rather than
    // a whole `Run` per run to ask whether it was skipped and when it is due.
    let waiting: Vec<i64> = state
        .index
        .active_runs()
        .into_iter()
        .filter(|r| r.state.state_type == StateType::Scheduled && r.engine_pid.is_none())
        .map(|r| r.id)
        .collect();
    let marks = state.store.run_marks(waiting.iter().copied());
    let mut held = 0;
    for id in waiting {
        let Some(mark) = marks.get(&id) else { continue };
        if is_mark_skip(&mark.details) {
            continue;
        }
        if mark.scheduled_time.is_some_and(|t| t <= now) {
            state.timer.push(now, TimerEvent::Due(id));
            held += 1;
        }
    }
    let _ = state.record_engine_event(
        EventName::SchedulerResumed,
        None,
        None,
        json!({"held": held, "schedules": schedules}),
    );
    Some((held, schedules))
}

/// Scheduled runs whose time has passed and that the pause is holding.
pub fn held_runs(state: &Arc<AppState>) -> usize {
    if !state.is_paused() {
        return 0;
    }
    let now = now_micros();
    let waiting: Vec<i64> = state
        .index
        .active_runs()
        .into_iter()
        .filter(|r| r.state.state_type == StateType::Scheduled && r.engine_pid.is_none())
        .map(|r| r.id)
        .collect();
    if waiting.is_empty() {
        return 0;
    }
    // One batched read of the marks, rather than a whole `Run` per waiting run.
    state
        .store
        .run_marks(waiting.iter().copied())
        .values()
        .filter(|m| !is_mark_skip(&m.details) && m.scheduled_time.is_some_and(|t| t <= now))
        .count()
}

fn restore_pause(state: &Arc<AppState>) -> Option<Pause> {
    let pause: Option<Pause> = state
        .store
        .kv_get(PAUSE_KEY)
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_str(&v).ok());
    *state.pause.write().unwrap_or_else(|e| e.into_inner()) = pause.clone();
    if let Some(p) = &pause {
        if let Some(at) = p.until {
            state
                .timer
                .push(at.max(now_micros()), TimerEvent::SchedulerResume(p.since));
        }
    }
    pause
}
/// Engines are warmed this long before a scheduled run is due.
pub const PREWARM_SECS: i64 = 5;
/// The mark a waiting run carries in its state details when a person skipped
/// its fire: `details.skip = "user"`. `schedule_skip` is the durable record;
/// the mark only saves the Due handler a lookup and never outlives the table.
pub const SKIP_MARK: &str = "skip";
const SKIP_BY_PERSON: &str = "user";

/// Whether a waiting run belongs to a fire a person skipped.
///
/// Takes a whole `Run`, which most callers have. The scheduler's fire path holds
/// only a [`cereyan_store::ScheduleRunMark`] and asks the same question, so the
/// rule itself lives in [`is_mark_skip`] and this is a one-line delegate. One
/// definition of "skipped by a person", two shapes to apply it to — the second is
/// not a second rule.
pub fn is_marked(run: &Run) -> bool {
    is_mark_skip(&run.state.details)
}

/// The skip mark rule, over just the state details it reads. See [`is_marked`].
fn is_mark_skip(details: &serde_json::Map<String, serde_json::Value>) -> bool {
    details.get(SKIP_MARK).and_then(|v| v.as_str()) == Some(SKIP_BY_PERSON)
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

    /// The schedules of several flows at once, keyed by flow id and ordered by
    /// id within each flow — the same contents and order as calling `for_flow`
    /// for each, from one pass over the table instead of one pass per flow.
    pub fn for_flows(&self, flow_ids: &[i64]) -> HashMap<i64, Vec<ScheduleRow>> {
        if flow_ids.is_empty() {
            return HashMap::new();
        }
        let schedules = self.schedules.read().unwrap_or_else(|e| e.into_inner());
        group_by_flow(schedules.values(), flow_ids)
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

/// Group schedules by flow, keeping only `flow_ids` and ordering each group by
/// id — the same contents and order as `Scheduler::for_flow` per flow.
fn group_by_flow<'a, I>(schedules: I, flow_ids: &[i64]) -> HashMap<i64, Vec<ScheduleRow>>
where
    I: Iterator<Item = &'a ScheduleRow>,
{
    let wanted: HashSet<i64> = flow_ids.iter().copied().collect();
    let mut by_flow: HashMap<i64, Vec<ScheduleRow>> = HashMap::new();
    for s in schedules {
        if wanted.contains(&s.flow_id) {
            by_flow.entry(s.flow_id).or_default().push(s.clone());
        }
    }
    for rows in by_flow.values_mut() {
        rows.sort_by_key(|s| s.id);
    }
    by_flow
}

/// Bring code-declared schedules of a flow in line with its declarations.
/// Register a flow's code-declared schedules, returning the ids of those whose
/// spec changed.
///
/// **A changed spec leaves runs materialized under the old one**, and
/// look-ahead continues from the latest of them: a flow moved from daily to
/// hourly kept its three nightly runs and gained no hourly one for three days.
/// The caller drops them ([`drop_unstarted`]) before the scheduler starts, as
/// an edit through the API does ([`rebuild`]).
pub fn sync_code_schedules(
    state: &AppState,
    flow: &Flow,
    decls: &[ScheduleDecl],
) -> Result<Vec<i64>, String> {
    let mut changed: Vec<i64> = Vec::new();
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
                let stored = serde_json::to_string(&row.schedule).map_err(|e| e.to_string())?;
                if stored != spec {
                    changed.push(row.id);
                }
                state
                    .store
                    .upsert_schedule(ScheduleWrite {
                        id: Some(row.id),
                        flow_id: flow.id,
                        spec,
                        catchup: decl.catchup.as_str().into(),
                        catchup_max: decl.catchup_max,
                        catchup_window: decl.catchup_window,
                        jitter: decl.jitter,
                        start_deadline: decl.start_deadline,
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
                        catchup_window: decl.catchup_window,
                        jitter: decl.jitter,
                        start_deadline: decl.start_deadline,
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
    Ok(changed)
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
    // A restart inside a global pause keeps holding: no catch-up until resumed.
    let paused = restore_pause(state).is_some();
    let last_wakeup: Option<i64> = state
        .store
        .kv_get(LAST_WAKEUP_KEY)
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok())
        .filter(|_| with_catch_up && !paused);
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
    // Paused runs that asked to be woken keep their wake time across a restart.
    crate::waits::rearm_all(state);
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
    // A fire older than the window is not worth running any more.
    let mut expired = 0usize;
    let fires: Vec<_> = match row.catchup_window {
        Some(window) if window > 0 => {
            let cutoff = now - window * 1_000_000;
            let before = fires.len();
            let kept: Vec<_> = fires
                .into_iter()
                .filter(|f| to_micros(*f) >= cutoff)
                .collect();
            expired = before - kept.len();
            kept
        }
        _ => fires,
    };
    if fires.is_empty() && expired == 0 {
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
        if let Some(run) =
            create_scheduled_run(state, &flow, row, scheduled, "catchup", false, None)
        {
            crate::dispatch::enqueue_run(state, &run, &flow, None);
        }
    }
    let _ = state.record_engine_event(
        EventName::ScheduleCatchup,
        None,
        Some(row.flow_id),
        json!({
            "schedule_id": row.id, "policy": row.catchup.as_str(), "missed": fires.len() + expired,
            "created": chosen.len(), "dropped": dropped, "expired": expired,
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
    unique: Option<cereyan_store::UniqueCheck>,
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
            unique,
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
    if !row.active || state.is_paused() {
        return;
    }
    let Some(flow) = state.store.get_flow(row.flow_id).ok().flatten() else {
        return;
    };
    if let Schedule::Continuous { delay } = row.schedule {
        seed_continuous(state, &row, &flow, delay);
        return;
    }
    let now = now_micros();
    let all_skips = state.store.list_skips(row.id).unwrap_or_default();
    if all_skips.first().is_some_and(|t| *t <= now) {
        // A passed skip is history: its run, if one was made, already ended Skipped.
        let _ = state.store.delete_skips_before(row.id, now);
    }
    let skips: HashSet<i64> = all_skips.into_iter().filter(|t| *t > now).collect();
    // Marks, not runs: a schedule may hold up to LOOKAHEAD_MAX of these and the
    // loop reads only the id, the scheduled time and the skip mark from each.
    let mut existing = state
        .store
        .future_run_marks(row.id, now)
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
    let mut starting = existing
        .iter()
        .filter(|r| !is_mark_skip(&r.details))
        .count();
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
        if let Some(run) =
            create_scheduled_run(state, &flow, &row, scheduled, "schedule", skip, None)
        {
            if !skip {
                starting += 1;
            }
            // A newly created run is converted rather than read back: it was
            // just written, so its mark is already in hand. This clones one
            // small JSON map per *created* run, against a full run projection
            // per *existing* run before.
            existing.push(ScheduleRunMark {
                id: run.id,
                scheduled_time: run.scheduled_time,
                details: run.state.details.clone(),
            });
            created += 1;
        } else {
            break;
        }
        if created > LOOKAHEAD_MAX {
            break;
        }
    }
    let flow_deadline = FlowOptions::from_map(&flow.options).start_deadline;
    for mark in &existing {
        if let Some(fire) = mark.scheduled_time {
            let marked = is_mark_skip(&mark.details);
            let due = fire + jitter_offset(row.id, fire, row.jitter);
            arm_run(state, mark.id, due, marked);
            let deadline = row
                .start_deadline
                .or_else(|| flow_deadline.map(|d| d.round() as i64));
            if let Some(seconds) = deadline.filter(|d| *d > 0) {
                if !marked {
                    state.timer.push(
                        due + seconds * 1_000_000,
                        TimerEvent::StartDeadline(mark.id),
                    );
                }
            }
        }
    }
    // Wake again when the earliest future run fires so the look-ahead is kept.
    if let Some(first) = existing.iter().filter_map(|r| r.scheduled_time).min() {
        state.timer.remove_schedule_events(row.id);
        state.timer.push(first + 1_000, TimerEvent::Fire(row.id));
    }
    let next_fire = existing
        .iter()
        .filter(|r| !is_mark_skip(&r.details))
        .filter_map(|r| r.scheduled_time)
        .min();
    let mut updated = row.clone();
    updated.next_fire = next_fire;
    updated.skipped = skips.len() as i64;
    state.scheduler.put(updated);
}

/// The unique key that keeps a continuous schedule to one run at a time.
pub fn continuous_key(schedule_id: i64) -> String {
    format!("continuous:{schedule_id}")
}

/// Serializes `seed_continuous` and `join_now` for one schedule. Each reads the
/// waiting run, arms it and updates the cached row; interleaved, a seed that
/// created the run before a join moved it to now re-arms it for its old time
/// afterwards, and the joined run waits out the full delay.
///
/// The race is per schedule, so the lock is striped by schedule id: unrelated
/// schedules finishing runs together do not queue behind each other's store
/// round trips, and the stripes are fixed, so nothing grows with the number
/// of schedules a long-running server has seen.
const CONTINUOUS_STRIPES: usize = 64;

static CONTINUOUS_LOCKS: [std::sync::Mutex<()>; CONTINUOUS_STRIPES] =
    [const { std::sync::Mutex::new(()) }; CONTINUOUS_STRIPES];

fn continuous_lock(schedule_id: i64) -> std::sync::MutexGuard<'static, ()> {
    CONTINUOUS_LOCKS[schedule_id.rem_euclid(CONTINUOUS_STRIPES as i64) as usize]
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// A continuous schedule has at most one run that has not finished. With none,
/// create it, due at the later of now and the last run's end plus `delay`, so
/// the wait holds no processor. Called on activation, at start, on resume, and
/// when a run of the schedule reaches a final state; seeding twice leaves one
/// run, because the store refuses a second holder of the schedule's key.
fn seed_continuous(state: &Arc<AppState>, row: &ScheduleRow, flow: &Flow, delay: f64) {
    let _loop = continuous_lock(row.id);
    let now = now_micros();
    let active = state
        .store
        .active_runs_of_schedule(row.id)
        .unwrap_or_default();
    let waiting = if active.is_empty() {
        let last_end = state.store.last_end_of_schedule(row.id).ok().flatten();
        let due = last_end
            .map(|end| end + (delay * 1e6) as i64)
            .unwrap_or(now)
            .max(now);
        let unique = cereyan_store::UniqueCheck {
            key: continuous_key(row.id),
            states: ["Scheduled", "Pending", "Running", "Paused", "Cancelling"]
                .map(String::from)
                .to_vec(),
            since: None,
        };
        create_scheduled_run(state, flow, row, due, "continuous", false, Some(unique))
    } else {
        active
            .into_iter()
            .find(|r| r.state.state_type == StateType::Scheduled && r.engine_pid.is_none())
    };
    let mut next_fire = None;
    if let Some(run) = waiting.filter(|r| r.state.state_type == StateType::Scheduled) {
        if let Some(fire) = run.scheduled_time {
            let due = fire + jitter_offset(row.id, fire, row.jitter);
            arm_run(state, run.id, due, false);
            let deadline = row.start_deadline.or_else(|| {
                FlowOptions::from_map(&flow.options)
                    .start_deadline
                    .map(|d| d.round() as i64)
            });
            if let Some(seconds) = deadline.filter(|d| *d > 0) {
                state
                    .timer
                    .push(due + seconds * 1_000_000, TimerEvent::StartDeadline(run.id));
            }
            next_fire = Some(due).filter(|d| *d > now);
        }
    }
    let mut updated = row.clone();
    updated.next_fire = next_fire;
    updated.skipped = 0;
    state.scheduler.put(updated);
    state.publish_schedule(row.id);
}

/// Move a continuous schedule's waiting run to now, so it joins the line at
/// once. `Err` when the schedule is not continuous, is paused, or has no run
/// waiting (it is already in line or running).
pub fn join_now(state: &Arc<AppState>, schedule_id: i64) -> Result<i64, String> {
    let guard = continuous_lock(schedule_id);
    let row = state
        .scheduler
        .get(schedule_id)
        .ok_or_else(|| "schedule not found".to_string())?;
    if !row.schedule.is_continuous() {
        return Err("only a continuous schedule has a run waiting to join the line".into());
    }
    if !row.active {
        return Err("the schedule is paused; resume it to start the loop".into());
    }
    let waiting = state
        .store
        .active_runs_of_schedule(schedule_id)
        .unwrap_or_default()
        .into_iter()
        .find(|r| {
            r.state.state_type == StateType::Scheduled
                && r.engine_pid.is_none()
                && !state.supervisor.queued(r.id)
        })
        .ok_or_else(|| "no run is waiting: it is already in line or running".to_string())?;
    let now = now_micros();
    let params = serde_json::to_string(&waiting.parameters).unwrap_or_else(|_| "{}".into());
    if !state
        .store
        .reschedule_run(waiting.id, now, &params)
        .map_err(|e| e.to_string())?
    {
        return Err("the run started meanwhile".into());
    }
    let run = state.store.get_run(waiting.id).ok().flatten();
    let flow = run
        .as_ref()
        .and_then(|r| state.store.get_flow(r.flow_id).ok().flatten());
    // Join the line now rather than through a `Due` timer, so the reply (and
    // any read after it) already sees the run in line instead of still
    // waiting, with a jittered next fire, for a timer to queue it. Held by
    // the global pause, it is armed instead and resume_all re-arms it.
    let enqueue_now = match (&run, &flow) {
        (Some(run), Some(flow)) if !state.is_paused() => {
            state.timer.remove_run_events(run.id);
            Some((run.clone(), flow.clone()))
        }
        _ => {
            arm_run(state, waiting.id, now, false);
            None
        }
    };
    let mut updated = row;
    updated.next_fire = None;
    state.scheduler.put(updated);
    // Enqueueing can end the run (an overlap policy), and its final state
    // seeds the loop again, which takes this lock: release it first.
    drop(guard);
    if let Some((run, flow)) = enqueue_now {
        crate::dispatch::enqueue_run(state, &run, &flow, None);
    }
    if let Some(run) = &run {
        state.publish_run(run);
    }
    state.publish_schedule(schedule_id);
    Ok(waiting.id)
}

/// A deterministic offset in `[0, jitter)` seconds, as microseconds, from the
/// schedule id and the fire time (FNV-1a), so a run keeps its due time across
/// restarts. Zero jitter is zero offset.
///
/// Uses rejection sampling to eliminate modulo bias: hash values in the range
/// `[2^64 - (2^64 % m), 2^64)` are rejected and rehashed.
pub fn jitter_offset(schedule_id: i64, fire: i64, jitter_secs: i64) -> i64 {
    if jitter_secs <= 0 {
        return 0;
    }
    let modulus = jitter_secs as u64 * 1_000_000;
    // Rejection threshold: values >= this are rejected to avoid modulo bias.
    let threshold = u64::MAX - (u64::MAX % modulus);
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in schedule_id
        .to_le_bytes()
        .into_iter()
        .chain(fire.to_le_bytes())
    {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    // Rejection sampling: rehash if the value falls in the biased range.
    while hash >= threshold {
        hash ^= hash >> 33;
        hash = hash.wrapping_mul(0xff51_afd7_ed55_8ccd);
        hash ^= hash >> 33;
    }
    (hash % modulus) as i64
}

/// Arm a Scheduled run's due, late-check and pre-warm timers.
pub fn arm_run(state: &Arc<AppState>, run_id: i64, due: i64, skipped: bool) {
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
            // Held by the global pause: it stays Scheduled and resume_all re-arms it.
            if state.is_paused() {
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
            // A continuous schedule's time is a floor for joining the line,
            // not a promise to start: its runs are never Late.
            let continuous = run
                .schedule_id
                .and_then(|sid| state.scheduler.get(sid))
                .is_some_and(|row| row.schedule.is_continuous());
            if run.state.state_type == StateType::Scheduled
                && run.engine_pid.is_none()
                && run.state.name != "Late"
                && !is_marked(&run)
                && !continuous
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
        TimerEvent::StartDeadline(run_id) => {
            let Some(run) = state.store.get_run(run_id).ok().flatten() else {
                return;
            };
            let unstarted = matches!(
                run.state.state_type,
                StateType::Scheduled | StateType::Pending
            ) && run.engine_pid.is_none()
                && !is_marked(&run);
            if !unstarted {
                return;
            }
            state.supervisor.dequeue(run_id);
            let mut skipped = State::named(cereyan_core::StateName::Skipped);
            skipped.message = Some("missed start deadline".into());
            skipped
                .details
                .insert("reason".into(), json!("missed_start_deadline"));
            if let Ok(crate::state::TransitionResult::Accepted(_)) =
                state.transition_run(run_id, skipped, false)
            {
                let _ = state.record_engine_event(
                    EventName::RunSkipped,
                    Some(run_id),
                    Some(run.flow_id),
                    json!({"reason": "missed_start_deadline", "scheduled_time": run.scheduled_time}),
                );
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
        TimerEvent::WakeRun(run_id) => crate::waits::on_timer(state, run_id),
        TimerEvent::SchedulerResume(since) => {
            if state.pause().is_some_and(|p| p.since == since) {
                resume_all(state);
            }
        }
    }
}

#[cfg(test)]
mod jitter_tests {
    use super::jitter_offset;

    #[test]
    fn offset_is_deterministic_and_bounded() {
        assert_eq!(jitter_offset(7, 1_000_000, 0), 0);
        let a = jitter_offset(7, 1_000_000, 60);
        let b = jitter_offset(7, 1_000_000, 60);
        assert_eq!(a, b);
        assert!((0..60_000_000).contains(&a));
        assert_ne!(
            jitter_offset(8, 1_000_000, 60),
            jitter_offset(7, 2_000_000, 60)
        );
        let spread: std::collections::HashSet<i64> = (0..50)
            .map(|i| jitter_offset(1, i * 300_000_000, 60) / 1_000_000)
            .collect();
        assert!(spread.len() > 10, "offsets should spread across the window");
    }
}

#[cfg(test)]
mod group_tests {
    use super::*;

    /// A schedule row with only the fields `group_by_flow` reads set
    /// meaningfully; the rest take their defaults, since grouping looks at
    /// nothing but `id` and `flow_id`.
    fn row(id: i64, flow_id: i64) -> ScheduleRow {
        ScheduleRow {
            id,
            external_id: cereyan_core::new_id(),
            flow_id,
            schedule: cereyan_core::schedule::Schedule::Cron {
                cron: "0 * * * *".into(),
                timezone: None,
                day_or: true,
            },
            catchup: CatchupPolicy::default(),
            catchup_max: 0,
            catchup_window: None,
            jitter: 0,
            start_deadline: None,
            active: true,
            paused_reason: None,
            paused_until: None,
            source: "code".into(),
            code_key: None,
            persist: false,
            created_at: 0,
            updated_at: 0,
            next_fire: None,
            skipped: 0,
            loop_state: None,
        }
    }

    /// The per-flow read, kept as the oracle.
    fn for_flow_the_old_way(all: &[ScheduleRow], flow_id: i64) -> Vec<ScheduleRow> {
        let mut v: Vec<ScheduleRow> = all
            .iter()
            .filter(|s| s.flow_id == flow_id)
            .cloned()
            .collect();
        v.sort_by_key(|s| s.id);
        v
    }

    #[test]
    fn grouping_matches_the_per_flow_read() {
        // Deliberately out of id order, and interleaved across flows.
        let all = vec![
            row(7, 2),
            row(3, 1),
            row(9, 3),
            row(1, 1),
            row(5, 2),
            row(2, 1),
        ];
        let got = group_by_flow(all.iter(), &[1, 2, 3]);
        for f in [1, 2, 3] {
            assert_eq!(
                got.get(&f).cloned().unwrap_or_default(),
                for_flow_the_old_way(&all, f),
                "grouping differs for flow {f}"
            );
        }
    }

    #[test]
    fn each_group_is_ordered_by_id() {
        let all = [row(9, 1), row(2, 1), row(5, 1)];
        let got = group_by_flow(all.iter(), &[1]);
        let ids: Vec<i64> = got[&1].iter().map(|s| s.id).collect();
        assert_eq!(ids, vec![2, 5, 9], "groups must be ordered by id");
    }

    #[test]
    fn a_flow_with_no_schedules_has_no_group() {
        let all = [row(1, 1)];
        let got = group_by_flow(all.iter(), &[1, 2]);
        assert!(got.contains_key(&1));
        assert!(!got.contains_key(&2), "an empty flow should have no group");
    }

    #[test]
    fn only_the_wanted_flows_are_grouped() {
        let all = [row(1, 1), row(2, 2), row(3, 3)];
        let got = group_by_flow(all.iter(), &[2]);
        assert_eq!(got.len(), 1);
        assert_eq!(got[&2][0].id, 2, "another flow's schedule leaked in");
    }

    #[test]
    fn no_flows_means_nothing_grouped() {
        let all = [row(1, 1)];
        assert!(group_by_flow(all.iter(), &[]).is_empty());
    }
}

#[cfg(test)]
mod skip_mark_tests {
    use super::*;

    /// The rule that decides whether a fire was skipped by a person. It is now
    /// the single definition behind both `is_marked` and the fire path's
    /// `is_mark_skip`, and it had no test before this — which is why the wrong
    /// key passed the suite when injected.
    #[test]
    fn only_a_user_skip_marks_a_run() {
        let mut d = serde_json::Map::new();
        assert!(!is_mark_skip(&d), "empty details are not a mark");

        d.insert(SKIP_MARK.into(), serde_json::Value::String("user".into()));
        assert!(is_mark_skip(&d), "skip=user is the mark");

        for other in ["system", "scheduler", "User", "user ", ""] {
            let mut d = serde_json::Map::new();
            d.insert(SKIP_MARK.into(), serde_json::Value::String(other.into()));
            assert!(!is_mark_skip(&d), "skip={other:?} is not a person's skip");
        }

        // A non-string value is not a mark either.
        let mut d = serde_json::Map::new();
        d.insert(SKIP_MARK.into(), serde_json::Value::Bool(true));
        assert!(!is_mark_skip(&d), "skip=true is not a mark");
        d.insert(SKIP_MARK.into(), serde_json::Value::Null);
        assert!(!is_mark_skip(&d), "skip=null is not a mark");

        // The key must be the mark key, not the value.
        let mut d = serde_json::Map::new();
        d.insert("user".into(), serde_json::Value::String("skip".into()));
        assert!(
            !is_mark_skip(&d),
            "the key is the mark key; a `user` key must not mark a run"
        );
    }

    /// `is_marked` and `is_mark_skip` are one rule over two shapes. If they ever
    /// drift, a run read whole and the same run read as a mark would be treated
    /// differently — which is exactly the bug the fire path would then have.
    ///
    /// The runs come from a real store rather than a hand-built struct: `Run` has
    /// thirty-nine required fields and no `Default`, and a literal here would be a
    /// second thing to keep in step when the struct changes.
    #[test]
    fn the_two_shapes_of_the_rule_agree() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = cereyan_store::Store::open(dir.path()).unwrap();
        let flow = store
            .upsert_flow(
                "p",
                "etl",
                "m",
                &dir.path().to_string_lossy(),
                None,
                "[]",
                "{}",
            )
            .unwrap();

        // One run per shape of the rule, so both are exercised against real
        // decoded state details rather than a literal.
        for (i, details) in [
            "{\"skip\":\"user\"}",
            "{\"skip\":\"system\"}",
            "{}",
            "{\"other\":1}",
        ]
        .iter()
        .enumerate()
        {
            let (id, _) = store
                .create_run_full(cereyan_store::CreateRun {
                    flow_id: flow,
                    name: format!("r{i}"),
                    parameters: "{}".into(),
                    tags: "[]".into(),
                    created_by: "test".into(),
                    initial_state: Some(State::from_parts(
                        StateType::Scheduled,
                        Some("skipped"),
                        None,
                        serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(details)
                            .unwrap(),
                    )),
                    ..Default::default()
                })
                .unwrap();
            let run = store.get_run(id).unwrap().unwrap();
            let from_mark = is_mark_skip(&run.state.details);
            assert_eq!(
                is_marked(&run),
                from_mark,
                "the two shapes disagree for {:?}",
                run.state.details
            );
            // And against the store's own mark, which the fire path reads.
            let marks = store.run_marks([id]);
            assert_eq!(
                marks[&id].details, run.state.details,
                "the mark's details differ from the run's"
            );
            assert_eq!(is_mark_skip(&marks[&id].details), from_mark);
        }
    }
}
