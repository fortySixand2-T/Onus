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

/// A scratch output directory of this test's own, under the OS temp dir — never
/// the repo, never a home.
///
/// It removes itself **on drop**, not at the end of the test body, so a test
/// that fails or panics still leaves nothing behind. (The first version cleaned
/// up on the success path only, and the deliberate red-check runs promptly left
/// nineteen directories in `/tmp` — a test that litters only when it fails is a
/// test that litters exactly when you are least likely to notice.)
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("onus-p2-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Scratch(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scratch_dir(name: &str) -> Scratch {
    Scratch::new(name)
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
    let mut app = ai_vs_ai(7, Some(ReplayWriter::new(config_writing_to(dir.path()))));
    app.insert_resource(StateHashLog::default());
    let outcome = play_to_decision(&mut app);

    let written = app
        .world()
        .resource::<ReplayWriter>()
        .written()
        .expect("a decided match wrote no log")
        .to_path_buf();
    assert_eq!(logs_in(dir.path()), vec![written.clone()], "one log, in the configured dir");
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
}

/// **Exactly once.** A decided match keeps ticking, and then the app exits; one
/// file exists at the end, with one write behind it — while the writer was
/// *asked* to write on every one of those ticks.
#[test]
fn a_decided_match_writes_exactly_one_log_however_long_it_runs() {
    let dir = scratch_dir("once");
    let mut app = ai_vs_ai(7, Some(ReplayWriter::new(config_writing_to(dir.path()))));
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
    assert_eq!(logs_in(dir.path()), vec![first.clone()], "a second log was written");
    assert_eq!(
        std::fs::read(&first).expect("read"),
        bytes,
        "the log was rewritten in place"
    );
}

/// A session that ends **before** a decision still leaves its log: the exit path
/// is not merely a duplicate of the decision path.
#[test]
fn an_exit_before_any_decision_still_writes_what_was_played() {
    let dir = scratch_dir("exit");
    let mut app = ai_vs_ai(7, Some(ReplayWriter::new(config_writing_to(dir.path()))));
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
    assert_eq!(logs_in(dir.path()).len(), 1);
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
            dir: dir.path().display().to_string(),
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
    assert!(logs_in(dir.path()).is_empty(), "a disabled writer wrote a file");
}

/// A missing `replay.ron` is a legitimate state — logging off — while a
/// malformed one is an error, so a typo cannot masquerade as "off".
#[test]
fn a_missing_config_is_off_and_a_malformed_one_is_an_error() {
    let dir = scratch_dir("config");
    assert_eq!(
        ReplayConfig::load_from_dir(dir.path()).expect("a missing config is not an error"),
        ReplayConfig::default()
    );
    std::fs::write(dir.join(ReplayConfig::FILE), "(enabled: yes-please)").expect("write");
    let err = ReplayConfig::load_from_dir(dir.path()).expect_err("a malformed config must be an error");
    assert!(err.contains("replay.ron"), "the error does not name the file: {err}");
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
}

