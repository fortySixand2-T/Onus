//! Client-side state & presentation — everything that is NOT sim truth.
//!
//! Selection markers, input state, and how units look / where you can click
//! them. This layer may use rendering types; the `sim` core may not, and never
//! depends on this module. Both `input` and `ui` build on top of it.

use bevy::prelude::*;

use crate::sim::UnitKind;

// ---- presentation / interaction tunables -----------------------------------

pub const RESOURCE_SIZE: f32 = 34.0;
pub const RESOURCE_COLOR: Color = Color::srgb(0.95, 0.85, 0.20);
pub const SELECT_COLOR: Color = Color::srgb(1.0, 1.0, 1.0);
pub const DRAG_COLOR: Color = Color::srgb(0.50, 0.90, 0.55);

/// Max seconds between two clicks on the same unit to count as a double-click.
pub const DOUBLE_CLICK_SECS: f32 = 0.30;
/// Mouse travel (world units) past which a left-press is a box-drag, not a click.
pub const DRAG_THRESHOLD: f32 = 8.0;

/// Fill color for a unit of the given kind.
pub fn unit_color(kind: UnitKind) -> Color {
    match kind {
        UnitKind::Worker => Color::srgb(0.30, 0.80, 0.90),
        UnitKind::Soldier => Color::srgb(0.90, 0.40, 0.30),
        UnitKind::Scout => Color::srgb(0.60, 0.90, 0.40),
    }
}

/// On-screen (and hit-box) side length for a unit of the given kind.
pub fn unit_size(kind: UnitKind) -> f32 {
    match kind {
        UnitKind::Worker => 20.0,
        UnitKind::Soldier => 26.0,
        UnitKind::Scout => 16.0,
    }
}

// ---- components ------------------------------------------------------------

/// Anything the cursor can pick (units and resource nodes).
#[derive(Component)]
pub struct Selectable;

/// Presentation marker: this entity is currently selected. NOT sim state — the
/// sim never reads it, so selection stays purely client-side.
#[derive(Component)]
pub struct Selected;

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
