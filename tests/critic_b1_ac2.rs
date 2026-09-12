//! **Critic probes for B1 AC2** — "`AiCommander` constructible from any named
//! strategy; a match takes a (strategy, strategy) pair, one per side."
//!
//! Independent of `tests/b1_matchup.rs`: these are written from the spec, not
//! from the implementation. What they pin:
//!   - the two ways to say "the default" (`None` and `Some(default index)`) are
//!     the *same match*, even when the default is not index 0;
//!   - the whole script is per-commander (barracks, army, worker target,
//!     cadence) — not just the parts the shipped default happens to exercise;
//!   - a side's pre-contact play does not depend on what the opponent is
//!     running, for any opponent;
//!   - the same (strategy pair, seed) replays bit-identically *across
//!     processes and build profiles* (pinned to a file on first run);
//!   - the seed derivation is a function of (seed, slot) only;
//!   - dedup / slot order / degenerate matchups are total and deterministic.

use std::path::PathBuf;

use bevy::prelude::*;

use onus::sim::combat::{Casualties, Health};
use onus::sim::content::Content;
use onus::sim::economy::{Building, ProductionQueue, Stockpiles, UnitDefIdx};
use onus::sim::spatial::Faction;
use onus::sim::{
    AiAction, AiCommander, AiCommanders, AiJournal, CommandQueue, Position, RateReport,
    ResourceNode,
};

// ---- fixture content --------------------------------------------------------

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data")
}

fn scratch(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/critic_b1_ac2")
        .join(name)
}

struct Plan<'a> {
    id: &'a str,
    think: u32,
    workers: u32,
    barracks: &'a [(&'a str, u32, f32)],
    army: &'a [(&'a str, u32)],
    attack_at: u32,
}

fn render(p: &Plan) -> String {
    let b: Vec<String> = p
        .barracks
        .iter()
        .map(|(b, at, off)| format!("            (building: \"{b}\", at_tick: {at}, offset: {off:?}),"))
        .collect();
    let a: Vec<String> = p
        .army
        .iter()
        .map(|(u, n)| format!("            (unit: \"{u}\", count: {n}),"))
        .collect();
    format!(
        "        (\n            id: \"{}\",\n            think_interval_ticks: {},\n            worker_target: {},\n            barracks: [\n{}\n            ],\n            army: [\n{}\n            ],\n            attack_at_army: {},\n            attack_interval_ticks: 600,\n            attack_spread: 60.0,\n        ),",
        p.id,
        p.think,
        p.workers,
        b.join("\n"),
        a.join("\n"),
        p.attack_at,
    )
}

/// A content dir: the shipped units/resources, the shipped `mvp` strategy plus
/// `extra`, and an explicit default id.
fn content_of(name: &str, default: &str, extra: &[Plan]) -> Content {
    let base = std::fs::read_to_string(data_dir().join("strategies.ron")).expect("strategies.ron");
    let rendered: Vec<String> = extra.iter().map(render).collect();
    let text = base.replace("    ],\n)", &format!("{}\n    ],\n)", rendered.join("\n")));
    assert_ne!(text, base, "the strategies-list splice found no anchor");
    let text = text.replace("default: \"mvp\"", &format!("default: \"{default}\""));
    let dir = scratch(name);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    for f in ["units.ron", "resources.ron"] {
        std::fs::copy(data_dir().join(f), dir.join(f)).expect("copy content file");
    }
    std::fs::write(dir.join("strategies.ron"), text).expect("write strategies.ron");
    Content::load_from_dir(&dir).expect("fixture content is valid")
}

fn rip() -> Plan<'static> {
    Plan {
        id: "p_rip",
        think: 30,
        workers: 6,
        barracks: &[("gene_vats", 300, 130.0)],
        army: &[("ripper", 1)],
        attack_at: 3,
    }
}
fn arc() -> Plan<'static> {
    Plan {
        id: "p_arc",
        think: 40,
        workers: 10,
        barracks: &[("aether_spire", 300, 150.0)],
        army: &[("arclight", 1)],
        attack_at: 3,
    }
}
fn bul() -> Plan<'static> {
    Plan {
        id: "p_bul",
        think: 7,
        workers: 6,
        barracks: &[("foundry", 300, 110.0)],
        army: &[("bulwark", 1)],
        attack_at: 9,
    }
}

