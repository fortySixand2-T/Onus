//! Critic C3 (adversarial). Tests that SHOULD pass per CAMPAIGN_PLAN C3 and
//! the invariants. Placed on the box as tests/zz_critic_c3.rs (scratch only).

use bevy::prelude::*;
use std::path::PathBuf;

use onus::headless::{self, MatchSettings};
use onus::sim::content::Content;
use onus::sim::nation::{NationRules, SideRules, TraitDef, TraitStat, TraitTarget};
use onus::sim::three::{Bank, EconomyMode, LeyWell, ThreeEconomy, WellState};
use onus::sim::{
    AiCommanders, Building, CommandLog, CommandQueue, Faction, Health, MatchLog, Order, Position,
    ProductionQueue, ReplaySource, StateHashLog, Stockpiles, UnitDefIdx,
};

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data")
}

fn shipped() -> Content {
    Content::load_from_dir(&data_dir()).expect("content loads")
}

fn quiet(economy: EconomyMode, a: Option<&str>, b: Option<&str>) -> App {
    let s = MatchSettings::default().with_economy(economy).with_nations(a, b);
    let mut app = headless::battle(shipped(), &s).expect("builds");
    app.insert_resource(AiCommanders::new(0, &[]));
    app
}

fn push(app: &mut App, o: Order, by: Faction) {
    app.world_mut().resource_mut::<CommandQueue>().0.push_back(o.issued_by(by));
}

fn banks(app: &App, f: Faction) -> [u32; 3] {
    let s = app.world().resource::<Stockpiles>();
    [s.alloy(f), s.biomass(f), s.aether(f)]
}

fn grant(app: &mut App, f: Faction, bank: Bank, n: u32) {
    let _ = app.world_mut().resource_mut::<Stockpiles>().add_in(f, bank, n);
}

fn wells(app: &mut App) -> Vec<(Entity, Vec2, LeyWell)> {
    let mut q = app.world_mut().query::<(Entity, &Position, &LeyWell)>();
    let mut v: Vec<_> = q.iter(app.world()).map(|(e, p, w)| (e, p.0, *w)).collect();
    v.sort_by_key(|(_, _, w)| w.site());
    v
}

fn middle(app: &mut App) -> (usize, Vec2) {
    let t = shipped().three_resource.unwrap();
    let k = t.wells.sites.iter().position(|s| s.along == 0.5 && s.side == 0.0).unwrap();
    (k, wells(app)[k].1)
}

fn worker_at(app: &mut App, f: Faction, at: Vec2) -> Entity {
    let c = app.world().resource::<Content>().clone();
    let w = c.unit_index("worker").unwrap();
    app.world_mut()
        .spawn((Position(at), UnitDefIdx(w), c.units[w].mvp_kind, f, Health::from_def(&c, w)))
        .id()
}

const BASE: [Vec2; 2] = [Vec2::new(-750.0, 0.0), Vec2::new(750.0, 0.0)];

fn place(app: &mut App, f: Faction, id: &str, offset: Vec2) -> Entity {
    let c = app.world().resource::<Content>().clone();
    let def = c.building_index(id).unwrap();
    let base = if f == Faction::A { BASE[0] } else { BASE[1] };
    push(app, Order::Place { faction: f, building: def, pos: base + offset }, f);
    headless::tick(app, 1);
    let mut q = app.world_mut().query::<(Entity, &Building, &Faction)>();
    let mut v: Vec<Entity> = q
        .iter(app.world())
        .filter(|(_, b, g)| b.def == def && **g == f)
        .map(|(e, ..)| e)
        .collect();
    v.sort_by_key(|e| e.to_bits());
    *v.last().expect("placed")
}

// ---- AC2: holder change through a contest -------------------------------------

