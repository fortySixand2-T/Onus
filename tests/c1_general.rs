//! C1 AC1 — **the General**: a unit type in `units.ron`, one per side; a side
//! whose General dies loses the battle (CAMPAIGN_PLAN C1, DESIGN_BRIEF
//! "Campaign battles").
//!
//! Campaign rules are opt-in: a match set up with a `BattleSetup` fields the
//! Generals and plays by them; `MatchSettings::default()` is the skirmish and
//! has neither. Every stat below is read from the loaded content (F-044).

use std::path::PathBuf;

use bevy::prelude::*;

use onus::headless::{self, MatchSettings, MatchSetupError};
use onus::sim::battle::{BattleSetup, BattleSetupError, BattleState, General, LossCause};
use onus::sim::combat::Health;
use onus::sim::content::Content;
use onus::sim::economy::{Building, ProductionQueue, Stockpiles, UnitDefIdx};
use onus::sim::spatial::Faction;
use onus::sim::{AiCommanders, CommandQueue, MatchState, Order, Position};

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data")
}

fn content() -> Content {
    Content::load_from_dir(&data_dir()).expect("content loads")
}

fn one_general_each() -> BattleSetup {
    BattleSetup::default()
        .with_general(Faction::A, "general_synthesis")
        .with_general(Faction::B, "general_bulwark")
}

/// A battle with both commanders stood down, so nothing but the test acts.
fn quiet_battle(setup: BattleSetup) -> App {
    let mut app = headless::ai_vs_ai(content(), &MatchSettings::default().with_battle(setup))
        .expect("battle builds");
    app.insert_resource(AiCommanders::default());
    app
}

fn generals(app: &mut App) -> Vec<(Entity, Faction, usize)> {
    let mut q = app
        .world_mut()
        .query_filtered::<(Entity, &Faction, &UnitDefIdx), With<General>>();
    let mut v: Vec<_> = q.iter(app.world()).map(|(e, f, d)| (e, *f, d.0)).collect();
    v.sort_by_key(|(e, _, _)| e.to_bits());
    v
}

fn general_of(app: &mut App, side: Faction) -> Entity {
    let g: Vec<Entity> = generals(app)
        .into_iter()
        .filter(|(_, f, _)| *f == side)
        .map(|(e, _, _)| e)
        .collect();
    assert_eq!(g.len(), 1, "exactly one General on {side:?}");
    g[0]
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

fn outcome(app: &App) -> Option<onus::sim::MatchOutcome> {
    app.world().resource::<MatchState>().outcome()
}

#[test]
fn generals_are_their_own_list_in_units_ron_and_never_join_the_roster() {
    let c = content();
    let ids: Vec<&str> = c.generals.iter().map(|g| g.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "general_synthesis",
            "general_bulwark",
            "general_frenzy",
            "general_focus",
            "general_logistics",
            "general_endurance"
        ],
        "one General type per nation, in RON order"
    );
    for (k, g) in c.generals.iter().enumerate() {
        assert!(c.unit_index(&g.id).is_none(), "`{}` must not be a roster unit", g.id);
        let idx = c.general_index(&g.id).unwrap();
        assert_eq!(idx, c.units.len() + k, "Generals index past the roster");
        let def = c.unit_def(idx).expect("a General has a unit definition");
        assert_eq!(def.id, g.id);
        assert_eq!(
            (def.offense, def.defense, def.armor, def.speed),
            (g.offense, g.defense, g.armor, g.speed)
        );
        assert!(def.offense > 0 && def.mvp_attack_ticks > 0, "a General fights");
        assert!(def.nemesis.is_none(), "a General has no nemesis");
        assert_eq!(def.mvp_alloy_cost, 0, "a General is not bought");
        assert!(!def.gathers);
    }
    assert!(c.unit_def(c.units.len() + c.generals.len()).is_none());
    assert!(c.battle.is_some(), "the shipped content has a battle block");
}

#[test]
fn the_skirmish_fields_no_general_and_carries_no_battle_state() {
    let mut app = headless::ai_vs_ai(content(), &MatchSettings::default()).unwrap();
    headless::tick(&mut app, 5);
    assert!(generals(&mut app).is_empty());
    assert!(app.world().get_resource::<BattleState>().is_none());
}

