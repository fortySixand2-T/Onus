//! Critic probes for Phase 2 (the shipped binary writes a replay log).
//! **Critic-owned**: the implementer may not edit this file.
//!
//! Judged against the invariants, not the implementer's framing. The one most
//! at risk here is F-003: the writer reads a wall clock, and the sim may not.

use std::path::{Path, PathBuf};

use bevy::prelude::*;

use onus::replay_io::{ReplayConfig, ReplayWriter, WriteOutcome};
use onus::sim::ai::AiCommanders;
use onus::sim::combat::{Casualties, Health};
use onus::sim::content::Content;
use onus::sim::economy::{Building, ProductionQueue, Stockpiles, UnitDefIdx};
use onus::sim::replay::{MatchLog, SimIds};
use onus::sim::spatial::Faction;
use onus::sim::{
    CommandLog, CommandQueue, MatchOutcome, MatchState, Position, RateReport, ResourceNode,
    StateHashLog,
};

// ---- harness (mine; the critic depends on no fixture the implementer owns) --

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data")
}

fn content() -> Content {
    Content::load_from_dir(&data_dir()).expect("assets/data/*.ron")
}

fn src_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// A scratch directory that removes itself on drop — including when a probe
/// panics, so a red run leaves nothing behind.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir()
            .join(format!("onus-critic-p2-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Scratch(dir)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn config_in(dir: &Path) -> ReplayConfig {
    ReplayConfig {
        enabled: true,
        dir: dir.display().to_string(),
        prefix: "critic".to_string(),
        max_collisions: 8,
    }
}

fn logs_in(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| rd.map(|e| e.expect("entry").path()).collect())
        .unwrap_or_default();
    v.retain(|p| p.extension().is_some_and(|e| e == "ron"));
    v.sort();
    v
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

fn spawn_unit(app: &mut App, id: &str, faction: Faction, pos: Vec2) {
    let (idx, kind, hp) = {
        let c = app.world().resource::<Content>();
        let idx = c.unit_index(id).unwrap_or_else(|| panic!("unit `{id}`"));
        (idx, c.units[idx].mvp_kind, Health::from_def(c, idx))
    };
    app.world_mut()
        .spawn((Position(pos), UnitDefIdx(idx), kind, faction, hp));
}

fn spawn_building(app: &mut App, id: &str, faction: Faction, pos: Vec2) -> Entity {
    let (def, hp) = {
        let c = app.world().resource::<Content>();
        let def = c.building_index(id).unwrap_or_else(|| panic!("building `{id}`"));
        (def, Health::from_building_def(c, def))
    };
    app.world_mut()
        .spawn((Position(pos), Building { def }, faction, ProductionQueue::default(), hp))
        .id()
}

/// The AI-vs-AI fixture, driven through the **shipped** registrations.
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
        onus::add_replay_writer(&mut app, w.config().clone());
        app.insert_resource(w);
    }
    for (faction, base) in [
        (Faction::A, Vec2::new(-750.0, 0.0)),
        (Faction::B, Vec2::new(750.0, 0.0)),
    ] {
        spawn_building(&mut app, "hq", faction, base);
        app.world_mut()
            .spawn((Position(base + Vec2::new(0.0, 250.0)), ResourceNode { amount: 100_000 }));
        for i in 0..3 {
            spawn_unit(&mut app, "worker", faction, base + Vec2::new(0.0, 20.0 * i as f32));
        }
    }
    app.insert_resource(AiCommanders::new(seed, &[Faction::A, Faction::B]));
    app.insert_resource(CommandLog::new(seed));
    app
}

/// Everything the sim decided, for comparison across app configurations.
struct Trace {
    hashes: StateHashLog,
    final_hash: u64,
    issued: Vec<u64>,
    log: MatchLog,
    outcome: Option<MatchOutcome>,
}

fn play(seed: u64, ticks: u32, writer: Option<ReplayWriter>) -> (Trace, App) {
    let mut app = ai_vs_ai(seed, writer);
    app.insert_resource(StateHashLog::default());
    let mut issued = Vec::with_capacity(ticks as usize);
    for _ in 0..ticks {
        step(&mut app);
        issued.push(app.world().resource::<SimIds>().issued());
    }
    let hashes = app.world().resource::<StateHashLog>().clone();
    let log = app.world().resource::<CommandLog>().log().clone();
    let outcome = app.world().resource::<MatchState>().outcome();
    let final_hash = onus::sim::state_hash(app.world_mut());
    (Trace { hashes, final_hash, issued, log, outcome }, app)
}

fn assert_same(what: &str, a: &Trace, b: &Trace) {
    assert_eq!(a.hashes.first_divergence(&b.hashes), None, "{what}: a per-tick hash moved");
    assert_eq!(a.hashes.0.len(), b.hashes.0.len(), "{what}: different number of ticks");
    assert_eq!(a.final_hash, b.final_hash, "{what}: the final world differs");
    assert_eq!(a.issued, b.issued, "{what}: the SimId registry drifted");
    assert_eq!(a.log.commands, b.log.commands, "{what}: the command stream differs");
    assert_eq!(a.log.content, b.log.content, "{what}: the content stamp differs");
    assert_eq!(a.outcome, b.outcome, "{what}: a different verdict");
}

// ---- 1. the wall clock must not reach the sim ------------------------------

