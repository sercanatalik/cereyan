use std::time::{Duration, Instant};

use cereyan_core::{new_id, State, StateType};
use cereyan_store::{CreateRun, ListRunsFilter, NewLog, ReportEvent, Store, StoreError};
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
fn one_fire_of_a_schedule_holds_one_run() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "etl", "daily");
    let make = |schedule_id: Option<i64>, at: Option<i64>| {
        store.create_run_full(CreateRun {
            flow_id: f,
            name: "r".into(),
            parameters: "{}".into(),
            tags: "[]".into(),
            created_by: "schedule".into(),
            schedule_id,
            scheduled_time: at,
            ..Default::default()
        })
    };
    assert!(make(Some(7), Some(1_000)).is_ok());
    assert!(
        make(Some(7), Some(1_000)).is_err(),
        "a second run for one fire is refused"
    );
    assert!(make(Some(7), Some(2_000)).is_ok(), "another fire is fine");
    assert!(
        make(Some(8), Some(1_000)).is_ok(),
        "another schedule is fine"
    );
    // Runs that are not a schedule's fires may share a moment.
    assert!(make(None, None).is_ok());
    assert!(make(None, None).is_ok());
}

#[test]
fn duplicate_fires_are_unlinked_by_the_migration() {
    let dir = TempDir::new().unwrap();
    {
        // A store as an earlier release left it: schema 10, two runs for one fire.
        let conn = rusqlite::Connection::open(dir.path().join("db.sqlite")).unwrap();
        for sql in [
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
        ] {
            conn.execute_batch(sql).unwrap();
        }
        conn.execute_batch(
            "INSERT INTO flow (id, external_id, project, name, module, source_dir, created_at, last_seen_at)
                 VALUES (1, randomblob(16), 'etl', 'daily', 'pipeline', '.', 0, 0);
             INSERT INTO run (id, external_id, flow_id, name, created_at, schedule_id, scheduled_time)
                 VALUES (1, randomblob(16), 1, 'first', 0, 7, 1000),
                        (2, randomblob(16), 1, 'second', 0, 7, 1000);
             PRAGMA user_version = 10;",
        )
        .unwrap();
    }

    let store = open(&dir);
    let count = |sql: &str| -> i64 {
        store
            .with_reader(|c| Ok(c.query_row(sql, [], |r| r.get(0))?))
            .unwrap()
    };
    // Both runs happened, so both stay; only the earlier one still claims the fire.
    assert_eq!(count("SELECT COUNT(*) FROM run"), 2);
    assert_eq!(
        count("SELECT COUNT(*) FROM run WHERE schedule_id IS NOT NULL"),
        1
    );
    assert_eq!(count("SELECT id FROM run WHERE schedule_id IS NOT NULL"), 1);
}

#[test]
fn one_rejected_event_does_not_discard_its_report() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "etl", "daily");
    let (run, _) = store.create_run(f, "one", "{}", "[]").unwrap();
    let first = new_id();
    let created = |seq: i64, external_id| ReportEvent::TaskRunCreated {
        seq,
        external_id,
        name: "load".into(),
        task_key: "pipeline.load".into(),
        dynamic_key: "load-0".into(),
        parents: Vec::new(),
        pass: 0,
    };
    // Two creations claiming one (run, pass, dynamic key). The second is
    // refused, and the event after it still applies: a whole report used to be
    // discarded for one bad row, which is how an execution went missing.
    let events = vec![
        created(1, first),
        created(2, new_id()),
        ReportEvent::TaskRunTransition {
            seq: 3,
            external_id: first,
            state: State::new(StateType::Pending),
            force: false,
        },
    ];
    let outcome = store.apply_report(run, events.clone()).unwrap();
    assert_eq!(outcome.rejected.len(), 1);
    assert_eq!(outcome.rejected[0].seq, 2);
    assert_eq!(outcome.rejected[0].kind, "task_run_created");
    let tasks = store.task_runs_by_run(run, None).unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].state.state_type, StateType::Pending);

    // Redelivery of the same batch changes nothing.
    let again = store.apply_report(run, events).unwrap();
    assert_eq!(again.applied, 0);
    assert_eq!(again.skipped, 3);
    assert!(again.rejected.is_empty());
    assert_eq!(store.task_runs_by_run(run, None).unwrap().len(), 1);
}

