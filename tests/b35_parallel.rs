//! L2 integration tests for **B3.5 AC0b** — *parallel* production: a strategy
//! may open the same building more than once, and the commander trains across
//! every barracks it owns that can make the unit.
//!
//! F-027 measured what `queue_depth` actually buys: only the idle gap between a
//! unit popping and the next decision (7 / 8 / 8 units at depth 1 / 3 / 8 over
//! 6 000 ticks), because `economy::production` advances only the queue *head*.
//! One barracks builds one unit at a time whatever the queue holds — so the
//! real throughput lever is **barracks count**, and load-time validation used to
//! refuse a strategy that opened the same building twice (B1 AC1).
//!
//! What is encoded here:
//!   - **neutrality**: with the shipped data (one opening per building) the
//!     default matchup and two named fixture matchups replay to the same
//!     per-tick `state_hash` sequence and the same `AiJournal` as before;
//!   - N openings of one building place **N** buildings, each once, never N+1;
//!   - three barracks finish materially more units than one over a horizon
//!     (the measurement the tuning run needs), and that is a *different*
//!     mechanism from queue depth;
//!   - still **one army order per decision**, with several barracks available;
//!   - affordability: a commander that can pay for one building places one, and
//!     the placements it could not afford consume no randomness;
//!   - determinism, including that the target-barracks choice never depends on
//!     spawn or query order.
//!
//! Everything runs headless (`MinimalPlugins`) through the shipped sim chain.

use std::path::PathBuf;

use bevy::prelude::*;

use onus::sim::combat::{Casualties, Health};
use onus::sim::content::Content;
use onus::sim::economy::{Building, ProductionQueue, Stockpiles, UnitDefIdx};
use onus::sim::spatial::Faction;
use onus::sim::{
    AiAction, AiCommanders, AiJournal, CommandQueue, Position, RateReport, ResourceNode,
};

// ---- harness ----------------------------------------------------------------

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data")
}

fn content() -> Content {
    Content::load_from_dir(&data_dir()).expect("assets/data/*.ron parse into sim structs")
}

/// One strategy entry, spelled out. `barracks` is a list, repeats and all —
/// which is the capability this file is about.
#[allow(clippy::too_many_arguments)]
fn strategy(
    id: &str,
    think: u32,
    worker_target: u32,
    barracks: &[(&str, u32, f32)],
    army: &[(&str, u32)],
    attack_at_army: u32,
    queue_depth: u32,
) -> String {
    let openings: Vec<String> = barracks
        .iter()
        .map(|(b, at, off)| {
            format!("            (building: \"{b}\", at_tick: {at}, offset: {off:?}),")
        })
        .collect();
    let army: Vec<String> = army
        .iter()
        .map(|(u, n)| format!("            (unit: \"{u}\", count: {n}),"))
        .collect();
    format!(
        "        (\n            id: \"{id}\",\n            think_interval_ticks: {think},\n            \
         worker_target: {worker_target},\n            barracks: [\n{}\n            ],\n            army: [\n{}\n            ],\n            \
         attack_at_army: {attack_at_army},\n            attack_interval_ticks: 600,\n            attack_spread: 60.0,\n            \
         queue_depth: {queue_depth},\n        ),",
        openings.join("\n"),
        army.join("\n"),
    )
}

/// The shipped set (default `mvp` intact) plus fixture strategies, as a content
/// dir of its own. Returns the load *result*, so a validation probe can read the
/// error instead of panicking.
fn try_content_with(
    name: &str,
    extra: &[String],
) -> Result<Content, onus::sim::content::ContentError> {
    let shipped = std::fs::read_to_string(data_dir().join("strategies.ron")).expect("strategies");
    let text = shipped.replace("    ],\n)", &format!("{}\n    ],\n)", extra.join("\n")));
    assert_ne!(text, shipped, "fixture splice found no strategies list");
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/b35_parallel")
        .join(name);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    for file in ["units.ron", "resources.ron"] {
        std::fs::copy(data_dir().join(file), dir.join(file)).expect("copy");
    }
    std::fs::write(dir.join("strategies.ron"), text).expect("write strategies.ron");
    Content::load_from_dir(&dir)
}

