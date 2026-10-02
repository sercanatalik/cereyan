use std::time::{Duration, Instant};

use cereyan_core::{new_id, Id, State, StateType};
use cereyan_store::{CreateRun, ListRunsFilter, NewEvent, NewLog, ReportEvent, Store, StoreError};
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
    //
    // Indexes added by later migrations must be dropped explicitly. This is not
    // obvious: `task_run_run_state` did not need dropping, because migration
    // 0010 rebuilds `task_run` (new table, drop, rename) and a table rebuild
    // discards manually-added indexes — so 0018 replayed cleanly by accident.
    // `event` is never rebuilt, so its index survives and 0019 collided with it.
    // Migrations are not idempotent, so anything a later migration creates has
    // to be removed here by name.
    //
    // The rule for the next index migration: add the index to this batch AND to
    // the three narrower wind-backs above (`the_task_count_index_...`,
    // `the_event_run_index_...`, `the_run_parent_index_...`,
    // `the_run_schedule_index_...`), or `Store::open` fails with
    // "index <name> already exists" and five unrelated tests break at once. The
    // error names the index, so the fix is mechanical -- but it is four edits in
    // four places, which is why it is written down rather than left in a
    // reviewer's memory. `run_state_scheduled` (migration 0022) was the fifth.
    {
        let conn = rusqlite::Connection::open(dir.path().join("db.sqlite")).unwrap();
        conn.execute_batch(
            "DROP TABLE schedule_skip; ALTER TABLE flow DROP COLUMN flow_group; \
             ALTER TABLE schedule DROP COLUMN catchup_window; \
             ALTER TABLE schedule DROP COLUMN jitter; \
             ALTER TABLE schedule DROP COLUMN start_deadline; \
             ALTER TABLE run DROP COLUMN attributes; \
             ALTER TABLE task_run DROP COLUMN result_ref; \
             ALTER TABLE task_run DROP COLUMN input_hash; \
             DROP INDEX run_unique_key; ALTER TABLE run DROP COLUMN unique_key; \
             DROP TABLE task_state; \
             DROP TABLE worker_flow; DROP TABLE worker; DROP INDEX run_host_start; \
             ALTER TABLE run DROP COLUMN host; ALTER TABLE run DROP COLUMN processor; \
             ALTER TABLE run DROP COLUMN lease; ALTER TABLE run DROP COLUMN source_hash; \
             DROP INDEX task_run_run_state; DROP INDEX event_run_id; \
             DROP INDEX run_parent_run_id; DROP INDEX run_schedule_id; \
             DROP INDEX run_state_scheduled; \
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

#[test]
fn workers_register_claim_runs_and_are_forgotten() {
    use cereyan_store::WorkerRegistration;
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let reg = |processors: i64| WorkerRegistration {
        name: "gpu-1".into(),
        version: "3.0.0".into(),
        cpus: 16,
        processors,
        labels: serde_json::from_str(r#"{"gpu": "true"}"#).unwrap(),
        shared_paths: vec!["/mnt/lake".into()],
        meta: serde_json::from_str(r#"{"hostname": "gpu-box-1"}"#).unwrap(),
    };
    let w = store.register_worker(reg(2)).unwrap();
    assert_eq!(
        (w.name.as_str(), w.processors, w.state.as_str()),
        ("gpu-1", 2, "online")
    );
    // The same name again updates the record.
    let again = store.register_worker(reg(4)).unwrap();
    assert_eq!((again.id, again.processors), (w.id, 4));
    assert_eq!(store.list_workers().unwrap().len(), 1);

    let flow_id = flow(&store, "etl", "load");
    store
        .set_worker_flows(w.id, vec![(flow_id, "abc".into())])
        .unwrap();
    assert_eq!(
        store.all_worker_flows().unwrap(),
        vec![(w.id, flow_id, "abc".to_string())]
    );

    store.set_worker_state(w.id, "offline").unwrap();
    let mut meta = serde_json::Map::new();
    meta.insert("memory_free".into(), serde_json::json!(1024));
    store.touch_worker(w.id, meta).unwrap();
    let touched = store.get_worker(w.id).unwrap().unwrap();
    assert_eq!(touched.state, "online");
    assert_eq!(touched.meta["hostname"], "gpu-box-1");
    assert_eq!(touched.meta["memory_free"], 1024);

    let (run_id, _) = store.create_run(flow_id, "r", "{}", "[]").unwrap();
    assert_eq!(
        store
            .claim_run(run_id, "gpu-1", Some(2), Some("abc".into()))
            .unwrap(),
        1
    );
    assert_eq!(store.claim_run(run_id, "server", Some(1), None).unwrap(), 2);
    let run = store.get_run(run_id).unwrap().unwrap();
    assert_eq!(
        (run.host.as_deref(), run.processor, run.lease),
        (Some("server"), Some(1), 2)
    );
    assert_eq!(run.source_hash.as_deref(), Some("abc"));

    assert!(store.delete_worker(w.id).unwrap());
    assert!(store.all_worker_flows().unwrap().is_empty());
}

#[test]
fn runs_by_flow_on_host_counts_since_a_start_time() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let load = flow(&store, "etl", "load");
    let train = flow(&store, "ml", "train");
    let run_to = |flow_id: i64, host: &str, end: Option<StateType>| {
        let (run, _) = store.create_run(flow_id, "r", "{}", "[]").unwrap();
        store.claim_run(run, host, Some(1), None).unwrap();
        store
            .transition_run(run, State::new(StateType::Pending), false)
            .unwrap();
        store
            .transition_run(run, State::new(StateType::Running), false)
            .unwrap();
        if let Some(t) = end {
            store.transition_run(run, State::new(t), false).unwrap();
        }
    };
    run_to(load, "gpu-1", Some(StateType::Completed));
    let since = cereyan_core::now_micros();
    run_to(load, "gpu-1", Some(StateType::Completed));
    run_to(load, "gpu-1", Some(StateType::Completed));
    run_to(load, "gpu-1", Some(StateType::Failed));
    run_to(train, "gpu-1", None);
    run_to(load, "server", Some(StateType::Completed));

    let got = store.runs_by_flow_on_host("gpu-1", since, 200).unwrap();
    assert_eq!(got.len(), 2);
    assert_eq!(
        (
            got[0].flow.as_str(),
            got[0].completed,
            got[0].failed,
            got[0].running
        ),
        ("load", 2, 1, 0)
    );
    assert!(got[0].last_completed_at.is_some());
    assert_eq!(
        (got[1].flow.as_str(), got[1].completed, got[1].running),
        ("train", 0, 1)
    );
    assert_eq!(got[1].last_completed_at, None);
    assert!(store
        .runs_by_flow_on_host("gpu-2", 0, 200)
        .unwrap()
        .is_empty());
}

// ---- append_event returns the stored row -------------------------------------
//
// `append_event` assembles the `Event` from the values it just inserted instead
// of reading it back. These tests pin that struct to the row the store actually
// holds, so the two can never drift apart (a swapped column, a forgotten `seq`).

fn new_event(name: &str, run_id: Option<i64>, payload: serde_json::Value) -> NewEvent {
    NewEvent {
        name: name.to_string(),
        run_id,
        flow_id: None,
        payload,
        resource: cereyan_core::Resource {
            kind: "run".into(),
            id: "run-1".into(),
            name: "the run".into(),
        },
        related: vec![cereyan_core::Resource {
            kind: "flow".into(),
            id: "flow-1".into(),
            name: "etl".into(),
        }],
    }
}

#[test]
fn append_event_returns_the_row_that_was_stored() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let _flow_id = flow(&store, "p", "etl");

    let (returned, external_id) = store
        .append_event(new_event(
            "run.completed",
            None,
            serde_json::json!({"a": 1, "b": "two"}),
        ))
        .unwrap();

    let stored = store.get_event(returned.id).unwrap().expect("row exists");
    assert_eq!(returned, stored, "returned event differs from the stored row");
    assert_eq!(returned.external_id, external_id);
    assert_eq!(returned.seq, returned.id, "seq is the row id");
    assert_eq!(stored.payload.get("a").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(stored.related.len(), 1);
    assert_eq!(stored.related[0].kind, "flow");
    // flow_id stays unset when neither the event nor its run names one.
    assert_eq!(stored.flow_id, None);
    assert_eq!(stored.run_id, None);
}

#[test]
fn append_event_derives_flow_id_from_the_run() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let flow_id = flow(&store, "p", "etl");
    let (run_id, _) = store
        .create_run_full(CreateRun {
            flow_id: flow_id,
            name: "r1".into(),
            parameters: "{}".into(),
            tags: "[]".into(),
            created_by: "test".into(),
            ..Default::default()
        })
        .unwrap();

    let (returned, _) = store
        .append_event(new_event("task_run.completed", Some(run_id), serde_json::json!({})))
        .unwrap();
    let stored = store.get_event(returned.id).unwrap().expect("row exists");
    assert_eq!(returned, stored);
    assert_eq!(stored.flow_id, Some(flow_id), "flow derived from the run");
    assert_eq!(stored.run_id, Some(run_id));
}

#[test]
fn append_event_non_object_payload_matches_the_decoder() {
    // The row decoder parses the stored text back into a Map and falls back to
    // an empty map for a non-object. The returned struct must agree.
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    for payload in [
        serde_json::json!([1, 2, 3]),
        serde_json::json!("a string"),
        serde_json::json!(7),
        serde_json::json!(null),
    ] {
        let (returned, _) = store
            .append_event(new_event("custom", None, payload.clone()))
            .unwrap();
        let stored = store.get_event(returned.id).unwrap().expect("row exists");
        assert_eq!(
            returned, stored,
            "returned event differs from stored row for payload {payload}"
        );
    }
}

#[test]
fn append_events_returns_every_stored_row_in_order() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let names = ["first", "second", "third", "fourth"];
    let events: Vec<NewEvent> = names
        .iter()
        .map(|n| new_event(n, None, serde_json::json!({"n": n})))
        .collect();

    let appended = store.append_events(events).unwrap();
    assert_eq!(appended.len(), names.len());
    for (i, (returned, _)) in appended.iter().enumerate() {
        let stored = store.get_event(returned.id).unwrap().expect("row exists");
        assert_eq!(*returned, stored, "batch entry {i} differs from the row");
        assert_eq!(stored.name, names[i], "order preserved");
    }
    // Ids are assigned in commit order, so sequences increase across the batch.
    for pair in appended.windows(2) {
        assert!(pair[0].0.id < pair[1].0.id, "ids increase across the batch");
    }
}

#[test]
fn transition_run_returns_the_state_that_was_persisted() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let flow_id = flow(&store, "p", "etl");
    let (run_id, _) = store
        .create_run_full(CreateRun {
            flow_id,
            name: "r1".into(),
            parameters: "{}".into(),
            tags: "[]".into(),
            created_by: "test".into(),
            ..Default::default()
        })
        .unwrap();

    // The state machine requires a Pending step before Running.
    let pending = store
        .transition_run(run_id, State::new(StateType::Pending), false)
        .unwrap();
    assert_eq!(
        pending,
        store.get_run(run_id).unwrap().unwrap().state,
        "Pending return differs from the stored row"
    );

    let returned = store
        .transition_run(run_id, State::new(StateType::Running), false)
        .unwrap();
    // The state the write reports must equal the state the row now holds.
    let row = store.get_run(run_id).unwrap().expect("run exists");
    assert_eq!(returned, row.state, "write returned a different state than stored");
    assert_eq!(returned.state_type, StateType::Running);
    assert_eq!(returned.name, "Running");
}

// ---- worker flow membership is read per worker -------------------------------

fn worker(store: &Store, name: &str) -> i64 {
    use cereyan_store::WorkerRegistration;
    store
        .register_worker(WorkerRegistration {
            name: name.into(),
            version: "3.0.0".into(),
            cpus: 8,
            processors: 2,
            labels: serde_json::from_str("{}").unwrap(),
            shared_paths: vec![],
            meta: serde_json::from_str("{}").unwrap(),
        })
        .unwrap()
        .id
}

#[test]
fn worker_flow_details_returns_only_that_workers_rows() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f1 = flow(&store, "p", "etl");
    let f2 = flow(&store, "p", "billing");
    let f3 = flow(&store, "q", "other");

    let w1 = worker(&store, "gpu-1");
    let w2 = worker(&store, "gpu-2");
    store
        .set_worker_flows(w1, vec![(f1, "h1".into()), (f2, "h2".into())])
        .unwrap();
    store
        .set_worker_flows(w2, vec![(f3, "h3".into())])
        .unwrap();

    let mine = store.worker_flow_details(w1).unwrap();
    assert_eq!(mine.len(), 2, "only this worker's rows");
    let mut ids: Vec<i64> = mine.iter().map(|(id, _, _, _)| *id).collect();
    ids.sort();
    assert_eq!(ids, vec![f1, f2]);

    // The joined flow columns must be the flow's own.
    for (id, source_dir, module, hash) in &mine {
        let f = store.get_flow(*id).unwrap().expect("flow exists");
        assert_eq!(source_dir, &f.source_dir);
        assert_eq!(module, &f.module);
        assert!(hash == "h1" || hash == "h2", "hash is the reported one");
    }

    // The other worker sees only its own.
    let theirs = store.worker_flow_details(w2).unwrap();
    assert_eq!(theirs.len(), 1);
    assert_eq!(theirs[0].0, f3);
    assert_eq!(theirs[0].3, "h3");
}

#[test]
fn worker_flow_details_skips_a_claim_on_a_deleted_flow() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f1 = flow(&store, "p", "etl");
    let f2 = flow(&store, "p", "gone");
    let w = worker(&store, "gpu-1");
    store
        .set_worker_flows(w, vec![(f1, "h1".into()), (f2, "h2".into())])
        .unwrap();
    assert_eq!(store.worker_flow_details(w).unwrap().len(), 2);

    // Deleting the flow drops it from the join, as the old lookup-by-id did.
    store.delete_flow(f2).unwrap();
    let rows = store.worker_flow_details(w).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, f1);
}

#[test]
fn worker_flow_counts_groups_by_worker() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f1 = flow(&store, "p", "a");
    let f2 = flow(&store, "p", "b");
    let f3 = flow(&store, "p", "c");
    let w1 = worker(&store, "gpu-1");
    let w2 = worker(&store, "gpu-2");
    let w3 = worker(&store, "idle"); // claims nothing

    store
        .set_worker_flows(w1, vec![(f1, "h".into()), (f2, "h".into())])
        .unwrap();
    store
        .set_worker_flows(w2, vec![(f3, "h".into())])
        .unwrap();

    let counts: std::collections::HashMap<i64, i64> =
        store.worker_flow_counts().unwrap().into_iter().collect();
    assert_eq!(counts.get(&w1).copied(), Some(2));
    assert_eq!(counts.get(&w2).copied(), Some(1));
    assert!(
        !counts.contains_key(&w3),
        "a worker with no claims has no group, so the caller must default to 0"
    );

    // Replacing a worker's flows replaces its count, not adds to it.
    store.set_worker_flows(w1, vec![(f1, "h".into())]).unwrap();
    let counts: std::collections::HashMap<i64, i64> =
        store.worker_flow_counts().unwrap().into_iter().collect();
    assert_eq!(counts.get(&w1).copied(), Some(1));
}

// ---- single-row getters go through the statement cache ----------------------
//
// These getters moved from `query_row` (prepare/step/finalize, recompiling the
// SQL every call) to `prepare_cached`. The risk in that change is plumbing, not
// logic, so these tests pin the values: each getter must return exactly what the
// uncached query returned, for a row that exists and one that does not.

#[test]
fn single_row_getters_return_the_row_and_none_for_a_missing_id() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);

    let f = flow(&store, "p", "etl");
    let (run_id, run_ext) = store
        .create_run_full(CreateRun {
            flow_id: f,
            name: "r1".into(),
            parameters: "{}".into(),
            tags: "[]".into(),
            created_by: "test".into(),
            ..Default::default()
        })
        .unwrap();

    // Present and absent, for every getter this change touched.
    assert!(store.get_run(run_id).unwrap().is_some(), "get_run");
    assert!(store.get_run(run_id + 9_999).unwrap().is_none(), "get_run missing");
    assert_eq!(
        store.get_run_by_external_id(&run_ext).unwrap().map(|r| r.id),
        Some(run_id),
        "get_run_by_external_id"
    );
    assert!(
        store.get_run_by_external_id(&new_id()).unwrap().is_none(),
        "get_run_by_external_id missing"
    );

    assert!(store.get_flow(f).unwrap().is_some(), "get_flow");
    assert!(store.get_flow(f + 9_999).unwrap().is_none(), "get_flow missing");
    let by_key = store.get_flow_by_key("p", "etl").unwrap();
    assert_eq!(by_key.as_ref().map(|x| x.id), Some(f), "get_flow_by_key");
    assert!(
        store.get_flow_by_key("p", "absent").unwrap().is_none(),
        "get_flow_by_key missing"
    );
    assert!(
        store.get_flow_by_key("other", "etl").unwrap().is_none(),
        "get_flow_by_key wrong project"
    );

    assert!(store.get_task_run(9_999).unwrap().is_none(), "get_task_run missing");
    assert!(store.get_schedule(9_999).unwrap().is_none(), "get_schedule missing");
    assert!(store.get_backfill(9_999).unwrap().is_none(), "get_backfill missing");
    assert!(store.get_event(9_999).unwrap().is_none(), "get_event missing");
    assert!(store.get_rule(9_999).unwrap().is_none(), "get_rule missing");
    assert!(store.get_expectation(9_999).unwrap().is_none(), "get_expectation missing");
    assert!(store.get_artifact(9_999).unwrap().is_none(), "get_artifact missing");
    assert!(store.get_variable("absent").unwrap().is_none(), "get_variable missing");
    assert!(store.get_variable("absent").unwrap().is_none(), "get_variable repeated");
}

#[test]
fn get_run_returns_the_same_row_every_time() {
    // Repeated reads must be stable, and must agree with the batch reader for
    // the same id, which was already on the statement cache.
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let (run_id, _) = store
        .create_run_full(CreateRun {
            flow_id: f,
            name: "r1".into(),
            parameters: r#"{"day":"2026-09-29"}"#.into(),
            tags: r#"["a"]"#.into(),
            created_by: "test".into(),
            ..Default::default()
        })
        .unwrap();

    let first = store.get_run(run_id).unwrap().expect("row exists");
    for _ in 0..5 {
        assert_eq!(store.get_run(run_id).unwrap(), Some(first.clone()));
    }
    let batch = store.get_runs(&[run_id]).unwrap();
    assert_eq!(batch.len(), 1);
    assert_eq!(batch[0].id, first.id);
    assert_eq!(batch[0].parameters, first.parameters);
    assert_eq!(batch[0].tags, first.tags);
    assert_eq!(batch[0].task_counts, first.task_counts);
}

#[test]
fn latest_run_with_param_still_matches_through_the_cache() {
    // Its statement embeds a JSON path, so it genuinely varies; the test covers
    // that the cached path still selects the right run and honours the key.
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let (with, _) = store
        .create_run_full(CreateRun {
            flow_id: f,
            name: "a".into(),
            parameters: r#"{"day":"2026-01-01"}"#.into(),
            tags: "[]".into(),
            created_by: "test".into(),
            ..Default::default()
        })
        .unwrap();
    let (without, _) = store
        .create_run_full(CreateRun {
            flow_id: f,
            name: "b".into(),
            parameters: r#"{"other":"x"}"#.into(),
            tags: "[]".into(),
            created_by: "test".into(),
            ..Default::default()
        })
        .unwrap();

    assert_eq!(
        store.latest_run_with_param(f, "day", "2026-01-01").unwrap().map(|r| r.id),
        Some(with)
    );
    assert!(store.latest_run_with_param(f, "day", "1999-01-01").unwrap().is_none());
    // A key no run carries must not match the run that lacks it.
    assert!(store.latest_run_with_param(f, "absent", "x").unwrap().is_none());
    let _ = without;
}

// ---- checkpoints are built from one chain walk ------------------------------
//
// `checkpoints` used to evaluate the crash-chain CTE twice and read one `kv` row
// per chain element. It now walks the chain once and reads the seeds in a single
// query. The precedence rules are what matter, so these tests pin them.


fn run_of(store: &Store, flow_id: i64, name: &str) -> (i64, Id) {
    store
        .create_run_full(CreateRun {
            flow_id,
            name: name.into(),
            created_by: "test".into(),
            ..Default::default()
        })
        .unwrap()
}

/// A retry run whose parent is a crashed run, so it forms a chain.
fn crash_retry_of(store: &Store, flow_id: i64, name: &str, parent: i64) -> (i64, Id) {
    store
        .create_run_full(CreateRun {
            flow_id,
            name: name.into(),
            created_by: "crash:3".into(),
            parent_run_id: Some(parent),
            attempt: 1,
            ..Default::default()
        })
        .unwrap()
}

/// A completed task run that stored a replayable result.
fn completed_with_result(
    store: &Store,
    run_id: i64,
    dynamic_key: &str,
    pass: i64,
    result_ref: &str,
) {
    let (id, _) = store
        .create_task_run(run_id, dynamic_key, "t", dynamic_key, pass)
        .unwrap();
    let state = State::new(StateType::Completed);
    let mut state = state;
    state.details.insert(
        "checkpoint".into(),
        serde_json::json!({"result_ref": result_ref, "input_hash": format!("h-{result_ref}")}),
    );
    store.transition_task_run(id, state, true).unwrap();
}

fn seed_of(store: &Store, run_id: i64, entries: &[(&str, &str)]) {
    let json = serde_json::to_string(
        &entries
            .iter()
            .map(|(k, r)| serde_json::json!({
                "dynamic_key": k,
                "task_key": "t",
                "input_hash": format!("h-{r}"),
                "result_ref": r,
                "run_id": run_id,
                "pass": 0,
            }))
            .collect::<Vec<_>>(),
    )
    .unwrap();
    store
        .kv_set(&cereyan_store::checkpoint_seed_key(run_id), &json)
        .unwrap();
}

fn result_refs(cps: &[cereyan_store::Checkpoint]) -> Vec<(String, String)> {
    cps.iter()
        .map(|c| (c.dynamic_key.clone(), c.result_ref.clone()))
        .collect()
}

