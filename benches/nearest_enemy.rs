//! L4 nearest-enemy benches (see BUILD_PLAN.md / TESTING). This is where M2's
//! timing is *logged* — never inside the sim. It measures the full "nearest
//! enemy for every unit" pass, the realistic workload, so the naive O(n²) scan
//! and the spatial-grid pass are compared apples-to-apples on identical layouts.

use bevy::math::Vec2;
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use onus::sim::spatial::{brute_force_nearest_enemy, random_layout, SpatialGrid, Unit};

const MIN: Vec2 = Vec2::new(-1000.0, -1000.0);
const MAX: Vec2 = Vec2::new(1000.0, 1000.0);

/// A deterministic layout for a given unit count (fixed seed, fixed box).
fn layout(n: usize) -> Vec<Unit> {
    random_layout(n, 0xA11CE, MIN, MAX)
}

/// Cell size ≈ one unit per cell for the box/count (good grid density).
fn cell_size_for(n: usize) -> f32 {
    let area = (MAX.x - MIN.x) * (MAX.y - MIN.y);
    (area / n as f32).sqrt()
}

/// Naive: nearest enemy for every unit, brute-force O(n²).
fn naive_all(units: &[Unit]) {
    for i in 0..units.len() {
        black_box(brute_force_nearest_enemy(black_box(units), i));
    }
}

/// Grid: build the index, then nearest enemy for every unit (end-to-end, so the
/// naive pass — which has no build step — is compared honestly).
fn grid_all(units: &[Unit], cell: f32) {
    let grid = SpatialGrid::build(units, cell);
    for i in 0..units.len() {
        black_box(grid.nearest_enemy(black_box(units), i));
    }
}

fn bench_nearest_enemy(c: &mut Criterion) {
    let mut group = c.benchmark_group("nearest_enemy_all");
    for &n in &[1000usize, 2000] {
        let units = layout(n);
        let cell = cell_size_for(n);
        group.bench_with_input(BenchmarkId::new("naive", n), &units, |b, u| {
            b.iter(|| naive_all(u));
        });
        group.bench_with_input(BenchmarkId::new("grid", n), &units, |b, u| {
            b.iter(|| grid_all(u, cell));
        });
    }
    group.finish();
}

criterion_group!(benches, bench_nearest_enemy);
criterion_main!(benches);
