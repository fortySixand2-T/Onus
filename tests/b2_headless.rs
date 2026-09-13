//! L2 integration tests for **B2 AC1** — the lifted headless-match constructor
//! `onus::headless`.
//!
//! Until now the standard AI-vs-AI fixture lived in `benches/replay_hash.rs`
//! and was hand-rolled again in every test that needed one. B2's batch runner
//! needs the same match, so it moves into the library where the bench, the
//! coming bin, and tests share *one* definition.
//!
//! What is encoded here:
//!
//!   - **equivalence** — the default settings reproduce the pre-B2 bench
//!     fixture *exactly*, pinned by per-tick `state_hash` values captured from
//!     that fixture before it was touched. "Lifted unchanged" is demonstrated,
//!     not asserted;
//!   - **shape** — the fixture's spawn positions, node amounts, worker count,
//!     starting Alloy and commanders, read back from the world;
//!   - **fallibility** — an unknown strategy name is refused, never silently
//!     defaulted (B1's rule: a typo must not play the default and be reported
//!     under another name);
//!   - **parameterisation** — naming strategies changes who plays, and hashing
//!     stays opt-in;
//!   - **determinism** — same settings, same content ⇒ identical per-tick hash
//!     log and identical `AiJournal`;
//!   - **stepping** — `tick(app, n)` advances exactly `n` sim ticks.

use bevy::prelude::*;

use onus::headless::{self, MatchSettings};
use onus::sim::combat::Casualties;
use onus::sim::content::Content;
use onus::sim::economy::{Building, ProductionQueue, Stockpiles, UnitDefIdx};
use onus::sim::spatial::Faction;
use onus::sim::{AiCommanders, AiJournal, CommandLog, Position, ResourceNode, StateHashLog};

fn content() -> Content {
    headless::content().expect("assets/data/*.ron parse into sim structs")
}

fn built(settings: &MatchSettings) -> App {
    headless::ai_vs_ai(content(), settings).expect("the settings name only shipped strategies")
}

// ---- equivalence with the pre-B2 bench fixture ------------------------------

/// Captured from `benches/replay_hash.rs`'s own `ai_vs_ai(4, true)` **before**
/// it was lifted: (tick, state hash) at six points across 600 ticks, plus an
/// FNV-style fold of all 600 so no tick in between can drift unseen.
const BENCH_FIXTURE_HASHES: [(usize, u64); 6] = [
    (1, 0x9a74_d7ad_acad_19be),
    (10, 0x624e_8c15_e192_2eb5),
    (60, 0x3f80_afb2_a96f_1736),
    (120, 0x545c_43c2_42aa_59f0),
    (300, 0xa5b4_138c_f475_fd00),
    (600, 0x1007_e832_7293_09b0),
];
const BENCH_FIXTURE_FOLD: u64 = 0x6060_0370_7bc1_9408;

fn fold(hashes: &[u64]) -> u64 {
    hashes.iter().fold(0xcbf2_9ce4_8422_2325u64, |a, h| {
        (a ^ h).wrapping_mul(0x1000_0000_01b3)
    })
}

#[test]
fn default_settings_reproduce_the_bench_fixture_tick_for_tick() {
    let mut app = built(&MatchSettings::default().with_seed(4).with_hashing(true));
    headless::tick(&mut app, 600);
    let log = &app.world().resource::<StateHashLog>().0;
    assert_eq!(log.len(), 600, "one hash per tick");
    for (n, want) in BENCH_FIXTURE_HASHES {
        assert_eq!(
            log[n - 1],
            want,
            "tick {n} diverges from the pre-B2 bench fixture"
        );
    }
    assert_eq!(
        fold(log),
        BENCH_FIXTURE_FOLD,
        "some tick between the pinned ones diverges from the pre-B2 bench fixture"
    );
}

#[test]
fn the_default_fixture_has_the_shape_the_bench_built() {
    let c = content();
    let app = built(&MatchSettings::default().with_seed(4));

    // Starting Alloy is content, not a constant.
    let stock = app.world().resource::<Stockpiles>();
    for f in [Faction::A, Faction::B] {
        assert_eq!(stock.alloy(f), c.economy.starting_alloy);
    }

    // One HQ per side, at ±750.
    let mut app = app;
    let mut hqs: Vec<(Faction, Vec2)> = app
        .world_mut()
        .query_filtered::<(&Faction, &Position), (With<Building>, With<ProductionQueue>)>()
        .iter(app.world())
        .map(|(f, p)| (*f, p.0))
        .collect();
    hqs.sort_by_key(|(f, _)| format!("{f:?}"));
    assert_eq!(
        hqs,
        vec![
            (Faction::A, Vec2::new(-750.0, 0.0)),
            (Faction::B, Vec2::new(750.0, 0.0)),
        ]
    );

    // One node per side, 250 above the base, 100_000 deep.
    let mut nodes: Vec<(f32, u32)> = app
        .world_mut()
        .query::<(&ResourceNode, &Position)>()
        .iter(app.world())
        .map(|(n, p)| (p.0.x, n.amount))
        .collect();
    nodes.sort_by(|a, b| a.0.total_cmp(&b.0));
    assert_eq!(nodes, vec![(-750.0, 100_000), (750.0, 100_000)]);
    let ys: Vec<f32> = app
        .world_mut()
        .query::<(&ResourceNode, &Position)>()
        .iter(app.world())
        .map(|(_, p)| p.0.y)
        .collect();
    assert!(ys.iter().all(|y| *y == 250.0), "nodes sit above the base");

    // Three workers per side.
    let worker = c.unit_index("worker").unwrap();
    for f in [Faction::A, Faction::B] {
        let n = app
            .world_mut()
            .query::<(&UnitDefIdx, &Faction)>()
            .iter(app.world())
            .filter(|(d, who)| d.0 == worker && **who == f)
            .count();
        assert_eq!(n, 3, "{f:?} starts with three workers");
    }

    // Two commanders, both on the content default, and a log stamped with the
    // match seed.
    let cs = app.world().resource::<AiCommanders>();
    assert_eq!(cs.commanders().len(), 2);
    assert!(cs.commanders().iter().all(|c| c.strategy_index().is_none()));
    assert_eq!(app.world().resource::<CommandLog>().seed(), 4);
    // Resources the sim chain needs, present.
    assert!(app.world().contains_resource::<Casualties>());
}

