//! M2 adversarial critic suite — isolated review of the spatial index against
//! the spec. Everything here attacks the CORE M2 acceptance criterion: the
//! uniform-grid `nearest_enemy` MUST return the byte-identical answer of
//! `brute_force_nearest_enemy`, including the deterministic smallest-index
//! tie-break, on adversarial layouts — and the grid must provably do less work.
//!
//! Public API only (`onus::sim::*`). Seeded, dependency-free RNG (the crate's
//! own `SplitMix64`) so every case is reproducible.

use bevy::math::Vec2;

use onus::sim::spatial::{
    brute_force_nearest_enemy, brute_force_nearest_enemy_counted, random_layout, SpatialGrid,
    SplitMix64, Unit,
};
use onus::sim::Faction;

fn unit(x: f32, y: f32, f: Faction) -> Unit {
    Unit {
        pos: Vec2::new(x, y),
        faction: f,
    }
}

/// The differential oracle, applied exhaustively: for EVERY unit the grid's
/// answer must equal brute force's, for a grid built from data-derived bounds,
/// from the given explicit bounds, AND across several cell sizes (the answer is
/// a geometric fact and must not depend on grid resolution).
fn assert_grid_eq_brute_all(units: &[Unit], min: Vec2, max: Vec2, cells: &[f32]) {
    for &cell in cells {
        let g_auto = SpatialGrid::build(units, cell);
        let g_fixed = SpatialGrid::build_with_bounds(units, cell, min, max);
        for i in 0..units.len() {
            let expected = brute_force_nearest_enemy(units, i);
            assert_eq!(
                g_auto.nearest_enemy(units, i),
                expected,
                "AUTO-bounds grid disagrees at unit {i} (cell={cell})"
            );
            assert_eq!(
                g_fixed.nearest_enemy(units, i),
                expected,
                "FIXED-bounds grid disagrees at unit {i} (cell={cell}, min={min:?}, max={max:?})"
            );
            // The counted variant must agree too (it backs the speedup proof).
            let (be, _) = brute_force_nearest_enemy_counted(units, i);
            let (ge, _) = g_auto.nearest_enemy_counted(units, i);
            assert_eq!(be, ge, "counted variant disagrees at unit {i} (cell={cell})");
        }
    }
}

/// Fixed-bounds-only differential check. For scenarios whose whole point is
/// out-of-bounds clamping, the units span a huge region, so an AUTO-bounds grid
/// with a small cell would allocate an absurd number of cells — irrelevant to
/// what's under test. Here we pin the tight explicit bounds and still demand the
/// grid match brute exactly across cell sizes.
fn assert_grid_fixed_eq_brute(units: &[Unit], min: Vec2, max: Vec2, cells: &[f32]) {
    for &cell in cells {
        let g = SpatialGrid::build_with_bounds(units, cell, min, max);
        for i in 0..units.len() {
            let expected = brute_force_nearest_enemy(units, i);
            assert_eq!(
                g.nearest_enemy(units, i),
                expected,
                "FIXED-bounds grid disagrees at unit {i} (cell={cell}, min={min:?}, max={max:?})"
            );
            let (be, _) = brute_force_nearest_enemy_counted(units, i);
            let (ge, _) = g.nearest_enemy_counted(units, i);
            assert_eq!(be, ge, "counted variant disagrees at unit {i} (cell={cell})");
        }
    }
}

/// Deterministic layout in an arbitrary box with a per-call seed.
fn layout_in(n: usize, seed: u64, min: Vec2, max: Vec2) -> Vec<Unit> {
    random_layout(n, seed, min, max)
}

// ---------------------------------------------------------------------------
// 1. Extreme / negative coordinates, many seeds, many cell sizes.
// ---------------------------------------------------------------------------

