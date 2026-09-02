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
use onus::sim::replay::{CommandFate, LoggedCommand, LoggedOrder, MatchLog, SimId, SimIds};
use onus::sim::{
    AiCommanders, AiJournal, Attribution, CommandLog, CommandQueue, CommandTick, MatchState,
    MoveTarget, Order, Position, RateReport, ReplaySource, ResourceNode, StateHashLog,
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
            schedule: CommandTick::Asap,
            fate: CommandFate::Taken,
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
            schedule: CommandTick::Asap,
            fate: CommandFate::Taken,
            attribution: Attribution::SelfSigned,
            order: LoggedOrder::Place {
                faction: Faction::B,
                building: "hq".to_string(),
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
    let good = {
        let mut l = MatchLog::new(2);
        l.commands.push(LoggedCommand {
            tick: 1,
            schedule: CommandTick::Asap,
            fate: CommandFate::Taken,
            attribution: Attribution::By(Faction::A),
            order: LoggedOrder::Train {
                building: 3,
                unit: "worker".to_string(),
            },
        });
        l
    };
    let text = good.to_ron().expect("writes");
    assert_eq!(MatchLog::from_ron(&text).expect("reads"), good);

    // Version.
    assert!(MatchLog::from_ron(&text.replace("version: 2", "version: 3")).is_err());

    // Tick order, and a command naming an entity the sim never identified: no
    // replay can resolve either, so both are refused — at *both* boundaries,
    // which is why the text under test is serialized directly rather than
    // through `to_ron` (which now refuses them too).
    let raw = |log: &MatchLog| {
        ron::ser::to_string_pretty(log, ron::ser::PrettyConfig::default()).expect("serializes")
    };
    let mut jumbled = good.clone();
    jumbled.commands.push(LoggedCommand {
        tick: 0,
        ..good.commands[0].clone()
    });
    assert!(jumbled.to_ron().is_err(), "a backwards log was written");
    assert!(
        MatchLog::from_ron(&raw(&jumbled)).is_err(),
        "a log whose ticks run backwards was accepted"
    );

    let mut nameless = good.clone();
    nameless.commands[0].order = LoggedOrder::MoveTo {
        units: vec![0, SimId::UNIDENTIFIED.0],
        dest: (1.0, 2.0),
    };
    assert!(
        nameless.to_ron().is_err(),
        "a log naming an unidentified entity was written"
    );
    assert!(
        MatchLog::from_ron(&raw(&nameless)).is_err(),
        "a command naming an unidentified entity was accepted"
    );

    // Not RON at all.
    assert!(MatchLog::from_ron("this is not a log").is_err());
    assert!(MatchLog::load(&scratch("does-not-exist-at-all")).is_err());
}

// ---- AC3: a replay reproduces the match, tick for tick ----------------------

/// The starting world a match is played from, built the same way twice. A
/// replay is only meaningful against an identical starting world — that is the
/// other half of `(world, seed, log)`.
fn recorded_run(seed: u64, ticks: u32) -> (StateHashLog, MatchLog, App) {
    let mut app = ai_vs_ai(seed);
    app.insert_resource(StateHashLog::default());
    tick(&mut app, ticks);
    let hashes = app.world().resource::<StateHashLog>().clone();
    let log = app.world().resource::<CommandLog>().log().clone();
    (hashes, log, app)
}

/// A world identical to `ai_vs_ai`'s, with no AI: the replay drives it entirely
/// from the log.
fn replay_run(log: MatchLog, ticks: u32) -> (StateHashLog, App) {
    let mut app = ai_vs_ai(log.seed);
    app.world_mut().remove_resource::<AiCommanders>();
    app.insert_resource(AiCommanders::default());
    app.insert_resource(StateHashLog::default());
    app.insert_resource(CommandLog::new(log.seed));
    app.insert_resource(ReplaySource::new(log));
    tick(&mut app, ticks);
    let hashes = app.world().resource::<StateHashLog>().clone();
    (hashes, app)
}

/// **AC3.** Record an AI-vs-AI match, persist its log to disk, load it back,
/// and replay it into a fresh world with no AI at all: every tick's state hash
/// must match, and so must the final state.
#[test]
fn a_replay_of_the_persisted_log_reproduces_the_match_tick_for_tick() {
    const TICKS: u32 = 3_000;
    let (recorded, log, mut original) = recorded_run(4, TICKS);
    let path = scratch("replay");
    log.save(&path).expect("save");
    let loaded = MatchLog::load(&path).expect("load");

    let (replayed, mut app) = replay_run(loaded, TICKS);
    assert_eq!(recorded.0.len(), TICKS as usize, "a hash per tick");
    assert_eq!(
        recorded.first_divergence(&replayed),
        None,
        "the replay diverged from the recording"
    );

    // The final state, spelled out — a hash that matched but a world that did
    // not would mean the hash is the thing that is wrong.
    assert_eq!(
        app.world().resource::<Stockpiles>().alloy(Faction::A),
        original.world().resource::<Stockpiles>().alloy(Faction::A)
    );
    assert_eq!(
        app.world().resource::<Stockpiles>().alloy(Faction::B),
        original.world().resource::<Stockpiles>().alloy(Faction::B)
    );
    assert_eq!(
        app.world().resource::<Casualties>().total(),
        original.world().resource::<Casualties>().total()
    );
    assert_eq!(
        app.world().resource::<MatchState>().outcome(),
        original.world().resource::<MatchState>().outcome()
    );
    assert_eq!(
        onus::sim::state_hash(app.world_mut()),
        onus::sim::state_hash(original.world_mut()),
        "the replayed world is not the recorded world"
    );
    // The replay really did consume the log (rather than reproducing the match
    // by coincidence, with the AI still playing).
    let source = app.world().resource::<ReplaySource>();
    assert!(source.cursor() > 20, "the replay fed almost nothing");
    assert_eq!(source.skipped(), 0, "the replay skipped commands");
    assert!(
        app.world().resource::<AiJournal>().0.is_empty(),
        "the AI was still thinking during the replay"
    );
}

/// A replay all the way to the end of the match: the outcome, and the tick it
/// was decided on, are reproduced.
#[test]
fn a_replay_reaches_the_same_verdict_on_the_same_tick() {
    const BUDGET: u32 = 8 * 60 * 60;
    let mut app = ai_vs_ai(7);
    app.insert_resource(StateHashLog::default());
    let mut played = 0;
    while app.world().resource::<MatchState>().outcome().is_none() && played < BUDGET {
        step(&mut app);
        played += 1;
    }
    let decided = app
        .world()
        .resource::<MatchState>()
        .outcome()
        .expect("the match decided inside the budget");
    let recorded = app.world().resource::<StateHashLog>().clone();
    let log = app.world().resource::<CommandLog>().log().clone();

    let (replayed, app2) = replay_run(log, played);
    assert_eq!(
        app2.world().resource::<MatchState>().outcome(),
        Some(decided),
        "the replay reached a different verdict"
    );
    assert_eq!(recorded.first_divergence(&replayed), None);
}

/// The property a replay is worthless without: a *different* log gives a
/// different match. Drop one command and the hashes must part company — so the
/// AC3 test cannot be passing because the log is ignored.
#[test]
fn a_log_missing_one_command_replays_into_a_different_match() {
    const TICKS: u32 = 1_500;
    let (recorded, log, _) = recorded_run(4, TICKS);
    let mut damaged = log.clone();
    let dropped = damaged.commands.len() / 2;
    let at = damaged.commands[dropped].tick;
    damaged.commands.remove(dropped);
    let (replayed, _) = replay_run(damaged, TICKS);
    let divergence = recorded
        .first_divergence(&replayed)
        .expect("dropping a command changed nothing");
    assert!(
        divergence >= at as usize,
        "the runs diverged at tick {divergence}, before the command dropped at tick {at}"
    );
}

/// A stray live order during a replay — a click, a fixture, an editor — is
/// **discarded**, not applied: the recorded commands are the only commands.
#[test]
fn a_rogue_live_order_cannot_desync_a_replay() {
    const TICKS: u32 = 1_200;
    let (recorded, log, _) = recorded_run(4, TICKS);
    let mut app = ai_vs_ai(log.seed);
    app.world_mut().remove_resource::<AiCommanders>();
    app.insert_resource(AiCommanders::default());
    app.insert_resource(StateHashLog::default());
    app.insert_resource(CommandLog::new(log.seed));
    app.insert_resource(ReplaySource::new(log));
    let victim = spawn_unit(&mut app, "ripper", Faction::A, Vec2::new(-700.0, 40.0));
    for t in 0..TICKS {
        if t % 17 == 0 {
            app.world_mut().resource_mut::<CommandQueue>().0.push_back(
                Order::MoveTo {
                    units: vec![victim],
                    dest: Vec2::new(t as f32, 900.0),
                }
                .issued_by(Faction::A),
            );
        }
        step(&mut app);
    }
    let replayed = app.world().resource::<StateHashLog>().clone();
    assert!(
        app.world().resource::<ReplaySource>().discarded() > 50,
        "the rogue orders were never seen"
    );
    // The extra unit is not in the recorded world, so the hashes cannot be
    // compared directly — what must hold is that no rogue order was *applied*.
    assert!(
        app.world().get::<MoveTarget>(victim).is_none(),
        "a live order was applied during a replay"
    );
    assert_eq!(replayed.0.len(), TICKS as usize);
    assert_eq!(recorded.0.len(), replayed.0.len());
}

/// Replaying a log produces the same log: the recording is a fixed point, which
/// is what makes it safe to record while replaying (and is how M6 will compare
/// two peers' streams).
#[test]
fn a_replay_records_the_same_log_it_was_given() {
    const TICKS: u32 = 1_500;
    let (_, log, _) = recorded_run(4, TICKS);
    let (_, app) = replay_run(log.clone(), TICKS);
    let again = app.world().resource::<CommandLog>().log().clone();
    assert_eq!(again.commands, log.commands, "the replay logged a different stream");
    assert_eq!(app.world().resource::<CommandLog>().late(), 0);
}

// ---- the state hash itself --------------------------------------------------

/// The hash has to *notice* the state it claims to cover. Each field is changed
/// on its own, and each must move the hash.
#[test]
fn the_state_hash_covers_every_piece_of_state_it_claims_to() {
    let base = |seed_ticks: u32| {
        let mut app = ai_vs_ai(4);
        tick(&mut app, seed_ticks);
        app
    };
    let hash_of = |app: &mut App| onus::sim::state_hash(app.world_mut());

    #[allow(clippy::type_complexity)]
    let mutate: Vec<(&str, Box<dyn Fn(&mut App)>)> = vec![
        (
            "position",
            Box::new(|app: &mut App| {
                let e = first_with::<Position>(app);
                app.world_mut().get_mut::<Position>(e).unwrap().0.x += 1.0;
            }),
        ),
        (
            "position, by one bit",
            Box::new(|app: &mut App| {
                let e = first_with::<Position>(app);
                let mut p = app.world_mut().get_mut::<Position>(e).unwrap();
                p.0.x = f32::from_bits(p.0.x.to_bits() ^ 1);
            }),
        ),
        (
            "health",
            Box::new(|app: &mut App| {
                let e = first_with::<Health>(app);
                app.world_mut().get_mut::<Health>(e).unwrap().current -= 1;
            }),
        ),
        (
            "stockpile",
            Box::new(|app: &mut App| {
                let _ = app
                    .world_mut()
                    .resource_mut::<Stockpiles>()
                    .add(Faction::B, 1);
            }),
        ),
        (
            "production queue",
            Box::new(|app: &mut App| {
                let e = first_with::<ProductionQueue>(app);
                app.world_mut()
                    .get_mut::<ProductionQueue>(e)
                    .unwrap()
                    .items
                    .push_back(onus::sim::QueuedUnit {
                        unit: 0,
                        ticks_left: 5,
                    });
            }),
        ),
        (
            "carried alloy",
            Box::new(|app: &mut App| {
                let e = first_with::<Position>(app);
                app.world_mut().entity_mut(e).insert(onus::sim::Carrying(3));
            }),
        ),
        (
            "gather claim",
            Box::new(|app: &mut App| {
                let e = first_with::<Position>(app);
                app.world_mut()
                    .entity_mut(e)
                    .insert(onus::sim::GatherPhase::ToDropoff);
            }),
        ),
        (
            "resource node",
            Box::new(|app: &mut App| {
                let e = first_with::<ResourceNode>(app);
                app.world_mut().get_mut::<ResourceNode>(e).unwrap().amount -= 1;
            }),
        ),
        (
            "casualties",
            Box::new(|app: &mut App| {
                let e = first_with::<Health>(app);
                app.world_mut().despawn(e);
            }),
        ),
        (
            "match state",
            Box::new(|app: &mut App| {
                step(app);
            }),
        ),
    ];

    for (what, change) in mutate {
        let mut app = base(400);
        let before = hash_of(&mut app);
        change(&mut app);
        let after = hash_of(&mut app);
        assert_ne!(before, after, "the state hash ignores {what}");
    }
}

/// ...and must **not** notice anything that is not sim state: a component the
/// sim never reads, or the archetype shuffling that adding one causes.
#[test]
fn the_state_hash_ignores_what_is_not_sim_state() {
    #[derive(Component)]
    struct Decoration(#[allow(dead_code)] u32);

    let mut app = ai_vs_ai(4);
    tick(&mut app, 400);
    let before = onus::sim::state_hash(app.world_mut());

    let ents: Vec<Entity> = {
        let mut q = app.world_mut().query_filtered::<Entity, With<Position>>();
        q.iter(app.world()).collect()
    };
    for (i, e) in ents.iter().enumerate() {
        app.world_mut().entity_mut(*e).insert(Decoration(i as u32));
    }
    assert_eq!(
        onus::sim::state_hash(app.world_mut()),
        before,
        "a non-sim component moved the state hash"
    );

    // Hashing is a pure read: doing it repeatedly changes nothing.
    assert_eq!(onus::sim::state_hash(app.world_mut()), before);
    assert_eq!(onus::sim::state_hash(app.world_mut()), before);

    // The AI journal and the command log are records *about* the run, not state
    // the run reads: a replay has neither, and must still hash equal.
    app.world_mut().resource_mut::<AiJournal>().0.clear();
    assert_eq!(onus::sim::state_hash(app.world_mut()), before);
}

fn first_with<C: Component>(app: &mut App) -> Entity {
    let mut q = app.world_mut().query_filtered::<Entity, With<C>>();
    let mut all: Vec<Entity> = q.iter(app.world()).collect();
    all.sort_by_key(|e| e.to_bits());
    *all.first().expect("an entity with that component")
}

// ---- the addressing scheme the log depends on -------------------------------

/// The reason the log is keyed on `SimId` and not on `Entity`: entity ids are an
/// ECS allocation detail. Two apps that hold the *same sim state* but differ by
/// one resource hand out different entity ids — and must still agree on every
/// sim id and on the state hash.
#[test]
fn sim_ids_and_the_state_hash_survive_a_difference_in_entity_allocation() {
    #[derive(Resource)]
    struct Unrelated(#[allow(dead_code)] u32);

    let run = |extra: bool| {
        let mut app = ai_vs_ai(4);
        if extra {
            app.insert_resource(Unrelated(1));
            app.insert_resource(StateHashLog::default());
        }
        tick(&mut app, 600);
        let ids: Vec<(u64, [u32; 2])> = {
            let mut q = app.world_mut().query::<(&SimId, &Position)>();
            let mut v: Vec<(u64, [u32; 2])> = q
                .iter(app.world())
                .map(|(id, p)| (id.0, [p.0.x.to_bits(), p.0.y.to_bits()]))
                .collect();
            v.sort();
            v
        };
        let entity_bits: Vec<u64> = {
            let mut q = app.world_mut().query_filtered::<Entity, With<SimId>>();
            let mut v: Vec<u64> = q.iter(app.world()).map(|e| e.to_bits()).collect();
            v.sort();
            v
        };
        (ids, entity_bits, onus::sim::state_hash(app.world_mut()))
    };
    let plain = run(false);
    let padded = run(true);
    assert_ne!(
        plain.1, padded.1,
        "the two apps allocated identical entity ids — this test proves nothing \
         unless they differ, so the fixture needs a bigger difference"
    );
    assert_eq!(plain.0, padded.0, "sim ids moved with the entity ids");
    assert_eq!(
        plain.2, padded.2,
        "the state hash moved with the entity ids"
    );
}

/// Every thing in the world is identified — before its tick is hashed, and
/// before the next tick's orders can name it — so a real match's log never
/// contains an unresolvable command.
#[test]
fn everything_in_a_live_match_is_identified_and_the_log_resolves() {
    let mut app = ai_vs_ai(4);
    for t in 0..1_500u32 {
        step(&mut app);
        let unidentified = {
            let mut q = app
                .world_mut()
                .query_filtered::<Entity, (With<Position>, Without<SimId>)>();
            q.iter(app.world()).count()
        };
        assert_eq!(
            unidentified, 0,
            "tick {t}: something in the world has no sim id"
        );
    }
    let log = app.world().resource::<CommandLog>().log().clone();
    assert!(log.commands.len() > 20);
    for c in &log.commands {
        for id in c.order.sim_ids() {
            assert!(
                id.is_identified(),
                "the log names an unidentified entity at tick {}",
                c.tick
            );
        }
    }
    // And such a log loads (the `from_ron` guard is about hand-made logs).
    assert_eq!(MatchLog::from_ron(&log.to_ron().unwrap()).unwrap(), log);
}

// ---- AC4: the seeded RNG is stepped only inside the sim ---------------------

/// The sim's only generator is `SplitMix64`, and it lives — and is stepped —
/// only under `src/sim/`. Source-level over the **whole** driver, not just the
/// sim, because the failure this rules out is the driver rolling a number and
/// feeding it in: the sim would still be "deterministic given its inputs" and
/// the match would still not replay.
#[test]
fn the_only_generator_lives_in_the_sim_and_nothing_else_rolls_a_number() {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut offenders: Vec<String> = Vec::new();
    let mut stack = vec![src.clone()];
    let mut files: Vec<PathBuf> = Vec::new();
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("read src") {
            let p = entry.expect("entry").path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|e| e == "rs") {
                files.push(p);
            }
        }
    }
    files.sort();
    for path in &files {
        let rel = path
            .strip_prefix(&src)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let in_sim = rel.starts_with("sim/");
        let text = std::fs::read_to_string(path).expect("read source");
        for (i, line) in text.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            // Anything that produces randomness, anywhere.
            // (`random(` on its own would flag `TileGrid::random`, the sim's
            // own *seeded* fixture generator — the needles below are the ways a
            // program gets a number it did not seed.)
            for needle in ["rand::", "thread_rng", "::random()", "getrandom"] {
                if code.contains(needle) {
                    offenders.push(format!("{rel}:{}: {}", i + 1, code.trim()));
                }
            }
            // The project's own generator: sim only.
            for needle in ["SplitMix64", "next_u64(", "next_f32(", "range_f32("] {
                if code.contains(needle) && !in_sim {
                    offenders.push(format!("{rel}:{}: {}", i + 1, code.trim()));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "randomness outside the sim: {offenders:#?}"
    );

    // ...and no RNG crate is a dependency at all, so there is nothing else to
    // roll with.
    let manifest = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"),
    )
    .expect("read Cargo.toml");
    for line in manifest.lines() {
        let dep = line.split('#').next().unwrap_or("").trim();
        assert!(
            !dep.starts_with("rand") && !dep.starts_with("fastrand"),
            "an RNG crate is a dependency: {dep}"
        );
    }
}

/// The generator advances **only while the sim is running**, and by exactly the
/// sim's own schedule: frames with the clock stopped do not move it, and neither
/// do the ticks after the match is decided (the chain is off).
#[test]
fn the_rng_advances_only_when_the_sim_ticks() {
    let rng_state = |app: &App| -> Vec<u64> {
        app.world()
            .resource::<AiCommanders>()
            .commanders()
            .iter()
            .map(|c| c.rng_state())
            .collect()
    };

    // 1. A shipped-shape app (sim in `FixedUpdate`) with the clock stopped:
    //    frames go by, the RNG does not move.
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .insert_resource(Time::<Fixed>::from_hz(60.0))
        .insert_resource(content())
        .init_resource::<CommandQueue>()
        .init_resource::<RateReport>()
        .init_resource::<Casualties>()
        .insert_resource(Stockpiles::starting(content().economy.starting_alloy));
    onus::add_sim_systems(&mut app, FixedUpdate);
    spawn_building(&mut app, "hq", Faction::A, Vec2::ZERO);
    app.world_mut().spawn((
        Position(Vec2::new(250.0, 0.0)),
        ResourceNode { amount: 100_000 },
    ));
    spawn_unit(&mut app, "worker", Faction::A, Vec2::ZERO);
    app.insert_resource(AiCommanders::new(3, &[Faction::A]));
    app.world_mut().resource_mut::<Time<Virtual>>().pause();
    let before = rng_state(&app);
    for _ in 0..60 {
        app.update();
    }
    assert_eq!(
        rng_state(&app),
        before,
        "the RNG moved on a frame the sim did not tick"
    );

    // 2. Running the sim does move it (or the test above proves nothing).
    let mut app = ai_vs_ai(9);
    let start = rng_state(&app);
    tick(&mut app, 600);
    let running = rng_state(&app);
    assert_ne!(running, start, "the AI never stepped its RNG at all");

    // 3. Past the end of the match, the chain is off and so is the RNG.
    const BUDGET: u32 = 8 * 60 * 60;
    let mut played = 0;
    while app.world().resource::<MatchState>().outcome().is_none() && played < BUDGET {
        step(&mut app);
        played += 1;
    }
    assert!(
        app.world().resource::<MatchState>().outcome().is_some(),
        "the match never ended, so the freeze is untested"
    );
    let at_end = rng_state(&app);
    tick(&mut app, 300);
    assert_eq!(
        rng_state(&app),
        at_end,
        "the RNG kept rolling after the match was decided"
    );
}

/// A replay steps **no** RNG: the commanders are stood down, so even a replay
/// app that was handed seeded commanders leaves their streams untouched — and
/// still reproduces the match. (The decisions those streams produced are in the
/// log, as orders.)
#[test]
fn a_replay_reproduces_the_match_without_stepping_the_rng() {
    const TICKS: u32 = 1_200;
    let (recorded, log, _) = recorded_run(4, TICKS);
    let mut app = ai_vs_ai(log.seed);
    app.insert_resource(StateHashLog::default());
    app.insert_resource(CommandLog::new(log.seed));
    app.insert_resource(ReplaySource::new(log));
    let before: Vec<u64> = app
        .world()
        .resource::<AiCommanders>()
        .commanders()
        .iter()
        .map(|c| c.rng_state())
        .collect();
    assert!(!before.is_empty(), "the replay app has no commanders to freeze");
    tick(&mut app, TICKS);
    let after: Vec<u64> = app
        .world()
        .resource::<AiCommanders>()
        .commanders()
        .iter()
        .map(|c| c.rng_state())
        .collect();
    assert_eq!(before, after, "a replay stepped the AI's RNG");
    assert_eq!(
        recorded.first_divergence(&app.world().resource::<StateHashLog>().clone()),
        None,
        "the replay diverged"
    );
}

/// One seed fixes both commanders' streams, and two seeds do not share one.
#[test]
fn the_seed_is_the_whole_of_the_randomness() {
    let states = |seed: u64| {
        AiCommanders::new(seed, &[Faction::A, Faction::B])
            .commanders()
            .iter()
            .map(|c| c.rng_state())
            .collect::<Vec<u64>>()
    };
    assert_eq!(states(11), states(11), "one seed, two streams");
    assert_ne!(states(11), states(12), "two seeds, one stream");
    let one = states(11);
    assert_ne!(one[0], one[1], "both commanders share a stream");
}

// ---- the M4c carry-over: nothing shipped is self-signed ---------------------

/// M4c left one item open: a `src/` emitter that forgets `.issued_by(..)`
/// self-signs rather than failing loudly. Self-signing is *safe* (an order with
/// no coherent issuer is refused whole — F-009), but it is not something any
/// shipped producer should ever do, and until M5 there was no record of what the
/// sim was actually asked to do.
///
/// The command log is that record: every command the sim consumed, with its
/// attribution. So the claim is now checked over **values reaching the sim**,
/// rather than over the spelling of the call sites (the text scan F-009
/// deleted): in a shipped match, every command is signed.
#[test]
fn every_command_a_shipped_match_applies_is_signed() {
    let mut app = ai_vs_ai(4);
    for t in 0..3_000u32 {
        step(&mut app);
        // Per tick, not just at the end: a self-signed order that appears and is
        // consumed mid-match would not show up in an end-state check.
        let queue_unsigned = {
            let q = app.world().resource::<CommandQueue>();
            q.0.iter()
                .filter(|c| c.attribution() != Attribution::By(Faction::A)
                    && c.attribution() != Attribution::By(Faction::B))
                .count()
        };
        assert_eq!(
            queue_unsigned, 0,
            "tick {t}: the queue holds an unsigned order"
        );
    }
    let log = app.world().resource::<CommandLog>();
    assert!(log.commands().len() > 20, "nothing was ordered at all");
    for c in log.commands() {
        assert!(
            matches!(c.attribution, Attribution::By(_)),
            "tick {}: a {:?} command reached the sim {:?}",
            c.tick,
            c.attribution,
            c.order
        );
    }
    // And the log distinguishes them, so this test could fail: an unsigned
    // order pushed by hand is recorded as self-signed.
    let unit = spawn_unit(&mut app, "ripper", Faction::A, Vec2::new(-700.0, 0.0));
    app.world_mut().resource_mut::<CommandQueue>().0.push_back(Order::MoveTo {
        units: vec![unit],
        dest: Vec2::new(0.0, 0.0),
    });
    step(&mut app);
    assert!(
        app.world()
            .resource::<CommandLog>()
            .commands()
            .iter()
            .any(|c| c.attribution == Attribution::SelfSigned),
        "the log cannot tell a self-signed command from a signed one"
    );
}

// ---- the hash sees the state that has not fired yet -------------------------

/// A command the sim has accepted and is holding for a future tick is state the
/// sim owns. Two worlds identical but for one of them must not hash equal:
/// waiting until it fires is waiting until after the divergence.
#[test]
fn the_hash_sees_a_command_held_for_a_future_tick() {
    let mut a = sim_app();
    let unit_a = spawn_unit(&mut a, "ripper", Faction::A, Vec2::ZERO);
    let mut b = sim_app();
    let _unit_b = spawn_unit(&mut b, "ripper", Faction::A, Vec2::ZERO);
    tick(&mut a, 5);
    tick(&mut b, 5);
    assert_eq!(
        onus::sim::state_hash(a.world_mut()),
        onus::sim::state_hash(b.world_mut()),
        "the fixtures were not identical to begin with"
    );
    push_at(
        &mut a,
        400,
        Order::MoveTo {
            units: vec![unit_a],
            dest: Vec2::new(9.0, 9.0),
        }
        .issued_by(Faction::A),
    );
    step(&mut a);
    step(&mut b);
    let (ha, hb) = (
        onus::sim::state_hash(a.world_mut()),
        onus::sim::state_hash(b.world_mut()),
    );
    assert_ne!(ha, hb, "a held command is invisible to the state hash");

    // ...and it stays visible for every tick it is held, not just the first.
    for t in 0..50 {
        step(&mut a);
        step(&mut b);
        assert_ne!(
            onus::sim::state_hash(a.world_mut()),
            onus::sim::state_hash(b.world_mut()),
            "the held command became invisible again {t} ticks later"
        );
    }
}

/// Same for the registry: a difference in the next id to be issued is invisible
/// until the next spawn, which is one spawn too late.
#[test]
fn the_hash_sees_the_next_sim_id_to_be_issued() {
    let mut a = sim_app();
    let mut b = sim_app();
    spawn_unit(&mut a, "ripper", Faction::A, Vec2::ZERO);
    spawn_unit(&mut b, "ripper", Faction::A, Vec2::ZERO);
    tick(&mut a, 2);
    tick(&mut b, 2);
    assert_eq!(
        onus::sim::state_hash(a.world_mut()),
        onus::sim::state_hash(b.world_mut())
    );
    // One world has issued an id the other has not: the entity that took it is
    // gone, so *nothing else* distinguishes the two worlds.
    let ghost = a
        .world_mut()
        .spawn(Position(Vec2::new(1.0, 1.0)))
        .id();
    step(&mut a);
    step(&mut b);
    a.world_mut().despawn(ghost);
    assert_ne!(
        onus::sim::state_hash(a.world_mut()),
        onus::sim::state_hash(b.world_mut()),
        "the registry's next id is invisible to the state hash"
    );
}

/// The direction the new rows could break: identical worlds *with* pending
/// commands still hash equal, and the rows are sensitive to the order the sim
/// will apply them in (a queue is a sequence, not a set).
#[test]
fn pending_command_rows_are_equal_for_equal_queues_and_ordered() {
    let build = |swap: bool| {
        let mut app = sim_app();
        let u = spawn_unit(&mut app, "ripper", Faction::A, Vec2::ZERO);
        tick(&mut app, 3);
        let first = Order::MoveTo {
            units: vec![u],
            dest: Vec2::new(1.0, 0.0),
        }
        .issued_by(Faction::A);
        let second = Order::MoveTo {
            units: vec![u],
            dest: Vec2::new(2.0, 0.0),
        }
        .issued_by(Faction::A);
        if swap {
            push_at(&mut app, 300, second);
            push_at(&mut app, 300, first);
        } else {
            push_at(&mut app, 300, first);
            push_at(&mut app, 300, second);
        }
        step(&mut app);
        app
    };
    let mut plain = build(false);
    let mut same = build(false);
    let mut swapped = build(true);
    assert_eq!(
        onus::sim::state_hash(plain.world_mut()),
        onus::sim::state_hash(same.world_mut()),
        "two identical queues hash differently"
    );
    assert_ne!(
        onus::sim::state_hash(plain.world_mut()),
        onus::sim::state_hash(swapped.world_mut()),
        "the order the sim will apply held commands in is not hashed"
    );
}

/// The other direction the new rows could break, and the one that matters most:
/// a recorded match still replays hash for hash. (It does because no shipped
/// producer schedules ahead — the queue is empty at the end of every tick in
/// both runs — which the next test pins.)
#[test]
fn the_new_rows_do_not_disturb_a_recorded_replay() {
    const TICKS: u32 = 1_500;
    let (recorded, log, _) = recorded_run(4, TICKS);
    let (replayed, app) = replay_run(log, TICKS);
    assert_eq!(recorded.first_divergence(&replayed), None);
    assert!(
        app.world().resource::<CommandQueue>().0.is_empty(),
        "a replay ended a tick still holding a command"
    );
}

/// **The limit of the pending-command rows, made unreachable rather than
/// documented.** The log records a command by the tick it *applied* on, not the
/// tick it was queued on, so a producer that scheduled a command many ticks
/// ahead would make a recording hold it while the replay of that recording does
/// not — identical worlds, different hashes, until it fires. Nothing in `src/`
/// schedules ahead: `push_at` is called only by the replay itself (which pushes
/// for the current tick), and every other producer pushes `Asap`. If M6 adds
/// ahead-scheduling, the log has to record the queued tick as well as the
/// applied one.
#[test]
fn nothing_in_src_schedules_a_command_ahead_of_the_tick_it_applies_on() {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut stack = vec![src.clone()];
    let mut callers: Vec<String> = Vec::new();
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("read src") {
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
            for (i, line) in std::fs::read_to_string(&p).expect("read").lines().enumerate() {
                let code = line.split("//").next().unwrap_or("");
                // The definition itself is not a call site.
                if code.contains("push_at(") && !code.contains("pub fn push_at") {
                    callers.push(format!("{rel}:{}: {}", i + 1, code.trim()));
                }
            }
        }
    }
    assert_eq!(
        callers.len(),
        1,
        "exactly one caller of `push_at` is expected (the replay, pushing for \
         the current tick); found {callers:#?}"
    );
    assert!(
        callers[0].starts_with("sim/replay.rs"),
        "something other than the replay schedules a command: {callers:?}"
    );
    // And what it schedules is never *ahead* of the tick it is fed on. The
    // replay re-pushes each command on the schedule the log recorded, so the
    // check that matters is on the log: a command scheduled for a tick later
    // than the one the sim took it on is a story the sim cannot produce, and
    // `validate` refuses it. (Behavioural, not textual — the old spelling of
    // this check pinned a line of code rather than the property.)
    let replay = std::fs::read_to_string(src.join("sim/replay.rs")).expect("replay.rs");
    assert!(
        replay.contains("if entry.tick > now {"),
        "the replay no longer stops at the first command past the current tick"
    );
    let mut ahead = MatchLog::new(1);
    ahead.commands.push(LoggedCommand {
        tick: 5,
        schedule: CommandTick::At(900),
        fate: CommandFate::Taken,
        attribution: Attribution::By(Faction::A),
        order: LoggedOrder::MoveTo {
            units: vec![0],
            dest: (1.0, 1.0),
        },
    });
    assert!(
        ahead.validate().is_err(),
        "a log claiming a command was taken 895 ticks before its schedule was \
         accepted — ahead-scheduling is M6's to introduce, and the format must \
         not admit an incoherent version of it in the meantime"
    );
}

// ---- the producer, not just the boundary ------------------------------------

/// **The sim must never record a log its own validator refuses.** Unifying the
/// two boundaries turned a latent read failure into a live write failure until
/// the *producer* was fixed too: `apply_commands` used to mint
/// `SimId::UNIDENTIFIED` for any entity it could not resolve — including one
/// that had simply died before the command applied — and `validate` refuses
/// that value, so the log could not be written at exactly the moment it was the
/// report.
#[test]
fn a_command_naming_a_dead_unit_is_logged_by_the_id_it_had() {
    let mut app = sim_app();
    let unit = spawn_unit(&mut app, "ripper", Faction::A, Vec2::ZERO);
    tick(&mut app, 2);
    let id = app.world().get::<SimId>(unit).copied().expect("identified");
    // It dies, and only then does the order reach the sim.
    app.world_mut().despawn(unit);
    app.world_mut().resource_mut::<CommandQueue>().0.push_back(
        Order::MoveTo {
            units: vec![unit],
            dest: Vec2::new(5.0, 0.0),
        }
        .issued_by(Faction::A),
    );
    step(&mut app);
    let log = app.world().resource::<CommandLog>();
    assert_eq!(log.commands().len(), 1, "the command was not logged");
    assert_eq!(
        log.commands()[0].order.sim_ids(),
        vec![id],
        "a command naming a dead unit was not logged by the id that unit had"
    );
    assert!(
        log.log().validate().is_ok(),
        "the sim recorded a log it calls invalid: {:?}",
        log.log().validate().err()
    );
    assert_eq!(log.unrecorded(), 0);
}

/// An entity the sim has never had in its world (no `Position`, so `identify`
/// never saw it) has **no name**: it cannot be logged and a replay would have
/// nothing to point at. So the sim does not obey it either — the name is
/// dropped from the order, the rest of the order stands, and what is logged is
/// exactly what was applied.
///
/// The two shapes this replaced both failed: naming it `UNIDENTIFIED` wrote a
/// log the validator refuses, and *issuing* it an id at log-write time made the
/// record path grow the registry — hashed state the replay path cannot grow.
#[test]
fn an_order_naming_something_the_world_never_held_is_dropped_from_it() {
    let mut app = sim_app_with_alloy(1_000);
    let real = spawn_unit(&mut app, "ripper", Faction::A, Vec2::ZERO);
    tick(&mut app, 2);
    let real_id = app.world().get::<SimId>(real).copied().expect("identified");
    let issued_before = app.world().resource::<SimIds>().issued();

    let bare = app.world_mut().spawn_empty().id();
    app.world_mut().resource_mut::<CommandQueue>().0.push_back(
        Order::MoveTo {
            units: vec![bare, real],
            dest: Vec2::new(40.0, 0.0),
        }
        .issued_by(Faction::A),
    );
    step(&mut app);

    // The nameable half of the order was obeyed...
    assert!(
        app.world().get::<MoveTarget>(real).is_some(),
        "dropping the unnameable name threw the whole order away"
    );
    assert!(app.world().get::<MoveTarget>(bare).is_none());
    // ...and the log says exactly that, naming one entity, not two.
    let log = app.world().resource::<CommandLog>();
    assert_eq!(log.commands().len(), 1);
    assert_eq!(
        log.commands()[0].order.sim_ids(),
        vec![real_id],
        "the log does not match what was applied"
    );
    assert_eq!(log.unnameable(), 1, "the refusal was not counted");
    // Nothing was issued an id by writing the log.
    assert_eq!(
        app.world().resource::<SimIds>().issued(),
        issued_before,
        "logging an order issued a new sim id"
    );

    // An order whose *single* subject is unnameable is refused whole: nothing
    // logged, nothing charged.
    let alloy_before = app.world().resource::<Stockpiles>().alloy(Faction::A);
    let ghost = app.world_mut().spawn_empty().id();
    app.world_mut().resource_mut::<CommandQueue>().0.push_back(
        Order::Train {
            building: ghost,
            unit: 0,
        }
        .issued_by(Faction::A),
    );
    step(&mut app);
    assert_eq!(app.world().resource::<CommandLog>().commands().len(), 1);
    assert_eq!(
        app.world().resource::<Stockpiles>().alloy(Faction::A),
        alloy_before
    );
    let log = app.world().resource::<CommandLog>().log().clone();
    let path = scratch("unnameable");
    log.save(&path).expect("the sim's log must be writable");
    assert_eq!(MatchLog::load(&path).expect("load"), log);
}

/// **The invariant behind that choice: the record path and the replay path
/// mutate the registry identically.** Ids are issued in one place, `identify`,
/// which both paths run; nothing else may grow the registry, because the replay
/// cannot reproduce a growth that came from writing the log.
///
/// A recording that contains an order naming a thing outside the world — the
/// case that broke it — must replay id-for-id and hash-for-hash, and a building
/// placed after that order must carry the same id in both runs.
#[test]
fn the_record_and_replay_paths_grow_the_registry_identically() {
    const TICKS: u32 = 600;
    let world = |seed: u64| {
        let mut app = sim_app_with_alloy(5_000);
        spawn_building(&mut app, "hq", Faction::A, Vec2::new(-100.0, 0.0));
        spawn_unit(&mut app, "ripper", Faction::A, Vec2::ZERO);
        app.insert_resource(StateHashLog::default());
        app.insert_resource(CommandLog::new(seed));
        app
    };

    // Record: an order naming a bare entity at tick 10, a building placed at
    // tick 20 (so its id is the one that would shift), and ids sampled per tick.
    let mut rec = world(3);
    let mut rec_ids = Vec::new();
    let foundry = rec
        .world()
        .resource::<Content>()
        .building_index("foundry")
        .expect("foundry");
    for t in 0..TICKS {
        if t == 10 {
            let bare = rec.world_mut().spawn_empty().id();
            rec.world_mut().resource_mut::<CommandQueue>().0.push_back(
                Order::MoveTo {
                    units: vec![bare],
                    dest: Vec2::new(1.0, 1.0),
                }
                .issued_by(Faction::A),
            );
        }
        if t == 20 {
            rec.world_mut().resource_mut::<CommandQueue>().0.push_back(
                Order::Place {
                    faction: Faction::A,
                    building: foundry,
                    pos: Vec2::new(200.0, 0.0),
                }
                .issued_by(Faction::A),
            );
        }
        step(&mut rec);
        rec_ids.push(rec.world().resource::<SimIds>().issued());
    }
    let log = rec.world().resource::<CommandLog>().log().clone();
    let hashes = rec.world().resource::<StateHashLog>().clone();
    log.save(&scratch("registry-parity")).expect("saves");

    // Replay the same log into the same starting world.
    let mut rep = world(3);
    rep.insert_resource(ReplaySource::new(log));
    let mut rep_ids = Vec::new();
    for _ in 0..TICKS {
        step(&mut rep);
        rep_ids.push(rep.world().resource::<SimIds>().issued());
    }

    // The registry has to have actually grown, or the comparison is vacuous.
    assert!(
        rec_ids.last() > rec_ids.first(),
        "the fixture never issued a new id: {rec_ids:?}"
    );
    assert_eq!(
        rec_ids.iter().zip(rep_ids.iter()).position(|(a, b)| a != b),
        None,
        "the registry grew differently: recorded {rec_ids:?} vs replayed {rep_ids:?}"
    );
    assert_eq!(
        hashes.first_divergence(&rep.world().resource::<StateHashLog>().clone()),
        None,
        "the replay of a log containing an unnameable order diverged"
    );
    // The building really was placed, and carries the same id in both runs.
    let id_of_building = |app: &mut App| {
        let mut q = app.world_mut().query::<(&SimId, &Building)>();
        let mut v: Vec<u64> = q
            .iter(app.world())
            .filter(|(_, b)| b.def == foundry)
            .map(|(id, _)| id.0)
            .collect();
        v.sort();
        v
    };
    let placed = id_of_building(&mut rec);
    assert_eq!(placed.len(), 1, "the fixture never placed its building");
    assert_eq!(placed, id_of_building(&mut rep), "the ids drifted");
}

/// The structural half of the same invariant: the record path takes the
/// registry **read-only**, and the only place an id is issued is `identify`.
/// (Prose would not have stopped the last one; this does.)
#[test]
fn only_identify_can_issue_a_sim_id() {
    let sim = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sim");
    let mod_rs = std::fs::read_to_string(sim.join("mod.rs")).expect("mod.rs");
    assert!(
        mod_rs.contains("ids: Option<Res<replay::SimIds>>"),
        "`apply_commands` no longer takes the registry read-only — writing the \
         log could mutate it again"
    );
    let replay = std::fs::read_to_string(sim.join("replay.rs")).expect("replay.rs");
    // `assign` is the only thing that grows the registry.
    let callers: Vec<(usize, String)> = replay
        .lines()
        .enumerate()
        .filter(|(_, l)| {
            let code = l.split("//").next().unwrap_or("");
            code.contains("assign(") && !code.contains("fn assign(")
        })
        .map(|(i, l)| (i + 1, l.trim().to_string()))
        .collect();
    assert_eq!(
        callers.len(),
        1,
        "the registry is grown from more than one place: {callers:?}"
    );
    // ...and that one call site is inside `identify`.
    let identify_at = replay.find("pub fn identify(").expect("identify exists");
    let identify_line = replay[..identify_at].lines().count() + 1;
    let identify_end = identify_line
        + replay[identify_at..]
            .find("\n}\n")
            .map(|i| replay[identify_at..identify_at + i].lines().count())
            .expect("identify has a body");
    assert!(
        (identify_line..=identify_end).contains(&callers[0].0),
        "the registry is grown outside `identify` (line {} of replay.rs: {})",
        callers[0].0,
        callers[0].1
    );
    // Nothing may hand out an id on demand again.
    for banned in ["fn id_for", "id_for("] {
        assert!(
            !replay.contains(banned),
            "`{banned}` is back: issuing an id on demand is what desynced a \
             replay of its own log"
        );
    }
}

/// End to end, and per sample as the log grows: a match whose world moves out
/// from under a replay — units named by the log dying before their commands —
/// still produces a log that saves, and the re-recorded log is the one it was
/// given (the fixed point survives dead entities).
#[test]
fn a_replay_into_a_world_that_lost_units_still_records_a_writable_log() {
    const TICKS: u32 = 900;
    let (_, log, _) = recorded_run(4, TICKS);
    let named: Vec<(u32, SimId)> = log
        .commands
        .iter()
        .filter(|c| c.tick > 100)
        .filter_map(|c| c.order.sim_ids().first().copied().map(|id| (c.tick, id)))
        .take(3)
        .collect();
    assert!(!named.is_empty(), "the recording named no entity after tick 100");

    let mut app = ai_vs_ai(log.seed);
    app.insert_resource(CommandLog::new(log.seed));
    app.insert_resource(ReplaySource::new(log.clone()));
    for t in 0..TICKS {
        for (at, id) in &named {
            if t + 1 == *at {
                if let Some(e) = app.world().resource::<SimIds>().entity(*id) {
                    app.world_mut().despawn(e);
                }
            }
        }
        step(&mut app);
        let produced = app.world().resource::<CommandLog>().log().clone();
        produced
            .validate()
            .unwrap_or_else(|e| panic!("tick {t}: the sim recorded an invalid log: {e}"));
    }
    let produced = app.world().resource::<CommandLog>().log().clone();
    let path = scratch("replay-lost-units");
    produced.save(&path).expect("the log of a damaged replay must still save");
    assert_eq!(
        produced.commands, log.commands,
        "killing a named unit changed the stream the replay recorded"
    );
}

// ---- what the hash must not invent ------------------------------------------

/// A command pushed after the match is decided can never be applied — the chain
/// is gated off — so it has no causal reach at all, and the hash of a frozen sim
/// must not move for it. Hashing it would turn a click into a desync report.
#[test]
fn an_order_pushed_after_the_match_is_over_leaves_the_frozen_hash_alone() {
    let mut app = sim_app();
    spawn_building(&mut app, "hq", Faction::A, Vec2::new(-100.0, 0.0));
    let doomed = spawn_building(&mut app, "hq", Faction::B, Vec2::new(100.0, 0.0));
    let unit = spawn_unit(&mut app, "ripper", Faction::A, Vec2::new(-90.0, 0.0));
    tick(&mut app, 2);
    app.world_mut().despawn(doomed);
    tick(&mut app, 3);
    assert!(
        app.world().resource::<MatchState>().is_over(),
        "the fixture never decided"
    );
    let frozen = onus::sim::state_hash(app.world_mut());
    for t in 0..20u32 {
        app.world_mut().resource_mut::<CommandQueue>().0.push_back(
            Order::MoveTo {
                units: vec![unit],
                dest: Vec2::new(t as f32, 0.0),
            }
            .issued_by(Faction::A),
        );
        step(&mut app);
        assert_eq!(
            onus::sim::state_hash(app.world_mut()),
            frozen,
            "click {t} on a finished match moved the hash of a frozen sim"
        );
    }
    assert!(
        app.world().resource::<CommandQueue>().0.len() >= 20,
        "the queue did not actually hold the orders, so this proves nothing"
    );
}

/// The direction that narrowing could break: a command genuinely **held for a
/// later tick** is still hashed, in a running sim and in a finished one.
#[test]
fn a_command_held_for_a_later_tick_is_hashed_either_way() {
    for finished in [false, true] {
        let mut a = sim_app();
        let ua = spawn_unit(&mut a, "ripper", Faction::A, Vec2::ZERO);
        let mut b = sim_app();
        let _ = spawn_unit(&mut b, "ripper", Faction::A, Vec2::ZERO);
        if finished {
            for app in [&mut a, &mut b] {
                spawn_building(app, "hq", Faction::A, Vec2::new(-100.0, 0.0));
                let d = spawn_building(app, "hq", Faction::B, Vec2::new(100.0, 0.0));
                tick(app, 2);
                app.world_mut().despawn(d);
                tick(app, 3);
                assert!(app.world().resource::<MatchState>().is_over());
            }
        } else {
            tick(&mut a, 3);
            tick(&mut b, 3);
        }
        assert_eq!(
            onus::sim::state_hash(a.world_mut()),
            onus::sim::state_hash(b.world_mut()),
            "the fixtures differed to begin with (finished: {finished})"
        );
        push_at(
            &mut a,
            5_000,
            Order::MoveTo {
                units: vec![ua],
                dest: Vec2::new(9.0, 9.0),
            }
            .issued_by(Faction::A),
        );
        assert_ne!(
            onus::sim::state_hash(a.world_mut()),
            onus::sim::state_hash(b.world_mut()),
            "a command held for a later tick is invisible (finished: {finished})"
        );
    }
}

/// The driver half of the same problem: the shipped app kept emitting orders
/// after the match was decided, so the queue grew without bound for the rest of
/// the session. Input stops when the match does.
#[test]
fn the_shipped_app_stops_emitting_orders_once_the_match_is_decided() {
    let lib = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs"),
    )
    .expect("read src/lib.rs");
    for emitter in ["input::emit_commands", "input::emit_build_commands"] {
        let at = lib
            .find(emitter)
            .unwrap_or_else(|| panic!("{emitter} is not registered"));
        let tail = &lib[at..at + 200.min(lib.len() - at)];
        let end = tail.find(",\n").unwrap_or(tail.len());
        assert!(
            tail[..end].contains("run_if(sim::victory::match_running)"),
            "{emitter} keeps emitting orders after the match is over: {}",
            tail[..end].trim()
        );
    }
}