// ---- naming a strategy stays fallible ---------------------------------------

#[test]
fn an_unknown_strategy_is_refused_not_defaulted() {
    let err = headless::ai_vs_ai(
        content(),
        &MatchSettings::default().with_strategies("rush", "trutle"),
    )
    .expect_err("a typo must not build a match");
    assert_eq!(err.id, "trutle");
    assert_eq!(err.faction, Some(Faction::B));
    assert!(err.to_string().contains("trutle"));
}

#[test]
fn named_strategies_reach_the_commanders() {
    let c = content();
    let app = built(&MatchSettings::default().with_strategies("rush", "turtle"));
    let cs = app.world().resource::<AiCommanders>();
    let names: Vec<&str> = cs
        .commanders()
        .iter()
        .map(|cm| cm.strategy(&c).id.as_str())
        .collect();
    assert_eq!(names, vec!["rush", "turtle"]);
}

#[test]
fn named_strategies_play_differently_from_the_default() {
    let settings = MatchSettings::default().with_seed(7);
    let mut a = built(&settings);
    let mut b = built(&settings.clone().with_strategies("rush", "turtle"));
    headless::tick(&mut a, 900);
    headless::tick(&mut b, 900);
    assert_ne!(
        a.world().resource::<AiJournal>().0,
        b.world().resource::<AiJournal>().0,
        "naming strategies must change what is played"
    );
}

// ---- hashing is opt-in ------------------------------------------------------

#[test]
fn hashing_is_opt_in_and_does_not_change_the_match() {
    let mut plain = built(&MatchSettings::default().with_seed(11));
    assert!(!plain.world().contains_resource::<StateHashLog>());
    let mut hashed = built(&MatchSettings::default().with_seed(11).with_hashing(true));
    headless::tick(&mut plain, 300);
    headless::tick(&mut hashed, 300);
    assert_eq!(
        plain.world().resource::<AiJournal>().0,
        hashed.world().resource::<AiJournal>().0,
        "asking for hashes must not change the sim"
    );
    assert_eq!(hashed.world().resource::<StateHashLog>().0.len(), 300);
}

// ---- determinism ------------------------------------------------------------

#[test]
fn same_settings_same_content_replay_bit_identically() {
    let settings = MatchSettings::default()
        .with_seed(99)
        .with_strategies("mass_ripper", "mass_arclight")
        .with_hashing(true);
    let mut runs = Vec::new();
    for _ in 0..2 {
        let mut app = built(&settings);
        headless::tick(&mut app, 600);
        let hashes = app.world().resource::<StateHashLog>().0.clone();
        let journal = app.world().resource::<AiJournal>().0.clone();
        runs.push((hashes, journal));
    }
    assert_eq!(runs[0].0, runs[1].0, "per-tick hashes must be identical");
    assert_eq!(runs[0].1, runs[1].1, "AI journals must be identical");
    assert!(!runs[0].1.is_empty(), "the commanders did something");
}

#[test]
fn different_seeds_diverge() {
    let s = MatchSettings::default().with_hashing(true);
    let mut a = built(&s.clone().with_seed(1));
    let mut b = built(&s.with_seed(2));
    headless::tick(&mut a, 600);
    headless::tick(&mut b, 600);
    assert_ne!(
        a.world().resource::<StateHashLog>().0,
        b.world().resource::<StateHashLog>().0
    );
}

// ---- the stepping helper ----------------------------------------------------

#[test]
fn tick_advances_exactly_one_sim_tick_per_step() {
    let mut app = built(&MatchSettings::default().with_seed(3).with_hashing(true));
    headless::step(&mut app);
    assert_eq!(app.world().resource::<StateHashLog>().0.len(), 1);
    headless::tick(&mut app, 5);
    assert_eq!(app.world().resource::<StateHashLog>().0.len(), 6);
    let cs = app.world().resource::<AiCommanders>();
    assert!(
        cs.commanders().iter().all(|c| c.tick() == 6),
        "every commander saw six ticks"
    );
}
