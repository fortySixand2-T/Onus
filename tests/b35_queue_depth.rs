//! L2 integration tests for **B3.5 AC0** — the cap on units-in-production is
//! *content*, not a Rust constant.
//!
//! Before this AC the army step read `if b.queued == 0`: one unit in production
//! per barracks, forever, whatever the script said. That single constant pinned
//! a mass probe's throughput at one unit per `mvp_train_ticks`, which is why
//! scaling train times lengthens a match by *shrinking the army* instead of
//! lengthening the fight (F-026). `StrategyDef::queue_depth` moves that cap
//! into `strategies.ron`.
//!
//! What is encoded here:
//!   - **depth 1 is exactly today's behaviour** — the shipped default matchup
//!     and two fixture matchups replay to the same per-tick `state_hash`
//!     sequence and the same `AiJournal` as the pre-change binary produced;
//!   - a depth-`N` strategy really fills its queue to `N`, **one per decision**,
//!     and never deeper;
//!   - depth is per-strategy: two sides at different depths in one match each
//!     honour their own;
//!   - over a stated horizon a deeper queue finishes **more** units;
//!   - affordability still gates: a commander that can pay for one unit queues
//!     one, whatever its depth;
//!   - determinism: same (strategy pair, seed, depth) replays bit-identically.
//!
//! Everything runs headless (`MinimalPlugins`) through the *shipped* sim chain
//! (`onus::add_sim_systems`).

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

/// One strategy entry, spelled out — including its `queue_depth`, which is the
/// whole point of this file.
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

/// The shipped set (default `mvp` intact) plus the fixture strategies a test
/// needs, loaded as a content dir of its own.
fn content_with(name: &str, extra: &[String]) -> Content {
    let shipped = std::fs::read_to_string(data_dir().join("strategies.ron")).expect("strategies");
    let text = shipped.replace("    ],\n)", &format!("{}\n    ],\n)", extra.join("\n")));
    assert!(text != shipped, "fixture splice found no strategies list");
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/b35_queue_depth")
        .join(name);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    for file in ["units.ron", "resources.ron"] {
        std::fs::copy(data_dir().join(file), dir.join(file)).expect("copy");
    }
    std::fs::write(dir.join("strategies.ron"), text).expect("write strategies.ron");
    Content::load_from_dir(&dir).expect("fixture content loads")
}

/// The two depth-1 probe scripts the neutrality goldens were captured with.
/// Their text is fixed: a golden is only a floor if the fixture cannot drift.
fn neutral_content(name: &str) -> Content {
    content_with(
        name,
        &[
            strategy(
                "depth_ripper",
                30,
                6,
                &[("gene_vats", 300, 130.0)],
                &[("ripper", 1)],
                3,
                1,
            ),
            strategy(
                "depth_bulwark",
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

/// The standard headless AI-vs-AI match shape (the M4c fixture), with the
/// commanders, the starting Alloy and whether there is anything to mine handed
/// in — so a throughput test can be rich and an affordability test poor.
fn match_app(c: Content, commanders: AiCommanders, alloy: u32, nodes: bool) -> App {
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
    }
    app.insert_resource(commanders);
    app
}

/// A match of `a` vs `b`, by strategy name, on the content handed in.
fn matchup(c: Content, seed: u64, a: &str, b: &str, alloy: u32, nodes: bool) -> App {
    let commanders = AiCommanders::matchup(&c, seed, &[(Faction::A, a), (Faction::B, b)])
        .expect("both strategies are named in this content");
    match_app(c, commanders, alloy, nodes)
}

/// A stable digest of the whole action trace. **Byte-for-byte the digest
/// `tests/b1_matchup.rs` uses** — same relabelling of entities by first
/// appearance, same rendering, same FNV — because the shipped-default goldens
/// below are that file's, and a golden is only a floor if it is computed the
/// same way.
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
            // The rest name content indices and positions, never entities.
            other => format!("{other:?}"),
        };
        for b in format!("{t}|{f:?}|{action}").as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100_0000_01b3);
        }
    }
    h
}

