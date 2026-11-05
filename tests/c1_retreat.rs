//! C1 AC4 — **retreat**: an `Order`. The side's units head for its entry edge,
//! and the battle ends as a retreat once the withdrawal finishes; the outcome
//! records which units got off the field (CAMPAIGN_PLAN C1).

use bevy::prelude::*;

use onus::headless::{self, MatchSettings};
use onus::sim::battle::{BattleSetup, BattleState, General, LossCause};
use onus::sim::combat::Health;
use onus::sim::content::Content;
use onus::sim::economy::{Building, ProductionQueue, UnitDefIdx};
use onus::sim::replay::{CommandLog, LoggedOrder, SimId};
use onus::sim::spatial::Faction;
use onus::sim::{AiCommanders, CommandQueue, MatchState, MoveTarget, Order, Position};

fn content() -> Content {
    headless::content().expect("content loads")
}

fn setup() -> BattleSetup {
    BattleSetup::default()
        .with_general(Faction::A, "general_logistics")
        .with_general(Faction::B, "general_bulwark")
}

fn quiet_battle() -> App {
    let mut app = headless::ai_vs_ai(content(), &MatchSettings::default().with_battle(setup()))
        .expect("battle builds");
    app.insert_resource(AiCommanders::default());
    app
}

fn order(app: &mut App, o: Order) {
    app.world_mut().resource_mut::<CommandQueue>().0.push_back(o);
}

/// Every unit of `side` on the field: (entity, def, hp, sim id), entity order.
fn units_of(app: &mut App, side: Faction) -> Vec<(Entity, usize, Health, Option<SimId>)> {
    let mut q = app
        .world_mut()
        .query::<(Entity, &Faction, &UnitDefIdx, &Health, Option<&SimId>)>();
    let mut v: Vec<_> = q
        .iter(app.world())
        .filter(|(_, f, ..)| **f == side)
        .map(|(e, _, d, h, id)| (e, d.0, *h, id.copied()))
        .collect();
    v.sort_by_key(|(e, ..)| e.to_bits());
    v
}

fn buildings_of(app: &mut App, side: Faction) -> usize {
    let mut q = app.world_mut().query::<(&Building, &Faction)>();
    q.iter(app.world()).filter(|(_, f)| **f == side).count()
}

fn spawn_unit(app: &mut App, id: &str, faction: Faction, pos: Vec2) -> Entity {
    let (idx, kind, hp) = {
        let c = app.world().resource::<Content>();
        let idx = c.unit_index(id).unwrap();
        (idx, c.units[idx].mvp_kind, Health::from_def(c, idx))
    };
    app.world_mut()
        .spawn((Position(pos), UnitDefIdx(idx), kind, faction, hp))
        .id()
}

fn run_to_outcome(app: &mut App, cap: u32) {
    for _ in 0..cap {
        if app.world().resource::<MatchState>().outcome().is_some() {
            return;
        }
        headless::step(app);
    }
    panic!("no outcome within {cap} ticks");
}

#[test]
fn a_retreat_walks_the_whole_side_off_its_entry_edge_and_ends_the_battle() {
    let mut app = quiet_battle();
    // A soldier in the middle of the map, and a wounded one: both must get out.
    spawn_unit(&mut app, "bulwark", Faction::A, Vec2::new(-100.0, 120.0));
    let hurt = spawn_unit(&mut app, "ripper", Faction::A, Vec2::new(-300.0, -60.0));
    app.world_mut().get_mut::<Health>(hurt).unwrap().current = 7;
    headless::step(&mut app);
    let before = units_of(&mut app, Faction::A);
    let b_before = units_of(&mut app, Faction::B);
    let b_buildings = buildings_of(&mut app, Faction::B);
    let a_buildings = buildings_of(&mut app, Faction::A);
    assert!(before.len() >= 5, "workers, a General and two soldiers");

    order(&mut app, Order::Retreat { faction: Faction::A }.issued_by(Faction::A));
    headless::step(&mut app);
    {
        let b = app.world().resource::<BattleState>();
        assert!(b.retreating(Faction::A) && !b.retreating(Faction::B));
    }
    // Everyone on A's side is now heading for A's edge, square across it.
    let edge = app.world().resource::<BattleState>().edge(Faction::A);
    for (e, ..) in units_of(&mut app, Faction::A) {
        let p = app.world().get::<Position>(e).unwrap().0;
        let t = app.world().get::<MoveTarget>(e).expect("retreating").0;
        assert!(edge.beyond(t).abs() < 1e-3, "the target is on the edge");
        assert!((t - p).dot(edge.outward) > 0.0, "heading out");
    }

    run_to_outcome(&mut app, 3000);
    let o = app.world().resource::<MatchState>().outcome().unwrap();
    assert_eq!(o.winner, Some(Faction::B), "a retreat cedes the field");
    let r = app.world().resource::<BattleState>().result().cloned().unwrap();
    assert_eq!(r.loss(Faction::A), Some(LossCause::Retreated));
    assert_eq!(r.loss(Faction::B), None);
    assert!(units_of(&mut app, Faction::A).is_empty(), "the field is empty of A");

    // The outcome names everyone who got away, with the HP they left with.
    let mut escaped: Vec<_> = r.escaped.iter().map(|e| e.sim_id).collect();
    escaped.sort();
    let mut fielded: Vec<_> = before.iter().map(|(.., id)| id.unwrap()).collect();
    fielded.sort();
    assert_eq!(escaped, fielded, "every unit A had got off the field");
    assert!(r.escaped.iter().all(|e| e.side == Faction::A));
    let hurt_id = before.iter().find(|(e, ..)| *e == hurt).unwrap().3.unwrap();
    let h = r.escaped.iter().find(|e| e.sim_id == hurt_id).unwrap();
    assert_eq!(h.hp, 7, "HP is carried off the field as it was");
    let generals: Vec<_> = r.escaped.iter().filter(|e| e.general).collect();
    assert_eq!(generals.len(), 1, "the General got away, and is marked so");
    assert_eq!(generals[0].unit, content().general_index("general_logistics").unwrap());
    // Crossing order: by tick, and every crossing is on or before the end.
    assert!(r.escaped.windows(2).all(|w| w[0].tick <= w[1].tick));
    assert!(r.escaped.iter().all(|e| e.tick <= r.tick));

    // Buildings stay where they are; the other side is untouched.
    assert_eq!(buildings_of(&mut app, Faction::A), a_buildings);
    assert_eq!(buildings_of(&mut app, Faction::B), b_buildings);
    assert_eq!(units_of(&mut app, Faction::B).len(), b_before.len());
}

