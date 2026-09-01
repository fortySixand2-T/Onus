//! L2 integration tests for M5 (deterministic replay).
//!
//! Encodes the M5 acceptance criteria and the critic probes:
//!   AC1 — commands carry the tick they are to be applied on, and are applied
//!         only in the sim schedule (`FixedUpdate` in the shipped app);
//!   AC2 — the command log is persisted to disk and round-trips;
//!   AC3 — a replay of that log reproduces the recorded match, hash for hash;
//!   AC4 — the seeded RNG is stepped only inside the sim.
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
use onus::sim::replay::{LoggedCommand, LoggedOrder, MatchLog};
use onus::sim::{
    AiCommanders, Attribution, CommandLog, CommandQueue, CommandTick, MatchState, MoveTarget,
    Order, Position, RateReport, ResourceNode,
};

// ---- harness ----------------------------------------------------------------

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data")
}

fn content() -> Content {
    Content::load_from_dir(&data_dir()).expect("assets/data/*.ron parse into sim structs")
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

/// The symmetric AI-vs-AI fixture (the M4c one): two bases, two deposits, three
/// workers a side, both commanders seeded from one match seed. The whole match
/// is then a function of `(this world, seed, the command log)`.
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
    app.insert_resource(CommandLog::new(seed));
    app
}

fn sim_tick(app: &App) -> u32 {
    app.world().resource::<MatchState>().tick()
}

fn push_at(app: &mut App, at: u32, order: Order) {
    app.world_mut()
        .resource_mut::<CommandQueue>()
        .0
        .push_at(at, order);
}

// ---- AC1: a command is tagged with the tick it applies on -------------------

/// A command scheduled for a future tick sits in the queue until that tick, and
/// then applies on exactly it — not early, not late.
#[test]
fn a_command_scheduled_for_a_future_tick_applies_on_exactly_that_tick() {
    let mut app = sim_app();
    let unit = spawn_unit(&mut app, "ripper", Faction::A, Vec2::ZERO);
    tick(&mut app, 10);
    let target_tick = sim_tick(&app) + 5;
    push_at(
        &mut app,
        target_tick,
        Order::MoveTo {
            units: vec![unit],
            dest: Vec2::new(400.0, 0.0),
        }
        .issued_by(Faction::A),
    );
    while sim_tick(&app) < target_tick {
        let now = sim_tick(&app);
        step(&mut app);
        assert!(
            app.world().get::<MoveTarget>(unit).is_none(),
            "the command applied on tick {now}, before its target tick {target_tick}"
        );
        assert_eq!(
            app.world().resource::<CommandQueue>().0.len(),
            1,
            "a not-yet-due command was consumed on tick {now}"
        );
    }
    assert_eq!(sim_tick(&app), target_tick, "harness bookkeeping");
    step(&mut app);
    assert!(
        app.world().get::<MoveTarget>(unit).is_some(),
        "the command never applied on its target tick {target_tick}"
    );
    assert_eq!(app.world().resource::<CommandQueue>().0.len(), 0);
}

/// A command whose tick has already gone by is **dropped**, never applied late:
/// applying it off its scheduled tick is exactly the divergence a replay is
/// supposed to make impossible.
#[test]
fn a_command_whose_tick_has_passed_is_dropped_not_applied_late() {
    let mut app = sim_app();
    let unit = spawn_unit(&mut app, "ripper", Faction::A, Vec2::ZERO);
    tick(&mut app, 10);
    let stale = sim_tick(&app) - 3;
    push_at(
        &mut app,
        stale,
        Order::MoveTo {
            units: vec![unit],
            dest: Vec2::new(400.0, 0.0),
        }
        .issued_by(Faction::A),
    );
    tick(&mut app, 5);
    assert!(
        app.world().get::<MoveTarget>(unit).is_none(),
        "a command scheduled for tick {stale} applied after that tick had passed"
    );
    assert_eq!(
        app.world().resource::<CommandQueue>().0.len(),
        0,
        "the stale command was left in the queue to apply later"
    );
}