/// A held for half an interval, B arrived (contest), A left: B now holds, a
/// change of holder, so B's progress starts from zero. A's half must not be
/// credited to B.
#[test]
fn critic_holder_change_through_a_contest_restarts_progress() {
    let t = shipped().three_resource.unwrap();
    let w = &t.wells;
    let half = w.interval_ticks / 2;
    let mut app = quiet(EconomyMode::ThreeResource, None, None);
    let (k, mid) = middle(&mut app);
    let a = worker_at(&mut app, Faction::A, mid);
    headless::tick(&mut app, half);
    worker_at(&mut app, Faction::B, mid + Vec2::new(3.0, 0.0));
    headless::tick(&mut app, 5);
    assert_eq!(wells(&mut app)[k].2.state(), WellState::Contested);
    app.world_mut().despawn(a);
    headless::tick(&mut app, w.interval_ticks - 1);
    assert_eq!(wells(&mut app)[k].2.state(), WellState::Held(Faction::B));
    assert_eq!(banks(&app, Faction::B)[2], t.starting_aether, "B credited A's progress");
    headless::tick(&mut app, 1);
    assert_eq!(banks(&app, Faction::B)[2], t.starting_aether + w.per_interval);
    assert_eq!(banks(&app, Faction::A)[2], t.starting_aether);
}

/// Exactly at `hold_radius` is "within"; a hair beyond is not.
#[test]
fn critic_hold_radius_boundary() {
    let t = shipped().three_resource.unwrap();
    let w = &t.wells;
    let mut app = quiet(EconomyMode::ThreeResource, None, None);
    let (k, mid) = middle(&mut app);
    worker_at(&mut app, Faction::A, mid + Vec2::new(0.0, w.hold_radius));
    headless::tick(&mut app, 1);
    assert_eq!(wells(&mut app)[k].2.state(), WellState::Held(Faction::A));
    let mut app = quiet(EconomyMode::ThreeResource, None, None);
    let (k, mid) = middle(&mut app);
    worker_at(&mut app, Faction::A, mid + Vec2::new(0.0, w.hold_radius + 0.5));
    headless::tick(&mut app, 1);
    assert_eq!(wells(&mut app)[k].2.state(), WellState::Empty);
}

// ---- AC1: storage cap ---------------------------------------------------------

/// Biomass never grows past the cap; when the cap shrinks (a Vats falls) the
/// side grows nothing until it is under the new cap again.
#[test]
fn critic_biomass_never_grows_above_the_cap_and_a_shrunken_cap_stops_growth() {
    let c = shipped();
    let t = c.three_resource.clone().unwrap();
    let cap = |id: &str| t.growth.sources.iter().find(|s| s.building == id).unwrap().storage;
    let mut app = quiet(EconomyMode::ThreeResource, None, None);
    grant(&mut app, Faction::A, Bank::Alloy, 5_000);
    let vats = place(&mut app, Faction::A, "gene_vats", Vec2::new(0.0, -150.0));
    let full = cap("hq") + cap("gene_vats");
    for _ in 0..(full * t.growth.interval_ticks / 2) {
        headless::tick(&mut app, 1);
        assert!(banks(&app, Faction::A)[1] <= full, "over the cap");
    }
    assert_eq!(banks(&app, Faction::A)[1], full, "reaches the cap");
    app.world_mut().despawn(vats);
    let held = banks(&app, Faction::A)[1];
    headless::tick(&mut app, t.growth.interval_ticks * 20);
    assert_eq!(banks(&app, Faction::A)[1], held, "grew above a shrunken cap");
}

// ---- AC3: refused orders charge nothing ----------------------------------------

#[test]
fn critic_a_biomass_unit_without_biomass_is_refused_and_charges_no_bank() {
    let mut app = quiet(EconomyMode::ThreeResource, None, None);
    grant(&mut app, Faction::A, Bank::Alloy, 5_000);
    grant(&mut app, Faction::A, Bank::Aether, 500);
    let vats = place(&mut app, Faction::A, "gene_vats", Vec2::new(0.0, -150.0));
    // Drain Biomass.
    let have = banks(&app, Faction::A)[1];
    assert!(app.world_mut().resource_mut::<Stockpiles>().try_spend_in(Faction::A, Bank::Biomass, have));
    let c = shipped();
    let ripper = c.unit_index("ripper").unwrap();
    let before = banks(&app, Faction::A);
    push(&mut app, Order::Train { building: vats, unit: ripper }, Faction::A);
    headless::tick(&mut app, 1);
    assert_eq!(app.world().get::<ProductionQueue>(vats).unwrap().items.len(), 0);
    // Growth may land on this tick; Alloy and Aether must be untouched.
    let after = banks(&app, Faction::A);
    assert_eq!([after[0], after[2]], [before[0], before[2]]);
}

