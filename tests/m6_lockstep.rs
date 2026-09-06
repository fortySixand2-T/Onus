//! L2 integration tests for M6 (lockstep networking).
//!
//! Two `App`s in one process, connected by a **real TCP socket pair on an
//! ephemeral loopback port** — never a fixed port, and every socket closes with
//! the test. In-process is not the whole proof (the critic asks for
//! cross-process determinism, and `tests/m6_cross_process.rs` supplies it), but
//! it is what makes the protocol testable tick by tick.
//!
//! Both peers are driven through the shipped installers, `onus::add_sim_systems`
//! and `onus::net::add_net_link` — no hand-rolled system list (F-004).

use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;

use bevy::prelude::*;

use onus::net::{Frame, NetConfig, NetLink, PROTOCOL_VERSION};
use onus::sim::combat::{Casualties, Health};
use onus::sim::content::Content;
use onus::sim::economy::{Building, ProductionQueue, Stockpiles, UnitDefIdx};
use onus::sim::replay::{CommandLog, SimIds, StateHashLog};
use onus::sim::spatial::Faction;
use onus::sim::{
    CommandQueue, MatchState, MoveTarget, Order, Position, RateReport, SimId, TickGate,
};

// ---- harness ----------------------------------------------------------------

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data")
}

fn content() -> Content {
    Content::load_from_dir(&data_dir()).expect("assets/data/*.ron")
}

/// A connected pair of streams on an ephemeral loopback port. The listener is
/// dropped with the function, so nothing is left bound.
fn socket_pair() -> (TcpStream, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    let client = TcpStream::connect(addr).expect("connect");
    let (server, _) = listener.accept().expect("accept");
    (server, client)
}

fn test_config() -> NetConfig {
    NetConfig {
        turn_delay_ticks: 3,
        hash_interval_ticks: 5,
        stall_timeout_secs: 30.0,
    }
}

/// One peer: the shipped sim chain, the shipped lockstep link, and a world both
/// sides spawn identically.
fn peer(stream: TcpStream, me: Faction, seed: u64, config: NetConfig) -> App {
    let c = content();
    let alloy = c.economy.starting_alloy;
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .insert_resource(Time::<Fixed>::from_hz(60.0))
        .insert_resource(c)
        .init_resource::<CommandQueue>()
        .init_resource::<RateReport>()
        .init_resource::<Casualties>()
        .insert_resource(Stockpiles::starting(alloy))
        .insert_resource(CommandLog::new(seed));
    onus::add_sim_systems(&mut app, Update);
    let link = NetLink::from_stream(stream, me, config).expect("link");
    onus::net::add_net_link(&mut app, Update, link);
    // The same starting world on both sides — a lockstep match assumes it, and
    // the handshake's content check is what makes assuming it safe.
    for (faction, base) in [
        (Faction::A, Vec2::new(-400.0, 0.0)),
        (Faction::B, Vec2::new(400.0, 0.0)),
    ] {
        let (def, hp) = {
            let c = app.world().resource::<Content>();
            let def = c.building_index("hq").expect("hq");
            (def, Health::from_building_def(c, def))
        };
        app.world_mut().spawn((
            Position(base),
            Building { def },
            faction,
            ProductionQueue::default(),
            hp,
        ));
        for i in 0..2 {
            let (idx, kind, hp) = {
                let c = app.world().resource::<Content>();
                let idx = c.unit_index("ripper").expect("ripper");
                (idx, c.units[idx].mvp_kind, Health::from_def(c, idx))
            };
            app.world_mut().spawn((
                Position(base + Vec2::new(0.0, 40.0 * i as f32)),
                UnitDefIdx(idx),
                kind,
                faction,
                hp,
            ));
        }
    }
    app
}

/// Two peers on one socket pair, ready to be stepped together.
fn pair_of_peers(seed: u64, config: NetConfig) -> (App, App) {
    let (a, b) = socket_pair();
    (
        peer(a, Faction::A, seed, config),
        peer(b, Faction::B, seed, config),
    )
}

fn step(app: &mut App) {
    let dt = app.world().resource::<Time<Fixed>>().timestep();
    app.world_mut().resource_mut::<Time<Fixed>>().advance_by(dt);
    app.update();
}

