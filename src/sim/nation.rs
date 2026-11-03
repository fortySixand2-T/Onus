//! Nations in the 1v1 (C2). **Render-free** — ECS + [`Content`] only.
//!
//! A nation is data (`assets/data/nations.ron`): a name, a look, the General
//! type it fields, the strategy its AI plays, and a list of **traits**. A trait
//! is a modifier on the content a side plays with — "Foundry units cost 10%
//! less Alloy", "workers carry +1" — written as a target, a stat and either a
//! per-mille `mul` or an `add`. There is **no code per nation**: the sim never
//! asks "is this the Forgeborn?", it asks "what does this side's Bulwark
//! cost?".
//!
//! ## How a trait reaches the sim
//! A match names at most one nation per side (`MatchSettings::nations`).
//! When either side has one, [`install`] inserts a [`NationRules`] resource
//! holding, per side, the unit and building definitions with that nation's
//! traits **already applied** — clones of the RON definitions, computed once
//! at match setup, in RON order. Every read site that a trait can touch (cost,
//! training time, carry, gather time, the 4 stats, reach, cadence, building
//! pools) reads through [`Defs`], which answers from the side's table when
//! there is one and from [`Content`] otherwise.
//!
//! ## The golden licence (C2 AC2)
//! With no nation on either side **nothing is inserted**: no `NationRules`
//! resource exists, every [`Defs`] lookup is the plain [`Content`] lookup it
//! replaced, and the state hash emits no nation row. A match with no nation is
//! the match as it was before C2.
//!
//! ## Integer arithmetic
//! A `mul` is converted once to an integer per-mille factor
//! ([`TraitDef::mul_milli`]) and applied as `round(v * milli / 1000)`
//! (half up) in u64, so no float enters the sim's per-tick arithmetic and the
//! derived numbers are identical on every machine. The single float stat,
//! reach, takes `add` whole world units.

use bevy::ecs::prelude::*;
use serde::Deserialize;

use crate::sim::combat::Health;
use crate::sim::content::{BuildingDef, Content, UnitDef};
use crate::sim::spatial::Faction;

// ---- definitions -----------------------------------------------------------

/// One nation (C0's roster, C2's data). `deny_unknown_fields`: a typo in a
/// trait list must be a load error, not a trait serde quietly drops.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NationDef {
    pub id: String,
    pub name: String,
    /// The art it holds holy, a resource `domain` (`None`: the synthesists).
    #[serde(default)]
    pub domain: Option<String>,
    /// Its main colour (P3: you must see which nation you face), linear RGB in
    /// [0, 1]. Client-side only; the sim never reads it.
    pub colour: (f32, f32, f32),
    /// Its accent colour, as `colour`.
    pub accent: (f32, f32, f32),
    /// The General type it fields (a `units.ron` `generals` id).
    pub general: String,
    /// Its battle personality: a `strategies.ron` id the AI plays for it.
    pub strategy: String,
    /// The modifiers its armies get, applied in this order.
    pub traits: Vec<TraitDef>,
}

/// What a trait applies to.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub enum TraitTarget {
    /// One roster unit, by id.
    Unit(String),
    /// Every unit the named building can train (its RON `produces` list).
    Barracks(String),
    /// Every unit that gathers.
    Workers,
    /// Every unit with offense > 0.
    CombatUnits,
    /// One building, by id.
    Building(String),
    /// Every building.
    Buildings,
}

impl TraitTarget {
    fn is_buildings(&self) -> bool {
        matches!(self, TraitTarget::Building(_) | TraitTarget::Buildings)
    }
}

/// The stat a trait changes. Each takes exactly one form, checked at load:
/// `mul` (a factor, per-mille in the sim) or `add` (whole points).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum TraitStat {
    /// `mul`: the Alloy a unit or building costs.
    AlloyCost,
    /// `mul`: a unit's training time in ticks.
    TrainTicks,
    /// `add`: Alloy a gatherer carries per trip.
    CarryCapacity,
    /// `mul`: ticks a gatherer spends harvesting one load.
    GatherTicks,
    /// `add`: a unit's reach, in whole world units.
    AttackRange,
    /// `mul`: a unit's attack interval in ticks (< 1 attacks faster).
    AttackTicks,
    /// `add`: design points of Speed.
    Speed,
    /// `add`: design points of Offense.
    Offense,
    /// `add`: design points of Defense (a unit's HP pool).
    Defense,
    /// `add`: design points of Armor (units or buildings).
    Armor,
    /// `mul`: a building's HP pool.
    BuildingHp,
}

