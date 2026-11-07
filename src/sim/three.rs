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
//! ## Aether (channelled)
//! Ley-wells stand where the RON's sites put them, relative to the two HQs.
//! A side **holds** a well while at least one of its units or buildings is
//! within `hold_radius` of it and no enemy unit or building is. A held well
//! banks `per_interval` Aether to its holder every `interval_ticks` ticks of
//! holding. Progress pauses while the well is empty or contested, and
//! restarts from zero when the holder changes.
//!
//! ## Determinism
//! Fixed-size per-side arrays, sums (which no order can change), integer
//! arithmetic in the per-tick path, and the tick counted by the sim itself.

use bevy::ecs::prelude::*;
use bevy::math::Vec2;
use serde::{Deserialize, Serialize};

use crate::sim::content::{Acquisition, Content};
use crate::sim::economy::{Building, Stockpiles};
use crate::sim::nation::{Defs, NationRules};
use crate::sim::spatial::Faction;
use crate::sim::{Position, UnitDefIdx};

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
    pub wells: WellsDef,
    /// What the scripted AI details to the wells (AC5).
    pub ai: ThreeAiDef,
}

/// The AI's well detachments in a three-resource match.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThreeAiDef {
    /// Gatherers kept standing on the side's home well (its lowest-entity
    /// gatherers), out of mining.
    pub keepers: u32,
    /// Combat units detailed to contest the middle well (its lowest-entity
    /// fighters), out of the attack waves; called back to the home well when
    /// an enemy reaches it.
    pub guards: u32,
    /// A keeper or guard within this fraction of `wells.hold_radius` of its
    /// well is on it and gets no new order. In (0, 1].
    pub on_well_fraction: f32,
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

/// How Aether is channelled: the ley-wells and what holding one yields.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WellsDef {
    /// World units: a side holds a well while at least one of its units or
    /// buildings is this close and no enemy one is.
    pub hold_radius: f32,
    /// Ticks of holding per landing.
    pub interval_ticks: u32,
    /// Aether a held well banks to its holder per interval.
    pub per_interval: u32,
    /// Where the wells stand, in the frame of the two HQs.
    pub sites: Vec<WellSite>,
}

/// One ley-well's place, relative to the two HQs (so every map, and both
/// orientations of one, get the same layout): `along` the line from side A's
/// HQ to side B's (0 at A, 1 at B), then `side` world units across it, to the
/// left of A→B. A layout meant to be fair is **point-symmetric**: for every
/// site `(along, side)` there is one at `(1 - along, -side)`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WellSite {
    pub along: f32,
    pub side: f32,
}