/// The per-tick `state_hash` sequence of a match, folded into one number. The
/// fold is over **every** sample in order, so a single tick that differs moves
/// it — it is a compression of the sequence, not a summary of the end state.
fn hash_trace(app: &mut App, samples: u32, every: u32) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for _ in 0..samples {
        tick(app, every);
        h ^= onus::sim::state_hash(app.world_mut());
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// The army barracks' queue lengths of one side (the victory building excluded:
/// worker production is not what `queue_depth` caps), ascending.
fn army_queue_lens(app: &mut App, f: Faction) -> Vec<usize> {
    let victory: Vec<usize> = {
        let c = app.world().resource::<Content>();
        c.buildings
            .iter()
            .enumerate()
            .filter(|(_, b)| b.victory)
            .map(|(i, _)| i)
            .collect()
    };
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

/// Army units a side put into production (the journal's own count).
fn ordered_army(app: &App, f: Faction) -> usize {
    app.world()
        .resource::<AiJournal>()
        .for_faction(f)
        .into_iter()
        .filter(|(_, a)| matches!(a, AiAction::TrainArmy { .. }))
        .count()
}

// ---- 1. depth 1 is exactly today's behaviour --------------------------------

/// The neutrality proof. `queue_depth: 1` is the condition the Rust constant
/// expressed (`queued < 1` *is* `queued == 0`), so shipping at depth 1 must
/// reproduce the pre-change match bit for bit.
///
/// The three shipped-content goldens are **`tests/b1_matchup.rs`'s own**,
/// unedited — the pre-AC2 state hashes that have survived every milestone
/// since. If adding a field to `strategies.ron` had changed a single tick of
/// the default match, these would move.
#[test]
fn the_shipped_default_matchup_is_unmoved_by_the_new_field() {
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

/// And the same for *named* depth-1 scripts: the whole per-tick `state_hash`
/// sequence (30 samples, every 100 ticks) and the action trace of two fixture
/// matchups, captured from the **pre-change binary** before `queue_depth`
/// existed. Anything but a byte-identical replay means depth 1 is not the old
/// constant.
#[test]
fn a_depth_one_match_replays_the_pre_change_hashes_and_journal() {
    for (name, seed, a, b, trace_golden, journal_golden) in [
        (
            "neutral_a",
            4u64,
            "depth_ripper",
            "depth_bulwark",
            0xff87_0184_09ac_e43eu64,
            0x4e16_04bd_46e0_99f6u64,
        ),
        (
            "neutral_b",
            11,
            "depth_bulwark",
            "depth_ripper",
            0xc0a7_7e73_6876_e285,
            0x565d_a2a6_6530_936a,
        ),
    ] {
        let c = neutral_content(name);
        let alloy = c.economy.starting_alloy;
        let mut app = matchup(c, seed, a, b, alloy, true);
        let trace = hash_trace(&mut app, 30, 100);
        assert_eq!(
            trace, trace_golden,
            "{a} vs {b} @ seed {seed}: the per-tick state hashes moved \
             (got {trace:#018x}, pre-change {trace_golden:#018x})"
        );
        let journal = journal_digest(&app);
        assert_eq!(
            journal, journal_golden,
            "{a} vs {b} @ seed {seed}: the commanders' decisions moved \
             (got {journal:#018x}, pre-change {journal_golden:#018x})"
        );
    }
}

// ---- 2. depth N really queues N, one per decision ---------------------------

/// A depth-3 script with Alloy to spare fills its barracks to three and stops
/// there — checked on **every** tick of a 3 000-tick run, not once.
///
/// It also fills **one per decision**: the tick the queue first reaches 2 is at
/// least one `think_interval_ticks` after it first reached 1, and 3 after 2. A
/// commander that queued three in one `think()` would fill them on the same
/// tick.
#[test]
fn a_depth_three_strategy_fills_to_three_one_per_decision_and_no_further() {
    let think = 30;
    let c = content_with(
        "depth3",
        &[strategy(
            "deep_ripper",
            think,
            1,
            &[("gene_vats", 300, 130.0)],
            &[("ripper", 1)],
            100_000, // never attacks: this test measures production, not war
            3,
        )],
    );
    let mut app = matchup(c, 5, "deep_ripper", "deep_ripper", 100_000, true);

    let mut first_reached = [None; 4]; // first tick the queue held 1 / 2 / 3
    let mut deepest = 0usize;
    for t in 0..3_000u32 {
        step(&mut app);
        for len in army_queue_lens(&mut app, Faction::A) {
            assert!(
                len <= 3,
                "tick {t}: the queue reached {len}, past the strategy's depth of 3"
            );
            deepest = deepest.max(len);
            if (1..=3).contains(&len) && first_reached[len].is_none() {
                first_reached[len] = Some(t);
            }
        }
    }
    assert_eq!(deepest, 3, "the queue never reached the depth its script asks for");
    let at = |n: usize| first_reached[n].unwrap_or_else(|| panic!("the queue never held {n}"));
    assert!(
        at(2) >= at(1) + think,
        "the queue went from 1 to 2 inside one decision ({} -> {})",
        at(1),
        at(2)
    );
    assert!(
        at(3) >= at(2) + think,
        "the queue went from 2 to 3 inside one decision ({} -> {})",
        at(2),
        at(3)
    );
}

/// Depth is **per strategy**: one match, two depths, each side its own. Read
/// off the commander's own script — not the content's default, which is 1.
#[test]
fn each_side_honours_its_own_queue_depth() {
    let c = content_with(
        "per_side",
        &[
            strategy(
                "shallow",
                30,
                1,
                &[("gene_vats", 300, 130.0)],
                &[("ripper", 1)],
                100_000,
                1,
            ),
            strategy(
                "deep",
                30,
                1,
                &[("gene_vats", 300, 130.0)],
                &[("ripper", 1)],
                100_000,
                4,
            ),
        ],
    );
    assert_eq!(c.ai.queue_depth, 1, "the default's depth is the shipped one");
    let mut app = matchup(c, 9, "shallow", "deep", 100_000, true);

    let mut deepest = [0usize; 2];
    for t in 0..3_000u32 {
        step(&mut app);
        for (i, f) in [Faction::A, Faction::B].into_iter().enumerate() {
            for len in army_queue_lens(&mut app, f) {
                let cap = if i == 0 { 1 } else { 4 };
                assert!(
                    len <= cap,
                    "tick {t}: {f:?} queued {len} against its own depth of {cap}"
                );
                deepest[i] = deepest[i].max(len);
            }
        }
    }
    assert_eq!(deepest, [1, 4], "each side did not reach its own depth");
}

// ---- 3. what the tuning run needs: depth is a throughput lever --------------

/// Over a **7 200-tick (2-minute) horizon**, the deeper queue finishes more
/// units. The two sides run the same script — same barracks, same unit, same
/// cadence, mirrored geography, neither attacking — and differ only in depth.
///
/// The mechanism is the idle gap, not parallelism: `economy::production` ticks
/// only the head of a queue, so a barracks still builds one unit at a time. At
/// depth 1 it stands idle from the moment a unit pops until its commander's
/// next decision; at depth 3 the next item is already there. The script here
/// thinks every 300 ticks against a 720-tick Ripper, which makes that gap large
/// enough to read cleanly.
#[test]
fn a_deeper_queue_finishes_more_units_over_a_fixed_horizon() {
    const HORIZON: u32 = 7_200;
    let c = content_with(
        "horizon",
        &[
            strategy(
                "slow_shallow",
                300,
                1,
                &[("gene_vats", 300, 130.0)],
                &[("ripper", 1)],
                100_000,
                1,
            ),
            strategy(
                "slow_deep",
                300,
                1,
                &[("gene_vats", 300, 130.0)],
                &[("ripper", 1)],
                100_000,
                3,
            ),
        ],
    );
    let mut app = matchup(c, 3, "slow_shallow", "slow_deep", 100_000, true);
    tick(&mut app, HORIZON);

    let shallow = combat_units(&mut app, Faction::A);
    let deep = combat_units(&mut app, Faction::B);
    // Printed, not only asserted: the tuning run wants the size of the effect,
    // not just its sign (`cargo test -- --nocapture`).
    println!("b3.5 horizon {HORIZON} ticks: depth 1 -> {shallow} units, depth 3 -> {deep} units");
    assert!(
        deep > shallow,
        "over {HORIZON} ticks the depth-3 side finished {deep} units and the \
         depth-1 side {shallow}: depth is not a throughput lever"
    );
    // ...and it is the queue doing it, not a different build order.
    assert!(
        ordered_army(&app, Faction::B) > ordered_army(&app, Faction::A),
        "the deeper side did not order more units"
    );
}

// ---- 4. affordability still gates -------------------------------------------

/// Depth raises the cap; it does not create Alloy. A depth-3 commander with
/// exactly one Ripper's worth of Alloy left after its barracks queues **one**,
/// and the sim never charges for more than it queued.
///
/// The match is poor by construction: no resource nodes, `worker_target: 1`
/// (three workers are already on the field, so it never trains one), and a
/// stockpile of Gene-Vats + one Ripper.
#[test]
fn affordability_gates_the_queue_whatever_the_depth() {
    let c = content_with(
        "poor",
        &[strategy(
            "broke_deep",
            30,
            1,
            &[("gene_vats", 300, 130.0)],
            &[("ripper", 1)],
            100_000,
            3,
        )],
    );
    let purse = c.building("gene_vats").expect("a barracks").alloy_cost
        + c.unit("ripper").expect("a unit").mvp_alloy_cost;
    let mut app = matchup(c, 2, "broke_deep", "broke_deep", purse, false);

    for t in 0..2_000u32 {
        step(&mut app);
        for len in army_queue_lens(&mut app, Faction::A) {
            assert!(len <= 1, "tick {t}: queued {len} on one Ripper's worth of Alloy");
        }
    }
    assert_eq!(
        ordered_army(&app, Faction::A),
        1,
        "a commander that could pay for one unit ordered a different number"
    );
    assert_eq!(
        app.world().resource::<Stockpiles>().alloy(Faction::A),
        0,
        "the purse does not match what was queued"
    );

    // ...and the cap was the *purse*, not the depth: pay the commander and the
    // same script fills to three. (Without this the first half would pass on a
    // build that has no `queue_depth` at all.)
    let paid = 10 * c_ripper_cost(&app);
    let _ = app.world_mut().resource_mut::<Stockpiles>().add(Faction::A, paid);
    let mut deepest = 0usize;
    for t in 0..2_000u32 {
        step(&mut app);
        for len in army_queue_lens(&mut app, Faction::A) {
            assert!(len <= 3, "tick {t}: queued {len} past the strategy's depth of 3");
            deepest = deepest.max(len);
        }
    }
    assert_eq!(deepest, 3, "with Alloy in hand the depth-3 script stayed shallow");
}

/// One Ripper's Alloy cost, read off the content the match is running.
fn c_ripper_cost(app: &App) -> u32 {
    app.world()
        .resource::<Content>()
        .unit("ripper")
        .expect("a unit")
        .mvp_alloy_cost
}

// ---- 5. determinism ----------------------------------------------------------

/// Same (strategy pair, seed, depth) ⇒ same match, and a different depth ⇒ a
/// different match. The field is content, so it belongs to the identity of the
/// match exactly as every other content value does.
#[test]
fn the_same_pair_seed_and_depth_replays_bit_identically() {
    let scripts = |depth: u32| {
        vec![strategy(
            "det_deep",
            30,
            6,
            &[("gene_vats", 300, 130.0)],
            &[("ripper", 1)],
            3,
            depth,
        )]
    };
    let run = |name: &str, depth: u32| {
        let c = content_with(name, &scripts(depth));
        let alloy = c.economy.starting_alloy;
        let mut app = matchup(c, 6, "det_deep", "det_deep", alloy, true);
        (hash_trace(&mut app, 20, 100), journal_digest(&app))
    };
    let first = run("det_a", 3);
    let second = run("det_b", 3);
    assert_eq!(first.0, second.0, "the per-tick state hashes diverged");
    assert_eq!(first.1, second.1, "the commanders took different decisions");

    let shallow = run("det_c", 1);
    assert_ne!(
        first.0, shallow.0,
        "changing the queue depth changed nothing about the match"
    );
}