#[test]
fn checkpoints_come_from_completed_task_runs() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let (run, _) = run_of(&store, f, "r1");
    completed_with_result(&store, run, "orders", 0, "art/1");

    let cps = store.checkpoints(run).unwrap();
    assert_eq!(result_refs(&cps), vec![("orders".to_string(), "art/1".to_string())]);
    assert_eq!(cps[0].run_id, run);
    assert_eq!(cps[0].input_hash, "h-art/1");
}

#[test]
fn a_run_with_no_results_has_no_checkpoints() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let (run, _) = run_of(&store, f, "r1");
    assert!(store.checkpoints(run).unwrap().is_empty());
}

#[test]
fn an_unrelated_run_contributes_nothing() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let (a, _) = run_of(&store, f, "a");
    let (b, _) = run_of(&store, f, "b");
    completed_with_result(&store, a, "orders", 0, "art/1");
    assert!(
        store.checkpoints(b).unwrap().is_empty(),
        "another run's work leaked in"
    );
}

#[test]
fn a_crash_retry_inherits_its_ancestors_checkpoints() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let (parent, _) = run_of(&store, f, "parent");
    let (child, _) = crash_retry_of(&store, f, "child", parent);
    let (grand, _) = crash_retry_of(&store, f, "grand", child);

    completed_with_result(&store, parent, "orders", 0, "art/1");
    completed_with_result(&store, child, "billing", 0, "art/2");

    // The deepest run sees the whole chain, ordered by dynamic key.
    let cps = store.checkpoints(grand).unwrap();
    assert_eq!(
        result_refs(&cps),
        vec![
            ("billing".to_string(), "art/2".to_string()),
            ("orders".to_string(), "art/1".to_string()),
        ],
        "ordered by dynamic key"
    );
}

#[test]
fn a_later_pass_wins_over_an_earlier_one() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let (run, _) = run_of(&store, f, "r1");
    completed_with_result(&store, run, "orders", 0, "art/old");
    completed_with_result(&store, run, "orders", 1, "art/new");

    let cps = store.checkpoints(run).unwrap();
    assert_eq!(result_refs(&cps), vec![("orders".into(), "art/new".into())]);
    assert_eq!(cps[0].pass, 1);
}

#[test]
fn a_runs_own_work_wins_over_its_own_seed() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let (run, _) = run_of(&store, f, "r1");
    seed_of(&store, run, &[("orders", "art/from-seed")]);
    completed_with_result(&store, run, "orders", 0, "art/own");

    let cps = store.checkpoints(run).unwrap();
    assert_eq!(
        result_refs(&cps),
        vec![("orders".into(), "art/own".into())],
        "the run's own completed work must win over its seed"
    );
}

#[test]
fn the_newest_seed_in_the_chain_wins() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let (parent, _) = run_of(&store, f, "parent");
    let (child, _) = crash_retry_of(&store, f, "child", parent);
    // Only seeds, no completed work, so seed precedence is what decides.
    seed_of(&store, parent, &[("orders", "art/parent-seed")]);
    seed_of(&store, child, &[("orders", "art/child-seed")]);

    let cps = store.checkpoints(child).unwrap();
    assert_eq!(
        result_refs(&cps),
        vec![("orders".into(), "art/child-seed".into())],
        "the newer seed in the chain must win"
    );
}

#[test]
fn a_run_inherits_its_ancestors_seed() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let (parent, _) = run_of(&store, f, "parent");
    let (child, _) = crash_retry_of(&store, f, "child", parent);
    seed_of(&store, parent, &[("orders", "art/parent-seed")]);

    let cps = store.checkpoints(child).unwrap();
    assert_eq!(result_refs(&cps), vec![("orders".into(), "art/parent-seed".into())]);
}

#[test]
fn an_incomplete_result_is_skipped() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let (run, _) = run_of(&store, f, "r1");
    // A checkpoint detail missing `input_hash` must not be picked up.
    let (id, _) = store.create_task_run(run, "orders", "t", "orders", 0).unwrap();
    let mut state = State::new(StateType::Completed);
    state.details.insert(
        "checkpoint".into(),
        serde_json::json!({"result_ref": "art/1"}),
    );
    store.transition_task_run(id, state, true).unwrap();

    assert!(
        store.checkpoints(run).unwrap().is_empty(),
        "a result without an input hash is not replayable and must be skipped"
    );
}

#[test]
fn checkpoints_come_back_ordered_by_dynamic_key() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let (run, _) = run_of(&store, f, "r1");
    for k in ["zulu", "alpha", "mike"] {
        completed_with_result(&store, run, k, 0, k);
    }
    let keys: Vec<String> = store
        .checkpoints(run)
        .unwrap()
        .into_iter()
        .map(|c| c.dynamic_key)
        .collect();
    assert_eq!(keys, vec!["alpha", "mike", "zulu"]);
}

// ---- backfill queries are batched -------------------------------------------

/// Create a run of a flow for one backfill value.
fn backfill_run(store: &Store, flow_id: i64, name: &str, backfill_id: i64, day: &str) -> i64 {
    store
        .create_run_full(CreateRun {
            flow_id,
            name: name.into(),
            parameters: format!(r#"{{"day":"{day}"}}"#),
            created_by: format!("backfill:{backfill_id}"),
            backfill_id: Some(backfill_id),
            ..Default::default()
        })
        .unwrap()
        .0
}

fn finish(store: &Store, run_id: i64, state: StateType) {
    store.transition_run(run_id, State::new(state), true).unwrap();
}

#[test]
fn a_value_whose_latest_run_completed_is_not_missing() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let r = backfill_run(&store, f, "a", 1, "2026-01-01");
    finish(&store, r, StateType::Completed);

    let done = store
        .latest_completed_param_values(f, "day", &["2026-01-01".to_string()])
        .unwrap();
    assert!(done.contains("2026-01-01"));
}

#[test]
fn a_value_whose_latest_run_failed_is_missing() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let r = backfill_run(&store, f, "a", 1, "2026-01-01");
    finish(&store, r, StateType::Failed);

    let done = store
        .latest_completed_param_values(f, "day", &["2026-01-01".to_string()])
        .unwrap();
    assert!(done.is_empty(), "a failed latest run must not count as done");
}

/// The rule that makes the window function necessary: an older `Completed` run
/// does NOT make a value present, because the latest run decides.
#[test]
fn an_older_completed_run_does_not_hide_a_newer_failure() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let first = backfill_run(&store, f, "a", 1, "2026-01-01");
    finish(&store, first, StateType::Completed);
    // A later attempt for the same value failed.
    let second = backfill_run(&store, f, "b", 1, "2026-01-01");
    finish(&store, second, StateType::Failed);
    assert!(second > first, "the second run must be the latest by id");

    let done = store
        .latest_completed_param_values(f, "day", &["2026-01-01".to_string()])
        .unwrap();
    assert!(
        done.is_empty(),
        "an older completed run must not mask the newer failure: the value \
         still needs rerunning"
    );
}

#[test]
fn a_value_that_never_ran_is_missing() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let done = store
        .latest_completed_param_values(f, "day", &["2026-09-09".to_string()])
        .unwrap();
    assert!(done.is_empty());
}

#[test]
fn only_the_requested_values_are_considered() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let r = backfill_run(&store, f, "a", 1, "2026-01-01");
    finish(&store, r, StateType::Completed);

    // The completed value was not asked about, so the asked-about one is missing.
    let done = store
        .latest_completed_param_values(f, "day", &["2026-02-02".to_string()])
        .unwrap();
    assert!(done.is_empty());
}

#[test]
fn another_flows_completed_run_does_not_count() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let a = flow(&store, "p", "etl");
    let b = flow(&store, "p", "other");
    let r = backfill_run(&store, b, "x", 1, "2026-01-01");
    finish(&store, r, StateType::Completed);

    let done = store
        .latest_completed_param_values(a, "day", &["2026-01-01".to_string()])
        .unwrap();
    assert!(
        done.is_empty(),
        "another flow's completed run made this value look done"
    );
}

#[test]
fn a_mixed_set_reports_only_the_done_values() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let ok = backfill_run(&store, f, "a", 1, "d1");
    finish(&store, ok, StateType::Completed);
    let bad = backfill_run(&store, f, "b", 1, "d2");
    finish(&store, bad, StateType::Failed);
    // d3 never ran.

    let asked = ["d1", "d2", "d3"].map(String::from).to_vec();
    let done = store
        .latest_completed_param_values(f, "day", &asked)
        .unwrap();
    assert_eq!(
        done.into_iter().collect::<std::collections::BTreeSet<_>>(),
        ["d1".to_string()].into_iter().collect::<std::collections::BTreeSet<_>>(),
    );
}

#[test]
fn an_empty_value_set_asks_nothing() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    assert!(store
        .latest_completed_param_values(f, "day", &[])
        .unwrap()
        .is_empty());
}

#[test]
fn bulk_counts_are_grouped_per_backfill() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let a1 = backfill_run(&store, f, "a1", 1, "d1");
    let a2 = backfill_run(&store, f, "a2", 1, "d2");
    let b1 = backfill_run(&store, f, "b1", 2, "d1");
    finish(&store, a1, StateType::Completed);
    finish(&store, a2, StateType::Failed);
    finish(&store, b1, StateType::Completed);

    let counts = store.backfill_counts_many(&[1, 2, 3]).unwrap();
    let one: std::collections::HashMap<String, i64> =
        counts.get(&1).unwrap().iter().cloned().collect();
    assert_eq!(one.get("Completed"), Some(&1));
    assert_eq!(one.get("Failed"), Some(&1));
    let two: std::collections::HashMap<String, i64> =
        counts.get(&2).unwrap().iter().cloned().collect();
    assert_eq!(two.get("Completed"), Some(&1));
    assert!(
        !counts.contains_key(&3),
        "a backfill with no runs must have no group, so callers default to empty"
    );
}

#[test]
fn bulk_counts_for_no_backfills_is_empty() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    assert!(store.backfill_counts_many(&[]).unwrap().is_empty());
}

// ---- task counts are served from a covering index ----------------------------
//
// `task_counts` is computed by a correlated subquery over `task_run` filtered
// on `run_id` and grouped on `state_type`. The UNIQUE(run_id, dynamic_key)
// autoindex can seek `run_id` but does not carry `state_type`, so the planner
// had to visit the table once per task run. `task_run(run_id, state_type)` makes
// the search covering.

/// The query plan lines for a run's task-count aggregate.
fn task_counts_plan(store: &Store) -> Vec<String> {
    store
        .with_reader(|c| {
            let sql = "SELECT json_group_object(st, n) FROM (
                          SELECT COALESCE(t.state_type,'Pending') AS st, COUNT(*) AS n
                          FROM task_run t WHERE t.run_id = ?1 GROUP BY st)";
            let mut stmt = c.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?;
            let rows = stmt
                .query_map([1i64], |r| r.get::<_, String>(3))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .unwrap()
}

#[test]
fn the_task_count_aggregate_uses_a_covering_index() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let plan = task_counts_plan(&store).join(" | ");
    assert!(
        plan.contains("COVERING INDEX"),
        "the aggregate is not served from the index alone: {plan}"
    );
    assert!(
        !plan.contains("SCAN task_run"),
        "the aggregate fell back to scanning task_run: {plan}"
    );
}

#[test]
fn the_task_count_index_exists_after_open() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let names: Vec<String> = store
        .with_reader(|c| {
            let mut stmt =
                c.prepare("SELECT name FROM sqlite_master WHERE type='index' AND tbl_name='task_run'")?;
            let rows = stmt
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .unwrap();
    assert!(
        names.iter().any(|n| n == "task_run_run_state"),
        "index missing, found: {names:?}"
    );
}

/// A database that was genuinely at the previous schema version must gain the
/// index on open. The store is opened fully, then rolled back to 17 and its
/// index dropped, which is the state a server upgrading from 3.0 would be in.
#[test]
fn the_task_count_index_is_added_to_an_older_database() {
    let dir = TempDir::new().unwrap();
    {
        // Close the store first, then roll the file back with a direct
        // connection: the store's own connections are read-only or owned by the
        // writer thread, so a reader cannot do this.
        drop(open(&dir));
        let conn = rusqlite::Connection::open(dir.path().join("db.sqlite")).unwrap();
        // Roll back to the version just before this migration, so only the
        // migration that adds the index replays. Rolling back further would
        // replay every later migration too, and migrations are not idempotent:
        // an earlier revision of this test used 17 and began failing as soon as
        // a second migration was added, which is what this comment exists to stop.
        // Drop both indexes this test is not about, so the replay of 0018 and
        // 0019 can create them again.
        conn.execute_batch(
            "DROP INDEX task_run_run_state; DROP INDEX event_run_id; \
             DROP INDEX run_parent_run_id; DROP INDEX run_schedule_id; \
             DROP INDEX run_state_scheduled; PRAGMA user_version = 17;",
        )
        .unwrap();
    }
    let store = open(&dir);
    assert_eq!(
        store
            .with_reader(|c| Ok(c.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))?))
            .unwrap(),
        cereyan_store::latest_schema_version(),
        "the upgrade did not advance the version"
    );
    let plan = task_counts_plan(&store).join(" | ");
    assert!(
        plan.contains("COVERING INDEX"),
        "an upgraded database did not get the covering index: {plan}"
    );
}

#[test]
fn task_counts_are_unchanged_by_the_index() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let (run, _) = run_of(&store, f, "r1");
    completed_with_result(&store, run, "orders", 0, "art/1");

    // Distinct states, so the map has more than one entry.
    let (t2, _) = store.create_task_run(run, "billing", "t", "billing", 0).unwrap();
    let (t3, _) = store.create_task_run(run, "shipping", "t", "shipping", 0).unwrap();
    store
        .transition_task_run(t2, State::new(StateType::Failed), true)
        .unwrap();
    store
        .transition_task_run(t3, State::new(StateType::Running), true)
        .unwrap();

    let counts = &store.get_run(run).unwrap().unwrap().task_counts;
    assert_eq!(counts.get("Completed"), Some(&1), "one completed task run");
    assert_eq!(counts.get("Failed"), Some(&1));
    assert_eq!(counts.get("Running"), Some(&1));

    // The list reader agrees with the single-row reader.
    let page = store
        .list_runs(&ListRunsFilter {
            limit: Some(10),
            ..Default::default()
        })
        .unwrap();
    let listed = page
        .items
        .iter()
        .find(|r| r.id == run)
        .expect("run is in the page");
    assert_eq!(&listed.task_counts, counts);
}

#[test]
fn a_run_with_no_task_runs_has_empty_counts() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let (run, _) = run_of(&store, f, "r1");
    assert!(store.get_run(run).unwrap().unwrap().task_counts.is_empty());
}

/// The new index must not disturb the uniqueness it sits alongside.
#[test]
fn the_unique_task_run_constraint_still_holds() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let (run, _) = run_of(&store, f, "r1");
    store.create_task_run(run, "orders", "t", "same-key", 0).unwrap();
    let dup = store.create_task_run(run, "orders", "t", "same-key", 0);
    assert!(
        dup.is_err(),
        "UNIQUE(run_id, dynamic_key) stopped being enforced"
    );
}


// ---- reading one pass of a run ------------------------------------------------
//
// `retry_inner` used to read every pass and keep only the last. It now resolves
// the last pass with `next_pass` and reads that pass alone. The risk is that
// those two disagree about which pass is last, so these tests pin the
// correspondence against real task-run data.

/// A run whose task runs occupy the given passes, one task per pass.
fn run_with_passes(store: &Store, flow_id: i64, name: &str, passes: &[i64]) -> i64 {
    let (run, _) = run_of(store, flow_id, name);
    for p in passes {
        store
            .create_task_run(run, &format!("t{p}"), "t", &format!("k{p}"), *p)
            .unwrap();
    }
    run
}

/// How a caller derives the latest pass from `next_pass`.
fn latest_pass_via_next_pass(store: &Store, run: i64) -> i64 {
    (store.next_pass(run).unwrap() - 1).max(0)
}

/// The expression `retry_inner` used before this change.
fn latest_pass_the_old_way(store: &Store, run: i64) -> i64 {
    let tasks = store.task_runs_by_run(run, None).unwrap();
    tasks.iter().map(|t| t.pass).max().unwrap_or(0)
}

#[test]
fn the_latest_pass_agrees_with_scanning_every_pass() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");

    // The cases the two expressions could plausibly disagree on.
    let cases: Vec<(&str, Vec<i64>)> = vec![
        ("no-task-runs", vec![]),
        ("pass-zero-only", vec![0]),
        ("three-passes", vec![0, 1, 2]),
        ("non-contiguous", vec![2]),
        ("high-pass-only", vec![7]),
        ("sparse", vec![0, 3, 4]),
    ];
    for (name, passes) in cases {
        let run = run_with_passes(&store, f, name, &passes);
        assert_eq!(
            latest_pass_via_next_pass(&store, run),
            latest_pass_the_old_way(&store, run),
            "disagreement for {name}"
        );
    }
}

#[test]
fn next_pass_is_one_past_the_highest_pass() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    assert_eq!(store.next_pass(run_with_passes(&store, f, "none", &[])).unwrap(), 0);
    assert_eq!(
        store.next_pass(run_with_passes(&store, f, "one", &[0])).unwrap(),
        1
    );
    assert_eq!(
        store.next_pass(run_with_passes(&store, f, "three", &[0, 1, 2])).unwrap(),
        3
    );
}

#[test]
fn reading_a_pass_returns_only_that_pass() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let run = run_with_passes(&store, f, "r", &[0, 1, 2]);

    // Three passes of one task each: reading everything shows the difference.
    let all = store.task_runs_by_run(run, None).unwrap();
    assert_eq!(all.len(), 3, "the unfiltered read should see every pass");

    let last = latest_pass_via_next_pass(&store, run);
    let only_last = store.task_runs_by_run(run, Some(last)).unwrap();
    assert_eq!(only_last.len(), 1, "the filtered read should see one pass");
    assert!(only_last.iter().all(|t| t.pass == last));

    // And it is exactly the last pass of the unfiltered read.
    let expected: Vec<_> = all.iter().filter(|t| t.pass == last).cloned().collect();
    assert_eq!(only_last, expected, "filtered read differs from the old filter");
}

#[test]
fn a_pass_with_no_task_runs_reads_nothing() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let run = run_with_passes(&store, f, "r", &[]);
    // This is the empty case: the clamp makes the requested pass 0, and there
    // are no task runs to return.
    assert_eq!(latest_pass_via_next_pass(&store, run), 0);
    assert!(store.task_runs_by_run(run, Some(0)).unwrap().is_empty());
}

#[test]
fn task_runs_of_another_run_are_not_included() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let a = run_with_passes(&store, f, "a", &[0, 1]);
    let b = run_with_passes(&store, f, "b", &[0, 1, 2, 3]);
    let rows = store.task_runs_by_run(a, Some(1)).unwrap();
    assert!(rows.iter().all(|t| t.run_id == a), "another run's task run leaked in");
    assert_eq!(rows.len(), 1);
    let _ = b;
}

// ---- schedule skips are read in one query ------------------------------------
//
// `upcoming_runs` used to read skip rows one schedule at a time. It now asks for
// all the schedules it is listing in a single read. The tests compare that read
// against the per-schedule one it replaces, so a change in either shows up.

use cereyan_store::ScheduleWrite;

fn schedule(store: &Store, flow_id: i64, spec: &str) -> i64 {
    store
        .upsert_schedule(ScheduleWrite {
            id: None,
            flow_id,
            spec: spec.into(),
            catchup: "none".into(),
            catchup_max: 0,
            catchup_window: None,
            jitter: 0,
            start_deadline: None,
            active: true,
            ..Default::default()
        })
        .unwrap()
}

#[test]
fn skips_are_grouped_per_schedule() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let a = schedule(&store, f, "daily");
    let b = schedule(&store, f, "hourly");
    let c = schedule(&store, f, "weekly"); // never skips

    store.add_skips(a, vec![300, 100, 200], "api").unwrap();
    store.add_skips(b, vec![50], "policy").unwrap();

    let got = store.list_skip_rows_many(&[a, b, c]).unwrap();
    assert_eq!(got.len(), 2, "a schedule with no skips must have no entry");
    assert!(!got.contains_key(&c));

    // Fire times ascending within each schedule.
    let fires_a: Vec<i64> = got[&a].iter().map(|(f, _, _)| *f).collect();
    assert_eq!(fires_a, vec![100, 200, 300], "fires out of order");
    assert!(got[&a].iter().all(|(_, _, by)| by == "api"));
    assert_eq!(got[&b][0].0, 50);
    assert_eq!(got[&b][0].2, "policy");
}

/// The batched read must return exactly what the per-schedule reads returned.
#[test]
fn the_batched_read_agrees_with_the_per_schedule_read() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let ids: Vec<i64> = ["daily", "hourly", "weekly", "monthly"]
        .iter()
        .map(|s| schedule(&store, f, s))
        .collect();
    store.add_skips(ids[0], vec![10, 20], "api").unwrap();
    store.add_skips(ids[2], vec![7, 8, 9], "policy").unwrap();
    store.add_skips(ids[3], vec![1], "catchup").unwrap();

    let batched = store.list_skip_rows_many(&ids).unwrap();
    for id in &ids {
        let one = store.list_skip_rows(*id).unwrap();
        let grouped = batched.get(id).cloned().unwrap_or_default();
        assert_eq!(grouped, one, "batched read differs for schedule {id}");
    }
    // And the key set matches: a schedule with no rows is absent, not empty.
    let expected: Vec<i64> = ids
        .iter()
        .copied()
        .filter(|id| !store.list_skip_rows(*id).unwrap().is_empty())
        .collect();
    let mut got_keys: Vec<i64> = batched.keys().copied().collect();
    let mut want_keys = expected.clone();
    got_keys.sort_unstable();
    want_keys.sort_unstable();
    assert_eq!(got_keys, want_keys);
}

#[test]
fn only_the_requested_schedules_are_read() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let wanted = schedule(&store, f, "daily");
    let other = schedule(&store, f, "hourly");
    store.add_skips(wanted, vec![1], "api").unwrap();
    store.add_skips(other, vec![2], "api").unwrap();

    let got = store.list_skip_rows_many(&[wanted]).unwrap();
    assert_eq!(got.len(), 1);
    assert!(got.contains_key(&wanted), "the requested schedule is missing");
    assert!(
        !got.contains_key(&other),
        "another schedule's skips leaked in"
    );
}

