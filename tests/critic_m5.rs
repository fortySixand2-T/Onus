//! Critic probes for M5 (deterministic replay). **Critic-owned**: the
//! implementer may not edit this file.
//!
//! Every probe here is written against the M5 spec and the project invariants,
//! not against the implementation's own framing.

use std::path::{Path, PathBuf};

use bevy::prelude::*;

use onus::sim::ai::AiCommanders;
use onus::sim::combat::{Casualties, Health};
use onus::sim::content::Content;
use onus::sim::economy::{Building, ProductionQueue, Stockpiles, UnitDefIdx};
use onus::sim::replay::{
    CommandFate, LoggedCommand, LoggedOrder, MatchLog, SimId, SimIds, LOG_FORMAT_VERSION,
};
use onus::sim::spatial::Faction;
use onus::sim::{
    AiJournal, Attribution, CommandLog, CommandQueue, CommandTick, MatchState, MoveTarget, Order,
    Position, RateReport, ReplaySource, ResourceNode, StateHashLog,
};

// ---- harness (duplicated deliberately: the critic depends on no fixture the
// implementer owns) ----------------------------------------------------------

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data")
}

fn content() -> Content {
    Content::load_from_dir(&data_dir()).expect("assets/data/*.ron")
}

fn sim_app_with(alloy: u32, pad: usize) -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .insert_resource(Time::<Fixed>::from_hz(60.0))
        .insert_resource(content())
        .init_resource::<CommandQueue>()
        .init_resource::<RateReport>()
        .init_resource::<Casualties>()
        .insert_resource(Stockpiles::starting(alloy));
    // Shift the ECS's entity allocation without adding anything to the world
    // the sim can see: these carry no `Position`, so they are not "things in
    // the world" and are never identified.
    for _ in 0..pad {
        app.world_mut().spawn_empty();
    }
    onus::add_sim_systems(&mut app, Update);
    app
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
        .spawn((Position(pos), Building { def }, faction, ProductionQueue::default()))
        .id()
}

/// The symmetric AI-vs-AI match, with `pad` extra bare entities allocated first
/// so two otherwise identical runs hand out different `Entity` ids.
fn ai_vs_ai_padded(seed: u64, pad: usize) -> App {
    let mut app = sim_app_with(content().economy.starting_alloy, pad);
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

fn ai_vs_ai(seed: u64) -> App {
    ai_vs_ai_padded(seed, 0)
}

/// A scratch path — file or directory — that removes itself **on drop**, so a
/// probe that fails or panics leaves nothing behind either. (A suite that
/// litters only on the runs you are least likely to watch is the same class of
/// problem this file exists to catch elsewhere.)
struct ScratchPath(PathBuf);

impl std::ops::Deref for ScratchPath {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for ScratchPath {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for ScratchPath {
    fn drop(&mut self) {
        if self.0.is_dir() {
            let _ = std::fs::remove_dir_all(&self.0);
        } else {
            let _ = std::fs::remove_file(&self.0);
        }
    }
}

/// A serial number per scratch path. Probes run concurrently and two of them may
/// ask for the same fixture *name*; without this, one probe's `Drop` could
/// delete the directory another is still reading.
fn scratch_serial() -> u64 {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

fn scratch(name: &str) -> ScratchPath {
    let p = std::env::temp_dir().join(format!(
        "onus-critic-m5-{name}-{}-{}.ron",
        std::process::id(),
        scratch_serial()
    ));
    let _ = std::fs::remove_file(&p);
    ScratchPath(p)
}

fn src_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
}

// ---- 1. the chain position of the new systems (F-008) -----------------------

/// **The justification the M5 diff wrote into the frozen M4c probe.**
///
/// `tests/critic_m4c.rs` added `sim/replay.rs` to the allowlist of files that
/// may name the gather claim, with the reason: "It is allowed only because it
/// is ordered after the sweep". The allowlist is per *file*, so that reason has
/// to hold for every system `sim/replay.rs` puts in the chain — otherwise the
/// allowance is wider than the argument that earns it, and a claim reader added
/// to a pre-sweep system in that file passes the M4c probe while violating
/// F-008.
#[test]
fn every_replay_system_in_the_chain_is_ordered_after_the_gather_claim_sweep() {
    let lib = std::fs::read_to_string(src_dir().join("lib.rs")).expect("src/lib.rs");
    let sweep = lib
        .find("sim::economy::repair_gather_claims")
        .expect("the sweep is in the chain");
    let mut before: Vec<&str> = Vec::new();
    let mut at = 0usize;
    while let Some(i) = lib[at..].find("sim::replay::") {
        let start = at + i;
        let rest = &lib[start..];
        let end = rest
            .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == ':'))
            .unwrap_or(rest.len());
        let name = &rest[..end];
        // Only registrations, not `init_resource::<...>` type paths.
        if !name.contains("CommandLog") && !name.contains("SimIds") && start < sweep {
            before.push(name);
        }
        at = start + end;
    }
    assert!(
        before.is_empty(),
        "sim/replay.rs is on the M4c gather-claim allowlist because it is \
         \"ordered after the sweep\", but these of its systems are registered \
         *before* it: {before:?}"
    );
}

/// The property the allowlist edit actually needs, whether or not the reason it
/// gave is the true one: nothing in `sim/replay.rs` that runs ahead of the
/// sweep may read either half of the gather claim.
#[test]
fn no_pre_sweep_replay_system_reads_the_gather_claim() {
    let replay = std::fs::read_to_string(src_dir().join("sim/replay.rs")).expect("replay.rs");
    let start = replay.find("pub fn identify(").expect("identify exists");
    // The function body runs to the next top-level item.
    let rest = &replay[start..];
    let end = rest[1..].find("\n}\n").map(|i| i + 2).unwrap_or(rest.len());
    let body = &rest[..end];
    for claim in ["GatherTarget", "GatherPhase", "SplitClaim"] {
        assert!(
            !body.contains(claim),
            "`identify` runs at the head of the chain, *before* \
             `repair_gather_claims`, and reads `{claim}` — a half claim reaches \
             it (F-008)"
        );
    }
}

/// The F-008 comment must sit on the system it is about. It is the only
/// instruction a future implementer gets about where a new claim reader may go,
/// and "belongs after *this* system" is a lie if `this system` is not the sweep.
#[test]
fn the_f008_instruction_names_the_sweep_and_not_whatever_follows_it() {
    let lib = std::fs::read_to_string(src_dir().join("lib.rs")).expect("src/lib.rs");
    // Comment prose, with the `//` line noise flattened out, so the instruction
    // is found wherever the line breaks happen to fall.
    let flat: String = lib
        .lines()
        .map(|l| l.trim().trim_start_matches("//").trim())
        .collect::<Vec<_>>()
        .join(" ");
    let marker = "Any new reader of the claim belongs after this system";
    let i = flat.find(marker).expect("the F-008 instruction is in lib.rs");
    let after = &flat[i + marker.len()..];
    let next_sys = after
        .find("sim::")
        .map(|j| {
            let r = &after[j..];
            let e = r
                .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == ':'))
                .unwrap_or(r.len());
            r[..e].to_string()
        })
        .unwrap_or_default();
    assert_eq!(
        next_sys, "sim::economy::repair_gather_claims",
        "the F-008 instruction \"belongs after this system\" now points at \
         `{next_sys}`, not at the sweep \u{2014} following it puts a new claim \
         reader ahead of `repair_gather_claims`"
    );
}

// ---- 2. persistence boundaries ---------------------------------------------

/// **Save/load is a round trip, or the write-side check is not a check.**
///
/// `to_ron` refuses a non-finite coordinate on the stated ground that "a log
/// that cannot be read back exactly is not a log". `from_ron` refuses a second
/// class of log as well — one naming an entity the sim never identified — but
/// the writer accepts it. The sim mints exactly that value (`apply_commands`
/// records `SimId::UNIDENTIFIED` for any entity it cannot resolve), so a match
/// can produce a log that saves and then will not load.
#[test]
fn anything_the_writer_accepts_the_reader_must_accept() {
    let mut log = MatchLog::new(9);
    log.commands.push(LoggedCommand {
        tick: 4,
        schedule: CommandTick::Asap,
        fate: CommandFate::Taken,
        attribution: Attribution::By(Faction::A),
        order: LoggedOrder::MoveTo {
            units: vec![SimId::UNIDENTIFIED.0],
            dest: (1.0, 2.0),
        },
    });
    let path = scratch("unidentified");
    let wrote = log.save(&path);
    if wrote.is_ok() {
        assert!(
            MatchLog::load(&path).is_ok(),
            "`save` wrote a log that `load` refuses: the write-side boundary \
             check admits a value it cannot read back"
        );
    }
}

/// An empty file, a truncated file and an unknown version are all errors, and
/// none of them is a panic.
#[test]
fn a_damaged_log_file_is_an_error_never_a_panic() {
    let mut good = MatchLog::new(3);
    good.commands.push(LoggedCommand {
        tick: 1,
        schedule: CommandTick::Asap,
        fate: CommandFate::Taken,
        attribution: Attribution::SelfSigned,
        order: LoggedOrder::Place {
            faction: Faction::A,
            building: "hq".into(),
            pos: (1.0, 2.0),
        },
    });
    let text = good.to_ron().expect("a good log serializes");

    for (name, body) in [
        ("empty", String::new()),
        ("whitespace", "   \n\t\n".to_string()),
        ("truncated", text[..text.len() / 2].to_string()),
        ("byte-truncated", text[..text.len() - 1].to_string()),
        ("garbage", "not ron at all }{".to_string()),
        (
            "version",
            text.replacen(&format!("version: {LOG_FORMAT_VERSION}"), "version: 99", 1),
        ),
    ] {
        let path = scratch(&format!("damaged-{name}"));
        std::fs::write(&path, &body).unwrap();
        let got = std::panic::catch_unwind(|| MatchLog::load(&path));
        match got {
            Err(_) => panic!("loading the {name} log panicked"),
            Ok(Ok(_)) => panic!("the {name} log was accepted"),
            Ok(Err(_)) => {}
        }
    }
}

// ---- 3. tick tagging --------------------------------------------------------

fn push_at(app: &mut App, at: u32, order: Order) {
    app.world_mut().resource_mut::<CommandQueue>().0.push_at(at, order);
}

/// Tick 0 is a tick like any other: a command tagged for it applies on the very
/// first tick the sim runs, not on the second and not never.
#[test]
fn a_command_tagged_for_tick_zero_applies_on_tick_zero() {
    let mut app = sim_app_with(0, 0);
    let unit = spawn_unit(&mut app, "ripper", Faction::A, Vec2::ZERO);
    push_at(&mut app, 0, Order::MoveTo { units: vec![unit], dest: Vec2::new(50.0, 0.0) }.issued_by(Faction::A));
    step(&mut app);
    let log = app.world().resource::<CommandLog>();
    assert_eq!(log.commands().len(), 1, "the tick-0 command never applied");
    assert_eq!(log.commands()[0].tick, 0, "it was stamped with the wrong tick");
    assert_eq!(log.late(), 0, "a tick-0 command was counted late");
}

/// Commands pushed out of tick order still each apply on their own tick, and
/// on no other. The queue is not a sorted structure, so this is the probe that
/// "retained, in order" does not mean "retained, in the order I happened to
/// push them, and applied when the head of the queue says so".
#[test]
fn commands_pushed_out_of_order_each_apply_on_their_own_tick() {
    let mut app = sim_app_with(0, 0);
    let u = spawn_unit(&mut app, "ripper", Faction::A, Vec2::ZERO);
    for at in [7u32, 3, 11, 5] {
        push_at(
            &mut app,
            at,
            Order::MoveTo { units: vec![u], dest: Vec2::new(at as f32, 0.0) }.issued_by(Faction::A),
        );
    }
    tick(&mut app, 14);
    let ticks: Vec<u32> = app.world().resource::<CommandLog>().commands().iter().map(|c| c.tick).collect();
    assert_eq!(ticks, vec![3, 5, 7, 11], "commands did not land on their own ticks");
    assert_eq!(app.world().resource::<CommandLog>().late(), 0);
}

/// A command tagged far in the future waits, unobserved, and then applies on
/// exactly its tick. Nothing about the state before then may depend on it.
#[test]
fn a_far_future_command_waits_and_then_lands_on_its_tick() {
    const AT: u32 = 900;
    let mut app = sim_app_with(0, 0);
    let u = spawn_unit(&mut app, "ripper", Faction::A, Vec2::ZERO);
    push_at(&mut app, AT, Order::MoveTo { units: vec![u], dest: Vec2::new(400.0, 0.0) }.issued_by(Faction::A));
    tick(&mut app, AT);
    assert!(app.world().get::<MoveTarget>(u).is_none(), "the future command applied early");
    assert_eq!(app.world().resource::<CommandLog>().commands().len(), 0);
    step(&mut app);
    assert_eq!(app.world().resource::<CommandLog>().commands().len(), 1);
    assert_eq!(app.world().resource::<CommandLog>().commands()[0].tick, AT);
}

/// Two commands on the same tick apply in push order and are logged in push
/// order — the only order a replay can reproduce.
#[test]
fn two_commands_on_one_tick_keep_their_push_order() {
    let mut app = sim_app_with(0, 0);
    let u = spawn_unit(&mut app, "ripper", Faction::A, Vec2::ZERO);
    tick(&mut app, 3);
    for x in [10.0f32, 20.0, 30.0] {
        push_at(&mut app, 3, Order::MoveTo { units: vec![u], dest: Vec2::new(x, 0.0) }.issued_by(Faction::A));
    }
    step(&mut app);
    let log = app.world().resource::<CommandLog>();
    let xs: Vec<f32> = log
        .commands()
        .iter()
        .map(|c| match &c.order {
            LoggedOrder::MoveTo { dest, .. } => dest.0,
            _ => panic!("shape"),
        })
        .collect();
    assert_eq!(xs, vec![10.0, 20.0, 30.0], "same-tick commands were reordered");
    assert_eq!(app.world().get::<MoveTarget>(u).map(|m| m.0.x), Some(30.0), "the last command did not win");
}

/// A command whose tick has gone by is **dropped and never applied late**.
///
/// **Phase 1 (item 1c) changed what the log does with it, and I accept that
/// change.** The original form of this probe asserted the command was not
/// logged at all; that assertion was a *means*, and the end it served was the
/// worry written beside it — "a log entry the sim did not act on would replay
/// as an action the recording never took". 1c reaches the same end by the
/// opposite means: the drop is recorded, with the schedule it carried and
/// `CommandFate::Late`, and a replay re-pushes on the **recorded schedule** so
/// it is late again and applied by nobody. That is strictly more than the old
/// form gave, because a log that omitted the command would also describe a
/// queue the replay never held.
///
/// So the end is what is asserted here, end to end and through a file — and
/// with the replay proved to have actually run, since "the replay did not apply
/// it" passes vacuously if the replay refused the log outright.
#[test]
fn a_late_command_is_dropped_never_applied_and_replays_as_dropped() {
    let mut app = sim_app_with(0, 0);
    let u = spawn_unit(&mut app, "ripper", Faction::A, Vec2::ZERO);
    let v = spawn_unit(&mut app, "ripper", Faction::A, Vec2::new(0.0, 40.0));
    tick(&mut app, 10);
    // A command whose tick has gone, and — on the same tick — one that has not:
    // the fates have to be decided per command, and the order kept.
    push_at(&mut app, 2, Order::MoveTo { units: vec![u], dest: Vec2::new(1.0, 0.0) }.issued_by(Faction::A));
    push(&mut app, Order::MoveTo { units: vec![v], dest: Vec2::new(2.0, 0.0) }.issued_by(Faction::A));
    tick(&mut app, 3);

    assert!(app.world().get::<MoveTarget>(u).is_none(), "a stale command was applied");
    assert_eq!(
        app.world().get::<MoveTarget>(v).map(|m| m.0.x),
        Some(2.0),
        "the command that was in time was dropped along with the late one"
    );
    assert_eq!(app.world().resource::<CommandLog>().late(), 1);

    // Recorded: both of them, in push order, each with its own schedule and fate.
    let log = app.world().resource::<CommandLog>().log().clone();
    assert_eq!(log.commands.len(), 2, "the account of the tick is incomplete");
    assert_eq!(log.commands[0].fate, CommandFate::Late);
    assert_eq!(log.commands[0].schedule, CommandTick::At(2));
    assert!(log.commands[0].tick > 2, "the log does not say when the sim saw it");
    assert_eq!(log.commands[1].fate, CommandFate::Taken);
    assert_eq!(log.commands[1].schedule, CommandTick::Asap);
    assert_eq!(log.commands[0].tick, log.commands[1].tick, "the two were taken on one tick");

    // The account survives a file exactly.
    assert!(log.validate().is_ok(), "the sim wrote an account it calls impossible");
    let path = scratch("late-account");
    log.save(&path).expect("save");
    let back = MatchLog::load(&path).expect("load");
    assert_eq!(back.commands, log.commands, "a `Late` entry did not survive the file");

    // ...and replaying it applies the late one exactly as the recording did:
    // never — while the timely one still lands, so the replay demonstrably ran.
    let mut r = sim_app_with(0, 0);
    let u2 = spawn_unit(&mut r, "ripper", Faction::A, Vec2::ZERO);
    let v2 = spawn_unit(&mut r, "ripper", Faction::A, Vec2::new(0.0, 40.0));
    r.insert_resource(CommandLog::new(back.seed));
    r.insert_resource(ReplaySource::new(back.clone()));
    tick(&mut r, 16);
    let source = r.world().resource::<ReplaySource>();
    assert_eq!(
        source.rejection(),
        None,
        "the replay refused the log outright, so this probe proves nothing"
    );
    assert_eq!(source.cursor(), 2, "the replay never fed the commands");
    assert!(
        r.world().get::<MoveTarget>(u2).is_none(),
        "a logged late command was applied by the replay"
    );
    assert_eq!(
        r.world().get::<MoveTarget>(v2).map(|m| m.0.x),
        Some(2.0),
        "the replay dropped the command that was in time"
    );
    assert_eq!(
        r.world().resource::<CommandLog>().log().commands,
        back.commands,
        "the replay recorded a different account of the drop"
    );
    assert_eq!(r.world().resource::<CommandLog>().late(), 1, "the replay's own drop count differs");
}