#[test]
fn the_entry_edge_is_behind_the_base_by_the_ron_offset() {
    let c = content();
    let offset = c.battle.as_ref().unwrap().entry_edge_offset;
    let mut app = quiet_battle();
    let hq = |app: &mut App, side: Faction| {
        let mut q = app.world_mut().query::<(&Building, &Faction, &Position)>();
        q.iter(app.world())
            .find(|(b, f, _)| **f == side && c.buildings[b.def].victory)
            .map(|(.., p)| p.0)
            .unwrap()
    };
    let (ha, hb) = (hq(&mut app, Faction::A), hq(&mut app, Faction::B));
    let b = app.world().resource::<BattleState>();
    let (ea, eb) = (b.edge(Faction::A), b.edge(Faction::B));
    // Outward is away from the enemy base; the edge stands `offset` behind.
    assert!(ea.outward.dot(hb - ha) < 0.0 && eb.outward.dot(ha - hb) < 0.0);
    assert!((ea.beyond(ha) + offset).abs() < 1e-3, "A's HQ is `offset` inside A's edge");
    assert!((eb.beyond(hb) + offset).abs() < 1e-3, "B's HQ is `offset` inside B's edge");
}

#[test]
fn a_retreat_overrides_any_other_order_and_stops_production() {
    let c = content();
    let mut app = quiet_battle();
    let soldier = spawn_unit(&mut app, "ravager", Faction::A, Vec2::new(-200.0, 0.0));
    headless::step(&mut app);
    // Queue a worker at A's HQ, then retreat.
    let hq = {
        let mut q = app.world_mut().query::<(Entity, &Building, &Faction)>();
        q.iter(app.world())
            .find(|(_, b, f)| **f == Faction::A && c.buildings[b.def].victory)
            .map(|(e, ..)| e)
            .unwrap()
    };
    let worker = c.unit_index("worker").unwrap();
    order(&mut app, Order::Train { building: hq, unit: worker }.issued_by(Faction::A));
    headless::step(&mut app);
    assert!(!app.world().get::<ProductionQueue>(hq).unwrap().items.is_empty());
    order(&mut app, Order::Retreat { faction: Faction::A }.issued_by(Faction::A));
    headless::step(&mut app);
    assert!(app.world().get::<ProductionQueue>(hq).unwrap().items.is_empty());
    // A move order after the retreat does not turn the soldier around.
    order(
        &mut app,
        Order::MoveTo { units: vec![soldier], dest: Vec2::new(500.0, 0.0) }.issued_by(Faction::A),
    );
    headless::step(&mut app);
    let edge = app.world().resource::<BattleState>().edge(Faction::A);
    let t = app.world().get::<MoveTarget>(soldier).unwrap().0;
    assert!(edge.beyond(t).abs() < 1e-3, "still heading for the edge");
}

#[test]
fn a_retreat_is_a_sides_own_to_order() {
    let mut app = quiet_battle();
    headless::step(&mut app);
    order(&mut app, Order::Retreat { faction: Faction::A }.issued_by(Faction::B));
    headless::tick(&mut app, 5);
    assert!(!app.world().resource::<BattleState>().retreating(Faction::A));
}

