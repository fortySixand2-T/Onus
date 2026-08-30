//! L2 integration tests for M4c (scripted AI + win condition).
//!
//! Encodes the M4c acceptance criteria, the critic probes, and the three items
//! BUILD_PLAN.md inherited into this milestone:
//!   AC1 — a scripted AI gathers, builds a Barracks, trains a mixed force and
//!         attack-moves, on a **tick** timer, deterministic given the seed;
//!   AC2 — destroying the enemy HQ ends the match (≤ ~8 min = 28_800 ticks);
//!   inherited — orders carry their issuer (cross-faction orders are refused),
//!         orders against despawned entities never panic, and a gather claim is
//!         never written or released without its phase.
//!
//! Everything runs headless (`MinimalPlugins`) through the *shipped* sim chain
//! (`onus::add_sim_systems`) — no render types, no hand-rolled system list
//! (F-004), one fixed timestep handed to the sim per step (F-003).

use std::path::PathBuf;

use bevy::prelude::*;

use onus::sim::combat::{Casualties, Health};
use onus::sim::content::Content;
use onus::sim::economy::{Building, ProductionQueue, Stockpiles, UnitDefIdx};
use onus::sim::spatial::Faction;
use onus::sim::{
    AiAction, AiCommanders, AiJournal, CommandQueue, GatherPhase, GatherTarget, MoveTarget, Order,
    Position, RateReport, ResourceNode,
};

// ---- harness ----------------------------------------------------------------

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data")
}

fn content() -> Content {
    Content::load_from_dir(&data_dir()).expect("assets/data/*.ron parse into sim structs")
}

/// A headless app running the shipped sim chain on `Update`, so one
/// `step()` == exactly one 60 Hz sim tick.
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

fn spawn_building(app: &mut App, id: &str, faction: Faction, pos: Vec2) -> Entity {
    let def = {
        let c = app.world().resource::<Content>();
        c.building_index(id)
            .unwrap_or_else(|| panic!("building `{id}`"))
    };
    app.world_mut()
        .spawn((
            Position(pos),
            Building { def },
            faction,
            ProductionQueue::default(),
        ))
        .id()
}

fn alloy(app: &App, f: Faction) -> u32 {
    app.world().resource::<Stockpiles>().alloy(f)
}

fn unit_index(app: &App, id: &str) -> usize {
    app.world().resource::<Content>().unit_index(id).unwrap()
}

fn queued(app: &App, building: Entity) -> usize {
    app.world()
        .get::<ProductionQueue>(building)
        .map(|q| q.items.len())
        .unwrap_or(0)
}

// ---- inherited 1: orders carry their issuer ---------------------------------

/// `Order::Train` used to charge the *targeted building's* faction, whoever
/// issued it. With a second commander on the field that is a way to spend the
/// enemy's Alloy (and fill their queue). A signed order against someone else's
/// building must be refused outright.
#[test]
fn a_train_order_against_another_factions_building_is_refused() {
    let mut app = sim_app_with_alloy(1_000);
    let hq = spawn_building(&mut app, "hq", Faction::B, Vec2::ZERO);
    let worker = unit_index(&app, "worker");
    push(
        &mut app,
        Order::Train {
            building: hq,
            unit: worker,
        }
        .issued_by(Faction::A),
    );
    step(&mut app);
    assert_eq!(alloy(&app, Faction::B), 1_000, "B paid for A's order");
    assert_eq!(
        alloy(&app, Faction::A),
        1_000,
        "A was charged for a refusal"
    );
    assert_eq!(queued(&app, hq), 0, "A queued a unit in B's HQ");
}

/// The property the ownership check could break: a commander's order against
/// its **own** building still trains, and is charged exactly once.
#[test]
fn a_train_order_against_ones_own_building_still_trains() {
    let mut app = sim_app_with_alloy(1_000);
    let hq = spawn_building(&mut app, "hq", Faction::B, Vec2::ZERO);
    let worker = unit_index(&app, "worker");
    let cost = app.world().resource::<Content>().units[worker].mvp_alloy_cost;
    push(
        &mut app,
        Order::Train {
            building: hq,
            unit: worker,
        }
        .issued_by(Faction::B),
    );
    step(&mut app);
    assert_eq!(queued(&app, hq), 1, "the owner's own order was refused");
    assert_eq!(alloy(&app, Faction::B), 1_000 - cost);
}