#[test]
fn a_battle_fields_one_general_per_side_in_front_of_its_base() {
    let c = content();
    let mut app = quiet_battle(one_general_each());
    let g = generals(&mut app);
    assert_eq!(g.len(), 2);
    let a = general_of(&mut app, Faction::A);
    let b = general_of(&mut app, Faction::B);
    let def_a = app.world().get::<UnitDefIdx>(a).unwrap().0;
    let def_b = app.world().get::<UnitDefIdx>(b).unwrap().0;
    assert_eq!(def_a, c.general_index("general_synthesis").unwrap());
    assert_eq!(def_b, c.general_index("general_bulwark").unwrap());
    let hp = *app.world().get::<Health>(a).unwrap();
    let pool = c.unit_def(def_a).unwrap().defense * c.combat.hp_per_defense;
    assert_eq!((hp.current, hp.max), (pool, pool), "Defense is his HP pool");
    // In front of his own base, towards the enemy: A's base is the left one.
    let pa = app.world().get::<Position>(a).unwrap().0;
    let pb = app.world().get::<Position>(b).unwrap().0;
    assert!(pa.x < 0.0 && pb.x > 0.0 && pa.x < pb.x);
    assert!(app.world().get_resource::<BattleState>().is_some());
}

#[test]
fn a_side_whose_general_dies_loses_the_battle() {
    let mut app = quiet_battle(one_general_each());
    headless::step(&mut app);
    assert!(outcome(&app).is_none(), "nothing has fallen yet");
    let a = general_of(&mut app, Faction::A);
    let at = app.world().get::<Position>(a).unwrap().0;
    // One blow from an enemy soldier standing beside him kills him.
    app.world_mut().get_mut::<Health>(a).unwrap().current = 1;
    spawn_unit(&mut app, "ravager", Faction::B, at + Vec2::new(10.0, 0.0));
    let mut ticks = 0;
    while outcome(&app).is_none() && ticks < 120 {
        headless::step(&mut app);
        ticks += 1;
    }
    let o = outcome(&app).expect("the battle is decided");
    assert_eq!(o.winner, Some(Faction::B), "A lost its General, so B wins");
    assert!(app.world().get_entity(a).is_err(), "the General is dead");
    let r = app.world().resource::<BattleState>().result().cloned().unwrap();
    assert_eq!(r.winner, Some(Faction::B));
    assert_eq!(r.tick, o.tick, "the battle and the match agree on the tick");
    assert_eq!(r.loss(Faction::A), Some(LossCause::GeneralsFell));
    assert_eq!(r.loss(Faction::B), None);
    // Both HQs still stand: the General's death alone decided it.
    let c = content();
    let mut q = app.world_mut().query::<(&Building, &Faction)>();
    let hqs = q
        .iter(app.world())
        .filter(|(b, _)| c.buildings[b.def].victory)
        .count();
    assert_eq!(hqs, 2);
}

#[test]
fn both_generals_falling_on_one_tick_is_a_draw() {
    let mut app = quiet_battle(one_general_each());
    headless::step(&mut app);
    let a = general_of(&mut app, Faction::A);
    let b = general_of(&mut app, Faction::B);
    app.world_mut().despawn(a);
    app.world_mut().despawn(b);
    headless::step(&mut app);
    let o = outcome(&app).expect("decided");
    assert_eq!(o.winner, None);
    let r = app.world().resource::<BattleState>().result().cloned().unwrap();
    assert_eq!(r.loss(Faction::A), Some(LossCause::GeneralsFell));
    assert_eq!(r.loss(Faction::B), Some(LossCause::GeneralsFell));
}

#[test]
fn a_side_that_never_fielded_a_general_cannot_lose_one() {
    let setup = BattleSetup::default().with_general(Faction::A, "general_focus");
    let mut app = quiet_battle(setup);
    headless::tick(&mut app, 30);
    assert!(outcome(&app).is_none(), "B has no General, and that is not a loss");
    let a = general_of(&mut app, Faction::A);
    app.world_mut().despawn(a);
    headless::step(&mut app);
    assert_eq!(outcome(&app).unwrap().winner, Some(Faction::B));
}

