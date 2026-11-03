//! Campaign battle rules in the 1v1 (C1). **Render-free** — ECS + math +
//! [`Content`] only.
//!
//! A campaign battle is the same match the skirmish plays, with rules layered
//! on top. Every one of them is **opt-in**: nothing here runs, spawns or is
//! hashed unless a match was set up with a [`BattleSetup`] (which inserts the
//! [`BattleState`] resource every system below is gated on). A skirmish —
//! `MatchSettings::default()` — therefore plays exactly the match it always
//! played, tick for tick and hash for hash.
//!
//! ## The General
//! A General is a unit (`units.ron` `generals`), fielded per side by the
//! setup — one or several (armies converging on one battle each bring theirs),
//! standing in a line across the side's front. **A side whose Generals have
//! all fallen loses the battle**; while any one of them lives, it fights on.
//! The rule only binds a side that has fielded one: a side seen with no
//! General at the start of every tick so far has nothing to lose that way.
//!
//! ## Auras
//! Each General carries an aura (`units.ron`): friendly non-General units
//! within its radius get its stat modifiers. [`auras`] computes, once per
//! tick before combat, every unit's total [`StatBonus`] into the
//! [`AuraField`]; combat (offense, armor, defense, attack interval), movement
//! (speed) and [`regen`] read it. **Kind** = the General type: different
//! kinds stack (adds sum, attack factors multiply); of one kind a unit takes
//! the strongest (the largest scaling factor). The Synthesis rule
//! (`PerDistinctDomain`) multiplies by the distinct domains among the
//! friendly units in the radius, capped. An aura never touches the nemesis
//! bonus: there is no stat for it, and the damage rule applies it unchanged.
//! Everything is computed from entity-sorted snapshots, so no query order
//! reaches a bonus.
//!
//! ## Deciding a battle
//! [`battle_watch`] latches, at the **start** of a tick, which losses each side
//! can suffer (it has fielded a General / it has held a victory building) —
//! the same start-of-tick discipline as `victory::match_watch` (F-010).
//! [`battle_end`] runs just before `victory::match_end`, on the state the tick
//! ended in, and decides from per-side counts in fixed-size arrays, so no
//! query order can reach the outcome. Both sides losing on one tick is a draw.
//! The decision is written into the sim's own [`MatchState`] (so the whole
//! chain stops exactly as it does for a skirmish win), and the battle's own
//! account of it into [`BattleState`].

use bevy::ecs::prelude::*;
use bevy::math::Vec2;

use crate::sim::combat::{Health, StatBonus};
use crate::sim::content::{AuraModifier, AuraScaling, AuraStat, Content};
use crate::sim::economy::{Building, UnitDefIdx};
use crate::sim::spatial::Faction;
use crate::sim::victory::count_victory_buildings;
use crate::sim::{MatchState, Position};

fn faction_slot(f: Faction) -> usize {
    match f {
        Faction::A => 0,
        Faction::B => 1,
    }
}

const SLOTS: [Faction; 2] = [Faction::A, Faction::B];

// ---- components ------------------------------------------------------------

/// Marks a General on the field. His definition is his `UnitDefIdx` (an index
/// past the roster — see [`crate::sim::content::GeneralDef`]).
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct General;

// ---- setup -----------------------------------------------------------------

/// One side's part of a battle setup.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SideSetup {
    /// The Generals this side fields (ids from `units.ron` `generals`), in
    /// field order. Several armies converging on one battle bring several
    /// Generals; the same type may appear more than once (a house raises
    /// extra Generals of its own kind).
    pub generals: Vec<String>,
}

/// How a campaign battle starts. Indexed by faction slot (A, B).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BattleSetup {
    pub sides: [SideSetup; 2],
}

impl BattleSetup {
    /// Field one more General, `id`, on `side`.
    pub fn with_general(mut self, side: Faction, id: &str) -> Self {
        self.sides[faction_slot(side)].generals.push(id.to_string());
        self
    }

    /// Check every name this setup uses against `content`, before anything is
    /// spawned — a typo must refuse the battle, never field a different one.
    pub fn check(&self, content: &Content) -> Result<(), BattleSetupError> {
        if content.battle.is_none() {
            return Err(BattleSetupError::NoBattleContent);
        }
        for (side, s) in SLOTS.iter().zip(&self.sides) {
            for id in &s.generals {
                if content.general_index(id).is_none() {
                    return Err(BattleSetupError::UnknownGeneral {
                        id: id.clone(),
                        side: *side,
                    });
                }
            }
        }
        Ok(())
    }
}

