//! Critic probes for M5 (deterministic replay). **Critic-owned**: the
//! implementer may not edit this file.
//!
//! Every probe here is written against the M5 spec and the project invariants,
//! not against the implementation's own framing.

use std::path::PathBuf;

use bevy::prelude::*;

use onus::sim::ai::AiCommanders;
use onus::sim::combat::{Casualties, Health};
use onus::sim::content::Content;
use onus::sim::economy::{Building, ProductionQueue, Stockpiles, UnitDefIdx};
use onus::sim::replay::{LoggedCommand, LoggedOrder, MatchLog, SimId, SimIds};
use onus::sim::spatial::Faction;
use onus::sim::{
    AiJournal, Attribution, CommandLog, CommandQueue, MatchState, MoveTarget, Order, Position,
    RateReport, ReplaySource, ResourceNode, StateHashLog,
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

fn scratch(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("onus-critic-m5-{name}-{}.ron", std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
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
        attribution: Attribution::SelfSigned,
        order: LoggedOrder::Place { faction: Faction::A, building: 0, pos: (1.0, 2.0) },
    });
    let text = good.to_ron().expect("a good log serializes");

    for (name, body) in [
        ("empty", String::new()),
        ("whitespace", "   \n\t\n".to_string()),
        ("truncated", text[..text.len() / 2].to_string()),
        ("byte-truncated", text[..text.len() - 1].to_string()),
        ("garbage", "not ron at all }{".to_string()),
        ("version", text.replacen("version: 1", "version: 99", 1)),
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

/// A command whose tick has gone by is dropped, never applied late, and never
/// written into the log (a log entry the sim did not act on would replay as an
/// action the recording never took).
#[test]
fn a_late_command_is_dropped_and_is_not_written_into_the_log() {
    let mut app = sim_app_with(0, 0);
    let u = spawn_unit(&mut app, "ripper", Faction::A, Vec2::ZERO);
    tick(&mut app, 10);
    push_at(&mut app, 2, Order::MoveTo { units: vec![u], dest: Vec2::new(1.0, 0.0) }.issued_by(Faction::A));
    tick(&mut app, 3);
    assert!(app.world().get::<MoveTarget>(u).is_none(), "a stale command was applied");
    assert!(app.world().resource::<CommandLog>().commands().is_empty(), "a command that never applied is in the log");
    assert_eq!(app.world().resource::<CommandLog>().late(), 1);
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
    use onus::sim::replay::SimId;
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
    use onus::sim::replay::SimId;
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