#[test]
fn an_empty_schedule_set_asks_nothing() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    assert!(store.list_skip_rows_many(&[]).unwrap().is_empty());
}

#[test]
fn repeated_fires_are_recorded_once() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let s = schedule(&store, f, "daily");
    assert_eq!(store.add_skips(s, vec![5], "api").unwrap(), 1);
    // The primary key is (schedule_id, fire_time), so a repeat is ignored.
    assert_eq!(store.add_skips(s, vec![5], "api").unwrap(), 0);
    let got = store.list_skip_rows_many(&[s]).unwrap();
    assert_eq!(got[&s].len(), 1, "a fire was recorded twice");
}

// ---- name existence stops at the first match --------------------------------
//
// `run_name_exists` used `SELECT COUNT(*) ... LIMIT 1`, which counts every row
// carrying the name — the `LIMIT` on an aggregate is applied after the aggregate
// is computed, so it does not stop the walk. The caller only ever saw a bool.

#[test]
fn a_name_in_use_is_reported_present() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let (run, _) = run_of(&store, f, "r1");
    assert_eq!(
        store
            .get_run(run)
            .unwrap()
            .expect("row exists")
            .name,
        "r1"
    );
    assert!(store.run_name_exists("r1").unwrap(), "an existing name was absent");
}

#[test]
fn an_unused_name_is_reported_absent() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    assert!(!store.run_name_exists("nope").unwrap());
}

/// The case the old `COUNT(*)` made slow: many runs sharing one name. The
/// answer must not depend on how many share it.
#[test]
fn a_name_shared_by_many_runs_answers_the_same_as_one() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");

    // The scheduler builds names with no schedule id, so several schedules of a
    // flow firing at the same second all produce this one name.
    let (one, _) = run_of(&store, f, "etl-20260929T120000");
    for i in 0..200 {
        store
            .create_run_full(CreateRun {
                flow_id: f,
                name: "etl-20260929T120000".into(),
                created_by: format!("schedule:{i}"),
                ..Default::default()
            })
            .unwrap();
    }
    let many = store
        .create_run_full(CreateRun {
            flow_id: f,
            name: "etl-20260929T120000".into(),
            created_by: "schedule:last".into(),
            ..Default::default()
        })
        .unwrap()
        .0;

    assert!(store.run_name_exists("etl-20260929T120000").unwrap());
    assert!(one < many, "expected 201 runs");
    // Same answer as a name with a single run, and same as one with none.
    let (solo, _) = run_of(&store, f, "etl-20260930T120000");
    assert!(solo > many);
    assert!(store.run_name_exists("etl-20260930T120000").unwrap());
    assert!(!store.run_name_exists("etl-20260931T120000").unwrap());
}

#[test]
fn the_empty_name_reports_whether_a_run_has_one() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    assert!(
        !store.run_name_exists("").unwrap(),
        "no run has an empty name yet"
    );
    let f = flow(&store, "p", "etl");
    run_of(&store, f, "");
    assert!(store.run_name_exists("").unwrap(), "a run now has an empty name");
}

#[test]
fn repeated_checks_are_stable() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    run_of(&store, f, "r1");
    for _ in 0..5 {
        assert!(store.run_name_exists("r1").unwrap());
        assert!(!store.run_name_exists("r2").unwrap());
    }
}

// ---- per-flow reads, batched -------------------------------------------------
//
// `GET /api/flows` used to call `recent_run_states` and a full `list_runs` per
// flow, and `Scheduler::for_flow` per flow. These tests pin the batched reads to
// the per-flow reads they replace, since a silent divergence here would show up
// as wrong dots and wrong health on the flows page.

#[test]
fn batched_recent_runs_match_the_per_flow_read() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let a = flow(&store, "p", "a");
    let b = flow(&store, "p", "b");
    let c = flow(&store, "p", "c"); // never runs

    // More than the cap for `a`, fewer for `b`.
    for i in 0..15 {
        run_of(&store, a, &format!("a{i}"));
    }
    run_of(&store, b, "b0");
    run_of(&store, b, "b1");

    for (id, limit) in [(a, 10usize), (b, 10), (c, 10)] {
        let want = store.recent_run_states(id, limit).unwrap();
        let got = store
            .recent_run_states_many(&[a, b, c], limit)
            .unwrap()
            .get(&id)
            .cloned()
            .unwrap_or_default();
        assert_eq!(got, want, "batched recent runs differ for flow {id}");
    }
}

#[test]
fn the_recent_run_cap_is_per_flow() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let a = flow(&store, "p", "a");
    let b = flow(&store, "p", "b");
    for i in 0..15 {
        run_of(&store, a, &format!("a{i}"));
        run_of(&store, b, &format!("b{i}"));
    }
    for cap in [1usize, 3, 10, 20] {
        let got = store.recent_run_states_many(&[a, b], cap).unwrap();
        assert_eq!(got[&a].len(), cap.min(15), "cap {cap} not applied to a");
        assert_eq!(got[&b].len(), cap.min(15), "cap {cap} not applied to b");
    }
}

#[test]
fn recent_runs_are_newest_first_per_flow() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let a = flow(&store, "p", "a");
    for i in 0..5 {
        run_of(&store, a, &format!("a{i}"));
    }
    let got = store.recent_run_states_many(&[a], 10).unwrap();
    let ids: Vec<i64> = got[&a].iter().map(|(id, _, _, _)| *id).collect();
    let mut desc = ids.clone();
    desc.sort_unstable_by(|x, y| y.cmp(x));
    assert_eq!(ids, desc, "not newest first");
}

#[test]
fn recent_runs_exclude_flows_that_were_not_asked_about() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let a = flow(&store, "p", "a");
    let b = flow(&store, "p", "b");
    run_of(&store, a, "a0");
    run_of(&store, b, "b0");
    let got = store.recent_run_states_many(&[a], 10).unwrap();
    assert!(got.contains_key(&a));
    assert!(!got.contains_key(&b), "an unrequested flow's runs leaked in");
}

#[test]
fn recent_runs_for_no_flows_asks_nothing() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    assert!(store.recent_run_states_many(&[], 10).unwrap().is_empty());
}

#[test]
fn last_completed_at_matches_the_per_flow_query() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let a = flow(&store, "p", "a");
    let b = flow(&store, "p", "b");
    let c = flow(&store, "p", "c");

    // `a`: one completed run.
    let a1 = run_of(&store, a, "a1").0;
    finish(&store, a1, StateType::Completed);
    // `b`: several, the newest last, so the newest must win.
    for i in 0..3 {
        let r = run_of(&store, b, &format!("b{i}")).0;
        finish(&store, r, StateType::Completed);
    }
    // `c`: runs but none completed.

    let got = store.last_completed_at_many(&[a, b, c]).unwrap();

    // The oracle: the per-flow query the handler used to run.
    let oracle = |flow_id: i64| -> Option<i64> {
        store
            .list_runs(&ListRunsFilter {
                flow_id: Some(flow_id),
                state_type: Some("Completed".into()),
                limit: Some(1),
                sort: Some("created_desc".into()),
                ..Default::default()
            })
            .unwrap()
            .items
            .into_iter()
            .next()
            .and_then(|r| r.end_time)
    };
    for id in [a, b, c] {
        assert_eq!(
            got.get(&id).copied(),
            oracle(id),
            "last completion differs for flow {id}"
        );
    }
    assert!(!got.contains_key(&c), "a flow with no completed run has one");
}

#[test]
fn the_newest_completion_wins() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let a = flow(&store, "p", "a");
    let first = run_of(&store, a, "first").0;
    finish(&store, first, StateType::Completed);
    let first_at = store.get_run(first).unwrap().unwrap().end_time;
    let second = run_of(&store, a, "second").0;
    finish(&store, second, StateType::Completed);
    let second_at = store.get_run(second).unwrap().unwrap().end_time;
    assert!(second > first, "the second run must be newer");
    assert!(
        second_at.unwrap() >= first_at.unwrap(),
        "end times should not go backwards"
    );
    assert_eq!(
        store.last_completed_at_many(&[a]).unwrap().get(&a).copied(),
        second_at
    );
}

#[test]
fn last_completed_at_for_no_flows_asks_nothing() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    assert!(store.last_completed_at_many(&[]).unwrap().is_empty());
}

// ---- cancelling targets and flow-name lookup ---------------------------------
//
// The `cancel_runs` rule action used to deep-clone the whole active index and
// then read each surviving run in full, to read two columns. It now asks one
// query. The predicate must select exactly the same runs `StateType::is_terminal`
// excluded before.

/// A run left in the given state, returning its id.
fn run_in_state(store: &Store, flow_id: i64, name: &str, state: StateType) -> i64 {
    let (id, _) = run_of(store, flow_id, name);
    store.transition_run(id, State::new(state), true).unwrap();
    id
}

fn cancellable_ids(store: &Store, flow_id: Option<i64>, states: &[StateType]) -> Vec<i64> {
    store
        .cancellable_runs(flow_id, states)
        .unwrap()
        .into_iter()
        .map(|(id, _, _)| id)
        .collect()
}

#[test]
fn terminal_runs_are_not_cancellable() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");

    // A run left in its initial state is a candidate.
    let fresh = run_of(&store, f, "fresh").0;
    // Each terminal state must be excluded.
    let done = run_in_state(&store, f, "done", StateType::Completed);
    let bad = run_in_state(&store, f, "bad", StateType::Failed);
    let gone = run_in_state(&store, f, "gone", StateType::Cancelled);
    let boom = run_in_state(&store, f, "boom", StateType::Crashed);

    let ids = cancellable_ids(&store, None, &[]);
    assert!(ids.contains(&fresh), "a fresh run must be cancellable");
    for terminal in [done, bad, gone, boom] {
        assert!(
            !ids.contains(&terminal),
            "run {terminal} is in a terminal state and must not be cancellable"
        );
    }
}

/// `is_terminal` treats `Crashed` as terminal, with no rerun exception — the
/// same must hold here. `active_runs_of_schedule` uses a *different* rule and is
/// deliberately not what this query does.
#[test]
fn a_crashed_run_is_not_cancellable_even_with_a_pending_rerun() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let crashed = run_in_state(&store, f, "boom", StateType::Crashed);
    // A rerun exists, which is exactly the case the schedule query treats as live.
    store
        .create_run_full(CreateRun {
            flow_id: f,
            name: "retry".into(),
            created_by: "crash:3".into(),
            parent_run_id: Some(crashed),
            attempt: 1,
            ..Default::default()
        })
        .unwrap();
    assert!(
        !cancellable_ids(&store, None, &[]).contains(&crashed),
        "a crashed run must stay excluded; StateType::is_terminal treats it as terminal"
    );
}

#[test]
fn a_named_flow_restricts_the_targets() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let a = flow(&store, "p", "a");
    let b = flow(&store, "p", "b");
    let ra = run_in_state(&store, a, "ra", StateType::Running);
    let rb = run_in_state(&store, b, "rb", StateType::Running);

    assert_eq!(cancellable_ids(&store, Some(a), &[]), vec![ra]);
    assert_eq!(cancellable_ids(&store, Some(b), &[]), vec![rb]);
    // No flow named means every flow.
    let mut all = cancellable_ids(&store, None, &[]);
    all.sort_unstable();
    let mut want = vec![ra, rb];
    want.sort_unstable();
    assert_eq!(all, want);
}

#[test]
fn named_states_restrict_the_targets() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let running = run_in_state(&store, f, "r", StateType::Running);
    let queued = run_in_state(&store, f, "q", StateType::Pending);
    let cancelling = run_in_state(&store, f, "c", StateType::Cancelling);

    assert_eq!(
        cancellable_ids(&store, Some(f), &[StateType::Running]),
        vec![running]
    );
    let mut two = cancellable_ids(&store, Some(f), &[StateType::Running, StateType::Pending]);
    two.sort_unstable();
    let mut want = vec![running, queued];
    want.sort_unstable();
    assert_eq!(two, want);
    // An empty list is no restriction, not "match nothing".
    let mut all = cancellable_ids(&store, Some(f), &[]);
    all.sort_unstable();
    let mut want_all = vec![running, queued, cancelling];
    want_all.sort_unstable();
    assert_eq!(all, want_all);
}

#[test]
fn a_state_filter_never_reaches_a_terminal_run() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let running = run_in_state(&store, f, "r", StateType::Running);
    let done = run_in_state(&store, f, "d", StateType::Completed);
    // Naming a terminal state must not resurrect it.
    let ids = cancellable_ids(&store, Some(f), &[StateType::Running, StateType::Completed]);
    assert_eq!(ids, vec![running]);
    assert!(!ids.contains(&done));
}

#[test]
fn targets_carry_their_parameters_and_engine() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let (id, _) = store
        .create_run_full(CreateRun {
            flow_id: f,
            name: "r".into(),
            parameters: r#"{"region":"eu","day":"2026-09-29"}"#.into(),
            tags: "[]".into(),
            created_by: "test".into(),
            ..Default::default()
        })
        .unwrap();
    store.transition_run(id, State::new(StateType::Running), true).unwrap();

    let rows = store.cancellable_runs(Some(f), &[]).unwrap();
    assert_eq!(rows.len(), 1);
    let (rid, params, engine_pid) = &rows[0];
    assert_eq!(*rid, id);
    let parsed: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(params).unwrap();
    assert_eq!(parsed.get("region").unwrap(), "eu");
    assert_eq!(parsed.get("day").unwrap(), "2026-09-29");
    // No engine assigned yet, so the caller sees None and cancels immediately.
    assert_eq!(*engine_pid, None);
}

#[test]
fn targets_come_back_oldest_first() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let ids: Vec<i64> = (0..5)
        .map(|i| run_in_state(&store, f, &format!("r{i}"), StateType::Running))
        .collect();
    assert_eq!(cancellable_ids(&store, Some(f), &[]), ids);
}

#[test]
fn flows_by_name_finds_unique_ambiguous_and_absent() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let a1 = flow(&store, "proj-a", "shared");
    let a2 = flow(&store, "proj-a", "unique");
    let b1 = flow(&store, "proj-b", "shared");

    let shared = store.flows_by_name("shared").unwrap();
    let mut ids: Vec<i64> = shared.iter().map(|f| f.id).collect();
    ids.sort_unstable();
    assert_eq!(ids, vec![a1, b1], "a name in two projects is ambiguous");

    let unique = store.flows_by_name("unique").unwrap();
    assert_eq!(unique.len(), 1);
    assert_eq!(unique[0].id, a2);
    assert_eq!(unique[0].project, "proj-a", "the flow row is returned whole");

    assert!(store.flows_by_name("absent").unwrap().is_empty());
    assert!(store.flows_by_name("").unwrap().is_empty());
}


// ---- median run durations, batched -------------------------------------------
//
// `saturation` measured each uncapped flow with its own query. It now asks for
// all of them at once. The value must be identical, including the even-count
// case where the median is the *upper* middle rather than the mean of the two.

fn set_duration(store: &Store, run_id: i64, micros: i64) {
    let home = store.home().to_path_buf();
    // The store's writer owns the connection, so a short-lived direct
    // connection is the only way to set a value no API exposes.
    let conn = rusqlite::Connection::open(home.join("db.sqlite")).unwrap();
    conn.execute(
        "UPDATE run SET total_run_time = ?1 WHERE id = ?2",
        rusqlite::params![micros, run_id],
    )
    .unwrap();
}

fn median_of(store: &Store, flow_id: i64) -> Option<i64> {
    store.median_run_duration(flow_id).unwrap()
}

fn medians_of(store: &Store, ids: &[i64]) -> std::collections::HashMap<i64, i64> {
    store.median_run_duration_many(ids).unwrap()
}

#[test]
fn batched_medians_match_the_per_flow_read() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let a = flow(&store, "p", "a");
    let b = flow(&store, "p", "b");
    let c = flow(&store, "p", "c"); // no timed runs

    // Odd count for `a`, even for `b`, none for `c`.
    for (i, d) in [10i64, 20, 30].iter().enumerate() {
        set_duration(&store, run_of(&store, a, &format!("a{i}")).0, *d);
    }
    for (i, d) in [5i64, 15, 25, 35].iter().enumerate() {
        set_duration(&store, run_of(&store, b, &format!("b{i}")).0, *d);
    }

    let got = medians_of(&store, &[a, b, c]);
    for id in [a, b, c] {
        assert_eq!(
            got.get(&id).copied(),
            median_of(&store, id),
            "batched median differs for flow {id}"
        );
    }
    assert!(!got.contains_key(&c), "a flow with no timed runs has no median");
}

/// The case most likely to be got wrong: an even number of samples picks the
/// upper middle, not the mean of the two middles.
#[test]
fn an_even_count_picks_the_upper_middle() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    for (i, d) in [10i64, 20, 30, 40].iter().enumerate() {
        set_duration(&store, run_of(&store, f, &format!("r{i}")).0, *d);
    }
    // Sorted [10, 20, 30, 40]; len/2 == 2 -> 30, not (20+30)/2 == 25.
    assert_eq!(median_of(&store, f), Some(30));
    assert_eq!(medians_of(&store, &[f]).get(&f).copied(), Some(30));
}

#[test]
fn the_newest_durations_are_the_ones_sampled() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    // 150 runs, old ones tiny and the newest large. The sample cap of 101 means
    // the median comes from the newest 101, so it must be large.
    for i in 0..150i64 {
        set_duration(&store, run_of(&store, f, &format!("r{i}")).0, i);
    }
    let single = median_of(&store, f).expect("a median");
    assert_eq!(medians_of(&store, &[f]).get(&f).copied(), Some(single));
    // The newest 101 are i in 49..=150; the median of that is 99 or 100.
    assert!(
        (99..=100).contains(&single),
        "expected the newest sample's median, got {single}"
    );
}

#[test]
fn unrequested_flows_do_not_contribute() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let a = flow(&store, "p", "a");
    let b = flow(&store, "p", "b");
    set_duration(&store, run_of(&store, a, "a0").0, 100);
    set_duration(&store, run_of(&store, b, "b0").0, 900);

    let got = medians_of(&store, &[a]);
    assert_eq!(got.get(&a).copied(), Some(100));
    assert!(!got.contains_key(&b), "an unrequested flow appeared");
}

#[test]
fn asking_for_no_flows_asks_nothing() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    assert!(medians_of(&store, &[]).is_empty());
}

#[test]
fn a_run_with_no_duration_is_not_sampled() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    // One timed, one not.
    set_duration(&store, run_of(&store, f, "timed").0, 42);
    let _untimed = run_of(&store, f, "untimed");
    assert_eq!(median_of(&store, f), Some(42));
    assert_eq!(medians_of(&store, &[f]).get(&f).copied(), Some(42));
}

#[test]
fn a_flow_with_only_untimed_runs_has_no_median() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let _ = run_of(&store, f, "r");
    assert_eq!(median_of(&store, f), None);
    assert!(!medians_of(&store, &[f]).contains_key(&f));
}

// ---- batched flows and start times -------------------------------------------
//
// The overdue sweep runs every five seconds and used to do a flow read, a
// forty-row duration read and a full run materialisation *per running run*. These
// are the two batched reads that replaced them, and they must agree with the
// single-row reads they stand in for.

#[test]
fn batched_flows_match_the_single_reads() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let a = flow(&store, "p", "a");
    let b = flow(&store, "p", "b");

    let got = store.get_flows_by_ids(&[a, b]).unwrap();
    assert_eq!(got.len(), 2);
    for id in [a, b] {
        assert_eq!(
            got.get(&id),
            store.get_flow(id).unwrap().as_ref(),
            "batched flow differs for {id}"
        );
    }
    // A deleted or unknown id is simply absent, as `get_flow` returns None.
    assert!(!got.contains_key(&(a + 9_999)));
    assert!(store.get_flows_by_ids(&[a + 9_999]).unwrap().is_empty());
}

#[test]
fn batched_flows_of_nothing_asks_nothing() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    assert!(store.get_flows_by_ids(&[]).unwrap().is_empty());
}

#[test]
fn batched_flows_exclude_unrequested_ones() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let a = flow(&store, "p", "a");
    let b = flow(&store, "p", "b");
    let got = store.get_flows_by_ids(&[a]).unwrap();
    assert_eq!(got.len(), 1);
    assert!(got.contains_key(&a));
    assert!(!got.contains_key(&b), "an unrequested flow leaked in");
}

#[test]
fn batched_start_times_match_the_single_reads() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    // A finished run has an end time and a start time derived from it.
    let finished = run_in_state(&store, f, "done", StateType::Completed);
    // A queued run has neither.
    let queued = run_of(&store, f, "queued").0;

    let got = store.run_start_times(&[finished, queued]).unwrap();
    for id in [finished, queued] {
        let want = store.get_run(id).unwrap().and_then(|r| r.start_time);
        assert_eq!(
            got.get(&id).copied().flatten(),
            want,
            "batched start time differs for {id}"
        );
    }
    // Present with no value, not absent: a run that has not started is
    // distinguishable from a run that does not exist.
    assert!(got.contains_key(&queued), "an unstarted run must still be present");
    assert_eq!(got.get(&queued).copied().flatten(), None);
    assert!(!got.contains_key(&(queued + 9_999)), "an unknown id appeared");
}

#[test]
fn batched_start_times_of_nothing_asks_nothing() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    assert!(store.run_start_times(&[]).unwrap().is_empty());
}

#[test]
fn a_running_run_has_a_start_time() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let r = run_in_state(&store, f, "r", StateType::Running);
    let got = store.run_start_times(&[r]).unwrap();
    assert!(
        got.get(&r).copied().flatten().is_some(),
        "a Running run must have a start time for the sweep to measure it"
    );
}

// ---- a run's events are read through an index --------------------------------
//
// The `event` table had an index for every filter `query_events` supports except
// `run_id`, so the run detail page's event list was a full scan of the
// fastest-growing table in the system. These tests pin the plan, because the
// rows and their order are identical with or without the index.

