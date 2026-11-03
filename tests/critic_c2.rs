//! Critic C2: adversarial probes against the nation layer. Every test here
//! should pass per the C2 spec (CAMPAIGN_PLAN C2, DESIGN_BRIEF nations) and the
//! standing invariants (M5 replay guarantee, sim validity).

use std::path::PathBuf;

use bevy::prelude::*;

use onus::headless::{self, MatchSettings, MatchSetupError};
use onus::sim::battle::{BattleSetup, BattleSetupError, Roster};
use onus::sim::combat::{damage_between, StatBonus};
use onus::sim::content::Content;
use onus::sim::economy::Building;
use onus::sim::nation::{Defs, NationRules};
use onus::sim::{
    AiCommanders, CommandQueue, Faction, Health, MoveTarget, Order, Position, Stockpiles,
    UnitDefIdx,
};

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data")
}

fn with_traits(name: &str, traits: &str) -> Content {
    let out = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/critic_c2")
        .join(name);
    std::fs::create_dir_all(&out).unwrap();
    for f in ["units.ron", "resources.ron", "strategies.ron"] {
        std::fs::copy(data_dir().join(f), out.join(f)).unwrap();
    }
    let text = format!(
        r#"(nations: [(id: "x", name: "X", domain: None, colour: (0.1,0.2,0.3),
            accent: (0.4,0.5,0.6), general: "general_focus", strategy: "mvp",
            traits: [{traits}])])"#
    );
    std::fs::write(out.join("nations.ron"), text).unwrap();
    Content::load_from_dir(&out).expect("the critic's nation loads")
}

/// A match with X on side A (or no nation), and no commanders.
fn quiet(c: Content, a: Option<&str>) -> App {
    let s = MatchSettings::default().with_nations(a, None);
    let mut app = headless::battle(c, &s).expect("builds");
    app.insert_resource(AiCommanders::new(0, &[]));
    app
}

fn spawn(app: &mut App, c: &Content, f: Faction, unit: usize, at: Vec2) -> Entity {
    let hp = Defs::new(c, app.world().get_resource::<NationRules>()).unit_health(f, unit);
    app.world_mut()
        .spawn((Position(at), UnitDefIdx(unit), c.units[unit].mvp_kind, f, hp))
        .id()
}

fn push(app: &mut App, o: Order, by: Faction) {
    app.world_mut()
        .resource_mut::<CommandQueue>()
        .0
        .push_back(o.issued_by(by));
}

// ---- AC1: the traits reach the sim where they claim to, and nowhere else -----

