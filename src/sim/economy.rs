//! The Alloy economy (M4a): per-faction stockpiles and the worker
//! gather/deposit loop. **Render-free** — ECS + math + `Content` only.
//!
//! Every number this module uses (load size, harvest duration, interaction
//! radii, costs) comes from `assets/data/*.ron` via [`Content`]; there are no
//! economy constants in Rust.
//!
//! ## Conservation
//! Alloy is only ever *moved*, never minted: a harvest subtracts exactly what it
//! adds to the worker's [`Carrying`], and a deposit moves exactly that into the
//! faction [`Stockpiles`]. So at every tick
//! `stockpile + carried + still-in-deposit` is invariant.
//!
//! ## Determinism
//! No wall-clock and no hash maps: stockpiles are a fixed-size array indexed by
//! faction, definitions are looked up by index into the stable RON order, and
//! wherever the outcome could depend on which worker acts first (two workers
//! draining the last of a deposit) the entities are processed in ascending
//! entity-index order rather than in query/archetype order.

use bevy::ecs::prelude::*;
use bevy::math::Vec2;
use std::collections::VecDeque;

use crate::sim::content::Content;
use crate::sim::spatial::Faction;
use crate::sim::{GatherTarget, MoveTarget, Position, ResourceNode};

// ---- stockpiles ------------------------------------------------------------

fn faction_slot(f: Faction) -> usize {
    match f {
        Faction::A => 0,
        Faction::B => 1,
    }
}

/// Banked Alloy per faction. A fixed-size array (not a map) so iteration order
/// can never influence a sim outcome.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Stockpiles {
    alloy: [u32; 2],
}

impl Stockpiles {
    /// Start both factions at `amount` Alloy (match setup).
    pub fn starting(amount: u32) -> Self {
        Self {
            alloy: [amount, amount],
        }
    }

    pub fn alloy(&self, f: Faction) -> u32 {
        self.alloy[faction_slot(f)]
    }

    /// Bank `amount` (a deposit). Saturating: totals never wrap.
    pub fn add(&mut self, f: Faction, amount: u32) {
        let slot = &mut self.alloy[faction_slot(f)];
        *slot = slot.saturating_add(amount);
    }

    /// All-or-nothing spend. Returns `true` iff the faction could afford it and
    /// the Alloy was deducted — the single place a cost is ever charged, so a
    /// rejected order costs nothing and an accepted one is charged exactly once.
    #[must_use]
    pub fn try_spend(&mut self, f: Faction, amount: u32) -> bool {
        let slot = &mut self.alloy[faction_slot(f)];
        if *slot >= amount {
            *slot -= amount;
            true
        } else {
            false
        }
    }
}

// ---- components ------------------------------------------------------------

/// Which [`Content::units`] definition this entity is (index into the stable RON
/// order). The data-driven replacement for hardcoded per-unit stats.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnitDefIdx(pub usize);

/// Alloy currently in a worker's hands (in flight — not yet banked).
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Carrying(pub u32);

/// Where a gathering worker is in its loop.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub enum GatherPhase {
    /// Heading to the assigned deposit.
    ToNode,
    /// Standing at the deposit, mining one load.
    Harvesting { ticks_left: u32 },
    /// Heading back to a drop-off building with a load.
    ToDropoff,
}

/// A constructed building; `def` indexes [`Content::buildings`].
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Building {
    pub def: usize,
}

/// One unit being trained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueuedUnit {
    /// Index into [`Content::units`].
    pub unit: usize,
    pub ticks_left: u32,
}

/// A building's production queue. Costs are charged when an item is *enqueued*,
/// never again when it completes.
#[derive(Component, Debug, Default)]
pub struct ProductionQueue {
    pub items: VecDeque<QueuedUnit>,
}

// ---- the gather loop -------------------------------------------------------

