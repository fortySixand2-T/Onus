//! Adversarial critic probes for M4c (scripted AI + win condition).
//!
//! Written against the M4c spec only: a deterministic scripted AI, a match that
//! terminates when the enemy HQ falls, orders that carry their issuer, dead
//! entities that never panic, and the F-008 gather pair. Everything runs
//! headless through the *shipped* sim chain (`onus::add_sim_systems`, F-004).

use std::path::PathBuf;

use bevy::prelude::*;

use onus::sim::combat::{Casualties, Health, Target};
use onus::sim::content::Content;
use onus::sim::economy::{Building, ProductionQueue, Stockpiles, UnitDefIdx};
use onus::sim::spatial::{brute_force_nearest_enemy, Faction, Unit};
use onus::sim::{
    AiCommanders, AiJournal, CommandQueue, GatherPhase, GatherTarget, MatchOutcome, MatchState,
    MoveTarget, Order, Position, RateReport, ResourceNode,
};

// ---- harness ----------------------------------------------------------------

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data")
}

fn content() -> Content {
    Content::load_from_dir(&data_dir()).expect("assets/data/*.ron load")
}

fn sim_app_with_alloy(alloy: u32) -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .insert_resource(Time::<Fixed>::from_hz(60.0))
        .insert_resource(content())
        .init_resource::<CommandQueue>()
        .init_resource::<RateReport>()
        .init_resource::<Casualties>()
        .insert_resource(Stockpiles::starting(alloy));
    onus::add_sim_systems(&mut app, Update);
    app
}

