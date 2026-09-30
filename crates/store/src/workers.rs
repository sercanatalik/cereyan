//! The worker registry and run placement: which machines add processors, what
//! each can run, and where every run executed.

use cereyan_core::{now_micros, Worker};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde_json::{json, Map, Value};

use crate::writer::WriteCommand;
use crate::{Result, Store};

/// Runs of one flow that started on a host, by state: what a worker's status
/// page counts.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct HostFlowCounts {
    pub flow: String,
    pub completed: i64,
    pub failed: i64,
    pub crashed: i64,
    pub cancelled: i64,
    pub running: i64,
    /// When the flow's latest completed run here ended (microseconds).
    pub last_completed_at: Option<i64>,
}

const WORKER_COLUMNS: &str = "id, name, version, cpus, processors, labels, shared_paths, meta, \
     state, registered_at, last_seen_at";

fn worker_from_row(row: &Row<'_>) -> rusqlite::Result<Worker> {
    let map =
        |text: String| -> Map<String, Value> { serde_json::from_str(&text).unwrap_or_default() };
    Ok(Worker {
        id: row.get(0)?,
        name: row.get(1)?,
        version: row.get(2)?,
        cpus: row.get(3)?,
        processors: row.get(4)?,
        labels: map(row.get(5)?),
        shared_paths: serde_json::from_str(&row.get::<_, String>(6)?).unwrap_or_default(),
        meta: map(row.get(7)?),
        state: row.get(8)?,
        registered_at: row.get(9)?,
        last_seen_at: row.get(10)?,
    })
}

/// What a worker declares when it registers.
#[derive(Debug, Clone)]
pub struct WorkerRegistration {
    pub name: String,
    pub version: String,
    pub cpus: i64,
    pub processors: i64,
    pub labels: Map<String, Value>,
    pub shared_paths: Vec<String>,
    pub meta: Map<String, Value>,
}

impl Store {
    fn exec(&self, f: impl FnOnce(&Connection) -> Result<Value> + Send + 'static) -> Result<Value> {
        self.write(|reply| WriteCommand::Exec(Box::new(f), reply))
    }

