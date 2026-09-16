use rusqlite::Connection;

use crate::error::StoreError;
use crate::Result;

/// Numbered migrations embedded at build time. Index 0 is version 1.
const MIGRATIONS: &[&str] = &[
    include_str!("../migrations/0001_init.sql"),
    include_str!("../migrations/0002_server.sql"),
    include_str!("../migrations/0003_scheduling.sql"),
    include_str!("../migrations/0004_observability.sql"),
    include_str!("../migrations/0005_counts_index.sql"),
    include_str!("../migrations/0006_expectations.sql"),
    include_str!("../migrations/0007_flow_group.sql"),
    include_str!("../migrations/0008_schedule_skips.sql"),
    include_str!("../migrations/0009_event_flow_index.sql"),
    include_str!("../migrations/0010_task_run_pass.sql"),
    include_str!("../migrations/0011_unique_schedule_fire.sql"),
];

pub fn latest_version() -> i64 {
    MIGRATIONS.len() as i64
}

pub fn apply(conn: &Connection) -> Result<()> {
    let current: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let latest = latest_version();
    if current > latest {
        return Err(StoreError::Downgrade {
            db: current,
            lib: latest,
        });
    }
    if current == latest {
        return Ok(());
    }
    // A migration that rebuilds a table drops the old one, and dropping a table
    // while foreign keys are enforced deletes its rows first, cascading into
    // children such as task_run_state. The pragma is a no-op inside a
    // transaction, so it is set here; `foreign_key_check` before the commit
    // takes over the enforcement it turns off.
    conn.execute_batch("PRAGMA foreign_keys = OFF")?;
    let migrated = migrate(conn, current, latest);
    let restored = conn.execute_batch("PRAGMA foreign_keys = ON");
    migrated?;
    restored?;
    Ok(())
}

fn migrate(conn: &Connection, current: i64, latest: i64) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    for (i, sql) in MIGRATIONS.iter().enumerate() {
        let version = i as i64 + 1;
        if version > current {
            tx.execute_batch(sql)?;
        }
    }
    if let Some(violation) = first_foreign_key_violation(&tx)? {
        return Err(StoreError::Migration(violation));
    }
    tx.execute_batch(&format!("PRAGMA user_version = {latest}"))?;
    tx.commit()?;
    Ok(())
}

/// The first row of `PRAGMA foreign_key_check`, as "child references a missing
/// row in parent". Nothing should reference a row the migrations did not carry
/// over, and an upgrade that broke one is refused rather than committed.
fn first_foreign_key_violation(conn: &Connection) -> Result<Option<String>> {
    let mut stmt = conn.prepare("PRAGMA foreign_key_check")?;
    let mut rows = stmt.query([])?;
    match rows.next()? {
        Some(row) => {
            let child: String = row.get(0)?;
            let parent: String = row.get(2)?;
            Ok(Some(format!(
                "{child} references a missing row in {parent}"
            )))
        }
        None => Ok(None),
    }
}