// ---- AC4: a trait that cannot act ----------------------------------------------

/// "A trait that cannot act in the mode its list is played in is a load
/// error." Growth on a building that is no growth source acts on nothing in
/// a three-resource match (as AlloyYield on a non-gatherer, which is refused).
#[test]
fn critic_growth_on_a_building_that_grows_nothing_is_refused() {
    let c = shipped();
    let k = c.nations.iter().position(|n| n.id == "verdant").unwrap();
    let mut bad = c.clone();
    bad.nations[k].three_resource_traits.push(TraitDef {
        applies_to: TraitTarget::Building("foundry".into()),
        stat: TraitStat::Growth,
        add: None,
        mul: Some(1.5),
    });
    assert!(
        SideRules::build_in(&bad, k, EconomyMode::ThreeResource).is_err(),
        "a Growth trait on a non-source building loads and silently does nothing"
    );
}

// ---- AC7: replay from disk with only what the log carries ----------------------

#[test]
fn critic_three_resource_nation_match_replays_from_disk_into_a_bare_world() {
    const TICKS: u32 = 2_400;
    let s = MatchSettings::default()
        .with_seed(13)
        .with_economy(EconomyMode::ThreeResource)
        .with_nations(Some("verdant"), Some("tollmarch"))
        .with_strategies("mass_ripper", "mass_sentinel")
        .with_hashing(true);
    let mut rec = headless::ai_vs_ai(shipped(), &s).unwrap();
    headless::tick(&mut rec, TICKS);
    let recorded = rec.world().resource::<StateHashLog>().clone();
    let log = rec.world().resource::<CommandLog>().log().clone();
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/zz_critic_c3");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("three.ron");
    log.save(&path).unwrap();
    let log = MatchLog::load_for(&path, &shipped()).unwrap();
    assert_eq!(log.economy, EconomyMode::ThreeResource);
    // Only the seed from the log; no nations, no economy, default strategies.
    let rs = MatchSettings::default().with_seed(log.seed).with_hashing(true);
    let mut rep = headless::ai_vs_ai(shipped(), &rs).unwrap();
    rep.insert_resource(AiCommanders::default());
    rep.insert_resource(StateHashLog::default());
    rep.insert_resource(CommandLog::new(log.seed));
    rep.insert_resource(ReplaySource::new(log));
    headless::tick(&mut rep, TICKS);
    assert_eq!(rep.world().resource::<ReplaySource>().rejection(), None);
    assert!(rep.world().contains_resource::<ThreeEconomy>());
    let c = shipped();
    let rules = rep.world().resource::<NationRules>();
    let hq = c.building_index("hq").unwrap();
    assert!(rules.side(Faction::A).unwrap().growth_milli[hq] > 1000, "Verdant lean played");
    let replayed = rep.world().resource::<StateHashLog>().clone();
    assert_eq!(recorded.first_divergence(&replayed), None);
    assert_eq!(banks(&rec, Faction::A), banks(&rep, Faction::A));
    assert_eq!(banks(&rec, Faction::B), banks(&rep, Faction::B));
}

/// Two runs of one seeded three-resource match hash identically every tick.
#[test]
fn critic_three_resource_ai_match_is_deterministic() {
    let s = MatchSettings::default()
        .with_seed(21)
        .with_economy(EconomyMode::ThreeResource)
        .with_nations(Some("aetherkin"), Some("forgeborn"))
        .with_strategies("mass_arclight", "mass_bulwark")
        .with_hashing(true);
    let run = || {
        let mut app = headless::ai_vs_ai(shipped(), &s).unwrap();
        headless::tick(&mut app, 3_000);
        app.world().resource::<StateHashLog>().0.clone()
    };
    let (x, y) = (run(), run());
    assert_eq!(x.len(), 3_000);
    assert_eq!(x, y);
}

