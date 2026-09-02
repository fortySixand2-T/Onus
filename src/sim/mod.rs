//! The simulation core — **render-free** and headless-testable. This is the
//! unit the harness judges (see BUILD_PLAN.md). It imports only ECS, math, and
//! time; no rendering types (`Color`/`Sprite`/`Transform`/`Window`) appear here,
//! so the sim stays a pure, deterministic function of its state and commands.
//!
//! Presentation and interaction (selection, colors, cursor) live in `client`;
//! Bevy is a thin driver that spawns sprites and calls these systems.

use bevy::ecs::prelude::*;
use bevy::math::Vec2;
use bevy::time::{Fixed, Time};
use std::collections::VecDeque;

pub mod ai;
pub mod combat;
pub mod content;
pub mod economy;
pub mod pathfind;
pub mod replay;
pub mod spatial;
pub mod victory;
pub use ai::{AiAction, AiCommander, AiCommanders, AiJournal};
pub use combat::{AttackCooldown, Casualties, Engaging, Health, Target};
pub use content::Content;
pub use economy::{
    Building, Carrying, GatherPhase, ProductionQueue, QueuedUnit, Stockpiles, UnitDefIdx,
};
pub use pathfind::{astar, FlowField, TileGrid};
pub use replay::{
    state_hash, CommandLog, LoggedCommand, LoggedOrder, MatchLog, ReplaySource, SimId, SimIds,
    StateHashLog,
};
pub use victory::{match_running, MatchOutcome, MatchState};
pub use spatial::{
    brute_force_nearest_enemy, random_layout, Faction, SpatialGrid, SplitMix64, Unit,
};

// ---- tunables --------------------------------------------------------------

// Move speed used to be the global `SPEED` constant. **Retired in M4b**: Speed
// is one of the four stats, so it is per-unit RON data
// (`speed * mvp_combat.speed_per_point`, see `combat::move_speed`) and a unit
// without a definition simply does not move.

/// Distance at which a moving unit snaps to its target and stops.
pub const STOP_EPS: f32 = 1.0;

// ---- components ------------------------------------------------------------

/// Sim-space position — the source of truth for where a thing is.
#[derive(Component)]
pub struct Position(pub Vec2);

/// The silhouette class of a unit — sim data, loaded from `units.ron`
/// (`mvp_kind`) and written by the sim when it spawns a unit, never patched in
/// later by the presentation layer. Its presence marks an entity as a
/// commandable unit;
/// resource nodes carry no `UnitKind`. Purely sim data — visuals (color/size)
/// for a kind live in `client`.
#[derive(Component, Clone, Copy, PartialEq, Eq, Debug, serde::Deserialize)]
pub enum UnitKind {
    Worker,
    Soldier,
    Scout,
}

impl UnitKind {
    pub fn label(self) -> &'static str {
        match self {
            UnitKind::Worker => "Worker",
            UnitKind::Soldier => "Soldier",
            UnitKind::Scout => "Scout",
        }
    }
}

/// A gatherable resource node.
#[derive(Component)]
pub struct ResourceNode {
    pub amount: u32,
}

/// Where a unit is currently ordered to move. Removed on arrival.
#[derive(Component)]
pub struct MoveTarget(pub Vec2);

