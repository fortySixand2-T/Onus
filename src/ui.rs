//! Presentation: sync transforms from sim, draw selection/drag gizmos, update
//! the options panel, and report tick/frame rates. All read the sim; none change it.

use bevy::prelude::*;

use crate::core::*;

/// Copy sim-space `Position` into the render `Transform`. One-way, every frame.
pub fn sync_transform(mut query: Query<(&Position, &mut Transform)>) {
    for (pos, mut transform) in &mut query {
        transform.translation.x = pos.0.x;
        transform.translation.y = pos.0.y;
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
        let size = kind.map(|k| k.size()).unwrap_or(RESOURCE_SIZE) + 6.0;
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
    mut text_q: Query<&mut Text, With<OptionsPanel>>,
) {
    let Ok(mut text) = text_q.single_mut() else {
        return;
    };

    if let Some(node) = selected_res.iter().next() {
        text.0 = format!(
            "Resource node selected — {} left\n[G] Gather   [Q] Queue gather",
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
        info!("sim ticks: {} | frames: {}", report.sim_ticks, report.frames);
        report.sim_ticks = 0;
        report.frames = 0;
        report.elapsed = 0.0;
    }
}
