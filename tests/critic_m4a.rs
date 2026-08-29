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

// =============================================================================
// M4a critic — second pass. Attacks the *fix* diff (ff41033..HEAD): the shared
// sim-chain definition, the partial-accept deposit, load-time validation,
// sim-owned `UnitKind`, RON `starting_alloy`, and the harvest tick boundary.
// Appended; the six probes above are untouched.
// =============================================================================
mod pass2 {
    use super::*;
    use onus::sim::economy::GatherPhase;
    use onus::sim::{GatherTarget, UnitKind};
    use std::path::PathBuf;

    // ---- helpers -----------------------------------------------------------

    fn src_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
    }

    fn read_src(rel: &str) -> String {
        std::fs::read_to_string(src_dir().join(rel)).expect("source readable")
    }

    fn rust_sources() -> Vec<(String, String)> {
        fn walk(dir: &std::path::Path, out: &mut Vec<(String, String)>) {
            for e in std::fs::read_dir(dir).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    walk(&p, out);
                } else if p.extension().is_some_and(|x| x == "rs") {
                    out.push((
                        p.display().to_string(),
                        std::fs::read_to_string(&p).unwrap(),
                    ));
                }
            }
        }
        let mut out = Vec::new();
        walk(&src_dir(), &mut out);
        out
    }

    /// The app the *game* runs: the shared chain from `src/lib.rs`, installed on
    /// `Update` so one `step()` is exactly one fixed tick.
    fn shipped_app(content: Content, starting: u32) -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(Time::<Fixed>::from_hz(60.0))
            .insert_resource(content)
            .init_resource::<CommandQueue>()
            .init_resource::<RateReport>()
            .insert_resource(Stockpiles::starting(starting));
        onus::add_sim_systems(&mut app, Update);
        app
    }

    fn data_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data")
    }

    /// Write a mutated copy of the content pair and try to load it.
    /// `edits` are `(file, from, to)` textual substitutions.
    fn load_edited(name: &str, edits: &[(&str, &str, &str)]) -> Result<Content, String> {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/critic_content")
            .join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let mut units = std::fs::read_to_string(data_dir().join("units.ron")).unwrap();
        let mut res = std::fs::read_to_string(data_dir().join("resources.ron")).unwrap();
        for (file, from, to) in edits {
            let buf = if *file == "units.ron" {
                &mut units
            } else {
                &mut res
            };
            assert!(buf.contains(from), "anchor `{from}` missing from {file}");
            *buf = buf.replace(from, to);
        }
        std::fs::write(dir.join("units.ron"), units).unwrap();
        std::fs::write(dir.join("resources.ron"), res).unwrap();
        Content::load_from_dir(&dir).map_err(|e| e.to_string())
    }

    fn edited(name: &str, edits: &[(&str, &str, &str)]) -> Content {
        load_edited(name, edits).expect("edited content should still be valid")
    }

    fn train(app: &mut App, building: Entity, unit_id: &str) {
        let unit = app
            .world()
            .resource::<Content>()
            .unit_index(unit_id)
            .unwrap();
        push(app, Order::Train { building, unit });
    }

    // ---- P7: one shared definition of the sim chain -------------------------

    /// F-004's fix is "there is exactly one sim-chain definition". That is only
    /// true if nothing else in `src/` registers sim systems, and if the shipped
    /// registration goes through `add_sim_systems`.
    #[test]
    fn the_sim_chain_has_exactly_one_definition_in_src() {
        let lib = read_src("lib.rs");
        assert!(
            lib.contains("add_sim_systems(&mut app, FixedUpdate)"),
            "build_app must install the shared chain on FixedUpdate"
        );
        for sys in ["sim::apply_commands", "sim::economy::production", "sim::movement"] {
            assert_eq!(
                lib.matches(sys).count(),
                1,
                "`{sys}` is registered more than once in src/lib.rs"
            );
        }
        // No other source file may schedule a sim system.
        for (path, src) in rust_sources() {
            if path.ends_with("lib.rs") {
                continue;
            }
            for line in src.lines() {
                let l = line.trim();
                if l.starts_with("//") || l.starts_with("///") {
                    continue;
                }
                assert!(
                    !(l.contains("add_systems") && l.contains("FixedUpdate")),
                    "{path} registers a second FixedUpdate list: {l}"
                );
            }
        }
    }

    /// The shipped chain must actually deliver: charge, advance, spawn, gather,
    /// deposit — all of it under `add_sim_systems`, no test-local list.
    #[test]
    fn the_shipped_chain_charges_produces_and_gathers() {
        let c = content();
        let cost = c.unit("worker").unwrap().mvp_alloy_cost;
        let ticks = c.unit("worker").unwrap().mvp_train_ticks;
        let cap = c.unit("worker").unwrap().mvp_carry_capacity;
        let mut app = shipped_app(c, 1000);
        let hq = spawn_building(&mut app, "hq", Faction::A, Vec2::ZERO);
        train(&mut app, hq, "worker");
        step(&mut app);
        assert_eq!(alloy(&app, Faction::A), 1000 - cost);
        step_n(&mut app, ticks);
        assert_eq!(count_units(&mut app), 1, "the paid-for worker arrived");

        let w = spawn_worker(&mut app, Faction::A, Vec2::new(10.0, 0.0));
        let node = spawn_deposit(&mut app, Vec2::new(150.0, 0.0), 100);
        order_gather(&mut app, w, node);
        step_n(&mut app, 400);
        let banked = alloy(&app, Faction::A) - (1000 - cost);
        assert!(
            banked >= cap && banked.is_multiple_of(cap),
            "the shipped chain banked whole loads (got {banked}, load = {cap})"
        );
    }

    /// The chain is installed on `Update` in tests and `FixedUpdate` in the
    /// game. Same scenario, both schedules, must produce the same state — a
    /// per-tick differential, so the test composition cannot flatter the game.
    #[test]
    fn update_and_fixedupdate_installations_agree() {
        fn scenario(app: &mut App) -> Vec<u64> {
            let hq = spawn_building(app, "hq", Faction::A, Vec2::new(-20.0, 10.0));
            let node = spawn_deposit(app, Vec2::new(220.0, 30.0), 300);
            for i in 0..3 {
                let w = spawn_worker(app, Faction::A, Vec2::new(i as f32 * 9.0, 0.0));
                order_gather(app, w, node);
            }
            train(app, hq, "worker");
            Vec::new()
        }
        fn hash(app: &mut App) -> u64 {
            let mut h = alloy(app, Faction::A) as u64;
            h = h.wrapping_mul(31).wrapping_add(count_units(app) as u64);
            h = h.wrapping_mul(31).wrapping_add(carried(app));
            h = h.wrapping_mul(31).wrapping_add(in_deposits(app));
            let mut q = app.world_mut().query::<&Position>();
            let mut bits: Vec<u64> = q
                .iter(app.world())
                .map(|p| ((p.0.x.to_bits() as u64) << 32) | p.0.y.to_bits() as u64)
                .collect();
            bits.sort_unstable();
            for b in bits {
                h = h.wrapping_mul(1099511628211).wrapping_add(b);
            }
            h
        }

        // (a) the test installation: chain on Update, one manual fixed step.
        let mut a = shipped_app(content(), 800);
        scenario(&mut a);
        let mut ha = Vec::new();
        for _ in 0..800 {
            step(&mut a);
            ha.push(hash(&mut a));
        }

        // (b) the shipped installation: chain on FixedUpdate, driven by the
        // fixed-timestep loop with a manual (deterministic) frame duration.
        let mut b = App::new();
        b.add_plugins(MinimalPlugins)
            .insert_resource(Time::<Fixed>::from_hz(60.0))
            .insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
                std::time::Duration::from_nanos(16_666_667),
            ))
            .insert_resource(content())
            .init_resource::<CommandQueue>()
            .init_resource::<RateReport>()
            .insert_resource(Stockpiles::starting(800));
        onus::add_sim_systems(&mut b, FixedUpdate);
        scenario(&mut b);
        let mut hb = Vec::new();
        while hb.len() < 800 {
            let before = b.world().resource::<RateReport>().sim_ticks;
            b.update();
            let after = b.world().resource::<RateReport>().sim_ticks;
            for _ in before..after {
                hb.push(hash(&mut b));
            }
        }
        hb.truncate(800);
        let first_diff = ha.iter().zip(hb.iter()).position(|(x, y)| x != y);
        assert_eq!(
            first_diff, None,
            "Update- and FixedUpdate-installed sim diverge at tick {first_diff:?}"
        );
    }

    // ---- P8: the partial-accept deposit ------------------------------------

    fn ledger(app: &mut App) -> u64 {
        alloy(app, Faction::A) as u64
            + alloy(app, Faction::B) as u64
            + carried(app)
            + in_deposits(app)
    }

    /// A worker parked forever at a *completely* full stockpile: nothing is
    /// destroyed, nothing is minted, and when room appears the held load banks.
    #[test]
    fn a_worker_at_a_full_stockpile_keeps_its_load_and_resumes_when_room_appears() {
        let c = content();
        let cap = c.unit("worker").unwrap().mvp_carry_capacity;
        let mut app = shipped_app(c, u32::MAX);
        spawn_building(&mut app, "hq", Faction::A, Vec2::ZERO);
        let node = spawn_deposit(&mut app, Vec2::new(120.0, 0.0), 200);
        let w = spawn_worker(&mut app, Faction::A, Vec2::ZERO);
        order_gather(&mut app, w, node);

        // `Stockpiles::starting` seeds *both* factions, so the baseline is 2x.
        let total = 2 * u32::MAX as u64 + 200;
        for t in 0..600 {
            step(&mut app);
            assert_eq!(ledger(&mut app), total, "tick {t}: no Alloy created or lost");
        }
        assert_eq!(alloy(&app, Faction::A), u32::MAX, "the counter is pinned");
        assert_eq!(
            app.world().get::<Carrying>(w).unwrap().0,
            cap,
            "the unaccepted load is still in the worker's hands"
        );

        // Make room: the parked worker must bank what it held (no deadlock).
        assert!(app
            .world_mut()
            .resource_mut::<Stockpiles>()
            .try_spend(Faction::A, 1000));
        let after_spend = ledger(&mut app);
        step_n(&mut app, 10);
        assert_eq!(
            app.world().get::<Carrying>(w).unwrap().0,
            0,
            "the held load banks once the stockpile has room"
        );
        assert_eq!(ledger(&mut app), after_spend, "still only moved, never made");
    }

    /// Two loaded workers deposit on the same tick into a stockpile with room
    /// for less than one load: the accepted total equals the room, and both
    /// remainders survive.
    #[test]
    fn two_same_tick_deposits_split_the_last_room_without_loss() {
        let c = content();
        let cap = c.unit("worker").unwrap().mvp_carry_capacity;
        assert!(cap >= 4, "probe assumes a multi-Alloy load");
        let idx = c.unit_index("worker").unwrap();
        let room = 5u32;
        let mut app = shipped_app(c, u32::MAX - room);
        spawn_building(&mut app, "hq", Faction::A, Vec2::ZERO);
        let node = spawn_deposit(&mut app, Vec2::new(400.0, 0.0), 0);

        let mut ws = Vec::new();
        for _ in 0..2 {
            ws.push(
                app.world_mut()
                    .spawn((
                        Position(Vec2::ZERO),
                        UnitDefIdx(idx),
                        Faction::A,
                        Carrying(cap),
                        GatherTarget(node),
                        GatherPhase::ToDropoff,
                    ))
                    .id(),
            );
        }
        let total = 2 * (u32::MAX - room) as u64 + 2 * cap as u64;
        step(&mut app);
        assert_eq!(ledger(&mut app), total, "same-tick deposits are conserved");
        assert_eq!(
            alloy(&app, Faction::A),
            u32::MAX,
            "the stockpile took exactly the room it had"
        );
        assert_eq!(
            carried(&mut app),
            2 * cap as u64 - room as u64,
            "both remainders stay in hand"
        );
        for t in 0..200 {
            step(&mut app);
            assert_eq!(ledger(&mut app), total, "tick {t}");
        }
    }

    /// A worker whose faction has no drop-off must never bank into another
    /// faction's stockpile.
    #[test]
    fn a_deposit_never_crosses_faction_lines() {
        let c = content();
        let cap = c.unit("worker").unwrap().mvp_carry_capacity;
        let mut app = shipped_app(c, 0);
        spawn_building(&mut app, "hq", Faction::A, Vec2::ZERO);
        let node = spawn_deposit(&mut app, Vec2::new(150.0, 0.0), 100);
        let w = spawn_worker(&mut app, Faction::B, Vec2::ZERO);
        order_gather(&mut app, w, node);
        step_n(&mut app, 500);
        assert_eq!(alloy(&app, Faction::A), 0, "A never receives B's load");
        assert_eq!(alloy(&app, Faction::B), 0, "B has no drop-off yet");
        assert_eq!(app.world().get::<Carrying>(w).unwrap().0, cap);

        spawn_building(&mut app, "hq", Faction::B, Vec2::new(-30.0, 0.0));
        step_n(&mut app, 500);
        assert!(alloy(&app, Faction::B) >= cap, "B banks its own load");
        assert_eq!(alloy(&app, Faction::A), 0, "A's stockpile is untouched");
        assert_eq!(ledger(&mut app), 100, "conserved across the whole probe");
    }

    // ---- P9: tick boundaries are exactly the RON numbers --------------------

    /// `mvp_gather_ticks: 1` — the tightest boundary. A load must land on the
    /// first mining tick and not before.
    #[test]
    fn a_one_tick_gather_lands_on_the_first_mining_tick() {
        let c = edited(
            "gather_ticks_1",
            &[("units.ron", "mvp_gather_ticks: 90,", "mvp_gather_ticks: 1,")],
        );
        let cap = c.unit("worker").unwrap().mvp_carry_capacity as u64;
        let mut app = shipped_app(c, 0);
        spawn_building(&mut app, "hq", Faction::A, Vec2::new(500.0, 0.0));
        let node = spawn_deposit(&mut app, Vec2::ZERO, 1000);
        let w = spawn_worker(&mut app, Faction::A, Vec2::ZERO);
        order_gather(&mut app, w, node);

        step(&mut app); // order applied; in range ⇒ mining starts
        assert_eq!(carried(&mut app), 0, "no load on the tick mining begins");
        step(&mut app);
        assert_eq!(carried(&mut app), cap, "one tick of mining ⇒ one load");
        assert_eq!(in_deposits(&mut app), 1000 - cap, "exactly one load taken");
    }

    /// The RON tick counts must mean the same thing for both timers: a load and
    /// a unit that each declare `n` ticks must land on the same tick after their
    /// order. A drift between the two is the F-003/off-by-one class of bug.
    #[test]
    fn gather_and_production_honour_the_ron_tick_counts_identically() {
        fn train_delay(n: u32) -> u32 {
            let c = edited(
                &format!("train_ticks_{n}"),
                &[(
                    "units.ron",
                    "mvp_train_ticks: 600,",
                    &format!("mvp_train_ticks: {n},"),
                )],
            );
            assert_eq!(c.unit("worker").unwrap().mvp_train_ticks, n);
            let mut app = shipped_app(c, 1000);
            let hq = spawn_building(&mut app, "hq", Faction::A, Vec2::ZERO);
            train(&mut app, hq, "worker");
            for t in 1..=n + 5 {
                step(&mut app);
                if count_units(&mut app) == 1 {
                    return t;
                }
            }
            panic!("the unit never arrived for mvp_train_ticks = {n}");
        }
        fn gather_delay(n: u32) -> u32 {
            let c = edited(
                &format!("gather_ticks_{n}"),
                &[(
                    "units.ron",
                    "mvp_gather_ticks: 90,",
                    &format!("mvp_gather_ticks: {n},"),
                )],
            );
            let mut app = shipped_app(c, 0);
            spawn_building(&mut app, "hq", Faction::A, Vec2::new(600.0, 0.0));
            let node = spawn_deposit(&mut app, Vec2::ZERO, 1000);
            let w = spawn_worker(&mut app, Faction::A, Vec2::ZERO);
            order_gather(&mut app, w, node);
            for t in 1..=n + 5 {
                step(&mut app);
                if carried(&mut app) > 0 {
                    return t;
                }
            }
            panic!("the load never landed for mvp_gather_ticks = {n}");
        }

        // Same declared duration ⇒ same delay, for every n including n = 1.
        for n in [1u32, 2, 5, 9] {
            assert_eq!(
                train_delay(n),
                gather_delay(n),
                "a {n}-tick unit and a {n}-tick load must take the same time"
            );
        }
        // And the duration scales exactly with the RON number (no fixed padding
        // that grows, no rounding).
        assert_eq!(train_delay(9) - train_delay(5), 4);
        assert_eq!(gather_delay(9) - gather_delay(5), 4);
    }

    /// Nothing is harvested early: with the shipped `mvp_gather_ticks`, the
    /// deposit is untouched on every tick before the load lands.
    #[test]
    fn no_alloy_leaves_the_deposit_before_the_ron_gather_tick() {
        let c = content();
        let g = c.unit("worker").unwrap().mvp_gather_ticks;
        let cap = c.unit("worker").unwrap().mvp_carry_capacity as u64;
        let mut app = shipped_app(c, 0);
        spawn_building(&mut app, "hq", Faction::A, Vec2::new(600.0, 0.0));
        let node = spawn_deposit(&mut app, Vec2::ZERO, 1000);
        let w = spawn_worker(&mut app, Faction::A, Vec2::ZERO);
        order_gather(&mut app, w, node);
        for t in 1..=g {
            step(&mut app);
            assert_eq!(carried(&mut app), 0, "load appeared early, at tick {t}");
            assert_eq!(in_deposits(&mut app), 1000, "deposit debited early at {t}");
        }
        step(&mut app);
        assert_eq!(carried(&mut app), cap, "the load lands one tick later");
        assert_eq!(in_deposits(&mut app), 1000 - cap);
    }

    // ---- P10: load-time validation -----------------------------------------

    #[test]
    fn the_shipped_content_is_not_wrongly_rejected() {
        Content::load_default().expect("the real assets must load");
        // A no-op edit still loads: validation rejects content, not editing.
        edited("noop", &[("units.ron", "name: \"Worker\"", "name: \"Worker\"")]);
    }

    #[test]
    fn validation_rejects_every_class_of_unplayable_content() {
        type Case<'a> = (&'a str, Vec<(&'a str, &'a str, &'a str)>, &'a str);
        let cases: Vec<Case> = vec![
            (
                "free_building",
                vec![("units.ron", "alloy_cost: 150, produces: [\"bulwark\"", "alloy_cost: 0, produces: [\"bulwark\"")],
                "foundry",
            ),
            (
                "zero_train_ticks",
                vec![("units.ron", "mvp_train_ticks: 900,", "mvp_train_ticks: 0,")],
                "sentinel",
            ),
            (
                "unknown_currency",
                vec![("resources.ron", "currency: \"alloy\",", "currency: \"unobtanium\",")],
                "unobtanium",
            ),
            (
                "zero_gather_range",
                vec![("resources.ron", "gather_range: 40.0,", "gather_range: 0.0,")],
                "range",
            ),
            (
                "zero_deposit_range",
                vec![("resources.ron", "deposit_range: 48.0,", "deposit_range: -1.0,")],
                "range",
            ),
            (
                "orphan_unit",
                vec![("units.ron", "produces: [\"arclight\"]", "produces: []")],
                "arclight",
            ),
        ];
        for (name, edits, needle) in cases {
            let err = load_edited(name, &edits)
                .err()
                .unwrap_or_else(|| panic!("`{name}` must be rejected at load"));
            assert!(
                err.contains(needle),
                "`{name}`: the error must name the offender (`{needle}`), got: {err}"
            );
        }
    }

    /// `starting_alloy` is data the sim reads; a file without it must not
    /// silently default to 0 Alloy.
    #[test]
    fn missing_starting_alloy_is_a_load_error_not_a_silent_zero() {
        let err = load_edited(
            "no_starting_alloy",
            &[("resources.ron", "starting_alloy: 300,", "")],
        )
        .expect_err("resources.ron without starting_alloy must not load");
        assert!(err.contains("starting_alloy") || err.contains("parse"), "{err}");
    }

    // ---- P11: starting Alloy and costs come from the RON --------------------

    #[test]
    fn starting_alloy_comes_from_ron_and_is_seeded_once_for_both_factions() {
        let c = edited(
            "starting_777",
            &[("resources.ron", "starting_alloy: 300,", "starting_alloy: 777,")],
        );
        assert_eq!(c.economy.starting_alloy, 777);
        let mut app = shipped_app(c, 777);
        assert_eq!(alloy(&app, Faction::A), 777);
        assert_eq!(alloy(&app, Faction::B), 777);
        // Nothing re-seeds it on later ticks.
        step_n(&mut app, 120);
        assert_eq!(alloy(&app, Faction::A), 777, "seeded exactly once");

        // And the shipped app takes it from the RON, not a Rust literal.
        let lib = read_src("lib.rs");
        assert!(
            lib.contains("Stockpiles::starting(starting_alloy)"),
            "build_app must seed stockpiles from content.economy.starting_alloy"
        );
        for (path, src) in rust_sources() {
            assert!(
                !src.contains("init_resource::<Stockpiles>"),
                "{path} seeds an empty stockpile instead of the RON value"
            );
        }
    }

    /// Costs are read, not compiled in: edit the RON and every charge follows.
    #[test]
    fn every_charge_follows_the_edited_ron_price() {
        let c = edited(
            "prices",
            &[
                ("units.ron", "mvp_alloy_cost: 10,", "mvp_alloy_cost: 37,"),
                (
                    "units.ron",
                    "alloy_cost: 150, produces: [\"bulwark\"",
                    "alloy_cost: 137, produces: [\"bulwark\"",
                ),
            ],
        );
        let worker_cost = c.unit("worker").unwrap().mvp_alloy_cost;
        let foundry_cost = c.building("foundry").unwrap().alloy_cost;
        assert_eq!((worker_cost, foundry_cost), (37, 137));
        let foundry_def = c.building_index("foundry").unwrap();
        let mut app = shipped_app(c, 1000);
        let hq = spawn_building(&mut app, "hq", Faction::A, Vec2::ZERO);
        train(&mut app, hq, "worker");
        push(
            &mut app,
            Order::Place {
                faction: Faction::A,
                building: foundry_def,
                pos: Vec2::new(200.0, 0.0),
            },
        );
        step(&mut app);
        assert_eq!(
            alloy(&app, Faction::A),
            1000 - 37 - 137,
            "the edited prices, each charged once"
        );
    }

    // ---- P12: the sim owns UnitKind ----------------------------------------

    /// Every unit the sim spawns carries the `mvp_kind` its RON entry declares,
    /// with no presentation system in the app at all — and no unit exists
    /// without a kind.
    #[test]
    fn every_produced_unit_carries_its_ron_kind() {
        let c = content();
        let mut expected: Vec<(String, UnitKind)> = Vec::new();
        let mut orders: Vec<(String, String)> = Vec::new();
        for b in &c.buildings {
            for u in &b.produces {
                let def = c.unit(u).unwrap();
                expected.push((def.id.clone(), def.mvp_kind));
                orders.push((b.id.clone(), u.clone()));
            }
        }
        let max_ticks = c.units.iter().map(|u| u.mvp_train_ticks).max().unwrap();
        let mut app = shipped_app(c, 100_000);
        for (b, u) in &orders {
            let e = spawn_building(&mut app, b, Faction::A, Vec2::new(0.0, 0.0));
            train(&mut app, e, u);
        }
        step_n(&mut app, max_ticks + 2);

        let mut q = app.world_mut().query::<(&UnitDefIdx, Option<&UnitKind>)>();
        let seen: Vec<(usize, Option<UnitKind>)> = q
            .iter(app.world())
            .map(|(i, k)| (i.0, k.copied()))
            .collect();
        assert_eq!(seen.len(), expected.len(), "every ordered unit was produced");
        let content = app.world().resource::<Content>().clone();
        for (idx, kind) in seen {
            let def = &content.units[idx];
            assert_eq!(
                kind,
                Some(def.mvp_kind),
                "`{}` spawned without / with the wrong RON kind",
                def.id
            );
        }
    }

    /// The presentation layer must no longer write sim components.
    #[test]
    fn the_render_layer_does_not_insert_sim_components() {
        let ui = read_src("ui.rs");
        for sim_component in ["UnitKind", "UnitDefIdx", "Carrying", "GatherPhase"] {
            for line in ui.lines() {
                let l = line.trim();
                if l.starts_with("//") {
                    continue;
                }
                assert!(
                    !(l.contains("insert") && l.contains(sim_component)),
                    "src/ui.rs inserts the sim component `{sim_component}`: {l}"
                );
            }
        }
    }

    // ---- P13: rejected orders are still free after the fixes ---------------

    #[test]
    fn an_out_of_range_train_index_is_free_and_produces_nothing() {
        let c = content();
        let mut app = shipped_app(c, 500);
        let hq = spawn_building(&mut app, "hq", Faction::A, Vec2::ZERO);
        push(
            &mut app,
            Order::Train {
                building: hq,
                unit: 9999,
            },
        );
        push(
            &mut app,
            Order::Place {
                faction: Faction::A,
                building: 9999,
                pos: Vec2::ZERO,
            },
        );
        step_n(&mut app, 60);
        assert_eq!(alloy(&app, Faction::A), 500, "rejected orders cost nothing");
        assert_eq!(count_units(&mut app), 0);
        assert_eq!(count_buildings(&mut app), 1);
    }
}
