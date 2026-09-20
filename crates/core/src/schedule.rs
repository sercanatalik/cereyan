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
        }
    }

    pub fn timezone_name(&self) -> String {
        let tz = match self {
            Schedule::Cron { timezone, .. }
            | Schedule::Interval { timezone, .. }
            | Schedule::RRule { timezone, .. } => timezone.as_deref(),
        };
        resolve_tz(tz)
            .map(|t| t.name().to_string())
            .unwrap_or_else(|_| "UTC".into())
    }

    /// The first fire strictly after `after`.
    pub fn next_after(&self, after: DateTime<Utc>) -> Result<Option<DateTime<Utc>>, ScheduleError> {
        match self {
            Schedule::Cron {
                cron,
                timezone,
                day_or,
            } => {
                let tz = resolve_tz(timezone.as_deref())?;
                let parsed = parse_cron(cron, *day_or)?;
                let local = after.with_timezone(&tz);
                match parsed.find_next_occurrence(&local, false) {
                    Ok(next) => Ok(Some(next.with_timezone(&Utc))),
                    Err(_) => Ok(None),
                }
            }
            Schedule::Interval {
                interval,
                anchor,
                timezone,
            } => {
                let tz = resolve_tz(timezone.as_deref())?;
                // A missing anchor is the Unix epoch so evaluation is deterministic;
                // schedules created at runtime pin their anchor at creation.
                let anchor = from_micros(anchor.unwrap_or(0));
                if *interval <= 0.0 || interval.is_nan() {
                    return Err(ScheduleError::Interval);
                }
                let secs = *interval;
                if secs < 86_400.0 {
                    // Elapsed-time intervals: fixed seconds since the anchor.
                    let elapsed = (after - anchor).num_microseconds().unwrap_or(0) as f64 / 1e6;
                    let steps = if elapsed < 0.0 {
                        0.0
                    } else {
                        (elapsed / secs).floor() + 1.0
                    };
                    let next = anchor + Duration::microseconds((steps * secs * 1e6) as i64);
                    Ok(Some(next))
                } else {
                    // Wall-clock intervals: add whole days in the local zone so
                    // the fire keeps its local time across DST.
                    let days = (secs / 86_400.0).round().max(1.0) as i64;
                    let local_anchor = anchor.with_timezone(&tz);
                    let mut candidate = local_anchor;
                    let after_local = after.with_timezone(&tz);
                    if candidate > after_local {
                        return Ok(Some(candidate.with_timezone(&Utc)));
                    }
                    let elapsed_days =
                        (after_local.date_naive() - local_anchor.date_naive()).num_days();
                    let first = (elapsed_days / days).max(0);
                    for steps in (first..).take(4) {
                        let date = local_anchor.date_naive() + Duration::days(steps * days);
                        let naive = date.and_time(local_anchor.time());
                        candidate = local_to_instant(tz, naive).unwrap_or(candidate);
                        if candidate > after_local {
                            return Ok(Some(candidate.with_timezone(&Utc)));
                        }
                    }
                    Ok(Some(candidate.with_timezone(&Utc)))
                }
            }
            Schedule::RRule { rrule, timezone } => {
                let tz = resolve_tz(timezone.as_deref())?;
                let set = parse_rrule(rrule, tz)?;
                let after_tz: DateTime<rrule::Tz> = after.with_timezone(&rrule::Tz::Tz(tz));
                // `after` is an inclusive filter, so asking for one occurrence
                // answers with the cursor's own when the cursor sits on one —
                // which is precisely what the scheduler asks once it has made a
                // run. Look past it. A set carrying RDATEs can stack several
                // occurrences on one instant, so one spare is not enough.
                let result = set.after(after_tz).all(8);
                Ok(result
                    .dates
                    .into_iter()
                    .find(|d| d.with_timezone(&Utc) > after)
                    .map(|d| d.with_timezone(&Utc)))
            }
        }
    }

    /// The next `count` fires strictly after `after`, in microseconds UTC, after
    /// validating the schedule. What `cereyan check` previews.
    pub fn next_fires(&self, after: Micros, count: usize) -> Result<Vec<Micros>, ScheduleError> {
        self.validate()?;
        let mut out = Vec::with_capacity(count);
        let mut cursor = DateTime::<Utc>::from_timestamp_micros(after).ok_or_else(|| {
            ScheduleError::Invalid(format!("reference time {after} is out of range"))
        })?;
        while out.len() < count {
            match self.next_after(cursor)? {
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
        while out.len() < max {
            match self.next_after(cursor)? {
                Some(next) if next <= end => {
                    out.push(next);
                    cursor = next;
                }
                _ => break,
            }
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
}
