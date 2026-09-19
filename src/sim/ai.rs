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
//! - The script (build order, thresholds, timings) is data: a named strategy in
//!   `strategies.ron`. There are no AI constants in Rust.
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

use crate::sim::content::{Content, StrategyDef};
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
    /// Placed one of the barracks its strategy opens (index into
    /// [`Content::buildings`], so a multi-barracks strategy's trace says *which*).
    PlaceBarracks { building: usize, pos: Vec2 },
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
    /// Which script it runs: an index into [`Content::strategies`] (the stable
    /// RON order), or `None` for "whatever the content's default is". An index,
    /// never a name — resolving a name happens once, at construction, so the
    /// decision path never compares strings.
    strategy: Option<usize>,
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
        Self::with_index(faction, seed, None)
    }

    /// A commander for `faction` running the strategy named `id`, refused
    /// (never silently defaulted) if `content` has no such strategy: a typo in
    /// a matchup must not quietly play the default and report the result under
    /// a name nobody ran.
    ///
    /// The RNG stream is derived exactly as [`AiCommander::new`]'s is — from
    /// (`seed`, faction slot) **only**, never from the strategy — so one seed
    /// means one opening across every matchup, and a difference between two
    /// matchups is attributable to the scripts.
    pub fn with_strategy(
        content: &Content,
        faction: Faction,
        seed: u64,
        id: &str,
    ) -> Result<Self, UnknownStrategy> {
        match content.strategy_index(id) {
            Some(i) => Ok(Self::with_index(faction, seed, Some(i))),
            None => Err(UnknownStrategy {
                id: id.to_string(),
                faction: Some(faction),
            }),
        }
    }

    fn with_index(faction: Faction, seed: u64, strategy: Option<usize>) -> Self {
        let slot = faction_slot(faction) as u64;
        Self {
            faction,
            strategy,
            rng: SplitMix64::new(seed ^ slot.wrapping_mul(0x9E37_79B9_7F4A_7C15)),
            tick: 0,
            army_cursor: 0,
            next_attack_tick: 0,
        }
    }

    /// The index of the strategy it was named with, or `None` if it runs the
    /// content's default.
    pub fn strategy_index(&self) -> Option<usize> {
        self.strategy
    }

    /// The script it actually runs, resolved against the content it plays on.
    /// Panics if the index is out of range, which can only mean the commander
    /// was built against *different* content than the match is running — a
    /// silent fallback there would report a match under the wrong strategy.
    pub fn strategy<'a>(&self, content: &'a Content) -> &'a StrategyDef {
        match self.strategy {
            None => &content.ai,
            Some(i) => content.strategies.get(i).unwrap_or_else(|| {
                panic!(
                    "commander's strategy index {i} is out of range for this content \
                     ({} strategies) — it was built against different content",
                    content.strategies.len()
                )
            }),
        }
    }

    pub fn tick(&self) -> u32 {
        self.tick
    }

    /// This commander's RNG state. The sim's only randomness lives here, and
    /// this is how a test checks that it moved only while the sim was running.
    pub fn rng_state(&self) -> u64 {
        self.rng.state()
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

    /// The commanders of one match: **one strategy per side**, named. Refused
    /// if either name is unknown to `content` (the error says which side and
    /// which id). Stored in faction-slot order, one commander per faction, and
    /// seeded exactly as [`AiCommanders::new`] seeds them — so the same seed
    /// means the same opening whatever the pairing.
    pub fn matchup(
        content: &Content,
        seed: u64,
        sides: &[(Faction, &str)],
    ) -> Result<Self, UnknownStrategy> {
        let mut cs: Vec<AiCommander> = sides
            .iter()
            .map(|(f, id)| AiCommander::with_strategy(content, *f, seed, id))
            .collect::<Result<_, _>>()?;
        cs.sort_by_key(|c| faction_slot(c.faction));
        cs.dedup_by_key(|c| faction_slot(c.faction));
        Ok(Self(cs))
    }

    pub fn commanders(&self) -> &[AiCommander] {
        &self.0
    }
}