/// **The invariant this task puts at risk.** The writer reads a clock, owns a
/// path, and is a `Resource` in the same `World` — which in Bevy 0.19 is an
/// entity, and inserting one shifts every entity id the world hands out (F-011,
/// the defect that produced `SimId`). None of that may reach the sim.
///
/// Five app configurations that differ in the writer, the clock, the path and
/// whether a file is actually produced, over a match long enough to spawn,
/// train, fight and kill: every tick's hash, the registry, the command stream,
/// the content stamp and the verdict must be identical in all five.
#[test]
fn no_configuration_of_the_writer_changes_a_single_tick_of_the_sim() {
    // Past the decision, so the enabled writers actually reach the filesystem
    // during the run being compared — a writer that never acted would prove
    // nothing about a writer that did.
    const TICKS: u32 = 4_800;
    let a = Scratch::new("determinism-a");
    let b = Scratch::new("determinism-b");

    let (none, _) = play(7, TICKS, None);
    let (disabled, _) = play(7, TICKS, Some(ReplayWriter::new(ReplayConfig::default())));
    let (wall, wall_app) = play(7, TICKS, Some(ReplayWriter::new(config_in(a.path()))));
    let (fixed, fixed_app) = play(
        7,
        TICKS,
        Some(ReplayWriter::with_fixed_clock(config_in(a.path()), 1_700_000_000)),
    );
    // A different path, a different prefix, a different collision budget — all
    // of which change the *filename* the clock helps compute.
    let (elsewhere, other_app) = play(
        7,
        TICKS,
        Some(ReplayWriter::with_fixed_clock(
            ReplayConfig {
                enabled: true,
                dir: b.path().display().to_string(),
                prefix: "a-completely-different-prefix".to_string(),
                max_collisions: 1,
            },
            1,
        )),
    );

    assert_same("a writer at all", &none, &disabled);
    assert_same("a wall-clock writer", &none, &wall);
    assert_same("a fixed-clock writer", &none, &fixed);
    assert_same("a writer with a different path and clock", &none, &elsewhere);
    assert_same("wall vs fixed clock", &wall, &fixed);

    // ...and none of it is vacuous: the match was decided and played out, and
    // all three enabled writers really did write, to three different names.
    assert!(none.outcome.is_some(), "the fixture never decided, so nothing was written");
    assert!(none.log.commands.len() > 20, "the fixture played almost nothing");
    assert!(none.issued.last().copied().unwrap_or(0) > 8, "nothing was ever spawned");
    let mut names = Vec::new();
    for (what, app) in [("wall", &wall_app), ("fixed", &fixed_app), ("elsewhere", &other_app)] {
        let w = app.world().resource::<ReplayWriter>();
        assert!(w.asks() > 100, "{what}: the writer was barely asked");
        let p = w
            .written()
            .unwrap_or_else(|| panic!("{what}: the writer never wrote, error {:?}", w.error()));
        assert!(p.exists(), "{what}: the reported file is not there");
        names.push(p.file_name().unwrap().to_string_lossy().to_string());
    }
    assert_eq!(
        names.iter().collect::<std::collections::BTreeSet<_>>().len(),
        3,
        "the three writers produced the same filename, so the clock and the path \
         were not actually varied: {names:?}"
    );
}