#[test]
fn differential_extreme_and_negative_coords() {
    // Sub-unit cell included (stresses ring expansion) but boxes are kept to a
    // modest extent so the *magnitude* of the coordinates is extreme without the
    // cell count exploding — the coordinate magnitude is what we're probing.
    let cells = [0.5f32, 3.0, 17.0, 250.0, 5000.0];
    for seed in 0..80u64 {
        let n = 30 + (seed as usize % 11) * 30; // 30..330
        // Wildly off-origin, negative / mixed-sign boxes, various magnitudes.
        let (min, max) = match seed % 4 {
            0 => (Vec2::new(-1_000_000.0, -1_000_000.0), Vec2::new(-999_700.0, -999_700.0)),
            1 => (Vec2::new(-50.0, -50.0), Vec2::new(50.0, 50.0)),
            2 => (Vec2::new(1e5, -1e5), Vec2::new(1e5 + 300.0, -1e5 + 300.0)),
            _ => (Vec2::new(-3.0, -3.0), Vec2::new(3.0, 3.0)),
        };
        let units = layout_in(n, seed.wrapping_mul(0x9E37_79B9) ^ 0xDEAD, min, max);
        assert_grid_eq_brute_all(&units, min, max, &cells);
    }
}

// ---------------------------------------------------------------------------
// 2. Coordinates that land EXACTLY on cell boundaries (floor/clamp edge).
// ---------------------------------------------------------------------------

#[test]
fn differential_units_on_exact_cell_boundaries() {
    // Grid origin at (0,0), cell size an exact power of two so `k * cell` is
    // representable exactly in f32; place units at exact lattice multiples so
    // each sits precisely on a cell corner/edge (floor is on a knife-edge).
    let cell = 4.0f32;
    let min = Vec2::new(0.0, 0.0);
    let max = Vec2::new(64.0, 64.0);
    let mut units = Vec::new();
    let mut rng = SplitMix64::new(0xB0341);
    for gx in 0..=16 {
        for gy in 0..=16 {
            let f = if rng.next_u64() & 1 == 0 { Faction::A } else { Faction::B };
            units.push(unit(gx as f32 * cell, gy as f32 * cell, f));
        }
    }
    assert_grid_eq_brute_all(&units, min, max, &[cell, cell / 2.0, cell * 2.0, 1.0, 7.0]);
}

// ---------------------------------------------------------------------------
// 3. Many units at the SAME point (zero-distance clusters), both factions.
// ---------------------------------------------------------------------------

#[test]
fn differential_many_units_same_point() {
    let mut units = Vec::new();
    let mut rng = SplitMix64::new(0x5A_5A);
    // 60 units all at the origin, random factions.
    for _ in 0..60 {
        let f = if rng.next_u64() & 1 == 0 { Faction::A } else { Faction::B };
        units.push(unit(0.0, 0.0, f));
    }
    // A second identical stack somewhere else, plus a lone far enemy.
    for _ in 0..40 {
        let f = if rng.next_u64() & 1 == 0 { Faction::A } else { Faction::B };
        units.push(unit(123.0, -77.0, f));
    }
    units.push(unit(150.0, 90.0, Faction::A)); // a lone far-ish enemy, still in a sane box
    assert_grid_eq_brute_all(
        &units,
        Vec2::new(-10.0, -100.0),
        Vec2::new(200.0, 100.0),
        &[1.0, 25.0, 100.0, 10000.0],
    );
}

// ---------------------------------------------------------------------------
// 4. Exactly-equidistant enemies: the tie-break MUST pick the smallest index,
//    and must not depend on grid cell size (which changes visitation order).
// ---------------------------------------------------------------------------

