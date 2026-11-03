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
//! ## Retreat
//! `Order::Retreat` turns a side around: [`withdraw`] walks every one of its
//! units straight out to its entry edge (behind its base), records each one
//! that crosses it as **escaped** (with its HP), and stops the side's
//! production. The battle ends as a retreat once the side has nothing left on
//! the field; the result lists everyone who got away.
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

use crate::sim::combat::{Engaging, Health, StatBonus};
use crate::sim::content::{AuraModifier, AuraScaling, AuraStat, Content};
use crate::sim::economy::{self, Building, ProductionQueue, UnitDefIdx};
use crate::sim::replay::SimId;
use crate::sim::spatial::Faction;
use crate::sim::victory::count_victory_buildings;
use crate::sim::{AttackMoveDest, MatchState, MoveTarget, Position};

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

/// One unit in a [`Roster`]: a roster unit id and its HP (`None`: full).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RosterUnit {
    pub id: String,
    pub hp: Option<u32>,
}

/// One building in a [`Roster`]: a building id and its HP (`None`: full).
/// Siege damage persists between battles (C0), so a city's buildings arrive
/// with whatever HP the last siege left them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RosterBuilding {
    pub id: String,
    pub hp: Option<u32>,
}

/// What a side brings to a battle **instead of the skirmish start** (C1 AC5):
/// its army and its buildings, each with its HP. C4 builds these from
/// campaign state; the order of each list is the order things are placed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Roster {
    pub units: Vec<RosterUnit>,
    pub buildings: Vec<RosterBuilding>,
}

impl Roster {
    /// One more unit `id` at full HP.
    pub fn unit(mut self, id: &str) -> Self {
        self.units.push(RosterUnit { id: id.to_string(), hp: None });
        self
    }

    /// One more unit `id` with `hp` HP.
    pub fn unit_hp(mut self, id: &str, hp: u32) -> Self {
        self.units.push(RosterUnit { id: id.to_string(), hp: Some(hp) });
        self
    }

    /// One more building `id` at full HP.
    pub fn building(mut self, id: &str) -> Self {
        self.buildings.push(RosterBuilding { id: id.to_string(), hp: None });
        self
    }

    /// One more building `id` with `hp` HP.
    pub fn building_hp(mut self, id: &str, hp: u32) -> Self {
        self.buildings.push(RosterBuilding { id: id.to_string(), hp: Some(hp) });
        self
    }
}

/// One side's part of a battle setup.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SideSetup {
    /// The Generals this side fields (ids from `units.ron` `generals`), in
    /// field order. Several armies converging on one battle bring several
    /// Generals; the same type may appear more than once (a house raises
    /// extra Generals of its own kind).
    pub generals: Vec<String>,
    /// The side's army and buildings. `None`: the skirmish start (an HQ and
    /// its workers); `Some`: exactly this, and no skirmish start at all.
    pub roster: Option<Roster>,
}

/// How a campaign battle starts. Indexed by faction slot (A, B).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BattleSetup {
    pub sides: [SideSetup; 2],
}

impl BattleSetup {
    /// `side`'s part of the setup.
    pub fn side(&self, side: Faction) -> &SideSetup {
        &self.sides[faction_slot(side)]
    }

