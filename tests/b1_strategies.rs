//! L2 integration tests for **B1 AC1** — `mvp_ai` promoted into a *set* of
//! named strategies in `assets/data/strategies.ron`, with a schema that lets one
//! strategy open **several** barracks and build across domains.
//!
//! What is encoded here:
//!   - the shipped set carries the old `mvp_ai` numbers, unchanged, under a
//!     named default the loader resolves;
//!   - a multi-barracks strategy really opens all of them and trains across
//!     domains, through the shipped sim chain;
//!   - **the B1 critic probe**: a strategy naming a unit none of *its* barracks
//!     can produce is refused at load — and every strategy is checked, not just
//!     the default;
//!   - the content fingerprint covers every strategy and every field of one;
//!   - the promotion is behaviour-preserving: the one-barracks default replays
//!     bit-for-bit as it did before the refactor (a golden state hash taken from
//!     the pre-B1 build).
//!
//! Everything runs headless (`MinimalPlugins`) through the *shipped* sim chain
//! (`onus::add_sim_systems`) — no render types, no hand-rolled system list.

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

/// The shipped `strategies.ron`, as text.
fn shipped_strategies() -> String {
    std::fs::read_to_string(data_dir().join("strategies.ron")).expect("strategies.ron")
}

/// A content dir whose `strategies.ron` is `text` and whose other files are the
/// shipped ones, loaded.
fn load_with_strategies(name: &str, text: &str) -> Result<Content, String> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/b1_content")
        .join(name);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    for file in ["units.ron", "resources.ron"] {
        std::fs::copy(data_dir().join(file), dir.join(file)).expect("copy");
    }
    std::fs::write(dir.join("strategies.ron"), text).expect("write strategies.ron");
    Content::load_from_dir(&dir).map_err(|e| e.to_string())
}

/// A whole strategies file holding exactly the entries given.
fn strategies_file(default: &str, entries: &[String]) -> String {
    format!("(\n    default: \"{default}\",\n    strategies: [\n{}\n    ],\n)\n", entries.join("\n"))
}

