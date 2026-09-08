use std::time::{Duration, Instant};

use cereyan_core::{State, StateType};
use cereyan_store::{ListRunsFilter, NewLog, Store, StoreError};
use tempfile::TempDir;

fn open(dir: &TempDir) -> Store {
    Store::open(dir.path()).expect("open store")
}

fn flow(store: &Store, project: &str, name: &str) -> i64 {
    store
        .upsert_flow(project, name, "pipeline", "/tmp/proj", None, "[]", "{}")
        .unwrap()
}

#[test]
fn creates_home_and_schema_on_first_open() {
    let dir = TempDir::new().unwrap();
    let home = dir.path().join("nested").join("home");
    let store = Store::open(&home).unwrap();
    assert!(home.join("db.sqlite").exists());
    assert!(home.join("db.lock").exists());
    let mode: String = store
        .with_reader(|c| Ok(c.query_row("PRAGMA journal_mode", [], |r| r.get(0))?))
        .unwrap();
    assert_eq!(mode, "wal");
}

#[test]
fn fresh_home_contains_only_expected_files() {
    let dir = TempDir::new().unwrap();
    {
        let store = open(&dir);
        let f = flow(&store, "etl", "daily");
        let (run, _) = store.create_run(f, "one", "{}", "[]").unwrap();
        store
            .transition_run(run, State::new(StateType::Pending), false)
            .unwrap();
        store
            .transition_run(run, State::new(StateType::Running), false)
            .unwrap();
        store
            .transition_run(run, State::new(StateType::Completed), false)
            .unwrap();
    }
    let mut names: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    // After a clean close the WAL is truncated; the -wal and -shm files may
    // or may not remain depending on the platform, so allow both forms.
    for n in &names {
        assert!(
            ["db.sqlite", "db.sqlite-wal", "db.sqlite-shm", "db.lock"].contains(&n.as_str()),
            "unexpected file {n}"
        );
    }
    assert!(names.contains(&"db.sqlite".to_string()));
    assert!(names.contains(&"db.lock".to_string()));
}

#[test]
fn lock_conflict_reports_holder_pid() {
    let dir = TempDir::new().unwrap();
    let _first = open(&dir);
    match Store::open(dir.path()) {
        Err(StoreError::Locked { holder, .. }) => {
            // Unix locks are advisory, so the holder's PID can still be read out of the
            // locked file. Windows locks are mandatory: the exclusive lock blocks the
            // read too, and the PID is reported as "unknown". Naming the holder is a
            // nicety; refusing the second opener is the requirement, and that holds on
            // both. Storing the PID outside db.lock would break the runtime-home
            // requirement that the home hold exactly four files.
            if cfg!(unix) {
                assert_eq!(holder, std::process::id().to_string());
            } else {
                assert!(
                    holder == std::process::id().to_string() || holder == "unknown",
                    "unexpected holder {holder:?}"
                );
            }
        }
        other => panic!("expected lock error, got {:?}", other.map(|_| ())),
    }
}

#[test]
fn lock_released_when_holder_drops() {
    let dir = TempDir::new().unwrap();
    {
        let _first = open(&dir);
    }
    let _second = open(&dir);
}

#[test]
fn crashed_holder_recovery_needs_no_cleanup() {
    // Simulate a crashed holder: a lock file with a stale PID but no OS lock.
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("db.lock"), "999999").unwrap();
    let _store = open(&dir);
}

#[test]
fn corrupt_file_is_quarantined() {
    let dir = TempDir::new().unwrap();
    {
        let store = open(&dir);
        flow(&store, "etl", "daily");
    }
    std::fs::write(dir.path().join("db.sqlite"), b"definitely not a database").unwrap();
    let store = open(&dir);
    assert!(store.list_flows(None).unwrap().is_empty());
    let quarantined = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .any(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("db.sqlite.corrupt-")
        });
    assert!(quarantined);
}

#[test]
fn downgrade_is_refused() {
    let dir = TempDir::new().unwrap();
    {
        let _store = open(&dir);
    }
    let conn = rusqlite::Connection::open(dir.path().join("db.sqlite")).unwrap();
    conn.execute_batch("PRAGMA user_version = 999").unwrap();
    drop(conn);
    match Store::open(dir.path()) {
        Err(StoreError::Downgrade { db, lib }) => {
            assert_eq!(db, 999);
            assert!(lib < 999);
        }
        other => panic!("expected downgrade error, got {:?}", other.map(|_| ())),
    }
}

