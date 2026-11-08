//! The campaign's content (C4): `assets/data/campaign.ron`, parsed and
//! validated against the battle [`Content`] it plays with. **Render-free**:
//! plain file IO and serde.
//!
//! Every campaign number lives here — the map (cities, roads, terrain
//! multipliers), the start armies, the General ranks and their aura steps, the
//! repair rate, the levies, the raise cost, Logistics, the battle sizing and
//! the length target. Nothing in `src/campaign/` hardcodes one.
//!
//! The file is **separate from [`Content`]** on purpose: the battle content's
//! fingerprint (what every match log and golden is pinned against) does not
//! move when the campaign is added or tuned. The campaign has its own
//! fingerprint ([`CampaignContent::fingerprint`]), which a campaign save
//! records next to the battle content's.

use std::fmt;
use std::path::Path;

use ron::extensions::Extensions;
use serde::Deserialize;

use crate::sim::content::Content;
use crate::sim::three::EconomyMode;

/// Per-mille scale for every multiplier in this file (1000 = x1).
pub const MILLI: u32 = 1000;

fn to_milli(v: f32) -> u32 {
    (v * MILLI as f32).round() as u32
}

// ---- the file --------------------------------------------------------------

/// A kind of road (C4 AC6): its length multiplier.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerrainDef {
    pub id: String,
    /// A road of this terrain costs `length x length_mult` march points
    /// (rounded up). Must be >= 1: terrain only ever slows a march.
    pub length_mult: f32,
}

impl TerrainDef {
    pub fn mult_milli(&self) -> u32 {
        to_milli(self.length_mult)
    }
}

/// What a city adds to its owner every round (C4 AC8).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LevyDef {
    /// Alloy into the owner's treasury.
    pub alloy: u32,
    /// Roster units (combat units, never a worker) that join one of the
    /// owner's armies at full HP.
    pub units: Vec<String>,
}

/// One city of the map.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CityDef {
    pub id: String,
    pub name: String,
    /// The nation holding it at the start (a `nations.ron` id).
    pub owner: String,
    /// The nation's capital: taking it absorbs the nation. Exactly one per
    /// nation.
    #[serde(default)]
    pub capital: bool,
    /// Its buildings (`units.ron` building ids), at full HP at the start, in
    /// the order a battle places them.
    pub buildings: Vec<String>,
    pub levy: LevyDef,
}

/// A road between two cities, walkable both ways.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoadDef {
    pub from: String,
    pub to: String,
    /// In march points, before the terrain multiplier. > 0.
    pub length: u32,
    /// A [`TerrainDef`] id.
    pub terrain: String,
}

/// An army on the map at the start.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartArmyDef {
    pub nation: String,
    /// A `units.ron` `generals` id.
    pub general: String,
    /// A city the nation holds at the start.
    pub at: String,
    /// `(unit id, count)` pairs, combat units only.
    pub units: Vec<(String, u32)>,
}

/// One General rank (C4 AC2): reached at `xp` experience. Each rank
/// strengthens the aura: `radius_mult` scales its radius, and
/// `extra_factor` adds steps to its strength (an aura's modifiers are applied
/// `factor` times; a Flat aura has factor 1). The last rank is the cap.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RankDef {
    pub xp: u32,
    pub radius_mult: f32,
    pub extra_factor: u32,
}

impl RankDef {
    pub fn radius_milli(&self) -> u32 {
        to_milli(self.radius_mult)
    }
}

/// Experience a General earns from a battle he survives (C4 AC2). Winning
/// earns more than surviving a loss; dying earns nothing (he is replaced).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct XpDef {
    pub win: u32,
    pub survive: u32,
}

/// Map movement (C4 AC6).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarchDef {
    /// The General type whose army gets the Logistics bonus (Tollmarch's).
    pub logistics_general: String,
    /// Extra march points per round for an army that General leads.
    pub logistics_bonus: u32,
}

/// How a campaign battle is sized (C4 AC9).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BattleRulesDef {
    /// Ticks a battle may run before the defender is held to have kept the
    /// city (the attacker withdraws). > 0.
    pub tick_cap: u32,
    /// Workers the attacker brings (battle-only: they never join an army).
    pub attacker_workers: u32,
    /// Workers the defending city fields (battle-only).
    pub defender_workers: u32,
    /// Each side's starting Alloy is `starting_alloy + alloy_per_city x` the
    /// cities its nation holds: a nation's size reaches the battle.
    pub starting_alloy: u32,
    pub alloy_per_city: u32,
}