// ---- the content a log was recorded against ---------------------------------

/// Load `assets/data` with one textual substitution applied to `units.ron`,
/// into a scratch directory — the way the M4 suites edit content.
fn content_edited(name: &str, from: &str, to: &str) -> Content {
    let dir = std::env::temp_dir().join(format!("onus-p1-{name}"));
    std::fs::create_dir_all(&dir).expect("scratch content dir");
    for file in ["units.ron", "resources.ron"] {
        let text = std::fs::read_to_string(data_dir().join(file)).expect("read content");
        let text = if file == "units.ron" {
            assert!(text.contains(from), "`{from}` is not in units.ron");
            text.replacen(from, to, 1)
        } else {
            text
        };
        std::fs::write(dir.join(file), text).expect("write content");
    }
    Content::load_from_dir(&dir).expect("the edited content still loads")
}

/// The same content set with two unit definitions swapped: identical ids,
/// identical stats, different *positions* — which is exactly what a RON edit
/// does to the indices a log used to be written in terms of.
fn reordered_roster() -> Content {
    let mut c = content();
    assert!(c.units.len() > 2);
    c.units.swap(1, 2);
    c
}

/// **A log carries the content it was recorded against, and a replay refuses
/// one that does not match.** Without this, editing the roster turns every
/// stored log into a silently different match.
#[test]
fn a_log_recorded_against_other_content_is_refused() {
    let mut app = ai_vs_ai(4);
    tick(&mut app, 600);
    let log = app.world().resource::<CommandLog>().log().clone();
    assert!(
        log.content.is_known(),
        "the sim did not stamp the content it played with"
    );
    assert_eq!(log.content, content().fingerprint());

    // The same content: it loads, through the checked front door.
    let path = scratch("fingerprint");
    log.save(&path).expect("save");
    MatchLog::load_for(&path, &content()).expect("its own content must load");

    // A different roster: refused, by name, with both sides in the message.
    let edited = content_edited("costlier", "mvp_alloy_cost: 10", "mvp_alloy_cost: 11");
    let err = MatchLog::load_for(&path, &edited)
        .expect_err("a log recorded against other content must be refused");
    assert!(
        err.contains("different content") && err.contains("units"),
        "the refusal does not say what changed: {err}"
    );
    // ...and it is still *readable*, so a false rejection is recoverable.
    let inspected = MatchLog::load(&path).expect("a refused log is still readable");
    assert_eq!(inspected, log);
}