#[test]
fn task_run_pass_migration_keeps_history() {
    let dir = TempDir::new().unwrap();
    {
        // A store as an earlier release wrote it: schema 9, no `pass` column.
        let conn = rusqlite::Connection::open(dir.path().join("db.sqlite")).unwrap();
        for sql in [
            include_str!("../migrations/0001_init.sql"),
            include_str!("../migrations/0002_server.sql"),
            include_str!("../migrations/0003_scheduling.sql"),
            include_str!("../migrations/0004_observability.sql"),
            include_str!("../migrations/0005_counts_index.sql"),
            include_str!("../migrations/0006_expectations.sql"),
            include_str!("../migrations/0007_flow_group.sql"),
            include_str!("../migrations/0008_schedule_skips.sql"),
            include_str!("../migrations/0009_event_flow_index.sql"),
        ] {
            conn.execute_batch(sql).unwrap();
        }
        conn.execute_batch(
            "INSERT INTO flow (id, external_id, project, name, module, source_dir, created_at, last_seen_at)
                 VALUES (1, randomblob(16), 'etl', 'daily', 'pipeline', '.', 0, 0);
             INSERT INTO run (id, external_id, flow_id, name, created_at)
                 VALUES (1, randomblob(16), 1, 'one', 0);
             INSERT INTO task_run (id, external_id, run_id, name, task_key, dynamic_key, created_at)
                 VALUES (7, randomblob(16), 1, 'load', 'pipeline.load', 'load-0', 0);
             INSERT INTO task_run_state (task_run_id, type, name, timestamp)
                 VALUES (7, 'Completed', 'Completed', 0);
             PRAGMA user_version = 9;",
        )
        .unwrap();
    }

    let store = open(&dir);
    let tasks = store.task_runs_by_run(1, None).unwrap();
    assert_eq!(tasks.len(), 1);
    // Ids carry over: logs, artifacts and recorded states all point at them.
    assert_eq!(tasks[0].id, 7);
    assert_eq!(tasks[0].dynamic_key, "load-0");
    assert_eq!(tasks[0].pass, 0);
    // Dropping the old table with foreign keys enforced would have cascaded
    // into task_run_state and taken every recorded state with it.
    let states: i64 = store
        .with_reader(|c| {
            Ok(c.query_row(
                "SELECT COUNT(*) FROM task_run_state WHERE task_run_id = 7",
                [],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(states, 1);

    // The same call in a later pass is a second task run, not a conflict.
    store
        .create_task_run(1, "load", "pipeline.load", "load-0", 1)
        .unwrap();
    assert_eq!(store.task_runs_by_run(1, Some(1)).unwrap().len(), 1);
    assert_eq!(store.task_runs_by_run(1, None).unwrap().len(), 2);
    // Twice in one pass is still refused.
    assert!(store
        .create_task_run(1, "load", "pipeline.load", "load-0", 1)
        .is_err());
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
        .create_task_run(run, "load", "pipeline.load", "load-0", 0)
        .unwrap();
    let (t1, _) = store
        .create_task_run(run, "load", "pipeline.load", "load-1", 0)
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
    let tasks = store.task_runs_by_run(run, None).unwrap();
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
    // The group filter is a second predicate on the same joined flow row, so it
    // holds the project filter's budget and needs no index of its own.
    let filter = ListRunsFilter {
        group: Some("p".into()),
        limit: Some(50),
        ..Default::default()
    };
    let start = Instant::now();
    let page = store.list_runs(&filter).unwrap();
    let elapsed = start.elapsed();
    assert_eq!(page.items.len(), 50);
    assert!(
        elapsed < Duration::from_millis(10),
        "group runs query took {elapsed:?} at {total} runs"
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

#[test]
#[cfg(unix)]
fn home_is_owner_only_and_covers_what_is_in_it() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new().unwrap();
    let home = dir.path().join("fresh");
    let store = Store::open(&home).unwrap();
    let mode = std::fs::metadata(&home).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        mode & 0o077,
        0,
        "another account can reach the home: {mode:04o}"
    );
    // The store and the key are covered by the directory, whatever their own modes are.
    assert!(home.join("db.sqlite").exists());
    drop(store);
}

#[test]
#[cfg(unix)]
fn a_home_from_an_earlier_version_is_narrowed_on_open() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new().unwrap();
    let home = dir.path().join("old");
    // What every 1.4.0 install has: created before this requirement existed.
    std::fs::create_dir_all(&home).unwrap();
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o755)).unwrap();
    let store = Store::open(&home).unwrap();
    let mode = std::fs::metadata(&home).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        mode & 0o077,
        0,
        "an existing broad home was left broad: {mode:04o}"
    );
    drop(store);
}

fn grouped_flow(store: &Store, project: &str, name: &str, group: Option<&str>) -> i64 {
    store
        .upsert_flow_full(cereyan_store::UpsertFlow {
            project: project.into(),
            name: name.into(),
            module: "pipeline".into(),
            source_dir: "/tmp/proj".into(),
            description: None,
            tags: "[]".into(),
            parameter_schema: "{}".into(),
            options: "{}".into(),
            group: group.map(Into::into),
        })
        .unwrap()
}

