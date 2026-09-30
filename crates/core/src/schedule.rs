//! Schedule types and next-fire computation. Cron by wall clock in its
//! timezone, intervals under 24 hours by elapsed time, longer intervals by
//! wall clock, and RRule sets.

use std::str::FromStr;

use chrono::{DateTime, Duration, TimeZone, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

use crate::time::Micros;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum CatchupPolicy {
    #[default]
    Skip,
    Latest,
    All,
}

impl CatchupPolicy {
    pub fn parse(text: &str) -> Option<CatchupPolicy> {
        match text.to_ascii_lowercase().as_str() {
            "skip" => Some(CatchupPolicy::Skip),
            "latest" => Some(CatchupPolicy::Latest),
            "all" => Some(CatchupPolicy::All),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            CatchupPolicy::Skip => "skip",
            CatchupPolicy::Latest => "latest",
            CatchupPolicy::All => "all",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum Schedule {
    Cron {
        cron: String,
        #[serde(default)]
        timezone: Option<String>,
        #[serde(default = "default_true")]
        day_or: bool,
    },
    Interval {
        /// Seconds between fires.
        interval: f64,
        /// Anchor time in microseconds UTC; defaults to the schedule's creation.
        #[serde(default)]
        anchor: Option<Micros>,
        #[serde(default)]
        timezone: Option<String>,
    },
    RRule {
        rrule: String,
        #[serde(default)]
        timezone: Option<String>,
    },
    /// Runs again `delay` seconds after the last run ends, when a processor is
    /// free. It has no fire times of its own: the scheduler creates its one
    /// next run when the last one reaches a final state.
    Continuous {
        /// Seconds from the end of one run to the next joining the line.
        #[serde(default)]
        delay: f64,
    },
}

fn default_true() -> bool {
    true
}

#[derive(Debug, thiserror::Error)]
pub enum ScheduleError {
    #[error("invalid cron expression {expr:?}: {message}")]
    Cron { expr: String, message: String },
    #[error("unknown timezone {0:?}")]
    Timezone(String),
    #[error("interval must be positive")]
    Interval,
    #[error("invalid rrule: {0}")]
    RRule(String),
    #[error("{0}")]
    Invalid(String),
}

fn resolve_tz(name: Option<&str>) -> Result<Tz, ScheduleError> {
    match name {
        None | Some("") | Some("local") => Ok(local_tz()),
        Some(n) => n
            .parse::<Tz>()
            .map_err(|_| ScheduleError::Timezone(n.to_string())),
    }
}

/// The machine's IANA zone when it can be determined, else UTC.
pub fn local_tz() -> Tz {
    if let Ok(name) = std::env::var("TZ") {
        if let Ok(tz) = name.parse::<Tz>() {
            return tz;
        }
    }
    #[cfg(unix)]
    {
        if let Ok(target) = std::fs::read_link("/etc/localtime") {
            let text = target.to_string_lossy();
            if let Some(idx) = text.find("zoneinfo/") {
                if let Ok(tz) = text[idx + 9..].parse::<Tz>() {
                    return tz;
                }
            }
        }
    }
    chrono_tz::UTC
}

pub fn to_micros(dt: DateTime<Utc>) -> Micros {
    dt.timestamp_micros()
}

pub fn from_micros(micros: Micros) -> DateTime<Utc> {
    Utc.timestamp_micros(micros)
        .single()
        .unwrap_or_else(Utc::now)
}

/// A schedule with its timezone resolved and its expression parsed, ready to walk.
///
/// `next_after` used to do this work on every call, and a walk calls it once per
/// fire. For a per-minute cron over a one-day window that is 1,440 cron parses and
/// 1,440 `readlink("/etc/localtime")` syscalls to answer a question about two
/// timestamps. None of it varies within a walk: the expression and the zone are
/// fixed for its duration.
///
/// Splitting this out is a pure hoist. Every arm is the body the corresponding
/// `next_after` arm already had, moved.
enum Compiled {
    Cron {
        cron: croner::Cron,
        tz: Tz,
    },
    Interval {
        interval: f64,
        anchor: DateTime<Utc>,
        tz: Tz,
    },
    RRule {
        set: rrule::RRuleSet,
        tz: Tz,
    },
    /// A continuous schedule has no fire times of its own.
    None,
}

impl Schedule {
    /// Resolve the zone and parse the expression, once.
    fn compile(&self) -> Result<Compiled, ScheduleError> {
        Ok(match self {
            Schedule::Cron {
                cron,
                timezone,
                day_or,
            } => Compiled::Cron {
                cron: parse_cron(cron, *day_or)?,
                tz: resolve_tz(timezone.as_deref())?,
            },
            Schedule::Interval {
                interval,
                anchor,
                timezone,
            } => Compiled::Interval {
                interval: *interval,
                // A missing anchor is the Unix epoch so evaluation is deterministic;
                // schedules created at runtime pin their anchor at creation.
                anchor: from_micros(anchor.unwrap_or(0)),
                tz: resolve_tz(timezone.as_deref())?,
            },
            Schedule::RRule { rrule, timezone } => {
                let tz = resolve_tz(timezone.as_deref())?;
                Compiled::RRule {
                    set: parse_rrule(rrule, tz)?,
                    tz,
                }
            }
            Schedule::Continuous { .. } => Compiled::None,
        })
    }
}

impl Compiled {
    /// The first fire strictly after `after`.
    fn next_after(&self, after: DateTime<Utc>) -> Result<Option<DateTime<Utc>>, ScheduleError> {
        match self {
            Compiled::Cron { cron, tz } => {
                let local = after.with_timezone(tz);
                match cron.find_next_occurrence(&local, false) {
                    Ok(next) => Ok(Some(next.with_timezone(&Utc))),
                    Err(_) => Ok(None),
                }
            }
            Compiled::Interval {
                interval,
                anchor,
                tz,
            } => {
                let secs = *interval;
                if secs <= 0.0 || secs.is_nan() {
                    return Err(ScheduleError::Interval);
                }
                if secs < 86_400.0 {
                    // Elapsed-time intervals: fixed seconds since the anchor.
                    let elapsed = (after - *anchor).num_microseconds().unwrap_or(0) as f64 / 1e6;
                    let steps = if elapsed < 0.0 {
                        0.0
                    } else {
                        (elapsed / secs).floor() + 1.0
                    };
                    let next = *anchor + Duration::microseconds((steps * secs * 1e6) as i64);
                    Ok(Some(next))
                } else {
                    // Wall-clock intervals: add whole days in the local zone so
                    // the fire keeps its local time across DST.
                    let days = (secs / 86_400.0).round().max(1.0) as i64;
                    let local_anchor = anchor.with_timezone(tz);
                    let mut candidate = local_anchor;
                    let after_local = after.with_timezone(tz);
                    if candidate > after_local {
                        return Ok(Some(candidate.with_timezone(&Utc)));
                    }
                    let elapsed_days =
                        (after_local.date_naive() - local_anchor.date_naive()).num_days();
                    let first = (elapsed_days / days).max(0);
                    for steps in (first..).take(4) {
                        let date = local_anchor.date_naive() + Duration::days(steps * days);
                        let naive = date.and_time(local_anchor.time());
                        candidate = local_to_instant(*tz, naive).unwrap_or(candidate);
                        if candidate > after_local {
                            return Ok(Some(candidate.with_timezone(&Utc)));
                        }
                    }
                    Ok(Some(candidate.with_timezone(&Utc)))
                }
            }
            Compiled::RRule { set, tz } => {
                let after_tz: DateTime<rrule::Tz> = after.with_timezone(&rrule::Tz::Tz(*tz));
                // `after` is an inclusive filter, so asking for one occurrence
                // answers with the cursor's own when the cursor sits on one --
                // which is precisely what the scheduler asks once it has made a
                // run. Look past it. A set carrying RDATEs can stack several
                // occurrences on one instant, so one spare is not enough.
                // `RRuleSet::after` takes `self` by value, which is why this arm
                // re-parsed the rule text on every step. Cloning the parsed set
                // per step is not free either, but it is a copy of a parsed
                // structure rather than a fresh parse of the string, and the
                // caller needed the set kept for the next step.
                let result = set.clone().after(after_tz).all(8);
                Ok(result
                    .dates
                    .into_iter()
                    .find(|d| d.with_timezone(&Utc) > after)
                    .map(|d| d.with_timezone(&Utc)))
            }
            Compiled::None => Ok(None),
        }
    }

    /// The last fire at or before `after`, or `None` if it falls before `floor`.
    ///
    /// The mirror of [`Compiled::next_after`], and the direction a question about
    /// the *most recent* fires actually needs: walking forward from a window's
    /// start visits every fire in the window to reach its last two.
    ///
    /// Only the cron arm walks back. An interval is arithmetic in either
    /// direction and an rrule set is queried, so neither is asked -- and a silent
    /// `None` for them would read as "this schedule never fires", so the arms are
    /// written out rather than collapsed.
    fn previous_from(&self, after: DateTime<Utc>, floor: DateTime<Utc>) -> Option<DateTime<Utc>> {
        match self {
            Compiled::Cron { cron, tz } => {
                let local = after.with_timezone(tz);
                let found = cron
                    .find_previous_occurrence(&local, true)
                    .ok()?
                    .with_timezone(&Utc);
                (found >= floor).then_some(found)
            }
            Compiled::Interval { .. } | Compiled::RRule { .. } | Compiled::None => None,
        }
    }
}


impl Schedule {
    /// Pin an interval schedule's anchor to `now` when it has none.
    pub fn with_anchor_if_missing(mut self, now: Micros) -> Schedule {
        if let Schedule::Interval { anchor, .. } = &mut self {
            if anchor.is_none() {
                *anchor = Some(now);
            }
        }
        self
    }

    /// Validate the schedule: parse the pattern and resolve the zone.
    pub fn validate(&self) -> Result<(), ScheduleError> {
        match self {
            Schedule::Cron {
                cron,
                timezone,
                day_or,
            } => {
                resolve_tz(timezone.as_deref())?;
                parse_cron(cron, *day_or)?;
                Ok(())
            }
            Schedule::Interval {
                interval, timezone, ..
            } => {
                resolve_tz(timezone.as_deref())?;
                if *interval <= 0.0 || interval.is_nan() {
                    return Err(ScheduleError::Interval);
                }
                Ok(())
            }
            Schedule::RRule { rrule, timezone } => {
                let tz = resolve_tz(timezone.as_deref())?;
                parse_rrule(rrule, tz)?;
                Ok(())
            }
            Schedule::Continuous { delay } => {
                if *delay < 0.0 || delay.is_nan() {
                    return Err(ScheduleError::Invalid(
                        "a continuous schedule's delay must be zero or more".into(),
                    ));
                }
                Ok(())
            }
        }
    }

    /// Whether this is a continuous schedule, which has no fire times.
    pub fn is_continuous(&self) -> bool {
        matches!(self, Schedule::Continuous { .. })
    }

    pub fn timezone_name(&self) -> String {
        let tz = match self {
            Schedule::Cron { timezone, .. }
            | Schedule::Interval { timezone, .. }
            | Schedule::RRule { timezone, .. } => timezone.as_deref(),
            Schedule::Continuous { .. } => None,
        };
        resolve_tz(tz)
            .map(|t| t.name().to_string())
            .unwrap_or_else(|_| "UTC".into())
    }

    /// The first fire strictly after `after`.
    ///
    /// Compiles the schedule, then delegates. A caller that *walks* — `next_fires`,
    /// `fires_between` — must not come through here: it would recompile per step,
    /// which is the waste this split exists to remove. Those call `compile` once
    /// and loop on the result.
    pub fn next_after(&self, after: DateTime<Utc>) -> Result<Option<DateTime<Utc>>, ScheduleError> {
        self.compile()?.next_after(after)
    }

    /// The next `count` fires strictly after `after`, in microseconds UTC, after
    /// validating the schedule. What `cereyan check` previews.
    pub fn next_fires(&self, after: Micros, count: usize) -> Result<Vec<Micros>, ScheduleError> {
        self.validate()?;
        let mut out = Vec::with_capacity(count);
        let mut cursor = DateTime::<Utc>::from_timestamp_micros(after).ok_or_else(|| {
            ScheduleError::Invalid(format!("reference time {after} is out of range"))
        })?;
        // Compiled once, not once per fire. See `Compiled`.
        let compiled = self.compile()?;
        while out.len() < count {
            match compiled.next_after(cursor)? {
                Some(next) => {
                    out.push(next.timestamp_micros());
                    cursor = next;
                }
                None => break,
            }
        }
        Ok(out)
    }

    /// Fires strictly after `start` and at or before `end`, capped.
    pub fn fires_between(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        max: usize,
    ) -> Result<Vec<DateTime<Utc>>, ScheduleError> {
        let mut out = Vec::new();
        let mut cursor = start;
        // Compiled once, not once per fire. See `Compiled`.
        let compiled = self.compile()?;
        while out.len() < max {
            match compiled.next_after(cursor)? {
                Some(next) if next <= end => {
                    out.push(next);
                    cursor = next;
                }
                _ => break,
            }
        }
        Ok(out)
    }

    /// The last `count` fires at or before `end` and at or after `floor`,
    /// most recent first.
    ///
    /// The mirror of [`Schedule::fires_between`], for the question "when did this
    /// last run, and when before that". Walking forward from `floor` to answer it
    /// visits every fire in the window; walking back from `end` visits `count`.
    ///
    /// The bounds match `fires_between`'s: it returns fires in `(start, end]`, and
    /// this returns fires in `[floor, end]`. The lower end differs because the two
    /// walks cannot both be exclusive -- a backwards walk that skipped a fire
    /// exactly at `floor` would drop the oldest one it was asked for.
    ///
    /// Returns fewer than `count` when the schedule has not fired that often
    /// inside the window, and none for a schedule with no backwards walk.
    pub fn fires_before(
        &self,
        end: DateTime<Utc>,
        floor: DateTime<Utc>,
        count: usize,
    ) -> Result<Vec<DateTime<Utc>>, ScheduleError> {
        let compiled = self.compile()?;
        let mut out = Vec::with_capacity(count.min(64));
        let mut cursor = end;
        while out.len() < count {
            let Some(previous) = compiled.previous_from(cursor, floor) else {
                break;
            };
            if out.last().is_some_and(|last: &DateTime<Utc>| *last == previous) {
                // A schedule that fires twice in the same instant would otherwise
                // loop here forever. Not reachable for a cron, which has a
                // one-second resolution, but the walk must terminate.
                break;
            }
            out.push(previous);
            cursor = previous - Duration::microseconds(1);
        }
        Ok(out)
    }
}

/// The instant a local time names in `tz`.
///
/// Daylight saving removes some local times, and one that does not exist has
/// no instant: the fire belongs at the first instant after the gap, which is
/// what cron already does. The shift is not always an hour — Lord Howe moves
/// thirty minutes — so the boundary is found by stepping rather than assumed.
/// The search is bounded, so an unexpected zone cannot spin; a local time that
/// happens twice resolves to the earlier instant, so a run happens once.
fn local_to_instant(tz: Tz, naive: chrono::NaiveDateTime) -> Option<DateTime<Tz>> {
    if let Some(dt) = tz.from_local_datetime(&naive).earliest() {
        return Some(dt);
    }
    for minutes in 1..=180 {
        if let Some(dt) = tz
            .from_local_datetime(&(naive + Duration::minutes(minutes)))
            .earliest()
        {
            return Some(dt);
        }
    }
    None
}

fn parse_cron(expr: &str, day_or: bool) -> Result<croner::Cron, ScheduleError> {
    let mut parser =
        croner::parser::CronParser::builder().seconds(croner::parser::Seconds::Optional);
    if !day_or {
        parser = parser.dom_and_dow(true);
    }
    parser.build().parse(expr).map_err(|e| ScheduleError::Cron {
        expr: expr.to_string(),
        message: e.to_string(),
    })
}

/// Parse an rrule set, resolving a `DTSTART` that names no zone in `tz`.
///
/// iCalendar lets `DTSTART` carry its own zone, as a trailing `Z` or a `TZID=`,
/// and one that does keeps it. One that does not is resolved by the rrule crate
/// against the machine's zone, which ignores the schedule's `timezone` and is
/// how a schedule documented as 02:30 New York came to fire at 02:30 UTC. So
/// the zone is written in before parsing.
fn parse_rrule(text: &str, tz: Tz) -> Result<rrule::RRuleSet, ScheduleError> {
    let upper = text.to_ascii_uppercase();
    if !upper.contains("DTSTART") {
        return Err(ScheduleError::RRule("rrule must include DTSTART".into()));
    }
    let normalized = text.replace("\\n", "\n");
    let normalized = if dtstart_names_a_zone(&normalized) {
        normalized
    } else {
        normalized.replacen("DTSTART:", &format!("DTSTART;TZID={}:", tz.name()), 1)
    };
    rrule::RRuleSet::from_str(&normalized).map_err(|e| ScheduleError::RRule(e.to_string()))
}

/// Whether the `DTSTART` line states a zone of its own.
fn dtstart_names_a_zone(text: &str) -> bool {
    text.lines()
        .find(|line| line.to_ascii_uppercase().starts_with("DTSTART"))
        .is_some_and(|line| {
            let upper = line.to_ascii_uppercase();
            upper.contains("TZID=") || upper.trim_end().ends_with('Z')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn cron_wall_clock_across_dst() {
        // Istanbul does not observe DST anymore; use New York for a DST test.
        let s = Schedule::Cron {
            cron: "0 9 * * *".into(),
            timezone: Some("America/New_York".into()),
            day_or: true,
        };
        // 2026-03-07 09:00 EST is 14:00 UTC; DST starts 2026-03-08.
        let next = s.next_after(utc("2026-03-07T14:00:00Z")).unwrap().unwrap();
        assert_eq!(next, utc("2026-03-08T13:00:00Z"));
        let next2 = s.next_after(next).unwrap().unwrap();
        assert_eq!(next2, utc("2026-03-09T13:00:00Z"));
    }

    #[test]
    fn cron_in_istanbul() {
        let s = Schedule::Cron {
            cron: "0 9 * * *".into(),
            timezone: Some("Europe/Istanbul".into()),
            day_or: true,
        };
        let next = s.next_after(utc("2026-09-06T05:59:00Z")).unwrap().unwrap();
        assert_eq!(next, utc("2026-09-06T06:00:00Z"));
    }

    #[test]
    fn invalid_cron_and_timezone() {
        let bad = Schedule::Cron {
            cron: "not a cron".into(),
            timezone: None,
            day_or: true,
        };
        assert!(matches!(bad.validate(), Err(ScheduleError::Cron { .. })));
        let bad_tz = Schedule::Cron {
            cron: "* * * * *".into(),
            timezone: Some("Mars/Olympus".into()),
            day_or: true,
        };
        assert!(matches!(bad_tz.validate(), Err(ScheduleError::Timezone(_))));
    }

    #[test]
    fn short_interval_is_elapsed_time() {
        let anchor = utc("2026-03-08T05:00:00Z"); // midnight EST, DST at 2am local
        let s = Schedule::Interval {
            interval: 3600.0,
            anchor: Some(to_micros(anchor)),
            timezone: Some("America/New_York".into()),
        };
        let fires = s
            .fires_between(anchor, anchor + Duration::hours(5), 10)
            .unwrap();
        assert_eq!(fires.len(), 5);
        assert_eq!(fires[0], anchor + Duration::hours(1));
        assert_eq!(fires[4], anchor + Duration::hours(5));
    }

    #[test]
    fn daily_interval_keeps_local_time_across_dst() {
        // 09:00 New York on 2026-03-07 (EST) = 14:00Z; next day (EDT) = 13:00Z.
        let anchor = utc("2026-03-07T14:00:00Z");
        let s = Schedule::Interval {
            interval: 86_400.0,
            anchor: Some(to_micros(anchor)),
            timezone: Some("America/New_York".into()),
        };
        let next = s.next_after(anchor).unwrap().unwrap();
        assert_eq!(next, utc("2026-03-08T13:00:00Z"));
    }

    #[test]
    fn daily_interval_across_the_spring_forward_gap() {
        // 02:30 New York on 2026-03-07 (EST, UTC-5) is 07:30Z. On 2026-03-08
        // the clocks jump 02:00 -> 03:00, so 02:30 does not exist that day and
        // the fire belongs at the first instant after the gap: 03:00 EDT, 07:00Z.
        let anchor = utc("2026-03-07T07:30:00Z");
        let s = Schedule::Interval {
            interval: 86_400.0,
            anchor: Some(to_micros(anchor)),
            timezone: Some("America/New_York".into()),
        };
        let next = s.next_after(anchor).unwrap().unwrap();
        assert_eq!(next, utc("2026-03-08T07:00:00Z"));
        // The day after the gap is back to the anchor's wall-clock time.
        let after = s.next_after(next).unwrap().unwrap();
        assert_eq!(after, utc("2026-03-09T06:30:00Z"));
    }

    #[test]
    fn daily_interval_across_a_half_hour_gap() {
        // Lord Howe moves thirty minutes: 2026-10-04 goes 02:00 -> 02:30, so a
        // 02:15 schedule has no time that day. A fix that assumed an hour
        // would land at 03:15 instead of at the end of the gap.
        let anchor = utc("2026-10-02T15:45:00Z"); // 2026-10-03 02:15 local
        let tz: Tz = "Australia/Lord_Howe".parse().unwrap();
        let s = Schedule::Interval {
            interval: 86_400.0,
            anchor: Some(to_micros(anchor)),
            timezone: Some("Australia/Lord_Howe".into()),
        };
        let local = s.next_after(anchor).unwrap().unwrap().with_timezone(&tz);
        assert_eq!(local.date_naive().to_string(), "2026-10-04");
        assert_eq!(local.time().to_string(), "02:30:00");
    }

    #[test]
    fn daily_interval_fires_once_when_the_clocks_go_back() {
        // 2026-11-01 New York goes 02:00 -> 01:00, so 01:30 happens twice; the
        // run belongs at the earlier one, and once.
        let anchor = utc("2026-10-31T05:30:00Z"); // 01:30 EDT
        let tz: Tz = "America/New_York".parse().unwrap();
        let s = Schedule::Interval {
            interval: 86_400.0,
            anchor: Some(to_micros(anchor)),
            timezone: Some("America/New_York".into()),
        };
        let first = s.next_after(anchor).unwrap().unwrap();
        assert_eq!(first, utc("2026-11-01T05:30:00Z"));
        let second = s.next_after(first).unwrap().unwrap();
        assert_eq!(
            second.with_timezone(&tz).date_naive().to_string(),
            "2026-11-02",
            "the next fire is the following day, not the second 01:30"
        );
    }

    #[test]
    fn cron_and_interval_agree_across_the_gap() {
        // The interval rule exists because cron already had one; if they ever
        // disagree about the same wall-clock time, one of them is wrong.
        let after = utc("2026-03-07T07:30:00Z");
        let cron = Schedule::Cron {
            cron: "30 2 * * *".into(),
            timezone: Some("America/New_York".into()),
            day_or: true,
        };
        let interval = Schedule::Interval {
            interval: 86_400.0,
            anchor: Some(to_micros(after)),
            timezone: Some("America/New_York".into()),
        };
        assert_eq!(
            cron.next_after(after).unwrap().unwrap(),
            interval.next_after(after).unwrap().unwrap()
        );
    }

    #[test]
    fn fires_between_does_not_skip_the_gap_day() {
        // The look-ahead, the upcoming list and the projected fires all go
        // through fires_between, which is why the missing day was invisible.
        let anchor = utc("2026-03-06T07:30:00Z"); // 02:30 EST on the 6th
        let tz: Tz = "America/New_York".parse().unwrap();
        let s = Schedule::Interval {
            interval: 86_400.0,
            anchor: Some(to_micros(anchor)),
            timezone: Some("America/New_York".into()),
        };
        let days: Vec<String> = s
            .fires_between(anchor, utc("2026-03-10T12:00:00Z"), 10)
            .unwrap()
            .iter()
            .map(|f| f.with_timezone(&tz).date_naive().to_string())
            .collect();
        assert_eq!(
            days,
            ["2026-03-07", "2026-03-08", "2026-03-09", "2026-03-10"]
        );
    }

    #[test]
    fn rrule_fire_after_one_of_its_own_occurrences() {
        // What the scheduler asks every time it has just materialised a run.
        // The underlying filter is inclusive, so fetching one occurrence used
        // to answer with the cursor itself and the series stopped after one.
        let s = Schedule::RRule {
            rrule: "DTSTART:20260301T073000Z\nRRULE:FREQ=DAILY".into(),
            timezone: Some("UTC".into()),
        };
        let on_occurrence = utc("2026-03-07T07:30:00Z");
        assert_eq!(
            s.next_after(on_occurrence).unwrap().unwrap(),
            utc("2026-03-08T07:30:00Z")
        );
        // A cursor between occurrences already worked, and still must.
        assert_eq!(
            s.next_after(utc("2026-03-07T09:00:00Z")).unwrap().unwrap(),
            utc("2026-03-08T07:30:00Z")
        );
    }

    #[test]
    fn rrule_fires_between_returns_the_series() {
        // fires_between walks with `cursor = next`, so every step after the
        // first is the on-boundary case: this came back with one date.
        let s = Schedule::RRule {
            rrule: "DTSTART:20260301T073000Z\nRRULE:FREQ=DAILY".into(),
            timezone: Some("UTC".into()),
        };
        let fires = s
            .fires_between(utc("2026-03-06T00:00:00Z"), utc("2026-03-10T00:00:00Z"), 10)
            .unwrap();
        assert_eq!(
            fires,
            [
                utc("2026-03-06T07:30:00Z"),
                utc("2026-03-07T07:30:00Z"),
                utc("2026-03-08T07:30:00Z"),
                utc("2026-03-09T07:30:00Z"),
            ]
        );
    }

    #[test]
    fn rrule_dtstart_without_a_zone_uses_the_schedules_timezone() {
        // The documented form: a plain DTSTART and a separate timezone. This
        // fired at 02:30 UTC, five hours from where the schedule said.
        let s = Schedule::RRule {
            rrule: "DTSTART:20260301T023000\nRRULE:FREQ=DAILY".into(),
            timezone: Some("America/New_York".into()),
        };
        let tz: Tz = "America/New_York".parse().unwrap();
        let next = s.next_after(utc("2026-03-05T00:00:00Z")).unwrap().unwrap();
        let local = next.with_timezone(&tz);
        assert_eq!(local.time().to_string(), "02:30:00");
        assert_eq!(next, utc("2026-03-05T07:30:00Z"), "02:30 EST is 07:30Z");
    }

    #[test]
    fn rrule_dtstart_with_its_own_zone_keeps_it() {
        // iCalendar lets DTSTART carry a zone; when it does, the schedule's
        // timezone does not move the instants.
        let utc_form = Schedule::RRule {
            rrule: "DTSTART:20260301T073000Z\nRRULE:FREQ=DAILY".into(),
            timezone: Some("America/New_York".into()),
        };
        assert_eq!(
            utc_form
                .next_after(utc("2026-03-05T00:00:00Z"))
                .unwrap()
                .unwrap(),
            utc("2026-03-05T07:30:00Z")
        );
        let tzid_form = Schedule::RRule {
            rrule: "DTSTART;TZID=America/New_York:20260301T023000\nRRULE:FREQ=DAILY".into(),
            timezone: Some("UTC".into()),
        };
        assert_eq!(
            tzid_form
                .next_after(utc("2026-03-05T00:00:00Z"))
                .unwrap()
                .unwrap(),
            utc("2026-03-05T07:30:00Z"),
            "02:30 New York, not 02:30 UTC"
        );
    }

    #[test]
    fn rrule_weekly() {
        let s = Schedule::RRule {
            rrule: "DTSTART:20260901T090000Z\nRRULE:FREQ=WEEKLY;BYDAY=MO".into(),
            timezone: Some("UTC".into()),
        };
        let next = s.next_after(utc("2026-09-06T00:00:00Z")).unwrap().unwrap();
        assert_eq!(next, utc("2026-09-07T09:00:00Z"));
        assert!(matches!(
            Schedule::RRule {
                rrule: "RRULE:FREQ=DAILY".into(),
                timezone: None
            }
            .validate(),
            Err(ScheduleError::RRule(_))
        ));
    }

    #[test]
    fn fires_between_is_capped() {
        let s = Schedule::Cron {
            cron: "* * * * *".into(),
            timezone: Some("UTC".into()),
            day_or: true,
        };
        let start = utc("2026-01-01T00:00:00Z");
        let fires = s
            .fires_between(start, start + Duration::hours(3), 100)
            .unwrap();
        assert_eq!(fires.len(), 100);
        assert_eq!(fires[0], start + Duration::minutes(1));
    }

    #[test]
    fn missing_anchor_is_deterministic() {
        let s = Schedule::Interval {
            interval: 3600.0,
            anchor: None,
            timezone: None,
        };
        let after = utc("2026-01-01T00:30:00Z");
        assert_eq!(
            s.next_after(after).unwrap().unwrap(),
            utc("2026-01-01T01:00:00Z")
        );
        assert_eq!(
            s.clone().with_anchor_if_missing(5),
            Schedule::Interval {
                interval: 3600.0,
                anchor: Some(5),
                timezone: None
            }
        );
    }

    #[test]
    fn catchup_policy_parse() {
        assert_eq!(CatchupPolicy::parse("ALL"), Some(CatchupPolicy::All));
        assert_eq!(CatchupPolicy::parse("nope"), None);
    }

    #[test]
    fn continuous_has_no_fire_times() {
        let s = Schedule::Continuous { delay: 1800.0 };
        s.validate().unwrap();
        assert!(s.is_continuous());
        assert_eq!(s.next_after(utc("2026-03-01T00:00:00Z")).unwrap(), None);
        assert!(s.next_fires(0, 3).unwrap().is_empty());
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"kind": "continuous", "delay": 1800.0})
        );
        assert!(Schedule::Continuous { delay: -1.0 }.validate().is_err());
    }
}

#[cfg(test)]
mod walk_cost {
    use super::*;
    use std::time::Instant;

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    /// The backwards walk must agree with the forwards one, or the health check is
    /// showing a different deadline than it used to.
    ///
    /// Compared over expressions that fire at very different rates, in zones with
    /// and without daylight saving, because the two walks go through croner's
    /// opposite search directions and a disagreement would hide in the easy cases.
    #[test]
    fn walking_back_agrees_with_walking_forward() {
        let end = utc("2026-09-30T12:34:56Z");
        let cases: [(&str, Option<&str>); 8] = [
            ("* * * * *", None),
            ("*/7 * * * *", None),
            ("0 * * * *", None),
            ("0 3 * * *", Some("Europe/Istanbul")),
            ("0 0 1 1 *", None),
            ("30 2 * * SUN", Some("America/New_York")),
            ("0 0 * * *", Some("Australia/Lord_Howe")),
            ("15 14 1 * *", None),
        ];
        for (expr, tz) in cases {
            let s = Schedule::Cron {
                cron: expr.to_string(),
                timezone: tz.map(|t| t.to_string()),
                day_or: true,
            };
            // A window wide enough to hold a useful number of fires for even the
            // rarest expression, and one narrow enough to hold almost none -- both
            // ends matter, since a bug at either boundary loses a fire.
            // The forwards walk is capped, so over a wide window on a dense
            // expression it returns the *first* `CAP` fires and its "last four" are
            // months old. Comparing against that would be comparing against
            // nonsense -- which is how the first version of this test failed,
            // reporting the correct backwards answer as wrong. Where it truncates,
            // the forwards walk cannot be an oracle at all, so only the invariants
            // are checked there.
            const CAP: usize = 100_000;
            for days in [1i64, 3, 40, 400, 900] {
                let floor = end - Duration::days(days);
                let backwards = s.fires_before(end, floor, 4).unwrap();
                let forwards = s.fires_between(floor, end, CAP).unwrap();

                // Invariants that hold either way.
                assert!(
                    backwards.len() <= 4,
                    "{expr} over {days} days: asked for 4, got {}",
                    backwards.len()
                );
                for f in &backwards {
                    assert!(
                        *f > floor && *f <= end,
                        "{expr} over {days} days: {f} is outside ({floor}, {end}]"
                    );
                }
                for w in backwards.windows(2) {
                    assert!(w[0] > w[1], "{expr}: most recent first, got {w:?}");
                }

                if forwards.len() >= CAP {
                    assert!(
                        forwards.iter().all(|f| *f <= end),
                        "the capped forwards walk should still only return fires in the window"
                    );
                    continue;
                }
                let mut expected: Vec<DateTime<Utc>> =
                    forwards.iter().rev().copied().take(4).collect();
                // The backwards walk includes a fire exactly at `floor`; the
                // forwards one excludes a fire exactly at its `start`. Align them.
                expected.retain(|f| *f > floor);

                assert_eq!(
                    backwards, expected,
                    "{expr} in {tz:?} over {days} days: backwards {backwards:?} vs \
                     forwards' last {} {expected:?}",
                    forwards.len()
                );
            }
        }
    }

    #[test]
    fn a_backwards_walk_stops_at_the_floor() {
        let s = Schedule::Cron {
            cron: "0 0 1 1 *".into(),
            timezone: None,
            day_or: true,
        };
        let end = utc("2026-09-30T00:00:00Z");
        // One fire per year, so a 400-day window holds at most one.
        let fires = s.fires_before(end, end - Duration::days(400), 4).unwrap();
        assert!(fires.len() <= 1, "a yearly cron cannot fire twice in 400 days: {fires:?}");
        for f in &fires {
            assert!(*f >= end - Duration::days(400), "nothing before the floor");
        }
        // And a window with no fire at all yields nothing.
        let none = s.fires_before(end, end - Duration::days(10), 4).unwrap();
        assert!(none.is_empty(), "no fire in the last ten days: {none:?}");
    }

    #[test]
    fn a_continuous_schedule_has_no_fires_in_either_direction() {
        let s = Schedule::Continuous { delay: 30.0 };
        let end = utc("2026-09-30T00:00:00Z");
        assert!(s.fires_before(end, end - Duration::days(800), 2).unwrap().is_empty());
        assert!(s.fires_between(end - Duration::days(800), end, 10).unwrap().is_empty());
    }

    /// How much of a walk is the walk, and how much is re-resolving the zone and
    /// re-parsing the expression on every step?
    ///
    /// Opt-in: `CEREYAN_BENCH_REPORT=1 cargo test --release -p cereyan-core --lib walk_cost -- --nocapture`
    #[test]
    fn report_fires_between_cost() {
        if std::env::var("CEREYAN_BENCH_REPORT").is_err() {
            return;
        }
        let s = Schedule::Cron {
            cron: "* * * * *".into(),
            timezone: None,
            day_or: true,
        };
        let end = from_micros(1_757_000_000_000_000i64);
        let start = end - Duration::days(1);

        let t = Instant::now();
        let fires = s.fires_between(start, end, 100_000).unwrap();
        let compiled_walk = t.elapsed().as_secs_f64() * 1e3;
        let n = fires.len().max(1) as i64;

        // What the same walk cost when each step recompiled: `next_after` through
        // a schedule it is called on, which is the shape the health check used.
        let per_step = |f: &dyn Fn() -> f64| {
            let t = Instant::now();
            for _ in 0..n {
                std::hint::black_box(f());
            }
            t.elapsed().as_secs_f64() * 1e3
        };
        let parse = per_step(&|| {
            std::hint::black_box(parse_cron("* * * * *", true).unwrap());
            0.0
        });
        let tz = per_step(&|| {
            std::hint::black_box(resolve_tz(None).unwrap());
            0.0
        });

        // And the two answers the health check actually wanted.
        let t = Instant::now();
        let back = s
            .fires_before(end, end - Duration::days(800), 2)
            .unwrap();
        let backwards = t.elapsed().as_secs_f64() * 1e3;

        println!(
            "per-minute cron, one-day window, {} fires:\n  \
             fires_between, compiled once   {compiled_walk:8.3} ms  ({:.2} us/fire)\n  \
             of which parse_cron x{n}         {parse:8.3} ms  ({:.2} us each)\n  \
             of which resolve_tz x{n}         {tz:8.3} ms  ({:.2} us each)\n  \
             fires_before(.., 2) -- the two   {backwards:8.3} ms  ({back:?})",
            fires.len(),
            compiled_walk * 1000.0 / n as f64,
            parse * 1000.0 / n as f64,
            tz * 1000.0 / n as f64,
        );
        println!(
            "  the old deadline_window walked these {} fires and re-parsed on each \
             step: about {:.1} ms",
            fires.len(),
            parse + tz + compiled_walk
        );
    }
}
