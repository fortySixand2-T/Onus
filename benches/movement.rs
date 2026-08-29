//! L4 performance bench scaffold (see TESTING.md). Proves the criterion harness
//! works; real perf work starts at M2 (naive vs. spatial-grid nearest-enemy).

use bevy::math::Vec2;
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use onus::sim::step_toward;

fn bench_step_toward(c: &mut Criterion) {
    c.bench_function("step_toward", |b| {
        b.iter(|| {
            step_toward(
                black_box(Vec2::ZERO),
                black_box(Vec2::new(100.0, 50.0)),
                black_box(3.0),
            )
        });
    });
}

criterion_group!(benches, bench_step_toward);
criterion_main!(benches);
