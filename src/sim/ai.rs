//! The scripted AI commander (M4c). **Render-free** — ECS + math + [`Content`].
//!
//! ## It plays by the player's rules
//! The AI mutates no sim state. It reads the world and pushes [`Order`]s onto
//! the same [`CommandQueue`] the mouse writes to, signed with its own faction
//! ([`Order::issued_by`]), and `apply_commands` charges, refuses and applies
//! them exactly as it does the player's. So anything the AI can do, a player
//! could have done — there is no privileged path into the sim.
//!
//! ## It counts ticks, never seconds
//! Every timer here is a `FixedUpdate` tick counter carried in the commander's
//! own state. No `Time`, no wall-clock, no `Instant` — which is also why an AI
//! match replays identically at any frame rate.
//!
//! ## Determinism
//! - The script (build order, thresholds, timings) is data: `mvp_ai` in
//!   `units.ron`. There are no AI constants in Rust.
//! - The only randomness is [`SplitMix64`], seeded per commander from the match
//!   seed and stepped **only here**, inside the sim.
//! - Every world scan is snapshotted into a `Vec` sorted by ascending
//!   `Entity::to_bits()` before a decision is taken, so no query/archetype order
//!   can reach an outcome. No `HashMap` is involved anywhere.
//! - Commanders act in a fixed order (faction slot), one decision per
//!   `think_interval_ticks`.
//!
//! Every order the AI issues is also appended to [`AiJournal`] — the AI's action
//! trace, which is what makes "same seed ⇒ same actions" a statement about the
//! *decisions* rather than about the state they happen to produce.

use bevy::ecs::prelude::*;
use bevy::math::Vec2;

use crate::sim::content::Content;
use crate::sim::economy::{Building, ProductionQueue, Stockpiles, UnitDefIdx};
use crate::sim::spatial::{Faction, SplitMix64};
use crate::sim::{CommandQueue, GatherTarget, Order, Position, ResourceNode};

// ---- the action trace -------------------------------------------------------

/// One decision a commander took. Copy + comparable, so two runs' traces can be
/// compared directly (the AC1 determinism probe).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AiAction {
    /// Sent an idle gatherer to a deposit.
    Gather { unit: Entity, node: Entity },
    /// Queued a worker at its HQ.
    TrainWorker { at: Entity },
    /// Placed its barracks.
    PlaceBarracks { pos: Vec2 },
    /// Queued the next unit of the army build order (index into
    /// [`Content::units`]).
    TrainArmy { unit: usize },
    /// Sent `force` combat units at the enemy HQ.
    Attack { dest: Vec2, force: u32 },
}

/// Every action every AI commander has taken, in order. Sim-owned state (so it
/// is deterministic and replayable), appended to only by [`ai_commanders`].
#[derive(Resource, Debug, Default)]
pub struct AiJournal(pub Vec<(u32, Faction, AiAction)>);

impl AiJournal {
    /// The actions of one commander, tick-tagged.
    pub fn for_faction(&self, f: Faction) -> Vec<(u32, AiAction)> {
        self.0
            .iter()
            .filter(|(_, who, _)| *who == f)
            .map(|(t, _, a)| (*t, *a))
            .collect()
    }
}

// ---- commander state --------------------------------------------------------

/// One scripted commander's private state. Nothing here is world state — the
/// world is read fresh every decision — only the timers and cursors the script
/// needs, plus its own RNG stream.
#[derive(Debug, Clone)]
pub struct AiCommander {
    pub faction: Faction,
    rng: SplitMix64,
    /// Ticks this commander has been alive (its only clock).
    tick: u32,
    /// How far into the repeating army build order it is.
    army_cursor: u32,
    /// Earliest tick the next attack wave may go out.
    next_attack_tick: u32,
}

impl AiCommander {
    /// A commander for `faction`, with an RNG stream derived from the match
    /// `seed` and its faction slot — so one seed fixes both sides' behaviour,
    /// and the two sides do not share a stream.
    pub fn new(faction: Faction, seed: u64) -> Self {
        let slot = faction_slot(faction) as u64;
        Self {
            faction,
            rng: SplitMix64::new(seed ^ slot.wrapping_mul(0x9E37_79B9_7F4A_7C15)),
            tick: 0,
            army_cursor: 0,
            next_attack_tick: 0,
        }
    }

