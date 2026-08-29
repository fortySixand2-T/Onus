//! M3 adversarial critic tests — written independently of the implementer's
//! reasoning, judged against the BUILD_PLAN M3 spec + critic probes alone.
//!
//! Strategy: differential oracles. A hand-rolled BFS (with a *different* neighbour
//! iteration order than the production `DIRS`, so it cannot mirror an ordering bug)
//! is the ground truth for shortest-path distance and reachability. Every A* path
//! and every flow-field claim is checked against it over many seeded random grids.
//!
//! Only the public API (`onus::sim::*`) is used. RNG is the crate's seeded
//! `SplitMix64` — dependency-free and deterministic.

use bevy::math::Vec2;
use onus::sim::pathfind::{astar, astar_counted, FlowField, TileGrid};
use onus::sim::SplitMix64;

// ---- independent oracle ----------------------------------------------------

/// Neighbour order deliberately *reversed* vs production `DIRS` = [(0,-1),(-1,0),
/// (1,0),(0,1)]. BFS distance is order-invariant, so this must still agree — and
/// disagreement would expose an ordering coupling.
const ORACLE_DIRS: [(isize, isize); 4] = [(0, 1), (1, 0), (-1, 0), (0, -1)];

fn oracle_neighbors(g: &TileGrid, idx: usize) -> Vec<usize> {
    let (x, y) = (idx % g.width, idx / g.width);
    let mut out = Vec::new();
    for (dx, dy) in ORACLE_DIRS {
        let nx = x as isize + dx;
        let ny = y as isize + dy;
        if nx >= 0 && ny >= 0 && (nx as usize) < g.width && (ny as usize) < g.height {
            let ni = (ny as usize) * g.width + nx as usize;
            if g.is_walkable(ni) {
                out.push(ni);
            }
        }
    }
    out
}

/// BFS distance (in steps) from `source` to every cell, `None` where unreachable
/// or blocked. The single source of truth for shortest distance & reachability.
fn bfs_dist_from(g: &TileGrid, source: usize) -> Vec<Option<u32>> {
    let n = g.len();
    let mut dist = vec![None; n];
    if source >= n || !g.is_walkable(source) {
        return dist;
    }
    let mut q = std::collections::VecDeque::new();
    dist[source] = Some(0);
    q.push_back(source);
    while let Some(c) = q.pop_front() {
        let d = dist[c].unwrap();
        for nb in oracle_neighbors(g, c) {
            if dist[nb].is_none() {
                dist[nb] = Some(d + 1);
                q.push_back(nb);
            }
        }
    }
    dist
}

/// Assert a returned A* path is structurally valid AND shortest per the oracle.
fn assert_shortest_valid(g: &TileGrid, path: &[usize], start: usize, goal: usize) {
    assert!(!path.is_empty(), "path must be non-empty");
    assert_eq!(path[0], start, "path must start at start");
    assert_eq!(*path.last().unwrap(), goal, "path must end at goal");

    // No repeated cells.
    let mut seen = vec![false; g.len()];
    for &c in path {
        assert!(g.is_walkable(c), "every cell on path must be walkable: {c}");
        assert!(!seen[c], "path must not repeat cell {c}");
        seen[c] = true;
    }
    // Every consecutive pair edge-adjacent (Manhattan distance 1).
    for w in path.windows(2) {
        let (ax, ay) = (w[0] % g.width, w[0] / g.width);
        let (bx, by) = (w[1] % g.width, w[1] / g.width);
        let md = ax.abs_diff(bx) + ay.abs_diff(by);
        assert_eq!(md, 1, "steps {} -> {} must be 4-adjacent", w[0], w[1]);
    }
    // Length == oracle shortest distance + 1.
    let oracle = bfs_dist_from(g, start)[goal].expect("oracle says reachable");
    assert_eq!(
        path.len() as u32 - 1,
        oracle,
        "path length must equal BFS shortest distance"
    );
}

// ---- 1. A* optimality, validity, None-iff-unreachable ----------------------