/// The query plan lines for a run's events.
fn run_events_plan(store: &Store) -> Vec<String> {
    store
        .with_reader(|c| {
            let sql = "SELECT id, kind, timestamp, payload FROM event \
                       WHERE run_id = ?1 ORDER BY id DESC LIMIT 100";
            let mut stmt = c.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?;
            let rows = stmt
                .query_map([1i64], |r| r.get::<_, String>(3))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .unwrap()
}

/// Without this index the plan is `SCAN event`. This is the only test in the
/// suite that fails if the migration is removed.
#[test]
fn a_runs_events_are_read_through_an_index() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let plan = run_events_plan(&store).join(" | ");
    assert!(
        !plan.contains("SCAN event"),
        "listing a run's events still scans the event table: {plan}"
    );
    assert!(
        plan.contains("USING INDEX") || plan.contains("USING COVERING INDEX"),
        "the run's events are not served by an index: {plan}"
    );
    assert!(
        !plan.contains("TEMP B-TREE"),
        "the rows are sorted rather than walked in order: {plan}"
    );
}

#[test]
fn a_runs_events_keep_their_rows_and_order() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let (mine, _) = run_of(&store, f, "mine");
    let (other, _) = run_of(&store, f, "other");
    for i in 0..5i64 {
        let _ = store.append_event(new_event("run.updated", Some(mine), serde_json::json!({"i": i})));
        let _ = store.append_event(new_event("other.event", Some(other), serde_json::json!({})));
    }

    let mine_desc = store.list_events(None, 0, 100).unwrap();
    let only: Vec<_> = mine_desc
        .iter()
        .filter(|e| e.run_id == Some(mine))
        .collect();
    assert_eq!(only.len(), 5, "another run's events leaked in");
    let ids: Vec<i64> = only.iter().map(|e| e.id).collect();
    let mut desc = ids.clone();
    desc.sort_unstable_by(|a, b| b.cmp(a));
    assert_eq!(ids, desc, "descending by id");

    // A cursor page asks for what comes after an id, ascending. `list_events`
    // switches direction based on whether a cursor was given.
    let cursor = ids[2];
    let page = store.list_events(None, cursor, 100).unwrap();
    let after: Vec<i64> = page
        .iter()
        .filter(|e| e.run_id == Some(mine))
        .map(|e| e.id)
        .collect();
    let mut want: Vec<i64> = ids
        .iter()
        .copied()
        .filter(|id| *id > cursor)
        .collect();
    want.sort_unstable();
    assert_eq!(after, want, "the cursor page differs");
    assert!(
        after.windows(2).all(|w| w[0] < w[1]),
        "a cursor page must be ascending: {after:?}"
    );
}

#[test]
fn a_run_with_no_events_returns_nothing() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let (mine, _) = run_of(&store, f, "mine");
    assert!(mine > 0);
    let _ = store.append_event(new_event("run.updated", Some(mine + 999), serde_json::json!({})));
    let got: Vec<_> = store
        .list_events(None, 0, 100)
        .unwrap()
        .into_iter()
        .filter(|e| e.run_id == Some(mine))
        .collect();
    assert!(got.is_empty(), "a run with no events returned some");
}

/// The other four event filters must be planned exactly as they were before.
///
/// Asserting "uses its own index" per filter would be asserting a planner
/// decision, and it is not even always true: `event_time` is `(timestamp, id)`,
/// so a time filter ordered by `id` scans — and did so before this change too.
/// The invariant worth pinning is that *adding* the `run_id` index changes
/// nothing for the others, which is what comparing the plans directly tests.
///
/// A direct connection is used throughout because dropping an index needs write
/// access, which the store's read-only pool does not have.
#[test]
fn the_new_index_does_not_change_the_other_event_plans() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let (r, _) = run_of(&store, f, "r");
    for _ in 0..400 {
        let mut ev = new_event("run.updated", Some(r), serde_json::json!({}));
        ev.resource = cereyan_core::Resource {
            kind: "run".into(),
            id: r.to_string(),
            name: "r".into(),
        };
        let _ = store.append_event(ev);
    }
    drop(store);

    let conn = rusqlite::Connection::open(dir.path().join("db.sqlite")).unwrap();
    conn.execute_batch("ANALYZE").unwrap();
    let queries = [
        "SELECT id FROM event WHERE kind = 'run.updated' ORDER BY id DESC LIMIT 10",
        "SELECT id FROM event WHERE timestamp >= 1 ORDER BY id DESC LIMIT 10",
        "SELECT id FROM event WHERE timestamp >= 1 ORDER BY timestamp DESC LIMIT 10",
        "SELECT id FROM event WHERE resource_kind = 'run' AND resource_id = '1' ORDER BY id DESC LIMIT 10",
        "SELECT id FROM event WHERE flow_id = 1 ORDER BY id DESC LIMIT 10",
        "SELECT id FROM event ORDER BY id DESC LIMIT 10",
    ];
    let plan = |c: &rusqlite::Connection, sql: &str| -> String {
        let mut stmt = c.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(3))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        rows.join(" | ")
    };

    let with: Vec<String> = queries.iter().map(|q| plan(&conn, q)).collect();
    conn.execute_batch("DROP INDEX event_run_id").unwrap();
    let without: Vec<String> = queries.iter().map(|q| plan(&conn, q)).collect();

    for ((sql, a), b) in queries.iter().zip(&with).zip(&without) {
        assert_eq!(a, b, "the new index changed the plan for: {sql}");
    }
    // Sanity: the run query is the one that *does* differ, or this test would
    // pass on a database with no run index at all.
    let run_sql = "SELECT id FROM event WHERE run_id = 1 ORDER BY id DESC LIMIT 10";
    let with_run = plan(&conn, run_sql);
    conn.execute_batch("CREATE INDEX event_run_id ON event (run_id, id)").unwrap();
    let without_run = plan(&conn, run_sql);
    assert_ne!(
        with_run, without_run,
        "adding the index must change the run query's plan"
    );
    assert!(!without_run.contains("SCAN event"), "still scanning: {without_run}");
}

#[test]
fn the_event_run_index_is_added_to_an_older_database() {
    let dir = TempDir::new().unwrap();
    {
        // Close the store, then roll the file back with a direct connection: the
        // store's own connections are read-only or owned by the writer thread.
        drop(open(&dir));
        let conn = rusqlite::Connection::open(dir.path().join("db.sqlite")).unwrap();
        conn.execute_batch(
            "DROP INDEX event_run_id; DROP INDEX run_parent_run_id; DROP INDEX run_schedule_id; \
             DROP INDEX run_state_scheduled; PRAGMA user_version = 18;",
        )
            .unwrap();
    }
    let store = open(&dir);
    assert_eq!(
        store
            .with_reader(|c| Ok(c.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))?))
            .unwrap(),
        cereyan_store::latest_schema_version(),
        "the upgrade did not advance the version"
    );
    let plan = run_events_plan(&store).join(" | ");
    assert!(
        !plan.contains("SCAN event"),
        "an upgraded database did not get the index: {plan}"
    );
}

// ---- an event's run context --------------------------------------------------
//
// `record_event` runs on every event the server stores and reads six run values
// to build the event's resource and related entries. It used to read them out of
// a whole `Run`, which meant a correlated task_counts aggregate and four decoded
// JSON columns. These tests pin the context to the run it came from, since the
// two projections must not drift apart.

#[test]
fn the_run_context_matches_the_run_it_came_from() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let (run, _) = store
        .create_run_full(CreateRun {
            flow_id: f,
            name: "r1".into(),
            parameters: r#"{"day":"2026-09-29"}"#.into(),
            tags: r#"["prod","eu"]"#.into(),
            created_by: "test".into(),
            ..Default::default()
        })
        .unwrap();

    let ctx = store.run_event_context(run).unwrap().expect("the run exists");
    let full = store.get_run(run).unwrap().expect("the run exists");
    assert_eq!(ctx.external_id, full.external_id, "external id");
    assert_eq!(ctx.name, full.name, "name");
    assert_eq!(ctx.project, full.project, "project");
    assert_eq!(ctx.flow_name, full.flow_name, "flow name");
    assert_eq!(ctx.tags, full.tags, "tags");
    assert_eq!(ctx.state_name, full.state.name, "state name");
}

#[test]
fn the_run_context_follows_the_run() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");

    // A queued run: no state name yet, so the type is the fallback.
    let (queued, _) = run_of(&store, f, "queued");
    let ctx = store.run_event_context(queued).unwrap().expect("exists");
    let full = store.get_run(queued).unwrap().expect("exists");
    assert_eq!(ctx.state_name, full.state.name);
    assert_eq!(ctx.tags, full.tags);
    assert!(ctx.tags.is_empty(), "the run has no tags");

    // After a transition with a sub-state name.
    store
        .transition_run(
            queued,
            State::from_parts(StateType::Failed, Some("boom"), None, Default::default()),
            true,
        )
        .unwrap();
    let ctx = store.run_event_context(queued).unwrap().expect("exists");
    let full = store.get_run(queued).unwrap().expect("exists");
    assert_eq!(ctx.state_name, "boom", "the sub-state name is carried");
    assert_eq!(ctx.state_name, full.state.name);
}

#[test]
fn a_run_with_no_tags_yields_none() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let (run, _) = run_of(&store, f, "r");
    assert!(store
        .run_event_context(run)
        .unwrap()
        .expect("exists")
        .tags
        .is_empty());
}

#[test]
fn a_malformed_tag_list_yields_no_tags_rather_than_failing() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let (run, _) = store
        .create_run_full(CreateRun {
            flow_id: f,
            name: "r".into(),
            tags: "not json".into(),
            created_by: "test".into(),
            ..Default::default()
        })
        .unwrap();
    // The full reader swallows a malformed list too, so the two agree.
    let ctx = store.run_event_context(run).unwrap().expect("exists");
    let full = store.get_run(run).unwrap().expect("exists");
    assert!(ctx.tags.is_empty());
    assert_eq!(ctx.tags, full.tags);
}

#[test]
fn a_missing_run_has_no_context() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let (run, _) = run_of(&store, f, "r");
    assert!(store.run_event_context(run).unwrap().is_some());
    assert!(
        store.run_event_context(run + 9_999).unwrap().is_none(),
        "an unknown run must have no context, as get_run returns None"
    );
    // And a deleted one, which is the case the event path has to survive.
    store.delete_flow(f).unwrap();
    assert!(store.run_event_context(run).unwrap().is_none());
}

#[test]
fn the_context_does_not_carry_the_task_counts() {
    // Not a behaviour test so much as a statement of the shape: the context is
    // the six values, and nothing that would need the aggregate to produce.
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let (run, _) = run_of(&store, f, "r");
    for i in 0..5 {
        store
            .create_task_run(run, &format!("t{i}"), "t", &format!("k{i}"), 0)
            .unwrap();
    }
    // A run with task runs still yields a context: the aggregate is not needed
    // to produce one, which is the whole point of the projection.
    assert!(store.run_event_context(run).unwrap().is_some());
    // And the aggregate is still available where it is wanted. The counts are
    // keyed by state, and these five task runs are untransitioned, so they are
    // all `Pending`.
    let full = store.get_run(run).unwrap().expect("exists");
    assert_eq!(
        full.task_counts.get("Pending").copied(),
        Some(5),
        "get_run must still report task counts"
    );
}

/// Measures the two projections on the real code path, SQL and decode together.
///
/// The design's 9.38 µs / 4.00 µs figures came from the driver and cover SQL
/// only. This is the number that includes `run_from_row` decoding four JSON
/// columns against `run_event_context` decoding one.
#[test]
fn report_run_context_projection_cost() {
    if std::env::var("CEREYAN_BENCH_REPORT").is_err() {
        return; // opt-in: it is a measurement, not an assertion
    }
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let ids: Vec<i64> = (0..2_000)
        .map(|i| {
            let (id, _) = store
                .create_run_full(CreateRun {
                    flow_id: f,
                    name: format!("r{i}"),
                    parameters: r#"{"day":"2026-09-29","n":42,"nested":{"a":[1,2,3]}}"#.into(),
                    tags: r#"["prod","eu"]"#.into(),
                    created_by: "bench".into(),
                    ..Default::default()
                })
                .unwrap();
            for t in 0..20 {
                store.create_task_run(id, &format!("t{t}"), "k", &format!("d{t}"), 0).unwrap();
            }
            id
        })
        .collect();

    let n = 20_000i64;
    let t = Instant::now();
    for i in 0..n {
        std::hint::black_box(store.get_run(ids[(i as usize) % ids.len()]).unwrap());
    }
    let full = t.elapsed().as_secs_f64() / n as f64 * 1e6;

    let t = Instant::now();
    for i in 0..n {
        std::hint::black_box(store.run_event_context(ids[(i as usize) % ids.len()]).unwrap());
    }
    let light = t.elapsed().as_secs_f64() / n as f64 * 1e6;

    println!(
        "run projection: full RUN_COLUMNS {full:.2} us/row, \
         run_event_context {light:.2} us/row, {:.1}x",
        full / light
    );
}

// ---- a crashed run's rerun is found through an index -------------------------
//
// `active_runs_of_schedule` treats a Crashed run as live only while nothing
// points at it, via a correlated `NOT EXISTS` on `run.parent_run_id`. That column had no
// index, so the subquery scanned the whole run table once per crashed run: 21.5
// seconds against 2.7 ms on 50,000 runs. The plan is the only thing a test can
// see, so the plan is what it asserts.

/// A run of `schedule_id` in the given state.
fn scheduled_run_in(store: &Store, flow_id: i64, name: &str, schedule_id: i64, state: Option<StateType>) -> i64 {
    let (id, _) = store
        .create_run_full(CreateRun {
            flow_id,
            name: name.into(),
            created_by: "test".into(),
            schedule_id: Some(schedule_id),
            ..Default::default()
        })
        .unwrap();
    if let Some(st) = state {
        store.transition_run(id, State::new(st), true).unwrap();
    }
    id
}

/// The query plan lines for the pending-rerun check.
fn rerun_check_plan(dir: &std::path::Path) -> Vec<String> {
    let conn = rusqlite::Connection::open(dir.join("db.sqlite")).unwrap();
    let sql = "SELECT r.id FROM run r
               WHERE r.state_type = 'Crashed'
                 AND NOT EXISTS (SELECT 1 FROM run c WHERE c.parent_run_id = r.id)";
    let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(3))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    rows
}

/// Without this index the plan is `SCAN c` inside the correlated subquery. This
/// is the only test in the suite that fails if the migration is removed.
#[test]
fn the_rerun_check_uses_an_index_not_a_scan() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    drop(store);
    let plan = rerun_check_plan(dir.path()).join(" | ");
    assert!(
        !plan.contains("SCAN c"),
        "the pending-rerun check still scans the run table: {plan}"
    );
    assert!(
        plan.contains("run_parent_run_id"),
        "the search does not use the new index: {plan}"
    );
}

/// A crash rerun is created in the *same* schedule, so the crashed parent is
/// superseded and must not be counted — otherwise one attempt is counted twice.
#[test]
fn a_crashed_run_with_a_rerun_is_superseded() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let s = schedule(&store, f, "daily");
    let crashed = scheduled_run_in(&store, f, "crashed", s, Some(StateType::Crashed));
    // A rerun pointing at it, created the way `dispatch::crash_run` creates it:
    // same schedule, and an explicit initial state. The state matters -- this
    // query compares `state_type` without a COALESCE, so a run left with a NULL
    // state is excluded rather than treated as unfinished.
    store
        .create_run_full(CreateRun {
            flow_id: f,
            name: "retry".into(),
            created_by: format!("crash:{crashed}"),
            initial_state: Some(State::new(StateType::Scheduled)),
            parent_run_id: Some(crashed),
            attempt: 1,
            schedule_id: Some(s),
            ..Default::default()
        })
        .unwrap();

    let ids: Vec<i64> = store
        .active_runs_of_schedule(s)
        .unwrap()
        .into_iter()
        .map(|r| r.id)
        .collect();
    assert!(
        !ids.contains(&crashed),
        "a crashed run whose rerun is coming is superseded and must not count"
    );
    // The rerun itself, being Scheduled, is the outstanding one.
    assert!(
        ids.len() == 1 && ids[0] != crashed,
        "exactly the live attempt should be outstanding, got {ids:?}"
    );
}

/// Retries exhausted: no child exists, so the crashed run's work is still
/// outstanding and it counts.
#[test]
fn a_crashed_run_with_no_rerun_is_still_outstanding() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let s = schedule(&store, f, "daily");
    let crashed = scheduled_run_in(&store, f, "crashed", s, Some(StateType::Crashed));

    let ids: Vec<i64> = store
        .active_runs_of_schedule(s)
        .unwrap()
        .into_iter()
        .map(|r| r.id)
        .collect();
    assert!(
        ids.contains(&crashed),
        "a crashed run with nothing retrying it is still outstanding"
    );
}

#[test]
fn the_active_set_agrees_with_a_per_run_existence_check() {
    // The oracle: ask, per run, whether any run points at it. If the SQL rule
    // and this ever disagree the index is not the problem but the rule is.
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let s = schedule(&store, f, "daily");

    let running = scheduled_run_in(&store, f, "running", s, Some(StateType::Running));
    let done = scheduled_run_in(&store, f, "done", s, Some(StateType::Completed));
    let crashed_retried =
        scheduled_run_in(&store, f, "crashed-retried", s, Some(StateType::Crashed));
    store
        .create_run_full(CreateRun {
            flow_id: f,
            name: "retry".into(),
            created_by: format!("crash:{crashed_retried}"),
            initial_state: Some(State::new(StateType::Scheduled)),
            parent_run_id: Some(crashed_retried),
            attempt: 1,
            schedule_id: Some(s),
            ..Default::default()
        })
        .unwrap();

    let crashed_alone = scheduled_run_in(&store, f, "crashed-alone", s, Some(StateType::Crashed));

    let has_child = |id: i64| -> bool {
        store
            .with_reader(|c| {
                Ok(c.query_row(
                    "SELECT EXISTS(SELECT 1 FROM run c WHERE c.parent_run_id = ?1)",
                    [id],
                    |r| r.get::<_, bool>(0),
                )?)
            })
            .unwrap()
    };

    let got: Vec<i64> = store
        .active_runs_of_schedule(s)
        .unwrap()
        .into_iter()
        .map(|r| r.id)
        .collect();
    for id in [running, done, crashed_retried, crashed_alone] {
        // A crashed run is outstanding only when nothing is retrying it.
        let expected_active = id != done && id != crashed_retried;
        assert_eq!(
            got.contains(&id),
            expected_active,
            "run {id} active={} but a child exists={}",
            got.contains(&id),
            has_child(id)
        );
    }
    // The list is ordered by id, as before.
    let mut sorted = got.clone();
    sorted.sort_unstable();
    assert_eq!(got, sorted, "active runs must come back ordered by id");
}

/// The other ten `run` indexes must keep their plans. Asserting "uses its own
/// index" would be asserting a planner decision, so this compares the plans with
/// and without the new index instead.
#[test]
fn the_new_index_does_not_change_the_other_run_plans() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let sched = schedule(&store, f, "hourly");
    for i in 0..300i64 {
        let (id, _) = store
            .create_run_full(CreateRun {
                flow_id: f,
                name: format!("r{i}"),
                created_by: "test".into(),
                initial_state: Some(State::new(StateType::Scheduled)),
                // `run_schedule` is a *partial* index over rows where both
                // schedule_id and scheduled_time are set, so the fixture needs a
                // scheduled time for that index to be eligible at all.
                schedule_id: Some(sched),
                scheduled_time: Some(1_000 + i),
                ..Default::default()
            })
            .unwrap();
        assert!(id > 0);
        // Created Scheduled, then moved to Running: a run may not be created
        // directly in a running state.
        store
            .transition_run(id, State::new(StateType::Running), true)
            .unwrap();
    }
    drop(store);

    let conn = rusqlite::Connection::open(dir.path().join("db.sqlite")).unwrap();
    conn.execute_batch("ANALYZE").unwrap();
    // `run_schedule` is a *partial* index over rows where both schedule_id and
    // scheduled_time are set. SQLite only considers a partial index when the
    // WHERE clause implies its predicate, so the schedule query has to name
    // `scheduled_time IS NOT NULL` for the index to be eligible -- which is why
    // it appears here in that form and not as a bare `WHERE schedule_id = ?`.
    let queries: Vec<(&str, String, &str)> = vec![
        ("flow", "SELECT id FROM run WHERE flow_id = 1 ORDER BY id DESC LIMIT 10".into(), "run_flow_id"),
        ("state", "SELECT id FROM run WHERE state_type = 'Running' ORDER BY id DESC LIMIT 10".into(), "run_state_id"),
        ("name", "SELECT id FROM run WHERE name = 'r5'".into(), "run_name"),
        ("backfill", "SELECT id FROM run WHERE backfill_id = 1 ORDER BY id LIMIT 10".into(), "run_backfill"),
        ("schedule", format!("SELECT id FROM run WHERE schedule_id = {sched} AND scheduled_time IS NOT NULL ORDER BY scheduled_time LIMIT 10"), "run_schedule"),
        ("host", "SELECT id FROM run WHERE host = 'x' ORDER BY start_time LIMIT 10".into(), "run_host_start"),
        ("flow+state", "SELECT id FROM run WHERE flow_id = 1 AND state_type = 'Running' LIMIT 10".into(), "run_flow_state"),
    ];
    let plan = |sql: &str| -> String {
        let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(3))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        rows.join(" | ")
    };
    let with: Vec<String> = queries.iter().map(|(_, q, _)| plan(q)).collect();
    for ((label, _, index), p) in queries.iter().zip(&with) {
        assert!(p.contains(index), "the {label} filter lost {index}: {p}");
    }
    conn.execute_batch("DROP INDEX run_parent_run_id").unwrap();
    let without: Vec<String> = queries.iter().map(|(_, q, _)| plan(q)).collect();
    for (((_, sql, _), a), b) in queries.iter().zip(&with).zip(&without) {
        assert_eq!(a, b, "the new index changed the plan for: {sql}");
    }
}

#[test]
fn the_run_parent_index_is_added_to_an_older_database() {
    let dir = TempDir::new().unwrap();
    {
        drop(open(&dir));
        let conn = rusqlite::Connection::open(dir.path().join("db.sqlite")).unwrap();
        conn.execute_batch(
            "DROP INDEX run_parent_run_id; DROP INDEX run_schedule_id; \
             DROP INDEX run_state_scheduled; PRAGMA user_version = 19;",
        )
            .unwrap();
    }
    let store = open(&dir);
    assert_eq!(
        store
            .with_reader(|c| Ok(c.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))?))
            .unwrap(),
        cereyan_store::latest_schema_version(),
        "the upgrade did not advance the version"
    );
    let plan = rerun_check_plan(dir.path()).join(" | ");
    assert!(
        !plan.contains("SCAN c"),
        "an upgraded database did not get the index: {plan}"
    );
}

