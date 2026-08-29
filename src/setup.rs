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

    // Pre-placed player units. Each is a real roster entry from `units.ron`, so
    // it carries that unit's stats — speed, HP pool, damage (M4b) — rather than
    // a bare silhouette.
    let placements = [
        ("worker", Vec2::new(-220.0, 60.0)),
        ("worker", Vec2::new(-170.0, 30.0)),
        ("worker", Vec2::new(-210.0, -10.0)),
        ("bulwark", Vec2::new(70.0, -40.0)),
        ("bulwark", Vec2::new(120.0, -80.0)),
        ("bulwark", Vec2::new(160.0, -20.0)),
        ("sentinel", Vec2::new(-40.0, 170.0)),
        ("sentinel", Vec2::new(10.0, 200.0)),
    ];
    for (id, pos) in placements {
        spawn_unit(&mut commands, &content, id, PLAYER_FACTION, pos);
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

/// Spawn the roster unit `id` for `faction`. The sim half (definition index,
/// kind, faction, HP pool) is data from `units.ron`; only the sprite is
/// presentation.
fn spawn_unit(
    commands: &mut Commands,
    content: &Content,
    id: &str,
    faction: Faction,
    pos: Vec2,
) -> Entity {
    let def = content
        .unit_index(id)
        .unwrap_or_else(|| panic!("`{id}` in units.ron"));
    let kind = content.units[def].mvp_kind;
    commands
        .spawn((
            Position(pos),
            UnitDefIdx(def),
            kind,
            faction,
            Health::from_def(content, def),
            Selectable,
            Sprite::from_color(unit_color(kind), Vec2::splat(unit_size(kind))),
            Transform::from_translation(pos.extend(0.0)),
        ))
        .id()
}
