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
    let tx = conn.unchecked_transaction()?;
    for (i, sql) in MIGRATIONS.iter().enumerate() {
        let version = i as i64 + 1;
        if version > current {
            tx.execute_batch(sql)?;
        }
    }
    tx.execute_batch(&format!("PRAGMA user_version = {latest}"))?;
    tx.commit()?;
    Ok(())
}