/// The standard probe set, default still `mvp`.
fn probe_content(name: &str) -> Content {
    content_of(name, "mvp", &[rip(), arc(), bul()])
}

// ---- headless match ---------------------------------------------------------

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

fn match_app(c: Content, commanders: AiCommanders) -> App {
    let alloy = c.economy.starting_alloy;
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
        app.world_mut()
            .spawn((Position(base + Vec2::new(0.0, 250.0)), ResourceNode { amount: 100_000 }));
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

fn named_match(c: Content, seed: u64, a: &str, b: &str) -> App {
    let cs = AiCommanders::matchup(&c, seed, &[(Faction::A, a), (Faction::B, b)])
        .expect("both names are in the fixture set");
    match_app(c, cs)
}

fn trace(app: &App, f: Faction) -> Vec<(u32, AiAction)> {
    app.world().resource::<AiJournal>().for_faction(f)
}

fn digest(app: &App) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for (t, f, a) in &app.world().resource::<AiJournal>().0 {
        for b in format!("{t}|{f:?}|{a:?}").as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100_0000_01b3);
        }
    }
    h
}

/// Per-tick state hashes every 100 ticks, plus the journal digest.
fn run_hashes(app: &mut App, samples: u32) -> (Vec<u64>, u64) {
    let mut hs = Vec::new();
    for _ in 0..samples {
        tick(app, 100);
        hs.push(onus::sim::state_hash(app.world_mut()));
    }
    (hs, digest(app))
}

// ---- 1. `None` and `Some(default)` are the same match ------------------------

/// The commander carries `Option<usize>`, so "the default" is expressible twice.
/// They must be the *same match* — including when the default is **not** index
/// 0, which is the case `strategies[0]`-shaped confusion would survive.
#[test]
fn defaulting_and_naming_the_default_are_the_same_match_off_index_zero() {
    let c = content_of("dflt_named", "p_arc", &[rip(), arc(), bul()]);
    assert_eq!(c.default_strategy, "p_arc");
    assert_ne!(c.strategy_index("p_arc"), Some(0), "fixture is not off-index-0");

    // `new` means the content's default, whatever its index.
    let plain = AiCommander::new(Faction::A, 5);
    assert_eq!(plain.strategy_index(), None);
    assert_eq!(plain.strategy(&c).id, "p_arc");
    let named = AiCommander::with_strategy(&c, Faction::A, 5, "p_arc").unwrap();
    assert_eq!(named.strategy(&c).id, "p_arc");
    assert_eq!(
        plain.strategy(&c).think_interval_ticks,
        named.strategy(&c).think_interval_ticks
    );
    assert_eq!(plain.rng_state(), named.rng_state());

    let mut implicit = match_app(
        content_of("dflt_named_i", "p_arc", &[rip(), arc(), bul()]),
        AiCommanders::new(5, &[Faction::A, Faction::B]),
    );
    let mut explicit = named_match(
        content_of("dflt_named_e", "p_arc", &[rip(), arc(), bul()]),
        5,
        "p_arc",
        "p_arc",
    );
    let a = run_hashes(&mut implicit, 20);
    let b = run_hashes(&mut explicit, 20);
    assert_eq!(a.0, b.0, "naming the default played a different match");
    assert_eq!(a.1, b.1, "naming the default took different decisions");
}

