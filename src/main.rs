//! Onus — RTS in Bevy 0.19.
//!
//! M1: input & selection. Units are pre-placed; left-mouse selects (click,
//! box-drag, double-click type-select, shift-toggle); right-mouse commands
//! (move, or gather a resource). Input emits intents; the sim applies them in
//! `FixedUpdate`. See CLAUDE.md for the milestone acceptance criteria.

mod core;
mod input;
mod sim;
mod ui;

#[cfg(test)]
mod tests;

use bevy::prelude::*;

use crate::core::*;

fn main() {
    App::new()
        .add_plugins(DefaultPlugins)
        .insert_resource(Time::<Fixed>::from_hz(60.0))
        .init_resource::<CursorWorld>()
        .init_resource::<DragState>()
        .init_resource::<ClickTracker>()
        .init_resource::<CommandQueue>()
        .init_resource::<RateReport>()
        .add_systems(Startup, setup)
        .add_systems(
            Update,
            (
                input::update_cursor,
                input::selection.after(input::update_cursor),
                input::emit_commands.after(input::update_cursor),
                ui::sync_transform,
                ui::draw_selection,
                ui::update_options_panel,
                ui::report_rates,
            ),
        )
        .add_systems(FixedUpdate, (sim::apply_commands, sim::movement).chain())
        .run();
}

fn setup(mut commands: Commands) {
    commands.spawn(Camera2d);

    // Options / status panel (bottom-left).
    commands.spawn((
        Text::new(""),
        TextFont {
            font_size: bevy::text::FontSize::Px(16.0),
            ..default()
        },
        TextColor(Color::srgb(0.90, 0.90, 0.90)),
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(12.0),
            bottom: Val::Px(12.0),
            ..default()
        },
        OptionsPanel,
    ));

    // Pre-placed player units across three types (real spawning arrives in M4).
    let placements = [
        (UnitKind::Worker, Vec2::new(-220.0, 60.0)),
        (UnitKind::Worker, Vec2::new(-170.0, 30.0)),
        (UnitKind::Worker, Vec2::new(-210.0, -10.0)),
        (UnitKind::Soldier, Vec2::new(70.0, -40.0)),
        (UnitKind::Soldier, Vec2::new(120.0, -80.0)),
        (UnitKind::Soldier, Vec2::new(160.0, -20.0)),
        (UnitKind::Scout, Vec2::new(-40.0, 170.0)),
        (UnitKind::Scout, Vec2::new(10.0, 200.0)),
    ];
    for (kind, pos) in placements {
        spawn_unit(&mut commands, kind, pos);
    }

    // Resource nodes.
    for pos in [Vec2::new(300.0, 180.0), Vec2::new(-320.0, -170.0)] {
        commands.spawn((
            Position(pos),
            ResourceNode { amount: 1500 },
            Selectable,
            Sprite::from_color(RESOURCE_COLOR, Vec2::splat(RESOURCE_SIZE)),
            Transform::from_translation(pos.extend(0.0)),
        ));
    }
}

fn spawn_unit(commands: &mut Commands, kind: UnitKind, pos: Vec2) {
    commands.spawn((
        Position(pos),
        kind,
        Selectable,
        Sprite::from_color(kind.color(), Vec2::splat(kind.size())),
        Transform::from_translation(pos.extend(0.0)),
    ));
}