/// The sim may not so much as *name* a clock, a path, or the writer. Comments
/// stripped, because `ai.rs` has one that says there is no clock.
#[test]
fn nothing_under_src_sim_names_a_clock_a_path_or_the_writer() {
    let mut offenders: Vec<String> = Vec::new();
    let mut stack = vec![src_dir().join("sim")];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).expect("read src/sim") {
            let p = e.expect("entry").path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            if p.extension().is_some_and(|x| x == "rs") {
                let text = std::fs::read_to_string(&p).expect("read");
                for (i, line) in text.lines().enumerate() {
                    let code = match line.find("//") {
                        Some(at) => &line[..at],
                        None => line,
                    };
                    for banned in [
                        "SystemTime",
                        "Instant",
                        "UNIX_EPOCH",
                        "elapsed()",
                        "replay_io",
                        "ReplayWriter",
                        "ReplayConfig",
                    ] {
                        if code.contains(banned) {
                            offenders.push(format!("{}:{}: {banned}", p.display(), i + 1));
                        }
                    }
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "the sim names a clock, a path or the driver's writer: {offenders:#?}"
    );
}

/// The sim's own file-touching is confined to the content loader and the log's
/// own save/load; the writer's filesystem work happens outside `sim`.
#[test]
fn the_writer_lives_outside_the_sim_and_the_sim_does_not_depend_on_it() {
    let io = std::fs::read_to_string(src_dir().join("replay_io.rs")).expect("replay_io.rs");
    assert!(io.contains("SystemTime"), "the clock is not where it is claimed to be");
    // ...and `replay_io` is not a sim module.
    let sim_mod = std::fs::read_to_string(src_dir().join("sim/mod.rs")).expect("sim/mod.rs");
    assert!(!sim_mod.contains("replay_io"), "the sim declares the driver's writer");
    let lib = std::fs::read_to_string(src_dir().join("lib.rs")).expect("lib.rs");
    assert!(lib.contains("pub mod replay_io;"), "the writer is not a driver module");
}

// ---- 2. what reaches disk must be usable -----------------------------------

/// **Phase 2's own goal, end to end.** Anything the writer reports as `Written`
/// must exist, load through the *front door* against this build's content, and
/// replay to the recorded verdict. Over several seeds, so a single lucky match
/// cannot carry it.
#[test]
fn every_file_the_writer_claims_to_have_written_is_a_usable_replay() {
    for seed in [7u64, 4, 0xA11CE] {
        let dir = Scratch::new(&format!("usable-{seed}"));
        let mut app = ai_vs_ai(seed, Some(ReplayWriter::new(config_in(dir.path()))));
        let mut played = 0u32;
        while app.world().resource::<MatchState>().outcome().is_none() && played < 8 * 60 * 60 {
            step(&mut app);
            played += 1;
        }
        let decided = app
            .world()
            .resource::<MatchState>()
            .outcome()
            .expect("the match must decide inside the budget");
        // A few more frames, so the writer is asked repeatedly after the fact.
        tick(&mut app, 20);

        let writer = app.world().resource::<ReplayWriter>();
        assert!(writer.error().is_none(), "seed {seed}: write failed: {:?}", writer.error());
        let path = writer
            .written()
            .unwrap_or_else(|| panic!("seed {seed}: a decided match wrote nothing"))
            .to_path_buf();
        assert!(path.exists(), "seed {seed}: the reported path does not exist");
        assert_eq!(logs_in(dir.path()), vec![path.clone()], "seed {seed}: not exactly one log");

        // The bytes on disk are exactly what the sim would have written.
        let recorded = app.world().resource::<CommandLog>().log().clone();
        assert_eq!(
            std::fs::read_to_string(&path).expect("read the log"),
            recorded.to_ron().expect("the recorded log serializes"),
            "seed {seed}: the file is not the log"
        );

        // The front door accepts it, and it replays to the same verdict.
        let loaded = MatchLog::load_for(&path, &content())
            .unwrap_or_else(|e| panic!("seed {seed}: the shipped writer wrote a log the front door refuses: {e}"));
        assert_eq!(loaded.commands, recorded.commands, "seed {seed}: the file lost commands");
        assert!(loaded.content.is_known() && !loaded.content_changed);

        let mut r = ai_vs_ai(loaded.seed, None);
        r.insert_resource(CommandLog::new(loaded.seed));
        r.insert_resource(onus::sim::ReplaySource::new(loaded));
        tick(&mut r, played + 20);
        let src = r.world().resource::<onus::sim::ReplaySource>();
        assert_eq!(src.rejection(), None, "seed {seed}: the replay refused the shipped log");
        assert!(src.cursor() > 0, "seed {seed}: the replay fed nothing");
        assert_eq!(
            r.world().resource::<MatchState>().outcome(),
            Some(decided),
            "seed {seed}: the shipped log replays to a different verdict"
        );
    }
}

// ---- 3. the collision walk --------------------------------------------------

/// The name is not injective, so it is never trusted. With every candidate name
/// already taken by a file with known contents, the writer must lose the **new**
/// log and leave every old one byte-for-byte intact.
#[test]
fn exhausting_the_name_budget_loses_the_new_log_and_no_old_one() {
    let dir = Scratch::new("exhausted");
    const BUDGET: u32 = 3;
    const STAMP: u64 = 1_700_000_123;
    let seed = 7u64;
    let cfg = ReplayConfig {
        enabled: true,
        dir: dir.path().display().to_string(),
        prefix: "critic".to_string(),
        max_collisions: BUDGET,
    };
    // Every name the writer can produce, already taken.
    let mut planted: Vec<(PathBuf, String)> = Vec::new();
    for n in 0..=BUDGET {
        let name = if n == 0 {
            format!("critic-{STAMP}-seed{seed}.ron")
        } else {
            format!("critic-{STAMP}-seed{seed}-{n}.ron")
        };
        let p = dir.path().join(name);
        let body = format!("do not touch me ({n})");
        std::fs::write(&p, &body).expect("plant");
        planted.push((p, body));
    }

    let mut app = ai_vs_ai(seed, Some(ReplayWriter::with_fixed_clock(cfg, STAMP)));
    let mut played = 0u32;
    while app.world().resource::<MatchState>().outcome().is_none() && played < 8 * 60 * 60 {
        step(&mut app);
        played += 1;
    }
    tick(&mut app, 5);

    let writer = app.world().resource::<ReplayWriter>();
    assert!(writer.written().is_none(), "the writer overwrote one of the taken names");
    let err = writer.error().expect("running out of names was not reported");
    assert!(
        err.contains("refusing to overwrite") || err.to_lowercase().contains("taken"),
        "the failure does not say what happened: {err}"
    );
    for (p, body) in &planted {
        assert_eq!(
            &std::fs::read_to_string(p).expect("planted file vanished"),
            body,
            "an existing log was modified: {}",
            p.display()
        );
    }
    assert_eq!(logs_in(dir.path()).len(), planted.len(), "a stray file was left behind");
}

/// One free name after several taken ones: the writer walks to it, and writes
/// there rather than anywhere else.
#[test]
fn the_writer_walks_past_taken_names_to_the_first_free_one() {
    let dir = Scratch::new("walk");
    const STAMP: u64 = 1_700_000_777;
    let seed = 7u64;
    for n in 0..3 {
        let name = if n == 0 {
            format!("critic-{STAMP}-seed{seed}.ron")
        } else {
            format!("critic-{STAMP}-seed{seed}-{n}.ron")
        };
        std::fs::write(dir.path().join(name), "taken").expect("plant");
    }
    let mut app = ai_vs_ai(
        seed,
        Some(ReplayWriter::with_fixed_clock(config_in(dir.path()), STAMP)),
    );
    while app.world().resource::<MatchState>().outcome().is_none() {
        step(&mut app);
    }
    step(&mut app);
    let path = app
        .world()
        .resource::<ReplayWriter>()
        .written()
        .expect("nothing was written")
        .to_path_buf();
    assert_eq!(
        path.file_name().unwrap().to_string_lossy(),
        format!("critic-{STAMP}-seed{seed}-3.ron"),
        "the writer did not claim the first free name"
    );
    assert!(MatchLog::load_for(&path, &content()).is_ok());
}

/// A budget of zero means one attempt: taken ⇒ refused, free ⇒ written. An
/// off-by-one here would either overwrite or never write.
#[test]
fn a_zero_collision_budget_is_exactly_one_attempt() {
    const STAMP: u64 = 1_700_000_999;
    let seed = 7u64;
    let name = format!("critic-{STAMP}-seed{seed}.ron");
    for (taken, expect_written) in [(true, false), (false, true)] {
        let dir = Scratch::new(&format!("budget0-{taken}"));
        if taken {
            std::fs::write(dir.path().join(&name), "taken").expect("plant");
        }
        let cfg = ReplayConfig { max_collisions: 0, ..config_in(dir.path()) };
        let mut app = ai_vs_ai(seed, Some(ReplayWriter::with_fixed_clock(cfg, STAMP)));
        while app.world().resource::<MatchState>().outcome().is_none() {
            step(&mut app);
        }
        step(&mut app);
        let w = app.world().resource::<ReplayWriter>();
        assert_eq!(
            w.written().is_some(),
            expect_written,
            "budget 0 with the name {}: wrote={:?} error={:?}",
            if taken { "taken" } else { "free" },
            w.written(),
            w.error()
        );
        if taken {
            assert_eq!(
                std::fs::read_to_string(dir.path().join(&name)).unwrap(),
                "taken",
                "the single attempt overwrote the file it should have refused"
            );
        }
    }
}

// ---- 4. a hostile filesystem -----------------------------------------------

/// The writer never panics and never takes the game down, whatever the
/// filesystem says: a `dir` that is a regular file, and a `dir` inside an
/// unwritable parent. The match keeps running either way.
#[test]
fn a_filesystem_that_refuses_is_reported_and_the_match_survives() {
    // (a) `dir` is a file, not a directory.
    let scratch = Scratch::new("hostile");
    let as_file = scratch.path().join("i-am-a-file");
    std::fs::write(&as_file, "not a directory").expect("write");
    let cfg = ReplayConfig { dir: as_file.display().to_string(), ..config_in(scratch.path()) };
    let mut app = ai_vs_ai(7, Some(ReplayWriter::new(cfg)));
    let mut played = 0u32;
    while app.world().resource::<MatchState>().outcome().is_none() && played < 8 * 60 * 60 {
        step(&mut app);
        played += 1;
    }
    let before = onus::sim::state_hash(app.world_mut());
    tick(&mut app, 30);
    let w = app.world().resource::<ReplayWriter>();
    assert!(w.written().is_none(), "a file was written into a regular file");
    assert!(w.error().is_some(), "a failed write was not reported");
    assert!(w.asks() > 20, "the writer stopped being asked");
    assert_eq!(
        onus::sim::state_hash(app.world_mut()),
        before,
        "a failed write disturbed the frozen post-match sim"
    );
    assert_eq!(
        std::fs::read_to_string(&as_file).unwrap(),
        "not a directory",
        "the writer clobbered the file it mistook for a directory"
    );

    // (b) an unwritable parent (skipped when running as root, where it is not).
    let ro = Scratch::new("readonly");
    let mut perms = std::fs::metadata(ro.path()).expect("meta").permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o500);
        std::fs::set_permissions(ro.path(), perms.clone()).expect("chmod");
        let probe = ro.path().join("writable?");
        let writable = std::fs::write(&probe, "x").is_ok();
        let _ = std::fs::remove_file(&probe);
        if !writable {
            let cfg = ReplayConfig {
                dir: ro.path().join("nested").display().to_string(),
                ..config_in(ro.path())
            };
            let mut app = ai_vs_ai(7, Some(ReplayWriter::new(cfg)));
            let mut played = 0u32;
            while app.world().resource::<MatchState>().outcome().is_none() && played < 8 * 60 * 60 {
                step(&mut app);
                played += 1;
            }
            tick(&mut app, 5);
            let w = app.world().resource::<ReplayWriter>();
            assert!(w.written().is_none());
            assert!(w.error().is_some(), "an unwritable directory was not reported");
        }
        perms.set_mode(0o700);
        let _ = std::fs::set_permissions(ro.path(), perms);
    }
}

