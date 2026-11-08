//! The campaign layer (C4): a deterministic, seeded, **render-free** state
//! machine over the map of `assets/data/campaign.ron`. It owns the map, the
//! nations and their cities, ownership, the armies, rounds and conquest, and
//! its own state hash ([`Campaign::state_hash`]).
//!
//! Render-free on purpose, like `src/sim/`: no render type, no wall-clock, no
//! hash map anywhere. Every collection is a `Vec` walked in index order, and
//! the only randomness is the campaign's own seeded [`SplitMix64`], stepped
//! only here. Battles are decided by a [`Resolver`]; the shipped one plays
//! the real headless battle sim ([`battle::HeadlessResolver`]).
//!
//! A round ([`Campaign::play_round`]):
//! 1. **Order.** The living nations act in a seeded order. Each gives its
//!    decisions: the player's come from the caller, every other nation's
//!    from the campaign AI ([`ai`]).
//! 2. **Movement.** Every marching army walks its road.
//! 3. **Battles**, city by city in map order, wherever a hostile army stands.
//! 4. **Upkeep.**
//! 5. **The end checks**: domination, the player's capital, the round cap.

pub mod ai;
pub mod battle;
pub mod content;
pub mod resolve;

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::sim::content::Content;
use crate::sim::nation::{Defs, NationRules};
use crate::sim::spatial::{Faction, SplitMix64};

pub use resolve::{resolve_battle, HeadlessResolver};
pub use battle::{battle_settings, BattleOutcome, BattlePlan, PlanBuilding, PlanSide, Resolver, Side};
pub use content::{CampaignContent, CampaignError, Fnv};

// ---- state -----------------------------------------------------------------

/// A roster unit in an army: its `units.ron` index and current HP.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unit {
    pub def: usize,
    pub hp: u32,
}

/// An army's General.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Leader {
    /// His `UnitDefIdx` (a General type: `units.len() + k`).
    pub kind: usize,
}

/// Where an army is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Location {
    /// Standing at a city.
    At(usize),
    /// On road `road`, walking from `from` to `to`, `progress` march
    /// points along it (partial progress carries from round to round).
    Marching {
        road: usize,
        from: usize,
        to: usize,
        progress: u32,
    },
}

/// One army on the map: a General and his roster.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Army {
    /// Unique for the campaign, ascending in creation order.
    pub id: u32,
    /// Its nation (an index into [`Content::nations`]).
    pub nation: usize,
    pub general: Leader,
    pub units: Vec<Unit>,
    pub location: Location,
    /// The city it last marched from: where it withdraws after a failed
    /// assault.
    pub came_from: usize,
}

impl Army {
    /// The city it stands at, if it is not on a road.
    pub fn at(&self) -> Option<usize> {
        match self.location {
            Location::At(c) => Some(c),
            Location::Marching { .. } => None,
        }
    }
}

/// A city building: its `units.ron` building index and current HP (0:
/// destroyed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CityBuilding {
    pub def: usize,
    pub hp: u32,
}

/// One city's state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct City {
    /// Its holder (a nation index).
    pub owner: usize,
    /// Its buildings, in `campaign.ron` order (the order a battle places
    /// them).
    pub buildings: Vec<CityBuilding>,
    /// Levy units waiting in the city for an army (C4 AC8), at most
    /// `army_cap`. They defend it; they fall with it.
    pub garrison: Vec<Unit>,
}

impl City {
    /// Some building still stands.
    pub fn standing(&self) -> bool {
        self.buildings.iter().any(|b| b.hp > 0)
    }
}

/// One nation's state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NationState {
    /// Still in the campaign: it holds its capital.
    pub alive: bool,
    /// Alloy from its cities' levies (C4 AC8), spent on raising Generals
    /// and rebuilding.
    pub treasury: u32,
}

/// What a nation decides in its turn. The player's are logged
/// ([`save::CampaignLog`]); the AI's are recomputed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Decision {
    /// Army `army` (standing at a city) marches the road to the adjacent
    /// city `to` (a `campaign.ron` city id).
    March { army: u32, to: String },
}