fn content_with(name: &str, extra: &[String]) -> Content {
    try_content_with(name, extra).expect("fixture content loads")
}

/// The two single-opening probe scripts the neutrality goldens were captured
/// with. Their text is fixed: a golden is only a floor if the fixture cannot
/// drift.
fn neutral_content(name: &str) -> Content {
    content_with(
        name,
        &[
            strategy(
                "solo_ripper",
                30,
                6,
                &[("gene_vats", 300, 130.0)],
                &[("ripper", 1)],
                3,
                1,
            ),
            strategy(
                "solo_bulwark",
                30,
                6,
                &[("foundry", 300, 110.0)],
                &[("bulwark", 1)],
                8,
                1,
            ),
        ],
    )
}

fn step(app: &mut App) {
    let dt = app.world().resource::<Time<Fixed>>().timestep();
    app.world_mut().resource_mut::<Time<Fixed>>().advance_by(dt);
    app.update();
}

fn tick(app: &mut App, n: u32) {
    for _ in 0..n {
        step(app);
    }
}

/// The standard headless AI-vs-AI match shape (the M4c fixture). `extra` spawns
/// anything else the test wants on the field, with the app's content in hand —
/// which is how the spawn-order probe plants barracks itself.
fn match_app_with(
    c: Content,
    commanders: AiCommanders,
    alloy: u32,
    nodes: bool,
    extra: impl Fn(&mut World, Faction, Vec2),
) -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .insert_resource(Time::<Fixed>::from_hz(60.0))
        .insert_resource(c)
        .init_resource::<CommandQueue>()
        .init_resource::<RateReport>()
        .init_resource::<Casualties>()
        .insert_resource(Stockpiles::starting(alloy));
    onus::add_sim_systems(&mut app, Update);
    for (faction, base) in [
        (Faction::A, Vec2::new(-750.0, 0.0)),
        (Faction::B, Vec2::new(750.0, 0.0)),
    ] {
        let def = app
            .world()
            .resource::<Content>()
            .building_index("hq")
            .unwrap();
        app.world_mut().spawn((
            Position(base),
            Building { def },
            faction,
            ProductionQueue::default(),
        ));
        if nodes {
            app.world_mut().spawn((
                Position(base + Vec2::new(0.0, 250.0)),
                ResourceNode { amount: 100_000 },
            ));
        }
        for i in 0..3 {
            let (idx, kind, hp) = {
                let c = app.world().resource::<Content>();
                let idx = c.unit_index("worker").unwrap();
                (idx, c.units[idx].mvp_kind, Health::from_def(c, idx))
            };
            app.world_mut().spawn((
                Position(base + Vec2::new(0.0, 20.0 * i as f32)),
                UnitDefIdx(idx),
                kind,
                faction,
                hp,
            ));
        }
        extra(app.world_mut(), faction, base);
    }
    app.insert_resource(commanders);
    app
}

fn match_app(c: Content, commanders: AiCommanders, alloy: u32, nodes: bool) -> App {
    match_app_with(c, commanders, alloy, nodes, |_, _, _| {})
}

/// A match of `a` vs `b`, by strategy name, on the content handed in.
fn matchup(c: Content, seed: u64, a: &str, b: &str, alloy: u32, nodes: bool) -> App {
    let commanders = AiCommanders::matchup(&c, seed, &[(Faction::A, a), (Faction::B, b)])
        .expect("both strategies are named in this content");
    match_app(c, commanders, alloy, nodes)
}

/// A stable digest of the whole action trace — byte-for-byte the digest
/// `tests/b1_matchup.rs` and `tests/b35_queue_depth.rs` use, because the
/// shipped-default goldens below are theirs.
fn journal_digest(app: &App) -> u64 {
    let mut seen: Vec<u64> = Vec::new();
    let mut label = |e: Entity| -> usize {
        let bits = e.to_bits();
        match seen.iter().position(|b| *b == bits) {
            Some(i) => i,
            None => {
                seen.push(bits);
                seen.len() - 1
            }
        }
    };
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for (t, f, a) in &app.world().resource::<AiJournal>().0 {
        let action = match *a {
            AiAction::Gather { unit, node } => {
                format!("Gather{{unit:{},node:{}}}", label(unit), label(node))
            }
            AiAction::TrainWorker { at } => format!("TrainWorker{{at:{}}}", label(at)),
            other => format!("{other:?}"),
        };
        for b in format!("{t}|{f:?}|{action}").as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100_0000_01b3);
        }
    }
    h
}

