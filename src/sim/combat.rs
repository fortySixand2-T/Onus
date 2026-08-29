//! Combat (M4b): health, attacks, engagement and death. **Render-free** — ECS +
//! math + [`Content`] only.
//!
//! ## The 4-stat model (all four are RON data, none are Rust constants)
//! - **Offense** → damage per hit (`offense * combat.damage_per_offense`)
//! - **Armor** → *flat* mitigation per hit (`armor * combat.mitigation_per_armor`)
//! - **Defense** → the HP pool (`defense * combat.hp_per_defense`)
//! - **Speed** → movement (`speed * combat.speed_per_point`, used by
//!   [`crate::sim::movement`])
//!
//! plus the per-unit engagement numbers `mvp_attack_range` (reach) and
//! `mvp_attack_ticks` (cadence — **ticks**, never seconds).
//!
//! ## Nemesis
//! If `attacker.nemesis == defender.id`, damage is multiplied by
//! `nemesis_bonus.damage_mult` and armor is skipped. The multiplier is applied
//! as an **integer per-mille** (`floor(base * 1300 / 1000)`): the float from the
//! RON is converted once at [`NemesisBonus::mult_milli`], so no float ever
//! enters per-hit damage and the number is identical on every machine.
//!
//! ## Determinism and order-independence
//! One tick of combat is computed from a **snapshot taken at the start of the
//! tick**: who is alive, where they stand, and how much HP they have. Every
//! attacker resolves against that snapshot, damage accumulates into a local
//! ledger, and only then is HP written back and the dead despawned. So:
//! - the order attackers are processed in cannot change the outcome (they are
//!   still processed in ascending `Entity::to_bits()` for stable *commands*);
//! - two units that kill each other on one tick both die — a unit that was
//!   alive at the start of the tick always gets its blow in;
//! - each death is applied exactly once, in a single pass over unique entities,
//!   so there is no double-despawn and no double kill credit.
//!
//! HP uses `saturating_sub`. Unlike the M4a stockpile (F-005), HP is **not** a
//! conserved resource being moved from one ledger to another — it is a pool with
//! a floor at zero, and "overkill" damage has nowhere to go by design. Clamping
//! is the semantics, not a dropped quantity.

use bevy::ecs::prelude::*;
use bevy::math::Vec2;

use crate::sim::content::{Content, NemesisBonus};
use crate::sim::economy::UnitDefIdx;
use crate::sim::pathfind::{astar, TileGrid};
use crate::sim::spatial::{Faction, SpatialGrid, Unit};
use crate::sim::{GatherTarget, MoveTarget, Position};

// ---- components ------------------------------------------------------------

/// A unit's HP pool. `max` comes from `defense * combat.hp_per_defense`.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Health {
    pub current: u32,
    pub max: u32,
}

impl Health {
    /// A full pool for the unit definition at `unit` (index into
    /// [`Content::units`]). Defense is the HP pool — the mapping lives in the
    /// RON's `mvp_combat.hp_per_defense`.
    pub fn from_def(content: &Content, unit: usize) -> Self {
        let max = content
            .units
            .get(unit)
            // Saturating like every other derived stat: `Content::validate`
            // proves this product fits u32 for *loaded* content, so the clamp
            // only ever bites on a `Content` assembled in memory.
            .map(|u| u.defense.saturating_mul(content.combat.hp_per_defense))
            .unwrap_or(0);
        Self { current: max, max }
    }
}

/// Ticks remaining before this unit may hit again. Counted down one per sim
/// tick — a cadence in ticks, never a wall-clock timer. A unit without the
/// component is ready to fire.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AttackCooldown(pub u32);

/// The enemy this unit is currently engaging (sim state, so the engagement is
/// inspectable and replayable rather than recomputed by the renderer).
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Target(pub Entity);

/// Marks a [`MoveTarget`] that *combat* set while chasing.
///
/// The marker is what keeps auto-engagement from overriding the commander:
/// combat only ever re-aims or cancels a move that carries `Engaging`, and
/// [`crate::sim::apply_commands`] **removes** `Engaging` when an explicit
/// `MoveTo`/`Gather` order arrives. So an order always wins — including on the
/// tick it is issued, since orders are applied before the combat pass — and a
/// unit is auto-chased only while it has no orders of its own.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Engaging;

// ---- kill ledger -----------------------------------------------------------

/// Units lost per faction. A fixed-size array (not a map) so iteration order can
/// never influence a sim outcome. Incremented in exactly one place — the single
/// death pass of [`combat`] — which is what makes "despawns exactly once" and
/// "no double kill credit" the same statement.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Casualties {
    lost: [u32; 2],
}

fn faction_slot(f: Faction) -> usize {
    match f {
        Faction::A => 0,
        Faction::B => 1,
    }
}

