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
    /// (`f32::round`) and happens on a constant from the RON, so every machine
    /// gets the same integer and per-hit damage is bit-identical everywhere.
    pub fn mult_milli(&self) -> u32 {
        (self.damage_mult * Self::MULT_SCALE as f32).round() as u32
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
    pub resources: Vec<ResourceDef>,
    pub mvp_active: Vec<String>,
    pub economy: EconomyDef,
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

    /// Load `units.ron` + `resources.ron` from a data directory. Pure `std` file
    /// IO — works headless, with no engine asset pipeline.
    pub fn load_from_dir(dir: &Path) -> Result<Self, ContentError> {
        let units_file: UnitsFile = load_ron(&dir.join("units.ron"))?;
        let resources_file: ResourcesFile = load_ron(&dir.join("resources.ron"))?;

        let mut units = units_file.workers;
        units.extend(units_file.combat);

        let content = Content {
            units,
            buildings: units_file.mvp_buildings,
            combat: units_file.mvp_combat,
            nemesis_bonus: units_file.nemesis_bonus,
            resources: resources_file.resources,
            mvp_active: resources_file.mvp_active,
            economy: resources_file.mvp_economy,
        };
        content.validate()?;
        Ok(content)
    }

    /// Reject content the sim cannot run. Anything the sim *reads* must be
    /// stated in the data: a serde default that silently becomes 0 would show up
    /// as a worker mining nothing forever, or a unit that costs nothing.
    fn validate(&self) -> Result<(), ContentError> {
        let bad = |msg: String| Err(ContentError::Invalid { msg });

        for b in &self.buildings {
            if b.alloy_cost == 0 {
                return bad(format!("building `{}` has no Alloy cost", b.id));
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
                if u.mvp_attack_range <= 0.0 {
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
        if c.hp_per_defense == 0 || c.damage_per_offense == 0 || c.mitigation_per_armor == 0 {
            return bad("mvp_combat scaling factors must be positive".to_string());
        }
        if !(c.speed_per_point > 0.0 && c.engage_range > 0.0) {
            return bad("mvp_combat speed_per_point / engage_range must be positive".to_string());
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

        // Representability: the *largest* HP pool and the *largest* nemesis hit
        // the bounded stats can produce must both fit in the u32 the sim counts
        // them in. Data that can only be evaluated by saturating is data the
        // loader should refuse, not data the formula should paper over.
        let max_stat = c.max_stat as u64;
        let peak_hp = max_stat * c.hp_per_defense as u64;
        let peak_damage = max_stat * c.damage_per_offense as u64 * self.nemesis_bonus.mult_milli()
            as u64
            / NemesisBonus::MULT_SCALE as u64;
        if peak_hp > u32::MAX as u64 || peak_damage > u32::MAX as u64 {
            return bad(format!(
                "mvp_combat scaling overflows the sim's u32 arithmetic \
                 (peak HP {peak_hp}, peak damage {peak_damage})"
            ));
        }

        if self.resources.iter().all(|r| r.id != self.economy.currency) {
            return bad(format!(
                "mvp_economy currency `{}` is not a declared resource",
                self.economy.currency
            ));
        }
        if !(self.economy.gather_range > 0.0 && self.economy.deposit_range > 0.0) {
            return bad("mvp_economy ranges must be positive".to_string());
        }
        Ok(())
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