/// How a campaign ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CampaignOutcome {
    /// Domination: the last nation standing.
    Won { nation: usize },
    /// The player lost his capital.
    PlayerLost,
    /// The round cap came first.
    Undecided,
}

/// Why a round was refused. A refused round changes nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoundError {
    /// The campaign is over.
    Over,
    /// Decisions were given, but the campaign has no player.
    NoPlayer,
    /// A player decision that cannot be carried out.
    BadDecision { decision: Decision, why: String },
}

impl std::fmt::Display for RoundError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RoundError::Over => write!(f, "the campaign is over"),
            RoundError::NoPlayer => write!(f, "decisions given, but the campaign has no player"),
            RoundError::BadDecision { decision, why } => write!(f, "{decision:?} refused: {why}"),
        }
    }
}

impl std::error::Error for RoundError {}

/// One battle of a round, as it went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BattleRecord {
    pub round: u32,
    pub city: usize,
    pub attacker: usize,
    pub defender: usize,
    /// `None`: undecided (a draw, or the tick cap): the defender holds.
    pub winner: Option<Side>,
    /// The city changed hands.
    pub captured: bool,
    /// Taking it ended the defender (its capital).
    pub absorbed: bool,
    /// Battle ticks played (0 for an uncontested city).
    pub ticks: u32,
}

/// What a round did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoundReport {
    pub round: u32,
    /// The nations in the order they acted.
    pub order: Vec<usize>,
    pub battles: Vec<BattleRecord>,
}

/// Per-nation tables (their traits applied): what each nation's units and
/// buildings are, on the map and in its battles.
#[derive(Debug, Clone)]
pub(crate) struct Tables {
    /// `unit_hp[n][u]`: nation `n`'s full HP for roster unit `u`.
    pub unit_hp: Vec<Vec<u32>>,
    /// `building_hp[n][b]`: nation `n`'s full HP for building `b`.
    pub building_hp: Vec<Vec<u32>>,
    /// `unit_speed[n][u]`: nation `n`'s Speed for unit `u` (Generals too).
    pub unit_speed: Vec<Vec<u32>>,
}

impl Tables {
    fn new(content: &Content, economy: crate::sim::three::EconomyMode) -> Self {
        let mut t = Tables {
            unit_hp: Vec::new(),
            building_hp: Vec::new(),
            unit_speed: Vec::new(),
        };
        let all_units = content.units.len() + content.generals.len();
        for nation in &content.nations {
            let rules = NationRules::resolve_in(content, &[Some(nation.id.clone()), None], economy)
                .expect("a content nation resolves");
            let defs = Defs::new(content, rules.as_ref()).in_economy(economy);
            t.unit_hp.push(
                (0..content.units.len())
                    .map(|u| defs.unit_health(Faction::A, u).max)
                    .collect(),
            );
            t.building_hp.push(
                (0..content.buildings.len())
                    .map(|b| defs.building_health(Faction::A, b).max)
                    .collect(),
            );
            t.unit_speed.push(
                (0..all_units)
                    .map(|u| defs.unit(Faction::A, u).map_or(0, |d| d.speed))
                    .collect(),
            );
        }
        t
    }
}

/// The campaign: its content, its seeded RNG, and its whole state.
#[derive(Debug, Clone)]
pub struct Campaign {
    content: Arc<Content>,
    campaign: Arc<CampaignContent>,
    tables: Arc<Tables>,
    /// The battle content's fingerprint hash, taken once.
    content_fp: u64,
    seed: u64,
    rng: SplitMix64,
    round: u32,
    player: Option<usize>,
    nations: Vec<NationState>,
    cities: Vec<City>,
    armies: Vec<Army>,
    next_army: u32,
    outcome: Option<CampaignOutcome>,
    history: Vec<BattleRecord>,
    round_hashes: Vec<u64>,
}

