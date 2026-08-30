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
    Building, Carrying, GatherPhase, ProductionQueue, Stockpiles, UnitDefIdx,
};
use onus::sim::spatial::Faction;
use onus::sim::{CommandQueue, GatherTarget, Order, Position, RateReport, ResourceNode, UnitKind};

/// A headless app running **the shipped sim chain** (`onus::add_sim_systems`,
/// the same definition `build_app` installs on `FixedUpdate`) on `Update`, so
/// one `app.update()` == one sim tick and the test can hand the sim exactly one
/// fixed timestep per step — no real-time accumulator, no renderer, and no
/// hand-rolled system list that could drift from the game.
fn sim_app(content: Content) -> App {
    sim_app_with(content, Stockpiles::default())
}

fn sim_app_with(content: Content, stock: Stockpiles) -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .insert_resource(Time::<Fixed>::from_hz(60.0))
        .insert_resource(content)
        .init_resource::<CommandQueue>()
        .init_resource::<RateReport>()
        .insert_resource(stock);
    onus::add_sim_systems(&mut app, Update);
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

// ---- AC3: building placement + unit production cost Alloy -------------------

/// The same shipped chain, started with a given Alloy balance.
fn econ_app(content: Content, starting_alloy: u32) -> App {
    sim_app_with(content, Stockpiles::starting(starting_alloy))
}

fn place(app: &mut App, faction: Faction, id: &str, pos: Vec2) {
    let building = app
        .world()
        .resource::<Content>()
        .building_index(id)
        .unwrap();
    app.world_mut()
        .resource_mut::<CommandQueue>()
        .0
        .push_back(Order::Place {
            faction,
            building,
            pos,
        });
}

fn train(app: &mut App, building: Entity, unit_id: &str) {
    let unit = app
        .world()
        .resource::<Content>()
        .unit_index(unit_id)
        .unwrap();
    app.world_mut()
        .resource_mut::<CommandQueue>()
        .0
        .push_back(Order::Train { building, unit });
}

fn count_buildings(app: &mut App) -> usize {
    app.world_mut().query::<&Building>().iter(app.world()).len()
}

fn count_units(app: &mut App) -> usize {
    app.world_mut()
        .query::<&UnitDefIdx>()
        .iter(app.world())
        .len()
}

#[test]
fn placing_a_building_costs_exactly_its_ron_price_once() {
    let c = content();
    let cost = c.building("foundry").unwrap().alloy_cost;
    let mut app = econ_app(c, 1000);

    place(&mut app, Faction::A, "foundry", Vec2::new(60.0, 60.0));
    tick(&mut app, 1);
    assert_eq!(count_buildings(&mut app), 1, "one Foundry placed");
    assert_eq!(
        alloy(&app, Faction::A),
        1000 - cost,
        "the RON price, deducted once"
    );
    assert_eq!(
        alloy(&app, Faction::B),
        1000,
        "the other faction pays nothing"
    );

    // Nothing is charged again on later ticks.
    tick(&mut app, 120);
    assert_eq!(alloy(&app, Faction::A), 1000 - cost);
    assert_eq!(count_buildings(&mut app), 1);
}

#[test]
fn an_unaffordable_building_is_not_placed_and_costs_nothing() {
    let c = content();
    let cost = c.building("aether_spire").unwrap().alloy_cost;
    let mut app = econ_app(c, cost - 1);

    place(&mut app, Faction::A, "aether_spire", Vec2::ZERO);
    tick(&mut app, 5);
    assert_eq!(count_buildings(&mut app), 0, "no free build");
    assert_eq!(alloy(&app, Faction::A), cost - 1, "no partial charge");
}

#[test]
fn training_a_unit_charges_once_at_order_time_and_spawns_on_completion() {
    let c = content();
    let def = c.unit("worker").unwrap().clone();
    let mut app = econ_app(c, 1000);
    let hq = spawn_building(&mut app, "hq", Faction::A, Vec2::ZERO);

    train(&mut app, hq, "worker");
    tick(&mut app, 1);
    assert_eq!(
        alloy(&app, Faction::A),
        1000 - def.mvp_alloy_cost,
        "charged when queued, at the RON price"
    );
    assert_eq!(count_units(&mut app), 0, "not built yet");

    // Still cooking one tick before the training time elapses.
    tick(&mut app, def.mvp_train_ticks - 2);
    assert_eq!(count_units(&mut app), 0);

    tick(&mut app, 3);
    assert_eq!(count_units(&mut app), 1, "exactly one unit produced");
    assert_eq!(
        alloy(&app, Faction::A),
        1000 - def.mvp_alloy_cost,
        "completion never charges a second time"
    );

    // The produced unit is data-driven and belongs to the producing faction.
    let mut q = app
        .world_mut()
        .query::<(&UnitDefIdx, &Faction, &Position)>();
    let (idx, f, pos) = q.iter(app.world()).next().unwrap();
    assert_eq!(
        idx.0,
        app.world()
            .resource::<Content>()
            .unit_index("worker")
            .unwrap()
    );
    assert_eq!(*f, Faction::A);
    assert!(pos.0.length() < 200.0, "spawns near its producer");
}

