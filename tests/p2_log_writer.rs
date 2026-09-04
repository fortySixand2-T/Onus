//! L2 integration tests for Phase 2: **the shipped binary writes a replay log.**
//!
//! M5 gave the sim a log and the ability to save and load one; nothing shipped
//! ever wrote one. What is under test here is the driver half — when a log
//! reaches disk, what happens when it cannot, and the invariant most at risk in
//! a task whose whole subject is a filename: the wall clock lives in the
//! driver, and nothing it produces may reach sim state, a hash, or a decision.
//!
//! Everything runs headless (`MinimalPlugins`) through the *shipped*
//! registrations — `onus::add_sim_systems` and `onus::add_replay_writer` — so no
//! system list is hand-rolled here (F-004).

use std::path::{Path, PathBuf};

use bevy::prelude::*;

use onus::replay_io::{ReplayConfig, ReplayWriter, WriteOutcome};
use onus::sim::combat::{Casualties, Health};
use onus::sim::content::Content;
use onus::sim::economy::{Building, ProductionQueue, Stockpiles, UnitDefIdx};
use onus::sim::replay::{CommandLog, MatchLog, ReplaySource, StateHashLog};
use onus::sim::spatial::Faction;
use onus::sim::{
    AiCommanders, CommandQueue, MatchOutcome, MatchState, Position, RateReport, ResourceNode,
};

// ---- harness ----------------------------------------------------------------

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data")
}

fn content() -> Content {
    Content::load_from_dir(&data_dir()).expect("assets/data/*.ron")
}

/// A scratch output directory of this test's own, emptied first and removed at
/// the end: tests write to the OS temp dir, never into the repo or a home.
fn scratch_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("onus-p2-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn cleanup(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
}

fn logs_in(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .expect("read scratch dir")
        .map(|e| e.expect("entry").path())
        .filter(|p| p.extension().is_some_and(|e| e == "ron"))
        .collect();
    v.sort();
    v
}

fn config_writing_to(dir: &Path) -> ReplayConfig {
    ReplayConfig {
        enabled: true,
        dir: dir.display().to_string(),
        ..ReplayConfig::default()
    }
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
        c.building_index(id).unwrap_or_else(|| panic!("building `{id}`"))
    };
    app.world_mut()
        .spawn((
            Position(pos),
            Building { def },
            faction,
            ProductionQueue::default(),
            Health::from_building_def(&content(), def),
        ))
        .id()
}

/// The M4c AI-vs-AI fixture, driven through the shipped registrations. `writer`
/// is the replay configuration, or `None` for an app with no writer at all —
/// which is what every earlier test suite is.
fn ai_vs_ai(seed: u64, writer: Option<ReplayWriter>) -> App {
    let c = content();
    let alloy = c.economy.starting_alloy;
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .insert_resource(Time::<Fixed>::from_hz(60.0))
        .insert_resource(c)
        .init_resource::<CommandQueue>()
        .init_resource::<RateReport>()
        .init_resource::<Casualties>()
        .insert_resource(Stockpiles::starting(alloy));
    onus::add_sim_systems(&mut app, Update);
    if let Some(w) = writer {
        // The shipped registration, so the tests cannot drift from the binary.
        onus::add_replay_writer(&mut app, w.config().clone());
        app.insert_resource(w);
    }
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
            spawn_unit(&mut app, "worker", faction, base + Vec2::new(0.0, 20.0 * i as f32));
        }
    }
    app.insert_resource(AiCommanders::new(seed, &[Faction::A, Faction::B]));
    app.insert_resource(CommandLog::new(seed));
    app
}

/// Run an AI-vs-AI match to its decision (or the budget), returning the tick it
/// was decided on.
fn play_to_decision(app: &mut App) -> MatchOutcome {
    const BUDGET: u32 = 8 * 60 * 60;
    for _ in 0..BUDGET {
        step(app);
        if let Some(o) = app.world().resource::<MatchState>().outcome() {
            // A few more ticks, so "it does not write again" has something to
            // be true about.
            tick(app, 5);
            return o;
        }
    }
    panic!("the match never decided inside the budget");
}

// ---- the write happens, and the file is a real replay -----------------------

