//! L2 integration tests for M4a (economy: Alloy loop + content-as-data).
//!
//! Encodes the M4a acceptance criteria and the critic probes:
//!   AC1 — `units.ron` + `resources.ron` load into sim structs; no hardcoded
//!         costs/stats (every cost the sim charges is traceable to the RON);
//!   AC2 — worker gather/deposit loop with a per-faction stockpile, and the
//!         conservation law gathered == deposited + carried + still-in-deposit;
//!   AC3 — building placement and unit production each consume Alloy, exactly
//!         once, and never on a rejected order.
//!
//! The sim is exercised headless (`MinimalPlugins`) — no render types here.

use std::path::PathBuf;

use onus::sim::content::Content;

/// The repo's data directory, resolved without Bevy's `AssetServer` so the sim
/// stays loadable from a plain path in headless tests.
fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data")
}

fn content() -> Content {
    Content::load_from_dir(&data_dir()).expect("assets/data/*.ron parse into sim structs")
}

// ---- AC1: content is data ---------------------------------------------------

#[test]
fn loads_units_and_resources_from_ron() {
    let c = content();

    // The full MVP roster: Worker + the 5 combat units of the pentagon.
    let ids: Vec<&str> = c.units.iter().map(|u| u.id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["worker", "bulwark", "sentinel", "ripper", "ravager", "arclight"],
        "unit order is the RON order (stable, deterministic)"
    );

    // Stats come across intact.
    let bulwark = c.unit("bulwark").expect("bulwark defined");
    assert_eq!(
        (
            bulwark.speed,
            bulwark.offense,
            bulwark.defense,
            bulwark.armor
        ),
        (2, 4, 9, 9)
    );
    assert_eq!(bulwark.nemesis.as_deref(), Some("ravager"));
    assert!((c.nemesis_bonus.damage_mult - 1.3).abs() < 1e-6);
    assert!(c.nemesis_bonus.ignore_armor);

    // Resources: three domains defined, MVP active set is Alloy only.
    let res_ids: Vec<&str> = c.resources.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(res_ids, vec!["alloy", "biomass", "aether"]);
    assert_eq!(c.mvp_active, vec!["alloy".to_string()]);
    assert_eq!(c.economy.currency, "alloy");

    // Buildings: HQ + the three barracks, each with an Alloy cost.
    let b_ids: Vec<&str> = c.buildings.iter().map(|b| b.id.as_str()).collect();
    assert_eq!(
        b_ids,
        vec!["hq", "foundry", "gene_vats", "aether_spire"],
        "building order is the RON order"
    );
    for b in &c.buildings {
        assert!(b.alloy_cost > 0, "{} has an Alloy cost", b.id);
    }
}

#[test]
fn every_mvp_unit_has_an_alloy_cost_and_a_producer() {
    let c = content();
    for u in &c.units {
        assert!(u.mvp_alloy_cost > 0, "{} costs Alloy in the MVP", u.id);
        assert!(u.mvp_train_ticks > 0, "{} takes time to train", u.id);
        // Every unit is produced by some building that lists it.
        let producer = c
            .buildings
            .iter()
            .find(|b| b.produces.iter().any(|p| p == &u.id));
        assert!(producer.is_some(), "{} is produced by a building", u.id);
    }
    // The worker carries the gather parameters (data, not constants).
    let w = c.unit("worker").unwrap();
    assert!(w.gathers && w.mvp_carry_capacity > 0 && w.mvp_gather_ticks > 0);
}

#[test]
fn content_loads_from_the_default_asset_path() {
    // The driver inserts `Content` at startup from this path; tests run with the
    // repo root as the working directory, same as `cargo run`.
    let c = Content::load_default().expect("assets/data loads from the default path");
    assert_eq!(c.units.len(), 6);
}

#[test]
fn loading_is_deterministic_and_missing_dir_is_an_error() {
    let a = content();
    let b = content();
    assert_eq!(a.units.len(), b.units.len());
    for (x, y) in a.units.iter().zip(b.units.iter()) {
        assert_eq!(x.id, y.id);
        assert_eq!(x.mvp_alloy_cost, y.mvp_alloy_cost);
    }
    assert!(Content::load_from_dir(&data_dir().join("nope")).is_err());
}

