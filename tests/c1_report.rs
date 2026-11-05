//! C1 AC6 — **the outcome reports survivors**: per side, each surviving unit's
//! type and HP, each standing building's HP, and whether each General
//! survived (CAMPAIGN_PLAN C1). This is what the campaign carries forward.

use bevy::prelude::*;

use onus::headless::{self, MatchSettings};
use onus::sim::battle::{BattleResult, BattleSetup, BattleState, General, GeneralFate, Roster};
use onus::sim::combat::Health;
use onus::sim::content::Content;
use onus::sim::economy::UnitDefIdx;
use onus::sim::replay::SimId;
use onus::sim::spatial::Faction;
use onus::sim::{AiCommanders, CommandQueue, MatchState, Order};

fn content() -> Content {
    headless::content().expect("content loads")
}

fn quiet(setup: BattleSetup) -> App {
    let mut app = headless::ai_vs_ai(content(), &MatchSettings::default().with_battle(setup))
        .expect("battle builds");
    app.insert_resource(AiCommanders::default());
    app
}

fn generals_of(app: &mut App, side: Faction) -> Vec<Entity> {
    let mut q = app
        .world_mut()
        .query_filtered::<(Entity, &Faction), With<General>>();
    let mut v: Vec<_> = q
        .iter(app.world())
        .filter(|(_, f)| **f == side)
        .map(|(e, _)| e)
        .collect();
    v.sort_by_key(|e| e.to_bits());
    v
}

