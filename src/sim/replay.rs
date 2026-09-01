//! Deterministic replay (M5). **Render-free** — ECS + math + plain file IO,
//! exactly like [`content`](crate::sim::content); no `AssetServer`, no render
//! types, nothing that reads a clock.
//!
//! Three pieces, and the contract between them is the whole milestone:
//!
//! 1. **Every command carries the tick it applies on.** The queue holds
//!    [`Command`](crate::sim::Command)s, and [`apply_commands`] applies only the
//!    ones due on the sim's own tick counter.
//! 2. **[`CommandLog`] records every command the sim consumed**, with that tick
//!    and its attribution. It is the *input* half of the sim's state: a match is
//!    `(starting world, seed, log)`, and nothing else.
//! 3. **[`state_hash`] is the canonical hash of the sim's state.** One
//!    definition, in the sim, so replay verification and the critic's probes
//!    cannot drift apart into two different notions of "identical".
//!
//! A replay is then: rebuild the starting world, insert the log as a
//! [`ReplaySource`], and run. [`feed_replay`] pushes each tick's commands back
//! onto the same queue the mouse and the AI write to, so the replay is driven
//! through the sim's ordinary command path — there is no replay-only route into
//! the sim, exactly as there is no AI-only one (F-009).

use std::path::Path;

use bevy::ecs::prelude::*;
use bevy::math::Vec2;
use serde::{Deserialize, Serialize};

use crate::sim::combat::{AttackCooldown, Casualties, Engaging, Health, Target};
use crate::sim::economy::{Building, Carrying, GatherPhase, ProductionQueue, Stockpiles, UnitDefIdx};
use crate::sim::spatial::Faction;
use crate::sim::victory::MatchState;
use crate::sim::{
    Attribution, CommandQueue, GatherTarget, MoveTarget, Order, Position, ResourceNode, SignedOrder,
    UnitKind,
};

/// Format version of a persisted [`MatchLog`]. Bumped when the on-disk shape
/// changes; loading refuses a version it does not know, because a log it cannot
/// read exactly is a log it cannot replay at all.
pub const LOG_FORMAT_VERSION: u32 = 1;

// ---- stable identity --------------------------------------------------------

/// A **sim-stable** identity for a world entity: the order in which the sim
/// first saw it, counted by the sim itself.
///
/// A command log cannot address entities by `Entity::to_bits()`. Entity ids are
/// an ECS *allocation* detail, not sim state: in Bevy 0.19 even inserting one
/// extra resource into an app shifts every id the world hands out afterwards, so
/// a replay — which by construction has at least one resource the recording did
/// not ([`ReplaySource`]) — allocates its buildings and its trained units at
/// different ids than the run it is replaying. A log keyed on raw entity bits
/// then commands *the wrong units*, silently.
///
/// `SimId` is the coordinate that does not move: it is assigned by
/// [`identify`], in the sim, in a deterministic order, and depends on nothing
/// but the sim's own sequence of spawns.
#[derive(
    Component, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash, Serialize, Deserialize,
)]
pub struct SimId(pub u64);

impl SimId {
    /// "No id". A log containing one is refused by [`MatchLog::validate`] — at
    /// **both** boundaries — because it names something no replay can resolve.
    ///
    /// The sim itself never records it: the command log is written through
    /// [`SimIds::id_for`], which issues an id for anything that lacks one, so
    /// this value reaches a log only if something builds one by hand. What it
    /// is still used for is the state hash's *reference* lookups (the entity a
    /// `GatherTarget` or a combat `Target` points at), where "no id" is a fact
    /// worth hashing rather than an error.
    pub const UNIDENTIFIED: SimId = SimId(u64::MAX);

    pub fn is_identified(self) -> bool {
        self != SimId::UNIDENTIFIED
    }
}

/// The sim's own entity registry, both ways round: `SimId` → entity, and entity
/// → `SimId`.
///
/// Neither direction is a hash map. Forward is a `Vec` indexed by id; backward
/// is a `BTreeMap` keyed by `Entity::to_bits()`, which is *ordered*, so the only
/// iteration it could ever have is in sorted order and no hash order can reach
/// an outcome. (`identify`, the only thing that iterates candidates, walks a
/// sorted `Vec`.)
///
/// A slot whose entity has been despawned keeps the dead `Entity` value on
/// purpose: an order can name a unit that has since died, and the log has to
/// record *which* unit. Entity *generations* keep a recycled index from
/// colliding with the entry of the entity that used to hold it, and every order
/// path resolves through `Commands::get_entity` anyway (F-009).
#[derive(Resource, Debug, Default)]
pub struct SimIds {
    /// `SimId` → the entity it was issued to. A `Vec` indexed by id, never a
    /// map.
    slots: Vec<Entity>,
    /// `Entity::to_bits()` → its `SimId`. A `BTreeMap`, so it is *ordered*: a
    /// lookup by key is fine anywhere, and the only iteration of it is in
    /// sorted order, so no hash order can reach an outcome.
    ///
    /// This half exists because **a despawned entity still has to be
    /// nameable**. An order can name a unit that died before the order applied
    /// (a click one tick, a death the next; or a replay fed into a world where
    /// the unit is already gone), and the command log has to record *which*
    /// unit it named. The `SimId` component dies with its entity; this does
    /// not. Entity generations keep a recycled index from colliding with the
    /// entry of the entity that used to hold it.
    by_entity: std::collections::BTreeMap<u64, SimId>,
}

