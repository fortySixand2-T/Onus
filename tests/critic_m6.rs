//! Critic probes for M6 (lockstep networking). **Critic-owned**: the
//! implementer may not edit this file.
//!
//! Judged against the invariants and the spec, not the implementer's framing.
//! The two at greatest risk here are that the sim must stay render-free *and*
//! network-free and clock-free, and that a tick gate must not be able to change
//! what a match decides.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command as OsCommand, Stdio};

use bevy::prelude::*;

use onus::net::{Frame, NetConfig, NetLink, PROTOCOL_VERSION};
use onus::sim::combat::{Casualties, Health};
use onus::sim::content::Content;
use onus::sim::economy::{Building, ProductionQueue, Stockpiles, UnitDefIdx};
use onus::sim::replay::{CommandFate, CommandLog, MatchLog, StateHashLog};
use onus::sim::spatial::Faction;
use onus::sim::{
    CommandQueue, CommandTick, MatchState, Order, Position, RateReport, TickGate,
};

// ---- harness (mine) ---------------------------------------------------------

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data")
}

fn src_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn content() -> Content {
    Content::load_from_dir(&data_dir()).expect("assets/data/*.ron")
}

/// A scratch directory that removes itself on drop, failure included.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let d = std::env::temp_dir()
            .join(format!("onus-critic-m6-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("scratch");
        Scratch(d)
    }
    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A child that is killed and reaped however the probe ends.
struct Peer(Child);

impl Drop for Peer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[derive(Debug, Default, Clone)]
struct Outcome {
    tick: u32,
    hash: String,
    agreed: u32,
    failure: String,
}

fn parse_result(line: &str) -> Outcome {
    let mut o = Outcome::default();
    for f in line.split_whitespace().skip(1) {
        let (k, v) = f.split_once('=').unwrap_or((f, ""));
        match k {
            "tick" => o.tick = v.parse().unwrap_or(0),
            "hash" => o.hash = v.to_string(),
            "agreed" => o.agreed = v.parse().unwrap_or(0),
            "failure" => o.failure = v.to_string(),
            _ => {}
        }
    }
    o
}

/// Run two real processes. `host_seed` and `client_seed` are separate so the
/// negative controls can disagree about the match.
fn run_two_peers(
    ticks: u32,
    host_seed: u64,
    client_seed: u64,
    dump: Option<&std::path::Path>,
) -> (Outcome, Outcome) {
    let exe = env!("CARGO_BIN_EXE_netpeer");
    let mut cmd = OsCommand::new(exe);
    cmd.args(["host", &ticks.to_string(), &host_seed.to_string(), "127.0.0.1:0"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(d) = dump {
        cmd.env("ONUS_NET_DUMP", d);
    }
    let mut host = Peer(cmd.spawn().expect("spawn host"));
    let mut host_out = BufReader::new(host.0.stdout.take().expect("host stdout"));

    let mut line = String::new();
    host_out.read_line(&mut line).expect("port line");
    let port: u16 = line
        .trim()
        .strip_prefix("PORT ")
        .and_then(|p| p.parse().ok())
        .unwrap_or_else(|| panic!("no port announced: {line:?}"));
    assert!(port >= 1024, "the host did not bind an ephemeral port: {port}");

    let mut ccmd = OsCommand::new(exe);
    ccmd.args([
        "client",
        &ticks.to_string(),
        &client_seed.to_string(),
        &format!("127.0.0.1:{port}"),
    ])
    .stdout(Stdio::piped())
    .stderr(Stdio::null());
    if let Some(d) = dump {
        ccmd.env("ONUS_NET_DUMP", d);
    }
    let client = ccmd.output().expect("run client");
    let client_line = String::from_utf8_lossy(&client.stdout)
        .lines()
        .find(|l| l.starts_with("RESULT"))
        .unwrap_or_default()
        .to_string();

    let mut host_line = String::new();
    loop {
        let mut l = String::new();
        if host_out.read_line(&mut l).unwrap_or(0) == 0 {
            break;
        }
        if l.starts_with("RESULT") {
            host_line = l;
            break;
        }
    }
    let _ = host.0.wait();
    assert!(!host_line.is_empty(), "the host never reported");
    assert!(!client_line.is_empty(), "the client never reported");
    (parse_result(&host_line), parse_result(&client_line))
}

// ---- 1. the cross-process claim, and its missing negative controls ---------

/// **The control the shipped cross-process suite claims and does not have.**
///
/// `the_same_match_played_twice_across_processes_is_the_same_match` says in its
/// own doc: "*while a different seed is a different one, so the first assertion
/// cannot be passing on a constant*" — and never plays a different seed.
///
/// It could not have. Nothing in the `netpeer` fixture depends on the seed:
/// `add_sim_systems` installs `AiCommanders::default()`, which holds no
/// commanders, so the sim's only seeded thing never runs, and the seed reaches
/// nothing but `CommandLog::new` and the handshake — neither of which the state
/// hash observes. So "two runs of one seed agree" is satisfied by a match that
/// ignores the seed entirely, which is exactly what this one does.
///
/// A cross-process *determinism* test whose match cannot vary is not testing
/// determinism; it is testing that a constant equals itself.
#[test]
fn the_cross_process_match_actually_depends_on_the_seed_it_is_played_with() {
    const TICKS: u32 = 120;
    let mut hashes: Vec<(u64, String)> = Vec::new();
    for seed in [1u64, 3, 9, 12345] {
        let (host, client) = run_two_peers(TICKS, seed, seed, None);
        assert_eq!(host.failure, "none", "seed {seed}: {}", host.failure);
        assert_eq!(host.tick, TICKS, "seed {seed} stopped early");
        assert_eq!(host.hash, client.hash, "seed {seed}: the peers disagreed");
        hashes.push((seed, host.hash.clone()));
    }
    let distinct: std::collections::BTreeSet<&String> = hashes.iter().map(|(_, h)| h).collect();
    assert!(
        distinct.len() > 1,
        "four different seeds produced one final state ({hashes:?}), so the \
         cross-process suite's `the_same_match_played_twice...` proves nothing: \
         the fixture has no seeded behaviour for a seed to change"
    );
}

/// **The handshake refuses across a real socket, before the match.** Two
/// processes that disagree about the seed must stop — not play, not diverge
/// later, not agree by accident. This is also the vacuity control for the whole
/// cross-process suite: if a mismatched pair reported `none`, the suite would be
/// passing on a handshake that never ran.
#[test]
fn two_processes_that_disagree_about_the_seed_refuse_to_play() {
    const TICKS: u32 = 150;
    let (host, client) = run_two_peers(TICKS, 3, 4, None);
    for (who, o) in [("host", &host), ("client", &client)] {
        assert!(
            o.failure.contains("Handshake"),
            "{who} played on with a seed mismatch instead of refusing: {o:?}"
        );
        assert!(
            o.tick < TICKS,
            "{who} played the whole match despite refusing the handshake ({} ticks)",
            o.tick
        );
        assert_eq!(o.agreed, 0, "{who} compared hashes with a peer it had refused");
    }
}

/// **The canonical order is the same sequence in both processes.** The queue's
/// order is hashed, so "same commands" is not enough — the two peers must
/// record the same command *stream*. Compared as the two processes' own written
/// accounts, which is the strongest form available: neither can be reading the
/// other's memory.
#[test]
fn both_processes_record_the_identical_command_stream() {
    let dump = Scratch::new("dump");
    const TICKS: u32 = 200;
    let (host, client) = run_two_peers(TICKS, 7, 7, Some(dump.path()));
    assert_eq!(host.failure, "none");
    assert_eq!(client.failure, "none");
    assert_eq!(host.hash, client.hash, "the two processes ended in different worlds");

    let h = std::fs::read_to_string(dump.path().join("host.commands"))
        .expect("the host wrote no command dump");
    let c = std::fs::read_to_string(dump.path().join("client.commands"))
        .expect("the client wrote no command dump");
    assert!(
        h.lines().count() > 4,
        "the match exchanged almost no commands ({} lines), so ordering is untested",
        h.lines().count()
    );
    assert_eq!(
        h, c,
        "the two peers recorded different command streams — the canonical order \
         is not identical on both sides"
    );
    // ...and both peers' per-tick hashes agree the whole way, not just at the end.
    let hh = std::fs::read_to_string(dump.path().join("host.hashes")).expect("host hashes");
    let ch = std::fs::read_to_string(dump.path().join("client.hashes")).expect("client hashes");
    let common = hh.lines().count().min(ch.lines().count());
    assert!(common >= TICKS as usize, "only {common} ticks were hashed");
    for (a, b) in hh.lines().take(common).zip(ch.lines().take(common)) {
        assert_eq!(a, b, "the peers' per-tick hashes part company");
    }
}

// ---- 2. the handshake's four checks, each independently load-bearing -------

/// Frame the way the wire does: 4-byte little-endian length, then RON.
fn wire(frame: &Frame) -> Vec<u8> {
    let text = ron::ser::to_string(frame).expect("encode");
    let mut out = (text.len() as u32).to_le_bytes().to_vec();
    out.extend_from_slice(text.as_bytes());
    out
}

/// A peer app under test, plus a raw socket standing in for the other side, so
/// a probe can say anything at all — including things the shipped peer never
/// would.
fn peer_and_raw_socket(me: Faction, seed: u64) -> (App, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let raw = TcpStream::connect(addr).expect("connect");
    let (server, _) = listener.accept().expect("accept");
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
    let link = NetLink::from_stream(server, me, NetConfig::default()).expect("link");
    onus::net::add_net_link(&mut app, Update, link);
    (app, raw)
}

fn step(app: &mut App) {
    let dt = app.world().resource::<Time<Fixed>>().timestep();
    app.world_mut().resource_mut::<Time<Fixed>>().advance_by(dt);
    app.update();
}

/// **Each of the four disagreements is refused on its own.** One check must not
/// be covering for another: a peer that gets the protocol right and the seed
/// wrong has to be refused for the seed, and so on for each. Every case is a
/// *different* hello, identical to a good one except in one field.
#[test]
fn every_handshake_check_refuses_on_its_own() {
    let good_content = content().fingerprint();
    let cases: Vec<(&str, Frame, &str)> = vec![
        (
            "protocol",
            Frame::Hello {
                protocol: PROTOCOL_VERSION + 1,
                faction: Faction::B,
                seed: 7,
                content: good_content.clone(),
            },
            "protocol",
        ),
        (
            "side",
            Frame::Hello {
                protocol: PROTOCOL_VERSION,
                faction: Faction::A,
                seed: 7,
                content: good_content.clone(),
            },
            "command",
        ),
        (
            "seed",
            Frame::Hello {
                protocol: PROTOCOL_VERSION,
                faction: Faction::B,
                seed: 8,
                content: good_content.clone(),
            },
            "seed",
        ),
        (
            "content",
            Frame::Hello {
                protocol: PROTOCOL_VERSION,
                faction: Faction::B,
                seed: 7,
                content: Content::load_from_dir(&{
                    let d = std::env::temp_dir()
                        .join(format!("onus-critic-m6-hs-{}", std::process::id()));
                    let _ = std::fs::create_dir_all(&d);
                    let t = std::fs::read_to_string(data_dir().join("units.ron")).unwrap();
                    std::fs::write(d.join("units.ron"), t.replacen("mvp_alloy_cost: 10,", "mvp_alloy_cost: 12,", 1)).unwrap();
                    std::fs::copy(data_dir().join("resources.ron"), d.join("resources.ron")).unwrap();
                    d
                })
                .expect("edited content")
                .fingerprint(),
            },
            "content",
        ),
    ];

    // The good hello must be accepted, or every refusal below proves nothing.
    {
        let (mut app, mut raw) = peer_and_raw_socket(Faction::A, 7);
        raw.write_all(&wire(&Frame::Hello {
            protocol: PROTOCOL_VERSION,
            faction: Faction::B,
            seed: 7,
            content: good_content.clone(),
        }))
        .expect("write");
        raw.flush().ok();
        for _ in 0..40 {
            step(&mut app);
        }
        let link = app.world().resource::<NetLink>();
        assert!(
            link.failure().is_none() && link.peer() == Some(Faction::B),
            "a matching hello was refused, so the probe below cannot distinguish \
             a working check from a broken link: {:?}",
            link.failure()
        );
    }

    for (name, hello, expect) in cases {
        let (mut app, mut raw) = peer_and_raw_socket(Faction::A, 7);
        raw.write_all(&wire(&hello)).expect("write");
        raw.flush().ok();
        for _ in 0..40 {
            step(&mut app);
        }
        let link = app.world().resource::<NetLink>();
        let failure = link
            .failure()
            .unwrap_or_else(|| panic!("the {name} mismatch was accepted"));
        let text = format!("{failure:?}");
        assert!(
            text.contains("Handshake"),
            "the {name} mismatch failed for the wrong reason: {text}"
        );
        assert!(
            text.contains(expect),
            "the {name} mismatch was refused, but not for the {name}: {text}"
        );
        assert!(link.peer().is_none(), "the {name} mismatch still set a peer");
        // ...and nothing was played.
        assert_eq!(
            app.world().resource::<MatchState>().tick(),
            0,
            "the sim advanced despite a refused handshake ({name})"
        );
    }
    let _ = std::fs::remove_dir_all(
        std::env::temp_dir().join(format!("onus-critic-m6-hs-{}", std::process::id())),
    );
}

/// The fingerprint the handshake compares is **Phase 1's**, not a second notion
/// of "same content".
#[test]
fn the_handshake_compares_phase_ones_content_fingerprint() {
    let net = std::fs::read_to_string(src_dir().join("net.rs")).expect("net.rs");
    assert!(
        net.contains("content.fingerprint()") || net.contains("ours.fingerprint()"),
        "the handshake does not use `Content::fingerprint`"
    );
    let fps: Vec<&str> = net
        .lines()
        .filter(|l| l.split("//").next().unwrap_or("").contains("fn fingerprint"))
        .collect();
    assert!(fps.is_empty(), "`net.rs` defines its own fingerprint: {fps:?}");
    // The type on the wire is the sim's own.
    assert!(
        net.contains("use crate::sim::content::{Content, ContentFingerprint}"),
        "the wire carries something other than the sim's `ContentFingerprint`"
    );
}

// ---- 3. the gate: a stalled tick is not a tick -----------------------------

/// A world that decides on its very next tick, so a stall can be aimed exactly
/// at the moment of decision.
fn about_to_decide() -> (App, Entity) {
    let c = content();
    let mut app = App::new();
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
    let mut hqs = Vec::new();
    for (f, x) in [(Faction::A, -100.0f32), (Faction::B, 100.0)] {
        let (def, hp) = {
            let c = app.world().resource::<Content>();
            let def = c.building_index("hq").expect("hq");
            (def, Health::from_building_def(c, def))
        };
        hqs.push(
            app.world_mut()
                .spawn((Position(Vec2::new(x, 0.0)), Building { def }, f, ProductionQueue::default(), hp))
                .id(),
        );
    }
    step(&mut app);
    step(&mut app);
    (app, hqs[1])
}

/// **Nothing at all happens on a stalled tick.** Not the tick counter, not the
/// win check, not the content stamp, not the hash log — a hash for a tick that
/// did not happen is a desync report filed against an innocent peer.
#[test]
fn a_closed_gate_stops_every_part_of_the_tick() {
    let (mut app, _) = about_to_decide();
    let before_tick = app.world().resource::<MatchState>().tick();
    let before_hashes = app.world().resource::<StateHashLog>().0.len();
    let before_log = app.world().resource::<CommandLog>().log().clone();
    let before_hash = onus::sim::state_hash(app.world_mut());
    assert!(before_tick > 0, "the fixture never ran");
    assert!(before_hashes > 0, "the fixture never hashed");

    app.world_mut().resource_mut::<TickGate>().close();
    for _ in 0..50 {
        step(&mut app);
    }
    assert_eq!(app.world().resource::<MatchState>().tick(), before_tick, "the tick counter advanced");
    assert_eq!(
        app.world().resource::<StateHashLog>().0.len(),
        before_hashes,
        "a hash was recorded for a tick that did not happen"
    );
    assert_eq!(
        app.world().resource::<CommandLog>().log().commands,
        before_log.commands,
        "the log grew on a stalled tick"
    );
    assert_eq!(
        onus::sim::state_hash(app.world_mut()),
        before_hash,
        "the world changed while the gate was shut"
    );

    // ...and it is a pause, not a stop: opening it resumes exactly where it was.
    app.world_mut().resource_mut::<TickGate>().open();
    step(&mut app);
    assert_eq!(app.world().resource::<MatchState>().tick(), before_tick + 1);
    assert_eq!(app.world().resource::<StateHashLog>().0.len(), before_hashes + 1);
}

/// **A stall must not be able to decide a match, or to change what it decides.**
/// The dangerous shape is a decision that lands *because* of a stall, or one
/// that is lost by it. Aim a stall at the deciding tick itself: while shut,
/// nothing is decided; when opened, the same verdict on the same sim tick as a
/// run that never stalled.
#[test]
fn a_stall_cannot_decide_a_match_nor_change_what_it_decides() {
    // Reference: no stall at all.
    let (mut plain, b_hq) = about_to_decide();
    plain.world_mut().despawn(b_hq);
    step(&mut plain);
    let reference = plain
        .world()
        .resource::<MatchState>()
        .outcome()
        .expect("the fixture must decide on this tick");

    // The same run, with the gate shut across the deciding tick.
    let (mut stalled, b_hq2) = about_to_decide();
    stalled.world_mut().despawn(b_hq2);
    stalled.world_mut().resource_mut::<TickGate>().close();
    for _ in 0..80 {
        step(&mut stalled);
    }
    assert!(
        stalled.world().resource::<MatchState>().outcome().is_none(),
        "a match was decided on a tick that never happened"
    );
    stalled.world_mut().resource_mut::<TickGate>().open();
    step(&mut stalled);
    assert_eq!(
        stalled.world().resource::<MatchState>().outcome(),
        Some(reference),
        "the stall changed the verdict or the tick it was decided on"
    );
    assert_eq!(
        onus::sim::state_hash(stalled.world_mut()),
        onus::sim::state_hash(plain.world_mut()),
        "a stalled run and an unstalled one ended in different worlds"
    );
}

/// The frozen tail is still frozen with a gate installed, and a gate shut after
/// the match is over changes nothing either.
#[test]
fn the_gate_does_not_disturb_a_decided_match() {
    let (mut app, b_hq) = about_to_decide();
    app.world_mut().despawn(b_hq);
    step(&mut app);
    assert!(app.world().resource::<MatchState>().is_over());
    for _ in 0..10 {
        step(&mut app);
    }
    let h = app.world().resource::<StateHashLog>().0.clone();
    let tail = &h[h.len() - 8..];
    assert!(tail.iter().all(|x| *x == tail[0]), "the frozen tail moved: {tail:?}");
    let frozen = onus::sim::state_hash(app.world_mut());
    app.world_mut().resource_mut::<TickGate>().close();
    for _ in 0..20 {
        step(&mut app);
    }
    assert_eq!(onus::sim::state_hash(app.world_mut()), frozen);
    assert!(app.world().resource::<MatchState>().outcome().is_some());
}

// ---- 4. the turn-delay boundary, as arithmetic ------------------------------

/// "The first `turn_delay` ticks are free by arithmetic, not by exception."
/// Checked at both ends: tick 0 runs with no peer turn, tick `delay - 1` runs,
/// and tick `delay` does **not** until the peer's turn for it arrives.
#[test]
fn the_free_opening_ends_exactly_at_the_turn_delay() {
    const DELAY: u32 = 3;
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let mut raw = TcpStream::connect(addr).expect("connect");
    let (server, _) = listener.accept().expect("accept");
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
        .insert_resource(CommandLog::new(7));
    onus::add_sim_systems(&mut app, Update);
    let cfg = NetConfig {
        turn_delay_ticks: DELAY,
        hash_interval_ticks: 5,
        stall_timeout_secs: 600.0,
    };
    let link = NetLink::from_stream(server, Faction::A, cfg).expect("link");
    onus::net::add_net_link(&mut app, Update, link);

    // A well-formed peer that says hello and then says nothing else.
    raw.write_all(&wire(&Frame::Hello {
        protocol: PROTOCOL_VERSION,
        faction: Faction::B,
        seed: 7,
        content: content().fingerprint(),
    }))
    .expect("hello");
    raw.flush().ok();

    for _ in 0..60 {
        step(&mut app);
    }
    assert!(app.world().resource::<NetLink>().failure().is_none(), "the link failed early");
    assert_eq!(
        app.world().resource::<MatchState>().tick(),
        DELAY,
        "the free opening is not exactly `turn_delay` ticks long"
    );
    assert!(!app.world().resource::<TickGate>().is_open(), "the gate stayed open past the opening");

    // The peer's turn for tick DELAY arrives; exactly that tick becomes runnable.
    raw.write_all(&wire(&Frame::Turn { tick: DELAY, commands: vec![] })).expect("turn");
    raw.flush().ok();
    for _ in 0..20 {
        step(&mut app);
    }
    assert_eq!(
        app.world().resource::<MatchState>().tick(),
        DELAY + 1,
        "one delivered turn let through more than one tick"
    );
}

// ---- 5. one turn per peer per tick -----------------------------------------

/// Turn identity is the applying tick, and the latch is what makes it injective:
/// however many frames run while stalled, a peer sends **one** turn per applying
/// tick and never two.
#[test]
fn a_stalled_peer_sends_one_turn_per_tick_and_no_more() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let mut raw = TcpStream::connect(addr).expect("connect");
    let (server, _) = listener.accept().expect("accept");
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
        .insert_resource(CommandLog::new(7));
    onus::add_sim_systems(&mut app, Update);
    let cfg = NetConfig { turn_delay_ticks: 2, hash_interval_ticks: 5, stall_timeout_secs: 600.0 };
    onus::net::add_net_link(
        &mut app,
        Update,
        NetLink::from_stream(server, Faction::A, cfg).expect("link"),
    );
    raw.write_all(&wire(&Frame::Hello {
        protocol: PROTOCOL_VERSION,
        faction: Faction::B,
        seed: 7,
        content: content().fingerprint(),
    }))
    .expect("hello");
    raw.flush().ok();

    // Run well past the free opening so the peer stalls, then keep frames coming.
    for _ in 0..120 {
        step(&mut app);
    }
    raw.set_nonblocking(true).expect("nonblocking");
    let mut buf = Vec::new();
    let mut chunk = [0u8; 65536];
    loop {
        match std::io::Read::read(&mut raw, &mut chunk) {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(_) => break,
        }
    }
    // Decode the frames the peer sent and count turns per applying tick.
    let mut turns: std::collections::BTreeMap<u32, u32> = Default::default();
    let mut i = 0usize;
    while i + 4 <= buf.len() {
        let len = u32::from_le_bytes([buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]) as usize;
        if i + 4 + len > buf.len() {
            break;
        }
        let text = String::from_utf8_lossy(&buf[i + 4..i + 4 + len]).to_string();
        i += 4 + len;
        if let Ok(Frame::Turn { tick, .. }) = ron::from_str::<Frame>(&text) {
            *turns.entry(tick).or_default() += 1;
        }
    }
    assert!(!turns.is_empty(), "the peer sent no turns at all, so nothing is tested");
    let doubled: Vec<(u32, u32)> = turns.iter().filter(|(_, n)| **n > 1).map(|(t, n)| (*t, *n)).collect();
    assert!(
        doubled.is_empty(),
        "a stalled peer sent more than one turn for the same applying tick: {doubled:?}"
    );
}

// ---- 6. the sim knows nothing about the network or the clock ---------------

/// Nothing under `src/sim/` may name a peer, a socket, a clock or the link.
#[test]
fn nothing_under_src_sim_names_the_network_or_a_clock() {
    let mut offenders: Vec<String> = Vec::new();
    let mut stack = vec![src_dir().join("sim")];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).expect("read src/sim") {
            let p = e.expect("entry").path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            if !p.extension().is_some_and(|x| x == "rs") {
                continue;
            }
            let text = std::fs::read_to_string(&p).expect("read");
            for (i, line) in text.lines().enumerate() {
                let code = match line.find("//") {
                    Some(at) => &line[..at],
                    None => line,
                };
                for banned in [
                    "TcpStream", "TcpListener", "SocketAddr", "crate::net", "NetLink",
                    "NetConfig", "net::Frame", "WireCommand", "Time<Real>", "SystemTime",
                    "Instant", "UNIX_EPOCH", "replay_io",
                ] {
                    if code.contains(banned) {
                        offenders.push(format!("{}:{}: {banned}", p.display(), i + 1));
                    }
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "the sim names the network or a clock: {offenders:#?}"
    );
    // The gate is the entire interface, and it is not sim state: not hashed,
    // not logged.
    let replay = std::fs::read_to_string(src_dir().join("sim/replay.rs")).expect("replay.rs");
    assert!(
        !replay.contains("TickGate"),
        "the tick gate reaches the state hash or the log"
    );
}

/// The wall-clock timeout lives entirely in the driver.
#[test]
fn the_stall_timeout_is_read_only_by_the_driver() {
    let net = std::fs::read_to_string(src_dir().join("net.rs")).expect("net.rs");
    assert!(net.contains("Time<Real>"), "the link no longer reads a real clock");
    let mut sim_clock = Vec::new();
    let mut stack = vec![src_dir().join("sim")];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).expect("read") {
            let p = e.expect("e").path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                let t = std::fs::read_to_string(&p).expect("read");
                for (i, l) in t.lines().enumerate() {
                    let code = l.split("//").next().unwrap_or("");
                    // `Time<Fixed>::delta_secs()` is the *fixed timestep* — a
                    // constant, and F-003's approved shape. What may not appear
                    // is a real clock.
                    if code.contains("Time<Real>")
                        || code.contains("SystemTime")
                        || code.contains("Instant")
                        || code.contains("Res<Time>")
                    {
                        sim_clock.push(format!("{}:{}", p.display(), i + 1));
                    }
                }
            }
        }
    }
    assert!(sim_clock.is_empty(), "the sim reads a wall clock: {sim_clock:?}");
    // ...and the only clock it does read is the fixed timestep.
    let mut stack = vec![src_dir().join("sim")];
    let mut clocks = Vec::new();
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).expect("read") {
            let p = e.expect("e").path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                for l in std::fs::read_to_string(&p).expect("read").lines() {
                    let code = l.split("//").next().unwrap_or("");
                    if code.contains("Res<Time") && !code.contains("Res<Time<Fixed>>") {
                        clocks.push(format!("{}: {}", p.display(), code.trim()));
                    }
                }
            }
        }
    }
    assert!(clocks.is_empty(), "the sim reads a clock other than the fixed timestep: {clocks:?}");
}