#[test]
fn migration_upgrade_from_empty_db() {
    let dir = TempDir::new().unwrap();
    let conn = rusqlite::Connection::open(dir.path().join("db.sqlite")).unwrap();
    conn.execute_batch("PRAGMA user_version = 0").unwrap();
    drop(conn);
    let store = open(&dir);
    let v: i64 = store
        .with_reader(|c| Ok(c.query_row("PRAGMA user_version", [], |r| r.get(0))?))
        .unwrap();
    assert_eq!(v, cereyan_store::latest_schema_version());
}

#[test]
fn transitions_apply_rules_and_counters() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "etl", "daily");
    let (run, ext) = store.create_run(f, "one", "{\"x\": 1}", "[\"a\"]").unwrap();

    // Running from nothing is an invalid entry.
    let err = store
        .transition_run(run, State::new(StateType::Running), false)
        .unwrap_err();
    assert!(matches!(
        err,
        StoreError::RejectedWith {
            reason: "invalid-entry",
            ..
        }
    ));

    store
        .transition_run(run, State::new(StateType::Pending), false)
        .unwrap();
    store
        .transition_run(run, State::new(StateType::Running), false)
        .unwrap();
    let err = store
        .transition_run(run, State::new(StateType::Running), false)
        .unwrap_err();
    assert!(matches!(
        err,
        StoreError::RejectedWith {
            reason: "duplicate",
            ..
        }
    ));

    store
        .transition_run(
            run,
            State::new(StateType::Failed).with_message("ValueError: bad"),
            false,
        )
        .unwrap();
    let err = store
        .transition_run(run, State::new(StateType::Running), false)
        .unwrap_err();
    assert!(matches!(
        err,
        StoreError::RejectedWith {
            reason: "terminal",
            ..
        }
    ));

    let got = store.get_run(run).unwrap().unwrap();
    assert_eq!(got.external_id, ext);
    assert_eq!(got.state.state_type, StateType::Failed);
    assert_eq!(got.state.message.as_deref(), Some("ValueError: bad"));
    assert_eq!(got.failure_count, 1);
    assert_eq!(got.crash_count, 0);
    assert!(got.start_time.is_some());
    assert!(got.end_time.is_some());
    assert!(got.total_run_time.unwrap() >= 0);
    assert_eq!(got.parameters.get("x").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(got.tags, vec!["a".to_string()]);
    assert_eq!(got.project, "etl");
    assert_eq!(got.flow_name, "daily");

    // Forced transition out of a terminal state records the flag.
    let s = store
        .transition_run(run, State::new(StateType::Completed), true)
        .unwrap();
    assert_eq!(
        s.details.get("forced"),
        Some(&serde_json::Value::Bool(true))
    );
    assert_eq!(store.get_run_by_external_id(&ext).unwrap().unwrap().id, run);
}

#[test]
fn task_runs_and_logs() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "etl", "daily");
    let (run, _) = store.create_run(f, "one", "{}", "[]").unwrap();
    let (t0, _) = store
        .create_task_run(run, "load", "pipeline.load", "load-0")
        .unwrap();
    let (t1, _) = store
        .create_task_run(run, "load", "pipeline.load", "load-1")
        .unwrap();
    store
        .transition_task_run(t0, State::new(StateType::Pending), false)
        .unwrap();
    store
        .transition_task_run(t0, State::new(StateType::Running), false)
        .unwrap();
    store
        .transition_task_run(t0, State::new(StateType::Completed), false)
        .unwrap();
    let tasks = store.task_runs_by_run(run).unwrap();
    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks[0].id, t0);
    assert_eq!(tasks[0].state.state_type, StateType::Completed);
    assert_eq!(tasks[1].id, t1);
    assert_eq!(tasks[1].dynamic_key, "load-1");

    let logs: Vec<NewLog> = (0..3)
        .map(|i| NewLog {
            run_id: run,
            task_run_id: Some(t0),
            task_run_external_id: None,
            level: 20,
            logger: "cereyan.run".into(),
            timestamp: i,
            message: format!("line {i}"),
        })
        .collect();
    assert_eq!(store.append_logs(logs).unwrap(), 3);
    let got = store.logs_by_run(run, 0, 100).unwrap();
    assert_eq!(got.len(), 3);
    let after = store.logs_by_run(run, got[0].id, 100).unwrap();
    assert_eq!(after.len(), 2);
}