/// The deposit a unit is assigned to gather from. Read by `economy::gather`.
///
/// **Half of a pair (F-008).** `GatherTarget` and [`GatherPhase`] together are
/// the economy's claim on a unit; combat reads the target alone ("the economy
/// owns this one"), while only the economy can release the claim — so a lone
/// `GatherTarget` disarms a unit forever. The pairing is enforced at both ends,
/// because a rule kept by discipline is a rule that gets broken:
/// - **writing**: `GatherPhase` is a *required component* of `GatherTarget`, so
///   whoever writes the claim writes at least a phase with it;
/// - **removing**: [`economy::release_gather_job`] is the single implementation
///   of dropping it — and, because `#[require]` says nothing about removal and a
///   bare `remove::<GatherPhase>()` elsewhere would still split the pair, the
///   economy *sweeps* for split claims every tick in
///   [`economy::repair_gather_claims`].
///
/// That sweep is worth exactly its **position in the schedule**, so the claim
/// carries an ordering constraint: `repair_gather_claims` runs first among the
/// systems that play the match (only the win-condition watch precedes it, and it
/// reads no claim), ahead of all three readers of `GatherTarget` that exist today
/// — `ai::ai_commanders` (who is idle), `economy::gather` (run the job) and
/// `combat::combat` (the economy owns this unit). A half-claim therefore reaches
/// none of *those*; a reader ordered before the sweep would see one, so **a new
/// reader of this component must be ordered after `repair_gather_claims`**
/// (F-008).
#[derive(Component)]
#[require(GatherPhase)]
pub struct GatherTarget(pub Entity);

// ---- resources -------------------------------------------------------------

/// Player intents awaiting application in `FixedUpdate`. The seed of M5's
/// tick-tagged, loggable command stream. (Named `Order` to avoid Bevy's
/// prelude `Command` trait.)
#[derive(Resource, Default)]
pub struct CommandQueue(pub OrderQueue);

/// The queue itself. It holds [`Command`]s — a [`SignedOrder`] plus **the tick
/// it is to be applied on**. Two things are therefore impossible by
/// construction rather than by convention: an order in flight with no
/// attribution the sim can check it against (M4c, F-009), and an order with no
/// place in the tick stream (M5). Pushing takes anything that can become a
/// [`SignedOrder`], so a caller may push a signed order (`.issued_by(f)`) or a
/// bare one (attributed at the boundary, [`Attribution::SelfSigned`]).
#[derive(Default)]
pub struct OrderQueue(VecDeque<Command>);

impl OrderQueue {
    /// Enqueue an order for the next tick the sim drains the queue
    /// ([`CommandTick::Asap`]) — the shape input produces, since a click lands
    /// between ticks and cannot know which tick will pick it up. The sim stamps
    /// it with that tick when it applies it, and the log records the stamp.
    ///
    /// `impl Into<SignedOrder>` is the whole point: the attribution is decided
    /// here, once, for every producer.
    pub fn push_back(&mut self, order: impl Into<SignedOrder>) {
        self.0.push_back(Command {
            when: CommandTick::Asap,
            order: order.into(),
        });
    }

    /// Enqueue an order for **exactly** sim tick `at`. It is applied on that
    /// tick and no other: a tick that has already gone by drops it (see
    /// [`take_due`](Self::take_due)). This is the shape a replay feeds, and the
    /// shape a networked (M6) command takes.
    pub fn push_at(&mut self, at: u32, order: impl Into<SignedOrder>) {
        self.0.push_back(Command {
            when: CommandTick::At(at),
            order: order.into(),
        });
    }

    /// Take everything the sim should look at on tick `now`, in push order,
    /// and leave the rest.
    ///
    /// - [`CommandTick::Asap`] ⇒ taken, stamped `now`;
    /// - `At(t) == now` ⇒ taken;
    /// - `At(t) > now` ⇒ retained, in order;
    /// - `At(t) < now` ⇒ taken **and marked late**: it is removed from the
    ///   queue and never applied. Applying a command a tick late is precisely
    ///   the divergence a replay exists to rule out, so a missed tick is a lost
    ///   command, never a rescheduled one — but it is *returned* rather than
    ///   silently dropped, so the log can record that it happened. A log that
    ///   omitted it would describe a queue the replay never held.
    ///
    /// Order is preserved exactly, so what the sim does within a tick is a
    /// function of push order alone — no iteration order, no sorting by
    /// entity, nothing a `HashMap` could reach.
    pub fn take_due(&mut self, now: u32) -> Vec<TakenCommand> {
        let mut taken = Vec::new();
        let mut kept = VecDeque::with_capacity(self.0.len());
        for cmd in self.0.drain(..) {
            match cmd.when {
                CommandTick::At(t) if t > now => kept.push_back(Command {
                    when: CommandTick::At(t),
                    order: cmd.order,
                }),
                CommandTick::At(t) if t < now => taken.push(TakenCommand {
                    tick: now,
                    schedule: CommandTick::At(t),
                    late: true,
                    order: cmd.order,
                }),
                when => taken.push(TakenCommand {
                    tick: now,
                    schedule: when,
                    late: false,
                    order: cmd.order,
                }),
            }
        }
        self.0 = kept;
        taken
    }

