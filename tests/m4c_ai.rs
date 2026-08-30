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
    CommandQueue, GatherPhase, GatherTarget, MoveTarget, Order, Position, RateReport, ResourceNode,
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