fn sim_app() -> App {
    sim_app_with_alloy(0)
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

fn push(app: &mut App, order: Order) {
    app.world_mut()
        .resource_mut::<CommandQueue>()
        .0
        .push_back(order);
}

fn spawn_unit(app: &mut App, id: &str, faction: Faction, pos: Vec2) -> Entity {
    let (idx, kind, hp) = {
        let c = app.world().resource::<Content>();
        let idx = c.unit_index(id).unwrap_or_else(|| panic!("unit `{id}`"));
        (idx, c.units[idx].mvp_kind, Health::from_def(c, idx))
    };
    app.world_mut()
        .spawn((Position(pos), UnitDefIdx(idx), kind, faction, hp))
        .id()
}

/// A *live* building: definition, faction, queue **and** the HP pool the RON
/// buys it — i.e. what `setup`/`place_building` actually put on the field.
fn spawn_building(app: &mut App, id: &str, faction: Faction, pos: Vec2) -> Entity {
    let (def, hp) = {
        let c = app.world().resource::<Content>();
        let d = c
            .building_index(id)
            .unwrap_or_else(|| panic!("building `{id}`"));
        (d, Health::from_building_def(c, d))
    };
    app.world_mut()
        .spawn((
            Position(pos),
            Building { def },
            faction,
            ProductionQueue::default(),
            hp,
        ))
        .id()
}

fn outcome(app: &App) -> Option<MatchOutcome> {
    app.world().resource::<MatchState>().outcome()
}

fn hp(app: &App, e: Entity) -> Option<u32> {
    app.world().get::<Health>(e).map(|h| h.current)
}

// ============================================================================
// AC2 — the match terminates on HQ destruction
// ============================================================================

/// **The win condition, at the boundary.** M4c: "Win = destroy the enemy HQ; the
/// match then terminates." The `engaged` latch is set from the state
/// `match_end` observes *after* the tick has already been played, so an HQ that
/// falls on the very first sim tick is never observed standing — the latch never
/// closes and the match is permanently undecidable. A destroyed HQ must end the
/// match on whatever tick it dies, including the first.
#[test]
fn an_hq_destroyed_on_the_first_tick_still_ends_the_match() {
    let mut app = sim_app();
    spawn_building(&mut app, "hq", Faction::A, Vec2::new(-300.0, 0.0));
    let doomed = spawn_building(&mut app, "hq", Faction::B, Vec2::ZERO);
    // One hit from being razed, with an executioner already in range.
    app.world_mut()
        .entity_mut(doomed)
        .insert(Health { current: 1, max: 400 });
    spawn_unit(&mut app, "arclight", Faction::A, Vec2::new(-60.0, 0.0));

    tick(&mut app, 600);

    assert!(
        app.world().get::<Building>(doomed).is_none(),
        "the HQ survived — the fixture is wrong, not the sim"
    );
    let state = *app.world().resource::<MatchState>();
    assert_eq!(
        outcome(&app).map(|o| o.winner),
        Some(Some(Faction::A)),
        "the enemy HQ fell on tick 1 and the match never terminated \
         (engaged={}, tick={})",
        state.engaged(),
        state.tick()
    );
}

/// The same defect from the other side: a match that has *had* both HQs on the
/// board is decidable, so whether the loss lands on tick 1 or tick 50 cannot
/// change whether the match ends — only when.
#[test]
fn when_the_hq_falls_does_not_change_whether_the_match_ends() {
    let decide = |delay: u32| {
        let mut app = sim_app();
        spawn_building(&mut app, "hq", Faction::A, Vec2::new(-300.0, 0.0));
        let doomed = spawn_building(&mut app, "hq", Faction::B, Vec2::ZERO);
        app.world_mut()
            .entity_mut(doomed)
            .insert(Health { current: 1, max: 400 });
        tick(&mut app, delay);
        // The executioner arrives `delay` ticks in; from here it is identical.
        spawn_unit(&mut app, "arclight", Faction::A, Vec2::new(-60.0, 0.0));
        tick(&mut app, 600);
        outcome(&app).map(|o| o.winner)
    };
    let late = decide(50);
    assert_eq!(late, Some(Some(Faction::A)), "sanity: a late loss decides");
    assert_eq!(
        decide(0),
        late,
        "the same HQ loss decides or not depending on which tick it lands on"
    );
}

/// The recorded outcome is frozen: once written, neither the winner nor the tick
/// may move, and nothing the sim owns may keep changing.
#[test]
fn the_recorded_outcome_is_stable_and_the_sim_is_frozen_after_it() {
    let mut app = sim_app_with_alloy(10_000);
    spawn_building(&mut app, "hq", Faction::A, Vec2::new(-300.0, 0.0));
    let doomed = spawn_building(&mut app, "hq", Faction::B, Vec2::ZERO);
    app.world_mut()
        .entity_mut(doomed)
        .insert(Health { current: 40, max: 400 });
    spawn_unit(&mut app, "arclight", Faction::A, Vec2::new(-60.0, 0.0));
    let walker = spawn_unit(&mut app, "worker", Faction::A, Vec2::new(-300.0, -300.0));

    let mut decided = None;
    for _ in 0..1_200 {
        step(&mut app);
        if let Some(o) = outcome(&app) {
            decided = Some(o);
            break;
        }
    }
    let first = decided.expect("the HQ fell but the match did not end");
    assert_eq!(first.winner, Some(Faction::A));

    // Try to keep playing.
    push(
        &mut app,
        Order::MoveTo {
            units: vec![walker],
            dest: Vec2::new(500.0, 500.0),
        }
        .issued_by(Faction::A),
    );
    let pos_before = app.world().get::<Position>(walker).unwrap().0;
    let ticks_before = app.world().resource::<RateReport>().sim_ticks;
    let alloy_before = app.world().resource::<Stockpiles>().alloy(Faction::A);
    tick(&mut app, 300);

    assert_eq!(outcome(&app), Some(first), "the outcome moved after the end");
    assert_eq!(
        app.world().get::<Position>(walker).unwrap().0,
        pos_before,
        "a unit kept moving after the match ended"
    );
    assert_eq!(
        app.world().resource::<RateReport>().sim_ticks,
        ticks_before,
        "the sim kept ticking after the match ended"
    );
    assert_eq!(
        app.world().resource::<Stockpiles>().alloy(Faction::A),
        alloy_before
    );
}

/// A razed HQ is destroyed exactly once even when several attackers overkill it
/// on the same tick (a double despawn is a panic in Bevy), and razing it is not
/// a unit casualty.
#[test]
fn an_overkilled_hq_dies_once_and_is_not_a_casualty() {
    let mut app = sim_app();
    spawn_building(&mut app, "hq", Faction::A, Vec2::new(-600.0, 0.0));
    let doomed = spawn_building(&mut app, "hq", Faction::B, Vec2::ZERO);
    app.world_mut()
        .entity_mut(doomed)
        .insert(Health { current: 5, max: 400 });
    for i in 0..4 {
        spawn_unit(
            &mut app,
            "arclight",
            Faction::A,
            Vec2::new(-60.0, 20.0 * i as f32),
        );
    }
    tick(&mut app, 5);
    assert!(app.world().get::<Building>(doomed).is_none(), "still alive");
    assert_eq!(
        app.world().resource::<Casualties>().total(),
        0,
        "a razed building was counted as a unit casualty"
    );
    assert_eq!(
        app.world().resource::<Casualties>().lost(Faction::B),
        0,
        "a razed building was counted against its owner"
    );
}

/// Building damage is the RON's own arithmetic: `offense * damage_per_offense -
/// mvp_armor * mitigation_per_armor`, against a pool of `mvp_defense *
/// building_hp_per_defense`. Checked on a *different* building/attacker pair
/// than the implementer's, so a hardcoded number cannot pass both.
#[test]
fn building_damage_and_pool_follow_the_ron_for_every_building() {
    let c = content();
    let mut app = sim_app();
    // Keep a second faction's HQ off the board: this probe is about arithmetic.
    let sentinel = c.unit_index("sentinel").unwrap();
    for id in ["foundry", "gene_vats", "aether_spire"] {
        let def = c.building_index(id).unwrap();
        let pool = c.buildings[def].mvp_defense * c.combat.building_hp_per_defense;
        let expect = (c.units[sentinel].offense * c.combat.damage_per_offense)
            .saturating_sub(c.buildings[def].mvp_armor * c.combat.mitigation_per_armor);
        assert!(expect > 0, "fixture: `{id}` is immune to a Sentinel");

        let target = spawn_building(&mut app, id, Faction::B, Vec2::new(0.0, 0.0));
        assert_eq!(hp(&app, target), Some(pool), "`{id}` pool");
        let attacker = spawn_unit(&mut app, "sentinel", Faction::A, Vec2::new(-40.0, 0.0));
        step(&mut app);
        assert_eq!(
            hp(&app, target),
            Some(pool - expect),
            "`{id}` took the wrong damage from one Sentinel hit"
        );
        app.world_mut().entity_mut(target).despawn();
        app.world_mut().entity_mut(attacker).despawn();
        step(&mut app);
    }
}

// ============================================================================
// AC2 — buildings as targets must not perturb unit-vs-unit selection
// ============================================================================

/// Differential oracle (M2): with only units on the field, every engagement the
/// sim publishes must be the brute-force nearest enemy. Making buildings
/// targetable put them in the same layout the grid is built from, so this is the
/// regression that would catch a perturbed selection.
#[test]
fn unit_vs_unit_targeting_still_matches_the_brute_force_oracle() {
    // Bulwark vs Ravager: nobody can die in one tick (180/160 HP against 26/17
    // damage), so the oracle compares against a field that is still intact.
    let ids = ["bulwark", "ravager"];
    let mut app = sim_app();
    let mut rows: Vec<(Entity, Vec2, Faction)> = Vec::new();
    let mut rng = 0x1234_5678u64;
    let mut next = || {
        rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((rng >> 33) % 400) as f32 - 200.0
    };
    for i in 0..24 {
        let faction = if i % 2 == 0 { Faction::A } else { Faction::B };
        let pos = Vec2::new(next(), next());
        let e = spawn_unit(&mut app, ids[i % 2], faction, pos);
        rows.push((e, pos, faction));
    }
    // One tick: targets are published from the start-of-tick snapshot, so the
    // oracle reads the same positions the sim did.
    step(&mut app);

    let mut layout: Vec<(Entity, Unit)> = rows
        .iter()
        .map(|(e, p, f)| (*e, Unit { pos: *p, faction: *f }))
        .collect();
    layout.sort_by_key(|(e, _)| e.to_bits());
    let units: Vec<Unit> = layout.iter().map(|(_, u)| *u).collect();
    let engage = content().combat.engage_range;

    for (i, (e, _)) in layout.iter().enumerate() {
        let expect = brute_force_nearest_enemy(&units, i)
            .filter(|&j| units[i].pos.distance(units[j].pos) <= engage)
            .map(|j| layout[j].0);
        let got = app.world().get::<Target>(*e).map(|t| t.0);
        assert_eq!(got, expect, "unit {i} engaged the wrong enemy");
    }
}

// ============================================================================
// F-008 — the gather claim is a pair that cannot be split
// ============================================================================

/// F-008: a lone `GatherTarget` disarms a unit forever — combat reads it as "the
/// economy owns this one" and the economy's own query needs *both* halves to
/// ever release it. `#[require(GatherPhase)]` only covers insertion; a plain
/// `remove::<GatherPhase>()` still leaves the half that does the damage. The
/// economy must repair or release such a unit, not lose it.
#[test]
fn a_gather_target_without_its_phase_does_not_disarm_a_unit_forever() {
    let mut app = sim_app();
    let node = app
        .world_mut()
        .spawn((Position(Vec2::new(30.0, 0.0)), ResourceNode { amount: 500 }))
        .id();
    let worker = spawn_unit(&mut app, "worker", Faction::A, Vec2::ZERO);
    push(
        &mut app,
        Order::Gather {
            units: vec![worker],
            node,
            node_pos: Vec2::new(30.0, 0.0),
        }
        .issued_by(Faction::A),
    );
    step(&mut app);
    assert!(app.world().get::<GatherTarget>(worker).is_some());

    // Split the pair by hand — the shape F-008 names.
    app.world_mut().entity_mut(worker).remove::<GatherPhase>();
    tick(&mut app, 120);

    // The harm, made concrete: the worker is stuck. It banks nothing ever
    // again, and the economy cannot take the job back.
    assert_eq!(
        app.world().resource::<Stockpiles>().alloy(Faction::A),
        0,
        "fixture: this worker has no drop-off, it should have banked nothing"
    );
    let target = app.world().get::<GatherTarget>(worker).is_some();
    let phase = app.world().get::<GatherPhase>(worker).is_some();
    assert!(
        !target || phase,
        "a split claim survived: the unit keeps `GatherTarget` with no \
         `GatherPhase`, so the economy can never see it again and combat still \
         treats it as owned by the economy"
    );
}

// ============================================================================
// Orders: issuer, and dead entities
// ============================================================================

/// An *unsigned* order is documented as "self-signed — attributed to whatever it
/// touches, so it can never be cross-faction". One order that touches both
/// factions' units is exactly the counterexample: it commands two factions at
/// once, which no commander may do.
#[test]
fn one_unsigned_order_cannot_command_both_factions_at_once() {
    let mut app = sim_app();
    let mine = spawn_unit(&mut app, "ripper", Faction::A, Vec2::ZERO);
    let theirs = spawn_unit(&mut app, "ripper", Faction::B, Vec2::new(1_000.0, 0.0));
    push(
        &mut app,
        Order::MoveTo {
            units: vec![mine, theirs],
            dest: Vec2::new(500.0, 500.0),
        },
    );
    step(&mut app);
    let a = app.world().get::<MoveTarget>(mine).is_some();
    let b = app.world().get::<MoveTarget>(theirs).is_some();
    assert!(
        !(a && b),
        "a single unsigned order moved units of both factions — it is not \
         self-signed, it is unsigned"
    );
}

/// Every order variant, aimed at an entity that has already been despawned,
/// must be inert rather than fatal — including the ones an AI issues against
/// something it remembered from an earlier tick.
#[test]
fn every_order_variant_against_a_dead_entity_is_inert() {
    let mut app = sim_app_with_alloy(10_000);
    let dead_unit = spawn_unit(&mut app, "worker", Faction::A, Vec2::ZERO);
    let dead_building = spawn_building(&mut app, "hq", Faction::A, Vec2::new(100.0, 0.0));
    let dead_node = app
        .world_mut()
        .spawn((Position(Vec2::new(50.0, 0.0)), ResourceNode { amount: 10 }))
        .id();
    app.world_mut().entity_mut(dead_unit).despawn();
    app.world_mut().entity_mut(dead_building).despawn();
    app.world_mut().entity_mut(dead_node).despawn();

    let worker = app.world().resource::<Content>().unit_index("worker").unwrap();
    let before = app.world().resource::<Stockpiles>().alloy(Faction::A);
    for o in [
        Order::MoveTo {
            units: vec![dead_unit],
            dest: Vec2::ZERO,
        },
        Order::Gather {
            units: vec![dead_unit],
            node: dead_node,
            node_pos: Vec2::ZERO,
        },
        Order::Train {
            building: dead_building,
            unit: worker,
        },
    ] {
        push(&mut app, o.issued_by(Faction::A));
    }
    tick(&mut app, 5);
    assert_eq!(
        app.world().resource::<Stockpiles>().alloy(Faction::A),
        before,
        "an order against a dead entity still charged Alloy"
    );
}

// ============================================================================
// AC1 — determinism, and that the seed actually reaches the decisions
// ============================================================================

fn ai_vs_ai(seed: u64) -> App {
    let mut app = sim_app_with_alloy(content().economy.starting_alloy);
    for (faction, base) in [
        (Faction::A, Vec2::new(-750.0, 0.0)),
        (Faction::B, Vec2::new(750.0, 0.0)),
    ] {
        spawn_building(&mut app, "hq", faction, base);
        app.world_mut().spawn((
            Position(base + Vec2::new(0.0, 250.0)),
            ResourceNode { amount: 100_000 },
        ));
        for i in 0..3 {
            spawn_unit(
                &mut app,
                "worker",
                faction,
                base + Vec2::new(0.0, 20.0 * i as f32),
            );
        }
    }
    app.insert_resource(AiCommanders::new(seed, &[Faction::A, Faction::B]));
    app
}

/// Per-tick state hash of the whole sim: every position, HP, stockpile and the
/// journal length. Two runs of the same seed must agree on *every* tick, not
/// merely at the end.
fn state_hash(app: &mut App) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut mix = |v: u64| {
        h ^= v;
        h = h.wrapping_mul(0x1000_0000_01b3);
    };
    let mut rows: Vec<(u64, i64, i64, u64)> = {
        let mut q = app
            .world_mut()
            .query::<(Entity, &Position, Option<&Health>)>();
        q.iter(app.world())
            .map(|(e, p, hp)| {
                (
                    e.to_bits(),
                    (p.0.x * 1024.0) as i64,
                    (p.0.y * 1024.0) as i64,
                    hp.map(|h| h.current as u64).unwrap_or(u64::MAX),
                )
            })
            .collect()
    };
    rows.sort_unstable();
    for (a, b, c, d) in rows {
        mix(a);
        mix(b as u64);
        mix(c as u64);
        mix(d);
    }
    mix(app.world().resource::<Stockpiles>().alloy(Faction::A) as u64);
    mix(app.world().resource::<Stockpiles>().alloy(Faction::B) as u64);
    mix(app.world().resource::<AiJournal>().0.len() as u64);
    mix(app.world().resource::<Casualties>().total() as u64);
    h
}