    pub fn tick(&self) -> u32 {
        self.tick
    }
}

fn faction_slot(f: Faction) -> usize {
    match f {
        Faction::A => 0,
        Faction::B => 1,
    }
}

/// The scripted commanders on the field. Empty by default — a match with no AI
/// (every M1–M4b test) runs the same chain and simply has nobody to think.
#[derive(Resource, Debug, Default)]
pub struct AiCommanders(Vec<AiCommander>);

impl AiCommanders {
    /// Commanders for `factions`, all seeded from one match `seed`. Stored in
    /// faction-slot order, which is the order they act in: never insertion
    /// order, and never a map's.
    pub fn new(seed: u64, factions: &[Faction]) -> Self {
        let mut cs: Vec<AiCommander> = factions
            .iter()
            .map(|f| AiCommander::new(*f, seed))
            .collect();
        cs.sort_by_key(|c| faction_slot(c.faction));
        cs.dedup_by_key(|c| faction_slot(c.faction));
        Self(cs)
    }

    pub fn commanders(&self) -> &[AiCommander] {
        &self.0
    }
}

// ---- world snapshot ---------------------------------------------------------

struct UnitRow {
    entity: Entity,
    pos: Vec2,
    def: usize,
    faction: Faction,
    on_a_job: bool,
}

struct BuildingRow {
    entity: Entity,
    pos: Vec2,
    def: usize,
    faction: Faction,
    queued: usize,
}

/// Nearest of `candidates` to `from`, ties broken by ascending entity index so
/// the choice never depends on query order.
fn nearest(candidates: &[(Entity, Vec2)], from: Vec2) -> Option<(Entity, Vec2)> {
    candidates
        .iter()
        .min_by(|a, b| {
            let (da, db) = (a.1.distance_squared(from), b.1.distance_squared(from));
            da.partial_cmp(&db)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.0.to_bits().cmp(&b.0.to_bits()))
        })
        .copied()
}

// ---- the system -------------------------------------------------------------

/// One tick of every scripted commander. Runs **first** in the sim chain, so the
/// orders it issues are applied by `apply_commands` on the same tick — exactly
/// like a click that lands before the tick boundary.
#[allow(clippy::too_many_arguments)]
pub fn ai_commanders(
    content: Res<Content>,
    stock: Res<Stockpiles>,
    mut ai: ResMut<AiCommanders>,
    mut journal: ResMut<AiJournal>,
    mut queue: ResMut<CommandQueue>,
    units: Query<(
        Entity,
        &Position,
        &UnitDefIdx,
        &Faction,
        Option<&GatherTarget>,
    )>,
    buildings: Query<(Entity, &Position, &Building, &Faction, &ProductionQueue)>,
    nodes: Query<(Entity, &Position), With<ResourceNode>>,
) {
    if ai.0.is_empty() {
        return;
    }

    // ---- snapshot, in a stable order ---------------------------------------
    let mut unit_rows: Vec<UnitRow> = units
        .iter()
        .map(|(entity, pos, def, faction, job)| UnitRow {
            entity,
            pos: pos.0,
            def: def.0,
            faction: *faction,
            on_a_job: job.is_some(),
        })
        .collect();
    unit_rows.sort_unstable_by_key(|r| r.entity.to_bits());

    let mut building_rows: Vec<BuildingRow> = buildings
        .iter()
        .map(|(entity, pos, b, faction, q)| BuildingRow {
            entity,
            pos: pos.0,
            def: b.def,
            faction: *faction,
            queued: q.items.len(),
        })
        .collect();
    building_rows.sort_unstable_by_key(|r| r.entity.to_bits());

    let mut node_rows: Vec<(Entity, Vec2)> = nodes.iter().map(|(e, p)| (e, p.0)).collect();
    node_rows.sort_unstable_by_key(|(e, _)| e.to_bits());

    for commander in ai.0.iter_mut() {
        let tick = commander.tick;
        commander.tick = commander.tick.saturating_add(1);
        if tick % content.ai.think_interval_ticks != 0 {
            continue;
        }
        think(
            commander,
            tick,
            &content,
            &stock,
            &unit_rows,
            &building_rows,
            &node_rows,
            &mut queue,
            &mut journal,
        );
    }
}