// ---- 5. exactly once, around the edges -------------------------------------

/// A match decided on its very first tick still writes, once — and the write
/// does not depend on the match having run long enough for anything else.
#[test]
fn a_match_decided_immediately_writes_exactly_one_log() {
    let dir = Scratch::new("tick0");
    let c = content();
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .insert_resource(Time::<Fixed>::from_hz(60.0))
        .insert_resource(c)
        .init_resource::<CommandQueue>()
        .init_resource::<RateReport>()
        .init_resource::<Casualties>()
        .insert_resource(Stockpiles::starting(0));
    onus::add_sim_systems(&mut app, Update);
    onus::add_replay_writer(&mut app, config_in(dir.path()));
    app.insert_resource(CommandLog::new(1));
    spawn_building(&mut app, "hq", Faction::A, Vec2::new(-100.0, 0.0));
    let b = spawn_building(&mut app, "hq", Faction::B, Vec2::new(100.0, 0.0));
    step(&mut app);
    app.world_mut().despawn(b);
    step(&mut app);
    assert!(app.world().resource::<MatchState>().is_over(), "the fixture did not decide");
    let first = app.world().resource::<ReplayWriter>().written().map(|p| p.to_path_buf());
    assert!(first.is_some(), "an immediately decided match wrote nothing");
    tick(&mut app, 200);
    assert_eq!(
        app.world().resource::<ReplayWriter>().written().map(|p| p.to_path_buf()),
        first,
        "the writer wrote a second time"
    );
    assert!(app.world().resource::<ReplayWriter>().asks() > 150, "the latch was never tested");
    assert_eq!(logs_in(dir.path()).len(), 1, "more than one log reached disk");
}

