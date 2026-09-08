use cereyan_core::{propose, Proposal, RunPolicy, State, StateType};
use criterion::{criterion_group, criterion_main, Criterion};

fn bench_propose(c: &mut Criterion) {
    let current = State::new(StateType::Running);
    let policy = RunPolicy::default();
    c.bench_function("propose running -> completed", |b| {
        b.iter(|| {
            let proposal = Proposal::new(State::new(StateType::Completed));
            std::hint::black_box(propose(Some(&current), &proposal, &policy))
        })
    });
}

criterion_group!(benches, bench_propose);
criterion_main!(benches);