#[test]
fn flow_identity_is_project_and_name() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let a = flow(&store, "a", "daily_etl");
    let b = flow(&store, "b", "daily_etl");
    assert_ne!(a, b);
    // Upsert by key returns the same id and updates metadata.
    let a2 = store
        .upsert_flow(
            "a",
            "daily_etl",
            "jobs.daily",
            "/new/dir",
            Some("desc"),
            "[\"t\"]",
            "{}",
        )
        .unwrap();
    assert_eq!(a, a2);
    let flows = store.list_flows(Some("a")).unwrap();
    assert_eq!(flows.len(), 1);
    assert_eq!(flows[0].source_dir, "/new/dir");
    assert_eq!(flows[0].module, "jobs.daily");
    assert_eq!(flows[0].tags, vec!["t".to_string()]);
    assert_eq!(store.list_flows(None).unwrap().len(), 2);

    // Unique index enforced directly.
    let dup: rusqlite::Result<usize> = {
        let conn = rusqlite::Connection::open(dir.path().join("db.sqlite")).unwrap();
        conn.execute(
            "INSERT INTO flow (external_id, project, name, module, source_dir, created_at, last_seen_at) VALUES (x'00', 'a', 'daily_etl', 'm', '/d', 0, 0)",
            [],
        )
    };
    assert!(dup.is_err());
}

