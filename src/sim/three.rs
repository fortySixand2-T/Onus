//! The three-resource economy (C3): Biomass **grown**, Aether **channelled**,
//! beside the Alloy that workers mine. **Render-free** — ECS + math +
//! [`Content`] only.
//!
//! ## Opt-in, and the golden licence
//! A match plays the three-resource economy only when its setup asks for it
//! ([`EconomyMode::ThreeResource`], through `MatchSettings::economy` or the
//! client's pick). Then [`install`] inserts the [`ThreeEconomy`] resource,
//! and every three-resource system runs only while that resource exists. The
//! default, [`EconomyMode::Alloy`], inserts nothing: no resource, no Biomass,
//! no Aether, no well, no extra state-hash row. An Alloy-only match is the
//! match as it was before C3.
//!
//! ## Biomass (grown)
//! Every `growth.interval_ticks` ticks each side gains the `per_interval` of
//! every growth source it has standing (the HQ's small floor, each
//! Gene-Vats), and stops at its **storage cap**: the sum of those sources'
//! `storage`. Growth over the cap is not banked; Biomass already over it (a
//! cap that shrank when a Vats fell) is kept. A side's growth rate can carry a
//! nation's per-mille factor, so growth is accumulated in per-mille and only
//! whole Biomass is banked.
//!
//! ## Determinism
//! Fixed-size per-side arrays, sums (which no order can change), integer
//! arithmetic in the per-tick path, and the tick counted by the sim itself.

use bevy::ecs::prelude::*;
use serde::{Deserialize, Serialize};

use crate::sim::content::{Acquisition, Content};
use crate::sim::economy::{Building, Stockpiles};
use crate::sim::nation::{Defs, NationRules};
use crate::sim::spatial::Faction;

// ---- the mode ----------------------------------------------------------------

/// Which economy a match plays. `Alloy` — the default — is the economy every
/// milestone before C3 shipped, exactly as it was.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum EconomyMode {
    #[default]
    Alloy,
    ThreeResource,
}

impl EconomyMode {
    pub fn is_alloy(self) -> bool {
        self == EconomyMode::Alloy
    }

    pub fn label(self) -> &'static str {
        match self {
            EconomyMode::Alloy => "Alloy only",
            EconomyMode::ThreeResource => "Three resources",
        }
    }
}

/// One of a side's three banks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bank {
    Alloy,
    Biomass,
    Aether,
}

impl Bank {
    /// The bank a resource is held in, by how it is acquired.
    pub fn of(acquisition: Acquisition) -> Bank {
        match acquisition {
            Acquisition::Mined => Bank::Alloy,
            Acquisition::Grown => Bank::Biomass,
            Acquisition::Channeled => Bank::Aether,
        }
    }

    /// The bank holding the resource with id `resource`, if it is declared.
    pub fn of_resource(content: &Content, resource: &str) -> Option<Bank> {
        content
            .resources
            .iter()
            .find(|r| r.id == resource)
            .map(|r| Bank::of(r.acquisition))
    }
}

// ---- content -------------------------------------------------------------------

/// The `three_resource` block of `resources.ron`. Optional: content without it
/// can only play Alloy-only matches.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThreeResourceDef {
    /// Biomass each side starts a three-resource match with.
    pub starting_biomass: u32,
    /// Aether each side starts a three-resource match with.
    pub starting_aether: u32,
    pub growth: GrowthDef,
}

/// How Biomass grows.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrowthDef {
    /// Ticks between two landings of growth.
    pub interval_ticks: u32,
    /// The buildings that grow Biomass, each at most once in the list.
    pub sources: Vec<GrowthSource>,
}

/// One building that grows Biomass.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrowthSource {
    /// A building id.
    pub building: String,
    /// Biomass each standing building grows per interval.
    pub per_interval: u32,
    /// Storage each standing building adds to its side's cap.
    pub storage: u32,
}

impl ThreeResourceDef {
    /// `(per_interval, storage)` per building definition, in RON order.
    fn sources_by_building(&self, content: &Content) -> Vec<Option<(u32, u32)>> {
        let mut out = vec![None; content.buildings.len()];
        for s in &self.growth.sources {
            if let Some(i) = content.building_index(&s.building) {
                out[i] = Some((s.per_interval, s.storage));
            }
        }
        out
    }
}