/// Movement orders are ownership-checked per unit: a mixed list commands only
/// the issuer's own units, and leaves the enemy's alone.
#[test]
fn a_move_order_commands_only_the_issuers_own_units() {
    let mut app = sim_app();
    let mine = spawn_unit(&mut app, "ripper", Faction::A, Vec2::ZERO);
    let theirs = spawn_unit(&mut app, "ripper", Faction::B, Vec2::new(1_000.0, 0.0));
    push(
        &mut app,
        Order::MoveTo {
            units: vec![mine, theirs],
            dest: Vec2::new(500.0, 500.0),
        }
        .issued_by(Faction::A),
    );
    step(&mut app);
    assert!(
        app.world().get::<MoveTarget>(mine).is_some(),
        "the issuer's own unit ignored the order"
    );
    assert!(
        app.world().get::<MoveTarget>(theirs).is_none(),
        "a commander moved the enemy's unit"
    );
}

/// Same for `Order::Gather`: neither half of the order (the claim or the move)
/// may land on another faction's worker.
#[test]
fn a_gather_order_never_tasks_another_factions_worker() {
    let mut app = sim_app();
    let node_pos = Vec2::new(100.0, 0.0);
    let node = app
        .world_mut()
        .spawn((Position(node_pos), ResourceNode { amount: 500 }))
        .id();
    let theirs = spawn_unit(&mut app, "worker", Faction::B, Vec2::ZERO);
    push(
        &mut app,
        Order::Gather {
            units: vec![theirs],
            node,
            node_pos,
        }
        .issued_by(Faction::A),
    );
    step(&mut app);
    assert!(app.world().get::<GatherTarget>(theirs).is_none());
    assert!(app.world().get::<MoveTarget>(theirs).is_none());
}

/// `Order::Place` names the faction that gets the building; it must be the
/// faction that signed the order, or nothing is placed and nothing is charged.
#[test]
fn a_place_order_cannot_build_for_another_faction() {
    let mut app = sim_app_with_alloy(1_000);
    let foundry = app
        .world()
        .resource::<Content>()
        .building_index("foundry")
        .unwrap();
    push(
        &mut app,
        Order::Place {
            faction: Faction::B,
            building: foundry,
            pos: Vec2::ZERO,
        }
        .issued_by(Faction::A),
    );
    step(&mut app);
    let mut q = app.world_mut().query::<&Building>();
    assert_eq!(q.iter(app.world()).count(), 0, "a building was placed");
    assert_eq!(alloy(&app, Faction::B), 1_000);
    assert_eq!(alloy(&app, Faction::A), 1_000);
}

/// A signature cannot be laundered: an order signed by two different factions is
/// refused rather than resolved to either of them.
#[test]
fn a_doubly_signed_order_is_refused() {
    let mut app = sim_app_with_alloy(1_000);
    let hq = spawn_building(&mut app, "hq", Faction::B, Vec2::ZERO);
    let worker = unit_index(&app, "worker");
    push(
        &mut app,
        Order::Train {
            building: hq,
            unit: worker,
        }
        .issued_by(Faction::B)
        .issued_by(Faction::A),
    );
    step(&mut app);
    assert_eq!(queued(&app, hq), 0, "a re-signed order was executed");
    assert_eq!(alloy(&app, Faction::B), 1_000);
}

// ---- inherited 2: orders against dead entities -------------------------------