// ---- 7. the prerequisite, reviewed on its own ------------------------------

/// `CommandTick::Scheduled` is the sim's stamp. The shipped guard says "nothing
/// outside `take_due` may construct" it — but `take_all_pending` constructs it
/// too. The property that actually holds, and the one worth pinning, is that it
/// is constructed **only inside the sim's own queue module**: a producer, a
/// driver or the network may not mint one.
#[test]
fn only_the_sims_queue_module_constructs_a_stamped_schedule() {
    let mut offenders: Vec<String> = Vec::new();
    let mut stack = vec![src_dir()];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).expect("read src") {
            let p = e.expect("entry").path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            if !p.extension().is_some_and(|x| x == "rs") {
                continue;
            }
            let rel = p.strip_prefix(src_dir()).unwrap().to_string_lossy().to_string();
            let text = std::fs::read_to_string(&p).expect("read");
            for (i, line) in text.lines().enumerate() {
                let code = line.split("//").next().unwrap_or("");
                // A *construction* has a brace; a match arm or a path does not.
                // A *construction* and a *pattern* are spelled alike in Rust,
                // so the pattern positions are excluded explicitly: a match arm
                // begins the line with the pattern (often a tuple) or carries
                // its `=>` on the same line.
                let trimmed = code.trim();
                let is_pattern = trimmed.starts_with('(')
                    || trimmed.starts_with('|')
                    || code.contains("=>")
                    || code.contains("if let")
                    || code.contains("matches!");
                if code.contains("CommandTick::Scheduled {") && !is_pattern && rel != "sim/mod.rs"
                {
                    offenders.push(format!("{rel}:{}: {}", i + 1, code.trim()));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "a stamped schedule is constructed outside the sim's queue: {offenders:#?}"
    );
}

/// `discard_unseen` must keep what the sim is already holding and drop only what
/// arrived since — the distinction the wholesale `clear` got wrong.
#[test]
fn discarding_live_orders_does_not_discard_held_ones() {
    let c = content();
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .insert_resource(Time::<Fixed>::from_hz(60.0))
        .insert_resource(c)
        .init_resource::<CommandQueue>()
        .init_resource::<RateReport>()
        .init_resource::<Casualties>()
        .insert_resource(Stockpiles::starting(0))
        .insert_resource(CommandLog::new(1));
    onus::add_sim_systems(&mut app, Update);
    let (idx, kind, hp) = {
        let c = app.world().resource::<Content>();
        let i = c.unit_index("ripper").expect("ripper");
        (i, c.units[i].mvp_kind, Health::from_def(c, i))
    };
    let u = app
        .world_mut()
        .spawn((Position(Vec2::ZERO), UnitDefIdx(idx), kind, Faction::A, hp))
        .id();
    step(&mut app);
    // Held for a far tick, so the sim stamps it...
    app.world_mut().resource_mut::<CommandQueue>().0.push_at(
        400,
        Order::MoveTo { units: vec![u], dest: Vec2::new(9.0, 9.0) }.issued_by(Faction::A),
    );
    step(&mut app);
    assert_eq!(app.world().resource::<CommandQueue>().0.len(), 1, "the held command vanished");
    // ...and a fresh one that has not been looked at.
    app.world_mut().resource_mut::<CommandQueue>().0.push_back(
        Order::MoveTo { units: vec![u], dest: Vec2::new(1.0, 1.0) }.issued_by(Faction::A),
    );
    let dropped = app.world_mut().resource_mut::<CommandQueue>().0.discard_unseen();
    assert_eq!(dropped, 1, "the wrong number of commands was discarded");
    assert_eq!(
        app.world().resource::<CommandQueue>().0.len(),
        1,
        "discarding unseen orders threw away one the sim was holding"
    );
    // The survivor is the held one: it is applied on its own tick, and the log
    // says so. (`MoveTarget` is removed on arrival, so the world is the wrong
    // place to look for it afterwards.)
    for _ in 0..420 {
        step(&mut app);
    }
    let log = app.world().resource::<CommandLog>().log().clone();
    let held: Vec<_> = log
        .commands
        .iter()
        .filter(|c| matches!(c.schedule, CommandTick::Scheduled { apply: 400, .. }))
        .collect();
    assert_eq!(
        held.len(),
        1,
        "the held command was lost — the log records {:?}",
        log.commands.iter().map(|c| (c.tick, c.schedule, c.fate)).collect::<Vec<_>>()
    );
    assert_eq!(held[0].tick, 400, "it was applied on the wrong tick");
    assert_eq!(held[0].fate, CommandFate::Taken);
}

// ---- 8. the format-version decision, on its own merits ---------------------

/// The no-bump argument rests entirely on one fact: a reader that does not know
/// a variant **refuses** the log rather than misreading it. If RON were lenient
/// about unknown enum variants, a log written today would be silently
/// misinterpreted by a build that predates the addition, and the bump would be
/// mandatory. Checked directly, with a variant no build will ever know.
#[test]
fn a_log_naming_a_variant_this_build_does_not_know_is_refused() {
    let mut log = MatchLog::new(5);
    log.content = content().fingerprint();
    log.commands.push(onus::sim::replay::LoggedCommand {
        tick: 3,
        schedule: CommandTick::Scheduled { queued: 1, apply: 3 },
        fate: CommandFate::Taken,
        attribution: onus::sim::Attribution::By(Faction::A),
        order: onus::sim::replay::LoggedOrder::MoveTo { units: vec![0], dest: (1.0, 2.0) },
    });
    let text = log.to_ron().expect("a stamped log serializes");
    assert!(log.validate().is_ok(), "the sim's own stamped log is invalid");
    // Round trip, so the new variants really do survive the file.
    assert_eq!(MatchLog::from_ron(&text).expect("round trip").commands, log.commands);

    for (what, doctored) in [
        ("an unknown schedule", text.replace("Scheduled(", "Rescheduled(")),
        ("an unknown fate", text.replace("fate: Taken", "fate: Abandoned")),
    ] {
        assert!(
            MatchLog::from_ron(&doctored).is_err(),
            "{what} was accepted, so an older reader would misread a newer log \
             and the format version would have to be bumped"
        );
    }
}

/// Every `(schedule, fate)` pair the sim can now produce is accepted, and the
/// new ones the sim cannot produce are refused — the validator kept pace with
/// the two new variants.
#[test]
fn the_validator_kept_pace_with_the_new_schedule_and_fate() {
    let entry = |tick: u32, schedule: CommandTick, fate: CommandFate| {
        let mut log = MatchLog::new(1);
        log.content = content().fingerprint();
        log.commands.push(onus::sim::replay::LoggedCommand {
            tick,
            schedule,
            fate,
            attribution: onus::sim::Attribution::By(Faction::A),
            order: onus::sim::replay::LoggedOrder::MoveTo { units: vec![0], dest: (0.0, 0.0) },
        });
        log
    };
    for (t, s, f) in [
        (9u32, CommandTick::Scheduled { queued: 3, apply: 9 }, CommandFate::Taken),
        (9, CommandTick::Scheduled { queued: 3, apply: 20 }, CommandFate::Unplayed),
        (9, CommandTick::At(20), CommandFate::Unplayed),
    ] {
        assert!(entry(t, s, f).validate().is_ok(), "the sim's own ({s:?}, {f:?}) is refused");
    }
    for (t, s, f) in [
        // Stamped but taken on a tick it was not held for.
        (9u32, CommandTick::Scheduled { queued: 3, apply: 8 }, CommandFate::Taken),
        // Stamped as queued on the very tick it was taken: never held, so never stamped.
        (9, CommandTick::Scheduled { queued: 9, apply: 9 }, CommandFate::Taken),
        // Unplayed but its tick had already come.
        (9, CommandTick::At(9), CommandFate::Unplayed),
        (9, CommandTick::At(3), CommandFate::Unplayed),
        (9, CommandTick::Scheduled { queued: 3, apply: 9 }, CommandFate::Unplayed),
        // A stamp cannot be late: it was seen before the tick it was aimed at.
        (9, CommandTick::Scheduled { queued: 3, apply: 5 }, CommandFate::Late),
        (9, CommandTick::Asap, CommandFate::Unplayed),
    ] {
        assert!(
            entry(t, s, f).validate().is_err(),
            "({s:?}, {f:?}) at tick {t} was accepted, but no sim run produces it"
        );
    }
}

// ---- 9. F-004 and the guards -----------------------------------------------

/// One definition of how a networked match is driven, used by the shipped
/// binary and the tests alike.
#[test]
fn only_add_net_link_installs_the_lockstep_systems() {
    let mut definitions = 0;
    let mut offenders = Vec::new();
    let mut stack = vec![src_dir()];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).expect("read src") {
            let p = e.expect("entry").path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            if !p.extension().is_some_and(|x| x == "rs") {
                continue;
            }
            let text = std::fs::read_to_string(&p).expect("read");
            if text.contains("pub fn add_net_link") {
                definitions += 1;
            }
            let at = text.find("pub fn add_net_link").unwrap_or(usize::MAX);
            let end = if at == usize::MAX {
                0
            } else {
                at + text[at..].find("\n}\n").unwrap_or(text.len() - at)
            };
            for (i, line) in text.lines().enumerate() {
                let code = line.split("//").next().unwrap_or("");
                if code.contains("net::pump") || (code.contains("pump") && code.contains("add_systems")) {
                    let off: usize = text.lines().take(i).map(|l| l.len() + 1).sum();
                    if !(off > at && off < end) {
                        offenders.push(format!("{}:{}", p.display(), i + 1));
                    }
                }
            }
        }
    }
    assert_eq!(definitions, 1, "there is not exactly one `add_net_link`");
    assert!(offenders.is_empty(), "the pump is registered outside it: {offenders:#?}");
}