/// AC1's headline probe, tick by tick rather than at the end: the same seed
/// replays byte-identically, and a different seed genuinely diverges (so the
/// test cannot pass by the AI ignoring its seed).
#[test]
fn the_same_seed_replays_tick_for_tick_and_a_different_seed_diverges() {
    let trace = |seed: u64| {
        let mut app = ai_vs_ai(seed);
        let mut hashes = Vec::new();
        for _ in 0..3_000 {
            step(&mut app);
            hashes.push(state_hash(&mut app));
        }
        let journal = app.world().resource::<AiJournal>().0.len();
        let actions: Vec<String> = app
            .world()
            .resource::<AiJournal>()
            .0
            .iter()
            .map(|(t, f, a)| format!("{t}:{f:?}:{a:?}"))
            .collect();
        (hashes, journal, actions)
    };
    let a = trace(5);
    let b = trace(5);
    let first_divergence = a.0.iter().zip(b.0.iter()).position(|(x, y)| x != y);
    assert_eq!(
        first_divergence, None,
        "two runs of seed 5 diverge at tick {:?}",
        first_divergence
    );
    assert_eq!(a.2, b.2, "same seed, different actions");
    let c = trace(6);
    assert_ne!(a.2, c.2, "the seed does not reach the AI's decisions");
}