// ---- 4. the state hash ------------------------------------------------------

/// **The observer must not perturb the observed.** `record_state_hash` takes
/// `&mut World` and registers queries; a run that hashes every tick has to end
/// in exactly the world a run that hashes nothing ends in.
#[test]
fn hashing_every_tick_does_not_change_the_match() {
    const TICKS: u32 = 900;
    let mut watched = ai_vs_ai(4);
    watched.insert_resource(StateHashLog::default());
    tick(&mut watched, TICKS);

    let mut unwatched = ai_vs_ai(4);
    tick(&mut unwatched, TICKS);

    assert_eq!(
        onus::sim::state_hash(watched.world_mut()),
        onus::sim::state_hash(unwatched.world_mut()),
        "recording a per-tick hash changed the match it was measuring"
    );
    assert_eq!(
        watched.world().resource::<CommandLog>().commands().len(),
        unwatched.world().resource::<CommandLog>().commands().len()
    );
}

/// `state_hash` is a function of the world: calling it twice in a row on an
/// untouched world gives the same answer.
#[test]
fn the_state_hash_is_a_pure_function_of_the_world() {
    let mut app = ai_vs_ai(4);
    tick(&mut app, 200);
    let a = onus::sim::state_hash(app.world_mut());
    let b = onus::sim::state_hash(app.world_mut());
    let c = onus::sim::state_hash(app.world_mut());
    assert_eq!((a, b), (a, c), "the hash is not a function of the world alone");
}

/// **The hash must be blind to the ECS's entity allocation.** Two identical
/// matches whose worlds differ only in how many entity ids were handed out
/// before they started must agree tick for tick — that is the whole claim
/// `SimId` was introduced to support, and the padding here (bare entities with
/// no `Position`) is a difference the sim cannot see.
#[test]
fn a_shifted_entity_allocation_does_not_change_a_single_tick_hash() {
    const TICKS: u32 = 1_500;
    let run = |pad: usize| {
        let mut app = ai_vs_ai_padded(4, pad);
        app.insert_resource(StateHashLog::default());
        tick(&mut app, TICKS);
        app.world().resource::<StateHashLog>().clone()
    };
    let plain = run(0);
    for pad in [1usize, 2, 7, 64] {
        let shifted = run(pad);
        assert_eq!(
            plain.first_divergence(&shifted),
            None,
            "padding the entity allocator with {pad} bare entities changed the \
             match's per-tick hash"
        );
    }
}

/// The hash claims to cover "every piece of state the sim owns and writes". A
/// command already accepted by the sim and waiting in the queue for a future
/// tick is such a piece of state: the sim wrote it, keeps it across ticks, and
/// reads it later. Two worlds that differ only in it must not hash the same.
#[test]
fn the_hash_observes_a_command_the_sim_is_still_holding() {
    let mut a = sim_app_with(500, 0);
    let ua = spawn_unit(&mut a, "ripper", Faction::A, Vec2::ZERO);
    let mut b = sim_app_with(500, 0);
    let _ub = spawn_unit(&mut b, "ripper", Faction::A, Vec2::ZERO);
    tick(&mut a, 5);
    tick(&mut b, 5);
    assert_eq!(
        onus::sim::state_hash(a.world_mut()),
        onus::sim::state_hash(b.world_mut()),
        "the two fixtures were not identical to begin with"
    );
    push_at(&mut a, 500, Order::MoveTo { units: vec![ua], dest: Vec2::new(9.0, 9.0) }.issued_by(Faction::A));
    step(&mut a);
    step(&mut b);
    assert_ne!(
        onus::sim::state_hash(a.world_mut()),
        onus::sim::state_hash(b.world_mut()),
        "a command the sim has accepted and is holding for a future tick is \
         invisible to the state hash: two worlds that will diverge hash equal"
    );
}

// ---- 5. the replay path -----------------------------------------------------

fn replay_of(log: MatchLog, ticks: u32) -> App {
    let mut app = ai_vs_ai(log.seed);
    app.insert_resource(StateHashLog::default());
    app.insert_resource(CommandLog::new(log.seed));
    app.insert_resource(ReplaySource::new(log));
    tick(&mut app, ticks);
    app
}

fn recorded(seed: u64, ticks: u32) -> (StateHashLog, MatchLog) {
    let mut app = ai_vs_ai(seed);
    app.insert_resource(StateHashLog::default());
    tick(&mut app, ticks);
    (
        app.world().resource::<StateHashLog>().clone(),
        app.world().resource::<CommandLog>().log().clone(),
    )
}

/// **The spec's headline probe, through the file.** Same seed, same log ⇒
/// identical per-tick hash — and the log a replay writes is byte-identical on
/// disk to the log it consumed, so a recording is a fixed point of replay.
#[test]
fn a_replay_reproduces_every_tick_and_re_records_the_identical_file() {
    const TICKS: u32 = 1_200;
    let (hashes, log) = recorded(4, TICKS);
    let path = scratch("fixedpoint-a");
    log.save(&path).expect("save");
    let loaded = MatchLog::load(&path).expect("load");
    let app = replay_of(loaded, TICKS);

    let replayed = app.world().resource::<StateHashLog>().clone();
    assert_eq!(hashes.first_divergence(&replayed), None, "the replay diverged");

    let again = app.world().resource::<CommandLog>().log().clone();
    let path2 = scratch("fixedpoint-b");
    again.save(&path2).expect("save the replay's log");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        std::fs::read_to_string(&path2).unwrap(),
        "the log a replay produced is not the log it consumed"
    );
}

/// The AI is off during a replay, and *provably* off: its journal stays empty
/// and its RNG never moves. (An AI that still thought but happened to be
/// overwritten would make the replay accidental rather than driven.)
#[test]
fn the_ai_neither_thinks_nor_rolls_a_number_during_a_replay() {
    const TICKS: u32 = 800;
    let (_, log) = recorded(4, TICKS);
    let mut app = ai_vs_ai(log.seed);
    let before: Vec<u64> = app
        .world()
        .resource::<AiCommanders>()
        .commanders()
        .iter()
        .map(|c| c.rng_state())
        .collect();
    app.insert_resource(CommandLog::new(log.seed));
    app.insert_resource(ReplaySource::new(log));
    tick(&mut app, TICKS);
    let after: Vec<u64> = app
        .world()
        .resource::<AiCommanders>()
        .commanders()
        .iter()
        .map(|c| c.rng_state())
        .collect();
    assert_eq!(before, after, "the RNG advanced during a replay");
    assert!(app.world().resource::<AiJournal>().0.is_empty(), "the AI thought during a replay");
}

/// A live order injected during a replay — at any tick, and tagged for any tick
/// including a future one — changes nothing: the replay's per-tick hashes are
/// the recording's.
#[test]
fn no_live_order_of_any_tag_can_reach_a_replay() {
    const TICKS: u32 = 900;
    let (hashes, log) = recorded(4, TICKS);
    let mut app = ai_vs_ai(log.seed);
    app.insert_resource(StateHashLog::default());
    app.insert_resource(CommandLog::new(log.seed));
    app.insert_resource(ReplaySource::new(log));
    for t in 0..TICKS {
        // A worker of A, whichever the registry gave id 2 (a real unit of the
        // recorded world): ordering it is the most damaging live order there is.
        let victim = app.world().resource::<SimIds>().entity(SimId(2));
        if let Some(v) = victim {
            let q = &mut app.world_mut().resource_mut::<CommandQueue>().0;
            q.push_back(Order::MoveTo { units: vec![v], dest: Vec2::new(0.0, 900.0) }.issued_by(Faction::A));
            q.push_at(t + 3, Order::MoveTo { units: vec![v], dest: Vec2::new(0.0, -900.0) }.issued_by(Faction::A));
        }
        step(&mut app);
    }
    let replayed = app.world().resource::<StateHashLog>().clone();
    assert_eq!(
        hashes.first_divergence(&replayed),
        None,
        "a live order injected during a replay changed the match"
    );
}

/// A logged command naming an id this world never issued is refused, counted,
/// and does not panic — and a logged command naming an id whose entity has
/// since died is likewise harmless.
#[test]
fn a_log_that_names_an_impossible_entity_neither_panics_nor_guesses() {
    const TICKS: u32 = 400;
    let (_, log) = recorded(4, TICKS);
    let mut damaged = log.clone();
    damaged.commands.push(LoggedCommand {
        tick: 10,
        schedule: CommandTick::Asap,
        fate: CommandFate::Taken,
        attribution: Attribution::By(Faction::A),
        order: LoggedOrder::MoveTo { units: vec![9_999_999], dest: (5.0, 5.0) },
    });
    damaged.commands.sort_by_key(|c| c.tick);
    let app = replay_of(damaged, TICKS);
    assert!(
        app.world().resource::<ReplaySource>().unresolved() >= 1,
        "an unissued id was silently resolved to something"
    );
}

/// A replay run past the end of the match freezes exactly as the recording
/// does: the post-match ticks hash to a constant, and to the recording's.
#[test]
fn the_ticks_after_the_match_ends_are_frozen_in_the_replay_too() {
    const BUDGET: u32 = 8 * 60 * 60;
    let mut app = ai_vs_ai(7);
    app.insert_resource(StateHashLog::default());
    let mut played = 0;
    while app.world().resource::<MatchState>().outcome().is_none() && played < BUDGET {
        step(&mut app);
        played += 1;
    }
    assert!(app.world().resource::<MatchState>().outcome().is_some(), "the match never decided");
    let extra = 120;
    tick(&mut app, extra);
    let hashes = app.world().resource::<StateHashLog>().clone();
    let log = app.world().resource::<CommandLog>().log().clone();
    let rng_after: Vec<u64> = app
        .world()
        .resource::<AiCommanders>()
        .commanders()
        .iter()
        .map(|c| c.rng_state())
        .collect();

    // Frozen: every hash after the deciding tick is the same value.
    let tail = &hashes.0[hashes.0.len() - extra as usize..];
    assert!(tail.iter().all(|h| *h == tail[0]), "the sim kept changing after the match ended");

    let app2 = replay_of(log, played + extra);
    let replayed = app2.world().resource::<StateHashLog>().clone();
    assert_eq!(hashes.first_divergence(&replayed), None, "the replay diverged over the frozen tail");
    let rng2: Vec<u64> = app2
        .world()
        .resource::<AiCommanders>()
        .commanders()
        .iter()
        .map(|c| c.rng_state())
        .collect();
    assert_ne!(rng_after, rng2, "sanity: the replay is not running the AI");
}

/// **Seeded RNG stepped only inside the sim.** With the sim chain gated off
/// (the match decided), stepping the app must not move the generator; nor may
/// anything outside `FixedUpdate` in the shipped app.
#[test]
fn the_rng_does_not_move_when_the_sim_chain_is_not_running() {
    const BUDGET: u32 = 8 * 60 * 60;
    let mut app = ai_vs_ai(7);
    let mut played = 0;
    while app.world().resource::<MatchState>().outcome().is_none() && played < BUDGET {
        step(&mut app);
        played += 1;
    }
    let rng = |app: &App| -> Vec<u64> {
        app.world().resource::<AiCommanders>().commanders().iter().map(|c| c.rng_state()).collect()
    };
    let before = rng(&app);
    tick(&mut app, 300);
    assert_eq!(before, rng(&app), "the RNG advanced after the sim chain stopped");
    // And no wall-clock: stepping the app without advancing the fixed clock is
    // still not a sim tick's worth of randomness.
    for _ in 0..50 {
        app.update();
    }
    assert_eq!(before, rng(&app), "the RNG moved on an app update with no sim tick");
}

/// Two runs of the same seed in the same process agree tick for tick — the
/// probe that no address, pointer or hash-map order reaches an outcome.
#[test]
fn the_same_seed_twice_in_one_process_is_the_same_match() {
    const TICKS: u32 = 1_500;
    for seed in [1u64, 4, 0xA11CE] {
        let (a, la) = recorded(seed, TICKS);
        let (b, lb) = recorded(seed, TICKS);
        assert_eq!(a.first_divergence(&b), None, "seed {seed} played two different matches");
        assert_eq!(la.commands, lb.commands, "seed {seed} produced two different logs");
    }
}

// ============================================================================
// Pass 2 — probes against the fixes for F1..F4.
// ============================================================================

/// Replicates the resolution rule the M4c guard uses to turn a chain entry into
/// a function body: `sim::<module>::<name>` ⇒ `src/sim/<module>.rs`,
/// `sim::<name>` ⇒ `src/sim/mod.rs`, body = from `pub fn <name>(` to the first
/// line that is a bare `}`.
fn resolve_chain_system(path: &str) -> Result<String, String> {
    let src = src_dir();
    let name = path.rsplit("::").next().unwrap_or_default();
    let module: Vec<&str> = path.split("::").collect();
    let file = if module.len() >= 3 {
        src.join(format!("sim/{}.rs", module[1]))
    } else {
        src.join("sim/mod.rs")
    };
    let text = std::fs::read_to_string(&file)
        .map_err(|_| format!("{path}: no such file as {}", file.display()))?;
    let fn_at = text
        .find(&format!("pub fn {name}("))
        .ok_or_else(|| format!("{path}: no `pub fn {name}(` in {}", file.display()))?;
    let body = &text[fn_at..];
    let end = body[1..].find("\n}\n").map(|i| i + 3).unwrap_or(body.len());
    Ok(body[..end].to_string())
}

/// Every `sim::…` system path the chain names, in source order, paired with its
/// byte offset in `src/lib.rs`.
fn chain_system_paths() -> Vec<(usize, String)> {
    let lib = std::fs::read_to_string(src_dir().join("lib.rs")).expect("src/lib.rs");
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
        // The guard's own filter: a resource/type path is upper-camel.
        if name.is_empty() || name.starts_with(|c: char| c.is_uppercase()) {
            continue;
        }
        out.push((start, path.to_string()));
    }
    out
}

fn sweep_offset() -> usize {
    std::fs::read_to_string(src_dir().join("lib.rs"))
        .expect("src/lib.rs")
        .find("sim::economy::repair_gather_claims")
        .expect("the sweep is in the chain")
}

