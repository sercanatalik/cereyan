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
    include_str!("../migrations/0012_schedule_policies.sql"),
];

pub fn latest_version() -> i64 {
    MIGRATIONS.len() as i64
}

/// Before a schema upgrade, copy the database to
/// `<home>/backups/pre-migration-v<current>-<stamp>.sqlite`. A fresh store
/// (version 0) and one already at the latest version need nothing. The copy
/// failing is a reason not to migrate: a migration that rebuilds a table on a
/// full disk is what the copy exists for. `CEREYAN_NO_MIGRATION_BACKUP=1` skips it.
pub fn backup_before_upgrade(conn: &Connection, home: &std::path::Path) -> Result<()> {
    let current: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let latest = latest_version();
    if current == 0 || current >= latest {
        return Ok(());
    }
    if std::env::var("CEREYAN_NO_MIGRATION_BACKUP").is_ok_and(|v| v == "1") {
        return Ok(());
    }
    let dir = home.join(crate::manage::BACKUP_DIR);
    let path = dir.join(format!(
        "pre-migration-v{current}-{}.sqlite",
        crate::manage::utc_stamp(cereyan_core::now_micros())
    ));
    let attempt = std::fs::create_dir_all(&dir)
        .map_err(StoreError::from)
        .and_then(|_| {
            conn.execute("VACUUM INTO ?1", [path.to_string_lossy().as_ref()])
                .map(|_| ())
                .map_err(StoreError::from)
        });
    match attempt {
        Ok(()) => {
            eprintln!(
                "cereyan: the store is at schema {current} and this version needs {latest}; wrote a copy to {} before migrating (CEREYAN_NO_MIGRATION_BACKUP=1 skips this)",
                path.display()
            );
            Ok(())
        }
        Err(e) => Err(StoreError::Migration(format!(
            "could not write the pre-migration copy {}: {e}; free disk space, or set CEREYAN_NO_MIGRATION_BACKUP=1 to migrate without it",
            path.display()
        ))),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pre_migration_copy_only_for_an_older_schema() {
        let home = tempfile::tempdir().unwrap();
        let conn = Connection::open(home.path().join("db.sqlite")).unwrap();
        apply(&conn).unwrap();
        // Latest: nothing to copy.
        backup_before_upgrade(&conn, home.path()).unwrap();
        assert!(!home.path().join("backups").exists());
        // Older: a copy named after the current version appears.
        conn.execute_batch(&format!("PRAGMA user_version = {}", latest_version() - 1))
            .unwrap();
        backup_before_upgrade(&conn, home.path()).unwrap();
        let copies: Vec<String> = std::fs::read_dir(home.path().join("backups"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(copies.len(), 1);
        assert!(copies[0].starts_with(&format!("pre-migration-v{}-", latest_version() - 1)));
        let copy = Connection::open(home.path().join("backups").join(&copies[0])).unwrap();
        let tables: i64 = copy
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name = 'run'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tables, 1);
    }

    #[test]
    fn a_fresh_store_makes_no_copy() {
        let home = tempfile::tempdir().unwrap();
        let conn = Connection::open(home.path().join("db.sqlite")).unwrap();
        backup_before_upgrade(&conn, home.path()).unwrap();
        assert!(!home.path().join("backups").exists());
    }
}
