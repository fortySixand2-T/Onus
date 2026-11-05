//! C1 AC5 — **battle setup from a roster**: a side can start with a given army
//! (its General, its units and their HP) and given buildings (with their HP —
//! siege damage persists), instead of the skirmish start (CAMPAIGN_PLAN C1).
//! Layout numbers come from the loaded content.

use bevy::prelude::*;

use onus::headless::{self, MatchSettings, MatchSetupError};
use onus::sim::battle::{BattleSetup, BattleSetupError, BattleState, General, LossCause, Roster};
use onus::sim::combat::Health;
use onus::sim::content::Content;
use onus::sim::economy::{Building, UnitDefIdx};
use onus::sim::spatial::Faction;
use onus::sim::{AiCommanders, MatchState, MoveTarget, Position};

fn content() -> Content {
    headless::content().expect("content loads")
}

fn quiet(setup: BattleSetup) -> App {
    let mut app = headless::ai_vs_ai(content(), &MatchSettings::default().with_battle(setup))
        .expect("battle builds");
    app.insert_resource(AiCommanders::default());
    app
}

/// (entity, def, position, health) of `side`'s non-General units, entity order.
fn units_of(app: &mut App, side: Faction) -> Vec<(Entity, usize, Vec2, Health)> {
    let mut q = app.world_mut().query_filtered::<
        (Entity, &Faction, &UnitDefIdx, &Position, &Health),
        Without<General>,
    >();
    let mut v: Vec<_> = q
        .iter(app.world())
        .filter(|(_, f, ..)| **f == side)
        .map(|(e, _, d, p, h)| (e, d.0, p.0, *h))
        .collect();
    v.sort_by_key(|(e, ..)| e.to_bits());
    v
}

/// (entity, def, position, health if any) of `side`'s buildings.
fn buildings_of(app: &mut App, side: Faction) -> Vec<(Entity, usize, Vec2, Option<Health>)> {
    let mut q = app
        .world_mut()
        .query::<(Entity, &Faction, &Building, &Position, Option<&Health>)>();
    let mut v: Vec<_> = q
        .iter(app.world())
        .filter(|(_, f, ..)| **f == side)
        .map(|(e, _, b, p, h)| (e, b.def, p.0, h.copied()))
        .collect();
    v.sort_by_key(|(e, ..)| e.to_bits());
    v
}

fn general_of(app: &mut App, side: Faction) -> (Entity, Vec2) {
    let mut q = app
        .world_mut()
        .query_filtered::<(Entity, &Faction, &Position), With<General>>();
    q.iter(app.world())
        .find(|(_, f, _)| **f == side)
        .map(|(e, _, p)| (e, p.0))
        .unwrap()
}

fn army() -> Roster {
    Roster::default()
        .unit("ravager")
        .unit_hp("ripper", 11)
        .unit("bulwark")
        .unit("arclight")
        .building_hp("hq", 900)
        .building("foundry")
}

#[test]
fn a_roster_side_starts_with_exactly_its_roster_and_no_skirmish_start() {
    let c = content();
    let setup = BattleSetup::default()
        .with_general(Faction::A, "general_focus")
        .with_roster(Faction::A, army())
        .with_general(Faction::B, "general_bulwark");
    let mut app = quiet(setup);
    let a = units_of(&mut app, Faction::A);
    let mut ids: Vec<&str> = a.iter().map(|u| c.units[u.1].id.as_str()).collect();
    ids.sort();
    assert_eq!(ids, ["arclight", "bulwark", "ravager", "ripper"], "no skirmish workers");
    for (_, d, _, h) in &a {
        let full = Health::from_def(&c, *d).max;
        assert_eq!(h.max, full);
        let want = if c.units[*d].id == "ripper" { 11 } else { full };
        assert_eq!(h.current, want, "{}", c.units[*d].id);
    }
    let mut b = buildings_of(&mut app, Faction::A);
    b.sort_by_key(|x| c.buildings[x.1].id != "hq");
    let ids: Vec<&str> = b.iter().map(|x| c.buildings[x.1].id.as_str()).collect();
    assert_eq!(ids, ["hq", "foundry"], "the roster's buildings, and no other HQ");
    let hq_pool = Health::from_building_def(&c, b[0].1).max;
    assert_eq!(b[0].3, Some(Health { current: 900, max: hq_pool }), "siege damage carried");
    assert_eq!(b[1].3, None, "a building at full HP takes its pool from the RON");
    // B, with no roster, has today's skirmish start.
    let worker = c.unit_index("worker").unwrap();
    assert!(units_of(&mut app, Faction::B).iter().all(|u| u.1 == worker));
    assert!(!units_of(&mut app, Faction::B).is_empty());
    assert_eq!(buildings_of(&mut app, Faction::B).len(), 1);
    // Nobody starts with an order.
    for (e, ..) in a {
        assert!(app.world().get::<MoveTarget>(e).is_none());
    }
}