    pub fn front(&self) -> Option<&Command> {
        self.0.front()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Command> {
        self.0.iter()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn clear(&mut self) {
        self.0.clear();
    }
}

/// When a [`Command`] is to be applied. Serializable: the command log records
/// the schedule a command carried, not only the tick it ended up on, so a
/// replay can re-push it exactly as it was pushed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CommandTick {
    /// On the next tick the sim drains the queue. Input is asynchronous — a
    /// click has no tick of its own — so the sim assigns it one, and that
    /// assignment is what the command log records.
    Asap,
    /// On exactly this sim tick, or not at all.
    At(u32),
}

/// A [`SignedOrder`] plus the tick it is to be applied on: the unit the sim
/// consumes, the command log stores, and a replay feeds back in.
///
/// (The command *enum* is [`Order`] — named that way because Bevy's prelude
/// already has a `Command` trait; `Command` is the tick-tagged envelope around
/// it, which is what "tagged with a target tick" has to mean if the tag is to
/// be unforgeable.)
#[derive(Debug)]
pub struct Command {
    when: CommandTick,
    order: SignedOrder,
}

/// A command the sim has taken off the queue for tick [`tick`](Self::tick): the
/// schedule it carried, whether it missed its tick, and the order itself.
#[derive(Debug)]
pub struct TakenCommand {
    /// The tick the sim took it on.
    pub tick: u32,
    /// The schedule it was pushed with.
    pub schedule: CommandTick,
    /// Its scheduled tick had already gone by: it is **not** applied, only
    /// recorded.
    pub late: bool,
    pub order: SignedOrder,
}

impl Command {
    pub fn when(&self) -> CommandTick {
        self.when
    }

    pub fn attribution(&self) -> Attribution {
        self.order.attribution()
    }

    pub fn issuer(&self) -> Option<Faction> {
        self.order.issuer()
    }

    pub fn order(&self) -> &Order {
        self.order.order()
    }
}

/// Who an order is to be held to. Serializable: it is what the command log
/// stores in place of the (already resolved) signature.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Attribution {
    /// Signed by this faction ([`Order::issued_by`]). Checked against
    /// everything the order touches.
    By(Faction),
    /// Unsigned: attributed to the faction of whatever it touches. Not
    /// "unchecked" — an order that touches *two* factions has no coherent
    /// self-signature and is refused whole, so a self-signed order can never
    /// command more than one commander's side.
    SelfSigned,
    /// Signed by two different factions, which is not a signature at all. Such
    /// an order is dropped: re-signing someone else's order voids it rather
    /// than laundering ownership.
    Void,
}

/// An [`Order`] together with its [`Attribution`] — the only thing the command
/// queue can hold.
#[derive(Debug)]
pub struct SignedOrder {
    attribution: Attribution,
    order: Order,
}

impl SignedOrder {
    pub fn attribution(&self) -> Attribution {
        self.attribution
    }

    /// The signing faction, if the order was explicitly signed.
    pub fn issuer(&self) -> Option<Faction> {
        match self.attribution {
            Attribution::By(f) => Some(f),
            _ => None,
        }
    }

    pub fn order(&self) -> &Order {
        &self.order
    }