    /// Start `side` from `roster` instead of the skirmish start.
    pub fn with_roster(mut self, side: Faction, roster: Roster) -> Self {
        self.sides[faction_slot(side)].roster = Some(roster);
        self
    }

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
            let Some(roster) = &s.roster else {
                continue;
            };
            let bad_hp = |id: &str, hp: u32, max: u32| BattleSetupError::BadHp {
                id: id.to_string(),
                side: *side,
                hp,
                max,
            };
            for u in &roster.units {
                let Some(idx) = content.unit_index(&u.id) else {
                    return Err(BattleSetupError::UnknownUnit {
                        id: u.id.clone(),
                        side: *side,
                    });
                };
                let max = Health::from_def(content, idx).max;
                if let Some(hp) = u.hp.filter(|hp| *hp == 0 || *hp > max) {
                    return Err(bad_hp(&u.id, hp, max));
                }
            }
            for b in &roster.buildings {
                let Some(idx) = content.building_index(&b.id) else {
                    return Err(BattleSetupError::UnknownBuilding {
                        id: b.id.clone(),
                        side: *side,
                    });
                };
                let max = Health::from_building_def(content, idx).max;
                if let Some(hp) = b.hp.filter(|hp| *hp == 0 || *hp > max) {
                    return Err(bad_hp(&b.id, hp, max));
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
    /// A roster unit id the content does not define (Generals go in
    /// `generals`, never in a roster).
    UnknownUnit { id: String, side: Faction },
    /// A building id the content does not define.
    UnknownBuilding { id: String, side: Faction },
    /// An HP of 0 (a dead thing does not take the field) or above the pool.
    BadHp {
        id: String,
        side: Faction,
        hp: u32,
        max: u32,
    },
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
            BattleSetupError::UnknownUnit { id, side } => {
                write!(f, "no roster unit `{id}` (asked for by {side:?})")
            }
            BattleSetupError::UnknownBuilding { id, side } => {
                write!(f, "no building `{id}` (asked for by {side:?})")
            }
            BattleSetupError::BadHp { id, side, hp, max } => {
                write!(f, "`{id}` for {side:?} with HP {hp}: must be 1..={max}")
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

/// Place a side's [`Roster`]: buildings in a line across the base (the first
/// on it, the rest alternating either side), `building_spacing` apart; units in ranks of `formation_rank` just
/// behind the Generals' line, `formation_spacing` apart, each further rank one
/// spacing nearer the base. Buildings first, then units, in list order.
fn spawn_roster(
    world: &mut World,
    content: &Content,
    battle: &crate::sim::content::BattleDef,
    side: Faction,
    g: SideGround,
    roster: &Roster,
) {
    let across = Vec2::new(-g.front.y, g.front.x);
    for (k, b) in roster.buildings.iter().enumerate() {
        let def = content
            .building_index(&b.id)
            .expect("BattleSetup::check admits only known buildings");
        // The first stands on the base; the rest alternate either side of it
        // (k = 1, 2, 3, 4 → +1, -1, +2, -2 spacings).
        let step = k.div_ceil(2) as f32;
        let slot = if k % 2 == 1 { step } else { -step };
        let full = Health::from_building_def(content, def);
        let mut e = world.spawn((
            Position(g.base + across * (slot * battle.building_spacing)),
            Building { def },
            side,
            ProductionQueue::default(),
        ));
        if let Some(hp) = b.hp {
            e.insert(Health { current: hp, max: full.max });
        }
    }
    let rank = battle.formation_rank as usize;
    let total = roster.units.len();
    for (k, u) in roster.units.iter().enumerate() {
        let idx = content
            .unit_index(&u.id)
            .expect("BattleSetup::check admits only known units");
        let row = k / rank;
        let col = k % rank;
        let in_row = (total - row * rank).min(rank);
        let back = battle.general_front - battle.formation_spacing * (row as f32 + 1.0);
        let at = g.base + g.front * back + line_offset(g, col, in_row, battle.formation_spacing);
        let full = Health::from_def(content, idx);
        world.spawn((
            Position(at),
            UnitDefIdx(idx),
            content.units[idx].mvp_kind,
            side,
            Health {
                current: u.hp.unwrap_or(full.max),
                max: full.max,
            },
        ));
    }
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
    let mut fielded = Vec::new();
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
            let entity = world
                .spawn((
                    Position(at),
                    UnitDefIdx(idx),
                    def.mvp_kind,
                    *side,
                    Health::from_def(&content, idx),
                    General,
                ))
                .id();
            fielded.push(Fielded {
                entity,
                side: *side,
                unit: idx,
                escaped: false,
            });
        }
        if let Some(roster) = &setup.sides[slot].roster {
            spawn_roster(world, &content, battle, *side, g, roster);
        }
    }
    let edges = ground.map(|g| Edge {
        point: g.base - g.front * battle.entry_edge_offset,
        outward: -g.front,
    });
    let mut state = BattleState::new(edges);
    state.fielded = fielded;
    world.insert_resource(state);
    world.insert_resource(AuraField::default());
    world.insert_resource(crate::sim::ai::BattleAiJournal::default());
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
    /// It ordered a retreat, and every unit it had on the field has left it
    /// (escaped across its entry edge, or fallen on the way).
    Retreated,
}

/// A side's entry edge (C1 AC4): the line through `point` square to
/// `outward`, the direction away from the enemy. A retreating unit walks
/// straight out to it and has escaped once it stands on or beyond it
/// ([`Edge::reached`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Edge {
    pub point: Vec2,
    pub outward: Vec2,
}

impl Edge {
    /// How near the edge counts as on it, in world units. Numerical, not a
    /// gameplay number: a unit walked onto [`Edge::exit_for`] lands there
    /// only to float rounding (about 1e-6 on a front that is not
    /// axis-aligned, or from a base at a non-round coordinate), and without a
    /// tolerance it would stand a rounding error short forever. A hundredth
    /// of a world unit is far above that error and far below anything a
    /// player could see or a unit could cover in a tick.
    pub const ON_EDGE: f32 = 0.01;