/// **The F-008 guard must not silently skip.**
///
/// The M4c probe is now generated: it walks the chain, resolves each pre-sweep
/// entry to a function body on disk, and rejects one that names the claim. Its
/// comment claims it "requires that *no* system registered before the sweep, in
/// any file, so much as names either half of the claim". That is only true for
/// entries it can resolve — and it `continue`s past the ones it cannot, without
/// a word. Any chain entry the rule cannot resolve today is a system that, if
/// it were moved ahead of the sweep tomorrow, would be waved through.
#[test]
fn every_system_the_chain_names_resolves_to_a_body_the_guard_can_read() {
    let mut unresolved: Vec<String> = Vec::new();
    for (_, path) in chain_system_paths() {
        if let Err(e) = resolve_chain_system(&path) {
            unresolved.push(e);
        }
    }
    assert!(
        unresolved.is_empty(),
        "the F-008 guard's resolution rule cannot find these systems, and \
         skips whatever it cannot find without failing — so the guard is blind \
         to them: {unresolved:#?}"
    );
}

/// The guard has to actually examine something. A walker that resolves nothing
/// passes every input, and reads as comprehensive while checking nothing.
#[test]
fn the_generated_f008_guard_examines_at_least_one_system_and_a_whole_body() {
    let sweep = sweep_offset();
    let pre: Vec<(usize, String)> = chain_system_paths()
        .into_iter()
        .filter(|(at, _)| *at < sweep)
        .collect();
    assert!(
        !pre.is_empty(),
        "no system is registered before the sweep, so the generated guard \
         inspects nothing and would pass any change"
    );
    for (_, path) in &pre {
        let body = resolve_chain_system(path)
            .unwrap_or_else(|e| panic!("the guard cannot read a pre-sweep system: {e}"));
        assert!(
            body.trim_end().ends_with('}'),
            "`{path}`'s body was cut short by the guard's `\\n}}\\n` scan: it \
             would not see a claim read past the cut"
        );
    }
}

/// The comment that justifies moving `identify` after the sweep says "the sweep
/// is the chain's first system, so 'before the sweep' is a place nothing needs
/// to be". If that is the argument, it has to be true — and if it is not, the
/// place is not empty and the guard's one job is not the formality the comment
/// makes it sound.
#[test]
fn the_sweep_really_is_the_chains_first_system_if_the_comment_says_so() {
    let lib = std::fs::read_to_string(src_dir().join("lib.rs")).expect("src/lib.rs");
    let flat: String = lib
        .lines()
        .map(|l| l.trim().trim_start_matches("//").trim())
        .collect::<Vec<_>>()
        .join(" ");
    if !flat.contains("the sweep is the chain's first system") {
        return; // the claim was not made; nothing to hold it to.
    }
    let sweep = sweep_offset();
    let before: Vec<String> = chain_system_paths()
        .into_iter()
        .filter(|(at, _)| *at < sweep)
        .map(|(_, p)| p)
        .collect();
    assert!(
        before.is_empty(),
        "`src/lib.rs` says \"the sweep is the chain's first system\", but these \
         systems are registered before it: {before:?}"
    );
}

// ---- F1's fix: `identify` moved from the head to after the sweep -----------

/// Moving `identify` behind the sweep must not open a window. Over a whole
/// match, nothing in the world may ever be hashed, logged or read without an
/// id: the `(Position, no SimId)` population has to be empty at every tick
/// boundary, and the sim's own count of them (the hash's `UNIDENTIFIED` row)
/// has to stay zero.
#[test]
fn nothing_in_the_world_is_ever_unidentified_at_a_tick_boundary() {
    use onus::sim::replay::SimId;
    let mut app = ai_vs_ai(4);
    for t in 0..2_000u32 {
        step(&mut app);
        let n = app
            .world_mut()
            .query_filtered::<Entity, (With<Position>, Without<SimId>)>()
            .iter(app.world())
            .count();
        assert_eq!(n, 0, "tick {t} ended with {n} unidentified things in the world");
    }
}

/// Nothing may spawn between the sweep and `identify` — that is the window the
/// move created, and it is only safe while it is empty of spawners.
#[test]
fn nothing_between_the_sweep_and_identify_puts_anything_into_the_world() {
    let lib = std::fs::read_to_string(src_dir().join("lib.rs")).expect("src/lib.rs");
    let sweep = sweep_offset();
    let identify = lib.find("sim::replay::identify").expect("identify is in the chain");
    assert!(sweep < identify, "identify is no longer after the sweep");
    let between: Vec<String> = chain_system_paths()
        .into_iter()
        .filter(|(at, _)| *at > sweep && *at < identify)
        .map(|(_, p)| p)
        .collect();
    for path in &between {
        let body = resolve_chain_system(path).unwrap_or_else(|e| panic!("{e}"));
        for spawner in ["spawn(", "spawn_batch(", "spawn_empty("] {
            assert!(
                !body.contains(spawner),
                "`{path}` runs between the sweep and `identify` and spawns: its \
                 entities reach `apply_commands` (which logs by SimId) with no id"
            );
        }
    }
}

/// A command applied on the very first tick, naming an entity the fixture
/// spawned before the sim ever ran, is logged with a real id — not
/// `UNIDENTIFIED`. (`identify` is now the *second* system in the play block;
/// this is the property that move must not have cost.)
#[test]
fn a_first_tick_order_is_still_logged_against_a_real_id() {
    let mut app = sim_app_with(0, 0);
    let u = spawn_unit(&mut app, "ripper", Faction::A, Vec2::ZERO);
    push_at(
        &mut app,
        0,
        Order::MoveTo { units: vec![u], dest: Vec2::new(5.0, 0.0) }.issued_by(Faction::A),
    );
    step(&mut app);
    let log = app.world().resource::<CommandLog>();
    assert_eq!(log.commands().len(), 1);
    let ids = log.commands()[0].order.sim_ids();
    assert!(
        ids.iter().all(|i| i.is_identified()),
        "a first-tick order was logged against an unidentified entity: {ids:?}"
    );
    assert!(log.log().validate().is_ok(), "the sim wrote a log it calls invalid");
}

// ---- F3's fix: one validity predicate --------------------------------------

/// **The sim must never record a command its own log format calls invalid.**
///
/// `validate` is now the write-side check too, so a log holding an
/// `UNIDENTIFIED` id can no longer be saved at all. `apply_commands` still
/// *writes* that value for any entity it cannot resolve, so the two halves have
/// to agree: whatever the sim records must be something the sim can save.
#[test]
fn any_log_the_sim_records_is_a_log_the_sim_can_save() {
    const TICKS: u32 = 900;
    let (_, log) = recorded(4, TICKS);
    assert!(log.validate().is_ok(), "a plain recording is invalid");

    // A replay whose world has moved out from under the log — the case the
    // `unresolved`/`skipped` counters exist to report. The report is the log,
    // so the log has to survive.
    let mut app = ai_vs_ai(log.seed);
    app.insert_resource(CommandLog::new(log.seed));
    // The first command past tick 100 that names a unit, and the unit it names.
    let doomed = log
        .commands
        .iter()
        .filter(|c| c.tick > 100)
        .find_map(|c| c.order.sim_ids().first().copied().map(|id| (c.tick, id)));
    let Some((at, id)) = doomed else {
        panic!("the recording named no entity after tick 100");
    };
    app.insert_resource(ReplaySource::new(log));
    for t in 0..TICKS {
        if t + 1 == at {
            if let Some(e) = app.world().resource::<SimIds>().entity(id) {
                let _ = app.world_mut().despawn(e);
            }
        }
        step(&mut app);
    }
    let produced = app.world().resource::<CommandLog>().log().clone();
    let path = scratch("replay-of-a-moved-world");
    assert!(
        produced.save(&path).is_ok(),
        "the sim recorded a log it then refuses to write: {:?} \
         (SimId {id:?} was despawned before tick {at})",
        produced.validate().err()
    );
}

// ---- F4's fix: pending commands in the hash --------------------------------

/// A tiny match that ends on a known tick: both HQs stand for two ticks (so the
/// match is contested), then B's is removed.
fn decided_match() -> App {
    let mut app = sim_app_with(0, 0);
    spawn_building(&mut app, "hq", Faction::A, Vec2::new(-100.0, 0.0));
    let b = spawn_building(&mut app, "hq", Faction::B, Vec2::new(100.0, 0.0));
    let u = spawn_unit(&mut app, "ripper", Faction::A, Vec2::new(-90.0, 0.0));
    app.insert_resource(StateHashLog::default());
    tick(&mut app, 2);
    app.world_mut().despawn(b);
    tick(&mut app, 3);
    assert!(app.world().resource::<MatchState>().outcome().is_some(), "the fixture never decided");
    // The unit is only here so a live order has something real to name.
    app.world_mut().entity_mut(u).insert(Position(Vec2::new(-90.0, 0.0)));
    app
}

/// **A decided sim is frozen, and a click cannot un-freeze it.**
///
/// `record_state_hash` is documented as running "including the ticks after the
/// match is over, so a frozen sim is visibly frozen". Once the outcome is
/// written, `apply_commands` is gated off forever, so a command pushed after
/// that point can never be applied and can never influence any future state.
/// Hashing it makes a purely client-side event — a click on a finished match —
/// read as a state divergence, which is the one thing a desync check must not
/// invent.
#[test]
fn a_click_after_the_match_is_over_does_not_move_the_frozen_hash() {
    let mut app = decided_match();
    tick(&mut app, 5);
    let frozen = onus::sim::state_hash(app.world_mut());
    let unit = app
        .world_mut()
        .query_filtered::<Entity, With<onus::sim::UnitKind>>()
        .iter(app.world())
        .next()
        .expect("a unit to name");
    app.world_mut().resource_mut::<CommandQueue>().0.push_back(
        Order::MoveTo { units: vec![unit], dest: Vec2::new(0.0, 0.0) }.issued_by(Faction::A),
    );
    tick(&mut app, 5);
    assert_eq!(
        onus::sim::state_hash(app.world_mut()),
        frozen,
        "an order pushed after the match ended — which can never be applied — \
         changed the hash of a sim that is supposed to be frozen"
    );
    let hashes = app.world().resource::<StateHashLog>().clone();
    let tail = &hashes.0[hashes.0.len() - 8..];
    assert!(
        tail.iter().all(|h| *h == tail[0]),
        "the post-match hashes are not constant: {tail:?}"
    );
}

/// The hash's doc says it covers "the commands the queue is still holding **for
/// a future tick**". The code hashes every command in the queue, including an
/// `Asap` one, which is not held for a future tick — it is held for the next
/// drain, and in a running sim it is always drained before the hash is taken.
/// If the doc is the contract, an `Asap` command must not reach the hash.
#[test]
fn an_asap_command_is_not_a_command_held_for_a_future_tick() {
    let mut a = sim_app_with(500, 0);
    let ua = spawn_unit(&mut a, "ripper", Faction::A, Vec2::ZERO);
    let mut b = sim_app_with(500, 0);
    let _ = spawn_unit(&mut b, "ripper", Faction::A, Vec2::ZERO);
    tick(&mut a, 5);
    tick(&mut b, 5);
    assert_eq!(
        onus::sim::state_hash(a.world_mut()),
        onus::sim::state_hash(b.world_mut()),
        "the fixtures were not identical to begin with"
    );
    a.world_mut().resource_mut::<CommandQueue>().0.push_back(
        Order::MoveTo { units: vec![ua], dest: Vec2::new(1.0, 1.0) }.issued_by(Faction::A),
    );
    assert_eq!(
        onus::sim::state_hash(a.world_mut()),
        onus::sim::state_hash(b.world_mut()),
        "an `Asap` command — which the sim will drain on its next tick before \
         any hash is taken — is being hashed as though it were state held for a \
         future tick"
    );
}

/// The new rows must not have cost the properties pass 1 established: the hash
/// stays a pure function of the world, and hashing every tick still does not
/// change the match. (Re-run here against the *new* row set, over a longer run
/// than pass 1 used.)
#[test]
fn the_new_hash_rows_did_not_break_purity_or_neutrality() {
    const TICKS: u32 = 1_500;
    let mut watched = ai_vs_ai(4);
    watched.insert_resource(StateHashLog::default());
    tick(&mut watched, TICKS);
    let mut unwatched = ai_vs_ai(4);
    tick(&mut unwatched, TICKS);
    let a = onus::sim::state_hash(watched.world_mut());
    let b = onus::sim::state_hash(watched.world_mut());
    assert_eq!(a, b, "the hash is not a function of the world alone");
    assert_eq!(
        a,
        onus::sim::state_hash(unwatched.world_mut()),
        "recording a per-tick hash changed the match it was measuring"
    );
}

// ============================================================================
// Pass 3 — probes against the fixes for pass 2's F1..F4.
// ============================================================================

// ---- the registry: lazy issuance, recycling, growth ------------------------

/// Every `SimId` the registry has issued names a **different** entity. An id is
/// a coordinate; two ids for one thing means the log and the hash can disagree
/// about which coordinate that thing has.
fn registry_is_injective(app: &App) -> Result<(), String> {
    let ids = app.world().resource::<SimIds>();
    let mut seen: std::collections::BTreeMap<u64, u64> = std::collections::BTreeMap::new();
    for i in 0..ids.issued() {
        let Some(e) = ids.entity(SimId(i)) else {
            return Err(format!("id {i} was issued but resolves to nothing"));
        };
        if let Some(first) = seen.insert(e.to_bits(), i) {
            return Err(format!("{e:?} holds both SimId({first}) and SimId({i})"));
        }
    }
    Ok(())
}

/// The registry stays injective across a whole match, spawns and deaths
/// included.
#[test]
fn the_registry_never_issues_two_ids_for_one_entity() {
    let mut app = ai_vs_ai(4);
    for t in 0..2_000u32 {
        step(&mut app);
        if let Err(e) = registry_is_injective(&app) {
            panic!("tick {t}: {e}");
        }
    }
}

/// **The `to_bits` key, attacked.** The registry is now keyed on
/// `Entity::to_bits()` and never forgets, on the stated ground that generations
/// stop a recycled index colliding with the entry of the entity that used to
/// hold it.
///
/// Two things are checked over a whole match: the key is a *bijection* (every
/// id round-trips through the entity it names, and no two ids share a key), and
/// — if this Bevy ever hands the same index out twice — the second holder never
/// reaches the first's entry. If no index is ever recycled, the probe says so:
/// the hazard is then ruled out by the allocator rather than by the key.
#[test]
fn the_to_bits_key_is_a_bijection_and_survives_any_index_reuse() {
    let mut app = ai_vs_ai(4);
    let mut reused = 0usize;
    for t in 0..2_000u32 {
        step(&mut app);
        let ids = app.world().resource::<SimIds>();
        let mut by_index: std::collections::BTreeMap<String, Vec<u64>> = Default::default();
        let mut by_bits: std::collections::BTreeMap<u64, u64> = Default::default();
        for i in 0..ids.issued() {
            let e = ids.entity(SimId(i)).unwrap_or_else(|| panic!("tick {t}: id {i} names nothing"));
            assert_eq!(
                ids.id_of(e),
                Some(SimId(i)),
                "tick {t}: SimId({i}) names {e:?}, which the registry maps back to \
                 {:?} — the two halves disagree",
                ids.id_of(e)
            );
            if let Some(first) = by_bits.insert(e.to_bits(), i) {
                panic!("tick {t}: SimId({first}) and SimId({i}) share the key {e:?}");
            }
            by_index.entry(format!("{:?}", e.index())).or_default().push(i);
        }
        reused = reused.max(by_index.values().filter(|v| v.len() > 1).count());
    }
    // Whether or not the allocator recycled, the bijection above held at every
    // tick. `reused` is reported so a Bevy that starts recycling cannot make
    // this probe quietly weaker.
    println!("index reuse observed in the registry: {reused} indices");
}

/// The registry never forgets, so it grows for the life of a match. Growth
/// itself must be a function of the sim's own spawn sequence — the same match
/// played twice must issue exactly the same number of ids at every tick, or the
/// `NEXT_SIM_ID` row turns registry bookkeeping into a hash divergence.
#[test]
fn the_registry_grows_identically_in_two_runs_of_the_same_match() {
    const TICKS: u32 = 1_500;
    let trace = |seed: u64| {
        let mut app = ai_vs_ai(seed);
        let mut out = Vec::with_capacity(TICKS as usize);
        for _ in 0..TICKS {
            step(&mut app);
            out.push(app.world().resource::<SimIds>().issued());
        }
        out
    };
    assert_eq!(trace(4), trace(4), "two runs of one seed issued ids differently");
}

