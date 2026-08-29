//! World setup: camera, the options-panel UI, pre-placed units, and resource
//! nodes. Driver-side (uses render types). Real unit production arrives in M4.

use bevy::prelude::*;

use crate::client::*;
use crate::sim::*;

pub fn setup(mut commands: Commands, content: Res<Content>) {
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

    // Pre-placed player units across three types.
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
    let worker_def = content.unit_index("worker").expect("worker in units.ron");
    for (kind, pos) in placements {
        let e = spawn_unit(&mut commands, kind, pos);
        // Workers are data-driven from `units.ron` and gather for faction A.
        if kind == UnitKind::Worker {
            commands
                .entity(e)
                .insert((UnitDefIdx(worker_def), Faction::A));
        }
    }

    // The player HQ: exists at match start (its cost is not charged), accepts
    // worker deposits, and trains workers. Cost/roster come from the RON.
    let hq_def = content.building_index("hq").expect("hq in units.ron");
    let hq_pos = Vec2::new(-120.0, 120.0);
    commands.spawn((
        Position(hq_pos),
        Building { def: hq_def },
        Faction::A,
        ProductionQueue::default(),
        Selectable,
        Sprite::from_color(BUILDING_COLOR, Vec2::splat(BUILDING_SIZE)),
        Transform::from_translation(hq_pos.extend(0.0)),
    ));

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

fn spawn_unit(commands: &mut Commands, kind: UnitKind, pos: Vec2) -> Entity {
    commands
        .spawn((
            Position(pos),
            kind,
            Selectable,
            Sprite::from_color(unit_color(kind), Vec2::splat(unit_size(kind))),
            Transform::from_translation(pos.extend(0.0)),
        ))
        .id()
}