/// An exit in the **same frame** as the decision, and repeated exits after it,
/// still produce exactly one log — the decision write and the exit write cannot
/// both fire.
#[test]
fn an_exit_racing_the_decision_still_writes_exactly_one_log() {
    let dir = Scratch::new("race");
    let mut app = ai_vs_ai(7, Some(ReplayWriter::new(config_in(dir.path()))));
    let mut played = 0u32;
    // Stop one tick short of the decision, then decide and exit in one frame.
    while app.world().resource::<MatchState>().outcome().is_none() && played < 8 * 60 * 60 {
        app.world_mut().write_message(AppExit::Success);
        step(&mut app);
        played += 1;
        if played > 5 && app.world().resource::<ReplayWriter>().written().is_some() {
            break;
        }
    }
    for _ in 0..10 {
        app.world_mut().write_message(AppExit::Success);
        step(&mut app);
    }
    let w = app.world().resource::<ReplayWriter>();
    assert!(w.written().is_some(), "neither path wrote");
    assert!(w.asks() > 5, "the writer was barely asked");
    assert_eq!(
        logs_in(dir.path()).len(),
        1,
        "an exit and a decision both wrote: {:?}",
        logs_in(dir.path())
    );
    assert!(MatchLog::load_for(w.written().unwrap(), &content()).is_ok());
}

/// An app that never played writes nothing, and says which nothing it is — a
/// zero-byte or unstamped file on disk would be worse than none.
#[test]
fn an_app_that_never_ticked_writes_no_file_at_all() {
    let dir = Scratch::new("never");
    // A driver with the writer but no sim chain: nothing ever stamped the log,
    // so there is no match to replay. (In the shipped binary the chain always
    // runs, so this is the defensive branch, not a reachable state — which is
    // exactly why it is worth pinning that it produces no file.)
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .insert_resource(Time::<Fixed>::from_hz(60.0))
        .insert_resource(content())
        .init_resource::<MatchState>()
        .insert_resource(CommandLog::new(7));
    onus::add_replay_writer(&mut app, config_in(dir.path()));
    assert!(!app.world().resource::<CommandLog>().log().content.is_known());
    app.world_mut().write_message(AppExit::Success);
    app.update();
    let w = app.world().resource::<ReplayWriter>();
    assert_eq!(w.outcome(), Some(&WriteOutcome::NothingToWrite), "outcome: {:?}", w.outcome());
    assert!(w.asks() > 0, "the writer was never asked");
    assert!(logs_in(dir.path()).is_empty(), "a file was written for a match that never ran");
    assert!(
        !dir.path().join("critic").exists(),
        "an unplayed match still created output"
    );
}

/// A disabled writer must not touch the filesystem at all — not even to create
/// its directory. "Off" is off.
#[test]
fn a_disabled_writer_does_not_even_create_its_directory() {
    let scratch = Scratch::new("disabled");
    let target = scratch.path().join("should-never-exist");
    let cfg = ReplayConfig { enabled: false, ..config_in(&target) };
    let mut app = ai_vs_ai(7, Some(ReplayWriter::new(cfg)));
    let mut played = 0u32;
    while app.world().resource::<MatchState>().outcome().is_none() && played < 8 * 60 * 60 {
        step(&mut app);
        played += 1;
    }
    tick(&mut app, 20);
    let w = app.world().resource::<ReplayWriter>();
    assert!(w.asks() > 20, "the disabled writer was never asked, so this proves nothing");
    assert_eq!(w.outcome(), None, "a disabled writer recorded an outcome");
    assert!(!target.exists(), "a disabled writer created its output directory");
}

// ---- 6. the config ----------------------------------------------------------

/// Missing ⇒ off. Present-but-broken ⇒ error, in every way a file can be
/// broken — a typo in a field name must never be readable as "off".
#[test]
fn a_missing_config_is_off_and_no_broken_one_reads_as_off() {
    let dir = Scratch::new("config");
    // Missing.
    let cfg = ReplayConfig::load_from_dir(dir.path()).expect("a missing config is a state");
    assert!(!cfg.enabled, "the default is not off");
    assert_eq!(cfg, ReplayConfig::default());

    let write = |body: &str| {
        std::fs::write(dir.path().join(ReplayConfig::FILE), body).expect("write config");
        ReplayConfig::load_from_dir(dir.path())
    };
    for (name, body) in [
        ("empty", ""),
        ("whitespace", "  \n\t\n"),
        ("garbage", "not ron at all }{"),
        ("truncated", "(enabled: true, dir: \"x\", prefix: \"y\""),
        // A typo in a field name: the field it meant is then missing.
        ("typo", "(enable: true, dir: \"x\", prefix: \"y\", max_collisions: 4)"),
        ("missing-dir", "(enabled: true, prefix: \"y\", max_collisions: 4)"),
        ("wrong-type", "(enabled: \"yes\", dir: \"x\", prefix: \"y\", max_collisions: 4)"),
    ] {
        let got = write(body);
        assert!(
            got.is_err(),
            "a {name} config was accepted as {:?} — a broken config read as a state",
            got.ok()
        );
    }
    // A well-formed one round-trips exactly.
    let ok = write("(enabled: true, dir: \"somewhere\", prefix: \"p\", max_collisions: 3)")
        .expect("a well-formed config must load");
    assert_eq!(
        ok,
        ReplayConfig { enabled: true, dir: "somewhere".into(), prefix: "p".into(), max_collisions: 3 }
    );
}

/// The shipped configuration writes nothing anywhere until someone turns it on,
/// and its default directory is ignored by git.
#[test]
fn the_shipped_configuration_is_off_and_cannot_litter_the_repo() {
    let shipped = ReplayConfig::load_from_dir(&data_dir()).expect("assets/data/replay.ron");
    assert!(!shipped.enabled, "the shipped config writes a log on every run");
    assert!(!shipped.dir.starts_with('/'), "the default output path is absolute");
    assert!(!shipped.dir.contains(".."), "the default output path escapes upwards");
    let ignore = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".gitignore"),
    )
    .expect(".gitignore");
    assert!(
        ignore.lines().any(|l| l.trim().trim_start_matches('/') == shipped.dir),
        "the default replay directory `{}` is not gitignored",
        shipped.dir
    );
    // `replay.ron` is not part of `Content`, so it cannot reach the fingerprint.
    let before = content().fingerprint();
    assert_eq!(before, content().fingerprint());
    let c = std::fs::read_to_string(src_dir().join("sim/content.rs")).expect("content.rs");
    assert!(
        !c.contains("replay.ron") && !c.contains("ReplayConfig"),
        "the content loader knows about the replay config, so a path can reach a fingerprint"
    );
}