#[test]
fn astar_matches_bfs_over_many_random_grids() {
    for seed in 0..300u64 {
        let w = 6 + (seed as usize % 10);
        let h = 6 + ((seed as usize / 10) % 10);
        let frac = 0.15 + (seed % 4) as f32 * 0.08; // 0.15..=0.39
        let grid = TileGrid::random(w, h, 1.0, Vec2::ZERO, frac, seed);
        let n = grid.len();

        let mut rng = SplitMix64::new(seed ^ 0xD1CE);
        for _ in 0..8 {
            let start = (rng.next_u64() as usize) % n;
            let goal = (rng.next_u64() as usize) % n;

            let dist = bfs_dist_from(&grid, start);
            let reachable = grid.is_walkable(start)
                && grid.is_walkable(goal)
                && dist[goal].is_some();

            match astar(&grid, start, goal) {
                Some(path) => {
                    assert!(
                        reachable,
                        "A* returned a path where oracle says unreachable \
                         (seed {seed}, {start}->{goal})"
                    );
                    assert_shortest_valid(&grid, &path, start, goal);
                }
                None => assert!(
                    !reachable,
                    "A* returned None but oracle says reachable \
                     (seed {seed}, {start}->{goal})"
                ),
            }
        }
    }
}

#[test]
fn astar_none_on_blocked_start_or_goal() {
    let mut g = TileGrid::new(5, 5, 1.0, Vec2::ZERO);
    g.set_blocked(2, 2, true);
    let blocked = g.idx(2, 2);
    let open = g.idx(0, 0);
    assert!(astar(&g, blocked, open).is_none(), "blocked start => None");
    assert!(astar(&g, open, blocked).is_none(), "blocked goal => None");
}

#[test]
fn astar_none_on_sealed_pocket() {
    // A 1-cell pocket walled off from the rest.
    let rows = [
        ".....",
        ".###.",
        ".#.#.",
        ".###.",
        ".....",
    ];
    let g = TileGrid::from_rows(&rows, 1.0, Vec2::ZERO);
    let pocket = g.idx(2, 2);
    let outside = g.idx(0, 0);
    assert!(g.is_walkable(pocket), "pocket cell itself is walkable");
    assert!(astar(&g, outside, pocket).is_none(), "sealed pocket unreachable");
    assert!(astar(&g, pocket, outside).is_none(), "and unreachable back out");
    // But the pocket can reach itself.
    assert_eq!(astar(&g, pocket, pocket).map(|p| p.len()), Some(1));
}

#[test]
fn astar_start_equals_goal_is_singleton() {
    let g = TileGrid::new(4, 4, 1.0, Vec2::ZERO);
    let c = g.idx(1, 2);
    assert_eq!(astar(&g, c, c), Some(vec![c]));
}

// ---- 2. Determinism --------------------------------------------------------

#[test]
fn astar_is_deterministic_across_repeated_runs() {
    let grid = TileGrid::random(20, 20, 1.0, Vec2::ZERO, 0.28, 0x5EED);
    let n = grid.len();
    let mut rng = SplitMix64::new(7);
    for _ in 0..40 {
        let s = (rng.next_u64() as usize) % n;
        let ggoal = (rng.next_u64() as usize) % n;
        let first = astar(&grid, s, ggoal);
        for _ in 0..3 {
            assert_eq!(astar(&grid, s, ggoal), first, "A* must be deterministic");
        }
    }
}

#[test]
fn astar_depends_only_on_grid_content_not_construction() {
    // Same blocked layout built two different ways must yield identical paths.
    let rows = [
        "..........",
        ".####.###.",
        ".#..#.#.#.",
        ".#.##.#.#.",
        "......#...",
        ".####.###.",
        ".#........",
        ".#.######.",
        ".#......#.",
        "..........",
    ];
    let a = TileGrid::from_rows(&rows, 1.0, Vec2::ZERO);
    let mut b = TileGrid::new(10, 10, 1.0, Vec2::ZERO);
    for (y, row) in rows.iter().enumerate() {
        for (x, ch) in row.chars().enumerate() {
            if ch == '#' {
                b.set_blocked(x, y, true);
            }
        }
    }
    let start = a.idx(0, 0);
    let goal = a.idx(9, 9);
    assert_eq!(astar(&a, start, goal), astar(&b, start, goal));
}

// ---- 3. Flow field ---------------------------------------------------------