/// Nearest drop-off building of `faction`, by squared distance with the entity
/// index as the tie-break (so the choice never depends on query order).
fn nearest_dropoff(
    content: &Content,
    faction: Faction,
    from: Vec2,
    dropoffs: &[(Entity, Vec2, usize, Faction)],
) -> Option<Vec2> {
    dropoffs
        .iter()
        .filter(|(_, _, def, f)| {
            *f == faction && content.buildings.get(*def).is_some_and(|b| b.dropoff)
        })
        .min_by(|a, b| {
            let da = a.1.distance_squared(from);
            let db = b.1.distance_squared(from);
            da.partial_cmp(&db)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.0.to_bits().cmp(&b.0.to_bits()))
        })
        .map(|(_, pos, _, _)| *pos)
}

/// Drive every gathering worker one tick through walk → harvest → return →
/// deposit. Runs in `FixedUpdate` before [`crate::sim::movement`] so the
/// `MoveTarget` it sets is followed on the same tick.
#[allow(clippy::type_complexity)]
pub fn gather(
    content: Res<Content>,
    mut stock: ResMut<Stockpiles>,
    mut nodes: Query<(&Position, &mut ResourceNode)>,
    buildings: Query<(Entity, &Position, &Building, &Faction)>,
    mut workers: Query<(
        Entity,
        &Position,
        &UnitDefIdx,
        &Faction,
        &GatherTarget,
        &mut Carrying,
        &mut GatherPhase,
    )>,
    mut commands: Commands,
) {
    let gather_range = content.economy.gather_range;
    let deposit_range = content.economy.deposit_range;

    let dropoffs: Vec<(Entity, Vec2, usize, Faction)> = buildings
        .iter()
        .map(|(e, p, b, f)| (e, p.0, b.def, *f))
        .collect();

    // Stable processing order: ascending entity index, independent of archetype
    // layout. Matters when two workers race for the last Alloy in a deposit.
    let mut acting: Vec<Entity> = workers.iter().map(|(e, ..)| e).collect();
    acting.sort_unstable_by_key(|e| e.to_bits());

    for entity in acting {
        let Ok((_, pos, def_idx, faction, target, mut carrying, mut phase)) =
            workers.get_mut(entity)
        else {
            continue;
        };
        let Some(def) = content.units.get(def_idx.0) else {
            continue;
        };
        if !def.gathers {
            continue;
        }
        let (pos, faction, node_entity) = (pos.0, *faction, target.0);

        // The assigned deposit may have been consumed/despawned: drop the job.
        let node_pos = match nodes.get(node_entity) {
            Ok((p, _)) => Some(p.0),
            Err(_) => None,
        };

        match *phase {
            GatherPhase::ToNode => {
                let Some(node_pos) = node_pos else {
                    commands
                        .entity(entity)
                        .remove::<GatherTarget>()
                        .remove::<GatherPhase>();
                    continue;
                };
                if pos.distance(node_pos) <= gather_range {
                    *phase = GatherPhase::Harvesting {
                        ticks_left: def.mvp_gather_ticks,
                    };
                    commands.entity(entity).remove::<MoveTarget>();
                } else {
                    commands.entity(entity).insert(MoveTarget(node_pos));
                }
            }

            GatherPhase::Harvesting { ticks_left } => {
                if ticks_left > 0 {
                    *phase = GatherPhase::Harvesting {
                        ticks_left: ticks_left - 1,
                    };
                    continue;
                }
                // Move Alloy out of the deposit into the worker's hands — the
                // same number leaves the node as enters `Carrying`.
                let room = def.mvp_carry_capacity.saturating_sub(carrying.0);
                let taken = match nodes.get_mut(node_entity) {
                    Ok((_, mut node)) => {
                        let taken = room.min(node.amount);
                        node.amount -= taken;
                        taken
                    }
                    Err(_) => 0,
                };
                carrying.0 += taken;

                if carrying.0 > 0 {
                    *phase = GatherPhase::ToDropoff;
                } else {
                    // Nothing left here and nothing in hand: the job is over.
                    commands
                        .entity(entity)
                        .remove::<GatherTarget>()
                        .remove::<GatherPhase>();
                }
            }

            GatherPhase::ToDropoff => {
                let Some(drop_pos) = nearest_dropoff(&content, faction, pos, &dropoffs) else {
                    continue; // No drop-off yet — hold the load (still conserved).
                };
                if pos.distance(drop_pos) <= deposit_range {
                    stock.add(faction, carrying.0);
                    carrying.0 = 0;
                    match node_pos {
                        Some(_) => *phase = GatherPhase::ToNode,
                        None => {
                            commands
                                .entity(entity)
                                .remove::<GatherTarget>()
                                .remove::<GatherPhase>();
                        }
                    }
                    commands.entity(entity).remove::<MoveTarget>();
                } else {
                    commands.entity(entity).insert(MoveTarget(drop_pos));
                }
            }
        }
    }
}