// ---- 7. F-004: one definition ----------------------------------------------

/// `add_replay_writer` is the one definition of when a log reaches disk, and
/// nothing hand-rolls the registration.
#[test]
fn only_add_replay_writer_registers_the_writer() {
    let mut offenders: Vec<String> = Vec::new();
    let mut stack = vec![src_dir()];
    let mut definitions = 0;
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).expect("read src") {
            let p = e.expect("entry").path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            if p.extension().is_some_and(|x| x == "rs") {
                let text = std::fs::read_to_string(&p).expect("read");
                if text.contains("pub fn add_replay_writer") {
                    definitions += 1;
                }
                for (i, line) in text.lines().enumerate() {
                    let code = match line.find("//") {
                        Some(at) => &line[..at],
                        None => line,
                    };
                    for sys in ["write_on_decision", "write_on_exit"] {
                        if code.contains(sys) && !p.ends_with("replay_io.rs") {
                            // Registered anywhere but inside `add_replay_writer`.
                            let at = text.find("pub fn add_replay_writer").unwrap_or(usize::MAX);
                            let end = at.saturating_add(
                                text[at.min(text.len())..].find("\n}\n").unwrap_or(0),
                            );
                            let offset: usize =
                                text.lines().take(i).map(|l| l.len() + 1).sum();
                            if !(offset > at && offset < end) {
                                offenders.push(format!("{}:{}", p.display(), i + 1));
                            }
                        }
                    }
                }
            }
        }
    }
    assert_eq!(definitions, 1, "there is not exactly one `add_replay_writer`");
    assert!(
        offenders.is_empty(),
        "the writer's systems are registered outside the one definition: {offenders:#?}"
    );
    // The shipped binary uses it.
    let lib = std::fs::read_to_string(src_dir().join("lib.rs")).expect("lib.rs");
    let build = lib.find("pub fn build_app").expect("build_app");
    let build_end = build + lib[build..].find("\n}\n").expect("build_app has a body");
    assert!(
        lib[build..build_end].contains("add_replay_writer(&mut app"),
        "the shipped app does not install the writer"
    );
}

// ---- 8. the one thing the design says must be loud -------------------------

/// **A malformed `replay.ron` must not be indistinguishable from "off".**
///
/// That rule is stated three times in this diff — in `load_from_dir`
/// ("silently treating a typo as 'off' would make the feature look broken
/// instead of misconfigured"), in `assets/data/replay.ron` ("a missing optional
/// config is a state; a broken one is a mistake"), and in `build_app` ("a
/// malformed `replay.ron` disables it and says so"). The loader keeps its half:
/// it returns `Err`. The driver's half is the `error!` that turns that `Err`
/// into something a human sees.
///
/// `error!` is a `tracing` macro. With no subscriber installed it evaluates its
/// arguments and drops the message — and Bevy installs the subscriber in
/// `LogPlugin`, which arrives with `DefaultPlugins`. So an `error!` emitted
/// before `add_plugins(DefaultPlugins)` goes nowhere, and the typo becomes
/// silent after all. Every diagnostic in `build_app` has to come after the
/// plugin that can carry it.
#[test]
fn every_diagnostic_in_build_app_is_emitted_after_a_log_subscriber_exists() {
    let lib = std::fs::read_to_string(src_dir().join("lib.rs")).expect("lib.rs");
    let build = lib.find("pub fn build_app").expect("build_app");
    let build_end = build + lib[build..].find("\n}\n").expect("build_app has a body");
    let body = &lib[build..build_end];
    let subscriber = body
        .find("add_plugins(DefaultPlugins)")
        .expect("build_app installs DefaultPlugins, which carries LogPlugin");

    let mut deaf: Vec<String> = Vec::new();
    for macro_name in ["error!", "warn!", "info!"] {
        let mut at = 0usize;
        while let Some(i) = body[at..].find(macro_name) {
            let start = at + i;
            at = start + macro_name.len();
            // Skip mentions inside comments.
            let line_start = body[..start].rfind('\n').map(|n| n + 1).unwrap_or(0);
            let line = &body[line_start..start];
            if line.trim_start().starts_with("//") || line.contains("// ") {
                continue;
            }
            if start < subscriber {
                let n = body[..start].lines().count() + lib[..build].lines().count();
                deaf.push(format!("lib.rs:{n}: {macro_name}"));
            }
        }
    }
    assert!(
        deaf.is_empty(),
        "these diagnostics are emitted before `LogPlugin` installs a tracing \
         subscriber, so nothing is printed and the condition they report is \
         silent: {deaf:#?}"
    );
}

/// The same property from the other side: whatever `build_app` does with a
/// broken config, the *loader* must keep telling the truth, so a caller that
/// can report it still can.
#[test]
fn the_loader_still_reports_a_broken_config_whatever_the_driver_does_with_it() {
    let dir = Scratch::new("loud");
    std::fs::write(dir.path().join(ReplayConfig::FILE), "(enable: true)").expect("write");
    let err = ReplayConfig::load_from_dir(dir.path())
        .expect_err("a malformed config must not load as a state");
    assert!(
        err.contains(ReplayConfig::FILE),
        "the error does not name the file a human has to fix: {err}"
    );
}

