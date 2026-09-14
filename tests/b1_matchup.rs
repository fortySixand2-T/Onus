//! L2 integration tests for **B1 AC2** — a commander is constructible from any
//! *named* strategy, and a match takes a (strategy, strategy) pair, one per
//! side.
//!
//! What is encoded here:
//!   - a commander carries the **index** of the strategy it was named with
//!     (stable RON order), and `new` still means "the default";
//!   - naming a strategy that does not exist is **refused**, never silently
//!     defaulted — a typo in a matchup must not play the default and report the
//!     result under the wrong name;
//!   - both sides really run their own script: their openings, their build
//!     orders and their **think cadence** are per-commander;
//!   - **the B1 critic probes**: the two sides stay independent, and the same
//!     (strategy pair, seed) replays bit-identically;
//!   - the default matchup did not move — golden `state_hash` *and* golden
//!     `AiJournal` digests taken from the pre-AC2 build.
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
    AiAction, AiCommander, AiCommanders, AiJournal, CommandQueue, Position, RateReport,
    ResourceNode,
};

// ---- harness ----------------------------------------------------------------

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data")
}

fn content() -> Content {
    Content::load_from_dir(&data_dir()).expect("assets/data/*.ron parse into sim structs")
}

/// One strategy entry, spelled out, with its own think cadence.
fn strategy(
    id: &str,
    think: u32,
    barracks: &[(&str, u32, f32)],
    army: &[(&str, u32)],
    attack_at_army: u32,
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
         worker_target: 6,\n            barracks: [\n{}\n            ],\n            army: [\n{}\n            ],\n            \
         attack_at_army: {attack_at_army},\n            attack_interval_ticks: 600,\n            attack_spread: 60.0,\n        ),",
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
        .join("target/b1_matchup")
        .join(name);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    for file in ["units.ron", "resources.ron"] {
        std::fs::copy(data_dir().join(file), dir.join(file)).expect("copy");
    }
    std::fs::write(dir.join("strategies.ron"), text).expect("write strategies.ron");
    Content::load_from_dir(&dir).expect("fixture content loads")
}