/// F-008 still holds honestly: the sweep precedes every reader of the gather
/// claim, and the gate did not smuggle a new pre-sweep system in.
#[test]
fn the_gate_did_not_disturb_the_gather_claim_sweep() {
    let lib = std::fs::read_to_string(src_dir().join("lib.rs")).expect("lib.rs");
    let sweep = lib.find("sim::economy::repair_gather_claims").expect("the sweep");
    for after in [
        "sim::replay::identify",
        "sim::apply_commands",
        "sim::economy::gather",
        "sim::combat::combat",
        "sim::replay::record_unplayed",
        "sim::replay::record_state_hash",
    ] {
        let at = lib.find(after).unwrap_or_else(|| panic!("{after} is not in the chain"));
        assert!(sweep < at, "{after} is registered before the gather-claim sweep");
    }
    // `may_tick` gates the chain and is not itself a claim reader.
    let m = std::fs::read_to_string(src_dir().join("sim/mod.rs")).expect("sim/mod.rs");
    let at = m.find("pub fn may_tick").expect("may_tick");
    let body = &m[at..at + m[at..].find("\n}\n").expect("body")];
    for claim in ["GatherTarget", "GatherPhase", "SplitClaim"] {
        assert!(!body.contains(claim), "the tick gate's run condition reads `{claim}`");
    }
}