/// The scope decision, made executable: the fingerprint covers the **whole**
/// content, including fields the MVP sim never reads, and is exact about
/// floats and about order.
///
/// The trade is deliberate. A false rejection is loud, immediate and
/// recoverable; a false accept is the silent wrong replay the fingerprint
/// exists to eliminate. So the fingerprint errs towards rejecting.
#[test]
fn the_fingerprint_covers_the_whole_content_exactly() {
    let base = content().fingerprint();
    assert_eq!(base, content().fingerprint(), "the fingerprint is not stable");
    assert!(base.is_known());

    let moved = |name: &str, from: &str, to: &str| content_edited(name, from, to).fingerprint();
    // A field the MVP sim reads.
    assert_ne!(base, moved("cost", "mvp_alloy_cost: 10", "mvp_alloy_cost: 11"));
    // A field it does **not** read: the post-MVP per-domain cost.
    assert_ne!(
        base,
        moved(
            "postmvp",
            "cost: (resource: \"alloy\", amount: 10)",
            "cost: (resource: \"alloy\", amount: 11)"
        ),
        "an edit to a field the MVP never reads left the fingerprint unchanged \
         — the scope decision is not implemented"
    );
    // A name, which nothing at all reads.
    assert_ne!(base, moved("name", "name: \"Worker\"", "name: \"Labourer\""));
    // A float, changed by one representable step.
    assert_ne!(base, moved("float", "speed_per_point: 36.0", "speed_per_point: 36.000004"));
    // Order: same set of definitions, different RON order.
    let reordered = reordered_roster();
    assert_ne!(
        base,
        reordered.fingerprint(),
        "reordering the roster left the fingerprint unchanged"
    );
    // And the summary is diagnostic only — it changes with the roster, and
    // nothing is decided from it.
    assert!(base.summary().contains("units"));
}