impl TraitStat {
    /// Whether this stat takes `mul` (true) or `add` (false).
    fn is_mul(self) -> bool {
        matches!(
            self,
            TraitStat::AlloyCost
                | TraitStat::TrainTicks
                | TraitStat::GatherTicks
                | TraitStat::AttackTicks
                | TraitStat::BuildingHp
        )
    }

    /// Whether the stat exists on a unit / on a building.
    fn on_units(self) -> bool {
        !matches!(self, TraitStat::BuildingHp)
    }

    fn on_buildings(self) -> bool {
        matches!(
            self,
            TraitStat::AlloyCost | TraitStat::Armor | TraitStat::BuildingHp
        )
    }
}

/// One trait: `stat` of everything `applies_to` names, `mul`tiplied or with
/// `add` added. Exactly one of the two, the one the stat takes.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraitDef {
    pub applies_to: TraitTarget,
    pub stat: TraitStat,
    #[serde(default)]
    pub add: Option<i32>,
    #[serde(default)]
    pub mul: Option<f32>,
}

impl TraitDef {
    /// Per-mille scale of `mul`.
    pub const MULT_SCALE: u32 = 1000;

    /// `mul` as an integer per-mille factor (1000 = unchanged), rounded once.
    pub fn mul_milli(&self) -> u32 {
        self.mul
            .map(|m| (m * Self::MULT_SCALE as f32).round() as u32)
            .unwrap_or(Self::MULT_SCALE)
    }
}

/// `round(v * milli / 1000)`, half up, in u64; saturating back to u32.
pub fn scale_milli(v: u32, milli: u32) -> u32 {
    let scale = TraitDef::MULT_SCALE as u64;
    ((v as u64 * milli as u64 + scale / 2) / scale).min(u32::MAX as u64) as u32
}

// ---- one side's table -------------------------------------------------------

/// The content one side plays with: the roster and building definitions with
/// its nation's traits applied, and each building's HP pool.
#[derive(Debug, Clone)]
pub struct SideRules {
    /// Index of the nation in [`Content::nations`].
    pub nation: usize,
    /// [`Content::units`], with the traits applied (same order, same length).
    pub units: Vec<UnitDef>,
    /// [`Content::buildings`], with the traits applied.
    pub buildings: Vec<BuildingDef>,
    /// Full HP pool per building definition.
    pub building_pools: Vec<u32>,
}

impl SideRules {
    /// Apply nation `nation`'s traits, in RON order, to clones of the content's
    /// definitions. Errors name the trait that produced an unplayable value.
    pub fn build(content: &Content, nation: usize) -> Result<Self, String> {
        let n = content
            .nations
            .get(nation)
            .ok_or_else(|| format!("no nation at index {nation}"))?;
        let mut units = content.units.clone();
        let mut buildings = content.buildings.clone();
        let mut pool_milli = vec![TraitDef::MULT_SCALE as u64; buildings.len()];
        for (k, t) in n.traits.iter().enumerate() {
            let who = format!("nation `{}` trait {k} ({:?} {:?})", n.id, t.stat, t.applies_to);
            check_form(t).map_err(|e| format!("{who}: {e}"))?;
            if t.applies_to.is_buildings() {
                let targets: Vec<usize> = match &t.applies_to {
                    TraitTarget::Building(id) => vec![content
                        .building_index(id)
                        .ok_or_else(|| format!("{who}: names unknown building `{id}`"))?],
                    _ => (0..buildings.len()).collect(),
                };
                for i in targets {
                    let b = &mut buildings[i];
                    match t.stat {
                        TraitStat::AlloyCost => b.alloy_cost = scale_milli(b.alloy_cost, t.mul_milli()),
                        TraitStat::Armor => b.mvp_armor = add_points(b.mvp_armor, t.add)?,
                        TraitStat::BuildingHp => {
                            pool_milli[i] = pool_milli[i] * t.mul_milli() as u64
                                / TraitDef::MULT_SCALE as u64
                        }
                        _ => unreachable!("check_form admits only building stats here"),
                    }
                }
            } else {
                let targets: Vec<usize> = match &t.applies_to {
                    TraitTarget::Unit(id) => vec![content
                        .unit_index(id)
                        .ok_or_else(|| format!("{who}: names unknown unit `{id}`"))?],
                    TraitTarget::Barracks(id) => {
                        let b = content
                            .building(id)
                            .ok_or_else(|| format!("{who}: names unknown building `{id}`"))?;
                        b.produces
                            .iter()
                            .filter_map(|u| content.unit_index(u))
                            .collect()
                    }
                    TraitTarget::Workers => (0..units.len()).filter(|&i| units[i].gathers).collect(),
                    TraitTarget::CombatUnits => {
                        (0..units.len()).filter(|&i| content.units[i].offense > 0).collect()
                    }
                    TraitTarget::Building(_) | TraitTarget::Buildings => unreachable!(),
                };
                if targets.is_empty() {
                    return Err(format!("{who}: applies to no unit"));
                }
                for i in targets {
                    let u = &mut units[i];
                    let m = t.mul_milli();
                    match t.stat {
                        TraitStat::AlloyCost => u.mvp_alloy_cost = scale_milli(u.mvp_alloy_cost, m),
                        TraitStat::TrainTicks => u.mvp_train_ticks = scale_milli(u.mvp_train_ticks, m),
                        TraitStat::GatherTicks => {
                            u.mvp_gather_ticks = scale_milli(u.mvp_gather_ticks, m)
                        }
                        TraitStat::AttackTicks => {
                            u.mvp_attack_ticks = scale_milli(u.mvp_attack_ticks, m)
                        }
                        TraitStat::CarryCapacity => {
                            u.mvp_carry_capacity = add_points(u.mvp_carry_capacity, t.add)?
                        }
                        TraitStat::AttackRange => {
                            u.mvp_attack_range += t.add.unwrap_or(0) as f32;
                        }
                        TraitStat::Speed => u.speed = add_points(u.speed, t.add)?,
                        TraitStat::Offense => u.offense = add_points(u.offense, t.add)?,
                        TraitStat::Defense => u.defense = add_points(u.defense, t.add)?,
                        TraitStat::Armor => u.armor = add_points(u.armor, t.add)?,
                        TraitStat::BuildingHp => unreachable!("check_form refuses it on units"),
                    }
                }
            }
        }
        let building_pools = buildings
            .iter()
            .zip(&pool_milli)
            .map(|(b, &m)| {
                let base = b.mvp_defense as u64 * content.combat.building_hp_per_defense as u64;
                (base * m / TraitDef::MULT_SCALE as u64).min(u32::MAX as u64) as u32
            })
            .collect();
        let side = SideRules {
            nation,
            units,
            buildings,
            building_pools,
        };
        side.check(content).map_err(|e| format!("nation `{}`: {e}", n.id))?;
        Ok(side)
    }

