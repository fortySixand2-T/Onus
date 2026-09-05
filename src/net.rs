//! Lockstep networking (M6) — **driver-side, not sim**.
//!
//! Two peers play the same match by applying the same commands on the same
//! ticks. Nothing here is novel; what makes it possible is what M0–M5 built:
//!
//! - **`SimId`** (F-011). An `Entity` cannot cross a process boundary, exactly
//!   as it could not survive an extra resource being inserted. Commands on the
//!   wire name `SimId`s, so both peers resolve them to their own entities.
//! - **`sim::state_hash`** — one canonical hash, already covering held commands,
//!   the registry and the id counter. The peers exchange *that*; there is no
//!   second hash and no second notion of "identical".
//! - **The content fingerprint** (Phase 1). Two peers running different content
//!   would diverge for a reason no hash could explain after the fact, so the
//!   handshake refuses the match up front — `MatchLog::load_for`'s rule, applied
//!   to a peer instead of a file.
//! - **`CommandTick`'s stamped schedule** (the M6 prerequisite). A lockstep turn
//!   *is* a command scheduled ahead, and the log records when it was queued, so
//!   a networked match replays exactly like a local one.
//!
//! The sim knows none of this. It has [`TickGate`], which something outside may
//! hold shut, and that is the entire interface: no socket, no clock, no peer.

use std::collections::BTreeMap;
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::sim::content::{Content, ContentFingerprint};
use crate::sim::replay::{CommandLog, LoggedOrder, SimIds, StateHashLog};
use crate::sim::spatial::Faction;
use crate::sim::{
    Attribution, CommandQueue, MatchState, SignedOrder, TickGate,
};

/// Wire protocol version. Two peers that do not agree on it do not play: the
/// frames mean different things, and "mostly compatible" is how a desync gets
/// blamed on the simulation.
pub const PROTOCOL_VERSION: u32 = 1;

// ---- configuration ----------------------------------------------------------

/// Lockstep tunables, from `assets/data/net.ron`. Content-as-data, driver-side —
/// the sim never sees it (see that file).
#[derive(Resource, Debug, Clone, Copy, Deserialize, PartialEq)]
pub struct NetConfig {
    /// Ticks between issuing a command and applying it, on both peers.
    pub turn_delay_ticks: u32,
    /// Ticks between state-hash comparisons.
    pub hash_interval_ticks: u32,
    /// Wall-clock seconds to wait for a missing turn before calling the link
    /// stalled. The driver's clock, never the sim's.
    pub stall_timeout_secs: f32,
}

impl Default for NetConfig {
    fn default() -> Self {
        Self {
            turn_delay_ticks: 6,
            hash_interval_ticks: 30,
            stall_timeout_secs: 10.0,
        }
    }
}

impl NetConfig {
    pub const FILE: &'static str = "net.ron";

    pub fn load_from_dir(dir: &Path) -> Result<Self, String> {
        let path = dir.join(Self::FILE);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(format!("net config: cannot read {}: {e}", path.display())),
        };
        ron::from_str(&text).map_err(|e| format!("net config: cannot parse {}: {e}", path.display()))
    }

    /// A turn delay of zero would mean "apply a command on the tick it was
    /// issued", which no peer can do for the *other* peer's commands.
    pub fn validate(&self) -> Result<(), String> {
        if self.turn_delay_ticks == 0 {
            return Err("net config: turn_delay_ticks must be at least 1".to_string());
        }
        if self.hash_interval_ticks == 0 {
            return Err("net config: hash_interval_ticks must be at least 1".to_string());
        }
        if !(self.stall_timeout_secs.is_finite() && self.stall_timeout_secs > 0.0) {
            return Err("net config: stall_timeout_secs must be a positive number".to_string());
        }
        Ok(())
    }
}

// ---- the wire ---------------------------------------------------------------

/// One command as it crosses the wire: entities as [`SimId`](crate::sim::SimId)s
/// (the only entity name that means the same thing in two processes), plus the
/// attribution the receiver will hold it to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireCommand {
    pub attribution: Attribution,
    pub order: LoggedOrder,
}