/// **The headline.** A shipped-shape app plays a match to its end, writes a log
/// the moment the match is decided, and that file loads through the *front
/// door* and replays to the same verdict, tick for tick.
///
/// Not "a file appeared": the file is read back with `load_for` against the same
/// content and played, and the replay has to reach the same outcome on the same
/// tick with an identical per-tick hash trace.
#[test]
fn a_decided_match_writes_a_log_that_loads_and_replays_to_the_same_verdict() {
    let dir = scratch_dir("roundtrip");
    let mut app = ai_vs_ai(7, Some(ReplayWriter::new(config_writing_to(&dir))));
    app.insert_resource(StateHashLog::default());
    let outcome = play_to_decision(&mut app);

    let written = app
        .world()
        .resource::<ReplayWriter>()
        .written()
        .expect("a decided match wrote no log")
        .to_path_buf();
    assert_eq!(logs_in(&dir), vec![written.clone()], "one log, in the configured dir");
    assert!(
        std::fs::metadata(&written).expect("stat").len() > 0,
        "the log is empty"
    );

    // The front door, against the same content.
    let log = MatchLog::load_for(&written, &content()).expect("the shipped log must load");
    assert_eq!(log.seed, 7);
    assert!(
        log.commands.len() > 20,
        "the shipped log records almost nothing ({})",
        log.commands.len()
    );

    // ...and it replays to the same verdict.
    let recorded = app.world().resource::<StateHashLog>().clone();
    let mut replay = ai_vs_ai(log.seed, None);
    replay.world_mut().remove_resource::<AiCommanders>();
    replay.insert_resource(AiCommanders::default());
    replay.insert_resource(StateHashLog::default());
    replay.insert_resource(CommandLog::new(log.seed));
    replay.insert_resource(ReplaySource::new(log));
    tick(&mut replay, outcome.tick + 6);
    let source = replay.world().resource::<ReplaySource>();
    assert!(source.rejection().is_none(), "the replay refused the shipped log");
    assert!(source.cursor() > 20, "the replay fed nothing, so this proves nothing");
    assert_eq!(
        replay.world().resource::<MatchState>().outcome(),
        Some(outcome),
        "the replay of the shipped log reached a different verdict"
    );
    assert_eq!(
        recorded.first_divergence(&replay.world().resource::<StateHashLog>().clone()),
        None,
        "the replay of the shipped log diverged"
    );
    cleanup(&dir);
}

/// **Exactly once.** A decided match keeps ticking, and then the app exits; one
/// file exists at the end, with one write behind it — while the writer was
/// *asked* to write on every one of those ticks.
#[test]
fn a_decided_match_writes_exactly_one_log_however_long_it_runs() {
    let dir = scratch_dir("once");
    let mut app = ai_vs_ai(7, Some(ReplayWriter::new(config_writing_to(&dir))));
    play_to_decision(&mut app);
    let first = app
        .world()
        .resource::<ReplayWriter>()
        .written()
        .expect("no log written")
        .to_path_buf();
    let bytes = std::fs::read(&first).expect("read");

    // 300 more ticks of a decided match...
    tick(&mut app, 300);
    // ...and then an exit.
    app.world_mut().write_message(AppExit::Success);
    app.update();

    let writer = app.world().resource::<ReplayWriter>();
    assert!(
        writer.asks() > 300,
        "the writer was only asked {} times, so the latch is not what kept it to one",
        writer.asks()
    );
    assert_eq!(writer.written(), Some(first.as_path()));
    assert_eq!(logs_in(&dir), vec![first.clone()], "a second log was written");
    assert_eq!(
        std::fs::read(&first).expect("read"),
        bytes,
        "the log was rewritten in place"
    );
    cleanup(&dir);
}

/// A session that ends **before** a decision still leaves its log: the exit path
/// is not merely a duplicate of the decision path.
#[test]
fn an_exit_before_any_decision_still_writes_what_was_played() {
    let dir = scratch_dir("exit");
    let mut app = ai_vs_ai(7, Some(ReplayWriter::new(config_writing_to(&dir))));
    tick(&mut app, 600);
    assert!(
        app.world().resource::<MatchState>().outcome().is_none(),
        "the fixture decided early, so this tests the wrong path"
    );
    assert!(app.world().resource::<ReplayWriter>().written().is_none());

    app.world_mut().write_message(AppExit::Success);
    app.update();

    let written = app
        .world()
        .resource::<ReplayWriter>()
        .written()
        .expect("the exit wrote no log")
        .to_path_buf();
    let log = MatchLog::load_for(&written, &content()).expect("the partial log must load");
    assert!(log.commands.len() > 10, "the partial log is empty of commands");
    assert_eq!(logs_in(&dir).len(), 1);
    cleanup(&dir);
}