/// An AI issues orders against entities it remembered from an earlier tick; by
/// the time the order is applied the entity may be dead. `Commands::entity`
/// panics on a despawned entity — every order path must tolerate it.
#[test]
fn orders_against_a_despawned_entity_are_ignored_not_fatal() {
    let mut app = sim_app();
    let node_pos = Vec2::new(100.0, 0.0);
    let node = app
        .world_mut()
        .spawn((Position(node_pos), ResourceNode { amount: 500 }))
        .id();
    let ghost = spawn_unit(&mut app, "worker", Faction::A, Vec2::ZERO);
    let alive = spawn_unit(&mut app, "worker", Faction::A, Vec2::new(0.0, 10.0));
    app.world_mut().entity_mut(ghost).despawn();
    push(
        &mut app,
        Order::MoveTo {
            units: vec![ghost, alive],
            dest: Vec2::new(50.0, 0.0),
        }
        .issued_by(Faction::A),
    );
    push(
        &mut app,
        Order::Gather {
            units: vec![ghost],
            node,
            node_pos,
        }
        .issued_by(Faction::A),
    );
    let worker = unit_index(&app, "worker");
    push(
        &mut app,
        Order::Train {
            building: ghost,
            unit: worker,
        }
        .issued_by(Faction::A),
    );
    step(&mut app); // must not panic
    assert!(
        app.world().get::<MoveTarget>(alive).is_some(),
        "the live unit in the same order lost its command"
    );
}

// ---- inherited 3: the claim is a pair (F-008) --------------------------------

/// A lone `GatherTarget` disarms a unit forever (combat reads it alone, the
/// economy can only release the pair). The pairing must be structural: writing
/// the claim alone still yields a phase.
#[test]
fn a_gather_claim_can_never_be_written_without_its_phase() {
    let mut app = sim_app();
    let node = app
        .world_mut()
        .spawn((Position(Vec2::ZERO), ResourceNode { amount: 10 }))
        .id();
    let e = spawn_unit(&mut app, "ripper", Faction::A, Vec2::ZERO);
    app.world_mut().entity_mut(e).insert(GatherTarget(node));
    assert!(
        app.world().get::<GatherPhase>(e).is_some(),
        "a claim was written without its phase"
    );
}

/// And the release is a pair too: when the economy drops a job neither half is
/// left behind (here: a soldier that can never gather).
#[test]
fn a_gather_claim_is_always_released_as_a_pair() {
    let mut app = sim_app();
    let node = app
        .world_mut()
        .spawn((Position(Vec2::ZERO), ResourceNode { amount: 500 }))
        .id();
    let soldier = spawn_unit(&mut app, "ripper", Faction::A, Vec2::ZERO);
    app.world_mut()
        .entity_mut(soldier)
        .insert((GatherTarget(node), GatherPhase::ToNode));
    tick(&mut app, 3);
    assert!(app.world().get::<GatherTarget>(soldier).is_none());
    assert!(app.world().get::<GatherPhase>(soldier).is_none());
}

/// The compatibility rule, stated: an **unsigned** order is self-signed — it is
/// attributed to whatever it touches, so it can never be cross-faction, and a
/// fixture that drives one side's economy directly keeps working. (This is what
/// keeps the ownership check from silently disowning `Faction::B` fixtures.)
#[test]
fn an_unsigned_order_is_self_signed() {
    let mut app = sim_app_with_alloy(1_000);
    let hq = spawn_building(&mut app, "hq", Faction::B, Vec2::ZERO);
    let worker = unit_index(&app, "worker");
    let cost = app.world().resource::<Content>().units[worker].mvp_alloy_cost;
    let unit = spawn_unit(&mut app, "ripper", Faction::B, Vec2::new(300.0, 0.0));
    push(
        &mut app,
        Order::Train {
            building: hq,
            unit: worker,
        },
    );
    push(
        &mut app,
        Order::MoveTo {
            units: vec![unit],
            dest: Vec2::ZERO,
        },
    );
    step(&mut app);
    assert_eq!(queued(&app, hq), 1, "an unsigned order was disowned");
    assert_eq!(alloy(&app, Faction::B), 1_000 - cost, "the owner paid");
    assert!(app.world().get::<MoveTarget>(unit).is_some());
}