// ---- spending: placement and production ------------------------------------

/// Apply a "place this building" order. The Alloy is charged in exactly one
/// place ([`Stockpiles::try_spend`], all-or-nothing) and only then is the
/// building spawned, so a rejected order costs nothing and an accepted one is
/// never charged twice.
pub fn place_building(
    content: &Content,
    stock: &mut Stockpiles,
    commands: &mut Commands,
    faction: Faction,
    building: usize,
    pos: Vec2,
) -> bool {
    let Some(def) = content.buildings.get(building) else {
        return false;
    };
    if !stock.try_spend(faction, def.alloy_cost) {
        return false;
    }
    commands.spawn((
        Position(pos),
        Building { def: building },
        faction,
        ProductionQueue::default(),
    ));
    true
}

/// Apply a "train this unit here" order: charge once, then enqueue. Rejected
/// (unknown ids, a building that doesn't produce the unit, or not enough Alloy)
/// ⇒ nothing spent and nothing queued.
pub fn enqueue_unit(
    content: &Content,
    stock: &mut Stockpiles,
    building_def: usize,
    faction: Faction,
    queue: &mut ProductionQueue,
    unit: usize,
) -> bool {
    let Some(def) = content.units.get(unit) else {
        return false;
    };
    if !content.produces(building_def, unit) {
        return false;
    }
    if !stock.try_spend(faction, def.mvp_alloy_cost) {
        return false;
    }
    queue.items.push_back(QueuedUnit {
        unit,
        ticks_left: def.mvp_train_ticks,
    });
    true
}

/// Advance each building's production queue by one tick and spawn the head item
/// when its timer runs out. **Never charges Alloy** — the cost was taken when
/// the item was enqueued, so a unit is paid for exactly once.
pub fn production(
    content: Res<Content>,
    mut buildings: Query<(Entity, &Position, &Faction, &mut ProductionQueue)>,
    mut commands: Commands,
) {
    // Stable order (ascending entity index) so spawn order never depends on
    // archetype/query layout.
    let mut ordered: Vec<Entity> = buildings.iter().map(|(e, ..)| e).collect();
    ordered.sort_unstable_by_key(|e| e.to_bits());

    for entity in ordered {
        let Ok((_, pos, faction, mut queue)) = buildings.get_mut(entity) else {
            continue;
        };
        let (pos, faction) = (pos.0, *faction);
        let Some(head) = queue.items.front_mut() else {
            continue;
        };
        if head.ticks_left > 0 {
            head.ticks_left -= 1;
            continue;
        }
        let done = queue.items.pop_front().expect("head exists");
        let spawn_pos = pos + rally_offset(&content, done.unit);
        let mut spawned = commands.spawn((Position(spawn_pos), UnitDefIdx(done.unit), faction));
        if content.units.get(done.unit).is_some_and(|u| u.gathers) {
            spawned.insert(Carrying(0));
        }
    }
}

/// Where a finished unit appears relative to its producer: just outside the
/// drop-off radius, on a fixed spoke chosen from the unit's definition index —
/// deterministic, with no RNG and no clock.
fn rally_offset(content: &Content, unit: usize) -> Vec2 {
    let spokes = content.units.len().max(1);
    let angle = std::f32::consts::TAU * (unit % spokes) as f32 / spokes as f32;
    Vec2::new(angle.cos(), angle.sin()) * (content.economy.deposit_range + 1.0)
}