/// **The desync check fires, and fires only on a real disagreement.** A peer
/// that reports a different hash for a tick this side has played must stop the
/// match at that tick; a peer that agrees must not.
#[test]
fn a_wrong_hash_from_the_peer_stops_the_match_and_a_right_one_does_not() {
    for (name, corrupt) in [("agreeing", false), ("disagreeing", true)] {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let mut raw = TcpStream::connect(addr).expect("connect");
        let (server, _) = listener.accept().expect("accept");
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
            .insert_resource(CommandLog::new(7));
        onus::add_sim_systems(&mut app, Update);
        let cfg = NetConfig { turn_delay_ticks: 2, hash_interval_ticks: 2, stall_timeout_secs: 600.0 };
        onus::net::add_net_link(
            &mut app,
            Update,
            NetLink::from_stream(server, Faction::A, cfg).expect("link"),
        );
        raw.write_all(&wire(&Frame::Hello {
            protocol: PROTOCOL_VERSION,
            faction: Faction::B,
            seed: 7,
            content: content().fingerprint(),
        }))
        .expect("hello");
        // Feed turns so the sim can advance, then a hash for tick 2.
        for t in 0..12u32 {
            raw.write_all(&wire(&Frame::Turn { tick: t, commands: vec![] })).expect("turn");
        }
        raw.flush().ok();
        for _ in 0..20 {
            step(&mut app);
        }
        let ours = app
            .world()
            .resource::<StateHashLog>()
            .0
            .get(2)
            .copied()
            .expect("tick 2 was played");
        let claimed = if corrupt { ours ^ 0xdead_beef } else { ours };
        raw.write_all(&wire(&Frame::Hash { tick: 2, hash: claimed })).expect("hash");
        raw.flush().ok();
        for _ in 0..20 {
            step(&mut app);
        }
        let link = app.world().resource::<NetLink>();
        if corrupt {
            let f = link.failure().unwrap_or_else(|| panic!("{name}: a wrong hash was accepted"));
            let text = format!("{f:?}");
            assert!(text.contains("Desync"), "{name}: failed for the wrong reason: {text}");
            assert!(text.contains("tick: 2"), "{name}: the desync names the wrong tick: {text}");
            assert!(!app.world().resource::<TickGate>().is_open(), "{name}: the sim kept running");
        } else {
            assert!(link.failure().is_none(), "{name}: an agreeing hash stopped the match: {:?}", link.failure());
            assert!(link.hashes_agreed() > 0, "{name}: the comparison never ran");
        }
    }
}