#[test]
fn flow_group_round_trips_and_reaches_runs() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);

    // Declared and undeclared sit side by side; the undeclared one stays NULL.
    let declared = grouped_flow(&store, "warehouse", "load", Some("nightly"));
    let plain = grouped_flow(&store, "warehouse", "reconcile", None);
    let by_key = store.get_flow_by_key("warehouse", "load").unwrap().unwrap();
    assert_eq!(by_key.group.as_deref(), Some("nightly"));
    assert_eq!(by_key.group_or_project(), "nightly");
    let plain_row = store
        .get_flow_by_key("warehouse", "reconcile")
        .unwrap()
        .unwrap();
    assert_eq!(plain_row.group, None);
    assert_eq!(plain_row.group_or_project(), "warehouse");

    // Runs read the flow's group; an undeclared flow's runs read its project.
    let (run, _) = store.create_run(declared, "one", "{}", "[]").unwrap();
    let (other, _) = store.create_run(plain, "two", "{}", "[]").unwrap();
    assert_eq!(store.get_run(run).unwrap().unwrap().group, "nightly");
    assert_eq!(store.get_run(other).unwrap().unwrap().group, "warehouse");

    // Renaming the group moves the history: nothing was stored on the run.
    grouped_flow(&store, "warehouse", "load", Some("overnight"));
    assert_eq!(store.get_run(run).unwrap().unwrap().group, "overnight");
    let page = store.list_runs(&ListRunsFilter::default()).unwrap();
    assert!(page.items.iter().all(|r| r.group != "nightly"));

    // Clearing it falls back to the project again.
    grouped_flow(&store, "warehouse", "load", None);
    assert_eq!(store.get_run(run).unwrap().unwrap().group, "warehouse");
}

#[test]
fn flows_written_before_the_group_column_read_as_their_project() {
    let dir = TempDir::new().unwrap();
    {
        let store = open(&dir);
        grouped_flow(&store, "warehouse", "load", Some("nightly"));
        grouped_flow(&store, "analytics", "rollup", None);
    }
    // Wind the schema back to before 0007, as an older release left it.
    {
        let conn = rusqlite::Connection::open(dir.path().join("db.sqlite")).unwrap();
        conn.execute_batch(
            "DROP TABLE schedule_skip; ALTER TABLE flow DROP COLUMN flow_group; \
             PRAGMA user_version = 6;",
        )
        .unwrap();
    }
    let store = open(&dir);
    let flows = store.list_flows(None).unwrap();
    assert_eq!(flows.len(), 2);
    // The migration adds the column without a backfill, so every row reads as
    // its project until something declares a group again.
    for f in &flows {
        assert_eq!(f.group, None);
        assert_eq!(f.group_or_project(), f.project);
    }
}

#[test]
fn group_filter_matches_the_resolved_group() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);

    // Two projects sharing a declared group, with undeclared flows either side.
    let wh_nightly = grouped_flow(&store, "warehouse", "load", Some("nightly"));
    let an_nightly = grouped_flow(&store, "analytics", "rollup", Some("nightly"));
    let wh_plain = grouped_flow(&store, "warehouse", "reconcile", None);
    grouped_flow(&store, "analytics", "audit", None);

    // A declared group gathers its flows from every project that has one.
    let nightly = store.list_flows_filtered(None, Some("nightly")).unwrap();
    assert_eq!(nightly.len(), 2);
    assert!(nightly
        .iter()
        .all(|f| f.group.as_deref() == Some("nightly")));

    // A project name selects that project's flows that declared no group,
    // because the fallback is what the filter matches.
    let by_project = store.list_flows_filtered(None, Some("warehouse")).unwrap();
    assert_eq!(
        by_project
            .iter()
            .map(|f| f.name.as_str())
            .collect::<Vec<_>>(),
        ["reconcile"]
    );

    // Project and group together narrow to the one flow in both.
    let both = store
        .list_flows_filtered(Some("warehouse"), Some("nightly"))
        .unwrap();
    assert_eq!(both.len(), 1);
    assert_eq!(both[0].id, wh_nightly);

    // A group nothing resolves to is empty rather than an error.
    assert!(store
        .list_flows_filtered(None, Some("absent"))
        .unwrap()
        .is_empty());

    // The same rule reaches runs through the flow join.
    store.create_run(wh_nightly, "one", "{}", "[]").unwrap();
    store.create_run(an_nightly, "two", "{}", "[]").unwrap();
    store.create_run(wh_plain, "three", "{}", "[]").unwrap();
    let runs = store
        .list_runs(&ListRunsFilter {
            group: Some("nightly".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(runs.items.len(), 2);
    assert!(runs.items.iter().all(|r| r.group == "nightly"));
    let plain = store
        .list_runs(&ListRunsFilter {
            group: Some("warehouse".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(plain.items.len(), 1);
    assert_eq!(plain.items[0].name, "three");
}
