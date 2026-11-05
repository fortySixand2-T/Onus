//! C3: the three-resource economy (Biomass grown, Aether channelled), opt-in
//! through `MatchSettings::economy`. Every number a test checks is read from
//! the shipped RON, never restated here.

use bevy::prelude::*;
use std::path::PathBuf;

use onus::headless::Orientation;
use onus::headless::{self, MatchSettings};
use onus::sim::content::Content;
use onus::sim::three::{EconomyMode, LeyWell, ThreeEconomy, WellState};
use onus::sim::{
    AiCommanders, Building, CommandQueue, Faction, Health, Order, Position, Stockpiles, UnitDefIdx,
};

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data")
}

fn shipped() -> Content {
    Content::load_from_dir(&data_dir()).expect("content loads")
}

/// A match with no commanders: only what a test orders happens.
fn quiet(economy: EconomyMode) -> App {
    let s = MatchSettings::default().with_economy(economy);
    let mut app = headless::battle(shipped(), &s).expect("builds");
    app.insert_resource(AiCommanders::new(0, &[]));
    app
}

fn push(app: &mut App, o: Order, by: Faction) {
    app.world_mut()
        .resource_mut::<CommandQueue>()
        .0
        .push_back(o.issued_by(by));
}

fn biomass(app: &App, f: Faction) -> u32 {
    app.world().resource::<Stockpiles>().biomass(f)
}

/// `(per_interval, storage)` of the growth source on building `id`.
fn source(c: &Content, id: &str) -> (u32, u32) {
    let t = c
        .three_resource
        .as_ref()
        .expect("shipped content has the block");
    let s = t
        .growth
        .sources
        .iter()
        .find(|s| s.building == id)
        .unwrap_or_else(|| panic!("a growth source on `{id}`"));
    (s.per_interval, s.storage)
}

const BASE: [Vec2; 2] = [Vec2::new(-750.0, 0.0), Vec2::new(750.0, 0.0)];

// ---- AC1: Biomass -------------------------------------------------------------

#[test]
fn the_shipped_content_carries_a_three_resource_block_and_alloy_mode_ignores_it() {
    let c = shipped();
    let t = c
        .three_resource
        .as_ref()
        .expect("resources.ron has `three_resource`");
    assert!(t.growth.interval_ticks > 0);
    // The Grown resource's own building is a growth source, and so is the HQ
    // (a floor every side has from the first tick).
    let grown = c
        .resources
        .iter()
        .find(|r| r.acquisition == onus::sim::content::Acquisition::Grown)
        .expect("a Grown resource");
    assert!(source(&c, &grown.building).0 > 0);
    assert!(source(&c, "hq").0 > 0);

    // The default is Alloy-only, and an Alloy-only match has no three-resource
    // state at all: no resource, no Biomass, ever.
    assert_eq!(MatchSettings::default().economy, EconomyMode::Alloy);
    let mut app = quiet(EconomyMode::Alloy);
    headless::tick(&mut app, t.growth.interval_ticks * 3);
    assert!(!app.world().contains_resource::<ThreeEconomy>());
    for f in [Faction::A, Faction::B] {
        assert_eq!(biomass(&app, f), 0);
        assert_eq!(app.world().resource::<Stockpiles>().aether(f), 0);
    }
}

#[test]
fn biomass_starts_from_the_ron_and_grows_from_the_hq_every_interval() {
    let c = shipped();
    let t = c.three_resource.clone().unwrap();
    let (hq_rate, _) = source(&c, "hq");
    let every = t.growth.interval_ticks;
    let mut app = quiet(EconomyMode::ThreeResource);
    assert!(app.world().contains_resource::<ThreeEconomy>());
    for f in [Faction::A, Faction::B] {
        assert_eq!(biomass(&app, f), t.starting_biomass);
        assert_eq!(
            app.world().resource::<Stockpiles>().aether(f),
            t.starting_aether
        );
    }
    // Nothing lands before the interval is up...
    headless::tick(&mut app, every - 1);
    for f in [Faction::A, Faction::B] {
        assert_eq!(biomass(&app, f), t.starting_biomass);
    }
    // ...and the HQ's growth lands on its last tick, once per interval.
    headless::tick(&mut app, 1);
    for f in [Faction::A, Faction::B] {
        assert_eq!(biomass(&app, f), t.starting_biomass + hq_rate);
    }
    headless::tick(&mut app, every * 2);
    for f in [Faction::A, Faction::B] {
        assert_eq!(biomass(&app, f), t.starting_biomass + 3 * hq_rate);
    }
}