/// The campaign's length target (C0: 15-25 rounds).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LengthDef {
    pub min_rounds: u32,
    pub max_rounds: u32,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RulesDef {
    pub march: MarchDef,
    pub xp: XpDef,
    /// Rounds an army waits for a new General (rank 0) after its General
    /// falls. While it waits, it cannot march.
    pub general_replacement_rounds: u32,
    /// At most this many absorbed Generals per nation (C0: 3).
    pub max_absorbed_generals: u32,
    /// Treasury Alloy a nation pays to raise an extra General of its own
    /// house at one of its cities (C4 AC5).
    pub raise_general_cost: u32,
    /// Building HP an unattacked city repairs per building per round.
    pub repair_per_round: u32,
    /// Per-nation repair multipliers `(nation id, mult)`: a perk, so it acts
    /// only on the nation's own cities. A nation not listed repairs at x1.
    pub repair_mult: Vec<(String, f32)>,
    /// The most roster units one army holds; a levy beyond it is lost.
    pub army_cap: u32,
    pub battle: BattleRulesDef,
    /// The backstop: a campaign undecided after this many rounds is over,
    /// undecided.
    pub round_cap: u32,
    pub length_target: LengthDef,
}

/// The campaign AI's numbers (C4).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignAiDef {
    /// An army marches on a hostile city only with at least this many units.
    pub attack_min_units: u32,
    /// A nation raises extra Generals only while it has fewer armies.
    pub max_armies: u32,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct CampaignFile {
    economy: EconomyMode,
    terrain: Vec<TerrainDef>,
    cities: Vec<CityDef>,
    roads: Vec<RoadDef>,
    start_armies: Vec<StartArmyDef>,
    ranks: Vec<RankDef>,
    rules: RulesDef,
    ai: CampaignAiDef,
}

// ---- the loaded, resolved content -----------------------------------------

/// A road with its ends and terrain resolved to indices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Road {
    pub a: usize,
    pub b: usize,
    /// March points to walk it: `ceil(length x terrain mult)`.
    pub cost: u32,
}

impl Road {
    /// The other end, seen from `from` (which must be an end).
    pub fn other(&self, from: usize) -> usize {
        if self.a == from {
            self.b
        } else {
            self.a
        }
    }
}

/// The campaign content, parsed, validated and resolved to indices: cities,
/// nations (indices into [`Content::nations`]), units and buildings (indices
/// into the battle content).
#[derive(Debug, Clone)]
pub struct CampaignContent {
    pub economy: EconomyMode,
    pub terrain: Vec<TerrainDef>,
    pub cities: Vec<CityDef>,
    pub roads_def: Vec<RoadDef>,
    pub start_armies: Vec<StartArmyDef>,
    pub ranks: Vec<RankDef>,
    pub rules: RulesDef,
    pub ai: CampaignAiDef,
    /// Resolved: per city, its starting owner (nation index).
    pub city_owner: Vec<usize>,
    /// Resolved: per city, its buildings (building indices).
    pub city_buildings: Vec<Vec<usize>>,
    /// Resolved: per city, its levy units (unit indices).
    pub city_levy: Vec<Vec<usize>>,
    /// Resolved: per nation (index into `Content::nations`), its capital.
    pub capital: Vec<usize>,
    /// Resolved roads, in RON order.
    pub roads: Vec<Road>,
    /// Resolved: per nation, its own General type (`UnitDefIdx`).
    pub nation_general: Vec<usize>,
    /// Resolved: the Logistics General type.
    pub logistics_general: usize,
    /// Resolved: per nation, its repair multiplier (per mille).
    pub repair_milli: Vec<u32>,
    fingerprint: u64,
}

/// Why the campaign content failed to load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CampaignError {
    Io { path: String, msg: String },
    Parse { path: String, msg: String },
    Invalid { msg: String },
}

impl fmt::Display for CampaignError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CampaignError::Io { path, msg } => write!(f, "cannot read {path}: {msg}"),
            CampaignError::Parse { path, msg } => write!(f, "cannot parse {path}: {msg}"),
            CampaignError::Invalid { msg } => write!(f, "invalid campaign: {msg}"),
        }
    }
}

impl std::error::Error for CampaignError {}

/// FNV-1a, the same discipline as the content fingerprint and the sim's
/// state hash: words in a fixed order, no map anywhere.
#[derive(Debug, Clone, Copy)]
pub struct Fnv(u64);