impl Campaign {
    /// A new campaign on `campaign`'s map, seeded by `seed`. `player` names
    /// the nation the caller plays (`None`: every nation is AI). Refuses an
    /// unknown player nation.
    pub fn new(
        content: Content,
        campaign: CampaignContent,
        seed: u64,
        player: Option<&str>,
    ) -> Result<Self, CampaignError> {
        let player = match player {
            None => None,
            Some(id) => match content.nations.iter().position(|n| n.id == id) {
                Some(n) => Some(n),
                None => {
                    return Err(CampaignError::Invalid {
                        msg: format!("unknown player nation `{id}`"),
                    })
                }
            },
        };
        let tables = Tables::new(&content, campaign.economy);
        let content_fp = content.fingerprint().hash();
        let nations = content
            .nations
            .iter()
            .map(|_| NationState {
                alive: true,
                treasury: 0,
            })
            .collect();
        let cities = campaign
            .city_owner
            .iter()
            .zip(&campaign.city_buildings)
            .map(|(&owner, bs)| City {
                owner,
                buildings: bs
                    .iter()
                    .map(|&def| CityBuilding {
                        def,
                        hp: tables.building_hp[owner][def],
                    })
                    .collect(),
                garrison: Vec::new(),
            })
            .collect();
        let mut c = Campaign {
            content: Arc::new(content),
            campaign: Arc::new(campaign),
            tables: Arc::new(tables),
            content_fp,
            seed,
            rng: SplitMix64::new(seed),
            round: 0,
            player,
            nations,
            cities,
            armies: Vec::new(),
            next_army: 1,
            outcome: None,
            history: Vec::new(),
            round_hashes: Vec::new(),
        };
        let starts = c.campaign.start_armies.clone();
        for s in &starts {
            let nation = c.nation_index(&s.nation).expect("validated at load");
            let kind = c.content.general_index(&s.general).expect("validated at load");
            let at = c.campaign.city_index(&s.at).expect("validated at load");
            let mut units = Vec::new();
            for (id, count) in &s.units {
                let def = c.content.unit_index(id).expect("validated at load");
                for _ in 0..*count {
                    units.push(Unit {
                        def,
                        hp: c.tables.unit_hp[nation][def],
                    });
                }
            }
            c.spawn_army(nation, kind, units, at);
        }
        Ok(c)
    }

    fn spawn_army(&mut self, nation: usize, kind: usize, units: Vec<Unit>, at: usize) -> u32 {
        let id = self.next_army;
        self.next_army += 1;
        self.armies.push(Army {
            id,
            nation,
            general: Leader { kind },
            units,
            location: Location::At(at),
            came_from: at,
        });
        id
    }

    // ---- reads ------------------------------------------------------------

    pub fn content(&self) -> &Content {
        &self.content
    }

    pub fn campaign(&self) -> &CampaignContent {
        &self.campaign
    }

    /// The battle content's fingerprint hash.
    pub fn content_fingerprint(&self) -> u64 {
        self.content_fp
    }

    pub fn seed(&self) -> u64 {
        self.seed
    }

    /// Rounds played so far.
    pub fn round(&self) -> u32 {
        self.round
    }

    pub fn player(&self) -> Option<usize> {
        self.player
    }

    pub fn nations(&self) -> &[NationState] {
        &self.nations
    }

    pub fn cities(&self) -> &[City] {
        &self.cities
    }

    /// Every army, ascending id.
    pub fn armies(&self) -> &[Army] {
        &self.armies
    }

    pub fn army(&self, id: u32) -> Option<&Army> {
        self.armies.iter().find(|a| a.id == id)
    }

    pub fn outcome(&self) -> Option<CampaignOutcome> {
        self.outcome
    }

    pub fn is_over(&self) -> bool {
        self.outcome.is_some()
    }

    /// Every battle fought so far, in order.
    pub fn history(&self) -> &[BattleRecord] {
        &self.history
    }

    /// The state hash after each round played, in order.
    pub fn round_hashes(&self) -> &[u64] {
        &self.round_hashes
    }

    pub fn nation_index(&self, id: &str) -> Option<usize> {
        self.content.nations.iter().position(|n| n.id == id)
    }