#[test]
fn tie_break_smallest_index_exact_equidistance() {
    // All these enemy offsets have squared distance EXACTLY 25 from the origin
    // (integers ⇒ exact in f32), spread across cells in every direction.
    let d5: &[(f32, f32)] = &[
        (5.0, 0.0),
        (0.0, 5.0),
        (-5.0, 0.0),
        (0.0, -5.0),
        (3.0, 4.0),
        (4.0, 3.0),
        (-3.0, 4.0),
        (-4.0, 3.0),
        (3.0, -4.0),
        (4.0, -3.0),
        (-3.0, -4.0),
        (-4.0, -3.0),
    ];
    let mut units = vec![unit(0.0, 0.0, Faction::A)]; // index 0 = query
    // Interleave friendlies (same faction, ignored) and equidistant enemies so
    // the smallest-index enemy is not simply index 1.
    for (k, &(x, y)) in d5.iter().enumerate() {
        // a nearer-looking friendly (must be ignored by faction filter)
        units.push(unit(x * 0.1, y * 0.1, Faction::A));
        units.push(unit(x, y, Faction::B));
        let _ = k;
    }
    // brute is the oracle for the exact expected index.
    let expected = brute_force_nearest_enemy(&units, 0);
    assert!(expected.is_some());
    // Across many cell sizes (each changes ring/cell visitation order) the grid
    // must return that SAME smallest index.
    for cell in [0.5f32, 1.0, 2.0, 2.5, 5.0, 10.0, 1000.0] {
        let g = SpatialGrid::build(&units, cell);
        assert_eq!(
            g.nearest_enemy(&units, 0),
            expected,
            "tie-break drifted with cell={cell}"
        );
        let gf = SpatialGrid::build_with_bounds(
            &units,
            cell,
            Vec2::new(-5.0, -5.0),
            Vec2::new(5.0, 5.0),
        );
        assert_eq!(gf.nearest_enemy(&units, 0), expected, "fixed-bounds tie-break drift cell={cell}");
    }
}

/// Randomised equidistance stress: rings of enemies all at the same radius, many
/// seeds and cell sizes, verifying the grid never disagrees with brute on which
/// (smallest) index wins.
#[test]
fn tie_break_random_equidistant_rings() {
    for seed in 0..40u64 {
        let mut rng = SplitMix64::new(seed.wrapping_add(1));
        let mut units = vec![unit(0.0, 0.0, Faction::A)];
        // pick an integer radius whose square is exact; scatter enemies at that
        // radius using pythagorean-ish integer points and axis points.
        let pts: [(f32, f32); 8] = [
            (5.0, 0.0),
            (4.0, 3.0),
            (3.0, 4.0),
            (0.0, 5.0),
            (-4.0, 3.0),
            (-5.0, 0.0),
            (0.0, -5.0),
            (3.0, -4.0),
        ];
        // random count of equidistant enemies + random friendlies mixed in
        let m = 2 + (rng.next_u64() % 6) as usize;
        for i in 0..m {
            units.push(unit(pts[i % pts.len()].0, pts[i % pts.len()].1, Faction::B));
            if rng.next_u64() & 1 == 0 {
                units.push(unit(0.1, 0.1, Faction::A)); // ignored friendly
            }
        }
        let expected = brute_force_nearest_enemy(&units, 0);
        for cell in [1.0f32, 2.0, 2.5, 5.0, 6.0] {
            let g = SpatialGrid::build(&units, cell);
            assert_eq!(g.nearest_enemy(&units, 0), expected, "seed={seed} cell={cell}");
        }
    }
}

// ---------------------------------------------------------------------------
// 5. Units far OUTSIDE the explicit grid bounds (heavy clamping into edge
//    cells) — the stop rule must stay sound with a large ring count.
// ---------------------------------------------------------------------------

#[test]
fn differential_units_far_outside_tight_bounds() {
    for seed in 0..30u64 {
        // A multi-ring grid so a broken stop rule under clamping has room to
        // show. Bounds tight; units spread far beyond ⇒ heavy edge-cell clamping.
        let min = Vec2::new(-1000.0, -1000.0);
        let max = Vec2::new(1000.0, 1000.0);
        let n = 60 + (seed as usize % 5) * 40;
        // Place most units far outside the box on all sides.
        let mut rng = SplitMix64::new(seed ^ 0xF00D);
        let mut units = Vec::with_capacity(n);
        for _ in 0..n {
            let far = 30_000.0;
            let x = rng.range_f32(-far, far);
            let y = rng.range_f32(-far, far);
            let f = if rng.next_u64() & 1 == 0 { Faction::A } else { Faction::B };
            units.push(unit(x, y, f));
        }
        // 50x50 and 20x20 grids ⇒ almost everything clamps to edge cells.
        assert_grid_fixed_eq_brute(&units, min, max, &[40.0, 100.0]);
    }
}