// ---- a schedule's runs are found through an index -----------------------------
//
// `run_schedule` is a *partial* unique index over rows where both schedule_id and
// scheduled_time are set. SQLite only considers a partial index when the query's
// WHERE implies its predicate, so `WHERE schedule_id = ?` cannot use it and both
// `active_runs_of_schedule` and `list_runs`-by-schedule scanned the run table.
//
// The tempting fix is to add `AND scheduled_time IS NOT NULL` to the queries.
// That would drop every crash rerun, which is created with `scheduled_time: None`
// — and a Scheduled rerun that vanished from its schedule's active set would stop
// holding the schedule's slot. The test below pins that the index covers it.

/// The query plan lines for a bare schedule filter.
fn schedule_filter_plan(dir: &std::path::Path, schedule_id: i64) -> Vec<String> {
    let conn = rusqlite::Connection::open(dir.join("db.sqlite")).unwrap();
    let sql = format!("SELECT id FROM run WHERE schedule_id = {schedule_id} ORDER BY id");
    let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
    stmt.query_map([], |r| r.get::<_, String>(3))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
}

/// Without this index the plan is `SCAN r`. This is the only test in the suite
/// that fails if the migration is removed.
#[test]
fn the_schedule_filter_uses_an_index_not_a_scan() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let s = schedule(&store, f, "hourly");
    drop(store);
    let plan = schedule_filter_plan(dir.path(), s).join(" | ");
    assert!(
        !plan.contains("SCAN r"),
        "a schedule's runs are still found by scanning the run table: {plan}"
    );
    assert!(
        plan.contains("run_schedule_id"),
        "the search does not use the new index: {plan}"
    );
}

/// The case the partial index could not have served, and the reason the query
/// must not be "fixed" by implying the partial predicate.
#[test]
fn a_rerun_with_no_scheduled_time_is_still_found_by_the_schedule() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let s = schedule(&store, f, "hourly");

    let crashed = scheduled_run_in(&store, f, "crashed", s, Some(StateType::Crashed));
    // Exactly as `dispatch::crash_run` creates it: same schedule, no scheduled
    // time, parent pointing at the crashed run.
    let (rerun, _) = store
        .create_run_full(CreateRun {
            flow_id: f,
            name: "crashed-retry-1".into(),
            created_by: format!("crash:{crashed}"),
            initial_state: Some(State::new(StateType::Scheduled)),
            parent_run_id: Some(crashed),
            attempt: 1,
            schedule_id: Some(s),
            scheduled_time: None,
            ..Default::default()
        })
        .unwrap();

    // The rerun is outstanding: it is Scheduled, and nothing is retrying *it*.
    let active: Vec<i64> = store
        .active_runs_of_schedule(s)
        .unwrap()
        .into_iter()
        .map(|r| r.id)
        .collect();
    assert!(
        active.contains(&rerun),
        "a rerun with no scheduled time must hold its schedule's slot, got {active:?}"
    );
    assert!(
        !active.contains(&crashed),
        "the superseded crashed parent must not be counted"
    );

    // And the plain schedule filter finds it too.
    let all: Vec<i64> = store
        .list_runs(&ListRunsFilter {
            schedule_id: Some(s),
            limit: Some(100),
            ..Default::default()
        })
        .unwrap()
        .items
        .into_iter()
        .map(|r| r.id)
        .collect();
    assert!(all.contains(&rerun), "the schedule filter lost the rerun");
    assert!(all.contains(&crashed));
}

#[test]
fn a_schedules_active_runs_are_unchanged_by_the_index() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let s = schedule(&store, f, "hourly");

    let running = scheduled_run_in(&store, f, "running", s, Some(StateType::Running));
    let done = scheduled_run_in(&store, f, "done", s, Some(StateType::Completed));
    let failed = scheduled_run_in(&store, f, "failed", s, Some(StateType::Failed));
    // Created but never used below: a run with no transition has a NULL state,
    // which the scheduled filter excludes. Kept so the fixture stays realistic.
    let _queued = scheduled_run_in(&store, f, "queued", s, None);
    let crashed_alone =
        scheduled_run_in(&store, f, "crashed-alone", s, Some(StateType::Crashed));
    let other_sched = schedule(&store, f, "other");
    let elsewhere = scheduled_run_in(&store, f, "elsewhere", other_sched, Some(StateType::Running));

    let got: Vec<i64> = store
        .active_runs_of_schedule(s)
        .unwrap()
        .into_iter()
        .map(|r| r.id)
        .collect();

    // Running, Scheduled (crashed alone: nothing retrying it, so its work is
    // still outstanding), and the never-transitioned one is excluded because the
    // query compares state_type without a COALESCE -- pre-existing, pinned.
    let mut expected: Vec<i64> = vec![running, crashed_alone];
    expected.sort_unstable();
    let mut actual = got.clone();
    actual.sort_unstable();
    assert_eq!(
        actual, expected,
        "only running and crashed-alone are outstanding"
    );

    assert!(!got.contains(&done), "a completed run is not active");
    assert!(!got.contains(&failed), "a failed run is not active");
    assert!(
        !got.contains(&elsewhere),
        "another schedule's run must not appear"
    );

    // Still ordered by id.
    let mut sorted = got.clone();
    sorted.sort_unstable();
    assert_eq!(got, sorted, "active runs come back ordered by id");
}

#[test]
fn the_schedule_index_does_not_change_the_other_run_plans() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let sched = schedule(&store, f, "hourly");
    for i in 0..300i64 {
        let (id, _) = store
            .create_run_full(CreateRun {
                flow_id: f,
                name: format!("r{i}"),
                created_by: "test".into(),
                initial_state: Some(State::new(StateType::Scheduled)),
                schedule_id: Some(sched),
                scheduled_time: Some(1_000 + i),
                ..Default::default()
            })
            .unwrap();
        assert!(id > 0);
        store
            .transition_run(id, State::new(StateType::Running), true)
            .unwrap();
    }
    drop(store);

    let conn = rusqlite::Connection::open(dir.path().join("db.sqlite")).unwrap();
    conn.execute_batch("ANALYZE").unwrap();
    // `run_schedule` is partial, so the schedule query has to name
    // `scheduled_time IS NOT NULL` for it to be eligible at all.
    let queries: Vec<(&str, String, &str)> = vec![
        ("flow", "SELECT id FROM run WHERE flow_id = 1 ORDER BY id DESC LIMIT 10".into(), "run_flow_id"),
        ("state", "SELECT id FROM run WHERE state_type = 'Running' ORDER BY id DESC LIMIT 10".into(), "run_state_id"),
        ("name", "SELECT id FROM run WHERE name = 'r5'".into(), "run_name"),
        ("backfill", "SELECT id FROM run WHERE backfill_id = 1 ORDER BY id LIMIT 10".into(), "run_backfill"),
        ("schedule", format!("SELECT id FROM run WHERE schedule_id = {sched} AND scheduled_time IS NOT NULL ORDER BY scheduled_time LIMIT 10"), "run_schedule"),
        ("host", "SELECT id FROM run WHERE host = 'x' ORDER BY start_time LIMIT 10".into(), "run_host_start"),
        ("flow+state", "SELECT id FROM run WHERE flow_id = 1 AND state_type = 'Running' LIMIT 10".into(), "run_flow_state"),
        ("parent", "SELECT id FROM run WHERE parent_run_id = 1".into(), "run_parent_run_id"),
    ];
    let plan = |sql: &str| -> String {
        let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
        stmt.query_map([], |r| r.get::<_, String>(3))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
            .join(" | ")
    };
    let with: Vec<String> = queries.iter().map(|(_, q, _)| plan(q)).collect();
    for ((label, _, index), p) in queries.iter().zip(&with) {
        assert!(p.contains(index), "the {label} filter lost {index}: {p}");
    }
    conn.execute_batch("DROP INDEX run_schedule_id").unwrap();
    let without: Vec<String> = queries.iter().map(|(_, q, _)| plan(q)).collect();
    for (((_, sql, _), a), b) in queries.iter().zip(&with).zip(&without) {
        assert_eq!(a, b, "the new index changed the plan for: {sql}");
    }
}

#[test]
fn the_run_schedule_index_is_added_to_an_older_database() {
    let dir = TempDir::new().unwrap();
    {
        drop(open(&dir));
        let conn = rusqlite::Connection::open(dir.path().join("db.sqlite")).unwrap();
        conn.execute_batch(
            "DROP INDEX run_parent_run_id; DROP INDEX run_schedule_id; \
             DROP INDEX run_state_scheduled; \
             PRAGMA user_version = 19;",
        )
        .unwrap();
    }
    let store = open(&dir);
    assert_eq!(
        store
            .with_reader(|c| Ok(c.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))?))
            .unwrap(),
        cereyan_store::latest_schema_version(),
        "the upgrade did not advance the version"
    );
    let plan = schedule_filter_plan(dir.path(), 1).join(" | ");
    assert!(
        !plan.contains("SCAN r"),
        "an upgraded database did not get the index: {plan}"
    );
}

// ---- a schedule's future runs are read as marks -------------------------------
//
// The scheduler's fire path materialises every future run of a schedule — up to
// `LOOKAHEAD_MAX = 100` — and reads three fields from each. These tests pin the
// marks to the whole runs they replace, because the two projections must not
// drift apart.

/// A schedule holding `n` future scheduled runs, some of them skipped.
fn schedule_with_future(store: &Store, f: i64, s: i64, n: i64) -> Vec<i64> {
    (0..n)
        .map(|i| {
            store
                .create_run_full(CreateRun {
                    flow_id: f,
                    name: format!("fire-{i}"),
                    created_by: "schedule".into(),
                    initial_state: Some(State::new(StateType::Scheduled)),
                    schedule_id: Some(s),
                    scheduled_time: Some(10_000 + i * 1_000),
                    ..Default::default()
                })
                .unwrap()
                .0
        })
        .collect()
}

#[test]
fn the_marks_match_the_runs_they_replace() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let s = schedule(&store, f, "hourly");
    let ids = schedule_with_future(&store, f, s, 5);

    // Mark one as skipped, the way a person skipping a fire would.
    store
        .transition_run(
            ids[2],
            State::from_parts(
                StateType::Scheduled,
                Some("skipped"),
                None,
                [("skip".to_string(), serde_json::Value::String("user".into()))].into_iter().collect(),
            ),
            true,
        )
        .unwrap();

    let marks = store.future_run_marks(s, 0).unwrap();
    let runs = store.future_runs_of_schedule(s, 0).unwrap();
    assert_eq!(marks.len(), runs.len(), "the two readers disagree on the count");

    for run in &runs {
        let mark = marks
            .iter()
            .find(|m| m.id == run.id)
            .unwrap_or_else(|| panic!("no mark for run {}", run.id));
        assert_eq!(mark.scheduled_time, run.scheduled_time, "scheduled time");
        assert_eq!(
            mark.details, run.state.details,
            "state details must match exactly, including the skip mark"
        );
    }
    // The skip mark is actually present, so the comparison above is not vacuous.
    assert_eq!(
        marks.iter().filter(|m| m.details.get("skip").is_some()).count(),
        1,
        "one run carries the skip mark"
    );
}

#[test]
fn the_marks_keep_the_reader_order_and_filters() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let s = schedule(&store, f, "hourly");
    let ids = schedule_with_future(&store, f, s, 6);

    // A run in another state, and a run of another schedule.
    let running = run_of(&store, f, "running").0;
    store.transition_run(running, State::new(StateType::Running), true).unwrap();
    let other_s = schedule(&store, f, "other");
    schedule_with_future(&store, f, other_s, 2);

    let marks = store.future_run_marks(s, 0).unwrap();
    let runs = store.future_runs_of_schedule(s, 0).unwrap();

    assert_eq!(
        marks.iter().map(|m| m.id).collect::<Vec<_>>(),
        runs.iter().map(|r| r.id).collect::<Vec<_>>(),
        "the same rows, in the same order"
    );
    let times: Vec<_> = marks.iter().filter_map(|m| m.scheduled_time).collect();
    let mut sorted = times.clone();
    sorted.sort_unstable();
    assert_eq!(times, sorted, "ordered by scheduled time");

    // Only this schedule's Scheduled runs.
    assert!(!marks.iter().any(|m| m.id == running), "a running run is excluded");
    assert!(
        marks.iter().all(|m| ids.contains(&m.id)),
        "another schedule's runs are excluded"
    );

    // The after-time filter.
    let cutoff = 13_000;
    let later = store.future_run_marks(s, cutoff).unwrap();
    assert!(
        later.iter().all(|m| m.scheduled_time.is_some_and(|t| t > cutoff)),
        "everything returned is after the cutoff"
    );
    assert_eq!(
        later.len(),
        marks.iter().filter(|m| m.scheduled_time.is_some_and(|t| t > cutoff)).count(),
        "the cutoff selects the same rows the whole reader would"
    );
}

#[test]
fn a_schedule_with_no_future_runs_has_no_marks() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let s = schedule(&store, f, "hourly");
    assert!(store.future_run_marks(s, 0).unwrap().is_empty());
}

#[test]
fn a_runs_marks_are_keyed_by_id_and_skip_missing_ones() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let ids = schedule_with_future(&store, f, schedule(&store, f, "hourly"), 4);

    let marks = store.run_marks(ids.iter().copied().chain(std::iter::once(999_999)));
    assert_eq!(marks.len(), 4, "a run that does not exist has no mark");
    for id in &ids {
        let mark = marks.get(id).unwrap_or_else(|| panic!("no mark for {id}"));
        assert_eq!(mark.id, *id);
    }
    // And it agrees with the per-schedule reader for the same runs.
    let by_schedule = store
        .future_run_marks(schedule(&store, f, "empty"), 0)
        .unwrap();
    assert!(by_schedule.is_empty());
    assert!(
        store.run_marks(std::iter::empty()).is_empty(),
        "an empty list reads nothing"
    );
}

/// Measures both projections on the real code path — the SQL and the decode.
/// Opt-in: it is a measurement, not an assertion.
#[test]
fn report_schedule_mark_projection_cost() {
    if std::env::var("CEREYAN_BENCH_REPORT").is_err() {
        return;
    }
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let s = schedule(&store, f, "hourly");
    schedule_with_future(&store, f, s, 100);
    // Task runs on each, so the task_counts aggregate has work to do.
    let task_runs: Vec<i64> = store
        .with_reader(|c| {
            let mut st = c.prepare("SELECT id FROM run LIMIT 100")?;
            let v = st
                .query_map([], |r| r.get::<_, i64>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(v)
        })
        .unwrap();
    for id in task_runs {
        for t in 0..20 {
            store
                .create_task_run(id, &format!("t{t}"), "k", &format!("d{t}"), 0)
                .unwrap();
        }
    }

    let iters = 200i64;
    let t = Instant::now();
    for _ in 0..iters {
        std::hint::black_box(store.future_runs_of_schedule(s, 0).unwrap());
    }
    let full = t.elapsed().as_secs_f64() / iters as f64 * 1e3;
    let t = Instant::now();
    for _ in 0..iters {
        std::hint::black_box(store.future_run_marks(s, 0).unwrap());
    }
    let marks = t.elapsed().as_secs_f64() / iters as f64 * 1e3;
    println!(
        "future runs of a 100-run schedule: whole runs {full:.3} ms/fire, \
         marks {marks:.3} ms/fire, {:.1}x",
        full / marks
    );
}

// ---- flow labels -------------------------------------------------------------
//
// `GET /metrics` labels its per-flow series with a flow's project and name, and
// used to read every flow through `list_flows` to get them: fourteen columns,
// three of them JSON, parsed per flow. These tests pin the labels to the flows
// they came from, since the two readers must not drift apart.

/// A flow with a schema, tags and options big enough that reading them is
/// visibly more work than reading its labels.
fn heavy_flow(store: &Store, project: &str, name: &str) -> i64 {
    let schema = serde_json::json!({
        "day": "string", "region": "string", "limit": "integer",
        "nested": {"a": 1, "b": [1, 2, 3]},
    })
    .to_string();
    let options = serde_json::json!({"retries": 5, "timeout": 3600, "concurrency": 2}).to_string();
    let tags = serde_json::json!(["prod", "eu", "team-a", "team-b"]).to_string();
    store
        .upsert_flow_full(cereyan_store::UpsertFlow {
            project: project.into(),
            name: name.into(),
            module: "pipeline".into(),
            source_dir: "/tmp/proj".into(),
            description: Some("a flow with a reasonably long description".into()),
            tags,
            parameter_schema: schema,
            options,
            ..Default::default()
        })
        .unwrap()
}

#[test]
fn the_labels_match_the_flows_they_came_from() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let a = heavy_flow(&store, "warehouse", "load");
    let b = heavy_flow(&store, "analytics", "rollup");
    flow(&store, "bare", "minimal");

    let labels = store.flow_labels().unwrap();
    let flows = store.list_flows(None).unwrap();
    assert_eq!(labels.len(), flows.len(), "the two readers disagree on the count");

    for f in &flows {
        let label = labels
            .iter()
            .find(|l| l.id == f.id)
            .unwrap_or_else(|| panic!("no label for flow {}", f.id));
        assert_eq!(label.project, f.project, "project for flow {}", f.id);
        assert_eq!(label.name, f.name, "name for flow {}", f.id);
    }
    let by_id = |id: i64| labels.iter().find(|l| l.id == id).unwrap().clone();
    assert_eq!(by_id(a).project, "warehouse");
    assert_eq!(by_id(a).name, "load");
    assert_eq!(by_id(b).project, "analytics");
}

#[test]
fn reading_labels_does_not_depend_on_the_size_of_a_flow() {
    // The point of the reader: a flow's schema, tags and options must not affect
    // how long its labels take to read. If a JSON column were being parsed, the
    // heavy flow would cost measurably more.
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    for i in 0..200 {
        heavy_flow(&store, "p", &format!("heavy-{i}"));
        flow(&store, "p", &format!("bare-{i}"));
    }
    let big = heavy_flow(&store, "p", "one-huge-flow");
    // Make one flow's schema enormous.
    let huge = "x".repeat(400_000);
    store
        .upsert_flow_full(cereyan_store::UpsertFlow {
            project: "p".into(),
            name: "enormous".into(),
            module: "pipeline".into(),
            source_dir: "/tmp/proj".into(),
            description: None,
            tags: serde_json::json!(["t"]).to_string(),
            parameter_schema: format!("{{\"blob\":\"{huge}\"}}"),
            options: "{}".into(),
            ..Default::default()
        })
        .unwrap();

    let labels = store.flow_labels().unwrap();
    assert_eq!(labels.len(), 402, "every flow has a label");
    // And the enormous one is labelled like any other.
    let enormous = labels.iter().find(|l| l.name == "enormous").unwrap();
    assert_eq!(enormous.project, "p");
    // Reading labels stays cheap; reading the whole flow does not.
    let t = Instant::now();
    for _ in 0..50 {
        std::hint::black_box(store.flow_labels().unwrap());
    }
    let labels_cost = t.elapsed().as_secs_f64() / 50.0 * 1e3;
    let t = Instant::now();
    for _ in 0..5 {
        std::hint::black_box(store.list_flows(None).unwrap());
    }
    let flows_cost = t.elapsed().as_secs_f64() / 5.0 * 1e3;
    println!(
        "402 flows (one with a 400 KB schema): labels {labels_cost:.3} ms, \
         whole flows {flows_cost:.3} ms, {:.1}x  [run {big}]",
        flows_cost / labels_cost
    );
}

#[test]
fn an_empty_flow_table_yields_no_labels() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    assert!(store.flow_labels().unwrap().is_empty());
}

#[test]
fn the_whole_flow_reader_is_unchanged() {
    // Narrowing the metrics reader must not have cost the other callers the
    // columns only `list_flows` carries.
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let id = heavy_flow(&store, "p", "etl");
    let f = store.get_flow(id).unwrap().expect("the flow exists");
    assert_eq!(f.module, "pipeline");
    assert_eq!(f.source_dir, "/tmp/proj");
    assert!(f.tags.contains(&"prod".to_string()), "tags: {:?}", f.tags);
    assert!(
        f.parameter_schema.get("nested").is_some(),
        "schema: {:?}",
        f.parameter_schema
    );
    assert!(!f.options.is_empty(), "options: {:?}", f.options);
    assert!(f.description.is_some());
}

// ---- the queue view's run projection -----------------------------------------
//
// `GET /api/queue` reads up to 550 queued runs on a page polled every five
// seconds, through `RUN_COLUMNS`: 37 columns, a correlated `task_counts`
// aggregate evaluated once per row, and four JSON columns decoded. The view shows
// nine plain columns. These tests pin the narrow reader to the wide one, because
// the two must not drift apart.

#[test]
fn the_queue_runs_match_the_whole_runs_they_replace() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let s = schedule(&store, f, "hourly");

    // Runs that differ in every field the projection carries.
    let plain = run_of(&store, f, "plain").0;
    let scheduled = store
        .create_run_full(CreateRun {
            flow_id: f,
            name: "scheduled".into(),
            created_by: "schedule".into(),
            initial_state: Some(State::new(StateType::Scheduled)),
            schedule_id: Some(s),
            scheduled_time: Some(5_000),
            ..Default::default()
        })
        .unwrap()
        .0;
    let backfilled = store
        .create_run_full(CreateRun {
            flow_id: f,
            name: "backfilled".into(),
            created_by: "backfill:7".into(),
            initial_state: Some(State::new(StateType::Scheduled)),
            backfill_id: Some(7),
            ..Default::default()
        })
        .unwrap()
        .0;

    let ids = [plain, scheduled, backfilled];
    let rows = store.queue_runs(&ids).unwrap();
    let whole = store.get_runs(&ids).unwrap();
    assert_eq!(rows.len(), whole.len(), "the two readers disagree on the count");

    for r in &whole {
        let row = rows
            .iter()
            .find(|q| q.id == r.id)
            .unwrap_or_else(|| panic!("no queue row for run {}", r.id));
        assert_eq!(row.name, r.name, "name for run {}", r.id);
        assert_eq!(row.flow_name, r.flow_name, "flow name for run {}", r.id);
        assert_eq!(row.project, r.project, "project for run {}", r.id);
        assert_eq!(row.state_name, r.state.name, "state name for run {}", r.id);
        assert_eq!(row.created_by, r.created_by, "created_by for run {}", r.id);
        assert_eq!(row.backfill_id, r.backfill_id, "backfill for run {}", r.id);
        assert_eq!(row.schedule_id, r.schedule_id, "schedule for run {}", r.id);
        assert_eq!(
            row.scheduled_time, r.scheduled_time,
            "scheduled time for run {}",
            r.id
        );
    }
    // The run carrying each distinguishing value is actually present, so the
    // comparison above is not vacuous.
    assert_eq!(
        rows.iter().filter(|r| r.schedule_id.is_some()).count(),
        1,
        "one run is scheduled"
    );
    assert_eq!(
        rows.iter().filter(|r| r.backfill_id.is_some()).count(),
        1,
        "one run is a backfill"
    );
    assert_eq!(
        rows.iter().filter(|r| r.scheduled_time.is_some()).count(),
        1,
        "one run has a scheduled time"
    );
}

