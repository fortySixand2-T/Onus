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
use std::cmp::Reverse;
use std::collections::{BinaryHeap, VecDeque};

use crate::sim::spatial::SplitMix64;

/// The four edge-adjacent neighbour offsets, in a fixed order. "Contiguous" in
/// M3 means edge-adjacent (4-connected); diagonals are *not* neighbours.
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

/// Manhattan distance between two cells — admissible & consistent for unit-cost
/// 4-connected movement, so A* returns a shortest path and never reopens.
fn manhattan(grid: &TileGrid, a: usize, b: usize) -> u32 {
    let (ax, ay) = grid.xy(a);
    let (bx, by) = grid.xy(b);
    (ax.abs_diff(bx) + ay.abs_diff(by)) as u32
}

// ---- A* --------------------------------------------------------------------

/// Shortest 4-connected path of cell ids from `start` to `goal` over walkable
/// cells, inclusive of both ends, or `None` **iff** the goal is unreachable
/// (blocked start/goal, or no contiguous walkable route). See [`astar_counted`].
pub fn astar(grid: &TileGrid, start: usize, goal: usize) -> Option<Vec<usize>> {
    astar_counted(grid, start, goal).0
}

/// As [`astar`], but also returns the number of nodes *expanded* (popped and
/// settled from the open set) — the search's deterministic work, used to prove
/// the flow field solves a group move in far less work than N separate A* runs.
pub fn astar_counted(grid: &TileGrid, start: usize, goal: usize) -> (Option<Vec<usize>>, usize) {
    let n = grid.len();
    // A path must start and end on walkable cells.
    if start >= n || goal >= n || !grid.is_walkable(start) || !grid.is_walkable(goal) {
        return (None, 0);
    }

    let mut g_score = vec![u32::MAX; n];
    let mut came_from = vec![usize::MAX; n];
    // Open set ordered by (f, g, cell) via `Reverse` so the min pops first — the
    // deterministic tie-break that makes the path independent of push order.
    let mut open: BinaryHeap<Reverse<(u32, u32, usize)>> = BinaryHeap::new();
    let mut neighbors = Vec::with_capacity(4);
    let mut expanded = 0usize;

    g_score[start] = 0;
    open.push(Reverse((manhattan(grid, start, goal), 0, start)));

    while let Some(Reverse((_f, g, cell))) = open.pop() {
        // Skip stale heap entries (a better path to `cell` was already settled).
        if g > g_score[cell] {
            continue;
        }
        expanded += 1;
        if cell == goal {
            return (Some(reconstruct(&came_from, start, goal)), expanded);
        }
        grid.walkable_neighbors(cell, &mut neighbors);
        for &nb in &neighbors {
            let tentative = g + 1;
            if tentative < g_score[nb] {
                g_score[nb] = tentative;
                came_from[nb] = cell;
                open.push(Reverse((tentative + manhattan(grid, nb, goal), tentative, nb)));
            }
        }
    }
    (None, expanded)
}

/// Walk `came_from` from `goal` back to `start`, producing `start..=goal`.
fn reconstruct(came_from: &[usize], start: usize, goal: usize) -> Vec<usize> {
    let mut path = vec![goal];
    let mut cur = goal;
    while cur != start {
        cur = came_from[cur];
        path.push(cur);
    }
    path.reverse();
    path
}

// ---- flow field ------------------------------------------------------------

/// A group-move solution: one BFS outward from a single goal that assigns every
/// reachable cell the next step toward the goal. N units share this *one* field
/// instead of running N separate A* searches. Built by [`FlowField::compute`].
///
/// The edges are unit-cost and undirected, so a plain FIFO BFS yields shortest
/// distances and a field whose reachability is identical to A*'s. Deterministic:
/// FIFO queue over the fixed [`DIRS`] neighbour order, all state `Vec`-indexed.
pub struct FlowField {
    goal: usize,
    /// BFS distance (in steps) from each cell to the goal; `u32::MAX` if the goal
    /// is unreachable from that cell (or the cell is blocked).
    dist: Vec<u32>,
    /// `from[cell]` = the neighbour a unit on `cell` steps to next to head toward
    /// the goal, or `usize::MAX` for the goal itself and for unreachable cells.
    from: Vec<usize>,
}

impl FlowField {
    /// Compute the field toward `goal`. See [`FlowField::compute_counted`].
    pub fn compute(grid: &TileGrid, goal: usize) -> Self {
        Self::compute_counted(grid, goal).0
    }

    /// As [`FlowField::compute`], but also returns the number of nodes expanded
    /// (cells dequeued) — the *whole group's* search work, one BFS in total, the
    /// deterministic figure compared against N×A* in the M3 findings.
    pub fn compute_counted(grid: &TileGrid, goal: usize) -> (Self, usize) {
        let n = grid.len();
        let mut dist = vec![u32::MAX; n];
        let mut from = vec![usize::MAX; n];
        let mut expanded = 0usize;

        if goal < n && grid.is_walkable(goal) {
            let mut queue = VecDeque::new();
            let mut neighbors = Vec::with_capacity(4);
            dist[goal] = 0;
            queue.push_back(goal);
            while let Some(cell) = queue.pop_front() {
                expanded += 1;
                grid.walkable_neighbors(cell, &mut neighbors);
                for &nb in &neighbors {
                    if dist[nb] == u32::MAX {
                        // First time reached ⇒ shortest (unit-cost BFS). Point the
                        // neighbour back at `cell`, which is one step closer.
                        dist[nb] = dist[cell] + 1;
                        from[nb] = cell;
                        queue.push_back(nb);
                    }
                }
            }
        }
        (Self { goal, dist, from }, expanded)
    }

    /// The goal cell this field flows toward.
    #[inline]
    pub fn goal(&self) -> usize {
        self.goal
    }

    /// Whether the goal is reachable from `cell` by following the field. True for
    /// the goal itself.
    #[inline]
    pub fn is_reachable(&self, cell: usize) -> bool {
        cell < self.dist.len() && self.dist[cell] != u32::MAX
    }

    /// BFS distance from `cell` to the goal in steps, or `None` if unreachable.
    #[inline]
    pub fn distance(&self, cell: usize) -> Option<u32> {
        self.is_reachable(cell).then(|| self.dist[cell])
    }

    /// The next cell a unit on `cell` should move to. `None` at the goal (already
    /// arrived) or from an unreachable cell (no flow direction there).
    #[inline]
    pub fn next(&self, cell: usize) -> Option<usize> {
        match self.from.get(cell).copied() {
            Some(usize::MAX) | None => None,
            Some(next) => Some(next),
        }
    }

    /// Whether `cell` carries a flow direction (equivalently, `next(cell)` is
    /// `Some`). This is the goal-excluded reachability the critic probe checks
    /// against A*: every non-goal cell has a direction iff A* finds a path.
    #[inline]
    pub fn has_direction(&self, cell: usize) -> bool {
        self.next(cell).is_some()
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