    pub fn city_index(&self, id: &str) -> Option<usize> {
        self.campaign.city_index(id)
    }

    /// The cities `nation` holds, ascending index.
    pub fn cities_of(&self, nation: usize) -> Vec<usize> {
        (0..self.cities.len())
            .filter(|&c| self.cities[c].owner == nation)
            .collect()
    }

    /// The living nations, ascending index.
    pub fn alive(&self) -> Vec<usize> {
        (0..self.nations.len()).filter(|&n| self.nations[n].alive).collect()
    }

    /// Nation `n`'s full HP for roster unit `u`.
    pub fn unit_max_hp(&self, nation: usize, u: usize) -> u32 {
        self.tables.unit_hp[nation][u]
    }

    /// Nation `n`'s full HP for building `b`.
    pub fn building_max_hp(&self, nation: usize, b: usize) -> u32 {
        self.tables.building_hp[nation][b]
    }

    // ---- the state hash ---------------------------------------------------

    /// The campaign's state hash: FNV-1a over every field of the state, in a
    /// fixed order (nations, cities, armies by index and id). Two campaigns
    /// with one seed, one content and one decision log hash alike after
    /// every round; any divergence shows.
    pub fn state_hash(&self) -> u64 {
        let mut h = Fnv::default();
        h.u64(self.content_fp)
            .u64(self.campaign.fingerprint())
            .u64(self.seed)
            .u64(self.rng.state())
            .u64(self.round as u64)
            .u64(self.player.map_or(u64::MAX, |p| p as u64))
            .u64(match self.outcome {
                None => 0,
                Some(CampaignOutcome::Won { nation }) => 1 + ((nation as u64) << 8),
                Some(CampaignOutcome::PlayerLost) => 2,
                Some(CampaignOutcome::Undecided) => 3,
            });
        h.u64(self.nations.len() as u64);
        for n in &self.nations {
            h.u64(n.alive as u64).u64(n.treasury as u64);
        }
        h.u64(self.cities.len() as u64);
        for c in &self.cities {
            h.u64(c.owner as u64).u64(c.buildings.len() as u64);
            for b in &c.buildings {
                h.u64(b.def as u64).u64(b.hp as u64);
            }
            h.u64(c.garrison.len() as u64);
            for u in &c.garrison {
                h.u64(u.def as u64).u64(u.hp as u64);
            }
        }
        h.u64(self.next_army as u64).u64(self.armies.len() as u64);
        for a in &self.armies {
            h.u64(a.id as u64)
                .u64(a.nation as u64)
                .u64(a.general.kind as u64)
                .u64(a.came_from as u64);
            match a.location {
                Location::At(c) => h.u64(0).u64(c as u64),
                Location::Marching {
                    road,
                    from,
                    to,
                    progress,
                } => h
                    .u64(1)
                    .u64(road as u64)
                    .u64(from as u64)
                    .u64(to as u64)
                    .u64(progress as u64),
            };
            h.u64(a.units.len() as u64);
            for u in &a.units {
                h.u64(u.def as u64).u64(u.hp as u64);
            }
        }
        h.u64(self.history.len() as u64);
        h.finish()
    }

    // ---- a round ----------------------------------------------------------

    /// Play one round: `decisions` are the player's (empty when there is no
    /// player, or he holds everything); every other nation decides by the
    /// campaign AI; battles go to `resolver`. **Atomic**: a refused round
    /// leaves the campaign exactly as it was.
    pub fn play_round(
        &mut self,
        decisions: &[Decision],
        resolver: &mut dyn Resolver,
    ) -> Result<RoundReport, RoundError> {
        if self.is_over() {
            return Err(RoundError::Over);
        }
        if self.player.is_none() && !decisions.is_empty() {
            return Err(RoundError::NoPlayer);
        }
        let mut next = self.clone();
        let report = next.round_inner(decisions, resolver)?;
        *self = next;
        Ok(report)
    }