impl SimIds {
    /// The entity holding `id`, if the sim has ever issued that id.
    pub fn entity(&self, id: SimId) -> Option<Entity> {
        usize::try_from(id.0)
            .ok()
            .and_then(|i| self.slots.get(i))
            .copied()
    }

    /// The id issued to `e`, alive or dead, if it was ever issued one.
    pub fn id_of(&self, e: Entity) -> Option<SimId> {
        self.by_entity.get(&e.to_bits()).copied()
    }

    /// The id for `e`, **issuing one if it has none**.
    ///
    /// The log is written through this, so the sim can never record
    /// [`SimId::UNIDENTIFIED`] — a value its own [`MatchLog::validate`] refuses,
    /// which would make the log unwritable exactly when something unusual had
    /// happened and the log was the report. An entity the sim has never seen in
    /// the world (a bare fixture entity with no `Position`) gets a real id here
    /// rather than a sentinel: it is a thing an order named, so it is a thing
    /// the log can name.
    pub fn id_for(&mut self, e: Entity) -> SimId {
        match self.id_of(e) {
            Some(id) => id,
            None => self.assign(e),
        }
    }

    /// How many ids have been issued.
    pub fn issued(&self) -> u64 {
        self.slots.len() as u64
    }

    fn assign(&mut self, e: Entity) -> SimId {
        let id = SimId(self.slots.len() as u64);
        self.slots.push(e);
        self.by_entity.insert(e.to_bits(), id);
        id
    }
}

/// Give every *thing in the world* that does not have one a [`SimId`].
///
/// "A thing in the world" is an entity with a [`Position`] — every unit,
/// building and resource node has one, and nothing else in the sim does. New
/// entities are identified in ascending `Entity::to_bits()` order, the same
/// stable-order convention combat uses (F-007), so two runs that spawn the same
/// things in the same tick issue the same ids.
///
/// It is an **exclusive** system, so the ids exist the moment it returns rather
/// than at the next sync point, and it runs **twice** in the chain: once early
/// (immediately after the gather-claim sweep, and before everything that
/// addresses an entity by its id — `apply_commands` logs by it, `feed_replay`
/// resolves by it) and once at the tail (so everything the tick *created* — a
/// placed building, a trained unit — has an id before that tick's state hash is
/// taken, and before the next tick's orders can name it). It is idempotent: the
/// second run does nothing unless the tick spawned something.
pub fn identify(world: &mut World) {
    if !world.contains_resource::<SimIds>() {
        world.init_resource::<SimIds>();
    }
    let mut fresh: Vec<Entity> = world
        .query_filtered::<Entity, (With<Position>, Without<SimId>)>()
        .iter(world)
        .collect();
    if fresh.is_empty() {
        return;
    }
    fresh.sort_unstable_by_key(|e| e.to_bits());
    let mut assigned: Vec<(Entity, SimId)> = Vec::with_capacity(fresh.len());
    world.resource_scope(|_w, mut ids: Mut<SimIds>| {
        for e in fresh {
            assigned.push((e, ids.assign(e)));
        }
    });
    for (e, id) in assigned {
        if let Ok(mut ent) = world.get_entity_mut(e) {
            ent.insert(id);
        }
    }
}

// ---- the loggable form of an order -----------------------------------------

/// An [`Order`] in the form that goes to disk.
///
/// Two deliberate differences from `Order`:
/// - entities are [`SimId`]s, not `Entity`s. Entity ids are an ECS allocation
///   detail that shifts with the app's configuration, so a log keyed on them
///   replays into the wrong units — see [`SimId`];
/// - there is no `By` wrapper: the signature has already been resolved into an
///   [`Attribution`] by the time a command is applied, and that is what is
///   stored beside the order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum LoggedOrder {
    MoveTo {
        units: Vec<u64>,
        dest: (f32, f32),
    },
    Gather {
        units: Vec<u64>,
        node: u64,
        node_pos: (f32, f32),
    },
    Place {
        faction: Faction,
        building: usize,
        pos: (f32, f32),
    },
    Train {
        building: u64,
        unit: usize,
    },
}

fn xy(v: Vec2) -> (f32, f32) {
    (v.x, v.y)
}

fn vec2((x, y): (f32, f32)) -> Vec2 {
    Vec2::new(x, y)
}

