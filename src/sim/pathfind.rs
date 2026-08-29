//! Pathfinding for the sim core (M3). **Render-free** — ECS-free math over a
//! tile map, `math` + `std` only; no `Color`/`Sprite`/`Transform`/`Window`, so
//! it is fully headless-testable and deterministic.
//!
//! Three pieces, each a pure function of its inputs:
//!   - [`TileGrid`] — a `width × height` map of walkable/blocked cells, laid over
//!     world space by an `origin` + `cell_size`.
//!   - `astar` (AC2) — shortest 4-connected path between two cells over walkable
//!     cells; a single unit follows the returned waypoints around obstacles.
//!   - `FlowField` (AC3) — one BFS from a goal that *every* unit in a group move
//!     follows, instead of running N separate A* searches.
//!
//! Determinism: A* orders its open set by `(f, g, cell_index)`, so the returned
//! path is identical across runs and independent of container iteration order.
//! The flow field's BFS is FIFO over a fixed neighbour order. All bookkeeping is
//! `Vec` indexed by cell id — **no `HashMap`/`HashSet`**, so no iteration-order
//! nondeterminism can leak in. No wall-clock lives here; the only "cost" measured
//! is a deterministic *nodes-expanded* counter (see the `*_counted` variants),
//! which the M3 flow-field-vs-N×A* comparison is built on.

use bevy::math::Vec2;

use crate::sim::spatial::SplitMix64;

/// The four edge-adjacent neighbour offsets, in a fixed order. "Contiguous" in
/// M3 means edge-adjacent (4-connected); diagonals are *not* neighbours.
#[allow(dead_code)] // consumed by A* (AC2) and the flow field (AC3)
const DIRS: [(isize, isize); 4] = [(0, -1), (-1, 0), (1, 0), (0, 1)];

// ---- tile grid -------------------------------------------------------------

/// A render-free tile map: a `width × height` grid of walkable/blocked cells.
///
/// Cells are indexed `y * width + x`. The grid is laid over world space by
/// `origin` (the world position of cell `(0,0)`'s lower corner) and `cell_size`,
/// so a path of cell ids maps to a list of world-space waypoints (cell centres)
/// a unit can follow with the M2 movement code.
pub struct TileGrid {
    pub width: usize,
    pub height: usize,
    pub cell_size: f32,
    pub origin: Vec2,
    /// `blocked[y * width + x]` — true where a unit may not stand or pass.
    blocked: Vec<bool>,
}

impl TileGrid {
    /// An all-walkable `width × height` grid.
    pub fn new(width: usize, height: usize, cell_size: f32, origin: Vec2) -> Self {
        Self {
            width,
            height,
            cell_size,
            origin,
            blocked: vec![false; width * height],
        }
    }

    /// Build a grid from ASCII rows: `'#'` = blocked, anything else = walkable.
    /// Row `0` is `y = 0`. A readable fixture for tests and hand-built maps.
    pub fn from_rows(rows: &[&str], cell_size: f32, origin: Vec2) -> Self {
        let height = rows.len();
        let width = rows.iter().map(|r| r.chars().count()).max().unwrap_or(0);
        let mut blocked = vec![false; width * height];
        for (y, row) in rows.iter().enumerate() {
            for (x, ch) in row.chars().enumerate() {
                if ch == '#' {
                    blocked[y * width + x] = true;
                }
            }
        }
        Self {
            width,
            height,
            cell_size,
            origin,
            blocked,
        }
    }

    /// A deterministic obstacle field: each cell is blocked independently with
    /// probability `blocked_fraction`, drawn from a seeded [`SplitMix64`]. Same
    /// `(w, h, fraction, seed)` ⇒ byte-identical layout.
    pub fn random(
        width: usize,
        height: usize,
        cell_size: f32,
        origin: Vec2,
        blocked_fraction: f32,
        seed: u64,
    ) -> Self {
        let mut rng = SplitMix64::new(seed);
        let mut blocked = vec![false; width * height];
        for b in blocked.iter_mut() {
            if rng.next_f32() < blocked_fraction {
                *b = true;
            }
        }
        Self {
            width,
            height,
            cell_size,
            origin,
            blocked,
        }
    }