impl Casualties {
    pub fn lost(&self, f: Faction) -> u32 {
        self.lost[faction_slot(f)]
    }

    pub fn total(&self) -> u32 {
        self.lost[0] + self.lost[1]
    }

    fn record(&mut self, f: Faction) {
        self.lost[faction_slot(f)] += 1;
    }
}

// ---- the damage formula ----------------------------------------------------

/// Damage one hit from unit definition `attacker` deals to unit definition
/// `defender` — the whole 4-stat + nemesis rule, in integer arithmetic:
///
/// - base = `offense * damage_per_offense`
/// - nemesis (`attacker.nemesis == defender.id`): `base * mult_milli / 1000`,
///   truncated (both operands are non-negative, so truncation is a floor), and
///   armor is skipped when `nemesis_bonus.ignore_armor`;
/// - otherwise: `base - defender.armor * mitigation_per_armor`, floored at 0
///   (armor can fully absorb a hit; it can never heal).
///
/// Pure and total — no clock, no RNG, no allocation. The sim and the tests read
/// the same function, and it is the only place damage is decided.
pub fn damage_per_hit(content: &Content, attacker: usize, defender: usize) -> u32 {
    let (Some(a), Some(d)) = (content.units.get(attacker), content.units.get(defender)) else {
        return 0;
    };
    let base = a.offense.saturating_mul(content.combat.damage_per_offense);
    let mitigation = d.armor.saturating_mul(content.combat.mitigation_per_armor);

    let is_nemesis = a.nemesis.as_deref() == Some(d.id.as_str());
    if is_nemesis {
        let bonus = content.nemesis_bonus;
        // Integer per-mille: widened to u64 so the multiply cannot wrap, then
        // divided back down. No float touches this arithmetic. The narrowing
        // back to u32 **saturates** — a truncating `as u32` would turn a bonus
        // into a penalty at large offense, which is the F-005 mistake (silent
        // arithmetic mangling a quantity) in a new place. `Content::validate`
        // also bounds the stats so shipped data can never reach the ceiling;
        // the saturation is the backstop for anything built in memory.
        let boosted = (base as u64 * bonus.mult_milli() as u64 / NemesisBonus::MULT_SCALE as u64)
            .min(u32::MAX as u64) as u32;
        if bonus.ignore_armor {
            boosted
        } else {
            boosted.saturating_sub(mitigation)
        }
    } else {
        base.saturating_sub(mitigation)
    }
}

/// Movement speed of a unit definition in world units per second — Speed is
/// per-unit RON data (`speed * combat.speed_per_point`), which is what retires
/// the old global `sim::SPEED` constant.
pub fn move_speed(content: &Content, unit: usize) -> f32 {
    content
        .units
        .get(unit)
        .map(|u| u.speed as f32 * content.combat.speed_per_point)
        .unwrap_or(0.0)
}

// ---- M3 pathing for the approach -------------------------------------------

/// Whether the straight line `from → to` stays on walkable cells. Sampled at
/// half-cell steps, so no blocked cell on the segment can be skipped over.
fn segment_is_clear(grid: &TileGrid, from: Vec2, to: Vec2) -> bool {
    let d = to - from;
    let len = d.length();
    if len <= f32::EPSILON {
        return grid.is_walkable(grid.cell_at(from));
    }
    let steps = (len / (grid.cell_size * 0.5)).ceil() as u32;
    for s in 0..=steps {
        let p = from + d * (s as f32 / steps as f32);
        if !grid.is_walkable(grid.cell_at(p)) {
            return false;
        }
    }
    true
}

/// Where an attacker should head next to reach `to`. With no tile grid (or a
/// clear line of travel) that is simply `to`; otherwise it is the next waypoint
/// of the M3 A* path — so a unit walks *around* obstacles instead of through
/// them, recomputed each tick because the target moves.
pub fn approach_waypoint(grid: Option<&TileGrid>, from: Vec2, to: Vec2) -> Vec2 {
    let Some(grid) = grid else {
        return to;
    };
    if segment_is_clear(grid, from, to) {
        return to;
    }
    let (start, goal) = (grid.cell_at(from), grid.cell_at(to));
    match astar(grid, start, goal) {
        // path[0] is the cell we stand in; steer to the centre of the next one.
        Some(path) if path.len() >= 2 => grid.cell_center(path[1]),
        _ => to,
    }
}

// ---- the combat system -----------------------------------------------------

/// One unit as this tick's snapshot sees it.
struct Row {
    entity: Entity,
    pos: Vec2,
    def: usize,
    faction: Faction,
    hp: u32,
    max_hp: u32,
    has_health: bool,
    cooldown: u32,
    engaging: bool,
    has_move_target: bool,
    /// On a gather job: the economy owns this unit's movement until the
    /// commander says otherwise.
    gathering: bool,
}