impl LoggedOrder {
    /// The loggable form of an order, with each entity resolved to its
    /// [`SimId`] by `id_of`. `None` only for [`Order::By`], which the queue has
    /// already peeled off before a command is ever applied — a signature is an
    /// [`Attribution`], not an order.
    pub fn of(order: &Order, mut id_of: impl FnMut(Entity) -> SimId) -> Option<Self> {
        let ids = |es: &[Entity], id_of: &mut dyn FnMut(Entity) -> SimId| {
            es.iter().map(|e| id_of(*e).0).collect::<Vec<u64>>()
        };
        Some(match order {
            Order::MoveTo { units, dest } => LoggedOrder::MoveTo {
                units: ids(units, &mut id_of),
                dest: xy(*dest),
            },
            Order::Gather {
                units,
                node,
                node_pos,
            } => {
                let units = ids(units, &mut id_of);
                LoggedOrder::Gather {
                    units,
                    node: id_of(*node).0,
                    node_pos: xy(*node_pos),
                }
            }
            Order::Place {
                faction,
                building,
                pos,
            } => LoggedOrder::Place {
                faction: *faction,
                building: *building,
                pos: xy(*pos),
            },
            Order::Train { building, unit } => LoggedOrder::Train {
                building: id_of(*building).0,
                unit: *unit,
            },
            Order::By { .. } => return None,
        })
    }

    /// Every [`SimId`] this order names.
    pub fn sim_ids(&self) -> Vec<SimId> {
        match self {
            LoggedOrder::MoveTo { units, .. } => units.iter().map(|i| SimId(*i)).collect(),
            LoggedOrder::Gather { units, node, .. } => units
                .iter()
                .chain(std::iter::once(node))
                .map(|i| SimId(*i))
                .collect(),
            LoggedOrder::Place { .. } => vec![],
            LoggedOrder::Train { building, .. } => vec![SimId(*building)],
        }
    }

    /// Back to an `Order` in `ids`' world. An id the registry has never issued
    /// is an error rather than a guess: a replay that quietly commanded
    /// *something else* is worse than one that stops.
    ///
    /// What this cannot check is whether the entity behind a known id is the
    /// one the recording meant — no order can know which world it will be
    /// replayed into. That is caught where it shows up, as a per-tick state
    /// hash mismatch, and it is harmless to the sim either way: every order
    /// path resolves entities with `Commands::get_entity` (F-009).
    pub fn to_order(&self, ids: &SimIds) -> Result<Order, String> {
        let ent = |id: u64| {
            ids.entity(SimId(id))
                .ok_or_else(|| format!("log: no entity for sim id {id}"))
        };
        let ents = |all: &[u64]| all.iter().map(|i| ent(*i)).collect::<Result<Vec<_>, _>>();
        Ok(match self {
            LoggedOrder::MoveTo { units, dest } => Order::MoveTo {
                units: ents(units)?,
                dest: vec2(*dest),
            },
            LoggedOrder::Gather {
                units,
                node,
                node_pos,
            } => Order::Gather {
                units: ents(units)?,
                node: ent(*node)?,
                node_pos: vec2(*node_pos),
            },
            LoggedOrder::Place {
                faction,
                building,
                pos,
            } => Order::Place {
                faction: *faction,
                building: *building,
                pos: vec2(*pos),
            },
            LoggedOrder::Train { building, unit } => Order::Train {
                building: ent(*building)?,
                unit: *unit,
            },
        })
    }

    /// Every float this order carries — the values a persisted log has to
    /// reproduce bit for bit.
    fn floats(&self) -> Vec<f32> {
        match self {
            LoggedOrder::MoveTo { dest, .. } => vec![dest.0, dest.1],
            LoggedOrder::Gather { node_pos, .. } => vec![node_pos.0, node_pos.1],
            LoggedOrder::Place { pos, .. } => vec![pos.0, pos.1],
            LoggedOrder::Train { .. } => vec![],
        }
    }
}

/// One command as it was applied: the tick, who it was held to, and what it
/// said.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoggedCommand {
    pub tick: u32,
    pub attribution: Attribution,
    pub order: LoggedOrder,
}

/// A whole match's command stream, plus the seed that drives the sim's RNG.
/// This *is* the replay: the pair (starting world, `MatchLog`) determines every
/// tick that follows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MatchLog {
    pub version: u32,
    pub seed: u64,
    pub commands: Vec<LoggedCommand>,
}

impl Default for MatchLog {
    fn default() -> Self {
        Self {
            version: LOG_FORMAT_VERSION,
            seed: 0,
            commands: Vec::new(),
        }
    }
}

impl MatchLog {
    pub fn new(seed: u64) -> Self {
        Self {
            seed,
            ..Default::default()
        }
    }

    /// The commands due on `tick`, in the order they were applied.
    pub fn at(&self, tick: u32) -> impl Iterator<Item = &LoggedCommand> {
        self.commands.iter().filter(move |c| c.tick == tick)
    }

