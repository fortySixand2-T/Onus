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
use crate::sim::content::{AuraModifier, BuildingDef, Content, UnitDef};
use crate::sim::spatial::Faction;
use crate::sim::three::{Bank, EconomyMode};

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
    /// The modifiers its armies get in an **Alloy-only** match, applied in
    /// this order (C2's stand-ins).
    pub traits: Vec<TraitDef>,
    /// The modifiers its armies get in a **three-resource** match (C3): its
    /// brief lean and speciality, applied in this order instead of
    /// [`NationDef::traits`]. `#[serde(default)]`: a nation without the line
    /// has no trait there.
    #[serde(default)]
    pub three_resource_traits: Vec<TraitDef>,
    /// Its **mixed-army** trait, if it has one: see [`SynthesisDef`].
    /// `#[serde(default)]`: a nation without the line has no such trait.
    #[serde(default)]
    pub synthesis: Option<SynthesisDef>,
}

/// A mixed-army trait (C2), the nation-level form of C1's Synthesis aura: on
/// every tick, each of the side's units **with a domain** gets `modifiers` ×
/// the number of distinct domains, **other than its own**, among the side's
/// own units within `radius` of it, capped at `max`. A mono-domain army gets
/// nothing; an enemy never counts; a General and a domainless unit (a worker)
/// neither counts nor gains. It is a bonus like an aura's ([`AuraModifier`]
/// stats; there is no nemesis stat, so it can never touch the hard counters),
/// computed into the same [`crate::sim::battle::AuraField`] by
/// [`crate::sim::battle::auras`], and stacks with any General's aura as a kind
/// of its own.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SynthesisDef {
    /// World units.
    pub radius: f32,
    pub modifiers: Vec<AuraModifier>,
    /// The cap on the domain count, `1..=(domains - 1)`.
    pub max: u32,
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
    /// `mul`, three-resource only (C3): a unit's brief `cost`, in its own
    /// resource.
    Cost,
    /// `mul`, three-resource only (C3): the Biomass a growth-source building
    /// grows.
    Growth,
    /// `mul` of at least 1, three-resource only (C3): the Alloy a gatherer's
    /// load banks ("richer mines"), accumulated per-mille per side.
    AlloyYield,
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
                | TraitStat::Cost
                | TraitStat::Growth
                | TraitStat::AlloyYield
        )
    }

    /// Whether the stat exists on a unit / on a building.
    fn on_units(self) -> bool {
        !matches!(self, TraitStat::BuildingHp | TraitStat::Growth)
    }

    fn on_buildings(self) -> bool {
        matches!(
            self,
            TraitStat::AlloyCost | TraitStat::Armor | TraitStat::BuildingHp | TraitStat::Growth
        )
    }

    /// Whether the stat can act in a match playing `economy` on what the
    /// trait names (`buildings`: a building target). The three-resource
    /// stats act only there; a unit's Alloy price (`mvp_alloy_cost`) is not
    /// charged there (its brief `cost` is), so `AlloyCost` on units acts only
    /// in Alloy-only. Buildings are priced in Alloy in both.
    fn acts_in(self, economy: EconomyMode, buildings: bool) -> Result<(), String> {
        match (self, economy) {
            (TraitStat::Cost | TraitStat::Growth | TraitStat::AlloyYield, EconomyMode::Alloy) => {
                Err("acts only in the three-resource economy, not in an Alloy-only list"
                    .to_string())
            }
            (TraitStat::AlloyCost, EconomyMode::ThreeResource) if !buildings => Err(
                "a unit's Alloy price is Alloy-only: in the three-resource economy use `Cost`"
                    .to_string(),
            ),
            _ => Ok(()),
        }
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
    /// Per-mille growth factor per building definition (C3; 1000 unless a
    /// `Growth` trait applies).
    pub growth_milli: Vec<u32>,
    /// Per-mille Alloy yield per unit definition (C3; 1000 unless an
    /// `AlloyYield` trait applies).
    pub yield_milli: Vec<u32>,
}

