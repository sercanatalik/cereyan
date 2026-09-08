use cereyan_server::timer::{Timer, TimerEvent};
use criterion::{criterion_group, criterion_main, Criterion};

fn bench_timer(c: &mut Criterion) {
    c.bench_function("timer push+pop 10k", |b| {
        b.iter(|| {
            let t = Timer::new();
            for i in 0..10_000i64 {
                t.push(i, TimerEvent::Due(i));
            }
            std::hint::black_box(t.pop_due(10_000).len())
        })
    });
}

criterion_group!(benches, bench_timer);
criterion_main!(benches);