// ---- 9. the guard against the void defect must guard against it ------------

/// Resolve a `path::to::function` named in `src/lib.rs` to its body, searching
/// every `.rs` under `src/`. `Err` on anything it cannot pin down to exactly
/// one definition — a guard that skips what it cannot read is the shape of
/// defect this whole milestone keeps paying for.
fn resolve_src_fn(name: &str) -> Result<String, String> {
    let needle = format!("pub fn {name}(");
    let mut files: Vec<PathBuf> = Vec::new();
    let mut stack = vec![src_dir()];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).map_err(|e| e.to_string())? {
            let p = e.map_err(|e| e.to_string())?.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                files.push(p);
            }
        }
    }
    files.sort();
    let mut found = Vec::new();
    for f in files {
        let text = std::fs::read_to_string(&f).map_err(|e| e.to_string())?;
        if let Some(at) = text.find(&needle) {
            let rest = &text[at..];
            let end = rest[1..]
                .find("\n}\n")
                .map(|i| i + 3)
                .ok_or_else(|| format!("{name}: no closing brace at column 0 in {}", f.display()))?;
            found.push(rest[..end].to_string());
        }
    }
    match found.len() {
        1 => Ok(found.pop().expect("one")),
        0 => Err(format!("{name}: no `{needle}` anywhere under src/")),
        n => Err(format!("{name}: `{needle}` is defined in {n} places")),
    }
}

/// **The regression guard, guarding the regression.**
///
/// The pass-1 defect was a diagnostic emitted before `LogPlugin` installed a
/// `tracing` subscriber, so the message was evaluated and dropped. The fix moved
/// the reporting out of `build_app` and into `replay_io::load_config_or_report`
/// — which means the positional guards that scan `build_app`'s body for
/// `error!`/`warn!`/`info!` now scan a body that contains none, and pass on an
/// empty set. Restoring the exact defect (moving the call back above
/// `add_plugins`) leaves every such guard green.
///
/// So the property has to be about **what `build_app` calls**, not about what it
/// spells: every function it invokes that can emit a diagnostic must be invoked
/// after the plugin that can carry one.
#[test]
fn nothing_build_app_calls_can_report_before_the_subscriber_exists() {
    let lib = std::fs::read_to_string(src_dir().join("lib.rs")).expect("lib.rs");
    let at = lib.find("pub fn build_app").expect("build_app");
    let end = at + lib[at..].find("\n}\n").expect("build_app ends");
    let body = &lib[at..end];
    let subscriber = body
        .find("add_plugins(DefaultPlugins)")
        .expect("build_app installs DefaultPlugins, which carries LogPlugin");

    // Every `something::name(` call in the body, with where it is called.
    let mut deaf: Vec<String> = Vec::new();
    let mut checked = 0usize;
    let mut i = 0usize;
    while i < body.len() {
        let Some(rel) = body[i..].find("::") else { break };
        let colons = i + rel;
        // The identifier after `::`, and the `(` that would make it a call.
        let after = &body[colons + 2..];
        let idl = after
            .find(|c: char| !(c.is_alphanumeric() || c == '_'))
            .unwrap_or(after.len());
        let name = &after[..idl];
        i = colons + 2 + idl.max(1);
        if name.is_empty() || !after[idl..].starts_with('(') {
            continue;
        }
        if name.starts_with(|c: char| c.is_uppercase()) || name == "new" || name == "from_hz" {
            continue;
        }
        // Only functions this crate defines; anything else is Bevy's.
        let Ok(callee) = resolve_src_fn(name) else { continue };
        checked += 1;
        let reports = ["error!", "warn!", "info!", "debug!", "trace!"]
            .iter()
            .any(|m| {
                callee
                    .lines()
                    .any(|l| l.split("//").next().unwrap_or("").contains(m))
            });
        if reports && colons < subscriber {
            let line = body[..colons].lines().count() + lib[..at].lines().count();
            deaf.push(format!("lib.rs:{line}: {name}(..) can report, and is called here"));
        }
    }
    assert!(
        checked > 0,
        "no callee of `build_app` was resolved, so this guard inspects nothing"
    );
    assert!(
        deaf.is_empty(),
        "these are called before `LogPlugin` installs a subscriber and can emit a \
         diagnostic, which `tracing` will drop: {deaf:#?}"
    );
}

/// The seam has to be the shipped path, not a parallel one: `build_app` must
/// call the very function the capturing-subscriber test exercises.
#[test]
fn build_app_reports_through_the_function_the_tests_observe() {
    let lib = std::fs::read_to_string(src_dir().join("lib.rs")).expect("lib.rs");
    let at = lib.find("pub fn build_app").expect("build_app");
    let end = at + lib[at..].find("\n}\n").expect("build_app ends");
    assert!(
        lib[at..end].contains("load_config_or_report("),
        "the shipped app no longer loads its config through the reporting seam"
    );
    let seam = resolve_src_fn("load_config_or_report").expect("the seam is one function");
    assert!(
        seam.lines()
            .any(|l| l.split("//").next().unwrap_or("").contains("error!")),
        "the seam no longer reports anything, so the capture test observes nothing"
    );
    // ...and the loader it wraps still refuses, so the seam has something to report.
    let dir = Scratch::new("seam");
    std::fs::write(dir.path().join(ReplayConfig::FILE), "(enable: true)").expect("write");
    assert!(ReplayConfig::load_from_dir(dir.path()).is_err());
}

// ---- 10. `write_or_discard`'s removal --------------------------------------