// ---- off by default, and off means nothing on disk --------------------------

/// The default is **off**, and the shipped `replay.ron` says so — a log per run
/// is unbounded growth on disk.
#[test]
fn replay_logging_is_off_by_default_and_writes_nothing() {
    assert!(!ReplayConfig::default().enabled);
    let shipped = ReplayConfig::load_from_dir(&data_dir()).expect("the shipped replay.ron parses");
    assert!(!shipped.enabled, "the shipped config turns logging on");

    let dir = scratch_dir("off");
    let mut app = ai_vs_ai(
        7,
        Some(ReplayWriter::new(ReplayConfig {
            enabled: false,
            dir: dir.display().to_string(),
            ..ReplayConfig::default()
        })),
    );
    play_to_decision(&mut app);
    app.world_mut().write_message(AppExit::Success);
    app.update();
    // The machinery ran — it was asked, repeatedly, and declined.
    let writer = app.world().resource::<ReplayWriter>();
    assert!(writer.asks() > 5, "the writer was never even asked");
    assert_eq!(writer.outcome(), None, "a disabled writer acted");
    assert!(logs_in(&dir).is_empty(), "a disabled writer wrote a file");
    cleanup(&dir);
}

/// A missing `replay.ron` is a legitimate state — logging off — while a
/// malformed one is an error, so a typo cannot masquerade as "off".
#[test]
fn a_missing_config_is_off_and_a_malformed_one_is_an_error() {
    let dir = scratch_dir("config");
    assert_eq!(
        ReplayConfig::load_from_dir(&dir).expect("a missing config is not an error"),
        ReplayConfig::default()
    );
    std::fs::write(dir.join(ReplayConfig::FILE), "(enabled: yes-please)").expect("write");
    let err = ReplayConfig::load_from_dir(&dir).expect_err("a malformed config must be an error");
    assert!(err.contains("replay.ron"), "the error does not name the file: {err}");
    cleanup(&dir);
}

// ---- the failure path is loud, and never fatal ------------------------------

/// A write that cannot happen is **reported and survivable**: the app keeps
/// running, the sim keeps its state, and the reason is on the writer where a
/// caller (and the console) can see it.
#[test]
fn a_write_that_fails_is_reported_and_the_game_keeps_running() {
    let dir = scratch_dir("failure");
    // The output directory is a *file*, so creating it must fail.
    let blocked = dir.join("not-a-dir");
    std::fs::write(&blocked, "occupied").expect("write");
    let mut app = ai_vs_ai(
        7,
        Some(ReplayWriter::new(ReplayConfig {
            enabled: true,
            dir: blocked.display().to_string(),
            ..ReplayConfig::default()
        })),
    );
    let outcome = play_to_decision(&mut app);

    let writer = app.world().resource::<ReplayWriter>();
    let err = writer.error().expect("the failure was swallowed").to_string();
    assert!(err.contains("not written"), "unhelpful failure message: {err}");
    assert!(writer.written().is_none());

    // The game is still there, and still knows who won.
    tick(&mut app, 60);
    assert_eq!(app.world().resource::<MatchState>().outcome(), Some(outcome));
    // ...and it does not retry once a frame forever.
    assert_eq!(
        app.world().resource::<ReplayWriter>().error().map(str::to_string),
        Some(err),
        "the failure changed after being recorded"
    );
    cleanup(&dir);
}

/// A log the sim could not write back is refused **at the writer**, not written
/// as a file nothing can load. (The front door is `to_ron`; this is the driver
/// honouring it.)
#[test]
fn a_log_that_cannot_be_read_back_is_never_written() {
    let dir = scratch_dir("poisoned");
    let mut app = ai_vs_ai(7, Some(ReplayWriter::new(config_writing_to(&dir))));
    tick(&mut app, 300);
    // Poison it the way the sim itself would: the content changes mid-match.
    let edited = {
        let scratch = scratch_dir("poisoned-content");
        for file in ["units.ron", "resources.ron"] {
            let text = std::fs::read_to_string(data_dir().join(file)).expect("read");
            let text = if file == "units.ron" {
                text.replacen("mvp_alloy_cost: 10", "mvp_alloy_cost: 11", 1)
            } else {
                text
            };
            std::fs::write(scratch.join(file), text).expect("write");
        }
        Content::load_from_dir(&scratch).expect("edited content")
    };
    app.insert_resource(edited);
    tick(&mut app, 5);
    assert!(
        app.world().resource::<CommandLog>().log().content_changed,
        "the fixture did not poison the log"
    );

    app.world_mut().write_message(AppExit::Success);
    app.update();
    let writer = app.world().resource::<ReplayWriter>();
    assert!(
        writer.error().is_some_and(|e| e.contains("content changed")),
        "a log the sim refuses to write back was written anyway: {:?}",
        writer.outcome()
    );
    assert!(logs_in(&dir).is_empty(), "an unloadable file was left on disk");
    cleanup(&dir);
}

