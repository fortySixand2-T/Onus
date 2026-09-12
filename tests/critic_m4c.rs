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
    // The edit lands in whichever shipped file states the anchor: unit stats in
    // `units.ron`, the AI script in `strategies.ron` (B1).
    let mut hit = 0;
    for file in ["units.ron", "resources.ron", "strategies.ron"] {
        let text = std::fs::read_to_string(data_dir().join(file)).expect("shipped file");
        let edited = if text.contains(from) {
            hit += 1;
            text.replacen(from, to, 1)
        } else {
            text
        };
        std::fs::write(dir.join(file), edited).expect("write");
    }
    assert_eq!(hit, 1, "fixture: `{from}` is in {hit} shipped files, not exactly 1");
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
            "(building: \"foundry\", at_tick",
            "(building: \"hq\", at_tick",
        ),
        (
            "(building: \"foundry\", at_tick",
            "(building: \"nonesuch\", at_tick",
        ),
        ("offset: 130.0", "offset: 0.0"),
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
        "at_tick: 300,",
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
    // B1: the AI script lives in `strategies.ron`; the opening's tick is
    // `at_tick`.
    let strategies =
        std::fs::read_to_string(data_dir().join("strategies.ron")).expect("strategies.ron");
    std::fs::write(
        dir.join("strategies.ron"),
        strategies.replacen("at_tick: 300", "at_tick: 1200", 1),
    )
    .expect("write");
    for file in ["units.ron", "resources.ron"] {
        std::fs::copy(data_dir().join(file), dir.join(file)).expect("copy");
    }
    let slow = Content::load_from_dir(&dir).expect("mutated content loads");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(c.ai.barracks[0].at_tick, 300, "fixture drifted");
    assert_eq!(slow.ai.barracks[0].at_tick, 1_200);

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
    assert!(late >= 1_200, "the RON's at_tick was ignored: {late}");
}

// ============================================================================
// Critic pass 2 — the three fixes themselves
// ============================================================================

/// **F-008, second pass.** The fix's own documentation states the property
/// absolutely: the sweep "runs before the tick's gather and combat passes", so
/// "a half-claim therefore cannot survive into any reader". `ai_commanders` is a
/// reader of `GatherTarget` — it snapshots `on_a_job: job.is_some()` and skips
/// any unit that looks employed — and it runs **first** in the sim chain,
/// *before* `repair_gather_claims`. So a lone `GatherTarget` still survives into
/// a reader, and that reader still believes the lie: the economy cannot see the
/// worker (its query needs both halves) and the commander will not re-task it,
/// which is F-008's shape one reader further out.
///
/// Two identical idle workers; one carries a half claim. Both are idle as far as
/// the economy is concerned, so the commander must task both on its first
/// decision.
#[test]
fn a_split_gather_claim_is_still_read_as_a_job_by_the_ai() {
    let mut app = sim_app_with_alloy(0);
    spawn_building(&mut app, "hq", Faction::A, Vec2::ZERO);
    let node = app
        .world_mut()
        .spawn((Position(Vec2::new(200.0, 0.0)), ResourceNode { amount: 10_000 }))
        .id();
    let clean = spawn_unit(&mut app, "worker", Faction::A, Vec2::new(0.0, 10.0));
    let split = spawn_unit(&mut app, "worker", Faction::A, Vec2::new(0.0, 20.0));
    // The F-008 shape: the pair written, then one half removed.
    app.world_mut()
        .entity_mut(split)
        .insert((GatherTarget(node), GatherPhase::ToNode));
    app.world_mut().entity_mut(split).remove::<GatherPhase>();
    app.insert_resource(AiCommanders::new(1, &[Faction::A]));

    step(&mut app);

    let tasked: Vec<Entity> = app
        .world()
        .resource::<AiJournal>()
        .0
        .iter()
        .filter_map(|(_, _, a)| match a {
            onus::sim::AiAction::Gather { unit, .. } => Some(*unit),
            _ => None,
        })
        .collect();
    assert!(
        tasked.contains(&clean),
        "fixture: an idle worker was never tasked at all"
    );
    assert!(
        tasked.contains(&split),
        "a half claim survived into a reader: the commander read a lone \
         `GatherTarget` as a job and left the worker idle (tasked={tasked:?})"
    );
}

/// The same defect measured in its consequence rather than in the journal: the
/// worker with the half claim must start mining on the same decision as its
/// twin, not a whole `think_interval_ticks` later.
#[test]
fn a_worker_with_a_half_claim_is_not_left_idle_for_a_whole_think_interval() {
    let mut app = sim_app_with_alloy(0);
    spawn_building(&mut app, "hq", Faction::A, Vec2::ZERO);
    let node = app
        .world_mut()
        .spawn((Position(Vec2::new(200.0, 0.0)), ResourceNode { amount: 10_000 }))
        .id();
    let split = spawn_unit(&mut app, "worker", Faction::A, Vec2::new(0.0, 20.0));
    app.world_mut()
        .entity_mut(split)
        .insert((GatherTarget(node), GatherPhase::ToNode));
    app.world_mut().entity_mut(split).remove::<GatherPhase>();
    app.insert_resource(AiCommanders::new(1, &[Faction::A]));

    step(&mut app);
    assert!(
        app.world().get::<GatherTarget>(split).is_some()
            && app.world().get::<GatherPhase>(split).is_some(),
        "the worker was still unemployed after the tick that was supposed to \
         both sweep the half claim and re-task it"
    );
}