/// **A cleanup that can remove a file it did not create is worse than the
/// litter it fixes.** The claim is that only the path claimed with `create_new`
/// in the same call is ever removed. A symlink is the sharpest test: if the
/// claim followed one, the writer would write through it and then delete
/// somebody else's file.
#[test]
fn a_symlink_at_a_candidate_name_is_neither_written_through_nor_removed() {
    #[cfg(unix)]
    {
        let dir = Scratch::new("symlink");
        let precious = dir.path().join("precious.txt");
        std::fs::write(&precious, "an old log nobody may touch").expect("write");
        const STAMP: u64 = 1_700_009_000;
        let seed = 7u64;
        // The first candidate name is a symlink pointing at the precious file.
        let candidate = dir.path().join(format!("critic-{STAMP}-seed{seed}.ron"));
        std::os::unix::fs::symlink(&precious, &candidate).expect("symlink");

        let mut app = ai_vs_ai(
            seed,
            Some(ReplayWriter::with_fixed_clock(config_in(dir.path()), STAMP)),
        );
        while app.world().resource::<MatchState>().outcome().is_none() {
            step(&mut app);
        }
        step(&mut app);

        // The precious file is untouched...
        assert_eq!(
            std::fs::read_to_string(&precious).expect("the precious file was removed"),
            "an old log nobody may touch",
            "the writer wrote through a symlink"
        );
        // ...the symlink itself still points where it did...
        assert!(
            std::fs::symlink_metadata(&candidate).is_ok(),
            "the writer removed a symlink it did not create"
        );
        // ...and the log went somewhere else entirely, and is usable.
        let w = app.world().resource::<ReplayWriter>();
        let written = w.written().unwrap_or_else(|| panic!("nothing written: {:?}", w.error()));
        assert_ne!(written, candidate.as_path(), "the writer claimed the symlink");
        assert!(MatchLog::load_for(written, &content()).is_ok());
    }
}

/// The removal is reachable from exactly one place, and that place hands it the
/// path it has just claimed. A second caller — or a caller that passes anything
/// else — would put an arbitrary path within reach of a `remove_file`.
#[test]
fn only_the_freshly_claimed_path_can_reach_the_cleanup() {
    let io = std::fs::read_to_string(src_dir().join("replay_io.rs")).expect("replay_io.rs");
    // Call sites outside the definition and outside its own unit tests.
    let tests_at = io.find("mod tests {").unwrap_or(io.len());
    let shipped = &io[..tests_at];
    let calls: Vec<usize> = shipped
        .match_indices("write_or_discard(")
        .map(|(i, _)| i)
        .filter(|i| !shipped[..*i].ends_with("fn "))
        .collect();
    assert_eq!(
        calls.len(),
        1,
        "`write_or_discard` is called from {} places in shipped code; each is a \
         path a `remove_file` can reach",
        calls.len()
    );
    // And `remove_file` appears nowhere else in the writer.
    let removals: Vec<&str> = shipped
        .lines()
        .filter(|l| l.split("//").next().unwrap_or("").contains("remove_file"))
        .collect();
    assert_eq!(removals.len(), 1, "the writer deletes files from more than one place: {removals:?}");
    // The one call site is the one that just claimed the path.
    let at = calls[0];
    let window = &shipped[at.saturating_sub(300)..at];
    assert!(
        window.contains("claim_path("),
        "the cleanup is handed a path that was not claimed in the same expression"
    );
}

/// **The startup order the sim's entity allocation depends on.** M5's first
/// defect was `ReplaySource`'s insertion shifting every later entity id
/// (F-011), so the order in which `build_app` mutates the `World` is not a free
/// refactor. `DefaultPlugins` must stay the first thing that touches the app,
/// and the config load — which touches only the filesystem — must not be
/// interleaved among the resource insertions.
#[test]
fn build_app_touches_the_world_in_the_order_the_sim_was_pinned_against() {
    let lib = std::fs::read_to_string(src_dir().join("lib.rs")).expect("lib.rs");
    let at = lib.find("pub fn build_app").expect("build_app");
    let end = at + lib[at..].find("\n}\n").expect("build_app ends");
    let body = &lib[at..end];

    let mut ops: Vec<&str> = Vec::new();
    for line in body.lines() {
        let code = line.split("//").next().unwrap_or("").trim();
        for op in [
            "App::new()",
            "add_plugins(DefaultPlugins)",
            "insert_resource(Time::<Fixed>",
            "insert_resource(content)",
            "init_resource::<CursorWorld>",
            "init_resource::<DragState>",
            "init_resource::<ClickTracker>",
            "init_resource::<CommandQueue>",
            "init_resource::<RateReport>",
            "insert_resource(Stockpiles::starting",
            "add_sim_systems(&mut app",
            "add_replay_writer(&mut app",
        ] {
            if code.contains(op) {
                ops.push(op);
            }
        }
    }
    assert_eq!(
        ops,
        vec![
            "App::new()",
            "add_plugins(DefaultPlugins)",
            "insert_resource(Time::<Fixed>",
            "insert_resource(content)",
            "init_resource::<CursorWorld>",
            "init_resource::<DragState>",
            "init_resource::<ClickTracker>",
            "init_resource::<CommandQueue>",
            "init_resource::<RateReport>",
            "insert_resource(Stockpiles::starting",
            "add_sim_systems(&mut app",
            "add_replay_writer(&mut app",
        ],
        "the order in which `build_app` populates the `World` changed; in Bevy \
         0.19 a resource is an entity, so this shifts entity allocation, and \
         the sim's allocation-independence was pinned against this order"
    );
    // The config load reads a file and touches no `World`, so it may sit
    // anywhere after the subscriber — but it must not be an app operation.
    let seam = body.find("load_config_or_report(").expect("the seam is called");
    let plugins = body.find("add_plugins(DefaultPlugins)").expect("plugins");
    assert!(seam > plugins, "the config load moved back above the subscriber");
}