    /// Record a worker by name, or refresh the record of one that registered
    /// before; either way it is online again.
    pub fn register_worker(&self, w: WorkerRegistration) -> Result<Worker> {
        let now = now_micros();
        let id = self.exec(move |conn| {
            let id: i64 = conn.query_row(
                "INSERT INTO worker (name, version, cpus, processors, labels, shared_paths, meta,
                                     state, registered_at, last_seen_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'online', ?8, ?8)
                 ON CONFLICT (name) DO UPDATE SET
                   version = excluded.version, cpus = excluded.cpus, processors = excluded.processors,
                   labels = excluded.labels, shared_paths = excluded.shared_paths, meta = excluded.meta,
                   state = CASE WHEN worker.state = 'draining' THEN 'draining' ELSE 'online' END,
                   last_seen_at = excluded.last_seen_at
                 RETURNING id",
                params![
                    w.name,
                    w.version,
                    w.cpus,
                    w.processors,
                    Value::Object(w.labels).to_string(),
                    json!(w.shared_paths).to_string(),
                    Value::Object(w.meta).to_string(),
                    now
                ],
                |r| r.get(0),
            )?;
            Ok(json!(id))
        })?;
        let id = id.as_i64().unwrap_or_default();
        self.get_worker(id)?
            .ok_or_else(|| crate::StoreError::NotFound("worker"))
    }

    /// Replace the flows a worker can run, each with its module fingerprint.
    pub fn set_worker_flows(&self, worker_id: i64, flows: Vec<(i64, String)>) -> Result<()> {
        self.exec(move |conn| {
            conn.execute("DELETE FROM worker_flow WHERE worker_id = ?1", [worker_id])?;
            let mut stmt = conn.prepare_cached(
                "INSERT OR REPLACE INTO worker_flow (worker_id, flow_id, module_hash) VALUES (?1, ?2, ?3)",
            )?;
            for (flow_id, hash) in flows {
                stmt.execute(params![worker_id, flow_id, hash])?;
            }
            Ok(Value::Null)
        })
        .map(|_| ())
    }

    /// A heartbeat: last seen now, and the latest metadata merged in.
    pub fn touch_worker(&self, worker_id: i64, meta: Map<String, Value>) -> Result<()> {
        let now = now_micros();
        self.exec(move |conn| {
            let current: Option<String> = conn
                .query_row("SELECT meta FROM worker WHERE id = ?1", [worker_id], |r| {
                    r.get(0)
                })
                .optional()?;
            let mut merged: Map<String, Value> = current
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or_default();
            merged.extend(meta);
            conn.execute(
                "UPDATE worker SET last_seen_at = ?1, meta = ?2,
                   state = CASE WHEN state = 'offline' THEN 'online' ELSE state END
                 WHERE id = ?3",
                params![now, Value::Object(merged).to_string(), worker_id],
            )?;
            Ok(Value::Null)
        })
        .map(|_| ())
    }

    /// `online`, `draining`, or `offline`.
    pub fn set_worker_state(&self, worker_id: i64, state: &str) -> Result<bool> {
        let state = state.to_string();
        self.exec(move |conn| {
            let n = conn.execute(
                "UPDATE worker SET state = ?1 WHERE id = ?2",
                params![state, worker_id],
            )?;
            Ok(json!(n > 0))
        })
        .map(|v| v.as_bool().unwrap_or(false))
    }

    pub fn set_worker_processors(&self, worker_id: i64, processors: i64) -> Result<bool> {
        self.exec(move |conn| {
            let n = conn.execute(
                "UPDATE worker SET processors = ?1 WHERE id = ?2",
                params![processors, worker_id],
            )?;
            Ok(json!(n > 0))
        })
        .map(|v| v.as_bool().unwrap_or(false))
    }

    /// Remove a worker and what it could run.
    pub fn delete_worker(&self, worker_id: i64) -> Result<bool> {
        self.exec(move |conn| {
            conn.execute("DELETE FROM worker_flow WHERE worker_id = ?1", [worker_id])?;
            let n = conn.execute("DELETE FROM worker WHERE id = ?1", [worker_id])?;
            Ok(json!(n > 0))
        })
        .map(|v| v.as_bool().unwrap_or(false))
    }

    pub fn list_workers(&self) -> Result<Vec<Worker>> {
        let sql = format!("SELECT {WORKER_COLUMNS} FROM worker ORDER BY name");
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(&sql)?;
            let rows = stmt
                .query_map([], worker_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    pub fn get_worker(&self, worker_id: i64) -> Result<Option<Worker>> {
        let sql = format!("SELECT {WORKER_COLUMNS} FROM worker WHERE id = ?1");
        self.with_reader(|conn| {
            Ok(conn
                .query_row(&sql, [worker_id], worker_from_row)
                .optional()?)
        })
    }

    /// Every worker's flows with the module fingerprint it reported.
    pub fn all_worker_flows(&self) -> Result<Vec<(i64, i64, String)>> {
        self.with_reader(|conn| {
            let mut stmt =
                conn.prepare_cached("SELECT worker_id, flow_id, module_hash FROM worker_flow")?;
            let rows = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// One worker's claimed flows, with the two flow columns a fingerprint
    /// check needs and the hash the worker reported. Filtered to `worker_id` by
    /// the `worker_flow` primary key, so this is an index range scan rather
    /// than a read of every worker's rows.
    ///
    /// A membership row whose flow no longer exists yields no row, matching the
    /// old behaviour of skipping it.
    pub fn worker_flow_details(&self, worker_id: i64) -> Result<Vec<(i64, String, String, String)>> {
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT wf.flow_id, f.source_dir, f.module, wf.module_hash
                 FROM worker_flow wf JOIN flow f ON f.id = wf.flow_id
                 WHERE wf.worker_id = ?1",
            )?;
            let rows = stmt
                .query_map(params![worker_id], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// How many flows each worker has claimed, in one grouped read.
    pub fn worker_flow_counts(&self) -> Result<Vec<(i64, i64)>> {
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT worker_id, COUNT(*) FROM worker_flow GROUP BY worker_id",
            )?;
            let rows = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// Hand a run to an engine: record where it runs and take the next lease.
    /// Returns the new lease.
    pub fn claim_run(
        &self,
        run_id: i64,
        host: &str,
        processor: Option<i64>,
        source_hash: Option<String>,
    ) -> Result<i64> {
        let host = host.to_string();
        self.exec(move |conn| {
            let lease: i64 = conn.query_row(
                "UPDATE run SET host = ?1, processor = ?2, lease = lease + 1,
                   source_hash = COALESCE(?3, source_hash)
                 WHERE id = ?4 RETURNING lease",
                params![host, processor, source_hash, run_id],
                |r| r.get(0),
            )?;
            Ok(json!(lease))
        })
        .map(|v| v.as_i64().unwrap_or_default())
    }

    /// The lease a run currently holds.
    pub fn run_lease(&self, run_id: i64) -> Result<Option<i64>> {
        self.with_reader(|conn| {
            Ok(conn
                .query_row("SELECT lease FROM run WHERE id = ?1", [run_id], |r| {
                    r.get(0)
                })
                .optional()?)
        })
    }

    /// Runs that executed on `host` and overlap the window, oldest first:
    /// (run id, flow name, processor, state type, start, end).
    #[allow(clippy::type_complexity)]
    pub fn runs_on_host(
        &self,
        host: &str,
        since: i64,
        limit: usize,
    ) -> Result<
        Vec<(
            i64,
            String,
            Option<i64>,
            Option<String>,
            Option<i64>,
            Option<i64>,
        )>,
    > {
        let host = host.to_string();
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT r.id, f.name, r.processor, r.state_type, r.start_time, r.end_time
                 FROM run r JOIN flow f ON f.id = r.flow_id
                 WHERE r.host = ?1 AND r.start_time IS NOT NULL
                   AND (r.end_time IS NULL OR r.end_time >= ?2)
                 ORDER BY r.start_time LIMIT ?3",
            )?;
            let rows = stmt
                .query_map(params![host, since, limit as i64], |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// Runs that started on `host` at or after `since`, counted per flow and
    /// state, busiest flows first, at most `limit` flows.
    pub fn runs_by_flow_on_host(
        &self,
        host: &str,
        since: i64,
        limit: usize,
    ) -> Result<Vec<HostFlowCounts>> {
        let host = host.to_string();
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT f.name,
                   SUM(r.state_type = 'Completed'), SUM(r.state_type = 'Failed'),
                   SUM(r.state_type = 'Crashed'), SUM(r.state_type = 'Cancelled'),
                   SUM(r.state_type IN ('Running', 'Cancelling')),
                   MAX(CASE WHEN r.state_type = 'Completed' THEN r.end_time END)
                 FROM run r JOIN flow f ON f.id = r.flow_id
                 WHERE r.host = ?1 AND r.start_time >= ?2
                 GROUP BY f.name ORDER BY COUNT(*) DESC, f.name LIMIT ?3",
            )?;
            let rows = stmt
                .query_map(params![host, since, limit as i64], |r| {
                    Ok(HostFlowCounts {
                        flow: r.get(0)?,
                        completed: r.get(1)?,
                        failed: r.get(2)?,
                        crashed: r.get(3)?,
                        cancelled: r.get(4)?,
                        running: r.get(5)?,
                        last_completed_at: r.get(6)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }
}