/// Everything a peer ever says.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Frame {
    /// Sent once, first, by both sides.
    Hello {
        protocol: u32,
        /// Which side this peer commands. The two must differ — a match with
        /// two commanders of one faction is not a match.
        faction: Faction,
        /// The match seed. Both peers must play the same one or their AI, and
        /// anything else seeded, diverges immediately.
        seed: u64,
        /// The content this peer is running. Two peers on different rosters
        /// would diverge for a reason no state hash could explain afterwards.
        content: ContentFingerprint,
    },
    /// This peer's commands for tick `tick`. Sent **every turn, even empty** —
    /// silence would be indistinguishable from a lost packet, and "all inputs
    /// for a tick are present" has to be decidable.
    Turn {
        tick: u32,
        commands: Vec<WireCommand>,
    },
    /// This peer's `sim::state_hash` at the end of tick `tick`.
    Hash { tick: u32, hash: u64 },
}

/// Length-prefixed RON. RON because the project already speaks it and a log and
/// a frame should be readable by the same eyes; length-prefixed because a stream
/// is a stream and a frame boundary has to be explicit.
fn encode(frame: &Frame) -> Result<Vec<u8>, String> {
    let text = ron::ser::to_string(frame).map_err(|e| format!("net: encode: {e}"))?;
    let len = u32::try_from(text.len()).map_err(|_| "net: frame too large".to_string())?;
    let mut out = Vec::with_capacity(4 + text.len());
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(text.as_bytes());
    Ok(out)
}

/// A non-blocking framed connection: bytes in, whole [`Frame`]s out.
///
/// Non-blocking because this is pumped from inside the schedule — a read that
/// waited would freeze the frame, and a lockstep implementation that blocks the
/// renderer is a lockstep implementation nobody can watch.
#[derive(Debug)]
pub struct Connection {
    stream: TcpStream,
    inbox: Vec<u8>,
    outbox: Vec<u8>,
    closed: bool,
}

impl Connection {
    pub fn new(stream: TcpStream) -> Result<Self, String> {
        stream
            .set_nonblocking(true)
            .map_err(|e| format!("net: cannot make the socket non-blocking: {e}"))?;
        let _ = stream.set_nodelay(true);
        Ok(Self {
            stream,
            inbox: Vec::new(),
            outbox: Vec::new(),
            closed: false,
        })
    }

    /// Queue a frame for sending. Bytes actually leave in [`flush`](Self::flush).
    pub fn send(&mut self, frame: &Frame) -> Result<(), String> {
        self.outbox.extend_from_slice(&encode(frame)?);
        Ok(())
    }

    /// Push whatever the socket will take. A would-block is not an error: the
    /// rest waits for the next pump.
    pub fn flush(&mut self) -> Result<(), String> {
        while !self.outbox.is_empty() {
            match self.stream.write(&self.outbox) {
                Ok(0) => {
                    self.closed = true;
                    return Err("net: the peer closed the connection".to_string());
                }
                Ok(n) => {
                    self.outbox.drain(..n);
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => return Ok(()),
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) => {
                    self.closed = true;
                    return Err(format!("net: send failed: {e}"));
                }
            }
        }
        Ok(())
    }

    /// Every whole frame that has arrived since the last call, in order.
    pub fn receive(&mut self) -> Result<Vec<Frame>, String> {
        let mut buf = [0u8; 8192];
        loop {
            match self.stream.read(&mut buf) {
                Ok(0) => {
                    self.closed = true;
                    break;
                }
                Ok(n) => self.inbox.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) => {
                    self.closed = true;
                    return Err(format!("net: receive failed: {e}"));
                }
            }
        }
        let mut frames = Vec::new();
        loop {
            if self.inbox.len() < 4 {
                break;
            }
            let len = u32::from_le_bytes([self.inbox[0], self.inbox[1], self.inbox[2], self.inbox[3]])
                as usize;
            if self.inbox.len() < 4 + len {
                break;
            }
            let text = String::from_utf8(self.inbox[4..4 + len].to_vec())
                .map_err(|e| format!("net: frame is not text: {e}"))?;
            self.inbox.drain(..4 + len);
            frames.push(ron::from_str(&text).map_err(|e| format!("net: decode: {e}"))?);
        }
        Ok(frames)
    }

    pub fn is_closed(&self) -> bool {
        self.closed
    }
}

