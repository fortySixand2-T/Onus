//! M4a adversarial probes (critic). Judged against the M4a ACs only:
//!   AC1 content-as-data, AC2 gather/deposit loop + per-faction stockpile,
//!   AC3 placement and unit production each consume Alloy — exactly once.
//! Probes: conservation, exactly-once charging, costs from RON, determinism.
//!
//! These tests drive the *shipped* wiring (`onus::build_app`) wherever the claim
//! is about the game, not about a test-only system composition.

use bevy::prelude::*;

use onus::sim::content::Content;
use onus::sim::economy::{gather, production, Building, Carrying, ProductionQueue, Stockpiles, UnitDefIdx};
use onus::sim::spatial::Faction;
use onus::sim::{
    apply_commands, movement, CommandQueue, Order, Position, RateReport, ResourceNode,
};

// ---- helpers ---------------------------------------------------------------

fn content() -> Content {
    Content::load_from_dir(
        &std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data"),
    )
    .expect("content loads")
}

fn alloy(app: &App, f: Faction) -> u32 {
    app.world().resource::<Stockpiles>().alloy(f)
}

fn count_units(app: &mut App) -> usize {
    app.world_mut()
        .query::<&UnitDefIdx>()
        .iter(app.world())
        .len()
}

fn count_buildings(app: &mut App) -> usize {
    app.world_mut().query::<&Building>().iter(app.world()).len()
}

