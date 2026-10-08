//! Adversarial critic probes for **B3.5 AC0b** — parallel production.
//!
//! Every probe here is one that *should pass per the spec*:
//!   1. only the "repeats a building" refusal is gone; every other refusal stands;
//!   2. N openings of a building produce exactly N buildings, never N+1;
//!   3. an RNG draw happens only when a placement is actually emitted;
//!   4. the army step considers every owned barracks that can make the unit,
//!      one order per decision, on a target rule no query order can reach;
//!   5. neutrality on the shipped data;
//!   6. the measured 9 / 27 / 9 throughput numbers.
//!
//! Written from the spec, independently of `tests/b35_parallel.rs`.

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

/// A whole strategies file with exactly the entries given, spelled out field by
/// field so a probe can break any one of them.
#[allow(clippy::too_many_arguments)]
fn entry(
    id: &str,
    think: u32,
    worker_target: u32,
    barracks: &[(&str, u32, f32)],
    army: &[(&str, u32)],
    attack_at_army: u32,
    attack_interval: u32,
    spread: f32,
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
    let mut s = String::new();
    s.push_str("        (\n");
    s.push_str(&format!("            id: \"{id}\",\n"));
    s.push_str(&format!("            think_interval_ticks: {think},\n"));
    s.push_str(&format!("            worker_target: {worker_target},\n"));
    s.push_str(&format!("            barracks: [\n{}\n            ],\n", openings.join("\n")));
    s.push_str(&format!("            army: [\n{}\n            ],\n", army.join("\n")));
    s.push_str(&format!("            attack_at_army: {attack_at_army},\n"));
    s.push_str(&format!("            attack_interval_ticks: {attack_interval},\n"));
    s.push_str(&format!("            attack_spread: {spread:?},\n"));
    s.push_str(&format!("            queue_depth: {queue_depth},\n"));
    s.push_str("        ),");
    s
}

/// A sane entry with the openings/army given.
fn ok_entry(id: &str, barracks: &[(&str, u32, f32)], army: &[(&str, u32)]) -> String {
    entry(id, 30, 6, barracks, army, 3, 600, 60.0, 1)
}

fn file_of(default: &str, entries: &[String]) -> String {
    format!(
        "(\n    default: \"{default}\",\n    strategies: [\n{}\n    ],\n)\n",
        entries.join("\n")
    )
}

/// Load a content dir whose `strategies.ron` is `text` and whose other files are
/// the shipped ones.
fn load(name: &str, text: &str) -> Result<Content, String> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/critic_b35_ac0b")
        .join(name);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    for file in ["units.ron", "resources.ron"] {
        std::fs::copy(data_dir().join(file), dir.join(file)).expect("copy");
    }
    std::fs::write(dir.join("strategies.ron"), text).expect("write");
    Content::load_from_dir(&dir).map_err(|e| e.to_string())
}