/// **The new `Unplayed` fate, end to end.** A match decided while the sim is
/// still holding a command must record it and let go of it — and the whole
/// thing must replay hash for hash, because the queue's contents are hashed.
#[test]
fn a_command_held_when_the_match_ends_is_recorded_and_replays_identically() {
    let build = |replay: Option<MatchLog>| -> (Vec<u64>, MatchLog, u64) {
        let c = content();
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(Time::<Fixed>::from_hz(60.0))
            .insert_resource(c)
            .init_resource::<CommandQueue>()
            .init_resource::<RateReport>()
            .init_resource::<Casualties>()
            .insert_resource(Stockpiles::starting(0))
            .insert_resource(StateHashLog::default());
        onus::add_sim_systems(&mut app, Update);
        let live = replay.is_none();
        match replay {
            Some(log) => {
                app.insert_resource(CommandLog::new(log.seed));
                app.insert_resource(onus::sim::ReplaySource::new(log));
            }
            None => {
                app.insert_resource(CommandLog::new(4));
            }
        }
        let mut hqs = Vec::new();
        for (f, x) in [(Faction::A, -100.0f32), (Faction::B, 100.0)] {
            let (def, hp) = {
                let c = app.world().resource::<Content>();
                let def = c.building_index("hq").expect("hq");
                (def, Health::from_building_def(c, def))
            };
            hqs.push(
                app.world_mut()
                    .spawn((Position(Vec2::new(x, 0.0)), Building { def }, f, ProductionQueue::default(), hp))
                    .id(),
            );
        }
        let (idx, kind, hp) = {
            let c = app.world().resource::<Content>();
            let i = c.unit_index("ripper").expect("ripper");
            (i, c.units[i].mvp_kind, Health::from_def(c, i))
        };
        let u = app
            .world_mut()
            .spawn((Position(Vec2::new(-90.0, 0.0)), UnitDefIdx(idx), kind, Faction::A, hp))
            .id();
        for t in 0..40u32 {
            if live && t == 3 {
                // Held for a tick the match will never reach.
                app.world_mut().resource_mut::<CommandQueue>().0.push_at(
                    900,
                    Order::MoveTo { units: vec![u], dest: Vec2::new(5.0, 5.0) }.issued_by(Faction::A),
                );
            }
            // Not gated on `live`: this is how the *world* ends the match, and
            // a replay reproduces `(same starting world, seed, log)` — a
            // despawn done by the fixture is part of the world, not the log.
            if t == 6 {
                app.world_mut().despawn(hqs[1]);
            }
            step(&mut app);
        }
        let hashes = app.world().resource::<StateHashLog>().0.clone();
        let log = app.world().resource::<CommandLog>().log().clone();
        let h = onus::sim::state_hash(app.world_mut());
        (hashes, log, h)
    };

    let (rec_hashes, log, rec_hash) = build(None);
    let unplayed: Vec<_> = log.commands.iter().filter(|c| c.fate == CommandFate::Unplayed).collect();
    assert_eq!(unplayed.len(), 1, "the held command was not recorded as unplayed: {:?}", log.commands);
    assert!(
        matches!(unplayed[0].schedule, CommandTick::Scheduled { apply: 900, .. } | CommandTick::At(900)),
        "the unplayed command lost its schedule: {:?}",
        unplayed[0].schedule
    );
    assert!(log.validate().is_ok(), "the sim recorded a log it calls invalid");

    let (rep_hashes, again, rep_hash) = build(Some(log.clone()));
    assert_eq!(rec_hashes, rep_hashes, "the replay diverged from the recording");
    assert_eq!(rec_hash, rep_hash, "the replayed world is not the recorded world");
    assert_eq!(again.commands, log.commands, "the replay recorded a different account");
}