/// The sweep queues `remove` commands against entities it saw in its own query.
/// If those commands are not flushed before `combat` despawns one of them, the
/// deferred removal lands on a despawned entity — which is an error (panic) in
/// Bevy 0.19, not a no-op. A doomed unit carrying a half claim exercises exactly
/// that ordering.
#[test]
fn sweeping_a_half_claim_off_a_unit_that_dies_this_tick_is_not_fatal() {
    let mut app = sim_app();
    let node = app
        .world_mut()
        .spawn((Position(Vec2::new(500.0, 0.0)), ResourceNode { amount: 100 }))
        .id();
    let doomed = spawn_unit(&mut app, "worker", Faction::A, Vec2::ZERO);
    app.world_mut()
        .entity_mut(doomed)
        .insert(Health { current: 1, max: 40 });
    app.world_mut()
        .entity_mut(doomed)
        .insert((GatherTarget(node), GatherPhase::ToNode));
    app.world_mut().entity_mut(doomed).remove::<GatherPhase>();
    spawn_unit(&mut app, "arclight", Faction::B, Vec2::new(30.0, 0.0));

    tick(&mut app, 10);
    assert!(
        app.world().get_entity(doomed).is_err(),
        "fixture: the doomed unit never died"
    );
}

// ---- the `engaged` gate ----------------------------------------------------

/// Both HQs razed on the same tick is a **draw**, not a race — and the gate
/// moving to the start of the tick must not turn the draw into a winner.
#[test]
fn both_hqs_falling_on_the_same_tick_is_a_draw() {
    let mut app = sim_app();
    let a_hq = spawn_building(&mut app, "hq", Faction::A, Vec2::new(-1_000.0, 0.0));
    let b_hq = spawn_building(&mut app, "hq", Faction::B, Vec2::new(1_000.0, 0.0));
    for e in [a_hq, b_hq] {
        app.world_mut()
            .entity_mut(e)
            .insert(Health { current: 1, max: 400 });
    }
    spawn_unit(&mut app, "arclight", Faction::B, Vec2::new(-940.0, 0.0));
    spawn_unit(&mut app, "arclight", Faction::A, Vec2::new(940.0, 0.0));

    tick(&mut app, 60);
    assert!(
        app.world().get_entity(a_hq).is_err() && app.world().get_entity(b_hq).is_err(),
        "fixture: both HQs were supposed to fall"
    );
    let o = outcome(&app).expect("simultaneous loss left the match undecided");
    assert_eq!(o.winner, None, "a simultaneous loss picked a winner");
}

/// The property the gate exists to protect, in its harshest form: a fixture with
/// only one side on the board never terminates — **even when that side's own
/// only HQ is destroyed**. (An M1-M4b fixture is not a match.)
#[test]
fn a_one_sided_fixture_is_never_decided_even_when_its_lone_hq_dies() {
    let mut app = sim_app();
    let lone = spawn_building(&mut app, "hq", Faction::A, Vec2::ZERO);
    app.world_mut()
        .entity_mut(lone)
        .insert(Health { current: 1, max: 400 });
    spawn_unit(&mut app, "arclight", Faction::B, Vec2::new(60.0, 0.0));
    for t in 0..300 {
        step(&mut app);
        assert!(
            outcome(&app).is_none(),
            "tick {t}: a match with only one HQ on the board was decided"
        );
    }
    assert!(app.world().get_entity(lone).is_err(), "fixture: HQ survived");
}

/// Nothing the sim owns may move after the outcome is recorded — positions,
/// stockpiles, casualties, the AI's journal, a production queue mid-build, and
/// the tick counter itself. Checked over a live AI commander with work pending,
/// so "frozen" is not an artefact of an empty world.
#[test]
fn nothing_the_sim_owns_moves_after_the_match_is_recorded() {
    let mut app = sim_app_with_alloy(content().economy.starting_alloy);
    spawn_building(&mut app, "hq", Faction::A, Vec2::new(-300.0, 0.0));
    let doomed = spawn_building(&mut app, "hq", Faction::B, Vec2::ZERO);
    app.world_mut()
        .entity_mut(doomed)
        .insert(Health { current: 1, max: 400 });
    app.world_mut().spawn((
        Position(Vec2::new(-200.0, 0.0)),
        ResourceNode { amount: 100_000 },
    ));
    for i in 0..3 {
        spawn_unit(
            &mut app,
            "worker",
            Faction::A,
            Vec2::new(-300.0, 20.0 * i as f32),
        );
    }
    spawn_unit(&mut app, "arclight", Faction::A, Vec2::new(-60.0, 0.0));
    app.insert_resource(AiCommanders::new(9, &[Faction::A]));

    let mut decided_at = None;
    for t in 0..600u32 {
        step(&mut app);
        if outcome(&app).is_some() {
            decided_at = Some(t);
            break;
        }
    }
    decided_at.expect("the match never terminated");

    let snapshot = |app: &mut App| {
        let h = state_hash(app);
        let queued: usize = {
            let mut q = app.world_mut().query::<&ProductionQueue>();
            q.iter(app.world()).map(|p| p.items.len()).sum()
        };
        let s = *app.world().resource::<MatchState>();
        let j = app.world().resource::<AiJournal>().0.len();
        (h, queued, s, j)
    };
    let before = snapshot(&mut app);
    tick(&mut app, 300);
    let after = snapshot(&mut app);
    assert_eq!(before.0, after.0, "world state changed after termination");
    assert_eq!(before.1, after.1, "a production queue advanced after termination");
    assert_eq!(before.2, after.2, "MatchState changed after termination");
    assert_eq!(before.3, after.3, "the AI kept issuing orders after termination");
}

// ---- order attribution -----------------------------------------------------