    /// Is this a log that can be written, read back, and replayed as itself?
    ///
    /// **One predicate, called by both boundaries** ([`to_ron`](Self::to_ron)
    /// and [`from_ron`](Self::from_ron)). The two used to check different
    /// things, which meant the sim could write a log that would then never
    /// load — the log destroyed exactly when it was wanted, with the error
    /// arriving where nothing could be done about it. A write-side check that
    /// admits a value the read side refuses is not a check (F-005's rule, one
    /// boundary further out), so there is now only one list:
    ///
    /// - every coordinate is finite. `NaN`/`±inf` have no round-tripping RON
    ///   spelling, so a log holding one would come back as something else;
    /// - ticks are non-decreasing. They are by construction (one drain per
    ///   tick, in order); a log that is not is corrupt or hand-edited, and
    ///   replaying it would silently drop everything out of order;
    /// - every entity named is *identified*. `SimId::UNIDENTIFIED` is the value
    ///   the sim records for an entity it never gave an id to, and no replay
    ///   can resolve it — so a log containing one is refused where it is
    ///   produced rather than accepted and rejected later.
    ///
    /// The format version is deliberately **not** here: writing refuses a
    /// version this build does not produce, reading refuses one it does not
    /// understand, and those are different sentences about the same number.
    pub fn validate(&self) -> Result<(), String> {
        let mut last = 0u32;
        for c in &self.commands {
            for f in c.order.floats() {
                if !f.is_finite() {
                    return Err(format!(
                        "log: non-finite coordinate {f} in the command at tick {}",
                        c.tick
                    ));
                }
            }
            if c.tick < last {
                return Err(format!(
                    "log: tick {} follows tick {last} — commands are out of order",
                    c.tick
                ));
            }
            last = c.tick;
            for id in c.order.sim_ids() {
                if !id.is_identified() {
                    return Err(format!(
                        "log: the command at tick {} names an unidentified entity — \
                         it was recorded against something the sim never gave an id, \
                         and no replay can resolve it",
                        c.tick
                    ));
                }
            }
        }
        Ok(())
    }

    /// Serialize to RON — the format the rest of the project's data already
    /// uses, so a log is readable and diffable with no new dependency. Refuses
    /// anything [`validate`](Self::validate) refuses: what this writes,
    /// [`from_ron`](Self::from_ron) reads.
    pub fn to_ron(&self) -> Result<String, String> {
        if self.version != LOG_FORMAT_VERSION {
            return Err(format!(
                "log: refusing to write format version {} (this build writes {LOG_FORMAT_VERSION})",
                self.version
            ));
        }
        self.validate()?;
        ron::ser::to_string_pretty(self, ron::ser::PrettyConfig::default())
            .map_err(|e| format!("log: serialize: {e}"))
    }

    pub fn from_ron(text: &str) -> Result<Self, String> {
        let log: MatchLog = ron::from_str(text).map_err(|e| format!("log: parse: {e}"))?;
        if log.version != LOG_FORMAT_VERSION {
            return Err(format!(
                "log: format version {} is not {LOG_FORMAT_VERSION}",
                log.version
            ));
        }
        log.validate()?;
        Ok(log)
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let text = self.to_ron()?;
        std::fs::write(path, text).map_err(|e| format!("log: write {}: {e}", path.display()))
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("log: read {}: {e}", path.display()))?;
        Self::from_ron(&text)
    }
}

/// The match's command log — sim-owned state, appended to in exactly one place
/// ([`apply_commands`](crate::sim::apply_commands)).
#[derive(Resource, Debug, Default)]
pub struct CommandLog {
    log: MatchLog,
    /// Commands dropped because their tick had already gone by. Never applied
    /// late; counted so a desync has a number attached to it.
    late: u32,
    /// Commands applied while no [`SimIds`] registry was in the world, so they
    /// could not be named in the sim's own coordinates. Always zero for the
    /// shipped chain, which installs the registry with itself; non-zero only in
    /// a hand-composed app, where it is the honest count of what this log is
    /// missing.
    unrecorded: u32,
}

impl CommandLog {
    /// A log for a match played with `seed`.
    pub fn new(seed: u64) -> Self {
        Self {
            log: MatchLog::new(seed),
            late: 0,
            unrecorded: 0,
        }
    }

    pub fn log(&self) -> &MatchLog {
        &self.log
    }

    pub fn commands(&self) -> &[LoggedCommand] {
        &self.log.commands
    }

    pub fn seed(&self) -> u64 {
        self.log.seed
    }

    pub fn late(&self) -> u32 {
        self.late
    }

    /// Commands that were applied but could not be logged (no registry). The
    /// gap between what the sim did and what this log says it did.
    pub fn unrecorded(&self) -> u32 {
        self.unrecorded
    }