/// A Speed trait moves side A's units faster in `movement`, and only side A's.
#[test]
fn a_speed_trait_reaches_movement_for_its_own_side_only() {
    let c = with_traits("speed", r#"(applies_to: CombatUnits, stat: Speed, add: 1)"#);
    let b = c.unit_index("bulwark").unwrap();
    let mut app = quiet(c.clone(), Some("x"));
    let ea = spawn(&mut app, &c, Faction::A, b, Vec2::new(-600.0, 500.0));
    let eb = spawn(&mut app, &c, Faction::B, b, Vec2::new(600.0, 500.0));
    app.world_mut().entity_mut(ea).insert(MoveTarget(Vec2::new(-600.0, 900.0)));
    app.world_mut().entity_mut(eb).insert(MoveTarget(Vec2::new(600.0, 900.0)));
    headless::step(&mut app);
    let dy = |e: Entity| app.world().get::<Position>(e).unwrap().0.y - 500.0;
    let (da, db) = (dy(ea), dy(eb));
    let s = c.units[b].speed as f32;
    assert!(db > 0.0 && da > 0.0, "both moved: {da} {db}");
    assert!(
        ((da / db) - (s + 1.0) / s).abs() < 1e-3,
        "A moved {da}, B moved {db}: ratio must be (speed+1)/speed"
    );
}

/// Offense and Armor traits reach combat: the damage A deals and takes is the
/// traited definitions', computed independently through `damage_between`.
#[test]
fn offense_and_armor_traits_reach_combat_on_both_ends_of_a_hit() {
    let c = with_traits(
        "off_arm",
        r#"(applies_to: CombatUnits, stat: Offense, add: 1),
           (applies_to: CombatUnits, stat: Armor, add: 1)"#,
    );
    let s = c.unit_index("sentinel").unwrap();
    let mut app = quiet(c.clone(), Some("x"));
    let ea = spawn(&mut app, &c, Faction::A, s, Vec2::new(0.0, 600.0));
    let eb = spawn(&mut app, &c, Faction::B, s, Vec2::new(10.0, 600.0));
    let before = |app: &App, e: Entity| app.world().get::<Health>(e).unwrap().current;
    let (a0, b0) = (before(&app, ea), before(&app, eb));
    headless::step(&mut app);
    let (a1, b1) = (before(&app, ea), before(&app, eb));
    let rules = app.world().resource::<NationRules>().clone();
    let d = Defs::new(&c, Some(&rules));
    let (ua, ub) = (d.unit(Faction::A, s).unwrap(), d.unit(Faction::B, s).unwrap());
    let a_hits_b = damage_between(&c, ua, ub, StatBonus::NONE, StatBonus::NONE);
    let b_hits_a = damage_between(&c, ub, ua, StatBonus::NONE, StatBonus::NONE);
    assert_eq!(b0 - b1, a_hits_b, "A's traited offense");
    assert_eq!(a0 - a1, b_hits_a, "A's traited armor");
    let plain = damage_between(&c, &c.units[s], &c.units[s], StatBonus::NONE, StatBonus::NONE);
    assert!(a_hits_b > plain && b_hits_a < plain, "the traits changed the hit");
}

/// A building AlloyCost trait reaches placement, for its side only.
#[test]
fn a_building_cost_trait_reaches_placement_for_its_side_only() {
    let c = with_traits("bcost", r#"(applies_to: Building("foundry"), stat: AlloyCost, mul: 0.5)"#);
    let foundry = c.building_index("foundry").unwrap();
    let mut app = quiet(c.clone(), Some("x"));
    let before = {
        let s = app.world().resource::<Stockpiles>();
        [s.alloy(Faction::A), s.alloy(Faction::B)]
    };
    for (f, x) in [(Faction::A, -750.0), (Faction::B, 750.0)] {
        push(
            &mut app,
            Order::Place { faction: f, building: foundry, pos: Vec2::new(x, -150.0) },
            f,
        );
    }
    headless::step(&mut app);
    let s = app.world().resource::<Stockpiles>();
    let base = c.buildings[foundry].alloy_cost;
    assert_eq!(before[0] - s.alloy(Faction::A), base / 2);
    assert_eq!(before[1] - s.alloy(Faction::B), base);
}

/// A campaign roster under a nation: a unit may never hold more HP than its
/// pool. A Defense -1 nation fielding a roster unit at the *base* pool's HP is
/// **refused** at setup (implementer fix, F-068: refuse, never clamp), naming
/// the side's traited pool; at that pool it is admitted with current <= max.
#[test]
fn a_roster_unit_never_starts_above_its_sides_pool() {
    let c = with_traits("frail", r#"(applies_to: CombatUnits, stat: Defense, add: -1)"#);
    let s = c.unit_index("sentinel").unwrap();
    let base_max = Health::from_def(&c, s).max;
    let settings = |hp: u32| {
        let setup = BattleSetup::default()
            .with_general(Faction::A, "general_bulwark")
            .with_general(Faction::B, "general_bulwark")
            .with_roster(Faction::A, Roster::default().unit_hp("sentinel", hp));
        MatchSettings::default()
            .with_nations(Some("x"), None)
            .with_battle(setup)
    };
    let rules = NationRules::resolve(&c, &[Some("x".into()), None]).unwrap().unwrap();
    let side_max = Defs::new(&c, Some(&rules)).unit_health(Faction::A, s).max;
    assert!(side_max < base_max, "the trait shrinks the pool");
    match headless::battle(c.clone(), &settings(base_max)) {
        Err(MatchSetupError::Battle(BattleSetupError::BadHp { hp, max, side, .. })) => {
            assert_eq!((hp, max, side), (base_max, side_max, Faction::A));
        }
        Err(e) => panic!("refused for the wrong reason: {e}"),
        Ok(_) => panic!("a roster sentinel above its side's pool was admitted"),
    }
    let mut app = headless::battle(c.clone(), &settings(side_max)).expect("at the pool: admitted");
    let mut q = app.world_mut().query::<(&Faction, &UnitDefIdx, &Health)>();
    let mut seen = 0;
    for (f, d, h) in q.iter(app.world()) {
        if *f == Faction::A && d.0 == s {
            seen += 1;
            assert!(
                h.current <= h.max,
                "roster sentinel starts at {}/{}: HP above its pool",
                h.current,
                h.max
            );
        }
    }
    assert!(seen > 0, "the roster sentinel was spawned");
}

/// The same for a roster building under a BuildingHp < 1 nation.
#[test]
fn a_roster_building_never_starts_above_its_sides_pool() {
    let c = with_traits("brittle", r#"(applies_to: Buildings, stat: BuildingHp, mul: 0.5)"#);
    let foundry = c.building_index("foundry").unwrap();
    let base_max = Health::from_building_def(&c, foundry).max;
    let settings = |hp: u32| {
        let setup = BattleSetup::default()
            .with_general(Faction::A, "general_bulwark")
            .with_general(Faction::B, "general_bulwark")
            .with_roster(Faction::A, Roster::default().building_hp("foundry", hp));
        MatchSettings::default()
            .with_nations(Some("x"), None)
            .with_battle(setup)
    };
    let rules = NationRules::resolve(&c, &[Some("x".into()), None]).unwrap().unwrap();
    let side_max = Defs::new(&c, Some(&rules)).building_health(Faction::A, foundry).max;
    assert!(side_max < base_max, "the trait shrinks the pool");
    match headless::battle(c.clone(), &settings(base_max)) {
        Err(MatchSetupError::Battle(BattleSetupError::BadHp { hp, max, side, .. })) => {
            assert_eq!((hp, max, side), (base_max, side_max, Faction::A));
        }
        Err(e) => panic!("refused for the wrong reason: {e}"),
        Ok(_) => panic!("a roster foundry above its side's pool was admitted"),
    }
    let mut app =
        headless::battle(c.clone(), &settings(side_max)).expect("at the pool: admitted");
    let mut q = app.world_mut().query::<(&Faction, &Building, &Health)>();
    let mut seen = 0;
    for (f, b, h) in q.iter(app.world()) {
        if *f == Faction::A && b.def == foundry {
            seen += 1;
            assert!(
                h.current <= h.max,
                "roster foundry starts at {}/{}: HP above its pool",
                h.current,
                h.max
            );
        }
    }
    assert!(seen > 0, "the roster foundry was spawned");
}