    fn round_inner(
        &mut self,
        decisions: &[Decision],
        resolver: &mut dyn Resolver,
    ) -> Result<RoundReport, RoundError> {
        self.round += 1;
        let order = self.turn_order();
        for &n in &order {
            if !self.nations[n].alive {
                continue;
            }
            let ds = if Some(n) == self.player {
                decisions.to_vec()
            } else {
                ai::decide(self, n)
            };
            for d in ds {
                self.apply(n, &d)
                    .map_err(|why| RoundError::BadDecision { decision: d.clone(), why })?;
            }
        }
        self.movement();
        let battles = self.battles(&order, resolver);
        self.upkeep();
        self.end_checks();
        let h = self.state_hash();
        self.round_hashes.push(h);
        Ok(RoundReport {
            round: self.round,
            order,
            battles,
        })
    }

    /// The living nations in this round's seeded order (a Fisher-Yates
    /// shuffle of the ascending list on the campaign RNG).
    fn turn_order(&mut self) -> Vec<usize> {
        let mut order = self.alive();
        for i in (1..order.len()).rev() {
            let j = (self.rng.next_u64() % (i as u64 + 1)) as usize;
            order.swap(i, j);
        }
        order
    }

    /// The campaign RNG, for the AI's tie-breaks (stepped only in the
    /// campaign).
    pub(crate) fn rng(&mut self) -> &mut SplitMix64 {
        &mut self.rng
    }

    fn apply(&mut self, nation: usize, d: &Decision) -> Result<(), String> {
        match d {
            Decision::March { army, to } => {
                let to = self
                    .city_index(to)
                    .ok_or_else(|| format!("unknown city `{to}`"))?;
                let i = self
                    .armies
                    .iter()
                    .position(|a| a.id == *army)
                    .ok_or_else(|| format!("no army {army}"))?;
                let a = &self.armies[i];
                if a.nation != nation {
                    return Err(format!("army {army} is not this nation's"));
                }
                let Some(from) = a.at() else {
                    return Err(format!("army {army} is already marching"));
                };
                let road = self
                    .campaign
                    .road_between(from, to)
                    .ok_or_else(|| "no road joins those cities".to_string())?;
                let a = &mut self.armies[i];
                a.location = Location::Marching {
                    road,
                    from,
                    to,
                    progress: 0,
                };
                a.came_from = from;
                Ok(())
            }
        }
    }

    /// Army `id`'s march points per round (C4 AC6): the lowest Speed in its
    /// roster (its nation's tables, so a Speed trait counts), or its
    /// General's with no units; plus the Logistics bonus when the Logistics
    /// General leads it. 0 for an unknown army.
    pub fn march_points(&self, id: u32) -> u32 {
        let Some(a) = self.army(id) else { return 0 };
        let speed = &self.tables.unit_speed[a.nation];
        let base = a
            .units
            .iter()
            .map(|u| speed[u.def])
            .min()
            .unwrap_or(speed[a.general.kind]);
        let bonus = if a.general.kind == self.campaign.logistics_general {
            self.campaign.rules.march.logistics_bonus
        } else {
            0
        };
        base.saturating_add(bonus)
    }

    /// Every marching army walks its road, in id order: its march points
    /// add to its progress, and once the progress covers the road's cost it
    /// stands at the far city (points left over are not carried past a city:
    /// an army stops at every city it reaches).
    fn movement(&mut self) {
        for i in 0..self.armies.len() {
            let pts = self.march_points(self.armies[i].id);
            let a = &mut self.armies[i];
            if let Location::Marching {
                road,
                from,
                to,
                progress,
            } = a.location
            {
                let progress = progress.saturating_add(pts);
                a.location = if progress >= self.campaign.roads[road].cost {
                    Location::At(to)
                } else {
                    Location::Marching {
                        road,
                        from,
                        to,
                        progress,
                    }
                };
            }
        }
    }