/// Step both peers, alternating, the way two processes interleave.
fn step_both(a: &mut App, b: &mut App, n: u32) {
    for _ in 0..n {
        step(a);
        step(b);
    }
}

fn tick_of(app: &App) -> u32 {
    app.world().resource::<MatchState>().tick()
}

fn link_of(app: &App) -> &NetLink {
    app.world().resource::<NetLink>()
}

fn hashes_of(app: &App) -> StateHashLog {
    app.world().resource::<StateHashLog>().clone()
}

/// A unit of `faction`'s, by ascending `SimId` — the same unit on both peers,
/// which is the whole point of `SimId` existing.
fn unit_of(app: &mut App, faction: Faction) -> (Entity, SimId) {
    let mut q = app
        .world_mut()
        .query::<(Entity, &SimId, &Faction, &UnitDefIdx)>();
    let mut all: Vec<(Entity, SimId)> = q
        .iter(app.world())
        .filter(|(_, _, f, _)| **f == faction)
        .map(|(e, id, _, _)| (e, *id))
        .collect();
    all.sort_by_key(|(_, id)| id.0);
    *all.first().expect("a unit of that faction")
}

// ---- AC1: commands exchanged between two clients ----------------------------

/// **The handshake first.** Two peers agree on protocol, seed, content and who
/// commands which side before a single tick is played — and only then does
/// either sim advance.
#[test]
fn two_peers_shake_hands_before_either_sim_advances() {
    let (mut a, mut b) = pair_of_peers(7, test_config());
    // Before anything is pumped, neither knows the other exists...
    assert!(!link_of(&a).is_ready());
    assert_eq!(tick_of(&a), 0);

    step_both(&mut a, &mut b, 4);

    assert!(link_of(&a).is_ready(), "A never completed the handshake");
    assert!(link_of(&b).is_ready(), "B never completed the handshake");
    assert_eq!(link_of(&a).peer(), Some(Faction::B));
    assert_eq!(link_of(&b).peer(), Some(Faction::A));
    assert!(link_of(&a).failure().is_none());
    // ...and the sims did advance once it was done, or the gate is stuck shut
    // and every other test here would pass for the wrong reason.
    assert!(tick_of(&a) > 0, "A never ticked after the handshake");
    assert!(tick_of(&b) > 0, "B never ticked after the handshake");
}

/// **AC1.** A command issued on one peer is applied on **both**, on the same
/// tick, naming the same unit.
#[test]
fn a_command_issued_on_one_peer_is_applied_on_both() {
    let (mut a, mut b) = pair_of_peers(7, test_config());
    step_both(&mut a, &mut b, 6);

    // A commands one of its own units.
    let (unit_a, id) = unit_of(&mut a, Faction::A);
    a.world_mut().resource_mut::<CommandQueue>().0.push_back(
        Order::MoveTo {
            units: vec![unit_a],
            dest: Vec2::new(-100.0, 250.0),
        }
        .issued_by(Faction::A),
    );
    // It must not take effect immediately: the other peer has never heard of it.
    step(&mut a);
    assert!(
        a.world().get::<MoveTarget>(unit_a).is_none(),
        "a local command was applied before the peer could have it"
    );

    step_both(&mut a, &mut b, 10);

    // Both peers moved the same unit — B resolves the `SimId` to its own entity.
    let (unit_b, id_b) = unit_of(&mut b, Faction::A);
    assert_eq!(id, id_b, "the two peers disagree about which unit that is");
    let dest_a = a.world().get::<MoveTarget>(unit_a).map(|m| m.0);
    let dest_b = b.world().get::<MoveTarget>(unit_b).map(|m| m.0);
    assert_eq!(
        dest_a,
        Some(Vec2::new(-100.0, 250.0)),
        "the issuing peer never applied its own command"
    );
    assert_eq!(dest_b, dest_a, "the peer applied a different command");
    // And both logs record it on the same tick, with the same schedule.
    let entry = |app: &App| {
        let log = app.world().resource::<CommandLog>();
        log.commands()
            .iter()
            .map(|c| (c.tick, c.schedule, c.fate, c.order.clone()))
            .collect::<Vec<_>>()
    };
    assert!(!entry(&a).is_empty(), "nothing was logged at all");
    assert_eq!(
        entry(&a),
        entry(&b),
        "the two peers recorded different accounts of the same match"
    );
}

