use cereyan_store::{
    ArmExpectation, CreateBackfill, NewEvent, NewLog, ResetScope, RuleWrite, ScheduleWrite, Store,
    UpsertArtifact,
};
use tempfile::TempDir;

fn open(dir: &TempDir) -> Store {
    Store::open(dir.path()).expect("open store")
}

fn flow(store: &Store, project: &str, name: &str) -> i64 {
    store
        .upsert_flow(project, name, "pipeline", "/tmp/proj", None, "[]", "{}")
        .unwrap()
}

fn count(store: &Store, sql: &str) -> i64 {
    store
        .with_reader(|c| Ok(c.query_row(sql, [], |r| r.get(0))?))
        .unwrap()
}

/// A run with logs, a task run, an artifact, an event, an expectation and a
/// stored answer.
fn busy_run(store: &Store, flow_id: i64, rule_id: i64, logs: usize) -> i64 {
    let (run, _) = store.create_run(flow_id, "r", "{}", "[]").unwrap();
    store.create_task_run(run, "t", "t", "t-0", 0).unwrap();
    let lines = (0..logs)
        .map(|i| NewLog {
            run_id: run,
            task_run_id: None,
            task_run_external_id: None,
            level: 20,
            logger: "test".into(),
            timestamp: i as i64,
            message: format!("line {i}"),
        })
        .collect();
    store.append_logs(lines).unwrap();
    store
        .upsert_artifact(UpsertArtifact {
            run_id: run,
            task_run_id: None,
            kind: "markdown".into(),
            key: None,
            data: "done".into(),
            external_id: None,
        })
        .unwrap();
    store
        .append_event(NewEvent {
            name: "run.completed".into(),
            run_id: Some(run),
            ..Default::default()
        })
        .unwrap();
    store
        .arm_expectation(ArmExpectation {
            rule_id,
            key: format!("k{run}"),
            run_id: Some(run),
            flow_id: None,
            armed_at: 0,
            deadline: 1,
        })
        .unwrap();
    store.record_firing(rule_id, None, Some(run), "[]").unwrap();
    store.kv_set(&format!("run.input:{run}"), "yes").unwrap();
    run
}

fn rule(store: &Store, name: &str, source: &str) -> i64 {
    store
        .upsert_rule(RuleWrite {
            id: None,
            name: name.into(),
            enabled: true,
            source: source.into(),
            module: None,
            spec: "{}".into(),
        })
        .unwrap()
}

fn schedule(store: &Store, flow_id: i64, source: &str) -> i64 {
    store
        .upsert_schedule(ScheduleWrite {
            id: None,
            flow_id,
            spec: "{}".into(),
            catchup: "skip".into(),
            catchup_max: 1,
            catchup_window: None,
            jitter: 0,
            start_deadline: None,
            active: true,
            source: source.into(),
            code_key: None,
            persist: false,
        })
        .unwrap()
}

#[test]
fn an_event_about_a_run_names_its_flow() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "etl", "daily");
    let (run, _) = store.create_run(f, "r", "{}", "[]").unwrap();
    let (event, _) = store
        .append_event(NewEvent {
            name: "run.completed".into(),
            run_id: Some(run),
            ..Default::default()
        })
        .unwrap();
    let flow_id = count(
        &store,
        &format!("SELECT flow_id FROM event WHERE id = {event}"),
    );
    assert_eq!(flow_id, f);
}

#[test]
fn removing_flows_leaves_nothing_behind_and_keeps_other_projects() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let alert = rule(&store, "alert-etl", "ui");
    let a = flow(&store, "etl", "a");
    let b = flow(&store, "etl", "b");
    let keep = flow(&store, "warehouse", "load");
    let run_a = busy_run(&store, a, alert, 12);
    busy_run(&store, b, alert, 3);
    let kept_run = busy_run(&store, keep, alert, 2);
    schedule(&store, a, "code");
    store
        .create_backfill(CreateBackfill {
            flow_id: b,
            parameter: "day".into(),
            start_value: "2026-09-01".into(),
            end_value: "2026-09-02".into(),
            interval_secs: 86_400.0,
            concurrency: 1,
            total: 2,
            extra_parameters: "{}".into(),
        })
        .unwrap();
    store.set_variable("region", "eu", "[]", false).unwrap();

    let preview = store.project_counts("etl").unwrap();
    assert_eq!(
        (
            preview.flows,
            preview.runs,
            preview.schedules,
            preview.backfills,
            preview.events
        ),
        (2, 2, 1, 1, 2)
    );
    assert_eq!(preview.active_runs, 0);

    let deleted = store.delete_flows_batched(&[a, b], 5).unwrap();
    assert_eq!(
        (deleted.flows, deleted.runs, deleted.logs, deleted.events),
        (2, 2, 15, 2)
    );
    assert_eq!(
        (
            deleted.task_runs,
            deleted.artifacts,
            deleted.schedules,
            deleted.backfills
        ),
        (2, 2, 1, 1)
    );

    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM flow WHERE project = 'etl'"),
        0
    );
    assert_eq!(count(&store, "SELECT COUNT(*) FROM log"), 2);
    assert_eq!(
        count(
            &store,
            &format!("SELECT COUNT(*) FROM event WHERE run_id != {kept_run}")
        ),
        0
    );
    assert_eq!(
        count(
            &store,
            &format!("SELECT COUNT(*) FROM expectation WHERE run_id != {kept_run}")
        ),
        0
    );
    assert_eq!(
        count(
            &store,
            &format!("SELECT COUNT(*) FROM kv WHERE key = 'run.input:{run_a}'")
        ),
        0
    );
    assert_eq!(
        count(
            &store,
            &format!("SELECT COUNT(*) FROM kv WHERE key = 'run.input:{kept_run}'")
        ),
        1
    );
    assert_eq!(count(&store, "SELECT COUNT(*) FROM rule"), 1);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM variable"), 1);
    assert_eq!(store.list_projects().unwrap().len(), 1);
}