#[test]
fn a_queue_is_charged_once_per_item_and_stops_when_alloy_runs_out() {
    let c = content();
    let cost = c.unit("sentinel").unwrap().mvp_alloy_cost;
    let ticks = c.unit("sentinel").unwrap().mvp_train_ticks;
    // Exactly two Sentinels' worth of Alloy, three ordered.
    let mut app = econ_app(c, 2 * cost + cost / 2);
    let foundry = spawn_building(&mut app, "foundry", Faction::A, Vec2::ZERO);

    for _ in 0..3 {
        train(&mut app, foundry, "sentinel");
    }
    tick(&mut app, 1);
    assert_eq!(
        alloy(&app, Faction::A),
        cost / 2,
        "two affordable orders charged once each; the third rejected"
    );
    assert_eq!(
        app.world()
            .get::<ProductionQueue>(foundry)
            .unwrap()
            .items
            .len(),
        2
    );

    tick(&mut app, 2 * ticks + 4);
    assert_eq!(count_units(&mut app), 2, "exactly the two paid-for units");
    assert_eq!(alloy(&app, Faction::A), cost / 2, "no further charges");
    assert!(app
        .world()
        .get::<ProductionQueue>(foundry)
        .unwrap()
        .items
        .is_empty());
}

#[test]
fn a_building_only_trains_what_its_ron_produces_list_names() {
    let c = content();
    let mut app = econ_app(c, 1000);
    let hq = spawn_building(&mut app, "hq", Faction::A, Vec2::ZERO);

    train(&mut app, hq, "bulwark"); // Foundry unit, not an HQ unit.
    tick(&mut app, 5);
    assert_eq!(alloy(&app, Faction::A), 1000, "a rejected order is free");
    assert!(app
        .world()
        .get::<ProductionQueue>(hq)
        .unwrap()
        .items
        .is_empty());
    assert_eq!(count_units(&mut app), 0);
}

#[test]
fn a_train_order_for_a_missing_building_is_free_and_ignored() {
    let c = content();
    let mut app = econ_app(c, 1000);
    let ghost = app.world_mut().spawn_empty().id();
    train(&mut app, ghost, "worker");
    tick(&mut app, 5);
    assert_eq!(alloy(&app, Faction::A), 1000);
    assert_eq!(count_units(&mut app), 0);
}

#[test]
fn placement_and_production_are_deterministic() {
    let run = || {
        let mut app = econ_app(content(), 900);
        let hq = spawn_building(&mut app, "hq", Faction::A, Vec2::ZERO);
        place(&mut app, Faction::A, "foundry", Vec2::new(80.0, 0.0));
        for _ in 0..4 {
            train(&mut app, hq, "worker");
        }
        tick(&mut app, 1400);
        (
            alloy(&app, Faction::A),
            count_units(&mut app),
            count_buildings(&mut app),
        )
    };
    assert_eq!(run(), run());
}

// ---- AC3 (driver side): input emits orders, it never spends -----------------

use bevy::input::ButtonInput;
use onus::client::{CursorWorld, Selected, PLACE_KEYS, TRAIN_KEYS};
use onus::input::emit_build_commands;

fn input_app(content: Content) -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .insert_resource(content)
        .init_resource::<ButtonInput<KeyCode>>()
        .init_resource::<CursorWorld>()
        .init_resource::<CommandQueue>()
        .init_resource::<Stockpiles>()
        .add_systems(Update, emit_build_commands);
    app
}

fn press(app: &mut App, key: KeyCode) {
    let mut keys = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
    keys.clear();
    keys.press(key);
}

#[test]
fn build_hotkeys_emit_orders_and_never_touch_sim_state() {
    let c = content();
    let foundry = c.building_index("foundry").unwrap();
    let mut app = input_app(c);
    app.world_mut().resource_mut::<CursorWorld>().0 = Some(Vec2::new(40.0, -20.0));

    press(&mut app, PLACE_KEYS[0]);
    app.update();

    let queue = app.world().resource::<CommandQueue>();
    assert_eq!(queue.0.len(), 1, "one order emitted");
    // M4c: input signs what it emits, so the sim can refuse cross-faction orders.
    let signed = queue.0.front().unwrap();
    assert_eq!(signed.issuer(), Some(Faction::A), "the player's order is signed");
    match signed.order() {
        Order::Place {
            faction,
            building,
            pos,
        } => {
            assert_eq!(*faction, Faction::A);
            assert_eq!(*building, foundry);
            assert_eq!(*pos, Vec2::new(40.0, -20.0));
        }
        _ => panic!("expected a Place order"),
    }
    // Input emits; only the sim spends.
    assert_eq!(app.world().resource::<Stockpiles>().alloy(Faction::A), 0);
}