#[test]
fn list_runs_filters_and_keyset_cursor() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let fa = flow(&store, "a", "etl");
    let fb = flow(&store, "b", "etl");
    let mut ids = Vec::new();
    for i in 0..25 {
        let f = if i % 2 == 0 { fa } else { fb };
        let (run, _) = store
            .create_run(f, &format!("run-{i}"), "{}", "[]")
            .unwrap();
        store
            .transition_run(run, State::new(StateType::Pending), false)
            .unwrap();
        if i % 5 == 0 {
            store
                .transition_run(run, State::new(StateType::Running), false)
                .unwrap();
            store
                .transition_run(run, State::new(StateType::Failed), false)
                .unwrap();
        }
        ids.push(run);
    }
    let page = store
        .list_runs(&ListRunsFilter {
            limit: Some(10),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(page.items.len(), 10);
    assert_eq!(page.items[0].id, *ids.last().unwrap());
    let page2 = store
        .list_runs(&ListRunsFilter {
            limit: Some(10),
            cursor: page.next_cursor,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(page2.items.len(), 10);
    assert!(page2.items[0].id < page.items[9].id);
    let page3 = store
        .list_runs(&ListRunsFilter {
            limit: Some(10),
            cursor: page2.next_cursor,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(page3.items.len(), 5);
    assert!(page3.next_cursor.is_none());

    let failed = store
        .list_runs(&ListRunsFilter {
            state_type: Some("Failed".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(failed.items.len(), 5);
    let project_a = store
        .list_runs(&ListRunsFilter {
            project: Some("a".into()),
            limit: Some(500),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(project_a.items.len(), 13);
    assert!(project_a.items.iter().all(|r| r.project == "a"));
    let by_flow = store
        .list_runs(&ListRunsFilter {
            flow: Some("etl".into()),
            project: Some("b".into()),
            limit: Some(500),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(by_flow.items.len(), 12);
    assert!(store.run_name_exists("run-3").unwrap());
    assert!(!store.run_name_exists("nope").unwrap());
}

#[test]
fn burst_inserts_are_batched() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "etl", "daily");
    let (run, _) = store.create_run(f, "one", "{}", "[]").unwrap();
    let before = store.commit_count();
    let start = Instant::now();
    let mut pending = Vec::with_capacity(10_000);
    for i in 0..10_000 {
        let log = NewLog {
            run_id: run,
            task_run_id: None,
            task_run_external_id: None,
            level: 20,
            logger: "burst".into(),
            timestamp: i,
            message: "m".into(),
        };
        pending.push(
            store
                .submit(|reply| cereyan_store::WriteCommand::AppendLogs(vec![log], reply))
                .unwrap(),
        );
    }
    let submitted_in = start.elapsed();
    for rx in pending {
        rx.recv().unwrap().unwrap();
    }
    let commits = store.commit_count() - before;
    assert_eq!(store.logs_by_run(run, 0, 10_000).unwrap().len(), 10_000);
    assert!(
        submitted_in < Duration::from_millis(100),
        "submitting took {submitted_in:?}"
    );
    assert!(commits <= 20, "10,000 rows took {commits} transactions");
}

#[test]
fn write_acknowledged_after_commit() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "etl", "daily");
    let (run, _) = store.create_run(f, "one", "{}", "[]").unwrap();
    // A second, independent connection sees the row right after the ack.
    let conn = rusqlite::Connection::open_with_flags(
        dir.path().join("db.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM run WHERE id = ?1", [run], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(n, 1);
}

fn seed_runs(store: &Store, flows: &[i64], total: usize) {
    let conn = rusqlite::Connection::open(store.home().join("db.sqlite")).unwrap();
    conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = OFF;")
        .unwrap();
    let tx = conn.unchecked_transaction().unwrap();
    {
        let mut stmt = tx
            .prepare(
                "INSERT INTO run (external_id, flow_id, name, state_type, state_name, created_at, start_time, end_time)
                 VALUES (?1, ?2, ?3, 'Completed', 'Completed', ?4, ?4, ?4)",
            )
            .unwrap();
        for i in 0..total {
            let id = cereyan_core::new_id();
            stmt.execute(rusqlite::params![
                id.as_bytes().as_slice(),
                flows[i % flows.len()],
                format!("r{i}"),
                i as i64
            ])
            .unwrap();
        }
    }
    tx.commit().unwrap();
}

fn latest_runs_timing(total: usize) {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let flows: Vec<i64> = (0..20)
        .map(|i| flow(&store, "p", &format!("f{i}")))
        .collect();
    seed_runs(&store, &flows, total);
    // Warm up the page cache once, then measure.
    let filter = ListRunsFilter {
        flow_id: Some(flows[3]),
        limit: Some(50),
        ..Default::default()
    };
    store.list_runs(&filter).unwrap();
    let start = Instant::now();
    let page = store.list_runs(&filter).unwrap();
    let elapsed = start.elapsed();
    assert_eq!(page.items.len(), 50);
    assert!(
        elapsed < Duration::from_millis(10),
        "latest runs query took {elapsed:?} at {total} runs"
    );
    let filter = ListRunsFilter {
        project: Some("p".into()),
        limit: Some(50),
        ..Default::default()
    };
    let start = Instant::now();
    let page = store.list_runs(&filter).unwrap();
    let elapsed = start.elapsed();
    assert_eq!(page.items.len(), 50);
    assert!(
        elapsed < Duration::from_millis(10),
        "project runs query took {elapsed:?} at {total} runs"
    );
}

#[test]
#[ignore = "wall-clock ceiling; calibrated hardware only. Run with --ignored or via just bench"]
fn latest_runs_query_is_index_backed_at_200k() {
    latest_runs_timing(200_000);
}

#[test]
#[ignore = "seeds one million runs; run with --ignored"]
fn latest_runs_query_is_index_backed_at_1m() {
    latest_runs_timing(1_000_000);
}

#[test]
#[ignore = "wall-clock ceiling; calibrated hardware only. Run with --ignored or via just bench"]
fn bulk_create_10k_runs_is_fast() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "etl", "daily");
    let cmds: Vec<cereyan_store::CreateRun> = (0..10_000)
        .map(|i| cereyan_store::CreateRun {
            flow_id: f,
            name: format!("r{i}"),
            parameters: "{}".into(),
            tags: "[]".into(),
            created_by: "backfill:1".into(),
            initial_state: Some(State::new(StateType::Scheduled)),
            backfill_id: Some(1),
            ..Default::default()
        })
        .collect();
    let start = Instant::now();
    let created = store.create_runs_bulk(cmds).unwrap();
    let elapsed = start.elapsed();
    assert_eq!(created.len(), 10_000);
    assert!(
        elapsed < Duration::from_secs(1),
        "bulk create took {elapsed:?}"
    );
}