    /// How far past the edge `p` stands (negative: still on the field).
    pub fn beyond(&self, p: Vec2) -> f32 {
        (p - self.point).dot(self.outward)
    }

    /// Has a unit at `p` got off the field: on the edge (within
    /// [`Edge::ON_EDGE`]) or beyond it?
    pub fn reached(&self, p: Vec2) -> bool {
        self.beyond(p) >= -Self::ON_EDGE
    }

    /// The nearest point of the edge to `p` — where a retreat from `p` heads.
    pub fn exit_for(&self, p: Vec2) -> Vec2 {
        p - self.outward * self.beyond(p)
    }
}

/// A unit that got off the field in a retreat (C1 AC4), as it left.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Escaped {
    pub side: Faction,
    /// The unit's sim id (`SimId::UNIDENTIFIED` if it never had one).
    pub sim_id: SimId,
    /// Its `UnitDefIdx` (a roster unit or a General).
    pub unit: usize,
    pub hp: u32,
    pub max_hp: u32,
    pub general: bool,
    /// The tick it crossed the edge.
    pub tick: u32,
}

/// How a battle ended: the winner (`None`: both sides lost on one tick), the
/// tick, why each losing side lost, and who got off the field in a retreat.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BattleResult {
    pub winner: Option<Faction>,
    pub tick: u32,
    /// Per faction slot: why that side lost, or `None` if it did not.
    pub lost: [Option<LossCause>; 2],
    /// Every unit that escaped in a retreat, in the order they crossed (tick,
    /// then ascending entity bits).
    pub escaped: Vec<Escaped>,
    /// Per faction slot: what that side came out of the battle with.
    pub sides: [SideReport; 2],
}

impl BattleResult {
    /// Why `side` lost, if it did.
    pub fn loss(&self, side: Faction) -> Option<LossCause> {
        self.lost[faction_slot(side)]
    }

    /// What `side` came out of the battle with (C1 AC6).
    pub fn report(&self, side: Faction) -> &SideReport {
        &self.sides[faction_slot(side)]
    }
}

/// A unit that came out of a battle alive (C1 AC6): still on the field when
/// it was decided, or escaped in a retreat (with the HP it left with).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnitReport {
    pub sim_id: SimId,
    /// Its `UnitDefIdx` (a roster unit or a General).
    pub unit: usize,
    pub hp: u32,
    pub max_hp: u32,
    pub general: bool,
    pub escaped: bool,
}

/// A building still standing when the battle was decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BuildingReport {
    pub sim_id: SimId,
    /// Its index in `Content::buildings`.
    pub building: usize,
    pub hp: u32,
    pub max_hp: u32,
}

/// What became of one General the setup fielded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GeneralFate {
    /// His `UnitDefIdx`.
    pub unit: usize,
    /// On the field at the end, or escaped.
    pub survived: bool,
    pub escaped: bool,
}

/// One side's account of a battle: every survivor (by sim id), every
/// standing building (by sim id), and each fielded General's fate (in the
/// setup's order).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SideReport {
    pub units: Vec<UnitReport>,
    pub buildings: Vec<BuildingReport>,
    pub generals: Vec<GeneralFate>,
}

/// A General the setup fielded, tracked to the end so a fallen one still has
/// a fate in the report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Fielded {
    entity: Entity,
    side: Faction,
    unit: usize,
    escaped: bool,
}

/// The battle's own sim state. Present only in a campaign battle — which is
/// what every battle system is gated on.
#[derive(Resource, Debug, Clone, PartialEq)]
pub struct BattleState {
    /// Per slot: has this side been seen with a General at the start of a tick?
    generals_seen: [bool; 2],
    /// Per slot: has this side been seen with a victory building?
    victory_seen: [bool; 2],
    /// Per slot: the side's entry edge, where a retreat leaves the field.
    edges: [Edge; 2],
    /// Per slot: has this side ordered a retreat?
    retreating: [bool; 2],
    /// Everyone who has escaped so far, in crossing order.
    escaped: Vec<Escaped>,
    /// Every General the setup fielded, in spawn order.
    fielded: Vec<Fielded>,
    result: Option<BattleResult>,
}

impl BattleState {
    /// A battle not yet fought, with the sides' entry edges.
    pub fn new(edges: [Edge; 2]) -> Self {
        BattleState {
            generals_seen: [false; 2],
            victory_seen: [false; 2],
            edges,
            retreating: [false; 2],
            escaped: Vec::new(),
            fielded: Vec::new(),
            result: None,
        }
    }