// ============================================================================
// Pass 2 — probes against the seeded fixture and the second drain.
// ============================================================================

/// A connected pair on an ephemeral loopback port; the listener dies with the
/// call, so nothing stays bound.
fn socket_pair() -> (TcpStream, TcpStream) {
    let l = TcpListener::bind("127.0.0.1:0").expect("bind");
    let a = l.local_addr().expect("addr");
    let client = TcpStream::connect(a).expect("connect");
    let (server, _) = l.accept().expect("accept");
    (server, client)
}

fn net_cfg(delay: u32, hash_every: u32) -> NetConfig {
    NetConfig {
        turn_delay_ticks: delay,
        hash_interval_ticks: hash_every,
        stall_timeout_secs: 600.0,
    }
}

/// The `netpeer` world, rebuilt here so the critic depends on no fixture the
/// implementer owns: seeded layout for both sides, a resource node each, and —
/// optionally — this side's own commander.
fn seeded_world(app: &mut App, seed: u64, me: Option<Faction>) {
    for (faction, base) in [
        (Faction::A, Vec2::new(-750.0, 0.0)),
        (Faction::B, Vec2::new(750.0, 0.0)),
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
        app.world_mut().spawn((
            Position(base + Vec2::new(0.0, 250.0)),
            onus::sim::ResourceNode { amount: 100_000 },
        ));
        let slot = match faction {
            Faction::A => 0u64,
            Faction::B => 1,
        };
        let spots = onus::sim::random_layout(
            3,
            seed ^ (slot.wrapping_mul(0x9E37_79B9_7F4A_7C15)),
            base - Vec2::splat(60.0),
            base + Vec2::splat(60.0),
        );
        for spot in spots {
            let (idx, kind, hp) = {
                let c = app.world().resource::<Content>();
                let i = c.unit_index("worker").expect("worker");
                (i, c.units[i].mvp_kind, Health::from_def(c, i))
            };
            app.world_mut()
                .spawn((Position(spot.pos), UnitDefIdx(idx), kind, faction, hp));
        }
    }
    if let Some(f) = me {
        app.insert_resource(onus::sim::AiCommanders::new(seed, &[f]));
    }
}

fn base_app(seed: u64) -> App {
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
        .insert_resource(CommandLog::new(seed))
        .insert_resource(StateHashLog::default());
    app
}

/// Two linked peers, each with its own commander, built through the shipped
/// installers.
fn linked_pair(seed: u64, cfg: NetConfig) -> (App, App) {
    let (sa, sb) = socket_pair();
    let mut a = base_app(seed);
    onus::add_sim_systems(&mut a, Update);
    onus::net::add_net_link(
        &mut a,
        Update,
        NetLink::from_stream(sa, Faction::A, cfg).expect("link a"),
    );
    seeded_world(&mut a, seed, Some(Faction::A));
    let mut b = base_app(seed);
    onus::add_sim_systems(&mut b, Update);
    onus::net::add_net_link(
        &mut b,
        Update,
        NetLink::from_stream(sb, Faction::B, cfg).expect("link b"),
    );
    seeded_world(&mut b, seed, Some(Faction::B));
    (a, b)
}

fn step_both(a: &mut App, b: &mut App) {
    step(a);
    step(b);
}

// ---- 1. the second drain, as a principle rather than a patch ---------------

/// **Nothing local may reach `apply_commands` unscheduled.** The behavioural
/// form, over a match whose only producer is an in-chain one: every command
/// either peer logs must carry a real schedule, because an `Asap` command is by
/// definition one the other peer never heard of.
#[test]
fn no_command_in_a_networked_match_is_ever_applied_unscheduled() {
    let (mut a, mut b) = linked_pair(11, net_cfg(4, 10));
    for _ in 0..420 {
        step_both(&mut a, &mut b);
    }
    for (who, app) in [("A", &a), ("B", &b)] {
        let link = app.world().resource::<NetLink>();
        assert!(link.failure().is_none(), "{who}: {:?}", link.failure());
        let log = app.world().resource::<CommandLog>().log().clone();
        assert!(
            !log.commands.is_empty(),
            "{who} logged no commands at all, so nothing is being tested"
        );
        let loose: Vec<_> = log
            .commands
            .iter()
            .filter(|c| matches!(c.schedule, CommandTick::Asap))
            .collect();
        assert!(
            loose.is_empty(),
            "{who} applied {} command(s) unscheduled — a command the other peer \
             never heard of: {:?}",
            loose.len(),
            loose.iter().map(|c| (c.tick, c.schedule)).collect::<Vec<_>>()
        );
    }
    // ...and the two peers played the same match.
    let ha = a.world().resource::<StateHashLog>().0.clone();
    let hb = b.world().resource::<StateHashLog>().0.clone();
    let n = ha.len().min(hb.len());
    assert!(n > 300, "the peers barely advanced ({n} ticks)");
    assert_eq!(ha[..n], hb[..n], "the two peers diverged");
    assert_eq!(
        a.world().resource::<CommandLog>().log().commands,
        b.world().resource::<CommandLog>().log().commands,
        "the peers recorded different command streams"
    );
}

/// **The structural form, which is what stops a third gap.** The drain is
/// placed after the one in-chain producer that exists today. The property it
/// stands for is general: *every* system that can push an unscheduled order
/// must run before `collect_local`. Anything that pushes and is ordered after it
/// reintroduces exactly the defect, and nothing else would notice.
#[test]
fn every_in_chain_producer_of_unscheduled_orders_runs_before_the_drain() {
    let lib = std::fs::read_to_string(src_dir().join("lib.rs")).expect("lib.rs");
    let net = std::fs::read_to_string(src_dir().join("net.rs")).expect("net.rs");

    // Where the drain sits, from its registration.
    let reg = net
        .find("collect_local\n")
        .or_else(|| net.find("collect_local\r\n"))
        .expect("collect_local is registered");
    let window = &net[reg..reg + 260.min(net.len() - reg)];
    let anchor_after = window
        .find(".after(")
        .map(|i| {
            let r = &window[i + 7..];
            r[..r.find(')').unwrap_or(0)].to_string()
        })
        .expect("the drain declares what it runs after");
    let anchor_before = window
        .find(".before(")
        .map(|i| {
            let r = &window[i + 8..];
            r[..r.find(')').unwrap_or(0)].to_string()
        })
        .expect("the drain declares what it runs before");
    assert!(
        anchor_before.ends_with("apply_commands"),
        "the drain no longer runs before the application: {anchor_before}"
    );

    // Every system in the chain that pushes an unscheduled order.
    let mut pushers: Vec<String> = Vec::new();
    let mut stack = vec![src_dir().join("sim")];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).expect("read src/sim") {
            let p = e.expect("entry").path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            if !p.extension().is_some_and(|x| x == "rs") {
                continue;
            }
            let text = std::fs::read_to_string(&p).expect("read");
            // The enclosing `pub fn` of every `push_back(` call.
            let mut at = 0usize;
            while let Some(i) = text[at..].find("push_back(") {
                let site = at + i;
                at = site + 10;
                let line_start = text[..site].rfind('\n').map(|n| n + 1).unwrap_or(0);
                if text[line_start..site].trim_start().starts_with("//") {
                    continue;
                }
                // Definitions of the queue's own API are not producers.
                if text[line_start..site].contains("pub fn push_back") {
                    continue;
                }
                let Some(fnpos) = text[..site].rfind("\npub fn ") else { continue };
                let name: String = text[fnpos + 8..]
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                if !pushers.contains(&name) {
                    pushers.push(name);
                }
            }
        }
    }
    assert!(
        !pushers.is_empty(),
        "no in-chain producer of unscheduled orders was found — the guard \
         resolved nothing"
    );

    // Each such system, if it is in the chain, must be registered no later than
    // the drain's anchor.
    let anchor_at = lib
        .find(anchor_after.rsplit("::").next().unwrap_or(&anchor_after))
        .expect("the drain's anchor is in the chain");
    let mut late: Vec<String> = Vec::new();
    let mut excluded: Vec<String> = Vec::new();
    for name in &pushers {
        // Only the ones the chain actually runs.
        let Some(pos) = lib.find(&format!("::{name},")).or_else(|| lib.find(&format!("::{name}\n"))) else {
            continue;
        };
        if pos <= anchor_at {
            continue;
        }
        // A producer that cannot coexist with a lockstep link is not a hole in
        // it. `feed_replay` is the only one: it is gated on a `ReplaySource`,
        // and `ai_commanders` — which the drain anchors on — is gated on the
        // absence of one, so a networked match never runs it. The exclusion is
        // asserted, not assumed: if that gating ever goes, this fires.
        let window = &lib[pos..(pos + 200).min(lib.len())];
        if window.contains("run_if(resource_exists::<sim::replay::ReplaySource>)") {
            excluded.push(name.clone());
            continue;
        }
        late.push(format!("{name} (registered after {anchor_after})"));
    }
    assert_eq!(
        excluded,
        vec!["feed_replay".to_string()],
        "the set of producers excused from the drain changed; each exclusion has \
         to be a producer a networked match provably never runs"
    );
    assert!(
        late.is_empty(),
        "these chain systems push unscheduled orders after the lockstep drain, \
         so their commands are applied locally and never cross the wire: \
         {late:#?}"
    );
}

