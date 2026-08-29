//! Spatial index for the sim core (M2). **Render-free** — ECS `Component` +
//! `math` + `std` only; no `Color`/`Sprite`/`Transform`/`Window`. Everything
//! here is a pure, deterministic function of its inputs, so it is fully
//! headless-testable and safe to differential-test against brute force.
//!
//! The problem M2 solves: given many units split across two factions, find each
//! unit's nearest *enemy* (nearest unit of the other faction). The naive pass is
//! O(n²); [`SpatialGrid`] buckets units into a uniform grid and answers the same
//! query by expanding cell rings, giving the identical answer far faster.
//!
//! Determinism: ties (equal squared distance) always resolve to the smallest
//! unit index, independent of grid bucket iteration order. No wall-clock lives
//! here — all timing/measurement happens in `benches/`.

use bevy::ecs::prelude::Component;
use bevy::math::Vec2;

// ---- factions --------------------------------------------------------------

/// Which side a unit fights for. For M2 the "two sides" are a mirror nation
/// (see MVP_PLAN.md) — this is just the sim-pure tag that makes one unit an
/// *enemy* of another. Unit *types* don't matter at this milestone.
#[derive(Component, Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Faction {
    A,
    B,
}

/// A unit as the spatial index sees it: a position and a side. Plain data,
/// shared by both the brute-force and grid nearest-enemy passes so they operate
/// on byte-identical input (a prerequisite for the differential oracle).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Unit {
    pub pos: Vec2,
    pub faction: Faction,
}

// ---- deterministic RNG -----------------------------------------------------

/// A tiny dependency-free splitmix64 generator. Seeded and reproducible: the
/// same seed always yields the same stream, so layouts used in tests and benches
/// are byte-for-byte repeatable without pulling in the `rand` crate.
#[derive(Clone)]
pub struct SplitMix64(u64);

impl SplitMix64 {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// Next raw 64-bit value (advances the state).
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform `f32` in `[0, 1)` (24 bits of mantissa precision).
    pub fn next_f32(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }

    /// Uniform `f32` in `[lo, hi)`.
    pub fn range_f32(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.next_f32()
    }
}

/// Build a deterministic layout of `n` units, uniformly placed in the
/// `[min, max)` box and split (≈50/50, by a fair coin) across the two factions.
/// Same `(n, seed, min, max)` ⇒ identical layout. This is the M2 spawn substrate.
pub fn random_layout(n: usize, seed: u64, min: Vec2, max: Vec2) -> Vec<Unit> {
    let mut rng = SplitMix64::new(seed);
    (0..n)
        .map(|_| {
            let pos = Vec2::new(rng.range_f32(min.x, max.x), rng.range_f32(min.y, max.y));
            let faction = if rng.next_u64() & 1 == 0 {
                Faction::A
            } else {
                Faction::B
            };
            Unit { pos, faction }
        })
        .collect()
}

// ---- nearest-enemy: brute force (the naive O(n²) pass) ---------------------

/// Nearest enemy of unit `i` by the naive scan: check every other unit, keep the
/// nearest of the opposite faction. Returns its index, or `None` if unit `i` has
/// no enemy (e.g. a single unit, or all units share a faction).
///
/// Ties (equal squared distance) resolve to the **smallest index**, so the
/// result is fully deterministic and a valid oracle for the grid.
pub fn brute_force_nearest_enemy(units: &[Unit], i: usize) -> Option<usize> {
    brute_force_nearest_enemy_counted(units, i).0
}

/// As [`brute_force_nearest_enemy`], but also returns the number of enemy
/// distance evaluations performed — the naive pass's *work*, used to prove the
/// grid's speedup deterministically (alongside the wall-clock bench).
pub fn brute_force_nearest_enemy_counted(units: &[Unit], i: usize) -> (Option<usize>, usize) {
    let me = units[i];
    let mut best: Option<(f32, usize)> = None;
    let mut work = 0usize;
    for (j, u) in units.iter().enumerate() {
        if u.faction == me.faction {
            continue;
        }
        work += 1;
        let d2 = me.pos.distance_squared(u.pos);
        // Replace only on a strictly smaller distance; equal distances keep the
        // earlier (smaller) index, giving the deterministic tie-break.
        if best.is_none_or(|(bd2, _)| d2 < bd2) {
            best = Some((d2, j));
        }
    }
    (best.map(|(_, j)| j), work)
}

// ---- nearest-enemy: uniform spatial grid -----------------------------------