    /// Cell id for `(x, y)`. Caller ensures the coordinates are in bounds.
    #[inline]
    pub fn idx(&self, x: usize, y: usize) -> usize {
        y * self.width + x
    }

    /// `(x, y)` of a cell id.
    #[inline]
    pub fn xy(&self, idx: usize) -> (usize, usize) {
        (idx % self.width, idx / self.width)
    }

    /// Total cell count.
    #[inline]
    pub fn len(&self) -> usize {
        self.width * self.height
    }

    /// Whether the grid has no cells.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.blocked.is_empty()
    }

    /// Whether cell `idx` is walkable (in range and not blocked).
    #[inline]
    pub fn is_walkable(&self, idx: usize) -> bool {
        idx < self.blocked.len() && !self.blocked[idx]
    }

    /// Whether cell `(x, y)` is walkable (in range and not blocked).
    #[inline]
    pub fn is_walkable_xy(&self, x: usize, y: usize) -> bool {
        x < self.width && y < self.height && !self.blocked[self.idx(x, y)]
    }

    /// Block or unblock cell `(x, y)`.
    pub fn set_blocked(&mut self, x: usize, y: usize, blocked: bool) {
        let i = self.idx(x, y);
        self.blocked[i] = blocked;
    }

    /// World-space centre of a cell — the waypoint a unit steers to.
    #[inline]
    pub fn cell_center(&self, idx: usize) -> Vec2 {
        let (x, y) = self.xy(idx);
        self.origin
            + Vec2::new(
                (x as f32 + 0.5) * self.cell_size,
                (y as f32 + 0.5) * self.cell_size,
            )
    }

    /// Cell id containing world point `p`, clamped to the grid bounds.
    pub fn cell_at(&self, p: Vec2) -> usize {
        let fx = ((p.x - self.origin.x) / self.cell_size).floor() as isize;
        let fy = ((p.y - self.origin.y) / self.cell_size).floor() as isize;
        let x = fx.clamp(0, self.width as isize - 1) as usize;
        let y = fy.clamp(0, self.height as isize - 1) as usize;
        self.idx(x, y)
    }

    /// Append the walkable edge-adjacent neighbours of `idx` to `out` (cleared
    /// first), in the fixed [`DIRS`] order. Shared by A* and the flow field so
    /// both agree exactly on what "contiguous" means.
    #[allow(dead_code)] // consumed by A* (AC2) and the flow field (AC3)
    fn walkable_neighbors(&self, idx: usize, out: &mut Vec<usize>) {
        out.clear();
        let (x, y) = self.xy(idx);
        for (dx, dy) in DIRS {
            let nx = x as isize + dx;
            let ny = y as isize + dy;
            if nx >= 0 && ny >= 0 && (nx as usize) < self.width && (ny as usize) < self.height {
                let ni = self.idx(nx as usize, ny as usize);
                if !self.blocked[ni] {
                    out.push(ni);
                }
            }
        }
    }
}

// ---- L1 unit tests ---------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn open_grid() -> TileGrid {
        TileGrid::new(5, 5, 1.0, Vec2::ZERO)
    }

    #[test]
    fn index_and_walkability_round_trip() {
        let mut g = open_grid();
        assert!(g.is_walkable_xy(2, 3));
        g.set_blocked(2, 3, true);
        assert!(!g.is_walkable_xy(2, 3));
        let i = g.idx(2, 3);
        assert_eq!(g.xy(i), (2, 3));
        assert!(!g.is_walkable(i));
    }

    #[test]
    fn neighbors_are_edge_adjacent_and_skip_blocked_and_edges() {
        let mut g = open_grid();
        g.set_blocked(1, 0, true); // block one of corner (0,0)'s neighbours
        let mut out = Vec::new();
        g.walkable_neighbors(g.idx(0, 0), &mut out);
        // Corner (0,0): only (1,0) and (0,1) are in bounds; (1,0) is blocked.
        assert_eq!(out, vec![g.idx(0, 1)]);
    }
}