    /// Fight every battle of the round, city by city in map order.
    fn battles(&mut self, order: &[usize], resolver: &mut dyn Resolver) -> Vec<BattleRecord> {
        let mut out = Vec::new();
        for city in 0..self.cities.len() {
            let owner = self.cities[city].owner;
            // The attacker: the first nation, in this round's order, with an
            // army standing at a city it does not hold. Any other hostile
            // nation there waits for a later round.
            let Some(&attacker) = order.iter().find(|&&n| {
                n != owner && self.armies.iter().any(|a| a.nation == n && a.at() == Some(city))
            }) else {
                continue;
            };
            let mut plan = battle::plan(self, city, attacker);
            let outcome = if plan.undefended() {
                BattleOutcome::uncontested(&plan)
            } else {
                // A fought battle draws its seed and map side (C4 AC9).
                plan.seed = self.rng.next_u64();
                plan.flipped = self.rng.next_u64() & 1 == 1;
                resolver.resolve(&self.content, &self.campaign, &plan)
            };
            let rec = self.apply_outcome(&plan, &outcome);
            self.history.push(rec.clone());
            out.push(rec);
        }
        out
    }

    /// Carry a battle's outcome into the campaign.
    fn apply_outcome(&mut self, plan: &BattlePlan, outcome: &BattleOutcome) -> BattleRecord {
        let city = plan.city;
        let attacker = plan.attack.nation;
        let defender = plan.defend.nation;
        let mut rec = BattleRecord {
            round: self.round,
            city,
            attacker,
            defender,
            winner: outcome.winner,
            captured: false,
            absorbed: false,
            ticks: outcome.ticks,
        };
        self.survivors(plan, outcome);
        if outcome.winner == Some(Side::Attack) {
            rec.captured = true;
            self.cities[city].owner = attacker;
            // A garrison falls with its city.
            self.cities[city].garrison.clear();
            // The defenders withdraw to their nearest city.
            let defenders: Vec<u32> = plan.defend.armies.clone();
            for id in defenders {
                self.withdraw(id, None);
            }
            if self.campaign.capital[defender] == city {
                rec.absorbed = true;
                self.absorb(defender, attacker);
            }
        } else {
            // A failed assault falls back where it came from.
            for &id in &plan.attack.armies {
                let back = self.army(id).map(|a| a.came_from);
                self.withdraw(id, back);
            }
        }
        rec
    }

    /// Every unit back to where it came from (C4 AC8): an army's survivors
    /// to the army, the garrison's to the garrison, each with its HP after
    /// the battle and in its old order; the dead (HP 0) are gone.
    fn survivors(&mut self, plan: &BattlePlan, outcome: &BattleOutcome) {
        assert_eq!(outcome.attack.units.len(), plan.attack.units.len(), "outcome per plan unit");
        assert_eq!(outcome.defend.units.len(), plan.defend.units.len(), "outcome per plan unit");
        for (side, out) in [(&plan.attack, &outcome.attack), (&plan.defend, &outcome.defend)] {
            for &id in &side.armies {
                if let Some(a) = self.armies.iter_mut().find(|a| a.id == id) {
                    a.units.clear();
                }
            }
            if side.nation == plan.defend.nation {
                self.cities[plan.city].garrison.clear();
            }
            for (u, &hp) in side.units.iter().zip(&out.units) {
                if hp == 0 {
                    continue;
                }
                let unit = Unit { def: u.def, hp };
                match u.army {
                    Some(id) => {
                        if let Some(a) = self.armies.iter_mut().find(|a| a.id == id) {
                            a.units.push(unit);
                        }
                    }
                    None => self.cities[plan.city].garrison.push(unit),
                }
            }
        }
    }