// ---- filenames are a coordinate, and this one is not injective --------------

/// Two matches finishing **in the same second with the same seed** produce two
/// files, not one overwritten one. The name is not unique; the *claim* is —
/// `create_new` fails on an existing file and the writer walks the suffix.
#[test]
fn two_matches_in_one_second_with_one_seed_do_not_overwrite_each_other() {
    let dir = scratch_dir("collision");
    let mut written = Vec::new();
    for _ in 0..3 {
        let mut app = ai_vs_ai(
            7,
            // The same fixed second every time, and the same seed: the filename
            // is identical by construction.
            Some(ReplayWriter::with_fixed_clock(config_writing_to(&dir), 1_700_000_000)),
        );
        play_to_decision(&mut app);
        written.push(
            app.world()
                .resource::<ReplayWriter>()
                .written()
                .expect("no log written")
                .to_path_buf(),
        );
    }
    let mut unique = written.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), 3, "three matches wrote {unique:?}");
    assert_eq!(logs_in(&dir).len(), 3, "a log was overwritten");
    for path in &written {
        MatchLog::load_for(path, &content()).expect("every log must load");
    }
    // The names are the same stem plus a claim suffix, in the order they were
    // taken — no timestamp guessing, no randomness.
    assert!(written[0].to_string_lossy().contains("-1700000000-seed7.ron"));
    assert!(written[1].to_string_lossy().ends_with("-seed7-1.ron"));
    assert!(written[2].to_string_lossy().ends_with("-seed7-2.ron"));
    cleanup(&dir);
}

/// Rather than overwrite, the writer **gives up and says so** when every name it
/// may use is taken.
#[test]
fn a_writer_with_no_free_name_refuses_rather_than_overwriting() {
    let dir = scratch_dir("exhausted");
    let mut app = ai_vs_ai(
        7,
        Some(ReplayWriter::with_fixed_clock(
            ReplayConfig {
                enabled: true,
                dir: dir.display().to_string(),
                max_collisions: 1,
                ..ReplayConfig::default()
            },
            1_700_000_000,
        )),
    );
    // Both names it may use are already somebody else's log.
    for name in ["onus-1700000000-seed7.ron", "onus-1700000000-seed7-1.ron"] {
        std::fs::write(dir.join(name), "not mine").expect("write");
    }
    play_to_decision(&mut app);
    let writer = app.world().resource::<ReplayWriter>();
    assert!(
        writer.error().is_some_and(|e| e.contains("refusing to overwrite")),
        "the writer did not report giving up: {:?}",
        writer.outcome()
    );
    for name in ["onus-1700000000-seed7.ron", "onus-1700000000-seed7-1.ron"] {
        assert_eq!(
            std::fs::read_to_string(dir.join(name)).expect("read"),
            "not mine",
            "the writer overwrote an existing log"
        );
    }
    cleanup(&dir);
}

// ---- the clock is the driver's, and the sim must not feel it ----------------

/// **The invariant most at risk in this task.** A run that writes a log must end
/// in the byte-identical world as one that does not, tick for tick — the writer
/// reads sim state and a wall clock, and neither the writing nor the clock may
/// reach the simulation.
#[test]
fn writing_a_log_does_not_perturb_the_match() {
    let dir = scratch_dir("neutrality");
    let trace = |writer: Option<ReplayWriter>| {
        let mut app = ai_vs_ai(7, writer);
        app.insert_resource(StateHashLog::default());
        let outcome = play_to_decision(&mut app);
        let hashes = app.world().resource::<StateHashLog>().clone();
        let commands = app.world().resource::<CommandLog>().log().commands.clone();
        let final_hash = onus::sim::state_hash(app.world_mut());
        (outcome, hashes, commands, final_hash, app)
    };
    // Three runs: no writer at all, a writer on the wall clock, and a writer on
    // a fixed clock. If the clock could reach the sim, the second and third
    // would differ.
    let (o1, h1, c1, f1, _) = trace(None);
    let (o2, h2, c2, f2, wrote) = trace(Some(ReplayWriter::new(config_writing_to(&dir))));
    let (o3, h3, c3, f3, _) = trace(Some(ReplayWriter::with_fixed_clock(
        config_writing_to(&dir),
        42,
    )));

    assert!(
        wrote.world().resource::<ReplayWriter>().written().is_some(),
        "the writing run did not write, so this proves nothing"
    );
    assert_eq!(h1.first_divergence(&h2), None, "writing a log changed the match");
    assert_eq!(h1.first_divergence(&h3), None, "the writer's clock reached the match");
    assert_eq!((o1, f1), (o2, f2));
    assert_eq!((o1, f1), (o3, f3));
    assert_eq!(c1, c2, "writing a log changed the command stream");
    assert_eq!(c1, c3);
    cleanup(&dir);
}