/// Commands from **both** sides, interleaved, still land identically — and the
/// per-tick hashes agree the whole way, which is determinism in one process.
#[test]
fn commands_from_both_peers_produce_one_identical_match() {
    let (mut a, mut b) = pair_of_peers(11, test_config());
    step_both(&mut a, &mut b, 6);
    for round in 0..8u32 {
        let (ua, _) = unit_of(&mut a, Faction::A);
        a.world_mut().resource_mut::<CommandQueue>().0.push_back(
            Order::MoveTo {
                units: vec![ua],
                dest: Vec2::new(-300.0 + round as f32 * 10.0, 100.0),
            }
            .issued_by(Faction::A),
        );
        let (ub, _) = unit_of(&mut b, Faction::B);
        b.world_mut().resource_mut::<CommandQueue>().0.push_back(
            Order::MoveTo {
                units: vec![ub],
                dest: Vec2::new(300.0 - round as f32 * 10.0, -100.0),
            }
            .issued_by(Faction::B),
        );
        step_both(&mut a, &mut b, 12);
    }
    let (ha, hb) = (hashes_of(&a), hashes_of(&b));
    let common = ha.0.len().min(hb.0.len());
    assert!(common > 80, "the peers barely ticked ({common})");
    assert_eq!(
        ha.0[..common],
        hb.0[..common],
        "two peers playing the same commands ended up in different worlds"
    );
    assert!(
        a.world().resource::<CommandLog>().commands().len() >= 16,
        "the fixture issued nothing, so this proves nothing"
    );
    assert_eq!(
        a.world().resource::<CommandLog>().commands().len(),
        b.world().resource::<CommandLog>().commands().len()
    );
}

/// A pair of peers with a **seeded commander on each side** and a world worth
/// playing: an HQ, a deposit and workers each. The commander is the sim's only
/// randomness, so this is what makes a fixture depend on its seed at all.
fn pair_of_ai_peers(seed: u64, config: NetConfig) -> (App, App) {
    let (sa, sb) = socket_pair();
    let build = |stream: TcpStream, me: Faction| {
        let mut app = peer(stream, me, seed, config);
        // The bare `peer` world has no economy; give both sides one, identically.
        for (faction, base) in [
            (Faction::A, Vec2::new(-750.0, 0.0)),
            (Faction::B, Vec2::new(750.0, 0.0)),
        ] {
            app.world_mut().spawn((
                Position(base + Vec2::new(0.0, 250.0)),
                onus::sim::ResourceNode { amount: 100_000 },
            ));
            for i in 0..3 {
                let (idx, kind, hp) = {
                    let c = app.world().resource::<Content>();
                    let idx = c.unit_index("worker").expect("worker");
                    (idx, c.units[idx].mvp_kind, Health::from_def(c, idx))
                };
                app.world_mut().spawn((
                    Position(base + Vec2::new(30.0, 20.0 * i as f32)),
                    UnitDefIdx(idx),
                    kind,
                    faction,
                    hp,
                ));
            }
        }
        app.insert_resource(onus::sim::AiCommanders::new(seed, &[me]));
        app
    };
    (build(sa, Faction::A), build(sb, Faction::B))
}