/// The ≤ ~8 min target, over seeds the implementer did not name, and a decision
/// that is *earned*: the loser's HQ is genuinely gone, and it was gone before
/// the budget ran out.
#[test]
fn ai_vs_ai_matches_are_decided_by_a_dead_hq_across_many_seeds() {
    const BUDGET: u32 = 8 * 60 * 60;
    let mut lengths = Vec::new();
    for seed in [2u64, 3, 4, 8, 13, 21, 34, 55, 1234] {
        let mut app = ai_vs_ai(seed);
        let mut decided = None;
        for _ in 0..BUDGET {
            step(&mut app);
            if let Some(o) = outcome(&app) {
                decided = Some(o);
                break;
            }
        }
        let o = decided.unwrap_or_else(|| panic!("seed {seed}: undecided in {BUDGET} ticks"));
        let winner = o.winner.expect("a symmetric match ended in a draw");
        // Earned: the loser has no victory building left, and the winner does.
        let victory_def = content().building_index("hq").unwrap();
        let standing: Vec<Faction> = {
            let mut q = app.world_mut().query::<(&Building, &Faction)>();
            q.iter(app.world())
                .filter(|(b, _)| b.def == victory_def)
                .map(|(_, f)| *f)
                .collect()
        };
        assert_eq!(
            standing,
            vec![winner],
            "seed {seed}: the winner is not the only side with an HQ"
        );
        lengths.push((seed, o.tick, winner));
    }
    println!("decided: {lengths:?}");
}