    /// The traited definitions must still be content the sim can run, under
    /// the same rules [`Content`]'s loader applies to the RON.
    fn check(&self, content: &Content) -> Result<(), String> {
        let c = &content.combat;
        for b in &self.buildings {
            if b.alloy_cost == 0 {
                return Err(format!("building `{}` would cost no Alloy", b.id));
            }
            if b.mvp_armor > c.max_stat {
                return Err(format!("building `{}` armor above max_stat", b.id));
            }
        }
        for (b, &pool) in self.buildings.iter().zip(&self.building_pools) {
            if pool == 0 {
                return Err(format!("building `{}` would have no HP", b.id));
            }
        }
        for (u, base) in self.units.iter().zip(&content.units) {
            if u.mvp_alloy_cost == 0 {
                return Err(format!("unit `{}` would cost no Alloy", u.id));
            }
            if u.mvp_train_ticks == 0 {
                return Err(format!("unit `{}` would train in no time", u.id));
            }
            if base.gathers && (u.mvp_carry_capacity == 0 || u.mvp_gather_ticks == 0) {
                return Err(format!("unit `{}` would gather nothing", u.id));
            }
            if !base.gathers && u.mvp_carry_capacity != base.mvp_carry_capacity {
                return Err(format!("unit `{}` does not gather: no carry trait", u.id));
            }
            for (stat, v) in [
                ("offense", u.offense),
                ("defense", u.defense),
                ("armor", u.armor),
                ("speed", u.speed),
            ] {
                if v > c.max_stat {
                    return Err(format!(
                        "unit `{}` {stat} {v} above the design scale max {}",
                        u.id, c.max_stat
                    ));
                }
            }
            if u.defense == 0 || u.speed == 0 {
                return Err(format!("unit `{}` would have no HP pool or no speed", u.id));
            }
            if base.offense > 0 {
                if u.offense == 0 {
                    return Err(format!("unit `{}` would lose its offense", u.id));
                }
                if u.mvp_attack_ticks == 0 {
                    return Err(format!("unit `{}` would attack every tick", u.id));
                }
                if !(u.mvp_attack_range.is_finite()
                    && u.mvp_attack_range > 0.0
                    && u.mvp_attack_range <= c.engage_range
                    && u.mvp_attack_range <= c.attack_move_range)
                {
                    return Err(format!(
                        "unit `{}` reach {} must be positive and within engage_range \
                         and attack_move_range",
                        u.id, u.mvp_attack_range
                    ));
                }
            } else if u.offense != 0
                || u.mvp_attack_ticks != base.mvp_attack_ticks
                || u.mvp_attack_range != base.mvp_attack_range
            {
                return Err(format!("unit `{}` is a non-combatant: no combat trait", u.id));
            }
        }
        Ok(())
    }
}

