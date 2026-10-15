//! Agents defined in the UI, and the tool calls a person approved for good.

use cereyan_core::time::Micros;
use cereyan_core::{new_id, now_micros, Id};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Result, Store, StoreError};

/// A stored agent definition and where it is registered.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AgentRow {
    pub id: i64,
    pub external_id: Id,
    pub project: String,
    pub name: String,
    /// `ui`; `code` is reserved for agents declared in modules.
    pub source: String,
    pub enabled: bool,
    /// The flow the agent runs as, once registered.
    pub flow_id: Option<i64>,
    /// The definition as the UI stores it.
    #[cfg_attr(feature = "openapi", schema(value_type = Object))]
    pub definition: Value,
    pub created_at: Micros,
    pub updated_at: Micros,
}

#[derive(Clone, Debug, Default)]
pub struct AgentWrite {
    pub id: Option<i64>,
    pub project: String,
    pub name: String,
    pub source: String,
    pub enabled: bool,
    pub definition: String,
}

/// A tool call approved with "always allow": matched by tool and argument hash.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ApprovalRow {
    pub id: i64,
    pub agent_id: i64,
    pub tool: String,
    pub args_hash: String,
    #[cfg_attr(feature = "openapi", schema(value_type = Object))]
    pub args: Value,
    pub first_run_id: Option<i64>,
    pub created_at: Micros,
}

const AGENT_COLUMNS: &str =
    "id, external_id, project, name, source, enabled, flow_id, definition, created_at, updated_at";

fn agent_from_row(row: &Row<'_>) -> rusqlite::Result<AgentRow> {
    let blob: Vec<u8> = row.get(1)?;
    let definition: String = row.get(7)?;
    Ok(AgentRow {
        id: row.get(0)?,
        external_id: Id::from_bytes(&blob).unwrap_or_else(new_id),
        project: row.get(2)?,
        name: row.get(3)?,
        source: row.get(4)?,
        enabled: row.get::<_, i64>(5)? != 0,
        flow_id: row.get(6)?,
        definition: serde_json::from_str(&definition).unwrap_or(Value::Null),
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
    })
}

fn approval_from_row(row: &Row<'_>) -> rusqlite::Result<ApprovalRow> {
    let args: String = row.get(4)?;
    Ok(ApprovalRow {
        id: row.get(0)?,
        agent_id: row.get(1)?,
        tool: row.get(2)?,
        args_hash: row.get(3)?,
        args: serde_json::from_str(&args).unwrap_or(Value::Null),
        first_run_id: row.get(5)?,
        created_at: row.get(6)?,
    })
}

fn get_agent(conn: &Connection, id: i64) -> Result<Option<AgentRow>> {
    let sql = format!("SELECT {AGENT_COLUMNS} FROM agent WHERE id = ?1");
    Ok(conn.query_row(&sql, [id], agent_from_row).optional()?)
}