/// The property the fingerprint could break: the sim always stamps, so
/// **every** log it records can be checked against its own content — asserted
/// per sample as the log grows, not once at the end.
#[test]
fn every_log_the_sim_records_matches_the_content_it_played() {
    let mut app = ai_vs_ai(4);
    for t in 0..1_200u32 {
        step(&mut app);
        if t % 200 == 0 || t == 1_199 {
            let log = app.world().resource::<CommandLog>().log().clone();
            log.matches_content(&content())
                .unwrap_or_else(|e| panic!("tick {t}: {e}"));
        }
    }
    // Including a match that is decided on its very first tick: the stamp is
    // ungated, so even a sim that never plays a live tick stamps its log.
    let mut over = sim_app();
    spawn_building(&mut over, "hq", Faction::A, Vec2::new(-100.0, 0.0));
    let b = spawn_building(&mut over, "hq", Faction::B, Vec2::new(100.0, 0.0));
    tick(&mut over, 2);
    over.world_mut().despawn(b);
    tick(&mut over, 2);
    assert!(over.world().resource::<MatchState>().is_over());
    over.world()
        .resource::<CommandLog>()
        .log()
        .matches_content(&content())
        .expect("a match decided early still stamps its log");
}

/// The backstop: even a `ReplaySource` built from a log nobody checked is
/// refused by the sim itself, rather than played as a different match.
#[test]
fn the_sim_refuses_to_replay_a_log_from_other_content() {
    const TICKS: u32 = 400;
    let (_, log, _) = recorded_run(4, TICKS);
    let mut app = ai_vs_ai(log.seed);
    // Same starting world, different content.
    let edited = content_edited("replay-refuse", "mvp_alloy_cost: 10", "mvp_alloy_cost: 11");
    app.insert_resource(edited);
    app.insert_resource(CommandLog::new(log.seed));
    app.insert_resource(ReplaySource::new(log));
    tick(&mut app, TICKS);
    let source = app.world().resource::<ReplaySource>();
    assert!(
        source.rejection().is_some(),
        "the sim replayed a log recorded against different content"
    );
    assert_eq!(source.cursor(), 0, "it fed commands from a rejected log");
    assert_eq!(
        app.world().resource::<CommandLog>().commands().len(),
        0,
        "a refused replay still applied commands"
    );
}