// ---- AC5: a fallen guard detachment is replaced once per wave --------------------

use onus::sim::{AiAction, AiJournal, AttackMoveDest};

/// F-083's rule as shipped: guards that fall are not refilled body by body,
/// but the detachment IS refilled after the next attack wave goes out. Kill
/// side A's whole guard detachment the tick it is sent to the middle well;
/// A must not re-guard the middle before its next wave, and must re-guard it
/// after that wave.
#[test]
fn critic_a_fallen_guard_detachment_is_replaced_after_the_next_wave() {
    let s = MatchSettings::default()
        .with_seed(3)
        .with_economy(EconomyMode::ThreeResource)
        .with_strategies("mass_sentinel", "mass_sentinel");
    let mut app = headless::ai_vs_ai(shipped(), &s).unwrap();
    let mid_entity = {
        let (k, _) = middle(&mut app);
        wells(&mut app)[k].0
    };
    let mid_pos = middle(&mut app).1;
    let guard_to_mid = |app: &App, from: usize| -> Option<usize> {
        let j = &app.world().resource::<AiJournal>().0;
        j.iter().enumerate().skip(from).find_map(|(i, (_, f, a))| {
            (*f == Faction::A && matches!(a, AiAction::Guard { well, .. } if *well == mid_entity))
                .then_some(i)
        })
    };
    // 1. Wait until A has sent its full detachment (`ai.guards` bodies) to
    //    the middle; the same tick, every member is still attack-moving there.
    let full = shipped().three_resource.as_ref().unwrap().ai.guards;
    let sent = |app: &App| -> u32 {
        app.world().resource::<AiJournal>().0.iter().filter_map(|(_, f, a)| match a {
            AiAction::Guard { well, force } if *f == Faction::A && *well == mid_entity => Some(*force),
            _ => None,
        }).sum()
    };
    let mut t = 0;
    while sent(&app) < full {
        headless::tick(&mut app, 1);
        t += 1;
        assert!(t < 20_000, "A never sent its full detachment to the middle well");
    }
    assert_eq!(sent(&app), full, "the first detachment is exactly `ai.guards`");
    // 2. Kill every A unit attack-moving to the middle well (the detachment).
    let doomed: Vec<Entity> = {
        let mut q = app.world_mut().query::<(Entity, &Faction, &AttackMoveDest)>();
        q.iter(app.world())
            .filter(|(_, f, d)| **f == Faction::A && d.0 == mid_pos)
            .map(|(e, ..)| e)
            .collect()
    };
    assert!(!doomed.is_empty(), "the detachment is attack-moving to the well");
    for e in doomed {
        app.world_mut().despawn(e);
    }
    let after_kill = app.world().resource::<AiJournal>().0.len();
    // 3. Until A's next wave: no new guard to the middle.
    let wave = loop {
        headless::tick(&mut app, 1);
        t += 1;
        assert!(t < 50_000, "A sent no further wave");
        let j = &app.world().resource::<AiJournal>().0;
        if let Some(i) = j.iter().enumerate().skip(after_kill).find_map(|(i, (_, f, a))| {
            (*f == Faction::A && matches!(a, AiAction::Attack { .. })).then_some(i)
        }) {
            break i;
        }
    };
    let early = guard_to_mid(&app, after_kill);
    assert!(early.is_none() || early.unwrap() > wave, "refilled before the wave");
    // 4. After the wave: the detachment is refilled within a few decisions.
    for _ in 0..600 {
        if guard_to_mid(&app, wave).is_some() {
            return;
        }
        headless::tick(&mut app, 1);
    }
    panic!("A never re-guarded the middle well after its next wave");
}