/// A run created with no state has a NULL `state_type` and `state_name`. The
/// whole reader reports it as `Scheduled`; the projection must agree.
#[test]
fn a_run_with_no_state_reads_as_scheduled_in_both_readers() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let (id, _) = store
        .create_run_full(CreateRun {
            flow_id: f,
            name: "no-state".into(),
            created_by: "test".into(),
            ..Default::default()
        })
        .unwrap();
    let row = store.queue_runs(&[id]).unwrap().pop().expect("a row");
    let whole = store.get_run(id).unwrap().expect("the run");
    assert_eq!(row.state_name, whole.state.name, "both readers must agree");
    assert_eq!(row.state_name, "Scheduled");
}

#[test]
fn the_queue_readers_keep_their_filters_and_order() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let s = schedule(&store, f, "hourly");

    // Three scheduled runs at distinct times, one running, one before the window.
    let due: Vec<i64> = [1_000, 2_000, 3_000]
        .iter()
        .map(|t| {
            store
                .create_run_full(CreateRun {
                    flow_id: f,
                    name: format!("due-{t}"),
                    created_by: "schedule".into(),
                    initial_state: Some(State::new(StateType::Scheduled)),
                    schedule_id: Some(s),
                    scheduled_time: Some(*t),
                    ..Default::default()
                })
                .unwrap()
                .0
        })
        .collect();
    let running = store
        .create_run_full(CreateRun {
            flow_id: f,
            name: "running".into(),
            created_by: "api".into(),
            initial_state: Some(State::new(StateType::Scheduled)),
            schedule_id: Some(s),
            scheduled_time: Some(1_500),
            ..Default::default()
        })
        .unwrap()
        .0;
    store
        .transition_run(running, State::new(StateType::Running), true)
        .unwrap();
    let past = store
        .create_run_full(CreateRun {
            flow_id: f,
            name: "past".into(),
            created_by: "schedule".into(),
            initial_state: Some(State::new(StateType::Scheduled)),
            schedule_id: Some(s),
            scheduled_time: Some(100),
            ..Default::default()
        })
        .unwrap()
        .0;

    // The lower bound is exclusive and the upper inclusive, as `scheduled_between`
    // has always been; `past` at 100 is below the window.
    let rows = store.scheduled_queue_runs(500, 5_000, 50).unwrap();
    let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
    assert_eq!(ids, due, "only scheduled runs inside the window, in time order");
    // The whole reader agrees, row for row and in the same order.
    let whole: Vec<i64> = store
        .scheduled_between(500, 5_000, 50)
        .unwrap()
        .into_iter()
        .map(|r| r.id)
        .collect();
    assert_eq!(ids, whole, "the two readers return the same runs, in order");

    assert!(
        !rows.iter().any(|r| r.id == running),
        "a running run is not scheduled"
    );
    assert!(
        !rows.iter().any(|r| r.id == past),
        "a run before the window is excluded"
    );

    // The limit is honoured, and the bounds are half-open at the bottom.
    assert_eq!(store.scheduled_queue_runs(0, 9_999, 2).unwrap().len(), 2);
    assert!(
        store.scheduled_queue_runs(1_000, 5_000, 50).unwrap().iter().all(|r| r.scheduled_time != Some(1_000)),
        "a run at exactly the lower bound is excluded, as before"
    );
}

#[test]
fn the_queue_readers_skip_what_does_not_exist() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let one = run_of(&store, f, "real").0;
    let rows = store.queue_runs(&[one, one + 999_999]).unwrap();
    assert_eq!(rows.len(), 1, "a run that does not exist has no queue row");
    assert_eq!(rows[0].id, one);
    assert!(
        store.queue_runs(&[]).unwrap().is_empty(),
        "an empty list reads nothing"
    );
    assert!(
        store.scheduled_queue_runs(0, i64::MAX, 50).unwrap().is_empty(),
        "no scheduled runs yields nothing"
    );
}

/// Measures both readers on the real code path — SQL and decode together. Opt-in:
/// it is a measurement, not an assertion.
#[test]
fn report_queue_run_projection_cost() {
    if std::env::var("CEREYAN_BENCH_REPORT").is_err() {
        return;
    }
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let s = schedule(&store, f, "hourly");

    // Two fixtures, because they are not the same case: a queued run normally has
    // no task runs at all, and one rolling back to Pending has twenty.
    let mut queued = Vec::new();
    for i in 0..550 {
        let (id, _) = store
            .create_run_full(CreateRun {
                flow_id: f,
                name: format!("queued-{i}"),
                parameters: r#"{"day":"2026-09-30","region":"eu","nested":{"a":1,"b":[1,2,3]}}"#.to_string(),
                tags: r#"["prod","eu"]"#.into(),
                created_by: "schedule".into(),
                initial_state: Some(State::new(StateType::Scheduled)),
                schedule_id: Some(s),
                scheduled_time: Some(1_000 + i),
                ..Default::default()
            })
            .unwrap();
        queued.push(id);
    }
    for t in 0..20 {
        for id in queued.iter().take(50) {
            store
                .create_task_run(*id, &format!("t{t}"), "k", &format!("d{t}"), 0)
                .unwrap();
        }
    }

    let iters = 30i64;
    for (label, ids) in [
        ("550 queued runs, 50 with 20 task runs each", queued.clone()),
        ("550 queued runs, none with task runs", {
            let bare: Vec<i64> = queued[50..].to_vec();
            bare
        }),
    ] {
        let t = Instant::now();
        for _ in 0..iters {
            std::hint::black_box(store.get_runs(&ids).unwrap());
        }
        let full = t.elapsed().as_secs_f64() / iters as f64 * 1e3;
        let t = Instant::now();
        for _ in 0..iters {
            std::hint::black_box(store.queue_runs(&ids).unwrap());
        }
        let light = t.elapsed().as_secs_f64() / iters as f64 * 1e3;
        println!(
            "queue projection, {label}: whole runs {full:.3} ms, \
             queue rows {light:.3} ms, {:.1}x",
            full / light
        );
    }
}

// ---- a run's parameters, on their own ----------------------------------------
//
// `/api/resources/acquire` needs the parameters to render resource-name
// templates, and used to read a whole `Run` for them — 37 columns, a correlated
// `task_counts` aggregate and four decoded JSON documents. These tests pin the
// one-column reader to the wide one.

#[test]
fn the_run_parameters_match_the_whole_run() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");

    // Flat, nested, array-valued and empty: a resource name may be a template over
    // any of them, so all four have to survive identically.
    let cases: Vec<(&str, &str)> = vec![
        ("flat", r#"{"tenant":"acme","day":"2026-09-30"}"#),
        ("nested", r#"{"nested":{"a":1,"b":[1,2,3]}}"#),
        ("array", r#"{"list":[1,2,3,4,5],"tags":["a","b"]}"#),
        ("empty", "{}"),
    ];
    let mut ids = Vec::new();
    for (name, params) in &cases {
        let (id, _) = store
            .create_run_full(CreateRun {
                flow_id: f,
                name: name.to_string(),
                parameters: params.to_string(),
                tags: "[]".into(),
                created_by: "test".into(),
                ..Default::default()
            })
            .unwrap();
        ids.push(id);
    }

    for (id, (name, params)) in ids.iter().zip(&cases) {
        let got = store
            .run_parameters(*id)
            .unwrap()
            .unwrap_or_else(|| panic!("no parameters for {name}"));
        let whole = store.get_run(*id).unwrap().expect("the run");
        assert_eq!(
            got, whole.parameters,
            "parameters differ for {name}: {params}"
        );
    }
    // And the values are the ones that went in, so the comparison is not vacuous.
    let acme = store.run_parameters(ids[0]).unwrap().unwrap();
    assert_eq!(acme["tenant"], serde_json::json!("acme"));
    let nested = store.run_parameters(ids[1]).unwrap().unwrap();
    assert_eq!(nested["nested"]["b"][2], serde_json::json!(3), "nesting survives");
    assert_eq!(nested["nested"]["b"].as_array().unwrap().len(), 3);
}

#[test]
fn a_missing_run_has_no_parameters() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let (id, _) = run_of(&store, f, "r");
    assert!(store.run_parameters(id).unwrap().is_some());
    assert!(
        store.run_parameters(id + 999_999).unwrap().is_none(),
        "a run that does not exist has no parameters, as get_run returns None"
    );
}

/// A malformed column yields an empty map through both readers, so the acquire
/// path's `unwrap_or_default()` behaves the same before and after.
#[test]
fn a_malformed_parameters_column_yields_an_empty_map() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let (id, _) = store
        .create_run_full(CreateRun {
            flow_id: f,
            name: "broken".into(),
            parameters: "not json".into(),
            tags: "[]".into(),
            created_by: "test".into(),
            ..Default::default()
        })
        .unwrap();
    let got = store.run_parameters(id).unwrap();
    let whole = store.get_run(id).unwrap().expect("the run");
    assert_eq!(got, Some(whole.parameters), "both readers agree");
    assert!(got.unwrap().is_empty(), "and it is empty");
}

/// Measures both readers on the real code path. Opt-in: it is a measurement, not
/// an assertion.
#[test]
fn report_run_parameters_cost() {
    if std::env::var("CEREYAN_BENCH_REPORT").is_err() {
        return;
    }
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let ids: Vec<i64> = (0..2_000)
        .map(|i| {
            let (id, _) = store
                .create_run_full(CreateRun {
                    flow_id: f,
                    name: format!("r{i}"),
                    parameters: r#"{"tenant":"acme","region":"eu","day":"2026-09-30","nested":{"a":1,"b":[1,2,3]},"list":[1,2,3,4,5]}"#.to_string(),
                    tags: r#"["prod","eu"]"#.to_string(),
                    created_by: "bench".into(),
                    ..Default::default()
                })
                .unwrap();
            for t in 0..20 {
                store
                    .create_task_run(id, &format!("t{t}"), "k", &format!("d{t}"), 0)
                    .unwrap();
            }
            id
        })
        .collect();

    let n = 20_000i64;
    let t = Instant::now();
    for i in 0..n {
        std::hint::black_box(store.get_run(ids[(i as usize) % ids.len()]).unwrap());
    }
    let full = t.elapsed().as_secs_f64() / n as f64 * 1e6;
    let t = Instant::now();
    for i in 0..n {
        std::hint::black_box(store.run_parameters(ids[(i as usize) % ids.len()]).unwrap());
    }
    let light = t.elapsed().as_secs_f64() / n as f64 * 1e6;
    println!(
        "run parameters: whole run {full:.2} us, parameters alone {light:.2} us, {:.1}x",
        full / light
    );
}

// ---- a backfill's newest run ------------------------------------------------
//
// `backfill_completed` runs on every terminal run of a backfill, and aggregating
// the backfill's whole run set each time is quadratic. The guard is a single seek
// for the newest run, so these tests pin what that lookup returns and that it is
// a search rather than a scan.

#[test]
fn the_newest_backfill_run_is_a_seek_not_a_scan() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let plan = store
        .with_reader(|c| {
            let mut st = c.prepare(
                "EXPLAIN QUERY PLAN SELECT COALESCE(state_name, 'Scheduled') FROM run \
                 WHERE backfill_id = 1 ORDER BY id DESC LIMIT 1",
            )?;
            let rows = st
                .query_map([], |r| r.get::<_, String>(3))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows.join(" | "))
        })
        .unwrap();
    assert!(
        plan.contains("run_backfill"),
        "the guard must use the backfill index: {plan}"
    );
    assert!(!plan.contains("SCAN run"), "and must not scan: {plan}");
}

#[test]
fn the_newest_backfill_run_is_the_last_one_created() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    // The guard reads only `run.backfill_id`, so an id with no backfill row is a
    // faithful fixture and keeps the test to the query under test.
    let b = 7i64;

    let mut ids = Vec::new();
    for name in ["first", "second", "third"].iter() {
        let (id, _) = store
            .create_run_full(CreateRun {
                flow_id: f,
                name: (*name).to_string(),
                created_by: format!("backfill:{b}"),
                backfill_id: Some(b),
                initial_state: Some(State::new(StateType::Scheduled)),
                ..Default::default()
            })
            .unwrap();
        // A run may not be created in a running state, so move it there.
        store.transition_run(id, State::new(StateType::Running), true).unwrap();
        ids.push(id);
    }

    assert_eq!(
        store.newest_backfill_run_state(b).unwrap(),
        Some("Running".to_string()),
        "the last-created run, whatever the earlier ones are doing"
    );

    // Change the newest run and the answer follows. A sub-state name is carried
    // through, which is what a skipped or replayed run looks like.
    store
        .transition_run(ids[2], State::from_parts(StateType::Completed, Some("Cached"), None, Default::default()), true)
        .unwrap();
    assert_eq!(
        store.newest_backfill_run_state(b).unwrap(),
        Some("Cached".to_string()),
        "the newest run's own state name, not its state type"
    );

    // A backfill with no runs has no newest run.
    assert!(
        store.newest_backfill_run_state(b + 1_000).unwrap().is_none(),
        "a backfill with no runs has no newest run"
    );
}

#[test]
fn an_unstarted_run_reports_scheduled() {
    // `state_name` is nullable; the guard coalesces it the way `backfill_counts`
    // does, so the two readers agree on what a not-yet-written run is called.
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let b = 7;
    let (id, _) = store
        .create_run_full(CreateRun {
            flow_id: f,
            name: "bare".into(),
            created_by: format!("backfill:{b}"),
            backfill_id: Some(b),
            ..Default::default()
        })
        .unwrap();
    let raw: Option<String> = store
        .with_reader(|c| {
            Ok(c.query_row("SELECT state_name FROM run WHERE id = ?1", [id], |r| {
                r.get(0)
            })?)
        })
        .unwrap();
    assert!(raw.is_none(), "the fixture really has no state name");
    assert_eq!(
        store.newest_backfill_run_state(b).unwrap(),
        Some("Scheduled".to_string())
    );
}

// ---- a run's flow name, on its own --------------------------------------------
//
// A worker reports a flow name beside each of its engines every five seconds.
// It used to read each engine's run whole — 37 columns, a correlated
// `task_counts` aggregate, four decoded JSON documents — for one string.

#[test]
fn the_flow_names_match_the_whole_runs() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    // Runs spread across flows, so a wrong join would show up.
    let mut ids = Vec::new();
    for (i, (project, name)) in [("warehouse", "load"), ("analytics", "rollup"), ("p", "etl")]
        .iter()
        .enumerate()
    {
        for j in 0..3 {
            let (id, _) = store
                .create_run_full(CreateRun {
                    flow_id: flow(&store, project, name),
                    name: format!("{name}-{j}"),
                    parameters: "{}".into(),
                    tags: "[]".into(),
                    created_by: "test".into(),
                    ..Default::default()
                })
                .unwrap();
            ids.push(id);
        }
        let _ = i;
    }

    let names = store.flow_names(&ids);
    let whole = store.get_runs(&ids).unwrap();
    assert_eq!(names.len(), whole.len(), "the two readers disagree on the count");
    for r in &whole {
        assert_eq!(
            names.get(&r.id).map(|s| s.as_str()),
            Some(r.flow_name.as_str()),
            "flow name for run {}",
            r.id
        );
    }
    // And the distinct names are actually present, so the comparison is not
    // passing because every run reported the same thing.
    let distinct: std::collections::HashSet<&String> = names.values().collect();
    assert_eq!(distinct.len(), 3, "three flows, three names: {distinct:?}");
}

#[test]
fn the_flow_names_reader_handles_what_it_is_given() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let (one, _) = run_of(&store, f, "one");
    let (two, _) = run_of(&store, f, "two");

    assert!(
        store.flow_names(&[]).is_empty(),
        "an empty list reads nothing"
    );

    // An unknown id contributes nothing, rather than a blank name.
    let names = store.flow_names(&[one, two, 999_999]);
    assert_eq!(names.len(), 2, "the unknown id has no entry");
    assert!(!names.contains_key(&999_999));

    // Two engines reporting the same run get the same name from one entry.
    let names = store.flow_names(&[one, one]);
    assert_eq!(names.len(), 1);
    assert_eq!(names[&one], "etl");

    // A run whose flow was deleted has no name, like the whole reader omitting it.
    store.delete_flow(f).unwrap();
    assert!(
        store.flow_names(&[one]).is_empty(),
        "a run with no flow has no flow name"
    );
}

#[test]
fn the_whole_run_reader_still_returns_everything() {
    // Narrowing the heartbeat's reader must not have cost `get_runs` anything.
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let (id, _) = store
        .create_run_full(CreateRun {
            flow_id: f,
            name: "r".into(),
            parameters: r#"{"day":"2026-09-30"}"#.into(),
            tags: r#"["prod"]"#.into(),
            created_by: "test".into(),
            ..Default::default()
        })
        .unwrap();
    for t in 0..3 {
        store.create_task_run(id, &format!("t{t}"), "k", &format!("d{t}"), 0).unwrap();
    }
    let r = store.get_run(id).unwrap().expect("the run");
    assert_eq!(r.parameters["day"], serde_json::json!("2026-09-30"));
    assert_eq!(r.tags, vec!["prod".to_string()]);
    assert_eq!(
        r.task_counts.get("Pending").copied(),
        Some(3),
        "the task-count aggregate is still produced"
    );
    assert_eq!(r.flow_name, "etl");
    // And `get_runs`, the whole-run reader this replaced at the call site.
    let all = store.get_runs(&[id]).unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].task_counts.get("Pending").copied(), Some(3));
}

// ---- a run as a timeline row --------------------------------------------------
//
// The timeline lists the runs a host might take next. It reads five fields of
// each, from `RUN_COLUMNS`, and read the whole flow table even for the server's
// own timeline where the flow rows are never consulted.

#[test]
fn the_timeline_rows_match_the_whole_runs() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f1 = flow(&store, "p", "etl");
    let f2 = flow(&store, "q", "billing");

    // One scheduled, one queued without a scheduled time: the two branches of
    // `scheduled_time.or(Some(created_at))`.
    let scheduled = store
        .create_run_full(CreateRun {
            flow_id: f1,
            name: "scheduled".into(),
            parameters: "{}".into(),
            tags: "[]".into(),
            created_by: "test".into(),
            initial_state: Some(State::new(StateType::Scheduled)),
            scheduled_time: Some(5_000),
            ..Default::default()
        })
        .unwrap()
        .0;
    let queued = run_of(&store, f2, "queued").0;

    let ids = [scheduled, queued];
    let rows = store.timeline_run_rows(&ids).unwrap();
    let whole = store.get_runs(&ids).unwrap();
    assert_eq!(rows.len(), whole.len(), "the two readers disagree on the count");

    for r in &whole {
        let row = rows
            .iter()
            .find(|t| t.id == r.id)
            .unwrap_or_else(|| panic!("no timeline row for run {}", r.id));
        assert_eq!(row.flow_name, r.flow_name, "flow name for run {}", r.id);
        assert_eq!(row.flow_id, r.flow_id, "flow id for run {}", r.id);
        assert_eq!(
            row.scheduled_time, r.scheduled_time,
            "scheduled time for run {}",
            r.id
        );
        assert_eq!(row.created_at, r.created_at, "created at for run {}", r.id);
    }
    // The two branches are both represented, so the comparison is not vacuous.
    assert_eq!(
        rows.iter().filter(|r| r.scheduled_time.is_some()).count(),
        1,
        "one run is scheduled"
    );
    assert_eq!(
        rows.iter().filter(|r| r.scheduled_time.is_none()).count(),
        1,
        "one run is not"
    );
}

#[test]
fn the_scheduled_timeline_rows_keep_their_window_and_order() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let s = schedule(&store, f, "hourly");
    // 499, 500 and 5,000 sit on or beside the two window edges. The window is
    // `after` exclusive and `until` inclusive, so 500 is *out* and 5,000 is *in*.
    // An injection turning `>` into `>=` passed every other test in this file,
    // because nothing was on the boundary.
    let at = |t: i64| -> i64 {
        store
            .create_run_full(CreateRun {
                flow_id: f,
                name: format!("t{t}"),
                parameters: "{}".into(),
                tags: "[]".into(),
                created_by: "schedule".into(),
                initial_state: Some(State::new(StateType::Scheduled)),
                schedule_id: Some(s),
                scheduled_time: Some(t),
                ..Default::default()
            })
            .unwrap()
            .0
    };
    let before = at(499);
    let on_the_lower_edge = at(500);
    let mut due = vec![at(1_000), at(2_000)];
    due.push(at(3_000));
    due.push(at(5_000));

    let rows = store.scheduled_timeline_run_rows(500, 5_000, 50).unwrap();
    let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
    assert_eq!(
        ids, due,
        "runs from `after` (exclusive) to `until` (inclusive), in time order"
    );
    assert!(!ids.contains(&before), "a run before the window is excluded");
    assert!(
        !ids.contains(&on_the_lower_edge),
        "`after` is exclusive, so a run due exactly at it is excluded"
    );
    assert!(
        ids.contains(due.last().unwrap()),
        "`until` is inclusive, so a run due exactly at it is included"
    );
    // The whole reader agrees on the boundary too -- it is the same WHERE clause,
    // and this is the assertion that would catch the two drifting apart.
    let whole: Vec<i64> = store
        .scheduled_between(500, 5_000, 50)
        .unwrap()
        .into_iter()
        .map(|r| r.id)
        .collect();
    assert_eq!(ids, whole, "the two readers agree at the edges");
    let times: Vec<i64> = rows.iter().filter_map(|r| r.scheduled_time).collect();
    let mut sorted = times.clone();
    sorted.sort_unstable();
    assert_eq!(times, sorted, "ordered by scheduled time");

    assert_eq!(store.scheduled_timeline_run_rows(0, 9_999, 2).unwrap().len(), 2);
}