// ---- AC2: the worker gather / deposit loop ---------------------------------

use bevy::prelude::*;

use onus::sim::economy::{
    gather, Building, Carrying, GatherPhase, ProductionQueue, Stockpiles, UnitDefIdx,
};
use onus::sim::spatial::Faction;
use onus::sim::{
    apply_commands, movement, CommandQueue, GatherTarget, Order, Position, RateReport, ResourceNode,
};

/// A headless sim app running the real `FixedUpdate` chain in `Update` (one
/// `app.update()` == one sim tick), so tests step the sim deterministically
/// without a real-time accumulator or a renderer. `tick` advances `Time<Fixed>`
/// by exactly one timestep per step — no wall-clock enters the sim.
fn sim_app(content: Content) -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .insert_resource(Time::<Fixed>::from_hz(60.0))
        .insert_resource(content)
        .init_resource::<CommandQueue>()
        .init_resource::<RateReport>()
        .init_resource::<Stockpiles>()
        .add_systems(Update, (apply_commands, gather, movement).chain());
    app
}

/// One sim tick: hand the sim exactly one fixed timestep, then run the chain.
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

fn spawn_worker(app: &mut App, faction: Faction, pos: Vec2) -> Entity {
    let idx = app
        .world()
        .resource::<Content>()
        .unit_index("worker")
        .unwrap();
    app.world_mut()
        .spawn((Position(pos), UnitDefIdx(idx), faction))
        .id()
}

fn spawn_building(app: &mut App, id: &str, faction: Faction, pos: Vec2) -> Entity {
    let def = app
        .world()
        .resource::<Content>()
        .building_index(id)
        .unwrap();
    app.world_mut()
        .spawn((
            Position(pos),
            Building { def },
            faction,
            ProductionQueue::default(),
        ))
        .id()
}

fn spawn_deposit(app: &mut App, pos: Vec2, amount: u32) -> Entity {
    app.world_mut()
        .spawn((Position(pos), ResourceNode { amount }))
        .id()
}

fn order_gather(app: &mut App, unit: Entity, node: Entity) {
    let node_pos = app.world().get::<Position>(node).unwrap().0;
    app.world_mut()
        .resource_mut::<CommandQueue>()
        .0
        .push_back(Order::Gather {
            units: vec![unit],
            node,
            node_pos,
        });
}

/// Alloy still sitting in every deposit on the map.
fn in_deposits(app: &mut App) -> u32 {
    app.world_mut()
        .query::<&ResourceNode>()
        .iter(app.world())
        .map(|n| n.amount)
        .sum()
}

/// Alloy being carried by workers (in flight).
fn carried(app: &mut App) -> u32 {
    app.world_mut()
        .query::<&Carrying>()
        .iter(app.world())
        .map(|c| c.0)
        .sum()
}

fn alloy(app: &App, f: Faction) -> u32 {
    app.world().resource::<Stockpiles>().alloy(f)
}

#[test]
fn worker_harvests_carries_and_deposits_into_the_faction_stockpile() {
    let c = content();
    let cap = c.unit("worker").unwrap().mvp_carry_capacity;
    let mut app = sim_app(c);

    spawn_building(&mut app, "hq", Faction::A, Vec2::ZERO);
    let node = spawn_deposit(&mut app, Vec2::new(300.0, 0.0), 1000);
    let w = spawn_worker(&mut app, Faction::A, Vec2::ZERO);
    order_gather(&mut app, w, node);

    // Walks out, harvests a load: carrying == capacity, deposit down by capacity.
    tick(&mut app, 200);
    assert_eq!(carried(&mut app), cap, "worker carries exactly one load");
    assert_eq!(in_deposits(&mut app), 1000 - cap);
    assert_eq!(
        alloy(&app, Faction::A),
        0,
        "nothing banked until it returns"
    );

    // Walks back to the HQ and deposits.
    tick(&mut app, 200);
    assert_eq!(alloy(&app, Faction::A), cap, "one load banked at the HQ");
    assert_eq!(carried(&mut app), 0, "hands are empty after depositing");
    assert_eq!(alloy(&app, Faction::B), 0, "stockpiles are per faction");

    // The loop repeats without a new order.
    tick(&mut app, 600);
    assert!(
        alloy(&app, Faction::A) >= 2 * cap,
        "the gather loop keeps cycling"
    );
}