impl SideRules {
    /// [`SideRules::build_in`] for an Alloy-only match.
    pub fn build(content: &Content, nation: usize) -> Result<Self, String> {
        Self::build_in(content, nation, EconomyMode::Alloy)
    }

    /// Apply nation `nation`'s traits for `economy` (its `traits` in an
    /// Alloy-only match, its `three_resource_traits` in a three-resource one),
    /// in RON order, to clones of the content's definitions. Errors name the
    /// trait that produced an unplayable value, or that cannot act in the
    /// economy its list is played in.
    pub fn build_in(content: &Content, nation: usize, economy: EconomyMode) -> Result<Self, String> {
        let n = content
            .nations
            .get(nation)
            .ok_or_else(|| format!("no nation at index {nation}"))?;
        let list = match economy {
            EconomyMode::Alloy => &n.traits,
            EconomyMode::ThreeResource => &n.three_resource_traits,
        };
        let mut units = content.units.clone();
        let mut buildings = content.buildings.clone();
        // Each building's HP pool, scaled by every BuildingHp trait in RON order,
        // each rounding once half up (`scale_milli`) like every other `mul`.
        let mut building_pools: Vec<u32> = buildings
            .iter()
            .map(|b| b.mvp_defense.saturating_mul(content.combat.building_hp_per_defense))
            .collect();
        let mut growth_milli = vec![TraitDef::MULT_SCALE; buildings.len()];
        let mut yield_milli = vec![TraitDef::MULT_SCALE; units.len()];
        let list_name = match economy {
            EconomyMode::Alloy => "trait",
            EconomyMode::ThreeResource => "three_resource trait",
        };
        for (k, t) in list.iter().enumerate() {
            let who = format!(
                "nation `{}` {list_name} {k} ({:?} {:?})",
                n.id, t.stat, t.applies_to
            );
            check_form(t).map_err(|e| format!("{who}: {e}"))?;
            t.stat
                .acts_in(economy, t.applies_to.is_buildings())
                .map_err(|e| format!("{who}: {e}"))?;
            if t.stat == TraitStat::AlloyYield && t.mul_milli() < TraitDef::MULT_SCALE {
                return Err(format!("{who}: an Alloy yield is a bonus, mul >= 1"));
            }
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
                            building_pools[i] = scale_milli(building_pools[i], t.mul_milli())
                        }
                        TraitStat::Growth => {
                            growth_milli[i] = scale_milli(growth_milli[i], t.mul_milli())
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
                        TraitStat::Cost => u.cost.amount = scale_milli(u.cost.amount, m),
                        TraitStat::AlloyYield => yield_milli[i] = scale_milli(yield_milli[i], m),
                        TraitStat::BuildingHp | TraitStat::Growth => {
                            unreachable!("check_form refuses it on units")
                        }
                    }
                }
            }
        }
        let side = SideRules {
            nation,
            units,
            buildings,
            building_pools,
            growth_milli,
            yield_milli,
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
        for ((u, base), &yield_milli) in self.units.iter().zip(&content.units).zip(&self.yield_milli) {
            if u.mvp_alloy_cost == 0 {
                return Err(format!("unit `{}` would cost no Alloy", u.id));
            }
            if u.cost.amount == 0 {
                return Err(format!("unit `{}` would cost nothing", u.id));
            }
            if u.mvp_train_ticks == 0 {
                return Err(format!("unit `{}` would train in no time", u.id));
            }
            if !base.gathers && yield_milli != TraitDef::MULT_SCALE {
                return Err(format!("unit `{}` does not gather: no yield trait", u.id));
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
        if let Some(syn) = &n.synthesis {
            let who = format!("nation `{}` synthesis", n.id);
            if !(syn.radius.is_finite() && syn.radius > 0.0) {
                return Err(format!("{who} radius must be finite and positive"));
            }
            crate::sim::content::check_aura_modifiers(&who, &syn.modifiers, &content.combat)?;
            let others = content.resources.len().saturating_sub(1);
            if syn.max == 0 || syn.max as usize > others {
                return Err(format!("{who} max must be 1..={others} (the other domains)"));
            }
        }
        // Both lists, each in the economy it is played in.
        SideRules::build_in(content, i, EconomyMode::Alloy)?;
        SideRules::build_in(content, i, EconomyMode::ThreeResource)?;
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
    /// [`NationRules::resolve_in`] for an Alloy-only match.
    pub fn resolve(
        content: &Content,
        names: &[Option<String>; 2],
    ) -> Result<Option<Self>, UnknownNation> {
        Self::resolve_in(content, names, EconomyMode::Alloy)
    }

    /// The rules for nations named per faction slot (A, B), in a match playing
    /// `economy` (C3: which trait list each nation plays). `Ok(None)` when
    /// neither side names one. Refuses an unknown id.
    pub fn resolve_in(
        content: &Content,
        names: &[Option<String>; 2],
        economy: EconomyMode,
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
                SideRules::build_in(content, i, economy)
                    .expect("the loader validated every nation's traits"),
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
///
/// The trait lists played are the world's economy's (C3): install the economy
/// ([`crate::sim::three::install`]) first.
pub fn install(world: &mut World, names: &[Option<String>; 2]) -> Result<(), UnknownNation> {
    let content = world.resource::<Content>().clone();
    let economy = crate::sim::three::mode_of(world);
    let Some(rules) = NationRules::resolve_in(&content, names, economy)? else {
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
    // A mixed-army trait is computed into the aura field, so a skirmish with
    // one needs the field (a campaign battle installs it anyway). Only then:
    // a nation without one leaves the world as it was.
    let synthesis = SIDES
        .iter()
        .filter_map(|f| rules.nation_of(*f))
        .any(|i| content.nations[i].synthesis.is_some());
    world.insert_resource(rules);
    if synthesis && !world.contains_resource::<crate::sim::battle::AuraField>() {
        world.init_resource::<crate::sim::battle::AuraField>();
    }
    Ok(())
}

// ---- the lookup every read site uses ----------------------------------------

/// The definitions a faction plays with: its nation's table when the match
/// has one for it, the plain [`Content`] otherwise. Cheap to copy.
#[derive(Clone, Copy)]
pub struct Defs<'a> {
    pub content: &'a Content,
    rules: Option<&'a NationRules>,
    /// The economy the match plays (C3): what a unit is priced in.
    economy: EconomyMode,
}

impl<'a> Defs<'a> {
    /// The definitions of an Alloy-only match (see [`Defs::in_economy`]).
    pub fn new(content: &'a Content, rules: Option<&'a NationRules>) -> Self {
        Self {
            content,
            rules,
            economy: EconomyMode::Alloy,
        }
    }

    /// The same definitions, in a match playing `economy` (C3).
    pub fn in_economy(mut self, economy: EconomyMode) -> Self {
        self.economy = economy;
        self
    }

    /// The economy these definitions are priced for.
    pub fn economy(&self) -> EconomyMode {
        self.economy
    }

    /// What `faction` pays to train roster unit `i`, and from which bank.
    /// Alloy-only: its `mvp_alloy_cost` in Alloy, as every milestone before C3.
    /// Three-resource (C3): its brief `cost`, in the bank of that resource.
    pub fn unit_price(&self, faction: Faction, i: usize) -> Option<(Bank, u32)> {
        let u = self.roster_unit(faction, i)?;
        match self.economy {
            EconomyMode::Alloy => Some((Bank::Alloy, u.mvp_alloy_cost)),
            EconomyMode::ThreeResource => Some((
                Bank::of_resource(self.content, &u.cost.resource)?,
                u.cost.amount,
            )),
        }
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

    /// `faction`'s per-mille factor on the Biomass building `i` grows (C3).
    pub fn growth_milli(&self, faction: Faction, i: usize) -> u32 {
        self.side(faction)
            .and_then(|s| s.growth_milli.get(i).copied())
            .unwrap_or(TraitDef::MULT_SCALE)
    }

    /// `faction`'s per-mille Alloy yield on a load banked by unit `i` (C3).
    pub fn yield_milli(&self, faction: Faction, i: usize) -> u32 {
        self.side(faction)
            .and_then(|s| s.yield_milli.get(i).copied())
            .unwrap_or(TraitDef::MULT_SCALE)
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