/// The dual: with a non-`mvp` default, naming `mvp` explicitly must play the
/// *`mvp`* script, not the content's default.
#[test]
fn naming_a_non_default_strategy_does_not_fall_back_to_the_default() {
    let c = content_of("nondflt", "p_arc", &[rip(), arc(), bul()]);
    let cmd = AiCommander::with_strategy(&c, Faction::A, 1, "mvp").unwrap();
    assert_eq!(cmd.strategy(&c).id, "mvp");
    assert_eq!(cmd.strategy_index(), c.strategy_index("mvp"));

    let mut m = named_match(
        content_of("nondflt_m", "p_arc", &[rip(), arc(), bul()]),
        5,
        "mvp",
        "p_arc",
    );
    tick(&mut m, 2_000);
    let placed = |f: Faction| -> Vec<String> {
        let c = m.world().resource::<Content>();
        trace(&m, f)
            .into_iter()
            .filter_map(|(_, a)| match a {
                AiAction::PlaceBarracks { building, .. } => Some(c.buildings[building].id.clone()),
                _ => None,
            })
            .collect()
    };
    assert_eq!(placed(Faction::A), vec!["foundry".to_string()], "A did not play `mvp`");
    assert_eq!(
        placed(Faction::B),
        vec!["aether_spire".to_string()],
        "B did not play the default it was named with"
    );
}

// ---- 2. the whole script is per-commander ------------------------------------

/// Not just the barracks list: `worker_target` is read from the commander's own
/// script too. A side asking for 10 workers must train more than a side asking
/// for 6, in the same match.
#[test]
fn worker_target_is_read_from_each_sides_own_script() {
    let mut m = named_match(probe_content("workers"), 8, "p_rip", "p_arc");
    tick(&mut m, 2_400);
    let workers = |f: Faction| {
        trace(&m, f)
            .into_iter()
            .filter(|(_, a)| matches!(a, AiAction::TrainWorker { .. }))
            .count()
    };
    let a = workers(Faction::A); // target 6, starts with 3
    let b = workers(Faction::B); // target 10, starts with 3
    assert!(a > 0 && b > 0, "a side trained no workers at all (A={a}, B={b})");
    assert!(
        b > a,
        "both sides used one worker target (A wanted 6 -> {a}, B wanted 10 -> {b})"
    );
}

/// Cadence: each side acts on multiples of *its own* `think_interval_ticks`,
/// with coprime intervals so neither can be mistaken for the other or for the
/// default's 30.
#[test]
fn each_side_acts_only_on_its_own_think_interval() {
    let mut m = named_match(probe_content("cadence"), 8, "p_bul", "p_arc");
    tick(&mut m, 2_400);
    let ticks = |f: Faction| -> Vec<u32> { trace(&m, f).into_iter().map(|(t, _)| t).collect() };
    let a = ticks(Faction::A); // think 7
    let b = ticks(Faction::B); // think 40
    assert!(!a.is_empty() && !b.is_empty(), "a side never acted");
    assert!(a.iter().all(|t| t % 7 == 0), "A acted off its 7-tick beat: {a:?}");
    assert!(b.iter().all(|t| t % 40 == 0), "B acted off its 40-tick beat: {b:?}");
    assert!(
        a.iter().any(|t| t % 30 != 0),
        "A never acted off the default's 30-tick beat — the cadence may not be its own"
    );
    assert!(
        b.iter().any(|t| t % 30 != 0),
        "B never acted off the default's 30-tick beat — the cadence may not be its own"
    );
}

// ---- 3. the two sides stay independent (B1 critic probe) --------------------

/// A's pre-contact play must be identical whatever B is running — its script,
/// its cursor and its RNG stream are its own. Checked for every opponent in the
/// set, including the mirror, and including B's own commander state.
#[test]
fn a_sides_opening_is_identical_whatever_the_opponent_plays() {
    const PRE_CONTACT: u32 = 900;
    let a_state = |name: &str, b: &str| {
        let mut m = named_match(probe_content(name), 8, "p_rip", b);
        tick(&mut m, PRE_CONTACT);
        let cs = m.world().resource::<AiCommanders>();
        let a = cs
            .commanders()
            .iter()
            .find(|k| k.faction == Faction::A)
            .expect("A has a commander");
        (trace(&m, Faction::A), a.rng_state(), a.tick())
    };
    let base = a_state("indep0", "p_rip");
    assert!(!base.0.is_empty(), "A did nothing to compare");
    for (i, b) in ["p_arc", "p_bul", "mvp"].iter().enumerate() {
        let got = a_state(&format!("indep{}", i + 1), b);
        assert_eq!(got.0, base.0, "A's decisions changed when only B's strategy did (B={b})");
        assert_eq!(got.1, base.1, "A's RNG stream depends on B's strategy (B={b})");
        assert_eq!(got.2, base.2, "A's clock depends on B's strategy (B={b})");
    }
}