fn result(app: &App) -> BattleResult {
    app.world()
        .resource::<BattleState>()
        .result()
        .cloned()
        .expect("decided")
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

/// What `side` has on the field: (sim id, def, hp, max, general), by sim id.
fn field(app: &mut App, side: Faction) -> Vec<(SimId, usize, u32, u32, bool)> {
    let mut q = app
        .world_mut()
        .query::<(&Faction, &UnitDefIdx, &Health, &SimId, Has<General>)>();
    let mut v: Vec<_> = q
        .iter(app.world())
        .filter(|(f, ..)| **f == side)
        .map(|(_, d, h, id, g)| (*id, d.0, h.current, h.max, g))
        .collect();
    v.sort();
    v
}

#[test]
fn the_report_lists_each_sides_survivors_buildings_and_generals() {
    let c = content();
    let roster = Roster::default()
        .unit("ravager")
        .unit_hp("ripper", 11)
        .building_hp("hq", 900)
        .building("foundry");
    let setup = BattleSetup::default()
        .with_general(Faction::A, "general_focus")
        .with_roster(Faction::A, roster)
        .with_general(Faction::B, "general_bulwark");
    let mut app = quiet(setup);
    headless::step(&mut app);
    // A wound taken in the battle shows in the report.
    let rav = c.unit_index("ravager").unwrap();
    {
        let mut q = app
            .world_mut()
            .query::<(&UnitDefIdx, &Faction, &mut Health)>();
        for (d, f, mut h) in q.iter_mut(app.world_mut()) {
            if *f == Faction::A && d.0 == rav {
                h.current = 5;
            }
        }
    }
    let gb = generals_of(&mut app, Faction::B)[0];
    app.world_mut().despawn(gb);
    headless::step(&mut app);
    let r = result(&app);
    assert_eq!(r.winner, Some(Faction::A));

    let a = r.report(Faction::A);
    let want: Vec<_> = field(&mut app, Faction::A)
        .into_iter()
        .map(|(id, d, hp, max, g)| (id, d, hp, max, g, false))
        .collect();
    let got: Vec<_> = a
        .units
        .iter()
        .map(|u| (u.sim_id, u.unit, u.hp, u.max_hp, u.general, u.escaped))
        .collect();
    assert_eq!(got, want, "every unit on the field, by sim id, with its HP");
    let mut ids: Vec<(&str, u32)> = a
        .units
        .iter()
        .map(|u| (c.unit_def(u.unit).unwrap().id.as_str(), u.hp))
        .collect();
    ids.sort();
    let focus_hp = Health::from_def(&c, c.general_index("general_focus").unwrap()).max;
    assert_eq!(
        ids,
        [("general_focus", focus_hp), ("ravager", 5), ("ripper", 11)]
    );
    assert!(a.buildings.windows(2).all(|w| w[0].sim_id < w[1].sim_id));
    let mut blds: Vec<_> = a
        .buildings
        .iter()
        .map(|b| (c.buildings[b.building].id.as_str(), b.hp, b.max_hp))
        .collect();
    blds.sort_by_key(|b| b.0 != "hq");
    let hq_max = Health::from_building_def(&c, c.building_index("hq").unwrap()).max;
    let f = c.building_index("foundry").unwrap();
    let f_max = Health::from_building_def(&c, f).max;
    assert_eq!(blds, [("hq", 900, hq_max), ("foundry", f_max, f_max)]);
    let gfocus = c.general_index("general_focus").unwrap();
    assert_eq!(
        a.generals,
        [GeneralFate {
            unit: gfocus,
            survived: true,
            escaped: false
        }]
    );

    let b = r.report(Faction::B);
    let gbul = c.general_index("general_bulwark").unwrap();
    assert_eq!(
        b.generals,
        [GeneralFate {
            unit: gbul,
            survived: false,
            escaped: false
        }]
    );
    let worker = c.unit_index("worker").unwrap();
    assert!(!b.units.is_empty() && b.units.iter().all(|u| u.unit == worker && !u.general));
    assert_eq!(b.buildings.len(), 1, "B's HQ still stands");
    assert!(b.units.windows(2).all(|w| w[0].sim_id < w[1].sim_id));
}

#[test]
fn each_general_has_a_fate_in_setup_order() {
    let c = content();
    let setup = BattleSetup::default()
        .with_general(Faction::A, "general_synthesis")
        .with_general(Faction::A, "general_frenzy")
        .with_general(Faction::B, "general_endurance");
    let mut app = quiet(setup);
    headless::step(&mut app);
    // The first General the setup named falls (entity order is not setup order).
    let synthesis = c.general_index("general_synthesis").unwrap();
    let a = generals_of(&mut app, Faction::A)
        .into_iter()
        .find(|e| app.world().get::<UnitDefIdx>(*e).unwrap().0 == synthesis)
        .unwrap();
    app.world_mut().despawn(a);
    let b = generals_of(&mut app, Faction::B);
    app.world_mut().despawn(b[0]);
    headless::step(&mut app);
    let r = result(&app);
    assert_eq!(r.winner, Some(Faction::A));
    let fates: Vec<_> = r
        .report(Faction::A)
        .generals
        .iter()
        .map(|g| (g.unit, g.survived))
        .collect();
    assert_eq!(
        fates,
        [
            (c.general_index("general_synthesis").unwrap(), false),
            (c.general_index("general_frenzy").unwrap(), true),
        ]
    );
    let alive: Vec<_> = r
        .report(Faction::A)
        .units
        .iter()
        .filter(|u| u.general)
        .collect();
    assert_eq!(alive.len(), 1, "the dead General is not a survivor");
}

#[test]
fn a_retreat_reports_who_got_away_with_the_hp_they_left_with() {
    let c = content();
    let roster = Roster::default()
        .unit_hp("bulwark", 9)
        .unit("ravager")
        .building("hq");
    let setup = BattleSetup::default()
        .with_general(Faction::A, "general_logistics")
        .with_roster(Faction::A, roster)
        .with_general(Faction::B, "general_bulwark");
    let mut app = quiet(setup);
    headless::step(&mut app);
    let before = field(&mut app, Faction::A);
    app.world_mut().resource_mut::<CommandQueue>().0.push_back(
        Order::Retreat {
            faction: Faction::A,
        }
        .issued_by(Faction::A),
    );
    run_to_outcome(&mut app, 3000);
    let r = result(&app);
    let a = r.report(Faction::A);
    let got: Vec<_> = a
        .units
        .iter()
        .map(|u| (u.sim_id, u.unit, u.hp, u.max_hp, u.general))
        .collect();
    assert_eq!(got, before, "everyone escaped, as they were");
    assert!(a.units.iter().all(|u| u.escaped));
    let glog = c.general_index("general_logistics").unwrap();
    assert_eq!(
        a.generals,
        [GeneralFate {
            unit: glog,
            survived: true,
            escaped: true
        }]
    );
    assert_eq!(a.buildings.len(), 1, "a city is left behind standing");
    let b = r.report(Faction::B);
    assert_eq!(b.generals.len(), 1);
    assert!(b.generals[0].survived && !b.generals[0].escaped);
}