/// Re-signing cannot launder ownership, however deep the stack and whichever
/// signature is outermost: the order is `Void` and is dropped whole.
#[test]
fn a_re_signed_order_is_void_and_cannot_be_laundered() {
    let mut app = sim_app();
    let mine = spawn_unit(&mut app, "ripper", Faction::A, Vec2::ZERO);
    let theirs = spawn_unit(&mut app, "ripper", Faction::B, Vec2::new(500.0, 0.0));
    let mk = |units: Vec<Entity>| Order::MoveTo {
        units,
        dest: Vec2::new(900.0, 900.0),
    };
    push(&mut app, mk(vec![mine]).issued_by(Faction::A).issued_by(Faction::B));
    push(&mut app, mk(vec![theirs]).issued_by(Faction::B).issued_by(Faction::A));
    push(
        &mut app,
        mk(vec![mine, theirs])
            .issued_by(Faction::A)
            .issued_by(Faction::B)
            .issued_by(Faction::A),
    );
    step(&mut app);
    for (who, e) in [("A", mine), ("B", theirs)] {
        assert!(
            app.world().get::<MoveTarget>(e).is_none(),
            "{who}'s unit obeyed a void (re-signed) order"
        );
    }
}

/// "Refused whole" must be whole for every entity-list variant, every list
/// ordering, and regardless of where the foreign entity sits (first, last, or
/// between unowned entities) — and regardless of which faction was spawned
/// first, so the refusal cannot be an artefact of entity-index order.
#[test]
fn an_unsigned_two_faction_order_is_refused_for_every_ordering() {
    for a_first in [true, false] {
        let mut app = sim_app();
        let (a_unit, b_unit) = if a_first {
            let a = spawn_unit(&mut app, "ripper", Faction::A, Vec2::ZERO);
            let b = spawn_unit(&mut app, "ripper", Faction::B, Vec2::new(600.0, 0.0));
            (a, b)
        } else {
            let b = spawn_unit(&mut app, "ripper", Faction::B, Vec2::new(600.0, 0.0));
            let a = spawn_unit(&mut app, "ripper", Faction::A, Vec2::ZERO);
            (a, b)
        };
        let node_pos = Vec2::new(-400.0, 0.0);
        let node = app
            .world_mut()
            .spawn((Position(node_pos), ResourceNode { amount: 500 }))
            .id();
        let bare = app.world_mut().spawn(Position(Vec2::ZERO)).id();
        let lists = [
            vec![a_unit, b_unit],
            vec![b_unit, a_unit],
            vec![bare, a_unit, node, b_unit],
            vec![b_unit, node, a_unit, bare],
        ];
        for units in lists {
            push(
                &mut app,
                Order::MoveTo {
                    units: units.clone(),
                    dest: Vec2::new(900.0, 900.0),
                },
            );
            push(
                &mut app,
                Order::Gather {
                    units,
                    node,
                    node_pos,
                },
            );
            step(&mut app);
            for (who, e) in [("A", a_unit), ("B", b_unit)] {
                assert!(
                    app.world().get::<MoveTarget>(e).is_none(),
                    "a_first={a_first}: {who}'s unit was commanded by a two-faction order"
                );
                assert!(
                    app.world().get::<GatherTarget>(e).is_none(),
                    "a_first={a_first}: {who}'s unit was tasked by a two-faction order"
                );
            }
        }
    }
}

/// An unsigned order that names only entities nobody owns has no faction to
/// derive, and must not become a licence over anything else: it commands the
/// unowned entities it named and nothing more.
#[test]
fn an_unsigned_order_over_unowned_entities_commands_nothing_owned() {
    let mut app = sim_app();
    let a_unit = spawn_unit(&mut app, "ripper", Faction::A, Vec2::ZERO);
    let b_unit = spawn_unit(&mut app, "ripper", Faction::B, Vec2::new(600.0, 0.0));
    let bare = app.world_mut().spawn(Position(Vec2::ZERO)).id();
    push(
        &mut app,
        Order::MoveTo {
            units: vec![bare],
            dest: Vec2::new(50.0, 50.0),
        },
    );
    step(&mut app);
    assert!(app.world().get::<MoveTarget>(a_unit).is_none());
    assert!(app.world().get::<MoveTarget>(b_unit).is_none());
    assert!(
        app.world().get::<MoveTarget>(bare).is_some(),
        "the unowned entity the order actually named was not commanded"
    );
}

/// An order whose named entities have all died is inert, signed or not, and
/// leaves the derivation with nothing to disagree about.
#[test]
fn an_order_whose_named_entities_died_is_inert() {
    let mut app = sim_app();
    let a_unit = spawn_unit(&mut app, "ripper", Faction::A, Vec2::ZERO);
    let ghost_a = spawn_unit(&mut app, "ripper", Faction::A, Vec2::new(-600.0, 0.0));
    let ghost_b = spawn_unit(&mut app, "ripper", Faction::B, Vec2::new(600.0, 0.0));
    app.world_mut().entity_mut(ghost_a).despawn();
    app.world_mut().entity_mut(ghost_b).despawn();
    push(
        &mut app,
        Order::MoveTo {
            units: vec![ghost_a, ghost_b],
            dest: Vec2::new(9.0, 9.0),
        },
    );
    push(
        &mut app,
        Order::MoveTo {
            units: vec![ghost_b],
            dest: Vec2::new(9.0, 9.0),
        }
        .issued_by(Faction::A),
    );
    step(&mut app);
    assert!(
        app.world().get::<MoveTarget>(a_unit).is_none(),
        "an order naming only dead entities reached a live one"
    );
}

/// A signed order may name the enemy, but it commands only the signer — checked
/// on both entity-list variants, so `Gather` is not the looser of the two.
#[test]
fn a_signed_order_commands_only_the_signers_units_in_both_variants() {
    let mut app = sim_app();
    let mine = spawn_unit(&mut app, "worker", Faction::A, Vec2::ZERO);
    let theirs = spawn_unit(&mut app, "worker", Faction::B, Vec2::new(600.0, 0.0));
    let node_pos = Vec2::new(-200.0, 0.0);
    let node = app
        .world_mut()
        .spawn((Position(node_pos), ResourceNode { amount: 500 }))
        .id();
    push(
        &mut app,
        Order::Gather {
            units: vec![theirs, mine],
            node,
            node_pos,
        }
        .issued_by(Faction::A),
    );
    step(&mut app);
    assert!(
        app.world().get::<GatherTarget>(mine).is_some(),
        "the signer's own worker was not tasked"
    );
    assert!(
        app.world().get::<GatherTarget>(theirs).is_none(),
        "a signed order tasked the enemy's worker"
    );
    assert!(
        app.world().get::<MoveTarget>(theirs).is_none(),
        "a signed order moved the enemy's worker"
    );
}