// ---- lazy issuance during logging ------------------------------------------

/// A world with one thing in it that is **not** a thing in the world: a bare
/// entity with no `Position`, which `identify` will never see. An order may
/// still name it, and `apply_commands` now issues it an id at log-write time.
fn world_with_a_bare_entity() -> (App, Entity, Entity) {
    let mut app = sim_app_with(5_000, 0);
    let hq = spawn_building(&mut app, "hq", Faction::A, Vec2::new(-100.0, 0.0));
    spawn_building(&mut app, "hq", Faction::B, Vec2::new(100.0, 0.0));
    spawn_unit(&mut app, "worker", Faction::A, Vec2::new(-90.0, 0.0));
    app.world_mut()
        .spawn((Position(Vec2::new(-60.0, 60.0)), ResourceNode { amount: 100_000 }));
    let bare = app.world_mut().spawn_empty().id();
    app.insert_resource(CommandLog::new(11));
    (app, hq, bare)
}

fn worker_index() -> usize {
    content().unit_index("worker").expect("worker")
}

/// **The sharpest consequence of lazy issuance.** `issued()` is a hashed row,
/// and the log is written through `id_for`, which *mutates* the registry. So an
/// order naming something the registry has not seen issues an id — and the
/// replay of that log, driven by `feed_replay`, has to arrive at the same
/// registry to reproduce the same hash. Record a match containing such an
/// order, replay it, and require every tick to agree.
#[test]
fn an_order_naming_a_thing_outside_the_world_does_not_desync_its_own_replay() {
    const TICKS: u32 = 300;
    let (mut app, hq, bare) = world_with_a_bare_entity();
    app.insert_resource(StateHashLog::default());
    for t in 0..TICKS {
        if t == 10 {
            push_at(
                &mut app,
                10,
                Order::MoveTo { units: vec![bare], dest: Vec2::new(3.0, 4.0) }
                    .issued_by(Faction::A),
            );
        }
        if t == 20 {
            push_at(
                &mut app,
                20,
                Order::Train { building: hq, unit: worker_index() }.issued_by(Faction::A),
            );
        }
        step(&mut app);
    }
    let recorded = app.world().resource::<StateHashLog>().clone();
    let log = app.world().resource::<CommandLog>().log().clone();
    assert!(log.validate().is_ok(), "the sim recorded a log it calls invalid");

    // The same starting world, rebuilt identically — bare entity and all.
    let (mut back, _, _) = world_with_a_bare_entity();
    back.insert_resource(StateHashLog::default());
    back.insert_resource(CommandLog::new(log.seed));
    back.insert_resource(ReplaySource::new(log));
    tick(&mut back, TICKS);
    let replayed = back.world().resource::<StateHashLog>().clone();
    assert_eq!(
        recorded.first_divergence(&replayed),
        None,
        "issuing an id while writing the log desynced the replay of that log \
         (unresolved: {}, issued: recorded {} vs replayed {})",
        back.world().resource::<ReplaySource>().unresolved(),
        app.world().resource::<SimIds>().issued(),
        back.world().resource::<SimIds>().issued(),
    );
}

/// The harm the desync above does, named. F-011 exists because a log keyed on
/// something that shifts "commands *the wrong units*, silently". Issuing an id
/// while writing the log shifts exactly that: a building placed after the shift
/// gets a different `SimId` in the recording than in the replay, so every later
/// command naming it names something else.
#[test]
fn a_building_placed_after_a_logged_order_gets_the_same_id_in_a_replay() {
    const TICKS: u32 = 120;
    let foundry = content().building_index("foundry").expect("foundry");
    let play = |replay: Option<MatchLog>| -> (Vec<u64>, u64, usize) {
        let (mut app, _hq, bare) = world_with_a_bare_entity();
        let live = replay.is_none();
        if let Some(log) = replay {
            app.insert_resource(CommandLog::new(log.seed));
            app.insert_resource(ReplaySource::new(log));
        }
        for t in 0..TICKS {
            if live && t == 10 {
                push_at(
                    &mut app,
                    10,
                    Order::MoveTo { units: vec![bare], dest: Vec2::new(3.0, 4.0) }
                        .issued_by(Faction::A),
                );
            }
            if live && t == 20 {
                push_at(
                    &mut app,
                    20,
                    Order::Place {
                        faction: Faction::A,
                        building: foundry,
                        pos: Vec2::new(-140.0, 40.0),
                    }
                    .issued_by(Faction::A),
                );
            }
            step(&mut app);
        }
        let mut placed: Vec<u64> = app
            .world_mut()
            .query::<(&SimId, &Building)>()
            .iter(app.world())
            .filter(|(_, b)| b.def == foundry)
            .map(|(i, _)| i.0)
            .collect();
        placed.sort_unstable();
        let n = placed.len();
        let issued = app.world().resource::<SimIds>().issued();
        let log_len = app.world().resource::<CommandLog>().commands().len();
        let _ = log_len;
        (placed, issued, n)
    };
    let (rec_ids, rec_issued, n) = play(None);
    assert_eq!(n, 1, "the fixture placed no building, so nothing is being tested");
    let log = {
        let (mut app, _hq, bare) = world_with_a_bare_entity();
        for t in 0..TICKS {
            if t == 10 {
                push_at(&mut app, 10, Order::MoveTo { units: vec![bare], dest: Vec2::new(3.0, 4.0) }.issued_by(Faction::A));
            }
            if t == 20 {
                push_at(&mut app, 20, Order::Place { faction: Faction::A, building: foundry, pos: Vec2::new(-140.0, 40.0) }.issued_by(Faction::A));
            }
            step(&mut app);
        }
        app.world().resource::<CommandLog>().log().clone()
    };
    let (rep_ids, rep_issued, rep_n) = play(Some(log));
    assert_eq!(rep_n, 1, "the replay did not place the building the log records");
    assert_eq!(
        rec_ids, rep_ids,
        "the building placed by the replay carries a different SimId than the \
         recording's ({rec_issued} ids issued in the recording, {rep_issued} in \
         the replay) — every later command naming it names something else"
    );
}

/// The registry trajectory of a faithful replay is the recording's, tick for
/// tick. `NEXT_SIM_ID` is hashed, so anything that issues an id in one run and
/// not the other is a desync of the desync detector itself.
#[test]
fn a_replay_issues_the_same_ids_at_the_same_ticks_as_the_recording() {
    const TICKS: u32 = 1_200;
    let mut rec = ai_vs_ai(4);
    let mut rec_trace = Vec::new();
    for _ in 0..TICKS {
        step(&mut rec);
        rec_trace.push(rec.world().resource::<SimIds>().issued());
    }
    let log = rec.world().resource::<CommandLog>().log().clone();

    let mut app = ai_vs_ai(log.seed);
    app.insert_resource(CommandLog::new(log.seed));
    app.insert_resource(ReplaySource::new(log));
    let mut rep_trace = Vec::new();
    for _ in 0..TICKS {
        step(&mut app);
        rep_trace.push(app.world().resource::<SimIds>().issued());
    }
    assert_eq!(rec_trace, rep_trace, "the replay's registry drifted from the recording's");
}

// ---- `SimId::UNIDENTIFIED` is unreachable from the sim ----------------------

/// **The universal claim, tested exhaustively.** `SimId::UNIDENTIFIED` "is
/// never recorded by the sim" — so throw every shape of bad order at
/// `apply_commands` and require that no logged command names it, that the log
/// validates, and that it saves.
#[test]
fn no_order_of_any_shape_can_put_unidentified_into_a_log() {
    let (mut app, hq, bare) = world_with_a_bare_entity();
    let doomed = spawn_unit(&mut app, "ripper", Faction::A, Vec2::new(-80.0, 10.0));
    let enemy = spawn_unit(&mut app, "ripper", Faction::B, Vec2::new(80.0, 10.0));
    step(&mut app);
    // A unit that is despawned before the order naming it is applied.
    app.world_mut().despawn(doomed);
    // An entity that never existed at all.
    let ghost = app.world_mut().spawn_empty().id();
    app.world_mut().despawn(ghost);

    let orders: Vec<Order> = vec![
        Order::MoveTo { units: vec![bare], dest: Vec2::ZERO }.issued_by(Faction::A),
        Order::MoveTo { units: vec![doomed], dest: Vec2::ZERO }.issued_by(Faction::A),
        Order::MoveTo { units: vec![ghost], dest: Vec2::ZERO }.issued_by(Faction::A),
        Order::MoveTo { units: vec![doomed, enemy], dest: Vec2::ZERO },
        Order::Gather { units: vec![ghost], node: bare, node_pos: Vec2::ZERO },
        Order::Train { building: ghost, unit: worker_index() },
        Order::Train { building: hq, unit: usize::MAX },
        Order::Place { faction: Faction::A, building: usize::MAX, pos: Vec2::ZERO },
    ];
    for o in orders {
        app.world_mut().resource_mut::<CommandQueue>().0.push_back(o);
    }
    // And the same again, signed by two different factions, so the queue peels
    // the signature into `Attribution::Void` — which is still recorded.
    app.world_mut().resource_mut::<CommandQueue>().0.push_back(
        Order::MoveTo { units: vec![bare], dest: Vec2::ZERO }
            .issued_by(Faction::A)
            .issued_by(Faction::B),
    );
    tick(&mut app, 3);

    let log = app.world().resource::<CommandLog>().log().clone();
    assert!(!log.commands.is_empty(), "nothing was recorded, so nothing was tested");
    for c in &log.commands {
        for id in c.order.sim_ids() {
            assert!(
                id.is_identified(),
                "the command at tick {} was logged with SimId::UNIDENTIFIED: {:?}",
                c.tick,
                c.order
            );
        }
    }
    assert!(log.validate().is_ok(), "the sim recorded a log it calls invalid: {:?}", log.validate());
    let path = scratch("every-bad-order");
    assert!(log.save(&path).is_ok(), "the sim recorded a log it cannot save");
    assert!(MatchLog::load(&path).is_ok(), "the sim saved a log it cannot load");
    assert_eq!(app.world().resource::<CommandLog>().unrecorded(), 0);
    assert!(registry_is_injective(&app).is_ok());
}

// ---- the input gate --------------------------------------------------------

/// Both order emitters are gated, and on the *sim's* run condition — not on a
/// second, drifting notion of "the match is over".
#[test]
fn both_order_emitters_are_gated_on_the_sims_own_run_condition() {
    let lib = std::fs::read_to_string(src_dir().join("lib.rs")).expect("src/lib.rs");
    for emitter in ["input::emit_commands", "input::emit_build_commands"] {
        let at = lib.find(emitter).unwrap_or_else(|| panic!("{emitter} is not registered"));
        let window = &lib[at..lib.len().min(at + 220)];
        assert!(
            window.contains("run_if(sim::victory::match_running)"),
            "`{emitter}` is not gated on the sim's `match_running`: {window}"
        );
    }
}

/// **The gate must not cost an order.** Everything the sim would have applied
/// before it was decided still applies — including on the deciding tick itself,
/// where `apply_commands` runs before `match_end` writes the outcome.
#[test]
fn an_order_landing_on_the_deciding_tick_is_still_applied() {
    let mut app = sim_app_with(0, 0);
    spawn_building(&mut app, "hq", Faction::A, Vec2::new(-100.0, 0.0));
    let b_hq = spawn_building(&mut app, "hq", Faction::B, Vec2::new(100.0, 0.0));
    let u = spawn_unit(&mut app, "ripper", Faction::A, Vec2::new(-90.0, 0.0));
    tick(&mut app, 2);
    assert!(app.world().resource::<MatchState>().engaged());
    // B's HQ is gone: the *next* tick is the deciding one.
    app.world_mut().despawn(b_hq);
    app.world_mut().resource_mut::<CommandQueue>().0.push_back(
        Order::MoveTo { units: vec![u], dest: Vec2::new(-40.0, 0.0) }.issued_by(Faction::A),
    );
    step(&mut app);
    assert!(
        app.world().resource::<MatchState>().is_over(),
        "the fixture did not decide on the tick under test"
    );
    assert!(
        app.world().get::<MoveTarget>(u).is_some(),
        "an order queued before the match was decided was dropped on the \
         deciding tick"
    );
    assert_eq!(
        app.world().resource::<CommandLog>().commands().len(),
        1,
        "the deciding tick's command is missing from the log"
    );
}

// ---- the F-008 guard, third edition ----------------------------------------

/// The new resolver's rule: search every `.rs` under `src/sim/` for
/// `pub fn <name>(`, require **exactly one**, and require a closing `}` at
/// column 0. Applied here to every `sim::…` function path the chain names — a
/// name that resolves to zero or to two is a name the guard cannot vouch for.
#[test]
fn every_chain_function_resolves_to_exactly_one_readable_definition() {
    let sim = src_dir().join("sim");
    let mut files: Vec<PathBuf> = Vec::new();
    let mut stack = vec![sim];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).expect("read src/sim") {
            let p = e.expect("entry").path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                files.push(p);
            }
        }
    }
    files.sort();
    let mut problems: Vec<String> = Vec::new();
    for (_, path) in chain_system_paths() {
        let name = path.rsplit("::").next().unwrap_or_default();
        let needle = format!("pub fn {name}(");
        let mut hits: Vec<String> = Vec::new();
        for f in &files {
            let text = std::fs::read_to_string(f).expect("read");
            let Some(at) = text.find(&needle) else { continue };
            if text[at + 1..].find("\n}\n").is_none() {
                problems.push(format!("{path}: body in {} has no `}}` at column 0", f.display()));
            }
            hits.push(f.display().to_string());
        }
        match hits.len() {
            1 => {}
            0 => problems.push(format!("{path}: no `{needle}` under src/sim")),
            n => problems.push(format!("{path}: `{needle}` defined in {n} places: {hits:?}")),
        }
    }
    assert!(problems.is_empty(), "the F-008 guard cannot resolve: {problems:#?}");
}

/// **Run conditions, however they are spelled.** The guard checks a condition
/// only when `src/lib.rs` contains the exact text `run_if(<path>)`. A condition
/// composed with `not(..)`, `.and(..)` or `.or(..)` is spelled differently and
/// would be passed over in silence — the same "skip what you cannot parse" the
/// third edition was written to remove. Every `sim::…` function named inside a
/// `run_if(..)` must be covered: either by the exact spelling, or by being
/// textually before the sweep (where the first loop catches it anyway).
#[test]
fn every_run_condition_the_chain_uses_is_actually_checked_by_the_guard() {
    let lib = std::fs::read_to_string(src_dir().join("lib.rs")).expect("src/lib.rs");
    let sweep = sweep_offset();
    let mut uncovered: Vec<String> = Vec::new();
    let mut at = 0usize;
    while let Some(i) = lib[at..].find("run_if(") {
        let start = at + i + "run_if(".len();
        // The balanced contents of the `run_if(..)` call.
        let mut depth = 1usize;
        let mut end = start;
        for (k, c) in lib[start..].char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = start + k;
                        break;
                    }
                }
                _ => {}
            }
        }
        let inner = &lib[start..end];
        at = end.max(start + 1);
        for m in inner.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == ':')) {
            if !m.starts_with("sim::") {
                continue;
            }
            let name = m.rsplit("::").next().unwrap_or_default();
            if name.is_empty() || name.starts_with(|c: char| c.is_uppercase()) {
                continue;
            }
            let exact = lib.contains(&format!("run_if({m})"));
            let pre_sweep = lib.find(m).is_some_and(|p| p < sweep);
            if !exact && !pre_sweep {
                uncovered.push(format!("{m} (inside `run_if({inner})`)"));
            }
        }
    }
    assert!(
        uncovered.is_empty(),
        "these run conditions are named by the chain but match neither the \
         guard's exact `run_if(<path>)` test nor its before-the-sweep test, so \
         the guard skips them without saying so: {uncovered:#?}"
    );
}

// ---- the properties the fixes could have broken ----------------------------