#[test]
fn alloy_is_conserved_across_the_whole_loop() {
    let c = content();
    let mut app = sim_app(c);
    spawn_building(&mut app, "hq", Faction::A, Vec2::ZERO);
    let n1 = spawn_deposit(&mut app, Vec2::new(250.0, 60.0), 300);
    let n2 = spawn_deposit(&mut app, Vec2::new(-200.0, -140.0), 300);
    let total = 600;

    for i in 0..6 {
        let w = spawn_worker(&mut app, Faction::A, Vec2::new(i as f32 * 12.0, 0.0));
        order_gather(&mut app, w, if i % 2 == 0 { n1 } else { n2 });
    }

    // Check the conservation law on *every* tick, not just at the end.
    for _ in 0..1500 {
        step(&mut app);
        let banked = alloy(&app, Faction::A);
        let sum = banked + carried(&mut app) + in_deposits(&mut app);
        assert_eq!(sum, total, "gathered == deposited + carried + in-deposit");
    }
    assert!(alloy(&app, Faction::A) > 0, "the loop actually ran");
}

#[test]
fn a_depleted_deposit_yields_nothing_extra() {
    let c = content();
    let cap = c.unit("worker").unwrap().mvp_carry_capacity;
    let mut app = sim_app(c);
    spawn_building(&mut app, "hq", Faction::A, Vec2::ZERO);
    // Half a load left in the deposit: the worker gets exactly that, no more.
    let node = spawn_deposit(&mut app, Vec2::new(200.0, 0.0), cap / 2);
    let w = spawn_worker(&mut app, Faction::A, Vec2::ZERO);
    order_gather(&mut app, w, node);

    tick(&mut app, 1200);
    assert_eq!(in_deposits(&mut app), 0, "deposit is mined out");
    assert_eq!(
        alloy(&app, Faction::A),
        cap / 2,
        "exactly what the deposit held — nothing created"
    );
    assert_eq!(carried(&mut app), 0);
}

#[test]
fn gather_loop_is_deterministic_across_identical_runs() {
    let run = || {
        let mut app = sim_app(content());
        spawn_building(&mut app, "hq", Faction::A, Vec2::ZERO);
        let node = spawn_deposit(&mut app, Vec2::new(220.0, 90.0), 250);
        for i in 0..5 {
            let w = spawn_worker(&mut app, Faction::A, Vec2::new(i as f32 * 9.0, 4.0));
            order_gather(&mut app, w, node);
        }
        tick(&mut app, 900);
        (
            alloy(&app, Faction::A),
            in_deposits(&mut app),
            carried(&mut app),
        )
    };
    assert_eq!(
        run(),
        run(),
        "same setup ⇒ same state (no wall-clock, no map order)"
    );
}

#[test]
fn a_move_order_keeps_the_carried_load_and_stops_gathering() {
    let c = content();
    let cap = c.unit("worker").unwrap().mvp_carry_capacity;
    let mut app = sim_app(c);
    spawn_building(&mut app, "hq", Faction::A, Vec2::ZERO);
    let node = spawn_deposit(&mut app, Vec2::new(300.0, 0.0), 100);
    let w = spawn_worker(&mut app, Faction::A, Vec2::ZERO);
    order_gather(&mut app, w, node);
    tick(&mut app, 200);
    assert_eq!(carried(&mut app), cap);

    app.world_mut()
        .resource_mut::<CommandQueue>()
        .0
        .push_back(Order::MoveTo {
            units: vec![w],
            dest: Vec2::new(-400.0, 400.0),
        });
    tick(&mut app, 300);
    assert!(app.world().get::<GatherTarget>(w).is_none());
    assert!(app.world().get::<GatherPhase>(w).is_none());
    assert_eq!(
        carried(&mut app),
        cap,
        "the load is still in flight, not lost"
    );
    assert_eq!(in_deposits(&mut app), 100 - cap);
    assert_eq!(alloy(&app, Faction::A), 0);
}
