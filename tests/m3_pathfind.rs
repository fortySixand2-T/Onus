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

use onus::sim::pathfind::TileGrid;

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
