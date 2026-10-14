//! The effect journal: flow-body effects recorded with their slot and hash, in
//! the same write as the effect, and read back through a crash chain.

use cereyan_core::{State, StateType};
use cereyan_store::{
    effect_seed_key, CreateRun, EffectMark, NewEvent, ReportEvent, Store, UpsertArtifact,
};
use serde_json::json;
use tempfile::TempDir;

fn open(dir: &TempDir) -> Store {
    Store::open(dir.path()).expect("open store")
}

fn flow(store: &Store) -> i64 {
    store
        .upsert_flow("p", "etl", "pipeline", "/tmp/proj", None, "[]", "{}")
        .unwrap()
}

fn mark(slot: &str, hash: &str, pass: i64) -> Option<EffectMark> {
    Some(EffectMark {
        slot: slot.into(),
        hash: hash.into(),
        pass,
    })
}

fn rerun(store: &Store, flow_id: i64, parent: i64, created_by: String) -> i64 {
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

fn event(run: i64) -> NewEvent {
    NewEvent {
        name: "orders.ready".into(),
        run_id: Some(run),
        payload: json!({"day": "d"}),
        ..Default::default()
    }
}

fn slots(store: &Store, run: i64) -> Vec<(String, String, i64)> {
    store
        .effects(run)
        .unwrap()
        .into_iter()
        .map(|r| (r.slot, r.hash, r.run_id))
        .collect()
}

#[test]
fn every_effect_is_journaled_with_its_write() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store);
    let (run, _) = store.create_run(f, "r", "{}", "[]").unwrap();
    store
        .append_event_with_effect(event(run), mark("event:orders.ready-0", "h1", 0))
        .unwrap();
    store
        .task_state_set(run, "", "progress", "1", mark("state:progress-0", "h2", 0))
        .unwrap();
    store
        .merge_run_attributes(run, r#"{"stage":"a"}"#, mark("attributes:stage-0", "h3", 0))
        .unwrap();
    let aid = store
        .upsert_artifact(UpsertArtifact {
            run_id: run,
            kind: "markdown".into(),
            data: "\"x\"".into(),
            effect: mark("artifact:~markdown-0", "h4", 0),
            ..Default::default()
        })
        .unwrap();
    // Writes without a mark journal nothing.
    store.append_event(event(run)).unwrap();
    store.task_state_set(run, "", "other", "1", None).unwrap();

    let rows = store.effects(run).unwrap();
    assert_eq!(
        rows.iter().map(|r| r.slot.as_str()).collect::<Vec<_>>(),
        [
            "artifact:~markdown-0",
            "attributes:stage-0",
            "event:orders.ready-0",
            "state:progress-0"
        ]
    );
    let artifact = rows
        .iter()
        .find(|r| r.slot.starts_with("artifact:"))
        .unwrap();
    assert_eq!(artifact.r#ref.as_deref(), Some(aid.to_string().as_str()));
}

#[test]
fn a_report_journals_its_marked_events_and_artifacts() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store);
    let (run, _) = store.create_run(f, "r", "{}", "[]").unwrap();
    let ext = cereyan_core::new_id();
    store
        .apply_report(
            run,
            vec![
                ReportEvent::Custom {
                    seq: 1,
                    name: "orders.ready".into(),
                    payload: json!({}),
                    task_run_external_id: None,
                    effect: mark("event:orders.ready-0", "h1", 1),
                },
                ReportEvent::Custom {
                    seq: 2,
                    name: "unmarked".into(),
                    payload: json!({}),
                    task_run_external_id: None,
                    effect: None,
                },
                ReportEvent::Artifact {
                    seq: 3,
                    external_id: ext,
                    task_run_external_id: None,
                    artifact_kind: "markdown".into(),
                    key: None,
                    data: json!("x"),
                    effect: mark("artifact:~markdown-0", "h2", 1),
                },
            ],
        )
        .unwrap();
    let rows = store.effects(run).unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|r| r.pass == 1));
    let artifact = rows
        .iter()
        .find(|r| r.slot.starts_with("artifact:"))
        .unwrap();
    assert_eq!(artifact.r#ref, Some(ext.to_string()));
    // A report from an older engine, without the field, still applies.
    let old: ReportEvent =
        serde_json::from_value(json!({"kind": "custom", "seq": 4, "name": "legacy"})).unwrap();
    assert_eq!(store.apply_report(run, vec![old]).unwrap().applied, 1);
}

#[test]
fn a_slot_is_replaced_and_the_chain_merges_nearest_last() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store);
    let (crashed, _) = store.create_run(f, "r", "{}", "[]").unwrap();
    store
        .append_event_with_effect(event(crashed), mark("event:orders.ready-0", "old", 0))
        .unwrap();
    store
        .append_event_with_effect(event(crashed), mark("event:orders.ready-0", "new", 1))
        .unwrap();
    store
        .task_state_set(
            crashed,
            "",
            "progress",
            "1",
            mark("state:progress-0", "p1", 1),
        )
        .unwrap();
    let again = rerun(&store, f, crashed, format!("crash:{crashed}"));
    store
        .task_state_set(
            again,
            "",
            "progress",
            "2",
            mark("state:progress-0", "p2", 0),
        )
        .unwrap();
    assert_eq!(
        slots(&store, again),
        [
            (
                "event:orders.ready-0".to_string(),
                "new".to_string(),
                crashed
            ),
            ("state:progress-0".to_string(), "p2".to_string(), again),
        ]
    );
    // A retry is not a crash link: it sees only its seed and its own entries.
    let retry = rerun(&store, f, again, format!("retry:{again}"));
    assert!(slots(&store, retry).is_empty());
    store
        .kv_set(
            &effect_seed_key(retry),
            &serde_json::to_string(&store.effects(again).unwrap()).unwrap(),
        )
        .unwrap();
    store
        .task_state_set(
            retry,
            "",
            "progress",
            "3",
            mark("state:progress-0", "p3", 0),
        )
        .unwrap();
    let merged = slots(&store, retry);
    assert_eq!(merged[0].1, "new");
    assert_eq!(
        merged[1],
        ("state:progress-0".to_string(), "p3".to_string(), retry)
    );
    // Rows go with their run.
    store.delete_run(retry).unwrap();
    assert_eq!(
        store
            .with_reader(|c| Ok(c.query_row(
                "SELECT COUNT(*) FROM effect WHERE run_id = ?1",
                [retry],
                |r| r.get::<_, i64>(0)
            )?))
            .unwrap(),
        0
    );
}

#[test]
fn the_state_listing_reads_through_the_chain() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir);
    let f = flow(&store);
    let (crashed, _) = store.create_run(f, "r", "{}", "[]").unwrap();
    store
        .task_state_set(crashed, "", "progress", "1", None)
        .unwrap();
    store
        .task_state_set(crashed, "t-0", "job", "\"j\"", None)
        .unwrap();
    let again = rerun(&store, f, crashed, format!("crash:{crashed}"));
    store
        .task_state_set(again, "", "progress", "2", None)
        .unwrap();
    let rows: Vec<(String, String, serde_json::Value)> = store
        .task_state_list(again)
        .unwrap()
        .into_iter()
        .map(|r| (r.scope, r.key, r.value))
        .collect();
    assert_eq!(
        rows,
        [
            (String::new(), "progress".to_string(), json!(2)),
            ("t-0".to_string(), "job".to_string(), json!("j")),
        ]
    );
    // An unrelated run lists nothing of the chain.
    let (other, _) = store.create_run(f, "o", "{}", "[]").unwrap();
    assert!(store.task_state_list(other).unwrap().is_empty());
}
