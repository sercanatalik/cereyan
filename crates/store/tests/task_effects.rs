//! Task effects: the lineage a call's effect key derives from, and the `once`
//! calls an interrupted run left in doubt.

use cereyan_core::{State, StateType};
use cereyan_store::{lineage_key, CreateRun, CreateTaskRun, Store};
use tempfile::TempDir;

fn open(dir: &TempDir) -> Store {
    Store::open(dir.path()).expect("open store")
}

fn flow(store: &Store) -> i64 {
    store
        .upsert_flow("p", "pay", "pipeline", "/tmp/proj", None, "[]", "{}")
        .unwrap()
}

fn child(store: &Store, flow_id: i64, parent: i64, created_by: String) -> i64 {
    store
        .create_run_full(CreateRun {
            flow_id,
            name: "again".into(),
            created_by,
            initial_state: Some(State::new(StateType::Scheduled)),
            parent_run_id: Some(parent),
            attempt: 1,
            ..Default::default()
        })
        .unwrap()
        .0
}

/// A task run of `key`, taken through `states`.
fn task(store: &Store, run: i64, key: &str, once: bool, states: &[StateType]) {
    let (id, _) = store
        .create_task_run_full(CreateTaskRun {
            run_id: run,
            name: key.split('-').next().unwrap().into(),
            task_key: "m.charge".into(),
            dynamic_key: key.into(),
            external_id: None,
            parents: Vec::new(),
            pass: 0,
            once,
        })
        .unwrap();
    for s in states {
        store
            .transition_task_run(id, State::new(*s), false)
            .unwrap();
    }
}

fn root(store: &Store, run: i64) -> i64 {
    store.lineage(run).unwrap().0
}

#[test]
fn the_lineage_follows_crash_links_and_seeded_retries_only() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store);
    let (first, _) = store.create_run(f, "r", "{}", "[]").unwrap();
    let rerun = child(&store, f, first, format!("crash:{first}"));
    assert_eq!(root(&store, first), first);
    assert_eq!(root(&store, rerun), first);
    // A retry from failure records the lineage it continues.
    let retry = child(&store, f, rerun, format!("retry:{rerun}"));
    store
        .kv_set(
            &lineage_key(retry),
            &format!(r#"{{"root": {first}, "from": {rerun}}}"#),
        )
        .unwrap();
    assert_eq!(root(&store, retry), first);
    let retry_rerun = child(&store, f, retry, format!("crash:{retry}"));
    assert_eq!(root(&store, retry_rerun), first);
    // A retry from the start does not: it is a lineage of its own.
    let fresh = child(&store, f, rerun, format!("retry:{rerun}"));
    assert_eq!(root(&store, fresh), fresh);
    // The external id is the root's.
    let (_, ext) = store.lineage(rerun).unwrap();
    assert_eq!(ext, store.get_run(first).unwrap().unwrap().external_id);
}

#[test]
fn only_started_unfinished_once_calls_are_in_doubt() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store);
    let (crashed, _) = store.create_run(f, "r", "{}", "[]").unwrap();
    use StateType::*;
    task(&store, crashed, "charge-0", true, &[Pending, Running]);
    task(&store, crashed, "plain-0", false, &[Pending, Running]);
    task(&store, crashed, "never-0", true, &[]);
    task(
        &store,
        crashed,
        "done-0",
        true,
        &[Pending, Running, Completed],
    );
    task(&store, crashed, "later-0", true, &[Pending, Running]);
    let rerun = child(&store, f, crashed, format!("crash:{crashed}"));
    // The rerun completed `later-0` before crashing again.
    task(
        &store,
        rerun,
        "later-0",
        true,
        &[Pending, Running, Completed],
    );
    let third = child(&store, f, rerun, format!("crash:{rerun}"));
    assert_eq!(store.in_doubt(third).unwrap(), ["charge-0"]);
    // A run is never in doubt about its own task runs.
    assert!(store.in_doubt(crashed).unwrap().is_empty());
    // Through a retry from failure, too.
    let retry = child(&store, f, third, format!("retry:{third}"));
    store
        .kv_set(
            &lineage_key(retry),
            &format!(r#"{{"root": {crashed}, "from": {third}}}"#),
        )
        .unwrap();
    assert_eq!(store.in_doubt(retry).unwrap(), ["charge-0"]);
    // The task run JSON says once.
    let rows = store.task_runs_by_run(crashed, None).unwrap();
    let charge = rows.iter().find(|t| t.dynamic_key == "charge-0").unwrap();
    let plain = rows.iter().find(|t| t.dynamic_key == "plain-0").unwrap();
    assert!(charge.once && !plain.once);
}
