//! Content-as-data (M4a): unit / resource / building definitions loaded from
//! `assets/data/*.ron` into plain sim structs.
//!
//! **Render-free and engine-free**: this module uses `serde` + `ron` + `std`
//! only (plus the ECS `Resource` derive so the driver can insert it), so the
//! whole content layer is loadable from a plain file path in headless tests —
//! no Bevy `AssetServer` involved. Nothing here reads the clock or a HashMap:
//! definitions keep their RON order, and every lookup is a linear scan over that
//! stable order, so iteration can never perturb sim outcomes.
//!
//! Costs and stats live in the RON, never in Rust constants — see the schema
//! note at the top of `assets/data/units.ron`.

use std::fmt;
use std::path::Path;

use bevy::ecs::prelude::Resource;

use crate::sim::UnitKind;
use ron::extensions::Extensions;
use serde::Deserialize;

// ---- definitions -----------------------------------------------------------

/// A post-MVP per-domain price tag (`biomass`/`aether`/`alloy`). Retained from
/// the design brief; the MVP charges [`UnitDef::mvp_alloy_cost`] instead.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Cost {
    pub resource: String,
    pub amount: u32,
}

/// One unit of the roster (Worker or a combat unit).
#[derive(Debug, Clone, Deserialize)]
pub struct UnitDef {
    pub id: String,
    pub name: String,
    /// Producing building for workers (`hq`).
    #[serde(default)]
    pub source: Option<String>,
    /// Producing building for combat units (a barracks id).
    #[serde(default)]
    pub barracks: Option<String>,
    /// Post-MVP per-domain cost (unused by the MVP sim).
    pub cost: Cost,
    /// Silhouette class (drives selection hit-boxes and, client-side, the
    /// sprite). Data, so a new unit needs no code change.
    pub mvp_kind: UnitKind,
    /// Alloy charged once when this unit's production is ordered.
    pub mvp_alloy_cost: u32,
    /// `FixedUpdate` ticks to finish the unit.
    pub mvp_train_ticks: u32,
    /// Alloy carried per trip (gatherers only).
    #[serde(default)]
    pub mvp_carry_capacity: u32,
    /// Ticks spent harvesting one load at a deposit (gatherers only).
    #[serde(default)]
    pub mvp_gather_ticks: u32,
    /// `FixedUpdate` ticks between two hits — a cadence in *ticks*, never
    /// seconds. `0` marks a non-combatant (and then `offense` must be 0 too).
    /// Deliberately **not** `#[serde(default)]`: a missing cadence must be a
    /// load error, not a silent 0 that makes a unit fire every tick.
    pub mvp_attack_ticks: u32,
    /// World-unit radius within which this unit can hit. `0` ⇒ non-combatant.
    pub mvp_attack_range: f32,
    pub speed: u32,
    pub offense: u32,
    pub defense: u32,
    pub armor: u32,
    /// The single prey this unit gets the nemesis bonus against (M4b).
    #[serde(default)]
    pub nemesis: Option<String>,
    #[serde(default)]
    pub gathers: bool,
    #[serde(default)]
    pub builds: bool,
}

/// A placeable/starting building and what it can train.
#[derive(Debug, Clone, Deserialize)]
pub struct BuildingDef {
    pub id: String,
    pub name: String,
    /// Alloy charged once when the building is placed.
    pub alloy_cost: u32,
    /// Unit ids this building can produce.
    pub produces: Vec<String>,
    /// Workers may deposit their load here (the HQ).
    #[serde(default)]
    pub dropoff: bool,
    /// Losing this building loses the match (M4c). Data, not a hardcoded "hq"
    /// string: the win condition is content.
    #[serde(default)]
    pub victory: bool,
    /// Design-scale Defense — the building's HP pool, through
    /// `mvp_combat.building_hp_per_defense` (M4c). Buildings are killable or
    /// there is no win condition, so this is **not** `#[serde(default)]`: a
    /// missing pool must be a load error, not a building with 0 HP.
    pub mvp_defense: u32,
    /// Design-scale Armor — flat mitigation per hit, on the same
    /// `mvp_combat.mitigation_per_armor` scale the units use.
    pub mvp_armor: u32,
}

/// One entry of the scripted AI's repeating army build order.
#[derive(Debug, Clone, Deserialize)]
pub struct ArmyItem {
    /// Unit id, which the barracks below must be able to produce.
    pub unit: String,
    /// How many of this unit per cycle of the build order.
    pub count: u32,
}

/// One tech opening in a strategy's `barracks` list: which building, the
/// earliest tick it may go up, and how far from the HQ it stands.
#[derive(Debug, Clone, Deserialize)]
pub struct BarracksOpening {
    /// Placeable (non-victory) building id.
    pub building: String,
    /// Earliest tick the commander will place this one.
    pub at_tick: u32,
    /// How far from its HQ it goes (world units; the seeded RNG picks only the
    /// direction).
    pub offset: f32,
}

/// One named strategy (B1) — a complete script a commander can be built from:
/// build order, thresholds and timings. **Every duration is in `FixedUpdate`
/// ticks**, never seconds, and every number here is content: there are no AI
/// tuning constants in Rust.
///
/// A strategy may open **several** barracks, which is how its army spans
/// domains; every unit of `army` must be producible by one of them, and is
/// trained there.
#[derive(Debug, Clone, Deserialize)]
pub struct StrategyDef {
    /// How everything else names this strategy (unique across the set).
    pub id: String,
    /// Ticks between two decisions (the commander's "APM").
    pub think_interval_ticks: u32,
    /// Workers it keeps mining before spending on anything else.
    pub worker_target: u32,
    /// The tech openings, in the order it wants them. A building **may repeat**
    /// (B3.5): each entry is one placement with its own `at_tick`/`offset`, and
    /// the commander trains across all of them — barracks count is how a
    /// strategy buys parallel production.
    pub barracks: Vec<BarracksOpening>,
    /// The repeating army build order.
    pub army: Vec<ArmyItem>,
    /// Combat units it wants before it attacks.
    pub attack_at_army: u32,
    /// Ticks between two attack waves.
    pub attack_interval_ticks: u32,
    /// Radius of the seeded scatter around the enemy HQ each wave aims at.
    pub attack_spread: f32,
    /// How many army units this strategy allows in production at one barracks
    /// at once (B3.5). The cap the commander tops its queue up to — **one item
    /// per decision**, so a depth of 3 fills over three decisions.
    ///
    /// This used to be the Rust constant `if b.queued == 0` in `sim::ai`, which
    /// pinned every strategy's throughput at one unit per `mvp_train_ticks`
    /// whatever its script said; as content it is a tuning lever like any
    /// other. `1` reproduces the old behaviour exactly (`queued < 1` *is*
    /// `queued == 0`).
    ///
    /// Deliberately **not** `#[serde(default)]`: a missing depth must be a load
    /// error, not a silent `0` — which would mean "never train", content the
    /// sim cannot run. `Content::validate` refuses `0` for the same reason.
    pub queue_depth: u32,
    /// The order this strategy's attack waves go out on (B4, F-048): a plain
    /// [`WaveOrder::Move`] or an [`WaveOrder::AttackMove`] that fights armed
    /// enemies on the way and then resumes its march.
    ///
    /// Deliberately **not** `#[serde(default)]`, like `queue_depth`: a missing
    /// wave order is a load error, so no strategy can play a wave order nobody
    /// chose.
    pub wave_order: WaveOrder,
}

/// Which order an AI attack wave is sent on (B4, F-048). Content, not code:
/// the switch between the two lives in `strategies.ron` (`move` /
/// `attack_move`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaveOrder {
    /// `Order::MoveTo`: a pure move. The wave fights only what is already in
    /// a unit's own reach as it walks, and never chases. Every match before
    /// B4's attack-move lever played this.
    Move,
    /// `Order::AttackMove`: march to the destination, engaging enemy armed
    /// units inside `CombatDef::attack_move_range` on the way (armed units before workers and
    /// buildings), then resume the march.
    AttackMove,
}

impl WaveOrder {
    /// The RON spelling, for fingerprints and reports.
    pub fn label(self) -> &'static str {
        match self {
            WaveOrder::Move => "move",
            WaveOrder::AttackMove => "attack_move",
        }
    }
}

impl StrategyDef {
    /// Total units in one cycle of the build order, or `None` if the counts do
    /// not fit a `u32`. Checked, not wrapping: the cursor arithmetic below
    /// divides by this, and `Content::validate` refuses content it cannot hold
    /// (F-005 — a sum that wraps in release is content the loader must reject,
    /// not a number the sim quietly mangles).
    pub fn cycle_len(&self) -> Option<u32> {
        self.army
            .iter()
            .try_fold(0u32, |acc, item| acc.checked_add(item.count))
    }

