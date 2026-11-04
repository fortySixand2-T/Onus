//! C3: the three-resource economy (Biomass grown, Aether channelled), opt-in
//! through `MatchSettings::economy`. Every number a test checks is read from
//! the shipped RON, never restated here.

use bevy::prelude::*;
use std::path::PathBuf;

use onus::headless::{self, MatchSettings};
use onus::sim::content::Content;
use onus::sim::three::{EconomyMode, ThreeEconomy};
use onus::sim::{AiCommanders, Building, CommandQueue, Faction, Order, Stockpiles};

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
    let t = c.three_resource.as_ref().expect("shipped content has the block");
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
    let t = c.three_resource.as_ref().expect("resources.ron has `three_resource`");
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
        assert_eq!(app.world().resource::<Stockpiles>().aether(f), t.starting_aether);
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
    assert!(t.starting_biomass < hq_storage, "the start is under the HQ's storage");
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
    assert_eq!(biomass(&app, Faction::A), t.starting_biomass + hq_rate + vats_rate);
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
    let _ = app
        .world_mut()
        .resource_mut::<Stockpiles>()
        .add_in(Faction::A, onus::sim::three::Bank::Biomass, 1);
    assert_ne!(before, onus::sim::state_hash(app.world_mut()));
}