/// Mixed inside/outside: query points both inside and outside the bounds, close
/// real enemies inside, decoy enemies far outside.
#[test]
fn differential_mixed_inside_outside() {
    let min = Vec2::new(-10.0, -10.0);
    let max = Vec2::new(10.0, 10.0);
    let units = [
        unit(0.0, 0.0, Faction::A),
        unit(2.0, 0.0, Faction::B),
        unit(-40000.0, 0.0, Faction::B),
        unit(0.0, 55000.0, Faction::A),
        unit(9.5, -9.5, Faction::B),
        unit(-9.5, 9.5, Faction::A),
        unit(70000.0, 70000.0, Faction::B),
        unit(1.0, 1.0, Faction::A),
    ];
    assert_grid_fixed_eq_brute(&units, min, max, &[1.0, 4.0, 20.0]);
}

// ---------------------------------------------------------------------------
// 6. A single enemy hidden among many friendlies.
// ---------------------------------------------------------------------------

#[test]
fn differential_single_enemy_among_friendlies() {
    for seed in 0..30u64 {
        let mut rng = SplitMix64::new(seed.wrapping_mul(7) + 3);
        let mut units = Vec::new();
        let n = 300;
        for _ in 0..n {
            let x = rng.range_f32(-500.0, 500.0);
            let y = rng.range_f32(-500.0, 500.0);
            units.push(unit(x, y, Faction::A)); // all friendly
        }
        // exactly one enemy, placed at a random spot
        let ex = rng.range_f32(-500.0, 500.0);
        let ey = rng.range_f32(-500.0, 500.0);
        units.push(unit(ex, ey, Faction::B));
        // Every A's nearest enemy must be that single B (index n); the B has none.
        let g = SpatialGrid::build(&units, 25.0);
        for i in 0..n {
            assert_eq!(g.nearest_enemy(&units, i), Some(n), "seed={seed} unit {i}");
            assert_eq!(brute_force_nearest_enemy(&units, i), Some(n));
        }
        // The lone B is surrounded by 300 A enemies: its nearest enemy is the
        // nearest A, and the grid must agree with brute on which one.
        assert_eq!(
            g.nearest_enemy(&units, n),
            brute_force_nearest_enemy(&units, n),
            "lone enemy's nearest (an A) must match brute (seed={seed})"
        );
        assert!(g.nearest_enemy(&units, n).is_some(), "the lone B has 300 enemies");
    }
}

// ---------------------------------------------------------------------------
// 7. Tight clusters far apart.
// ---------------------------------------------------------------------------

#[test]
fn differential_tight_clusters() {
    for seed in 0..30u64 {
        let mut rng = SplitMix64::new(seed + 100);
        let mut units = Vec::new();
        let centers = [
            Vec2::new(-800.0, -800.0),
            Vec2::new(800.0, 800.0),
            Vec2::new(-800.0, 800.0),
            Vec2::new(0.0, 0.0),
        ];
        for c in centers {
            for _ in 0..50 {
                let x = c.x + rng.range_f32(-0.5, 0.5); // extremely tight
                let y = c.y + rng.range_f32(-0.5, 0.5);
                let f = if rng.next_u64() & 1 == 0 { Faction::A } else { Faction::B };
                units.push(unit(x, y, f));
            }
        }
        assert_grid_eq_brute_all(
            &units,
            Vec2::new(-1000.0, -1000.0),
            Vec2::new(1000.0, 1000.0),
            &[5.0, 100.0],
        );
    }
}

