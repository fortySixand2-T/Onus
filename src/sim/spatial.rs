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
}