/// The trait has the one form its stat takes, a sane value, and a target the
/// stat exists on.
fn check_form(t: &TraitDef) -> Result<(), String> {
    match (t.stat.is_mul(), t.add, t.mul) {
        (true, None, Some(m)) => {
            if !(m.is_finite() && m > 0.0) {
                return Err("mul must be finite and positive".to_string());
            }
        }
        (false, Some(a), None) => {
            if a == 0 {
                return Err("add must not be 0".to_string());
            }
        }
        (true, _, _) => return Err("takes `mul` (and no `add`)".to_string()),
        (false, _, _) => return Err("takes `add` (and no `mul`)".to_string()),
    }
    if t.applies_to.is_buildings() && !t.stat.on_buildings() {
        return Err("is not a building stat".to_string());
    }
    if !t.applies_to.is_buildings() && !t.stat.on_units() {
        return Err("is not a unit stat".to_string());
    }
    Ok(())
}

fn add_points(v: u32, add: Option<i32>) -> Result<u32, String> {
    let r = v as i64 + add.unwrap_or(0) as i64;
    if r < 0 {
        return Err(format!("{v} + {} is negative", add.unwrap_or(0)));
    }
    Ok(r.min(u32::MAX as i64) as u32)
}

/// Load-time check of every nation (called by [`Content`]'s validation):
/// unique ids, real references, colours in range, and traits that leave the
/// content playable.
pub(crate) fn validate(content: &Content) -> Result<(), String> {
    for (i, n) in content.nations.iter().enumerate() {
        if let Some(j) = content.nations[..i].iter().position(|m| m.id == n.id) {
            return Err(format!("duplicate nation id `{}` at positions {j} and {i}", n.id));
        }
        if content.general_index(&n.general).is_none() {
            return Err(format!("nation `{}` names unknown general `{}`", n.id, n.general));
        }
        if content.strategy_index(&n.strategy).is_none() {
            return Err(format!("nation `{}` names unknown strategy `{}`", n.id, n.strategy));
        }
        if let Some(d) = &n.domain {
            if !content.resources.iter().any(|r| &r.domain == d) {
                return Err(format!("nation `{}` names unknown domain `{d}`", n.id));
            }
        }
        for (what, (r, g, b)) in [("colour", n.colour), ("accent", n.accent)] {
            if ![r, g, b].iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)) {
                return Err(format!("nation `{}` {what} must be in [0, 1]", n.id));
            }
        }
        SideRules::build(content, i)?;
    }
    Ok(())
}

// ---- the match's nations ----------------------------------------------------

/// The nations of one match, per faction slot, with their tables. Inserted
/// **only** when at least one side has a nation ([`install`]); a match with
/// none has no such resource, which is the golden licence.
#[derive(Resource, Debug, Clone)]
pub struct NationRules {
    sides: [Option<SideRules>; 2],
}

/// A nation was named that the loaded content does not have. Refused, never
/// defaulted, like an unknown strategy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnknownNation {
    pub id: String,
    pub faction: Faction,
}

impl std::fmt::Display for UnknownNation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown nation `{}` for side {:?}", self.id, self.faction)
    }
}

impl std::error::Error for UnknownNation {}

fn slot(f: Faction) -> usize {
    match f {
        Faction::A => 0,
        Faction::B => 1,
    }
}

const SIDES: [Faction; 2] = [Faction::A, Faction::B];

impl NationRules {
    /// The rules for nations named per faction slot (A, B). `Ok(None)` when
    /// neither side names one. Refuses an unknown id.
    pub fn resolve(
        content: &Content,
        names: &[Option<String>; 2],
    ) -> Result<Option<Self>, UnknownNation> {
        if names.iter().all(Option::is_none) {
            return Ok(None);
        }
        let mut sides: [Option<SideRules>; 2] = [None, None];
        for (k, name) in names.iter().enumerate() {
            let Some(id) = name else { continue };
            let Some(i) = content.nations.iter().position(|n| &n.id == id) else {
                return Err(UnknownNation {
                    id: id.clone(),
                    faction: SIDES[k],
                });
            };
            sides[k] = Some(
                SideRules::build(content, i).expect("the loader validated every nation's traits"),
            );
        }
        Ok(Some(Self { sides }))
    }

