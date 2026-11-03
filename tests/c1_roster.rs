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
    let ids: Vec<&str> = a.iter().map(|u| c.units[u.1].id.as_str()).collect();
    assert_eq!(ids, ["ravager", "ripper", "bulwark", "arclight"], "no skirmish workers");
    for (_, d, _, h) in &a {
        let full = Health::from_def(&c, *d).max;
        assert_eq!(h.max, full);
        let want = if c.units[*d].id == "ripper" { 11 } else { full };
        assert_eq!(h.current, want, "{}", c.units[*d].id);
    }
    let b = buildings_of(&mut app, Faction::A);
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
    let b = buildings_of(&mut app, Faction::A);
    let base = b[0].2;
    // The front points from A's base towards the General.
    let front = (gpos - base).normalize();
    let u = units_of(&mut app, Faction::A);
    assert_eq!(u.len(), n);
    let depth = |p: Vec2| (p - base).dot(front);
    let first: Vec<_> = u[..rank].iter().map(|x| x.2).collect();
    let second: Vec<_> = u[rank..].iter().map(|x| x.2).collect();
    for p in &first {
        assert!((depth(*p) - (depth(gpos) - battle.formation_spacing)).abs() < 1e-3);
    }
    for p in &second {
        assert!((depth(*p) - (depth(gpos) - 2.0 * battle.formation_spacing)).abs() < 1e-3);
    }
    for row in [&first, &second] {
        for w in row.windows(2) {
            assert!((w[0].distance(w[1]) - battle.formation_spacing).abs() < 1e-3);
        }
    }
    // Buildings: the first on the base, the rest either side, a spacing apart.
    let across = |p: Vec2| (p - base).perp_dot(front);
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
            .err()
            .expect("refused");
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
