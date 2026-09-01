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
use onus::sim::replay::{LoggedCommand, LoggedOrder, MatchLog, SimId};
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
    let good = {
        let mut l = MatchLog::new(2);
        l.commands.push(LoggedCommand {
            tick: 1,
            attribution: Attribution::By(Faction::A),
            order: LoggedOrder::Train {
                building: 3,
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

    // A command naming an entity the sim never identified: no replay can
    // resolve it, so the log is refused rather than replayed with a hole in it.
    let mut nameless = good.clone();
    nameless.commands[0].order = LoggedOrder::MoveTo {
        units: vec![0, SimId::UNIDENTIFIED.0],
        dest: (1.0, 2.0),
    };
    assert!(
        MatchLog::from_ron(&nameless.to_ron().unwrap()).is_err(),
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
