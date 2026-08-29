//! Headless integration tests for M1 selection & command logic.
//!
//! These drive the real systems with a `MinimalPlugins` app (no window/renderer),
//! injecting input and cursor state directly, then asserting on components. They
//! verify the M1 acceptance criteria deterministically and seed M5's determinism
//! checks.

use bevy::input::ButtonInput;
use bevy::prelude::*;

use crate::core::*;
use crate::input::{emit_commands, selection};
use crate::sim::{apply_commands, step_toward};

// ---- harness ---------------------------------------------------------------

fn test_app() -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .init_resource::<ButtonInput<MouseButton>>()
        .init_resource::<ButtonInput<KeyCode>>()
        .init_resource::<CursorWorld>()
        .init_resource::<DragState>()
        .init_resource::<ClickTracker>()
        .init_resource::<CommandQueue>()
        // Note: no `update_cursor` — we set `CursorWorld` directly (no window).
        // `apply_commands` runs in Update here so orders resolve within one frame.
        .add_systems(Update, (selection, emit_commands, apply_commands).chain());
    app
}

fn spawn_unit(app: &mut App, kind: UnitKind, pos: Vec2) -> Entity {
    app.world_mut()
        .spawn((Position(pos), kind, Selectable))
        .id()
}

fn spawn_resource(app: &mut App, pos: Vec2) -> Entity {
    app.world_mut()
        .spawn((Position(pos), ResourceNode { amount: 100 }, Selectable))
        .id()
}

fn set_cursor(app: &mut App, pos: Vec2) {
    app.world_mut().resource_mut::<CursorWorld>().0 = Some(pos);
}

fn set_shift(app: &mut App, shift: bool) {
    let mut keys = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
    keys.clear();
    if shift {
        keys.press(KeyCode::ShiftLeft);
    }
}

/// A single left-click at `pos` (press+release in one frame → a click, not a drag).
fn left_click(app: &mut App, pos: Vec2, shift: bool) {
    {
        let mut mouse = app.world_mut().resource_mut::<ButtonInput<MouseButton>>();
        mouse.clear();
        mouse.press(MouseButton::Left);
        mouse.release(MouseButton::Left);
    }
    set_shift(app, shift);
    set_cursor(app, pos);
    app.update();
    app.world_mut()
        .resource_mut::<ButtonInput<MouseButton>>()
        .clear();
}

/// A left box-drag from `from` to `to` (press one frame, release the next).
fn box_drag(app: &mut App, from: Vec2, to: Vec2, shift: bool) {
    {
        let mut mouse = app.world_mut().resource_mut::<ButtonInput<MouseButton>>();
        mouse.clear();
        mouse.press(MouseButton::Left);
    }
    set_shift(app, shift);
    set_cursor(app, from);
    app.update(); // records drag.start = from

    {
        let mut mouse = app.world_mut().resource_mut::<ButtonInput<MouseButton>>();
        mouse.clear();
        mouse.release(MouseButton::Left);
    }
    set_shift(app, shift);
    set_cursor(app, to);
    app.update(); // resolves the rectangle

    app.world_mut()
        .resource_mut::<ButtonInput<MouseButton>>()
        .clear();
}

fn right_click(app: &mut App, pos: Vec2) {
    {
        let mut mouse = app.world_mut().resource_mut::<ButtonInput<MouseButton>>();
        mouse.clear();
        mouse.press(MouseButton::Right);
        mouse.release(MouseButton::Right);
    }
    set_cursor(app, pos);
    app.update();
    app.world_mut()
        .resource_mut::<ButtonInput<MouseButton>>()
        .clear();
}

fn selected_kinds(app: &mut App) -> Vec<UnitKind> {
    let mut q = app
        .world_mut()
        .query_filtered::<&UnitKind, With<Selected>>();
    q.iter(app.world()).copied().collect()
}

fn is_selected(app: &App, e: Entity) -> bool {
    app.world().get::<Selected>(e).is_some()
}

fn move_target(app: &App, e: Entity) -> Option<Vec2> {
    app.world().get::<MoveTarget>(e).map(|m| m.0)
}

fn has_gather_target(app: &App, e: Entity) -> bool {
    app.world().get::<GatherTarget>(e).is_some()
}

// ---- tests -----------------------------------------------------------------