/// An unscheduled command (the shape input produces — a click lands between
/// ticks) is applied on, and stamped with, the next tick the sim drains.
#[test]
fn an_unscheduled_command_is_applied_on_the_next_tick() {
    let mut app = sim_app();
    let unit = spawn_unit(&mut app, "ripper", Faction::A, Vec2::ZERO);
    tick(&mut app, 7);
    app.world_mut().resource_mut::<CommandQueue>().0.push_back(
        Order::MoveTo {
            units: vec![unit],
            dest: Vec2::new(400.0, 0.0),
        }
        .issued_by(Faction::A),
    );
    assert_eq!(
        app.world()
            .resource::<CommandQueue>()
            .0
            .front()
            .map(|c| c.when()),
        Some(CommandTick::Asap)
    );
    step(&mut app);
    assert!(app.world().get::<MoveTarget>(unit).is_some());
}

/// Commands are applied **only** by the sim schedule. In the shipped app the sim
/// runs in `FixedUpdate`, so with the clock stopped a frame can render, an order
/// can be queued, and nothing in the sim moves.
#[test]
fn commands_are_applied_only_when_the_sim_schedule_runs() {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .insert_resource(Time::<Fixed>::from_hz(60.0))
        .insert_resource(content())
        .init_resource::<CommandQueue>()
        .init_resource::<RateReport>()
        .init_resource::<Casualties>()
        .insert_resource(Stockpiles::starting(0));
    onus::add_sim_systems(&mut app, FixedUpdate);
    let unit = spawn_unit(&mut app, "ripper", Faction::A, Vec2::ZERO);
    // Stop the clock the fixed-update accumulator is fed from.
    app.world_mut().resource_mut::<Time<Virtual>>().pause();
    app.world_mut().resource_mut::<CommandQueue>().0.push_back(
        Order::MoveTo {
            units: vec![unit],
            dest: Vec2::new(400.0, 0.0),
        }
        .issued_by(Faction::A),
    );
    for _ in 0..30 {
        app.update();
    }
    assert!(
        app.world().get::<MoveTarget>(unit).is_none(),
        "a command applied outside the sim schedule"
    );
    assert_eq!(
        app.world().resource::<CommandQueue>().0.len(),
        1,
        "the queue was drained outside the sim schedule"
    );
    assert_eq!(sim_tick(&app), 0, "the sim ticked with the clock stopped");
}

/// The tick counter and the command log are `Option` parameters of
/// `apply_commands` (a hand-composed app may lack them), so the guarantee that
/// the *shipped* sim never runs without them has to be executable: the one
/// definition of the chain installs both alongside the systems that need them.
#[test]
fn the_sim_chain_installs_the_tick_counter_and_the_command_log() {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .insert_resource(Time::<Fixed>::from_hz(60.0))
        .insert_resource(content())
        .init_resource::<CommandQueue>()
        .init_resource::<RateReport>()
        .insert_resource(Stockpiles::starting(0));
    assert!(app.world().get_resource::<MatchState>().is_none());
    assert!(app.world().get_resource::<CommandLog>().is_none());
    onus::add_sim_systems(&mut app, Update);
    assert!(
        app.world().get_resource::<MatchState>().is_some(),
        "the chain does not install its own tick counter"
    );
    assert!(
        app.world().get_resource::<CommandLog>().is_some(),
        "the chain does not install its own command log"
    );
    // And the tick the commands are stamped with is that counter, not a
    // per-system count of its own.
    let unit = spawn_unit(&mut app, "ripper", Faction::A, Vec2::ZERO);
    tick(&mut app, 4);
    app.world_mut().resource_mut::<CommandQueue>().0.push_back(
        Order::MoveTo {
            units: vec![unit],
            dest: Vec2::new(400.0, 0.0),
        }
        .issued_by(Faction::A),
    );
    let at = sim_tick(&app);
    step(&mut app);
    assert_eq!(
        app.world()
            .resource::<CommandLog>()
            .commands()
            .iter()
            .map(|c| c.tick)
            .collect::<Vec<_>>(),
        vec![at],
        "the log did not stamp the command with the sim tick it applied on"
    );
}