#[test]
fn the_timeline_readers_skip_what_they_are_not_given() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let one = run_of(&store, f, "real").0;
    assert_eq!(store.timeline_run_rows(&[one, 999_999]).unwrap().len(), 1);
    assert!(store.timeline_run_rows(&[]).unwrap().is_empty());
    assert!(store.scheduled_timeline_run_rows(0, i64::MAX, 50).unwrap().is_empty());
}

#[test]
fn the_flow_options_reader_returns_only_the_asked_for_flows() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let a = flow(&store, "p", "etl");
    let b = flow(&store, "q", "billing");
    let c = flow(&store, "r", "lonely");

    let got = store.flow_options_by_id(&[a, b, 999_999]).unwrap();
    assert_eq!(got.len(), 2, "only the two that exist");
    assert!(!got.contains_key(&c), "an unasked flow is not read");
    // The options are the flow's own.
    assert_eq!(got[&a], store.get_flow(a).unwrap().expect("the flow").options);
    assert_eq!(got[&b], store.get_flow(b).unwrap().expect("the flow").options);
    // And the remote-eligibility rule still applies to them.
    use cereyan_core::FlowOptions;
    for (id, options) in &got {
        let by_reader = FlowOptions::from_map(options).may_run_remotely();
        let by_whole = FlowOptions::from_map(
            &store.get_flow(*id).unwrap().expect("the flow").options,
        )
        .may_run_remotely();
        assert_eq!(by_reader, by_whole, "the rule agrees for flow {id}");
    }

    assert!(store.flow_options_by_id(&[]).unwrap().is_empty());
}

/// Opt-in measurement of the two reader shapes over one real store.
///
/// The timeline keeps twenty rows out of a few hundred candidates and shows four
/// fields of each, so the question is what the whole-run reader costs for that.
/// A measurement, not an assertion: run with
/// `CEREYAN_BENCH_REPORT=1 cargo test -p cereyan-store --test store report_timeline -- --nocapture`.
#[test]
fn report_timeline_reads() {
    if std::env::var("CEREYAN_BENCH_REPORT").is_err() {
        return;
    }
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let now = 1_000_000i64;

    // A realistic registration: 2,000 flows, and a queue and a schedule on top.
    for i in 0..2_000i64 {
        store
            .upsert_flow_full(cereyan_store::UpsertFlow {
                project: format!("p{}", i % 20),
                name: format!("flow_{i}"),
                module: format!("m{}", i % 20),
                source_dir: format!("/srv/flow_{i}"),
                description: None,
                tags: format!("[\"t{}\"]", i % 5),
                parameter_schema: r#"{"type":"object","properties":{"day":{"type":"string"},"region":{"type":"string"},"nested":{"type":"object"}}}"#.into(),
                options: r#"{"retries":5,"remote":"auto"}"#.into(),
                group: None,
                ..Default::default()
            })
            .unwrap();
    }
    let flows: Vec<i64> = (0..2_000i64).map(|i| i + 1).collect();
    for (i, flow_id) in flows.iter().enumerate() {
        for k in 0..3i64 {
            store
                .create_run_full(CreateRun {
                    flow_id: *flow_id,
                    name: format!("r{i}-{k}"),
                    parameters: r#"{"day":"2026-09-30","nested":{"a":1,"b":[1,2,3]}}"#.into(),
                    tags: r#"["prod"]"#.into(),
                    created_by: "bench".into(),
                    initial_state: Some(State::new(StateType::Scheduled)),
                    scheduled_time: Some(now + (i as i64 * 3 + k) * 1_000),
                    ..Default::default()
                })
                .unwrap();
        }
    }

    // The 300 ids a busy queue snapshot would hand the timeline.
    let ids: Vec<i64> = store
        .with_reader(|c| {
            let mut st = c
                .prepare("SELECT id FROM run ORDER BY id LIMIT 300")
                .unwrap();
            Ok(st
                .query_map([], |r| r.get::<_, i64>(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap())
        })
        .unwrap();

    let iters = 200i64;
    let horizon = now + 3_600_000_000;
    let ms = |t: std::time::Instant, n: i64| t.elapsed().as_secs_f64() / n as f64 * 1e3;

    // The old shape: whole runs for the queued ids, whole runs for the window.
    let t = std::time::Instant::now();
    for _ in 0..iters {
        let mut c = store.get_runs(&ids).unwrap();
        c.extend(store.scheduled_between(now, horizon, 100).unwrap());
        std::hint::black_box(c.len());
    }
    let whole = ms(t, iters);

    // The new shape: five columns each.
    let t = std::time::Instant::now();
    for _ in 0..iters {
        let mut c = store.timeline_run_rows(&ids).unwrap();
        c.extend(store.scheduled_timeline_run_rows(now, horizon, 100).unwrap());
        std::hint::black_box(c.len());
    }
    let narrow = ms(t, iters);

    // Each half on its own, so the total is attributable rather than a single
    // number that could be hiding a cost concentrated in one reader.
    let t = std::time::Instant::now();
    for _ in 0..iters {
        std::hint::black_box(store.get_runs(&ids).unwrap().len());
    }
    let whole_queued = ms(t, iters);
    let t = std::time::Instant::now();
    for _ in 0..iters {
        std::hint::black_box(store.timeline_run_rows(&ids).unwrap().len());
    }
    let narrow_queued = ms(t, iters);
    let t = std::time::Instant::now();
    for _ in 0..iters {
        std::hint::black_box(
            store
                .scheduled_between(now, horizon, 100)
                .unwrap()
                .len(),
        );
    }
    let whole_scheduled = ms(t, iters);
    let t = std::time::Instant::now();
    for _ in 0..iters {
        std::hint::black_box(
            store
                .scheduled_timeline_run_rows(now, horizon, 100)
                .unwrap()
                .len(),
        );
    }
    let narrow_scheduled = ms(t, iters);

    // And the read the timeline no longer does when it is not filtering.
    let t = std::time::Instant::now();
    for _ in 0..iters / 4 {
        std::hint::black_box(store.list_flows(None).unwrap().len());
    }
    let all_flows = ms(t, iters / 4);
    let candidate_flows: Vec<i64> = {
        let mut v: Vec<i64> = store
            .timeline_run_rows(&ids)
            .unwrap()
            .into_iter()
            .map(|r| r.flow_id)
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    };
    let t = std::time::Instant::now();
    for _ in 0..iters / 4 {
        std::hint::black_box(store.flow_options_by_id(&candidate_flows).unwrap().len());
    }
    let some_flows = ms(t, iters / 4);
    let before_server = whole + all_flows;
    let after_server = narrow;
    let after_worker = narrow + some_flows;

    println!(
        "timeline, 2000 flows / 6000 runs, 300 queued + 100 scheduled candidates:\n  \
         both, whole runs   {whole:7.3} ms\n  \
         both, five columns {narrow:7.3} ms  ({:.1}x)\n  \
         queued, whole      {whole_queued:7.3} ms -> five columns {narrow_queued:7.3} ms  ({:.1}x)\n  \
         scheduled, whole   {whole_scheduled:7.3} ms -> five columns {narrow_scheduled:7.3} ms  ({:.1}x)\n  \
         list_flows(None)   {all_flows:7.3} ms  ({} flows, dead when unfiltered)\n  \
         flow_options_by_id {some_flows:7.3} ms  ({} of them, only when filtering)",
        whole / narrow,
        whole_queued / narrow_queued,
        whole_scheduled / narrow_scheduled,
        2_000,
        candidate_flows.len(),
    );
    // What each view costs end to end, which is the number that matters: the
    // server's own view is the default one, and it is where the dead read was.
    println!(
        "  server view (id == 0): {before_server:.2} ms -> {after_server:.2} ms  ({:.1}x)",
        before_server / after_server
    );
    println!(
        "  worker view (id != 0): {before_server:.2} ms -> {after_worker:.2} ms  ({:.1}x)",
        before_server / after_worker
    );
}

// ---- the pending schedule window's index ---------------------------------------
//
// Three readers ask "what is due between now and an hour from now?". Before
// `run_state_scheduled` the planner seeked on `state_type` and walked every
// scheduled run, because the two existing state_type-leading indexes hold `id` and
// `start_time` but not `scheduled_time`. The index changes a plan and nothing
// else -- and that is a claim worth proving rather than asserting, so the tests
// below drop the index and compare.

/// Drop the window index, run `f`, put it back. Lets a test read the same rows
/// both ways, which is the only honest way to show an index changed no result.
///
/// Opens its own connection rather than going through `with_reader`, because a
/// pooled reader is read-only and cannot drop an index. The same pattern the
/// migration wind-back tests use.
fn without_window_index<T>(dir: &TempDir, f: impl FnOnce() -> T) -> T {
    let db = dir.path().join("db.sqlite");
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute_batch("DROP INDEX IF EXISTS run_state_scheduled")
        .unwrap();
    let out = f();
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute_batch("CREATE INDEX run_state_scheduled ON run (state_type, scheduled_time)")
        .unwrap();
    out
}

/// The plan for `sql`, compiled on a fresh connection.
///
/// Deliberately not `with_reader`: a pooled reader keeps the plan it compiled for
/// a given SQL text, so dropping an index behind its back goes unnoticed and the
/// test passes or fails depending on which connection it happened to get. That was
/// observed while writing this -- the first version of the plan test read the
/// index's own plan back as "no index" until an unrelated query forced the pooled
/// connection to re-prepare.
fn sqlite_master_index_count(db: &std::path::Path, name: &str) -> i64 {
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name = ?1",
        [name],
        |r| r.get(0),
    )
    .unwrap()
}

fn plan_on_a_fresh_connection(db: &std::path::Path, sql: &str) -> Vec<String> {
    let conn = rusqlite::Connection::open(db).unwrap();
    let mut st = conn
        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .unwrap();
    st.query_map([], |r| r.get::<_, String>(3))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
}

#[test]
fn the_window_index_exists() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let names: Vec<String> = store
        .with_reader(|c| {
            let mut st = c
                .prepare("SELECT name FROM sqlite_master WHERE type='index' AND tbl_name='run'")
                .unwrap();
            Ok(st
                .query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap())
        })
        .unwrap();
    assert!(
        names.iter().any(|n| n == "run_state_scheduled"),
        "the window index is created by migration 0022; found {names:?}"
    );
}

#[test]
fn the_window_read_is_a_range_seek_with_no_sort() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let s = schedule(&store, f, "hourly");
    for t in [1_000i64, 2_000, 3_000] {
        store
            .create_run_full(CreateRun {
                flow_id: f,
                name: format!("due-{t}"),
                parameters: "{}".into(),
                tags: "[]".into(),
                created_by: "schedule".into(),
                initial_state: Some(State::new(StateType::Scheduled)),
                schedule_id: Some(s),
                scheduled_time: Some(t),
                ..Default::default()
            })
            .unwrap();
    }

    let db = dir.path().join("db.sqlite");
    const WINDOW_SQL: &str = "SELECT r.id FROM run r WHERE r.state_type = 'Scheduled' \
         AND r.scheduled_time > 500 AND r.scheduled_time <= 4000 \
         ORDER BY r.scheduled_time, r.id LIMIT 100";

    let with = plan_on_a_fresh_connection(&db, WINDOW_SQL);
    let without = without_window_index(&dir, || {
        assert_eq!(
            sqlite_master_index_count(&db, "run_state_scheduled"),
            0,
            "the index must actually be gone before reading the plan without it"
        );
        plan_on_a_fresh_connection(&db, WINDOW_SQL)
    });
    let joined = |v: &[String]| v.join(" | ");

    assert!(
        joined(&with).contains("run_state_scheduled"),
        "the index serves the window read: {}",
        joined(&with)
    );
    assert!(
        joined(&with).contains("scheduled_time>"),
        "and it is a range seek on the window, not a scan: {}",
        joined(&with)
    );
    assert!(
        !joined(&with).to_uppercase().contains("TEMP B-TREE"),
        "and the ordering no longer needs a temporary sort: {}",
        joined(&with)
    );
    // The reason the sort mattered: without it, the LIMIT cannot stop the scan.
    assert!(
        joined(&without).to_uppercase().contains("TEMP B-TREE"),
        "without the index the read does sort, which is what the index removes: {}",
        joined(&without)
    );
}

/// The claim this whole change rests on: same rows, same order, same count, with
/// and without the index. Proven by dropping the index and re-reading.
#[test]
fn the_window_index_changes_no_result() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let s = schedule(&store, f, "hourly");

    // Scheduled at times on and beside both window edges, plus one running and one
    // finished, so the state filter is doing something.
    let at = |t: i64, st: StateType| -> i64 {
        store
            .create_run_full(CreateRun {
                flow_id: f,
                name: format!("t{t}"),
                parameters: "{}".into(),
                tags: "[]".into(),
                created_by: "schedule".into(),
                initial_state: Some(State::new(st)),
                schedule_id: Some(s),
                scheduled_time: Some(t),
                ..Default::default()
            })
            .unwrap()
            .0
    };
    for t in [499i64, 500, 1_000, 2_000, 5_000] {
        at(t, StateType::Scheduled);
    }
    let running = at(1_500, StateType::Scheduled);
    store
        .transition_run(running, State::new(StateType::Running), true)
        .unwrap();
    at(1_200, StateType::Completed);

    // A manually queued run: Scheduled, but with no schedule of its own. This is
    // the row that makes `run_schedule` -- a partial index over runs that have a
    // schedule -- unusable for this query, and the reason the new index cannot be
    // partial either. If this run went missing the window read would quietly stop
    // reporting manually queued work, which is the worst possible failure for a
    // queue view.
    let manual = store
        .create_run_full(CreateRun {
            flow_id: f,
            name: "manual".into(),
            parameters: "{}".into(),
            tags: "[]".into(),
            created_by: "api".into(),
            initial_state: Some(State::new(StateType::Scheduled)),
            schedule_id: None,
            scheduled_time: Some(2_500),
            ..Default::default()
        })
        .unwrap()
        .0;
    assert!(
        store
            .with_reader(|c| {
                Ok(c.query_row(
                    "SELECT schedule_id IS NULL FROM run WHERE id = ?1",
                    [manual],
                    |r| r.get::<_, bool>(0),
                )?)
            })
            .unwrap(),
        "the fixture row must really have no schedule, or it tests nothing"
    );

    for (after, until) in [(500i64, 5_000i64), (0, 100), (1_000, 1_000), (0, i64::MAX)] {
        let with = store
            .scheduled_timeline_run_rows(after, until, 50)
            .unwrap()
            .iter()
            .map(|r| (r.id, r.flow_name.clone(), r.flow_id, r.scheduled_time, r.created_at))
            .collect::<Vec<_>>();
        let without = without_window_index(&dir, || {
            store
                .scheduled_timeline_run_rows(after, until, 50)
                .unwrap()
                .iter()
                .map(|r| (r.id, r.flow_name.clone(), r.flow_id, r.scheduled_time, r.created_at))
                .collect::<Vec<_>>()
        });
        assert_eq!(
            with, without,
            "window ({after}, {until}]: the index changed the result"
        );
    }

    // The manual run is in the answer, with the index and without it. This is the
    // assertion that would fail if the index were made partial.
    let ids_with: Vec<i64> = store
        .scheduled_timeline_run_rows(500, 5_000, 50)
        .unwrap()
        .iter()
        .map(|r| r.id)
        .collect();
    let ids_without = without_window_index(&dir, || {
        store
            .scheduled_timeline_run_rows(500, 5_000, 50)
            .unwrap()
            .iter()
            .map(|r| r.id)
            .collect::<Vec<_>>()
    });
    assert!(ids_with.contains(&manual), "a manually queued run is offered");
    assert!(ids_without.contains(&manual), "and stays offered without the index");
    assert_eq!(ids_with, ids_without, "and the two agree");

    // The whole reader, and the queue projection, agree with each other either way.
    let wide_with: Vec<i64> = store
        .scheduled_between(500, 5_000, 50)
        .unwrap()
        .into_iter()
        .map(|r| r.id)
        .collect();
    let wide_without = without_window_index(&dir, || {
        store
            .scheduled_between(500, 5_000, 50)
            .unwrap()
            .into_iter()
            .map(|r| r.id)
            .collect::<Vec<_>>()
    });
    assert_eq!(wide_with, wide_without, "scheduled_between agrees either way");

    let queue_with: Vec<i64> = store
        .scheduled_queue_runs(500, 5_000, 50)
        .unwrap()
        .iter()
        .map(|r| r.id)
        .collect();
    let queue_without = without_window_index(&dir, || {
        store
            .scheduled_queue_runs(500, 5_000, 50)
            .unwrap()
            .iter()
            .map(|r| r.id)
            .collect::<Vec<_>>()
    });
    assert_eq!(queue_with, queue_without, "scheduled_queue_runs agrees either way");
    assert_eq!(wide_with, queue_with, "and the two readers agree with each other");
}

/// The index is on `run`, the fastest-growing table, so adding a fourth
/// `state_type`-leading index could plausibly have displaced an existing plan.
/// Captured for every other query over `run` and asserted to be unchanged.
#[test]
fn the_window_index_displaces_no_other_plan() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");

    let db = dir.path().join("db.sqlite");
    let plan_of = |_store: &Store, sql: &str| plan_on_a_fresh_connection(&db, sql);

    // One query per index on `run` that a live path uses.
    let queries: [(&str, &str); 6] = [
        ("state_type then id", "SELECT id FROM run WHERE state_type='Scheduled' ORDER BY id LIMIT 10"),
        ("state_type then start_time", "SELECT id FROM run WHERE state_type='Running' AND start_time > 0 ORDER BY start_time LIMIT 10"),
        ("a flow's runs", "SELECT id FROM run WHERE flow_id=1 ORDER BY id LIMIT 10"),
        ("a flow's scheduled runs", "SELECT id FROM run WHERE flow_id=1 AND state_type='Scheduled' LIMIT 10"),
        ("the newest run of a backfill", "SELECT id FROM run WHERE backfill_id=1 ORDER BY id DESC LIMIT 1"),
        ("a schedule's fires", "SELECT id FROM run WHERE schedule_id=1 AND scheduled_time > 0 ORDER BY scheduled_time LIMIT 10"),
    ];
    let _ = f;
    for (what, sql) in queries {
        let with = plan_of(&store, sql);
        let without = without_window_index(&dir, || plan_of(&store, sql));
        assert_eq!(
            with, without,
            "`{what}` changed plan: {} -> {}",
            with.join(" | "),
            without.join(" | ")
        );
    }
}

/// Opt-in measurement of the window read with and without its index.
///
/// The number that matters is not a ratio but a shape: before the index the read
/// cost a pass over the whole pending queue, and after it the cost is flat. So
/// this reports a column of queue sizes rather than one comparison, and it builds
/// each database by replaying the real migrations.
///
/// `CEREYAN_BENCH_REPORT=1 cargo test -p cereyan-store --test store report_scheduled_window -- --nocapture`
#[test]
fn report_scheduled_window() {
    if std::env::var("CEREYAN_BENCH_REPORT").is_err() {
        return;
    }
    const SP: i64 = 3_600_000; // spacing between scheduled times, microseconds
    const WINDOW: i64 = 3_600_000_000; // one hour
    const FLOWS: i64 = 500;

    /// A store with `n` pending scheduled runs, `n` finished ones and 500 flows.
    fn filled(n: i64) -> (TempDir, Store) {
        let dir = TempDir::new().unwrap();
        let store = open(&dir);
        for i in 1..=FLOWS {
            store
                .upsert_flow_full(cereyan_store::UpsertFlow {
                    project: format!("p{}", i % 20),
                    name: format!("flow_{i}"),
                    module: "m".into(),
                    source_dir: "/srv".into(),
                    description: None,
                    tags: "[]".into(),
                    parameter_schema: "{}".into(),
                    options: "{}".into(),
                    group: None,
                    ..Default::default()
                })
                .unwrap();
        }
        for i in 1..=n {
            for (state, at) in [
                (StateType::Scheduled, Some(i * SP)),
                (StateType::Completed, Some(i * SP)),
            ] {
                store
                    .create_run_full(CreateRun {
                        flow_id: (i % FLOWS) + 1,
                        name: format!("r{i}"),
                        parameters: "{}".into(),
                        tags: "[]".into(),
                        created_by: "bench".into(),
                        initial_state: Some(State::new(state)),
                        scheduled_time: at,
                        ..Default::default()
                    })
                    .unwrap();
            }
        }
        (dir, store)
    }

    /// Time the window read, holding the connection and the prepared statement
    /// open across iterations.
    ///
    /// The first version of this harness opened a fresh `rusqlite::Connection`
    /// inside the loop and reported ~340 us flat -- which is the cost of opening a
    /// connection and parsing the schema four times over, not the cost of the read.
    /// It flattened the very column the measurement exists to show. Holding both
    /// open is the difference between measuring the query and measuring SQLite's
    /// startup.
    fn time_window(db: &std::path::Path, after: i64, until: i64, iters: i64) -> f64 {
        let conn = rusqlite::Connection::open(db).unwrap();
        let mut st = conn
            .prepare(
                "SELECT r.id, f.name, r.flow_id, r.scheduled_time, r.created_at \
                 FROM run r JOIN flow f ON f.id = r.flow_id \
                 WHERE r.state_type = 'Scheduled' AND r.scheduled_time > ?1 \
                 AND r.scheduled_time <= ?2 ORDER BY r.scheduled_time, r.id LIMIT 100",
            )
            .unwrap();
        let mut once = || {
            st.query_map([after, until], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, Option<i64>>(3)?,
                    r.get::<_, i64>(4)?,
                ))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
        };
        for _ in 0..30 {
            once();
        }
        let t = Instant::now();
        for _ in 0..iters {
            std::hint::black_box(once());
        }
        t.elapsed().as_secs_f64() / iters as f64 * 1e6
    }

    /// How many runs the window actually returns, so the timings cannot be read as
    /// "fast because it returned nothing".
    fn rows_due(db: &std::path::Path, after: i64, until: i64) -> i64 {
        rusqlite::Connection::open(db)
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM run WHERE state_type='Scheduled' \
                 AND scheduled_time > ?1 AND scheduled_time <= ?2",
                [after, until],
                |r| r.get(0),
            )
            .unwrap()
    }

    println!(
        "pending schedule window, {FLOWS} flows, one-hour window at the far end of the queue\n\
         {:>9} {:>12} {:>12} {:>8} {:>10}",
        "scheduled", "no index", "with index", "ratio", "rows due"
    );
    for n in [1i64, 10, 25, 50, 100, 250, 500, 2_000, 10_000, 20_000] {
        let (dir, _store) = filled(n);
        // A one-hour window at the far end, so the read is asking for the tail of
        // the queue -- the case the queue page and the timeline actually hit.
        let (after, until) = (n * SP - WINDOW / 1000, n * SP);
        let db = dir.path().join("db.sqlite");

        let without = without_window_index(&dir, || time_window(&db, after, until, 300));
        let with = time_window(&db, after, until, 300);
        let due = rows_due(&db, after, until);
        assert!(due > 0, "the window must return something, or the timing is hollow");
        println!(
            "{:>9} {:>10.1}us {:>10.1}us {:>7.0}x {:>10}",
            n,
            without,
            with,
            without / with,
            due
        );
    }
    println!(
        "\nthe middle column is the finding: it rises by about 0.49 us per queued run\n\
         with no sign of flattening, and the third column does not move. There is no\n\
         crossover -- the index wins at every size from 1 pending run up, though at\n\
         that end the absolute saving is 1.7 us and the 2x ratio flatters it."
    );
}