/// The direction the backstop could break: a replay whose content *does* match
/// is not rejected, and still reproduces the match.
#[test]
fn a_matching_log_is_not_refused_by_the_backstop() {
    const TICKS: u32 = 900;
    let (recorded, log, _) = recorded_run(4, TICKS);
    let (replayed, app) = replay_run(log, TICKS);
    assert!(
        app.world().resource::<ReplaySource>().rejection().is_none(),
        "a matching log was refused"
    );
    assert!(
        app.world().resource::<ReplaySource>().cursor() > 0,
        "the matching replay fed nothing at all, so the comparison is vacuous"
    );
    assert_eq!(recorded.first_divergence(&replayed), None);
}

/// **Migration: there is none.** A version-1 log is refused by name, not
/// upgraded — the one thing it is missing is which content it was recorded
/// against, and inventing that is exactly the silent wrong replay the
/// fingerprint exists to prevent.
#[test]
fn a_version_one_log_is_refused_rather_than_upgraded() {
    // Exactly what a version-1 log looked like: no content fingerprint, because
    // the format had none.
    let v1 = r#"(
    version: 1,
    seed: 4,
    commands: [],
)"#;
    let err = MatchLog::from_ron(v1).expect_err("a version-1 log must be refused");
    assert!(
        err.contains("version") || err.contains("content"),
        "the refusal explains neither the version nor the missing content: {err}"
    );
    // ...and it is refused *because it is version 1*, not merely because the
    // field is missing: the same log with the field still added is refused.
    let v1_padded = r#"(
    version: 1,
    seed: 4,
    content: (hash: 7, summary: "whatever"),
    commands: [],
)"#;
    let err = MatchLog::from_ron(v1_padded).expect_err("a version-1 log must be refused");
    assert!(
        err.contains("version 1"),
        "the refusal does not name the version: {err}"
    );
}