/// A log the sim could not write back is refused **at the writer**, not written
/// as a file nothing can load. (The front door is `to_ron`; this is the driver
/// honouring it.)
#[test]
fn a_log_that_cannot_be_read_back_is_never_written() {
    let dir = scratch_dir("poisoned");
    let mut app = ai_vs_ai(7, Some(ReplayWriter::new(config_writing_to(dir.path()))));
    tick(&mut app, 300);
    // Poison it the way the sim itself would: the content changes mid-match.
    let edited = {
        let scratch = scratch_dir("poisoned-content");
        for file in ["units.ron", "resources.ron", "strategies.ron"] {
            let text = std::fs::read_to_string(data_dir().join(file)).expect("read");
            let text = if file == "units.ron" {
                text.replacen("mvp_alloy_cost: 10", "mvp_alloy_cost: 11", 1)
            } else {
                text
            };
            std::fs::write(scratch.join(file), text).expect("write");
        }
        Content::load_from_dir(scratch.path()).expect("edited content")
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
    assert!(logs_in(dir.path()).is_empty(), "an unloadable file was left on disk");
}

// ---- a diagnostic nobody can see is not a diagnostic ------------------------

/// A `tracing` subscriber that keeps what it is told, so a test can assert that
/// a diagnostic was **emitted**, not merely that the condition it describes
/// occurred.
///
/// This is the whole lesson of the finding: `tracing` evaluates an event with no
/// subscriber installed and drops it, so "the code calls `error!`" and "the
/// operator is told" are different claims. Only the second one matters, and only
/// the second one is checked here.
mod capture {
    use bevy::log::tracing::{
        field::{Field, Visit},
        span, Event, Level, Metadata, Subscriber,
    };
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    pub struct Captured {
        pub events: Mutex<Vec<(Level, String)>>,
    }

    pub struct Recorder(pub Arc<Captured>);

    struct Text(String);

    impl Visit for Text {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            if field.name() == "message" {
                self.0 = format!("{value:?}");
            }
        }
    }

    impl Subscriber for Recorder {
        fn enabled(&self, _: &Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _: &span::Attributes<'_>) -> span::Id {
            span::Id::from_u64(1)
        }
        fn record(&self, _: &span::Id, _: &span::Record<'_>) {}
        fn record_follows_from(&self, _: &span::Id, _: &span::Id) {}
        fn event(&self, event: &Event<'_>) {
            let mut text = Text(String::new());
            event.record(&mut text);
            self.0
                .events
                .lock()
                .expect("lock")
                .push((*event.metadata().level(), text.0));
        }
        fn enter(&self, _: &span::Id) {}
        fn exit(&self, _: &span::Id) {}
    }
}

/// Run `f` with a capturing subscriber installed, and return what was emitted.
fn emitted<T>(f: impl FnOnce() -> T) -> (T, Vec<(bevy::log::tracing::Level, String)>) {
    let captured = std::sync::Arc::new(capture::Captured::default());
    let out = bevy::log::tracing::subscriber::with_default(
        capture::Recorder(captured.clone()),
        f,
    );
    let events = captured.events.lock().expect("lock").clone();
    (out, events)
}

/// **A broken config is reported where a human can see it**, and the report
/// names the file. Asserted by observing the *emission*, under a subscriber —
/// the code path that produced this defect emitted its `error!` five lines
/// before `LogPlugin` installed one, so the message was formatted and dropped.
#[test]
fn a_broken_config_is_reported_and_not_emitted_into_the_void() {
    let dir = scratch_dir("broken-config");
    std::fs::write(dir.join(ReplayConfig::FILE), "(enable: true)").expect("write");

    let (config, events) = emitted(|| onus::replay_io::load_config_or_report(dir.path()));

    // It fell back to off...
    assert_eq!(config, ReplayConfig::default());
    // ...and it *said so*, at error level, naming the file.
    let errors: Vec<&(bevy::log::tracing::Level, String)> = events
        .iter()
        .filter(|(l, _)| *l == bevy::log::tracing::Level::ERROR)
        .collect();
    assert_eq!(
        errors.len(),
        1,
        "a malformed config produced {} error diagnostics, not one: {events:?}",
        errors.len()
    );
    assert!(
        errors[0].1.contains("replay.ron"),
        "the report does not name the file: {:?}",
        errors[0].1
    );
    assert!(
        errors[0].1.contains("disabled"),
        "the report does not say what it did about it: {:?}",
        errors[0].1
    );
}

/// The direction that could break: the quiet cases stay quiet. A **missing**
/// config is the off state, not a mistake, and a **valid** one is neither — so
/// neither may emit anything, or the diagnostic that matters drowns.
#[test]
fn a_missing_or_valid_config_reports_nothing() {
    let dir = scratch_dir("quiet-config");

    let (config, events) = emitted(|| onus::replay_io::load_config_or_report(dir.path()));
    assert_eq!(config, ReplayConfig::default());
    assert!(events.is_empty(), "a missing config was reported: {events:?}");

    // And a real one, parsed and returned, silently.
    std::fs::write(
        dir.join(ReplayConfig::FILE),
        "(enabled: true, dir: \"somewhere\", prefix: \"p\", max_collisions: 3)",
    )
    .expect("write");
    let (config, events) = emitted(|| onus::replay_io::load_config_or_report(dir.path()));
    assert_eq!(
        config,
        ReplayConfig {
            enabled: true,
            dir: "somewhere".to_string(),
            prefix: "p".to_string(),
            max_collisions: 3
        }
    );
    assert!(events.is_empty(), "a valid config was reported: {events:?}");
}

// ---- can anything `build_app` reaches report too early? ---------------------

/// Every `.rs` file under `src/`, sorted.
fn src_files() -> Vec<PathBuf> {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    let mut stack = vec![src];
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
    files
}

/// The body of `fn <name>(` if **exactly one** function in `src/` is called
/// that. An ambiguous or unknown name (`new`, or anything from a dependency)
/// resolves to `None` — the guard below counts what it could not resolve rather
/// than assuming it was harmless.
fn fn_body(name: &str) -> Option<String> {
    let mut found: Vec<String> = Vec::new();
    for file in src_files() {
        let text = std::fs::read_to_string(&file).expect("read");
        for needle in [format!("pub fn {name}("), format!("fn {name}(")] {
            let mut from = 0usize;
            while let Some(rel) = text[from..].find(&needle) {
                let at = from + rel;
                // `fn foo(` inside `pub fn foo(` would match twice; count the
                // `pub` form only once by skipping a `fn` preceded by `pub `.
                let preceded_by_pub = text[..at].ends_with("pub ");
                if !(needle.starts_with("fn ") && preceded_by_pub) {
                    let rest = &text[at..];
                    let end = rest.find("\n}\n").map(|i| i + 2).unwrap_or(rest.len());
                    found.push(rest[..end].to_string());
                }
                from = at + needle.len();
            }
        }
    }
    match found.len() {
        1 => found.pop(),
        _ => None,
    }
}

/// Names called in `body`: every identifier immediately followed by `(`, taken
/// as its last path segment (`replay_io::load_config_or_report(..)` ⇒
/// `load_config_or_report`), with its offset.
fn calls_in(body: &str) -> Vec<(usize, String)> {
    let bytes = body.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c.is_alphanumeric() || c == '_' {
            let start = i;
            while i < bytes.len() && {
                let c = bytes[i] as char;
                c.is_alphanumeric() || c == '_'
            } {
                i += 1;
            }
            let is_declaration = body[..start].trim_end().ends_with("fn");
            if body[i..].starts_with('(') && !is_declaration {
                out.push((start, body[start..i].to_string()));
            }
        } else {
            i += 1;
        }
    }
    out
}

