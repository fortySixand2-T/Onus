//! L2 integration tests for M4a (economy: Alloy loop + content-as-data).
//!
//! Encodes the M4a acceptance criteria and the critic probes:
//!   AC1 — `units.ron` + `resources.ron` load into sim structs; no hardcoded
//!         costs/stats (every cost the sim charges is traceable to the RON);
//!   AC2 — worker gather/deposit loop with a per-faction stockpile, and the
//!         conservation law gathered == deposited + carried + still-in-deposit;
//!   AC3 — building placement and unit production each consume Alloy, exactly
//!         once, and never on a rejected order.
//!
//! The sim is exercised headless (`MinimalPlugins`) — no render types here.

use std::path::PathBuf;

use onus::sim::content::Content;

/// The repo's data directory, resolved without Bevy's `AssetServer` so the sim
/// stays loadable from a plain path in headless tests.
fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data")
}

fn content() -> Content {
    Content::load_from_dir(&data_dir()).expect("assets/data/*.ron parse into sim structs")
}

// ---- AC1: content is data ---------------------------------------------------

#[test]
fn loads_units_and_resources_from_ron() {
    let c = content();

    // The full MVP roster: Worker + the 5 combat units of the pentagon.
    let ids: Vec<&str> = c.units.iter().map(|u| u.id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["worker", "bulwark", "sentinel", "ripper", "ravager", "arclight"],
        "unit order is the RON order (stable, deterministic)"
    );

    // Stats come across intact.
    let bulwark = c.unit("bulwark").expect("bulwark defined");
    assert_eq!(
        (
            bulwark.speed,
            bulwark.offense,
            bulwark.defense,
            bulwark.armor
        ),
        (2, 4, 9, 9)
    );
    assert_eq!(bulwark.nemesis.as_deref(), Some("ravager"));
    assert!((c.nemesis_bonus.damage_mult - 1.3).abs() < 1e-6);
    assert!(c.nemesis_bonus.ignore_armor);

    // Resources: three domains defined, MVP active set is Alloy only.
    let res_ids: Vec<&str> = c.resources.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(res_ids, vec!["alloy", "biomass", "aether"]);
    assert_eq!(c.mvp_active, vec!["alloy".to_string()]);
    assert_eq!(c.economy.currency, "alloy");

    // Buildings: HQ + the three barracks, each with an Alloy cost.
    let b_ids: Vec<&str> = c.buildings.iter().map(|b| b.id.as_str()).collect();
    assert_eq!(
        b_ids,
        vec!["hq", "foundry", "gene_vats", "aether_spire"],
        "building order is the RON order"
    );
    for b in &c.buildings {
        assert!(b.alloy_cost > 0, "{} has an Alloy cost", b.id);
    }
}

#[test]
fn every_mvp_unit_has_an_alloy_cost_and_a_producer() {
    let c = content();
    for u in &c.units {
        assert!(u.mvp_alloy_cost > 0, "{} costs Alloy in the MVP", u.id);
        assert!(u.mvp_train_ticks > 0, "{} takes time to train", u.id);
        // Every unit is produced by some building that lists it.
        let producer = c
            .buildings
            .iter()
            .find(|b| b.produces.iter().any(|p| p == &u.id));
        assert!(producer.is_some(), "{} is produced by a building", u.id);
    }
    // The worker carries the gather parameters (data, not constants).
    let w = c.unit("worker").unwrap();
    assert!(w.gathers && w.mvp_carry_capacity > 0 && w.mvp_gather_ticks > 0);
}

#[test]
fn content_loads_from_the_default_asset_path() {
    // The driver inserts `Content` at startup from this path; tests run with the
    // repo root as the working directory, same as `cargo run`.
    let c = Content::load_default().expect("assets/data loads from the default path");
    assert_eq!(c.units.len(), 6);
}

#[test]
fn loading_is_deterministic_and_missing_dir_is_an_error() {
    let a = content();
    let b = content();
    assert_eq!(a.units.len(), b.units.len());
    for (x, y) in a.units.iter().zip(b.units.iter()) {
        assert_eq!(x.id, y.id);
        assert_eq!(x.mvp_alloy_cost, y.mvp_alloy_cost);
    }
    assert!(Content::load_from_dir(&data_dir().join("nope")).is_err());
}