/// Pass 1 and pass 2's structural guarantees, re-established against the new
/// registry, the new hash filter and the gated input.
#[test]
fn the_pass_three_fixes_did_not_cost_the_earlier_guarantees() {
    const TICKS: u32 = 1_200;
    // Allocation independence, with the registry now keyed on `to_bits`.
    let run = |pad: usize| {
        let mut app = ai_vs_ai_padded(4, pad);
        app.insert_resource(StateHashLog::default());
        tick(&mut app, TICKS);
        app.world().resource::<StateHashLog>().clone()
    };
    let plain = run(0);
    for pad in [1usize, 3, 17] {
        assert_eq!(
            plain.first_divergence(&run(pad)),
            None,
            "padding the entity allocator with {pad} bare entities moved a hash"
        );
    }
    // Observer neutrality and purity, with the new rows.
    let mut watched = ai_vs_ai(4);
    watched.insert_resource(StateHashLog::default());
    tick(&mut watched, TICKS);
    let mut unwatched = ai_vs_ai(4);
    tick(&mut unwatched, TICKS);
    let a = onus::sim::state_hash(watched.world_mut());
    assert_eq!(a, onus::sim::state_hash(watched.world_mut()), "the hash is not pure");
    assert_eq!(
        a,
        onus::sim::state_hash(unwatched.world_mut()),
        "hashing every tick changed the match"
    );
    // A held (`At`) command is still observed; an `Asap` one still is not.
    let mut held = ai_vs_ai(4);
    tick(&mut held, 20);
    let before = onus::sim::state_hash(held.world_mut());
    let victim = held.world().resource::<SimIds>().entity(SimId(2)).expect("a thing");
    held.world_mut().resource_mut::<CommandQueue>().0.push_back(
        Order::MoveTo { units: vec![victim], dest: Vec2::ZERO }.issued_by(Faction::A),
    );
    assert_eq!(
        onus::sim::state_hash(held.world_mut()),
        before,
        "an `Asap` command is being hashed again"
    );
    held.world_mut().resource_mut::<CommandQueue>().0.push_at(
        9_000,
        Order::MoveTo { units: vec![victim], dest: Vec2::ZERO }.issued_by(Faction::A),
    );
    assert_ne!(
        onus::sim::state_hash(held.world_mut()),
        before,
        "a command held for a later tick is no longer observed"
    );
}

// ============================================================================
// Pass 4 — probes against the fix for pass 3 ("the sim commands only what it
// can name"). The fix changed what `apply_commands` *does*, so M4's order
// semantics are in scope again.
// ============================================================================

/// Two HQs (so the match is contested and never ends), a worker, a deposit, and
/// two entities that are not things in the world at all.
struct Named {
    app: App,
    hq: Entity,
    worker: Entity,
    node: Entity,
    bare: Entity,
    bare2: Entity,
}

fn nameable_fixture() -> Named {
    let mut app = sim_app_with(5_000, 0);
    let hq = spawn_building(&mut app, "hq", Faction::A, Vec2::new(-100.0, 0.0));
    spawn_building(&mut app, "hq", Faction::B, Vec2::new(100.0, 0.0));
    let worker = spawn_unit(&mut app, "worker", Faction::A, Vec2::new(-90.0, 0.0));
    let node = app
        .world_mut()
        .spawn((Position(Vec2::new(-60.0, 60.0)), ResourceNode { amount: 100_000 }))
        .id();
    let bare = app.world_mut().spawn_empty().id();
    let bare2 = app.world_mut().spawn_empty().id();
    app.insert_resource(CommandLog::new(21));
    Named { app, hq, worker, node, bare, bare2 }
}

fn alloy(app: &App, f: Faction) -> u32 {
    app.world().resource::<Stockpiles>().alloy(f)
}

fn issued(app: &App) -> u64 {
    app.world().resource::<SimIds>().issued()
}

fn push(app: &mut App, order: Order) {
    app.world_mut().resource_mut::<CommandQueue>().0.push_back(order);
}

// ---- 1. the invariant, stated generally -------------------------------------

/// **`apply_commands` never grows the registry.** This is the general form of
/// the pass-3 defect: ids are hashed, the replay path resolves read-only, so
/// any issuance on the record path is a divergence the replay cannot reproduce.
/// Throw every unnameable shape at it on a tick that spawns nothing, and the
/// id count must not move.
#[test]
fn applying_commands_never_issues_an_id() {
    let mut f = nameable_fixture();
    tick(&mut f.app, 5);
    let before = issued(&f.app);
    let orders = vec![
        Order::MoveTo { units: vec![f.bare], dest: Vec2::ZERO }.issued_by(Faction::A),
        Order::MoveTo { units: vec![f.bare, f.bare2], dest: Vec2::ZERO }.issued_by(Faction::A),
        Order::MoveTo { units: vec![f.worker, f.bare], dest: Vec2::new(-50.0, 0.0) }
            .issued_by(Faction::A),
        Order::Gather { units: vec![f.worker], node: f.bare, node_pos: Vec2::ZERO }
            .issued_by(Faction::A),
        Order::Gather { units: vec![f.bare], node: f.node, node_pos: Vec2::new(-60.0, 60.0) }
            .issued_by(Faction::A),
        Order::Train { building: f.bare, unit: worker_index() }.issued_by(Faction::A),
        Order::MoveTo { units: vec![f.bare], dest: Vec2::ZERO },
        Order::MoveTo { units: vec![f.bare], dest: Vec2::ZERO }
            .issued_by(Faction::A)
            .issued_by(Faction::B),
    ];
    for o in orders {
        push(&mut f.app, o);
    }
    tick(&mut f.app, 3);
    assert_eq!(
        issued(&f.app),
        before,
        "applying commands issued {} new id(s): naming a thing created one",
        issued(&f.app) - before
    );
    assert!(f.app.world().resource::<CommandLog>().unnameable() > 0, "nothing was refused");
    assert!(f.app.world().resource::<CommandLog>().log().validate().is_ok());
}

/// An unnameable order must be **inert**: two identical worlds, one of which is
/// handed an order naming nothing it knows, must hash the same. (This also
/// pins that the diagnostic counters are not hashed.)
#[test]
fn an_unnameable_order_changes_no_state_at_all() {
    let mut a = nameable_fixture();
    let mut b = nameable_fixture();
    tick(&mut a.app, 5);
    tick(&mut b.app, 5);
    assert_eq!(
        onus::sim::state_hash(a.app.world_mut()),
        onus::sim::state_hash(b.app.world_mut()),
        "the fixtures were not identical to begin with"
    );
    push(&mut a.app, Order::MoveTo { units: vec![a.bare, a.bare2], dest: Vec2::new(7.0, 7.0) }
        .issued_by(Faction::A));
    push(&mut a.app, Order::Train { building: a.bare, unit: worker_index() }.issued_by(Faction::A));
    push(&mut a.app, Order::Gather { units: vec![a.worker], node: a.bare, node_pos: Vec2::ZERO }
        .issued_by(Faction::A));
    tick(&mut a.app, 3);
    tick(&mut b.app, 3);
    assert_eq!(
        onus::sim::state_hash(a.app.world_mut()),
        onus::sim::state_hash(b.app.world_mut()),
        "an order the sim cannot name changed the world anyway"
    );
}

// ---- 2. the partial-drop rule, and what it costs ----------------------------

/// A list order loses its unnameable members and the rest stands — and what is
/// logged is what stood, so the replay does the same thing.
#[test]
fn a_list_order_keeps_the_names_the_sim_knows_and_drops_the_rest() {
    let mut f = nameable_fixture();
    tick(&mut f.app, 3);
    push(&mut f.app, Order::MoveTo {
        units: vec![f.bare, f.worker, f.bare2],
        dest: Vec2::new(-40.0, 0.0),
    }
    .issued_by(Faction::A));
    step(&mut f.app);
    assert!(
        f.app.world().get::<MoveTarget>(f.worker).is_some(),
        "the nameable member of the order was not obeyed"
    );
    let log = f.app.world().resource::<CommandLog>();
    assert_eq!(log.commands().len(), 1, "the order was not logged");
    match &log.commands()[0].order {
        LoggedOrder::MoveTo { units, .. } => {
            assert_eq!(units.len(), 1, "the log kept a name the sim dropped: {units:?}");
        }
        other => panic!("wrong shape: {other:?}"),
    }
    assert_eq!(log.unnameable(), 2, "the dropped names were not counted");
}

/// A list order **all** of whose members are unnameable is a no-op. Whatever
/// the sim chooses to do with it — log an empty order or refuse it — the record
/// and the replay must agree, and nothing in the world may move.
#[test]
fn an_all_unnameable_list_order_is_inert_and_replays_as_itself() {
    const TICKS: u32 = 60;
    let build = |replay: Option<MatchLog>| -> (u64, MatchLog, u64) {
        let mut f = nameable_fixture();
        let live = replay.is_none();
        if let Some(log) = replay {
            f.app.insert_resource(CommandLog::new(log.seed));
            f.app.insert_resource(ReplaySource::new(log));
        }
        for t in 0..TICKS {
            if live && t == 10 {
                push(&mut f.app, Order::MoveTo {
                    units: vec![f.bare, f.bare2],
                    dest: Vec2::new(7.0, 7.0),
                }
                .issued_by(Faction::A));
            }
            step(&mut f.app);
        }
        let h = onus::sim::state_hash(f.app.world_mut());
        let log = f.app.world().resource::<CommandLog>().log().clone();
        (h, log, issued(&f.app))
    };
    let (rec_hash, log, rec_issued) = build(None);
    assert!(log.validate().is_ok(), "the sim recorded a log it calls invalid");
    let (rep_hash, again, rep_issued) = build(Some(log.clone()));
    assert_eq!(rec_issued, rep_issued, "the registries drifted");
    assert_eq!(rec_hash, rep_hash, "an inert order desynced its own replay");
    assert_eq!(again.commands, log.commands, "the replay logged a different stream");
}

/// An order whose **single** subject is unnameable is refused whole: no half of
/// it applies. `Gather` carries a move half that reaches every unit in the
/// order, so "refused whole" has to mean that half does not land either.
#[test]
fn a_refused_single_subject_order_applies_no_half_of_itself() {
    use onus::sim::GatherTarget;
    let mut f = nameable_fixture();
    tick(&mut f.app, 3);
    push(&mut f.app, Order::Gather {
        units: vec![f.worker],
        node: f.bare,
        node_pos: Vec2::new(500.0, 500.0),
    }
    .issued_by(Faction::A));
    step(&mut f.app);
    assert!(
        f.app.world().get::<GatherTarget>(f.worker).is_none(),
        "a refused gather still claimed the worker"
    );
    assert!(
        f.app.world().get::<MoveTarget>(f.worker).is_none(),
        "a refused gather still applied its move half"
    );
    assert!(
        f.app.world().resource::<CommandLog>().commands().is_empty(),
        "an order the sim refused whole is in the log, so a replay will re-run it"
    );
}

// ---- 3. charging and side effects on the refusal path -----------------------

/// **A refused order charges nothing, and a nameable one charges exactly once.**
/// The nameability test has to sit ahead of every effect, the same way the
/// pass-3 defect was a check sitting behind one.
#[test]
fn refusal_is_free_and_acceptance_charges_exactly_once() {
    let mut f = nameable_fixture();
    tick(&mut f.app, 3);
    let start = alloy(&f.app, Faction::A);
    let queued = |app: &App, e: Entity| {
        app.world().get::<ProductionQueue>(e).map(|q| q.items.len()).unwrap_or(0)
    };
    let q0 = queued(&f.app, f.hq);

    // Refused: the building it names is not a thing in the world.
    push(&mut f.app, Order::Train { building: f.bare, unit: worker_index() }.issued_by(Faction::A));
    tick(&mut f.app, 2);
    assert_eq!(alloy(&f.app, Faction::A), start, "a refused Train charged Alloy");
    assert_eq!(queued(&f.app, f.hq), q0, "a refused Train queued a unit somewhere");
    assert!(f.app.world().resource::<CommandLog>().commands().is_empty());

    // Accepted: exactly one charge, once.
    let cost = content().units[worker_index()].mvp_alloy_cost;
    push(&mut f.app, Order::Train { building: f.hq, unit: worker_index() }.issued_by(Faction::A));
    tick(&mut f.app, 2);
    assert_eq!(alloy(&f.app, Faction::A), start - cost, "a Train did not charge exactly once");
    assert_eq!(queued(&f.app, f.hq), q0 + 1);

    // `Place` names no entity, so nameability cannot refuse it — and it still
    // charges exactly once.
    let after_train = alloy(&f.app, Faction::A);
    let foundry = content().building_index("foundry").expect("foundry");
    let bcost = content().buildings[foundry].alloy_cost;
    push(&mut f.app, Order::Place { faction: Faction::A, building: foundry, pos: Vec2::new(-140.0, 40.0) }
        .issued_by(Faction::A));
    tick(&mut f.app, 2);
    assert_eq!(alloy(&f.app, Faction::A), after_train - bcost, "Place did not charge exactly once");
}

// ---- 4. the M4 semantics the new control flow runs through ------------------

/// **F-009's precedent is unchanged.** An *unsigned* order naming two factions
/// is still refused whole (there is no commander it could have come from), and
/// a *signed* order naming another faction's unit still commands its own. The
/// new per-entity drop must not have quietly turned the first into the second.
#[test]
fn the_signing_rules_survive_the_nameability_filter() {
    let mut f = nameable_fixture();
    // Workers, not fighters: a combat unit would acquire a `MoveTarget` by
    // chasing, and this probe is about what *orders* do.
    let theirs = spawn_unit(&mut f.app, "worker", Faction::B, Vec2::new(900.0, 0.0));
    let mine = spawn_unit(&mut f.app, "worker", Faction::A, Vec2::new(-900.0, 0.0));
    tick(&mut f.app, 3);
    let target = |app: &App, e: Entity| app.world().get::<MoveTarget>(e).map(|m| m.0);

    // Unsigned, two factions: refused whole.
    let a = Vec2::new(-500.0, 0.0);
    push(&mut f.app, Order::MoveTo { units: vec![mine, theirs], dest: a });
    step(&mut f.app);
    assert_ne!(target(&f.app, mine), Some(a), "an unsigned two-faction order moved the first unit");
    assert_ne!(target(&f.app, theirs), Some(a), "an unsigned two-faction order moved the second unit");

    // The same shape with one member unnameable must not become *coherent* by
    // dropping it: A's unit plus a bare entity is still one faction, and this
    // is the case where the new filter and F-009 could disagree.
    let b = Vec2::new(-400.0, 0.0);
    push(&mut f.app, Order::MoveTo { units: vec![mine, f.bare], dest: b });
    step(&mut f.app);
    assert_eq!(
        target(&f.app, mine),
        Some(b),
        "an unsigned order over one faction plus an unnameable name was refused"
    );

    // Signed by A, two factions: commands A's own, and only that.
    let c = Vec2::new(-300.0, 0.0);
    push(&mut f.app, Order::MoveTo { units: vec![mine, theirs], dest: c }.issued_by(Faction::A));
    step(&mut f.app);
    assert_eq!(target(&f.app, mine), Some(c), "the issuer's own unit was not moved");
    assert_ne!(
        target(&f.app, theirs),
        Some(c),
        "a signed order commanded another faction's unit"
    );
}

/// A unit that has died is still **nameable** (the registry does not forget), so
/// an order naming it is logged against the right id and applied harmlessly —
/// no panic, no resurrection.
#[test]
fn ordering_a_dead_but_registered_unit_is_named_logged_and_harmless() {
    let mut f = nameable_fixture();
    tick(&mut f.app, 3);
    let id = f.app.world().resource::<SimIds>().id_of(f.worker).expect("identified");
    f.app.world_mut().despawn(f.worker);
    let before = issued(&f.app);
    push(&mut f.app, Order::MoveTo { units: vec![f.worker], dest: Vec2::ZERO }.issued_by(Faction::A));
    tick(&mut f.app, 2);
    assert_eq!(issued(&f.app), before, "naming a dead unit issued a new id");
    let log = f.app.world().resource::<CommandLog>();
    assert_eq!(log.commands().len(), 1, "an order naming a dead-but-known unit was dropped");
    assert_eq!(log.commands()[0].order.sim_ids(), vec![id], "it was logged against the wrong id");
    assert_eq!(log.unnameable(), 0, "a unit the registry knows was called unnameable");
    assert!(log.log().validate().is_ok());
}

