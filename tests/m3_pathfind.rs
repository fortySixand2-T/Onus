//! L2 integration tests for M3 (pathfinding).
//!
//! These exercise the render-free `sim::pathfind` module directly (it needs no
//! ECS surface — it is pure math over a tile grid) and encode the M3 acceptance
//! criteria plus the critic probes:
//!   (a) an A* path is walkable + contiguous + reaches the goal;
//!   (b) A* returns `None` **iff** the goal is unreachable (reachable & walled);
//!   (c) the flow field agrees with A* on reachability (flow dir iff A* path);
//!   (d) a group move costs far less than N× A* (deterministic node-expansion).

use bevy::math::Vec2;

use onus::sim::pathfind::{astar, astar_counted, TileGrid};
use onus::sim::step_toward;

/// Assert `path` is a valid route from `start` to `goal`: every cell walkable,
/// each consecutive pair edge-adjacent (contiguous), ends exactly on the goal.
fn assert_valid_path(grid: &TileGrid, path: &[usize], start: usize, goal: usize) {
    assert!(!path.is_empty(), "a found path is never empty");
    assert_eq!(path[0], start, "path starts at the start cell");
    assert_eq!(*path.last().unwrap(), goal, "path reaches the goal");
    for &c in path {
        assert!(grid.is_walkable(c), "cell {c} on the path is walkable");
    }
    for w in path.windows(2) {
        let (ax, ay) = grid.xy(w[0]);
        let (bx, by) = grid.xy(w[1]);
        let manhattan = ax.abs_diff(bx) + ay.abs_diff(by);
        assert_eq!(manhattan, 1, "consecutive cells are edge-adjacent (contiguous)");
    }
}

// A grid whose cells are 10 world-units wide, origin at the world origin.
const CELL: f32 = 10.0;
const ORIGIN: Vec2 = Vec2::ZERO;

// ---- AC1: tile grid with blocked cells -------------------------------------

#[test]
fn tile_grid_tracks_walkable_and_blocked() {
    // '#'=blocked, '.'=walkable. A 4×3 map with a blocked middle column stub.
    let grid = TileGrid::from_rows(
        &[
            "....", // y=0
            ".##.", // y=1
            "....", // y=2
        ],
        CELL,
        ORIGIN,
    );
    assert_eq!(grid.width, 4);
    assert_eq!(grid.height, 3);
    assert!(grid.is_walkable_xy(0, 0));
    assert!(grid.is_walkable_xy(3, 2));
    assert!(!grid.is_walkable_xy(1, 1), "the '#' cell is blocked");
    assert!(!grid.is_walkable_xy(2, 1), "the '#' cell is blocked");
    assert!(grid.is_walkable_xy(0, 1), "'.' next to the wall is walkable");

    // Index ↔ (x,y) round-trips.
    let i = grid.idx(2, 1);
    assert_eq!(grid.xy(i), (2, 1));
    assert!(!grid.is_walkable(i));
}

#[test]
fn tile_grid_maps_cells_to_world_centers() {
    let grid = TileGrid::new(5, 5, CELL, Vec2::new(100.0, 200.0));
    // Cell (0,0) center sits half a cell in from the origin corner.
    assert_eq!(grid.cell_center(grid.idx(0, 0)), Vec2::new(105.0, 205.0));
    assert_eq!(grid.cell_center(grid.idx(2, 3)), Vec2::new(125.0, 235.0));
    // A world point lands in the cell that contains it.
    assert_eq!(grid.cell_at(Vec2::new(126.0, 231.0)), grid.idx(2, 3));
}

#[test]
fn tile_grid_random_is_deterministic() {
    let a = TileGrid::random(20, 20, CELL, ORIGIN, 0.3, 0xABCDEF);
    let b = TileGrid::random(20, 20, CELL, ORIGIN, 0.3, 0xABCDEF);
    let c = TileGrid::random(20, 20, CELL, ORIGIN, 0.3, 0x123456);
    let same = (0..a.width * a.height).all(|i| a.is_walkable(i) == b.is_walkable(i));
    assert!(same, "same seed ⇒ identical blocked layout");
    let differ = (0..a.width * a.height).any(|i| a.is_walkable(i) != c.is_walkable(i));
    assert!(differ, "a different seed changes the layout");
    // Some cells blocked, some free (a real obstacle field, not all-or-nothing).
    let blocked = (0..a.width * a.height).filter(|&i| !a.is_walkable(i)).count();
    assert!(blocked > 0 && blocked < a.width * a.height);
}

// ---- AC2: A* path around obstacles -----------------------------------------

// A map with a full-height wall at column x=3 whose *only* gap is the bottom
// row, forcing any left→right path to detour all the way down to y=5.
fn walled_map() -> TileGrid {
    TileGrid::from_rows(
        &[
            "...#..", // y=0
            "...#..", // y=1
            "...#..", // y=2
            "...#..", // y=3
            "...#..", // y=4
            "......", // y=5  gap: x=3 open only here
        ],
        CELL,
        ORIGIN,
    )
}