fn push(app: &mut App, order: Order) {
    app.world_mut()
        .resource_mut::<CommandQueue>()
        .0
        .push_back(order);
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

// ---- P1: the shipped app must run the production system -------------------

/// The shipped `FixedUpdate` chain in `src/lib.rs`.
///
/// `build_app()` cannot be constructed inside a test (DefaultPlugins builds a
/// winit event loop, which panics off the main thread), so the wiring is
/// asserted at the source level: the milestone claim is about what the *game*
/// runs, and `tests/m4a_economy.rs` proves production only under a test-only
/// system composition that the driver does not install.
fn shipped_fixed_update_chain() -> String {
    let src = std::fs::read_to_string(
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs"),
    )
    .expect("src/lib.rs readable");
    // The last mention is the `add_systems(FixedUpdate, ..)` registration
    // (earlier ones are doc comments).
    let at = src
        .rfind("FixedUpdate")
        .expect("build_app registers a FixedUpdate chain");
    src[at..].to_string()
}

/// AC3: "Unit production (HQ→Worker, Barracks→its units) each costs Alloy."
/// Charging is only half of production. `economy::production` is the only
/// system that advances a queue and spawns the paid-for unit, and the shipped
/// app never schedules it: in the real game the Alloy is spent and the unit
/// never arrives.
#[test]
fn shipped_app_schedules_the_production_system() {
    let chain = shipped_fixed_update_chain();
    assert!(
        chain.contains("gather"),
        "sanity: the gather system is in the shipped chain"
    );
    assert!(
        chain.contains("production"),
        "build_app()'s FixedUpdate chain omits `economy::production`: Alloy is \
         charged when a unit is queued but no queue is ever advanced, so the \
         paid-for unit never spawns in the shipped game. Chain was:\n{chain}"
    );
}

// ---- P2: conservation edges in the gather loop -----------------------------

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

fn step(app: &mut App) {
    let dt = app.world().resource::<Time<Fixed>>().timestep();
    app.world_mut().resource_mut::<Time<Fixed>>().advance_by(dt);
    app.update();
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

fn order_gather(app: &mut App, unit: Entity, node: Entity) {
    let node_pos = app.world().get::<Position>(node).unwrap().0;
    push(
        app,
        Order::Gather {
            units: vec![unit],
            node,
            node_pos,
        },
    );
}

fn in_deposits(app: &mut App) -> u64 {
    app.world_mut()
        .query::<&ResourceNode>()
        .iter(app.world())
        .map(|n| n.amount as u64)
        .sum()
}

fn carried(app: &mut App) -> u64 {
    app.world_mut()
        .query::<&Carrying>()
        .iter(app.world())
        .map(|c| c.0 as u64)
        .sum()
}

/// Conservation must hold on a full stockpile too: a deposit that would exceed
/// the counter silently vanishes today (`Stockpiles::add` saturates), so Alloy
/// is destroyed rather than moved.
#[test]
fn a_deposit_into_a_near_full_stockpile_destroys_no_alloy() {
    let c = content();
    let cap = c.unit("worker").unwrap().mvp_carry_capacity as u64;
    let base = u32::MAX - 4;
    let mut app = sim_app(c);
    app.insert_resource(Stockpiles::starting(base));

    spawn_building(&mut app, "hq", Faction::A, Vec2::ZERO);
    let node = spawn_deposit(&mut app, Vec2::new(200.0, 0.0), 100);
    let w = spawn_worker(&mut app, Faction::A, Vec2::ZERO);
    order_gather(&mut app, w, node);

    let total = base as u64 + 100;
    for _ in 0..400 {
        step(&mut app);
        let sum = alloy(&app, Faction::A) as u64 + carried(&mut app) + in_deposits(&mut app);
        assert_eq!(sum, total, "banked + carried + in-deposit is invariant");
    }
    assert!(cap > 0);
}

fn spawn_deposit(app: &mut App, pos: Vec2, amount: u32) -> Entity {
    app.world_mut()
        .spawn((Position(pos), ResourceNode { amount }))
        .id()
}

/// Re-tasking a loaded worker to a second deposit must move Alloy, not mint or
/// destroy it, on every tick of the transition.
#[test]
fn retasking_a_loaded_worker_conserves_alloy() {
    let c = content();
    let mut app = sim_app(c);
    spawn_building(&mut app, "hq", Faction::A, Vec2::ZERO);
    let n1 = spawn_deposit(&mut app, Vec2::new(240.0, 0.0), 100);
    let n2 = spawn_deposit(&mut app, Vec2::new(-240.0, 60.0), 100);
    let w = spawn_worker(&mut app, Faction::A, Vec2::ZERO);
    order_gather(&mut app, w, n1);
    let total: u64 = 200;

    for i in 0..1200 {
        // Re-target mid-trip, repeatedly, including while loaded.
        if i == 150 {
            order_gather(&mut app, w, n2);
        }
        if i == 400 {
            order_gather(&mut app, w, n1);
        }
        if i == 640 {
            push(
                &mut app,
                Order::MoveTo {
                    units: vec![w],
                    dest: Vec2::new(400.0, -300.0),
                },
            );
        }
        if i == 800 {
            order_gather(&mut app, w, n2);
        }
        step(&mut app);
        let sum = alloy(&app, Faction::A) as u64 + carried(&mut app) + in_deposits(&mut app);
        assert_eq!(sum, total, "tick {i}: Alloy is only ever moved");
    }
    assert!(alloy(&app, Faction::A) > 0, "the loop actually ran");
}

/// Two workers finishing on the same tick against the last few Alloy: the sum
/// taken can never exceed what the deposit held.
#[test]
fn two_workers_cannot_over_mine_the_last_of_a_deposit() {
    let c = content();
    let mut app = sim_app(c);
    spawn_building(&mut app, "hq", Faction::A, Vec2::ZERO);
    let node = spawn_deposit(&mut app, Vec2::new(200.0, 0.0), 3);
    // Identical positions ⇒ identical arrival tick ⇒ same-tick contention.
    let a = spawn_worker(&mut app, Faction::A, Vec2::new(0.0, 0.0));
    let b = spawn_worker(&mut app, Faction::A, Vec2::new(0.0, 0.0));
    order_gather(&mut app, a, node);
    order_gather(&mut app, b, node);

    for _ in 0..600 {
        step(&mut app);
        let sum = alloy(&app, Faction::A) as u64 + carried(&mut app) + in_deposits(&mut app);
        assert_eq!(sum, 3, "no duplication of the last load");
    }
    assert_eq!(alloy(&app, Faction::A), 3);
}

// ---- P3: the exactly-once ledger -------------------------------------------

/// Full economy app (as `tests/m4a_economy.rs` composes it) so the ledger probe
/// exercises charging *and* delivery.
fn econ_app(content: Content, starting: u32) -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .insert_resource(Time::<Fixed>::from_hz(60.0))
        .insert_resource(content)
        .init_resource::<CommandQueue>()
        .init_resource::<RateReport>()
        .insert_resource(Stockpiles::starting(starting))
        .add_systems(
            Update,
            (apply_commands, production, gather, movement).chain(),
        );
    app
}

/// Every Alloy that left the stockpile must be accounted for by exactly one
/// thing that exists (a building, a finished unit, or an item still in a
/// queue). Catches double-charging, free goods, and charge-without-delivery.
#[test]
fn alloy_ledger_balances_over_a_long_mixed_run() {
    let c = content();
    let start = 900u64;
    let hq_trains = c.unit("worker").unwrap().mvp_alloy_cost as u64;
    let foundry_cost = c.building("foundry").unwrap().alloy_cost as u64;
    let sentinel = c.unit("sentinel").unwrap().mvp_alloy_cost as u64;
    let mut app = econ_app(c, start as u32);

    let hq = spawn_building(&mut app, "hq", Faction::A, Vec2::ZERO);
    let mut placed = 0u64; // the pre-existing HQ is free (match setup)
    let mut trained = 0u64;

    let worker = app
        .world()
        .resource::<Content>()
        .unit_index("worker")
        .unwrap();
    let sentinel_idx = app
        .world()
        .resource::<Content>()
        .unit_index("sentinel")
        .unwrap();
    let foundry_def = app
        .world()
        .resource::<Content>()
        .building_index("foundry")
        .unwrap();

    // Order more than we can afford; rejections must be free.
    for _ in 0..6 {
        push(
            &mut app,
            Order::Train {
                building: hq,
                unit: worker,
            },
        );
    }
    push(
        &mut app,
        Order::Place {
            faction: Faction::A,
            building: foundry_def,
            pos: Vec2::new(120.0, 0.0),
        },
    );
    step(&mut app);
    // The six worker orders and the Foundry all fit in 900.
    placed += foundry_cost;
    trained += 6 * hq_trains;
    assert_eq!(
        alloy(&app, Faction::A) as u64,
        start - placed - trained,
        "each accepted order charged exactly once"
    );

    // Now order Sentinels until the stockpile refuses.
    let foundry = {
        let mut q = app.world_mut().query::<(Entity, &Building)>();
        q.iter(app.world())
            .find(|(e, b)| b.def == foundry_def && *e != hq)
            .map(|(e, _)| e)
            .expect("foundry placed")
    };
    let before = alloy(&app, Faction::A) as u64;
    for _ in 0..10 {
        push(
            &mut app,
            Order::Train {
                building: foundry,
                unit: sentinel_idx,
            },
        );
    }
    step(&mut app);
    let spent = before - alloy(&app, Faction::A) as u64;
    assert_eq!(spent % sentinel, 0, "only whole Sentinels charged");
    let queued_sentinels = spent / sentinel;
    trained += spent;


    // Run everything to completion.
    step_n(&mut app, 9200);

    let units = count_units(&mut app) as u64;
    let buildings = count_buildings(&mut app) as u64;
    assert_eq!(buildings, 2, "HQ + the one placed Foundry");
    assert_eq!(
        units,
        6 + queued_sentinels,
        "exactly the units that were paid for, no more"
    );
    assert_eq!(
        alloy(&app, Faction::A) as u64 + placed + trained,
        start,
        "ledger: remaining + spent == starting Alloy"
    );
    assert!(
        app.world()
            .get::<ProductionQueue>(hq)
            .unwrap()
            .items
            .is_empty()
    );
}

fn step_n(app: &mut App, n: u32) {
    for _ in 0..n {
        step(app);
    }
}

/// Determinism probe: identical scripted runs produce identical per-tick state
/// hashes (stockpile, unit count, and every position), not just equal endings.
#[test]
fn economy_is_deterministic_per_tick() {
    let run = || {
        let c = content();
        let mut app = econ_app(c, 700);
        let hq = spawn_building(&mut app, "hq", Faction::A, Vec2::ZERO);
        let node = spawn_deposit(&mut app, Vec2::new(260.0, 40.0), 200);
        let worker = app
            .world()
            .resource::<Content>()
            .unit_index("worker")
            .unwrap();
        for i in 0..4 {
            let w = spawn_worker(&mut app, Faction::A, Vec2::new(i as f32 * 7.0, 3.0));
            order_gather(&mut app, w, node);
        }
        let mut hashes: Vec<u64> = Vec::new();
        for t in 0..1200 {
            if t == 50 || t == 700 {
                push(
                    &mut app,
                    Order::Train {
                        building: hq,
                        unit: worker,
                    },
                );
            }
            step(&mut app);
            let mut h = alloy(&app, Faction::A) as u64;
            h = h.wrapping_mul(31).wrapping_add(count_units(&mut app) as u64);
            h = h.wrapping_mul(31).wrapping_add(in_deposits(&mut app));
            h = h.wrapping_mul(31).wrapping_add(carried(&mut app));
            let mut q = app.world_mut().query::<&Position>();
            let mut bits: Vec<u64> = q
                .iter(app.world())
                .map(|p| ((p.0.x.to_bits() as u64) << 32) | p.0.y.to_bits() as u64)
                .collect();
            bits.sort_unstable();
            for b in bits {
                h = h.wrapping_mul(1099511628211).wrapping_add(b);
            }
            hashes.push(h);
        }
        hashes
    };
    assert_eq!(run(), run(), "per-tick state hashes must match");
}
