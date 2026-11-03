//! C1 AC2 — **several Generals per side**: a battle can field more than one
//! General per side, and that side loses only when **all** of them are dead
//! (CAMPAIGN_PLAN C1; F-052 update 5 — converging armies each bring their
//! General). Layout numbers are read from the loaded content.

use bevy::prelude::*;

use onus::headless::{self, MatchSettings, MatchSetupError};
use onus::sim::battle::{BattleSetup, BattleSetupError, BattleState, General, LossCause};
use onus::sim::content::Content;
use onus::sim::economy::UnitDefIdx;
use onus::sim::spatial::Faction;
use onus::sim::{AiCommanders, MatchState, Position};

fn content() -> Content {
    headless::content().expect("content loads")
}

fn quiet_battle(setup: BattleSetup) -> App {
    let mut app = headless::ai_vs_ai(content(), &MatchSettings::default().with_battle(setup))
        .expect("battle builds");
    app.insert_resource(AiCommanders::default());
    app
}

/// The side's Generals in spawn order (entity bits are allocated in order).
fn generals_of(app: &mut App, side: Faction) -> Vec<(Entity, usize, Vec2)> {
    let mut q = app
        .world_mut()
        .query_filtered::<(Entity, &Faction, &UnitDefIdx, &Position), With<General>>();
    let mut v: Vec<_> = q
        .iter(app.world())
        .filter(|(_, f, _, _)| **f == side)
        .map(|(e, _, d, p)| (e, d.0, p.0))
        .collect();
    v.sort_by_key(|(e, _, _)| e.to_bits());
    v
}

fn three_against_one() -> BattleSetup {
    BattleSetup::default()
        .with_general(Faction::A, "general_synthesis")
        .with_general(Faction::A, "general_frenzy")
        .with_general(Faction::A, "general_focus")
        .with_general(Faction::B, "general_endurance")
}

#[test]
fn a_side_fields_every_general_its_setup_names_in_a_line_across_its_front() {
    let c = content();
    let battle = c.battle.clone().unwrap();
    let mut app = quiet_battle(three_against_one());
    let a = generals_of(&mut app, Faction::A);
    let ids: Vec<&str> = a.iter().map(|(_, d, _)| c.unit_def(*d).unwrap().id.as_str()).collect();
    assert_eq!(ids, ["general_synthesis", "general_frenzy", "general_focus"]);
    assert_eq!(generals_of(&mut app, Faction::B).len(), 1);
    // One line: the same distance in front of the base, `formation_spacing`
    // apart, centred on the line a lone General would stand at.
    let lone = generals_of(&mut app, Faction::B)[0].2;
    for w in a.windows(2) {
        let (p, q) = (w[0].2, w[1].2);
        assert_eq!(p.x, q.x, "the line runs across the front");
        assert!(((q.y - p.y).abs() - battle.formation_spacing).abs() < 1e-3);
    }
    let centre = a.iter().map(|(_, _, p)| p.y).sum::<f32>() / 3.0;
    assert!((centre - lone.y).abs() < 1e-3, "the line is centred on the front");
    assert!((a[0].2.x.abs() - lone.x.abs()).abs() < 1e-3, "both sides stand as far forward");
}

#[test]
fn the_same_general_type_may_be_fielded_twice() {
    let setup = BattleSetup::default()
        .with_general(Faction::A, "general_bulwark")
        .with_general(Faction::A, "general_bulwark");
    let mut app = quiet_battle(setup);
    let a = generals_of(&mut app, Faction::A);
    assert_eq!(a.len(), 2);
    assert_eq!(a[0].1, a[1].1);
    assert_ne!(a[0].2, a[1].2, "two Generals never share a spot");
}

#[test]
fn a_side_fights_on_while_any_general_lives_and_loses_when_the_last_falls() {
    let mut app = quiet_battle(three_against_one());
    headless::step(&mut app);
    let a = generals_of(&mut app, Faction::A);
    // Two of three fall, on separate ticks: the battle goes on.
    app.world_mut().despawn(a[0].0);
    headless::step(&mut app);
    app.world_mut().despawn(a[2].0);
    headless::tick(&mut app, 10);
    assert!(app.world().resource::<MatchState>().outcome().is_none());
    assert!(app.world().resource::<BattleState>().result().is_none());
    // The last one falls: A has lost.
    app.world_mut().despawn(a[1].0);
    headless::step(&mut app);
    let o = app.world().resource::<MatchState>().outcome().expect("decided");
    assert_eq!(o.winner, Some(Faction::B));
    let r = app.world().resource::<BattleState>().result().cloned().unwrap();
    assert_eq!(r.loss(Faction::A), Some(LossCause::GeneralsFell));
    assert_eq!(r.loss(Faction::B), None);
}

#[test]
fn all_of_a_sides_generals_falling_on_one_tick_loses_it_that_tick() {
    let mut app = quiet_battle(three_against_one());
    headless::step(&mut app);
    let before = app.world().resource::<MatchState>().tick();
    for (e, _, _) in generals_of(&mut app, Faction::A) {
        app.world_mut().despawn(e);
    }
    headless::step(&mut app);
    let o = app.world().resource::<MatchState>().outcome().expect("decided");
    assert_eq!((o.winner, o.tick), (Some(Faction::B), before));
}

#[test]
fn every_general_in_a_setup_is_checked_not_just_the_first() {
    let setup = BattleSetup::default()
        .with_general(Faction::A, "general_focus")
        .with_general(Faction::A, "general_typo");
    let err = headless::battle(content(), &MatchSettings::default().with_battle(setup))
        .err()
        .expect("refused");
    assert_eq!(
        err,
        MatchSetupError::Battle(BattleSetupError::UnknownGeneral {
            id: "general_typo".into(),
            side: Faction::A
        })
    );
}