#[test]
fn a_roster_stands_in_ranks_behind_its_generals_and_its_buildings_in_a_line() {
    let c = content();
    let battle = c.battle.clone().unwrap();
    let rank = battle.formation_rank as usize;
    let n = rank + 3; // one full rank and part of a second
    let mut roster = Roster::default().building("hq").building("foundry").building("gene_vats");
    for _ in 0..n {
        roster = roster.unit("ravager");
    }
    let setup = BattleSetup::default()
        .with_general(Faction::A, "general_focus")
        .with_roster(Faction::A, roster);
    let mut app = quiet(setup);
    let (_, gpos) = general_of(&mut app, Faction::A);
    // The roster's first building (the HQ) stands on the base.
    let mut b = buildings_of(&mut app, Faction::A);
    b.sort_by_key(|x| c.buildings[x.1].id != "hq");
    let base = b[0].2;
    // The front points from A's base towards the General.
    let front = (gpos - base).normalize();
    let u = units_of(&mut app, Faction::A);
    assert_eq!(u.len(), n);
    let depth = |p: Vec2| (p - base).dot(front);
    let across = |p: Vec2| (p - base).perp_dot(front);
    let in_rank = |r: f32| {
        let mut row: Vec<Vec2> = u
            .iter()
            .map(|x| x.2)
            .filter(|p| (depth(*p) - (depth(gpos) - r * battle.formation_spacing)).abs() < 1e-3)
            .collect();
        row.sort_by(|a, b| across(*a).total_cmp(&across(*b)));
        row
    };
    let (first, second) = (in_rank(1.0), in_rank(2.0));
    assert_eq!((first.len(), second.len()), (rank, n - rank), "a full rank, then the rest");
    for row in [&first, &second] {
        for w in row.windows(2) {
            assert!((w[0].distance(w[1]) - battle.formation_spacing).abs() < 1e-3);
        }
        let mid = row.iter().map(|p| across(*p)).sum::<f32>() / row.len() as f32;
        assert!(mid.abs() < 1e-3, "each rank is centred on the base's axis");
    }
    // Buildings: the first on the base, the rest either side, a spacing apart.
    assert!(depth(b[1].2).abs() < 1e-3 && depth(b[2].2).abs() < 1e-3);
    assert!((across(b[1].2).abs() - battle.building_spacing).abs() < 1e-3);
    assert!((across(b[2].2).abs() - battle.building_spacing).abs() < 1e-3);
    assert!(across(b[1].2) * across(b[2].2) < 0.0, "either side of the base");
}

