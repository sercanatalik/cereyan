use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, Write};
use std::path::Path;

use rusqlite::{Connection, OpenFlags};
use std::fs::TryLockError;

use crate::error::StoreError;
use crate::{Result, LOCK_FILE};

/// Take the exclusive advisory lock on `<home>/db.lock`. The lock is released
/// by the OS when the process exits, so a crashed holder never leaves a stale
/// lock behind. On success the holder's PID is written into the file so a
/// second opener can report it.
pub fn take_lock(home: &Path) -> Result<File> {
    let path = home.join(LOCK_FILE);
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)?;
    let locked = match file.try_lock() {
        Ok(()) => true,
        Err(TryLockError::WouldBlock) => false,
        Err(TryLockError::Error(e)) => return Err(e.into()),
    };
    if !locked {
        let mut holder = String::new();
        let _ = file.read_to_string(&mut holder);
        let holder = holder.trim();
        return Err(StoreError::Locked {
            home: home.display().to_string(),
            holder: if holder.is_empty() {
                "unknown".into()
            } else {
                holder.into()
            },
        });
    }
    file.set_len(0)?;
    file.rewind()?;
    write!(file, "{}", std::process::id())?;
    file.flush()?;
    Ok(file)
}

const PRAGMAS: &str = "
    PRAGMA journal_mode = WAL;
    PRAGMA synchronous = NORMAL;
    PRAGMA busy_timeout = 5000;
    PRAGMA foreign_keys = ON;
    PRAGMA temp_store = MEMORY;
    PRAGMA cache_size = -65536;
    PRAGMA mmap_size = 268435456;
    PRAGMA journal_size_limit = 67108864;
    PRAGMA wal_autocheckpoint = 1000;
";

/// Open the single write connection, quarantining a corrupt file first.
pub fn open_writer(db_path: &Path) -> Result<Connection> {
    if db_path.exists() && !passes_quick_check(db_path) {
        quarantine(db_path)?;
    }
    let conn = Connection::open(db_path)?;
    // Incremental vacuum only takes effect on a database created with it.
    let _ = conn.execute_batch("PRAGMA auto_vacuum = INCREMENTAL");
    conn.execute_batch(PRAGMAS)?;
    Ok(conn)
}

pub fn open_reader(db_path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.execute_batch(
        "PRAGMA busy_timeout = 5000; PRAGMA temp_store = MEMORY; PRAGMA cache_size = -16384; PRAGMA mmap_size = 268435456;",
    )?;
    Ok(conn)
}

/// Files up to this size always get the full `quick_check` (it is cheap there).
const FULL_CHECK_MAX_BYTES: u64 = 64 * 1024 * 1024;

/// Integrity check at open. A full `PRAGMA quick_check` reads the whole file,
/// so large databases that were closed cleanly (empty or absent WAL) only get
/// the cheap structural checks: the header must parse, the schema must read,
/// and the file must be at least as long as its page count says.
fn passes_quick_check(db_path: &Path) -> bool {
    let conn = match Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_WRITE) {
        Ok(c) => c,
        Err(_) => return false,
    };
    let file_len = std::fs::metadata(db_path).map(|m| m.len()).unwrap_or(0);
    let wal_len = std::fs::metadata(db_path.with_extension("sqlite-wal"))
        .map(|m| m.len())
        .unwrap_or(0);
    let full = wal_len > 0 || file_len <= FULL_CHECK_MAX_BYTES;
    if full {
        return match conn.query_row("PRAGMA quick_check", [], |r| r.get::<_, String>(0)) {
            Ok(s) => s == "ok",
            Err(_) => false,
        };
    }
    let pages: i64 = match conn.query_row("PRAGMA page_count", [], |r| r.get(0)) {
        Ok(n) => n,
        Err(_) => return false,
    };
    let page_size: i64 = match conn.query_row("PRAGMA page_size", [], |r| r.get(0)) {
        Ok(n) => n,
        Err(_) => return false,
    };
    if (pages * page_size) as u64 > file_len {
        return false;
    }
    conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| {
        r.get::<_, i64>(0)
    })
    .is_ok()
}

fn quarantine(db_path: &Path) -> Result<()> {
    let ts = cereyan_core::now_micros() / 1_000_000;
    let target = db_path.with_file_name(format!("db.sqlite.corrupt-{ts}"));
    eprintln!(
        "warning: cereyan database failed its integrity check; moving it to {} and starting empty",
        target.display()
    );
    std::fs::rename(db_path, &target)?;
    for suffix in ["-wal", "-shm"] {
        let side = db_path.with_file_name(format!("db.sqlite{suffix}"));
        if side.exists() {
            let _ = std::fs::remove_file(side);
        }
    }
    Ok(())
}