/// No order may spend or train on another faction's account. `Train` is the
/// sharp one: it charges the *building's* faction, so a signature that does not
/// match must cost the target nothing at all.
#[test]
fn no_order_spends_or_trains_on_another_factions_account() {
    let mut app = sim_app_with_alloy(10_000);
    let a_hq = spawn_building(&mut app, "hq", Faction::A, Vec2::ZERO);
    let b_hq = spawn_building(&mut app, "hq", Faction::B, Vec2::new(2_000.0, 0.0));
    let worker = content().unit_index("worker").unwrap();
    let before = (alloy(&app, Faction::A), alloy(&app, Faction::B));
    push(
        &mut app,
        Order::Train {
            building: b_hq,
            unit: worker,
        }
        .issued_by(Faction::A),
    );
    push(
        &mut app,
        Order::Place {
            faction: Faction::B,
            building: content().building_index("foundry").unwrap(),
            pos: Vec2::new(2_100.0, 0.0),
        }
        .issued_by(Faction::A),
    );
    step(&mut app);
    assert_eq!(
        (alloy(&app, Faction::A), alloy(&app, Faction::B)),
        before,
        "a cross-faction order moved Alloy"
    );
    let queued = app
        .world()
        .get::<ProductionQueue>(b_hq)
        .map(|q| q.items.len())
        .unwrap_or(0);
    assert_eq!(queued, 0, "A filled B's production queue");
    let _ = a_hq;
}

fn alloy(app: &App, f: Faction) -> u32 {
    app.world().resource::<Stockpiles>().alloy(f)
}

// ============================================================================
// M4c critic, pass 3 — the reordered sweep (`repair_gather_claims` first)
// ============================================================================

/// Every entity currently holding exactly one half of the claim.
fn half_claims(app: &mut App) -> Vec<Entity> {
    let mut q = app
        .world_mut()
        .query_filtered::<Entity, onus::sim::economy::SplitClaim>();
    let mut v: Vec<Entity> = q.iter(app.world()).collect();
    v.sort_unstable_by_key(|e| e.to_bits());
    v
}

fn split_the_claim(app: &mut App, e: Entity) {
    app.world_mut().entity_mut(e).remove::<GatherPhase>();
}

/// A one-sided AI fixture with a deposit, an HQ and workers.
fn ai_solo(seed: u64) -> (App, Entity) {
    let mut app = sim_app_with_alloy(content().economy.starting_alloy);
    spawn_building(&mut app, "hq", Faction::A, Vec2::ZERO);
    let node = app
        .world_mut()
        .spawn((
            Position(Vec2::new(200.0, 0.0)),
            ResourceNode { amount: 100_000 },
        ))
        .id();
    for i in 0..3 {
        spawn_unit(&mut app, "worker", Faction::A, Vec2::new(0.0, 20.0 * i as f32));
    }
    app.insert_resource(AiCommanders::new(seed, &[Faction::A]));
    (app, node)
}

/// **The sweep's guarantee must not depend on *when* in the think interval the
/// claim breaks.** F-008's third recurrence was a phase relationship: a half
/// claim that landed just before the commander's think tick was read as a live
/// job. Split at every offset within two whole think intervals; on the very next
/// tick no half claim may exist anywhere, and the worker must be back at work
/// within one think interval.
#[test]
fn no_reader_sees_a_half_claim_at_any_offset_in_the_think_interval() {
    let interval = content().ai.think_interval_ticks;
    for offset in 0..(2 * interval) {
        let (mut app, _node) = ai_solo(7);
        tick(&mut app, offset);
        // Pick a worker the AI has already tasked, and break its claim.
        let victim = {
            let mut q = app.world_mut().query_filtered::<Entity, With<GatherTarget>>();
            let mut v: Vec<Entity> = q.iter(app.world()).collect();
            v.sort_unstable_by_key(|e| e.to_bits());
            v.first().copied()
        };
        let Some(victim) = victim else { continue };
        split_the_claim(&mut app, victim);
        step(&mut app);
        assert!(
            half_claims(&mut app).is_empty(),
            "offset {offset}: a half claim survived the tick after the split"
        );
        // And the worker is not stranded: within one more think interval it is
        // either re-tasked or given something to do.
        let mut employed = false;
        for _ in 0..(interval + 2) {
            if app.world().get::<GatherTarget>(victim).is_some() {
                employed = true;
                break;
            }
            step(&mut app);
            assert!(
                half_claims(&mut app).is_empty(),
                "offset {offset}: a half claim appeared while re-employing"
            );
        }
        assert!(
            employed,
            "offset {offset}: the worker was left idle for longer than a think interval"
        );
    }
}