#[test]
fn a_bad_roster_is_refused_before_anything_spawns() {
    let c = content();
    let rip = c.unit_index("ripper").unwrap();
    let rip_max = Health::from_def(&c, rip).max;
    let cases: Vec<(Roster, BattleSetupError)> = vec![
        (
            Roster::default().unit("dragon"),
            BattleSetupError::UnknownUnit { id: "dragon".into(), side: Faction::B },
        ),
        (
            Roster::default().unit("general_focus"),
            BattleSetupError::UnknownUnit { id: "general_focus".into(), side: Faction::B },
        ),
        (
            Roster::default().building("castle"),
            BattleSetupError::UnknownBuilding { id: "castle".into(), side: Faction::B },
        ),
        (
            Roster::default().unit_hp("ripper", 0),
            BattleSetupError::BadHp { id: "ripper".into(), side: Faction::B, hp: 0, max: rip_max },
        ),
        (
            Roster::default().unit_hp("ripper", rip_max + 1),
            BattleSetupError::BadHp {
                id: "ripper".into(),
                side: Faction::B,
                hp: rip_max + 1,
                max: rip_max,
            },
        ),
    ];
    for (roster, want) in cases {
        let setup = BattleSetup::default().with_roster(Faction::B, roster);
        let err = headless::battle(content(), &MatchSettings::default().with_battle(setup))
            .expect_err("refused");
        assert_eq!(err, MatchSetupError::Battle(want));
    }
}

#[test]
fn a_field_battle_with_no_buildings_is_decided_by_the_generals() {
    let setup = BattleSetup::default()
        .with_general(Faction::A, "general_focus")
        .with_roster(Faction::A, Roster::default().unit("ravager"))
        .with_general(Faction::B, "general_bulwark")
        .with_roster(Faction::B, Roster::default().unit("ravager"));
    let mut app = quiet(setup);
    headless::tick(&mut app, 30);
    assert!(app.world().resource::<MatchState>().outcome().is_none(), "no HQ is not a loss");
    let (gb, _) = general_of(&mut app, Faction::B);
    app.world_mut().despawn(gb);
    headless::step(&mut app);
    let r = app.world().resource::<BattleState>().result().cloned().expect("decided");
    assert_eq!(r.winner, Some(Faction::A));
    assert_eq!(r.loss(Faction::B), Some(LossCause::GeneralsFell));
}

#[test]
fn a_damaged_city_falls_sooner() {
    let c = content();
    let setup = BattleSetup::default()
        .with_general(Faction::A, "general_focus")
        .with_roster(Faction::A, Roster::default().building_hp("hq", 1))
        .with_general(Faction::B, "general_bulwark");
    let mut app = quiet(setup);
    let hq = buildings_of(&mut app, Faction::A)[0].2;
    let idx = c.unit_index("ravager").unwrap();
    let hp = Health::from_def(&c, idx);
    app.world_mut().spawn((
        Position(hq + Vec2::new(0.0, -20.0)),
        UnitDefIdx(idx),
        c.units[idx].mvp_kind,
        Faction::B,
        hp,
    ));
    let mut t = 0;
    while app.world().resource::<MatchState>().outcome().is_none() && t < 120 {
        headless::step(&mut app);
        t += 1;
    }
    let r = app.world().resource::<BattleState>().result().cloned().expect("decided");
    assert_eq!(r.loss(Faction::A), Some(LossCause::VictoryBuildingLost));
    assert_eq!(r.winner, Some(Faction::B));
}

/// A roster too big for `formation_rank`-wide ranks between the Generals and
/// the entry edge wraps into wider ranks instead of spilling off the field:
/// every unit starts at least one `formation_spacing` inside its side's edge,
/// on a spot of its own.
#[test]
fn a_large_roster_starts_entirely_on_the_field() {
    let c = content();
    let b = c.battle.as_ref().unwrap();
    for n in [8usize, 49, 60, 100, 250] {
        let roster = (0..n).fold(Roster::default().building("hq"), |r, _| r.unit("ravager"));
        let setup = BattleSetup::default()
            .with_general(Faction::A, "general_focus")
            .with_roster(Faction::A, roster)
            .with_general(Faction::B, "general_bulwark");
        let mut app = quiet(setup);
        let edge = app.world().resource::<BattleState>().edge(Faction::A);
        let units = units_of(&mut app, Faction::A);
        assert_eq!(units.len(), n);
        for (_, _, p, _) in &units {
            assert!(
                edge.beyond(*p) <= -b.formation_spacing + 1e-3,
                "{n} units: one starts at {p}, {} from the edge",
                edge.beyond(*p)
            );
        }
        let mut spots: Vec<(i64, i64)> = units
            .iter()
            .map(|(_, _, p, _)| ((p.x * 16.0).round() as i64, (p.y * 16.0).round() as i64))
            .collect();
        spots.sort();
        spots.dedup();
        assert_eq!(spots.len(), n, "{n} units: two share a spot");
    }
}