#[test]
fn biomass_stops_at_the_storage_cap() {
    let c = shipped();
    let t = c.three_resource.clone().unwrap();
    let (hq_rate, hq_storage) = source(&c, "hq");
    assert!(
        t.starting_biomass < hq_storage,
        "the start is under the HQ's storage"
    );
    let intervals = (hq_storage - t.starting_biomass).div_ceil(hq_rate) + 3;
    let mut app = quiet(EconomyMode::ThreeResource);
    headless::tick(&mut app, t.growth.interval_ticks * intervals);
    for f in [Faction::A, Faction::B] {
        assert_eq!(biomass(&app, f), hq_storage, "growth stops at the cap");
    }
}

#[test]
fn a_gene_vats_adds_growth_and_storage_to_its_own_side_only() {
    let c = shipped();
    let t = c.three_resource.clone().unwrap();
    let (hq_rate, hq_storage) = source(&c, "hq");
    let (vats_rate, vats_storage) = source(&c, "gene_vats");
    let vats = c.building_index("gene_vats").unwrap();
    let every = t.growth.interval_ticks;

    let mut app = quiet(EconomyMode::ThreeResource);
    // Placed on tick 0, so it stands for the first interval.
    push(
        &mut app,
        Order::Place {
            faction: Faction::A,
            building: vats,
            pos: BASE[0] + Vec2::new(0.0, -150.0),
        },
        Faction::A,
    );
    headless::tick(&mut app, every);
    let mut q = app.world_mut().query::<(&Building, &Faction)>();
    assert_eq!(
        q.iter(app.world())
            .filter(|(b, f)| b.def == vats && **f == Faction::A)
            .count(),
        1,
        "the Vats was placed"
    );
    assert_eq!(
        biomass(&app, Faction::A),
        t.starting_biomass + hq_rate + vats_rate
    );
    assert_eq!(biomass(&app, Faction::B), t.starting_biomass + hq_rate);

    // Run both sides to their caps: A's is the HQ's storage plus the Vats'.
    let intervals = (hq_storage + vats_storage).div_ceil(hq_rate.min(vats_rate)) + 2;
    headless::tick(&mut app, every * intervals);
    assert_eq!(biomass(&app, Faction::A), hq_storage + vats_storage);
    assert_eq!(biomass(&app, Faction::B), hq_storage);
}

#[test]
fn a_three_resource_match_is_deterministic_and_hashes_its_banks() {
    let run = || {
        let s = MatchSettings::default()
            .with_seed(3)
            .with_economy(EconomyMode::ThreeResource)
            .with_hashing(true);
        let mut app = headless::ai_vs_ai(shipped(), &s).unwrap();
        headless::tick(&mut app, 900);
        app.world().resource::<onus::sim::StateHashLog>().clone()
    };
    assert_eq!(run(), run());

    // Biomass is in the hash: the same world with one more Biomass hashes
    // differently.
    let mut app = quiet(EconomyMode::ThreeResource);
    headless::tick(&mut app, 2);
    let before = onus::sim::state_hash(app.world_mut());
    let _ = app.world_mut().resource_mut::<Stockpiles>().add_in(
        Faction::A,
        onus::sim::three::Bank::Biomass,
        1,
    );
    assert_ne!(before, onus::sim::state_hash(app.world_mut()));
}

// ---- AC2: Aether from ley-wells ------------------------------------------------

/// Where the RON puts site `(along, side)` between two HQs, worked out here
/// independently of the sim: along the line from A's HQ to B's, then across
/// it (to the left of A→B).
fn site_at(a: Vec2, b: Vec2, along: f32, side: f32) -> Vec2 {
    let d = (b - a).normalize();
    a + (b - a) * along + Vec2::new(-d.y, d.x) * side
}