impl WellSite {
    /// Where this site stands between HQs `a` (side A's) and `b`.
    pub fn at(&self, a: Vec2, b: Vec2) -> Vec2 {
        let d = (b - a).normalize_or_zero();
        a + (b - a) * self.along + Vec2::new(-d.y, d.x) * self.side
    }
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
    let w = &t.wells;
    if !(w.hold_radius.is_finite() && w.hold_radius > 0.0) {
        return Err(format!("{who}: wells hold_radius must be finite and positive"));
    }
    if w.interval_ticks == 0 || w.per_interval == 0 {
        return Err(format!("{who}: wells interval_ticks and per_interval must be > 0"));
    }
    let f = t.ai.on_well_fraction;
    if !(f.is_finite() && f > 0.0 && f <= 1.0) {
        return Err(format!("{who}: ai on_well_fraction must be in (0, 1]"));
    }
    if w.sites.is_empty() {
        return Err(format!("{who}: wells needs at least one site"));
    }
    for (k, site) in w.sites.iter().enumerate() {
        if !(site.along.is_finite() && (0.0..=1.0).contains(&site.along) && site.side.is_finite())
        {
            return Err(format!(
                "{who}: well site {k} needs along in [0, 1] and a finite side"
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
    /// Per side, per-mille Alloy-yield bonus (an `AlloyYield` trait) banked
    /// but not yet whole.
    yield_milli: [u32; 2],
}

impl ThreeEconomy {
    /// The state, as words for the state hash.
    pub fn hash_words(&self) -> [u64; 5] {
        [
            self.tick as u64,
            self.growth_milli[0] as u64,
            self.growth_milli[1] as u64,
            self.yield_milli[0] as u64,
            self.yield_milli[1] as u64,
        ]
    }

    /// `faction` banked a load of `amount` Alloy from a gatherer whose yield
    /// is `milli` per-mille: the whole extra Alloy that bonus is now worth
    /// (the fraction stays accumulated for the next load).
    pub fn yield_bonus(&mut self, faction: Faction, amount: u32, milli: u32) -> u32 {
        let extra = milli.saturating_sub(crate::sim::nation::TraitDef::MULT_SCALE);
        if extra == 0 || amount == 0 {
            return 0;
        }
        let k = slot(faction);
        let total = self.yield_milli[k] as u64 + amount as u64 * extra as u64;
        self.yield_milli[k] = (total % 1000) as u32;
        (total / 1000) as u32
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

// ---- Aether ------------------------------------------------------------------------

/// Who holds a well this tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WellState {
    /// Nobody is within the radius.
    Empty,
    /// Only this side is.
    Held(Faction),
    /// Both sides are.
    Contested,
}

/// A ley-well (C3): sim state, positioned by [`Position`], spawned only in a
/// three-resource match ([`spawn_wells`]).
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeyWell {
    /// Index into the RON's `wells.sites`.
    site: usize,
    /// The side whose holding `progress` counts (kept while the well is empty
    /// or contested, so a holder that comes back picks up where it was).
    holder: Option<Faction>,
    /// Ticks of holding towards the next landing.
    progress: u32,
    /// This tick's state.
    state: WellState,
}

impl LeyWell {
    pub fn site(&self) -> usize {
        self.site
    }

    pub fn state(&self) -> WellState {
        self.state
    }

    pub fn holder(&self) -> Option<Faction> {
        self.holder
    }

    pub fn progress(&self) -> u32 {
        self.progress
    }

    /// The well, as two words for the state hash.
    pub fn hash_words(&self) -> [u64; 2] {
        let holder = match self.holder {
            None => 0,
            Some(Faction::A) => 1,
            Some(Faction::B) => 2,
        };
        let state = match self.state {
            WellState::Empty => 0,
            WellState::Held(Faction::A) => 1,
            WellState::Held(Faction::B) => 2,
            WellState::Contested => 3,
        };
        [
            self.site as u64,
            (holder << 40) | (state << 32) | self.progress as u64,
        ]
    }
}

/// Spawn the content's ley-wells between side A's HQ at `a` and side B's at
/// `b`, in site order. Call **last** in a match's setup, after everything else
/// is spawned (entity order is part of a match, F-011). Does nothing in an
/// Alloy-only world.
pub fn spawn_wells(world: &mut World, a: Vec2, b: Vec2) {
    if !is_three(world) {
        return;
    }
    let Some(t) = world.resource::<Content>().three_resource.clone() else {
        return;
    };
    for (site, s) in t.wells.sites.iter().enumerate() {
        world.spawn((
            Position(s.at(a, b)),
            LeyWell {
                site,
                holder: None,
                progress: 0,
                state: WellState::Empty,
            },
        ));
    }
}

/// The two HQs a world's wells are laid out between: each side's victory
/// building (the lowest entity of it, should a side have several). `None`
/// when a side has none.
pub fn hq_frame(world: &mut World) -> Option<(Vec2, Vec2)> {
    let content = world.resource::<Content>().clone();
    let mut q = world.query::<(Entity, &Position, &Building, &Faction)>();
    let mut best: [Option<(u64, Vec2)>; 2] = [None, None];
    for (e, p, b, f) in q.iter(world) {
        if !content.buildings.get(b.def).is_some_and(|d| d.victory) {
            continue;
        }
        let s = slot(*f);
        if best[s].is_none_or(|(bits, _)| e.to_bits() < bits) {
            best[s] = Some((e.to_bits(), p.0));
        }
    }
    Some((best[0]?.1, best[1]?.1))
}

/// What can hold a well: a unit or a building.
pub type Holds = Or<(With<UnitDefIdx>, With<Building>)>;

/// Channel Aether: decide who holds each well this tick, advance its holder's
/// progress, and bank `per_interval` to the holder on each full interval.
pub fn channel(
    content: Res<Content>,
    mut stock: ResMut<Stockpiles>,
    mut wells: Query<(Entity, &Position, &mut LeyWell)>,
    presence: Query<(&Position, &Faction), Holds>,
) {
    let Some(t) = &content.three_resource else {
        return;
    };
    let w = &t.wells;
    let r2 = w.hold_radius * w.hold_radius;
    // Who stands where: positions per side. Only "is anyone near" is asked of
    // it, which no order can change.
    let mut near: [Vec<Vec2>; 2] = [Vec::new(), Vec::new()];
    for (p, f) in &presence {
        near[slot(*f)].push(p.0);
    }
    // Stable order: ascending entity, so the banking order never depends on
    // the query's.
    let mut order: Vec<Entity> = wells.iter().map(|(e, ..)| e).collect();
    order.sort_unstable_by_key(|e| e.to_bits());
    for e in order {
        let Ok((_, pos, mut well)) = wells.get_mut(e) else {
            continue;
        };
        let here = SIDES.map(|f| near[slot(f)].iter().any(|p| p.distance_squared(pos.0) <= r2));
        well.state = match here {
            [true, true] => WellState::Contested,
            [true, false] => WellState::Held(Faction::A),
            [false, true] => WellState::Held(Faction::B),
            [false, false] => WellState::Empty,
        };
        let WellState::Held(f) = well.state else {
            continue;
        };
        if well.holder != Some(f) {
            well.holder = Some(f);
            well.progress = 0;
        }
        well.progress += 1;
        if well.progress >= w.interval_ticks {
            well.progress = 0;
            let _ = stock.add_in(f, Bank::Aether, w.per_interval);
        }
    }
}