/// **The drain must be inert where there is no peer.** The shipped binary plays
/// a local match with no link at all; a networked concern must cost it nothing.
#[test]
fn a_local_match_is_untouched_by_the_lockstep_drain() {
    let play = |with_link: bool| -> (Vec<u64>, MatchLog, u64) {
        let seed = 21u64;
        let mut app = base_app(seed);
        onus::add_sim_systems(&mut app, Update);
        if with_link {
            // A link that exists but has no peer: the sim must still be the sim.
            let (s, _keep) = socket_pair();
            onus::net::add_net_link(
                &mut app,
                Update,
                NetLink::from_stream(s, Faction::A, net_cfg(4, 10)).expect("link"),
            );
            std::mem::forget(_keep);
        }
        seeded_world(&mut app, seed, Some(Faction::A));
        for _ in 0..40 {
            step(&mut app);
        }
        let h = app.world().resource::<StateHashLog>().0.clone();
        let log = app.world().resource::<CommandLog>().log().clone();
        let s = onus::sim::state_hash(app.world_mut());
        (h, log, s)
    };
    let (plain_hashes, plain_log, plain_state) = play(false);
    assert!(plain_hashes.len() == 40, "a local match did not run every tick");
    assert!(!plain_log.commands.is_empty(), "the local commander issued nothing");

    // The same world again, with no link: bit-identical, which is the baseline
    // the networked variant must not have disturbed.
    let (again_hashes, again_log, again_state) = play(false);
    assert_eq!(plain_hashes, again_hashes);
    assert_eq!(plain_log.commands, again_log.commands);
    assert_eq!(plain_state, again_state);

    // And `collect_local` is registered only by `add_net_link`, so a local app
    // never has it at all.
    let net = std::fs::read_to_string(src_dir().join("net.rs")).expect("net.rs");
    let at = net.find("pub fn add_net_link").expect("add_net_link");
    let end = at + net[at..].find("\n}\n").expect("body");
    assert!(
        net[at..end].contains("collect_local"),
        "the drain is registered somewhere other than the one installer"
    );
    let outside = net[..at].matches("collect_local").count() + net[end..].matches("collect_local").count();
    assert!(
        outside <= 2,
        "`collect_local` is referenced {outside} times outside its installer — \
         it should be its definition and its doc only"
    );
}

// ---- 2. the canonical order, with two producers on one tick ----------------

/// **The canonical sequence survives the second drain.** A commander's order
/// and a human-equivalent order land in the queue at *different points in the
/// frame* now — one at frame top, one mid-tick. Both peers must still apply the
/// same commands in the same order, which is what the hash-counted queue order
/// requires.
#[test]
fn a_commander_and_a_click_on_one_tick_still_reach_both_peers_in_one_order() {
    let (mut a, mut b) = linked_pair(5, net_cfg(4, 10));
    // Let the handshake settle and the commanders start thinking.
    for _ in 0..40 {
        step_both(&mut a, &mut b);
    }
    // On the same tick, both peers issue a "player" order as well as whatever
    // their commander is doing.
    for round in 0..6u32 {
        for (app, f) in [(&mut a, Faction::A), (&mut b, Faction::B)] {
            let mut q = app.world_mut().query::<(Entity, &onus::sim::SimId, &Faction)>();
            let mut mine: Vec<(Entity, u64)> = q
                .iter(app.world())
                .filter(|(_, _, ff)| **ff == f)
                .map(|(e, id, _)| (e, id.0))
                .collect();
            mine.sort_by_key(|(_, id)| *id);
            if let Some((u, _)) = mine.first().copied() {
                app.world_mut().resource_mut::<CommandQueue>().0.push_back(
                    Order::MoveTo {
                        units: vec![u],
                        dest: Vec2::new(round as f32 * 7.0, 33.0),
                    }
                    .issued_by(f),
                );
            }
        }
        for _ in 0..25 {
            step_both(&mut a, &mut b);
        }
    }
    for (who, app) in [("A", &a), ("B", &b)] {
        assert!(
            app.world().resource::<NetLink>().failure().is_none(),
            "{who}: {:?}",
            app.world().resource::<NetLink>().failure()
        );
    }
    let la = a.world().resource::<CommandLog>().log().commands.clone();
    let lb = b.world().resource::<CommandLog>().log().commands.clone();
    assert!(la.len() >= 12, "too few commands to test an order ({})", la.len());
    assert_eq!(
        la, lb,
        "two producers on one tick reached the peers in different orders"
    );
    // Both sides are represented, so the ordering rule is actually exercised.
    let from_a = la.iter().filter(|c| c.attribution == onus::sim::Attribution::By(Faction::A)).count();
    let from_b = la.iter().filter(|c| c.attribution == onus::sim::Attribution::By(Faction::B)).count();
    assert!(from_a > 0 && from_b > 0, "only one side issued anything (A {from_a}, B {from_b})");
    let ha = a.world().resource::<StateHashLog>().0.clone();
    let hb = b.world().resource::<StateHashLog>().0.clone();
    let n = ha.len().min(hb.len());
    assert_eq!(ha[..n], hb[..n], "the peers' per-tick hashes parted company");
}

// ---- 3. the seeded layout ---------------------------------------------------

/// `random_layout` is a pure function of its arguments — the property the
/// headline cross-process claim now rests on. Same seed, same layout, every
/// time; different seeds, different layouts; and the two sides' derived seeds
/// can never collide.
#[test]
fn the_seeded_layout_is_a_pure_function_and_the_two_sides_never_share_one() {
    let at = |seed: u64| {
        onus::sim::random_layout(3, seed, Vec2::splat(-60.0), Vec2::splat(60.0))
            .into_iter()
            .map(|u| (u.pos.x.to_bits(), u.pos.y.to_bits()))
            .collect::<Vec<_>>()
    };
    for s in [0u64, 1, 7, 12345, u64::MAX] {
        assert_eq!(at(s), at(s), "the layout for seed {s} is not stable");
    }
    let distinct: std::collections::BTreeSet<Vec<(u32, u32)>> =
        [0u64, 1, 7, 12345, u64::MAX].iter().map(|s| at(*s)).collect();
    assert_eq!(distinct.len(), 5, "different seeds produced the same layout");

    // `seed ^ slot * K`: slot 0 is the seed itself, slot 1 is never equal to it.
    const K: u64 = 0x9E37_79B9_7F4A_7C15;
    for s in [0u64, 1, 7, 12345, u64::MAX, K] {
        let a = s ^ 0u64.wrapping_mul(K);
        let b = s ^ 1u64.wrapping_mul(K);
        assert_eq!(a, s, "slot 0 does not use the match seed unchanged");
        assert_ne!(a, b, "the two sides derived the same layout seed from {s}");
        assert_ne!(at(a), at(b), "the two sides were given the same layout");
    }
}

// ---- 4. the control's two horizons, and whether each is load-bearing -------