/// **A half claim on something that is not a gatherer is still a half claim.**
/// The sweep is filtered on the components alone, so a lone `GatherTarget`
/// parked on a building, a resource node or a soldier must be gone before any
/// reader runs — and must not panic anything on the way.
#[test]
fn a_half_claim_on_a_building_node_or_soldier_is_swept() {
    let mut app = sim_app();
    let hq = spawn_building(&mut app, "hq", Faction::A, Vec2::ZERO);
    let node = app
        .world_mut()
        .spawn((Position(Vec2::new(200.0, 0.0)), ResourceNode { amount: 500 }))
        .id();
    let soldier = spawn_unit(&mut app, "bulwark", Faction::A, Vec2::new(10.0, 0.0));
    for e in [hq, node, soldier] {
        app.world_mut()
            .entity_mut(e)
            .insert((GatherTarget(node), GatherPhase::ToNode));
        app.world_mut().entity_mut(e).remove::<GatherPhase>();
    }
    // And the mirror shape: a lone phase.
    let lone_phase = spawn_unit(&mut app, "worker", Faction::A, Vec2::new(0.0, 40.0));
    app.world_mut()
        .entity_mut(lone_phase)
        .insert((GatherTarget(node), GatherPhase::ToNode));
    app.world_mut().entity_mut(lone_phase).remove::<GatherTarget>();

    step(&mut app);
    assert!(
        half_claims(&mut app).is_empty(),
        "a half claim on a non-gatherer survived a tick"
    );
    for e in [hq, node, soldier, lone_phase] {
        assert!(
            app.world().get::<GatherTarget>(e).is_none() && app.world().get::<GatherPhase>(e).is_none(),
            "{e:?}: the claim was not dropped as a pair"
        );
    }
}

/// **A half claim must not disarm a soldier for even one tick.** Combat reads
/// `GatherTarget` as "the economy owns this unit"; the sweep runs ahead of
/// combat, so a soldier carrying a lone target at the tick boundary must still
/// fire on that very tick.
#[test]
fn a_half_claim_does_not_cost_a_soldier_a_single_tick_of_fire() {
    let mut app = sim_app();
    spawn_building(&mut app, "hq", Faction::A, Vec2::new(-400.0, 0.0));
    spawn_building(&mut app, "hq", Faction::B, Vec2::new(400.0, 0.0));
    let node = app
        .world_mut()
        .spawn((Position(Vec2::new(0.0, 300.0)), ResourceNode { amount: 10 }))
        .id();
    let atk = spawn_unit(&mut app, "bulwark", Faction::A, Vec2::new(0.0, 0.0));
    let def = spawn_unit(&mut app, "bulwark", Faction::B, Vec2::new(8.0, 0.0));
    app.world_mut()
        .entity_mut(atk)
        .insert((GatherTarget(node), GatherPhase::ToNode));
    app.world_mut().entity_mut(atk).remove::<GatherPhase>();
    let before = hp(&app, def).unwrap();

    step(&mut app);

    assert!(
        half_claims(&mut app).is_empty(),
        "the half claim survived the tick"
    );
    assert!(
        hp(&app, def).unwrap() < before,
        "a lone `GatherTarget` disarmed a soldier for a tick: enemy hp {before} unchanged"
    );
}

/// **Alloy is never destroyed by the sweep.** A claim split while the worker is
/// holding a load must keep the load (it is "in flight"), and banked + carried +
/// still-in-the-ground must equal the starting total on *every* tick.
#[test]
fn splitting_a_claim_conserves_alloy_every_tick() {
    use onus::sim::economy::Carrying;
    let mut app = sim_app();
    let node_pos = Vec2::new(120.0, 0.0);
    let node = app
        .world_mut()
        .spawn((Position(node_pos), ResourceNode { amount: 2_000 }))
        .id();
    spawn_building(&mut app, "hq", Faction::A, Vec2::ZERO);
    let w = spawn_unit(&mut app, "worker", Faction::A, Vec2::ZERO);
    push(
        &mut app,
        Order::Gather {
            units: vec![w],
            node,
            node_pos,
        }
        .issued_by(Faction::A),
    );
    let total = 2_000u32;
    for t in 0..900u32 {
        // Break the claim once per round trip, so the cycle still completes.
        if t % 120 == 3 && app.world().get::<GatherPhase>(w).is_some() {
            split_the_claim(&mut app, w);
        }
        let carried_before = app.world().get::<Carrying>(w).map(|c| c.0).unwrap_or(0);
        let banked_before = alloy(&app, Faction::A);
        step(&mut app);
        let carried = app.world().get::<Carrying>(w).map(|c| c.0).unwrap_or(0);
        let banked = alloy(&app, Faction::A);
        assert!(
            carried >= carried_before || banked - banked_before == carried_before - carried,
            "tick {t}: a carried load vanished without being banked \
             ({carried_before} -> {carried}, banked {banked_before} -> {banked})"
        );
        let in_ground = app
            .world()
            .get::<ResourceNode>(node)
            .map(|n| n.amount)
            .unwrap_or(0);
        assert_eq!(
            banked + carried + in_ground,
            total,
            "tick {t}: Alloy was created or destroyed (banked {banked}, carried {carried}, ground {in_ground})"
        );
        assert!(
            half_claims(&mut app).is_empty(),
            "tick {t}: a half claim survived into the readers"
        );
        // Re-task if the sweep confiscated: a split claim is not the economy's.
        if app.world().get::<GatherTarget>(w).is_none() {
            push(
                &mut app,
                Order::Gather {
                    units: vec![w],
                    node,
                    node_pos,
                }
                .issued_by(Faction::A),
            );
        }
    }
    assert!(alloy(&app, Faction::A) > 0, "nothing was ever banked");
}

/// **The sweep must be inert in a clean match.** No system in `src/` splits a
/// claim, so across a whole AI-vs-AI match the sweep's query must never match:
/// that is what makes moving it to the head of the chain a no-op for every other
/// system, rather than a behaviour change hidden inside a fix.
#[test]
fn the_sweep_never_fires_in_a_clean_ai_match() {
    for seed in [2u64, 13, 1234] {
        let mut app = ai_vs_ai(seed);
        for t in 0..3_000u32 {
            step(&mut app);
            let broken = half_claims(&mut app);
            assert!(
                broken.is_empty(),
                "seed {seed} tick {t}: the sim itself produced a half claim {broken:?}"
            );
            if outcome(&app).is_some() {
                break;
            }
        }
    }
}