/// The fixture set every matchup test below plays with: three single-domain
/// scripts that share nothing but the economy.
fn fixture_content(name: &str) -> Content {
    content_with(
        name,
        &[
            // Flesh: cheap Rippers, out of a Gene-Vats. Thinks fast.
            strategy(
                "probe_ripper",
                30,
                &[("gene_vats", 300, 130.0)],
                &[("ripper", 1)],
                3,
            ),
            // Energy: Arclights out of an Aether Spire. Thinks slowly (3x).
            strategy(
                "probe_arclight",
                90,
                &[("aether_spire", 300, 150.0)],
                &[("arclight", 1)],
                3,
            ),
            // Machine: Bulwarks out of a Foundry, sitting on a big army count.
            strategy(
                "probe_bulwark",
                30,
                &[("foundry", 300, 110.0)],
                &[("bulwark", 1)],
                8,
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
/// commanders handed in, so a test can play any pairing it likes.
fn ai_vs_ai_with(c: Content, commanders: AiCommanders) -> App {
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
        app.world_mut().spawn((
            Position(base + Vec2::new(0.0, 250.0)),
            ResourceNode { amount: 100_000 },
        ));
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

/// A match of `a` vs `b`, by strategy name, on the fixture content.
fn matchup(name: &str, seed: u64, a: &str, b: &str) -> App {
    let c = fixture_content(name);
    let commanders = AiCommanders::matchup(&c, seed, &[(Faction::A, a), (Faction::B, b)])
        .expect("both strategies are named in the fixture set");
    ai_vs_ai_with(c, commanders)
}

fn journal(app: &App, f: Faction) -> Vec<(u32, AiAction)> {
    app.world().resource::<AiJournal>().for_faction(f)
}

/// The units a side actually put into production, as ids.
fn trained(app: &App, f: Faction) -> Vec<String> {
    let c = app.world().resource::<Content>();
    journal(app, f)
        .into_iter()
        .filter_map(|(_, a)| match a {
            AiAction::TrainArmy { unit } => Some(c.units[unit].id.clone()),
            _ => None,
        })
        .collect()
}

/// The buildings a side placed, as ids.
fn placed(app: &App, f: Faction) -> Vec<String> {
    let c = app.world().resource::<Content>();
    journal(app, f)
        .into_iter()
        .filter_map(|(_, a)| match a {
            AiAction::PlaceBarracks { building, .. } => Some(c.buildings[building].id.clone()),
            _ => None,
        })
        .collect()
}

/// A stable digest of the whole action trace — what "the same decisions" means.
///
/// Entities in the trace are **relabelled by first appearance** before hashing.
/// Raw `Entity` bits are an ECS allocation detail and not sim state (F-011):
/// in Bevy a resource *is* an entity, so installing one more sim-owned resource
/// shifts every id the world hands out afterwards, without changing a single
/// decision. Relabelling keeps everything the trace actually says — which
/// commander acted, on which tick, on which *same* unit and node — and drops
/// only the allocation offset. The per-tick `state_hash` golden beside this one
/// is the real regression floor, and it is keyed by `SimId`, so it is untouched
/// by the same shift.
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

// ---- construction from a name -----------------------------------------------

/// A commander carries the *index* of its strategy (stable RON order), and the
/// old constructor still means "the default".
#[test]
fn a_commander_is_constructible_from_any_named_strategy() {
    let c = fixture_content("named");
    for id in ["mvp", "probe_ripper", "probe_arclight", "probe_bulwark"] {
        let cmd = AiCommander::with_strategy(&c, Faction::A, 7, id)
            .unwrap_or_else(|_| panic!("`{id}` is in the fixture set"));
        assert_eq!(
            cmd.strategy_index(),
            c.strategy_index(id),
            "`{id}` resolved to the wrong index"
        );
        assert_eq!(cmd.strategy(&c).id, id);
        assert_eq!(cmd.faction, Faction::A);
    }
    // `new` is the default strategy, resolved against the content it plays on.
    let plain = AiCommander::new(Faction::B, 7);
    assert_eq!(plain.strategy(&c).id, c.default_strategy);
}

/// Decision 2: a typo is an error the caller must handle. It must never quietly
/// play the default and report the match under a name nobody ran.
#[test]
fn an_unknown_strategy_is_refused_never_silently_defaulted() {
    let c = fixture_content("unknown");
    let err = AiCommander::with_strategy(&c, Faction::A, 1, "mass_rippr").unwrap_err();
    assert_eq!(err.id, "mass_rippr");
    assert!(
        err.to_string().contains("mass_rippr"),
        "the error does not name the offending strategy: {err}"
    );

    let err = AiCommanders::matchup(
        &c,
        1,
        &[(Faction::A, "probe_ripper"), (Faction::B, "turtl")],
    )
    .unwrap_err();
    assert_eq!(err.id, "turtl");
    assert_eq!(err.faction, Some(Faction::B), "the error names the side");

    // ...and the good side being valid does not rescue the match.
    assert!(AiCommanders::matchup(&c, 1, &[(Faction::A, "nope"), (Faction::B, "mvp")]).is_err());
    assert!(
        AiCommanders::matchup(&c, 1, &[(Faction::A, "mvp"), (Faction::B, "probe_ripper")]).is_ok()
    );
}

/// Decision 4: the pair is stored in faction-slot order, whatever order it was
/// given in, and a faction still gets exactly one brain.
#[test]
fn a_matchup_is_one_strategy_per_side_in_slot_order() {
    let c = fixture_content("slots");
    let cs = AiCommanders::matchup(
        &c,
        5,
        &[(Faction::B, "probe_arclight"), (Faction::A, "probe_ripper")],
    )
    .unwrap();
    let got: Vec<(Faction, &str)> = cs
        .commanders()
        .iter()
        .map(|k| (k.faction, k.strategy(&c).id.as_str()))
        .collect();
    assert_eq!(
        got,
        vec![
            (Faction::A, "probe_ripper"),
            (Faction::B, "probe_arclight")
        ],
        "the pair is not in faction-slot order"
    );
}

/// Decision 5: the RNG stream is a function of (seed, faction slot) *only* —
/// not of the strategy. B2 needs one seed to mean one opening across matchups.
#[test]
fn the_seed_stream_does_not_depend_on_the_strategy() {
    let c = fixture_content("seeds");
    let state = |id: Option<&str>| match id {
        Some(id) => AiCommander::with_strategy(&c, Faction::A, 9, id)
            .unwrap()
            .rng_state(),
        None => AiCommander::new(Faction::A, 9).rng_state(),
    };
    let base = state(None);
    for id in ["mvp", "probe_ripper", "probe_arclight", "probe_bulwark"] {
        assert_eq!(state(Some(id)), base, "`{id}` perturbed the seed derivation");
    }
    // And the two sides still do not share a stream.
    assert_ne!(
        AiCommander::with_strategy(&c, Faction::A, 9, "probe_ripper")
            .unwrap()
            .rng_state(),
        AiCommander::with_strategy(&c, Faction::B, 9, "probe_ripper")
            .unwrap()
            .rng_state(),
    );
}

// ---- both sides really run their own script ---------------------------------

/// The whole point of the AC: each side plays *its* build order, through the
/// shipped sim chain.
#[test]
fn each_side_plays_its_own_strategy() {
    let mut app = matchup("own", 4, "probe_ripper", "probe_bulwark");
    tick(&mut app, 3_000);

    assert_eq!(placed(&app, Faction::A), vec!["gene_vats".to_string()]);
    assert_eq!(placed(&app, Faction::B), vec!["foundry".to_string()]);

    let a = trained(&app, Faction::A);
    let b = trained(&app, Faction::B);
    assert!(!a.is_empty(), "A trained nothing");
    assert!(!b.is_empty(), "B trained nothing");
    assert!(a.iter().all(|u| u == "ripper"), "A built {a:?}");
    assert!(b.iter().all(|u| u == "bulwark"), "B built {b:?}");
}

/// Decision 6: `think_interval_ticks` is per-commander. Two strategies with
/// different APMs must not both think on the default's cadence.
#[test]
fn each_side_thinks_on_its_own_cadence() {
    let mut app = matchup("cadence", 4, "probe_ripper", "probe_arclight");
    tick(&mut app, 3_000);

    let ticks = |f| -> Vec<u32> { journal(&app, f).into_iter().map(|(t, _)| t).collect() };
    let a = ticks(Faction::A);
    let b = ticks(Faction::B);
    assert!(!a.is_empty() && !b.is_empty(), "a side never acted");
    assert!(a.iter().all(|t| t % 30 == 0), "A acted off its 30-tick beat");
    assert!(
        b.iter().all(|t| t % 90 == 0),
        "B acted off its 90-tick beat (the default's cadence leaked in): {b:?}"
    );
    assert!(
        a.iter().any(|t| t % 90 != 0),
        "A never acted on a tick B could not have — the cadences are indistinguishable"
    );
}

/// Critic probe: **the two sides stay independent**. Swapping B's strategy
/// leaves A's decisions untouched up to first contact — A's script, cursor and
/// RNG stream are its own.
#[test]
fn the_two_sides_stay_independent() {
    const PRE_CONTACT: u32 = 900;
    let a_trace = |name: &str, b: &str| {
        let mut app = matchup(name, 4, "probe_ripper", b);
        tick(&mut app, PRE_CONTACT);
        journal(&app, Faction::A)
    };
    let vs_bulwark = a_trace("indep_a", "probe_bulwark");
    let vs_arclight = a_trace("indep_b", "probe_arclight");
    let vs_self = a_trace("indep_c", "probe_ripper");
    assert!(!vs_bulwark.is_empty(), "A did nothing to compare");
    assert_eq!(
        vs_bulwark, vs_arclight,
        "A's decisions changed when only B's strategy did"
    );
    assert_eq!(vs_bulwark, vs_self, "A's decisions depend on B's script");
}

/// Critic probe: the same (strategy pair, seed) replays bit-identically — same
/// per-tick state hash *and* the same action trace. A different seed does not.
#[test]
fn the_same_strategy_pair_and_seed_replays_bit_identically() {
    let run = |name: &str, seed: u64| {
        let mut app = matchup(name, seed, "probe_ripper", "probe_arclight");
        let mut hashes = Vec::new();
        for _ in 0..30 {
            tick(&mut app, 100);
            hashes.push(onus::sim::state_hash(app.world_mut()));
        }
        (hashes, journal_digest(&app))
    };
    let first = run("det_a", 4);
    let second = run("det_b", 4);
    assert_eq!(first.0, second.0, "the per-tick state hashes diverged");
    assert_eq!(first.1, second.1, "the commanders took different decisions");

    let other = run("det_c", 11);
    assert_ne!(
        first.0, other.0,
        "a different seed played an identical match"
    );

    // The pairing itself is part of the identity: swap B's strategy and the
    // match must differ.
    let mut swapped = matchup("det_d", 4, "probe_ripper", "probe_bulwark");
    let mut hashes = Vec::new();
    for _ in 0..30 {
        tick(&mut swapped, 100);
        hashes.push(onus::sim::state_hash(swapped.world_mut()));
    }
    assert_ne!(first.0, hashes, "changing a side's strategy changed nothing");
}

// ---- the default matchup did not move ---------------------------------------

/// The regression floor: a match where both sides run the default plays exactly
/// the match it played before AC2 — same per-tick `state_hash` and the same
/// `AiJournal`. The state-hash goldens are the pre-AC2 build's, untouched. The
/// journal goldens were **recomputed once** at B2 AC4, when `journal_digest`
/// stopped hashing raw entity bits (see its docs): adding the sim's `Produced`
/// resource shifts every entity id by one without changing any decision, and a
/// golden that moves for that is measuring the allocator, not the AI.
#[test]
fn the_default_matchup_is_byte_for_byte_what_it_was_before_ac2() {
    for (seed, state_golden, journal_golden) in [
        (4u64, 0xa71f_64ca_d502_03e9u64, 0xe78e_ebdc_5c2c_a733u64),
        (11, 0x5b39_8ee4_7854_23dc, 0x00b7_f8d8_713f_e467),
        (23, 0xf4b5_7d1c_3c3f_2af7, 0x4682_1006_f2fa_e62a),
    ] {
        // The implicit default, through the untouched constructor.
        let mut app = ai_vs_ai_with(content(), AiCommanders::new(seed, &[Faction::A, Faction::B]));
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

        // ...and naming the default explicitly, as a matchup, is the same match.
        let c = content();
        let pair = AiCommanders::matchup(
            &c,
            seed,
            &[(Faction::A, "mvp"), (Faction::B, "mvp")],
        )
        .expect("the shipped default is a named strategy");
        let mut named = ai_vs_ai_with(c, pair);
        tick(&mut named, 3_000);
        assert_eq!(
            onus::sim::state_hash(named.world_mut()),
            state_golden,
            "seed {seed}: naming the default played a different match than defaulting to it"
        );
        assert_eq!(journal_digest(&named), journal_golden);
    }
}