#[test]
fn a_general_who_falls_during_the_retreat_is_a_general_lost() {
    let mut app = quiet_battle();
    headless::step(&mut app);
    order(&mut app, Order::Retreat { faction: Faction::A }.issued_by(Faction::A));
    headless::step(&mut app);
    let g = {
        let mut q = app.world_mut().query_filtered::<(Entity, &Faction), With<General>>();
        q.iter(app.world()).find(|(_, f)| **f == Faction::A).unwrap().0
    };
    app.world_mut().despawn(g);
    headless::step(&mut app);
    let r = app.world().resource::<BattleState>().result().cloned().expect("decided");
    assert_eq!(r.loss(Faction::A), Some(LossCause::GeneralsFell));
    assert_eq!(r.winner, Some(Faction::B));
}

#[test]
fn the_retreat_order_is_logged() {
    let mut app = quiet_battle();
    headless::step(&mut app);
    order(&mut app, Order::Retreat { faction: Faction::A }.issued_by(Faction::A));
    headless::step(&mut app);
    let log = app.world().resource::<CommandLog>();
    assert!(log
        .commands()
        .iter()
        .any(|c| c.order == LoggedOrder::Retreat { faction: Faction::A }));
}

#[test]
fn a_skirmish_has_no_retreat() {
    let mut app = headless::ai_vs_ai(content(), &MatchSettings::default()).unwrap();
    app.insert_resource(AiCommanders::default());
    headless::step(&mut app);
    let before = units_of(&mut app, Faction::A).len();
    order(&mut app, Order::Retreat { faction: Faction::A }.issued_by(Faction::A));
    headless::tick(&mut app, 600);
    assert_eq!(units_of(&mut app, Faction::A).len(), before, "nobody left the field");
    assert!(app.world().resource::<MatchState>().outcome().is_none());
}

#[test]
fn a_mutual_retreat_on_one_tick_has_no_winner_and_both_sides_escapees() {
    let mut app = quiet_battle();
    spawn_unit(&mut app, "ravager", Faction::A, Vec2::new(-100.0, 200.0));
    spawn_unit(&mut app, "ripper", Faction::B, Vec2::new(100.0, -200.0));
    headless::step(&mut app);
    let fielded = [units_of(&mut app, Faction::A).len(), units_of(&mut app, Faction::B).len()];
    order(&mut app, Order::Retreat { faction: Faction::A }.issued_by(Faction::A));
    order(&mut app, Order::Retreat { faction: Faction::B }.issued_by(Faction::B));
    run_to_outcome(&mut app, 60 * 120);
    let st = app.world().resource::<BattleState>();
    assert_eq!(st.retreat_ordered(Faction::A), st.retreat_ordered(Faction::B));
    let r = st.result().cloned().unwrap();
    assert_eq!(r.winner, None, "neither side held the field");
    assert_eq!(r.lost, [Some(LossCause::Retreated), Some(LossCause::Retreated)]);
    for (s, side) in [Faction::A, Faction::B].into_iter().enumerate() {
        let got = r.escaped.iter().filter(|e| e.side == side).count();
        assert_eq!(got, fielded[s], "{side:?}: every unit got away and is listed");
        assert!(r.report(side).generals.iter().all(|g| g.survived && g.escaped));
    }
    // The battle ended when the slower side was off, not the faster.
    assert_eq!(r.tick, r.escaped.iter().map(|e| e.tick).max().unwrap());
}

#[test]
fn of_two_retreats_the_earlier_order_loses_even_if_the_later_side_is_off_first() {
    let mut app = quiet_battle();
    // B's soldier stands by B's own edge; A's is deep in B's half.
    let eb = app.world().resource::<BattleState>().edge(Faction::B);
    spawn_unit(&mut app, "ripper", Faction::B, eb.point - eb.outward * 20.0);
    spawn_unit(&mut app, "ravager", Faction::A, Vec2::new(500.0, 0.0));
    headless::step(&mut app);
    order(&mut app, Order::Retreat { faction: Faction::A }.issued_by(Faction::A));
    headless::step(&mut app);
    order(&mut app, Order::Retreat { faction: Faction::B }.issued_by(Faction::B));
    run_to_outcome(&mut app, 60 * 120);
    let st = app.world().resource::<BattleState>();
    let (a, b) = (st.retreat_ordered(Faction::A).unwrap(), st.retreat_ordered(Faction::B).unwrap());
    assert!(a < b, "A ordered first: {a} vs {b}");
    let r = st.result().cloned().unwrap();
    assert_eq!(r.winner, Some(Faction::B), "A conceded first");
    assert_eq!(r.lost, [Some(LossCause::Retreated), None]);
    let b_off = r.escaped.iter().filter(|e| e.side == Faction::B).map(|e| e.tick).max().unwrap();
    assert!(b_off < r.tick, "B was off at {b_off}; the battle waited for A until {}", r.tick);
}