    /// The `n`-th unit id of the endlessly repeating build order. Pure integer
    /// walk over the RON order — no allocation, so a build order of a million
    /// units costs nothing, and no iteration order to leak.
    pub fn army_at(&self, n: u32) -> Option<&str> {
        let cycle = self.cycle_len()?;
        if cycle == 0 {
            return None;
        }
        let mut k = n % cycle;
        for item in &self.army {
            if k < item.count {
                return Some(&item.unit);
            }
            k -= item.count;
        }
        None
    }
}

/// The nemesis rule (consumed by combat in M4b).
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct NemesisBonus {
    /// The design-facing multiplier (`1.3` = +30%). Read *once*, here; per-hit
    /// damage uses [`NemesisBonus::mult_milli`] instead, so no float ever
    /// reaches the arithmetic that decides how much HP a unit loses.
    pub damage_mult: f32,
    pub ignore_armor: bool,
}

impl NemesisBonus {
    /// Denominator of the integer multiplier: the bonus is held as per-mille.
    pub const MULT_SCALE: u32 = 1_000;

    /// `damage_mult` as an integer per-mille (`1.3` → `1300`). The one and only
    /// float→int conversion in the damage path; rounding is half-away-from-zero
    /// (`f64::round`, computed in `f64` so `1.3 * 1000` is not first mangled by
    /// `f32`), so every machine gets the same integer and per-hit damage is
    /// bit-identical everywhere.
    ///
    /// The cast can only saturate for a multiplier `Content::validate` refuses
    /// to load ([`NemesisBonus::milli_exact`] is the check), so for any content
    /// the loader accepted this *is* `round(damage_mult * 1000)` — the formula
    /// documented in `units.ron` holds as written rather than approximately.
    pub fn mult_milli(&self) -> u32 {
        self.milli_exact().unwrap_or(u32::MAX)
    }

    /// `round(damage_mult * 1000)` when it is finite and fits a `u32`, else
    /// `None`. A multiplier the sim cannot represent is content the loader must
    /// reject: silently applying `u32::MAX` per-mille (≈4_294_967×) in place of
    /// the stated number would make the loader and the arithmetic disagree
    /// about what the data means.
    pub fn milli_exact(&self) -> Option<u32> {
        let scaled = (self.damage_mult as f64 * Self::MULT_SCALE as f64).round();
        if scaled.is_finite() && (0.0..=u32::MAX as f64).contains(&scaled) {
            Some(scaled as u32)
        } else {
            None
        }
    }
}

/// M4b combat scaling: how the 1-10 design stats become sim numbers. Data, so
/// balance changes never touch Rust.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct CombatDef {
    /// HP pool = `defense * hp_per_defense`.
    pub hp_per_defense: u32,
    /// Damage per hit = `offense * damage_per_offense`.
    pub damage_per_offense: u32,
    /// Flat mitigation per hit = `defender.armor * mitigation_per_armor`.
    pub mitigation_per_armor: u32,
    /// Movement = `speed * speed_per_point` world units per second.
    pub speed_per_point: f32,
    /// Radius within which an idle unit picks a fight (world units).
    pub engage_range: f32,
    /// Upper bound of the 1-10 design scale. Stats above it are rejected at
    /// load: validation that admits values the per-hit arithmetic cannot
    /// represent is worse than no validation at all.
    pub max_stat: u32,
    /// Buildings' HP pool = `building.mvp_defense * building_hp_per_defense`.
    /// Separate from `hp_per_defense` because a base is meant to outlast a
    /// soldier without leaving the 1-10 design scale.
    pub building_hp_per_defense: u32,
    /// Leash: how far a unit already chasing will follow before giving up. At
    /// least `engage_range` — pathing around an obstacle legitimately opens the
    /// straight-line gap, and a unit that dropped its target there would
    /// oscillate at the wall instead of coming around it.
    pub pursue_range: f32,
    /// Radius within which a unit under an **attack-move** acquires a target
    /// (B4, F-048): the attack-move's own number, used by attack-movers only,
    /// so it cannot move a match in which no strategy's `wave_order` is
    /// `attack_move`. Once chasing, the `pursue_range` leash applies as for
    /// every unit. Bounded below by the longest unit `mvp_attack_range` (an
    /// attack-mover always fights what it can already hit) and above by
    /// `pursue_range`. Required, like every combat number: no serde default.
    pub attack_move_range: f32,
}

/// How a resource enters the economy.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
pub enum Acquisition {
    Mined,
    Grown,
    Channeled,
}

/// One resource of the (post-MVP) three-domain economy.
#[derive(Debug, Clone, Deserialize)]
pub struct ResourceDef {
    pub id: String,
    pub name: String,
    pub domain: String,
    pub building: String,
    pub acquisition: Acquisition,
    pub powers: Vec<String>,
}

/// MVP economy tunables (the single currency and the interaction radii).
#[derive(Debug, Clone, Deserialize)]
pub struct EconomyDef {
    pub currency: String,
    /// Alloy each faction starts the match with.
    pub starting_alloy: u32,
    pub gather_range: f32,
    pub deposit_range: f32,
}

/// One General type (C1): a unit that leads an army in a campaign battle and
/// whose death loses that battle. Its own list in `units.ron` (`generals`), so
/// it never joins [`Content::units`]: a General is not trained, never mines,
/// never rides in an AI attack wave, and the roster-sized tables (production
/// counts, rally spokes, the pentagon) keep their shape.
///
/// The sim still addresses a General by a [`crate::sim::economy::UnitDefIdx`]:
/// the index space runs on past the roster, so `units.len() + k` is
/// `generals[k]` (see [`Content::unit_def`]). Combat, movement and the HP pool
/// read that one lookup, so a General fights under exactly the 4-stat model.
///
/// `deny_unknown_fields`: a General has **no nemesis** (an aura never touches
/// the hard counters, and neither does the General himself), so a `nemesis:`
/// written on one is a load error rather than a field serde quietly drops.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GeneralDef {
    pub id: String,
    pub name: String,
    /// Silhouette class, as for every unit.
    pub mvp_kind: UnitKind,
    /// Ticks between two hits. A General fights, so this is required and > 0.
    pub mvp_attack_ticks: u32,
    /// World-unit reach.
    pub mvp_attack_range: f32,
    pub speed: u32,
    pub offense: u32,
    pub defense: u32,
    pub armor: u32,
    /// What he does for the friendly units around him (C1 AC3).
    pub aura: AuraDef,
    /// The General as a plain unit definition, synthesised at load (never
    /// written in the RON): no cost, no training time, no nemesis, no gathering.
    /// What combat and movement read, through [`Content::unit_def`].
    #[serde(skip)]
    pub unit: Option<UnitDef>,
}

/// A stat an aura can change (C1). There is deliberately **no nemesis
/// variant**: an aura never touches the hard counters, so a `stat: Nemesis`
/// in the RON is a load error (an unknown variant), not a rule enforced later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum AuraStat {
    /// Adds design points of Offense (damage per hit).
    Offense,
    /// Adds design points of Defense: incoming damage is scaled by
    /// `defense / (defense + bonus)` (the HP pool itself never changes).
    Defense,
    /// Adds design points of Armor (flat mitigation per hit).
    Armor,
    /// Adds design points of Speed.
    Speed,
    /// × the attack interval (`mul` < 1 attacks faster).
    AttackTicks,
    /// Adds HP every `mvp_battle.regen_interval_ticks`.
    Regen,
}

/// One modifier in an aura: `add` design points to a stat, or (attack
/// interval only) `mul` it. Exactly one of the two, checked at load.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuraModifier {
    pub stat: AuraStat,
    #[serde(default)]
    pub add: Option<u32>,
    #[serde(default)]
    pub mul: Option<f32>,
}

impl AuraModifier {
    /// Per-mille scale of `mul`.
    pub const MULT_SCALE: u32 = 1000;

    /// `mul` as an integer per-mille factor (1000 = unchanged), so no float
    /// enters the per-tick arithmetic — the same treatment as the nemesis bonus.
    pub fn mul_milli(&self) -> u32 {
        self.mul
            .map(|m| (m * Self::MULT_SCALE as f32).round() as u32)
            .unwrap_or(Self::MULT_SCALE)
    }
}

/// How an aura's strength scales (C1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum AuraScaling {
    /// The modifiers as written.
    Flat,
    /// The Synthesis rule: the modifiers × the number of distinct domains
    /// among the friendly (non-General) units inside the radius, capped at
    /// `max`. A pure army gets ×1; no units, ×0.
    PerDistinctDomain { max: u32 },
}

/// A General's aura (C1): friendly units within `radius` get `modifiers`,
/// scaled by `scaling`. The aura's **kind** is its General type: auras of
/// different kinds stack; of the same kind, a unit takes the strongest.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuraDef {
    pub radius: f32,
    pub modifiers: Vec<AuraModifier>,
    pub scaling: AuraScaling,
}