/// **The doc is a claim about the code, so check it.** `repair_gather_claims`
/// documents that the readers of the gather claim are exactly three
/// (`ai_commanders`, `gather`, `combat`), all ordered after it. A fourth reader
/// anywhere in `src/` — or the same three moving — silently voids the guarantee.
#[test]
fn the_only_readers_of_the_gather_claim_are_the_three_the_doc_names() {
    use std::fs;
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    // M5 adds a fourth file that names the claim: `sim/replay.rs`'s canonical
    // state hash, which *records* both halves rather than acting on either.
    //
    // The allowlist is file-granular, so admitting a file says nothing about
    // where its systems run — which is the whole of what F-008 needs. The
    // ordering assertion below is therefore **system-granular and general**: it
    // walks the chain in `src/lib.rs` and requires that *no* system registered
    // before the sweep, in any file, so much as names either half of the claim.
    // That is what earns `sim/replay.rs` its place here, and it holds for every
    // system in it (and in every other admitted file) rather than for an
    // enumerated few.
    let allowed = [
        "sim/economy.rs",
        "sim/ai.rs",
        "sim/combat.rs",
        "sim/mod.rs",
        "sim/replay.rs",
    ];
    let mut offenders: Vec<String> = Vec::new();
    let mut stack = vec![src.clone()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).expect("read src") {
            let p = entry.expect("entry").path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            if p.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let rel = p
                .strip_prefix(&src)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let text = fs::read_to_string(&p).expect("read");
            for (i, line) in text.lines().enumerate() {
                let code = line.trim_start();
                if code.starts_with("//") {
                    continue;
                }
                let reads = code.contains("&GatherTarget")
                    || code.contains("&GatherPhase")
                    || code.contains("With<GatherTarget>")
                    || code.contains("With<GatherPhase>");
                if reads && !allowed.contains(&rel.as_str()) {
                    offenders.push(format!("{rel}:{}: {}", i + 1, code));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "a reader of the gather claim lives outside the files the doc names \
         (and outside the sweep's protection): {offenders:#?}"
    );

    // The ordering that earns the allowlist its entries, checked over every
    // system the chain actually registers rather than over a list written by
    // hand: nothing that runs before the sweep may name the claim.
    let pre = systems_registered_before_the_sweep();
    assert!(
        !pre.is_empty(),
        "no system is registered before the sweep, so this check inspects \
         nothing and would pass any change"
    );
    for (system, body) in pre {
        for claim in CLAIM_SPELLINGS {
            assert!(
                !body.contains(claim),
                "`{system}` is registered before `repair_gather_claims` and \
                 names `{claim}`: a half claim reaches it (F-008)"
            );
        }
    }
    // Run conditions are evaluated before the systems they gate, so a condition
    // that read the claim would read it ahead of the sweep wherever it is
    // written.
    for (_, path) in chain_fn_paths() {
        let lib = std::fs::read_to_string(src.join("lib.rs")).expect("read src/lib.rs");
        if !lib.contains(&format!("run_if({path})")) {
            continue;
        }
        let body = resolve_sim_fn(&path).unwrap_or_else(|e| panic!("{e}"));
        for claim in CLAIM_SPELLINGS {
            assert!(
                !body.contains(claim),
                "the run condition `{path}` names `{claim}`: a condition is \
                 evaluated before the sweep it gates (F-008)"
            );
        }
    }
}


/// Resolve a chain-registered `sim::…` path to the source of its function body.
///
/// **Every failure to resolve is an error, never a skip.** The rule this guard
/// enforces reads as comprehensive ("no system before the sweep names the
/// claim"), and a walker that quietly `continue`s past a name it cannot find is
/// a walker that waves that system through — which is exactly what happens the
/// day someone moves it ahead of the sweep. So a name that does not resolve to
/// exactly one definition fails the test by name.
///
/// The lookup is by *definition*, not by module path: a system may be
/// re-exported (`pub use victory::match_running` makes `sim::match_running`
/// legal) or live in a nested submodule, so every `.rs` file under `src/sim/`
/// is searched and exactly one `pub fn <name>(` must be found.
fn resolve_sim_fn(path: &str) -> Result<String, String> {
    let src = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let name = path.rsplit("::").next().unwrap_or_default();
    if name.is_empty() {
        return Err(format!("{path}: not a function path"));
    }
    let needle = format!("pub fn {name}(");
    let mut found: Vec<(String, String)> = Vec::new();
    let mut stack = vec![src.join("sim")];
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).map_err(|e| format!("{path}: read dir: {e}"))? {
            let p = entry.map_err(|e| format!("{path}: {e}"))?.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|e| e == "rs") {
                files.push(p);
            }
        }
    }
    files.sort();
    for file in files {
        let text = std::fs::read_to_string(&file).map_err(|e| format!("{path}: {e}"))?;
        let Some(at) = text.find(&needle) else {
            continue;
        };
        let rest = &text[at..];
        // The body runs to the first line that is a bare `}` at column 0.
        let end = rest[1..]
            .find("\n}\n")
            .map(|i| i + 3)
            .ok_or_else(|| {
                format!(
                    "{path}: `{needle}` in {} has no closing `}}` at column 0, so \
                     the guard cannot read the whole body",
                    file.display()
                )
            })?;
        found.push((file.display().to_string(), rest[..end].to_string()));
    }
    match found.len() {
        0 => Err(format!(
            "{path}: no `{needle}` anywhere under src/sim — the guard cannot \
             read this system, so it cannot vouch for it"
        )),
        1 => Ok(found.pop().expect("one").1),
        _ => Err(format!(
            "{path}: `{needle}` is defined in {} places ({:?}) — the guard \
             cannot tell which one the chain runs",
            found.len(),
            found.iter().map(|(f, _)| f).collect::<Vec<_>>()
        )),
    }
}

