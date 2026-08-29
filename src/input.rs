//! Input → intent. Reads mouse/keyboard and the world cursor, updates the
//! client-side selection, and pushes sim orders onto the [`CommandQueue`].
//! Nothing here mutates sim state (`Position`/`MoveTarget`) directly.

use bevy::prelude::*;

use crate::client::*;
use crate::sim::*;

/// Resolve the OS cursor to a world position via the 2D camera.
pub fn update_cursor(
    windows: Query<&Window>,
    camera_q: Query<(&Camera, &GlobalTransform)>,
    mut cursor: ResMut<CursorWorld>,
) {
    let (Ok(window), Ok((camera, cam_tf))) = (windows.single(), camera_q.single()) else {
        cursor.0 = None;
        return;
    };
    cursor.0 = window
        .cursor_position()
        .and_then(|p| camera.viewport_to_world_2d(cam_tf, p).ok());
}

/// Half-extent of a selectable's hit box (unit size, or resource size).
fn half_extent(kind: Option<&UnitKind>) -> f32 {
    kind.map(|k| unit_size(*k)).unwrap_or(RESOURCE_SIZE) * 0.5
}

fn within(cursor: Vec2, center: Vec2, half: f32) -> bool {
    let d = (cursor - center).abs();
    d.x <= half && d.y <= half
}

fn in_rect(p: Vec2, min: Vec2, max: Vec2) -> bool {
    p.x >= min.x && p.x <= max.x && p.y >= min.y && p.y <= max.y
}

fn clear_selection(commands: &mut Commands, selected: &Query<Entity, With<Selected>>) {
    for e in selected {
        commands.entity(e).remove::<Selected>();
    }
}

type SelectableItem<'a> = (Entity, &'a Position, Option<&'a UnitKind>, Option<&'a ResourceNode>);

/// Topmost selectable under the cursor: the containing box with the nearest center.
fn pick(
    selectables: &Query<SelectableItem, With<Selectable>>,
    cursor: Vec2,
) -> Option<(Entity, Option<UnitKind>)> {
    let mut best: Option<(Entity, Option<UnitKind>, f32)> = None;
    for (e, pos, kind, _res) in selectables {
        if within(cursor, pos.0, half_extent(kind)) {
            let d = pos.0.distance_squared(cursor);
            if best.is_none_or(|(_, _, bd)| d < bd) {
                best = Some((e, kind.copied(), d));
            }
        }
    }
    best.map(|(e, k, _)| (e, k))
}

/// Left-mouse selection: click, box-drag, double-click type-select, shift-toggle.
#[allow(clippy::too_many_arguments)] // a Bevy system's params are its dependency injection
pub fn selection(
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    time: Res<Time>,
    cursor: Res<CursorWorld>,
    mut drag: ResMut<DragState>,
    mut clicks: ResMut<ClickTracker>,
    selectables: Query<SelectableItem, With<Selectable>>,
    selected_q: Query<Entity, With<Selected>>,
    mut commands: Commands,
) {
    let Some(cur) = cursor.0 else { return };
    let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);

    if mouse.just_pressed(MouseButton::Left) {
        drag.start = Some(cur);
    }

    if !mouse.just_released(MouseButton::Left) {
        return;
    }

    let start = drag.start.take().unwrap_or(cur);
    let min = start.min(cur);
    let max = start.max(cur);
    let is_drag = (max - min).length() > DRAG_THRESHOLD;

    if is_drag {
        // Box-select: own units whose center falls inside the rectangle.
        if !shift {
            clear_selection(&mut commands, &selected_q);
        }
        for (e, pos, kind, _res) in &selectables {
            if kind.is_some() && in_rect(pos.0, min, max) {
                commands.entity(e).insert(Selected);
            }
        }
        return;
    }

    // Single click.
    match pick(&selectables, cur) {
        Some((e, kind)) => {
            let now = time.elapsed_secs();
            let is_double =
                clicks.last_entity == Some(e) && (now - clicks.last_time) < DOUBLE_CLICK_SECS;
            clicks.last_time = now;
            clicks.last_entity = Some(e);

            if shift {
                // Shift takes precedence over double-click: an explicit per-unit
                // toggle, so rapid shift-clicks never turn into a type-select.
                if selected_q.get(e).is_ok() {
                    commands.entity(e).remove::<Selected>();
                } else {
                    commands.entity(e).insert(Selected);
                }
            } else if is_double && kind.is_some() {
                // Select all units of this type. The camera is static in M1, so
                // every unit is on-screen; add a viewport filter when panning lands.
                let target = kind.unwrap();
                clear_selection(&mut commands, &selected_q);
                for (e2, _pos, kind2, _res) in &selectables {
                    if kind2.copied() == Some(target) {
                        commands.entity(e2).insert(Selected);
                    }
                }
            } else {
                clear_selection(&mut commands, &selected_q);
                commands.entity(e).insert(Selected);
            }
        }
        None => {
            if !shift {
                clear_selection(&mut commands, &selected_q);
            }
        }
    }
}

/// Right-mouse command: move the selected units, or gather if a resource is under
/// the cursor. Emits an [`Order`]; the sim applies it in `FixedUpdate`.
pub fn emit_commands(
    mouse: Res<ButtonInput<MouseButton>>,
    cursor: Res<CursorWorld>,
    selectables: Query<SelectableItem, With<Selectable>>,
    selected_units: Query<Entity, (With<Selected>, With<UnitKind>)>,
    mut queue: ResMut<CommandQueue>,
) {
    if !mouse.just_pressed(MouseButton::Right) {
        return;
    }
    let Some(cur) = cursor.0 else { return };

    let units: Vec<Entity> = selected_units.iter().collect();
    if units.is_empty() {
        return;
    }

    // Resource under the cursor?
    let mut node = None;
    for (e, pos, kind, res) in &selectables {
        if kind.is_none() && res.is_some() && within(cur, pos.0, RESOURCE_SIZE * 0.5) {
            node = Some((e, pos.0));
            break;
        }
    }

    match node {
        Some((node, node_pos)) => queue.0.push_back(Order::Gather {
            units,
            node,
            node_pos,
        }),
        None => queue.0.push_back(Order::MoveTo { units, dest: cur }),
    }
}