    pub fn edge(&self, side: Faction) -> Edge {
        self.edges[faction_slot(side)]
    }

    pub fn retreating(&self, side: Faction) -> bool {
        self.retreating[faction_slot(side)]
    }

    /// Everyone who has escaped so far.
    pub fn escaped(&self) -> &[Escaped] {
        &self.escaped
    }

    /// Order `side` off the field (from an `Order::Retreat`). Once ordered, a
    /// retreat is not called back. A decided battle ignores it.
    pub(crate) fn order_retreat(&mut self, side: Faction) {
        if self.result.is_none() {
            self.retreating[faction_slot(side)] = true;
        }
    }

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
            Some(LossCause::Retreated) => 3,
        };
        let escapes = |w: &mut Vec<u64>, all: &[Escaped]| {
            w.push(all.len() as u64);
            for e in all {
                w.push(faction_slot(e.side) as u64);
                w.push(e.sim_id.0);
                w.push(e.unit as u64);
                w.push(e.hp as u64 | (e.max_hp as u64) << 32);
                w.push(e.general as u64);
                w.push(e.tick as u64);
            }
        };
        let mut w = vec![
            self.generals_seen[0] as u64 | (self.generals_seen[1] as u64) << 1,
            self.victory_seen[0] as u64 | (self.victory_seen[1] as u64) << 1,
            self.retreating[0] as u64 | (self.retreating[1] as u64) << 1,
        ];
        for e in &self.edges {
            for v in [e.point.x, e.point.y, e.outward.x, e.outward.y] {
                w.push(v.to_bits() as u64);
            }
        }
        escapes(&mut w, &self.escaped);
        w.push(self.fielded.len() as u64);
        for g in &self.fielded {
            w.push(faction_slot(g.side) as u64 | (g.escaped as u64) << 1);
            w.push(g.unit as u64);
        }
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
                escapes(&mut w, &r.escaped);
                for side in &r.sides {
                    w.push(side.units.len() as u64);
                    for u in &side.units {
                        w.push(u.sim_id.0);
                        w.push(u.unit as u64);
                        w.push(u.hp as u64 | (u.max_hp as u64) << 32);
                        w.push(u.general as u64 | (u.escaped as u64) << 1);
                    }
                    w.push(side.buildings.len() as u64);
                    for b in &side.buildings {
                        w.push(b.sim_id.0);
                        w.push(b.building as u64);
                        w.push(b.hp as u64 | (b.max_hp as u64) << 32);
                    }
                    w.push(side.generals.len() as u64);
                    for g in &side.generals {
                        w.push(g.unit as u64);
                        w.push(g.survived as u64 | (g.escaped as u64) << 1);
                    }
                }
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
#[allow(clippy::type_complexity)]
pub fn battle_end(
    content: Res<Content>,
    mut state: ResMut<MatchState>,
    mut battle: ResMut<BattleState>,
    generals: Query<&Faction, With<General>>,
    units: Query<(
        Entity,
        &Faction,
        &UnitDefIdx,
        Option<&Health>,
        Option<&SimId>,
        Has<General>,
    )>,
    buildings: Query<(&Building, &Faction)>,
    standing: Query<(&Building, &Faction, Option<&Health>, Option<&SimId>)>,
) {
    if state.is_over() {
        return;
    }
    let g = count_generals(&generals);
    let v = count_victory_buildings(&content, &buildings);
    let mut on_field = [0u32; 2];
    for (_, f, ..) in &units {
        on_field[faction_slot(*f)] += 1;
    }
    // A General who escaped in a retreat did not fall.
    let mut got_away = [false; 2];
    for e in battle.escaped.iter().filter(|e| e.general) {
        got_away[faction_slot(e.side)] = true;
    }
    let mut lost: [Option<LossCause>; 2] = [None, None];
    for s in 0..2 {
        lost[s] = if battle.generals_seen[s] && g[s] == 0 && !got_away[s] {
            Some(LossCause::GeneralsFell)
        } else if battle.victory_seen[s] && v[s] == 0 {
            Some(LossCause::VictoryBuildingLost)
        } else if battle.retreating[s] && on_field[s] == 0 {
            Some(LossCause::Retreated)
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
    let sides = report(&content, &battle, &units, &standing);
    let escaped = battle.escaped.clone();
    battle.result = Some(BattleResult {
        winner,
        tick,
        lost,
        escaped,
        sides,
    });
}

/// Each side's account of the battle (C1 AC6), on the state it ended in.
/// Survivors and buildings are sorted by sim id (escapees after anyone on the
/// field with the same id, which cannot happen), Generals kept in setup order —
/// so no query order reaches the report.
#[allow(clippy::type_complexity)]
fn report(
    content: &Content,
    battle: &BattleState,
    units: &Query<(
        Entity,
        &Faction,
        &UnitDefIdx,
        Option<&Health>,
        Option<&SimId>,
        Has<General>,
    )>,
    standing: &Query<(&Building, &Faction, Option<&Health>, Option<&SimId>)>,
) -> [SideReport; 2] {
    let mut sides: [SideReport; 2] = Default::default();
    for (_, f, def, health, id, general) in units {
        let hp = health.copied().unwrap_or_else(|| Health::from_def(content, def.0));
        sides[faction_slot(*f)].units.push(UnitReport {
            sim_id: id.copied().unwrap_or(SimId::UNIDENTIFIED),
            unit: def.0,
            hp: hp.current,
            max_hp: hp.max,
            general,
            escaped: false,
        });
    }
    for e in &battle.escaped {
        sides[faction_slot(e.side)].units.push(UnitReport {
            sim_id: e.sim_id,
            unit: e.unit,
            hp: e.hp,
            max_hp: e.max_hp,
            general: e.general,
            escaped: true,
        });
    }
    for (b, f, health, id) in standing {
        let hp = health
            .copied()
            .unwrap_or_else(|| Health::from_building_def(content, b.def));
        sides[faction_slot(*f)].buildings.push(BuildingReport {
            sim_id: id.copied().unwrap_or(SimId::UNIDENTIFIED),
            building: b.def,
            hp: hp.current,
            max_hp: hp.max,
        });
    }
    for g in &battle.fielded {
        let alive = units.get(g.entity).is_ok_and(|row| row.5);
        sides[faction_slot(g.side)].generals.push(GeneralFate {
            unit: g.unit,
            survived: alive || g.escaped,
            escaped: g.escaped,
        });
    }
    for s in &mut sides {
        s.units.sort_by_key(|u| (u.sim_id, u.escaped, u.unit));
        s.buildings.sort_by_key(|b| (b.sim_id, b.building));
    }
    sides
}

// ---- retreat -----------------------------------------------------------------

/// Carry out every ordered retreat (C1 AC4). Runs right after
/// `apply_commands`, so a retreat ordered this tick starts this tick — and
/// overrides any other order given to the side's units, every tick, until
/// they are off the field. For each unit of a retreating side, in ascending
/// entity order: on or past its entry edge, it has **escaped** (recorded with
/// its HP, then removed from the field); otherwise it walks straight out to
/// the edge, dropping any chase, attack-move or gather job. The side's
/// production stops (queues are cleared): nothing new joins a withdrawal.
#[allow(clippy::type_complexity)]
pub fn withdraw(
    content: Res<Content>,
    state: Res<MatchState>,
    mut battle: ResMut<BattleState>,
    units: Query<(
        Entity,
        &Position,
        &Faction,
        &UnitDefIdx,
        Option<&Health>,
        Option<&SimId>,
        Has<General>,
    )>,
    mut queues: Query<(&Faction, &mut ProductionQueue)>,
    mut commands: Commands,
) {
    if !battle.retreating.contains(&true) || state.is_over() {
        return;
    }
    for (f, mut q) in &mut queues {
        if battle.retreating[faction_slot(*f)] && !q.items.is_empty() {
            q.items.clear();
        }
    }
    let mut rows: Vec<_> = units
        .iter()
        .filter(|(_, _, f, ..)| battle.retreating[faction_slot(**f)])
        .collect();
    rows.sort_unstable_by_key(|(e, ..)| e.to_bits());
    let tick = state.tick();
    for (e, pos, f, def, health, id, general) in rows {
        let edge = battle.edges[faction_slot(*f)];
        if edge.reached(pos.0) {
            let hp = health.copied().unwrap_or_else(|| Health::from_def(&content, def.0));
            battle.escaped.push(Escaped {
                side: *f,
                sim_id: id.copied().unwrap_or(SimId::UNIDENTIFIED),
                unit: def.0,
                hp: hp.current,
                max_hp: hp.max,
                general,
                tick,
            });
            if let Some(g) = battle.fielded.iter_mut().find(|g| g.entity == e) {
                g.escaped = true;
            }
            commands.entity(e).despawn();
        } else {
            let mut ent = commands.entity(e);
            ent.insert(MoveTarget(edge.exit_for(pos.0)))
                .remove::<(Engaging, AttackMoveDest)>();
            economy::release_gather_job(&mut ent);
        }
    }
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
    if !state.tick().is_multiple_of(b.regen_interval_ticks) {
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
