//! Critic probes for BALANCE_PLAN B1, checkbox 1 — "promote `mvp_ai` into a
//! `strategies` set in data, extended so a strategy may open multiple barracks".
//!
//! Written against the spec only. Three families:
//!   (a) load-time refusal — every broken strategy in the set is refused, by a
//!       message that names *which* strategy, and the check reaches entries
//!       that are not the default;
//!   (b) the RNG stream — a draw is taken only when a placement happens, so the
//!       commander's randomness cannot depend on its economy or its clock (a
//!       budget-dependent draw would desync a replay against a different
//!       starting stockpile);
//!   (c) multi-barracks behaviour — each opening goes up exactly once, the army
//!       trains across domains without stalling, the two sides stay
//!       independent, and the same (strategy, seed) replays bit-identically.

use std::path::{Path, PathBuf};

use bevy::prelude::*;

use onus::sim::combat::{Casualties, Health};
use onus::sim::content::Content;
use onus::sim::economy::{Building, ProductionQueue, Stockpiles, UnitDefIdx};
use onus::sim::spatial::Faction;
use onus::sim::{
    AiAction, AiCommanders, AiJournal, CommandQueue, Position, RateReport, ResourceNode,
    StateHashLog,
};

// ---- harness ---------------------------------------------------------------

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data")
}

fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/critic_b1")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// A content dir carrying the shipped `units.ron`/`resources.ron` and the given
/// `strategies.ron`.
fn dir_with_strategies(name: &str, strategies: &str) -> PathBuf {
    let dir = scratch(name);
    for f in ["units.ron", "resources.ron"] {
        std::fs::copy(data_dir().join(f), dir.join(f)).expect("copy shipped file");
    }
    std::fs::write(dir.join("strategies.ron"), strategies).expect("write strategies.ron");
    dir
}

fn try_strategies(name: &str, strategies: &str) -> Result<Content, String> {
    Content::load_from_dir(&dir_with_strategies(name, strategies)).map_err(|e| e.to_string())
}

/// The shipped MVP entry, verbatim in shape.
const MVP: &str = r#"(
    id: "mvp",
    think_interval_ticks: 30,
    worker_target: 6,
    barracks: [ (building: "foundry", at_tick: 300, offset: 130.0) ],
    army: [ (unit: "sentinel", count: 2), (unit: "bulwark", count: 1) ],
    attack_at_army: 3,
    attack_interval_ticks: 600,
    attack_spread: 60.0,
    queue_depth: 1,
)"#;

/// A second, perfectly good entry. The refusal battery breaks *this* one, so
/// every refusal is proof the validator reaches a non-default strategy.
const PROBE: &str = r#"(
    id: "probe",
    think_interval_ticks: 30,
    worker_target: 6,
    barracks: [ (building: "foundry", at_tick: 300, offset: 130.0) ],
    army: [ (unit: "sentinel", count: 2) ],
    attack_at_army: 3,
    attack_interval_ticks: 600,
    attack_spread: 60.0,
    queue_depth: 1,
)"#;

fn set(default: &str, entries: &[&str]) -> String {
    format!(
        "(\n    default: \"{default}\",\n    strategies: [\n{}\n    ],\n)",
        entries.join(",\n")
    )
}

fn edited(from: &str, to: &str) -> String {
    assert!(PROBE.contains(from), "probe fixture drifted: `{from}` missing");
    PROBE.replace(from, to)
}

// ============================================================================
// (a) load-time refusal
// ============================================================================