// ---- 4. replay identity (B1 critic probe) -----------------------------------

/// The same (strategy pair, seed) replays bit-identically — and not only within
/// one process: the trace is pinned to a file on first run, so running this
/// suite again, in another profile, has to agree with it.
#[test]
fn a_non_default_pair_replays_bit_identically_across_runs_and_profiles() {
    let run = |name: &str, seed: u64| {
        let mut m = named_match(probe_content(name), seed, "p_rip", "p_bul");
        run_hashes(&mut m, 25)
    };
    let first = run("rep_a", 4);
    let second = run("rep_b", 4);
    assert_eq!(first.0, second.0, "per-tick state hashes diverged between two runs");
    assert_eq!(first.1, second.1, "the commanders took different decisions");

    let pin_dir = scratch("pins");
    std::fs::create_dir_all(&pin_dir).expect("pin dir");
    let pin = pin_dir.join("p_rip_vs_p_bul_seed4.txt");
    let record = format!(
        "{}\n{:#018x}\n",
        first.0.iter().map(|h| format!("{h:#018x}")).collect::<Vec<_>>().join(","),
        first.1
    );
    match std::fs::read_to_string(&pin) {
        Ok(prev) => assert_eq!(
            prev, record,
            "the same (pair, seed) played a different match than a previous process did \
             (delete {} to re-pin)",
            pin.display()
        ),
        Err(_) => std::fs::write(&pin, &record).expect("write pin"),
    }

    // Identity includes the pairing and the seed.
    let other_seed = run("rep_c", 11);
    assert_ne!(first.0, other_seed.0, "a different seed played the same match");
    let mut swapped = named_match(probe_content("rep_d"), 4, "p_rip", "p_arc");
    let swapped = run_hashes(&mut swapped, 25);
    assert_ne!(first.0, swapped.0, "changing B's strategy changed nothing");
    let mut mirrored = named_match(probe_content("rep_e"), 4, "p_bul", "p_rip");
    let mirrored = run_hashes(&mut mirrored, 25);
    assert_ne!(first.0, mirrored.0, "swapping the sides changed nothing");
}

// ---- 5. the seed derivation is (seed, slot) only ----------------------------

#[test]
fn the_rng_stream_is_a_function_of_seed_and_slot_only() {
    let c = probe_content("seeds");
    for seed in [0u64, 1, 7, 4, u64::MAX] {
        for f in [Faction::A, Faction::B] {
            let base = AiCommander::new(f, seed).rng_state();
            for id in ["mvp", "p_rip", "p_arc", "p_bul"] {
                assert_eq!(
                    AiCommander::with_strategy(&c, f, seed, id).unwrap().rng_state(),
                    base,
                    "`{id}` perturbed the stream for {f:?} at seed {seed}"
                );
            }
        }
        assert_ne!(
            AiCommander::with_strategy(&c, Faction::A, seed, "p_rip").unwrap().rng_state(),
            AiCommander::with_strategy(&c, Faction::B, seed, "p_rip").unwrap().rng_state(),
            "the two sides share a stream at seed {seed}"
        );
    }
}

// ---- 6. refusal, dedup, slot order, degenerate matchups ---------------------