// ---- a log names content by id, not by position -----------------------------

/// **The property indices could not have.** A log records `Place`/`Train` by
/// the content's *id*, so it still names the same things after the RON order
/// changes — which "content is data" guarantees will happen.
///
/// (The fingerprint would refuse such a log at the front door; that is the
/// point of the diagnostic path. What this shows is that the log's *meaning*
/// survives the edit, so a deliberate re-validation is a decision someone can
/// make rather than data that is already lost.)
#[test]
fn a_log_still_names_the_same_things_after_the_roster_is_reordered() {
    let mut app = ai_vs_ai(4);
    tick(&mut app, 2_000);
    let log = app.world().resource::<CommandLog>().log().clone();
    let named: Vec<String> = log
        .commands
        .iter()
        .flat_map(|c| c.order.content_ids())
        .map(|(what, id)| format!("{what}:{id}"))
        .collect();
    assert!(
        named.iter().any(|n| n.starts_with("unit:")),
        "the match trained nothing, so this proves nothing"
    );
    assert!(
        named.iter().any(|n| n.starts_with("building:")),
        "the match placed nothing, so this proves nothing"
    );

    // Under the original content and under a reordered roster, every logged
    // order resolves — to *different indices*, naming the *same definitions*.
    let base = content();
    let moved = reordered_roster();
    let ids = app.world().resource::<SimIds>();
    let mut differed = 0;
    for c in &log.commands {
        let a = c.order.to_order(ids, &base).expect("resolves under its own content");
        let b = c
            .order
            .to_order(ids, &moved)
            .expect("resolves under a reordered roster");
        match (&a, &b) {
            (Order::Train { unit: x, .. }, Order::Train { unit: y, .. }) => {
                assert_eq!(base.units[*x].id, moved.units[*y].id, "a Train changed unit");
                differed += usize::from(x != y);
            }
            (Order::Place { building: x, .. }, Order::Place { building: y, .. }) => {
                assert_eq!(
                    base.buildings[*x].id, moved.buildings[*y].id,
                    "a Place changed building"
                );
            }
            _ => {}
        }
    }
    assert!(
        differed > 0,
        "no logged order landed on a different index under the reordered \
         roster, so the reorder did not exercise anything"
    );
}