/// The AI is a commander with a budget, not a printer: across a long match no
/// faction's Alloy may go up except through deposits, and the AI must never end
/// a match having spent more than it banked. (Conservation check on the AI's
/// own spending path.)
#[test]
fn the_ai_never_spends_alloy_it_does_not_have() {
    let mut app = ai_vs_ai(3);
    for t in 0..3_000u32 {
        step(&mut app);
        for f in [Faction::A, Faction::B] {
            let a = app.world().resource::<Stockpiles>().alloy(f);
            assert!(a < u32::MAX / 2, "tick {t}: {f:?} alloy wrapped to {a}");
        }
        if outcome(&app).is_some() {
            break;
        }
    }
}

// ============================================================================
// Content — the AI script and the building stats are data, validated
// ============================================================================

fn load_mutated(from: &str, to: &str) -> Result<Content, String> {
    let mut tag: u64 = 0xcbf2_9ce4_8422_2325;
    for b in from.bytes().chain(b"|".iter().copied()).chain(to.bytes()) {
        tag ^= b as u64;
        tag = tag.wrapping_mul(0x1000_0000_01b3);
    }
    let dir = std::env::temp_dir().join(format!(
        "onus-critic-m4c-{}-{tag:016x}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("tmp dir");
    let units = std::fs::read_to_string(data_dir().join("units.ron")).expect("units.ron");
    assert!(units.contains(from), "fixture: `{from}` not in units.ron");
    std::fs::write(dir.join("units.ron"), units.replacen(from, to, 1)).expect("write");
    std::fs::copy(data_dir().join("resources.ron"), dir.join("resources.ron")).expect("copy");
    let r = Content::load_from_dir(&dir).map_err(|e| e.to_string());
    let _ = std::fs::remove_dir_all(&dir);
    r
}

/// F-005, one level up: a building HP scale the sim cannot represent must be an
/// `Err` from the loader — in release too, where a wrapping multiply would
/// silently accept it.
#[test]
fn a_building_hp_scale_the_sim_cannot_represent_is_an_error() {
    let err = load_mutated("building_hp_per_defense: 40", "building_hp_per_defense: 4294967295")
        .expect_err("an unrepresentable building HP scale loaded");
    assert!(
        err.contains("building HP") || err.contains("overflow"),
        "unexpected rejection: {err}"
    );
    // And a building stat off the design scale is refused too.
    load_mutated("mvp_defense: 10, mvp_armor: 4", "mvp_defense: 11, mvp_armor: 4")
        .expect_err("a building above max_stat loaded");
    // A building with no pool at all is a load error, not a 0-HP building.
    load_mutated("mvp_defense: 10, mvp_armor: 4", "mvp_defense: 0, mvp_armor: 4")
        .expect_err("a 0-HP building loaded");
}

/// The AI script is content: every number it reads must be stated in the RON and
/// a script the sim cannot run must be refused at load, as an `Err`.
#[test]
fn the_ai_script_is_data_and_a_broken_script_is_refused() {
    for (from, to) in [
        ("think_interval_ticks: 30", "think_interval_ticks: 0"),
        ("attack_interval_ticks: 600", "attack_interval_ticks: 0"),
        ("worker_target: 6", "worker_target: 0"),
        ("attack_at_army: 3", "attack_at_army: 0"),
        (
            "barracks: \"foundry\",\n        barracks_at_tick",
            "barracks: \"hq\",\n        barracks_at_tick",
        ),
        (
            "barracks: \"foundry\",\n        barracks_at_tick",
            "barracks: \"nonesuch\",\n        barracks_at_tick",
        ),
        ("barracks_offset: 130.0", "barracks_offset: 0.0"),
        ("(unit: \"sentinel\", count: 2)", "(unit: \"sentinel\", count: 0)"),
        ("(unit: \"sentinel\", count: 2)", "(unit: \"worker\", count: 2)"),
        ("(unit: \"sentinel\", count: 2)", "(unit: \"nonesuch\", count: 2)"),
    ] {
        assert!(
            load_mutated(from, to).is_err(),
            "`{from}` -> `{to}` was accepted"
        );
    }
    // Every field is required: no serde default may paper over a missing one.
    for from in [
        "think_interval_ticks: 30,",
        "worker_target: 6,",
        "barracks_at_tick: 300,",
        "attack_spread: 60.0,",
    ] {
        assert!(
            load_mutated(from, "").is_err(),
            "a missing `{from}` loaded with a silent default"
        );
    }
}

/// No AI tuning constant may live in Rust: every number the script uses is read
/// from the RON, so changing the RON changes the AI.
#[test]
fn changing_the_ron_changes_the_ai() {
    let c = content();
    let dir = std::env::temp_dir().join(format!("onus-critic-m4c-slow-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("tmp dir");
    let units = std::fs::read_to_string(data_dir().join("units.ron")).expect("units.ron");
    std::fs::write(
        dir.join("units.ron"),
        units.replacen("barracks_at_tick: 300", "barracks_at_tick: 1200", 1),
    )
    .expect("write");
    std::fs::copy(data_dir().join("resources.ron"), dir.join("resources.ron")).expect("copy");
    let slow = Content::load_from_dir(&dir).expect("mutated content loads");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(c.ai.barracks_at_tick, 300, "fixture drifted");
    assert_eq!(slow.ai.barracks_at_tick, 1_200);

    let barracks_tick = |content: Content| {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(Time::<Fixed>::from_hz(60.0))
            .insert_resource(content)
            .init_resource::<CommandQueue>()
            .init_resource::<RateReport>()
            .init_resource::<Casualties>()
            .insert_resource(Stockpiles::starting(10_000));
        onus::add_sim_systems(&mut app, Update);
        spawn_building(&mut app, "hq", Faction::A, Vec2::ZERO);
        spawn_building(&mut app, "hq", Faction::B, Vec2::new(2_000.0, 0.0));
        app.world_mut().spawn((
            Position(Vec2::new(200.0, 0.0)),
            ResourceNode { amount: 100_000 },
        ));
        for i in 0..3 {
            spawn_unit(&mut app, "worker", Faction::A, Vec2::new(0.0, 20.0 * i as f32));
        }
        app.insert_resource(AiCommanders::new(11, &[Faction::A]));
        let mut placed = None;
        for _ in 0..2_000 {
            step(&mut app);
            let j = app.world().resource::<AiJournal>();
            if let Some((t, _, _)) = j
                .0
                .iter()
                .find(|(_, _, a)| matches!(a, onus::sim::AiAction::PlaceBarracks { .. }))
            {
                placed = Some(*t);
                break;
            }
        }
        placed
    };
    let fast = barracks_tick(content()).expect("the AI never opened its barracks");
    let late = barracks_tick(slow).expect("the AI never opened its barracks (slow script)");
    assert!((300..600).contains(&fast), "shipped script: barracks at {fast}");
    assert!(late >= 1_200, "the RON's barracks_at_tick was ignored: {late}");
}