/// One strategy entry, with the barracks list and army spelled out.
fn strategy(id: &str, barracks: &[(&str, u32, f32)], army: &[(&str, u32)]) -> String {
    let openings: Vec<String> = barracks
        .iter()
        .map(|(b, at, off)| format!("            (building: \"{b}\", at_tick: {at}, offset: {off:?}),"))
        .collect();
    let army: Vec<String> = army
        .iter()
        .map(|(u, n)| format!("            (unit: \"{u}\", count: {n}),"))
        .collect();
    format!(
        "        (\n            id: \"{id}\",\n            think_interval_ticks: 30,\n            \
         worker_target: 6,\n            barracks: [\n{}\n            ],\n            army: [\n{}\n            ],\n            \
         attack_at_army: 3,\n            attack_interval_ticks: 600,\n            attack_spread: 60.0,\n        ),",
        openings.join("\n"),
        army.join("\n"),
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

/// The standard headless AI-vs-AI match shape (the M4c fixture), on the content
/// given, so a test can hand the commanders any strategy set it likes.
fn ai_vs_ai_on(c: Content, seed: u64) -> App {
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
    app.insert_resource(AiCommanders::new(seed, &[Faction::A, Faction::B]));
    app
}

fn journal(app: &App, f: Faction) -> Vec<(u32, AiAction)> {
    app.world().resource::<AiJournal>().for_faction(f)
}

fn placements(app: &App, f: Faction) -> Vec<(usize, Vec2)> {
    journal(app, f)
        .into_iter()
        .filter_map(|(_, a)| match a {
            AiAction::PlaceBarracks { building, pos } => Some((building, pos)),
            _ => None,
        })
        .collect()
}

// ---- the set, and the promoted default --------------------------------------

/// The set exists, is data, and the default is a *name* the loader resolves —
/// not a block hidden inside `units.ron`.
#[test]
fn the_shipped_content_carries_a_named_strategy_set() {
    let c = content();
    assert!(!c.strategies.is_empty(), "no strategies were loaded");
    assert_eq!(c.default_strategy, "mvp");
    assert!(c.strategy("mvp").is_some(), "the default names no strategy");
    assert_eq!(c.strategy_index("mvp"), Some(0), "lookup is RON order");
    assert_eq!(c.strategy_index("nonesuch"), None);
    assert!(c.strategy("nonesuch").is_none());
    // `Content::ai` is the resolved default, field for field.
    assert_eq!(c.ai.id, c.default_strategy);
    let d = c.strategy(&c.default_strategy).unwrap();
    assert_eq!(c.ai.think_interval_ticks, d.think_interval_ticks);
    assert_eq!(c.ai.worker_target, d.worker_target);
    assert_eq!(c.ai.attack_at_army, d.attack_at_army);
    assert_eq!(c.ai.attack_interval_ticks, d.attack_interval_ticks);
    assert_eq!(c.ai.attack_spread, d.attack_spread);
    assert_eq!(c.ai.army.len(), d.army.len());
    assert_eq!(c.ai.barracks.len(), d.barracks.len());
    // And `units.ron` no longer carries a script at all.
    let units = std::fs::read_to_string(data_dir().join("units.ron")).expect("units.ron");
    assert!(
        !units.contains("mvp_ai"),
        "the AI script is still living in units.ron"
    );
}

/// The promotion preserved the M4c script **number for number** — the one
/// strategy this AC ships is the old `mvp_ai`, not a re-tune smuggled in with a
/// refactor.
#[test]
fn the_default_strategy_is_the_old_mvp_ai_number_for_number() {
    let s = content().ai;
    assert_eq!(s.think_interval_ticks, 30);
    assert_eq!(s.worker_target, 6);
    assert_eq!(s.attack_at_army, 3);
    assert_eq!(s.attack_interval_ticks, 600);
    assert_eq!(s.attack_spread, 60.0);
    assert_eq!(s.barracks.len(), 1, "the MVP opener opens one barracks");
    assert_eq!(s.barracks[0].building, "foundry");
    assert_eq!(s.barracks[0].at_tick, 300);
    assert_eq!(s.barracks[0].offset, 130.0);
    let army: Vec<(&str, u32)> = s
        .army
        .iter()
        .map(|i| (i.unit.as_str(), i.count))
        .collect();
    assert_eq!(army, vec![("sentinel", 2), ("bulwark", 1)]);
}

// ---- multiple barracks, across domains --------------------------------------

/// The schema extension, end to end: a strategy that opens Foundry, Gene-Vats
/// *and* Aether Spire places all three and trains units from all three — an army
/// spanning every domain, which the single-barracks `mvp_ai` could never field.
#[test]
fn a_strategy_may_open_several_barracks_and_build_across_domains() {
    let text = strategies_file(
        "wide",
        &[strategy(
            "wide",
            &[
                ("foundry", 60, 130.0),
                ("gene_vats", 120, 160.0),
                ("aether_spire", 180, 190.0),
            ],
            &[("sentinel", 1), ("ripper", 1), ("arclight", 1)],
        )],
    );
    let c = load_with_strategies("wide", &text).expect("a three-barracks strategy is valid");
    let defs: Vec<usize> = ["foundry", "gene_vats", "aether_spire"]
        .iter()
        .map(|b| c.building_index(b).unwrap())
        .collect();
    let units: Vec<usize> = ["sentinel", "ripper", "arclight"]
        .iter()
        .map(|u| c.unit_index(u).unwrap())
        .collect();

    let mut app = ai_vs_ai_on(c, 5);
    tick(&mut app, 14_000);

    let placed: Vec<usize> = placements(&app, Faction::A).iter().map(|(b, _)| *b).collect();
    for (name, def) in ["foundry", "gene_vats", "aether_spire"].iter().zip(&defs) {
        assert_eq!(
            placed.iter().filter(|b| *b == def).count(),
            1,
            "`{name}` was not opened exactly once (placements: {placed:?})"
        );
    }

    let trained: Vec<usize> = journal(&app, Faction::A)
        .into_iter()
        .filter_map(|(_, a)| match a {
            AiAction::TrainArmy { unit } => Some(unit),
            _ => None,
        })
        .collect();
    for (name, unit) in ["sentinel", "ripper", "arclight"].iter().zip(&units) {
        assert!(
            trained.contains(unit),
            "`{name}` was never trained, so the army does not span its domains \
             (trained: {trained:?})"
        );
    }
}

/// Each army entry is trained at *its own* barracks: the units that came out
/// belong to the buildings the strategy opened for them, in build-order.
#[test]
fn each_army_entry_is_trained_at_the_barracks_that_produces_it() {
    let text = strategies_file(
        "split",
        &[strategy(
            "split",
            &[("foundry", 60, 130.0), ("gene_vats", 120, 160.0)],
            &[("bulwark", 1), ("ravager", 1)],
        )],
    );
    let c = load_with_strategies("split", &text).expect("a two-barracks strategy is valid");
    let order: Vec<usize> = ["bulwark", "ravager"]
        .iter()
        .map(|u| c.unit_index(u).unwrap())
        .collect();
    let mut app = ai_vs_ai_on(c, 5);
    tick(&mut app, 16_000);
    let trained: Vec<usize> = journal(&app, Faction::A)
        .into_iter()
        .filter_map(|(_, a)| match a {
            AiAction::TrainArmy { unit } => Some(unit),
            _ => None,
        })
        .collect();
    assert!(
        trained.len() >= 3,
        "only {} army orders in 16k ticks — the cursor stalled",
        trained.len()
    );
    for (n, unit) in trained.iter().enumerate() {
        assert_eq!(*unit, order[n % order.len()], "build order diverged at {n}");
    }
}

// ---- the critic probe: unbuildable army is refused at load -------------------

/// **The B1 critic probe.** A strategy asking for a unit none of *its own*
/// barracks can produce is refused at load — not a commander that stalls
/// forever on a unit it has nowhere to train. Opening the right barracks makes
/// exactly the same army legal, so the rule is about the barracks list, not
/// about the unit.
#[test]
fn a_strategy_naming_a_unit_its_barracks_cannot_produce_is_refused_at_load() {
    let army = [("sentinel", 2), ("ripper", 1)];
    let narrow = strategies_file("narrow", &[strategy("narrow", &[("foundry", 300, 130.0)], &army)]);
    let err = load_with_strategies("narrow", &narrow)
        .expect_err("a Foundry-only strategy asking for a Ripper loaded");
    assert!(err.contains("narrow") && err.contains("ripper"), "unexpected rejection: {err}");

    let wide = strategies_file(
        "narrow",
        &[strategy(
            "narrow",
            &[("foundry", 300, 130.0), ("gene_vats", 400, 130.0)],
            &army,
        )],
    );
    load_with_strategies("narrow-ok", &wide)
        .expect("the same army is legal once the Gene-Vats is opened");
}

/// Validation is over the whole *set*: a broken entry is refused even when it is
/// not the default, and the error names it. An unreachable strategy is only
/// unreachable until the day a match names it.
#[test]
fn every_strategy_is_validated_not_only_the_default() {
    let text = strategies_file(
        "good",
        &[
            strategy("good", &[("foundry", 300, 130.0)], &[("sentinel", 2)]),
            strategy("rotten", &[("foundry", 300, 130.0)], &[("ripper", 1)]),
        ],
    );
    let err = load_with_strategies("rotten", &text)
        .expect_err("a broken non-default strategy loaded");
    assert!(
        err.contains("rotten"),
        "the error does not name the offending strategy: {err}"
    );
}

/// Every shape of unrunnable strategy is an `Err` from the loader, in both
/// profiles — each of these is a way a commander would otherwise fail silently.
#[test]
fn unrunnable_strategies_are_refused_at_load() {
    let ok = |b: &[(&str, u32, f32)], a: &[(&str, u32)]| strategy("s", b, a);
    let one = |s: String| strategies_file("s", &[s]);
    let cases: Vec<(&str, String)> = vec![
        (
            "no barracks at all",
            one(ok(&[], &[("sentinel", 1)])).replace("barracks: [\n\n            ]", "barracks: []"),
        ),
        (
            "a barracks that is not a building",
            one(ok(&[("nonesuch", 300, 130.0)], &[("sentinel", 1)])),
        ),
        (
            "the victory building as a barracks",
            one(ok(&[("hq", 300, 130.0)], &[("worker", 1)])),
        ),
        (
            "the same barracks twice",
            one(ok(
                &[("foundry", 300, 130.0), ("foundry", 600, 160.0)],
                &[("sentinel", 1)],
            )),
        ),
        (
            "a zero offset",
            one(ok(&[("foundry", 300, 0.0)], &[("sentinel", 1)])),
        ),
        (
            "an unknown unit",
            one(ok(&[("foundry", 300, 130.0)], &[("nonesuch", 1)])),
        ),
        (
            "a count of 0",
            one(ok(&[("foundry", 300, 130.0)], &[("sentinel", 0)])),
        ),
        (
            "counts that overflow u32",
            one(ok(
                &[("foundry", 300, 130.0)],
                &[("sentinel", u32::MAX), ("bulwark", 2)],
            )),
        ),
        (
            "an empty army",
            one(ok(&[("foundry", 300, 130.0)], &[]))
                .replace("army: [\n\n            ]", "army: []"),
        ),
        (
            "a duplicate id",
            strategies_file(
                "s",
                &[
                    ok(&[("foundry", 300, 130.0)], &[("sentinel", 1)]),
                    ok(&[("foundry", 300, 130.0)], &[("sentinel", 1)]),
                ],
            ),
        ),
        (
            "a default naming no strategy",
            strategies_file("nonesuch", &[ok(&[("foundry", 300, 130.0)], &[("sentinel", 1)])]),
        ),
        (
            "an empty set",
            "(\n    default: \"s\",\n    strategies: [],\n)\n".to_string(),
        ),
        (
            "a think interval of 0",
            one(ok(&[("foundry", 300, 130.0)], &[("sentinel", 1)]))
                .replace("think_interval_ticks: 30", "think_interval_ticks: 0"),
        ),
        (
            "a wave interval of 0",
            one(ok(&[("foundry", 300, 130.0)], &[("sentinel", 1)]))
                .replace("attack_interval_ticks: 600", "attack_interval_ticks: 0"),
        ),
        (
            "a worker target of 0",
            one(ok(&[("foundry", 300, 130.0)], &[("sentinel", 1)]))
                .replace("worker_target: 6", "worker_target: 0"),
        ),
        (
            "an attack threshold of 0",
            one(ok(&[("foundry", 300, 130.0)], &[("sentinel", 1)]))
                .replace("attack_at_army: 3", "attack_at_army: 0"),
        ),
        (
            "a negative spread",
            one(ok(&[("foundry", 300, 130.0)], &[("sentinel", 1)]))
                .replace("attack_spread: 60.0", "attack_spread: -1.0"),
        ),
        (
            "a missing field",
            one(ok(&[("foundry", 300, 130.0)], &[("sentinel", 1)]))
                .replace("worker_target: 6,", ""),
        ),
        (
            "a missing at_tick",
            one(ok(&[("foundry", 300, 130.0)], &[("sentinel", 1)]))
                .replace("at_tick: 300, ", ""),
        ),
    ];
    for (i, (what, text)) in cases.iter().enumerate() {
        assert!(
            load_with_strategies(&format!("bad{i}"), text).is_err(),
            "`{what}` was accepted, but the sim cannot run it:\n{text}"
        );
    }
}

/// The shipped set is the control: the same harness accepts it.
#[test]
fn the_shipped_strategies_file_loads_through_the_same_harness() {
    load_with_strategies("shipped", &shipped_strategies()).expect("the shipped set must load");
}

// ---- the fingerprint --------------------------------------------------------

/// A log is only meaningful against the content it was recorded under, and a
/// strategy *is* content: any edit to any field of any strategy must move the
/// fingerprint. (The standing per-field guard lives in `critic_m5.rs`; this is
/// the behavioural half — the guard proves the source mentions each field, this
/// proves the hash actually changes.)
#[test]
fn the_fingerprint_covers_every_strategy_and_every_field() {
    let base = content().fingerprint();
    let edits = [
        ("think_interval_ticks: 30", "think_interval_ticks: 31"),
        ("worker_target: 6", "worker_target: 7"),
        ("at_tick: 300", "at_tick: 301"),
        ("offset: 130.0", "offset: 131.0"),
        ("(unit: \"sentinel\", count: 2)", "(unit: \"sentinel\", count: 3)"),
        ("(unit: \"sentinel\", count: 2)", "(unit: \"bulwark\", count: 2)"),
        ("attack_at_army: 3", "attack_at_army: 4"),
        ("attack_interval_ticks: 600", "attack_interval_ticks: 601"),
        ("attack_spread: 60.0", "attack_spread: 61.0"),
        ("id: \"mvp\"", "id: \"mvp2\""),
        ("default: \"mvp\"", "default: \"mvp2\""),
    ];
    let shipped = shipped_strategies();
    for (i, (from, to)) in edits.iter().enumerate() {
        assert!(shipped.contains(from), "anchor `{from}` is not in strategies.ron");
        let text = shipped.replace(from, to);
        // Some edits (an id rename) only load when the default follows; either
        // way, what matters is that the content that *does* load hashes
        // differently — an edit the fingerprint cannot see is a false accept.
        let text = if *from == "id: \"mvp\"" {
            text.replace("default: \"mvp\"", "default: \"mvp2\"")
        } else if *from == "default: \"mvp\"" {
            text.replace("id: \"mvp\"", "id: \"mvp2\"")
        } else {
            text
        };
        let c = load_with_strategies(&format!("fp{i}"), &text)
            .unwrap_or_else(|e| panic!("`{from}` -> `{to}` must still load: {e}"));
        assert_ne!(
            c.fingerprint(),
            base,
            "editing `{from}` -> `{to}` left the fingerprint unchanged: a stale \
             log would still look valid"
        );
    }
    // `building` cannot be edited alone in the shipped one-barracks entry (the
    // army would stop being producible), so it is isolated with a pair that
    // differs in nothing else: same army, same timings, a second opening that
    // names a different building.
    let pair = |b: &str| {
        strategies_file(
            "s",
            &[strategy(
                "s",
                &[("foundry", 300, 130.0), (b, 400, 130.0)],
                &[("sentinel", 2)],
            )],
        )
    };
    let vats = load_with_strategies("fp-vats", &pair("gene_vats")).expect("loads");
    let spire = load_with_strategies("fp-spire", &pair("aether_spire")).expect("loads");
    assert_ne!(
        vats.fingerprint(),
        spire.fingerprint(),
        "the barracks a strategy opens is not in the fingerprint"
    );

    // And an untouched copy hashes the same, so the test above is not merely
    // detecting "a different directory".
    assert_eq!(
        load_with_strategies("fp-same", &shipped).unwrap().fingerprint(),
        base,
        "the fingerprint depends on something other than the content"
    );
}

// ---- the promotion changed no behaviour -------------------------------------

/// The promotion is a refactor, not a re-tune: with the one-barracks default,
/// the sim produces **bit-identically** the world it produced before B1 — the
/// same per-tick state hash, from the same seed, including the RNG stream shape
/// (one draw per placement, taken only when the placement happens).
///
/// The two hashes below were taken from the pre-B1 build (`mvp_ai` in
/// `units.ron`) at tick 3_000 of the standard AI-vs-AI fixture.
#[test]
fn the_one_barracks_default_replays_exactly_as_it_did_before_b1() {
    for (seed, golden) in [(4u64, 0xa71f_64ca_d502_03e9u64), (11, 0x5b39_8ee4_7854_23dc)] {
        let mut app = ai_vs_ai_on(content(), seed);
        tick(&mut app, 3_000);
        let got = onus::sim::state_hash(app.world_mut());
        assert_eq!(
            got, golden,
            "seed {seed}: the promoted script plays a different match \
             (got {got:#018x}, pre-B1 {golden:#018x})"
        );
        // ...and it still opens exactly one barracks: the Foundry.
        let placed = placements(&app, Faction::A);
        assert_eq!(placed.len(), 1, "the MVP opener placed {} barracks", placed.len());
        let foundry = content().building_index("foundry").unwrap();
        assert_eq!(placed[0].0, foundry);
        let hq = Vec2::new(-750.0, 0.0);
        assert!(
            (placed[0].1.distance(hq) - 130.0).abs() < 0.01,
            "the barracks is {} from the HQ, not 130",
            placed[0].1.distance(hq)
        );
    }
}