/// Every well, by site index: `(position, well)`.
fn wells(app: &mut App) -> Vec<(Vec2, LeyWell)> {
    let mut q = app.world_mut().query::<(&Position, &LeyWell)>();
    let mut v: Vec<(Vec2, LeyWell)> = q.iter(app.world()).map(|(p, w)| (p.0, *w)).collect();
    v.sort_by_key(|(_, w)| w.site());
    v
}

fn aether(app: &App, f: Faction) -> u32 {
    app.world().resource::<Stockpiles>().aether(f)
}

/// A worker of `f` standing at `at` (a non-combatant: two of them on one well
/// never fight, so a test of who holds it is not a test of combat).
fn worker_at(app: &mut App, f: Faction, at: Vec2) -> Entity {
    let c = app.world().resource::<Content>().clone();
    let w = c.unit_index("worker").unwrap();
    app.world_mut()
        .spawn((
            Position(at),
            UnitDefIdx(w),
            c.units[w].mvp_kind,
            f,
            Health::from_def(&c, w),
        ))
        .id()
}

fn middle_well(app: &mut App) -> Vec2 {
    let c = app.world().resource::<Content>().clone();
    let t = c.three_resource.unwrap();
    let k = t
        .wells
        .sites
        .iter()
        .position(|s| s.along == 0.5 && s.side == 0.0)
        .expect("a well in the middle of the map");
    wells(app)[k].0
}

#[test]
fn three_resource_matches_place_the_ron_ley_wells_between_the_hqs() {
    let c = shipped();
    let t = c.three_resource.clone().unwrap();
    assert!(
        t.wells.sites.len() >= 3,
        "a home well each and a contested one"
    );

    let mut alloy = quiet(EconomyMode::Alloy);
    assert!(
        wells(&mut alloy).is_empty(),
        "an Alloy-only match has no well"
    );

    for orientation in Orientation::ALL {
        let s = MatchSettings::default()
            .with_economy(EconomyMode::ThreeResource)
            .with_orientation(orientation);
        let mut app = headless::battle(shipped(), &s).unwrap();
        let (a, b) = match orientation {
            Orientation::Normal => (BASE[0], BASE[1]),
            Orientation::Swapped => (BASE[1], BASE[0]),
        };
        let got = wells(&mut app);
        assert_eq!(got.len(), t.wells.sites.len());
        for (k, site) in t.wells.sites.iter().enumerate() {
            let want = site_at(a, b, site.along, site.side);
            assert!(
                got[k].0.distance(want) < 1e-3,
                "{orientation:?} site {k}: {} vs {want}",
                got[k].0
            );
            assert_eq!(got[k].1.state(), WellState::Empty);
        }
        // The layout is the same map either way round: every well of one
        // orientation stands where a well of the other does.
        let normal: Vec<Vec2> = t
            .wells
            .sites
            .iter()
            .map(|s| site_at(BASE[0], BASE[1], s.along, s.side))
            .collect();
        for (p, _) in &got {
            assert!(
                normal.iter().any(|q| q.distance(*p) < 1e-3),
                "{p} is a site"
            );
        }
    }
}

#[test]
fn a_held_well_channels_aether_to_its_holder_every_interval() {
    let c = shipped();
    let t = c.three_resource.clone().unwrap();
    let w = &t.wells;
    let mut app = quiet(EconomyMode::ThreeResource);
    let mid = middle_well(&mut app);
    // At the edge of the radius still holds.
    worker_at(
        &mut app,
        Faction::A,
        mid + Vec2::new(w.hold_radius - 1.0, 0.0),
    );
    headless::tick(&mut app, w.interval_ticks - 1);
    assert_eq!(aether(&app, Faction::A), t.starting_aether);
    headless::tick(&mut app, 1);
    assert_eq!(aether(&app, Faction::A), t.starting_aether + w.per_interval);
    assert_eq!(aether(&app, Faction::B), t.starting_aether);
    headless::tick(&mut app, w.interval_ticks * 2);
    assert_eq!(
        aether(&app, Faction::A),
        t.starting_aether + 3 * w.per_interval
    );
    let k = t.wells.sites.iter().position(|s| s.along == 0.5).unwrap();
    assert_eq!(wells(&mut app)[k].1.state(), WellState::Held(Faction::A));

    // Outside the radius holds nothing.
    let mut app = quiet(EconomyMode::ThreeResource);
    let mid = middle_well(&mut app);
    worker_at(
        &mut app,
        Faction::B,
        mid + Vec2::new(0.0, w.hold_radius + 1.0),
    );
    headless::tick(&mut app, w.interval_ticks * 2);
    assert_eq!(aether(&app, Faction::B), t.starting_aether);
}