/// ...and the reason that rule is safe: **nothing in `src/` emits an unsigned
/// order**. Input signs with the player's faction, the AI signs with its own, so
/// every order the shipped game produces is ownership-checked. Source-level,
/// like the "one sim chain" probe: a new unsigned emitter is a test failure.
#[test]
fn every_order_emitted_in_src_is_signed() {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut unsigned = Vec::new();
    let mut files: Vec<PathBuf> = Vec::new();
    let mut stack = vec![src];
    while let Some(dir) = stack.pop() {
        let mut entries: Vec<_> = std::fs::read_dir(&dir)
            .expect("read src")
            .map(|e| e.expect("dir entry").path())
            .collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                files.push(path);
            }
        }
    }
    files.sort();
    for path in files {
        let text = std::fs::read_to_string(&path).expect("read source");
        // Every `push_back(<expr>)` whose expression is an `Order` must sign it.
        // The expression is taken paren-balanced, so this reads the whole
        // pushed value and nothing after it (the sim's other queues — the
        // production queue, the flow-field frontier — push no orders and are
        // simply not `Order` expressions).
        let bytes: Vec<char> = text.chars().collect();
        let mut i = 0;
        while let Some(rel) = text[i..].find("push_back(") {
            let start = i + rel + "push_back(".len();
            let mut depth = 1usize;
            let mut j = start;
            while j < bytes.len() && depth > 0 {
                match bytes[j] {
                    '(' => depth += 1,
                    ')' => depth -= 1,
                    _ => {}
                }
                j += 1;
            }
            let expr = &text[start..j.min(text.len())];
            if expr.contains("Order") && !expr.contains(".issued_by(") {
                unsigned.push(format!("{}: `{}`", path.display(), expr.trim()));
            }
            i = start;
        }
    }
    assert!(
        unsigned.is_empty(),
        "unsigned orders are emitted in src/: {unsigned:?}"
    );
}

// ---- AC1: the scripted AI ---------------------------------------------------

/// A one-sided match fixture: an AI commander for `Faction::A` with an HQ, a
/// deposit and three workers, and a passive enemy HQ to aim at. Everything the
/// AI does from here is its own script.
fn ai_match(seed: u64) -> App {
    let mut app = sim_app_with_alloy(starting_alloy());
    let hq = spawn_building(&mut app, "hq", Faction::A, Vec2::ZERO);
    let _ = hq;
    spawn_building(&mut app, "hq", Faction::B, Vec2::new(1_500.0, 0.0));
    app.world_mut().spawn((
        Position(Vec2::new(250.0, 0.0)),
        ResourceNode { amount: 100_000 },
    ));
    for i in 0..3 {
        spawn_unit(
            &mut app,
            "worker",
            Faction::A,
            Vec2::new(0.0, 20.0 * i as f32),
        );
    }
    app.insert_resource(AiCommanders::new(seed, &[Faction::A]));
    app
}

fn starting_alloy() -> u32 {
    content().economy.starting_alloy
}

fn journal(app: &App, f: Faction) -> Vec<(u32, AiAction)> {
    app.world().resource::<AiJournal>().for_faction(f)
}

fn my_buildings(app: &mut App, faction: Faction, id: &str) -> usize {
    let def = app
        .world()
        .resource::<Content>()
        .building_index(id)
        .unwrap();
    let mut q = app.world_mut().query::<(&Building, &Faction)>();
    q.iter(app.world())
        .filter(|(b, f)| b.def == def && **f == faction)
        .count()
}

fn units_of(app: &mut App, faction: Faction) -> Vec<usize> {
    let mut q = app.world_mut().query::<(&UnitDefIdx, &Faction)>();
    let mut v: Vec<usize> = q
        .iter(app.world())
        .filter(|(_, f)| **f == faction)
        .map(|(d, _)| d.0)
        .collect();
    v.sort_unstable();
    v
}

/// AC1, first clause: the AI mines. Every idle worker it owns is put on the
/// deposit, and the claim it hands out is a *pair* it never loses (per-tick
/// invariant, so a one-tick flicker is a failure, not a rounding error).
#[test]
fn the_ai_puts_its_workers_on_a_deposit_and_banks_alloy() {
    let mut app = ai_match(11);
    let workers: Vec<Entity> = {
        let mut q = app.world_mut().query::<(Entity, &UnitDefIdx)>();
        q.iter(app.world()).map(|(e, _)| e).collect()
    };
    let interval = content().ai.think_interval_ticks;
    tick(&mut app, interval + 1);
    for w in &workers {
        assert!(
            app.world().get::<GatherTarget>(*w).is_some(),
            "a worker was left idle after the first decision"
        );
    }
    // And it stays a pair for the whole run.
    for t in 0..600 {
        step(&mut app);
        for w in &workers {
            let has_target = app.world().get::<GatherTarget>(*w).is_some();
            let has_phase = app.world().get::<GatherPhase>(*w).is_some();
            assert_eq!(has_target, has_phase, "tick {t}: half a claim");
        }
    }
    assert!(
        alloy(&app, Faction::A) > 0,
        "600 ticks of mining banked nothing"
    );
}