#[test]
fn a_general_cannot_be_trained() {
    let c = content();
    let mut app = quiet_battle(one_general_each());
    headless::step(&mut app);
    let hq = {
        let mut q = app
            .world_mut()
            .query::<(Entity, &Building, &Faction)>();
        q.iter(app.world())
            .find(|(_, b, f)| **f == Faction::A && c.buildings[b.def].victory)
            .map(|(e, _, _)| e)
            .unwrap()
    };
    let before = app.world().resource::<Stockpiles>().alloy(Faction::A);
    let general = c.general_index("general_synthesis").unwrap();
    app.world_mut()
        .resource_mut::<CommandQueue>()
        .0
        .push_back(Order::Train { building: hq, unit: general }.issued_by(Faction::A));
    headless::step(&mut app);
    assert!(app.world().get::<ProductionQueue>(hq).unwrap().items.is_empty());
    assert_eq!(app.world().resource::<Stockpiles>().alloy(Faction::A), before);
}

#[test]
fn a_battle_naming_an_unknown_general_is_refused_before_anything_spawns() {
    let setup = BattleSetup::default().with_general(Faction::B, "general_nobody");
    let err = headless::battle(content(), &MatchSettings::default().with_battle(setup))
        .err()
        .expect("refused");
    assert_eq!(
        err,
        MatchSetupError::Battle(BattleSetupError::UnknownGeneral {
            id: "general_nobody".into(),
            side: Faction::B
        })
    );
}

/// A scratch copy of the shipped content with `units.ron` edited.
fn mutated(name: &str, edit: impl Fn(&str) -> String) -> Result<Content, String> {
    let out = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/c1_general")
        .join(name);
    std::fs::create_dir_all(&out).unwrap();
    for file in ["resources.ron", "strategies.ron"] {
        std::fs::copy(data_dir().join(file), out.join(file)).unwrap();
    }
    let text = std::fs::read_to_string(data_dir().join("units.ron")).unwrap();
    let edited = edit(&text);
    assert_ne!(edited, text, "`{name}`: the edit changed nothing");
    std::fs::write(out.join("units.ron"), edited).unwrap();
    Content::load_from_dir(&out).map_err(|e| e.to_string())
}

#[test]
fn a_general_with_a_nemesis_or_a_roster_id_is_a_load_error() {
    let anchor = "(id: \"general_focus\",";
    let nemesis = mutated("nemesis", |t| {
        t.replacen(anchor, "(id: \"general_focus\", nemesis: \"ravager\",", 1)
    });
    assert!(nemesis.is_err(), "a General has no nemesis: {nemesis:?}");
    let clash = mutated("clash", |t| t.replacen(anchor, "(id: \"ripper\",", 1));
    assert!(clash.is_err(), "a General's id may not be a roster unit's");
    let unarmed = mutated("unarmed", |t| {
        let at = t.find(anchor).unwrap();
        let line_end = at + t[at..].find('\n').unwrap();
        let line = &t[at..line_end];
        let off = line.find("offense: ").unwrap();
        let rest = &line[off + "offense: ".len()..];
        let n = rest.find(',').unwrap();
        let new_line = format!("{}offense: 0{}", &line[..off], &rest[n..]);
        format!("{}{}{}", &t[..at], new_line, &t[line_end..])
    });
    assert!(unarmed.is_err(), "a General fights");
}

#[test]
fn content_with_no_campaign_layer_still_loads_and_refuses_a_battle() {
    let c = mutated("no_layer", |t| {
        let at = t.find("    // ---- C1: the campaign battle layer").unwrap();
        format!("{})\n", &t[..at])
    })
    .expect("pre-C1 content still loads");
    assert!(c.generals.is_empty() && c.battle.is_none());
    let setup = one_general_each();
    assert_eq!(
        setup.check(&c),
        Err(BattleSetupError::NoBattleContent),
        "no campaign layer, no battle"
    );
}
