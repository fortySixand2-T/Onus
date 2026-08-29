//! L4 nearest-enemy benches (see BUILD_PLAN.md / TESTING). This is where M2's
//! timing is *logged* — never inside the sim. It measures the full "nearest
//! enemy for every unit" pass, the realistic workload, so the naive O(n²) scan
//! and the spatial-grid pass are compared apples-to-apples on identical layouts.

use bevy::math::Vec2;
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use onus::sim::spatial::{brute_force_nearest_enemy, random_layout, Unit};

/// A deterministic layout for a given unit count (fixed seed, fixed box).
fn layout(n: usize) -> Vec<Unit> {
    random_layout(n, 0xA11CE, Vec2::new(-1000.0, -1000.0), Vec2::new(1000.0, 1000.0))
}

/// Naive: nearest enemy for every unit, brute-force O(n²).
fn naive_all(units: &[Unit]) {
    for i in 0..units.len() {
        black_box(brute_force_nearest_enemy(black_box(units), i));
    }
}

fn bench_naive(c: &mut Criterion) {
    let mut group = c.benchmark_group("nearest_enemy_all");
    for &n in &[1000usize, 2000] {
        let units = layout(n);
        group.bench_with_input(BenchmarkId::new("naive", n), &units, |b, u| {
            b.iter(|| naive_all(u));
        });
    }
    group.finish();
}

criterion_group!(benches, bench_naive);
criterion_main!(benches);