#[test]
fn an_unknown_name_is_refused_on_either_side_and_names_the_culprit() {
    let c = probe_content("refuse");
    for bad in ["", "p_ripper", "P_RIP", "mvp ", "p_rip\n"] {
        let e = AiCommander::with_strategy(&c, Faction::A, 1, bad)
            .expect_err("`{bad}` is not a strategy in the set");
        assert_eq!(e.id, bad);
        assert!(AiCommanders::matchup(&c, 1, &[(Faction::A, bad), (Faction::B, "p_rip")]).is_err());
        assert!(AiCommanders::matchup(&c, 1, &[(Faction::A, "p_rip"), (Faction::B, bad)]).is_err());
    }
    let e = AiCommanders::matchup(&c, 1, &[(Faction::A, "p_rip"), (Faction::B, "nope")])
        .expect_err("B names nothing");
    assert_eq!(e.faction, Some(Faction::B));
    assert!(e.to_string().contains("nope"));
}

#[test]
fn degenerate_matchups_are_total_and_deterministic() {
    let c = probe_content("degenerate");
    // One side only: legal, one commander, in slot order.
    let one = AiCommanders::matchup(&c, 3, &[(Faction::B, "p_arc")]).unwrap();
    assert_eq!(one.commanders().len(), 1);
    assert_eq!(one.commanders()[0].faction, Faction::B);
    assert_eq!(one.commanders()[0].strategy(&c).id, "p_arc");

    // No sides: legal and empty; the shipped chain runs with nobody thinking.
    let none = AiCommanders::matchup(&c, 3, &[]).unwrap();
    assert!(none.commanders().is_empty());
    let mut m = match_app(probe_content("degenerate_m"), none);
    tick(&mut m, 200);
    assert!(m.world().resource::<AiJournal>().0.is_empty());

    // The same faction twice: still exactly one brain, and the same one every
    // time (dedup must not be order-of-the-day).
    for _ in 0..8 {
        let dup = AiCommanders::matchup(
            &c,
            3,
            &[(Faction::A, "p_rip"), (Faction::A, "p_bul"), (Faction::B, "p_arc")],
        )
        .unwrap();
        assert_eq!(dup.commanders().len(), 2, "a faction was given two brains");
        assert_eq!(dup.commanders()[0].faction, Faction::A);
        assert_eq!(dup.commanders()[1].faction, Faction::B);
        assert_eq!(
            dup.commanders()[0].strategy(&c).id,
            "p_rip",
            "dedup did not keep the first-named strategy for the duplicated side"
        );
    }

    // Slot order regardless of the order the pair was given in.
    let ba = AiCommanders::matchup(&c, 3, &[(Faction::B, "p_arc"), (Faction::A, "p_rip")]).unwrap();
    let ab = AiCommanders::matchup(&c, 3, &[(Faction::A, "p_rip"), (Faction::B, "p_arc")]).unwrap();
    let ids = |cs: &AiCommanders| -> Vec<(Faction, String)> {
        cs.commanders()
            .iter()
            .map(|k| (k.faction, k.strategy(&c).id.clone()))
            .collect()
    };
    assert_eq!(ids(&ba), ids(&ab));
    assert_eq!(
        ids(&ab),
        vec![
            (Faction::A, "p_rip".to_string()),
            (Faction::B, "p_arc".to_string())
        ]
    );
}

// ---- 7. characterisation: a commander resolved against foreign content ------

/// **Characterisation, not a requirement of the AC.** `strategy()` resolves a
/// bare index, so a commander built against one content and run against another
/// of at least the same size silently plays whatever sits at that index — the
/// exact mislabel the refusal path exists to prevent, arriving by a different
/// door. Recorded so a later fix has a test to flip.
#[test]
fn a_commander_resolved_against_foreign_content_silently_plays_another_script() {
    let built_on = content_of("foreign_a", "mvp", &[rip(), arc(), bul()]);
    // Same arity, different order: index 1 is `p_bul` here, `p_rip` there.
    let run_on = content_of("foreign_b", "mvp", &[bul(), arc(), rip()]);
    let cmd = AiCommander::with_strategy(&built_on, Faction::A, 1, "p_rip").unwrap();
    assert_eq!(cmd.strategy(&built_on).id, "p_rip");
    assert_eq!(
        cmd.strategy(&run_on).id,
        "p_bul",
        "if this now fails, foreign content is being caught — update this probe"
    );
}