/// Why a [`BattleSetup`] was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BattleSetupError {
    /// The content has no `mvp_battle` block, so it has no campaign layer.
    NoBattleContent,
    /// A General id the content does not define.
    UnknownGeneral { id: String, side: Faction },
}

impl std::fmt::Display for BattleSetupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BattleSetupError::NoBattleContent => {
                write!(f, "the content has no `mvp_battle` block (no campaign layer)")
            }
            BattleSetupError::UnknownGeneral { id, side } => {
                write!(f, "no general `{id}` (asked for by {side:?})")
            }
        }
    }
}

impl std::error::Error for BattleSetupError {}

/// Where a side stands on the map: its base, and the unit direction towards
/// the enemy. Map geography is the driver's (the headless fixture's bases);
/// the battle only places things relative to it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SideGround {
    pub base: Vec2,
    pub front: Vec2,
}

/// Where the `j`th of `n` in a line across the front stands, relative to the
/// line's centre: centred on it, `spacing` apart, perpendicular to `front`.
fn line_offset(g: SideGround, j: usize, n: usize, spacing: f32) -> Vec2 {
    let across = Vec2::new(-g.front.y, g.front.x);
    across * ((j as f32 - (n as f32 - 1.0) / 2.0) * spacing)
}

/// Field the setup's Generals and install the battle's state. Call after the
/// skirmish start has been spawned; the setup must already have passed
/// [`BattleSetup::check`]. Spawns in faction-slot order, so entity allocation
/// is a function of the setup alone.
pub fn install(world: &mut World, setup: &BattleSetup, ground: [SideGround; 2]) {
    let content = world.resource::<Content>().clone();
    let battle = content
        .battle
        .as_ref()
        .expect("BattleSetup::check admits only content with a battle block");
    for (slot, side) in SLOTS.iter().enumerate() {
        let g = ground[slot];
        let line = setup.sides[slot].generals.len();
        for (j, id) in setup.sides[slot].generals.iter().enumerate() {
            let idx = content
                .general_index(id)
                .expect("BattleSetup::check admits only known generals");
            let def = content.unit_def(idx).expect("a general has a unit definition");
            let at = g.base
                + g.front * battle.general_front
                + line_offset(g, j, line, battle.formation_spacing);
            world.spawn((
                Position(at),
                UnitDefIdx(idx),
                def.mvp_kind,
                *side,
                Health::from_def(&content, idx),
                General,
            ));
        }
    }
    world.insert_resource(BattleState::default());
    world.insert_resource(AuraField::default());
}

// ---- state -----------------------------------------------------------------

/// Why a side lost a battle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LossCause {
    /// Every General it fielded is dead.
    GeneralsFell,
    /// It held a victory building (the HQ) and has none left — the skirmish
    /// rule, which a campaign battle keeps.
    VictoryBuildingLost,
}

/// How a battle ended: the winner (`None`: both sides lost on one tick), the
/// tick, and why each losing side lost.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BattleResult {
    pub winner: Option<Faction>,
    pub tick: u32,
    /// Per faction slot: why that side lost, or `None` if it did not.
    pub lost: [Option<LossCause>; 2],
}

impl BattleResult {
    /// Why `side` lost, if it did.
    pub fn loss(&self, side: Faction) -> Option<LossCause> {
        self.lost[faction_slot(side)]
    }
}

/// The battle's own sim state. Present only in a campaign battle — which is
/// what every battle system is gated on.
#[derive(Resource, Debug, Clone, Default, PartialEq, Eq)]
pub struct BattleState {
    /// Per slot: has this side been seen with a General at the start of a tick?
    generals_seen: [bool; 2],
    /// Per slot: has this side been seen with a victory building?
    victory_seen: [bool; 2],
    result: Option<BattleResult>,
}

impl BattleState {
    pub fn result(&self) -> Option<&BattleResult> {
        self.result.as_ref()
    }

    pub fn generals_seen(&self, side: Faction) -> bool {
        self.generals_seen[faction_slot(side)]
    }