// ---- the link ---------------------------------------------------------------

/// Why a link stopped playing. Every one of these is loud: a lockstep match that
/// carries on after any of them is two people watching different games.
#[derive(Debug, Clone, PartialEq)]
pub enum LinkFailure {
    /// The peers do not agree on the protocol, the seed, the content, or on who
    /// is playing which side.
    Handshake(String),
    /// The state hashes differ: the two sims are no longer the same sim.
    Desync { tick: u32, ours: u64, theirs: u64 },
    /// The socket died, or said something this build cannot read.
    Transport(String),
    /// A turn did not arrive within the configured wall-clock timeout.
    Stalled { tick: u32, waited_secs: f32 },
}

/// The lockstep link: the connection, what the peer has said, and what we have
/// said. Driver-owned; the sim sees only [`TickGate`].
#[derive(Resource, Debug)]
pub struct NetLink {
    config: NetConfig,
    connection: Connection,
    /// Which side this process commands, and which the peer does. Two values of
    /// a two-valued enum: the identity is injective because the *set* is, and
    /// the handshake refuses a peer claiming the same side.
    me: Faction,
    peer: Option<Faction>,
    greeted: bool,
    /// Turns the peer has sent, by the tick they apply on. A `BTreeMap`, so the
    /// only iteration of it is in tick order and no hash order can reach an
    /// outcome.
    peer_turns: BTreeMap<u32, Vec<WireCommand>>,
    /// **Our own turns, buffered the same way.** Local commands do not go
    /// straight into the sim's queue: both peers must apply the same commands in
    /// the same *order*, and the order a frame happens to arrive in is not the
    /// same on both sides. So each side holds its own turn until the tick it
    /// applies on, and then both push the merged turn in one canonical order —
    /// faction A's commands, then faction B's — which is identical on both
    /// peers by construction rather than by timing.
    my_turns: BTreeMap<u32, Vec<(Attribution, crate::sim::Order)>>,
    /// The last tick we sent a turn for, so a stalled frame cannot send two.
    last_sent_turn: Option<u32>,
    /// Local commands taken out of the sim's queue and not yet sent.
    ///
    /// Drained **every** pump, including stalled ones. A command left in the
    /// sim's queue is an `Asap` command, and the very next tick that runs will
    /// apply it locally — a command the other peer has never heard of. That is
    /// not a hypothetical: it is what a stall used to cause, because the frame
    /// that resumed a stalled sim sent no new turn (the tick had not advanced)
    /// and so collected nothing.
    pending_local: Vec<(Attribution, crate::sim::Order)>,
    /// Peer hashes not yet compared (they may arrive before we reach that tick).
    peer_hashes: BTreeMap<u32, u64>,
    /// The last tick we sent a hash for, so each is sent once.
    last_hash_sent: Option<u32>,
    /// Ticks whose hashes matched — evidence the check is doing something.
    hashes_agreed: u32,
    failure: Option<LinkFailure>,
    /// Seconds spent waiting for the turn we are currently missing.
    waiting_secs: f32,
}

impl NetLink {
    /// Connect as the host: bind, announce the port, accept one peer.
    ///
    /// The bound address is returned so a caller can bind port 0 and learn what
    /// it got — tests must never hard-code a port, and two matches on one
    /// machine must not collide.
    pub fn host(
        addr: &str,
        me: Faction,
        config: NetConfig,
    ) -> Result<(TcpListener, std::net::SocketAddr), String> {
        let listener =
            TcpListener::bind(addr).map_err(|e| format!("net: cannot bind {addr}: {e}"))?;
        let local = listener
            .local_addr()
            .map_err(|e| format!("net: no local address: {e}"))?;
        let _ = (me, config);
        Ok((listener, local))
    }