/// AC1, second clause: it opens with exactly one Barracks — the one the RON
/// names — and never before the tick the script allows.
#[test]
fn the_ai_builds_one_barracks_and_not_before_its_tick() {
    let script = content().ai;
    let mut app = ai_match(11);
    tick(&mut app, script.barracks_at_tick);
    assert_eq!(
        my_buildings(&mut app, Faction::A, &script.barracks),
        0,
        "the barracks went up before the script's tick"
    );
    tick(&mut app, 4_000);
    assert_eq!(
        my_buildings(&mut app, Faction::A, &script.barracks),
        1,
        "the AI built no barracks, or more than one"
    );
    // The only randomness in the placement is its direction: it lands exactly
    // `barracks_offset` from the HQ.
    let placed = journal(&app, Faction::A)
        .into_iter()
        .find_map(|(_, a)| match a {
            AiAction::PlaceBarracks { pos } => Some(pos),
            _ => None,
        })
        .expect("a barracks was placed");
    assert!(
        (placed.length() - script.barracks_offset).abs() < 0.01,
        "the barracks is {} from the HQ, not {}",
        placed.length(),
        script.barracks_offset
    );
}

/// AC1, third clause: it trains a **mixed** force, in the RON build order.
#[test]
fn the_ai_trains_the_ron_build_order_and_the_force_is_mixed() {
    let c = content();
    let mut app = ai_match(11);
    tick(&mut app, 12_000);
    let trained: Vec<usize> = journal(&app, Faction::A)
        .into_iter()
        .filter_map(|(_, a)| match a {
            AiAction::TrainArmy { unit } => Some(unit),
            _ => None,
        })
        .collect();
    assert!(
        trained.len() >= 4,
        "only {} army orders in 12k ticks",
        trained.len()
    );
    for (n, unit) in trained.iter().enumerate() {
        let want = c.ai.army_at(n as u32).and_then(|id| c.unit_index(id));
        assert_eq!(Some(*unit), want, "build order diverged at position {n}");
    }
    let force = units_of(&mut app, Faction::A);
    let kinds: std::collections::BTreeSet<usize> = force
        .iter()
        .copied()
        .filter(|u| c.units[*u].offense > 0)
        .collect();
    assert!(
        kinds.len() >= 2,
        "the force is not mixed: {kinds:?} (units {force:?})"
    );
}

/// AC1, fourth clause: it attack-moves at the enemy HQ — the force is given the
/// destination and actually closes on it.
#[test]
fn the_ai_attack_moves_its_force_at_the_enemy_hq() {
    let mut app = ai_match(11);
    let mut attack = None;
    for _ in 0..12_000 {
        step(&mut app);
        if let Some(a) = journal(&app, Faction::A)
            .into_iter()
            .find_map(|(t, a)| match a {
                AiAction::Attack { dest, force } => Some((t, dest, force)),
                _ => None,
            })
        {
            attack = Some(a);
            break;
        }
    }
    let (_, dest, force) = attack.expect("the AI never attacked in 12k ticks");
    assert!(force >= content().ai.attack_at_army);
    assert!(
        dest.distance(Vec2::new(1_500.0, 0.0)) <= content().ai.attack_spread * 1.5,
        "the wave was aimed at {dest}, not at the enemy HQ"
    );
    // It closes: the army's distance to the enemy HQ falls.
    let army_distance = |app: &mut App| -> f32 {
        let c = content();
        let mut q = app.world_mut().query::<(&Position, &UnitDefIdx, &Faction)>();
        let ds: Vec<f32> = q
            .iter(app.world())
            .filter(|(_, d, f)| **f == Faction::A && c.units[d.0].offense > 0)
            .map(|(p, _, _)| p.0.distance(Vec2::new(1_500.0, 0.0)))
            .collect();
        ds.iter().copied().fold(f32::INFINITY, f32::min)
    };
    let before = army_distance(&mut app);
    tick(&mut app, 600);
    let after = army_distance(&mut app);
    assert!(
        after < before,
        "the force did not close on the enemy HQ ({before} -> {after})"
    );
}

