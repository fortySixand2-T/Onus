//! Shared components, resources, tunables, and the command type.
//!
//! Design rule (from CLAUDE.md): the sim owns truth. `Position` is sim state,
//! integrated in `FixedUpdate`; `Transform` is written from it in `Update`.
//! `Selected` is *presentation* — it's client-side and may change in `Update`.
//! Anything that alters the sim (movement, gather) goes through [`Command`] and
//! is applied only in `FixedUpdate`.

use bevy::prelude::*;
use std::collections::VecDeque;

// ---- tunables --------------------------------------------------------------

/// Unit move speed, world units per second.
pub const SPEED: f32 = 180.0;
/// Distance at which a moving unit snaps to its target and stops.
pub const STOP_EPS: f32 = 1.0;
/// Max seconds between two clicks on the same unit to count as a double-click.
pub const DOUBLE_CLICK_SECS: f32 = 0.30;
/// Mouse travel (world units) past which a left-press is a box-drag, not a click.
pub const DRAG_THRESHOLD: f32 = 8.0;

pub const RESOURCE_SIZE: f32 = 34.0;
pub const RESOURCE_COLOR: Color = Color::srgb(0.95, 0.85, 0.20);
pub const SELECT_COLOR: Color = Color::srgb(1.0, 1.0, 1.0);
pub const DRAG_COLOR: Color = Color::srgb(0.50, 0.90, 0.55);

// ---- components ------------------------------------------------------------

/// Sim-space position — the source of truth for where a thing is.
#[derive(Component)]
pub struct Position(pub Vec2);

/// The kind of a unit. Its presence marks an entity as a commandable unit;
/// resource nodes are `Selectable` but carry no `UnitKind`.
#[derive(Component, Clone, Copy, PartialEq, Eq, Debug)]
pub enum UnitKind {
    Worker,
    Soldier,
    Scout,
}

impl UnitKind {
    pub fn color(self) -> Color {
        match self {
            UnitKind::Worker => Color::srgb(0.30, 0.80, 0.90),
            UnitKind::Soldier => Color::srgb(0.90, 0.40, 0.30),
            UnitKind::Scout => Color::srgb(0.60, 0.90, 0.40),
        }
    }
    pub fn size(self) -> f32 {
        match self {
            UnitKind::Worker => 20.0,
            UnitKind::Soldier => 26.0,
            UnitKind::Scout => 16.0,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            UnitKind::Worker => "Worker",
            UnitKind::Soldier => "Soldier",
            UnitKind::Scout => "Scout",
        }
    }
}

/// Anything the cursor can pick (units and resource nodes).
#[derive(Component)]
pub struct Selectable;

/// Presentation marker: this entity is currently selected. NOT sim state.
#[derive(Component)]
pub struct Selected;

/// A gatherable resource node.
#[derive(Component)]
pub struct ResourceNode {
    pub amount: u32,
}

/// Where a unit is currently ordered to move. Removed on arrival.
#[derive(Component)]
pub struct MoveTarget(pub Vec2);

/// The node a unit is assigned to gather. The gather loop itself is M4;
/// for now this just records intent alongside a `MoveTarget` to the node.
#[derive(Component)]
#[allow(dead_code)] // read by the gather loop in M4
pub struct GatherTarget(pub Entity);

/// UI marker for the on-screen options/status text.
#[derive(Component)]
pub struct OptionsPanel;

// ---- resources -------------------------------------------------------------

/// Cursor position in world space, `None` when off-window or unresolved.
#[derive(Resource, Default)]
pub struct CursorWorld(pub Option<Vec2>);

/// World position where the current left-press began (for box-drag).
#[derive(Resource, Default)]
pub struct DragState {
    pub start: Option<Vec2>,
}

/// Tracks the last click for double-click detection.
#[derive(Resource, Default)]
pub struct ClickTracker {
    pub last_time: f32,
    pub last_entity: Option<Entity>,
}

/// Player intents awaiting application in `FixedUpdate`. The seed of M5's
/// tick-tagged, loggable command stream. (Named `Order` to avoid Bevy's
/// prelude `Command` trait.)
#[derive(Resource, Default)]
pub struct CommandQueue(pub VecDeque<Order>);

/// A sim-affecting order. Emitted by input in `Update`, applied in `FixedUpdate`.
pub enum Order {
    MoveTo {
        units: Vec<Entity>,
        dest: Vec2,
    },
    Gather {
        units: Vec<Entity>,
        node: Entity,
        node_pos: Vec2,
    },
}

/// Counters for the once-per-second sim-tick vs. frame report.
#[derive(Resource, Default)]
pub struct RateReport {
    pub sim_ticks: u32,
    pub frames: u32,
    pub elapsed: f32,
}
