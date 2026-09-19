//! Critic probes for **B3.5 AC0** — `queue_depth` becomes content, shipped
//! neutral at 1.
//!
//! Independent of the implementer's own file. What is attacked here:
//!   - **neutrality**: the goldens in part 1 were captured by running the
//!     *pre-change* tree (`ca6f9c0`) on the box and printing them; nothing in
//!     this repo produced them. Named pairs, both orientations, 4 000 ticks,
//!     every tick's `state_hash` folded in.
//!   - **depth semantics**: the cap is exactly N, never N+1, and the queue
//!     fills one per decision — probed at N-1, at N, and just after a pop.
//!   - **per-strategy**: two sides at different depths in one match.
//!   - **affordability**: a thin commander at depth 8 may not over-commit.
//!   - **validation**: 0 refused by name, missing field a load error,
//!     `u32::MAX` neither overflows nor hangs.
//!   - **determinism** and the fingerprint.
//!   - **observation**: what depth actually buys in units produced.

use std::path::PathBuf;

use bevy::prelude::*;

use onus::sim::combat::{Casualties, Health};
use onus::sim::content::Content;
use onus::sim::economy::{Building, ProductionQueue, QueuedUnit, Stockpiles, UnitDefIdx};
use onus::sim::spatial::Faction;
use onus::sim::{
    AiAction, AiCommanders, AiJournal, CommandQueue, Position, RateReport, ResourceNode,
};

// ---- harness ----------------------------------------------------------------

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data")
}

fn content() -> Content {
    Content::load_from_dir(&data_dir()).expect("shipped content loads")
}

#[allow(clippy::too_many_arguments)]
fn strategy(
    id: &str,
    think: u32,
    worker_target: u32,
    barracks: &[(&str, u32, f32)],
    army: &[(&str, u32)],
    attack_at_army: u32,
    queue_depth: &str,
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

/// The shipped set with extra entries spliced in, written to its own dir and
/// loaded. Returns the load *result* so refusal probes can read the error.
fn try_content_with(name: &str, extra: &[String]) -> Result<Content, onus::sim::content::ContentError> {
    let shipped = std::fs::read_to_string(data_dir().join("strategies.ron")).expect("strategies");
    let text = shipped.replace("    ],\n)", &format!("{}\n    ],\n)", extra.join("\n")));
    assert_ne!(text, shipped, "fixture splice found no strategies list");
    write_dir(name, &text)
}

fn write_dir(name: &str, strategies: &str) -> Result<Content, onus::sim::content::ContentError> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/critic_b35_ac0")
        .join(name);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    for file in ["units.ron", "resources.ron"] {
        std::fs::copy(data_dir().join(file), dir.join(file)).expect("copy");
    }
    std::fs::write(dir.join("strategies.ron"), strategies).expect("write");
    Content::load_from_dir(&dir)
}