/// Resolve one tick of combat: pick targets with the M2 spatial index, close
/// with M3 pathing, hit whatever is in reach, then apply damage and deaths.
///
/// See the module docs for why this is snapshot-based (order-independence,
/// mutual kills, exactly-once death).
#[allow(clippy::type_complexity)]
pub fn combat(
    content: Res<Content>,
    tiles: Option<Res<TileGrid>>,
    mut casualties: ResMut<Casualties>,
    mut units: Query<(
        Entity,
        &Position,
        &UnitDefIdx,
        &Faction,
        Option<&mut Health>,
        Option<&AttackCooldown>,
        Option<&Engaging>,
        Option<&MoveTarget>,
        Option<&GatherTarget>,
    )>,
    mut commands: Commands,
) {
    // ---- snapshot (stable order: ascending entity index) --------------------
    // `iter()` on a mutable query yields read-only items — the snapshot is taken
    // before anything is written, which is the whole point (see module docs).
    let mut rows: Vec<Row> = units
        .iter()
        .map(
            |(entity, pos, def, faction, health, cd, engaging, mt, gather)| {
                let full = Health::from_def(&content, def.0);
                Row {
                    entity,
                    pos: pos.0,
                    def: def.0,
                    faction: *faction,
                    // A unit that somehow reached the field without an HP pool is
                    // given its full one from the RON rather than being invulnerable.
                    hp: health.map(|h| h.current).unwrap_or(full.max),
                    max_hp: health.map(|h| h.max).unwrap_or(full.max),
                    has_health: health.is_some(),
                    cooldown: cd.map(|c| c.0).unwrap_or(0),
                    engaging: engaging.is_some(),
                    has_move_target: mt.is_some(),
                    gathering: gather.is_some(),
                }
            },
        )
        .collect();
    rows.sort_unstable_by_key(|r| r.entity.to_bits());
    if rows.is_empty() {
        return;
    }

    // ---- M2: nearest enemy, from the same snapshot --------------------------
    let layout: Vec<Unit> = rows
        .iter()
        .map(|r| Unit {
            pos: r.pos,
            faction: r.faction,
        })
        .collect();
    let engage_range = content.combat.engage_range;
    let pursue_range = content.combat.pursue_range;
    let grid = SpatialGrid::build(&layout, engage_range.max(1.0));

    // ---- resolve every attacker against the snapshot ------------------------
    let mut damage: Vec<u32> = vec![0; rows.len()];
    // Who each unit engaged, as a snapshot row index. The `Target` component is
    // written only after the death pass, so it can never end a tick pointing at
    // an entity that died on that tick.
    let mut chosen: Vec<Option<usize>> = vec![None; rows.len()];
    for i in 0..rows.len() {
        let row = &rows[i];
        let Some(def) = content.units.get(row.def) else {
            continue;
        };
        let next_cd = row.cooldown.saturating_sub(1);
        // A unit on a gather job never auto-engages. Today's Worker is also
        // unarmed (`offense: 0`), but that is a roster value, not a rule: a
        // mid-harvest gatherer stands still, so without this it would look
        // "idle" (no `MoveTarget`) and the chase branch below would hijack the
        // economy's job the moment any gatherer were given offense. The rule is
        // the guard; the stat is a coincidence.
        let armed = def.offense > 0 && def.mvp_attack_range > 0.0 && !row.gathering;

        // Non-combatants (the Worker) never engage: no target, no chase.
        let engaged = if armed {
            grid.nearest_enemy(&layout, i)
        } else {
            None
        };
        // Acquire inside `engage_range`; once chasing, hold on out to the
        // `pursue_range` leash so a detour around a wall does not drop the
        // target (which would leave the unit oscillating at the obstacle).
        let reach = if row.engaging {
            pursue_range
        } else {
            engage_range
        };
        let Some(j) = engaged.filter(|&j| row.pos.distance(rows[j].pos) <= reach) else {
            if row.cooldown != next_cd {
                commands.entity(row.entity).insert(AttackCooldown(next_cd));
            }
            let mut e = commands.entity(row.entity);
            if row.engaging {
                // Nothing left to chase: drop the order combat itself issued.
                e.remove::<MoveTarget>().remove::<Engaging>();
            }
            continue;
        };

        chosen[i] = Some(j);
        let mut e = commands.entity(row.entity);

        let mut cooldown = next_cd;
        if row.pos.distance(rows[j].pos) <= def.mvp_attack_range {
            if row.engaging {
                // In reach: stand and fight (only cancels combat's own order).
                e.remove::<MoveTarget>().remove::<Engaging>();
            }
            if cooldown == 0 {
                damage[j] += damage_per_hit(&content, row.def, rows[j].def);
                cooldown = def.mvp_attack_ticks;
            }
        } else if row.engaging || !row.has_move_target {
            // Out of reach but worth chasing — and the commander has not said
            // otherwise. M3 picks the step so walls are walked around.
            let waypoint = approach_waypoint(tiles.as_deref(), row.pos, rows[j].pos);
            e.insert((MoveTarget(waypoint), Engaging));
        }
        e.insert(AttackCooldown(cooldown));
    }

    // ---- apply damage, then deaths: one pass, each entity exactly once ------
    let mut died: Vec<bool> = vec![false; rows.len()];
    for (i, row) in rows.iter().enumerate() {
        if damage[i] == 0 && row.has_health {
            continue;
        }
        // Saturating: HP is a pool with a floor at zero, not a moved quantity.
        let current = row.hp.saturating_sub(damage[i]);
        if current == 0 {
            commands.entity(row.entity).despawn();
            casualties.record(row.faction);
            died[i] = true;
            continue;
        }
        if row.has_health {
            if let Ok((.., Some(mut h), _, _, _, _)) = units.get_mut(row.entity) {
                h.current = current;
            }
        } else {
            commands.entity(row.entity).insert(Health {
                current,
                max: row.max_hp,
            });
        }
    }

    // ---- publish the engagement, now that the dead are known ---------------
    // `Target` is sim state others read (M4c's AI, M5's state hash), so it must
    // be valid at the tick boundary: a unit that died this tick is not a target,
    // and a unit that died this tick holds no target.
    for (i, row) in rows.iter().enumerate() {
        if died[i] {
            continue;
        }
        match chosen[i] {
            Some(j) if !died[j] => {
                commands.entity(row.entity).insert(Target(rows[j].entity));
            }
            _ => {
                commands.entity(row.entity).remove::<Target>();
            }
        }
    }
}

