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
    /// The tech openings, in the order it wants them.
    pub barracks: Vec<BarracksOpening>,
    /// The repeating army build order.
    pub army: Vec<ArmyItem>,
    /// Combat units it wants before it attacks.
    pub attack_at_army: u32,
    /// Ticks between two attack waves.
    pub attack_interval_ticks: u32,
    /// Radius of the seeded scatter around the enemy HQ each wave aims at.
    pub attack_spread: f32,
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

// ---- file shapes -----------------------------------------------------------

#[derive(Deserialize)]
struct UnitsFile {
    workers: Vec<UnitDef>,
    combat: Vec<UnitDef>,
    mvp_buildings: Vec<BuildingDef>,
    mvp_combat: CombatDef,
    nemesis_bonus: NemesisBonus,
}

#[derive(Deserialize)]
struct StrategiesFile {
    /// Id of the strategy a match uses when nothing names one.
    default: String,
    strategies: Vec<StrategyDef>,
}

#[derive(Deserialize)]
struct ResourcesFile {
    resources: Vec<ResourceDef>,
    mvp_active: Vec<String>,
    mvp_economy: EconomyDef,
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
            },
            resources: resources_file.resources,
            mvp_active: resources_file.mvp_active,
            economy: resources_file.mvp_economy,
        };
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
            if !(s.attack_spread.is_finite() && s.attack_spread >= 0.0) {
                return bad(format!(
                    "strategy `{who}` attack_spread must be finite and non-negative"
                ));
            }

            // The tech openings. A strategy with none could never train the
            // army it declares (every unit must come from one of them), and a
            // building opened twice is a second placement the commander would
            // never make — content stating something it cannot mean.
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
                if opened.contains(&def) {
                    return bad(format!(
                        "strategy `{who}` opens `{}` twice",
                        opening.building
                    ));
                }
                opened.push(def);
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
            .f32(self.combat.pursue_range);
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
        assert_eq!(c.unit("arclight").unwrap().offense, 9);
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