const DIAGNOSTIC_MACROS: [&str; 5] = ["error!", "warn!", "info!", "debug!", "trace!"];

/// Can a call to `name` end up emitting a `tracing` diagnostic — directly, or
/// through anything it calls in this crate?
fn can_report(name: &str, depth: u32, seen: &mut Vec<String>) -> bool {
    if depth == 0 || seen.iter().any(|s| s == name) {
        return false;
    }
    seen.push(name.to_string());
    let Some(body) = fn_body(name) else {
        return false;
    };
    let code: String = body
        .lines()
        .map(|l| l.split("//").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n");
    if DIAGNOSTIC_MACROS.iter().any(|m| code.contains(m)) {
        return true;
    }
    calls_in(&code)
        .into_iter()
        .any(|(_, callee)| can_report(&callee, depth - 1, seen))
}

/// **Nothing `build_app` reaches may report before the subscriber exists.**
///
/// The defect was one of *position*: an `error!` five lines above
/// `add_plugins(DefaultPlugins)`, which `tracing` evaluates and drops. The
/// obvious guard — scan `build_app` for diagnostic macros — is worthless the
/// moment the reporting moves into a helper, which is exactly what fixing it
/// did: it then scans a body with no macros in it and passes on the empty set.
/// A guard that passes on an empty set reads as protection and is not.
///
/// So this resolves what `build_app` *calls* and asks whether any of it can
/// report, and it fails against the mutation that would actually reintroduce
/// the defect: moving the `load_config_or_report(..)` call back above
/// `add_plugins`.
#[test]
fn nothing_build_app_reaches_can_report_before_the_subscriber_exists() {
    let lib = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs"),
    )
    .expect("src/lib.rs");
    let signature = lib.find("pub fn build_app()").expect("build_app");
    // From the opening brace, so the function's own name in its signature is not
    // read as a call to itself.
    let at = signature + lib[signature..].find('{').expect("build_app has a body");
    let end = at + lib[at..].find("\n}\n").expect("build_app ends");
    let body = &lib[at..end];
    let code: String = body
        .lines()
        .map(|l| l.split("//").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n");
    let subscriber_at = code
        .find("add_plugins(DefaultPlugins)")
        .expect("build_app adds DefaultPlugins, which installs the subscriber");

    // **The classifier has teeth**: it recognises the one reporter this crate
    // has on the startup path. If this fails, the machinery below is inert and
    // the rest of the test means nothing.
    assert!(
        can_report("load_config_or_report", 4, &mut Vec::new()),
        "the guard cannot tell that `load_config_or_report` reports, so it \
         cannot tell that anything else does either"
    );

    // **And it resolved the call that matters**, rather than skipping past it.
    let calls = calls_in(&code);
    assert!(
        calls.iter().any(|(_, n)| n == "load_config_or_report"),
        "`build_app` no longer calls `load_config_or_report`; if the reporting \
         moved, move this guard with it — do not let it certify an empty set"
    );

    let mut offenders = Vec::new();
    // Direct diagnostics in `build_app` itself...
    for (i, line) in body.lines().enumerate() {
        let line_code = line.split("//").next().unwrap_or("");
        let offset = code.find(line_code.trim()).unwrap_or(usize::MAX);
        if DIAGNOSTIC_MACROS.iter().any(|m| line_code.contains(m)) && offset < subscriber_at {
            offenders.push(format!("line {}: {}", i + 1, line_code.trim()));
        }
    }
    // ...and anything it calls before the subscriber that can report.
    for (offset, name) in calls {
        if offset >= subscriber_at {
            continue;
        }
        if can_report(&name, 4, &mut Vec::new()) {
            offenders.push(format!("`{name}(..)` is called before the subscriber exists"));
        }
    }
    assert!(
        offenders.is_empty(),
        "these can report before `LogPlugin` installs a subscriber, so `tracing` \
         drops the message and the condition they report is silent: {offenders:#?}"
    );
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
            Some(ReplayWriter::with_fixed_clock(config_writing_to(dir.path()), 1_700_000_000)),
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
    assert_eq!(logs_in(dir.path()).len(), 3, "a log was overwritten");
    for path in &written {
        MatchLog::load_for(path, &content()).expect("every log must load");
    }
    // The names are the same stem plus a claim suffix, in the order they were
    // taken — no timestamp guessing, no randomness.
    assert!(written[0].to_string_lossy().contains("-1700000000-seed7.ron"));
    assert!(written[1].to_string_lossy().ends_with("-seed7-1.ron"));
    assert!(written[2].to_string_lossy().ends_with("-seed7-2.ron"));
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
                dir: dir.path().display().to_string(),
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
    let (o2, h2, c2, f2, wrote) = trace(Some(ReplayWriter::new(config_writing_to(dir.path()))));
    let (o3, h3, c3, f3, _) = trace(Some(ReplayWriter::with_fixed_clock(
        config_writing_to(dir.path()),
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
    onus::add_replay_writer(&mut app, config_writing_to(dir.path()));
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
    assert!(logs_in(dir.path()).is_empty());
}