/// "On a timer": every decision lands on a multiple of `think_interval_ticks`,
/// counted in **ticks** — there is no other clock in the AI.
#[test]
fn the_ai_acts_only_on_its_think_interval() {
    let interval = content().ai.think_interval_ticks;
    let mut app = ai_match(11);
    tick(&mut app, 6_000);
    let acts = journal(&app, Faction::A);
    assert!(!acts.is_empty());
    for (t, a) in acts {
        assert_eq!(t % interval, 0, "decision {a:?} taken off-timer at tick {t}");
    }
}

/// The AC1 determinism probe: same seed ⇒ the same actions, tick for tick. The
/// comparison is on the *decisions* (the journal), not merely on the state they
/// happen to produce.
#[test]
fn the_ai_is_deterministic_given_the_seed() {
    let run = |seed: u64| {
        let mut app = ai_match(seed);
        tick(&mut app, 6_000);
        let acts = journal(&app, Faction::A);
        let stock = alloy(&app, Faction::A);
        let force = units_of(&mut app, Faction::A);
        (acts, stock, force)
    };
    let a = run(11);
    let b = run(11);
    assert_eq!(a.0, b.0, "same seed, different actions");
    assert_eq!((a.1, a.2), (b.1, b.2), "same seed, different outcome");

    // A different seed moves what the seed actually decides: the barracks spoke.
    let c = run(12);
    let spoke = |acts: &[(u32, AiAction)]| {
        acts.iter().find_map(|(_, a)| match a {
            AiAction::PlaceBarracks { pos } => Some(*pos),
            _ => None,
        })
    };
    assert_ne!(
        spoke(&a.0),
        spoke(&c.0),
        "the seed does not reach the AI's choices"
    );
}

/// The AI is a commander, not a cheat: it never touches the other side's units,
/// buildings or Alloy — it can only issue orders, and its orders are signed.
#[test]
fn the_ai_commands_only_its_own_side() {
    let mut app = ai_match(11);
    let theirs = spawn_unit(&mut app, "worker", Faction::B, Vec2::new(40.0, 0.0));
    let start_b = alloy(&app, Faction::B);
    for t in 0..3_000 {
        step(&mut app);
        assert!(
            app.world().get::<GatherTarget>(theirs).is_none(),
            "tick {t}: the AI put the enemy's worker to work"
        );
        assert_eq!(
            alloy(&app, Faction::B),
            start_b,
            "tick {t}: the AI moved the enemy's Alloy"
        );
    }
    assert_eq!(
        my_buildings(&mut app, Faction::B, &content().ai.barracks),
        0,
        "the AI built in the enemy's name"
    );
}

/// No wall-clock and no hash-map iteration anywhere in the sim — the two ways a
/// deterministic AI stops being deterministic. Source-level, so it also catches
/// the next module.
#[test]
fn the_sim_reads_no_clock_and_iterates_no_hash_map() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sim");
    let mut offenders = Vec::new();
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("read src/sim")
        .map(|e| e.expect("entry").path())
        .filter(|p| p.extension().is_some_and(|e| e == "rs"))
        .collect();
    files.sort();
    for path in files {
        let text = std::fs::read_to_string(&path).expect("read source");
        for needle in [
            "Instant",
            "SystemTime",
            "Time<Real>",
            "elapsed(",
            "HashMap",
            "HashSet",
        ] {
            // Comments say the words; code is what matters, so only count lines
            // that are not comments.
            for line in text.lines() {
                let code = line.split("//").next().unwrap_or("");
                if code.contains(needle) {
                    offenders.push(format!("{}: {}", path.display(), line.trim()));
                }
            }
        }
    }
    assert!(offenders.is_empty(), "nondeterminism in the sim: {offenders:?}");
}

// ---- AC1: the script is content, and bad content is refused at load ---------