// ---------------------------------------------------------------------------
// 8. Large N differential.
// ---------------------------------------------------------------------------

#[test]
fn differential_large_n() {
    let min = Vec2::new(-2000.0, -2000.0);
    let max = Vec2::new(2000.0, 2000.0);
    for seed in [1u64, 7, 4242] {
        let units = layout_in(5000, seed, min, max);
        // one representative cell size (dense grid)
        let cell = 40.0;
        let g = SpatialGrid::build(&units, cell);
        let gf = SpatialGrid::build_with_bounds(&units, cell, min, max);
        for i in 0..units.len() {
            let expected = brute_force_nearest_enemy(&units, i);
            assert_eq!(g.nearest_enemy(&units, i), expected, "auto N=5000 seed={seed} i={i}");
            assert_eq!(gf.nearest_enemy(&units, i), expected, "fixed N=5000 seed={seed} i={i}");
        }
    }
}

// ---------------------------------------------------------------------------
// 9. Degenerate edge cases.
// ---------------------------------------------------------------------------

#[test]
fn edge_empty_build_does_not_panic() {
    // No query is defined on an empty set, but building must be valid.
    let _g = SpatialGrid::build(&[], 10.0);
    let _g2 = SpatialGrid::build_with_bounds(&[], 10.0, Vec2::ZERO, Vec2::ZERO);
}

#[test]
fn edge_single_unit_none() {
    let units = [unit(3.0, -4.0, Faction::A)];
    for cell in [0.1f32, 1.0, 1000.0] {
        assert_eq!(SpatialGrid::build(&units, cell).nearest_enemy(&units, 0), None);
    }
}

#[test]
fn edge_all_one_faction_none() {
    let units = [
        unit(0.0, 0.0, Faction::B),
        unit(10.0, 0.0, Faction::B),
        unit(-33.0, 91.0, Faction::B),
        unit(500.0, -400.0, Faction::B),
    ];
    let g = SpatialGrid::build(&units, 7.0);
    for i in 0..units.len() {
        assert_eq!(g.nearest_enemy(&units, i), None, "all-one-faction must be None at {i}");
    }
}

#[test]
fn edge_duplicate_positions_opposite_factions() {
    // Two units at the exact same point, opposite factions ⇒ distance 0, each is
    // the other's nearest.
    let units = [
        unit(42.0, 42.0, Faction::A),
        unit(42.0, 42.0, Faction::B),
        unit(100.0, 100.0, Faction::A),
    ];
    let g = SpatialGrid::build(&units, 3.0);
    assert_eq!(g.nearest_enemy(&units, 0), Some(1));
    assert_eq!(g.nearest_enemy(&units, 1), Some(0));
    assert_eq!(brute_force_nearest_enemy(&units, 0), g.nearest_enemy(&units, 0));
    assert_eq!(brute_force_nearest_enemy(&units, 1), g.nearest_enemy(&units, 1));
}

// ---------------------------------------------------------------------------
// 10. Determinism: result independent of insertion / iteration order.
//     Under any permutation of the units, grid must equal brute (the smallest-
//     index tie-break is faithfully reproduced regardless of bucket order).
// ---------------------------------------------------------------------------

#[test]
fn determinism_independent_of_insertion_order() {
    let base = layout_in(400, 0xABCDEF, Vec2::new(-300.0, -300.0), Vec2::new(300.0, 300.0));
    // Add deliberate exact-duplicate and equidistant structure so tie-breaks bite.
    let mut units = base.clone();
    units.push(unit(0.0, 0.0, Faction::A));
    units.push(unit(5.0, 0.0, Faction::B));
    units.push(unit(0.0, 5.0, Faction::B));
    units.push(unit(-5.0, 0.0, Faction::B));

    let mut rng = SplitMix64::new(0x1234_5678);
    for _perm in 0..20 {
        // Fisher-Yates shuffle (deterministic).
        let mut p = units.clone();
        for i in (1..p.len()).rev() {
            let j = (rng.next_u64() % (i as u64 + 1)) as usize;
            p.swap(i, j);
        }
        let g = SpatialGrid::build(&p, 30.0);
        for i in 0..p.len() {
            assert_eq!(
                g.nearest_enemy(&p, i),
                brute_force_nearest_enemy(&p, i),
                "grid != brute on a permutation at unit {i}"
            );
        }
    }
}