#[test]
fn projects_group_flows_with_their_last_run() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let a = flow(&store, "etl", "a");
    let b = flow(&store, "etl", "b");
    let c = flow(&store, "warehouse", "load");
    let projects = store.list_projects().unwrap();
    assert_eq!(projects.len(), 2);
    assert_eq!(
        (projects[0].name.as_str(), projects[0].flow_ids.clone()),
        ("etl", vec![a, b])
    );
    assert_eq!(
        (projects[1].name.as_str(), projects[1].flow_ids.clone()),
        ("warehouse", vec![c])
    );
    assert_eq!(projects[0].last_run_at, None);
}

#[test]
fn backup_is_a_copy_that_opens_with_the_old_rows() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "etl", "daily");
    store.create_run(f, "one", "{}", "[]").unwrap();
    store.create_run(f, "two", "{}", "[]").unwrap();
    let path = store.backup().unwrap();
    assert!(path.starts_with(dir.path().join("backups")));
    store.reset(ResetScope::History, &[f]).unwrap();
    let copy = rusqlite::Connection::open(&path).unwrap();
    let runs: i64 = copy
        .query_row("SELECT COUNT(*) FROM run", [], |r| r.get(0))
        .unwrap();
    assert_eq!(runs, 2);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM run"), 0);
}

#[test]
fn history_reset_keeps_definitions_and_settings() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let alert = rule(&store, "alert", "ui");
    let f = flow(&store, "etl", "daily");
    busy_run(&store, f, alert, 4);
    schedule(&store, f, "code");
    schedule(&store, f, "ui");
    store.set_variable("region", "eu", "[]", false).unwrap();
    store.kv_set("settings.retain_days", "7").unwrap();
    store.kv_set("scheduler.last_wakeup", "1").unwrap();

    let deleted = store.reset(ResetScope::History, &[f]).unwrap();
    assert_eq!(
        (deleted.runs, deleted.logs, deleted.events, deleted.flows),
        (1, 4, 1, 0)
    );
    for table in [
        "run",
        "task_run",
        "log",
        "event",
        "artifact",
        "rule_firing",
        "expectation",
    ] {
        assert_eq!(
            count(&store, &format!("SELECT COUNT(*) FROM {table}")),
            0,
            "{table}"
        );
    }
    assert_eq!(count(&store, "SELECT COUNT(*) FROM flow"), 1);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM schedule"), 2);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM variable"), 1);
    assert_eq!(count(&store, "SELECT fire_count FROM rule"), 0);
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) FROM kv WHERE key LIKE 'run.input:%'"
        ),
        0
    );
    assert_eq!(count(&store, "SELECT COUNT(*) FROM kv WHERE key IN ('settings.retain_days', 'scheduler.last_wakeup')"), 2);
}

#[test]
fn everything_reset_keeps_only_what_code_registered() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let live = flow(&store, "pipelines", "daily");
    let stale = flow(&store, "etl", "old");
    let code_rule = rule(&store, "from-code", "code");
    let ui_rule = rule(&store, "from-ui", "ui");
    busy_run(&store, stale, ui_rule, 2);
    let code_schedule = schedule(&store, live, "code");
    schedule(&store, live, "ui");
    schedule(&store, stale, "code");
    store.set_variable("region", "eu", "[]", false).unwrap();
    store
        .kv_set(&format!("rules.clock_last:{code_rule}"), "1")
        .unwrap();
    store
        .kv_set(&format!("rules.clock_last:{ui_rule}"), "1")
        .unwrap();
    store.kv_set("settings.crash_retries", "2").unwrap();

    let deleted = store.reset(ResetScope::Everything, &[live]).unwrap();
    assert_eq!(
        (
            deleted.flows,
            deleted.schedules,
            deleted.rules,
            deleted.variables
        ),
        (1, 2, 1, 1)
    );
    assert_eq!(count(&store, "SELECT id FROM flow"), live);
    assert_eq!(count(&store, "SELECT id FROM schedule"), code_schedule);
    assert_eq!(count(&store, "SELECT id FROM rule"), code_rule);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM variable"), 0);
    assert_eq!(
        count(
            &store,
            &format!("SELECT COUNT(*) FROM kv WHERE key = 'rules.clock_last:{code_rule}'")
        ),
        1
    );
    assert_eq!(
        count(
            &store,
            &format!("SELECT COUNT(*) FROM kv WHERE key = 'rules.clock_last:{ui_rule}'")
        ),
        0
    );
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) FROM kv WHERE key = 'settings.crash_retries'"
        ),
        1
    );
}

#[test]
fn table_counts_split_code_and_ui_definitions() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store, "etl", "daily");
    rule(&store, "from-code", "code");
    rule(&store, "from-ui", "ui");
    schedule(&store, f, "code");
    schedule(&store, f, "ui");
    store.set_variable("region", "eu", "[]", false).unwrap();
    let counts = store.table_counts().unwrap();
    assert_eq!(
        (
            counts.ui_rules,
            counts.ui_schedules,
            counts.variables,
            counts.runs
        ),
        (1, 1, 1, 0)
    );
}