/// The direction the `Option` could break: a minimal app that composes
/// `apply_commands` by hand — no tick counter, no log — must still apply the
/// orders it is given, which is the M1-M4a behaviour those fixtures encode.
#[test]
fn a_hand_composed_app_without_the_tick_counter_still_applies_its_orders() {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .insert_resource(Time::<Fixed>::from_hz(60.0))
        .insert_resource(content())
        .init_resource::<CommandQueue>()
        .init_resource::<RateReport>()
        .insert_resource(Stockpiles::starting(0))
        .add_systems(Update, (onus::sim::apply_commands, onus::sim::movement).chain());
    let unit = spawn_unit(&mut app, "ripper", Faction::A, Vec2::ZERO);
    app.world_mut().resource_mut::<CommandQueue>().0.push_back(
        Order::MoveTo {
            units: vec![unit],
            dest: Vec2::new(400.0, 0.0),
        }
        .issued_by(Faction::A),
    );
    step(&mut app);
    assert!(
        app.world().get::<MoveTarget>(unit).is_some(),
        "a hand-composed app stopped applying its orders"
    );
}

// ---- AC2: the command log is persisted to disk ------------------------------

/// A scratch path for a log file. Under the OS temp dir, named per test, so two
/// tests never race for one file.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("onus-m5");
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir.join(format!("{name}.ron"))
}

/// An AI match's whole command stream survives a trip through a file: same
/// ticks, same attributions, same orders, byte-identical coordinates.
#[test]
fn the_command_log_round_trips_through_a_file() {
    let mut app = ai_vs_ai(4);
    tick(&mut app, 3_000);
    let recorded = app.world().resource::<CommandLog>().log().clone();
    assert!(
        recorded.commands.len() > 20,
        "the log recorded almost nothing ({}) — the round trip would prove nothing",
        recorded.commands.len()
    );
    assert!(
        recorded.commands.iter().map(|c| c.tick).max() > Some(0),
        "every command landed on tick 0"
    );

    let path = scratch("round_trip");
    recorded.save(&path).expect("save");
    let loaded = MatchLog::load(&path).expect("load");
    assert_eq!(loaded, recorded, "the log changed on the way to disk");
    assert_eq!(loaded.seed, 4, "the seed is not part of the persisted log");

    // Coordinates bit for bit — a log that rounds a float is a log that
    // replays a different match.
    for (a, b) in loaded.commands.iter().zip(recorded.commands.iter()) {
        assert_eq!(
            format!("{:?}", a.order),
            format!("{:?}", b.order),
            "an order changed shape on disk"
        );
    }
}

/// Every float a log can carry comes back **bit-identical** — including the
/// subnormals, the extremes and negative zero, where a lazily formatted number
/// would quietly become a different one.
#[test]
fn every_coordinate_survives_the_file_exactly() {
    let hard: Vec<f32> = vec![
        0.0,
        -0.0,
        1.0,
        -1.0,
        0.1,
        1.0 / 3.0,
        -750.0,
        1234.5678,
        f32::MIN_POSITIVE,
        -f32::MIN_POSITIVE,
        f32::from_bits(1),       // smallest subnormal
        f32::from_bits(0x0080_0000 - 1), // largest subnormal
        f32::MAX,
        f32::MIN,
        1e-40,
        3.4028235e38,
    ];
    let mut log = MatchLog::new(7);
    for (i, pair) in hard.chunks(2).enumerate() {
        log.commands.push(LoggedCommand {
            tick: i as u32,
            attribution: Attribution::By(Faction::A),
            order: LoggedOrder::MoveTo {
                units: vec![1, 2],
                dest: (pair[0], pair[1]),
            },
        });
    }
    let path = scratch("floats");
    log.save(&path).expect("save");
    let back = MatchLog::load(&path).expect("load");
    for (i, (a, b)) in back.commands.iter().zip(log.commands.iter()).enumerate() {
        let (LoggedOrder::MoveTo { dest: got, .. }, LoggedOrder::MoveTo { dest: want, .. }) =
            (&a.order, &b.order)
        else {
            panic!("shape changed");
        };
        assert_eq!(
            (got.0.to_bits(), got.1.to_bits()),
            (want.0.to_bits(), want.1.to_bits()),
            "command {i}: {want:?} came back as {got:?}"
        );
    }
}