/// Load-time check of the block (called by [`Content`]'s validation).
pub(crate) fn validate(content: &Content) -> Result<(), String> {
    let Some(t) = &content.three_resource else {
        return Ok(());
    };
    let who = "three_resource";
    // One resource per way of acquiring it: the banks are keyed by it.
    for acq in [Acquisition::Mined, Acquisition::Grown, Acquisition::Channeled] {
        let n = content.resources.iter().filter(|r| r.acquisition == acq).count();
        if n != 1 {
            return Err(format!(
                "{who}: needs exactly one {acq:?} resource, the content declares {n}"
            ));
        }
    }
    // Every unit's price names a declared resource.
    for u in &content.units {
        if Bank::of_resource(content, &u.cost.resource).is_none() {
            return Err(format!(
                "{who}: unit `{}` costs `{}`, which is not a declared resource",
                u.id, u.cost.resource
            ));
        }
    }
    if t.growth.interval_ticks == 0 {
        return Err(format!("{who}: growth interval_ticks must be > 0"));
    }
    if t.growth.sources.is_empty() {
        return Err(format!("{who}: growth needs at least one source"));
    }
    for (i, s) in t.growth.sources.iter().enumerate() {
        if content.building_index(&s.building).is_none() {
            return Err(format!(
                "{who}: growth source `{}` is not a building",
                s.building
            ));
        }
        if t.growth.sources[..i].iter().any(|o| o.building == s.building) {
            return Err(format!(
                "{who}: growth source `{}` is listed twice",
                s.building
            ));
        }
        if s.per_interval == 0 || s.storage == 0 {
            return Err(format!(
                "{who}: growth source `{}` needs per_interval and storage > 0",
                s.building
            ));
        }
    }
    Ok(())
}

// ---- the match's state --------------------------------------------------------

/// The three-resource economy's own state. Its presence **is** the mode: it is
/// inserted only by [`install`], and every three-resource system runs only
/// while it exists.
#[derive(Resource, Debug, Clone, Default, PartialEq, Eq)]
pub struct ThreeEconomy {
    /// Ticks played since [`install`].
    tick: u32,
    /// Per side, per-mille Biomass grown but not yet whole.
    growth_milli: [u32; 2],
}

impl ThreeEconomy {
    /// The state, as words for the state hash.
    pub fn hash_words(&self) -> [u64; 3] {
        [
            self.tick as u64,
            self.growth_milli[0] as u64,
            self.growth_milli[1] as u64,
        ]
    }
}

fn slot(f: Faction) -> usize {
    match f {
        Faction::A => 0,
        Faction::B => 1,
    }
}

const SIDES: [Faction; 2] = [Faction::A, Faction::B];

/// Is this world playing the three-resource economy?
pub fn is_three(world: &World) -> bool {
    world.contains_resource::<ThreeEconomy>()
}

/// The mode a world plays.
pub fn mode_of(world: &World) -> EconomyMode {
    if is_three(world) {
        EconomyMode::ThreeResource
    } else {
        EconomyMode::Alloy
    }
}

/// Why a three-resource match cannot be set up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EconomyRefused(pub String);

impl std::fmt::Display for EconomyRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "economy refused: {}", self.0)
    }
}

impl std::error::Error for EconomyRefused {}

/// Install `mode` into a world whose start has been spawned, **before** its
/// nations are installed (a nation's traits depend on the mode). Alloy-only
/// installs nothing. Three-resource inserts [`ThreeEconomy`] and banks each
/// side's starting Biomass and Aether; refused when the content has no
/// `three_resource` block.
pub fn install(world: &mut World, mode: EconomyMode) -> Result<(), EconomyRefused> {
    if mode.is_alloy() {
        return Ok(());
    }
    let Some(t) = world.resource::<Content>().three_resource.clone() else {
        return Err(EconomyRefused(
            "this content has no `three_resource` block".to_string(),
        ));
    };
    if is_three(world) {
        return Ok(());
    }
    let mut stock = world.resource_mut::<Stockpiles>();
    for f in SIDES {
        let _ = stock.add_in(f, Bank::Biomass, t.starting_biomass);
        let _ = stock.add_in(f, Bank::Aether, t.starting_aether);
    }
    world.insert_resource(ThreeEconomy::default());
    Ok(())
}

// ---- Biomass ---------------------------------------------------------------------

/// Grow each side's Biomass: once per `growth.interval_ticks`, the sum of its
/// standing sources' rates (times its nation's growth factor), up to the sum
/// of their storage.
pub fn grow(
    content: Res<Content>,
    nations: Option<Res<NationRules>>,
    mut three: ResMut<ThreeEconomy>,
    mut stock: ResMut<Stockpiles>,
    buildings: Query<(&Building, &Faction)>,
) {
    let Some(t) = &content.three_resource else {
        return;
    };
    three.tick = three.tick.wrapping_add(1);
    if !three.tick.is_multiple_of(t.growth.interval_ticks) {
        return;
    }
    let sources = t.sources_by_building(&content);
    let defs = Defs::new(&content, nations.as_deref());
    // Sums only: no order of the query can change them.
    let mut rate_milli = [0u64; 2];
    let mut cap = [0u64; 2];
    for (b, f) in &buildings {
        let Some(Some((per, storage))) = sources.get(b.def) else {
            continue;
        };
        rate_milli[slot(*f)] += *per as u64 * defs.growth_milli(*f, b.def) as u64;
        cap[slot(*f)] += *storage as u64;
    }
    for f in SIDES {
        let s = slot(f);
        let acc = three.growth_milli[s] as u64 + rate_milli[s];
        let whole = acc / 1000;
        three.growth_milli[s] = (acc % 1000) as u32;
        let have = stock.amount(f, Bank::Biomass) as u64;
        let room = cap[s].saturating_sub(have);
        let grown = whole.min(room).min(u32::MAX as u64) as u32;
        if grown > 0 {
            let _ = stock.add_in(f, Bank::Biomass, grown);
        }
    }
}