    pub fn victory_seen(&self, side: Faction) -> bool {
        self.victory_seen[faction_slot(side)]
    }

    /// Everything this resource holds, as words for the sim's state hash.
    /// Sequences are length-prefixed, so two different states cannot flatten
    /// to one word list.
    pub(crate) fn hash_words(&self) -> Vec<u64> {
        let cause = |c: Option<LossCause>| match c {
            None => 0u64,
            Some(LossCause::GeneralsFell) => 1,
            Some(LossCause::VictoryBuildingLost) => 2,
        };
        let mut w = vec![
            self.generals_seen[0] as u64 | (self.generals_seen[1] as u64) << 1,
            self.victory_seen[0] as u64 | (self.victory_seen[1] as u64) << 1,
        ];
        match &self.result {
            None => w.push(0),
            Some(r) => {
                w.push(1);
                w.push(match r.winner {
                    None => 0,
                    Some(f) => 1 + faction_slot(f) as u64,
                });
                w.push(r.tick as u64);
                w.push(cause(r.lost[0]));
                w.push(cause(r.lost[1]));
            }
        }
        w
    }
}

// ---- systems ---------------------------------------------------------------

fn count_generals(generals: &Query<&Faction, With<General>>) -> [u32; 2] {
    let mut n = [0u32; 2];
    for f in generals {
        n[faction_slot(*f)] += 1;
    }
    n
}

/// Latch, at the start of a tick, which losses each side can suffer.
pub fn battle_watch(
    content: Res<Content>,
    state: Res<MatchState>,
    mut battle: ResMut<BattleState>,
    generals: Query<&Faction, With<General>>,
    buildings: Query<(&Building, &Faction)>,
) {
    if state.is_over() {
        return;
    }
    let g = count_generals(&generals);
    let v = count_victory_buildings(&content, &buildings);
    for s in 0..2 {
        battle.generals_seen[s] |= g[s] > 0;
        battle.victory_seen[s] |= v[s] > 0;
    }
}

/// Decide the battle on the state the tick ended in. Runs just before
/// `victory::match_end`, so on a tick it decides, the skirmish check sees a
/// decided match and leaves it alone.
pub fn battle_end(
    content: Res<Content>,
    mut state: ResMut<MatchState>,
    mut battle: ResMut<BattleState>,
    generals: Query<&Faction, With<General>>,
    buildings: Query<(&Building, &Faction)>,
) {
    if state.is_over() {
        return;
    }
    let g = count_generals(&generals);
    let v = count_victory_buildings(&content, &buildings);
    let mut lost: [Option<LossCause>; 2] = [None, None];
    for s in 0..2 {
        lost[s] = if battle.generals_seen[s] && g[s] == 0 {
            Some(LossCause::GeneralsFell)
        } else if battle.victory_seen[s] && v[s] == 0 {
            Some(LossCause::VictoryBuildingLost)
        } else {
            None
        };
    }
    let winner = match (lost[0].is_some(), lost[1].is_some()) {
        (false, false) => return,
        (true, true) => None,
        (true, false) => Some(Faction::B),
        (false, true) => Some(Faction::A),
    };
    let tick = state.tick();
    state.conclude(winner);
    battle.result = Some(BattleResult { winner, tick, lost });
}

// ---- auras -----------------------------------------------------------------

/// Every unit's aura bonus on this tick (C1 AC3), sorted by entity bits; a
/// unit not listed carries [`StatBonus::NONE`]. Recomputed by [`auras`] at
/// the start of each tick's fighting, from positions, so it is derived state
/// (a function of what the state hash already covers), never truth of its own.
#[derive(Resource, Debug, Clone, Default, PartialEq, Eq)]
pub struct AuraField {
    bonuses: Vec<(u64, StatBonus)>,
}

impl AuraField {
    /// The bonus `e` carries this tick.
    pub fn get(&self, e: Entity) -> StatBonus {
        self.bonuses
            .binary_search_by_key(&e.to_bits(), |(k, _)| *k)
            .map(|i| self.bonuses[i].1)
            .unwrap_or(StatBonus::NONE)
    }

    /// Every unit carrying a bonus, ascending entity bits.
    pub fn iter(&self) -> impl Iterator<Item = (Entity, StatBonus)> + '_ {
        self.bonuses.iter().map(|(k, b)| (Entity::from_bits(*k), *b))
    }
}