    pub(crate) fn record(
        &mut self,
        tick: u32,
        attribution: Attribution,
        order: &Order,
        id_of: impl FnMut(Entity) -> SimId,
    ) {
        if let Some(order) = LoggedOrder::of(order, id_of) {
            self.log.commands.push(LoggedCommand {
                tick,
                attribution,
                order,
            });
        }
    }

    pub(crate) fn record_unrecordable(&mut self) {
        self.unrecorded = self.unrecorded.saturating_add(1);
    }

    pub(crate) fn record_late(&mut self, late: u32) {
        self.late = self.late.saturating_add(late);
    }
}

// ---- replaying a log --------------------------------------------------------

/// A log being replayed. Its presence puts the sim in **replay mode**: the
/// scripted AI stands down (its decisions are already in the log, as orders),
/// and [`feed_replay`] is the only producer of commands for the tick.
#[derive(Resource, Debug)]
pub struct ReplaySource {
    log: MatchLog,
    /// How far through `log.commands` the replay has got. The log is ordered by
    /// tick, so this only ever moves forward — the replay never searches, and
    /// never depends on anything but its own position.
    cursor: usize,
    /// Commands the replay could not place because their tick had already gone
    /// by when it reached them. Zero for any log this build wrote.
    skipped: u32,
    /// Live commands (a click, a stray fixture order) discarded because they
    /// were not part of the recorded match.
    discarded: u32,
    /// Logged commands naming a [`SimId`] this world has never issued — the log
    /// does not match the starting world it is being replayed into.
    unresolved: u32,
}

impl ReplaySource {
    pub fn new(log: MatchLog) -> Self {
        Self {
            log,
            cursor: 0,
            skipped: 0,
            discarded: 0,
            unresolved: 0,
        }
    }

    pub fn log(&self) -> &MatchLog {
        &self.log
    }

    pub fn seed(&self) -> u64 {
        self.log.seed
    }

    /// Commands consumed so far.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn skipped(&self) -> u32 {
        self.skipped
    }

    pub fn discarded(&self) -> u32 {
        self.discarded
    }

    pub fn unresolved(&self) -> u32 {
        self.unresolved
    }

    /// Has every logged command been fed?
    pub fn finished(&self) -> bool {
        self.cursor >= self.log.commands.len()
    }
}

/// Feed this tick's logged commands back onto the ordinary command queue.
///
/// Two properties make the replay a replay rather than an approximation:
/// - **The recorded commands are the only ones.** Anything a live producer
///   pushed is discarded first (counted in [`ReplaySource::discarded`]), so a
///   stray click cannot desync a replay by adding an order the recording never
///   had.
/// - **They go through the same path as everything else.** They are pushed as
///   `At(tick)` commands onto the same queue the mouse and the AI write to, and
///   `apply_commands` checks their ownership and charges their cost exactly as
///   it did during the recording. There is no replay-only route into the sim
///   (the same rule that keeps the AI honest — F-009).
pub fn feed_replay(
    state: Res<MatchState>,
    ids: Res<SimIds>,
    mut source: ResMut<ReplaySource>,
    mut queue: ResMut<CommandQueue>,
) {
    let now = state.tick();
    let discarded = queue.0.len() as u32;
    queue.0.clear();
    source.discarded = source.discarded.saturating_add(discarded);

    while let Some(entry) = source.log.commands.get(source.cursor) {
        if entry.tick > now {
            break;
        }
        if entry.tick < now {
            // Only reachable from a hand-edited log: the sim's own ticks are
            // monotonic and `from_ron` rejects an out-of-order log. Counted, not
            // applied — a command off its tick is the divergence replay exists
            // to rule out.
            source.skipped = source.skipped.saturating_add(1);
            source.cursor += 1;
            continue;
        }
        match entry.order.to_order(&ids) {
            Ok(order) => queue
                .0
                .push_at(entry.tick, SignedOrder::from_parts(entry.attribution, order)),
            // An id this world has never issued: the log does not belong to
            // this starting world (or was hand-edited). Counted and dropped —
            // guessing an entity would be worse than missing a command, and the
            // state hash reports the divergence either way.
            Err(_) => source.unresolved = source.unresolved.saturating_add(1),
        }
        source.cursor += 1;
    }
}

// ---- the canonical state hash ----------------------------------------------

/// Every tick's [`state_hash`], in order. Opt-in: insert it and the sim records
/// a hash per tick; leave it out and the hashing pass does nothing (it is an
/// O(entities) walk, and only a replay check or a desync probe needs it).
#[derive(Resource, Debug, Default, Clone, PartialEq, Eq)]
pub struct StateHashLog(pub Vec<u64>);

impl StateHashLog {
    pub fn last(&self) -> Option<u64> {
        self.0.last().copied()
    }