// ---- 5. the absent-registry path --------------------------------------------

/// Nameability is enforced only where the registry exists, so **no shipped app
/// may be able to run without it.** The one definition of the chain installs it
/// (F-004); this is the probe that it cannot be composed away.
#[test]
fn the_one_chain_definition_always_installs_the_registry() {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .insert_resource(Time::<Fixed>::from_hz(60.0))
        .insert_resource(content())
        .init_resource::<CommandQueue>()
        .init_resource::<RateReport>()
        .init_resource::<Casualties>()
        .insert_resource(Stockpiles::starting(0));
    onus::add_sim_systems(&mut app, Update);
    assert!(
        app.world().get_resource::<SimIds>().is_some(),
        "`add_sim_systems` did not install the registry, so a shipped tick can \
         take the unchecked path"
    );
    assert!(app.world().get_resource::<CommandLog>().is_some());
    step(&mut app);
    assert!(app.world().get_resource::<SimIds>().is_some());
    // And `identify` re-creates it rather than leaving the chain unchecked.
    app.world_mut().remove_resource::<SimIds>();
    step(&mut app);
    assert!(
        app.world().get_resource::<SimIds>().is_some(),
        "a tick ran with no registry and did not restore one"
    );
}

// ---- 6. the structural guard, attacked --------------------------------------

/// The shipped guard checks that `assign` has one call site. The property it is
/// standing in for is larger: **nothing outside `assign` may grow the
/// registry.** A new method that pushed to `slots` directly would satisfy the
/// shipped guard and reintroduce exactly the pass-3 defect.
#[test]
fn nothing_but_assign_touches_the_registrys_storage() {
    let replay = std::fs::read_to_string(src_dir().join("sim/replay.rs")).expect("replay.rs");
    let at = replay.find("fn assign(").expect("assign exists");
    let end = at + replay[at..].find("\n    }\n").map(|i| i + 6).expect("assign has a body");
    let mut offenders: Vec<String> = Vec::new();
    for (i, line) in replay.lines().enumerate() {
        let code = line.split("//").next().unwrap_or("");
        let mutates = code.contains("slots.push")
            || code.contains("slots.insert")
            || code.contains("slots.remove")
            || code.contains("slots.clear")
            || code.contains("by_entity.insert")
            || code.contains("by_entity.remove")
            || code.contains("by_entity.clear");
        if !mutates {
            continue;
        }
        let offset: usize = replay.lines().take(i).map(|l| l.len() + 1).sum();
        if offset < at || offset > end {
            offenders.push(format!("replay.rs:{}: {}", i + 1, code.trim()));
        }
    }
    assert!(
        offenders.is_empty(),
        "the registry's storage is mutated outside `assign`: {offenders:#?}"
    );
    // And no on-demand issuance has come back under any name, anywhere in src/.
    let mut stack = vec![src_dir()];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).expect("read src") {
            let p = e.expect("entry").path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                let text = std::fs::read_to_string(&p).expect("read");
                assert!(
                    !text.contains("id_for"),
                    "`id_for` is back in {}",
                    p.display()
                );
            }
        }
    }
}

// ---- 7. the invariant under a match that does everything --------------------

/// **The general property, under load.** A match containing every shape the fix
/// touches — partial drops, whole refusals, an all-unnameable order, real
/// places and trains and gathers, and an unnameable order on the tick the match
/// is decided — must replay tick for tick from its own persisted log, with the
/// same registry at every tick and the same log written back out.
#[test]
fn a_match_full_of_unnameable_orders_still_replays_exactly() {
    const TICKS: u32 = 400;
    let foundry = content().building_index("foundry").expect("foundry");
    let script = |f: &mut Named, t: u32| {
        let (bare, bare2, worker, node, hq) = (f.bare, f.bare2, f.worker, f.node, f.hq);
        match t {
            10 => push(&mut f.app, Order::MoveTo { units: vec![worker, bare], dest: Vec2::new(-40.0, 0.0) }.issued_by(Faction::A)),
            15 => push(&mut f.app, Order::Gather { units: vec![worker], node: bare, node_pos: Vec2::new(9.0, 9.0) }.issued_by(Faction::A)),
            20 => push(&mut f.app, Order::Place { faction: Faction::A, building: foundry, pos: Vec2::new(-140.0, 40.0) }.issued_by(Faction::A)),
            25 => push(&mut f.app, Order::MoveTo { units: vec![bare, bare2], dest: Vec2::new(7.0, 7.0) }.issued_by(Faction::A)),
            30 => push(&mut f.app, Order::Train { building: bare2, unit: worker_index() }.issued_by(Faction::A)),
            35 => push(&mut f.app, Order::Train { building: hq, unit: worker_index() }.issued_by(Faction::A)),
            40 => push(&mut f.app, Order::Gather { units: vec![worker, bare], node, node_pos: Vec2::new(-60.0, 60.0) }.issued_by(Faction::A)),
            45 => push(&mut f.app, Order::MoveTo { units: vec![bare], dest: Vec2::ZERO }),
            50 => push(&mut f.app, Order::Gather { units: vec![bare], node: bare2, node_pos: Vec2::ZERO }.issued_by(Faction::A)),
            _ => {}
        }
    };
    // Record.
    let mut f = nameable_fixture();
    f.app.insert_resource(StateHashLog::default());
    let mut rec_ids = Vec::new();
    for t in 0..TICKS {
        script(&mut f, t);
        step(&mut f.app);
        rec_ids.push(issued(&f.app));
    }
    let recorded = f.app.world().resource::<StateHashLog>().clone();
    let log = f.app.world().resource::<CommandLog>().log().clone();
    assert!(
        f.app.world().resource::<CommandLog>().unnameable() >= 5,
        "the script did not actually exercise the refusal paths"
    );
    let path = scratch("nameable-match");
    log.save(&path).expect("the sim must be able to save its own log");
    let loaded = MatchLog::load(&path).expect("load");

    // Replay, through the file.
    let mut g = nameable_fixture();
    g.app.insert_resource(StateHashLog::default());
    g.app.insert_resource(CommandLog::new(loaded.seed));
    g.app.insert_resource(ReplaySource::new(loaded));
    let mut rep_ids = Vec::new();
    for _ in 0..TICKS {
        step(&mut g.app);
        rep_ids.push(issued(&g.app));
    }
    let replayed = g.app.world().resource::<StateHashLog>().clone();
    assert_eq!(rec_ids, rep_ids, "the registries drifted apart");
    assert_eq!(
        recorded.first_divergence(&replayed),
        None,
        "a match containing unnameable orders did not replay (unresolved {}, \
         skipped {})",
        g.app.world().resource::<ReplaySource>().unresolved(),
        g.app.world().resource::<ReplaySource>().skipped(),
    );
    let again = g.app.world().resource::<CommandLog>().log().clone();
    let path2 = scratch("nameable-match-again");
    again.save(&path2).expect("save the replay's log");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        std::fs::read_to_string(&path2).unwrap(),
        "the replay wrote a different log than it consumed"
    );
    assert_eq!(
        onus::sim::state_hash(f.app.world_mut()),
        onus::sim::state_hash(g.app.world_mut()),
        "the replayed world is not the recorded world"
    );
}

/// An unnameable order landing on the **deciding tick** — the tick after which
/// the chain is gated off forever — must not disturb the verdict or the frozen
/// tail.
#[test]
fn an_unnameable_order_on_the_deciding_tick_changes_nothing() {
    let mut f = nameable_fixture();
    f.app.insert_resource(StateHashLog::default());
    tick(&mut f.app, 3);
    let b_hq = f
        .app
        .world_mut()
        .query_filtered::<Entity, (With<Building>, With<Faction>)>()
        .iter(f.app.world())
        .find(|e| f.app.world().get::<Faction>(*e) == Some(&Faction::B))
        .expect("B's HQ");
    f.app.world_mut().despawn(b_hq);
    push(&mut f.app, Order::MoveTo { units: vec![f.bare, f.bare2], dest: Vec2::ZERO }
        .issued_by(Faction::A));
    push(&mut f.app, Order::Train { building: f.bare, unit: worker_index() }.issued_by(Faction::A));
    let before = issued(&f.app);
    step(&mut f.app);
    let outcome = f.app.world().resource::<MatchState>().outcome().expect("decided");
    assert_eq!(outcome.winner, Some(Faction::A), "the verdict moved");
    assert_eq!(issued(&f.app), before, "the deciding tick issued an id");
    tick(&mut f.app, 20);
    let h = f.app.world().resource::<StateHashLog>().clone();
    let tail = &h.0[h.0.len() - 15..];
    assert!(tail.iter().all(|x| *x == tail[0]), "the frozen tail is not frozen: {tail:?}");
}

/// **Logging can never change what the sim does.** The nameability decision is
/// claimed to come from the registry alone. Remove the log and the same orders
/// must meet the same fate — otherwise "what the sim did" would depend on
/// whether anybody was writing it down, which is the shape of the pass-3 defect
/// (the record path doing something the replay path does not).
#[test]
fn whether_a_log_exists_does_not_change_what_the_sim_obeys() {
    use onus::sim::GatherTarget;
    let outcome = |with_log: bool| -> (bool, bool, u32, u64) {
        let mut f = nameable_fixture();
        if !with_log {
            f.app.world_mut().remove_resource::<CommandLog>();
        }
        tick(&mut f.app, 3);
        let alloy_before = alloy(&f.app, Faction::A);
        push(&mut f.app, Order::MoveTo { units: vec![f.worker, f.bare], dest: Vec2::new(-40.0, 0.0) }
            .issued_by(Faction::A));
        push(&mut f.app, Order::Gather { units: vec![f.worker], node: f.bare, node_pos: Vec2::ZERO }
            .issued_by(Faction::A));
        push(&mut f.app, Order::Train { building: f.bare, unit: worker_index() }.issued_by(Faction::A));
        tick(&mut f.app, 2);
        (
            f.app.world().get::<MoveTarget>(f.worker).is_some(),
            f.app.world().get::<GatherTarget>(f.worker).is_some(),
            alloy_before - alloy(&f.app, Faction::A),
            issued(&f.app),
        )
    };
    assert_eq!(
        outcome(true),
        outcome(false),
        "the sim obeyed a different set of orders depending on whether a log \
         was present"
    );
}

/// The claim that `unnameable()` is "zero for anything a shipped producer
/// emits": everything input and the AI name is in the world, and everything in
/// the world is identified. Run a real AI-vs-AI match and hold it to that.
#[test]
fn a_shipped_match_never_names_anything_it_cannot_name() {
    let mut app = ai_vs_ai(4);
    tick(&mut app, 2_000);
    let log = app.world().resource::<CommandLog>();
    assert_eq!(log.unnameable(), 0, "the AI named something the sim could not name");
    assert_eq!(log.unrecorded(), 0, "a shipped chain failed to record a command");
    assert_eq!(log.late(), 0, "a shipped chain applied a command late");
    assert!(log.log().validate().is_ok());
}

// ============================================================================
// Pass 5 — probes against Phase 1 (log format v2: content fingerprint, content
// named by id, recorded schedule and fate).
// ============================================================================