#[test]
fn flow_field_distances_equal_bfs_everywhere() {
    for seed in 0..120u64 {
        let w = 8 + (seed as usize % 8);
        let h = 8 + ((seed as usize / 8) % 8);
        let grid = TileGrid::random(w, h, 1.0, Vec2::ZERO, 0.22, seed ^ 0xF10);
        let n = grid.len();
        let goal = (SplitMix64::new(seed).next_u64() as usize) % n;
        if !grid.is_walkable(goal) {
            // Blocked goal: field must declare everything unreachable.
            let field = FlowField::compute(&grid, goal);
            for c in 0..n {
                assert!(!field.has_direction(c));
            }
            continue;
        }
        let oracle = bfs_dist_from(&grid, goal);
        let field = FlowField::compute(&grid, goal);
        for c in 0..n {
            assert_eq!(
                field.distance(c),
                oracle[c],
                "flow distance must equal BFS distance (seed {seed}, cell {c})"
            );
        }
    }
}

#[test]
fn flow_field_reachability_exactly_equals_astar() {
    for seed in 0..120u64 {
        let grid = TileGrid::random(12, 12, 1.0, Vec2::ZERO, 0.25, seed ^ 0xAB);
        let n = grid.len();
        let goal = (SplitMix64::new(seed ^ 1).next_u64() as usize) % n;
        let field = FlowField::compute(&grid, goal);
        for c in 0..n {
            let astar_reaches = astar(&grid, c, goal).is_some();
            // is_reachable includes the goal itself; has_direction excludes it.
            assert_eq!(
                field.is_reachable(c),
                astar_reaches,
                "flow.is_reachable must equal A* reachability (seed {seed}, cell {c})"
            );
            let expect_dir = astar_reaches && c != goal && grid.is_walkable(goal);
            assert_eq!(
                field.has_direction(c),
                expect_dir,
                "has_direction iff A*-reachable non-goal (seed {seed}, cell {c})"
            );
        }
    }
}

#[test]
fn following_flow_next_reaches_goal_without_cycles() {
    for seed in 0..120u64 {
        let grid = TileGrid::random(14, 14, 1.0, Vec2::ZERO, 0.24, seed ^ 0xC0FFEE);
        let n = grid.len();
        let goal = (SplitMix64::new(seed ^ 9).next_u64() as usize) % n;
        if !grid.is_walkable(goal) {
            continue;
        }
        let field = FlowField::compute(&grid, goal);
        for c in 0..n {
            if !field.has_direction(c) {
                continue;
            }
            // Walk the field; it must strictly descend distance and land on goal
            // in exactly `distance(c)` steps, never exceeding n (no cycle/stall).
            let want = field.distance(c).unwrap();
            let mut cur = c;
            let mut steps = 0u32;
            while cur != goal {
                let nxt = field.next(cur).expect("non-goal reachable cell has next");
                // next must be a walkable, edge-adjacent neighbour one step closer.
                assert!(grid.is_walkable(nxt));
                let (ax, ay) = (cur % grid.width, cur / grid.width);
                let (bx, by) = (nxt % grid.width, nxt / grid.width);
                assert_eq!(ax.abs_diff(bx) + ay.abs_diff(by), 1, "next is 4-adjacent");
                assert_eq!(
                    field.distance(nxt).unwrap(),
                    field.distance(cur).unwrap() - 1,
                    "each step strictly descends distance"
                );
                cur = nxt;
                steps += 1;
                assert!(steps <= n as u32, "flow walk must terminate (no cycle)");
            }
            assert_eq!(steps, want, "flow step-count equals shortest distance");
        }
    }
}

#[test]
fn flow_field_is_deterministic() {
    let grid = TileGrid::random(16, 16, 1.0, Vec2::ZERO, 0.2, 0xBEEF);
    let goal = 100;
    let a = FlowField::compute(&grid, goal);
    let b = FlowField::compute(&grid, goal);
    for c in 0..grid.len() {
        assert_eq!(a.distance(c), b.distance(c));
        assert_eq!(a.next(c), b.next(c));
    }
}

// ---- 4. Group cost: one flow field << N x A* (deterministic node count) -----