// L1 unit tests: the damage formula in isolation (the sim reads the same fn).
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn content() -> Content {
        Content::load_from_dir(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data"))
            .expect("content loads")
    }

    #[test]
    fn damage_is_offense_minus_flat_armor() {
        let c = content();
        let (arclight, ravager) = (
            c.unit_index("arclight").unwrap(),
            c.unit_index("ravager").unwrap(),
        );
        // 9 offense * 5 = 45, minus 4 armor * 2 = 8 → 37.
        assert_eq!(damage_per_hit(&c, arclight, ravager), 37);
    }

    #[test]
    fn armor_can_absorb_a_hit_but_never_heals() {
        let c = content();
        let (worker, bulwark) = (
            c.unit_index("worker").unwrap(),
            c.unit_index("bulwark").unwrap(),
        );
        assert_eq!(damage_per_hit(&c, worker, bulwark), 0);
        assert_eq!(damage_per_hit(&c, 999, bulwark), 0, "unknown ids are inert");
    }

    #[test]
    fn nemesis_boosts_by_30_percent_and_skips_armor() {
        let c = content();
        let (bulwark, ravager, sentinel) = (
            c.unit_index("bulwark").unwrap(),
            c.unit_index("ravager").unwrap(),
            c.unit_index("sentinel").unwrap(),
        );
        // Bulwark preys on the Ravager: 4 * 5 = 20 base, * 1300 / 1000 = 26,
        // and the Ravager's 4 armor (8 mitigation) is ignored.
        assert_eq!(damage_per_hit(&c, bulwark, ravager), 26);
        // Against anything else the plain armor math applies: 20 - 5 * 2 = 10.
        assert_eq!(damage_per_hit(&c, bulwark, sentinel), 10);
        // The bonus is not symmetric: the prey gets nothing back.
        assert_eq!(
            damage_per_hit(&c, ravager, bulwark),
            35u32.saturating_sub(18),
            "Ravager preys on the Sentinel, not the Bulwark"
        );
    }

    #[test]
    fn the_nemesis_multiplier_is_integer_per_mille() {
        let c = content();
        assert_eq!(c.nemesis_bonus.mult_milli(), 1_300);
        // floor(base * 1300 / 1000) — stated here so a rounding change is a
        // test failure, not a silent balance shift.
        for base in [0u32, 1, 3, 7, 20, 45, 1_000] {
            let expect = (base as u64 * 1_300 / 1_000) as u32;
            assert_eq!(base * 13 / 10, expect, "base {base}");
        }
    }

    #[test]
    fn health_pool_comes_from_defense() {
        let c = content();
        let h = Health::from_def(&c, c.unit_index("bulwark").unwrap());
        assert_eq!((h.current, h.max), (180, 180)); // defense 9 * 20
    }
}