/// Every way a strategy can be unrunnable is refused at load, and the message
/// names the strategy it is talking about. The break is always in `probe`, the
/// *second* entry: a set whose non-default entries are unchecked is content the
/// sim cannot run, admitted.
#[test]
fn every_broken_non_default_strategy_is_refused_by_name() {
    let cases: Vec<(&str, String)> = vec![
        ("unknown unit", edited("\"sentinel\"", "\"nonesuch\"")),
        ("unknown building", edited("\"foundry\"", "\"nonesuch\"")),
        ("victory building as barracks", edited("\"foundry\"", "\"hq\"")),
        // Producible by `gene_vats`, which this strategy does not open.
        ("unit from a barracks it never opens", edited("\"sentinel\"", "\"ripper\"")),
        ("count 0", edited("count: 2", "count: 0")),
        ("empty army", edited("army: [ (unit: \"sentinel\", count: 2) ]", "army: []")),
        (
            "no barracks at all",
            edited(
                "barracks: [ (building: \"foundry\", at_tick: 300, offset: 130.0) ]",
                "barracks: []",
            ),
        ),
        (
            "army counts overflow u32",
            edited(
                "army: [ (unit: \"sentinel\", count: 2) ]",
                "army: [ (unit: \"sentinel\", count: 4294967295), \
                 (unit: \"bulwark\", count: 2) ]",
            ),
        ),
        ("think_interval_ticks 0", edited("think_interval_ticks: 30", "think_interval_ticks: 0")),
        (
            "attack_interval_ticks 0",
            edited("attack_interval_ticks: 600", "attack_interval_ticks: 0"),
        ),
        ("worker_target 0", edited("worker_target: 6", "worker_target: 0")),
        ("attack_at_army 0", edited("attack_at_army: 3", "attack_at_army: 0")),
        ("offset 0", edited("offset: 130.0", "offset: 0.0")),
        ("offset negative", edited("offset: 130.0", "offset: -130.0")),
        ("attack_spread negative", edited("attack_spread: 60.0", "attack_spread: -1.0")),
    ];

    let mut admitted: Vec<&str> = Vec::new();
    let mut anonymous: Vec<(&str, String)> = Vec::new();
    for (i, (what, entry)) in cases.iter().enumerate() {
        let ron = set("mvp", &[MVP, entry]);
        match try_strategies(&format!("refuse{i}"), &ron) {
            Ok(_) => admitted.push(what),
            Err(e) => {
                if !e.contains("probe") {
                    anonymous.push((what, e));
                }
            }
        }
    }
    assert!(
        admitted.is_empty(),
        "these unrunnable non-default strategies loaded: {admitted:#?}"
    );
    assert!(
        anonymous.is_empty(),
        "these refusals never name the offending strategy `probe`: {anonymous:#?}"
    );
}

#[test]
fn a_duplicate_strategy_id_is_refused() {
    let twice = PROBE.to_string();
    let ron = set("mvp", &[MVP, &twice, &twice]);
    let e = try_strategies("dup-id", &ron).expect_err("two strategies share an id");
    assert!(e.contains("probe"), "the refusal does not name the id: {e}");
}

#[test]
fn a_default_naming_no_strategy_is_refused() {
    let ron = set("nonesuch", &[MVP]);
    let e = try_strategies("bad-default", &ron).expect_err("the default names nothing");
    assert!(e.contains("nonesuch"), "the refusal does not name the default: {e}");
}

#[test]
fn an_empty_strategy_set_is_refused() {
    let ron = "(\n    default: \"mvp\",\n    strategies: [],\n)".to_string();
    try_strategies("empty-set", &ron).expect_err("a set with no strategies loaded");
}

/// The positive control: multi-barracks, multi-domain content is *admitted*.
/// (A validator that refused everything would pass the battery above.)
#[test]
fn a_multi_barracks_multi_domain_strategy_loads() {
    let c = try_strategies("positive", &set("mvp", &[MVP, MULTI]))
        .expect("a strategy opening two barracks and spanning two domains must load");
    let s = c.strategy("multi").expect("the multi strategy is in the set");
    assert_eq!(s.barracks.len(), 2, "the opening list did not survive the load");
    assert_eq!(c.ai.id, "mvp", "the default resolved to the wrong entry");
}

/// Two barracks, two domains: Foundry -> Sentinel, Gene-Vats -> Ripper.
const MULTI: &str = r#"(
    id: "multi",
    think_interval_ticks: 30,
    worker_target: 6,
    barracks: [
        (building: "foundry",   at_tick: 300, offset: 130.0),
        (building: "gene_vats", at_tick: 600, offset: 160.0),
    ],
    army: [ (unit: "sentinel", count: 1), (unit: "ripper", count: 1) ],
    attack_at_army: 3,
    attack_interval_ticks: 600,
    attack_spread: 60.0,
    queue_depth: 1,
)"#;

// ============================================================================
// the match fixture
// ============================================================================

fn step(app: &mut App) {
    let dt = app.world().resource::<Time<Fixed>>().timestep();
    app.world_mut().resource_mut::<Time<Fixed>>().advance_by(dt);
    app.update();
}

fn ai_vs_ai(c: Content, seed: u64) -> App {
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
            .expect("hq");
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
                let idx = c.unit_index("worker").expect("worker");
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
    app.insert_resource(StateHashLog::default());
    app
}

fn hq_of(faction: Faction) -> Vec2 {
    match faction {
        Faction::A => Vec2::new(-750.0, 0.0),
        _ => Vec2::new(750.0, 0.0),
    }
}

fn journal(app: &App, f: Faction) -> Vec<(u32, AiAction)> {
    app.world()
        .resource::<AiJournal>()
        .0
        .iter()
        .filter(|(_, who, _)| *who == f)
        .map(|(t, _, a)| (*t, *a))
        .collect()
}

/// The unit direction of the first barracks this faction placed — i.e. the
/// first number its RNG ever produced, read off the world.
fn first_place_dir(app: &App, f: Faction) -> Option<Vec2> {
    journal(app, f).into_iter().find_map(|(_, a)| match a {
        AiAction::PlaceBarracks { pos, .. } => Some((pos - hq_of(f)).normalize()),
        _ => None,
    })
}

