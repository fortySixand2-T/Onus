//! Client-side state & presentation — everything that is NOT sim truth.
//!
//! Selection markers, input state, and how units look / where you can click
//! them. This layer may use rendering types; the `sim` core may not, and never
//! depends on this module. Both `input` and `ui` build on top of it.

use bevy::prelude::*;

use crate::sim::spatial::Faction;
use crate::sim::{MatchOutcome, UnitKind};

// ---- presentation / interaction tunables -----------------------------------

pub const RESOURCE_SIZE: f32 = 34.0;
pub const RESOURCE_COLOR: Color = Color::srgb(0.95, 0.85, 0.20);
/// The side the local player commands (1v1 mirror; the AI takes the other).
pub const PLAYER_FACTION: Faction = Faction::A;

/// Hotkeys for placing buildings, paired positionally with the *placeable*
/// buildings of `units.ron` (those that are not the starting drop-off). Keeping
/// the roster in data means adding a barracks needs no code change here.
pub const PLACE_KEYS: [KeyCode; 3] = [KeyCode::KeyB, KeyCode::KeyN, KeyCode::KeyM];

/// Hotkeys for training the Nth unit of a selected building's `produces` list.
pub const TRAIN_KEYS: [KeyCode; 3] = [KeyCode::Digit1, KeyCode::Digit2, KeyCode::Digit3];

pub const BUILDING_SIZE: f32 = 56.0;
pub const BUILDING_COLOR: Color = Color::srgb(0.55, 0.60, 0.75);
pub const SELECT_COLOR: Color = Color::srgb(1.0, 1.0, 1.0);
pub const DRAG_COLOR: Color = Color::srgb(0.50, 0.90, 0.55);

/// Max seconds between two clicks on the same unit to count as a double-click.
pub const DOUBLE_CLICK_SECS: f32 = 0.30;
/// Mouse travel (world units) past which a left-press is a box-drag, not a click.
pub const DRAG_THRESHOLD: f32 = 8.0;

// ---- match result (presentation) -------------------------------------------

/// What the panel says when the match is over. Wording is presentation, so it
/// lives here with the colours and hotkeys rather than in the system that
/// renders it — and the three outcomes are three constants because a draw is a
/// *distinct* result, not a loss with a different sign.
pub const RESULT_WIN: &str = "VICTORY — the enemy HQ is destroyed";
pub const RESULT_LOSS: &str = "DEFEAT — your HQ is destroyed";
pub const RESULT_DRAW: &str = "DRAW — both HQs fell on the same tick";

/// The line that explains the thing a player actually notices: the game has
/// stopped taking orders. Without it a decided match is indistinguishable from
/// a hung one — which is exactly how this defect felt from the player's chair.
pub const RESULT_INPUT_OFF: &str = "The match is over — orders are no longer accepted.";

/// The panel's text for a decided match.
///
/// `timestep_secs` is the sim's **fixed** timestep, passed in rather than
/// assumed: elapsed match time is `tick × timestep`, arithmetic on the sim's own
/// tick count, and no clock is read to produce it. (A wall clock in the driver
/// is fine for a filename — Phase 2 — but display state derived from sim state
/// must stay a function of sim state, or two runs of one replay would describe
/// themselves differently.)
///
/// Pure, so it is unit-testable without an `App`.
pub fn match_result_text(outcome: MatchOutcome, timestep_secs: f32) -> String {
    let headline = match outcome.winner {
        Some(f) if f == PLAYER_FACTION => RESULT_WIN,
        Some(_) => RESULT_LOSS,
        None => RESULT_DRAW,
    };
    format!(
        "{headline}\nDecided at {} (tick {}). {RESULT_INPUT_OFF}",
        match_clock(outcome.tick, timestep_secs),
        outcome.tick
    )
}

/// `m:ss` of match time for a tick count, at the sim's fixed timestep.
///
/// The product is taken in `f64` and **rounded to the millisecond** before the
/// truncation to seconds, because a timestep is rarely exact in binary: at
/// 30 Hz, `120 * (1.0/30.0)` is 3.9999998, and truncating that reports a
/// four-second match as three. Round first, truncate second — the same "do not
/// let a float's last bit decide an integer" rule the damage path follows
/// (F-005/F-006), applied to a display figure.
pub fn match_clock(tick: u32, timestep_secs: f32) -> String {
    let millis = (tick as f64 * timestep_secs as f64 * 1000.0).round().max(0.0) as u64;
    let seconds = millis / 1000;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

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