    /// The table of `faction`, if it has a nation.
    pub fn side(&self, faction: Faction) -> Option<&SideRules> {
        self.sides[slot(faction)].as_ref()
    }

    /// The nation index of `faction` (into [`Content::nations`]).
    pub fn nation_of(&self, faction: Faction) -> Option<usize> {
        self.side(faction).map(|s| s.nation)
    }
}

/// Each side's nation id, per faction slot (A, B): what a match log records,
/// and what [`install`] takes back. All `None` when the match has no
/// [`NationRules`].
pub fn ids<'a>(content: &'a Content, rules: Option<&NationRules>) -> [Option<&'a str>; 2] {
    SIDES.map(|f| {
        rules
            .and_then(|r| r.nation_of(f))
            .and_then(|i| content.nations.get(i))
            .map(|n| n.id.as_str())
    })
}

/// Install the match's nations into a world: insert [`NationRules`] when a
/// side has one (nothing otherwise), and give every unit and building already
/// on the field the full HP pool of its side's table. Call before the first
/// tick, after the start has been spawned.
pub fn install(world: &mut World, names: &[Option<String>; 2]) -> Result<(), UnknownNation> {
    let content = world.resource::<Content>().clone();
    let Some(rules) = NationRules::resolve(&content, names)? else {
        return Ok(());
    };
    {
        let defs = Defs::new(&content, Some(&rules));
        let mut q = world.query::<(
            &Faction,
            Option<&crate::sim::economy::UnitDefIdx>,
            Option<&crate::sim::economy::Building>,
            &mut Health,
        )>();
        for (f, unit, building, mut h) in q.iter_mut(world) {
            let full = match (unit, building) {
                (Some(u), _) => defs.unit_health(*f, u.0),
                (None, Some(b)) => defs.building_health(*f, b.def),
                (None, None) => continue,
            };
            // Only a pool that is still full is re-filled: a damaged start
            // (a campaign roster's HP) keeps its HP, under the new maximum.
            if h.current == h.max {
                *h = full;
            } else {
                h.max = full.max;
                h.current = h.current.min(full.max);
            }
        }
    }
    world.insert_resource(rules);
    Ok(())
}

// ---- the lookup every read site uses ----------------------------------------

/// The definitions a faction plays with: its nation's table when the match
/// has one for it, the plain [`Content`] otherwise. Cheap to copy.
#[derive(Clone, Copy)]
pub struct Defs<'a> {
    pub content: &'a Content,
    rules: Option<&'a NationRules>,
}

impl<'a> Defs<'a> {
    pub fn new(content: &'a Content, rules: Option<&'a NationRules>) -> Self {
        Self { content, rules }
    }

    /// Plain content: no nation on either side.
    pub fn plain(content: &'a Content) -> Self {
        Self::new(content, None)
    }

    fn side(&self, f: Faction) -> Option<&'a SideRules> {
        self.rules.and_then(|r| r.side(f))
    }

    /// The unit definition behind a `UnitDefIdx` for `faction`: the traited
    /// roster, then the Generals (which no trait touches).
    pub fn unit(&self, faction: Faction, i: usize) -> Option<&'a UnitDef> {
        match self.side(faction) {
            Some(s) if i < s.units.len() => s.units.get(i),
            _ => self.content.unit_def(i),
        }
    }

    /// A roster unit only (never a General): what training reads.
    pub fn roster_unit(&self, faction: Faction, i: usize) -> Option<&'a UnitDef> {
        match self.side(faction) {
            Some(s) => s.units.get(i),
            None => self.content.units.get(i),
        }
    }

    pub fn building(&self, faction: Faction, i: usize) -> Option<&'a BuildingDef> {
        match self.side(faction) {
            Some(s) => s.buildings.get(i),
            None => self.content.buildings.get(i),
        }
    }

    /// A full HP pool for unit `i` of `faction`.
    pub fn unit_health(&self, faction: Faction, i: usize) -> Health {
        match self.side(faction) {
            Some(_) => {
                let max = self
                    .unit(faction, i)
                    .map(|u| u.defense.saturating_mul(self.content.combat.hp_per_defense))
                    .unwrap_or(0);
                Health { current: max, max }
            }
            None => Health::from_def(self.content, i),
        }
    }

    /// A full HP pool for building `i` of `faction`.
    pub fn building_health(&self, faction: Faction, i: usize) -> Health {
        match self.side(faction) {
            Some(s) => {
                let max = s.building_pools.get(i).copied().unwrap_or(0);
                Health { current: max, max }
            }
            None => Health::from_building_def(self.content, i),
        }
    }
}