/// One kind's modifiers at strength `factor`, folded into `into`.
fn apply_modifiers(into: &mut StatBonus, modifiers: &[AuraModifier], factor: u32) {
    for m in modifiers {
        let add = m.add.unwrap_or(0).saturating_mul(factor);
        match m.stat {
            AuraStat::Offense => into.offense = into.offense.saturating_add(add),
            AuraStat::Defense => into.defense = into.defense.saturating_add(add),
            AuraStat::Armor => into.armor = into.armor.saturating_add(add),
            AuraStat::Speed => into.speed = into.speed.saturating_add(add),
            AuraStat::Regen => into.regen = into.regen.saturating_add(add),
            // A factor of n applies the multiplier n times.
            AuraStat::AttackTicks => {
                for _ in 0..factor {
                    into.attack_milli = ((into.attack_milli as u64 * m.mul_milli() as u64)
                        / AuraModifier::MULT_SCALE as u64)
                        .max(1) as u32;
                }
            }
        }
    }
}

/// Compute the [`AuraField`] for this tick. Runs before combat.
#[allow(clippy::type_complexity)]
pub fn auras(
    content: Res<Content>,
    mut field: ResMut<AuraField>,
    generals: Query<(Entity, &Position, &Faction, &UnitDefIdx), With<General>>,
    units: Query<(Entity, &Position, &Faction, &UnitDefIdx), Without<General>>,
) {
    let mut gs: Vec<_> = generals.iter().map(|(e, p, f, d)| (e.to_bits(), p.0, *f, d.0)).collect();
    gs.sort_unstable_by_key(|g| g.0);
    let mut us: Vec<_> = units.iter().map(|(e, p, f, d)| (e.to_bits(), p.0, *f, d.0)).collect();
    us.sort_unstable_by_key(|u| u.0);

    // best[u][k]: the strongest factor of kind k reaching unit u.
    let kinds = content.generals.len();
    let mut best: Vec<u32> = vec![0; us.len() * kinds];
    let mut in_reach: Vec<usize> = Vec::new();
    for &(_, gpos, gside, gdef) in &gs {
        let Some(g) = content.general_at(gdef) else {
            continue;
        };
        let kind = gdef - content.units.len();
        let r2 = g.aura.radius * g.aura.radius;
        in_reach.clear();
        in_reach.extend(
            us.iter()
                .enumerate()
                .filter(|(_, u)| u.2 == gside && u.1.distance_squared(gpos) <= r2)
                .map(|(i, _)| i),
        );
        let factor = match g.aura.scaling {
            AuraScaling::Flat => 1,
            AuraScaling::PerDistinctDomain { max } => {
                let mut seen = vec![false; content.resources.len()];
                for &i in &in_reach {
                    if let Some(d) = content.domain_of(us[i].3) {
                        seen[d] = true;
                    }
                }
                (seen.iter().filter(|s| **s).count() as u32).min(max)
            }
        };
        for &i in &in_reach {
            let b = &mut best[i * kinds + kind];
            *b = (*b).max(factor);
        }
    }

    field.bonuses.clear();
    for (i, u) in us.iter().enumerate() {
        let mut bonus = StatBonus::NONE;
        for (k, g) in content.generals.iter().enumerate() {
            let factor = best[i * kinds + k];
            if factor > 0 {
                apply_modifiers(&mut bonus, &g.aura.modifiers, factor);
            }
        }
        if bonus != StatBonus::NONE {
            field.bonuses.push((u.0, bonus));
        }
    }
}

/// A `Regen` aura: every `mvp_battle.regen_interval_ticks` ticks, each unit
/// carrying a regen bonus heals that much, up to its pool. Runs after combat,
/// so the dead stay dead.
pub fn regen(
    content: Res<Content>,
    state: Res<MatchState>,
    field: Res<AuraField>,
    mut health: Query<&mut Health>,
) {
    let Some(b) = &content.battle else {
        return;
    };
    if state.tick() % b.regen_interval_ticks != 0 {
        return;
    }
    for (e, bonus) in field.iter() {
        if bonus.regen == 0 {
            continue;
        }
        if let Ok(mut h) = health.get_mut(e) {
            let healed = h.current.saturating_add(bonus.regen).min(h.max);
            if healed != h.current {
                h.current = healed;
            }
        }
    }
}