    pub fn into_parts(self) -> (Attribution, Order) {
        (self.attribution, self.order)
    }

    /// Rebuild a signed order from parts — the inverse of
    /// [`into_parts`](Self::into_parts), used by a replay to re-enqueue a
    /// command with **the attribution it was recorded with** rather than one
    /// derived again from a world that has moved on. The attribution is still
    /// only ever *checked* by `apply_commands`, so this grants no authority: it
    /// carries a claim forward, it does not make one true.
    pub fn from_parts(attribution: Attribution, order: Order) -> Self {
        SignedOrder { attribution, order }
    }
}

impl From<Order> for SignedOrder {
    /// Attribute an order at the queue boundary: peel any [`Order::By`]
    /// wrappers, and refuse (as [`Attribution::Void`]) a stack of signatures
    /// that disagree.
    fn from(order: Order) -> Self {
        let mut issuer: Option<Faction> = None;
        let mut conflict = false;
        let mut inner = order;
        while let Order::By { issuer: f, order } = inner {
            conflict |= issuer.is_some_and(|prev| prev != f);
            issuer = Some(f);
            inner = *order;
        }
        let attribution = match (conflict, issuer) {
            (true, _) => Attribution::Void,
            (false, Some(f)) => Attribution::By(f),
            (false, None) => Attribution::SelfSigned,
        };
        SignedOrder {
            attribution,
            order: inner,
        }
    }
}

/// A sim-affecting order. Emitted by input (or by the scripted AI, which is
/// held to exactly the same discipline) and applied in `FixedUpdate`.
///
/// ## Ownership (M4c)
/// An order is only legitimate from the commander that owns what it touches, so
/// every order carries its **issuer**: [`Order::issued_by`] signs it, the queue
/// stores it as a [`SignedOrder`] (there is no way to enqueue an order with no
/// attribution), and [`apply_commands`] refuses anything cross-faction —
/// training from another side's building, moving another side's units, or
/// placing a building for someone else.
///
/// The signature is a wrapper variant ([`Order::By`]) rather than a field on
/// each variant, so a bare order value is still constructible; the attribution
/// is decided for it at the queue boundary ([`Attribution::SelfSigned`]).
/// Self-signed is a *checked* mode, not an exemption: the issuer is derived
/// from what the order names, and an order naming two factions' entities has no
/// coherent issuer and is refused **whole**. So no order — signed, unsigned or
/// re-signed — can ever command two sides at once.
#[derive(Debug)]
pub enum Order {
    MoveTo {
        units: Vec<Entity>,
        dest: Vec2,
    },
    Gather {
        units: Vec<Entity>,
        node: Entity,
        node_pos: Vec2,
    },
    /// Place a building (index into `Content::buildings`) — costs Alloy.
    Place {
        faction: Faction,
        building: usize,
        pos: Vec2,
    },
    /// Train a unit (index into `Content::units`) at a building — costs Alloy.
    /// The building's faction pays, and (M4c) must *be* the issuer: a commander
    /// cannot spend the enemy's Alloy or fill the enemy's queue.
    Train {
        building: Entity,
        unit: usize,
    },
    /// `order`, signed by the faction that issued it. Built with
    /// [`Order::issued_by`].
    By {
        issuer: Faction,
        order: Box<Order>,
    },
}

impl Order {
    /// Sign this order: `Order::MoveTo { .. }.issued_by(Faction::B)`.
    pub fn issued_by(self, issuer: Faction) -> Order {
        Order::By {
            issuer,
            order: Box::new(self),
        }
    }

}

/// Counters for the once-per-second sim-tick vs. frame report. `sim_ticks` is
/// bumped in the sim; `frames`/`elapsed` are driven by the render-side reporter.
#[derive(Resource, Default)]
pub struct RateReport {
    pub sim_ticks: u32,
    pub frames: u32,
    pub elapsed: f32,
}

// ---- systems ---------------------------------------------------------------

