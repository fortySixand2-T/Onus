//! A campaign battle (C4): what the campaign puts on the field
//! ([`BattlePlan`]), what comes back ([`BattleOutcome`]), and who decides it
//! (a [`Resolver`]).

use serde::{Deserialize, Serialize};

use crate::headless::{MatchSettings, Orientation};
use crate::sim::battle::{AuraRank, BattleSetup, Roster};
use crate::sim::content::Content;
use crate::sim::spatial::Faction;

use super::content::CampaignContent;
use super::Campaign;

/// The two sides of a campaign battle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
    /// His rank (C4 AC2): an index into `CampaignContent::ranks`.
    pub rank: usize,
}

/// A roster unit on the field: whose army it belongs to (`None`: the city's
/// garrison), its type and HP.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanUnit {
    pub army: Option<u32>,
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
    /// Their units, army by army in army order, each in roster order; for
    /// the defender, the city's garrison last.
    pub units: Vec<PlanUnit>,
    /// The cities its nation holds: its size, which sets its purse.
    pub cities: u32,
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
    /// The battle's match seed, drawn from the campaign's rng (0 for a
    /// battle never fought: an undefended city).
    pub seed: u64,
    /// The battle is fought on the reflected map (the attacker on the
    /// right), drawn from the campaign's rng.
    pub flipped: bool,
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SideOutcome {
    /// Per plan General: did he survive?
    pub generals: Vec<bool>,
    /// Per plan unit: its HP after the battle (0: dead).
    pub units: Vec<u32>,
}

/// What a battle did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

/// A battle of the player's, played live (C6 AC2): its outcome, keyed by
/// the round and the city (a `campaign.ron` city id) it was fought over.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayedBattle {
    pub round: u32,
    pub city: String,
    pub outcome: BattleOutcome,
}

/// A resolver that resolves each plan once (C6 AC2): a live round is
/// replayed after each pause, and its AI-vs-AI battles come back from here
/// instead of being fought again. The same plan gives the same outcome
/// either way; this only saves the time.
pub struct Memo<R> {
    inner: R,
    seen: Vec<(BattlePlan, BattleOutcome)>,
}

impl<R: Resolver> Memo<R> {
    pub fn new(inner: R) -> Self {
        Memo {
            inner,
            seen: Vec::new(),
        }
    }

    /// Forget every outcome (a client clears it once a round is played).
    pub fn clear(&mut self) {
        self.seen.clear();
    }
}

impl<R: Resolver> Resolver for Memo<R> {
    fn resolve(&mut self, content: &Content, campaign: &CampaignContent, plan: &BattlePlan) -> BattleOutcome {
        if let Some((_, o)) = self.seen.iter().find(|(p, _)| p == plan) {
            return o.clone();
        }
        let o = self.inner.resolve(content, campaign, plan);
        self.seen.push((plan.clone(), o.clone()));
        o
    }
}

fn side(c: &Campaign, nation: usize, armies: Vec<u32>) -> PlanSide {
    let mut s = PlanSide {
        nation,
        armies: armies.clone(),
        generals: Vec::new(),
        units: Vec::new(),
        cities: c.cities_of(nation).len() as u32,
    };
    for id in armies {
        let a = c.army(id).expect("an army in the plan exists");
        // A fallen General's army fights without one (C4 AC2).
        if a.general.present() {
            s.generals.push(PlanGeneral {
                army: id,
                kind: a.general.kind,
                rank: c.campaign().rank_of(a.general.xp),
            });
        }
        s.units.extend(a.units.iter().map(|u| PlanUnit {
            army: Some(id),
            def: u.def,
            hp: u.hp,
        }));
    }
    s
}

/// The battle at `city`, `attacker` marching on it (C4 AC7): **every**
/// army of the attacker standing there (all arrived this round: a failed
/// assault falls back) against every army of the holder standing in it.
/// Several armies bring several Generals.
pub(crate) fn plan(c: &Campaign, city: usize, attacker: usize) -> BattlePlan {
    let defender = c.cities()[city].owner;
    let here = |n: usize| -> Vec<u32> {
        c.armies()
            .iter()
            .filter(|a| a.nation == n && a.at() == Some(city))
            .map(|a| a.id)
            .collect()
    };
    let attack = here(attacker);
    let mut defend = side(c, defender, here(defender));
    defend.units.extend(c.cities()[city].garrison.iter().map(|u| PlanUnit {
        army: None,
        def: u.def,
        hp: u.hp,
    }));
    BattlePlan {
        round: c.round(),
        city,
        seed: 0,
        flipped: false,
        attack: side(c, attacker, attack),
        defend,
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

/// The match a [`BattlePlan`] is fought as (C4 AC9), built from campaign
/// state alone. The attacker plays [`Faction::A`] and the holder
/// [`Faction::B`], on the map side and seed the plan drew. Each side plays
/// its nation (so its traits) and that nation's strategy, on the campaign's
/// economy, and starts with `starting_alloy + alloy_per_city x` the cities
/// it holds. Its roster is its plan units at their HP, then its battle-only
/// workers; the holder also fields the city's standing buildings at their
/// HP. The tick cap is the campaign's.
pub fn battle_settings(content: &Content, campaign: &CampaignContent, plan: &BattlePlan) -> MatchSettings {
    let rules = &campaign.rules.battle;
    let nation = |s: &PlanSide| &content.nations[s.nation];
    let roster = |s: &PlanSide, workers: u32| {
        let mut r = Roster::default();
        for u in &s.units {
            r = r.unit_hp(&content.units[u.def].id, u.hp);
        }
        for _ in 0..workers {
            r = r.unit("worker");
        }
        r
    };
    let mut defend = roster(&plan.defend, rules.defender_workers);
    for b in &plan.buildings {
        defend = defend.building_hp(&content.buildings[b.def].id, b.hp);
    }
    let mut setup = BattleSetup::default()
        .with_roster(Faction::A, roster(&plan.attack, rules.attacker_workers))
        .with_roster(Faction::B, defend);
    for (f, s) in [(Faction::A, &plan.attack), (Faction::B, &plan.defend)] {
        for g in &s.generals {
            let id = &content.general_at(g.kind).expect("a plan General is a General").id;
            let r = &campaign.ranks[g.rank];
            let rank = AuraRank {
                radius_milli: r.radius_milli(),
                extra_factor: r.extra_factor,
            };
            setup = setup.with_ranked_general(f, id, (!rank.is_plain()).then_some(rank));
        }
    }
    let purse = |s: &PlanSide| rules.starting_alloy.saturating_add(rules.alloy_per_city.saturating_mul(s.cities));
    let (a, d) = (nation(&plan.attack), nation(&plan.defend));
    MatchSettings::default()
        .with_seed(plan.seed)
        .with_strategies(&a.strategy, &d.strategy)
        .with_nations(Some(&a.id), Some(&d.id))
        .with_economy(campaign.economy)
        .with_tick_cap(rules.tick_cap)
        .with_orientation(if plan.flipped { Orientation::Swapped } else { Orientation::Normal })
        .with_starting_alloy(purse(&plan.attack), purse(&plan.defend))
        .with_battle(setup)
}