#[test]
fn determinism_repeated_build_identical() {
    let units = layout_in(500, 999, Vec2::new(-200.0, -200.0), Vec2::new(200.0, 200.0));
    let g1 = SpatialGrid::build(&units, 15.0);
    let g2 = SpatialGrid::build(&units, 15.0);
    for i in 0..units.len() {
        assert_eq!(g1.nearest_enemy(&units, i), g2.nearest_enemy(&units, i));
        // and the work count is reproducible too
        assert_eq!(
            g1.nearest_enemy_counted(&units, i).1,
            g2.nearest_enemy_counted(&units, i).1
        );
    }
}

// ---------------------------------------------------------------------------
// 11. Speedup MECHANISM, proven with the deterministic work counter (not
//     wall-clock): the grid must evaluate far fewer distances than brute, and
//     its work must grow much more slowly than brute's as N doubles.
// ---------------------------------------------------------------------------

fn total_work(units: &[Unit], cell: f32) -> (usize, usize) {
    let grid = SpatialGrid::build(units, cell);
    let mut brute = 0usize;
    let mut g = 0usize;
    for i in 0..units.len() {
        let (be, bw) = brute_force_nearest_enemy_counted(units, i);
        let (ge, gw) = grid.nearest_enemy_counted(units, i);
        assert_eq!(be, ge, "counted answers disagree at {i}");
        brute += bw;
        g += gw;
    }
    (brute, g)
}

#[test]
fn speedup_grid_does_far_less_work() {
    // A dense uniform field, cell ≈ one unit/cell.
    let min = Vec2::new(-1000.0, -1000.0);
    let max = Vec2::new(1000.0, 1000.0);
    let n = 3000usize;
    let area = (max.x - min.x) * (max.y - min.y);
    let cell = (area / n as f32).sqrt();
    let units = layout_in(n, 0x1CE, min, max);
    let (brute, grid) = total_work(&units, cell);
    assert!(
        grid.saturating_mul(15) < brute,
        "grid must evaluate <1/15 of brute's distances (grid={grid}, brute={brute})"
    );
}

#[test]
fn speedup_work_grows_sublinearly_vs_brute() {
    // Fixed density: as N doubles, brute total work ~4x (O(n^2) total), grid
    // total work should stay ~linear (~2x). Assert the grid's growth ratio is
    // dramatically smaller than brute's — the asymptotic mechanism, deterministic.
    let min = Vec2::new(-1000.0, -1000.0);
    let max = Vec2::new(1000.0, 1000.0);
    let area = (max.x - min.x) * (max.y - min.y);

    let n1 = 1500usize;
    let n2 = 3000usize;
    let cell1 = (area / n1 as f32).sqrt();
    let cell2 = (area / n2 as f32).sqrt();

    let (brute1, grid1) = total_work(&layout_in(n1, 0xBEEF, min, max), cell1);
    let (brute2, grid2) = total_work(&layout_in(n2, 0xBEEF, min, max), cell2);

    let brute_ratio = brute2 as f64 / brute1 as f64;
    let grid_ratio = grid2 as f64 / grid1 as f64;

    // Brute must show near-quadratic total growth (well above 3x for a 2x N).
    assert!(brute_ratio > 3.0, "brute work should grow ~quadratically (ratio={brute_ratio:.2})");
    // Grid growth must be far tamer than brute's.
    assert!(
        grid_ratio < brute_ratio * 0.6,
        "grid work must grow much slower than brute (grid_ratio={grid_ratio:.2}, brute_ratio={brute_ratio:.2})"
    );
}