/// Drain the commands **due on this tick**, turning intents into per-entity sim
/// components. Runs before [`movement`] so orders take effect on the same tick.
///
/// This is the only place a command is ever applied, and it is only ever
/// reached from the sim chain (`FixedUpdate` in the shipped app), so "applied
/// on tick N" is a property of the sim's own tick counter and not of when the
/// order happened to be pushed. Each command consumed is appended to the
/// [`replay::CommandLog`] with the tick it applied on — including the ones
/// refused for ownership or cost, because the log records what the sim was
/// *asked* to do, which is what a replay must feed back.
///
/// [`MatchState`] (the tick counter) and the [`replay::CommandLog`] are taken as
/// `Option`, and both are installed by `add_sim_systems` with the chain: an app
/// that composes this system by hand — the M4a fixtures do, deliberately, to
/// test it in isolation — still applies its unscheduled orders on the tick it
/// runs. What no *shipped* configuration can do is lose them, because the one
/// definition of the chain installs both alongside it (F-004).
#[allow(clippy::too_many_arguments)]
pub fn apply_commands(
    mut queue: ResMut<CommandQueue>,
    state: Option<Res<MatchState>>,
    log: Option<ResMut<replay::CommandLog>>,
    content: Res<Content>,
    mut stock: ResMut<Stockpiles>,
    mut producers: Query<(&Building, &Faction, &mut ProductionQueue)>,
    defs: Query<&UnitDefIdx>,
    owners: Query<&Faction>,
    // **`Res`, not `ResMut`**: naming a thing must never create one. The record
    // path mutating the registry is exactly the desync the replay path cannot
    // reproduce (see the comment in the loop below).
    ids: Option<Res<replay::SimIds>>,
    mut commands: Commands,
) {
    let now = state.map(|s| s.tick()).unwrap_or(0);
    let mut log = log;
    let due = queue.0.take_due(now);
    if let Some(log) = log.as_mut() {
        log.record_late(due.iter().filter(|c| c.late).count() as u32);
    }
    for taken in due {
        let TakenCommand {
            tick,
            schedule,
            late,
            order: signed,
        } = taken;
        // Ownership first. `Void` is an order nobody can be held to (signed by
        // two different factions); everything else yields the faction the rest
        // of this loop checks against.
        let (attribution, cmd) = signed.into_parts();

        // **The sim commands only what it can name.** An entity the registry
        // has never issued an id for has no name in the sim's own coordinates:
        // it cannot be written to the log, and a replay would have nothing to
        // point at. Such a name is therefore dropped from the order — and an
        // order whose *single* target is unnameable is refused whole — before
        // anything is logged or applied, so the recording and the replay of its
        // log do exactly the same thing.
        //
        // The alternative shapes both fail: naming it `UNIDENTIFIED` writes a
        // log the sim's own validator refuses (pass-2 F3), and *issuing* it an
        // id here makes writing the log mutate the registry — hashed state the
        // replay path never mutates, which desyncs a replay of a log that saved
        // perfectly well (pass-3). Ids are issued in exactly one place,
        // `replay::identify`, which both paths run identically.
        //
        // Whether an entity is nameable is decided by the registry alone, never
        // by whether a log happens to be present, so logging can never change
        // what the sim does. A hand-composed app with no registry (the M4a
        // fixtures) names nothing and refuses nothing: it applies its orders as
        // it always did and records none of them, counted in
        // `CommandLog::unrecorded`.
        let cmd = match ids.as_ref() {
            None => cmd,
            Some(ids) => {
                let mut dropped = 0u32;
                let named = nameable(ids, &content, cmd, &mut dropped);
                if let Some(log) = log.as_mut() {
                    log.record_unnameable(dropped + u32::from(named.is_none()));
                }
                match named {
                    Some(cmd) => cmd,
                    None => continue,
                }
            }
        };

        // Logged by `SimId`, the sim's own coordinate — an `Entity` is an
        // allocation detail that does not survive into another app. The id
        // comes from the **registry**, not from the entity's `SimId` component,
        // because an order can name an entity that has already been despawned:
        // the component died with it, the registry entry did not.
        let fate = if late {
            replay::CommandFate::Late
        } else {
            replay::CommandFate::Taken
        };
        match (log.as_mut(), ids.as_ref()) {
            (Some(log), Some(ids)) => {
                log.record(tick, schedule, fate, attribution, &cmd, &content, |e| {
                    ids.id_of(e).unwrap_or(replay::SimId::UNIDENTIFIED)
                })
            }
            (Some(log), None) => log.record_unrecordable(),
            (None, _) => {}
        }
        // Recorded, but **not applied**: a command that missed its tick is part
        // of the account of the match, not part of what the match did.
        if late {
            continue;
        }
        if attribution == Attribution::Void {
            continue;
        }
        match cmd {
            Order::MoveTo { units, dest } => {
                // An unsigned order is self-signed *coherently* or not at all:
                // if it names entities of two factions there is no commander it
                // could have come from, so the whole order is refused rather
                // than half-applied.
                let Some(issuer) = subject_issuer(&owners, attribution, &units) else {
                    continue;
                };
                for e in units {
                    // Two guards, and both are load-bearing once a *second*
                    // commander exists. `commandable` refuses another faction's
                    // unit; `get_entity` refuses an entity that is already gone
                    // (an AI issues orders against entities it remembered, and
                    // `Commands::entity` on a despawned entity panics).
                    if !commandable(&owners, issuer, e) {
                        continue;
                    }
                    let Ok(mut ent) = commands.get_entity(e) else {
                        continue;
                    };
                    // A move order cancels gathering, but a carried load is
                    // kept (it stays "in flight" — Alloy is never destroyed).
                    // It also cancels an auto-engagement: `Engaging` marks a
                    // move *combat* issued, so leaving it on would let the same
                    // tick's combat pass overwrite the commander's destination.
                    // Orders come from the commander; the sim only ever
                    // auto-chases a unit that has none.
                    ent.try_insert(MoveTarget(dest)).remove::<Engaging>();
                    economy::release_gather_job(&mut ent);
                }
            }
            Order::Gather {
                units,
                node,
                node_pos,
            } => {
                let Some(issuer) = subject_issuer(&owners, attribution, &units) else {
                    continue;
                };
                for e in units {
                    if !commandable(&owners, issuer, e) {
                        continue;
                    }
                    // A gather job is only ever handed to a unit whose
                    // definition says it gathers. Right-clicking a deposit with
                    // a mixed selection sends this order to soldiers too; giving
                    // them a `GatherTarget` they can never act on would leave
                    // the marker stuck on them forever (`economy::gather` has no
                    // reason to touch a non-gatherer), and anything that reads
                    // it — combat's "the economy owns this unit" rule — would
                    // then be reading a lie. A unit with no definition is not
                    // tasked either: the sim only gives jobs it has data for.
                    let gathers = defs
                        .get(e)
                        .ok()
                        .and_then(|d| content.units.get(d.0))
                        .is_some_and(|def| def.gathers);
                    let Ok(mut ent) = commands.get_entity(e) else {
                        continue;
                    };
                    // The move half of the order applies to everyone in the
                    // selection, and — being an explicit order — ends any chase.
                    ent.try_insert(MoveTarget(node_pos)).remove::<Engaging>();
                    if gathers {
                        // `insert_if_new` on `Carrying` so re-tasking a worker
                        // that is already holding a load never zeroes it. The
                        // claim is written as a pair (F-008) — `GatherPhase` is
                        // a required component of `GatherTarget`, and is named
                        // here too so the phase is an explicit `ToNode` rather
                        // than a default nobody reads.
                        ent.try_insert((GatherTarget(node), GatherPhase::ToNode))
                            .try_insert_if_new(Carrying(0));
                    }
                }
            }

            // Spending orders. Both charge Alloy in exactly one place
            // (`Stockpiles::try_spend`); a rejected order changes nothing.
            Order::Place {
                faction,
                building,
                pos,
            } => {
                // A commander places buildings for itself only — the order names
                // the faction that gets (and pays for) the building, so it must
                // be the faction that signed it. (Unsigned: the named faction is
                // itself the self-signature, and there is only one of them.)
                if matches!(attribution, Attribution::By(by) if by != faction) {
                    continue;
                }
                economy::place_building(
                    &content,
                    &mut stock,
                    &mut commands,
                    faction,
                    building,
                    pos,
                );
            }
            Order::Train { building, unit } => {
                // The building's faction pays, so only the building's faction
                // may order: this is what stops one side spending the other's
                // Alloy (and filling their queue).
                if let Ok((b, faction, mut queue)) = producers.get_mut(building) {
                    if matches!(attribution, Attribution::By(by) if by != *faction) {
                        continue;
                    }
                    economy::enqueue_unit(&content, &mut stock, b.def, *faction, &mut queue, unit);
                }
            }
            // Peeled off above; `signed` never returns a wrapper.
            Order::By { .. } => {}
        }
    }
}