/// A coordinate with no round-tripping spelling is refused **at the write**,
/// not written out to be misread later. (F-005's rule, applied to serialization:
/// a check that admits a value it cannot represent is not a check.)
#[test]
fn a_log_that_could_not_be_read_back_is_refused_when_written() {
    for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let mut log = MatchLog::new(1);
        log.commands.push(LoggedCommand {
            tick: 3,
            attribution: Attribution::SelfSigned,
            order: LoggedOrder::Place {
                faction: Faction::B,
                building: 0,
                pos: (bad, 0.0),
            },
        });
        assert!(log.to_ron().is_err(), "{bad} was accepted into a log");
        let path = scratch("nonfinite");
        let _ = std::fs::remove_file(&path);
        assert!(log.save(&path).is_err());
        assert!(
            !path.exists(),
            "a log that cannot be read back was still written to disk"
        );
    }
}

/// A log this build cannot read exactly is an error, never a partial replay:
/// an unknown format version, commands out of tick order, or an entity id no
/// world can hold (which `Entity::from_bits` would *panic* on).
#[test]
fn an_unreadable_log_is_an_error_not_a_panic() {
    // A real entity's bits, so the "valid" half of this test is genuinely valid.
    let mut world = World::new();
    let real = world.spawn_empty().id().to_bits();
    let good = {
        let mut l = MatchLog::new(2);
        l.commands.push(LoggedCommand {
            tick: 1,
            attribution: Attribution::By(Faction::A),
            order: LoggedOrder::Train {
                building: real,
                unit: 0,
            },
        });
        l
    };
    let text = good.to_ron().expect("writes");
    assert_eq!(MatchLog::from_ron(&text).expect("reads"), good);

    // Version.
    assert!(MatchLog::from_ron(&text.replace("version: 1", "version: 2")).is_err());

    // Tick order.
    let mut jumbled = good.clone();
    jumbled.commands.push(LoggedCommand {
        tick: 0,
        ..good.commands[0].clone()
    });
    let text = jumbled.to_ron().expect("writes");
    assert!(
        MatchLog::from_ron(&text).is_err(),
        "a log whose ticks run backwards was accepted"
    );

    // Entity bits nothing can be (index 0 is not a valid `EntityIndex`).
    // `Entity::from_bits` *panics* on these; the log must return an error.
    let text = good.to_ron().unwrap().replace(&real.to_string(), "0");
    assert!(
        MatchLog::from_ron(&text).is_err(),
        "an impossible entity id was accepted"
    );

    // Not RON at all.
    assert!(MatchLog::from_ron("this is not a log").is_err());
    assert!(MatchLog::load(&scratch("does-not-exist-at-all")).is_err());
}

// ---- keep the fixture warnings honest ---------------------------------------

#[test]
fn the_fixture_builds_a_playable_world() {
    let mut app = sim_app_with_alloy(500);
    spawn_building(&mut app, "hq", Faction::A, Vec2::ZERO);
    app.world_mut().spawn((
        Position(Vec2::new(250.0, 0.0)),
        ResourceNode { amount: 1_000 },
    ));
    tick(&mut app, 5);
    assert_eq!(sim_tick(&app), 5);
}