fn run(c: Content, seed: u64, ticks: u32) -> App {
    let mut app = ai_vs_ai(c, seed);
    for _ in 0..ticks {
        step(&mut app);
    }
    app
}

fn content_with(name: &str, edits: &[(&str, &str, &str)], strategies: &str) -> Content {
    let dir = scratch(name);
    for f in ["units.ron", "resources.ron"] {
        let mut text = std::fs::read_to_string(data_dir().join(f)).expect("read shipped");
        for (file, from, to) in edits {
            if *file == f {
                assert!(text.contains(from), "anchor `{from}` missing from {f}");
                text = text.replace(from, to);
            }
        }
        std::fs::write(dir.join(f), text).expect("write");
    }
    std::fs::write(dir.join("strategies.ron"), strategies).expect("write strategies.ron");
    Content::load_from_dir(Path::new(&dir)).expect("the edited content must load")
}

// ============================================================================
// (b) the RNG stream
// ============================================================================

/// **A placement considered but not afforded must not consume a draw.** If it
/// did, the commander's randomness would be a function of its economy, and the
/// same seed would replay differently against a different starting stockpile.
/// Two matches, identical in every way but `starting_alloy`, must place their
/// first barracks in the *same direction* — only later or earlier.
#[test]
fn a_placement_the_commander_cannot_afford_consumes_no_randomness() {
    let strategies = set("mvp", &[MVP]);
    let rich = content_with(
        "rng-rich",
        &[("resources.ron", "starting_alloy: 300", "starting_alloy: 3000")],
        &strategies,
    );
    let poor = content_with(
        "rng-poor",
        &[("resources.ron", "starting_alloy: 300", "starting_alloy: 20")],
        &strategies,
    );
    let rich = run(rich, 5, 3_000);
    let poor = run(poor, 5, 3_000);
    for f in [Faction::A, Faction::B] {
        let r = first_place_dir(&rich, f).expect("the rich commander never placed a barracks");
        let p = first_place_dir(&poor, f).expect("the poor commander never placed a barracks");
        assert!(
            (r - p).length() < 1e-4,
            "{f:?} placed toward {r:?} when rich and {p:?} when poor: the RNG stream \
             stepped on a placement it could not pay for, so the AI's randomness \
             depends on its economy"
        );
    }
}

/// The same property against the clock rather than the wallet: a placement not
/// yet at its `at_tick` must not step the stream either.
#[test]
fn a_placement_before_its_tick_consumes_no_randomness() {
    let early = content_with("rng-early", &[], &set("mvp", &[MVP]));
    let late = content_with(
        "rng-late",
        &[],
        &set("mvp", &[&MVP.replace("at_tick: 300", "at_tick: 1200")]),
    );
    let early = run(early, 5, 3_000);
    let late = run(late, 5, 3_000);
    for f in [Faction::A, Faction::B] {
        let e = first_place_dir(&early, f).expect("no barracks (early)");
        let l = first_place_dir(&late, f).expect("no barracks (late)");
        assert!(
            (e - l).length() < 1e-4,
            "{f:?} placed toward {e:?} at tick 300 and {l:?} at tick 1200: the stream \
             stepped on a decision that placed nothing"
        );
    }
}

// ============================================================================
// (c) multi-barracks behaviour
// ============================================================================

/// Every opening goes up, exactly once, and the building it named is really
/// there. A second journal entry for the same opening means the first order was
/// rejected — which is what committing the same Alloy twice in one decision
/// looks like from outside.
#[test]
fn every_opening_is_placed_exactly_once_and_the_building_exists() {
    let c = content_with("multi-place", &[], &set("multi", &[MVP, MULTI]));
    let foundry = c.building_index("foundry").expect("foundry");
    let vats = c.building_index("gene_vats").expect("gene_vats");
    let mut app = run(c, 3, 6_000);

    for f in [Faction::A, Faction::B] {
        let mut placed: Vec<usize> = journal(&app, f)
            .into_iter()
            .filter_map(|(_, a)| match a {
                AiAction::PlaceBarracks { building, .. } => Some(building),
                _ => None,
            })
            .collect();
        placed.sort_unstable();
        let mut want = vec![foundry, vats];
        want.sort_unstable();
        assert_eq!(
            placed, want,
            "{f:?} did not place each of its two openings exactly once"
        );
    }
    let mut q = app.world_mut().query::<(&Building, &Faction)>();
    for f in [Faction::A, Faction::B] {
        for def in [foundry, vats] {
            let n = q
                .iter(app.world())
                .filter(|(b, who)| b.def == def && **who == f)
                .count();
            assert_eq!(n, 1, "{f:?} owns {n} of building {def}, not 1");
        }
    }
}