impl Store {
    pub fn list_agents(&self) -> Result<Vec<AgentRow>> {
        let sql = format!("SELECT {AGENT_COLUMNS} FROM agent ORDER BY project, name");
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(&sql)?;
            let rows = stmt
                .query_map([], agent_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    pub fn get_agent(&self, id: i64) -> Result<Option<AgentRow>> {
        self.with_reader(|conn| get_agent(conn, id))
    }

    /// Insert or update an agent; a new one with a taken (project, name) answers `Invalid`.
    pub fn upsert_agent(&self, w: AgentWrite) -> Result<AgentRow> {
        let value = self.exec(move |conn| {
            let now = now_micros();
            let id = match w.id {
                Some(id) => {
                    conn.execute(
                        "UPDATE agent SET project = ?1, name = ?2, source = ?3, enabled = ?4, definition = ?5, updated_at = ?6 WHERE id = ?7",
                        params![w.project, w.name, w.source, w.enabled as i64, w.definition, now, id],
                    )?;
                    id
                }
                None => {
                    let taken: Option<i64> = conn
                        .query_row(
                            "SELECT id FROM agent WHERE project = ?1 AND name = ?2",
                            params![w.project, w.name],
                            |r| r.get(0),
                        )
                        .optional()?;
                    if taken.is_some() {
                        return Err(StoreError::Invalid(format!(
                            "an agent named {}/{} already exists",
                            w.project, w.name
                        )));
                    }
                    let ext = new_id();
                    conn.execute(
                        "INSERT INTO agent (external_id, project, name, source, enabled, definition, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
                        params![ext.as_bytes().as_slice(), w.project, w.name, w.source, w.enabled as i64, w.definition, now],
                    )?;
                    conn.last_insert_rowid()
                }
            };
            let row = get_agent(conn, id)?.ok_or(StoreError::NotFound("agent"))?;
            serde_json::to_value(row).map_err(|e| StoreError::Invalid(e.to_string()))
        })?;
        serde_json::from_value(value).map_err(|e| StoreError::Invalid(e.to_string()))
    }

    pub fn set_agent_flow(&self, id: i64, flow_id: Option<i64>) -> Result<()> {
        self.exec(move |conn| {
            conn.execute(
                "UPDATE agent SET flow_id = ?1 WHERE id = ?2",
                params![flow_id, id],
            )?;
            Ok(Value::Null)
        })?;
        Ok(())
    }

    pub fn delete_agent(&self, id: i64) -> Result<bool> {
        let value = self.exec(move |conn| {
            let n = conn.execute("DELETE FROM agent WHERE id = ?1", [id])?;
            Ok(Value::Bool(n > 0))
        })?;
        Ok(value.as_bool().unwrap_or(false))
    }

    pub fn list_approvals(&self, agent_id: i64) -> Result<Vec<ApprovalRow>> {
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT id, agent_id, tool, args_hash, args, first_run_id, created_at FROM remembered_approval WHERE agent_id = ?1 ORDER BY id",
            )?;
            let rows = stmt
                .query_map([agent_id], approval_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// Remember a call; an identical one already remembered keeps its row.
    pub fn add_approval(
        &self,
        agent_id: i64,
        tool: String,
        args_hash: String,
        args: String,
        first_run_id: Option<i64>,
    ) -> Result<ApprovalRow> {
        let value = self.exec(move |conn| {
            conn.execute(
                "INSERT OR IGNORE INTO remembered_approval (agent_id, tool, args_hash, args, first_run_id, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![agent_id, tool, args_hash, args, first_run_id, now_micros()],
            )?;
            let row = conn.query_row(
                "SELECT id, agent_id, tool, args_hash, args, first_run_id, created_at FROM remembered_approval WHERE agent_id = ?1 AND tool = ?2 AND args_hash = ?3",
                params![agent_id, tool, args_hash],
                approval_from_row,
            )?;
            serde_json::to_value(row).map_err(|e| StoreError::Invalid(e.to_string()))
        })?;
        serde_json::from_value(value).map_err(|e| StoreError::Invalid(e.to_string()))
    }

    pub fn delete_approval(&self, agent_id: i64, id: i64) -> Result<bool> {
        let value = self.exec(move |conn| {
            let n = conn.execute(
                "DELETE FROM remembered_approval WHERE id = ?1 AND agent_id = ?2",
                params![id, agent_id],
            )?;
            Ok(Value::Bool(n > 0))
        })?;
        Ok(value.as_bool().unwrap_or(false))
    }

    /// What one agent's flow spent since `since`: runs, tokens and estimated cost,
    /// from the `usage` state each run publishes.
    pub fn agent_usage_since(&self, flow_id: i64, since: i64) -> Result<(i64, i64, f64)> {
        self.with_reader(|conn| {
            let row = conn.query_row(
                "SELECT COUNT(*),
                        COALESCE(SUM(CAST(json_extract(s.value, '$.input_tokens') AS INTEGER)
                                     + CAST(json_extract(s.value, '$.output_tokens') AS INTEGER)), 0),
                        COALESCE(SUM(CAST(json_extract(s.value, '$.cost_usd') AS REAL)), 0)
                 FROM task_state s JOIN run r ON r.id = s.run_id
                 WHERE s.scope = '' AND s.key = 'usage' AND r.flow_id = ?1 AND r.created_at >= ?2",
                params![flow_id, since],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, f64>(2)?)),
            )?;
            Ok(row)
        })
    }

    /// The estimated cost of every agent run created since `since`, from the
    /// `usage` state each run publishes; unknown costs count as zero.
    pub fn agent_cost_since(&self, since: i64) -> Result<f64> {
        self.with_reader(|conn| {
            let total: f64 = conn.query_row(
                "SELECT COALESCE(SUM(CAST(json_extract(s.value, '$.cost_usd') AS REAL)), 0)
                 FROM task_state s JOIN run r ON r.id = s.run_id JOIN flow f ON f.id = r.flow_id
                 WHERE s.scope = '' AND s.key = 'usage' AND r.created_at >= ?1
                   AND json_extract(f.options, '$.agent') = 1",
                [since],
                |r| r.get(0),
            )?;
            Ok(total)
        })
    }
}