/// Campaign-battle layout numbers (C1). Only read when a match is set up as a
/// battle ([`crate::sim::battle::BattleSetup`]); a skirmish never touches them.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BattleDef {
    /// How far in front of its base (towards the enemy) a side's Generals stand.
    pub general_front: f32,
    /// Spacing between two neighbours in a starting formation (world units).
    pub formation_spacing: f32,
    /// A `Regen` aura heals once every this many ticks.
    pub regen_interval_ticks: u32,
    /// How far behind its base (away from the enemy) a side's entry edge
    /// runs — where a retreat leaves the field.
    pub entry_edge_offset: f32,
    /// Spacing of a roster's buildings in their line across the base.
    pub building_spacing: f32,
    /// Units per rank in a roster's starting formation.
    pub formation_rank: u32,
}

/// The AI's battle behaviour around its General (C1 AC7, `mvp_general_ai`).
/// Only read in a campaign battle; a skirmish commander never touches it.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GeneralAiDef {
    /// An enemy fighter this close to one of its Generals pulls its army
    /// back onto him.
    pub guard_radius: f32,
    /// It retreats once it has lost this fraction, in (0, 1], of the most
    /// fighters it has had on the field.
    pub retreat_at_army_lost: f32,
}

impl GeneralAiDef {
    /// [`GeneralAiDef::retreat_at_army_lost`] in thousandths, so the AI's
    /// decision is integer arithmetic.
    pub fn retreat_milli(&self) -> u32 {
        (self.retreat_at_army_lost * 1000.0).round() as u32
    }
}

// ---- file shapes -----------------------------------------------------------

#[derive(Deserialize)]
struct UnitsFile {
    workers: Vec<UnitDef>,
    combat: Vec<UnitDef>,
    mvp_buildings: Vec<BuildingDef>,
    mvp_combat: CombatDef,
    nemesis_bonus: NemesisBonus,
    /// C1. Optional so content with no campaign layer (and every pre-C1 data
    /// set a test keeps) still loads: no Generals means no campaign battles.
    #[serde(default)]
    generals: Vec<GeneralDef>,
    #[serde(default)]
    mvp_battle: Option<BattleDef>,
    #[serde(default)]
    mvp_general_ai: Option<GeneralAiDef>,
}

#[derive(Deserialize)]
struct StrategiesFile {
    /// Id of the strategy a match uses when nothing names one.
    default: String,
    strategies: Vec<StrategyDef>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NationsFile {
    nations: Vec<crate::sim::nation::NationDef>,
}

#[derive(Deserialize)]
struct ResourcesFile {
    resources: Vec<ResourceDef>,
    mvp_active: Vec<String>,
    mvp_economy: EconomyDef,
    /// C3: optional, so content without it loads (and fingerprints) as before.
    #[serde(default)]
    three_resource: Option<crate::sim::three::ThreeResourceDef>,
}

// ---- the loaded content ----------------------------------------------------

/// All game content, parsed. Inserted as an ECS resource by the driver at
/// startup; constructed directly from a path in tests.
#[derive(Debug, Clone, Resource)]
pub struct Content {
    /// Workers first, then combat units — RON order, stable.
    pub units: Vec<UnitDef>,
    pub buildings: Vec<BuildingDef>,
    /// Stat scaling for combat (M4b).
    pub combat: CombatDef,
    pub nemesis_bonus: NemesisBonus,
    /// Every named strategy, in RON order (B1).
    pub strategies: Vec<StrategyDef>,
    /// Id of the strategy a match uses when nothing names one; always resolves
    /// to one of [`Content::strategies`] (the loader refuses content where it
    /// does not).
    pub default_strategy: String,
    /// The default strategy, resolved — the script a commander runs when no
    /// other is named (M4c's `mvp_ai`, promoted).
    pub ai: StrategyDef,
    pub resources: Vec<ResourceDef>,
    pub mvp_active: Vec<String>,
    pub economy: EconomyDef,
    /// The General types (C1), in RON order. **Not** part of [`Content::units`]
    /// — see [`GeneralDef`] for the index space they share.
    pub generals: Vec<GeneralDef>,
    /// Campaign-battle layout (C1); `None` in content with no campaign layer.
    pub battle: Option<BattleDef>,
    /// The battle AI's numbers (C1); `None` in content with no campaign layer.
    pub general_ai: Option<GeneralAiDef>,
    /// The nations (C2), in RON order, from the optional `nations.ron`. Empty
    /// in content with no nations: then no match can name one.
    pub nations: Vec<crate::sim::nation::NationDef>,
    /// The three-resource economy's numbers (C3), from `resources.ron`'s
    /// optional `three_resource` block. `None`: the content can only play
    /// Alloy-only matches.
    pub three_resource: Option<crate::sim::three::ThreeResourceDef>,
}

// ---- content fingerprint ----------------------------------------------------

/// A fingerprint of a whole loaded [`Content`]: a 64-bit hash plus a short
/// human-readable summary.
///
/// A command log is only meaningful against the content it was recorded with —
/// it names units and buildings, and the sim's behaviour is data. The
/// fingerprint is what lets a replay **refuse** a log recorded against
/// different content instead of quietly playing a different match.
///
/// Only [`hash`](Self::hash) is compared. `summary` is diagnostic: it is what a
/// mismatch message shows a human, and nothing is decided from it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ContentFingerprint {
    hash: u64,
    summary: String,
}

impl ContentFingerprint {
    /// "No content was ever stamped here." `0` is reserved for this — the hash
    /// below never returns it — so "unknown" is a value the type can hold
    /// rather than a hash collision waiting to be misread.
    pub const UNKNOWN: u64 = 0;

    pub fn unknown() -> Self {
        Self {
            hash: Self::UNKNOWN,
            summary: String::new(),
        }
    }

    pub fn hash(&self) -> u64 {
        self.hash
    }

    pub fn summary(&self) -> &str {
        &self.summary
    }

    pub fn is_known(&self) -> bool {
        self.hash != Self::UNKNOWN
    }
}

impl fmt::Display for ContentFingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_known() {
            write!(f, "{:016x} ({})", self.hash, self.summary)
        } else {
            write!(f, "unknown")
        }
    }
}

/// FNV-1a over the pieces of a [`Content`]. Same discipline as the sim's state
/// hash: floats by their exact bits (never rounded into a bucket), strings
/// length-prefixed (so `"ab" + "c"` cannot collide with `"a" + "bc"`), every
/// sequence in its RON order, and no map anywhere — nothing here can depend on
/// iteration order.
struct Fnv(u64);

impl Fnv {
    fn new() -> Self {
        Fnv(0xcbf2_9ce4_8422_2325)
    }

    fn u64(&mut self, v: u64) -> &mut Self {
        self.0 ^= v;
        self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        self
    }

    fn u32(&mut self, v: u32) -> &mut Self {
        self.u64(v as u64)
    }

    fn bool(&mut self, v: bool) -> &mut Self {
        self.u64(v as u64)
    }

    /// Exact bits, so a change no decimal rendering would show still moves the
    /// fingerprint.
    fn f32(&mut self, v: f32) -> &mut Self {
        self.u64(v.to_bits() as u64)
    }

    fn str(&mut self, s: &str) -> &mut Self {
        self.u64(s.len() as u64);
        for b in s.bytes() {
            self.u64(b as u64);
        }
        self
    }

    fn opt_str(&mut self, s: &Option<String>) -> &mut Self {
        match s {
            None => self.u64(0),
            Some(s) => {
                self.u64(1);
                self.str(s)
            }
        }
    }

    fn strs(&mut self, all: &[String]) -> &mut Self {
        self.u64(all.len() as u64);
        for s in all {
            self.str(s);
        }
        self
    }

    /// Never [`ContentFingerprint::UNKNOWN`]: `0` means "nothing was stamped",
    /// and a real fingerprint must never be mistaken for that.
    fn finish(&self) -> u64 {
        if self.0 == ContentFingerprint::UNKNOWN {
            1
        } else {
            self.0
        }
    }
}

/// One nation trait, into the fingerprint (C2's form, shared by both lists).
fn hash_trait(h: &mut Fnv, t: &crate::sim::nation::TraitDef) {
    use crate::sim::nation::TraitTarget as T;
    match &t.applies_to {
        T::Unit(id) => h.u64(0).str(id),
        T::Barracks(id) => h.u64(1).str(id),
        T::Workers => h.u64(2),
        T::CombatUnits => h.u64(3),
        T::Building(id) => h.u64(4).str(id),
        T::Buildings => h.u64(5),
    };
    h.u64(t.stat as u64);
    h.bool(t.add.is_some()).u64(t.add.unwrap_or(0) as i64 as u64);
    h.bool(t.mul.is_some()).u32(t.mul_milli());
}