    /// The first tick at which two runs disagree, if any — the figure a desync
    /// report wants (M6 will exchange these).
    pub fn first_divergence(&self, other: &StateHashLog) -> Option<usize> {
        self.0
            .iter()
            .zip(other.0.iter())
            .position(|(a, b)| a != b)
            .or_else(|| (self.0.len() != other.0.len()).then_some(self.0.len().min(other.0.len())))
    }
}

/// Record this tick's state hash, if anybody asked for one. Runs last in the
/// chain, so the hash for tick N is the state tick N ended in.
pub fn record_state_hash(world: &mut World) {
    if !world.contains_resource::<StateHashLog>() {
        return;
    }
    let h = state_hash(world);
    world.resource_mut::<StateHashLog>().0.push(h);
}

/// Field tags, so two different components can never collide into the same row.
mod tag {
    pub const POSITION: u64 = 1;
    pub const HEALTH: u64 = 2;
    pub const FACTION: u64 = 3;
    pub const UNIT_DEF: u64 = 4;
    pub const UNIT_KIND: u64 = 5;
    pub const CARRYING: u64 = 6;
    pub const GATHER_TARGET: u64 = 7;
    pub const GATHER_PHASE: u64 = 8;
    pub const MOVE_TARGET: u64 = 9;
    pub const BUILDING: u64 = 10;
    pub const PRODUCTION: u64 = 11;
    pub const COMBAT_TARGET: u64 = 12;
    pub const COOLDOWN: u64 = 13;
    pub const ENGAGING: u64 = 14;
    pub const RESOURCE_NODE: u64 = 15;
    pub const STOCKPILES: u64 = 16;
    pub const CASUALTIES: u64 = 17;
    pub const MATCH_STATE: u64 = 18;
    pub const UNIDENTIFIED: u64 = 19;
    pub const PENDING_COMMAND: u64 = 20;
    pub const NEXT_SIM_ID: u64 = 21;
}

/// A 64-bit digest of one command the sim is holding. Everything about it that
/// could differ between two worlds: when it is to apply, who it is held to, and
/// what it says (entities as `SimId`s, coordinates as exact bits).
fn command_digest(cmd: &crate::sim::Command, id_of: impl FnMut(Entity) -> SimId) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut mix = |v: u64| {
        h ^= v;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    };
    match cmd.when() {
        crate::sim::CommandTick::Asap => mix(0),
        crate::sim::CommandTick::At(t) => {
            mix(1);
            mix(t as u64);
        }
    }
    mix(match cmd.attribution() {
        Attribution::By(f) => 1 + faction_bits(f),
        Attribution::SelfSigned => 3,
        Attribution::Void => 4,
    });
    match LoggedOrder::of(cmd.order(), id_of) {
        // Unreachable: the queue peels `Order::By` at the boundary. Hashed as a
        // distinct value rather than skipped, so it could never be a silent
        // match with something else.
        None => mix(u64::MAX),
        Some(order) => {
            let (variant, ids, floats, indices) = match &order {
                LoggedOrder::MoveTo { units, dest } => (1u64, units.clone(), vec![dest.0, dest.1], vec![]),
                LoggedOrder::Gather {
                    units,
                    node,
                    node_pos,
                } => {
                    let mut ids = units.clone();
                    ids.push(*node);
                    (2, ids, vec![node_pos.0, node_pos.1], vec![])
                }
                LoggedOrder::Place {
                    faction,
                    building,
                    pos,
                } => (
                    3,
                    vec![faction_bits(*faction)],
                    vec![pos.0, pos.1],
                    vec![*building],
                ),
                LoggedOrder::Train { building, unit } => (4, vec![*building], vec![], vec![*unit]),
            };
            mix(variant);
            mix(ids.len() as u64);
            for id in ids {
                mix(id);
            }
            for f in floats {
                mix(f.to_bits() as u64);
            }
            for i in indices {
                mix(i as u64);
            }
        }
    }
    h
}

/// A `SimId` lookup for entities named by *other* entities' components.
/// Unidentified (or despawned) targets collapse to [`SimId::UNIDENTIFIED`],
/// which is a value, not a hole.
fn sim_id_lookup(world: &mut World) -> impl Fn(Entity) -> SimId + '_ {
    move |e: Entity| {
        world
            .get::<SimId>(e)
            .copied()
            .unwrap_or(SimId::UNIDENTIFIED)
    }
}

fn faction_bits(f: Faction) -> u64 {
    match f {
        Faction::A => 0,
        Faction::B => 1,
    }
}