fn loaded(name: &str, text: &str) -> Content {
    load(name, text).expect("content loads")
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

/// The standard headless match shape. `extra` runs per side after the starting
/// workers, with the world in hand.
fn app_with(
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

/// A match with a commander only on side A — so nothing ever attacks and no
/// building is ever destroyed, which is what the conservation probes need.
fn solo(c: Content, seed: u64, id: &str, alloy: u32, nodes: bool) -> App {
    let commanders =
        AiCommanders::matchup(&c, seed, &[(Faction::A, id)]).expect("strategy is named");
    app_with(c, commanders, alloy, nodes, |_, _, _| {})
}

fn matchup(c: Content, seed: u64, a: &str, b: &str, alloy: u32, nodes: bool) -> App {
    let commanders = AiCommanders::matchup(&c, seed, &[(Faction::A, a), (Faction::B, b)])
        .expect("both strategies are named");
    app_with(c, commanders, alloy, nodes, |_, _, _| {})
}

fn journal(app: &App, f: Faction) -> Vec<(u32, AiAction)> {
    app.world().resource::<AiJournal>().for_faction(f)
}

fn placements(app: &App, f: Faction) -> Vec<(u32, usize, Vec2)> {
    journal(app, f)
        .into_iter()
        .filter_map(|(t, a)| match a {
            AiAction::PlaceBarracks { building, pos } => Some((t, building, pos)),
            _ => None,
        })
        .collect()
}

fn trains(app: &App, f: Faction) -> Vec<(u32, usize)> {
    journal(app, f)
        .into_iter()
        .filter_map(|(t, a)| match a {
            AiAction::TrainArmy { unit } => Some((t, unit)),
            _ => None,
        })
        .collect()
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

/// (def, queue length) of every non-victory building one side owns, ascending
/// by def then queue.
fn army_buildings(app: &mut App, f: Faction) -> Vec<(usize, usize)> {
    let victory = victory_defs(app);
    let mut q = app
        .world_mut()
        .query::<(&Building, &Faction, &ProductionQueue)>();
    let mut out: Vec<(usize, usize)> = q
        .iter(app.world())
        .filter(|(b, who, _)| **who == f && !victory.contains(&b.def))
        .map(|(b, _, p)| (b.def, p.items.len()))
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

fn hash_trace(app: &mut App, samples: u32, every: u32) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for _ in 0..samples {
        tick(app, every);
        h ^= onus::sim::state_hash(app.world_mut());
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Entity-label-stable digest of one side's whole action trace.
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

// =============================================================================
// 1. Only the duplicate-building clause was removed
// =============================================================================

/// Re-derived from the schema, independently of the implementer's list: every
/// refusal that is *not* "opens the same building twice" must still refuse, by
/// name. If a deletion took collateral, one of these loads.
#[test]
fn critic_every_other_refusal_still_refuses() {
    let one = |e: String| file_of("s", &[e]);
    let cases: Vec<(&str, String)> = vec![
        (
            "unknown building",
            one(ok_entry("s", &[("nonesuch", 300, 130.0)], &[("sentinel", 1)])),
        ),
        (
            "the victory building as a barracks",
            one(ok_entry("s", &[("hq", 300, 130.0)], &[("worker", 1)])),
        ),
        (
            "a unit no opened barracks can produce",
            one(ok_entry("s", &[("foundry", 300, 130.0)], &[("ripper", 1)])),
        ),
        (
            "a unit no opened barracks can produce, repeats and all",
            one(ok_entry(
                "s",
                &[("foundry", 300, 130.0), ("foundry", 600, 140.0)],
                &[("ripper", 1)],
            )),
        ),
        (
            "an unknown unit",
            one(ok_entry("s", &[("foundry", 300, 130.0)], &[("nosuchunit", 1)])),
        ),
        (
            "no barracks at all",
            one(ok_entry("s", &[], &[("sentinel", 1)])),
        ),
        (
            "an empty army",
            one(ok_entry("s", &[("foundry", 300, 130.0)], &[])),
        ),
        (
            "a zero offset",
            one(ok_entry("s", &[("foundry", 300, 0.0)], &[("sentinel", 1)])),
        ),
        (
            "a zero offset on the *second* opening of the same building",
            one(ok_entry(
                "s",
                &[("foundry", 300, 130.0), ("foundry", 600, 0.0)],
                &[("sentinel", 1)],
            )),
        ),
        (
            "a negative offset",
            one(ok_entry("s", &[("foundry", 300, -5.0)], &[("sentinel", 1)])),
        ),
        (
            "an army count of zero",
            one(ok_entry("s", &[("foundry", 300, 130.0)], &[("sentinel", 0)])),
        ),
        (
            "queue_depth 0",
            one(entry(
                "s",
                30,
                6,
                &[("foundry", 300, 130.0)],
                &[("sentinel", 1)],
                3,
                600,
                60.0,
                0,
            )),
        ),
        (
            "queue_depth 0 with repeated openings",
            one(entry(
                "s",
                30,
                6,
                &[("foundry", 300, 130.0), ("foundry", 600, 140.0)],
                &[("sentinel", 1)],
                3,
                600,
                60.0,
                0,
            )),
        ),
        (
            "think_interval_ticks 0",
            one(entry(
                "s",
                0,
                6,
                &[("foundry", 300, 130.0)],
                &[("sentinel", 1)],
                3,
                600,
                60.0,
                1,
            )),
        ),
        (
            "attack_interval_ticks 0",
            one(entry(
                "s",
                30,
                6,
                &[("foundry", 300, 130.0)],
                &[("sentinel", 1)],
                3,
                0,
                60.0,
                1,
            )),
        ),
        (
            "worker_target 0",
            one(entry(
                "s",
                30,
                0,
                &[("foundry", 300, 130.0)],
                &[("sentinel", 1)],
                3,
                600,
                60.0,
                1,
            )),
        ),
        (
            "attack_at_army 0",
            one(entry(
                "s",
                30,
                6,
                &[("foundry", 300, 130.0)],
                &[("sentinel", 1)],
                0,
                600,
                60.0,
                1,
            )),
        ),
        (
            "a negative attack_spread",
            one(entry(
                "s",
                30,
                6,
                &[("foundry", 300, 130.0)],
                &[("sentinel", 1)],
                3,
                600,
                -1.0,
                1,
            )),
        ),
        (
            "army counts that overflow u32",
            one(ok_entry(
                "s",
                &[("foundry", 300, 130.0)],
                &[("sentinel", u32::MAX), ("bulwark", 2)],
            )),
        ),
        (
            "an empty strategy set",
            file_of("s", &[]),
        ),
        (
            "a default naming no strategy",
            file_of(
                "ghost",
                &[ok_entry("s", &[("foundry", 300, 130.0)], &[("sentinel", 1)])],
            ),
        ),
        (
            "a broken *non-default* strategy, repeats and all",
            file_of(
                "good",
                &[
                    ok_entry("good", &[("foundry", 300, 130.0)], &[("sentinel", 1)]),
                    ok_entry(
                        "bad",
                        &[("foundry", 300, 130.0), ("foundry", 600, 140.0)],
                        &[("arclight", 1)],
                    ),
                ],
            ),
        ),
    ];
    for (what, text) in cases {
        let name: String = what.chars().filter(|c| c.is_alphanumeric()).collect();
        assert!(
            load(&name, &text).is_err(),
            "content with {what} must still be refused at load"
        );
    }
}

/// The one clause that *is* gone: repeats load, whatever their shape.
#[test]
fn critic_repeated_openings_are_accepted() {
    for (name, openings) in [
        (
            "twice",
            vec![("foundry", 300u32, 130.0f32), ("foundry", 600, 160.0)],
        ),
        (
            "thrice",
            vec![
                ("foundry", 300, 130.0),
                ("foundry", 600, 160.0),
                ("foundry", 900, 190.0),
            ],
        ),
        (
            "two identical entries",
            vec![("foundry", 300, 130.0), ("foundry", 300, 130.0)],
        ),
        (
            "interleaved with another building",
            vec![
                ("foundry", 300, 130.0),
                ("gene_vats", 400, 140.0),
                ("foundry", 500, 150.0),
            ],
        ),
    ] {
        let text = file_of(
            "s",
            &[ok_entry("s", &openings, &[("sentinel", 1)])],
        );
        assert!(
            load(&format!("accept_{name}"), &text).is_ok(),
            "repeated openings ({name}) must load"
        );
    }
}

// =============================================================================
// 2. The counting: N openings -> exactly N buildings
// =============================================================================

fn counting_case(
    name: &str,
    openings: &[(&str, u32, f32)],
    army: &[(&str, u32)],
    alloy: u32,
    ticks: u32,
) -> App {
    let text = file_of("s", &[ok_entry("s", openings, army)]);
    let c = loaded(name, &text);
    let mut app = solo(c, 0xC0FFEE, "s", alloy, false);
    tick(&mut app, ticks);
    app
}

#[test]
fn critic_three_openings_place_exactly_three() {
    let mut app = counting_case(
        "count3",
        &[
            ("foundry", 0, 100.0),
            ("foundry", 0, 140.0),
            ("foundry", 0, 180.0),
        ],
        &[("sentinel", 1)],
        100_000,
        3_000,
    );
    let p = placements(&app, Faction::A);
    assert_eq!(p.len(), 3, "three openings, three placements: {p:?}");
    assert_eq!(
        army_buildings(&mut app, Faction::A).len(),
        3,
        "three buildings stand, never a fourth"
    );
}

#[test]
fn critic_two_of_one_and_one_of_another_place_two_and_one() {
    let mut app = counting_case(
        "count21",
        &[
            ("foundry", 0, 100.0),
            ("gene_vats", 0, 140.0),
            ("foundry", 0, 180.0),
        ],
        &[("sentinel", 1), ("ripper", 1)],
        100_000,
        3_000,
    );
    let foundry = {
        let c = app.world().resource::<Content>();
        c.building_index("foundry").unwrap()
    };
    let vats = {
        let c = app.world().resource::<Content>();
        c.building_index("gene_vats").unwrap()
    };
    let built = army_buildings(&mut app, Faction::A);
    assert_eq!(
        built.iter().filter(|(d, _)| *d == foundry).count(),
        2,
        "two foundry openings, two foundries: {built:?}"
    );
    assert_eq!(
        built.iter().filter(|(d, _)| *d == vats).count(),
        1,
        "one gene_vats opening, one gene_vats: {built:?}"
    );
    assert_eq!(built.len(), 3);
}

#[test]
fn critic_openings_respect_their_own_at_ticks() {
    let mut app = counting_case(
        "counttimed",
        &[
            ("foundry", 0, 100.0),
            ("foundry", 600, 140.0),
            ("foundry", 1200, 180.0),
        ],
        &[("sentinel", 1)],
        100_000,
        3_000,
    );
    let p = placements(&app, Faction::A);
    assert_eq!(p.len(), 3, "still exactly three: {p:?}");
    let mut ticks: Vec<u32> = p.iter().map(|(t, _, _)| *t).collect();
    ticks.sort_unstable();
    assert!(ticks[0] < 600, "the at_tick-0 opening goes up first");
    assert!(
        ticks[1] >= 600 && ticks[1] < 1200,
        "the second waits for its own at_tick: {ticks:?}"
    );
    assert!(ticks[2] >= 1200, "the third waits for its own: {ticks:?}");
    assert_eq!(army_buildings(&mut app, Faction::A).len(), 3);
}

/// A commander that can only ever afford N-1 places N-1 — never N, never a
/// refused order the sim throws away. No resource nodes, so the starting Alloy
/// is all it will ever have.
#[test]
fn critic_openings_it_cannot_afford_are_not_placed() {
    // 3 foundries want 450; give 310 (one foundry at 150, then a worker at 10,
    // then a second foundry at 150 = 310 exactly), so the third never lands.
    let text = file_of(
        "s",
        &[ok_entry(
            "s",
            &[
                ("foundry", 0, 100.0),
                ("foundry", 0, 140.0),
                ("foundry", 0, 180.0),
            ],
            &[("sentinel", 1)],
        )],
    );
    let c = loaded("poor3", &text);
    let mut app = solo(c, 7, "s", 310, false);
    tick(&mut app, 2_000);
    let p = placements(&app, Faction::A);
    assert!(
        p.len() <= 2,
        "310 Alloy cannot buy three 150-Alloy foundries: {p:?}"
    );
    assert_eq!(
        army_buildings(&mut app, Faction::A).len(),
        p.len(),
        "every placement the commander *ordered* actually stands — no order was \
         emitted that the sim then refused"
    );
}

/// Conservation, the strong form: over a long rich match with three barracks,
/// every journal order was really applied. A double-committed budget would show
/// up as an order the sim refused.
#[test]
fn critic_every_order_is_applied_no_budget_is_committed_twice() {
    let text = file_of(
        "s",
        &[entry(
            "s",
            15,
            6,
            &[
                ("foundry", 0, 100.0),
                ("foundry", 0, 140.0),
                ("foundry", 0, 180.0),
            ],
            &[("sentinel", 1)],
            10_000, // never attacks, so nothing dies
            600,
            60.0,
            2,
        )],
    );
    let c = loaded("conserve", &text);
    let mut app = solo(c, 99, "s", 100_000, false);
    tick(&mut app, 6_000);
    let placed = placements(&app, Faction::A).len();
    let ordered = trains(&app, Faction::A).len();
    let standing = army_buildings(&mut app, Faction::A);
    assert_eq!(placed, standing.len(), "every placement order stands");
    let queued: usize = standing.iter().map(|(_, q)| q).sum();
    let built = combat_units(&mut app, Faction::A);
    assert_eq!(
        ordered,
        built + queued,
        "every TrainArmy order is either a unit on the field or still in a queue \
         (ordered={ordered}, built={built}, queued={queued})"
    );
}

// =============================================================================
// 3. An RNG draw happens only when a placement is emitted
// =============================================================================

/// F-044: "a stockpile that buys exactly one Foundry and no Aether Spire" —
/// the shipped Foundry's cost plus 10 (160 at F-033's prices), read from the
/// content so a cost re-tune moves the fixture instead of breaking it.
fn one_foundry_stock() -> u32 {
    let c = Content::load_from_dir(&data_dir()).expect("shipped content");
    let foundry = c.building("foundry").unwrap().alloy_cost;
    let spire = c.building("aether_spire").unwrap().alloy_cost;
    let stock = foundry + 10;
    assert!(
        stock < 2 * foundry && stock < spire,
        "fixture: {stock} must buy one Foundry ({foundry}) and no Aether Spire ({spire})"
    );
    stock
}

/// A rich commander and a poor one running the same script must place their
/// *first* barracks in the same direction: the placements the poor one could
/// not afford consumed no randomness.
#[test]
fn critic_unaffordable_placements_consume_no_randomness() {
    let text = file_of(
        "s",
        &[ok_entry(
            "s",
            &[
                ("foundry", 0, 100.0),
                ("foundry", 0, 100.0),
                ("foundry", 0, 100.0),
            ],
            &[("sentinel", 1)],
        )],
    );
    let c_rich = loaded("rng_rich", &text);
    let c_poor = loaded("rng_poor", &text);
    let mut rich = solo(c_rich, 0xABCDEF, "s", 100_000, false);
    let stock = one_foundry_stock();
    let mut poor = solo(c_poor, 0xABCDEF, "s", stock, false);
    tick(&mut rich, 600);
    tick(&mut poor, 600);
    let pr = placements(&rich, Faction::A);
    let pp = placements(&poor, Faction::A);
    assert_eq!(pr.len(), 3, "the rich commander places all three");
    assert_eq!(pp.len(), 1, "{stock} Alloy buys exactly one foundry: {pp:?}");
    assert_eq!(
        pp[0].2, pr[0].2,
        "the first placement's direction is the same draw in both runs — the two \
         placements the poor commander could not afford must consume no randomness"
    );
}

/// The same rule across *different* buildings: an opening that is due but
/// unaffordable must not eat the draw the next, cheaper opening takes.
#[test]
fn critic_a_skipped_expensive_opening_does_not_eat_the_draw() {
    // The Aether Spire costs more than the Foundry (200 v 150 at F-033). With
    // the Foundry's cost + 10 the spire is never affordable at the first
    // decision; the foundry is (F-044: the stock is read, not pinned).
    let stock = one_foundry_stock();
    let two = file_of(
        "s",
        &[ok_entry(
            "s",
            &[("aether_spire", 0, 100.0), ("foundry", 0, 100.0)],
            &[("sentinel", 1)],
        )],
    );
    let just_foundry = file_of(
        "s",
        &[ok_entry("s", &[("foundry", 0, 100.0)], &[("sentinel", 1)])],
    );
    let mut a = solo(loaded("rng_skip_two", &two), 0x5EED, "s", stock, false);
    let mut b = solo(loaded("rng_skip_one", &just_foundry), 0x5EED, "s", stock, false);
    tick(&mut a, 120);
    tick(&mut b, 120);
    let pa = placements(&a, Faction::A);
    let pb = placements(&b, Faction::A);
    assert_eq!(pb.len(), 1, "the one-opening script places its foundry");
    assert_eq!(
        pa.len(),
        1,
        "the two-opening script can only afford the foundry: {pa:?}"
    );
    assert_eq!(
        pa[0].2, pb[0].2,
        "the unaffordable Aether Spire consumed no randomness, so the foundry \
         lands on the very same draw"
    );
}

// =============================================================================
// 4. The target-barracks rule
// =============================================================================

/// Training spreads over every barracks that can make the unit: with three
/// foundries and ample Alloy, all three carry work and their queues stay within
/// one of each other.
#[test]
fn critic_training_spreads_over_every_barracks() {
    let text = file_of(
        "s",
        &[entry(
            "s",
            15,
            6,
            &[
                ("foundry", 0, 100.0),
                ("foundry", 0, 140.0),
                ("foundry", 0, 180.0),
            ],
            &[("sentinel", 1)],
            10_000,
            600,
            60.0,
            3,
        )],
    );
    let c = loaded("spread", &text);
    let mut app = solo(c, 3, "s", 100_000, false);
    tick(&mut app, 400);
    let qs: Vec<usize> = army_buildings(&mut app, Faction::A)
        .into_iter()
        .map(|(_, q)| q)
        .collect();
    assert_eq!(qs.len(), 3, "three barracks stand");
    assert!(
        qs.iter().all(|q| *q > 0),
        "every barracks is carrying work, not one hammered: {qs:?}"
    );
    let (lo, hi) = (qs.iter().min().unwrap(), qs.iter().max().unwrap());
    assert!(
        hi - lo <= 1,
        "shallowest-queue targeting keeps the lines even: {qs:?}"
    );
}

/// One army order per decision, however many barracks are free and however much
/// Alloy there is.
#[test]
fn critic_one_army_order_per_decision() {
    let text = file_of(
        "s",
        &[entry(
            "s",
            15,
            6,
            &[
                ("foundry", 0, 100.0),
                ("foundry", 0, 140.0),
                ("foundry", 0, 180.0),
            ],
            &[("sentinel", 1)],
            10_000,
            600,
            60.0,
            5,
        )],
    );
    let c = loaded("onceper", &text);
    let mut app = solo(c, 11, "s", 1_000_000, false);
    tick(&mut app, 3_000);
    let t = trains(&app, Faction::A);
    let mut ticks: Vec<u32> = t.iter().map(|(tk, _)| *tk).collect();
    let before = ticks.len();
    ticks.dedup();
    assert_eq!(
        before,
        ticks.len(),
        "at most one TrainArmy per decision tick, even with three idle barracks"
    );
}

/// The target choice cannot depend on spawn or archetype order. Plant three
/// foundries by hand, in two different orders and with two different geometries,
/// and require the *sequence of queue-length multisets* to match — the rule is
/// "shallowest queue, ties by entity bits", which is a function of the world,
/// not of which barracks happens to be nearer or listed first.
#[test]
fn critic_target_choice_is_independent_of_geometry() {
    let text = file_of(
        "s",
        &[entry(
            "s",
            15,
            6,
            &[("foundry", 100_000, 100.0)], // never placed by the commander
            &[("sentinel", 1)],
            10_000,
            600,
            60.0,
            4,
        )],
    );

    let run = |name: &str, offsets: [Vec2; 3]| -> Vec<Vec<usize>> {
        let c = loaded(name, &text);
        let commanders =
            AiCommanders::matchup(&c, 5, &[(Faction::A, "s")]).expect("named strategy");
        let foundry = c.building_index("foundry").unwrap();
        let mut app = app_with(c, commanders, 1_000_000, false, move |w, f, base| {
            if f != Faction::A {
                return;
            }
            for off in offsets {
                w.spawn((
                    Position(base + off),
                    Building { def: foundry },
                    f,
                    ProductionQueue::default(),
                ));
            }
        });
        let mut out = Vec::new();
        for _ in 0..40 {
            tick(&mut app, 15);
            out.push(
                army_buildings(&mut app, Faction::A)
                    .into_iter()
                    .map(|(_, q)| q)
                    .collect(),
            );
        }
        out
    };

    let near_first = run(
        "geo_a",
        [
            Vec2::new(40.0, 0.0),
            Vec2::new(200.0, 0.0),
            Vec2::new(400.0, 0.0),
        ],
    );
    let far_first = run(
        "geo_b",
        [
            Vec2::new(400.0, 0.0),
            Vec2::new(200.0, 0.0),
            Vec2::new(40.0, 0.0),
        ],
    );
    assert_eq!(
        near_first, far_first,
        "which barracks is nearer must not reach the target choice"
    );
    assert!(
        near_first.last().unwrap().iter().all(|q| *q > 0),
        "all three hand-planted barracks are used: {:?}",
        near_first.last().unwrap()
    );
}

/// Two runs of one repeated-opening match agree tick for tick.
#[test]
fn critic_repeated_openings_replay_identically() {
    let text = file_of(
        "s",
        &[
            ok_entry(
                "s",
                &[
                    ("foundry", 0, 100.0),
                    ("foundry", 300, 140.0),
                    ("gene_vats", 600, 180.0),
                ],
                &[("sentinel", 1), ("ripper", 2)],
            ),
            ok_entry("t", &[("foundry", 300, 130.0)], &[("sentinel", 1)]),
        ],
    );
    let run = |name: &str| -> (u64, u64) {
        let c = loaded(name, &text);
        let mut app = matchup(c, 0xD15EA5E, "s", "t", 2_000, true);
        let h = hash_trace(&mut app, 60, 50);
        (h, journal_digest(&app))
    };
    assert_eq!(run("replay_a"), run("replay_b"));
}

// =============================================================================
// 5. Neutrality on the shipped data
// =============================================================================

/// Shipped matchups, both orientations, several seeds, a longer horizon than the
/// shipped tests use. The expected values are those of the **parent commit**,
/// captured by running this same probe in a baseline tree.
#[test]
fn critic_shipped_matchups_are_bit_identical() {
    let cases: [(u64, &str, &str); 8] = [
        (1, "mvp", "mvp"),
        (2, "mvp", "rush"),
        (3, "rush", "mvp"),
        (4, "synth_triad", "turtle"),
        (5, "turtle", "synth_triad"),
        (6, "mass_arclight", "mass_ripper"),
        (7, "mass_ripper", "mass_arclight"),
        (8, "synth_steel_flesh", "mass_bulwark"),
    ];
    let mut got: Vec<(u64, u64)> = Vec::new();
    for (seed, a, b) in cases {
        let c = Content::load_from_dir(&data_dir()).expect("shipped content");
        let mut app = matchup(c, seed, a, b, 300, true);
        let h = hash_trace(&mut app, 120, 60); // 7 200 ticks
        got.push((h, journal_digest(&app)));
    }
    // Printed so the baseline tree can be read off the same way.
    for (i, (h, j)) in got.iter().enumerate() {
        println!("SHIPPED[{i}] hash={h:#018x} journal={j:#018x}");
    }
    // Captured by running this exact probe in a tree checked out at the parent
    // commit (92626b2), before AC0b — and re-pinned at B3.5 (F-032), when the
    // content re-tune moved every per-tick `state_hash`. F-032 demonstrates the
    // move is content-driven: the pre-tune `assets/data` under this identical
    // binary passes at the pre-B3.5 numbers. Rows 3-7 re-pinned again at B4
    // (F-041), when the opening reservation and the restored multi-barracks
    // scripts moved every matchup with more than one opening; the pre-B4 rows
    // are reproduced by this binary built with `--features
    // no-opening-reservation` on the pre-B4 `strategies.ron` (F-041's proof).
    // Rows 0-2 (`mvp`, `rush`: one opening each) did not move.
    let expected: [(u64, u64); 8] = [
        (0x47c76bb871e46efb, 0xbd416bd4be86da11),
        (0x79c341576e931fc4, 0x2a25c84bba78866a),
        (0xba80ea4ba6cacdea, 0x74973f97c60a686b),
        (0x622c06205abf7772, 0x65f9917da21d98f2), // B4; was (0xe94d2708dca81c49, 0x4666a3b3563b8890)
        (0x1aa33ab2d6a04527, 0xa6552ff89996a930), // B4; was (0xb7d0e39bdfb0757c, 0x926206a7e2150e39)
        (0x54b9d0fd2c5779be, 0x2dbb7d71846b917e), // B4; was (0x283fcec8bb5ebf41, 0xc7328be1b0e09085)
        (0x241e74a2e363c562, 0xf6546287f161a392), // B4; was (0x1bc7d81e51c54e99, 0x90d87cb290ad3f58)
        (0x3835febbbac676e3, 0xb54435b778763907), // B4; was (0x478a4be3e491483e, 0xc9f9d38ba031b30b)
    ];
    assert_eq!(
        got.to_vec(),
        expected.to_vec(),
        "a shipped matchup moved — AC0b must be behaviour-neutral"
    );
}

// =============================================================================
// 6. The measured throughput claim: 9 / 27 / 9 over 7 200 ticks
// =============================================================================

fn throughput(name: &str, openings: &[(&str, u32, f32)], depth: u32) -> usize {
    let text = file_of(
        "s",
        &[entry(
            "s",
            30,
            1,
            openings,
            &[("ripper", 1)],
            100_000, // never attacks: nothing dies, so the count is production
            600,
            60.0,
            depth,
        )],
    );
    let c = loaded(name, &text);
    let mut app = solo(c, 3, "s", 100_000, true);
    tick(&mut app, 7_200);
    combat_units(&mut app, Faction::A)
}

/// The reported table, re-derived: 7 200 ticks, ample Alloy, nothing attacking.
#[test]
fn critic_the_measured_throughput_numbers_reproduce() {
    let one_deep_1 = throughput("tp_1x1", &[("gene_vats", 300, 130.0)], 1);
    let three_deep_1 = throughput(
        "tp_3x1",
        &[
            ("gene_vats", 300, 130.0),
            ("gene_vats", 300, 160.0),
            ("gene_vats", 300, 190.0),
        ],
        1,
    );
    let one_deep_3 = throughput("tp_1x3", &[("gene_vats", 300, 130.0)], 3);
    println!("throughput 1x1={one_deep_1} 3x1={three_deep_1} 1x3={one_deep_3}");
    assert_eq!(one_deep_1, 9, "one barracks at depth 1");
    assert_eq!(three_deep_1, 27, "three barracks at depth 1");
    assert_eq!(
        one_deep_3, 9,
        "one barracks at depth 3 — depth is not the lever"
    );
}

/// The same lever, measured on a different unit and barracks, so the claim is
/// not a property of the Ripper: count multiplies, depth does not.
#[test]
fn critic_barracks_count_scales_and_depth_does_not() {
    let sentinels = |name: &str, openings: &[(&str, u32, f32)], depth: u32| -> usize {
        let text = file_of(
            "s",
            &[entry(
                "s",
                30,
                6,
                openings,
                &[("sentinel", 1)],
                100_000,
                600,
                60.0,
                depth,
            )],
        );
        let c = loaded(name, &text);
        let mut app = solo(c, 42, "s", 1_000_000, false);
        tick(&mut app, 7_200);
        combat_units(&mut app, Faction::A)
    };
    let one = sentinels("sc_1x1", &[("foundry", 0, 130.0)], 1);
    let three = sentinels(
        "sc_3x1",
        &[
            ("foundry", 0, 130.0),
            ("foundry", 0, 160.0),
            ("foundry", 0, 190.0),
        ],
        1,
    );
    let deep = sentinels("sc_1x3", &[("foundry", 0, 130.0)], 3);
    println!("sentinel throughput 1x1={one} 3x1={three} 1x3={deep}");
    assert_eq!(three, 3 * one, "three lines finish three times the units");
    assert_eq!(deep, one, "depth alone finishes no more units");
}