/// Why content failed to load. Carries the offending path/message so a startup
/// failure is diagnosable without a debugger.
#[derive(Debug)]
pub enum ContentError {
    Io {
        path: String,
        msg: String,
    },
    Parse {
        path: String,
        msg: String,
    },
    /// The files parsed but the content is not playable (a gatherer with no
    /// gather data, a free unit, a building producing an unknown unit, ...).
    /// Caught at load so it can never surface as a worker mining nothing.
    Invalid {
        msg: String,
    },
}

impl fmt::Display for ContentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ContentError::Io { path, msg } => write!(f, "cannot read {path}: {msg}"),
            ContentError::Parse { path, msg } => write!(f, "cannot parse {path}: {msg}"),
            ContentError::Invalid { msg } => write!(f, "invalid content: {msg}"),
        }
    }
}

impl std::error::Error for ContentError {}

fn load_ron<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, ContentError> {
    let text = std::fs::read_to_string(path).map_err(|e| ContentError::Io {
        path: path.display().to_string(),
        msg: e.to_string(),
    })?;
    // `IMPLICIT_SOME` lets the data files write `nemesis: "ravager"` instead of
    // `Some("ravager")` — the RON stays readable as content, not as Rust types.
    let opts = ron::Options::default().with_default_extension(Extensions::IMPLICIT_SOME);
    opts.from_str(&text).map_err(|e| ContentError::Parse {
        path: path.display().to_string(),
        msg: e.to_string(),
    })
}

/// Where the shipped data files live, relative to the working directory.
pub const DATA_DIR: &str = "assets/data";

impl Content {
    /// Load the shipped content from [`DATA_DIR`] (the driver's startup path).
    pub fn load_default() -> Result<Self, ContentError> {
        Self::load_from_dir(Path::new(DATA_DIR))
    }

    /// Load `units.ron` + `resources.ron` + `strategies.ron` from a data
    /// directory. Pure `std` file IO — works headless, with no engine asset
    /// pipeline.
    pub fn load_from_dir(dir: &Path) -> Result<Self, ContentError> {
        let units_file: UnitsFile = load_ron(&dir.join("units.ron"))?;
        let resources_file: ResourcesFile = load_ron(&dir.join("resources.ron"))?;
        let strategies_file: StrategiesFile = load_ron(&dir.join("strategies.ron"))?;
        // C2: optional, so content with no nations (every pre-C2 data set a
        // test keeps) still loads, and loads exactly as it did.
        let nations_path = dir.join("nations.ron");
        let nations = if nations_path.exists() {
            load_ron::<NationsFile>(&nations_path)?.nations
        } else {
            Vec::new()
        };

        let mut units = units_file.workers;
        units.extend(units_file.combat);

        let mut content = Content {
            units,
            buildings: units_file.mvp_buildings,
            combat: units_file.mvp_combat,
            nemesis_bonus: units_file.nemesis_bonus,
            strategies: strategies_file.strategies,
            default_strategy: strategies_file.default,
            // Resolved below — `validate` refuses content whose default names
            // no strategy, so this placeholder never survives a load.
            ai: StrategyDef {
                id: String::new(),
                think_interval_ticks: 0,
                worker_target: 0,
                barracks: Vec::new(),
                army: Vec::new(),
                attack_at_army: 0,
                attack_interval_ticks: 0,
                attack_spread: 0.0,
                queue_depth: 0,
                wave_order: WaveOrder::Move,
            },
            resources: resources_file.resources,
            mvp_active: resources_file.mvp_active,
            economy: resources_file.mvp_economy,
            generals: units_file.generals,
            battle: units_file.mvp_battle,
            general_ai: units_file.mvp_general_ai,
            nations,
            three_resource: resources_file.three_resource,
        };
        content.synthesise_general_units();
        content.validate()?;
        // `validate` has just proved the default names a strategy, so this
        // lookup cannot fail.
        let default = content
            .strategy(&content.default_strategy)
            .expect("validate admits only a default that names a strategy")
            .clone();
        content.ai = default;
        Ok(content)
    }