/// Reduce an order to the part of it the sim can **name**, or `None` if there is
/// nothing left to obey.
///
/// A name is a [`replay::SimId`], and the registry issues those in exactly one
/// place ([`replay::identify`], for everything in the world). An entity the
/// registry does not know is something the sim has never had in its world — a
/// bare fixture entity, say — so:
///
/// - in a list order (`MoveTo`, `Gather`) the unnameable entries are **dropped**
///   and the rest of the order stands, exactly as a signed order naming another
///   faction's units still commands the issuer's own (F-009);
/// - an order whose single subject is unnameable (`Gather`'s node, `Train`'s
///   building) is **refused whole**, since there is nothing left of it;
/// - `Place` names no entity, but it does name a **building definition**, and
///   `Train` names a unit definition: an index this build's content does not
///   have is refused the same way, because the log names content by id and
///   there would be no id to write.
///
/// `dropped` accumulates the names that could not be resolved, for the log's
/// diagnostic counter.
fn nameable(
    ids: &replay::SimIds,
    content: &Content,
    order: Order,
    dropped: &mut u32,
) -> Option<Order> {
    let keep = |units: Vec<Entity>, dropped: &mut u32| -> Vec<Entity> {
        let before = units.len();
        let kept: Vec<Entity> = units
            .into_iter()
            .filter(|e| ids.id_of(*e).is_some())
            .collect();
        *dropped = dropped.saturating_add((before - kept.len()) as u32);
        kept
    };
    Some(match order {
        Order::MoveTo { units, dest } => Order::MoveTo {
            units: keep(units, dropped),
            dest,
        },
        Order::Gather {
            units,
            node,
            node_pos,
        } => {
            ids.id_of(node)?;
            Order::Gather {
                units: keep(units, dropped),
                node,
                node_pos,
            }
        }
        // Content, too, is a thing an order names. An index this build does not
        // have cannot be written into the log (the log names content by id, so
        // there would be no id to write) and does nothing if applied, so it is
        // refused here — one place, before anything is logged or applied,
        // exactly like an unnameable entity.
        Order::Place {
            faction,
            building,
            pos,
        } => {
            content.buildings.get(building)?;
            Order::Place {
                faction,
                building,
                pos,
            }
        }
        Order::Train { building, unit } => {
            ids.id_of(building)?;
            content.units.get(unit)?;
            Order::Train { building, unit }
        }
        // Peeled at the queue boundary; never reaches here.
        Order::By { issuer, order } => Order::By { issuer, order },
    })
}