/// **A producer inside the tick is still a networked producer.** The scripted
/// commander runs *in* the sim chain, after the link's frame-time drain and
/// before `apply_commands` — so without a second drain its orders would be
/// applied locally, on the peer that thought of them, and the other side would
/// never hear of them.
///
/// Both peers must end up with the same account of the match and nothing
/// applied as a local `Asap` command.
#[test]
fn an_ai_on_each_peer_plays_one_identical_match() {
    let (mut a, mut b) = pair_of_ai_peers(9, test_config());
    step_both(&mut a, &mut b, 400);

    let account = |app: &App| {
        app.world()
            .resource::<CommandLog>()
            .commands()
            .iter()
            .map(|c| (c.tick, c.schedule, c.fate, c.order.clone()))
            .collect::<Vec<_>>()
    };
    let (la, lb) = (account(&a), account(&b));
    assert!(
        la.len() >= 10,
        "the commanders barely acted ({}), so this proves little",
        la.len()
    );
    assert_eq!(
        la, lb,
        "the two peers recorded different accounts of one match"
    );
    assert!(
        la.iter()
            .all(|(_, schedule, _, _)| *schedule != onus::sim::CommandTick::Asap),
        "a command was applied locally on the tick it was issued: {la:?}"
    );
    let (ha, hb) = (hashes_of(&a), hashes_of(&b));
    let common = ha.0.len().min(hb.0.len());
    assert!(common > 300, "the peers barely ran ({common})");
    assert_eq!(ha.0[..common], hb.0[..common], "the peers diverged");
    assert!(link_of(&a).failure().is_none() && link_of(&b).failure().is_none());
    assert!(link_of(&a).hashes_agreed() > 20);
}

/// The control for every "two runs agree" claim in this file: with a seeded
/// commander, **a different seed is a different match** — so agreement is
/// determinism and not a constant. (The cross-process pair of tests makes the
/// same pairing; this one is cheap enough to run alongside the rest.)
#[test]
fn a_different_seed_is_a_different_match() {
    // The two peers are up to a turn apart, so they are compared at the last
    // tick *both* have played — never at "the last one each", which are
    // different ticks and would fail for a reason that is not a divergence.
    let play = |seed: u64| {
        let (mut a, mut b) = pair_of_ai_peers(seed, test_config());
        step_both(&mut a, &mut b, 400);
        let (ha, hb) = (hashes_of(&a), hashes_of(&b));
        let common = ha.0.len().min(hb.0.len());
        assert!(common > 300, "seed {seed}: the peers barely ran ({common})");
        (ha.0[common - 1], hb.0[common - 1])
    };
    let (a9, b9) = play(9);
    let (a9_again, _) = play(9);
    let (a10, b10) = play(10);
    assert_eq!(a9, b9, "seed 9's peers ended in different worlds");
    assert_eq!(a10, b10, "seed 10's peers ended in different worlds");
    assert_eq!(a9, a9_again, "one seed produced two different matches");
    assert_ne!(
        a9, a10,
        "the match does not depend on the seed it is played with"
    );
}

// ---- AC2: a tick advances only when all inputs for it are present -----------

/// **AC2.** With the peer silent, the sim does not advance — at all. Not the
/// tick counter, not the outcome check, not the hash log.
#[test]
fn a_tick_does_not_advance_while_a_peers_turn_is_missing() {
    let (mut a, mut b) = pair_of_peers(7, test_config());
    step_both(&mut a, &mut b, 8);
    let stalled_at = tick_of(&a);
    let hashes_at = hashes_of(&a).0.len();
    assert!(stalled_at > 0, "nothing ran before the stall, so this proves nothing");
    // The last turn B sent is for its own tick plus the delay; A may run that
    // far and **not one tick further**.
    let last_turn_b_sent = tick_of(&b) + test_config().turn_delay_ticks;

    // B goes quiet.
    for _ in 0..40 {
        step(&mut a);
    }
    let after = tick_of(&a);
    assert!(
        after <= last_turn_b_sent + 1,
        "A reached tick {after}, past the last turn B sent ({last_turn_b_sent})"
    );
    // And once it is out of turns it is *completely* stopped.
    let frozen = tick_of(&a);
    let frozen_hashes = hashes_of(&a).0.len();
    for _ in 0..30 {
        step(&mut a);
    }
    assert_eq!(tick_of(&a), frozen, "the tick counter advanced on a stalled tick");
    assert_eq!(
        hashes_of(&a).0.len(),
        frozen_hashes,
        "a hash was recorded for a tick that never happened"
    );
    assert!(!a.world().resource::<TickGate>().is_open());
    assert!(frozen_hashes > hashes_at, "A never ran at all");

    // B comes back, and A resumes — a stall is a pause, not a failure.
    step_both(&mut a, &mut b, 10);
    assert!(
        tick_of(&a) > frozen,
        "A never resumed after the peer came back"
    );
    assert!(link_of(&a).failure().is_none());
}