/// One decision by one commander. Split out so the whole script reads top to
/// bottom: mine, replace workers, open the barracks, build the army, attack.
#[allow(clippy::too_many_arguments)]
fn think(
    c: &mut AiCommander,
    tick: u32,
    content: &Content,
    stock: &Stockpiles,
    units: &[UnitRow],
    buildings: &[BuildingRow],
    nodes: &[(Entity, Vec2)],
    queue: &mut CommandQueue,
    journal: &mut AiJournal,
) {
    let script = &content.ai;
    let me = c.faction;

    let order = |queue: &mut CommandQueue, journal: &mut AiJournal, o: Order, a: AiAction| {
        queue.0.push_back(o.issued_by(me));
        journal.0.push((tick, me, a));
    };

    // Its HQ: the victory building it owns (lowest entity index if it somehow
    // owns several). Without one it has no economy and no rally point.
    let hq = buildings
        .iter()
        .find(|b| b.faction == me && content.buildings.get(b.def).is_some_and(|def| def.victory))
        .map(|b| (b.entity, b.pos, b.queued));
    let Some((hq_entity, hq_pos, hq_queued)) = hq else {
        return;
    };

    // What it can still spend this decision. Orders are all-or-nothing and
    // charged by the sim, but the commander tracks its own budget so one
    // decision never commits the same Alloy twice.
    let mut budget = stock.alloy(me);

    // ---- 1. mine: every idle gatherer gets a deposit -----------------------
    if !nodes.is_empty() {
        for u in units.iter().filter(|u| u.faction == me && !u.on_a_job) {
            if !content.units.get(u.def).is_some_and(|d| d.gathers) {
                continue;
            }
            let Some((node, node_pos)) = nearest(nodes, u.pos) else {
                break;
            };
            order(
                queue,
                journal,
                Order::Gather {
                    units: vec![u.entity],
                    node,
                    node_pos,
                },
                AiAction::Gather {
                    unit: u.entity,
                    node,
                },
            );
        }
    }

    // ---- 2. workers: keep `worker_target` on the field ---------------------
    let workers_now = units
        .iter()
        .filter(|u| u.faction == me && content.units.get(u.def).is_some_and(|d| d.gathers))
        .count() as u32;
    if let Some(worker) = hq_worker(content, hq_entity, buildings) {
        let cost = content.units[worker].mvp_alloy_cost;
        let pending = hq_queued as u32;
        if workers_now.saturating_add(pending) < script.worker_target
            && hq_queued == 0
            && budget >= cost
        {
            budget -= cost;
            order(
                queue,
                journal,
                Order::Train {
                    building: hq_entity,
                    unit: worker,
                },
                AiAction::TrainWorker { at: hq_entity },
            );
        }
    }

    // ---- 3. tech: one barracks, once the script says so --------------------
    let barracks_def = content.building_index(&script.barracks);
    let my_barracks: Option<&BuildingRow> =
        barracks_def.and_then(|def| buildings.iter().find(|b| b.faction == me && b.def == def));
    if let (Some(def), None) = (barracks_def, my_barracks) {
        let cost = content.buildings[def].alloy_cost;
        if tick >= script.barracks_at_tick && budget >= cost {
            budget -= cost;
            // The only thing chance decides: which way the barracks goes.
            let angle = c.rng.range_f32(0.0, std::f32::consts::TAU);
            let pos = hq_pos + Vec2::new(angle.cos(), angle.sin()) * script.barracks_offset;
            order(
                queue,
                journal,
                Order::Place {
                    faction: me,
                    building: def,
                    pos,
                },
                AiAction::PlaceBarracks { pos },
            );
        }
    }

    // ---- 4. army: the repeating build order --------------------------------
    if let Some(b) = my_barracks {
        if b.queued == 0 {
            if let Some(unit) = script
                .army_at(c.army_cursor)
                .and_then(|id| content.unit_index(id))
            {
                let cost = content.units[unit].mvp_alloy_cost;
                if budget >= cost {
                    budget -= cost;
                    c.army_cursor = c.army_cursor.wrapping_add(1);
                    order(
                        queue,
                        journal,
                        Order::Train {
                            building: b.entity,
                            unit,
                        },
                        AiAction::TrainArmy { unit },
                    );
                }
            }
        }
    }
    // `budget` has done its job (one decision never commits the same Alloy
    // twice); the attack below spends nothing.
    let _ = budget;

    // ---- 5. attack-move at the enemy HQ ------------------------------------
    let force: Vec<Entity> = units
        .iter()
        .filter(|u| u.faction == me && content.units.get(u.def).is_some_and(|d| d.offense > 0))
        .map(|u| u.entity)
        .collect();
    if force.len() as u32 >= script.attack_at_army && tick >= c.next_attack_tick {
        let enemy_hq = buildings
            .iter()
            .find(|b| {
                b.faction != me && content.buildings.get(b.def).is_some_and(|def| def.victory)
            })
            .map(|b| b.pos);
        if let Some(target) = enemy_hq {
            let spread = script.attack_spread;
            let dest = target
                + Vec2::new(
                    c.rng.range_f32(-spread, spread),
                    c.rng.range_f32(-spread, spread),
                );
            c.next_attack_tick = tick.saturating_add(script.attack_interval_ticks);
            let count = force.len() as u32;
            order(
                queue,
                journal,
                Order::MoveTo { units: force, dest },
                AiAction::Attack { dest, force: count },
            );
        }
    }
}

