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
pub mod spatial;
pub mod victory;
pub use ai::{AiAction, AiCommander, AiCommanders, AiJournal};
pub use combat::{AttackCooldown, Casualties, Engaging, Health, Target};
pub use content::Content;
pub use economy::{
    Building, Carrying, GatherPhase, ProductionQueue, QueuedUnit, Stockpiles, UnitDefIdx,
};
pub use pathfind::{astar, FlowField, TileGrid};
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
///   [`economy::repair_gather_claims`], which runs before the tick's gather and
///   combat passes. A half-claim therefore cannot survive into any reader.
#[derive(Component)]
#[require(GatherPhase)]
pub struct GatherTarget(pub Entity);

// ---- resources -------------------------------------------------------------

/// Player intents awaiting application in `FixedUpdate`. The seed of M5's
/// tick-tagged, loggable command stream. (Named `Order` to avoid Bevy's
/// prelude `Command` trait.)
#[derive(Resource, Default)]
pub struct CommandQueue(pub OrderQueue);

/// The queue itself. It holds [`SignedOrder`]s, **not** bare [`Order`]s: the
/// element type is the witness that every order in flight has an attribution the
/// sim will check it against. Pushing takes anything that can become one, so a
/// caller may push a signed order (`.issued_by(f)`) or a bare order (which is
/// attributed at the boundary, [`Attribution::SelfSigned`]) — what it cannot do
/// is enqueue an order with no attribution at all, because no such value exists
/// on the other side of `push_back`.
#[derive(Default)]
pub struct OrderQueue(VecDeque<SignedOrder>);

impl OrderQueue {
    /// Enqueue an order. `impl Into<SignedOrder>` is the whole point: the
    /// attribution is decided here, once, for every producer.
    pub fn push_back(&mut self, order: impl Into<SignedOrder>) {
        self.0.push_back(order.into());
    }

    pub fn pop_front(&mut self) -> Option<SignedOrder> {
        self.0.pop_front()
    }

    pub fn front(&self) -> Option<&SignedOrder> {
        self.0.front()
    }

    pub fn iter(&self) -> impl Iterator<Item = &SignedOrder> {
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

/// Who an order is to be held to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

/// Drain the command queue, turning intents into per-entity sim components.
/// Runs before [`movement`] so orders take effect on the same tick.
pub fn apply_commands(
    mut queue: ResMut<CommandQueue>,
    content: Res<Content>,
    mut stock: ResMut<Stockpiles>,
    mut producers: Query<(&Building, &Faction, &mut ProductionQueue)>,
    defs: Query<&UnitDefIdx>,
    owners: Query<&Faction>,
    mut commands: Commands,
) {
    while let Some(signed) = queue.0.pop_front() {
        // Ownership first. `Void` is an order nobody can be held to (signed by
        // two different factions); everything else yields the faction the rest
        // of this loop checks against.
        let (attribution, cmd) = signed.into_parts();
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