/// The per-tick `state_hash` sequence of a match, folded into one number.
fn hash_trace(app: &mut App, samples: u32, every: u32) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for _ in 0..samples {
        tick(app, every);
        h ^= onus::sim::state_hash(app.world_mut());
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Indices of the victory buildings (worker production is not what this file
/// measures).
fn victory_defs(app: &App) -> Vec<usize> {
    let c = app.world().resource::<Content>();
    c.buildings
        .iter()
        .enumerate()
        .filter(|(_, b)| b.victory)
        .map(|(i, _)| i)
        .collect()
}

/// Queue lengths of one side's army barracks, ascending.
fn army_queue_lens(app: &mut App, f: Faction) -> Vec<usize> {
    let victory = victory_defs(app);
    let mut q = app
        .world_mut()
        .query::<(&Building, &Faction, &ProductionQueue)>();
    let mut out: Vec<usize> = q
        .iter(app.world())
        .filter(|(b, who, _)| **who == f && !victory.contains(&b.def))
        .map(|(_, _, p)| p.items.len())
        .collect();
    out.sort_unstable();
    out
}

/// How many army barracks one side owns.
fn army_barracks(app: &mut App, f: Faction) -> usize {
    army_queue_lens(app, f).len()
}

/// Combat units (offense > 0) a side has on the field.
fn combat_units(app: &mut App, f: Faction) -> usize {
    let offensive: Vec<usize> = {
        let c = app.world().resource::<Content>();
        c.units
            .iter()
            .enumerate()
            .filter(|(_, u)| u.offense > 0)
            .map(|(i, _)| i)
            .collect()
    };
    let mut q = app.world_mut().query::<(&UnitDefIdx, &Faction)>();
    q.iter(app.world())
        .filter(|(d, who)| **who == f && offensive.contains(&d.0))
        .count()
}

/// Every placement one side made, tick-tagged.
fn placements(app: &App, f: Faction) -> Vec<(u32, usize)> {
    app.world()
        .resource::<AiJournal>()
        .for_faction(f)
        .into_iter()
        .filter_map(|(t, a)| match a {
            AiAction::PlaceBarracks { building, .. } => Some((t, building)),
            _ => None,
        })
        .collect()
}

/// Army units a side put into production (the journal's own count).
fn ordered_army(app: &App, f: Faction) -> usize {
    app.world()
        .resource::<AiJournal>()
        .for_faction(f)
        .into_iter()
        .filter(|(_, a)| matches!(a, AiAction::TrainArmy { .. }))
        .count()
}

// ---- 1. neutrality: the shipped data is one opening per building ------------

/// The shipped set opens each building once, so allowing repeats must change
/// nothing about a shipped match. These three goldens are `tests/b1_matchup.rs`'s
/// own, unedited — the pre-AC2 state hashes that have survived every milestone.
#[test]
fn the_shipped_default_matchup_is_unmoved_by_parallel_production() {
    for (seed, state_golden, journal_golden) in [
        (4u64, 0xa71f_64ca_d502_03e9u64, 0xe78e_ebdc_5c2c_a733u64),
        (11, 0x5b39_8ee4_7854_23dc, 0x00b7_f8d8_713f_e467),
        (23, 0xf4b5_7d1c_3c3f_2af7, 0x4682_1006_f2fa_e62a),
    ] {
        let c = content();
        let commanders = AiCommanders::new(seed, &[Faction::A, Faction::B]);
        let alloy = c.economy.starting_alloy;
        let mut app = match_app(c, commanders, alloy, true);
        tick(&mut app, 3_000);
        let state = onus::sim::state_hash(app.world_mut());
        assert_eq!(
            state, state_golden,
            "seed {seed}: the default match moved (got {state:#018x}, pre-AC2 {state_golden:#018x})"
        );
        assert_eq!(
            journal_digest(&app),
            journal_golden,
            "seed {seed}: the default commanders took different decisions"
        );
    }
}

/// And the same for *named* single-opening scripts: the whole per-tick
/// `state_hash` sequence (30 samples, every 100 ticks) and the action trace of
/// two fixture matchups, captured from the **pre-change binary** before repeated
/// openings existed. Anything but a byte-identical replay means the capability
/// did not land neutral.
#[test]
fn a_single_opening_match_replays_the_pre_change_hashes_and_journal() {
    for (name, seed, a, b, trace_golden, journal_golden) in [
        (
            "neutral_a",
            4u64,
            "solo_ripper",
            "solo_bulwark",
            0xff87_0184_09ac_e43eu64,
            0x4e16_04bd_46e0_99f6u64,
        ),
        (
            "neutral_b",
            11,
            "solo_bulwark",
            "solo_ripper",
            0xc0a7_7e73_6876_e285,
            0x565d_a2a6_6530_936a,
        ),
    ] {
        let c = neutral_content(name);
        let alloy = c.economy.starting_alloy;
        let mut app = matchup(c, seed, a, b, alloy, true);
        let trace = hash_trace(&mut app, 30, 100);
        let journal = journal_digest(&app);
        println!("b3.5 neutrality {a} vs {b} @ {seed}: trace {trace:#018x} journal {journal:#018x}");
        assert_eq!(
            trace, trace_golden,
            "{a} vs {b} @ seed {seed}: the per-tick state hashes moved \
             (got {trace:#018x}, pre-change {trace_golden:#018x})"
        );
        assert_eq!(
            journal, journal_golden,
            "{a} vs {b} @ seed {seed}: the commanders' decisions moved \
             (got {journal:#018x}, pre-change {journal_golden:#018x})"
        );
    }
}

// ---- 2. N openings place N buildings ----------------------------------------

/// A strategy listing the same barracks three times ends up with three of them,
/// each placed once — and never a fourth, checked on every tick.
#[test]
fn three_openings_of_one_building_place_exactly_three() {
    let c = content_with(
        "three_openings",
        &[strategy(
            "triple_vats",
            30,
            1,
            &[
                ("gene_vats", 300, 130.0),
                ("gene_vats", 600, 160.0),
                ("gene_vats", 900, 190.0),
            ],
            &[("ripper", 1)],
            100_000, // never attacks
            1,
        )],
    );
    let vats = c.building_index("gene_vats").expect("a barracks");
    let mut app = matchup(c, 5, "triple_vats", "triple_vats", 100_000, true);

    for t in 0..3_000u32 {
        step(&mut app);
        let n = army_barracks(&mut app, Faction::A);
        assert!(n <= 3, "tick {t}: {n} barracks for a strategy that opens three");
    }
    assert_eq!(
        army_barracks(&mut app, Faction::A),
        3,
        "the commander did not place all three openings"
    );
    let placed = placements(&app, Faction::A);
    assert_eq!(
        placed.len(),
        3,
        "the journal records {} placements, not three: {placed:?}",
        placed.len()
    );
    assert!(
        placed.iter().all(|(_, b)| *b == vats),
        "something other than the Gene-Vats went up: {placed:?}"
    );
    // Each opening waits for its own `at_tick`.
    let ticks: Vec<u32> = placed.iter().map(|(t, _)| *t).collect();
    assert!(ticks[0] >= 300 && ticks[1] >= 600 && ticks[2] >= 900, "{ticks:?}");
    // ...and they are distinct decisions in ascending order.
    assert!(ticks[0] < ticks[1] && ticks[1] < ticks[2], "{ticks:?}");
}

// ---- 3. the measurement: barracks count is the throughput lever -------------

/// Over a **7 200-tick (2-minute) horizon** with ample Alloy, the same script
/// with three barracks finishes materially more units than with one. The two
/// sides differ only in how many times they open the Gene-Vats; neither
/// attacks, so nothing is lost.
#[test]
fn three_barracks_finish_materially_more_units_than_one() {
    const HORIZON: u32 = 7_200;
    let c = content_with(
        "parallel_horizon",
        &[
            strategy(
                "one_vats",
                30,
                1,
                &[("gene_vats", 300, 130.0)],
                &[("ripper", 1)],
                100_000,
                1,
            ),
            strategy(
                "three_vats",
                30,
                1,
                &[
                    ("gene_vats", 300, 130.0),
                    ("gene_vats", 300, 160.0),
                    ("gene_vats", 300, 190.0),
                ],
                &[("ripper", 1)],
                100_000,
                1,
            ),
        ],
    );
    let mut app = matchup(c, 3, "one_vats", "three_vats", 100_000, true);
    tick(&mut app, HORIZON);

    let one = combat_units(&mut app, Faction::A);
    let three = combat_units(&mut app, Faction::B);
    println!("b3.5 parallel {HORIZON} ticks: 1 barracks -> {one} units, 3 barracks -> {three} units");
    assert_eq!(army_barracks(&mut app, Faction::B), 3, "the three-barracks side is short");
    assert!(
        three >= 2 * one,
        "three barracks finished {three} units against one barracks' {one}: \
         production is not parallel"
    );
    assert!(
        ordered_army(&app, Faction::B) > ordered_army(&app, Faction::A),
        "the three-barracks side did not order more units"
    );
}

/// Barracks count and `queue_depth` are **different mechanisms**: depth buys
/// back the idle gap at one barracks (F-027), count multiplies the lines. Three
/// barracks at depth 1 must therefore not produce what one barracks at depth 3
/// does. Both numbers are printed — the tuning run needs their sizes.
#[test]
fn barracks_count_and_queue_depth_are_different_levers() {
    const HORIZON: u32 = 7_200;
    let c = content_with(
        "count_vs_depth",
        &[
            strategy(
                "wide_shallow",
                30,
                1,
                &[
                    ("gene_vats", 300, 130.0),
                    ("gene_vats", 300, 160.0),
                    ("gene_vats", 300, 190.0),
                ],
                &[("ripper", 1)],
                100_000,
                1,
            ),
            strategy(
                "narrow_deep",
                30,
                1,
                &[("gene_vats", 300, 130.0)],
                &[("ripper", 1)],
                100_000,
                3,
            ),
        ],
    );
    let mut app = matchup(c, 3, "wide_shallow", "narrow_deep", 100_000, true);
    tick(&mut app, HORIZON);

    let wide = combat_units(&mut app, Faction::A);
    let deep = combat_units(&mut app, Faction::B);
    println!(
        "b3.5 lever comparison {HORIZON} ticks: 3 barracks @ depth 1 -> {wide} units, \
         1 barracks @ depth 3 -> {deep} units"
    );
    assert!(
        wide > deep,
        "3 barracks at depth 1 finished {wide} units and 1 barracks at depth 3 \
         finished {deep}: the two levers are indistinguishable"
    );
}

// ---- 4. one order per decision, still ---------------------------------------

/// With three barracks up, the commander still queues **one** army unit per
/// decision — it tops up one line, it does not fill them all at once.
#[test]
fn one_army_order_per_decision_with_several_barracks() {
    let c = content_with(
        "one_per_decision",
        &[strategy(
            "triple_line",
            30,
            1,
            &[
                ("gene_vats", 300, 130.0),
                ("gene_vats", 300, 160.0),
                ("gene_vats", 300, 190.0),
            ],
            &[("ripper", 1)],
            100_000,
            2,
        )],
    );
    let mut app = matchup(c, 7, "triple_line", "triple_line", 100_000, true);
    tick(&mut app, 4_000);
    assert_eq!(army_barracks(&mut app, Faction::A), 3, "three lines expected");

    for f in [Faction::A, Faction::B] {
        let mut per_tick: Vec<u32> = app
            .world()
            .resource::<AiJournal>()
            .for_faction(f)
            .into_iter()
            .filter(|(_, a)| matches!(a, AiAction::TrainArmy { .. }))
            .map(|(t, _)| t)
            .collect();
        let before = per_tick.len();
        per_tick.dedup();
        assert_eq!(
            per_tick.len(),
            before,
            "{f:?} queued two army units in one decision"
        );
        assert!(before > 3, "{f:?} barely trained at all ({before} orders)");
    }
    // ...and the depth cap is still per barracks.
    for len in army_queue_lens(&mut app, Faction::A) {
        assert!(len <= 2, "a line queued {len} past the strategy's depth of 2");
    }
}

/// The target line is the **shallowest** queue, so three barracks fill evenly
/// rather than one line taking everything.
#[test]
fn training_spreads_across_the_barracks_it_owns() {
    let c = content_with(
        "spread",
        &[strategy(
            "spread_vats",
            30,
            1,
            &[
                ("gene_vats", 300, 130.0),
                ("gene_vats", 300, 160.0),
                ("gene_vats", 300, 190.0),
            ],
            &[("ripper", 1)],
            100_000,
            1,
        )],
    );
    let mut app = matchup(c, 11, "spread_vats", "spread_vats", 100_000, true);
    tick(&mut app, 2_000);
    let lens = army_queue_lens(&mut app, Faction::A);
    assert_eq!(lens.len(), 3, "three lines expected, got {lens:?}");
    assert!(
        lens.iter().all(|l| *l == 1),
        "the commander did not spread across its barracks: {lens:?}"
    );
}

// ---- 5. affordability --------------------------------------------------------

/// A commander that can pay for one building places one — the per-decision
/// budget is not committed three times — **and the two placements it could not
/// afford consume no randomness**: the same script with a single opening, the
/// same purse and the same seed leaves the RNG in exactly the same state (and
/// puts the building in the same place).
#[test]
fn a_thin_commander_places_one_building_and_wastes_no_randomness() {
    let scripts = |id: &str, n: usize| {
        let all = [
            ("gene_vats", 300u32, 130.0f32),
            ("gene_vats", 300, 160.0),
            ("gene_vats", 300, 190.0),
        ];
        vec![strategy(id, 30, 1, &all[..n], &[("ripper", 1)], 100_000, 1)]
    };
    let run = |name: &str, id: &str, n: usize, purse: u32| {
        let c = content_with(name, &scripts(id, n));
        let mut app = matchup(c, 2, id, id, purse, false);
        tick(&mut app, 1_000);
        let rng: Vec<u64> = app
            .world()
            .resource::<AiCommanders>()
            .commanders()
            .iter()
            .map(|k| k.rng_state())
            .collect();
        (army_barracks(&mut app, Faction::A), placements(&app, Faction::A), rng)
    };
    // Exactly one Gene-Vats' worth of Alloy, no nodes: nothing more is coming.
    let purse = content().building("gene_vats").expect("a barracks").alloy_cost;
    let (wide_n, wide_placed, wide_rng) = run("thin_three", "thin_three", 3, purse);
    let (solo_n, solo_placed, solo_rng) = run("thin_one", "thin_three", 1, purse);

    assert_eq!(wide_n, 1, "a commander with one building's Alloy placed {wide_n}");
    assert_eq!(wide_placed.len(), 1, "{wide_placed:?}");
    assert_eq!(
        wide_placed, solo_placed,
        "the unaffordable openings changed where the affordable one went"
    );
    assert_eq!(solo_n, 1, "the control run did not place its one building");
    assert_eq!(
        wide_rng, solo_rng,
        "a placement the commander could not afford consumed randomness"
    );
}

// ---- 6. determinism ----------------------------------------------------------

/// Same (pair, seed) replays bit-identically with repeated openings.
#[test]
fn a_repeated_opening_match_replays_bit_identically() {
    let scripts = vec![strategy(
        "det_wide",
        30,
        6,
        &[
            ("gene_vats", 300, 130.0),
            ("gene_vats", 400, 160.0),
            ("gene_vats", 500, 190.0),
        ],
        &[("ripper", 1)],
        4,
        1,
    )];
    let run = |name: &str| {
        let c = content_with(name, &scripts);
        let alloy = c.economy.starting_alloy;
        let mut app = matchup(c, 6, "det_wide", "det_wide", alloy, true);
        (hash_trace(&mut app, 20, 100), journal_digest(&app))
    };
    assert_eq!(run("det_a"), run("det_b"), "a repeated-opening match diverged");
}

/// The choice of *which* barracks to train at never depends on spawn or query
/// order. Three identical barracks are planted by the fixture itself, in one
/// order and then in the reverse order; the per-tick queue-length profile (and
/// the number of units that come out) is identical either way.
#[test]
fn the_target_barracks_never_depends_on_spawn_order() {
    let run = |name: &str, reverse: bool| {
        // A script that opens nothing it does not already own: the barracks are
        // on the field before the first decision.
        let c = content_with(
            name,
            &[strategy(
                "planted",
                30,
                1,
                &[("gene_vats", 100_000, 130.0)],
                &[("ripper", 1)],
                100_000,
                1,
            )],
        );
        let vats = c.building_index("gene_vats").expect("a barracks");
        let commanders =
            AiCommanders::matchup(&c, 8, &[(Faction::A, "planted"), (Faction::B, "planted")])
                .expect("named");
        let mut app = match_app_with(c, commanders, 100_000, true, move |w, f, base| {
            let mut offsets = [
                Vec2::new(0.0, -120.0),
                Vec2::new(120.0, 0.0),
                Vec2::new(0.0, 120.0),
            ];
            if reverse {
                offsets.reverse();
            }
            for o in offsets {
                w.spawn((
                    Position(base + o),
                    Building { def: vats },
                    f,
                    ProductionQueue::default(),
                ));
            }
        });
        let mut profile: Vec<Vec<usize>> = Vec::new();
        for _ in 0..40 {
            tick(&mut app, 50);
            profile.push(army_queue_lens(&mut app, Faction::A));
        }
        (profile, combat_units(&mut app, Faction::A), ordered_army(&app, Faction::A))
    };
    let forward = run("order_fwd", false);
    let backward = run("order_rev", true);
    assert!(forward.1 > 0, "the planted barracks never produced anything");
    assert_eq!(
        forward, backward,
        "reversing the spawn order of three identical barracks changed the match"
    );
}

// ---- 7. validation -----------------------------------------------------------

/// Repeated openings now load — and every other refusal still refuses.
#[test]
fn repeated_openings_load_and_the_other_refusals_stand() {
    let ok = try_content_with(
        "valid_repeat",
        &[strategy(
            "repeat_ok",
            30,
            6,
            &[
                ("gene_vats", 300, 130.0),
                ("gene_vats", 600, 160.0),
                ("gene_vats", 600, 160.0), // same tick and offset: legal too
            ],
            &[("ripper", 1)],
            3,
            1,
        )],
    );
    assert!(
        ok.is_ok(),
        "a strategy opening one building three times was refused: {:?}",
        ok.err()
    );

    let refused: Vec<(&str, String)> = vec![
        (
            "a barracks that is not a building",
            strategy("bad", 30, 6, &[("nonesuch", 300, 130.0)], &[("ripper", 1)], 3, 1),
        ),
        (
            "the victory building as a barracks",
            strategy("bad", 30, 6, &[("hq", 300, 130.0)], &[("worker", 1)], 3, 1),
        ),
        (
            "a unit no opened barracks can produce, repeats and all",
            strategy(
                "bad",
                30,
                6,
                &[("gene_vats", 300, 130.0), ("gene_vats", 600, 160.0)],
                &[("sentinel", 1)],
                3,
                1,
            ),
        ),
        (
            "no barracks at all",
            strategy("bad", 30, 6, &[], &[("ripper", 1)], 3, 1)
                .replace("barracks: [\n\n            ]", "barracks: []"),
        ),
        (
            "a zero offset on the repeat",
            strategy(
                "bad",
                30,
                6,
                &[("gene_vats", 300, 130.0), ("gene_vats", 600, 0.0)],
                &[("ripper", 1)],
                3,
                1,
            ),
        ),
        (
            "queue_depth 0",
            strategy(
                "bad",
                30,
                6,
                &[("gene_vats", 300, 130.0), ("gene_vats", 600, 160.0)],
                &[("ripper", 1)],
                3,
                0,
            ),
        ),
    ];
    for (what, text) in refused {
        let err = try_content_with("refused", &[text])
            .err()
            .unwrap_or_else(|| panic!("{what} loaded"));
        assert!(
            format!("{err}").contains("bad"),
            "{what}: the error does not name the strategy: {err}"
        );
    }
}