/// A uniform grid over the world that buckets units into square cells, so a
/// nearest-enemy query only inspects nearby cells instead of every unit.
///
/// Correctness (identical answer to [`brute_force_nearest_enemy`]): the query
/// scans cells in expanding Chebyshev rings around the query cell. Any cell at
/// ring `k ≥ 1` is at least `(k−1)·cell_size` away, so once a best enemy is
/// found we can stop as soon as the next ring's guaranteed minimum distance
/// **strictly exceeds** the best distance — nothing unexamined can beat *or tie*
/// it, so ties still resolve to the smallest index exactly as brute force does.
/// Units outside the bounds are clamped into edge cells; since their true
/// distance only *exceeds* the cell bound, the same stop rule stays valid.
pub struct SpatialGrid {
    cell_size: f32,
    min: Vec2,
    cols: usize,
    rows: usize,
    /// `cells[row * cols + col]` = indices of the units in that cell.
    cells: Vec<Vec<usize>>,
}

impl SpatialGrid {
    /// Build a grid whose bounds are the units' own extent. Empty input yields a
    /// valid 1×1 empty grid.
    pub fn build(units: &[Unit], cell_size: f32) -> Self {
        let mut min = Vec2::splat(f32::INFINITY);
        let mut max = Vec2::splat(f32::NEG_INFINITY);
        for u in units {
            min = min.min(u.pos);
            max = max.max(u.pos);
        }
        if units.is_empty() {
            min = Vec2::ZERO;
            max = Vec2::ZERO;
        }
        Self::build_with_bounds(units, cell_size, min, max)
    }

    /// Build a grid over an explicit `[min, max]` box. Units outside the box are
    /// clamped into the nearest edge cell (still answered correctly).
    pub fn build_with_bounds(units: &[Unit], cell_size: f32, min: Vec2, max: Vec2) -> Self {
        let cell_size = if cell_size > 0.0 { cell_size } else { 1.0 };
        let extent = (max - min).max(Vec2::ZERO);
        let cols = (extent.x / cell_size).floor() as usize + 1;
        let rows = (extent.y / cell_size).floor() as usize + 1;
        let mut grid = Self {
            cell_size,
            min,
            cols,
            rows,
            cells: vec![Vec::new(); cols * rows],
        };
        for (i, u) in units.iter().enumerate() {
            let (c, r) = grid.cell_of(u.pos);
            grid.cells[r * cols + c].push(i);
        }
        grid
    }

    /// Cell (col, row) containing `p`, clamped to the grid. Float→int casts
    /// saturate in Rust, and the clamp then folds any out-of-bounds point onto
    /// the nearest edge cell.
    fn cell_of(&self, p: Vec2) -> (usize, usize) {
        let fx = ((p.x - self.min.x) / self.cell_size).floor() as isize;
        let fy = ((p.y - self.min.y) / self.cell_size).floor() as isize;
        let col = fx.clamp(0, self.cols as isize - 1) as usize;
        let row = fy.clamp(0, self.rows as isize - 1) as usize;
        (col, row)
    }

    /// Nearest enemy of unit `i`, identical to [`brute_force_nearest_enemy`].
    pub fn nearest_enemy(&self, units: &[Unit], i: usize) -> Option<usize> {
        self.nearest_enemy_counted(units, i).0
    }

    /// As [`Self::nearest_enemy`], but also returns the number of enemy distance
    /// evaluations — the grid's *work*, for the deterministic speedup proof.
    pub fn nearest_enemy_counted(&self, units: &[Unit], i: usize) -> (Option<usize>, usize) {
        let me = units[i];
        let (c0, r0) = self.cell_of(me.pos);
        let max_ring = c0
            .max(self.cols - 1 - c0)
            .max(r0.max(self.rows - 1 - r0));

        let mut best_d2 = f32::INFINITY;
        let mut best_idx: Option<usize> = None;
        let mut work = 0usize;
        let mut r = 0usize;
        loop {
            // Scan the border cells at Chebyshev distance exactly `r`.
            let row_lo = r0.saturating_sub(r);
            let row_hi = (r0 + r).min(self.rows - 1);
            let col_lo = c0.saturating_sub(r);
            let col_hi = (c0 + r).min(self.cols - 1);
            for row in row_lo..=row_hi {
                for col in col_lo..=col_hi {
                    let cheby = (col as isize - c0 as isize)
                        .unsigned_abs()
                        .max((row as isize - r0 as isize).unsigned_abs());
                    if cheby != r {
                        continue; // interior already scanned in an earlier ring
                    }
                    for &j in &self.cells[row * self.cols + col] {
                        if units[j].faction == me.faction {
                            continue;
                        }
                        work += 1;
                        let d2 = me.pos.distance_squared(units[j].pos);
                        let better = match best_idx {
                            None => true,
                            Some(b) => d2 < best_d2 || (d2 == best_d2 && j < b),
                        };
                        if better {
                            best_d2 = d2;
                            best_idx = Some(j);
                        }
                    }
                }
            }

            // Unexamined rings (k > r) are ≥ r·cell_size away. Stop once that
            // strictly exceeds the best distance found — nothing left can tie it.
            let reach = r as f32 * self.cell_size;
            if best_idx.is_some() && reach * reach > best_d2 {
                break;
            }
            if r >= max_ring {
                break; // whole grid scanned
            }
            r += 1;
        }
        (best_idx, work)
    }
}