#[test]
fn a_contested_or_empty_well_channels_nothing_and_its_progress_pauses() {
    let c = shipped();
    let t = c.three_resource.clone().unwrap();
    let w = &t.wells;
    let k = t.wells.sites.iter().position(|s| s.along == 0.5).unwrap();
    let half = w.interval_ticks / 2;

    let mut app = quiet(EconomyMode::ThreeResource);
    let mid = middle_well(&mut app);
    // A holds for half an interval...
    let a = worker_at(&mut app, Faction::A, mid);
    headless::tick(&mut app, half);
    // ...then B arrives: contested, nobody banks, however long.
    let b = worker_at(&mut app, Faction::B, mid + Vec2::new(5.0, 0.0));
    headless::tick(&mut app, w.interval_ticks * 3);
    assert_eq!(wells(&mut app)[k].1.state(), WellState::Contested);
    assert_eq!(aether(&app, Faction::A), t.starting_aether);
    assert_eq!(aether(&app, Faction::B), t.starting_aether);
    // B leaves: A holds again and finishes the interval it had started.
    app.world_mut().despawn(b);
    headless::tick(&mut app, w.interval_ticks - half - 1);
    assert_eq!(aether(&app, Faction::A), t.starting_aether);
    headless::tick(&mut app, 1);
    assert_eq!(aether(&app, Faction::A), t.starting_aether + w.per_interval);

    // A leaves: empty, paused (the holder is kept, the clock stops).
    headless::tick(&mut app, half);
    app.world_mut().despawn(a);
    headless::tick(&mut app, w.interval_ticks * 3);
    assert_eq!(wells(&mut app)[k].1.state(), WellState::Empty);
    assert_eq!(aether(&app, Faction::A), t.starting_aether + w.per_interval);
    worker_at(&mut app, Faction::A, mid);
    headless::tick(&mut app, w.interval_ticks - half - 1);
    assert_eq!(aether(&app, Faction::A), t.starting_aether + w.per_interval);
    headless::tick(&mut app, 1);
    assert_eq!(
        aether(&app, Faction::A),
        t.starting_aether + 2 * w.per_interval
    );
}

#[test]
fn a_change_of_holder_restarts_the_progress() {
    let c = shipped();
    let t = c.three_resource.clone().unwrap();
    let w = &t.wells;
    let half = w.interval_ticks / 2;
    let mut app = quiet(EconomyMode::ThreeResource);
    let mid = middle_well(&mut app);
    let a = worker_at(&mut app, Faction::A, mid);
    headless::tick(&mut app, half);
    app.world_mut().despawn(a);
    worker_at(&mut app, Faction::B, mid);
    // B's interval starts when B takes it: A's half counts for nothing.
    headless::tick(&mut app, w.interval_ticks - 1);
    assert_eq!(aether(&app, Faction::B), t.starting_aether);
    headless::tick(&mut app, 1);
    assert_eq!(aether(&app, Faction::B), t.starting_aether + w.per_interval);
    assert_eq!(aether(&app, Faction::A), t.starting_aether);
}

#[test]
fn a_building_holds_a_well_like_a_unit() {
    let c = shipped();
    let t = c.three_resource.clone().unwrap();
    let w = &t.wells;
    let spire = c.building_index("aether_spire").unwrap();
    let mut app = quiet(EconomyMode::ThreeResource);
    let mid = middle_well(&mut app);
    push(
        &mut app,
        Order::Place {
            faction: Faction::B,
            building: spire,
            pos: mid + Vec2::new(0.0, 20.0),
        },
        Faction::B,
    );
    headless::tick(&mut app, w.interval_ticks + 2);
    assert_eq!(aether(&app, Faction::B), t.starting_aether + w.per_interval);
}

// ---- AC3: units are paid in their brief resource ------------------------------

use onus::sim::three::Bank;
use onus::sim::ProductionQueue;