/// The faction an entity-list order is to be checked against, or `None` if the
/// order must be refused outright.
///
/// - Signed: the signer, always.
/// - Unsigned: derived from the units it names. One faction (plus any unowned
///   entities) ⇒ that faction. **Two factions ⇒ refused**: there is no commander
///   who could have issued it, and half-applying it would let one order command
///   both sides — which is exactly what "self-signed" must not mean.
/// - `Some(None)`: the order names nothing owned by anybody (bare test
///   entities, a resource node), so there is no ownership to violate.
fn subject_issuer(
    owners: &Query<&Faction>,
    attribution: Attribution,
    units: &[Entity],
) -> Option<Option<Faction>> {
    match attribution {
        Attribution::Void => None,
        Attribution::By(f) => Some(Some(f)),
        Attribution::SelfSigned => {
            let mut seen: Option<Faction> = None;
            for e in units {
                let Ok(f) = owners.get(*e) else { continue };
                match seen {
                    None => seen = Some(*f),
                    Some(prev) if prev != *f => return None,
                    _ => {}
                }
            }
            Some(seen)
        }
    }
}

/// May `issuer` command `e`? Only if `e` is not somebody else's. `None` means
/// the order named nothing owned (see [`subject_issuer`]); an entity with no
/// `Faction` (a resource node, or a bare test entity) belongs to nobody, so no
/// commander is overriding another by touching it. Whether the entity *exists*
/// is a separate question, asked with `Commands::get_entity`.
fn commandable(owners: &Query<&Faction>, issuer: Option<Faction>, e: Entity) -> bool {
    match issuer {
        None => true,
        Some(by) => owners.get(e).map(|f| *f == by).unwrap_or(true),
    }
}

