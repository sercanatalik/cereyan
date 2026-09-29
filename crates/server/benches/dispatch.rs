//! `take_work` with 10k queued runs across four modules, half of them blocked
//! by a backfill's concurrency, so every poll walks past blocked runs to the
//! first one that can start.
use cereyan_server::supervisor::{EngineKey, QueuedRun, Supervisor, WorkDecision};
use cereyan_server::ServeConfig;
use criterion::{criterion_group, criterion_main, Criterion};

fn key(module: &str) -> EngineKey {
    EngineKey {
        source_dir: "/nonexistent".into(),
        module: module.into(),
        isolated: false,
        nice: 0,
    }
}

fn bench_take_work(c: &mut Criterion) {
    let config: ServeConfig = serde_json::from_value(serde_json::json!({
        "home": std::env::temp_dir(),
        "max_engines": 4,
    }))
    .unwrap();
    let sup = Supervisor::new(&config);
    sup.set_total("backfill:1", 0.0);
    let modules = ["etl", "ml", "reports", "mail"];
    sup.enqueue_many(
        (0..10_000i64)
            .map(|i| QueuedRun {
                run_id: i,
                key: key(modules[(i % 4) as usize]),
                priority: 0,
                order: i,
                // The first half waits on a backfill with no free slot.
                needs: if i < 5_000 {
                    vec![("backfill:1".into(), 1.0)]
                } else {
                    Vec::new()
                },
                not_before: None,
                flow_id: 1,
                remote_ok: true,
                prefer_worker: None,
                prefer_until: 0,
            })
            .collect(),
    );
    let k = key("etl");
    let mut next = 10_000i64;
    c.bench_function("take_work 10k queued, 4 modules, 5k blocked", |b| {
        b.iter(|| {
            let decision = sup.take_work("bench", 1, &k, "");
            if let WorkDecision::Run(id) = decision {
                sup.run_finished(id);
                // Keep the queue at 10k.
                sup.enqueue(QueuedRun {
                    run_id: next,
                    key: key("etl"),
                    priority: 0,
                    order: next,
                    needs: Vec::new(),
                    not_before: None,
                    flow_id: 1,
                    remote_ok: true,
                    prefer_worker: None,
                    prefer_until: 0,
                });
                next += 1;
            }
        })
    });
}

/// A worker's engine polling against 10k queued runs of which a third may go
/// remote and half of those are for flows whose code the worker lacks: the walk
/// passes over what it may not take.
fn bench_remote_take_work(c: &mut Criterion) {
    let config: ServeConfig = serde_json::from_value(serde_json::json!({
        "home": std::env::temp_dir(),
        "max_engines": 4,
    }))
    .unwrap();
    let sup = Supervisor::new(&config);
    sup.sync_worker(7, "w7", 4, "online", [1i64].into_iter().collect());
    sup.sync_worker(8, "w8", 4, "online", [2i64].into_iter().collect());
    sup.enqueue_many(
        (0..10_000i64)
            .map(|i| QueuedRun {
                run_id: i,
                key: key("etl"),
                priority: 0,
                order: i,
                needs: Vec::new(),
                not_before: None,
                flow_id: if i % 2 == 0 { 2 } else { 1 },
                remote_ok: i % 3 == 0,
                prefer_worker: None,
                prefer_until: 0,
            })
            .collect(),
    );
    let k = key("etl");
    let mut next = 10_000i64;
    c.bench_function("remote take_work 10k queued, 3 locations", |b| {
        b.iter(|| {
            if let WorkDecision::Run(id) = sup.take_work("w7-1", 1, &k, "") {
                sup.run_finished(id);
                sup.enqueue(QueuedRun {
                    run_id: next,
                    key: key("etl"),
                    priority: 0,
                    order: next,
                    needs: Vec::new(),
                    not_before: None,
                    flow_id: 1,
                    remote_ok: true,
                    prefer_worker: None,
                    prefer_until: 0,
                });
                next += 1;
            }
        })
    });
}

criterion_group!(benches, bench_take_work, bench_remote_take_work);
criterion_main!(benches);