/// A strategy was named that the loaded content does not have. Returned rather
/// than defaulted: every number B2/B3 print is keyed by strategy name, so a
/// typo must stop the caller, not mislabel a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownStrategy {
    /// The id that named nothing.
    pub id: String,
    /// The side that asked for it, when it was asked for on a side's behalf.
    pub faction: Option<Faction>,
}

impl std::fmt::Display for UnknownStrategy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.faction {
            Some(who) => write!(f, "no strategy `{}` (asked for by {:?})", self.id, who),
            None => write!(f, "no strategy `{}`", self.id),
        }
    }
}

impl std::error::Error for UnknownStrategy {}

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

/// One tick of every scripted commander. Runs **first among the deciders**:
/// only `economy::repair_gather_claims` precedes it, so no commander can read a
/// split gather claim as a live job (F-008). The orders it issues are applied
/// by `apply_commands` on the same tick — exactly
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
        // Its *own* cadence: two strategies with different APMs must not both
        // think on the default's beat.
        if tick % commander.strategy(&content).think_interval_ticks != 0 {
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
    let script = c.strategy(content);
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

    // ---- 3. tech: every barracks the script opens, in RON order ------------
    // Each opening goes up once, on the first decision at or after its own tick
    // that the commander can pay for — and a strategy may open several, which
    // is how its army spans domains.
    //
    // The list is **counted, not searched** (B3.5 AC0b): a building named N
    // times is N placements, so the k-th opening of a building is satisfied
    // only once the commander owns more than k of it (including any placed
    // earlier in *this* decision, which the world does not show yet). With one
    // opening per building this is exactly "do I have one?", so the shipped
    // data behaves as it did in M4c, RNG included: one draw, taken only when
    // the placement actually happens — a placement it cannot afford consumes
    // no randomness.
    //
    // Per opened def: (def, how many the commander has, how many openings of it
    // this loop has already walked). A `Vec` keyed by def, never a map.
    let mut tech: Vec<(usize, usize, usize)> = Vec::new();
    for opening in &script.barracks {
        let Some(def) = content.building_index(&opening.building) else {
            continue;
        };
        let slot = match tech.iter().position(|(d, _, _)| *d == def) {
            Some(i) => i,
            None => {
                let have = buildings
                    .iter()
                    .filter(|b| b.faction == me && b.def == def)
                    .count();
                tech.push((def, have, 0));
                tech.len() - 1
            }
        };
        let (_, have, walked) = tech[slot];
        tech[slot].2 = walked + 1;
        if have > walked {
            continue; // this opening is already standing
        }
        let cost = content.buildings[def].alloy_cost;
        if tick >= opening.at_tick && budget >= cost {
            // It is standing as far as the *next* openings are concerned, even
            // though `apply_commands` has not spawned it yet.
            tech[slot].1 = have + 1;
            budget -= cost;
            // The only thing chance decides: which way the barracks goes.
            let angle = c.rng.range_f32(0.0, std::f32::consts::TAU);
            let pos = hq_pos + Vec2::new(angle.cos(), angle.sin()) * opening.offset;
            order(
                queue,
                journal,
                Order::Place {
                    faction: me,
                    building: def,
                    pos,
                },
                AiAction::PlaceBarracks {
                    building: def,
                    pos,
                },
            );
        }
    }

    // ---- 4. army: the repeating build order --------------------------------
    // The next unit of the cursor is trained at one of *its own* barracks that
    // produces it (the loader has already refused a strategy asking for a unit
    // none of them can make). If no such barracks is up yet, or they are all
    // full, the cursor waits rather than skipping ahead — the build order is an
    // order.
    //
    // **Which** one (B3.5 AC0b): every barracks the commander owns whose def is
    // one its script opens and which can produce the unit is a candidate, and
    // it picks the one with the **shallowest queue, ties broken by ascending
    // `Entity::to_bits()`** — so several barracks fill evenly and no query or
    // archetype order can reach the choice (`buildings` is already sorted by
    // entity bits, so `min_by_key` on the queue length alone is that rule).
    //
    // "Full" is `script.queue_depth`, read off *this commander's own* strategy
    // (B3.5): the cap on units-in-production **at one barracks** is content, not
    // a Rust constant, and two sides in one match may run different depths.
    // Still **one order per decision** — one line is topped up by one, so the
    // per-decision `budget` commits at most one unit's Alloy.
    if let Some(unit) = script
        .army_at(c.army_cursor)
        .and_then(|id| content.unit_index(id))
    {
        let target = buildings
            .iter()
            .filter(|b| {
                b.faction == me
                    && tech.iter().any(|(def, _, _)| *def == b.def)
                    && content.produces(b.def, unit)
            })
            .min_by_key(|b| b.queued);
        if let Some(b) = target {
            if (b.queued as u32) < script.queue_depth {
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

    /// B3.5: the production cap the army step obeys comes off *the commander's
    /// own* strategy, never the content's default. With a set where one entry
    /// is deeper than the default, a commander named onto it must report that
    /// depth — which is what makes two sides at different depths possible.
    #[test]
    fn a_commander_reads_the_queue_depth_of_its_own_strategy() {
        let mut c = content();
        let deep = c.strategies.len() - 1;
        let id = c.strategies[deep].id.clone();
        c.strategies[deep].queue_depth = 4;
        let cmd = AiCommander::with_strategy(&c, Faction::A, 1, &id).expect("a named strategy");
        assert_eq!(cmd.strategy(&c).queue_depth, 4);
        // ...while the default is untouched at the shipped depth.
        assert_eq!(c.ai.queue_depth, 1);
        assert_eq!(
            AiCommander::new(Faction::B, 1).strategy(&c).queue_depth,
            1,
            "an unnamed commander did not fall back to the default's depth"
        );
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
    fn a_named_strategy_resolves_to_an_index_and_an_unknown_one_is_refused() {
        let c = content();
        let cmd = AiCommander::with_strategy(&c, Faction::A, 3, &c.default_strategy)
            .expect("the default is a named strategy");
        assert_eq!(cmd.strategy_index(), c.strategy_index(&c.default_strategy));
        assert_eq!(cmd.strategy(&c).id, c.default_strategy);
        // `new` carries no index and resolves to the content's default.
        let plain = AiCommander::new(Faction::A, 3);
        assert_eq!(plain.strategy_index(), None);
        assert_eq!(plain.strategy(&c).id, c.default_strategy);
        // A name nobody has is an error, not a quiet default.
        let err = AiCommander::with_strategy(&c, Faction::B, 3, "no_such_plan").unwrap_err();
        assert_eq!(err.id, "no_such_plan");
        assert_eq!(err.faction, Some(Faction::B));
    }

    #[test]
    fn a_matchup_seeds_exactly_as_the_default_constructor_does() {
        let c = content();
        let pair = AiCommanders::matchup(
            &c,
            7,
            &[
                (Faction::B, c.default_strategy.as_str()),
                (Faction::A, c.default_strategy.as_str()),
            ],
        )
        .expect("both sides name the default");
        let plain = AiCommanders::new(7, &[Faction::A, Faction::B]);
        assert_eq!(
            pair.commanders()
                .iter()
                .map(|k| (k.faction, k.rng_state()))
                .collect::<Vec<_>>(),
            plain
                .commanders()
                .iter()
                .map(|k| (k.faction, k.rng_state()))
                .collect::<Vec<_>>(),
            "naming the default changed the seed derivation"
        );
    }

    #[test]
    fn a_faction_gets_exactly_one_commander() {
        let cs = AiCommanders::new(1, &[Faction::A, Faction::A, Faction::B]);
        assert_eq!(cs.commanders().len(), 2, "a faction was given two brains");
        // ...and so does a matchup, however it was spelled.
        let c = content();
        let d = c.default_strategy.as_str();
        let cs = AiCommanders::matchup(
            &c,
            1,
            &[(Faction::A, d), (Faction::A, d), (Faction::B, d)],
        )
        .unwrap();
        assert_eq!(cs.commanders().len(), 2, "a faction was given two brains");
    }
}