/// The direction ids could break: under the content it was recorded with, every
/// logged order resolves back to **exactly** the index the sim applied — per
/// command, over a live match, not just at the end.
#[test]
fn every_logged_order_resolves_back_to_the_index_it_was_applied_with() {
    let mut app = sim_app_with_alloy(50_000);
    let hq = spawn_building(&mut app, "hq", Faction::A, Vec2::ZERO);
    let worker = app
        .world()
        .resource::<Content>()
        .unit_index("worker")
        .expect("worker");
    let foundry = app
        .world()
        .resource::<Content>()
        .building_index("foundry")
        .expect("foundry");
    tick(&mut app, 2);
    for t in 0..40u32 {
        if t % 2 == 0 {
            app.world_mut().resource_mut::<CommandQueue>().0.push_back(
                Order::Train {
                    building: hq,
                    unit: worker,
                }
                .issued_by(Faction::A),
            );
        } else {
            app.world_mut().resource_mut::<CommandQueue>().0.push_back(
                Order::Place {
                    faction: Faction::A,
                    building: foundry,
                    pos: Vec2::new(300.0 + t as f32, 0.0),
                }
                .issued_by(Faction::A),
            );
        }
        step(&mut app);
        let c = content();
        let ids = app.world().resource::<SimIds>();
        let log = app.world().resource::<CommandLog>();
        for entry in log.commands() {
            match entry.order.to_order(ids, &c).expect("resolves") {
                Order::Train { unit, .. } => assert_eq!(unit, worker, "tick {t}"),
                Order::Place { building, .. } => assert_eq!(building, foundry, "tick {t}"),
                _ => {}
            }
        }
    }
    assert!(app.world().resource::<CommandLog>().commands().len() >= 40);
}

/// An id this build's content does not have is refused **loudly**, at the
/// content boundary, and never quietly resolved to some index.
#[test]
fn a_log_naming_content_this_build_does_not_have_is_refused() {
    let mut app = ai_vs_ai(4);
    tick(&mut app, 600);
    let mut log = app.world().resource::<CommandLog>().log().clone();
    log.commands.push(LoggedCommand {
        tick: 600,
        schedule: CommandTick::Asap,
        fate: CommandFate::Taken,
        attribution: Attribution::By(Faction::A),
        order: LoggedOrder::Train {
            building: 0,
            unit: "ghost_unit".to_string(),
        },
    });
    let err = log
        .matches_content(&content())
        .expect_err("a log naming a unit this build lacks must be refused");
    assert!(
        err.contains("ghost_unit"),
        "the refusal does not name what is missing: {err}"
    );
    // ...and resolving it is an error, not a guess at an index.
    let ids = app.world().resource::<SimIds>();
    assert!(log.commands.last().unwrap().order.to_order(ids, &content()).is_err());
}

// ---- the log records the schedule, not only the applied tick ----------------

/// A command that missed its tick is **in the log**, with its schedule and the
/// reason it never applied. Before this the log was an account of what worked;
/// a replay of it held a different queue than the recording did.
#[test]
fn a_command_that_missed_its_tick_is_logged_with_its_schedule_and_reason() {
    let mut app = sim_app();
    let unit = spawn_unit(&mut app, "ripper", Faction::A, Vec2::ZERO);
    tick(&mut app, 10);
    let stale = sim_tick(&app) - 4;
    push_at(
        &mut app,
        stale,
        Order::MoveTo {
            units: vec![unit],
            dest: Vec2::new(400.0, 0.0),
        }
        .issued_by(Faction::A),
    );
    step(&mut app);
    // Not applied...
    assert!(app.world().get::<MoveTarget>(unit).is_none());
    // ...and recorded, as late, with the schedule it carried.
    let log = app.world().resource::<CommandLog>();
    assert_eq!(log.commands().len(), 1, "the dropped command is missing from the log");
    let c = &log.commands()[0];
    assert_eq!(c.fate, CommandFate::Late);
    assert_eq!(c.schedule, CommandTick::At(stale));
    assert_eq!(c.tick, stale + 4, "the log does not say when the sim saw it");
    assert_eq!(log.late(), 1);
    assert!(log.log().validate().is_ok());
}

/// A replay re-pushes each command **on its original schedule**, so a command
/// that was dropped late in the recording is dropped late in the replay — and
/// the replay's own log comes out identical. (Re-pushing on the tick it was
/// *seen* would turn a dropped command into an applied one.)
#[test]
fn a_replay_re_pushes_on_the_recorded_schedule_and_re_records_the_same_log() {
    const TICKS: u32 = 60;
    let build = || {
        let mut app = sim_app_with_alloy(1_000);
        spawn_unit(&mut app, "ripper", Faction::A, Vec2::ZERO);
        app.insert_resource(StateHashLog::default());
        app.insert_resource(CommandLog::new(2));
        app
    };
    let mut rec = build();
    let unit = {
        let mut q = rec.world_mut().query_filtered::<Entity, With<onus::sim::UnitKind>>();
        q.iter(rec.world()).next().expect("a unit")
    };
    for t in 0..TICKS {
        if t == 10 {
            // On time.
            push_at(
                &mut rec,
                10,
                Order::MoveTo {
                    units: vec![unit],
                    dest: Vec2::new(50.0, 0.0),
                }
                .issued_by(Faction::A),
            );
        }
        if t == 20 {
            // Too late: aimed at a tick that has gone by.
            push_at(
                &mut rec,
                15,
                Order::MoveTo {
                    units: vec![unit],
                    dest: Vec2::new(-900.0, 0.0),
                }
                .issued_by(Faction::A),
            );
        }
        step(&mut rec);
    }
    let log = rec.world().resource::<CommandLog>().log().clone();
    let fates: Vec<(u32, CommandTick, CommandFate)> = log
        .commands
        .iter()
        .map(|c| (c.tick, c.schedule, c.fate))
        .collect();
    assert_eq!(
        fates,
        vec![
            (10, CommandTick::At(10), CommandFate::Taken),
            (20, CommandTick::At(15), CommandFate::Late),
        ]
    );
    let hashes = rec.world().resource::<StateHashLog>().clone();

    let mut rep = build();
    rep.insert_resource(ReplaySource::new(log.clone()));
    tick(&mut rep, TICKS);
    assert_eq!(
        rep.world().resource::<CommandLog>().log().commands,
        log.commands,
        "the replay recorded a different account than the recording"
    );
    assert_eq!(
        hashes.first_divergence(&rep.world().resource::<StateHashLog>().clone()),
        None,
        "the replay diverged"
    );
    assert_eq!(rep.world().resource::<CommandLog>().late(), 1);
}

/// The property recording the schedule might break: an ordinary `Asap` command
/// — everything input and the AI emit — is logged as `Asap` and replays as
/// `Asap`, and a whole AI match still replays hash for hash.
#[test]
fn an_asap_command_is_logged_and_replayed_as_asap() {
    const TICKS: u32 = 3_000;
    let (recorded, log, _) = recorded_run(4, TICKS);
    assert!(
        log.commands.len() > 20,
        "the match issued almost nothing ({}), so this proves little",
        log.commands.len()
    );
    assert!(
        log.commands
            .iter()
            .all(|c| c.schedule == CommandTick::Asap && c.fate == CommandFate::Taken),
        "a shipped producer emitted something other than an Asap command"
    );
    let (replayed, app) = replay_run(log.clone(), TICKS);
    assert_eq!(recorded.first_divergence(&replayed), None);
    assert_eq!(
        app.world().resource::<CommandLog>().log().commands,
        log.commands
    );
}

// ---- the two boundaries agree ----------------------------------------------

/// Adversarial logs, each either accepted by **both** boundaries or refused by
/// both. The write side used to check only one of the three invalid classes, so
/// the sim could write a log that would then never load again.
#[test]
fn what_the_writer_accepts_the_reader_accepts_and_the_other_way_round() {
    let cmd = |tick: u32, order: LoggedOrder| LoggedCommand {
        tick,
        schedule: CommandTick::Asap,
        fate: CommandFate::Taken,
        attribution: Attribution::By(Faction::A),
        order,
    };
    let cases: Vec<(&str, Vec<LoggedCommand>)> = vec![
        ("empty", vec![]),
        (
            "ordinary",
            vec![
                cmd(0, LoggedOrder::MoveTo { units: vec![0, 1], dest: (1.5, -2.5) }),
                cmd(0, LoggedOrder::Train { building: 2, unit: "ripper".into() }),
                cmd(9, LoggedOrder::Gather { units: vec![3], node: 4, node_pos: (0.0, -0.0) }),
                cmd(9, LoggedOrder::Place { faction: Faction::B, building: "hq".into(), pos: (7.0, 8.0) }),
            ],
        ),
        ("nan", vec![cmd(1, LoggedOrder::MoveTo { units: vec![0], dest: (f32::NAN, 0.0) })]),
        ("inf", vec![cmd(1, LoggedOrder::Place { faction: Faction::A, building: "hq".into(), pos: (0.0, f32::INFINITY) })]),
        (
            "backwards",
            vec![
                cmd(5, LoggedOrder::Train { building: 0, unit: "worker".into() }),
                cmd(4, LoggedOrder::Train { building: 0, unit: "worker".into() }),
            ],
        ),
        (
            "unidentified in a move",
            vec![cmd(1, LoggedOrder::MoveTo { units: vec![0, SimId::UNIDENTIFIED.0], dest: (1.0, 1.0) })],
        ),
        (
            "unidentified node",
            vec![cmd(1, LoggedOrder::Gather { units: vec![0], node: SimId::UNIDENTIFIED.0, node_pos: (1.0, 1.0) })],
        ),
        (
            "unidentified building",
            vec![cmd(1, LoggedOrder::Train { building: SimId::UNIDENTIFIED.0, unit: "worker".into() })],
        ),
    ];
    for (name, commands) in cases {
        let mut log = MatchLog::new(5);
        log.commands = commands;
        let path = scratch(&format!("symmetry-{}", name.replace(' ', "-")));
        let _ = std::fs::remove_file(&path);
        let wrote = log.save(&path);
        match wrote {
            Ok(()) => {
                let back = MatchLog::load(&path).unwrap_or_else(|e| {
                    panic!("`{name}`: save wrote a log that load refuses: {e}")
                });
                assert_eq!(back, log, "`{name}`: the log changed on the way to disk");
            }
            Err(_) => {
                assert!(
                    !path.exists(),
                    "`{name}`: save refused and still wrote a file"
                );
                // A refused log is refused by the reader too — otherwise the
                // rejection is a matter of which door you came in by.
                let text = ron::ser::to_string_pretty(&log, ron::ser::PrettyConfig::default())
                    .expect("serializes as a value");
                assert!(
                    MatchLog::from_ron(&text).is_err(),
                    "`{name}`: the writer refused a log the reader accepts"
                );
            }
        }
    }
}

