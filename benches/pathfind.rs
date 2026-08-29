//! L4 pathfinding bench (see BUILD_PLAN.md / TESTING). This is where M3's group-
//! move timing is *logged* — never inside the sim. It measures the realistic
//! group workload: many units on one obstacle field, all ordered to a single
//! destination, solved two ways on the byte-identical map —
//!   - `n_astar`: one A* search per unit (N independent searches), vs
//!   - `flow_field`: one BFS from the goal that every unit then follows.
//!
//! The deterministic node-expansion counts behind the F-002 decision live in the
//! test `group_flow_field_costs_far_less_than_n_times_astar`; this bench is the
//! wall-clock corroboration.

use bevy::math::Vec2;
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use onus::sim::pathfind::{astar, FlowField, TileGrid};

const CELL: f32 = 10.0;
const ORIGIN: Vec2 = Vec2::ZERO;

/// A deterministic 48×48 obstacle field (20% blocked) and a far-corner goal —
/// the same setup the F-002 work-count test uses.
fn scenario() -> (TileGrid, usize, Vec<usize>) {
    let grid = TileGrid::random(48, 48, CELL, ORIGIN, 0.2, 0xC0FFEE);
    let goal = (grid.idx(47, 47)..grid.len())
        .chain(0..grid.idx(47, 47))
        .find(|&c| grid.is_walkable(c))
        .unwrap();
    let field = FlowField::compute(&grid, goal);
    let starts: Vec<usize> = (0..grid.len())
        .filter(|&c| c != goal && field.is_reachable(c))
        .collect();
    (grid, goal, starts)
}

fn bench_group_move(c: &mut Criterion) {
    let (grid, goal, starts) = scenario();
    let mut group = c.benchmark_group("group_move_to_one_dest");

    // N independent A* searches (the thing the flow field replaces).
    group.bench_function("n_astar", |b| {
        b.iter(|| {
            for &s in &starts {
                black_box(astar(black_box(&grid), s, goal));
            }
        });
    });

    // One flow field the whole group follows.
    group.bench_function("flow_field", |b| {
        b.iter(|| black_box(FlowField::compute(black_box(&grid), goal)));
    });

    group.finish();
}

criterion_group!(benches, bench_group_move);
criterion_main!(benches);