fn content_dir_with(name: &str, units_ron: String) -> ScratchPath {
    let dir = std::env::temp_dir().join(format!(
        "onus-critic-p1-{name}-{}-{}",
        std::process::id(),
        scratch_serial()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch content dir");
    std::fs::write(dir.join("units.ron"), units_ron).expect("write units.ron");
    std::fs::write(
        dir.join("resources.ron"),
        std::fs::read_to_string(data_dir().join("resources.ron")).expect("read resources.ron"),
    )
    .expect("write resources.ron");
    ScratchPath(dir)
}

/// A content directory built from the shipped one with `units.ron` edited.
fn content_from_edited_units(name: &str, edit: impl Fn(String) -> String) -> ScratchPath {
    let text = std::fs::read_to_string(data_dir().join("units.ron")).expect("units.ron");
    content_dir_with(name, edit(text))
}

/// The shipped roster with **one building id present twice** — the second copy
/// differing only in its Alloy cost. This is what a designer's copy-paste
/// produces; what the loader does with it is the question below.
fn duplicate_building_id_content() -> Result<Content, onus::sim::content::ContentError> {
    let dir = content_from_edited_units("dupe-building", |text| {
        let original = text
            .lines()
            .find(|l| l.contains("id: \"foundry\""))
            .expect("the foundry line")
            .to_string();
        let twin = original.replace("alloy_cost: 150", "alloy_cost: 999");
        assert_ne!(twin, original, "the twin is not actually different");
        text.replacen(&original, &format!("{original}\n{twin}"), 1)
    });
    Content::load_from_dir(&dir)
}

/// **The coordinate 1b replaced an index with has to be injective.**
///
/// A log names content by id and a replay resolves that id back to an index
/// with `Content::building_index` / `unit_index`, which return the *first*
/// match. So the round trip `index -> id -> index` is the identity only while
/// ids are unique. F-011 retired `Entity::to_bits()` because it was a
/// coordinate that could mean something else later; an id two definitions share
/// is a coordinate that means something else *now*.
///
/// Asserted over every content that can be loaded, not over one — including a
/// reordered roster and one where a unit and a building deliberately share an
/// id, which the namespace decision says must stay legal.
#[test]
fn every_content_id_resolves_back_to_the_definition_it_came_from() {
    let cross = Content::load_from_dir(&content_dir_with(
        "cross-namespace",
        std::fs::read_to_string(data_dir().join("units.ron")).expect("units.ron"),
    ));
    let mut sets: Vec<(String, Content)> = vec![("the shipped content".into(), content())];
    if let Ok(c) = cross {
        sets.push(("a reload of it".into(), c));
    }
    if let Ok(c) = shared_id_across_namespaces() {
        sets.push(("a unit and a resource sharing an id".into(), c));
    }
    assert!(sets.len() >= 2, "the probe is only exercising one content set");
    for (what, c) in sets {
        for (i, b) in c.buildings.iter().enumerate() {
            assert_eq!(
                c.building_index(&b.id),
                Some(i),
                "{what}: building {i} (`{}`) does not resolve back to itself, so a \
                 log naming it replays as a different building",
                b.id
            );
        }
        for (i, u) in c.units.iter().enumerate() {
            assert_eq!(
                c.unit_index(&u.id),
                Some(i),
                "{what}: unit {i} (`{}`) does not resolve back to itself",
                u.id
            );
        }
    }
}

/// ...and the only thing that could break it must be **unloadable**, in every
/// namespace an id is resolved in, and however many copies there are. This is
/// where the round-trip property is actually bought: not by care, but by
/// content that would break it not existing.
#[test]
fn content_that_would_break_the_round_trip_cannot_be_loaded() {
    // (a) buildings.
    let err = duplicate_building_id_content()
        .expect_err("content with a duplicated building id must not load")
        .to_string();
    assert!(
        err.contains("foundry") && err.to_lowercase().contains("duplicate"),
        "the refusal does not identify the duplicate: {err}"
    );

    // (b) units — the other list the log names. Renaming one unit onto
    // another's id is the same ambiguity a copy-paste makes, with no
    // structural editing of the RON.
    let dir = content_from_edited_units("dupe-unit", |text| {
        text.replacen("id: \"ravager\",", "id: \"ripper\",", 1)
    });
    let err = Content::load_from_dir(&dir)
        .expect_err("content with a duplicated unit id must not load")
        .to_string();
    assert!(
        err.contains("ripper") && err.to_lowercase().contains("duplicate"),
        "a duplicated unit id was not refused by name: {err}"
    );

    // (c) resources — named by `economy.currency`.
    let rtext = std::fs::read_to_string(data_dir().join("resources.ron")).expect("resources.ron");
    let rdir = content_dir_with("dupe-resource", std::fs::read_to_string(data_dir().join("units.ron")).unwrap());
    std::fs::write(
        rdir.join("resources.ron"),
        rtext.replacen(
            "id: \"biomass\"",
            "id: \"alloy\"",
            1,
        ),
    )
    .unwrap();
    let err = Content::load_from_dir(&rdir)
        .expect_err("content with a duplicated resource id must not load")
        .to_string();
    assert!(
        err.contains("alloy") && err.to_lowercase().contains("duplicate"),
        "a duplicated resource id was not refused by name: {err}"
    );

    // (d) the *third* copy is refused too — a check that only compares
    // neighbours, or only the first pair, would let this through.
    let dir3 = content_from_edited_units("dupe-triple", |text| {
        let original = text
            .lines()
            .find(|l| l.contains("id: \"gene_vats\""))
            .expect("the gene_vats line")
            .to_string();
        text.replacen(&original, &format!("{original}\n{original}\n{original}"), 1)
    });
    assert!(
        Content::load_from_dir(&dir3).is_err(),
        "three copies of one building id loaded"
    );
}

/// The refusal is only a guarantee if nothing can make a `Content` without it.
/// `Content`'s fields are public, so this is a structural check that `src/`
/// only ever builds one through `load_from_dir`, which validates.
#[test]
fn nothing_in_src_builds_a_content_that_skipped_validation() {
    let content_rs = std::fs::read_to_string(src_dir().join("sim/content.rs")).expect("content.rs");
    let at = content_rs
        .find("pub fn load_from_dir(")
        .expect("load_from_dir exists");
    let body_end = at + content_rs[at..].find("\n    }\n").expect("it has a body");
    assert!(
        content_rs[at..body_end].contains("content.validate()?"),
        "`load_from_dir` no longer validates what it loads"
    );
    let mut offenders: Vec<String> = Vec::new();
    let mut stack = vec![src_dir()];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).expect("read src") {
            let p = e.expect("entry").path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            if p.extension().is_some_and(|x| x == "rs") {
                let text = std::fs::read_to_string(&p).expect("read");
                for (i, line) in text.lines().enumerate() {
                    let code = line.split("//").next().unwrap_or("");
                    // The one literal is the one inside `load_from_dir`.
                    let is_literal = code.contains("Content {")
                        && !code.contains("struct ")
                        && !code.contains("impl ")
                        && !code.contains("fn ");
                    if is_literal {
                        let offset: usize = text.lines().take(i).map(|l| l.len() + 1).sum();
                        let inside = p.ends_with("content.rs") && offset > at && offset < body_end;
                        if !inside {
                            offenders.push(format!("{}:{}", p.display(), i + 1));
                        }
                    }
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "a `Content` is built outside `load_from_dir`, so it never met the \
         duplicate-id check: {offenders:#?}"
    );
}

/// The namespace decision, tested where it costs something: a **resource** and
/// a **unit** sharing an id is legal, loads, and replays — because every
/// reference site says which kind it means.
fn shared_id_across_namespaces() -> Result<Content, onus::sim::content::ContentError> {
    let dir = content_dir_with(
        "shared-id",
        std::fs::read_to_string(data_dir().join("units.ron")).expect("units.ron"),
    );
    let rtext = std::fs::read_to_string(data_dir().join("resources.ron")).expect("resources.ron");
    // `aether` is named by nothing the sim resolves (it is not the currency),
    // so renaming it to a unit id is a pure cross-namespace collision.
    std::fs::write(
        dir.join("resources.ron"),
        rtext.replacen("id: \"aether\"", "id: \"worker\"", 1),
    )
    .unwrap();
    Content::load_from_dir(&dir)
}

#[test]
fn an_id_shared_across_two_namespaces_is_legal_and_replays() {
    let c = shared_id_across_namespaces()
        .expect("a unit and a resource may share an id — the namespace decision");
    assert!(c.unit_index("worker").is_some());
    assert!(c.resources.iter().any(|r| r.id == "worker"));
    // The two namespaces stay distinct: the unit id does not resolve to a
    // building, and the roster round trip still holds.
    assert!(c.building_index("worker").is_none());
    for (i, u) in c.units.iter().enumerate() {
        assert_eq!(c.unit_index(&u.id), Some(i));
    }
    // And a match played on it records and replays.
    let (rec, log, rec_h) = play_a_place(&c, c.buildings.len() - 1, None);
    let (rep, again, rep_h) = play_a_place(&c, c.buildings.len() - 1, Some(log.clone()));
    assert_eq!(rec, rep, "a cross-namespace id changed what the log meant");
    assert_eq!(rec_h, rep_h);
    assert_eq!(again.commands, log.commands);
}

/// Record/replay a single `Place` of `def` against `c`, returning the Alloy
/// spent, the log, and the final state hash.
fn play_a_place(c: &Content, def: usize, replay: Option<MatchLog>) -> (u32, MatchLog, u64) {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .insert_resource(Time::<Fixed>::from_hz(60.0))
        .insert_resource(c.clone())
        .init_resource::<CommandQueue>()
        .init_resource::<RateReport>()
        .init_resource::<Casualties>()
        .insert_resource(Stockpiles::starting(5_000));
    onus::add_sim_systems(&mut app, Update);
    let hq = c.building_index("hq").expect("hq");
    for (f, x) in [(Faction::A, -100.0f32), (Faction::B, 100.0)] {
        app.world_mut().spawn((
            Position(Vec2::new(x, 0.0)),
            Building { def: hq },
            f,
            ProductionQueue::default(),
        ));
    }
    let live = replay.is_none();
    match replay {
        Some(log) => {
            app.insert_resource(CommandLog::new(log.seed));
            app.insert_resource(ReplaySource::new(log));
        }
        None => {
            app.insert_resource(CommandLog::new(5));
        }
    }
    for t in 0..40u32 {
        if live && t == 10 {
            app.world_mut().resource_mut::<CommandQueue>().0.push_back(
                Order::Place { faction: Faction::A, building: def, pos: Vec2::new(-200.0, 0.0) }
                    .issued_by(Faction::A),
            );
        }
        step(&mut app);
    }
    if let Some(src) = app.world().get_resource::<ReplaySource>() {
        assert_eq!(src.rejection(), None, "the replay was refused, so nothing is proved");
        assert!(src.cursor() > 0, "the replay fed nothing");
    }
    let built = app
        .world_mut()
        .query::<&Building>()
        .iter(app.world())
        .filter(|b| b.def == def)
        .count();
    assert_eq!(built, 1, "the building was never placed, so the probe is vacuous");
    let spent = 5_000 - app.world().resource::<Stockpiles>().alloy(Faction::A);
    let log = app.world().resource::<CommandLog>().log().clone();
    let h = onus::sim::state_hash(app.world_mut());
    (spent, log, h)
}

/// The round trip on the definition a first-match lookup would never reach if
/// ids were ambiguous: the **last** building in RON order. Recorded, replayed
/// and re-recorded, it must still mean that building and charge its price.
#[test]
fn a_log_naming_the_last_building_definition_replays_as_that_definition() {
    let c = content();
    let last = c.buildings.len() - 1;
    assert!(last > 0, "there is only one building to name");
    let cost = c.buildings[last].alloy_cost;
    assert!(cost > 0, "the fixture cannot tell a charge from a refusal");

    let (rec_spent, log, rec_hash) = play_a_place(&c, last, None);
    assert_eq!(rec_spent, cost, "the recording did not charge that building");
    assert_eq!(log.commands.len(), 1, "the order was not logged");
    assert!(log.validate().is_ok(), "the recording is not a valid log");
    assert!(
        log.matches_content(&c).is_ok(),
        "the fingerprint refused the very content the match was played with"
    );
    let (rep_spent, again, rep_hash) = play_a_place(&c, last, Some(log.clone()));
    assert_eq!(
        rec_spent, rep_spent,
        "the replay charged {rep_spent} where the recording charged {rec_spent}: \
         the logged id resolved to a different definition"
    );
    assert_eq!(rec_hash, rep_hash, "the replayed world is not the recorded world");
    assert_eq!(again.commands, log.commands, "the replay logged a different stream");
}

/// Every shipped content file still loads, and the stricter validator did not
/// cost any fixture the rest of the suite depends on.
#[test]
fn the_shipped_content_still_loads_under_the_stricter_validator() {
    let c = Content::load_from_dir(&data_dir()).expect("the shipped content must still load");
    assert!(!c.units.is_empty() && !c.buildings.is_empty() && !c.resources.is_empty());
    assert!(c.fingerprint().is_known());
}

// ---- 1a: the fingerprint --------------------------------------------------

/// **The fingerprint must cover every field, and must go on covering them.**
///
/// `Content::fingerprint`'s doc says "every field of every definition". The
/// shipped test spot-checks five fields, which cannot notice a *new* field
/// going unhashed — and a field the fingerprint does not see is a false accept,
/// which is the silent wrong replay the whole item exists to prevent. This is
/// the standing guard: every field name of every content struct must appear in
/// the body of `fingerprint()`.
#[test]
fn the_fingerprint_reads_every_field_of_every_content_struct() {
    let src = std::fs::read_to_string(src_dir().join("sim/content.rs")).expect("content.rs");
    let body_at = src.find("pub fn fingerprint(").expect("fingerprint exists");
    let body_end = body_at
        + src[body_at..]
            .find("\n    }\n")
            .expect("fingerprint has a body");
    let body = &src[body_at..body_end];

    // Field names of the content structs, read off the source.
    let structs = [
        "Cost", "UnitDef", "BuildingDef", "ArmyItem", "AiDef", "NemesisBonus", "CombatDef",
        "ResourceDef", "EconomyDef", "Content",
    ];
    let mut missing: Vec<String> = Vec::new();
    for name in structs {
        let at = src
            .find(&format!("pub struct {name} {{"))
            .unwrap_or_else(|| panic!("`pub struct {name}` not found"));
        let end = at + src[at..].find("\n}\n").expect("struct has an end");
        for line in src[at..end].lines() {
            let code = line.split("//").next().unwrap_or("").trim();
            let Some(field) = code.strip_prefix("pub ") else { continue };
            let Some(field) = field.split(':').next() else { continue };
            let field = field.trim();
            if field.is_empty() || !field.chars().all(|c| c.is_alphanumeric() || c == '_') {
                continue;
            }
            if !body.contains(field) {
                missing.push(format!("{name}.{field}"));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "`Content::fingerprint` says it covers every field, but never reads \
         these — an edit to one of them would leave a stale log looking valid: \
         {missing:#?}"
    );
}

/// The sim stamps its own logs, on every path, including a match that is over
/// before it starts. An unstamped log is one `load_for` refuses, so the
/// producer has to be incapable of writing one.
#[test]
fn every_log_the_shipped_chain_writes_is_stamped_and_loadable() {
    // (a) an ordinary match.
    let mut app = ai_vs_ai(4);
    tick(&mut app, 200);
    let log = app.world().resource::<CommandLog>().log().clone();
    assert!(log.content.is_known(), "an ordinary match wrote an unstamped log");
    assert!(log.matches_content(&content()).is_ok());

    // (b) a match decided on its very first tick: the play block never runs
    // past tick 0, and the stamp must still happen.
    let mut over = sim_app_with(0, 0);
    spawn_building(&mut over, "hq", Faction::A, Vec2::new(-100.0, 0.0));
    let b = spawn_building(&mut over, "hq", Faction::B, Vec2::new(100.0, 0.0));
    step(&mut over);
    over.world_mut().despawn(b);
    step(&mut over);
    assert!(over.world().resource::<MatchState>().is_over(), "the fixture did not decide");
    let short = over.world().resource::<CommandLog>().log().clone();
    assert!(
        short.content.is_known(),
        "a match that ended immediately wrote an unstamped log, which `load_for` refuses"
    );

    // (c) and the stamp keeps working after the match is over.
    tick(&mut over, 20);
    assert!(over.world().resource::<CommandLog>().log().content.is_known());

    // (d) a log the sim wrote always survives the front door, through a file.
    let path = scratch("stamped");
    log.save(&path).expect("save");
    MatchLog::load_for(&path, &content()).expect("the sim wrote a log its own front door refuses");
}

/// The fingerprint has to describe the content the sim is **actually running
/// on**. `stamp_content` stamps once and never re-stamps, and its doc argues
/// that is safe because "the first stamp is the one the recorded commands were
/// taken under". Commands recorded after a mid-match content change were not.
#[test]
fn the_stamp_describes_the_content_every_recorded_command_was_taken_under() {
    let a = content();
    let b = Content::load_from_dir(&content_dir_with(
        "swapped",
        std::fs::read_to_string(data_dir().join("units.ron"))
            .expect("units.ron")
            .replacen("mvp_alloy_cost: 10,", "mvp_alloy_cost: 11,", 1),
    ))
    .expect("the edited content loads");
    assert_ne!(a.fingerprint().hash(), b.fingerprint().hash(), "the fixture changed nothing");

    let mut app = sim_app_with(5_000, 0);
    let hq = spawn_building(&mut app, "hq", Faction::A, Vec2::new(-100.0, 0.0));
    spawn_building(&mut app, "hq", Faction::B, Vec2::new(100.0, 0.0));
    app.insert_resource(CommandLog::new(3));
    tick(&mut app, 3);
    // The content the sim runs on changes; the stamp does not.
    app.insert_resource(b.clone());
    push(&mut app, Order::Train { building: hq, unit: worker_index() }.issued_by(Faction::A));
    tick(&mut app, 3);

    let log = app.world().resource::<CommandLog>().log().clone();
    assert!(!log.commands.is_empty(), "nothing was recorded after the swap");
    let ran_under = app.world().resource::<Content>().fingerprint();
    assert_eq!(
        log.content.hash(),
        ran_under.hash(),
        "the log is stamped with content the recorded commands were *not* taken \
         under: it says [{}] and the sim was running [{}]. A replay against the \
         stamped content is accepted and plays a different match",
        log.content,
        ran_under
    );
}

// ---- 1b: content named by id, and the producer that must agree -------------

/// The content-index refusal has to sit where the nameability check sits:
/// ahead of every charge and every effect. An order naming a definition this
/// build does not have charges nothing, queues nothing, places nothing, and is
/// not logged — in the record path and the replay path alike.
#[test]
fn an_order_naming_content_this_build_lacks_is_free_and_unlogged() {
    let mut f = nameable_fixture();
    tick(&mut f.app, 3);
    let start = alloy(&f.app, Faction::A);
    let buildings = f.app.world().resource::<Content>().buildings.len();
    let units = f.app.world().resource::<Content>().units.len();
    let before_ids = issued(&f.app);
    let queued = |app: &App, e: Entity| {
        app.world().get::<ProductionQueue>(e).map(|q| q.items.len()).unwrap_or(0)
    };
    let q0 = queued(&f.app, f.hq);
    let placed = |app: &mut App| app.world_mut().query::<&Building>().iter(app.world()).count();
    let p0 = placed(&mut f.app);

    for o in [
        Order::Place { faction: Faction::A, building: buildings, pos: Vec2::new(-140.0, 40.0) }
            .issued_by(Faction::A),
        Order::Place { faction: Faction::A, building: usize::MAX, pos: Vec2::new(-160.0, 40.0) }
            .issued_by(Faction::A),
        Order::Train { building: f.hq, unit: units }.issued_by(Faction::A),
        Order::Train { building: f.hq, unit: usize::MAX }.issued_by(Faction::A),
    ] {
        push(&mut f.app, o);
    }
    tick(&mut f.app, 3);

    assert_eq!(alloy(&f.app, Faction::A), start, "an unknown definition was charged for");
    assert_eq!(queued(&f.app, f.hq), q0, "an unknown unit was queued");
    assert_eq!(placed(&mut f.app), p0, "an unknown building was placed");
    assert_eq!(issued(&f.app), before_ids, "refusing an unknown definition issued an id");
    let log = f.app.world().resource::<CommandLog>();
    assert!(
        log.commands().is_empty(),
        "an order the sim refused is in the log, so a replay will re-run it: {:?}",
        log.commands()
    );
    assert!(log.unnameable() >= 4, "the refusals were not counted");
    assert!(log.log().validate().is_ok());
    assert!(log.log().matches_content(&content()).is_ok());
}

// ---- 1c: the schedule and the fate ------------------------------------------

/// `validate` must accept exactly the `(schedule, fate)` pairs the sim can
/// produce — no more (or it admits a log describing a run that never happened)
/// and no fewer (or the sim writes logs its own validator refuses, which is
/// pass-2's F3 all over again).
#[test]
fn the_schedule_and_fate_the_validator_accepts_are_the_ones_the_sim_produces() {
    let entry = |tick: u32, schedule: CommandTick, fate: CommandFate| {
        let mut log = MatchLog::new(1);
        log.content = content().fingerprint();
        log.commands.push(LoggedCommand {
            tick,
            schedule,
            fate,
            attribution: Attribution::By(Faction::A),
            order: LoggedOrder::MoveTo { units: vec![0], dest: (1.0, 2.0) },
        });
        log
    };
    // The three the sim can produce.
    for (t, s, f) in [
        (5u32, CommandTick::Asap, CommandFate::Taken),
        (5, CommandTick::At(5), CommandFate::Taken),
        (5, CommandTick::At(2), CommandFate::Late),
    ] {
        assert!(entry(t, s, f).validate().is_ok(), "the sim's own ({s:?}, {f:?}) is refused");
    }
    // Everything else describes a run the sim cannot have had.
    for (t, s, f) in [
        (5u32, CommandTick::Asap, CommandFate::Late),
        (5, CommandTick::At(5), CommandFate::Late),
        (5, CommandTick::At(2), CommandFate::Taken),
        (5, CommandTick::At(9), CommandFate::Taken),
        (5, CommandTick::At(9), CommandFate::Late),
        (0, CommandTick::At(0), CommandFate::Late),
    ] {
        assert!(
            entry(t, s, f).validate().is_err(),
            "({s:?}, {f:?}) at tick {t} was accepted, but no sim run produces it"
        );
    }
}

/// A whole match with late commands, real commands and content orders mixed —
/// recorded, saved, loaded through the **front door**, replayed: every tick's
/// hash, the registry, and the log file must all come back identical.
#[test]
fn a_match_with_late_commands_replays_byte_for_byte() {
    const TICKS: u32 = 300;
    let foundry = content().building_index("foundry").expect("foundry");
    let script = |f: &mut Named, t: u32| {
        let (worker, node, hq, bare) = (f.worker, f.node, f.hq, f.bare);
        match t {
            10 => f.app.world_mut().resource_mut::<CommandQueue>().0.push_at(
                3,
                Order::MoveTo { units: vec![worker], dest: Vec2::new(-10.0, 0.0) }
                    .issued_by(Faction::A),
            ),
            11 => push(f_app(f), Order::Place { faction: Faction::A, building: foundry, pos: Vec2::new(-140.0, 40.0) }.issued_by(Faction::A)),
            20 => push(f_app(f), Order::Train { building: hq, unit: worker_index() }.issued_by(Faction::A)),
            25 => f.app.world_mut().resource_mut::<CommandQueue>().0.push_at(
                1,
                Order::Gather { units: vec![worker], node, node_pos: Vec2::new(-60.0, 60.0) }
                    .issued_by(Faction::A),
            ),
            30 => push(f_app(f), Order::Gather { units: vec![worker], node, node_pos: Vec2::new(-60.0, 60.0) }.issued_by(Faction::A)),
            35 => push(f_app(f), Order::MoveTo { units: vec![worker, bare], dest: Vec2::new(-30.0, 0.0) }.issued_by(Faction::A)),
            _ => {}
        }
    };
    let mut f = nameable_fixture();
    f.app.insert_resource(StateHashLog::default());
    let mut rec_ids = Vec::new();
    for t in 0..TICKS {
        script(&mut f, t);
        step(&mut f.app);
        rec_ids.push(issued(&f.app));
    }
    let recorded = f.app.world().resource::<StateHashLog>().clone();
    let log = f.app.world().resource::<CommandLog>().log().clone();
    assert!(
        log.commands.iter().any(|c| c.fate == CommandFate::Late),
        "the script produced no late command, so nothing is being tested"
    );
    assert!(log.commands.iter().any(|c| c.fate == CommandFate::Taken));
    let path = scratch("late-match");
    log.save(&path).expect("save");
    let loaded = MatchLog::load_for(&path, &content()).expect("the front door refused the sim's own log");

    let mut g = nameable_fixture();
    g.app.insert_resource(StateHashLog::default());
    g.app.insert_resource(CommandLog::new(loaded.seed));
    g.app.insert_resource(ReplaySource::new(loaded));
    let mut rep_ids = Vec::new();
    for _ in 0..TICKS {
        step(&mut g.app);
        rep_ids.push(issued(&g.app));
    }
    let source = g.app.world().resource::<ReplaySource>();
    assert_eq!(source.rejection(), None, "the replay was refused, so this proves nothing");
    assert!(source.cursor() > 0, "the replay fed nothing");
    assert_eq!(rec_ids, rep_ids, "the registries drifted");
    assert_eq!(
        recorded.first_divergence(&g.app.world().resource::<StateHashLog>().clone()),
        None,
        "a match containing late commands did not replay"
    );
    let again = g.app.world().resource::<CommandLog>().log().clone();
    let path2 = scratch("late-match-again");
    again.save(&path2).expect("save");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        std::fs::read_to_string(&path2).unwrap(),
        "the replay wrote a different log than it consumed"
    );
    assert_eq!(
        f.app.world().resource::<CommandLog>().late(),
        g.app.world().resource::<CommandLog>().late(),
        "the replay dropped a different number of commands than the recording"
    );
}

fn f_app(f: &mut Named) -> &mut App {
    &mut f.app
}

// ---- the version bump -------------------------------------------------------

/// A version-1 log is refused **by name**, and never partially deserialized
/// into a v2 struct. The three items are one format: all of them are absent
/// from a v1 log, so there is nothing to half-read.
#[test]
fn a_version_one_log_is_refused_and_never_half_read() {
    assert_eq!(LOG_FORMAT_VERSION, 2, "the bump did not happen");
    // A v1 log, spelled exactly as the old format wrote it: no `content`, no
    // `schedule`, no `fate`, and content named by index.
    let v1 = r#"(
    version: 1,
    seed: 7,
    commands: [
        (
            tick: 4,
            attribution: By(A),
            order: Place(
                faction: A,
                building: 0,
                pos: (1.0, 2.0),
            ),
        ),
    ],
)"#;
    let path = scratch("v1");
    std::fs::write(&path, v1).unwrap();
    for got in [MatchLog::from_ron(v1), MatchLog::load(&path)] {
        let err = got.expect_err("a version-1 log was accepted by a build that writes version 2");
        assert!(
            err.contains('1') && (err.contains("version") || err.contains("parse")),
            "a v1 log was refused, but not in terms a human can act on: {err}"
        );
    }
    assert!(MatchLog::load_for(&path, &content()).is_err());
    // And a v2 log with the version rewritten to 1 is refused too, so the
    // refusal is the version check and not an accident of the old shape.
    let mut ok = MatchLog::new(7);
    ok.content = content().fingerprint();
    let text = ok.to_ron().expect("serialize");
    let downgraded = text.replacen(&format!("version: {LOG_FORMAT_VERSION}"), "version: 1", 1);
    assert!(MatchLog::from_ron(&downgraded).is_err(), "a v1-labelled log was accepted");
}

/// An unstamped log is refused by the front door but still **readable** — the
/// diagnostic path the design promises, checked rather than assumed.
#[test]
fn a_refused_log_is_still_readable_for_diagnosis() {
    let mut unstamped = MatchLog::new(4);
    unstamped.commands.push(LoggedCommand {
        tick: 1,
        schedule: CommandTick::Asap,
        fate: CommandFate::Taken,
        attribution: Attribution::By(Faction::A),
        order: LoggedOrder::Place { faction: Faction::A, building: "hq".into(), pos: (0.0, 0.0) },
    });
    assert!(!unstamped.content.is_known());
    let path = scratch("unstamped");
    unstamped.save(&path).expect("an unstamped log still writes");
    let back = MatchLog::load(&path).expect("...and still reads, for diagnosis");
    assert_eq!(back.commands, unstamped.commands);
    assert!(back.matches_content(&content()).is_err(), "an unstamped log was accepted to play");
    assert!(MatchLog::load_for(&path, &content()).is_err());
}

// ============================================================================
// Pass 6 — probes against the fixes for pass 5's F1 and F2.
// ============================================================================

/// The `Content` the shipped roster loads to, with one value changed.
fn content_with_a_changed_stat() -> Content {
    Content::load_from_dir(&content_from_edited_units("changed-stat", |t| {
        t.replacen("mvp_alloy_cost: 10,", "mvp_alloy_cost: 11,", 1)
    }))
    .expect("the edited content loads")
}

/// **The escape clause, tested.** F2's producer can build a log its own
/// validator rejects (`content_changed`). That is only permissible if the
/// refusal is loud, at the write, and if no content exists the log could
/// correctly replay against — otherwise it is M5 pass-2's F3 again, where a
/// legitimate match became unsaveable and the report was lost.
#[test]
fn a_poisoned_log_is_refused_loudly_at_every_boundary_and_matches_nothing() {
    let a = content();
    let b = content_with_a_changed_stat();
    assert_ne!(a.fingerprint().hash(), b.fingerprint().hash());

    let mut app = sim_app_with(5_000, 0);
    let hq = spawn_building(&mut app, "hq", Faction::A, Vec2::new(-100.0, 0.0));
    spawn_building(&mut app, "hq", Faction::B, Vec2::new(100.0, 0.0));
    app.insert_resource(CommandLog::new(3));
    push(&mut app, Order::Train { building: hq, unit: worker_index() }.issued_by(Faction::A));
    tick(&mut app, 3);
    assert!(!app.world().resource::<CommandLog>().log().content_changed, "poisoned already");
    app.insert_resource(b.clone());
    push(&mut app, Order::Train { building: hq, unit: worker_index() }.issued_by(Faction::A));
    tick(&mut app, 3);

    let log = app.world().resource::<CommandLog>().log().clone();
    assert!(log.content_changed, "the mid-match content change was not recorded");
    assert!(log.commands.len() >= 2, "the fixture recorded nothing on either side");

    // Loud, at the write, and the message says what happened.
    let err = log.to_ron().expect_err("a poisoned log was serialized");
    assert!(err.to_lowercase().contains("content"), "the refusal is not diagnosable: {err}");
    let path = scratch("poisoned");
    let _ = std::fs::remove_file(&path);
    assert!(log.save(&path).is_err(), "a poisoned log was written");
    assert!(!path.exists(), "a log that cannot be read back was still written to disk");

    // And it matches *no* content — including both of the two it was played
    // under, which is what makes refusing it a loss of nothing.
    for (what, c) in [("the first", &a), ("the second", &b)] {
        assert!(
            log.matches_content(c).is_err(),
            "a poisoned log was accepted against {what} content it was played under"
        );
    }
    assert!(log.validate().is_err());
}

/// A replay driven from a poisoned in-memory log is refused by the backstop,
/// not played — the same guarantee as a mismatched fingerprint.
#[test]
fn a_poisoned_log_cannot_be_replayed_even_in_memory() {
    let b = content_with_a_changed_stat();
    let mut app = sim_app_with(5_000, 0);
    let hq = spawn_building(&mut app, "hq", Faction::A, Vec2::new(-100.0, 0.0));
    spawn_building(&mut app, "hq", Faction::B, Vec2::new(100.0, 0.0));
    app.insert_resource(CommandLog::new(3));
    tick(&mut app, 2);
    app.insert_resource(b);
    push(&mut app, Order::Train { building: hq, unit: worker_index() }.issued_by(Faction::A));
    tick(&mut app, 3);
    let log = app.world().resource::<CommandLog>().log().clone();
    assert!(log.content_changed);

    let mut r = sim_app_with(5_000, 0);
    spawn_building(&mut r, "hq", Faction::A, Vec2::new(-100.0, 0.0));
    spawn_building(&mut r, "hq", Faction::B, Vec2::new(100.0, 0.0));
    r.insert_resource(CommandLog::new(log.seed));
    r.insert_resource(ReplaySource::new(log));
    tick(&mut r, 10);
    let src = r.world().resource::<ReplaySource>();
    assert!(src.rejection().is_some(), "a poisoned log was replayed");
    assert_eq!(src.cursor(), 0, "a poisoned log fed commands anyway");
}

/// **The poison must only fire on a real change.** The stamp is recomputed
/// whenever Bevy says the `Content` resource changed, so a mutable borrow that
/// alters nothing, or a byte-identical reload, must leave the log clean — or
/// every legitimate match becomes unsaveable, which is the failure mode this
/// escape clause is borrowing against.
#[test]
fn touching_the_content_without_changing_it_does_not_poison_the_log() {
    // The fingerprint is a function of the content, not of the load.
    assert_eq!(content().fingerprint(), content().fingerprint(), "the fingerprint is unstable");

    let mut app = sim_app_with(5_000, 0);
    let hq = spawn_building(&mut app, "hq", Faction::A, Vec2::new(-100.0, 0.0));
    spawn_building(&mut app, "hq", Faction::B, Vec2::new(100.0, 0.0));
    app.insert_resource(CommandLog::new(3));
    tick(&mut app, 3);
    for _ in 0..5 {
        // A mutable borrow that changes nothing...
        let _ = app.world_mut().resource_mut::<Content>();
        tick(&mut app, 2);
        // ...and a byte-identical reinsertion.
        app.insert_resource(content());
        push(&mut app, Order::Train { building: hq, unit: worker_index() }.issued_by(Faction::A));
        tick(&mut app, 2);
    }
    let log = app.world().resource::<CommandLog>().log().clone();
    assert!(
        !log.content_changed,
        "touching the content without changing it marked the log as describing none"
    );
    assert!(log.validate().is_ok(), "a clean match produced an unwritable log");
    assert!(log.matches_content(&content()).is_ok());
    assert!(!log.commands.is_empty(), "nothing was recorded, so nothing is tested");
}

/// The "recompute only when unstamped or `Content` changed" shortcut must not
/// miss a log: a fresh `CommandLog` installed mid-match, long after the content
/// last changed, still gets stamped.
#[test]
fn a_log_installed_mid_match_is_still_stamped() {
    let mut app = ai_vs_ai(4);
    tick(&mut app, 50);
    assert!(app.world().resource::<CommandLog>().log().content.is_known());
    // A fresh, unstamped log, with the content untouched since tick 0.
    app.insert_resource(CommandLog::new(4));
    assert!(!app.world().resource::<CommandLog>().log().content.is_known());
    step(&mut app);
    let log = app.world().resource::<CommandLog>().log().clone();
    assert!(
        log.content.is_known(),
        "a log installed after the content last changed was never stamped, so \
         `load_for` would refuse it"
    );
    assert!(!log.content_changed, "a fresh log was marked as spanning a change");
    assert!(log.matches_content(&content()).is_ok());
}

/// In a shipped match the poison is dead code, which is the other half of what
/// makes the escape clause safe: refusing to write a poisoned log costs a real
/// match nothing, because a real match never produces one.
#[test]
fn a_shipped_match_never_poisons_its_log() {
    let mut app = ai_vs_ai(4);
    tick(&mut app, 2_000);
    let log = app.world().resource::<CommandLog>().log().clone();
    assert!(!log.content_changed, "an ordinary AI match marked its own log poisoned");
    assert!(log.content.is_known());
    assert!(log.validate().is_ok());
    let path = scratch("shipped-clean");
    log.save(&path).expect("a shipped match must be able to save its log");
    MatchLog::load_for(&path, &content()).expect("...and load it back through the front door");
}

/// `content_changed` is `#[serde(default)]`, so a v2 log written before the
/// field existed still loads — and loads as *not* poisoned, since nothing can
/// write a poisoned one to disk.
#[test]
fn a_v2_log_without_the_poison_field_still_loads_as_clean() {
    let mut ok = MatchLog::new(11);
    ok.content = content().fingerprint();
    ok.commands.push(LoggedCommand {
        tick: 2,
        schedule: CommandTick::Asap,
        fate: CommandFate::Taken,
        attribution: Attribution::By(Faction::A),
        order: LoggedOrder::Place { faction: Faction::A, building: "hq".into(), pos: (1.0, 2.0) },
    });
    let text = ok.to_ron().expect("serialize");
    // Strip the field, as a log written by the build before F2 would not have it.
    let stripped: String = text
        .lines()
        .filter(|l| !l.contains("content_changed"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!stripped.contains("content_changed"));
    let back = MatchLog::from_ron(&stripped).expect("a v2 log without the field must still load");
    assert!(!back.content_changed);
    assert_eq!(back.commands, ok.commands);
    assert!(back.matches_content(&content()).is_ok());
}