#[test]
fn click_selects_unit_then_empty_deselects() {
    let mut app = test_app();
    let w = spawn_unit(&mut app, UnitKind::Worker, Vec2::ZERO);

    left_click(&mut app, Vec2::ZERO, false);
    assert!(is_selected(&app, w), "clicking a unit selects it");

    left_click(&mut app, Vec2::new(500.0, 500.0), false);
    assert!(!is_selected(&app, w), "clicking empty ground deselects");
}

#[test]
fn shift_click_adds_and_removes() {
    let mut app = test_app();
    let a = spawn_unit(&mut app, UnitKind::Worker, Vec2::ZERO);
    let b = spawn_unit(&mut app, UnitKind::Soldier, Vec2::new(100.0, 0.0));

    left_click(&mut app, Vec2::ZERO, false);
    left_click(&mut app, Vec2::new(100.0, 0.0), true); // shift-add b
    assert!(is_selected(&app, a) && is_selected(&app, b), "shift adds to selection");

    left_click(&mut app, Vec2::new(100.0, 0.0), true); // shift-remove b
    assert!(is_selected(&app, a) && !is_selected(&app, b), "shift toggles off");
}

#[test]
fn box_drag_selects_units_but_not_resources() {
    let mut app = test_app();
    let inside_a = spawn_unit(&mut app, UnitKind::Worker, Vec2::ZERO);
    let inside_b = spawn_unit(&mut app, UnitKind::Soldier, Vec2::new(50.0, 0.0));
    let outside = spawn_unit(&mut app, UnitKind::Scout, Vec2::new(500.0, 500.0));
    let node = spawn_resource(&mut app, Vec2::new(10.0, 10.0)); // inside the rect

    box_drag(&mut app, Vec2::new(-100.0, -100.0), Vec2::new(100.0, 100.0), false);

    assert!(is_selected(&app, inside_a) && is_selected(&app, inside_b), "units in rect selected");
    assert!(!is_selected(&app, outside), "unit outside rect not selected");
    assert!(!is_selected(&app, node), "resources are not box-selected");
}

#[test]
fn double_click_selects_all_of_type() {
    let mut app = test_app();
    let w1 = spawn_unit(&mut app, UnitKind::Worker, Vec2::ZERO);
    let w2 = spawn_unit(&mut app, UnitKind::Worker, Vec2::new(60.0, 0.0));
    let w3 = spawn_unit(&mut app, UnitKind::Worker, Vec2::new(-60.0, 0.0));
    let s = spawn_unit(&mut app, UnitKind::Soldier, Vec2::new(200.0, 0.0));

    left_click(&mut app, Vec2::ZERO, false); // first click
    left_click(&mut app, Vec2::ZERO, false); // second click on same unit → double

    assert!(
        is_selected(&app, w1) && is_selected(&app, w2) && is_selected(&app, w3),
        "double-click selects every worker"
    );
    assert!(!is_selected(&app, s), "other types are not selected");
    assert_eq!(selected_kinds(&mut app).len(), 3);
}

#[test]
fn right_click_ground_moves_selection() {
    let mut app = test_app();
    let w = spawn_unit(&mut app, UnitKind::Worker, Vec2::ZERO);

    left_click(&mut app, Vec2::ZERO, false);
    right_click(&mut app, Vec2::new(300.0, 0.0));

    assert_eq!(move_target(&app, w), Some(Vec2::new(300.0, 0.0)), "move order sets MoveTarget");
    assert!(!has_gather_target(&app, w), "a move clears any gather intent");
}

#[test]
fn right_click_resource_assigns_gather() {
    let mut app = test_app();
    let w = spawn_unit(&mut app, UnitKind::Worker, Vec2::ZERO);
    let node_pos = Vec2::new(400.0, 0.0);
    spawn_resource(&mut app, node_pos);

    left_click(&mut app, Vec2::ZERO, false);
    right_click(&mut app, node_pos);

    assert_eq!(move_target(&app, w), Some(node_pos), "gather moves the unit to the node");
    assert!(has_gather_target(&app, w), "gather order records the gather target");
}

#[test]
fn step_toward_snaps_on_arrival() {
    // Overshoot within one step → snap to target and report arrival.
    let (pos, arrived) = step_toward(Vec2::ZERO, Vec2::new(1.0, 0.0), 100.0);
    assert!(arrived && pos == Vec2::new(1.0, 0.0));

    // Far away → advance exactly `step` along the direction, not arrived.
    let (pos, arrived) = step_toward(Vec2::ZERO, Vec2::new(100.0, 0.0), 10.0);
    assert!(!arrived);
    assert!((pos.x - 10.0).abs() < 1e-4 && pos.y.abs() < 1e-4);
}