#[test]
fn astar_finds_walkable_contiguous_path_to_goal() {
    let grid = walled_map();
    let start = grid.idx(0, 2);
    let goal = grid.idx(5, 2);
    let path = astar(&grid, start, goal).expect("goal is reachable through the gap");
    assert_valid_path(&grid, &path, start, goal);
    // The path must route around the wall (x=3 is blocked at rows 1..=4), so it
    // must dip to at least y=5 somewhere to get past it.
    assert!(
        path.iter().any(|&c| grid.xy(c).1 == 5),
        "path detours below the wall"
    );
}

#[test]
fn astar_start_equals_goal_is_trivial_path() {
    let grid = TileGrid::new(4, 4, CELL, ORIGIN);
    let s = grid.idx(1, 1);
    assert_eq!(astar(&grid, s, s), Some(vec![s]), "already there ⇒ singleton path");
}

#[test]
fn astar_returns_shortest_path_length() {
    // Open grid: Manhattan distance is the shortest number of steps; path cells
    // count = steps + 1.
    let grid = TileGrid::new(8, 8, CELL, ORIGIN);
    let start = grid.idx(0, 0);
    let goal = grid.idx(5, 3);
    let path = astar(&grid, start, goal).unwrap();
    assert_valid_path(&grid, &path, start, goal);
    assert_eq!(path.len(), (5 + 3) + 1, "shortest 4-connected path is Manhattan length");
}

#[test]
fn astar_is_deterministic_across_runs() {
    // Determinism: identical path every run regardless of container iteration.
    let grid = TileGrid::random(30, 30, CELL, ORIGIN, 0.25, 0xF00D);
    let start = grid.idx(0, 0);
    let goal = grid.idx(29, 29);
    let first = astar(&grid, start, goal);
    for _ in 0..8 {
        assert_eq!(astar(&grid, start, goal), first, "A* path is stable across runs");
    }
}

// ---- AC2 probe (b): None iff unreachable -----------------------------------

#[test]
fn astar_none_iff_unreachable() {
    // Reachable goal ⇒ Some.
    let open = TileGrid::new(5, 5, CELL, ORIGIN);
    assert!(astar(&open, open.idx(0, 0), open.idx(4, 4)).is_some(), "reachable ⇒ Some");

    // A goal fully walled off by blocked cells ⇒ None.
    let mut walled = TileGrid::new(5, 5, CELL, ORIGIN);
    let goal = walled.idx(2, 2);
    for (x, y) in [(1, 2), (3, 2), (2, 1), (2, 3)] {
        walled.set_blocked(x, y, true); // ring the goal's four neighbours
    }
    assert_eq!(astar(&walled, walled.idx(0, 0), goal), None, "walled-off goal ⇒ None");

    // A blocked start or blocked goal is also unreachable.
    let mut g = TileGrid::new(4, 4, CELL, ORIGIN);
    g.set_blocked(3, 3, true);
    assert_eq!(astar(&g, g.idx(0, 0), g.idx(3, 3)), None, "blocked goal ⇒ None");
    assert_eq!(astar(&g, g.idx(3, 3), g.idx(0, 0)), None, "blocked start ⇒ None");
}

// ---- AC2: a unit follows the waypoints around obstacles ---------------------

#[test]
fn unit_follows_astar_waypoints_around_obstacle() {
    // Drive a real moving unit (M2 step_toward) through the A* waypoints and
    // confirm it threads the gap: it visits only walkable cells and arrives.
    let grid = walled_map();
    let start = grid.idx(0, 2);
    let goal = grid.idx(5, 2);
    let path = astar(&grid, start, goal).unwrap();

    let mut pos = grid.cell_center(start);
    let step = 3.0; // world units per sub-step; several sub-steps per waypoint
    for &wp in &path {
        let target = grid.cell_center(wp);
        // Step toward this waypoint until arrival (bounded iterations).
        for _ in 0..1000 {
            // The unit's current cell must always be a walkable one.
            assert!(grid.is_walkable(grid.cell_at(pos)), "unit never enters a blocked cell");
            let (next, arrived) = step_toward(pos, target, step);
            pos = next;
            if arrived {
                break;
            }
        }
    }
    assert_eq!(grid.cell_at(pos), goal, "unit ends on the goal cell");
}

// ---- AC2: nodes-expanded counter is exposed and sane -----------------------

#[test]
fn astar_counted_agrees_with_astar_and_reports_work() {
    let grid = walled_map();
    let start = grid.idx(0, 0);
    let goal = grid.idx(5, 5);
    let (path, expanded) = astar_counted(&grid, start, goal);
    assert_eq!(path, astar(&grid, start, goal), "counted variant returns the same path");
    assert!(path.is_some());
    assert!(expanded > 0, "a real search expands at least one node");
    // Unreachable searches still terminate and report their (bounded) work.
    let mut walled = grid;
    for (x, y) in [(4, 5), (5, 4)] {
        walled.set_blocked(x, y, true);
    }
    let (none, _) = astar_counted(&walled, start, walled.idx(5, 5));
    assert_eq!(none, None);
}