/// **The** hash of the sim's state — one definition, in the sim, so a replay
/// check and a desync probe cannot drift into two different notions of
/// "identical".
///
/// It covers every piece of state the sim owns and writes: positions, health,
/// ownership, unit/building definitions, carried and banked Alloy, the gather
/// claim (both halves), move and combat targets, attack cooldowns, resource
/// nodes, production queues, the casualty ledger and the match state — **plus
/// the two pieces of sim state that are not components**: the commands the queue
/// is holding for a *later* tick, and the next `SimId` to be issued. Both are
/// things a difference in which is invisible until it fires, which is exactly
/// what a desync check must not wait for. An `Asap` command is **not** one of
/// them — it is drained before any hash is taken, and once the match is decided
/// it can never be applied at all, so hashing it would invent a divergence out
/// of a click.
///
/// **Order-independent, and allocation-independent.** Every fact is emitted as
/// a row keyed by `(SimId, field tag, ..)` and the rows are *sorted* before they
/// are mixed, so no query, archetype or storage order can reach the result —
/// the same reason `combat` walks entities in ascending `to_bits()` (F-007).
/// The key is the sim's own [`SimId`], not `Entity::to_bits()`, so two runs that
/// hold the same sim state hash equal even when the ECS handed them different
/// entity ids (which it does the moment the two apps differ by one resource).
/// Entities the sim has not identified are not silently invisible: they are
/// counted, and the count is hashed.
/// Floats are hashed as their exact bits, never rounded into a bucket: a hash
/// that rounds is a hash that agrees with a run that diverged.
///
/// What it deliberately does **not** cover: the AI's journal and the command
/// log, which are records *about* the run rather than state the run reads. A
/// replay is driven from the log with the AI stood down, so including either
/// would make a faithful replay look like a divergence.
pub fn state_hash(world: &mut World) -> u64 {
    let mut rows: Vec<(u64, u64, u64, u64)> = Vec::new();
    let row = |id: SimId, tag: u64, a: u64, b: u64| (id.0, tag, a, b);

    macro_rules! collect {
        ($q:ty, |$e:ident, $c:ident| $body:block) => {{
            let mut q = world.query::<(&SimId, $q)>();
            let mut out: Vec<(u64, u64, u64, u64)> = Vec::new();
            for (&$e, $c) in q.iter(world) {
                out.extend($body);
            }
            rows.extend(out);
        }};
    }

    collect!(&Position, |e, c| {
        [row(e, tag::POSITION, c.0.x.to_bits() as u64, c.0.y.to_bits() as u64)]
    });
    collect!(&Health, |e, c| {
        [row(e, tag::HEALTH, c.current as u64, c.max as u64)]
    });
    collect!(&Faction, |e, c| {
        [row(e, tag::FACTION, faction_bits(*c), 0)]
    });
    collect!(&UnitDefIdx, |e, c| {
        [row(e, tag::UNIT_DEF, c.0 as u64, 0)]
    });
    collect!(&UnitKind, |e, c| {
        [row(e, tag::UNIT_KIND, *c as u64, 0)]
    });
    collect!(&Carrying, |e, c| { [row(e, tag::CARRYING, c.0 as u64, 0)] });
    // A claim on another entity is hashed by *that* entity's `SimId`, for the
    // same reason the key is: raw entity bits are not comparable across runs.
    {
        let mut q = world.query::<(&SimId, &GatherTarget)>();
        let claims: Vec<(SimId, Entity)> = q.iter(world).map(|(id, t)| (*id, t.0)).collect();
        let of = sim_id_lookup(world);
        rows.extend(
            claims
                .into_iter()
                .map(|(id, target)| row(id, tag::GATHER_TARGET, of(target).0, 0)),
        );
    }
    collect!(&GatherPhase, |e, c| {
        let (which, ticks) = match c {
            GatherPhase::ToNode => (0u64, 0u64),
            GatherPhase::Harvesting { ticks_left } => (1, *ticks_left as u64),
            GatherPhase::ToDropoff => (2, 0),
        };
        [row(e, tag::GATHER_PHASE, which, ticks)]
    });
    collect!(&MoveTarget, |e, c| {
        [row(e, tag::MOVE_TARGET, c.0.x.to_bits() as u64, c.0.y.to_bits() as u64)]
    });
    collect!(&Building, |e, c| {
        [row(e, tag::BUILDING, c.def as u64, 0)]
    });
    {
        let mut q = world.query::<(&SimId, &Target)>();
        let aims: Vec<(SimId, Entity)> = q.iter(world).map(|(id, t)| (*id, t.0)).collect();
        let of = sim_id_lookup(world);
        rows.extend(
            aims.into_iter()
                .map(|(id, target)| row(id, tag::COMBAT_TARGET, of(target).0, 0)),
        );
    }
    collect!(&AttackCooldown, |e, c| {
        [row(e, tag::COOLDOWN, c.0 as u64, 0)]
    });
    collect!(&ResourceNode, |e, c| {
        [row(e, tag::RESOURCE_NODE, c.amount as u64, 0)]
    });

    // Markers and sequences, which need more than one row each.
    {
        let mut q = world.query_filtered::<&SimId, With<Engaging>>();
        let marked: Vec<SimId> = q.iter(world).copied().collect();
        rows.extend(marked.into_iter().map(|id| row(id, tag::ENGAGING, 1, 0)));
    }
    {
        let mut q = world.query::<(&SimId, &ProductionQueue)>();
        let queues: Vec<(u64, u64, u64, u64)> = q
            .iter(world)
            .flat_map(|(id, pq)| {
                let key = id.0;
                // The queue is a sequence: the *position* of an item is part of
                // the state, so it goes into the row's key.
                pq.items
                    .iter()
                    .enumerate()
                    .map(move |(i, item)| {
                        (
                            key,
                            tag::PRODUCTION,
                            i as u64,
                            (item.unit as u64) << 32 | item.ticks_left as u64,
                        )
                    })
                    .chain(std::iter::once((
                        key,
                        tag::PRODUCTION,
                        u64::MAX,
                        pq.items.len() as u64,
                    )))
                    .collect::<Vec<_>>()
            })
            .collect();
        rows.extend(queues);
    }

    // Anything in the world the sim has not identified. Should be nothing: the
    // chain identifies at its tail, so every entity spawned during a tick has an
    // id before that tick is hashed. Counted rather than skipped, so a hole in
    // the identification pass shows up as a changed hash instead of as silence.
    {
        let mut q = world.query_filtered::<Entity, (With<Position>, Without<SimId>)>();
        let n = q.iter(world).count() as u64;
        rows.push((u64::MAX, tag::UNIDENTIFIED, n, 0));
    }

    // Commands the sim has **accepted and is holding for a later tick**:
    // `take_due` keeps an `At(t > now)` command across ticks, so it is state the
    // sim owns, wrote, and will read later. Two worlds identical but for one
    // held command must not hash equal — that is a divergence that has not
    // happened yet, and the hash is what M6 peers exchange to notice one. Keyed
    // by position among the held commands, because the order the sim will apply
    // them in is state too.
    //
    // **`Asap` commands are deliberately not here.** An `Asap` command is not
    // held: it is drained by the very next `apply_commands`, which runs before
    // the hash is taken, so in a running sim it never survives to be hashed at
    // all. Where it *can* survive is a sim that is over — the chain is gated
    // off, so a click on a finished match sits in the queue forever with no
    // causal reach whatever. Hashing that would make a client-side event read
    // as a state divergence, which is the one thing a desync check must never
    // invent (and it would contradict `record_state_hash`'s promise that a
    // frozen sim is visibly frozen).
    {
        let w: &World = world;
        let pending: Vec<(u64, u64)> = match w.get_resource::<CommandQueue>() {
            None => Vec::new(),
            Some(queue) => queue
                .0
                .iter()
                .filter(|cmd| matches!(cmd.when(), crate::sim::CommandTick::At(_)))
                .enumerate()
                .map(|(i, cmd)| {
                    let digest = command_digest(cmd, |e| {
                        w.get::<SimId>(e)
                            .copied()
                            .or_else(|| w.get_resource::<SimIds>().and_then(|ids| ids.id_of(e)))
                            .unwrap_or(SimId::UNIDENTIFIED)
                    });
                    (i as u64, digest)
                })
                .collect(),
        };
        rows.extend(
            pending
                .into_iter()
                .map(|(i, d)| (u64::MAX, tag::PENDING_COMMAND, i, d)),
        );
    }

    // Sim resources, in a constant slot; the tag is what distinguishes them.
    let slot = u64::MAX;
    if let Some(ids) = world.get_resource::<SimIds>() {
        // The next id to be issued. Without it, two worlds whose registries
        // have drifted hash equal until the next spawn — the same
        // "invisible until it fires" gap as a pending command.
        rows.push((slot, tag::NEXT_SIM_ID, ids.issued(), 0));
    }
    if let Some(stock) = world.get_resource::<Stockpiles>() {
        rows.push((
            slot,
            tag::STOCKPILES,
            stock.alloy(Faction::A) as u64,
            stock.alloy(Faction::B) as u64,
        ));
    }
    if let Some(cas) = world.get_resource::<Casualties>() {
        rows.push((
            slot,
            tag::CASUALTIES,
            cas.lost(Faction::A) as u64,
            cas.lost(Faction::B) as u64,
        ));
    }
    if let Some(state) = world.get_resource::<MatchState>() {
        let outcome = match state.outcome() {
            None => 0u64,
            Some(o) => {
                let winner = match o.winner {
                    None => 1,
                    Some(f) => 2 + faction_bits(f),
                };
                (winner << 32) | (o.tick as u64 + 1)
            }
        };
        rows.push((
            slot,
            tag::MATCH_STATE,
            state.tick() as u64 | ((state.engaged() as u64) << 32),
            outcome,
        ));
    }

    rows.sort_unstable();

    // FNV-1a over the sorted rows.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut mix = |v: u64| {
        h ^= v;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    };
    for (a, b, c, d) in rows {
        mix(a);
        mix(b);
        mix(c);
        mix(d);
    }
    h
}
