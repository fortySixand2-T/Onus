//! World setup: camera, the options-panel UI, pre-placed units, and resource
//! nodes. Driver-side (uses render types). Real unit production arrives in M4.

use bevy::prelude::*;

use crate::client::*;
use crate::sim::*;

/// Seed of the shipped match. Fixed, so the campaign's first skirmish plays the
/// same way every launch; M5 will make it a replay input.
pub const MATCH_SEED: u64 = 20_260_829;

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
        ("worker", Vec2::new(-380.0, 60.0)),
        ("worker", Vec2::new(-330.0, 30.0)),
        ("worker", Vec2::new(-370.0, -10.0)),
        ("bulwark", Vec2::new(-230.0, -40.0)),
        ("bulwark", Vec2::new(-180.0, -80.0)),
        ("bulwark", Vec2::new(-140.0, -20.0)),
        ("sentinel", Vec2::new(-260.0, 170.0)),
        ("sentinel", Vec2::new(-210.0, 200.0)),
    ];
    for (id, pos) in placements {
        spawn_unit(&mut commands, &content, id, PLAYER_FACTION, pos);
    }

    // The two HQs: they exist at match start (their cost is not charged), accept
    // worker deposits, train workers — and are the win condition (M4c): destroy
    // the enemy's and the match is over.
    let hq_pos = Vec2::new(-420.0, 120.0);
    spawn_hq(&mut commands, &content, Faction::A, hq_pos);

    // The opponent: an HQ, its own workers, and a scripted commander. This is
    // what makes the shipped app a match rather than a sandbox.
    let enemy_hq = Vec2::new(420.0, -120.0);
    spawn_hq(&mut commands, &content, Faction::B, enemy_hq);
    for i in 0..3 {
        spawn_unit(
            &mut commands,
            &content,
            "worker",
            Faction::B,
            enemy_hq + Vec2::new(40.0, 30.0 * i as f32 - 30.0),
        );
    }
    commands.insert_resource(AiCommanders::new(MATCH_SEED, &[Faction::B]));

    // Resource nodes — one within reach of each base, plus a contested middle.
    for pos in [
        hq_pos + Vec2::new(180.0, -60.0),
        enemy_hq + Vec2::new(-180.0, 60.0),
        Vec2::new(0.0, 260.0),
    ] {
        commands.spawn((
            Position(pos),
            ResourceNode { amount: 1500 },
            Selectable,
            Sprite::from_color(RESOURCE_COLOR, Vec2::splat(RESOURCE_SIZE)),
            Transform::from_translation(pos.extend(0.0)),
        ));
    }
}

/// Spawn a faction's HQ: sim state (definition, faction, production queue and
/// the HP pool that makes it destroyable) plus its sprite.
fn spawn_hq(commands: &mut Commands, content: &Content, faction: Faction, pos: Vec2) {
    let hq_def = content.building_index("hq").expect("hq in units.ron");
    commands.spawn((
        Position(pos),
        Building { def: hq_def },
        faction,
        ProductionQueue::default(),
        Health::from_building_def(content, hq_def),
        Selectable,
        Sprite::from_color(BUILDING_COLOR, Vec2::splat(BUILDING_SIZE)),
        Transform::from_translation(pos.extend(0.0)),
    ));
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
