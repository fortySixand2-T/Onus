//! Presentation: sync transforms from sim, draw selection/drag gizmos, update
//! the options panel, and report tick/frame rates. All read the sim; none change it.

use bevy::prelude::*;

use crate::client::*;
use crate::sim::*;

/// Copy sim-space `Position` into the render `Transform`. One-way, every frame.
pub fn sync_transform(mut query: Query<(&Position, &mut Transform)>) {
    for (pos, mut transform) in &mut query {
        transform.translation.x = pos.0.x;
        transform.translation.y = pos.0.y;
    }
}

/// Give sim-spawned entities (units produced by a building, buildings placed by
/// an order) their presentation: sprite, transform, and click hit-box. The sim
/// creates them render-free; this is the driver catching up in `Update`.
#[allow(clippy::type_complexity)] // a Bevy query filter reads worse as a type alias
pub fn attach_visuals(
    new_units: Query<(Entity, &Position, &UnitKind), (With<UnitDefIdx>, Without<Sprite>)>,
    new_buildings: Query<(Entity, &Position), (With<Building>, Without<Sprite>)>,
    mut commands: Commands,
) {
    for (e, pos, kind) in &new_units {
        let kind = *kind;
        commands.entity(e).insert((
            Selectable,
            Sprite::from_color(unit_color(kind), Vec2::splat(unit_size(kind))),
            Transform::from_translation(pos.0.extend(0.0)),
        ));
    }
    for (e, pos) in &new_buildings {
        commands.entity(e).insert((
            Selectable,
            Sprite::from_color(BUILDING_COLOR, Vec2::splat(BUILDING_SIZE)),
            Transform::from_translation(pos.0.extend(0.0)),
        ));
    }
}

/// Outline selected entities, and draw the box-drag rectangle while dragging.
pub fn draw_selection(
    mut gizmos: Gizmos,
    selected: Query<(&Position, Option<&UnitKind>), With<Selected>>,
    drag: Res<DragState>,
    cursor: Res<CursorWorld>,
    mouse: Res<ButtonInput<MouseButton>>,
) {
    for (pos, kind) in &selected {
        let size = kind.map(|k| unit_size(*k)).unwrap_or(RESOURCE_SIZE) + 6.0;
        gizmos.rect_2d(
            Isometry2d::from_translation(pos.0),
            Vec2::splat(size),
            SELECT_COLOR,
        );
    }

    if mouse.pressed(MouseButton::Left) {
        if let (Some(start), Some(cur)) = (drag.start, cursor.0) {
            let extent = (cur - start).abs();
            if extent.length() > DRAG_THRESHOLD {
                gizmos.rect_2d(
                    Isometry2d::from_translation((start + cur) * 0.5),
                    extent,
                    DRAG_COLOR,
                );
            }
        }
    }
}

/// Show context options: resource details when a node is selected, otherwise a
/// unit-count line. This is the minimal M1 stand-in for a real command card.
pub fn update_options_panel(
    selected_res: Query<&ResourceNode, With<Selected>>,
    selected_units: Query<&UnitKind, With<Selected>>,
    selected_buildings: Query<(&Building, &ProductionQueue), With<Selected>>,
    content: Res<Content>,
    stock: Res<Stockpiles>,
    mut text_q: Query<&mut Text, With<OptionsPanel>>,
) {
    let Ok(mut text) = text_q.single_mut() else {
        return;
    };
    let alloy = format!("Alloy: {}", stock.alloy(PLAYER_FACTION));

    // A selected building shows what it can train and its queue.
    if let Some((building, queue)) = selected_buildings.iter().next() {
        if let Some(def) = content.buildings.get(building.def) {
            let trainable: Vec<String> = def
                .produces
                .iter()
                .enumerate()
                .filter_map(|(i, id)| {
                    let u = content.unit(id)?;
                    Some(format!("[{}] {} ({})", i + 1, u.name, u.mvp_alloy_cost))
                })
                .collect();
            text.0 = format!(
                "{alloy}\n{} selected — {} in production\n{}",
                def.name,
                queue.items.len(),
                trainable.join("   ")
            );
            return;
        }
    }

    if let Some(node) = selected_res.iter().next() {
        text.0 = format!(
            "{alloy}\nResource node selected — {} left\n[G] Gather   [Q] Queue gather",
            node.amount
        );
        return;
    }

    // Summarize the selection by unit type, e.g. "2 Worker, 1 Soldier".
    let (mut workers, mut soldiers, mut scouts) = (0u32, 0u32, 0u32);
    for kind in &selected_units {
        match kind {
            UnitKind::Worker => workers += 1,
            UnitKind::Soldier => soldiers += 1,
            UnitKind::Scout => scouts += 1,
        }
    }
    let total = workers + soldiers + scouts;
    if total == 0 {
        text.0 = String::new();
        return;
    }
    let parts: Vec<String> = [
        (UnitKind::Worker, workers),
        (UnitKind::Soldier, soldiers),
        (UnitKind::Scout, scouts),
    ]
    .into_iter()
    .filter(|(_, n)| *n > 0)
    .map(|(k, n)| format!("{n} {}", k.label()))
    .collect();
    text.0 = format!(
        "{}  selected  —  right-click: move / gather resource",
        parts.join(", ")
    );
}

/// Once per wall-clock second, log sim ticks vs. frames drawn.
pub fn report_rates(time: Res<Time>, mut report: ResMut<RateReport>) {
    report.frames += 1;
    report.elapsed += time.delta_secs();
    if report.elapsed >= 1.0 {
        info!(
            "sim ticks: {} | frames: {}",
            report.sim_ticks, report.frames
        );
        report.sim_ticks = 0;
        report.frames = 0;
        report.elapsed = 0.0;
    }
}