// ---- per-flow window reads ----------------------------------------------------
//
// Three readers asked for "the newest N runs of each of these flows" with a window
// function whose subquery had no bound on the partition, so SQLite ranked every run
// of every listed flow before discarding them. They are now a seek per flow.
//
// The tests here pin the answers to the batched form's, including the two places
// where "the newest matching run" and "the newest run, if it matches" differ.

/// A store with `flows` flows, each holding `per_flow` runs, seeded directly.
///
/// Direct SQL because the run's `end_time` and `total_run_time` are derived by
/// `transition_run` from the clock, and this fixture needs to choose them: some
/// completed runs must have **no** end time, which is the case
/// `last_completed_at_many` must not fall through, and only some runs must carry a
/// duration, which is what `median_run_duration_many` samples.
///
/// One run in five is Completed, one completed run in twenty has no end time, and
/// every seventh run has a duration. All three readers therefore have something to
/// reject as well as something to return.
fn flows_with_history(dir: &TempDir, flows: i64, per_flow: i64) -> (Store, Vec<i64>) {
    let store = open(dir);
    let mut ids = Vec::new();
    for f in 1..=flows {
        ids.push(flow(&store, "p", &format!("flow{f}")));
    }
    let conn = rusqlite::Connection::open(store.home().join("db.sqlite")).unwrap();
    conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = OFF;")
        .unwrap();
    let tx = conn.unchecked_transaction().unwrap();
    {
        let mut stmt = tx
            .prepare(
                "INSERT INTO run (external_id, flow_id, name, state_type, state_name,
                                  created_at, scheduled_time, start_time, end_time,
                                  total_run_time)
                 VALUES (?1, ?2, ?3, ?4, ?4, ?5, ?5, ?6, ?7, ?8)",
            )
            .unwrap();
        for (f, id) in ids.iter().enumerate() {
            for k in 0..per_flow {
                let completed = k % 5 == 0;
                let state = if completed { "Completed" } else { "Scheduled" };
                let created = 1_000 + k;
                // Every twentieth completed run has no end time.
                let end = if completed && k % 20 != 0 {
                    Some(2_000 + k)
                } else {
                    None
                };
                // Every seventh run has a recorded duration.
                let dur = if k % 7 == 0 { Some(100 + k) } else { None };
                stmt.execute(rusqlite::params![
                    cereyan_core::new_id().as_bytes().as_slice(),
                    id,
                    format!("r{f}-{k}"),
                    state,
                    created,
                    created + 10,
                    end,
                    dur,
                ])
                .unwrap();
            }
        }
    }
    tx.commit().unwrap();
    (store, ids)
}

#[test]
fn the_recent_runs_of_several_flows_are_the_newest_of_each() {
    let dir = TempDir::new().unwrap();
    let (store, ids) = flows_with_history(&dir, 4, 30);

    for limit in [1usize, 3, 10, 40] {
        let many = store.recent_run_states_many(&ids, limit).unwrap();
        for id in &ids {
            let single = store.recent_run_states(*id, limit).unwrap();
            assert_eq!(
                many.get(id).map(|v| v.as_slice()),
                Some(single.as_slice()),
                "flow {id} at limit {limit}: the batched reader must agree with the \
                 per-flow one it now shares its statement with"
            );
            assert!(
                single.len() <= limit,
                "flow {id}: asked for {limit}, got {}",
                single.len()
            );
        }
    }
    // Newest first within a flow, which is what the callers index by.
    let many = store.recent_run_states_many(&ids, 10).unwrap();
    for (id, rows) in &many {
        for w in rows.windows(2) {
            assert!(w[0].0 > w[1].0, "flow {id}: newest first, got {w:?}");
        }
    }
    assert!(store.recent_run_states_many(&[], 10).unwrap().is_empty());
}

#[test]
fn the_newest_completed_run_of_several_flows_is_taken_per_flow() {
    let dir = TempDir::new().unwrap();
    let (store, ids) = flows_with_history(&dir, 5, 40);
    let many = store.last_completed_at_many(&ids).unwrap();

    for id in &ids {
        // What the batched form computed, restated independently: rank the flow's
        // completed runs by id, take the first, and keep it only if it has an end
        // time. Not a fallback to an older run.
        let all = store
            .list_runs(&ListRunsFilter {
                flow_id: Some(*id),
                state_type: Some("Completed".into()),
                sort: Some("created_desc".into()),
                limit: Some(200),
                ..Default::default()
            })
            .unwrap();
        let newest = all.items.first().map(|r| r.end_time);
        match newest {
            Some(Some(at)) => assert_eq!(
                many.get(id),
                Some(&at),
                "flow {id}: the newest completed run's end time"
            ),
            // The newest completed run has no end time, so the flow is absent --
            // it must NOT report an older run's end time instead.
            Some(None) => assert!(
                !many.contains_key(id),
                "flow {id}: the newest completed run has no end time, so the flow \
                 must be absent rather than falling back to an older one"
            ),
            None => assert!(!many.contains_key(id), "flow {id}: no completed run"),
        }
    }
    assert!(store.last_completed_at_many(&[]).unwrap().is_empty());
    assert!(
        store.last_completed_at_many(&[999_999]).unwrap().is_empty(),
        "a flow that does not exist is absent, not an error"
    );
}

#[test]
fn the_median_duration_of_several_flows_is_the_middle_of_their_sample() {
    let dir = TempDir::new().unwrap();
    let (store, ids) = flows_with_history(&dir, 4, 60);
    let many = store.median_run_duration_many(&ids).unwrap();

    for id in &ids {
        // The sample is the newest N runs that have a duration; the median is the
        // middle value of that sample once sorted.
        let all = store
            .list_runs(&ListRunsFilter {
                flow_id: Some(*id),
                sort: Some("created_desc".into()),
                limit: Some(500),
                ..Default::default()
            })
            .unwrap();
        let mut sample: Vec<i64> = all
            .items
            .iter()
            .filter_map(|r| r.total_run_time)
            .take(cereyan_store::Store::MEDIAN_SAMPLE)
            .collect();
        sample.sort_unstable();
        let expected = sample.get(sample.len() / 2).copied();
        assert_eq!(
            many.get(id).copied(),
            expected,
            "flow {id}: median of the newest {} durations",
            cereyan_store::Store::MEDIAN_SAMPLE
        );
    }
    assert!(store.median_run_duration_many(&[]).unwrap().is_empty());
    // A flow with no run at all has no median, rather than a median of zero.
    assert!(
        store.median_run_duration_many(&[999_999]).unwrap().is_empty(),
        "a flow with no runs has no median, not a median of zero"
    );
}

/// `last_completed_at_many` must not fall through to an older run.
///
/// The rule is "the newest **completed** run, and only if it has an end time" — not
/// "the newest completed run that has an end time". The second reading is the
/// natural one to write in SQL, by adding `AND end_time IS NOT NULL` to the filter,
/// and it is wrong: a flow whose most recent completion has no recorded end time
/// would report an *older* completion's timestamp, which is a plausible-looking lie.
///
/// Adding that clause passed every test in this file, because the shared fixture
/// never made the newest completed run the one without an end time — the two
/// conditions fell on different runs by arithmetic. So the case is built here
/// explicitly, with the ordering under the test's control rather than derived.
#[test]
fn a_newest_completed_run_without_an_end_time_is_not_replaced_by_an_older_one() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");

    let conn = rusqlite::Connection::open(store.home().join("db.sqlite")).unwrap();
    let tx = conn.unchecked_transaction().unwrap();
    {
        let mut stmt = tx
            .prepare(
                "INSERT INTO run (external_id, flow_id, name, state_type, state_name,
                                  created_at, scheduled_time, end_time)
                 VALUES (?1, ?2, ?3, 'Completed', 'Completed', ?4, ?4, ?5)",
            )
            .unwrap();
        // Three completed runs, oldest first by id. Only the **newest** lacks an end
        // time, so the two readings disagree: the fall-through one answers with the
        // middle run's end time.
        for (k, end) in [(1_000i64, Some(5_000i64)), (2_000, Some(6_000)), (3_000, None)] {
            stmt.execute(rusqlite::params![
                cereyan_core::new_id().as_bytes().as_slice(),
                f,
                format!("c{k}"),
                k,
                end,
            ])
            .unwrap();
        }
    }
    tx.commit().unwrap();

    let got = store.last_completed_at_many(&[f]).unwrap();
    assert!(
        !got.contains_key(&f),
        "the newest completed run has no end time, so the flow has no completion          time to report -- got {:?}, which came from an older run",
        got.get(&f)
    );

    // And with an end time on the newest, it is reported -- so the assertion above
    // is about the missing end time, not about the reader finding nothing.
    let conn = rusqlite::Connection::open(store.home().join("db.sqlite")).unwrap();
    conn.execute(
        // A subquery rather than `ORDER BY ... LIMIT` directly: SQLite only allows
        // that on UPDATE when built with SQLITE_ENABLE_UPDATE_DELETE_LIMIT.
        "UPDATE run SET end_time = 7000
         WHERE id = (SELECT id FROM run WHERE flow_id = ?1 ORDER BY id DESC LIMIT 1)",
        [f],
    )
    .unwrap();
    assert_eq!(
        store.last_completed_at_many(&[f]).unwrap().get(&f),
        Some(&7_000),
        "once the newest completed run has an end time, that is what is reported"
    );

    // A newer *scheduled* run must not hide the completed one either.
    let conn = rusqlite::Connection::open(store.home().join("db.sqlite")).unwrap();
    conn.execute(
        "INSERT INTO run (external_id, flow_id, name, state_type, state_name, created_at)
         VALUES (?1, ?2, 'later', 'Scheduled', 'Scheduled', 9000)",
        rusqlite::params![cereyan_core::new_id().as_bytes().as_slice(), f],
    )
    .unwrap();
    assert_eq!(
        store.last_completed_at_many(&[f]).unwrap().get(&f),
        Some(&7_000),
        "a newer run that is not completed does not change the answer"
    );
}

/// A reader that took the *newest* runs must not return a shorter list than asked
/// for just because the flow's history is long. The failure this catches is a
/// ranked form whose partition is bounded, which returns the oldest N instead of
/// the newest.
#[test]
fn a_long_history_does_not_truncate_the_sample() {
    let dir = TempDir::new().unwrap();
    let (store, ids) = flows_with_history(&dir, 2, 200);
    let many = store.recent_run_states_many(&ids, 10).unwrap();
    for id in &ids {
        assert_eq!(
            many.get(id).map(|v| v.len()),
            Some(10),
            "flow {id}: 200 runs of history, asked for the newest 10"
        );
    }
    // The newest run is the last one created, so its id is the highest.
    let highest = store
        .list_runs(&ListRunsFilter {
            flow_id: Some(ids[0]),
            sort: Some("created_desc".into()),
            limit: Some(1),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        many[&ids[0]][0].0,
        highest.items[0].id,
        "the first row is the newest run, not the oldest"
    );
}

/// Opt-in measurement of the three per-flow window readers, in the shape they had
/// before (one ranked pass over every flow's whole history) and now (a seek per
/// flow).
///
/// `CEREYAN_BENCH_REPORT=1 cargo test --release -p cereyan-store --test store report_per_flow_window -- --nocapture`
#[test]
fn report_per_flow_window() {
    if std::env::var("CEREYAN_BENCH_REPORT").is_err() {
        return;
    }
    const MEDIAN_SAMPLE: usize = 101;

    /// The batched form these readers replaced, verbatim, so the comparison is the
    /// real query and not a model of it.
    fn batched_recent(conn: &rusqlite::Connection, ids: &[i64], limit: i64) -> Vec<(i64, i64)> {
        let list = serde_json::to_string(ids).unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT flow_id, id FROM (
                     SELECT flow_id, id,
                            ROW_NUMBER() OVER (PARTITION BY flow_id ORDER BY id DESC) AS rn
                     FROM run
                     WHERE flow_id IN (SELECT CAST(value AS INTEGER) FROM json_each(?1)))
                 WHERE rn <= ?2 ORDER BY flow_id, id DESC",
            )
            .unwrap();
        stmt.query_map(rusqlite::params![list, limit], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
    }
    fn batched_completed(conn: &rusqlite::Connection, ids: &[i64]) -> Vec<(i64, i64)> {
        let list = serde_json::to_string(ids).unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT flow_id, end_time FROM (
                     SELECT flow_id, end_time,
                            ROW_NUMBER() OVER (PARTITION BY flow_id ORDER BY id DESC) AS rn
                     FROM run
                     WHERE flow_id IN (SELECT CAST(value AS INTEGER) FROM json_each(?1))
                       AND COALESCE(state_type, 'Completed') = 'Completed')
                 WHERE rn = 1",
            )
            .unwrap();
        stmt.query_map(rusqlite::params![list], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, Option<i64>>(1)?))
        })
        .unwrap()
        .filter_map(|r| r.ok())
        .filter_map(|(f, e)| e.map(|e| (f, e)))
        .collect()
    }

    println!(
        "{:>6} {:>10} | {:>11} {:>11} {:>7} | {:>11} {:>11} {:>7}",
        "flows", "runs/flow", "recent old", "recent new", "ratio", "completed old", "completed new", "ratio"
    );
    for (flows, per_flow) in [(10i64, 100i64), (10, 1_000), (10, 10_000), (50, 2_000), (200, 500)] {
        let dir = TempDir::new().unwrap();
        let (store, ids) = flows_with_history(&dir, flows, per_flow);
        let conn = rusqlite::Connection::open(store.home().join("db.sqlite")).unwrap();
        let iters = 20i64;

        fn bench(mut f: impl FnMut(), iters: i64) -> f64 {
            for _ in 0..3 {
                f();
            }
            let t = Instant::now();
            for _ in 0..iters {
                f();
            }
            t.elapsed().as_secs_f64() / iters as f64 * 1e3
        }

        // The two forms must agree before either is timed.
        let narrow_recent: Vec<(i64, i64)> = store
            .recent_run_states_many(&ids, 10)
            .unwrap()
            .into_iter()
            .flat_map(|(f, rows)| rows.into_iter().map(move |r| (f, r.0)))
            .collect();
        let wide_recent = batched_recent(&conn, &ids, 10);
        assert_eq!(
            narrow_recent.len(),
            wide_recent.len(),
            "the two forms returned different numbers of rows"
        );
        let mut a = narrow_recent.clone();
        a.sort_unstable();
        let mut b = wide_recent.clone();
        b.sort_unstable();
        assert_eq!(a, b, "the two forms returned different runs");

        let ro = bench(
            || {
                std::hint::black_box(batched_recent(&conn, &ids, 10));
            },
            iters,
        );
        let rn = bench(
            || {
                std::hint::black_box(store.recent_run_states_many(&ids, 10).unwrap());
            },
            iters,
        );
        let co = bench(
            || {
                std::hint::black_box(batched_completed(&conn, &ids));
            },
            iters,
        );
        let cn = bench(
            || {
                std::hint::black_box(store.last_completed_at_many(&ids).unwrap());
            },
            iters,
        );
        assert!(rn > 0.0 && cn > 0.0);
        println!(
            "{:>6} {:>10} | {:>9.3}ms {:>9.3}ms {:>6.0}x | {:>9.3}ms {:>9.3}ms {:>6.0}x",
            flows,
            per_flow,
            ro,
            rn,
            ro / rn,
            co,
            cn,
            co / cn
        );
    }
    println!(
        "\nBoth new columns are flat as the runs-per-flow column grows by 100x, and\n\
         both old columns grow with it. That is the whole finding: the old form's\n\
         cost was the history it ranked, not the rows it returned."
    );
}


/// Opt-in: what `checkpoints()` costs on a crash chain, and how much of it is the
/// chain walk against the checkpoint read.
///
/// `CEREYAN_BENCH_REPORT=1 cargo test --release -p cereyan-store --test store report_checkpoints -- --nocapture`
#[test]
fn report_checkpoints() {
    if std::env::var("CEREYAN_BENCH_REPORT").is_err() {
        return;
    }
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "p", "etl");
    let conn = rusqlite::Connection::open(store.home().join("db.sqlite")).unwrap();
    conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = OFF;").unwrap();

    println!(
        "{:>6} {:>7} {:>8} {:>10} {:>10} {:>10} {:>8}",
        "chain", "tasks", "passes", "scanned", "kept", "checkpoints", "per row"
    );
    for (chain, tasks, passes) in [
        (1usize, 50usize, 1usize),
        (1, 200, 1),
        (1, 200, 3),
        (3, 200, 3),
        (3, 200, 10),
        (10, 200, 10),
    ] {
        // A fresh chain each time, so one measurement is not polluted by the last.
        let base = store
            .with_reader(|c| Ok(c.query_row("SELECT COALESCE(MAX(id), 0) FROM run", [], |r| r.get::<_, i64>(0))?))
            .unwrap();
        let ids: Vec<i64> = {
            let tx = conn.unchecked_transaction().unwrap();
            let mut ids = Vec::new();
            let mut prev: Option<i64> = None;
            {
                let mut ins = tx
                    .prepare(
                        "INSERT INTO run (external_id, flow_id, name, parameters, tags,
                                          state_type, state_name, created_by, parent_run_id, created_at)
                         VALUES (?1, ?2, ?3, '{}', '[]', 'Crashed', 'Crashed', ?4, ?5, ?6)",
                    )
                    .unwrap();
                let mut tr = tx
                    .prepare(
                        "INSERT INTO task_run (external_id, run_id, name, task_key, dynamic_key,
                                               state_type, state_name, pass, result_ref, input_hash, created_at)
                         VALUES (?1, ?2, ?3, 'k', ?4, 'Completed', 'Completed', ?5, ?6, ?7, 1)",
                    )
                    .unwrap();
                for r in 0..chain {
                    let id = base + 1 + r as i64;
                    ins.execute(rusqlite::params![
                        cereyan_core::new_id().as_bytes().as_slice(),
                        f,
                        format!("chain{r}"),
                        if prev.is_none() { "api".to_string() } else { "crash:x".to_string() },
                        prev,
                        id,
                    ])
                    .unwrap();
                    for p in 0..passes {
                        for t in 0..tasks {
                            tr.execute(rusqlite::params![
                                cereyan_core::new_id().as_bytes().as_slice(),
                                id,
                                format!("task{t}"),
                                format!("dyn{t}"),
                                p as i64,
                                format!("res{id}-{p}-{t}"),
                                format!("in{id}-{p}-{t}"),
                            ])
                            .unwrap();
                        }
                    }
                    prev = Some(id);
                    ids.push(id);
                }
            }
            tx.commit().unwrap();
            ids
        };
        let latest = *ids.last().unwrap();
        let scanned = chain * tasks * passes;
        let out = store.checkpoints(latest).unwrap();
        let iters = 200i64;
        for _ in 0..20 {
            std::hint::black_box(store.checkpoints(latest).unwrap());
        }
        let t = Instant::now();
        for _ in 0..iters {
            std::hint::black_box(store.checkpoints(latest).unwrap());
        }
        let ms = t.elapsed().as_secs_f64() / iters as f64 * 1e3;
        println!(
            "{:>6} {:>7} {:>8} {:>10} {:>10} {:>8.3}ms {:>7.1}us",
            chain, tasks, passes, scanned, out.len(), ms,
            ms * 1000.0 / scanned as f64
        );
    }
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
    println!(
        "\nthe per-row column is the question: if it is flat, the cost is the scan,\n\
         which is what finding the latest checkpoint per key requires, and there is\n\
         nothing to win without changing the question."
    );
}

/// Each flow's newest run with the parameter value is found, and a flow with
/// none reads as `None`: the fan-in check depends on both.
#[test]
fn latest_run_with_param_many_finds_each_flows_newest_run() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let sales = flow(&store, "p", "sales");
    let inventory = flow(&store, "p", "inventory");
    let empty = flow(&store, "p", "empty");
    store.create_run(sales, "s1", r#"{"day":"2026-09-06"}"#, "[]").unwrap();
    let (s2, _) = store.create_run(sales, "s2", r#"{"day":"2026-09-06"}"#, "[]").unwrap();
    store.create_run(sales, "s3", r#"{"day":"2026-09-07"}"#, "[]").unwrap();
    let (i1, _) = store.create_run(inventory, "i1", r#"{"day":"2026-09-06"}"#, "[]").unwrap();
    let latest = store
        .latest_run_with_param_many(&[sales, inventory, empty], "day", "2026-09-06")
        .unwrap();
    assert_eq!(latest[&sales].as_ref().map(|m| m.id), Some(s2));
    assert_eq!(latest[&inventory].as_ref().map(|m| m.id), Some(i1));
    assert!(latest[&empty].is_none());
}