/// The direction the gate could break: with both peers talking, nothing stalls
/// and the two stay within one turn of each other.
#[test]
fn two_talking_peers_never_stall() {
    let (mut a, mut b) = pair_of_peers(7, test_config());
    for _ in 0..60 {
        step(&mut a);
        step(&mut b);
        let (ta, tb) = (tick_of(&a), tick_of(&b));
        assert!(
            ta.abs_diff(tb) <= test_config().turn_delay_ticks + 1,
            "the peers drifted apart: {ta} vs {tb}"
        );
    }
    assert!(tick_of(&a) > 40, "the peers barely advanced: {}", tick_of(&a));
    assert!(a.world().resource::<TickGate>().is_open());
    assert!(link_of(&a).failure().is_none() && link_of(&b).failure().is_none());
}

/// A stall that never ends is reported — on the driver's wall clock, which is
/// the only clock allowed to have an opinion about it.
#[test]
fn a_peer_that_never_answers_is_reported_as_stalled() {
    let config = NetConfig {
        stall_timeout_secs: 0.001,
        ..test_config()
    };
    let (mut a, mut b) = pair_of_peers(7, config);
    step_both(&mut a, &mut b, 6);
    assert!(link_of(&a).is_ready(), "the handshake never completed");
    // B stops. A runs out of turns and then out of patience.
    for _ in 0..40 {
        step(&mut a);
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    match link_of(&a).failure() {
        Some(onus::net::LinkFailure::Stalled { tick, waited_secs }) => {
            assert!(*tick > 0);
            assert!(*waited_secs > 0.0);
        }
        other => panic!("a silent peer was not reported as stalled: {other:?}"),
    }
    assert!(!a.world().resource::<TickGate>().is_open());
}

/// **A command issued while the sim is stalled is still a turn, not a local
/// move.** This is the defect the cross-process run caught: the frame that
/// *resumes* a stalled sim sends no new turn (the tick has not advanced yet), so
/// a collection tied to sending left the command in the queue as `Asap` — and
/// the next tick applied it locally, on one peer only. It desynced two
/// processes at a different tick every time.
#[test]
fn a_command_issued_during_a_stall_is_still_exchanged_not_applied_locally() {
    let (mut a, mut b) = pair_of_peers(7, test_config());
    step_both(&mut a, &mut b, 8);

    // Stall A by taking B away, and run A until it is out of turns.
    for _ in 0..20 {
        step(&mut a);
    }
    assert!(
        !a.world().resource::<TickGate>().is_open(),
        "A is not stalled, so this tests nothing"
    );

    // The player clicks *during* the stall.
    let (unit, _) = unit_of(&mut a, Faction::A);
    a.world_mut().resource_mut::<CommandQueue>().0.push_back(
        Order::MoveTo {
            units: vec![unit],
            dest: Vec2::new(-77.0, 77.0),
        }
        .issued_by(Faction::A),
    );
    // A few more stalled frames, then B comes back.
    for _ in 0..3 {
        step(&mut a);
    }
    step_both(&mut a, &mut b, 30);

    // It was applied — and both peers recorded the same thing about it.
    assert!(
        a.world().get::<MoveTarget>(unit).is_some(),
        "the command issued during the stall was lost"
    );
    let account = |app: &App| {
        app.world()
            .resource::<CommandLog>()
            .commands()
            .iter()
            .map(|c| (c.tick, c.schedule, c.fate, c.order.clone()))
            .collect::<Vec<_>>()
    };
    assert!(!account(&a).is_empty(), "nothing was logged");
    assert_eq!(
        account(&a),
        account(&b),
        "a command issued during a stall was applied on one peer only"
    );
    // Specifically: it was never applied as a local `Asap` command.
    assert!(
        account(&a)
            .iter()
            .all(|(_, schedule, _, _)| *schedule != onus::sim::CommandTick::Asap),
        "a networked command was applied locally, on the tick it was issued: {:?}",
        account(&a)
    );
    // ...and the two peers are still the same sim.
    let (ha, hb) = (hashes_of(&a), hashes_of(&b));
    let common = ha.0.len().min(hb.0.len());
    assert!(common > 20);
    assert_eq!(ha.0[..common], hb.0[..common], "the peers diverged");
    assert!(link_of(&a).failure().is_none() && link_of(&b).failure().is_none());
}

// ---- AC3: periodic state-hash exchange detects desync -----------------------

/// **AC3.** The peers compare `sim::state_hash` on a schedule, and agree.
#[test]
fn peers_exchange_state_hashes_and_agree() {
    let (mut a, mut b) = pair_of_peers(7, test_config());
    step_both(&mut a, &mut b, 80);
    assert!(
        link_of(&a).hashes_agreed() >= 5,
        "the hash exchange never actually compared anything ({})",
        link_of(&a).hashes_agreed()
    );
    assert!(link_of(&b).hashes_agreed() >= 5);
    assert!(link_of(&a).failure().is_none());
    assert!(link_of(&b).failure().is_none());
}

/// **The probe the AC exists for: injected divergence is caught.** One peer's
/// world is nudged behind the protocol's back — exactly what a real desync is —
/// and the hash exchange must notice, name the tick, and stop the match.
#[test]
fn an_injected_divergence_is_caught_by_the_hash_exchange() {
    let (mut a, mut b) = pair_of_peers(7, test_config());
    step_both(&mut a, &mut b, 20);
    assert!(link_of(&a).failure().is_none(), "it failed before the injection");
    let agreed_before = link_of(&a).hashes_agreed();
    assert!(agreed_before > 0, "no hash was ever compared, so this proves nothing");

    // Move one unit on B only. Nothing in the protocol did this; the two sims
    // are simply no longer the same sim.
    let (ub, _) = unit_of(&mut b, Faction::B);
    b.world_mut().get_mut::<Position>(ub).expect("position").0.x += 1.0;

    step_both(&mut a, &mut b, 40);

    let caught = |app: &App| {
        matches!(
            link_of(app).failure(),
            Some(onus::net::LinkFailure::Desync { .. })
        )
    };
    assert!(
        caught(&a) || caught(&b),
        "a divergence went unnoticed: A={:?} B={:?}",
        link_of(&a).failure(),
        link_of(&b).failure()
    );
    // And the peer that noticed stopped playing.
    let stopped = if caught(&a) { &a } else { &b };
    assert!(!stopped.world().resource::<TickGate>().is_open());
    let frozen = tick_of(stopped);
    let mut stopped_mut = if caught(&a) { a } else { b };
    for _ in 0..10 {
        step(&mut stopped_mut);
    }
    assert_eq!(
        tick_of(&stopped_mut),
        frozen,
        "a desynced peer kept playing"
    );
}

// ---- the handshake refuses what it must -------------------------------------

/// A peer running different content is refused **before** the match, not
/// diagnosed after it: two rosters cannot produce one match, and no state hash
/// could explain why afterwards.
#[test]
fn a_peer_running_different_content_is_refused() {
    let (sa, sb) = socket_pair();
    let mut a = peer(sa, Faction::A, 7, test_config());
    let mut b = peer(sb, Faction::B, 7, test_config());
    // B is playing a different roster.
    let edited = {
        let dir = std::env::temp_dir().join(format!("onus-m6-content-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        for file in ["units.ron", "resources.ron"] {
            let text = std::fs::read_to_string(data_dir().join(file)).expect("read");
            let text = if file == "units.ron" {
                text.replacen("mvp_alloy_cost: 10", "mvp_alloy_cost: 11", 1)
            } else {
                text
            };
            std::fs::write(dir.join(file), text).expect("write");
        }
        let c = Content::load_from_dir(&dir).expect("edited content");
        let _ = std::fs::remove_dir_all(&dir);
        c
    };
    b.insert_resource(edited);

    step_both(&mut a, &mut b, 6);

    match link_of(&a).failure() {
        Some(onus::net::LinkFailure::Handshake(why)) => {
            assert!(why.contains("content"), "unhelpful refusal: {why}");
        }
        other => panic!("a peer on different content was accepted: {other:?}"),
    }
    assert!(!a.world().resource::<TickGate>().is_open());
    assert_eq!(tick_of(&a), 0, "it played a tick against foreign content");
}

/// ...and so is a peer that claims the same side, a different seed, or a
/// protocol this build does not speak. (Each is checked on its own, so one
/// passing check cannot cover for another.)
#[test]
fn the_handshake_refuses_every_disagreement_it_claims_to() {
    let cases: Vec<(&str, Frame, &str)> = vec![
        (
            "same faction",
            Frame::Hello {
                protocol: PROTOCOL_VERSION,
                faction: Faction::A,
                seed: 7,
                content: content().fingerprint(),
            },
            "command",
        ),
        (
            "different seed",
            Frame::Hello {
                protocol: PROTOCOL_VERSION,
                faction: Faction::B,
                seed: 8,
                content: content().fingerprint(),
            },
            "seed",
        ),
        (
            "different protocol",
            Frame::Hello {
                protocol: PROTOCOL_VERSION + 1,
                faction: Faction::B,
                seed: 7,
                content: content().fingerprint(),
            },
            "protocol",
        ),
    ];
    for (name, hello, expect) in cases {
        let (sa, sb) = socket_pair();
        let mut a = peer(sa, Faction::A, 7, test_config());
        // The "peer" is a bare socket that says one thing.
        let mut raw = onus::net::Connection::new(sb).expect("connection");
        raw.send(&hello).expect("send");
        raw.flush().expect("flush");
        for _ in 0..4 {
            step(&mut a);
        }
        match link_of(&a).failure() {
            Some(onus::net::LinkFailure::Handshake(why)) => assert!(
                why.contains(expect),
                "`{name}`: the refusal does not explain itself: {why}"
            ),
            other => panic!("`{name}` was accepted: {other:?}"),
        }
        assert_eq!(tick_of(&a), 0, "`{name}`: it played a tick anyway");
    }
}

// ---- the sim stays network-free ---------------------------------------------

/// The whole architectural claim, made executable: nothing under `src/sim/`
/// knows a peer, a socket, a clock or a frame exists.
#[test]
fn the_sim_knows_nothing_about_the_network() {
    let sim = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sim");
    let mut files = Vec::new();
    let mut stack = vec![sim];
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
                "TcpStream",
                "TcpListener",
                "std::net",
                "NetLink",
                "NetConfig",
                "crate::net",
                "async",
                "Instant",
                "SystemTime",
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
        "the network reached into the sim: {offenders:#?}"
    );
}

/// The gate is the entire interface, and it is *total*: on a stalled tick every
/// system in the chain is held, not merely the ones that play the match.
#[test]
fn a_closed_gate_holds_the_whole_chain() {
    let mut app = App::new();
    let c = content();
    app.add_plugins(MinimalPlugins)
        .insert_resource(Time::<Fixed>::from_hz(60.0))
        .insert_resource(c)
        .init_resource::<CommandQueue>()
        .init_resource::<RateReport>()
        .init_resource::<Casualties>()
        .insert_resource(Stockpiles::starting(0))
        .insert_resource(CommandLog::new(1))
        .insert_resource(StateHashLog::default());
    onus::add_sim_systems(&mut app, Update);
    for _ in 0..5 {
        step(&mut app);
    }
    let before = (
        tick_of(&app),
        hashes_of(&app).0.len(),
        app.world().resource::<CommandLog>().log().content.is_known(),
        app.world().resource::<SimIds>().issued(),
    );
    assert!(before.0 > 0 && before.1 > 0, "the fixture never ran");

    app.world_mut().resource_mut::<TickGate>().close();
    for _ in 0..20 {
        step(&mut app);
    }
    assert_eq!(
        (
            tick_of(&app),
            hashes_of(&app).0.len(),
            app.world().resource::<CommandLog>().log().content.is_known(),
            app.world().resource::<SimIds>().issued(),
        ),
        before,
        "something ran on a stalled tick"
    );

    app.world_mut().resource_mut::<TickGate>().open();
    step(&mut app);
    assert_eq!(tick_of(&app), before.0 + 1, "the gate did not reopen");
}