#[test]
fn one_flow_field_expands_far_fewer_nodes_than_n_astar() {
    // Large mostly-open grid, many units scattered far from a corner goal — the
    // scenario a group move actually hits. Node-expansion counter is the
    // deterministic cost (no wall-clock).
    let grid = TileGrid::random(50, 50, 1.0, Vec2::ZERO, 0.12, 0x9001);
    let n = grid.len();
    let goal = grid.idx(49, 49);
    // If the RNG happened to block the goal, pick the nearest open corner-ish cell.
    let goal = if grid.is_walkable(goal) { goal } else { grid.idx(0, 0) };
    assert!(grid.is_walkable(goal));

    // Collect reachable starts via the oracle.
    let reach = bfs_dist_from(&grid, goal);
    let starts: Vec<usize> = (0..n).filter(|&c| reach[c].is_some() && c != goal).collect();

    let (_field, field_expanded) = FlowField::compute_counted(&grid, goal);

    // N independent A* runs from a sample of the reachable starts.
    let n_units = 150usize.min(starts.len());
    let mut rng = SplitMix64::new(0x1234);
    let mut astar_total = 0usize;
    for _ in 0..n_units {
        let s = starts[(rng.next_u64() as usize) % starts.len()];
        let (path, exp) = astar_counted(&grid, s, goal);
        assert!(path.is_some());
        astar_total += exp;
    }

    // The literal spec claim: group (one field) < N x A*. Assert with a strong
    // margin so a marginal/near-tie result also fails the critic.
    assert!(
        astar_total > 2 * field_expanded,
        "flow field ({field_expanded} nodes) must be far cheaper than {n_units}x A* \
         ({astar_total} nodes)"
    );
    // The field is one BFS: it expands at most the whole grid once.
    assert!(
        field_expanded <= n,
        "one flow field expands each cell at most once (<= {n}), got {field_expanded}"
    );
}

// ---- 5. Edge cases ---------------------------------------------------------

#[test]
fn one_by_one_walkable_grid() {
    let g = TileGrid::new(1, 1, 1.0, Vec2::ZERO);
    assert_eq!(astar(&g, 0, 0), Some(vec![0]));
    let f = FlowField::compute(&g, 0);
    assert_eq!(f.distance(0), Some(0));
    assert_eq!(f.next(0), None);
    assert!(f.is_reachable(0));
    assert!(!f.has_direction(0));
}

#[test]
fn fully_blocked_grid_has_no_paths() {
    let rows = ["###", "###", "###"];
    let g = TileGrid::from_rows(&rows, 1.0, Vec2::ZERO);
    for s in 0..g.len() {
        for gg in 0..g.len() {
            assert!(astar(&g, s, gg).is_none(), "no path in fully blocked grid");
        }
    }
    let f = FlowField::compute(&g, 4);
    for c in 0..g.len() {
        assert!(!f.has_direction(c));
        assert!(!f.is_reachable(c));
    }
}

#[test]
fn single_corridor_path_is_forced_and_shortest() {
    // A width-1 S-corridor; the only path is forced. Length must equal BFS.
    let rows = [
        "#########",
        "#.......#",
        "#######.#",
        "#.......#",
        "#.#######",
        "#.......#",
        "#########",
    ];
    let g = TileGrid::from_rows(&rows, 1.0, Vec2::ZERO);
    let start = g.idx(1, 1);
    let goal = g.idx(1, 5);
    let path = astar(&g, start, goal).expect("corridor is connected");
    assert_shortest_valid(&g, &path, start, goal);
}

#[test]
fn goal_adjacent_to_start() {
    let g = TileGrid::new(6, 6, 1.0, Vec2::ZERO);
    let start = g.idx(2, 2);
    let goal = g.idx(3, 2);
    let path = astar(&g, start, goal).expect("adjacent cells connected");
    assert_eq!(path, vec![start, goal]);
    assert_shortest_valid(&g, &path, start, goal);
}

#[test]
fn large_open_grid_astar_and_flow_agree_with_bfs() {
    let grid = TileGrid::new(80, 80, 1.0, Vec2::ZERO);
    let goal = grid.idx(40, 40);
    let oracle = bfs_dist_from(&grid, goal);
    let field = FlowField::compute(&grid, goal);
    let mut rng = SplitMix64::new(0xF00D);
    for _ in 0..60 {
        let s = (rng.next_u64() as usize) % grid.len();
        let path = astar(&grid, s, goal).expect("open grid fully connected");
        assert_shortest_valid(&grid, &path, s, goal);
        assert_eq!(field.distance(s), oracle[s]);
    }
}