/// Every `sim::…` function path the chain in `src/lib.rs` names, in source
/// order, paired with its byte offset. Upper-camel names are resource/type
/// paths (`init_resource::<sim::X>`, `resource_exists::<sim::X>`), not systems.
fn chain_fn_paths() -> Vec<(usize, String)> {
    let lib = std::fs::read_to_string(
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs"),
    )
    .expect("read src/lib.rs");
    let mut out = Vec::new();
    let mut at = 0usize;
    while let Some(i) = lib[at..].find("sim::") {
        let start = at + i;
        let rest = &lib[start..];
        let end = rest
            .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == ':'))
            .unwrap_or(rest.len());
        let path = &rest[..end];
        at = start + end;
        let name = path.rsplit("::").next().unwrap_or_default();
        if name.is_empty() || name.starts_with(|c: char| c.is_uppercase()) {
            continue;
        }
        out.push((start, path.to_string()));
    }
    out
}

/// The gather claim, in the spellings a system would use to read it.
const CLAIM_SPELLINGS: [&str; 3] = ["GatherTarget", "GatherPhase", "SplitClaim"];

/// Every system the chain registers **before** `economy::repair_gather_claims`,
/// paired with the source of its body.
fn systems_registered_before_the_sweep() -> Vec<(String, String)> {
    let lib = std::fs::read_to_string(
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs"),
    )
    .expect("read src/lib.rs");
    let sweep = lib
        .find("sim::economy::repair_gather_claims")
        .expect("the sweep is in the chain");
    chain_fn_paths()
        .into_iter()
        .filter(|(at, _)| *at < sweep)
        .map(|(_, path)| {
            let body = resolve_sim_fn(&path).unwrap_or_else(|e| panic!("{e}"));
            (path, body)
        })
        .collect()
}

/// **Training still charges exactly once with the sweep at the head of the
/// chain.** Reordering systems is exactly how a charge gets applied twice or
/// zero times; assert the Alloy trace per tick, not just the end state.
#[test]
fn training_charges_exactly_once_with_the_sweep_first() {
    let cost = {
        let c = content();
        c.units[c.unit_index("worker").unwrap()].mvp_alloy_cost
    };
    let mut app = sim_app_with_alloy(1_000);
    let hq = spawn_building(&mut app, "hq", Faction::A, Vec2::ZERO);
    push(
        &mut app,
        Order::Train {
            building: hq,
            unit: content().unit_index("worker").unwrap(),
        }
        .issued_by(Faction::A),
    );
    let mut drops = 0u32;
    let mut prev = alloy(&app, Faction::A);
    for _ in 0..600 {
        step(&mut app);
        let now = alloy(&app, Faction::A);
        if now < prev {
            drops += 1;
            assert_eq!(prev - now, cost, "a charge that was not the unit's cost");
        }
        assert!(now <= prev, "Alloy appeared from nowhere");
        prev = now;
    }
    assert_eq!(drops, 1, "the unit was charged {drops} times, not once");
}

/// **The ≤8 minute target, over a wider seed sweep than the suite's.** A
/// scheduling change that starved the economy would show up here first.
#[test]
fn every_seed_decides_inside_eight_minutes_on_a_dead_hq() {
    const BUDGET: u32 = 8 * 60 * 60;
    let victory_def = content().building_index("hq").unwrap();
    for seed in [1u64, 5, 7, 9, 17, 42, 99, 256, 777, 4_242, 31_337, 65_535] {
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
        let standing: Vec<Faction> = {
            let mut q = app.world_mut().query::<(&Building, &Faction)>();
            q.iter(app.world())
                .filter(|(b, _)| b.def == victory_def)
                .map(|(_, f)| *f)
                .collect()
        };
        assert_eq!(standing, vec![winner], "seed {seed}: the win was not earned");
        assert!(o.tick <= BUDGET, "seed {seed}: decided at tick {}", o.tick);
    }
}

/// **Determinism, per tick, over a long match, with the sweep in its new slot.**
/// Same seed: identical state hash on every tick. Different seed: divergence.
#[test]
fn the_reordered_chain_is_still_deterministic_tick_for_tick() {
    let mut a = ai_vs_ai(21);
    let mut b = ai_vs_ai(21);
    let mut c = ai_vs_ai(22);
    let mut diverged = false;
    for t in 0..2_000u32 {
        step(&mut a);
        step(&mut b);
        step(&mut c);
        assert_eq!(
            state_hash(&mut a),
            state_hash(&mut b),
            "tick {t}: the same seed produced two different worlds"
        );
        if state_hash(&mut a) != state_hash(&mut c) {
            diverged = true;
        }
        if outcome(&a).is_some() && outcome(&b).is_some() {
            break;
        }
    }
    assert!(diverged, "two different seeds played an identical match");
}

/// **A claim split on the tick its holder dies.** The sweep now runs before
/// combat, so it queues a removal against a unit that is despawned later in the
/// same tick — the classic use-after-despawn shape.
#[test]
fn a_half_claim_on_a_unit_that_dies_this_tick_is_not_fatal() {
    let mut app = sim_app();
    spawn_building(&mut app, "hq", Faction::A, Vec2::new(-400.0, 0.0));
    spawn_building(&mut app, "hq", Faction::B, Vec2::new(400.0, 0.0));
    let node = app
        .world_mut()
        .spawn((Position(Vec2::new(0.0, 400.0)), ResourceNode { amount: 10 }))
        .id();
    let victim = spawn_unit(&mut app, "worker", Faction::A, Vec2::ZERO);
    for i in 0..6 {
        spawn_unit(&mut app, "ravager", Faction::B, Vec2::new(6.0, i as f32));
    }
    app.world_mut()
        .entity_mut(victim)
        .insert((GatherTarget(node), GatherPhase::ToNode));
    app.world_mut().entity_mut(victim).remove::<GatherPhase>();
    // Also re-split it every tick for as long as it lives.
    for _ in 0..600 {
        if app.world().get_entity(victim).is_ok() && app.world().get::<GatherPhase>(victim).is_some()
        {
            split_the_claim(&mut app, victim);
        }
        step(&mut app);
        if app.world().get_entity(victim).is_err() {
            return;
        }
    }
    panic!("fixture: the victim never died, so the race was never run");
}