/// **Is 600 ticks actually past the first seeded *decision*?** The two-horizon
/// control is justified on the ground that a short run tests the layout and a
/// long one tests the commander. That is only true if the commander has in fact
/// made a random choice by the long horizon — otherwise the long run adds
/// nothing the short one did not already have, and the justification is
/// decorative.
///
/// Watched directly, through the generator: the commander's RNG state must have
/// moved.
#[test]
fn the_long_horizon_reaches_a_seeded_decision_and_the_short_one_does_not() {
    let rng_of = |app: &App| -> u64 {
        app.world()
            .resource::<onus::sim::AiCommanders>()
            .commanders()
            .first()
            .expect("a commander")
            .rng_state()
    };
    let (mut a, mut b) = linked_pair(7, net_cfg(4, 10));
    let start = rng_of(&a);

    for _ in 0..120 {
        step_both(&mut a, &mut b);
    }
    let at_short = rng_of(&a);
    assert!(
        a.world().resource::<MatchState>().tick() >= 115,
        "the peers did not reach the short horizon"
    );

    for _ in 0..500 {
        step_both(&mut a, &mut b);
    }
    assert!(
        a.world().resource::<MatchState>().tick() >= 595,
        "the peers did not reach the long horizon (tick {})",
        a.world().resource::<MatchState>().tick()
    );
    let at_long = rng_of(&a);

    assert_eq!(
        at_short, start,
        "the commander already rolled a number before tick 120, so the short \
         horizon is not the layout-only control it is described as"
    );
    assert_ne!(
        at_long, start,
        "the commander never rolled a number by tick 600, so the long horizon \
         tests nothing the short one did not — the two-horizon justification \
         does not hold"
    );
    assert!(a.world().resource::<NetLink>().failure().is_none());
    assert!(b.world().resource::<NetLink>().failure().is_none());
}

/// **The other direction of the control's sensitivity.** With the commander
/// intact but the layout held constant, a short run must no longer tell two
/// seeds apart — which is what makes the *short* horizon the layout's guard,
/// and what a layout-only regression would look like.
#[test]
fn a_constant_layout_makes_the_short_horizon_blind_which_is_why_it_exists() {
    let play = |seed: u64, seeded_layout: bool, ticks: u32| -> u64 {
        let mut app = base_app(seed);
        onus::add_sim_systems(&mut app, Update);
        for (faction, base) in [
            (Faction::A, Vec2::new(-750.0, 0.0)),
            (Faction::B, Vec2::new(750.0, 0.0)),
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
            app.world_mut().spawn((
                Position(base + Vec2::new(0.0, 250.0)),
                onus::sim::ResourceNode { amount: 100_000 },
            ));
            let slot = match faction {
                Faction::A => 0u64,
                Faction::B => 1,
            };
            let positions: Vec<Vec2> = if seeded_layout {
                onus::sim::random_layout(
                    3,
                    seed ^ (slot.wrapping_mul(0x9E37_79B9_7F4A_7C15)),
                    base - Vec2::splat(60.0),
                    base + Vec2::splat(60.0),
                )
                .into_iter()
                .map(|u| u.pos)
                .collect()
            } else {
                (0..3).map(|i| base + Vec2::new(0.0, 20.0 * i as f32)).collect()
            };
            for pos in positions {
                let (idx, kind, hp) = {
                    let c = app.world().resource::<Content>();
                    let i = c.unit_index("worker").expect("worker");
                    (i, c.units[i].mvp_kind, Health::from_def(c, i))
                };
                app.world_mut().spawn((Position(pos), UnitDefIdx(idx), kind, faction, hp));
            }
        }
        app.insert_resource(onus::sim::AiCommanders::new(seed, &[Faction::A, Faction::B]));
        for _ in 0..ticks {
            step(&mut app);
        }
        onus::sim::state_hash(app.world_mut())
    };

    // Seeded layout: two seeds differ at the short horizon.
    assert_ne!(
        play(3, true, 120),
        play(9, true, 120),
        "the seeded layout does not distinguish two seeds at 120 ticks, so the \
         short horizon guards nothing"
    );
    // Constant layout: they do not — which is precisely the regression the
    // short horizon exists to catch, and the reason a long-only control would
    // have missed it.
    assert_eq!(
        play(3, false, 120),
        play(9, false, 120),
        "a constant layout still distinguished two seeds at 120 ticks, so the \
         short horizon is not testing what it is described as testing"
    );
    // ...and at the long horizon the commander alone is enough.
    assert_ne!(
        play(3, false, 620),
        play(9, false, 620),
        "with a constant layout the long horizon cannot tell two seeds apart \
         either, so nothing in the fixture is seeded after tick 120"
    );
}

// ---- 5. the gate and the guards, re-established under the second drain -----

/// The second drain must not have loosened the stall properties: a peer with no
/// turn still stops dead, and a command issued during the stall is still
/// exchanged rather than applied locally.
#[test]
fn the_second_drain_did_not_weaken_the_stall() {
    let (mut a, mut b) = linked_pair(13, net_cfg(3, 5));
    for _ in 0..40 {
        step_both(&mut a, &mut b);
    }
    let tick_before = a.world().resource::<MatchState>().tick();

    // B stops taking part; A must stall rather than run on.
    for _ in 0..60 {
        step(&mut a);
    }
    let stalled_at = a.world().resource::<MatchState>().tick();
    // A may play through the last turn B sent before falling silent — that is
    // what a turn delay *is* — and then must stop. What it may not do is run on.
    let ceiling = tick_before + net_cfg(3, 5).turn_delay_ticks + 1;
    assert!(
        stalled_at <= ceiling,
        "peer A ran on without its partner: {tick_before} -> {stalled_at} (ceiling {ceiling})"
    );
    // From here on nothing may move: not the tick, and not the hash log.
    let hashes_at_stall = a.world().resource::<StateHashLog>().0.len();
    for _ in 0..40 {
        step(&mut a);
    }
    assert_eq!(
        a.world().resource::<MatchState>().tick(),
        stalled_at,
        "peer A kept advancing while stalled"
    );
    assert_eq!(
        a.world().resource::<StateHashLog>().0.len(),
        hashes_at_stall,
        "a stalled peer recorded hashes for ticks that did not happen"
    );
    assert!(!a.world().resource::<TickGate>().is_open(), "the gate stayed open");

    // A command issued during the stall must not be applied locally.
    let mut q = a.world_mut().query::<(Entity, &onus::sim::SimId, &Faction)>();
    let victim = q
        .iter(a.world())
        .filter(|(_, _, f)| **f == Faction::A)
        .map(|(e, id, _)| (e, id.0))
        .min_by_key(|(_, id)| *id)
        .map(|(e, _)| e)
        .expect("a unit of A");
    a.world_mut().resource_mut::<CommandQueue>().0.push_back(
        Order::MoveTo { units: vec![victim], dest: Vec2::new(-1234.0, 0.0) }.issued_by(Faction::A),
    );
    for _ in 0..30 {
        step(&mut a);
    }
    assert_eq!(
        a.world().resource::<MatchState>().tick(),
        stalled_at,
        "a command issued during a stall let the sim advance"
    );
    let log = a.world().resource::<CommandLog>().log().clone();
    assert!(
        !log.commands.iter().any(|c| matches!(
            &c.order,
            onus::sim::replay::LoggedOrder::MoveTo { dest, .. } if dest.0 == -1234.0
        )),
        "a command issued during a stall was applied locally"
    );
    assert_eq!(
        a.world().resource::<StateHashLog>().0.len(),
        hashes_at_stall,
        "issuing a command during a stall recorded a hash for a tick that did \
         not happen"
    );

    // B comes back: both resume and stay together.
    for _ in 0..200 {
        step_both(&mut a, &mut b);
    }
    assert!(a.world().resource::<NetLink>().failure().is_none(), "A: {:?}", a.world().resource::<NetLink>().failure());
    assert!(b.world().resource::<NetLink>().failure().is_none(), "B: {:?}", b.world().resource::<NetLink>().failure());
    let ha = a.world().resource::<StateHashLog>().0.clone();
    let hb = b.world().resource::<StateHashLog>().0.clone();
    let n = ha.len().min(hb.len());
    assert!(n > tick_before as usize, "the peers never resumed");
    assert_eq!(ha[..n], hb[..n], "the peers diverged across the stall");
    // ...and the stalled command did eventually cross the wire.
    assert!(
        a.world()
            .resource::<CommandLog>()
            .log()
            .commands
            .iter()
            .any(|c| matches!(
                &c.order,
                onus::sim::replay::LoggedOrder::MoveTo { dest, .. } if dest.0 == -1234.0
            )),
        "the command issued during the stall was lost rather than deferred"
    );
}

/// The clippy gate is what it says it is: the one documented exception, and no
/// other, and the command in the ledger is the full one.
#[test]
fn the_recorded_clippy_gate_is_the_real_gate() {
    let findings = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("FINDINGS.md"),
    )
    .expect("FINDINGS.md");
    let flat: String = findings.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        flat.contains("cargo clippy --all-targets -- -D warnings"),
        "the ledger does not record the gate as `--all-targets -- -D warnings`"
    );
    assert!(
        flat.contains("critic_m3.rs:234"),
        "the ledger does not name the one known exception"
    );
}