/// ...and structurally: the sim knows nothing about any of this. No replay-io
/// type, no path, no clock is named anywhere under `src/sim/`.
#[test]
fn the_sim_never_names_the_writer_a_path_or_a_clock() {
    let sim = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sim");
    let mut files = Vec::new();
    let mut stack = vec![sim.clone()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).expect("read src/sim") {
            let p = e.expect("entry").path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                files.push(p);
            }
        }
    }
    files.sort();
    assert!(!files.is_empty());
    let mut offenders = Vec::new();
    for f in &files {
        let text = std::fs::read_to_string(f).expect("read");
        for (i, line) in text.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            for needle in [
                "ReplayWriter",
                "ReplayConfig",
                "replay_io",
                "SystemTime",
                "Instant",
                "Time<Real>",
            ] {
                if code.contains(needle) {
                    offenders.push(format!("{}:{}: {}", f.display(), i + 1, code.trim()));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "the sim names the driver's writer, its paths or its clock: {offenders:#?}"
    );
}

/// The one definition of when a log is written is `add_replay_writer`, and the
/// shipped binary installs it — a writer that exists only in a test harness is
/// a writer the game does not have (F-004).
#[test]
fn the_shipped_app_installs_the_writer_and_the_tests_use_the_same_definition() {
    let lib = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs"),
    )
    .expect("src/lib.rs");
    let build_at = lib.find("pub fn build_app()").expect("build_app");
    let build_end = build_at + lib[build_at..].find("\n}\n").expect("build_app ends");
    assert!(
        lib[build_at..build_end].contains("add_replay_writer(&mut app"),
        "the shipped app does not install the replay writer"
    );
    // ...and it registers both paths to disk.
    let reg_at = lib.find("pub fn add_replay_writer").expect("add_replay_writer");
    let reg_end = reg_at + lib[reg_at..].find("\n}\n").expect("it ends");
    let reg = &lib[reg_at..reg_end];
    assert!(reg.contains("write_on_decision") && reg.contains("write_on_exit"));
}

/// The outcome type says which of the three things happened, and a caller can
/// tell them apart — including "there was nothing to write", which is not a
/// failure.
///
/// The shipped shape: the sim is in `FixedUpdate`, so a session that exits
/// before the clock has ever handed the sim a tick has a log nothing stamped.
/// Writing that file would leave one `load_for` refuses.
#[test]
fn an_app_that_never_played_writes_nothing_and_says_so() {
    let dir = scratch_dir("nothing");
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .insert_resource(Time::<Fixed>::from_hz(60.0))
        .insert_resource(content())
        .init_resource::<CommandQueue>()
        .init_resource::<RateReport>()
        .init_resource::<Casualties>()
        .insert_resource(Stockpiles::starting(0));
    onus::add_sim_systems(&mut app, FixedUpdate);
    onus::add_replay_writer(&mut app, config_writing_to(&dir));
    // Stop the clock the fixed-update accumulator is fed from: frames happen,
    // the sim never ticks.
    app.world_mut().resource_mut::<Time<Virtual>>().pause();
    app.update();
    assert_eq!(
        app.world().resource::<MatchState>().tick(),
        0,
        "the sim ticked, so this tests the wrong path"
    );
    app.world_mut().write_message(AppExit::Success);
    app.update();
    let writer = app.world().resource::<ReplayWriter>();
    assert!(writer.asks() >= 1, "the writer was never asked");
    assert_eq!(writer.outcome(), Some(&WriteOutcome::NothingToWrite));
    assert!(logs_in(&dir).is_empty());
    cleanup(&dir);
}