    /// Take the single peer connection from a listener bound by [`host`](Self::host).
    pub fn accept(
        listener: &TcpListener,
        me: Faction,
        config: NetConfig,
    ) -> Result<Self, String> {
        let (stream, _) = listener
            .accept()
            .map_err(|e| format!("net: accept failed: {e}"))?;
        Self::from_stream(stream, me, config)
    }

    /// Connect to a host.
    pub fn connect(addr: &str, me: Faction, config: NetConfig) -> Result<Self, String> {
        let stream =
            TcpStream::connect(addr).map_err(|e| format!("net: cannot connect to {addr}: {e}"))?;
        Self::from_stream(stream, me, config)
    }

    pub fn from_stream(
        stream: TcpStream,
        me: Faction,
        config: NetConfig,
    ) -> Result<Self, String> {
        config.validate()?;
        Ok(Self {
            config,
            connection: Connection::new(stream)?,
            me,
            peer: None,
            greeted: false,
            peer_turns: BTreeMap::new(),
            my_turns: BTreeMap::new(),
            last_sent_turn: None,
            pending_local: Vec::new(),
            peer_hashes: BTreeMap::new(),
            last_hash_sent: None,
            hashes_agreed: 0,
            failure: None,
            waiting_secs: 0.0,
        })
    }

    pub fn config(&self) -> &NetConfig {
        &self.config
    }

    pub fn me(&self) -> Faction {
        self.me
    }

    pub fn peer(&self) -> Option<Faction> {
        self.peer
    }

    /// The handshake has completed: both sides agree on protocol, seed, content
    /// and sides.
    pub fn is_ready(&self) -> bool {
        self.peer.is_some() && self.failure.is_none()
    }

    pub fn failure(&self) -> Option<&LinkFailure> {
        self.failure.as_ref()
    }

    /// Ticks whose hashes both peers agreed on. Zero means the check has never
    /// actually run, whatever else looks green.
    pub fn hashes_agreed(&self) -> u32 {
        self.hashes_agreed
    }

    /// Have we got the peer's commands for `tick`?
    pub fn has_turn(&self, tick: u32) -> bool {
        self.peer_turns.contains_key(&tick)
    }

    fn fail(&mut self, failure: LinkFailure) {
        if self.failure.is_none() {
            error!("lockstep link stopped: {failure:?}");
            self.failure = Some(failure);
        }
    }
}

// ---- systems ----------------------------------------------------------------

/// Say hello once, as soon as the link exists.
fn greet(link: &mut NetLink, seed: u64, content: &Content) {
    if link.greeted {
        return;
    }
    link.greeted = true;
    let hello = Frame::Hello {
        protocol: PROTOCOL_VERSION,
        faction: link.me,
        seed,
        content: content.fingerprint(),
    };
    if let Err(e) = link.connection.send(&hello) {
        link.fail(LinkFailure::Transport(e));
    }
}

/// Check the peer's hello against ours. Everything here is a refusal *before*
/// the match rather than a mystery during it.
fn accept_hello(
    link: &mut NetLink,
    protocol: u32,
    faction: Faction,
    seed: u64,
    content: ContentFingerprint,
    our_seed: u64,
    ours: &Content,
) {
    if protocol != PROTOCOL_VERSION {
        link.fail(LinkFailure::Handshake(format!(
            "peer speaks protocol {protocol}, this build speaks {PROTOCOL_VERSION}"
        )));
        return;
    }
    if faction == link.me {
        link.fail(LinkFailure::Handshake(format!(
            "both peers claim to command {faction:?}"
        )));
        return;
    }
    if seed != our_seed {
        link.fail(LinkFailure::Handshake(format!(
            "peer's match seed is {seed}, ours is {our_seed}"
        )));
        return;
    }
    let mine = ours.fingerprint();
    if content.hash() != mine.hash() {
        link.fail(LinkFailure::Handshake(format!(
            "peer is running different content — theirs [{content}], ours [{mine}]"
        )));
        return;
    }
    link.peer = Some(faction);
}