#[test]
fn train_hotkey_targets_the_selected_building_and_its_ron_roster() {
    let c = content();
    let worker = c.unit_index("worker").unwrap();
    let hq_def = c.building_index("hq").unwrap();
    let mut app = input_app(c);
    let hq = app
        .world_mut()
        .spawn((
            Position(Vec2::ZERO),
            Building { def: hq_def },
            Faction::A,
            ProductionQueue::default(),
            Selected,
        ))
        .id();

    press(&mut app, TRAIN_KEYS[0]);
    app.update();

    let queue = app.world().resource::<CommandQueue>();
    assert_eq!(queue.0.len(), 1);
    let signed = queue.0.front().unwrap();
    assert_eq!(signed.issuer(), Some(Faction::A), "the player's order is signed");
    match signed.order() {
        Order::Train { building, unit } => {
            assert_eq!(*building, hq);
            assert_eq!(*unit, worker, "slot 1 = the HQ's first `produces` entry");
        }
        _ => panic!("expected a Train order"),
    }

    // The HQ has no second entry in `produces`, so slot 2 emits nothing.
    app.world_mut().resource_mut::<CommandQueue>().0.clear();
    press(&mut app, TRAIN_KEYS[1]);
    app.update();
    assert!(app.world().resource::<CommandQueue>().0.is_empty());
}

// ---- review follow-ups: load-time validation, sim-owned kind, tick counts ---

/// Write a content pair into a scratch dir, mutating `units.ron` by a textual
/// substitution, and try to load it.
fn load_mutated(dir_name: &str, from: &str, to: &str) -> Result<Content, String> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/content_probe")
        .join(dir_name);
    std::fs::create_dir_all(&dir).unwrap();
    let units = std::fs::read_to_string(data_dir().join("units.ron")).unwrap();
    assert!(units.contains(from), "the probe's anchor text still exists");
    std::fs::write(dir.join("units.ron"), units.replace(from, to)).unwrap();
    std::fs::copy(data_dir().join("resources.ron"), dir.join("resources.ron")).unwrap();
    Content::load_from_dir(&dir).map_err(|e| e.to_string())
}

#[test]
fn a_gatherer_missing_its_gather_data_is_rejected_at_load() {
    // Silently defaulting these to 0 makes a worker loop forever mining nothing,
    // so the loader must refuse the file instead.
    let err = load_mutated("no_capacity", "mvp_carry_capacity: 10,", "")
        .expect_err("a gatherer with no carry capacity is invalid content");
    assert!(
        err.contains("worker"),
        "the error names the offender: {err}"
    );

    let err = load_mutated("no_gather_ticks", "mvp_gather_ticks: 90,", "")
        .expect_err("a gatherer with no gather time is invalid content");
    assert!(
        err.contains("worker"),
        "the error names the offender: {err}"
    );
}

#[test]
fn unbuildable_or_free_content_is_rejected_at_load() {
    let err = load_mutated("free_unit", "mvp_alloy_cost: 70,", "mvp_alloy_cost: 0,")
        .expect_err("a free unit is invalid content");
    assert!(err.contains("sentinel"), "{err}");

    let err = load_mutated(
        "unknown_produces",
        "\"bulwark\", \"sentinel\"",
        "\"nonesuch\"",
    )
    .expect_err("a building producing an unknown unit is invalid content");
    assert!(err.contains("nonesuch"), "{err}");
}

#[test]
fn the_sim_owns_the_unit_kind_of_what_it_spawns() {
    // No render/ui systems in this app at all: if `UnitKind` is present it was
    // written by the sim, not completed later by the presentation layer.
    let c = content();
    let mut app = econ_app(c, 1000);
    let hq = spawn_building(&mut app, "hq", Faction::A, Vec2::ZERO);
    train(&mut app, hq, "worker");
    let ticks = app
        .world()
        .resource::<Content>()
        .unit("worker")
        .unwrap()
        .mvp_train_ticks;
    tick(&mut app, ticks + 2);

    let mut q = app.world_mut().query::<(&UnitDefIdx, &UnitKind)>();
    let (_, kind) = q.iter(app.world()).next().expect("the worker was produced");
    assert_eq!(
        *kind,
        UnitKind::Worker,
        "kind comes from units.ron via the sim"
    );
}

#[test]
fn a_load_takes_exactly_the_ron_gather_ticks() {
    let c = content();
    let g = c.unit("worker").unwrap().mvp_gather_ticks;
    let cap = c.unit("worker").unwrap().mvp_carry_capacity;
    let mut app = econ_app(c, 0);
    spawn_building(&mut app, "hq", Faction::A, Vec2::ZERO);
    // Standing on the deposit: harvesting starts on the first tick after the
    // order is applied, so the load lands exactly `g` ticks later.
    let node = spawn_deposit(&mut app, Vec2::ZERO, 1000);
    let w = spawn_worker(&mut app, Faction::A, Vec2::ZERO);
    order_gather(&mut app, w, node);

    tick(&mut app, 1); // order applied; the worker is in range and starts mining
    tick(&mut app, g - 1);
    assert_eq!(
        carried(&mut app),
        0,
        "still mining one tick before the deadline"
    );
    tick(&mut app, 1);
    assert_eq!(
        carried(&mut app),
        cap,
        "a load lands on the {g}th tick of mining"
    );
}