/// An army that spans domains actually gets built: the commander trains from
/// *both* of its barracks. A cursor that stalls on a unit whose barracks is not
/// up yet, and never recovers, fails here.
#[test]
fn a_multi_domain_army_trains_from_both_barracks() {
    let c = content_with("multi-army", &[], &set("multi", &[MVP, MULTI]));
    let sentinel = c.unit_index("sentinel").expect("sentinel");
    let ripper = c.unit_index("ripper").expect("ripper");
    let app = run(c, 3, 8_000);
    for f in [Faction::A, Faction::B] {
        let trained: Vec<usize> = journal(&app, f)
            .into_iter()
            .filter_map(|(_, a)| match a {
                AiAction::TrainArmy { unit } => Some(unit),
                _ => None,
            })
            .collect();
        assert!(
            trained.contains(&sentinel),
            "{f:?} never trained the Foundry half of its army"
        );
        assert!(
            trained.contains(&ripper),
            "{f:?} never trained the Gene-Vats half of its army: the build order \
             stalled on the second domain"
        );
    }
}

/// Same (strategy, seed) replays bit-identically — the B1 critic probe, run on a
/// multi-barracks strategy so the extra draws are in the stream.
#[test]
fn the_same_strategy_and_seed_replay_bit_identically() {
    let strategies = set("multi", &[MVP, MULTI]);
    let a = run(content_with("det-a", &[], &strategies), 21, 5_000);
    let b = run(content_with("det-b", &[], &strategies), 21, 5_000);
    let ha = a.world().resource::<StateHashLog>().0.clone();
    let hb = b.world().resource::<StateHashLog>().0.clone();
    assert_eq!(ha.len(), 5_000, "the sim did not record one hash per tick");
    assert_eq!(
        ha.iter().zip(hb.iter()).position(|(x, y)| x != y),
        None,
        "two runs of the same strategy and seed diverged"
    );
    assert_eq!(
        a.world().resource::<AiJournal>().0,
        b.world().resource::<AiJournal>().0,
        "the AI journals differ across two identical runs"
    );
}

/// The two sides stay independent: each commander places only near its own HQ,
/// its journal names only its own actions, and its stream is its own (the two
/// do not draw the same angles).
#[test]
fn the_two_sides_stay_independent() {
    let c = content_with("sides", &[], &set("multi", &[MVP, MULTI]));
    let app = run(c, 8, 5_000);
    let a = first_place_dir(&app, Faction::A).expect("A placed nothing");
    let b = first_place_dir(&app, Faction::B).expect("B placed nothing");
    assert!(
        (a - b).length() > 1e-4,
        "both commanders drew the same first angle {a:?}: the two streams are not \
         independently seeded"
    );
    for f in [Faction::A, Faction::B] {
        for (_, act) in journal(&app, f) {
            if let AiAction::PlaceBarracks { pos, .. } = act {
                let d = (pos - hq_of(f)).length();
                assert!(d < 400.0, "{f:?} placed a barracks {d} from its own HQ");
            }
        }
    }
}

// ============================================================================
// (d) the fingerprint covers the whole set, not just the default
// ============================================================================

#[test]
fn the_fingerprint_moves_for_any_edit_anywhere_in_the_set() {
    let base = content_with("fp-base", &[], &set("mvp", &[MVP, MULTI])).fingerprint();
    let cases: Vec<(&str, String)> = vec![
        // A field of the *non-default* entry.
        (
            "a non-default strategy's at_tick",
            set("mvp", &[MVP, &MULTI.replace("at_tick: 600", "at_tick: 601")]),
        ),
        (
            "a non-default strategy's offset",
            set("mvp", &[MVP, &MULTI.replace("offset: 160.0", "offset: 161.0")]),
        ),
        (
            "a non-default strategy's army count",
            set("mvp", &[MVP, &MULTI.replace("(unit: \"ripper\", count: 1)", "(unit: \"ripper\", count: 2)")]),
        ),
        (
            "a non-default strategy's second opening",
            set("mvp", &[MVP, &MULTI.replace("\"gene_vats\"", "\"aether_spire\"").replace("(unit: \"ripper\", count: 1)", "(unit: \"arclight\", count: 1)")]),
        ),
        // Which entry is the default.
        ("the default id", set("multi", &[MVP, MULTI])),
        // The order of the set.
        ("the order of the set", set("mvp", &[MULTI, MVP])),
    ];
    let mut blind: Vec<&str> = Vec::new();
    for (i, (what, ron)) in cases.iter().enumerate() {
        let c = Content::load_from_dir(&dir_with_strategies(&format!("fp{i}"), ron))
            .expect("the fixture must load");
        if c.fingerprint() == base {
            blind.push(what);
        }
    }
    assert!(
        blind.is_empty(),
        "`Content::fingerprint` cannot see these edits, so a stale log replays as \
         valid against changed content: {blind:#?}"
    );
}