/// Write a mutated copy of the content pair and try to load it.
fn load_mutated(name: &str, from: &str, to: &str) -> Result<Content, String> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/m4c_content")
        .join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let units = std::fs::read_to_string(data_dir().join("units.ron")).unwrap();
    assert!(units.contains(from), "anchor `{from}` missing from units.ron");
    std::fs::write(dir.join("units.ron"), units.replace(from, to)).unwrap();
    std::fs::copy(data_dir().join("resources.ron"), dir.join("resources.ron")).unwrap();
    Content::load_from_dir(&dir).map_err(|e| e.to_string())
}

/// The script has no silent defaults: an AI script the sim cannot run is an
/// `Err` at load, in **both** profiles (a check written in the arithmetic it is
/// checking would pass in debug by panicking and pass content in release —
/// F-005). Each case below is a way the AI would otherwise fail silently: a
/// commander that thinks every tick, one whose barracks cannot make what its
/// build order asks for, one with no build order at all, and counts that wrap.
#[test]
fn an_unrunnable_ai_script_is_refused_at_load() {
    for (name, from, to) in [
        ("think0", "think_interval_ticks: 30", "think_interval_ticks: 0"),
        ("wave0", "attack_interval_ticks: 600", "attack_interval_ticks: 0"),
        ("workers0", "worker_target: 6", "worker_target: 0"),
        ("army0", "attack_at_army: 3", "attack_at_army: 0"),
        ("nobarracks", "barracks: \"foundry\"", "barracks: \"nonesuch\""),
        ("hqbarracks", "barracks: \"foundry\"", "barracks: \"hq\""),
        (
            "wrongunit",
            "(unit: \"sentinel\", count: 2)",
            "(unit: \"ripper\", count: 2)",
        ),
        (
            "unknownunit",
            "(unit: \"sentinel\", count: 2)",
            "(unit: \"nonesuch\", count: 2)",
        ),
        (
            "count0",
            "(unit: \"sentinel\", count: 2)",
            "(unit: \"sentinel\", count: 0)",
        ),
        (
            "overflow",
            "(unit: \"sentinel\", count: 2)",
            "(unit: \"sentinel\", count: 4294967295)",
        ),
        ("offset", "barracks_offset: 130.0", "barracks_offset: 0.0"),
        ("spread", "attack_spread: 60.0", "attack_spread: -1.0"),
    ] {
        let got = load_mutated(name, from, to);
        assert!(
            got.is_err(),
            "`{name}` ({from} -> {to}) loaded, but the sim cannot run it"
        );
    }
}

/// Exactly one building is the victory target — zero makes the match
/// unwinnable, two make "the enemy HQ" ambiguous.
#[test]
fn the_victory_target_must_be_exactly_one_building() {
    assert!(load_mutated("novictory", "dropoff: true, victory: true", "dropoff: true").is_err());
    assert!(load_mutated(
        "twovictory",
        "alloy_cost: 150, produces: [\"bulwark\"",
        "alloy_cost: 150, victory: true, produces: [\"bulwark\""
    )
    .is_err());
}

/// The script really is data: change the RON and the commander's behaviour
/// changes with it — no Rust constant shadows it.
#[test]
fn the_ai_script_comes_from_the_ron_not_from_constants() {
    let edited = load_mutated(
        "slow",
        "think_interval_ticks: 30",
        "think_interval_ticks: 97",
    )
    .expect("still valid content");
    assert_eq!(edited.ai.think_interval_ticks, 97);
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .insert_resource(Time::<Fixed>::from_hz(60.0))
        .insert_resource(edited)
        .init_resource::<CommandQueue>()
        .init_resource::<RateReport>()
        .init_resource::<Casualties>()
        .insert_resource(Stockpiles::starting(starting_alloy()));
    onus::add_sim_systems(&mut app, Update);
    spawn_building(&mut app, "hq", Faction::A, Vec2::ZERO);
    app.world_mut().spawn((
        Position(Vec2::new(250.0, 0.0)),
        ResourceNode { amount: 100_000 },
    ));
    spawn_unit(&mut app, "worker", Faction::A, Vec2::ZERO);
    app.insert_resource(AiCommanders::new(3, &[Faction::A]));
    tick(&mut app, 1_000);
    let acts = journal(&app, Faction::A);
    assert!(!acts.is_empty(), "the edited script never acted");
    for (t, a) in acts {
        assert_eq!(t % 97, 0, "decision {a:?} at tick {t} ignores the edited RON");
    }
}