    /// The end of a round (C4 AC8), city by city in map order: each city
    /// pays its holder its Alloy levy; an army of the holder standing in it
    /// takes in its garrison; then its levy units join the holder's
    /// lowest-id army there with room under `army_cap`, or else the
    /// garrison (capped at `army_cap`; a levy beyond it is lost).
    fn upkeep(&mut self) {
        let cap = self.campaign.rules.army_cap as usize;
        for city in 0..self.cities.len() {
            let owner = self.cities[city].owner;
            let alloy = self.campaign.cities[city].levy.alloy;
            let t = &mut self.nations[owner].treasury;
            *t = t.saturating_add(alloy);
            let here: Vec<usize> = (0..self.armies.len())
                .filter(|&i| self.armies[i].nation == owner && self.armies[i].at() == Some(city))
                .collect();
            for &i in &here {
                while self.armies[i].units.len() < cap && !self.cities[city].garrison.is_empty() {
                    let u = self.cities[city].garrison.remove(0);
                    self.armies[i].units.push(u);
                }
            }
            for k in 0..self.campaign.city_levy[city].len() {
                let def = self.campaign.city_levy[city][k];
                let unit = Unit {
                    def,
                    hp: self.tables.unit_hp[owner][def],
                };
                match here.iter().find(|&&i| self.armies[i].units.len() < cap) {
                    Some(&i) => self.armies[i].units.push(unit),
                    None => {
                        if self.cities[city].garrison.len() < cap {
                            self.cities[city].garrison.push(unit);
                        }
                    }
                }
            }
        }
    }

    /// Army `id` falls back: to `to` if its nation still holds it, else to
    /// its nearest city; with no city left, it is disbanded.
    fn withdraw(&mut self, id: u32, to: Option<usize>) {
        let Some(i) = self.armies.iter().position(|a| a.id == id) else {
            return;
        };
        let nation = self.armies[i].nation;
        let from = self.armies[i].at().unwrap_or(self.armies[i].came_from);
        let dest = match to {
            Some(c) if self.cities[c].owner == nation => Some(c),
            _ => self.nearest_city_of(nation, from),
        };
        match dest {
            Some(c) => {
                self.armies[i].location = Location::At(c);
                self.armies[i].came_from = c;
            }
            None => {
                self.armies.remove(i);
            }
        }
    }

    /// The city of `nation` nearest `from` by road (march cost), ties to the
    /// lower index. `from` itself does not count.
    pub fn nearest_city_of(&self, nation: usize, from: usize) -> Option<usize> {
        let dist = self.distances(from);
        (0..self.cities.len())
            .filter(|&c| c != from && self.cities[c].owner == nation)
            .filter(|&c| dist[c] != u32::MAX)
            .min_by_key(|&c| (dist[c], c))
    }

    /// Road-cost distance from `from` to every city (Dijkstra, ties to the
    /// lower index; `u32::MAX`: unreachable, which validation rules out).
    pub fn distances(&self, from: usize) -> Vec<u32> {
        let n = self.cities.len();
        let mut dist = vec![u32::MAX; n];
        let mut done = vec![false; n];
        dist[from] = 0;
        while let Some(c) = (0..n)
            .filter(|&c| !done[c] && dist[c] != u32::MAX)
            .min_by_key(|&c| (dist[c], c))
        {
            done[c] = true;
            for (_, r) in self.campaign.roads_of(c) {
                let o = r.other(c);
                let d = dist[c].saturating_add(r.cost);
                if d < dist[o] {
                    dist[o] = d;
                }
            }
        }
        dist
    }

    /// `conqueror` took `fallen`'s capital: `fallen` is out of the campaign.
    /// Its cities pass to the conqueror and its armies are disbanded.
    fn absorb(&mut self, fallen: usize, conqueror: usize) {
        self.nations[fallen].alive = false;
        for c in &mut self.cities {
            if c.owner == fallen {
                c.owner = conqueror;
            }
        }
        self.armies.retain(|a| a.nation != fallen);
    }

    fn end_checks(&mut self) {
        let alive = self.alive();
        // A fallen player has lost, even in the round the last rival falls.
        if self.player.is_some_and(|p| !self.nations[p].alive) {
            self.outcome = Some(CampaignOutcome::PlayerLost);
        } else if alive.len() == 1 {
            self.outcome = Some(CampaignOutcome::Won { nation: alive[0] });
        } else if self.round >= self.campaign.rules.round_cap {
            self.outcome = Some(CampaignOutcome::Undecided);
        }
    }
}