/// One pump of the lockstep link, every fixed step — **including stalled ones**,
/// because a stall is exactly when the missing turn might arrive.
///
/// Ungated on purpose: the sim chain is what `TickGate` holds shut, and this is
/// the thing holding it. It reads sim state (the tick, the queue, the hash log)
/// and writes only the queue and the gate.
#[allow(clippy::too_many_arguments)]
pub fn pump(
    time: Res<Time<Real>>,
    content: Res<Content>,
    state: Res<MatchState>,
    log: Res<CommandLog>,
    ids: Res<SimIds>,
    hashes: Option<Res<StateHashLog>>,
    mut queue: ResMut<CommandQueue>,
    mut gate: ResMut<TickGate>,
    mut link: ResMut<NetLink>,
) {
    let now = state.tick();
    let seed = log.seed();

    greet(&mut link, seed, &content);

    // ---- read whatever has arrived -----------------------------------------
    match link.connection.receive() {
        Ok(frames) => {
            for frame in frames {
                match frame {
                    Frame::Hello {
                        protocol,
                        faction,
                        seed: peer_seed,
                        content: peer_content,
                    } => accept_hello(
                        &mut link,
                        protocol,
                        faction,
                        peer_seed,
                        peer_content,
                        seed,
                        &content,
                    ),
                    Frame::Turn { tick, commands } => {
                        link.peer_turns.insert(tick, commands);
                    }
                    Frame::Hash { tick, hash } => {
                        link.peer_hashes.insert(tick, hash);
                    }
                }
            }
        }
        Err(e) => link.fail(LinkFailure::Transport(e)),
    }
    if link.connection.is_closed() && link.failure.is_none() {
        link.fail(LinkFailure::Transport(
            "the peer closed the connection".to_string(),
        ));
    }

    // Nothing runs until both sides have said who they are — and nothing runs
    // again once the link has failed.
    if link.failure.is_some() || link.peer.is_none() {
        gate.close();
        let _ = link.connection.flush();
        return;
    }

    // ---- take the local commands out of the sim's queue, every pump ---------
    //
    // Every pump, not only the ones that send: a local command left in the
    // queue is an `Asap` command, and the next tick that runs applies it — a
    // command the other peer never had. The frame that resumes a stalled sim
    // sends no new turn (the tick has not advanced yet), so a collection tied to
    // sending would leave exactly those commands behind.
    for signed in queue.0.take_unscheduled() {
        link.pending_local.push(signed.into_parts());
    }

    // ---- send our turn for `now + delay`, exactly once ----------------------
    let turn = now + link.config.turn_delay_ticks;
    if link.last_sent_turn.is_none_or(|last| turn > last) {
        let mine: Vec<(Attribution, crate::sim::Order)> =
            std::mem::take(&mut link.pending_local);
        let mut commands = Vec::with_capacity(mine.len());
        let mut held = Vec::with_capacity(mine.len());
        for (attribution, order) in mine {
            let Some(wire) = LoggedOrder::of(&order, &content, |e| {
                ids.id_of(e).unwrap_or(crate::sim::SimId::UNIDENTIFIED)
            }) else {
                continue;
            };
            commands.push(WireCommand {
                attribution,
                order: wire,
            });
            held.push((attribution, order));
        }
        if let Err(e) = link.connection.send(&Frame::Turn {
            tick: turn,
            commands,
        }) {
            link.fail(LinkFailure::Transport(e));
        }
        link.my_turns.insert(turn, held);
        link.last_sent_turn = Some(turn);
    }

    // ---- hand this tick's turn to the sim, in one canonical order -----------
    //
    // Both peers push **the same commands in the same order**: faction A's,
    // then faction B's, each side's in the order it issued them. Pushing them as
    // they arrive would order them by network timing, and the state hash counts
    // the queue's *order* — two peers with the same commands in a different
    // order are two peers with different hashes, which is a desync report about
    // nothing.
    if link.has_turn(now) {
        let peer_faction = link.peer;
        let mut sides: Vec<(Faction, Vec<(Attribution, crate::sim::Order)>)> = Vec::new();
        if let Some(mine) = link.my_turns.remove(&now) {
            sides.push((link.me, mine));
        }
        if let Some(theirs) = link.peer_turns.get(&now).cloned() {
            let mut resolved = Vec::with_capacity(theirs.len());
            for wire in theirs {
                match wire.order.to_order(&ids, &content) {
                    Ok(order) => resolved.push((wire.attribution, order)),
                    Err(e) => link.fail(LinkFailure::Transport(format!(
                        "peer sent a command this build cannot resolve: {e}"
                    ))),
                }
            }
            if let Some(f) = peer_faction {
                sides.push((f, resolved));
            }
        }
        sides.sort_by_key(|(f, _)| match f {
            Faction::A => 0u8,
            Faction::B => 1,
        });
        for (_, orders) in sides {
            for (attribution, order) in orders {
                queue
                    .0
                    .push_at(now, SignedOrder::from_parts(attribution, order));
            }
        }
    }

    // ---- the gate: this tick runs iff the peer's turn for it is here --------
    //
    // The first `turn_delay_ticks` are the exception, and not a special case so
    // much as arithmetic: the earliest tick either peer can schedule a command
    // for is `0 + delay`, so ticks before that provably carry no input from
    // anybody. Waiting for a turn that cannot exist would deadlock the opening
    // of every match.
    let ready = now < link.config.turn_delay_ticks || link.has_turn(now);
    gate.set(ready && link.failure.is_none());
    if ready {
        link.waiting_secs = 0.0;
    } else {
        link.waiting_secs += time.delta_secs();
        if link.waiting_secs > link.config.stall_timeout_secs {
            let waited = link.waiting_secs;
            link.fail(LinkFailure::Stalled {
                tick: now,
                waited_secs: waited,
            });
            gate.close();
        }
    }

    // ---- periodic hash exchange --------------------------------------------
    //
    // `StateHashLog` holds one hash per tick that ran, so entry `i` is the state
    // tick `i` ended in — the same canonical `sim::state_hash` a replay compares
    // (there is exactly one notion of "identical" in this project). Ticks
    // `< now` have happened; `now` has not.
    if let Some(hashes) = hashes.as_ref() {
        let interval = link.config.hash_interval_ticks;
        let mut next = match link.last_hash_sent {
            Some(last) => last.saturating_add(interval),
            None => 0,
        };
        while next < now {
            let Some(ours) = hashes.0.get(next as usize).copied() else {
                break;
            };
            if let Err(e) = link.connection.send(&Frame::Hash {
                tick: next,
                hash: ours,
            }) {
                link.fail(LinkFailure::Transport(e));
            }
            link.last_hash_sent = Some(next);
            next = next.saturating_add(interval);
        }

        // Compare every peer hash for a tick we have actually played. A peer
        // hash for a tick we have not reached waits: it is not a disagreement
        // until there is something to disagree with.
        let comparable: Vec<u32> = link
            .peer_hashes
            .keys()
            .copied()
            .filter(|t| (*t as usize) < hashes.0.len())
            .collect();
        for tick in comparable {
            let theirs = link.peer_hashes.remove(&tick).unwrap_or_default();
            let ours = hashes.0[tick as usize];
            if theirs == ours {
                link.hashes_agreed = link.hashes_agreed.saturating_add(1);
            } else {
                link.fail(LinkFailure::Desync { tick, ours, theirs });
                gate.close();
            }
        }
    }

    if let Err(e) = link.connection.flush() {
        link.fail(LinkFailure::Transport(e));
    }
}

/// Install the lockstep link on `app`: **the one definition of how a networked
/// match is driven**, shared by the shipped binary and the tests, for the same
/// reason `add_sim_systems` and `add_replay_writer` are (F-004).
///
/// `pump` runs in the same schedule as the sim, immediately **before** it, and
/// ungated: it is what decides whether this tick happens at all, so it cannot
/// itself be gated by that decision. Everything it needs — the tick counter, the
/// registry, the hash log — is sim state it only reads; the two things it writes
/// are the command queue (which every producer writes) and the gate.
pub fn add_net_link(app: &mut App, schedule: impl bevy::ecs::schedule::ScheduleLabel + Clone, link: NetLink) {
    app.insert_resource(link);
    app.init_resource::<StateHashLog>();
    app.add_systems(schedule, pump.before(crate::sim::victory::match_watch));
}