    /// Reject content the sim cannot run. Anything the sim *reads* must be
    /// stated in the data: a serde default that silently becomes 0 would show up
    /// as a worker mining nothing forever, or a unit that costs nothing.
    fn validate(&self) -> Result<(), ContentError> {
        let bad = |msg: String| Err(ContentError::Invalid { msg });

        // **Ids are the coordinate everything else is written in**, so they must
        // be unique *within their namespace* — and that has to be enforced
        // where content is admitted, not noticed later.
        //
        // Every lookup here is `position(|x| x.id == id)`: first match wins. A
        // duplicate therefore makes an id ambiguous, and the second definition
        // unreachable by name. Nothing in the sim notices (it runs on indices),
        // but the command log names content **by id** — so `index -> id ->
        // index` stops being the identity, and a log that records "place the
        // second `foundry`" replays as the first one, with different costs and
        // different buildings, and no error anywhere. That is F-005's rule one
        // boundary further out: validation that admits values the rest of the
        // system cannot represent uniquely is worse than none.
        //
        // The namespaces are separate on purpose. Units, buildings and
        // resources are looked up by three different functions, and every
        // reference in the data says which kind it means (`produces` names
        // units, a strategy's `barracks` names a building, `economy.currency` names a
        // resource), so a unit and a building *may* share an id — nothing can
        // confuse them. What may never repeat is an id inside one list.
        for (what, ids) in [
            (
                "unit",
                self.units.iter().map(|u| u.id.as_str()).collect::<Vec<_>>(),
            ),
            (
                "building",
                self.buildings
                    .iter()
                    .map(|b| b.id.as_str())
                    .collect::<Vec<_>>(),
            ),
            (
                "resource",
                self.resources
                    .iter()
                    .map(|r| r.id.as_str())
                    .collect::<Vec<_>>(),
            ),
            (
                "strategy",
                self.strategies
                    .iter()
                    .map(|s| s.id.as_str())
                    .collect::<Vec<_>>(),
            ),
        ] {
            // A linear scan, not a set: the rosters are tiny, and this way the
            // error names *both* positions and no hash is involved.
            for (i, id) in ids.iter().enumerate() {
                if let Some(j) = ids[..i].iter().position(|earlier| earlier == id) {
                    return bad(format!(
                        "duplicate {what} id `{id}` at positions {j} and {i}: an id \
                         is how the command log names content, so a repeated one \
                         makes a recorded order replay as a different definition"
                    ));
                }
            }
        }

        for b in &self.buildings {
            if b.alloy_cost == 0 {
                return bad(format!("building `{}` has no Alloy cost", b.id));
            }
            // A building is killable (M4c: the win condition is a dead HQ), so
            // it needs a real pool, on the same design scale the units use.
            if b.mvp_defense == 0 {
                return bad(format!("building `{}` has no HP pool (mvp_defense 0)", b.id));
            }
            for (stat, value) in [("mvp_defense", b.mvp_defense), ("mvp_armor", b.mvp_armor)] {
                if value > self.combat.max_stat {
                    return bad(format!(
                        "building `{}` has {stat} {value}, above the design scale max {}",
                        b.id, self.combat.max_stat
                    ));
                }
            }
            for p in &b.produces {
                if self.unit_index(p).is_none() {
                    return bad(format!("building `{}` produces unknown unit `{p}`", b.id));
                }
            }
        }

        for u in &self.units {
            if u.mvp_alloy_cost == 0 {
                return bad(format!("unit `{}` has no Alloy cost", u.id));
            }
            if u.mvp_train_ticks == 0 {
                return bad(format!("unit `{}` has no training time", u.id));
            }
            if u.gathers && (u.mvp_carry_capacity == 0 || u.mvp_gather_ticks == 0) {
                return bad(format!(
                    "unit `{}` gathers but declares no mvp_carry_capacity / mvp_gather_ticks",
                    u.id
                ));
            }
            if !self.buildings.iter().any(|b| b.produces.contains(&u.id)) {
                return bad(format!("unit `{}` has no producing building", u.id));
            }

            // Combat (M4b). Every unit is killable, so every unit needs a real
            // HP pool and a way to move; only units that can actually hurt
            // something carry a cadence and a reach — and they must carry both.
            for (stat, value) in [
                ("offense", u.offense),
                ("defense", u.defense),
                ("armor", u.armor),
                ("speed", u.speed),
            ] {
                if value > self.combat.max_stat {
                    return bad(format!(
                        "unit `{}` has {stat} {value}, above the design scale max {}",
                        u.id, self.combat.max_stat
                    ));
                }
            }
            if u.defense == 0 {
                return bad(format!("unit `{}` has no HP pool (defense 0)", u.id));
            }
            if u.speed == 0 {
                return bad(format!("unit `{}` cannot move (speed 0)", u.id));
            }
            if u.offense > 0 {
                if u.mvp_attack_ticks == 0 {
                    return bad(format!("unit `{}` has offense but no attack cadence", u.id));
                }
                if !(u.mvp_attack_range.is_finite() && u.mvp_attack_range > 0.0) {
                    return bad(format!("unit `{}` has offense but no attack range", u.id));
                }
                if u.mvp_attack_range > self.combat.engage_range {
                    return bad(format!(
                        "unit `{}` reaches further than it will chase",
                        u.id
                    ));
                }
            } else if u.mvp_attack_ticks != 0 || u.mvp_attack_range != 0.0 {
                return bad(format!(
                    "unit `{}` has no offense but declares attack data",
                    u.id
                ));
            }
            if let Some(prey) = &u.nemesis {
                if self.unit_index(prey).is_none() {
                    return bad(format!("unit `{}` names unknown nemesis `{prey}`", u.id));
                }
            }
        }

        // The Generals (C1). A General is a unit the sim addresses by a
        // `UnitDefIdx` past the roster, so its id must be unique among the
        // Generals **and** distinct from every roster unit id: the two share
        // one index space, and a shared id would make "which unit is this"
        // depend on which list was searched first.
        for (i, g) in self.generals.iter().enumerate() {
            if let Some(j) = self.generals[..i].iter().position(|h| h.id == g.id) {
                return bad(format!(
                    "duplicate general id `{}` at positions {j} and {i}",
                    g.id
                ));
            }
            if self.unit_index(&g.id).is_some() {
                return bad(format!(
                    "general `{}` shares its id with a roster unit; Generals and \
                     units share one index space",
                    g.id
                ));
            }
            for (stat, value) in [
                ("offense", g.offense),
                ("defense", g.defense),
                ("armor", g.armor),
                ("speed", g.speed),
            ] {
                if value > self.combat.max_stat {
                    return bad(format!(
                        "general `{}` has {stat} {value}, above the design scale max {}",
                        g.id, self.combat.max_stat
                    ));
                }
            }
            if g.defense == 0 {
                return bad(format!("general `{}` has no HP pool (defense 0)", g.id));
            }
            if g.speed == 0 {
                return bad(format!("general `{}` cannot move (speed 0)", g.id));
            }
            // A General fights (the brief: "the General fights, but is fragile
            // enough that protecting him is a decision").
            if g.offense == 0 || g.mvp_attack_ticks == 0 {
                return bad(format!(
                    "general `{}` must fight: offense and mvp_attack_ticks positive",
                    g.id
                ));
            }
            if !(g.mvp_attack_range.is_finite()
                && g.mvp_attack_range > 0.0
                && g.mvp_attack_range <= self.combat.engage_range
                && g.mvp_attack_range <= self.combat.attack_move_range)
            {
                return bad(format!(
                    "general `{}` mvp_attack_range must be finite, positive and within \
                     engage_range and attack_move_range",
                    g.id
                ));
            }
        }
        for g in &self.generals {
            let a = &g.aura;
            if !(a.radius.is_finite() && a.radius > 0.0) {
                return bad(format!("general `{}` aura radius must be finite and positive", g.id));
            }
            check_aura_modifiers(&format!("general `{}` aura", g.id), &a.modifiers, &self.combat)
                .map_err(|msg| ContentError::Invalid { msg })?;
            if let AuraScaling::PerDistinctDomain { max } = a.scaling {
                if max == 0 || max as usize > self.resources.len() {
                    return bad(format!(
                        "general `{}` aura PerDistinctDomain max must be 1..={} (the domains)",
                        g.id,
                        self.resources.len()
                    ));
                }
            }
        }
        if !self.generals.is_empty() && self.battle.is_none() {
            return bad("generals need an mvp_battle block".to_string());
        }
        if let Some(b) = &self.battle {
            for (what, v) in [
                ("general_front", b.general_front),
                ("formation_spacing", b.formation_spacing),
                ("entry_edge_offset", b.entry_edge_offset),
                ("building_spacing", b.building_spacing),
            ] {
                if !(v.is_finite() && v > 0.0) {
                    return bad(format!("mvp_battle {what} must be finite and positive"));
                }
            }
            // A roster's first rank stands one spacing behind the Generals and
            // must stand at least one spacing inside the entry edge.
            if b.general_front + b.entry_edge_offset < 2.0 * b.formation_spacing {
                return bad(format!(
                    "mvp_battle general_front + entry_edge_offset ({} + {}) must be at least \
                     2 * formation_spacing ({}): a roster's first rank must stand one spacing \
                     inside the entry edge",
                    b.general_front, b.entry_edge_offset, b.formation_spacing
                ));
            }
            if b.regen_interval_ticks == 0 {
                return bad("mvp_battle regen_interval_ticks must be positive".to_string());
            }
            if b.formation_rank == 0 {
                return bad("mvp_battle formation_rank must be positive".to_string());
            }
        }
        if !self.generals.is_empty() && self.general_ai.is_none() {
            return bad("generals need an mvp_general_ai block".to_string());
        }
        if let Some(ai) = &self.general_ai {
            if !(ai.guard_radius.is_finite() && ai.guard_radius > 0.0) {
                return bad("mvp_general_ai guard_radius must be finite and positive".to_string());
            }
            let lost = ai.retreat_at_army_lost;
            if !(lost.is_finite() && lost > 0.0 && lost <= 1.0) || ai.retreat_milli() == 0 {
                return bad("mvp_general_ai retreat_at_army_lost must be in (0, 1]".to_string());
            }
        }

        // Combat scaling: every factor the sim multiplies a design stat by has
        // to be stated and non-degenerate — a 0 here would silently produce
        // units with no HP, no damage, or no movement.
        let c = &self.combat;
        if c.building_hp_per_defense == 0 {
            return bad("mvp_combat building_hp_per_defense must be positive".to_string());
        }
        if c.hp_per_defense == 0 || c.damage_per_offense == 0 || c.mitigation_per_armor == 0 {
            return bad("mvp_combat scaling factors must be positive".to_string());
        }
        // `is_finite` matters: `f32::INFINITY > 0.0` is true, and NaN compares
        // false against everything, so a bare `> 0.0` admits both.
        if !(c.speed_per_point.is_finite() && c.speed_per_point > 0.0) {
            return bad("mvp_combat speed_per_point must be finite and positive".to_string());
        }
        if !(c.engage_range.is_finite() && c.engage_range > 0.0) {
            return bad("mvp_combat engage_range must be finite and positive".to_string());
        }
        if !c.pursue_range.is_finite() {
            return bad("mvp_combat pursue_range must be finite".to_string());
        }
        if c.pursue_range < c.engage_range {
            return bad("mvp_combat pursue_range must be >= engage_range".to_string());
        }
        let longest_reach = self
            .units
            .iter()
            .map(|u| u.mvp_attack_range)
            .fold(0.0_f32, f32::max);
        if !(c.attack_move_range.is_finite()
            && c.attack_move_range >= longest_reach
            && c.attack_move_range <= c.pursue_range)
        {
            return bad(format!(
                "mvp_combat attack_move_range must be finite, >= the longest unit \
                 mvp_attack_range ({longest_reach}) and <= pursue_range"
            ));
        }
        if c.max_stat == 0 {
            return bad("mvp_combat max_stat must be positive".to_string());
        }
        let mult = self.nemesis_bonus.damage_mult;
        if !(mult.is_finite() && mult >= 1.0) {
            return bad("nemesis_bonus.damage_mult must be finite and >= 1.0".to_string());
        }
        // The multiplier is used as an integer per-mille. If `round(mult * 1000)`
        // does not fit a `u32` the sim cannot apply the number the data states,
        // and the representability proof below would be computed from a
        // saturated stand-in rather than from the content. Reject it here.
        let Some(milli) = self.nemesis_bonus.milli_exact() else {
            return bad(format!(
                "nemesis_bonus.damage_mult {mult} cannot be held as an integer \
                 per-mille (round(mult * {}) must fit u32)",
                NemesisBonus::MULT_SCALE
            ));
        };

        // Representability: every product the sim will later derive from this
        // data must fit the `u32` it counts in — at the largest stat the loader
        // itself calls legal (`max_stat`).
        //
        // The arithmetic here is **checked**, not raw. A validator that can
        // overflow is not a validator: in debug it panics where `load_from_dir`
        // promises an `Err`, and in release it wraps and *accepts* content the
        // sim cannot represent — the F-005 failure mode, one level up. `None`
        // (the product does not even fit `u64`) is a rejection exactly like a
        // product that does not fit `u32`, so the loader's notion of legal and
        // the saturating backstop in `combat::damage_per_hit` can never
        // disagree about what is legal.
        let max_stat = c.max_stat as u64;
        let milli = milli as u64;
        let peak_hp = max_stat.checked_mul(c.hp_per_defense as u64);
        let peak_base = max_stat.checked_mul(c.damage_per_offense as u64);
        let peak_damage = peak_base
            .and_then(|b| b.checked_mul(milli))
            .map(|p| p / NemesisBonus::MULT_SCALE as u64);
        let peak_mitigation = max_stat.checked_mul(c.mitigation_per_armor as u64);
        let peak_building_hp = max_stat.checked_mul(c.building_hp_per_defense as u64);
        let representable = |v: Option<u64>| v.is_some_and(|v| v <= u32::MAX as u64);
        for (what, value) in [
            ("HP pool", peak_hp),
            ("base damage", peak_base),
            ("nemesis damage", peak_damage),
            ("armor mitigation", peak_mitigation),
            ("building HP pool", peak_building_hp),
        ] {
            if !representable(value) {
                return bad(format!(
                    "mvp_combat scaling overflows the sim's u32 arithmetic: peak \
                     {what} at max_stat {} is {}",
                    c.max_stat,
                    value.map_or("beyond u64".to_string(), |v| v.to_string())
                ));
            }
        }

        // The win condition is data (M4c): exactly one building id is the thing
        // whose loss ends the match. Zero would make the match unwinnable and
        // two would make "the enemy HQ" ambiguous.
        let victory: Vec<&str> = self
            .buildings
            .iter()
            .filter(|b| b.victory)
            .map(|b| b.id.as_str())
            .collect();
        if victory.len() != 1 {
            return bad(format!(
                "exactly one building must be the victory target; found {:?}",
                victory
            ));
        }

        // The strategies (B1, promoted from M4c's single `mvp_ai`). Everything
        // they count is ticks, and every id they name has to resolve — a script
        // with a typo would show up as a commander that quietly never builds
        // anything. **Every** strategy is checked, not merely the default: an
        // entry is only unreachable until the day a match names it, and content
        // the sim cannot run must never be admitted in the first place.
        if self.strategies.is_empty() {
            return bad("strategies: the set is empty".to_string());
        }
        for s in &self.strategies {
            let who = &s.id;
            if s.think_interval_ticks == 0 {
                return bad(format!("strategy `{who}` think_interval_ticks must be positive"));
            }
            if s.attack_interval_ticks == 0 {
                return bad(format!("strategy `{who}` attack_interval_ticks must be positive"));
            }
            if s.worker_target == 0 {
                return bad(format!("strategy `{who}` worker_target must be positive"));
            }
            if s.attack_at_army == 0 {
                return bad(format!("strategy `{who}` attack_at_army must be positive"));
            }
            // Depth 0 is "never put a unit in production": a script that
            // declares an army and can never build one. Content the sim cannot
            // run is refused at load, by name, not discovered as a commander
            // that quietly sits still.
            if s.queue_depth == 0 {
                return bad(format!("strategy `{who}` queue_depth must be positive"));
            }
            if !(s.attack_spread.is_finite() && s.attack_spread >= 0.0) {
                return bad(format!(
                    "strategy `{who}` attack_spread must be finite and non-negative"
                ));
            }

            // The tech openings. A strategy with none could never train the
            // army it declares (every unit must come from one of them).
            //
            // A building **may** appear more than once (B3.5 AC0b): each entry
            // is one placement, with its own `at_tick` and `offset`, and the
            // commander trains across all of them. Barracks count is the
            // throughput lever (`economy::production` advances only a queue's
            // head, so one barracks is one unit at a time whatever the depth,
            // F-027), so repeated openings are exactly how a strategy buys
            // parallel production. Two identical entries are legal too — the
            // seeded RNG picks each placement's direction.
            if s.barracks.is_empty() {
                return bad(format!("strategy `{who}` opens no barracks"));
            }
            let mut opened: Vec<usize> = Vec::with_capacity(s.barracks.len());
            for opening in &s.barracks {
                if !(opening.offset.is_finite() && opening.offset > 0.0) {
                    return bad(format!(
                        "strategy `{who}` barracks `{}` offset must be finite and positive",
                        opening.building
                    ));
                }
                let Some(def) = self.building_index(&opening.building) else {
                    return bad(format!(
                        "strategy `{who}` barracks `{}` is not a building",
                        opening.building
                    ));
                };
                if self.buildings[def].victory {
                    return bad(format!(
                        "strategy `{who}` barracks `{}` is the victory target, not a \
                         placeable barracks",
                        opening.building
                    ));
                }
                if !opened.contains(&def) {
                    opened.push(def);
                }
            }

            if s.army.is_empty() {
                return bad(format!("strategy `{who}` army build order is empty"));
            }
            for item in &s.army {
                if item.count == 0 {
                    return bad(format!(
                        "strategy `{who}` army entry `{}` has count 0",
                        item.unit
                    ));
                }
                let Some(unit) = self.unit_index(&item.unit) else {
                    return bad(format!(
                        "strategy `{who}` army names unknown unit `{}`",
                        item.unit
                    ));
                };
                // The heart of it: a strategy may only ask for what the
                // barracks *it opens* can produce. Otherwise the commander
                // would stall forever on a unit it has nowhere to train.
                if !opened.iter().any(|&b| self.produces(b, unit)) {
                    return bad(format!(
                        "strategy `{who}` army names `{}`, which none of its barracks \
                         ({}) can produce",
                        item.unit,
                        s.barracks
                            .iter()
                            .map(|o| o.building.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
            }
            // Checked, in the arithmetic the sim will actually do: the
            // build-order cursor is taken modulo this sum, and a sum that wraps
            // would silently re-point the commander at a different unit (F-005).
            if s.cycle_len().is_none() {
                return bad(format!("strategy `{who}` army counts overflow u32"));
            }
        }
        // The default has to name one of them, or a match has no script at all.
        if self.strategy_index(&self.default_strategy).is_none() {
            return bad(format!(
                "default strategy `{}` names no strategy",
                self.default_strategy
            ));
        }

        if self.resources.iter().all(|r| r.id != self.economy.currency) {
            return bad(format!(
                "mvp_economy currency `{}` is not a declared resource",
                self.economy.currency
            ));
        }
        if !(self.economy.gather_range.is_finite()
            && self.economy.gather_range > 0.0
            && self.economy.deposit_range.is_finite()
            && self.economy.deposit_range > 0.0)
        {
            return bad("mvp_economy ranges must be finite and positive".to_string());
        }
        // The three-resource block (C3), before the nations: their traits are
        // checked against it.
        crate::sim::three::validate(self).map_err(|msg| ContentError::Invalid { msg })?;
        // The nations (C2), last: their traits are checked against the rest.
        crate::sim::nation::validate(self).map_err(|msg| ContentError::Invalid { msg })?;
        Ok(())
    }

    /// Fingerprint of this whole content set — every field of every definition,
    /// in RON order.
    ///
    /// **Deliberately the whole thing, including the post-MVP `cost` fields the
    /// MVP sim never reads.** That means an edit which could not have changed
    /// sim behaviour still invalidates a stored log. The asymmetry decides it: a
    /// false rejection is loud, immediate and recoverable (the log is still
    /// readable, and `MatchLog::load` will still parse it for inspection),
    /// while a false accept is the silent wrong replay this exists to
    /// eliminate. Narrowing the scope to "the fields the sim reads today" would
    /// also mean the fingerprint's meaning changes every time a field starts
    /// being read — the kind of rule that is true when written and false a
    /// milestone later.
    ///
    /// Order-stable and float-exact by construction (see [`Fnv`]): definitions
    /// keep their RON order, floats are hashed by their bits, and no map is
    /// involved, so two loads of the same files always agree.
    pub fn fingerprint(&self) -> ContentFingerprint {
        let mut h = Fnv::new();
        // A tag per section, so moving a value from one list to another cannot
        // leave the fingerprint unchanged.
        h.str("units").u64(self.units.len() as u64);
        for u in &self.units {
            h.str(&u.id).str(&u.name);
            h.opt_str(&u.source).opt_str(&u.barracks);
            h.str(&u.cost.resource).u32(u.cost.amount);
            h.u64(match u.mvp_kind {
                UnitKind::Worker => 0,
                UnitKind::Soldier => 1,
                UnitKind::Scout => 2,
            });
            h.u32(u.mvp_alloy_cost)
                .u32(u.mvp_train_ticks)
                .u32(u.mvp_carry_capacity)
                .u32(u.mvp_gather_ticks)
                .u32(u.mvp_attack_ticks);
            h.f32(u.mvp_attack_range);
            h.u32(u.speed).u32(u.offense).u32(u.defense).u32(u.armor);
            h.opt_str(&u.nemesis).bool(u.gathers).bool(u.builds);
        }

        h.str("buildings").u64(self.buildings.len() as u64);
        for b in &self.buildings {
            h.str(&b.id).str(&b.name).u32(b.alloy_cost);
            h.strs(&b.produces);
            h.bool(b.dropoff).bool(b.victory);
            h.u32(b.mvp_defense).u32(b.mvp_armor);
        }

        h.str("combat");
        h.u32(self.combat.hp_per_defense)
            .u32(self.combat.damage_per_offense)
            .u32(self.combat.mitigation_per_armor);
        h.f32(self.combat.speed_per_point)
            .f32(self.combat.engage_range)
            .f32(self.combat.pursue_range)
            .f32(self.combat.attack_move_range);
        h.u32(self.combat.max_stat)
            .u32(self.combat.building_hp_per_defense);

        h.str("nemesis");
        h.f32(self.nemesis_bonus.damage_mult)
            .bool(self.nemesis_bonus.ignore_armor);

        // The strategies, in RON order — never a map's, and never only the
        // default: an edit to any entry is an edit to the content a log was
        // recorded under.
        h.str("strategies").u64(self.strategies.len() as u64);
        for s in &self.strategies {
            h.str(&s.id)
                .u32(s.think_interval_ticks)
                .u32(s.worker_target)
                .u32(s.attack_at_army)
                .u32(s.attack_interval_ticks)
                .f32(s.attack_spread)
                .u32(s.queue_depth)
                .str(s.wave_order.label())
                .u64(s.barracks.len() as u64);
            for o in &s.barracks {
                h.str(&o.building).u32(o.at_tick).f32(o.offset);
            }
            h.u64(s.army.len() as u64);
            for item in &s.army {
                h.str(&item.unit).u32(item.count);
            }
        }
        // The default is part of the content: the same set with a different
        // default plays a different match.
        h.str("default_strategy").str(&self.default_strategy);
        // `ai` is the resolved default, hashed by the id it resolved to.
        h.str("ai").str(&self.ai.id);

        h.str("resources").u64(self.resources.len() as u64);
        for r in &self.resources {
            h.str(&r.id).str(&r.name).str(&r.domain).str(&r.building);
            h.u64(match r.acquisition {
                Acquisition::Mined => 0,
                Acquisition::Grown => 1,
                Acquisition::Channeled => 2,
            });
            h.strs(&r.powers);
        }
        h.str("mvp_active").strs(&self.mvp_active);

        h.str("economy")
            .str(&self.economy.currency)
            .u32(self.economy.starting_alloy)
            .f32(self.economy.gather_range)
            .f32(self.economy.deposit_range);

        // C1: the campaign layer. Mixed in **only when present**, so a content
        // set with no Generals and no battle block fingerprints exactly as it
        // did before C1 existed (a recorded skirmish log stays replayable
        // against such content).
        if !self.generals.is_empty() {
            h.str("generals").u64(self.generals.len() as u64);
            for g in &self.generals {
                h.str(&g.id).str(&g.name);
                h.u64(match g.mvp_kind {
                    UnitKind::Worker => 0,
                    UnitKind::Soldier => 1,
                    UnitKind::Scout => 2,
                });
                h.u32(g.mvp_attack_ticks).f32(g.mvp_attack_range);
                h.u32(g.speed).u32(g.offense).u32(g.defense).u32(g.armor);
                h.str("aura").f32(g.aura.radius);
                h.u64(g.aura.modifiers.len() as u64);
                for m in &g.aura.modifiers {
                    h.u64(m.stat as u64);
                    h.u32(m.add.unwrap_or(0)).u32(m.mul_milli());
                }
                match g.aura.scaling {
                    AuraScaling::Flat => h.u64(0),
                    AuraScaling::PerDistinctDomain { max } => h.u64(1).u32(max),
                };
            }
        }
        if let Some(b) = &self.battle {
            h.str("battle")
                .f32(b.general_front)
                .f32(b.formation_spacing)
                .u32(b.regen_interval_ticks)
                .f32(b.entry_edge_offset)
                .f32(b.building_spacing)
                .u32(b.formation_rank);
        }
        if let Some(ai) = &self.general_ai {
            h.str("general_ai")
                .f32(ai.guard_radius)
                .f32(ai.retreat_at_army_lost);
        }

        // C2: the nations, mixed in only when present (as C1's sections are),
        // so content with no nations fingerprints exactly as before C2.
        if !self.nations.is_empty() {
            h.str("nations").u64(self.nations.len() as u64);
            for n in &self.nations {
                h.str(&n.id).str(&n.name).opt_str(&n.domain);
                for (r, g, b) in [n.colour, n.accent] {
                    h.f32(r).f32(g).f32(b);
                }
                h.str(&n.general).str(&n.strategy);
                h.u64(n.traits.len() as u64);
                for t in &n.traits {
                    hash_trait(&mut h, t);
                }
                // C3: mixed in only when present, so a nation without a
                // three-resource list hashes as it did before C3.
                if !n.three_resource_traits.is_empty() {
                    h.str("three_resource_traits")
                        .u64(n.three_resource_traits.len() as u64);
                    for t in &n.three_resource_traits {
                        hash_trait(&mut h, t);
                    }
                }
                // Mixed in only when present, so a nation without one hashes
                // as it did before the trait kind existed.
                if let Some(syn) = &n.synthesis {
                    h.str("synthesis").f32(syn.radius).u32(syn.max);
                    h.u64(syn.modifiers.len() as u64);
                    for m in &syn.modifiers {
                        h.u64(m.stat as u64)
                            .bool(m.add.is_some())
                            .u32(m.add.unwrap_or(0))
                            .bool(m.mul.is_some())
                            .u32(m.mul_milli());
                    }
                }
            }
        }

        // C3: the three-resource block, mixed in only when present (as C1's
        // and C2's sections are), so content without it fingerprints exactly
        // as before C3.
        if let Some(t) = &self.three_resource {
            h.str("three_resource")
                .u32(t.starting_biomass)
                .u32(t.starting_aether);
            h.str("growth")
                .u32(t.growth.interval_ticks)
                .u64(t.growth.sources.len() as u64);
            for g in &t.growth.sources {
                h.str(&g.building).u32(g.per_interval).u32(g.storage);
            }
            h.str("wells")
                .f32(t.wells.hold_radius)
                .u32(t.wells.interval_ticks)
                .u32(t.wells.per_interval)
                .u64(t.wells.sites.len() as u64);
            for site in &t.wells.sites {
                h.f32(site.along).f32(site.side);
            }
            h.str("ai").u32(t.ai.keepers).u32(t.ai.guards);
        }

        let ids = |all: &[String]| {
            if all.is_empty() {
                "-".to_string()
            } else {
                all.join(",")
            }
        };
        let summary = format!(
            "{} units [{}], {} buildings [{}], {} resources",
            self.units.len(),
            ids(&self.units.iter().map(|u| u.id.clone()).collect::<Vec<_>>()),
            self.buildings.len(),
            ids(&self
                .buildings
                .iter()
                .map(|b| b.id.clone())
                .collect::<Vec<_>>()),
            self.resources.len(),
        );
        ContentFingerprint {
            hash: h.finish(),
            summary,
        }
    }

    /// Index of a unit definition by id (stable: the RON order).
    pub fn unit_index(&self, id: &str) -> Option<usize> {
        self.units.iter().position(|u| u.id == id)
    }

    pub fn unit(&self, id: &str) -> Option<&UnitDef> {
        self.units.iter().find(|u| u.id == id)
    }

    /// The unit definition behind a [`crate::sim::economy::UnitDefIdx`]: the
    /// roster for `i < units.len()`, then the Generals (C1). What combat,
    /// movement and the HP pool read, so a General fights under the same
    /// 4-stat model as every unit. Anything that must stay roster-only
    /// (training, gathering, the AI's waves) keeps reading `units` directly.
    pub fn unit_def(&self, i: usize) -> Option<&UnitDef> {
        match self.units.get(i) {
            Some(u) => Some(u),
            None => self
                .generals
                .get(i.checked_sub(self.units.len())?)
                .and_then(|g| g.unit.as_ref()),
        }
    }

    /// The `UnitDefIdx` value of the General `id` (`units.len() + k`).
    pub fn general_index(&self, id: &str) -> Option<usize> {
        self.generals
            .iter()
            .position(|g| g.id == id)
            .map(|k| self.units.len() + k)
    }

    /// The domain (an index into [`Content::resources`]) of the unit behind a
    /// `UnitDefIdx` value: the resource whose `powers` list it. `None` for a
    /// unit no resource powers (the Worker, a General).
    pub fn domain_of(&self, i: usize) -> Option<usize> {
        let id = &self.units.get(i)?.id;
        self.resources.iter().position(|r| r.powers.iter().any(|p| p == id))
    }

    /// The General behind a `UnitDefIdx` value, if it names one.
    pub fn general_at(&self, i: usize) -> Option<&GeneralDef> {
        self.generals.get(i.checked_sub(self.units.len())?)
    }

    /// Fill each [`GeneralDef::unit`] from its RON fields. Called once at load.
    fn synthesise_general_units(&mut self) {
        let currency = self.economy.currency.clone();
        for g in &mut self.generals {
            g.unit = Some(UnitDef {
                id: g.id.clone(),
                name: g.name.clone(),
                source: None,
                barracks: None,
                cost: Cost {
                    resource: currency.clone(),
                    amount: 0,
                },
                mvp_kind: g.mvp_kind,
                mvp_alloy_cost: 0,
                mvp_train_ticks: 0,
                mvp_carry_capacity: 0,
                mvp_gather_ticks: 0,
                mvp_attack_ticks: g.mvp_attack_ticks,
                mvp_attack_range: g.mvp_attack_range,
                speed: g.speed,
                offense: g.offense,
                defense: g.defense,
                armor: g.armor,
                nemesis: None,
                gathers: false,
                builds: false,
            });
        }
    }

    /// Index of a building definition by id (stable: the RON order).
    pub fn building_index(&self, id: &str) -> Option<usize> {
        self.buildings.iter().position(|b| b.id == id)
    }

    pub fn building(&self, id: &str) -> Option<&BuildingDef> {
        self.buildings.iter().find(|b| b.id == id)
    }

    /// Index of a strategy by id (stable: the RON order).
    pub fn strategy_index(&self, id: &str) -> Option<usize> {
        self.strategies.iter().position(|s| s.id == id)
    }

    pub fn strategy(&self, id: &str) -> Option<&StrategyDef> {
        self.strategies.iter().find(|s| s.id == id)
    }

    /// Can `building` train `unit`? Production is data-driven: a building may
    /// only make what its RON `produces` list names.
    pub fn produces(&self, building: usize, unit: usize) -> bool {
        match (self.buildings.get(building), self.units.get(unit)) {
            (Some(b), Some(u)) => b.produces.iter().any(|p| p == &u.id),
            _ => false,
        }
    }
}

// L1 unit tests: the lookups are stable and id-keyed (no HashMap ordering).
/// The modifiers of an aura-shaped bonus — a General's aura (C1) or a nation's
/// Synthesis trait (C2) — are well formed: at least one, each stat at most
/// once, `add` in `1..=max_stat`, and `mul` only on the attack interval, in
/// (0, 1). `who` names the owner in the error.
pub(crate) fn check_aura_modifiers(
    who: &str,
    modifiers: &[AuraModifier],
    combat: &CombatDef,
) -> Result<(), String> {
    if modifiers.is_empty() {
        return Err(format!("{who} has no modifiers"));
    }
    for (i, m) in modifiers.iter().enumerate() {
        if modifiers[..i].iter().any(|n| n.stat == m.stat) {
            return Err(format!("{who} lists {:?} twice", m.stat));
        }
        match (m.stat, m.add, m.mul) {
            // The attack interval is the one multiplied stat: faster
            // attacks, never slower, and never a zero interval.
            (AuraStat::AttackTicks, None, Some(mul)) => {
                let milli = m.mul_milli();
                if !(mul.is_finite() && mul > 0.0 && (1..AuraModifier::MULT_SCALE).contains(&milli))
                {
                    return Err(format!("{who} AttackTicks mul must be in (0, 1)"));
                }
            }
            (AuraStat::AttackTicks, ..) => {
                return Err(format!("{who} AttackTicks takes `mul` only"));
            }
            (stat, Some(add), None) => {
                if add == 0 || add > combat.max_stat {
                    return Err(format!(
                        "{who} {stat:?} add must be 1..={}",
                        combat.max_stat
                    ));
                }
            }
            (stat, ..) => {
                return Err(format!("{who} {stat:?} takes `add` only"));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn content() -> Content {
        Content::load_from_dir(
            &std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data"),
        )
        .expect("content loads")
    }

    #[test]
    fn lookups_resolve_by_id_in_ron_order() {
        let c = content();
        assert_eq!(c.unit_index("worker"), Some(0));
        // F-044: the lookup must land on the Arclight's own entry; its offense
        // is read off the RON text (not pinned), so a re-tune moves both sides.
        let text = std::fs::read_to_string(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data/units.ron"),
        )
        .unwrap();
        let entry = &text[text.find("id: \"arclight\"").unwrap()..];
        let at = entry.find("offense: ").unwrap() + "offense: ".len();
        let written: u32 = entry[at..].split(',').next().unwrap().trim().parse().unwrap();
        assert_eq!(c.unit("arclight").unwrap().offense, written);
        assert_eq!(c.building_index("hq"), Some(0));
        assert_eq!(c.unit_index("nonesuch"), None);
        assert_eq!(c.building_index("nonesuch"), None);
    }

    #[test]
    fn strategy_lookups_resolve_by_id_in_ron_order() {
        let c = content();
        // The set is a list, scanned in RON order — never a map.
        assert_eq!(c.strategy_index(&c.default_strategy), Some(0));
        assert_eq!(c.strategy_index("nonesuch"), None);
        assert!(c.strategy("nonesuch").is_none());
        for (i, s) in c.strategies.iter().enumerate() {
            assert_eq!(c.strategy_index(&s.id), Some(i), "`{}` moved", s.id);
        }
        // The default is resolved at load, not re-looked-up per decision.
        assert_eq!(c.ai.id, c.default_strategy);
    }

    #[test]
    fn every_strategy_can_train_its_own_army() {
        // The invariant `validate` enforces, stated as a property of the
        // shipped set: every army entry has a barracks *this* strategy opens.
        let c = content();
        for s in &c.strategies {
            let opened: Vec<usize> = s
                .barracks
                .iter()
                .map(|o| c.building_index(&o.building).expect("a building"))
                .collect();
            for item in &s.army {
                let unit = c.unit_index(&item.unit).expect("a unit");
                assert!(
                    opened.iter().any(|&b| c.produces(b, unit)),
                    "strategy `{}` cannot train `{}`",
                    s.id,
                    item.unit
                );
            }
        }
    }

    /// B3.5: the cap on units-in-production is content, and it ships neutral.
    /// Depth 1 is the constant the army step used to hold (`queued == 0`), so
    /// the whole shipped set must be at 1 — anything else is a balance change
    /// riding along with the capability.
    #[test]
    fn every_shipped_strategy_ships_queue_depth_one() {
        let c = content();
        for s in &c.strategies {
            assert_eq!(
                s.queue_depth, 1,
                "strategy `{}` ships at depth {} — B3.5 AC0 lands neutral",
                s.id, s.queue_depth
            );
        }
        assert_eq!(c.ai.queue_depth, 1, "the resolved default is not at depth 1");
    }

    /// Depth 0 means "never put a unit in production" — a script with an army
    /// it can never build. Refused at load, by name.
    #[test]
    fn a_queue_depth_of_zero_is_refused_and_names_the_strategy() {
        let mut c = content();
        let who = c.strategies[1].id.clone();
        c.strategies[1].queue_depth = 0;
        let err = c.validate().expect_err("depth 0 is content the sim cannot run");
        let msg = format!("{err:?}");
        assert!(msg.contains(&who), "the error does not name the strategy: {msg}");
        assert!(msg.contains("queue_depth"), "the error does not name the field: {msg}");
    }

    /// ...and a *missing* depth is a load error, not a silent default: the
    /// field carries no `#[serde(default)]`, exactly like `mvp_attack_ticks`.
    #[test]
    fn a_missing_queue_depth_is_a_load_error_not_a_default() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data");
        let out = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/content_missing_queue_depth");
        std::fs::create_dir_all(&out).expect("scratch dir");
        for file in ["units.ron", "resources.ron"] {
            std::fs::copy(dir.join(file), out.join(file)).expect("copy");
        }
        let text = std::fs::read_to_string(dir.join("strategies.ron")).expect("strategies");
        let stripped = text.replacen("            queue_depth: 1,\n", "", 1);
        assert_ne!(stripped, text, "the shipped set has no depth to strip");
        std::fs::write(out.join("strategies.ron"), stripped).expect("write");
        let err = Content::load_from_dir(&out).expect_err("a strategy with no depth must not load");
        assert!(
            format!("{err:?}").contains("queue_depth"),
            "the parse error does not name the missing field: {err:?}"
        );
    }

    #[test]
    fn production_table_is_data_driven() {
        let c = content();
        let hq = c.building_index("hq").unwrap();
        let foundry = c.building_index("foundry").unwrap();
        let worker = c.unit_index("worker").unwrap();
        let bulwark = c.unit_index("bulwark").unwrap();
        assert!(c.produces(hq, worker));
        assert!(!c.produces(hq, bulwark));
        assert!(c.produces(foundry, bulwark));
        assert!(!c.produces(foundry, worker));
        assert!(!c.produces(999, worker));
    }
}