// ---- L1 unit tests ---------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rng_same_seed_same_stream() {
        let mut a = SplitMix64::new(7);
        let mut b = SplitMix64::new(7);
        let mut c = SplitMix64::new(8);
        assert_eq!(a.next_u64(), b.next_u64(), "same seed ⇒ same value");
        assert_ne!(SplitMix64::new(8).next_u64(), a.next_u64());
        // range stays within bounds.
        for _ in 0..1000 {
            let v = c.range_f32(-3.0, 5.0);
            assert!((-3.0..5.0).contains(&v));
        }
    }

    #[test]
    fn layout_has_requested_count_and_both_factions() {
        let layout = random_layout(2000, 1, Vec2::new(-10.0, -10.0), Vec2::new(10.0, 10.0));
        assert_eq!(layout.len(), 2000);
        assert!(layout.iter().any(|u| u.faction == Faction::A));
        assert!(layout.iter().any(|u| u.faction == Faction::B));
        assert!(layout
            .iter()
            .all(|u| u.pos.x >= -10.0 && u.pos.x < 10.0 && u.pos.y >= -10.0 && u.pos.y < 10.0));
    }

    // ---- AC2: brute-force nearest-enemy ------------------------------------

    fn unit(x: f32, y: f32, f: Faction) -> Unit {
        Unit {
            pos: Vec2::new(x, y),
            faction: f,
        }
    }

    #[test]
    fn brute_finds_nearest_opposite_faction() {
        let units = [
            unit(0.0, 0.0, Faction::A),  // 0
            unit(10.0, 0.0, Faction::B), // 1 (far enemy)
            unit(5.0, 0.0, Faction::B),  // 2 (near enemy)
        ];
        assert_eq!(brute_force_nearest_enemy(&units, 0), Some(2), "nearest enemy, not just any");
        assert_eq!(brute_force_nearest_enemy(&units, 2), Some(0), "B's nearest enemy is the A");
    }

    #[test]
    fn brute_single_unit_has_no_enemy() {
        let units = [unit(3.0, 4.0, Faction::A)];
        assert_eq!(brute_force_nearest_enemy(&units, 0), None);
    }

    #[test]
    fn brute_all_one_faction_has_no_enemy() {
        let units = [
            unit(0.0, 0.0, Faction::A),
            unit(1.0, 1.0, Faction::A),
            unit(9.0, 9.0, Faction::A),
        ];
        for i in 0..units.len() {
            assert_eq!(brute_force_nearest_enemy(&units, i), None, "no enemy anywhere");
        }
    }

    #[test]
    fn brute_ties_break_to_smallest_index() {
        // Two enemies at the exact same distance (5) from unit 0.
        let units = [
            unit(0.0, 0.0, Faction::A), // 0
            unit(0.0, 5.0, Faction::B), // 1  distance 5
            unit(5.0, 0.0, Faction::B), // 2  distance 5
        ];
        assert_eq!(
            brute_force_nearest_enemy(&units, 0),
            Some(1),
            "equal distance ⇒ smallest index wins, deterministically"
        );
    }

    // ---- AC3: spatial grid == brute force (the differential oracle) ---------

    /// Assert the grid answers *every* query identically to brute force, for a
    /// grid built both from data-derived bounds and from the given tight bounds.
    fn assert_grid_eq_brute(units: &[Unit], cell_size: f32, min: Vec2, max: Vec2) {
        let g_auto = SpatialGrid::build(units, cell_size);
        let g_fixed = SpatialGrid::build_with_bounds(units, cell_size, min, max);
        for i in 0..units.len() {
            let expected = brute_force_nearest_enemy(units, i);
            assert_eq!(
                g_auto.nearest_enemy(units, i),
                expected,
                "data-bounds grid disagrees at unit {i}"
            );
            assert_eq!(
                g_fixed.nearest_enemy(units, i),
                expected,
                "fixed-bounds grid disagrees at unit {i}"
            );
        }
    }

    #[test]
    fn grid_matches_brute_over_many_seeds() {
        let (min, max) = (Vec2::new(-1000.0, -1000.0), Vec2::new(1000.0, 1000.0));
        for seed in 0..80u64 {
            // Vary count and cell size across seeds to probe many grid shapes.
            let n = 50 + (seed as usize % 7) * 60; // 50..410
            let cell = 20.0 + (seed % 5) as f32 * 55.0; // 20..240
            let units = random_layout(n, seed.wrapping_mul(0x0001_2345), min, max);
            assert_grid_eq_brute(&units, cell, min, max);
        }
    }

    #[test]
    fn grid_matches_brute_single_and_all_one_faction() {
        // Single unit → no enemy.
        let one = [unit(1.0, 2.0, Faction::A)];
        assert_eq!(SpatialGrid::build(&one, 10.0).nearest_enemy(&one, 0), None);

        // All one faction → no enemy for anyone.
        let all_a = [
            unit(0.0, 0.0, Faction::A),
            unit(30.0, 10.0, Faction::A),
            unit(-40.0, 90.0, Faction::A),
        ];
        let g = SpatialGrid::build(&all_a, 25.0);
        for i in 0..all_a.len() {
            assert_eq!(g.nearest_enemy(&all_a, i), None);
        }
    }

    #[test]
    fn grid_handles_units_outside_explicit_bounds() {
        // Tight bounds around the origin, but several units live far outside —
        // they must be bucketed into edge cells and still yield brute-identical
        // answers (query points inside *and* outside the bounds).
        let units = [
            unit(0.0, 0.0, Faction::A),
            unit(5.0, 5.0, Faction::B),
            unit(9000.0, -9000.0, Faction::B),  // far outside
            unit(-9000.0, 9000.0, Faction::A),  // far outside
            unit(3.0, -4000.0, Faction::B),     // outside on one axis
            unit(-2.0, 2.0, Faction::A),
        ];
        assert_grid_eq_brute(&units, 15.0, Vec2::new(-10.0, -10.0), Vec2::new(10.0, 10.0));
    }

    #[test]
    fn grid_handles_boundary_empty_cells_and_duplicates() {
        let (min, max) = (Vec2::new(0.0, 0.0), Vec2::new(100.0, 100.0));
        let units = [
            unit(0.0, 0.0, Faction::A),     // exactly on the min corner
            unit(100.0, 100.0, Faction::B), // exactly on the max corner
            unit(50.0, 50.0, Faction::A),   // isolated with empty neighbor cells
            unit(50.0, 50.0, Faction::B),   // duplicate position, opposite side (dist 0)
            unit(0.0, 100.0, Faction::B),   // other corners
            unit(100.0, 0.0, Faction::A),
        ];
        // small cells ⇒ many empty cells between occupied ones.
        assert_grid_eq_brute(&units, 7.0, min, max);
        // one big cell ⇒ everything in a single bucket (degenerate but valid).
        assert_grid_eq_brute(&units, 1000.0, min, max);
    }

    #[test]
    fn grid_visits_far_fewer_candidates_than_brute() {
        // The speedup mechanism, proven deterministically: on a dense uniform
        // field the grid evaluates far fewer distances than the naive scan.
        let (min, max) = (Vec2::new(-1000.0, -1000.0), Vec2::new(1000.0, 1000.0));
        let units = random_layout(2000, 99, min, max);
        let cell = 40.0;
        let grid = SpatialGrid::build(&units, cell);

        let mut brute_work = 0usize;
        let mut grid_work = 0usize;
        for i in 0..units.len() {
            let (be, bw) = brute_force_nearest_enemy_counted(&units, i);
            let (ge, gw) = grid.nearest_enemy_counted(&units, i);
            assert_eq!(be, ge, "counted variants must also agree at unit {i}");
            brute_work += bw;
            grid_work += gw;
        }
        assert!(
            grid_work * 10 < brute_work,
            "grid should evaluate <1/10 the distances of brute (grid={grid_work}, brute={brute_work})"
        );
    }
}