/// The direction a stricter writer could break: **every** log the sim itself
/// records still saves and loads, over a long live match, checked repeatedly as
/// it grows rather than once at the end.
#[test]
fn every_log_the_sim_records_still_saves_and_loads() {
    let mut app = ai_vs_ai(4);
    for t in 0..2_000u32 {
        step(&mut app);
        if t % 250 == 0 || t == 1_999 {
            let log = app.world().resource::<CommandLog>().log().clone();
            log.validate()
                .unwrap_or_else(|e| panic!("tick {t}: the sim recorded an invalid log: {e}"));
            let path = scratch("sim-written");
            log.save(&path)
                .unwrap_or_else(|e| panic!("tick {t}: the sim's own log will not save: {e}"));
            let back = MatchLog::load(&path)
                .unwrap_or_else(|e| panic!("tick {t}: the sim's own log will not load: {e}"));
            assert_eq!(back, log);
        }
    }
    assert!(app.world().resource::<CommandLog>().commands().len() > 20);
}

// ---- the chain's F-008 boundary, per system ---------------------------------

/// Resolve a chain-registered `sim::…` path to the source of its function body.
///
/// **A name that does not resolve is a failure, not a skip.** A walker that
/// quietly passes over what it cannot find reads as comprehensive while being
/// blind, and the system it skipped is exactly the one that gets moved ahead of
/// the sweep later. The lookup is by *definition*: a system may be re-exported
/// (`pub use victory::match_running`) or live in a nested submodule, so every
/// `.rs` under `src/sim/` is searched and exactly one `pub fn <name>(` must be
/// found, with a body the scan can read to its closing brace.
fn resolve_sim_fn(path: &str) -> Result<String, String> {
    let sim = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sim");
    let name = path.rsplit("::").next().unwrap_or_default();
    if name.is_empty() {
        return Err(format!("{path}: not a function path"));
    }
    let needle = format!("pub fn {name}(");
    let mut files: Vec<PathBuf> = Vec::new();
    let mut stack = vec![sim];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).map_err(|e| format!("{path}: {e}"))? {
            let p = entry.map_err(|e| format!("{path}: {e}"))?.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|e| e == "rs") {
                files.push(p);
            }
        }
    }
    files.sort();
    let mut found: Vec<(String, String)> = Vec::new();
    for file in files {
        let text = std::fs::read_to_string(&file).map_err(|e| format!("{path}: {e}"))?;
        let Some(at) = text.find(&needle) else {
            continue;
        };
        let rest = &text[at..];
        let end = rest[1..].find("\n}\n").map(|i| i + 3).ok_or_else(|| {
            format!(
                "{path}: `{needle}` in {} has no closing `}}` at column 0",
                file.display()
            )
        })?;
        found.push((file.display().to_string(), rest[..end].to_string()));
    }
    match found.len() {
        0 => Err(format!("{path}: no `{needle}` anywhere under src/sim")),
        1 => Ok(found.pop().expect("one").1),
        _ => Err(format!(
            "{path}: `{needle}` is defined {} times ({:?})",
            found.len(),
            found.iter().map(|(f, _)| f).collect::<Vec<_>>()
        )),
    }
}

/// Every `sim::…` function path the chain names, in source order, with its byte
/// offset in `src/lib.rs`. Upper-camel names are resource/type paths.
fn chain_fn_paths() -> Vec<(usize, String)> {
    let lib = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs"),
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

fn sweep_offset() -> usize {
    std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs"))
        .expect("read src/lib.rs")
        .find("sim::economy::repair_gather_claims")
        .expect("the sweep is in the chain")
}

/// **Every name the chain uses resolves.** The claim-freeness check below is
/// only as wide as this: a path it cannot turn into a body is a system it
/// cannot vouch for, and silence there is worse than the hand-written list it
/// replaced.
#[test]
fn every_function_the_chain_names_resolves_to_exactly_one_body() {
    let paths = chain_fn_paths();
    assert!(paths.len() >= 8, "the chain walker found almost nothing: {paths:?}");
    let mut unresolved = Vec::new();
    for (_, path) in &paths {
        match resolve_sim_fn(path) {
            Ok(body) => assert!(
                body.trim_end().ends_with('}'),
                "`{path}`'s body was cut short before its closing brace"
            ),
            Err(e) => unresolved.push(e),
        }
    }
    assert!(unresolved.is_empty(), "the chain names systems the guard cannot read: {unresolved:#?}");
}

/// F-008's rule, checked **per system** rather than per file: nothing the chain
/// runs before `repair_gather_claims` may name either half of the gather claim.
///
/// The file-granular allowlist in the M4c probes cannot express this — admitting
/// a file says nothing about where its systems run — so a claim read added to a
/// pre-sweep system of an already-admitted file would slip past it. This is the
/// check that does not.
#[test]
fn no_system_the_chain_runs_before_the_sweep_touches_the_gather_claim() {
    let sweep = sweep_offset();
    let pre: Vec<String> = chain_fn_paths()
        .into_iter()
        .filter(|(at, _)| *at < sweep)
        .map(|(_, p)| p)
        .collect();
    assert!(
        !pre.is_empty(),
        "nothing is registered before the sweep, so this check inspects nothing"
    );
    let mut offenders = Vec::new();
    for path in &pre {
        // Unresolvable is a failure, not a skip: see
        // `every_function_the_chain_names_resolves_to_exactly_one_body`.
        let body = resolve_sim_fn(path).unwrap_or_else(|e| panic!("{e}"));
        for claim in ["GatherTarget", "GatherPhase", "SplitClaim"] {
            if body.contains(claim) {
                offenders.push(format!("{path} names {claim}"));
            }
        }
    }
    // A run condition is evaluated before the systems it gates, so it is
    // "before the sweep" wherever in the file it is written.
    let lib = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs"),
    )
    .expect("read src/lib.rs");
    for (_, path) in chain_fn_paths() {
        if !lib.contains(&format!("run_if({path})")) {
            continue;
        }
        let body = resolve_sim_fn(&path).unwrap_or_else(|e| panic!("{e}"));
        for claim in ["GatherTarget", "GatherPhase", "SplitClaim"] {
            if body.contains(claim) {
                offenders.push(format!("the run condition {path} names {claim}"));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "code that runs before the gather-claim sweep reads the claim (F-008): {offenders:?}"
    );
}

/// The ordering `identify` actually needs — and the direction moving it after
/// the sweep could have broken: it must still run before every system that
/// addresses an entity by its `SimId` (`apply_commands` logs by it,
/// `feed_replay` resolves by it, `record_state_hash` keys by it).
#[test]
fn identify_runs_after_the_sweep_and_before_everything_that_uses_a_sim_id() {
    let order: Vec<String> = chain_fn_paths().into_iter().map(|(_, p)| p).collect();
    let at = |name: &str| {
        order
            .iter()
            .position(|p| p == name)
            .unwrap_or_else(|| panic!("{name} is not in the chain"))
    };
    let identify = at("sim::replay::identify");
    assert!(
        identify > at("sim::economy::repair_gather_claims"),
        "identify runs before the gather-claim sweep again"
    );
    for user in [
        "sim::apply_commands",
        "sim::replay::feed_replay",
        "sim::replay::record_state_hash",
    ] {
        assert!(
            identify < at(user),
            "{user} addresses entities by SimId but runs before identify"
        );
    }
}

/// The behaviour behind that ordering: an order issued against a unit that was
/// on the field before the first tick is logged with a **real** sim id, not the
/// unidentified sentinel — on tick 0, the earliest it could go wrong.
#[test]
fn an_order_on_the_very_first_tick_is_logged_with_a_real_sim_id() {
    let mut app = sim_app();
    let unit = spawn_unit(&mut app, "ripper", Faction::A, Vec2::ZERO);
    app.world_mut().resource_mut::<CommandQueue>().0.push_back(
        Order::MoveTo {
            units: vec![unit],
            dest: Vec2::new(50.0, 0.0),
        }
        .issued_by(Faction::A),
    );
    step(&mut app);
    let log = app.world().resource::<CommandLog>();
    assert_eq!(log.commands().len(), 1);
    let ids = log.commands()[0].order.sim_ids();
    assert_eq!(ids.len(), 1);
    assert!(
        ids[0].is_identified(),
        "a tick-0 order was logged against an unidentified entity"
    );
    assert_eq!(
        app.world().get::<SimId>(unit).copied(),
        Some(ids[0]),
        "the logged id is not the unit's"
    );
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
