//! Store maintenance: projects and their removal, table counts, the backup
//! copy, and the reset.

use std::path::PathBuf;

use rusqlite::params;
use serde::Serialize;

use crate::writer::{DeletedCounts, FlowRows, ResetScope, WriteCommand};
use crate::{Result, Store};

/// Directory under the home that holds backup copies.
pub const BACKUP_DIR: &str = "backups";

/// A project and its flows, as the store knows them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectRow {
    pub name: String,
    pub flow_ids: Vec<i64>,
    /// Latest start time of any of its runs.
    pub last_run_at: Option<i64>,
}

/// What removing a project deletes, and whether a run of it is in progress.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ProjectCounts {
    pub flows: i64,
    pub runs: i64,
    pub schedules: i64,
    pub backfills: i64,
    pub events: i64,
    /// Runs that are Pending, Running, Paused or Cancelling.
    pub active_runs: i64,
}

/// Row counts for the Data tab and the reset dialog.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TableCounts {
    pub runs: i64,
    pub task_runs: i64,
    pub logs: i64,
    pub events: i64,
    pub artifacts: i64,
    pub variables: i64,
    /// Rules not registered from code.
    pub ui_rules: i64,
    /// Schedules not registered from code.
    pub ui_schedules: i64,
}

impl Store {
    /// Every project with at least one flow, by name.
    pub fn list_projects(&self) -> Result<Vec<ProjectRow>> {
        self.with_reader(|c| {
            let mut stmt = c.prepare_cached(
                "SELECT f.project, f.id, (SELECT MAX(r.start_time) FROM run r WHERE r.flow_id = f.id)
                 FROM flow f ORDER BY f.project, f.id",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, Option<i64>>(2)?,
                ))
            })?;
            let mut out: Vec<ProjectRow> = Vec::new();
            for row in rows {
                let (name, id, last) = row?;
                match out.last_mut() {
                    Some(p) if p.name == name => {
                        p.flow_ids.push(id);
                        p.last_run_at = p.last_run_at.max(last);
                    }
                    _ => out.push(ProjectRow {
                        name,
                        flow_ids: vec![id],
                        last_run_at: last,
                    }),
                }
            }
            Ok(out)
        })
    }

    /// What removing `project` would delete.
    pub fn project_counts(&self, project: &str) -> Result<ProjectCounts> {
        self.with_reader(|c| {
            let flows = "SELECT id FROM flow WHERE project = ?1";
            let n = |sql: String| -> Result<i64> {
                Ok(c.query_row(&sql, params![project], |r| r.get(0))?)
            };
            Ok(ProjectCounts {
                flows: n("SELECT COUNT(*) FROM flow WHERE project = ?1".into())?,
                runs: n(format!(
                    "SELECT COUNT(*) FROM run WHERE flow_id IN ({flows})"
                ))?,
                schedules: n(format!(
                    "SELECT COUNT(*) FROM schedule WHERE flow_id IN ({flows})"
                ))?,
                backfills: n(format!(
                    "SELECT COUNT(*) FROM backfill WHERE flow_id IN ({flows})"
                ))?,
                events: n(format!(
                    "SELECT COUNT(*) FROM event WHERE flow_id IN ({flows})"
                ))?,
                active_runs: n(format!(
                    "SELECT COUNT(*) FROM run WHERE flow_id IN ({flows})
                     AND state_type IN ('Pending', 'Running', 'Paused', 'Cancelling')"
                ))?,
            })
        })
    }

    pub fn table_counts(&self) -> Result<TableCounts> {
        self.with_reader(|c| {
            let n = |sql: &str| -> Result<i64> { Ok(c.query_row(sql, [], |r| r.get(0))?) };
            Ok(TableCounts {
                runs: n("SELECT COUNT(*) FROM run")?,
                task_runs: n("SELECT COUNT(*) FROM task_run")?,
                logs: n("SELECT COUNT(*) FROM log")?,
                events: n("SELECT COUNT(*) FROM event")?,
                artifacts: n("SELECT COUNT(*) FROM artifact")?,
                variables: n("SELECT COUNT(*) FROM variable")?,
                ui_rules: n("SELECT COUNT(*) FROM rule WHERE source != 'code'")?,
                ui_schedules: n("SELECT COUNT(*) FROM schedule WHERE source != 'code'")?,
            })
        })
    }

    /// Delete flows with everything they recorded. Logs and events go first,
    /// at most `batch` rows per transaction so other writes keep flowing; the
    /// flows go last, in one transaction.
    pub fn delete_flows_batched(&self, flow_ids: &[i64], batch: i64) -> Result<DeletedCounts> {
        let mut logs = 0;
        let mut events = 0;
        for (rows, total) in [(FlowRows::Log, &mut logs), (FlowRows::Event, &mut events)] {
            loop {
                let n = self.write(|reply| WriteCommand::DeleteFlowRows {
                    rows,
                    flow_ids: flow_ids.to_vec(),
                    limit: batch,
                    reply,
                })?;
                *total += n as i64;
                if (n as i64) < batch {
                    break;
                }
                self.incremental_vacuum(2000)?;
            }
        }
        let mut out = self.write(|reply| WriteCommand::DeleteFlows {
            flow_ids: flow_ids.to_vec(),
            reply,
        })?;
        out.logs += logs;
        out.events += events;
        Ok(out)
    }

    /// Write a consistent copy of the database to
    /// `<home>/backups/db-<YYYYMMDD-HHMMSS>.sqlite` (UTC) and return its path.
    pub fn backup(&self) -> Result<PathBuf> {
        let dir = self.home().join(BACKUP_DIR);
        std::fs::create_dir_all(&dir)?;
        let stamp = utc_stamp(cereyan_core::now_micros());
        let mut path = dir.join(format!("db-{stamp}.sqlite"));
        let mut n = 1;
        while path.exists() {
            path = dir.join(format!("db-{stamp}-{n}.sqlite"));
            n += 1;
        }
        let target = path.to_string_lossy().into_owned();
        self.write(|reply| WriteCommand::BackupTo {
            path: target,
            reply,
        })?;
        Ok(path)
    }

    /// Delete the history, or everything but what `live_flows` registered
    /// from code, then compact the file.
    pub fn reset(&self, scope: ResetScope, live_flows: &[i64]) -> Result<DeletedCounts> {
        let out = self.write(|reply| WriteCommand::Reset {
            scope,
            live_flows: live_flows.to_vec(),
            reply,
        })?;
        self.write(WriteCommand::Vacuum)?;
        Ok(out)
    }
}

/// `YYYYMMDD-HHMMSS` in UTC for a time in microseconds since the epoch.
fn utc_stamp(micros: i64) -> String {
    let secs = micros.div_euclid(1_000_000);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}{month:02}{day:02}-{:02}{:02}{:02}",
        rem / 3_600,
        rem % 3_600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::utc_stamp;

    #[test]
    fn stamps_in_utc() {
        assert_eq!(utc_stamp(0), "19700101-000000");
        assert_eq!(utc_stamp(951_782_400_000_000), "20000229-000000");
        assert_eq!(utc_stamp(1_789_418_730_000_000), "20260914-204530");
    }
}