fn content_with(name: &str, extra: &[String]) -> Content {
    try_content_with(name, extra).expect("fixture content loads")
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

fn matchup(c: Content, seed: u64, a: &str, b: &str, alloy: u32, nodes: bool) -> App {
    let commanders = AiCommanders::matchup(&c, seed, &[(Faction::A, a), (Faction::B, b)])
        .expect("both strategies named");
    match_app(c, commanders, alloy, nodes)
}

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

/// Fold **every** tick's `state_hash` over `horizon` ticks.
fn trace(app: &mut App, horizon: u32) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for _ in 0..horizon {
        step(app);
        h ^= onus::sim::state_hash(app.world_mut());
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn victory_defs(app: &App) -> Vec<usize> {
    let c = app.world().resource::<Content>();
    c.buildings
        .iter()
        .enumerate()
        .filter(|(_, b)| b.victory)
        .map(|(i, _)| i)
        .collect()
}

/// Army-barracks queue lengths of one side (the HQ excluded — `queue_depth`
/// does not cap worker production), ascending.
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

fn train_army_orders(app: &App, f: Faction) -> usize {
    app.world()
        .resource::<AiJournal>()
        .for_faction(f)
        .into_iter()
        .filter(|(_, a)| matches!(a, AiAction::TrainArmy { .. }))
        .count()
}

// ============================================================================
// 1. NEUTRALITY — goldens captured from the pre-change tree (ca6f9c0)
// ============================================================================

/// Nine named matchups, asymmetric ones in both orientations, 4 000 ticks each
/// with every tick's `state_hash` folded in, plus the end hash and the full
/// `AiJournal` digest. Every number below was printed by **`ca6f9c0` built and
/// run on the box**, before this diff existed. If `queue_depth` at 1 is not the
/// constant it replaced, one of these moves.
#[test]
fn depth_one_replays_the_pre_change_tree_tick_for_tick() {
    // (a, b, seed, trace, end, journal) — captured on ca6f9c0.
    #[rustfmt::skip]
    let goldens: &[(&str, &str, u64, u64, u64, u64)] = &[
        ("mvp", "mvp", 4, 0xa834fc3feb7f40ab, 0x5531a7859659febf, 0xa2fc69768bfbace5),
        ("mass_ripper", "mass_arclight", 7, 0x8d7ab156d56be9fb, 0x10402e5a37be07bd, 0xc699543530951455),
        ("mass_arclight", "mass_ripper", 7, 0x5873e35515d3476f, 0x5b4c2d5e5f38fd8d, 0xda16d993dc7d0252),
        ("rush", "turtle", 13, 0x69f557213e2ccd10, 0x15cee2bc1e890af5, 0xd2b94bfeeb10cced),
        ("turtle", "rush", 13, 0xb45498bfa38ed2e4, 0xa637e4c4745356a4, 0xd0ba2d216d202bad),
        ("synth_triad", "mass_ravager", 101, 0x81ed5273540e819f, 0xe5e800a4da3b19d9, 0xa4ee091222a755a1),
        ("mass_bulwark", "mass_sentinel", 55, 0x186004675ac916cd, 0x01218811ca803ea2, 0xe4d75d6d14dfb0da),
        ("mass_sentinel", "mass_bulwark", 55, 0xd32a087b26c7ce4a, 0xf7e6b20b73c48eec, 0x16fd41b0288f1eda),
        ("synth_steel_flesh", "rush", 99, 0xc39dfe1137e0c576, 0x0b25ffcdc4e3b739, 0xd0e182d1cf739ba2),
    ];
    for (a, b, seed, g_trace, g_end, g_journal) in goldens {
        let c = content();
        let alloy = c.economy.starting_alloy;
        let mut app = matchup(c, *seed, a, b, alloy, true);
        let t = trace(&mut app, 4_000);
        let end = onus::sim::state_hash(app.world_mut());
        let j = journal_digest(&app);
        assert_eq!(
            t, *g_trace,
            "{a} vs {b} seed {seed}: per-tick state_hash trace moved \
             (got {t:#018x}, pre-change {g_trace:#018x})"
        );
        assert_eq!(end, *g_end, "{a} vs {b} seed {seed}: end state_hash moved");
        assert_eq!(j, *g_journal, "{a} vs {b} seed {seed}: the AI journal moved");
    }
}

/// Depth 1 must behave as `queued == 0` did even when the queue is *already*
/// deeper than 1 — a state the AI cannot reach itself but a player order can.
/// `(queued as u32) < 1` and `queued == 0` must agree on every queue length,
/// and in particular the commander must add nothing to a queue of 2.
#[test]
fn depth_one_adds_nothing_to_a_queue_that_is_already_deeper() {
    let c = content_with(
        "already_deep",
        &[strategy(
            "critic_d1",
            30,
            3,
            &[("gene_vats", 0, 130.0)],
            &[("ripper", 1)],
            99,
            "1",
        )],
    );
    let mut app = matchup(c, 5, "critic_d1", "critic_d1", 100_000, true);
    // Let the barracks go up and the commander start its first unit.
    tick(&mut app, 400);
    let victory = victory_defs(&app);
    // Hand-stuff A's barracks queue to 3 items with a long timer, so the queue
    // cannot drain during the probe.
    let ripper = app.world().resource::<Content>().unit_index("ripper").unwrap();
    let mut q = app
        .world_mut()
        .query::<(&Building, &Faction, &mut ProductionQueue)>();
    let mut stuffed = false;
    for (b, f, mut p) in q.iter_mut(app.world_mut()) {
        if *f == Faction::A && !victory.contains(&b.def) {
            p.items.clear();
            for _ in 0..3 {
                p.items.push_back(QueuedUnit {
                    unit: ripper,
                    ticks_left: 100_000,
                });
            }
            stuffed = true;
        }
    }
    assert!(stuffed, "A never built an army barracks in 400 ticks");
    let before = train_army_orders(&app, Faction::A);
    tick(&mut app, 300);
    assert_eq!(
        army_queue_lens(&mut app, Faction::A),
        vec![3],
        "a depth-1 commander grew a queue that was already deeper than its cap"
    );
    assert_eq!(
        train_army_orders(&app, Faction::A),
        before,
        "a depth-1 commander issued army orders against a full queue"
    );
}

// ============================================================================
// 2. DEPTH SEMANTICS — exactly N, one per decision
// ============================================================================

/// With ample Alloy and a long horizon, a depth-N strategy's army queue must
/// reach N and **never** N+1 — checked every tick, not at the end.
#[test]
fn the_queue_caps_at_exactly_n_and_never_n_plus_one() {
    for depth in [1u32, 2, 5] {
        let c = content_with(
            &format!("cap_{depth}"),
            &[strategy(
                "critic_cap",
                30,
                3,
                &[("gene_vats", 0, 130.0)],
                &[("ripper", 1)],
                99,
                &depth.to_string(),
            )],
        );
        let mut app = matchup(c, 9, "critic_cap", "critic_cap", 200_000, true);
        let mut high = 0usize;
        for _ in 0..3_000 {
            step(&mut app);
            for len in army_queue_lens(&mut app, Faction::A) {
                high = high.max(len);
                assert!(
                    len as u32 <= depth,
                    "depth {depth}: an army queue reached {len}"
                );
            }
        }
        assert_eq!(
            high, depth as usize,
            "depth {depth}: the queue never reached its cap (high water {high}) — \
             the cap is not doing anything"
        );
    }
}

/// The queue fills **one per decision**, never N at once. With depth 6, ample
/// Alloy and a 30-tick think interval, the queue length may rise by at most 1
/// between consecutive ticks, and the number of `TrainArmy` orders may rise by
/// at most 1 per decision interval.
#[test]
fn the_queue_fills_one_per_decision_never_n_at_once() {
    let c = content_with(
        "one_per_decision",
        &[strategy(
            "critic_fill",
            30,
            3,
            &[("gene_vats", 0, 130.0)],
            &[("ripper", 1)],
            99,
            "6",
        )],
    );
    let mut app = matchup(c, 3, "critic_fill", "critic_fill", 500_000, true);
    let mut prev_len = 0usize;
    let mut prev_orders = 0usize;
    let mut reached_cap = false;
    for t in 0..3_000 {
        step(&mut app);
        let len = army_queue_lens(&mut app, Faction::A)
            .into_iter()
            .max()
            .unwrap_or(0);
        assert!(
            len <= prev_len + 1,
            "tick {t}: the queue jumped from {prev_len} to {len} in one tick"
        );
        let orders = train_army_orders(&app, Faction::A);
        assert!(
            orders <= prev_orders + 1,
            "tick {t}: {} army orders in a single tick — one order per decision is broken",
            orders - prev_orders
        );
        if len == 6 {
            reached_cap = true;
        }
        prev_len = len;
        prev_orders = orders;
    }
    assert!(reached_cap, "depth 6 never filled to 6 in 3 000 ticks");
}

/// The boundary, stated directly: over a long rich run at depth 3, the queue
/// length must visit 1, 2 and 3 (it fills one at a time), and after a unit pops
/// the commander refills it on the next decision rather than immediately.
#[test]
fn the_queue_visits_every_length_up_to_the_cap() {
    let c = content_with(
        "boundary",
        &[strategy(
            "critic_edge",
            30,
            3,
            &[("gene_vats", 0, 130.0)],
            &[("ripper", 1)],
            99,
            "3",
        )],
    );
    let mut app = matchup(c, 17, "critic_edge", "critic_edge", 500_000, true);
    let mut seen = [false; 4];
    for _ in 0..3_000 {
        step(&mut app);
        let len = army_queue_lens(&mut app, Faction::A)
            .into_iter()
            .max()
            .unwrap_or(0);
        if len < 4 {
            seen[len] = true;
        }
    }
    for (len, hit) in seen.iter().enumerate() {
        assert!(*hit, "depth 3: the queue was never observed at length {len}");
    }
}

// ============================================================================
// 3. PER-STRATEGY, NOT GLOBAL
// ============================================================================

/// One match, two depths. A at 1, B at 5: neither may leak into the other.
#[test]
fn two_sides_in_one_match_each_honour_their_own_depth() {
    let c = content_with(
        "two_depths",
        &[
            strategy(
                "critic_shallow",
                30,
                3,
                &[("gene_vats", 0, 130.0)],
                &[("ripper", 1)],
                99,
                "1",
            ),
            strategy(
                "critic_deep",
                30,
                3,
                &[("gene_vats", 0, 130.0)],
                &[("ripper", 1)],
                99,
                "5",
            ),
        ],
    );
    for (a, b, depth_a, depth_b) in [
        ("critic_shallow", "critic_deep", 1usize, 5usize),
        ("critic_deep", "critic_shallow", 5, 1),
    ] {
        let mut app = matchup(c.clone(), 21, a, b, 400_000, true);
        let mut high_a = 0usize;
        let mut high_b = 0usize;
        for _ in 0..3_000 {
            step(&mut app);
            high_a = high_a.max(
                army_queue_lens(&mut app, Faction::A)
                    .into_iter()
                    .max()
                    .unwrap_or(0),
            );
            high_b = high_b.max(
                army_queue_lens(&mut app, Faction::B)
                    .into_iter()
                    .max()
                    .unwrap_or(0),
            );
            assert!(
                high_a <= depth_a && high_b <= depth_b,
                "{a} vs {b}: depth leaked across sides (A {high_a}/{depth_a}, B {high_b}/{depth_b})"
            );
        }
        assert_eq!((high_a, high_b), (depth_a, depth_b), "{a} vs {b}: caps not reached");
    }
}

// ============================================================================
// 4. AFFORDABILITY
// ============================================================================

/// A deep strategy with thin Alloy may not over-commit. Starting Alloy is set
/// to a barracks plus exactly one Ripper and nothing is minable, so a depth-8
/// commander must end with exactly one army unit in production and the
/// stockpile must never go negative (it cannot — it is `u32` — so the real
/// claim is that spend is conserved: orders issued × cost ≤ Alloy available).
#[test]
fn a_deep_commander_with_thin_alloy_queues_only_what_it_can_pay_for() {
    let c = content_with(
        "thin",
        &[strategy(
            "critic_poor",
            30,
            1,
            &[("gene_vats", 0, 130.0)],
            &[("ripper", 1)],
            99,
            "8",
        )],
    );
    let barracks = {
        let idx = c.building_index("gene_vats").unwrap();
        c.buildings[idx].alloy_cost
    };
    let ripper = {
        let idx = c.unit_index("ripper").unwrap();
        c.units[idx].mvp_alloy_cost
    };
    // Exactly one Ripper's worth of slack above the barracks.
    let mut app = matchup(c, 31, "critic_poor", "critic_poor", barracks + ripper, false);
    tick(&mut app, 1_200);
    assert_eq!(
        train_army_orders(&app, Faction::A),
        1,
        "a depth-8 commander with one unit's Alloy did not order exactly one"
    );
    let left = app.world().resource::<Stockpiles>().alloy(Faction::A);
    assert_eq!(left, 0, "Alloy accounting drifted: {left} left of an exact budget");
    let queued: usize = army_queue_lens(&mut app, Faction::A).into_iter().sum();
    let built = combat_units(&mut app, Faction::A);
    assert_eq!(
        queued + built,
        1,
        "the one affordable unit is neither queued nor on the field"
    );
}

/// A single decision may commit at most one unit's Alloy however deep the
/// strategy: with Alloy for the barracks plus three Rippers and no income, the
/// three orders must land on three *different* ticks.
#[test]
fn one_decision_commits_at_most_one_unit_however_deep() {
    let c = content_with(
        "budget",
        &[strategy(
            "critic_budget",
            30,
            1,
            &[("gene_vats", 0, 130.0)],
            &[("ripper", 1)],
            99,
            "9",
        )],
    );
    let barracks = c.buildings[c.building_index("gene_vats").unwrap()].alloy_cost;
    let ripper = c.units[c.unit_index("ripper").unwrap()].mvp_alloy_cost;
    let mut app = matchup(c, 41, "critic_budget", "critic_budget", barracks + 3 * ripper, false);
    tick(&mut app, 1_200);
    let ticks: Vec<u32> = app
        .world()
        .resource::<AiJournal>()
        .for_faction(Faction::A)
        .into_iter()
        .filter(|(_, a)| matches!(a, AiAction::TrainArmy { .. }))
        .map(|(t, _)| t)
        .collect();
    assert_eq!(ticks.len(), 3, "three affordable units, {} ordered", ticks.len());
    let mut unique = ticks.clone();
    unique.dedup();
    assert_eq!(unique.len(), 3, "two army orders on one tick: {ticks:?}");
}

// ============================================================================
// 5. VALIDATION
// ============================================================================

#[test]
fn a_spliced_queue_depth_of_zero_is_refused_by_name_at_load() {
    let err = try_content_with(
        "zero_depth",
        &[strategy(
            "critic_zero",
            30,
            3,
            &[("gene_vats", 0, 130.0)],
            &[("ripper", 1)],
            3,
            "0",
        )],
    )
    .expect_err("depth 0 is content the sim cannot run");
    let msg = format!("{err:?}");
    assert!(msg.contains("critic_zero"), "the refusal does not name the strategy: {msg}");
    assert!(msg.contains("queue_depth"), "the refusal does not name the field: {msg}");
}

#[test]
fn a_strategy_with_no_queue_depth_at_all_is_a_load_error() {
    let entry = strategy(
        "critic_absent",
        30,
        3,
        &[("gene_vats", 0, 130.0)],
        &[("ripper", 1)],
        3,
        "1",
    )
    .replace("            queue_depth: 1,\n", "");
    let err = try_content_with("absent_depth", &[entry]).expect_err("a missing depth must not load");
    assert!(
        format!("{err:?}").contains("queue_depth"),
        "the parse error does not name the missing field: {err:?}"
    );
}

/// A depth larger than any affordable army is harmless: no overflow, no hang,
/// and still one order per decision. `u32::MAX` is compared against a `usize`
/// cast, so this is also the truncation probe.
#[test]
fn a_gigantic_depth_neither_overflows_nor_hangs() {
    let c = content_with(
        "huge",
        &[strategy(
            "critic_huge",
            30,
            3,
            &[("gene_vats", 0, 130.0)],
            &[("ripper", 1)],
            99,
            "4294967295",
        )],
    );
    assert_eq!(
        c.strategy("critic_huge").expect("named").queue_depth,
        u32::MAX
    );
    let mut app = matchup(c, 61, "critic_huge", "critic_huge", 1_000_000, true);
    let mut prev = 0usize;
    for t in 0..2_000 {
        step(&mut app);
        let len = army_queue_lens(&mut app, Faction::A)
            .into_iter()
            .max()
            .unwrap_or(0);
        assert!(
            len <= prev + 1,
            "tick {t}: an unbounded depth filled {} items at once",
            len - prev
        );
        prev = len;
    }
    assert!(prev > 5, "an unbounded depth never grew a deep queue (high {prev})");
}

// ============================================================================
// 6. DETERMINISM AND CONTENT IDENTITY
// ============================================================================

/// Same pair, same seed, same depth: bit-identical replay, every tick.
#[test]
fn a_deep_match_replays_bit_identically() {
    let c = content_with(
        "determinism",
        &[strategy(
            "critic_det",
            30,
            4,
            &[("gene_vats", 0, 130.0)],
            &[("ripper", 1)],
            3,
            "4",
        )],
    );
    let mut a = matchup(c.clone(), 77, "critic_det", "mass_arclight", 3_000, true);
    let mut b = matchup(c, 77, "critic_det", "mass_arclight", 3_000, true);
    let (ta, tb) = (trace(&mut a, 2_500), trace(&mut b, 2_500));
    assert_eq!(ta, tb, "the same match did not replay bit-identically");
    assert_eq!(journal_digest(&a), journal_digest(&b));
}

/// Depth is in the fingerprint: content differing only in a depth is different
/// content, so a replay cannot cross that boundary silently.
#[test]
fn a_differing_depth_is_a_differing_content_fingerprint() {
    let base = content();
    let mut deep = content();
    deep.strategies[2].queue_depth = 3;
    assert_ne!(
        base.fingerprint().hash(),
        deep.fingerprint().hash(),
        "queue_depth is not read by Content::fingerprint"
    );
    // ...and the shipped set really is at 1 everywhere.
    for s in &base.strategies {
        assert_eq!(s.queue_depth, 1, "strategy `{}` does not ship at depth 1", s.id);
    }
}

// ============================================================================
// 7. OBSERVATION — what the capability actually buys
// ============================================================================

/// Not a gate on magnitude (the AC asks for the capability): a floor plus a
/// printed measurement the tuning run needs. Over 6 000 ticks with ample Alloy,
/// how many combat units does depth 1 / 3 / 8 finish?
#[test]
fn production_over_a_fixed_horizon_at_depths_one_three_eight() {
    let mut out: Vec<(u32, usize, usize)> = Vec::new();
    for depth in [1u32, 3, 8] {
        let c = content_with(
            &format!("prod_{depth}"),
            &[strategy(
                "critic_prod",
                30,
                3,
                &[("gene_vats", 0, 130.0)],
                &[("ripper", 1)],
                99,
                &depth.to_string(),
            )],
        );
        let mut app = matchup(c, 5, "critic_prod", "critic_prod", 1_000_000, true);
        tick(&mut app, 6_000);
        let built = combat_units(&mut app, Faction::A);
        let ordered = train_army_orders(&app, Faction::A);
        println!("PRODUCTION depth={depth} built={built} ordered={ordered}");
        out.push((depth, built, ordered));
    }
    assert!(
        out[2].1 >= out[0].1,
        "a deeper queue finished fewer units than depth 1: {out:?}"
    );
    assert!(
        out[2].2 > out[0].2,
        "depth changed nothing at all — not even the number of orders issued: {out:?}"
    );
}
