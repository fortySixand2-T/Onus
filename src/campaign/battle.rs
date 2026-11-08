//! A campaign battle (C4): what the campaign puts on the field
//! ([`BattlePlan`]), what comes back ([`BattleOutcome`]), and who decides it
//! (a [`Resolver`]).

use crate::sim::content::Content;

use super::content::CampaignContent;
use super::Campaign;

/// The two sides of a campaign battle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// The nation marching on the city.
    Attack,
    /// The city's holder.
    Defend,
}

/// A General on the field: whose army he leads, and his type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanGeneral {
    pub army: u32,
    /// His `UnitDefIdx`.
    pub kind: usize,
}

/// A roster unit on the field: whose army it belongs to, its type and HP.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanUnit {
    pub army: u32,
    pub def: usize,
    pub hp: u32,
}

/// One side of a [`BattlePlan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanSide {
    pub nation: usize,
    /// The armies it fields, ascending id.
    pub armies: Vec<u32>,
    /// Their Generals, in army order.
    pub generals: Vec<PlanGeneral>,
    /// Their units, army by army in army order, each in roster order.
    pub units: Vec<PlanUnit>,
}

impl PlanSide {
    /// Nothing on this side would take the field.
    pub fn is_empty(&self) -> bool {
        self.generals.is_empty() && self.units.is_empty()
    }
}

/// A city building on the field: its slot in the city, type and HP.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanBuilding {
    /// Its index in the city's building list.
    pub slot: usize,
    pub def: usize,
    pub hp: u32,
}

/// Everything a battle starts from, built from campaign state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BattlePlan {
    pub round: u32,
    /// The city fought over.
    pub city: usize,
    pub attack: PlanSide,
    pub defend: PlanSide,
    /// The city's standing buildings (a destroyed one does not take the
    /// field), defending with the holder, in city order.
    pub buildings: Vec<PlanBuilding>,
}

impl BattlePlan {
    /// The city has nothing to defend it: no army and no standing building.
    pub fn undefended(&self) -> bool {
        self.defend.is_empty() && self.buildings.is_empty()
    }
}

/// How one side came out of a battle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SideOutcome {
    /// Per plan General: did he survive?
    pub generals: Vec<bool>,
    /// Per plan unit: its HP after the battle (0: dead).
    pub units: Vec<u32>,
}

/// What a battle did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BattleOutcome {
    /// `None`: undecided (a draw or the tick cap) — the defender holds.
    pub winner: Option<Side>,
    pub attack: SideOutcome,
    pub defend: SideOutcome,
    /// Per plan building: its HP after the battle (0: destroyed).
    pub buildings: Vec<u32>,
    /// Battle ticks played.
    pub ticks: u32,
}

impl SideOutcome {
    /// Everyone walks off the field as he came on.
    pub fn unchanged(side: &PlanSide) -> Self {
        SideOutcome {
            generals: vec![true; side.generals.len()],
            units: side.units.iter().map(|u| u.hp).collect(),
        }
    }
}

impl BattleOutcome {
    /// `winner`, with nobody hurt: a scripted outcome.
    pub fn unchanged(plan: &BattlePlan, winner: Option<Side>) -> Self {
        BattleOutcome {
            winner,
            attack: SideOutcome::unchanged(&plan.attack),
            defend: SideOutcome::unchanged(&plan.defend),
            buildings: plan.buildings.iter().map(|b| b.hp).collect(),
            ticks: 0,
        }
    }

    /// A city nobody defends falls without a fight.
    pub fn uncontested(plan: &BattlePlan) -> Self {
        Self::unchanged(plan, Some(Side::Attack))
    }
}

/// Decides a campaign battle. The shipped one plays the headless battle sim;
/// a test may script one.
pub trait Resolver {
    fn resolve(&mut self, content: &Content, campaign: &CampaignContent, plan: &BattlePlan)
        -> BattleOutcome;
}

/// Any closure over the plan is a resolver (a scripted one, for tests).
impl<F: FnMut(&BattlePlan) -> BattleOutcome> Resolver for F {
    fn resolve(&mut self, _: &Content, _: &CampaignContent, plan: &BattlePlan) -> BattleOutcome {
        self(plan)
    }
}

fn side(c: &Campaign, nation: usize, armies: Vec<u32>) -> PlanSide {
    let mut s = PlanSide {
        nation,
        armies: armies.clone(),
        generals: Vec::new(),
        units: Vec::new(),
    };
    for id in armies {
        let a = c.army(id).expect("an army in the plan exists");
        s.generals.push(PlanGeneral {
            army: id,
            kind: a.general.kind,
        });
        s.units.extend(a.units.iter().map(|u| PlanUnit {
            army: id,
            def: u.def,
            hp: u.hp,
        }));
    }
    s
}

/// The battle at `city`, `attacker` marching on it: the attacker's first
/// army there against every army of the holder standing in it.
pub(crate) fn plan(c: &Campaign, city: usize, attacker: usize) -> BattlePlan {
    let defender = c.cities()[city].owner;
    let here = |n: usize| -> Vec<u32> {
        c.armies()
            .iter()
            .filter(|a| a.nation == n && a.at() == Some(city))
            .map(|a| a.id)
            .collect()
    };
    let mut attack = here(attacker);
    attack.truncate(1);
    BattlePlan {
        round: c.round(),
        city,
        attack: side(c, attacker, attack),
        defend: side(c, defender, here(defender)),
        buildings: c.cities()[city]
            .buildings
            .iter()
            .enumerate()
            .filter(|(_, b)| b.hp > 0)
            .map(|(slot, b)| PlanBuilding {
                slot,
                def: b.def,
                hp: b.hp,
            })
            .collect(),
    }
}