/// Give `f` `n` more of `bank`, directly (a test's shortcut to a rich side).
fn grant(app: &mut App, f: Faction, bank: Bank, n: u32) {
    let _ = app
        .world_mut()
        .resource_mut::<Stockpiles>()
        .add_in(f, bank, n);
}

fn banks(app: &App, f: Faction) -> [u32; 3] {
    let s = app.world().resource::<Stockpiles>();
    [s.alloy(f), s.biomass(f), s.aether(f)]
}

/// Place building `id` for `f` near its base and return it, after one tick.
fn place(app: &mut App, f: Faction, id: &str, offset: Vec2) -> Entity {
    let c = app.world().resource::<Content>().clone();
    let def = c.building_index(id).unwrap();
    let base = if f == Faction::A { BASE[0] } else { BASE[1] };
    push(
        app,
        Order::Place {
            faction: f,
            building: def,
            pos: base + offset,
        },
        f,
    );
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

fn train(app: &mut App, f: Faction, at: Entity, unit: &str) {
    let u = app.world().resource::<Content>().unit_index(unit).unwrap();
    push(
        app,
        Order::Train {
            building: at,
            unit: u,
        },
        f,
    );
    headless::tick(app, 1);
}

fn queued(app: &App, at: Entity) -> usize {
    app.world().get::<ProductionQueue>(at).unwrap().items.len()
}

/// `[alloy, biomass, aether]` with `amount` in the slot of `resource`.
fn priced(c: &Content, unit: &str) -> [u32; 3] {
    let u = c.unit(unit).unwrap();
    let mut v = [0; 3];
    let k = match Bank::of_resource(c, &u.cost.resource).unwrap() {
        Bank::Alloy => 0,
        Bank::Biomass => 1,
        Bank::Aether => 2,
    };
    v[k] = u.cost.amount;
    v
}

fn minus(a: [u32; 3], b: [u32; 3]) -> [u32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

#[test]
fn the_brief_prices_units_in_their_own_resource() {
    let c = shipped();
    let bank = |id: &str| Bank::of_resource(&c, &c.unit(id).unwrap().cost.resource).unwrap();
    for id in ["worker", "bulwark", "sentinel"] {
        assert_eq!(bank(id), Bank::Alloy, "{id}");
    }
    for id in ["ripper", "ravager"] {
        assert_eq!(bank(id), Bank::Biomass, "{id}");
    }
    assert_eq!(bank("arclight"), Bank::Aether);
}

#[test]
fn in_three_resource_mode_each_unit_charges_its_own_bank_and_buildings_charge_alloy() {
    let c = shipped();
    let mut app = quiet(EconomyMode::ThreeResource);
    grant(&mut app, Faction::A, Bank::Alloy, 2_000);
    grant(&mut app, Faction::A, Bank::Biomass, 500);
    grant(&mut app, Faction::A, Bank::Aether, 500);

    let before = banks(&app, Faction::A);
    let vats = place(&mut app, Faction::A, "gene_vats", Vec2::new(0.0, -150.0));
    let cost = c.building("gene_vats").unwrap().alloy_cost;
    assert_eq!(
        banks(&app, Faction::A),
        minus(before, [cost, 0, 0]),
        "buildings: Alloy"
    );
    let spire = place(
        &mut app,
        Faction::A,
        "aether_spire",
        Vec2::new(150.0, -150.0),
    );
    let foundry = place(&mut app, Faction::A, "foundry", Vec2::new(-150.0, -150.0));
    let hq = {
        let hq = c.building_index("hq").unwrap();
        let mut q = app.world_mut().query::<(Entity, &Building, &Faction)>();
        q.iter(app.world())
            .find(|(_, b, f)| b.def == hq && **f == Faction::A)
            .unwrap()
            .0
    };

    for (at, unit) in [
        (vats, "ripper"),
        (vats, "ravager"),
        (spire, "arclight"),
        (foundry, "sentinel"),
        (foundry, "bulwark"),
        (hq, "worker"),
    ] {
        let before = banks(&app, Faction::A);
        let q = queued(&app, at);
        train(&mut app, Faction::A, at, unit);
        assert_eq!(queued(&app, at), q + 1, "{unit} queued");
        assert_eq!(
            banks(&app, Faction::A),
            minus(before, priced(&c, unit)),
            "{unit} is charged its cost, in its own resource"
        );
    }
}

#[test]
fn a_unit_its_side_cannot_pay_for_in_its_resource_is_refused_and_charges_nothing() {
    let c = shipped();
    let t = c.three_resource.clone().unwrap();
    let mut app = quiet(EconomyMode::ThreeResource);
    grant(&mut app, Faction::A, Bank::Alloy, 2_000);
    let spire = place(
        &mut app,
        Faction::A,
        "aether_spire",
        Vec2::new(150.0, -150.0),
    );
    assert!(t.starting_aether < c.unit("arclight").unwrap().cost.amount);
    // Plenty of Alloy and Biomass, too little Aether: refused.
    let before = banks(&app, Faction::A);
    train(&mut app, Faction::A, spire, "arclight");
    assert_eq!(queued(&app, spire), 0);
    assert_eq!(banks(&app, Faction::A), before);
    // Exactly enough: accepted, and the bank is emptied.
    let need = c.unit("arclight").unwrap().cost.amount - before[2];
    grant(&mut app, Faction::A, Bank::Aether, need);
    train(&mut app, Faction::A, spire, "arclight");
    assert_eq!(queued(&app, spire), 1);
    assert_eq!(banks(&app, Faction::A)[2], 0);
}

#[test]
fn alloy_only_mode_still_prices_every_unit_in_alloy() {
    let c = shipped();
    let mut app = quiet(EconomyMode::Alloy);
    grant(&mut app, Faction::A, Bank::Alloy, 2_000);
    let vats = place(&mut app, Faction::A, "gene_vats", Vec2::new(0.0, -150.0));
    let before = banks(&app, Faction::A);
    train(&mut app, Faction::A, vats, "ripper");
    assert_eq!(queued(&app, vats), 1);
    let mvp = c.unit("ripper").unwrap().mvp_alloy_cost;
    assert_eq!(banks(&app, Faction::A), minus(before, [mvp, 0, 0]));
}

// ---- AC4: the real domain leans --------------------------------------------------

use onus::sim::nation::{NationRules, SideRules, TraitDef, TraitStat, TraitTarget};
use onus::sim::{Carrying, GatherPhase, GatherTarget, ResourceNode};

fn quiet_nations(economy: EconomyMode, a: Option<&str>, b: Option<&str>) -> App {
    let s = MatchSettings::default()
        .with_economy(economy)
        .with_nations(a, b);
    let mut app = headless::battle(shipped(), &s).expect("builds");
    app.insert_resource(AiCommanders::new(0, &[]));
    app
}

/// The three-resource trait of nation `id` on `stat`, if it has one.
fn lean(c: &Content, id: &str, stat: TraitStat) -> Option<TraitDef> {
    c.nations
        .iter()
        .find(|n| n.id == id)
        .unwrap()
        .three_resource_traits
        .iter()
        .find(|t| t.stat == stat)
        .cloned()
}

/// `round(v * mul)`, half up, from the RON's float (the brief's rule).
fn scaled(v: u32, t: &TraitDef) -> u32 {
    let milli = (t.mul.unwrap() * 1000.0).round() as u64;
    ((v as u64 * milli + 500) / 1000) as u32
}

#[test]
fn every_nation_carries_its_brief_lean_for_the_three_resource_economy() {
    let c = shipped();
    // Forgeborn: Foundry units cheaper, sturdier buildings.
    let t = lean(&c, "forgeborn", TraitStat::Cost).expect("forgeborn: cheaper Foundry units");
    assert_eq!(t.applies_to, TraitTarget::Barracks("foundry".into()));
    assert!(t.mul.unwrap() < 1.0);
    assert!(
        lean(&c, "forgeborn", TraitStat::BuildingHp)
            .unwrap()
            .mul
            .unwrap()
            > 1.0
    );
    // Verdant: Biomass grows faster, Gene-Vats train faster.
    assert!(lean(&c, "verdant", TraitStat::Growth).unwrap().mul.unwrap() > 1.0);
    let t = lean(&c, "verdant", TraitStat::TrainTicks).unwrap();
    assert_eq!(t.applies_to, TraitTarget::Barracks("gene_vats".into()));
    // Aetherkin: cheaper Aether, longer Arclight range.
    let t = lean(&c, "aetherkin", TraitStat::Cost).expect("aetherkin: cheaper Aether");
    assert_eq!(t.applies_to, TraitTarget::Barracks("aether_spire".into()));
    assert!(t.mul.unwrap() < 1.0);
    assert!(
        lean(&c, "aetherkin", TraitStat::AttackRange)
            .unwrap()
            .add
            .unwrap()
            > 0
    );
    // Tollmarch: richer Alloy mines, faster gathering.
    let t = lean(&c, "tollmarch", TraitStat::AlloyYield).expect("tollmarch: richer mines");
    assert_eq!(t.applies_to, TraitTarget::Workers);
    assert!(t.mul.unwrap() > 1.0);
    assert!(
        lean(&c, "tollmarch", TraitStat::GatherTicks)
            .unwrap()
            .mul
            .unwrap()
            < 1.0
    );
    // Ashen Reach: more building HP.
    assert!(
        lean(&c, "ashen_reach", TraitStat::BuildingHp)
            .unwrap()
            .mul
            .unwrap()
            > 1.0
    );
    // House Vael: no lean; Synthesis (both economies).
    let house = c.nations.iter().find(|n| n.id == "house").unwrap();
    assert!(house.three_resource_traits.is_empty());
    assert!(house.synthesis.is_some());
}

#[test]
fn verdant_grows_biomass_faster_in_three_resource_mode() {
    let c = shipped();
    let t = c.three_resource.clone().unwrap();
    let growth = lean(&c, "verdant", TraitStat::Growth).unwrap();
    let (hq_rate, hq_storage) = source(&c, "hq");
    let mut app = quiet_nations(EconomyMode::ThreeResource, Some("verdant"), None);
    let k = 40;
    assert!(
        t.starting_biomass + k * hq_rate * 2 < hq_storage,
        "under the cap"
    );
    headless::tick(&mut app, t.growth.interval_ticks * k);
    // Accumulated in per-mille: k intervals of `rate * milli`, whole only.
    let milli = (growth.mul.unwrap() * 1000.0).round() as u32;
    assert_eq!(
        biomass(&app, Faction::A),
        t.starting_biomass + k * hq_rate * milli / 1000
    );
    assert_eq!(biomass(&app, Faction::B), t.starting_biomass + k * hq_rate);
    assert!(biomass(&app, Faction::A) > biomass(&app, Faction::B));
}

#[test]
fn aetherkin_pays_less_aether_and_forgeborn_less_alloy_for_their_units() {
    let c = shipped();
    for (nation, building, unit, bank) in [
        ("aetherkin", "aether_spire", "arclight", Bank::Aether),
        ("forgeborn", "foundry", "sentinel", Bank::Alloy),
    ] {
        let t = lean(&c, nation, TraitStat::Cost).unwrap();
        let price = scaled(c.unit(unit).unwrap().cost.amount, &t);
        assert!(price < c.unit(unit).unwrap().cost.amount);
        let mut app = quiet_nations(EconomyMode::ThreeResource, Some(nation), None);
        grant(&mut app, Faction::A, Bank::Alloy, 2_000);
        grant(&mut app, Faction::A, Bank::Aether, 500);
        let at = place(&mut app, Faction::A, building, Vec2::new(150.0, -150.0));
        let before = app
            .world()
            .resource::<Stockpiles>()
            .amount(Faction::A, bank);
        train(&mut app, Faction::A, at, unit);
        assert_eq!(queued(&app, at), 1);
        assert_eq!(
            app.world()
                .resource::<Stockpiles>()
                .amount(Faction::A, bank),
            before - price,
            "{nation} pays {price} for a {unit}"
        );
    }
}

#[test]
fn tollmarch_banks_more_alloy_per_load_in_three_resource_mode_only() {
    let c = shipped();
    let rich = lean(&c, "tollmarch", TraitStat::AlloyYield).unwrap();
    let carry = c.unit("worker").unwrap().mvp_carry_capacity;
    let loads = 10u32;
    // Hand one worker a full load at its own HQ, `loads` times, and count
    // what the bank takes.
    let banked = |economy: EconomyMode| -> [u32; 2] {
        let mut app = quiet_nations(economy, Some("tollmarch"), None);
        let w = c.unit_index("worker").unwrap();
        let node = {
            let mut q = app.world_mut().query::<(Entity, &ResourceNode)>();
            q.iter(app.world()).next().unwrap().0
        };
        let mut out = [0; 2];
        for (k, f) in [Faction::A, Faction::B].into_iter().enumerate() {
            let base = if f == Faction::A { BASE[0] } else { BASE[1] };
            let worker = app
                .world_mut()
                .spawn((
                    Position(base),
                    UnitDefIdx(w),
                    c.units[w].mvp_kind,
                    f,
                    Health::from_def(&c, w),
                ))
                .id();
            let before = app.world().resource::<Stockpiles>().alloy(f);
            for _ in 0..loads {
                app.world_mut().entity_mut(worker).insert((
                    Position(base),
                    Carrying(carry),
                    GatherTarget(node),
                    GatherPhase::ToDropoff,
                ));
                headless::tick(&mut app, 1);
                assert_eq!(
                    app.world().get::<Carrying>(worker).unwrap().0,
                    0,
                    "deposited"
                );
            }
            out[k] = app.world().resource::<Stockpiles>().alloy(f) - before;
        }
        out
    };
    let milli = (rich.mul.unwrap() * 1000.0).round() as u32;
    assert_eq!(
        banked(EconomyMode::ThreeResource),
        [loads * carry * milli / 1000, loads * carry],
        "Tollmarch banks its yield, accumulated per-mille; the other side banks its loads"
    );
    // Alloy-only: Tollmarch plays its stand-in (faster gathering), and a load
    // is a load.
    assert_eq!(banked(EconomyMode::Alloy), [loads * carry, loads * carry]);
}

#[test]
fn alloy_only_mode_keeps_the_stand_in_traits_and_three_resource_mode_plays_the_leans() {
    let c = shipped();
    let arclight = c.unit_index("arclight").unwrap();
    let names = [Some("aetherkin".to_string()), Some("verdant".to_string())];
    let alloy = NationRules::resolve_in(&c, &names, EconomyMode::Alloy)
        .unwrap()
        .unwrap();
    let three = NationRules::resolve_in(&c, &names, EconomyMode::ThreeResource)
        .unwrap()
        .unwrap();
    // `resolve` is the Alloy-only one, as before C3.
    let plain = NationRules::resolve(&c, &names).unwrap().unwrap();
    let a = |r: &NationRules| r.side(Faction::A).unwrap().units[arclight].clone();
    assert_eq!(a(&plain).cost.amount, a(&alloy).cost.amount);
    assert_eq!(
        a(&alloy).cost.amount,
        c.units[arclight].cost.amount,
        "no Cost in Alloy"
    );
    assert!(a(&three).cost.amount < c.units[arclight].cost.amount);
    // The installed rules follow the match's mode.
    let app = quiet_nations(EconomyMode::ThreeResource, Some("aetherkin"), None);
    let r = app.world().resource::<NationRules>();
    assert_eq!(
        r.side(Faction::A).unwrap().units[arclight].cost.amount,
        a(&three).cost.amount
    );
}

#[test]
fn a_trait_that_cannot_act_in_its_list_s_economy_is_refused() {
    let c = shipped();
    let k = c.nations.iter().position(|n| n.id == "forgeborn").unwrap();
    let three_only = TraitDef {
        applies_to: TraitTarget::Buildings,
        stat: TraitStat::Growth,
        add: None,
        mul: Some(1.1),
    };
    let mut bad = c.clone();
    bad.nations[k].traits.push(three_only);
    let err = SideRules::build_in(&bad, k, EconomyMode::Alloy).unwrap_err();
    assert!(err.contains("three-resource"), "{err}");

    let alloy_price = TraitDef {
        applies_to: TraitTarget::Barracks("foundry".into()),
        stat: TraitStat::AlloyCost,
        add: None,
        mul: Some(0.9),
    };
    let mut bad = c.clone();
    bad.nations[k].three_resource_traits.push(alloy_price);
    let err = SideRules::build_in(&bad, k, EconomyMode::ThreeResource).unwrap_err();
    assert!(err.contains("Alloy-only"), "{err}");
}