/// The shipped data with the battle layout's three distances replaced.
fn tight_content(tag: &str, front: &str, spacing: &str, offset: &str) -> Result<Content, String> {
    let src = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data");
    let out = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/c1_roster")
        .join(tag);
    std::fs::create_dir_all(&out).unwrap();
    for f in ["resources.ron", "strategies.ron"] {
        std::fs::copy(src.join(f), out.join(f)).unwrap();
    }
    let text = std::fs::read_to_string(src.join("units.ron")).unwrap();
    let edited = text
        .replacen("general_front: 80.0,", &format!("general_front: {front},"), 1)
        .replacen("formation_spacing: 30.0,", &format!("formation_spacing: {spacing},"), 1)
        .replacen("entry_edge_offset: 150.0,", &format!("entry_edge_offset: {offset},"), 1);
    for (k, v) in [("general_front", front), ("formation_spacing", spacing), ("entry_edge_offset", offset)] {
        assert!(edited.contains(&format!("{k}: {v},")), "{tag}: {k} not replaced");
    }
    std::fs::write(out.join("units.ron"), edited).unwrap();
    Content::load_from_dir(&out).map_err(|e| e.to_string())
}

/// A layout with no room for one rank a spacing inside the entry edge
/// (`general_front + entry_edge_offset < 2 * formation_spacing`) is refused
/// at load, naming the fields, so no roster can ever start off the field.
#[test]
fn a_layout_with_no_room_for_a_rank_is_refused_at_load() {
    let err = tight_content("tight", "10.0", "30.0", "10.0").expect_err("front 10 + offset 10 < 2 x 30");
    for field in ["general_front", "entry_edge_offset", "formation_spacing"] {
        assert!(err.contains(field), "{err}");
    }
    // Just short of the boundary is refused too.
    tight_content("short", "20.0", "30.0", "39.5").expect_err("59.5 < 60");
}

/// At the boundary (`general_front + entry_edge_offset == 2 * formation_spacing`)
/// the layout loads, and every roster unit, however many, stands at least one
/// spacing inside its side's edge.
#[test]
fn a_layout_exactly_two_spacings_deep_loads_and_keeps_every_unit_on_the_field() {
    let c = tight_content("boundary", "20.0", "30.0", "40.0").expect("20 + 40 == 2 x 30 loads");
    let spacing = c.battle.as_ref().unwrap().formation_spacing;
    for n in [1usize, 8, 30] {
        let roster = (0..n).fold(Roster::default().building("hq"), |r, _| r.unit("ravager"));
        let setup = BattleSetup::default()
            .with_general(Faction::A, "general_focus")
            .with_roster(Faction::A, roster)
            .with_general(Faction::B, "general_bulwark");
        let mut app = headless::ai_vs_ai(c.clone(), &MatchSettings::default().with_battle(setup))
            .expect("battle builds");
        app.insert_resource(AiCommanders::default());
        let edge = app.world().resource::<BattleState>().edge(Faction::A);
        let units = units_of(&mut app, Faction::A);
        assert_eq!(units.len(), n);
        for (_, _, p, _) in &units {
            assert!(
                edge.beyond(*p) <= -spacing + 1e-3,
                "{n} units: one starts at {p}, {} from the edge",
                edge.beyond(*p)
            );
        }
    }
}