/// **The sweep must not run after the match is recorded.** It is inside the
/// play chain; once the outcome is written nothing the sim owns may change,
/// half claim or not.
#[test]
fn a_half_claim_present_at_match_end_changes_nothing_afterwards() {
    let mut app = sim_app();
    spawn_building(&mut app, "hq", Faction::A, Vec2::new(-400.0, 0.0));
    let b_hq = spawn_building(&mut app, "hq", Faction::B, Vec2::new(400.0, 0.0));
    let node = app
        .world_mut()
        .spawn((Position(Vec2::new(0.0, 300.0)), ResourceNode { amount: 10 }))
        .id();
    let w = spawn_unit(&mut app, "worker", Faction::A, Vec2::new(-380.0, 0.0));
    step(&mut app); // engage the latch on a contested field
    app.world_mut().entity_mut(b_hq).despawn();
    step(&mut app);
    assert!(outcome(&app).is_some(), "fixture: the match never ended");
    app.world_mut()
        .entity_mut(w)
        .insert((GatherTarget(node), GatherPhase::ToNode));
    app.world_mut().entity_mut(w).remove::<GatherPhase>();
    let before = state_hash(&mut app);
    tick(&mut app, 120);
    assert_eq!(
        state_hash(&mut app),
        before,
        "the sim kept running after the outcome was recorded"
    );
    assert!(
        app.world().get::<GatherTarget>(w).is_some(),
        "the sweep ran after the match was decided"
    );
}

/// **Targeting still matches the M2 brute-force oracle with half claims on the
/// field.** The sweep clears them first, so every unit is a combatant and the
/// choice must be the naive nearest enemy.
#[test]
fn targeting_matches_the_oracle_with_half_claims_on_the_field() {
    let mut app = sim_app();
    let node = app
        .world_mut()
        .spawn((Position(Vec2::new(0.0, 900.0)), ResourceNode { amount: 10 }))
        .id();
    let mut placed: Vec<(Entity, Vec2, Faction)> = Vec::new();
    for i in 0..8 {
        let fa = Vec2::new(-40.0 + 9.0 * i as f32, 3.0 * i as f32);
        let fb = Vec2::new(45.0 - 7.0 * i as f32, -4.0 * i as f32);
        let a = spawn_unit(&mut app, "bulwark", Faction::A, fa);
        let b = spawn_unit(&mut app, "ripper", Faction::B, fb);
        placed.push((a, fa, Faction::A));
        placed.push((b, fb, Faction::B));
        if i % 2 == 0 {
            for e in [a, b] {
                app.world_mut()
                    .entity_mut(e)
                    .insert((GatherTarget(node), GatherPhase::ToNode));
                app.world_mut().entity_mut(e).remove::<GatherPhase>();
            }
        }
    }
    step(&mut app);
    assert!(half_claims(&mut app).is_empty(), "half claims survived");

    let mut rows: Vec<(Entity, Vec2, Faction)> = placed.clone();
    rows.sort_unstable_by_key(|(e, _, _)| e.to_bits());
    let layout: Vec<Unit> = rows
        .iter()
        .map(|(_, p, f)| Unit {
            pos: *p,
            faction: *f,
        })
        .collect();
    for (i, (e, _, _)) in rows.iter().enumerate() {
        let want = brute_force_nearest_enemy(&layout, i).map(|j| rows[j].0);
        let got = app.world().get::<Target>(*e).map(|t| t.0);
        assert_eq!(
            got, want,
            "{e:?}: target disagreed with the brute-force oracle after a sweep"
        );
    }
}

/// **Two commanders touching the same worker on the same tick.** The sweep now
/// runs *before* `apply_commands`, so a claim written this tick is only swept
/// next tick; the argument for that is "every writer writes the pair". Push
/// every same-tick combination of Gather/Move/Gather-again at one worker and
/// assert the claim is a pair (or absent) at every tick boundary — never a half.
#[test]
fn same_tick_orders_from_two_sources_never_leave_a_half_claim() {
    let node_pos = Vec2::new(150.0, 0.0);
    let combos: Vec<Vec<u8>> = vec![
        vec![0],
        vec![1],
        vec![0, 1],
        vec![1, 0],
        vec![0, 0],
        vec![0, 1, 0],
        vec![1, 0, 1],
    ];
    for (i, combo) in combos.iter().enumerate() {
        let mut app = sim_app_with_alloy(500);
        let node = app
            .world_mut()
            .spawn((Position(node_pos), ResourceNode { amount: 1_000 }))
            .id();
        spawn_building(&mut app, "hq", Faction::A, Vec2::ZERO);
        let w = spawn_unit(&mut app, "worker", Faction::A, Vec2::ZERO);
        for t in 0..240u32 {
            if t % 20 == 0 {
                for kind in combo {
                    let o = match kind {
                        0 => Order::Gather {
                            units: vec![w],
                            node,
                            node_pos,
                        },
                        _ => Order::MoveTo {
                            units: vec![w],
                            dest: Vec2::new(-100.0, 0.0),
                        },
                    };
                    push(&mut app, o.issued_by(Faction::A));
                }
            }
            step(&mut app);
            let has_t = app.world().get::<GatherTarget>(w).is_some();
            let has_p = app.world().get::<GatherPhase>(w).is_some();
            assert_eq!(
                has_t, has_p,
                "combo {i} tick {t}: the claim was observed split (target={has_t}, phase={has_p})"
            );
            assert!(
                half_claims(&mut app).is_empty(),
                "combo {i} tick {t}: a half claim exists somewhere on the field"
            );
        }
    }
}