/// Advance `pos` toward `target` by at most `step`. Returns the new position and
/// whether the unit arrived (within one step or `STOP_EPS`). Pure — unit-tested.
pub fn step_toward(pos: Vec2, target: Vec2, step: f32) -> (Vec2, bool) {
    let to = target - pos;
    let dist = to.length();
    if dist <= step.max(STOP_EPS) {
        (target, true)
    } else {
        (pos + to / dist * step, false)
    }
}

/// Integrate units toward their `MoveTarget`, snapping and stopping on arrival.
/// Each unit moves at **its own** speed — `speed * mvp_combat.speed_per_point`
/// from `units.ron` (M4b) — so a Bulwark and a Ripper cover different ground in
/// the same tick. `delta_secs` is the fixed timestep by construction (F-003).
pub fn movement(
    time: Res<Time<Fixed>>,
    content: Res<Content>,
    mut report: ResMut<RateReport>,
    mut query: Query<(Entity, &mut Position, &MoveTarget, &UnitDefIdx)>,
    mut commands: Commands,
) {
    report.sim_ticks += 1;
    let dt = time.delta_secs();

    for (e, mut pos, target, def) in &mut query {
        let step = combat::move_speed(&content, def.0) * dt;
        let (next, arrived) = step_toward(pos.0, target.0, step);
        pos.0 = next;
        if arrived {
            commands.entity(e).remove::<MoveTarget>();
        }
    }
}

// L1 unit test: the movement math in isolation (see BUILD_PLAN.md / TESTING).
#[cfg(test)]
mod tests {
    use super::step_toward;
    use bevy::math::Vec2;

    #[test]
    fn step_toward_snaps_on_arrival() {
        // Overshoot within one step → snap to target and report arrival.
        let (pos, arrived) = step_toward(Vec2::ZERO, Vec2::new(1.0, 0.0), 100.0);
        assert!(arrived && pos == Vec2::new(1.0, 0.0));

        // Far away → advance exactly `step` along the direction, not arrived.
        let (pos, arrived) = step_toward(Vec2::ZERO, Vec2::new(100.0, 0.0), 10.0);
        assert!(!arrived);
        assert!((pos.x - 10.0).abs() < 1e-4 && pos.y.abs() < 1e-4);
    }
}