impl Default for Fnv {
    fn default() -> Self {
        Fnv(0xcbf2_9ce4_8422_2325)
    }
}

impl Fnv {
    pub fn u64(&mut self, v: u64) -> &mut Self {
        for b in v.to_le_bytes() {
            self.0 ^= b as u64;
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
        self
    }

    pub fn bytes(&mut self, s: &[u8]) -> &mut Self {
        self.u64(s.len() as u64);
        for b in s {
            self.0 ^= *b as u64;
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
        self
    }

    /// Never 0 (0 is "nothing stamped", as for the content fingerprint).
    pub fn finish(&self) -> u64 {
        if self.0 == 0 {
            1
        } else {
            self.0
        }
    }
}

/// The shipped campaign file's name in the data directory.
pub const CAMPAIGN_FILE: &str = "campaign.ron";

impl CampaignContent {
    /// Load `campaign.ron` from a data directory, validated against `content`.
    pub fn load_from_dir(dir: &Path, content: &Content) -> Result<Self, CampaignError> {
        let path = dir.join(CAMPAIGN_FILE);
        let text = std::fs::read_to_string(&path).map_err(|e| CampaignError::Io {
            path: path.display().to_string(),
            msg: e.to_string(),
        })?;
        Self::from_ron(&text, content).map_err(|e| match e {
            CampaignError::Parse { msg, .. } => CampaignError::Parse {
                path: path.display().to_string(),
                msg,
            },
            other => other,
        })
    }

    /// Load the shipped campaign from the crate's own `assets/data`.
    pub fn shipped(content: &Content) -> Result<Self, CampaignError> {
        Self::load_from_dir(
            &std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data"),
            content,
        )
    }

    /// Parse and validate campaign RON text against `content`.
    pub fn from_ron(text: &str, content: &Content) -> Result<Self, CampaignError> {
        let opts = ron::Options::default().with_default_extension(Extensions::IMPLICIT_SOME);
        let file: CampaignFile = opts.from_str(text).map_err(|e| CampaignError::Parse {
            path: "<campaign>".to_string(),
            msg: e.to_string(),
        })?;
        Self::resolve(file, content)
    }

    fn resolve(f: CampaignFile, content: &Content) -> Result<Self, CampaignError> {
        let bad = |msg: String| Err(CampaignError::Invalid { msg });
        let nation = |id: &str| content.nations.iter().position(|n| n.id == id);
        if content.nations.is_empty() {
            return bad("the battle content has no nations (nations.ron)".to_string());
        }
        if content.battle.is_none() {
            return bad("the battle content has no `mvp_battle` block".to_string());
        }
        if !f.economy.is_alloy() && content.three_resource.is_none() {
            return bad("economy ThreeResource, but the content has no `three_resource` block".into());
        }

        // Terrain.
        for (i, t) in f.terrain.iter().enumerate() {
            if f.terrain[..i].iter().any(|u| u.id == t.id) {
                return bad(format!("duplicate terrain `{}`", t.id));
            }
            if !(t.length_mult.is_finite() && t.mult_milli() >= MILLI) {
                return bad(format!(
                    "terrain `{}` has length_mult {}: must be a finite number >= 1",
                    t.id, t.length_mult
                ));
            }
        }

        // Cities.
        if f.cities.is_empty() {
            return bad("the map has no cities".to_string());
        }
        let mut city_owner = Vec::new();
        let mut city_buildings = Vec::new();
        let mut city_levy = Vec::new();
        let mut capital: Vec<Option<usize>> = vec![None; content.nations.len()];
        for (i, c) in f.cities.iter().enumerate() {
            if f.cities[..i].iter().any(|d| d.id == c.id) {
                return bad(format!("duplicate city `{}`", c.id));
            }
            let Some(owner) = nation(&c.owner) else {
                return bad(format!("city `{}` is held by unknown nation `{}`", c.id, c.owner));
            };
            if c.capital {
                if let Some(other) = capital[owner] {
                    return bad(format!(
                        "nation `{}` has two capitals: `{}` and `{}`",
                        c.owner, f.cities[other].id, c.id
                    ));
                }
                capital[owner] = Some(i);
            }
            let mut bs = Vec::new();
            for b in &c.buildings {
                let Some(k) = content.building_index(b) else {
                    return bad(format!("city `{}` has unknown building `{b}`", c.id));
                };
                bs.push(k);
            }
            let mut levy = Vec::new();
            for u in &c.levy.units {
                levy.push(combat_unit(content, u, &format!("city `{}` levy", c.id))?);
            }
            city_owner.push(owner);
            city_buildings.push(bs);
            city_levy.push(levy);
        }
        let mut caps = Vec::new();
        for (n, c) in capital.iter().enumerate() {
            match c {
                Some(c) => caps.push(*c),
                None => {
                    return bad(format!(
                        "nation `{}` has no capital on the map (exactly one city per nation \
                         must be `capital: true`)",
                        content.nations[n].id
                    ))
                }
            }
        }

        // Roads.
        let city = |id: &str| f.cities.iter().position(|c| c.id == id);
        let mut roads = Vec::new();
        for r in &f.roads {
            let (Some(a), Some(b)) = (city(&r.from), city(&r.to)) else {
                return bad(format!("road `{}`-`{}` names an unknown city", r.from, r.to));
            };
            if a == b {
                return bad(format!("road `{}`-`{}` joins a city to itself", r.from, r.to));
            }
            if r.length == 0 {
                return bad(format!("road `{}`-`{}` has length 0", r.from, r.to));
            }
            let Some(t) = f.terrain.iter().find(|t| t.id == r.terrain) else {
                return bad(format!(
                    "road `{}`-`{}` has unknown terrain `{}`",
                    r.from, r.to, r.terrain
                ));
            };
            if roads.iter().any(|q: &Road| (q.a == a && q.b == b) || (q.a == b && q.b == a)) {
                return bad(format!("duplicate road `{}`-`{}`", r.from, r.to));
            }
            let cost = (r.length as u64 * t.mult_milli() as u64).div_ceil(MILLI as u64) as u32;
            roads.push(Road { a, b, cost });
        }
        // Every city reachable from every other: a city no army can reach is
        // a city no one can ever take, and domination would be impossible.
        let mut seen = vec![false; f.cities.len()];
        let mut stack = vec![0usize];
        seen[0] = true;
        while let Some(c) = stack.pop() {
            for r in roads.iter().filter(|r| r.a == c || r.b == c) {
                let o = r.other(c);
                if !seen[o] {
                    seen[o] = true;
                    stack.push(o);
                }
            }
        }
        if let Some(i) = seen.iter().position(|s| !s) {
            return bad(format!(
                "city `{}` is not reachable by road from `{}`",
                f.cities[i].id, f.cities[0].id
            ));
        }

        // Start armies.
        for a in &f.start_armies {
            let Some(n) = nation(&a.nation) else {
                return bad(format!("start army of unknown nation `{}`", a.nation));
            };
            if content.general_index(&a.general).is_none() {
                return bad(format!(
                    "start army of `{}` has unknown general `{}`",
                    a.nation, a.general
                ));
            }
            let Some(at) = city(&a.at) else {
                return bad(format!("start army of `{}` at unknown city `{}`", a.nation, a.at));
            };
            if city_owner[at] != n {
                return bad(format!(
                    "start army of `{}` stands at `{}`, which it does not hold",
                    a.nation, a.at
                ));
            }
            let mut total = 0u32;
            for (u, count) in &a.units {
                combat_unit(content, u, &format!("start army of `{}`", a.nation))?;
                total = total.saturating_add(*count);
            }
            if total > f.rules.army_cap {
                return bad(format!(
                    "start army of `{}` has {total} units, above army_cap {}",
                    a.nation, f.rules.army_cap
                ));
            }
        }

        // Ranks: the first at 0 xp and x1, strictly rising xp, never weaker.
        if f.ranks.is_empty() {
            return bad("no General ranks (rank 0 is required)".to_string());
        }
        for (i, r) in f.ranks.iter().enumerate() {
            if !r.radius_mult.is_finite() || r.radius_milli() < MILLI {
                return bad(format!(
                    "rank {i} has radius_mult {}: must be a finite number >= 1",
                    r.radius_mult
                ));
            }
            if i == 0 {
                if r.xp != 0 || r.radius_milli() != MILLI || r.extra_factor != 0 {
                    return bad(
                        "rank 0 must be (xp: 0, radius_mult: 1.0, extra_factor: 0): a \
                         replacement General starts there, with his plain aura"
                            .to_string(),
                    );
                }
                continue;
            }
            let p = &f.ranks[i - 1];
            if r.xp <= p.xp {
                return bad(format!("rank {i} needs xp {} but rank {} needs {}", r.xp, i - 1, p.xp));
            }
            if r.radius_milli() < p.radius_milli() || r.extra_factor < p.extra_factor {
                return bad(format!("rank {i} is weaker than rank {}", i - 1));
            }
        }

        // Rules.
        let ru = &f.rules;
        let Some(logistics_general) = content.general_index(&ru.march.logistics_general) else {
            return bad(format!(
                "rules.march.logistics_general `{}` is not a general",
                ru.march.logistics_general
            ));
        };
        if ru.xp.win < ru.xp.survive {
            return bad("rules.xp: winning must earn at least what surviving earns".to_string());
        }
        if ru.army_cap == 0 {
            return bad("rules.army_cap is 0".to_string());
        }
        if ru.battle.tick_cap == 0 {
            return bad("rules.battle.tick_cap is 0".to_string());
        }
        if ru.round_cap == 0 {
            return bad("rules.round_cap is 0".to_string());
        }
        let lt = &ru.length_target;
        if lt.min_rounds == 0 || lt.min_rounds > lt.max_rounds || lt.max_rounds > ru.round_cap {
            return bad(format!(
                "rules.length_target {}..={} must be non-empty, start above 0 and end at or \
                 below round_cap {}",
                lt.min_rounds, lt.max_rounds, ru.round_cap
            ));
        }
        let mut repair_milli = vec![MILLI; content.nations.len()];
        for (i, (id, m)) in ru.repair_mult.iter().enumerate() {
            let Some(n) = nation(id) else {
                return bad(format!("rules.repair_mult names unknown nation `{id}`"));
            };
            if ru.repair_mult[..i].iter().any(|(o, _)| o == id) {
                return bad(format!("rules.repair_mult lists `{id}` twice"));
            }
            if !m.is_finite() || *m < 0.0 {
                return bad(format!("rules.repair_mult for `{id}` is {m}: must be >= 0"));
            }
            repair_milli[n] = to_milli(*m);
        }
        if f.ai.attack_min_units == 0 {
            return bad("ai.attack_min_units is 0".to_string());
        }
        if f.ai.max_armies == 0 {
            return bad("ai.max_armies is 0".to_string());
        }

        let mut nation_general = Vec::new();
        for n in &content.nations {
            match content.general_index(&n.general) {
                Some(g) => nation_general.push(g),
                None => return bad(format!("nation `{}` has unknown general `{}`", n.id, n.general)),
            }
        }

        let fingerprint = {
            let mut h = Fnv::default();
            h.bytes(format!("{f:?}").as_bytes());
            h.finish()
        };
        Ok(CampaignContent {
            economy: f.economy,
            terrain: f.terrain,
            cities: f.cities,
            roads_def: f.roads,
            start_armies: f.start_armies,
            ranks: f.ranks,
            rules: f.rules,
            ai: f.ai,
            city_owner,
            city_buildings,
            city_levy,
            capital: caps,
            roads,
            nation_general,
            logistics_general,
            repair_milli,
            fingerprint,
        })
    }

    /// The campaign content's fingerprint: an FNV hash of every parsed value
    /// (floats by their exact shortest rendering), never 0.
    pub fn fingerprint(&self) -> u64 {
        self.fingerprint
    }

    pub fn city_index(&self, id: &str) -> Option<usize> {
        self.cities.iter().position(|c| c.id == id)
    }

    /// The rank a General with `xp` experience holds: the highest whose
    /// threshold he has reached (the last rank is the cap).
    pub fn rank_of(&self, xp: u32) -> usize {
        self.ranks.iter().rposition(|r| r.xp <= xp).unwrap_or(0)
    }

    /// Roads touching `city`, in RON order.
    pub fn roads_of(&self, city: usize) -> impl Iterator<Item = (usize, &Road)> + '_ {
        self.roads
            .iter()
            .enumerate()
            .filter(move |(_, r)| r.a == city || r.b == city)
    }

    /// The road joining `a` and `b`, if any.
    pub fn road_between(&self, a: usize, b: usize) -> Option<usize> {
        self.roads
            .iter()
            .position(|r| (r.a == a && r.b == b) || (r.a == b && r.b == a))
    }
}

/// A combat unit id (a roster unit with offense): what an army holds.
fn combat_unit(content: &Content, id: &str, who: &str) -> Result<usize, CampaignError> {
    match content.unit_index(id) {
        Some(i) if content.units[i].offense > 0 && !content.units[i].gathers => Ok(i),
        Some(_) => Err(CampaignError::Invalid {
            msg: format!("{who} names `{id}`, which is not a combat unit"),
        }),
        None => Err(CampaignError::Invalid {
            msg: format!("{who} names unknown unit `{id}`"),
        }),
    }
}