/// The gatherer this HQ can train: the first unit in its RON `produces` list
/// whose definition gathers. Data, so "the HQ makes workers" is not a fact
/// spelled out in Rust.
fn hq_worker(content: &Content, hq: Entity, buildings: &[BuildingRow]) -> Option<usize> {
    let def = buildings.iter().find(|b| b.entity == hq)?.def;
    content
        .buildings
        .get(def)?
        .produces
        .iter()
        .filter_map(|id| content.unit_index(id))
        .find(|&u| content.units[u].gathers)
}

// L1 unit tests: the script's pure parts (cursor walk, seeding).
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn content() -> Content {
        Content::load_from_dir(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data"))
            .expect("content loads")
    }

    #[test]
    fn the_army_build_order_repeats_in_ron_order() {
        let c = content();
        let cycle = c.ai.cycle_len().expect("cycle fits u32");
        assert!(cycle > 0);
        // The walk is a pure function of the cursor: cursor n and n + cycle name
        // the same unit, for every n.
        for n in 0..(cycle * 3) {
            assert_eq!(c.ai.army_at(n), c.ai.army_at(n + cycle), "cursor {n}");
        }
        // And it visits every entry of the build order.
        for item in &c.ai.army {
            assert!(
                (0..cycle).any(|n| c.ai.army_at(n) == Some(item.unit.as_str())),
                "`{}` never comes up",
                item.unit
            );
        }
    }

    #[test]
    fn commanders_are_seeded_per_faction_and_ordered_by_slot() {
        let cs = AiCommanders::new(7, &[Faction::B, Faction::A]);
        assert_eq!(
            cs.commanders()
                .iter()
                .map(|c| c.faction)
                .collect::<Vec<_>>(),
            vec![Faction::A, Faction::B],
            "commanders act in faction-slot order, not insertion order"
        );
        // One seed, two independent streams; the same seed, the same stream.
        let draw = |f, seed| AiCommander::new(f, seed).rng.next_u64();
        assert_ne!(draw(Faction::A, 7), draw(Faction::B, 7));
        assert_eq!(draw(Faction::A, 7), draw(Faction::A, 7));
        assert_ne!(draw(Faction::A, 7), draw(Faction::A, 8));
    }

    #[test]
    fn a_faction_gets_exactly_one_commander() {
        let cs = AiCommanders::new(1, &[Faction::A, Faction::A, Faction::B]);
        assert_eq!(cs.commanders().len(), 2, "a faction was given two brains");
    }
}
