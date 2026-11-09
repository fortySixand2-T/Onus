//! Diplomacy (C5): `assets/data/diplomacy.ron`, parsed and validated against
//! the battle [`Content`]. **Render-free**: plain file IO and serde.
//!
//! Diplomacy is **opt-in**: a [`CampaignContent`](super::CampaignContent)
//! takes it on only through `with_diplomacy`. Without it a campaign plays
//! exactly as C4, and the campaign content's fingerprint is C4's.
//!
//! Every diplomacy number lives in the file: the dispositions, the ally cap,
//! the betrayal drop, the destabilize requirement, the costs, the AI's
//! thresholds, notice and the length of a steered war.

use std::fmt;
use std::path::Path;

use ron::extensions::Extensions;
use serde::{Deserialize, Serialize};

use crate::sim::content::Content;

use super::content::{CampaignError, Fnv};

/// A pair of nations' standing, from worst to best.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Standing {
    Hostile,
    Neutral,
    Tied,
    Allied,
}

impl Standing {
    /// Its worth in willingness: Hostile 0, Neutral 1, Tied 2, Allied 3.
    pub fn rank(self) -> i32 {
        self as i32
    }

    /// One step up, no higher than `cap`.
    pub fn up(self, cap: Standing) -> Standing {
        let s = match self {
            Standing::Hostile => Standing::Neutral,
            Standing::Neutral => Standing::Tied,
            Standing::Tied | Standing::Allied => Standing::Allied,
        };
        s.min(cap).max(self)
    }

    /// `steps` steps down, never below Hostile.
    pub fn down(self, steps: u32) -> Standing {
        let r = (self as u32).saturating_sub(steps);
        match r {
            0 => Standing::Hostile,
            1 => Standing::Neutral,
            2 => Standing::Tied,
            _ => Standing::Allied,
        }
    }
}

impl fmt::Display for Standing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

/// How far a nation takes part in diplomacy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engagement {
    /// Its AI acts and answers by its disposition (the default).
    Full,
    /// Abstains: it takes no action and accepts no offer.
    Abstain,
    /// Its AI answers offers but takes no action of its own.
    Passive,
}

/// One nation's diplomatic disposition (C0's nation sheet).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DispositionDef {
    pub nation: String,
    /// Who it hates or favours at the start: `(nation id, standing)`.
    pub towards: Vec<(String, Standing)>,
    /// Added to its willingness towards every nation.
    pub ally_bias: i32,
    /// How many betrayals it forgives.
    pub betrayal_tolerance: u32,
}

/// The diplomacy rules.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiplomacyRulesDef {
    pub ally_cap: u32,
    pub betrayal_standing_drop: u32,
    pub destabilize_requires_tied: u32,
    pub actions_per_round: u32,
    pub court_cost: u32,
    pub ally_cost: u32,
    pub incite_cost: u32,
    pub court_threshold: i32,
    pub ally_threshold: i32,
    pub notice_per_city: i32,
    pub war_rounds: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct DiplomacyFile {
    default_standing: Standing,
    dispositions: Vec<DispositionDef>,
    rules: DiplomacyRulesDef,
}

/// The diplomacy content, validated, with its dispositions in nation order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiplomacyContent {
    pub default_standing: Standing,
    /// One per nation, in `Content::nations` order.
    pub dispositions: Vec<DispositionDef>,
    pub rules: DiplomacyRulesDef,
    /// `start[a * n + b]`: the pair's standing at the start (symmetric).
    start: Vec<Standing>,
    /// Per nation: how far it takes part (a lever for the balance sim and
    /// tests, never in the file).
    engagement: Vec<Engagement>,
    /// The parsed file's hash (the abstentions are mixed in by
    /// [`fingerprint`](Self::fingerprint)).
    file_fp: u64,
}

/// The shipped diplomacy file's name in the data directory.
pub const DIPLOMACY_FILE: &str = "diplomacy.ron";

impl DiplomacyContent {
    /// Load `diplomacy.ron` from a data directory, validated against `content`.
    pub fn load_from_dir(dir: &Path, content: &Content) -> Result<Self, CampaignError> {
        let path = dir.join(DIPLOMACY_FILE);
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

    /// Load the shipped diplomacy from the crate's own `assets/data`.
    pub fn shipped(content: &Content) -> Result<Self, CampaignError> {
        Self::load_from_dir(
            &std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data"),
            content,
        )
    }

    /// Parse and validate diplomacy RON text against `content`.
    pub fn from_ron(text: &str, content: &Content) -> Result<Self, CampaignError> {
        let opts = ron::Options::default().with_default_extension(Extensions::IMPLICIT_SOME);
        let file: DiplomacyFile = opts.from_str(text).map_err(|e| CampaignError::Parse {
            path: "<diplomacy>".to_string(),
            msg: e.to_string(),
        })?;
        Self::resolve(file, content)
    }

    fn resolve(f: DiplomacyFile, content: &Content) -> Result<Self, CampaignError> {
        let bad = |msg: String| Err(CampaignError::Invalid { msg: format!("diplomacy: {msg}") });
        let n = content.nations.len();
        let nation = |id: &str| content.nations.iter().position(|d| d.id == id);
        if f.default_standing == Standing::Allied {
            return bad("default_standing is Allied: no pair starts Allied (alliances are made)".into());
        }
        let mut slot: Vec<Option<usize>> = vec![None; n];
        for (i, d) in f.dispositions.iter().enumerate() {
            let Some(k) = nation(&d.nation) else {
                return bad(format!("a disposition names unknown nation `{}`", d.nation));
            };
            if slot[k].is_some() {
                return bad(format!("nation `{}` has a disposition twice", d.nation));
            }
            slot[k] = Some(i);
            for (j, (to, s)) in d.towards.iter().enumerate() {
                if nation(to).is_none() {
                    return bad(format!("`{}` towards names unknown nation `{to}`", d.nation));
                }
                if *to == d.nation {
                    return bad(format!("`{}` towards names itself", d.nation));
                }
                if d.towards[..j].iter().any(|(o, _)| o == to) {
                    return bad(format!("`{}` towards names `{to}` twice", d.nation));
                }
                if *s == Standing::Allied {
                    return bad(format!("`{}` starts Allied with `{to}`: alliances are made in play", d.nation));
                }
            }
        }
        let mut order = Vec::with_capacity(n);
        for (k, s) in slot.iter().enumerate() {
            match s {
                Some(i) => order.push(*i),
                None => return bad(format!("nation `{}` has no disposition", content.nations[k].id)),
            }
        }
        let r = &f.rules;
        if r.ally_cap > 1 {
            return bad(format!("ally_cap is {}: C0 caps allies at 1", r.ally_cap));
        }
        if r.betrayal_standing_drop == 0 {
            return bad("betrayal_standing_drop is 0: a betrayal must cost at least one step".into());
        }
        let others = n.saturating_sub(1) as u32;
        if r.destabilize_requires_tied < 2 || r.destabilize_requires_tied > others {
            return bad(format!(
                "destabilize_requires_tied is {}: it incites two nations, so it must be 2..={others}",
                r.destabilize_requires_tied
            ));
        }
        if r.actions_per_round == 0 {
            return bad("actions_per_round is 0".into());
        }
        if r.war_rounds == 0 {
            return bad("war_rounds is 0".into());
        }
        let file_fp = {
            let mut h = Fnv::default();
            h.bytes(format!("{f:?}").as_bytes());
            h.finish()
        };
        let DiplomacyFile {
            default_standing,
            dispositions,
            rules,
        } = f;
        let mut dispositions: Vec<Option<DispositionDef>> = dispositions.into_iter().map(Some).collect();
        let dispositions: Vec<DispositionDef> =
            order.iter().map(|&i| dispositions[i].take().expect("each once")).collect();
        let mut start = vec![default_standing; n * n];
        let mut named = vec![false; n * n];
        for (a, d) in dispositions.iter().enumerate() {
            for (to, s) in &d.towards {
                let b = nation(to).expect("checked");
                for (x, y) in [(a, b), (b, a)] {
                    let k = x * n + y;
                    start[k] = if named[k] { start[k].min(*s) } else { *s };
                    named[k] = true;
                }
            }
        }
        Ok(DiplomacyContent {
            default_standing,
            dispositions,
            rules,
            start,
            engagement: vec![Engagement::Full; n],
            file_fp,
        })
    }

    /// Nations covered (the battle content's).
    pub fn nations(&self) -> usize {
        self.dispositions.len()
    }

    /// Where the pair `a`, `b` starts.
    pub fn start_standing(&self, a: usize, b: usize) -> Standing {
        self.start[a * self.nations() + b]
    }

    pub fn ally_bias(&self, n: usize) -> i32 {
        self.dispositions[n].ally_bias
    }

    pub fn betrayal_tolerance(&self, n: usize) -> u32 {
        self.dispositions[n].betrayal_tolerance
    }

    /// How far nation `n` takes part.
    pub fn engagement(&self, n: usize) -> Engagement {
        self.engagement[n]
    }

    /// Nation `n` takes no diplomatic action and accepts no offer.
    pub fn abstains(&self, n: usize) -> bool {
        self.engagement[n] == Engagement::Abstain
    }

    /// Nation `n`'s AI takes no diplomatic action of its own (it may still
    /// answer offers, unless it abstains).
    pub fn takes_no_initiative(&self, n: usize) -> bool {
        self.engagement[n] != Engagement::Full
    }

    /// The same diplomacy, with nation `n` abstaining (the balance sim's
    /// lever: a nation that does without diplomacy).
    pub fn with_abstaining(mut self, n: usize) -> Self {
        self.engagement[n] = Engagement::Abstain;
        self
    }

    /// The same diplomacy, with nation `n` passive: its AI answers offers but
    /// takes no diplomatic action of its own (a lever for tests).
    pub fn with_passive(mut self, n: usize) -> Self {
        self.engagement[n] = Engagement::Passive;
        self
    }

    /// An FNV hash of every parsed value and the abstentions, never 0.
    pub fn fingerprint(&self) -> u64 {
        let mut h = Fnv::default();
        h.u64(self.file_fp);
        for &e in &self.engagement {
            h.u64(e as u64);
        }
        h.finish()
    }
}

// ---- play -------------------------------------------------------------------

use super::{Campaign, Decision};

/// A war an AI is steered into (C5): through round `until` (the round it
/// began in counts as its first), its armies march on `target`'s capital.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WarTarget {
    pub target: usize,
    pub until: u32,
}

/// A diplomatic action, resolved to nation indices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    Court { with: usize },
    Ally { with: usize },
    Ask { ally: usize, target: usize },
    Betray { ally: usize },
    Incite { a: usize, b: usize },
}

/// One diplomatic action as it went: `accepted` is false when the other
/// nation refused an offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiplomacyRecord {
    pub round: u32,
    pub actor: usize,
    pub act: Act,
    pub accepted: bool,
}

/// What a nation offers another, for its answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Offer {
    Court,
    Ally,
}

/// The campaign's diplomatic state (C5); `None` in a campaign without
/// diplomacy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DiploState {
    n: usize,
    /// `standing[a * n + b]`, kept symmetric.
    standing: Vec<Standing>,
    /// Betrayals each nation has committed.
    betrayals: Vec<u32>,
    /// The war each nation is steered into, if any.
    war: Vec<Option<WarTarget>>,
    /// Every diplomatic action so far, in order.
    log: Vec<DiplomacyRecord>,
    /// Diplomatic actions each nation has taken this round (reset every
    /// round; not campaign state between rounds).
    acted: Vec<u32>,
}

impl DiploState {
    pub(super) fn new(d: &DiplomacyContent) -> Self {
        let n = d.nations();
        DiploState {
            n,
            standing: (0..n * n).map(|k| d.start_standing(k / n, k % n)).collect(),
            betrayals: vec![0; n],
            war: vec![None; n],
            log: Vec::new(),
            acted: vec![0; n],
        }
    }

    /// Into the campaign state hash: every standing, betrayal count and
    /// steered war, and how many actions have been recorded (as the battle
    /// history counts). This round's action count is not hashed: it is
    /// zero between rounds.
    pub(super) fn hash_into(&self, h: &mut Fnv) {
        h.u64(0xD1A0);
        h.u64(self.n as u64);
        for s in &self.standing {
            h.u64(*s as u64);
        }
        for &b in &self.betrayals {
            h.u64(b as u64);
        }
        for w in &self.war {
            match w {
                None => h.u64(0),
                Some(w) => h.u64(1).u64(w.target as u64).u64(w.until as u64),
            };
        }
        h.u64(self.log.len() as u64);
    }

    fn get(&self, a: usize, b: usize) -> Standing {
        self.standing[a * self.n + b]
    }

    fn set(&mut self, a: usize, b: usize, s: Standing) {
        self.standing[a * self.n + b] = s;
        self.standing[b * self.n + a] = s;
    }
}

impl Campaign {
    fn diplomacy_content(&self) -> Option<&DiplomacyContent> {
        self.campaign.diplomacy()
    }

    /// The campaign plays diplomacy (C5): its content took it on.
    pub fn has_diplomacy(&self) -> bool {
        self.diplo.is_some()
    }

    /// The pair's standing (`None` without diplomacy, or for a nation with
    /// itself).
    pub fn standing(&self, a: usize, b: usize) -> Option<Standing> {
        let d = self.diplo.as_ref()?;
        (a != b).then(|| d.get(a, b))
    }

    /// `a` and `b` are allies (both standing).
    pub fn allied(&self, a: usize, b: usize) -> bool {
        a != b
            && self.nations[a].alive
            && self.nations[b].alive
            && self.standing(a, b) == Some(Standing::Allied)
    }

    /// Nation `n`'s allies, ascending index.
    pub fn allies_of(&self, n: usize) -> Vec<usize> {
        (0..self.nations.len()).filter(|&m| self.allied(n, m)).collect()
    }

    /// Nation `n`'s ally, if it has one.
    pub fn ally_of(&self, n: usize) -> Option<usize> {
        self.allies_of(n).first().copied()
    }

    /// How many living nations stand Tied with nation `n`.
    pub fn tied_with(&self, n: usize) -> u32 {
        (0..self.nations.len())
            .filter(|&m| self.nations[m].alive && self.standing(n, m) == Some(Standing::Tied))
            .count() as u32
    }

    /// Betrayals nation `n` has committed.
    pub fn betrayals(&self, n: usize) -> u32 {
        self.diplo.as_ref().map_or(0, |d| d.betrayals[n])
    }

    /// The war nation `n` is steered into this round, if any.
    pub fn war_target(&self, n: usize) -> Option<WarTarget> {
        let w = self.diplo.as_ref()?.war[n]?;
        (w.until >= self.round && self.nations[w.target].alive).then_some(w)
    }

    /// Every diplomatic action so far, in order.
    pub fn diplomacy_log(&self) -> &[DiplomacyRecord] {
        self.diplo.as_ref().map_or(&[], |d| &d.log)
    }

    /// Nation `n`'s willingness towards `m` (C5): its `ally_bias`, plus their
    /// standing's rank, less `notice_per_city` for every city `m` holds above
    /// `n`'s own (C0's notice). `None` without diplomacy.
    pub fn willingness(&self, n: usize, m: usize) -> Option<i32> {
        let dc = self.diplomacy_content()?;
        let s = self.standing(n, m)?;
        let above = self.cities_of(m).len().saturating_sub(self.cities_of(n).len()) as i32;
        Some(dc.ally_bias(n) + s.rank() - dc.rules.notice_per_city * above)
    }

    /// Would nation `n` accept `offer` from `from`? An abstaining nation,
    /// and the player (who has no answer in C5), refuse everything; so does
    /// a nation `from` has betrayed more often than it forgives
    /// (`betrayal_tolerance`).
    pub fn accepts(&self, n: usize, from: usize, offer: Offer) -> bool {
        let Some(dc) = self.diplomacy_content() else { return false };
        if n == from || Some(n) == self.player || dc.abstains(n) || !self.nations[n].alive {
            return false;
        }
        if self.betrayals(from) > dc.betrayal_tolerance(n) {
            return false;
        }
        let Some(w) = self.willingness(n, from) else { return false };
        match offer {
            Offer::Court => w >= dc.rules.court_threshold,
            Offer::Ally => {
                self.standing(n, from) == Some(Standing::Tied)
                    && (self.allies_of(n).len() as u32) < dc.rules.ally_cap
                    && w >= dc.rules.ally_threshold
            }
        }
    }

    /// A nation other than `actor`, by id, still in the campaign.
    fn other_living(&self, actor: usize, id: &str) -> Result<usize, String> {
        let m = self.nation_index(id).ok_or_else(|| format!("unknown nation `{id}`"))?;
        if m == actor {
            return Err(format!("`{id}` is this nation"));
        }
        if !self.nations[m].alive {
            return Err(format!("`{id}` is out of the campaign"));
        }
        Ok(m)
    }

    fn pay(&self, n: usize, cost: u32, what: &str) -> Result<(), String> {
        let t = self.nations[n].treasury;
        if t < cost {
            return Err(format!("the treasury holds {t} Alloy; {what} costs {cost}"));
        }
        Ok(())
    }

    fn record(&mut self, actor: usize, act: Act, accepted: bool) {
        let round = self.round;
        let d = self.diplo.as_mut().expect("diplomacy");
        d.acted[actor] += 1;
        d.log.push(DiplomacyRecord { round, actor, act, accepted });
    }

    /// Carry out a diplomatic decision of `nation`'s (C5). An offer the
    /// other nation refuses is not an error: it is recorded, it uses the
    /// nation's action, and it costs nothing.
    pub(super) fn apply_diplomacy(&mut self, nation: usize, d: &Decision) -> Result<(), String> {
        let Some(dc) = self.campaign.diplomacy().cloned() else {
            return Err("this campaign has no diplomacy (diplomacy.ron is not attached)".to_string());
        };
        let r = &dc.rules;
        let acted = self.diplo.as_ref().expect("diplomacy").acted[nation];
        if acted >= r.actions_per_round {
            return Err(format!(
                "this nation has taken its {} diplomatic action(s) this round",
                r.actions_per_round
            ));
        }
        match d {
            Decision::Court { nation: id } => {
                let m = self.other_living(nation, id)?;
                let s = self.standing(nation, m).expect("diplomacy");
                if s >= Standing::Tied {
                    return Err(format!("already {s} with `{id}`: courting reaches Tied at most"));
                }
                self.pay(nation, r.court_cost, "courting")?;
                let yes = self.accepts(m, nation, Offer::Court);
                if yes {
                    self.nations[nation].treasury -= r.court_cost;
                    self.diplo.as_mut().expect("diplomacy").set(nation, m, s.up(Standing::Tied));
                }
                self.record(nation, Act::Court { with: m }, yes);
                Ok(())
            }
            Decision::Ally { nation: id } => {
                let m = self.other_living(nation, id)?;
                let s = self.standing(nation, m).expect("diplomacy");
                if s != Standing::Tied {
                    return Err(format!("{s} with `{id}`: an alliance needs Tied standing"));
                }
                let mine = self.allies_of(nation).len() as u32;
                if mine >= r.ally_cap {
                    return Err(format!("this nation has {mine} ally(ies), at the cap of {}", r.ally_cap));
                }
                self.pay(nation, r.ally_cost, "an alliance")?;
                let yes = self.accepts(m, nation, Offer::Ally);
                if yes {
                    self.nations[nation].treasury -= r.ally_cost;
                    let st = self.diplo.as_mut().expect("diplomacy");
                    st.set(nation, m, Standing::Allied);
                    // Allies are not steered into war with each other.
                    for (x, y) in [(nation, m), (m, nation)] {
                        if st.war[x].is_some_and(|w| w.target == y) {
                            st.war[x] = None;
                        }
                    }
                }
                self.record(nation, Act::Ally { with: m }, yes);
                Ok(())
            }
            Decision::Ask { target: id } => {
                let Some(ally) = self.ally_of(nation) else {
                    return Err("this nation has no ally to ask".to_string());
                };
                let t = self.other_living(nation, id)?;
                if t == ally {
                    return Err(format!("`{id}` is this nation's ally"));
                }
                if self.allied(t, ally) || self.allied(t, nation) {
                    return Err(format!("`{id}` is allied with one of the two: not a shared enemy"));
                }
                if Some(ally) != self.player {
                    // Steered for `war_rounds` rounds, this one counted.
                    let until = self.round.saturating_add(r.war_rounds - 1);
                    self.diplo.as_mut().expect("diplomacy").war[ally] = Some(WarTarget { target: t, until });
                }
                self.record(nation, Act::Ask { ally, target: t }, true);
                Ok(())
            }
            Decision::Betray { nation: id } => {
                let m = self.other_living(nation, id)?;
                if !self.allied(nation, m) {
                    return Err(format!("`{id}` is not this nation's ally"));
                }
                let alive: Vec<bool> = self.nations.iter().map(|n| n.alive).collect();
                let st = self.diplo.as_mut().expect("diplomacy");
                st.set(nation, m, Standing::Hostile);
                // Everyone else learns of it.
                for (o, &alive) in alive.iter().enumerate() {
                    if alive && o != nation && o != m {
                        let s = st.get(o, nation);
                        st.set(o, nation, s.down(r.betrayal_standing_drop));
                    }
                }
                st.betrayals[nation] += 1;
                // Its armies standing in the old ally's cities are now
                // hostile there, and assault them in this round's battles.
                self.record(nation, Act::Betray { ally: m }, true);
                Ok(())
            }
            Decision::Incite { a, b } => {
                let x = self.other_living(nation, a)?;
                let y = self.other_living(nation, b)?;
                if x == y {
                    return Err("inciting needs two different nations".to_string());
                }
                let tied = self.tied_with(nation);
                let need = r.destabilize_requires_tied;
                if tied < need {
                    return Err(format!("Tied with {tied} nation(s): inciting needs Tied with {need}"));
                }
                for (m, id) in [(x, a), (y, b)] {
                    let s = self.standing(nation, m).expect("diplomacy");
                    if s != Standing::Tied {
                        return Err(format!("{s} with `{id}`: only a Tied nation can be incited"));
                    }
                }
                if self.allied(x, y) {
                    return Err(format!("`{a}` and `{b}` are allies"));
                }
                self.pay(nation, r.incite_cost, "inciting")?;
                self.nations[nation].treasury -= r.incite_cost;
                let until = self.round.saturating_add(r.war_rounds - 1);
                let player = self.player;
                let st = self.diplo.as_mut().expect("diplomacy");
                st.set(x, y, Standing::Hostile);
                for (m, t) in [(x, y), (y, x)] {
                    if Some(m) != player {
                        st.war[m] = Some(WarTarget { target: t, until });
                    }
                }
                self.record(nation, Act::Incite { a: x, b: y }, true);
                Ok(())
            }
            Decision::March { .. } | Decision::Raise { .. } => unreachable!("not diplomatic"),
        }
    }

    /// The start of a round: no nation has acted yet.
    pub(super) fn diplomacy_new_round(&mut self) {
        if let Some(d) = self.diplo.as_mut() {
            d.acted.iter_mut().for_each(|a| *a = 0);
        }
    }

    /// This round's diplomatic records.
    pub(super) fn diplomacy_this_round(&self) -> Vec<DiplomacyRecord> {
        let round = self.round;
        self.diplomacy_log().iter().filter(|r| r.round == round).copied().collect()
    }

    /// The end of a round: a steered war that has run its rounds, or whose
    /// target has fallen, is over.
    pub(super) fn diplomacy_end_round(&mut self) {
        let round = self.round;
        let alive: Vec<bool> = self.nations.iter().map(|n| n.alive).collect();
        if let Some(d) = self.diplo.as_mut() {
            for w in d.war.iter_mut() {
                if w.is_some_and(|w| w.until <= round || !alive[w.target]) {
                    *w = None;
                }
            }
        }
    }

    /// `fallen` is out of the campaign: its alliance ends (the pair drops to
    /// Tied, at no cost), and no war is steered by it or at it.
    pub(super) fn diplomacy_fallen(&mut self, fallen: usize) {
        if let Some(d) = self.diplo.as_mut() {
            for m in 0..d.n {
                if m != fallen && d.get(fallen, m) == Standing::Allied {
                    d.set(fallen, m, Standing::Tied);
                }
            }
            d.war[fallen] = None;
            for w in d.war.iter_mut() {
                if w.is_some_and(|w| w.target == fallen) {
                    *w = None;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    //! C5 AC6: the diplomatic AI's fixed rules, in their fixed order, on
    //! hand-set standings. (Aetherkin decides; the shipped map; nobody
    //! plays unless named.)
    use super::super::ai::decide_diplomacy;
    use super::super::{Campaign, CampaignContent, Decision};
    use super::*;

    const HOUSE: usize = 0;
    const FORGE: usize = 1;
    const VERDANT: usize = 2;
    const AETH: usize = 3;
    const TOLL: usize = 4;
    const ASH: usize = 5;

    fn game(player: Option<&str>, edit: impl Fn(DiplomacyContent) -> DiplomacyContent) -> Campaign {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data");
        let content = Content::load_from_dir(&dir).unwrap();
        let d = edit(DiplomacyContent::shipped(&content).unwrap());
        let cc = CampaignContent::shipped(&content).unwrap().with_diplomacy(d);
        let mut c = Campaign::new(content, cc, 1, player).unwrap();
        for n in 0..c.nations.len() {
            c.nations[n].treasury = 1000;
        }
        assert_eq!(c.nation_index("aetherkin"), Some(AETH));
        c
    }
    fn set(c: &mut Campaign, a: usize, b: usize, s: Standing) {
        c.diplo.as_mut().unwrap().set(a, b, s);
    }
    fn id(n: usize) -> String {
        ["house", "forgeborn", "verdant", "aetherkin", "tollmarch", "ashen_reach"][n].to_string()
    }
    fn decide(c: &Campaign) -> Option<Decision> {
        decide_diplomacy(c, AETH)
    }

    #[test]
    fn it_courts_the_most_willing_nation_that_would_accept() {
        let c = game(None, |d| d);
        // Aetherkin (3 cities, bias 1) towards Tollmarch and the Reach (2
        // each) is 2, towards House 2; House (1 city) would refuse it.
        assert_eq!(c.willingness(AETH, TOLL), Some(2));
        assert!(!c.accepts(HOUSE, AETH, Offer::Court) && c.accepts(TOLL, AETH, Offer::Court));
        assert_eq!(decide(&c), Some(Decision::Court { nation: id(TOLL) }), "ties to the lower index");
    }

    #[test]
    fn it_takes_no_action_it_cannot_pay_for() {
        let mut c = game(None, |d| d);
        c.nations[AETH].treasury = c.campaign.diplomacy().unwrap().rules.court_cost - 1;
        assert_eq!(decide(&c), None);
    }

    #[test]
    fn an_alliance_comes_before_a_courting() {
        let mut c = game(None, |d| d);
        set(&mut c, AETH, VERDANT, Standing::Tied); // willing 2: below ally_threshold
        assert_eq!(decide(&c), Some(Decision::Court { nation: id(TOLL) }));
        set(&mut c, AETH, TOLL, Standing::Tied); // willing 3, and Tollmarch 3
        assert_eq!(decide(&c), Some(Decision::Ally { nation: id(TOLL) }));
    }

    #[test]
    fn inciting_comes_before_a_courting() {
        let mut c = game(None, |d| d);
        set(&mut c, AETH, FORGE, Standing::Tied);
        set(&mut c, AETH, VERDANT, Standing::Tied);
        assert_eq!(c.willingness(AETH, FORGE), Some(2), "too low for an alliance");
        assert_eq!(decide(&c), Some(Decision::Incite { a: id(FORGE), b: id(VERDANT) }));
        // Not a pair already steered.
        c.diplo.as_mut().unwrap().war[FORGE] = Some(WarTarget { target: HOUSE, until: 9 });
        assert_eq!(decide(&c), Some(Decision::Court { nation: id(TOLL) }));
    }

    #[test]
    fn with_an_ally_it_asks_it_to_attack_the_nearest_enemy_capital() {
        let mut c = game(None, |d| d);
        set(&mut c, AETH, TOLL, Standing::Allied);
        // From Leyhold, Cinderhold is the nearest capital.
        assert_eq!(decide(&c), Some(Decision::Ask { target: id(HOUSE) }));
        // An ally already at war is not asked again: Aetherkin courts.
        c.diplo.as_mut().unwrap().war[TOLL] = Some(WarTarget { target: FORGE, until: 9 });
        assert_eq!(decide(&c), Some(Decision::Court { nation: id(ASH) }));
    }

    #[test]
    fn it_betrays_its_ally_once_that_ally_is_its_last_rival() {
        let mut c = game(None, |d| d);
        set(&mut c, AETH, TOLL, Standing::Allied);
        for n in [HOUSE, FORGE, VERDANT] {
            c.nations[n].alive = false;
        }
        assert_eq!(decide(&c), Some(Decision::Ask { target: id(ASH) }), "the Reach still stands");
        c.nations[ASH].alive = false;
        assert_eq!(decide(&c), Some(Decision::Betray { nation: id(TOLL) }));
    }

    #[test]
    fn it_never_makes_an_offer_to_the_player() {
        let only_tollmarch_left = |player| {
            let mut c = game(player, |d| d);
            for n in [HOUSE, FORGE, VERDANT, ASH] {
                c.nations[n].alive = false;
            }
            c
        };
        // Tollmarch, as an AI, would be courted and then allied.
        let mut ai = only_tollmarch_left(None);
        assert_eq!(decide(&ai), Some(Decision::Court { nation: id(TOLL) }));
        set(&mut ai, AETH, TOLL, Standing::Tied);
        assert_eq!(decide(&ai), Some(Decision::Ally { nation: id(TOLL) }));
        // As the player, it is offered nothing.
        let mut c = only_tollmarch_left(Some("tollmarch"));
        assert_eq!(decide(&c), None, "no courting of the player");
        set(&mut c, AETH, TOLL, Standing::Tied);
        assert_eq!(c.willingness(AETH, TOLL), Some(3));
        assert_eq!(decide(&c), None, "no alliance offered to the player");
    }

    #[test]
    fn a_passive_or_abstaining_nation_takes_no_action() {
        assert_eq!(decide(&game(None, |d| d.with_passive(AETH))), None);
        assert_eq!(decide(&game(None, |d| d.with_abstaining(AETH))), None);
        // ... and an abstaining nation is not courted.
        let c = game(None, |d| d.with_abstaining(TOLL));
        assert_eq!(decide(&c), Some(Decision::Court { nation: id(ASH) }));
    }

    #[test]
    fn a_pair_of_allies_is_neither_incited_by_the_ai_nor_inciteable() {
        let mut c = game(None, |d| d);
        set(&mut c, AETH, FORGE, Standing::Tied);
        set(&mut c, AETH, VERDANT, Standing::Tied);
        set(&mut c, FORGE, VERDANT, Standing::Allied);
        assert_eq!(decide(&c), Some(Decision::Court { nation: id(TOLL) }));
        let incite = Decision::Incite { a: id(FORGE), b: id(VERDANT) };
        let why = c.apply_diplomacy(AETH, &incite).unwrap_err();
        assert!(why.contains("allies"), "{why}");
    }

    /// C5 AC7: each piece of diplomatic state moves the hash alone.
    #[test]
    fn standings_betrayals_and_wars_are_in_the_state_hash() {
        let c = game(None, |d| d);
        let h = c.state_hash();
        let mut s = c.clone();
        set(&mut s, AETH, TOLL, Standing::Tied);
        assert_ne!(s.state_hash(), h, "a standing");
        let mut b = c.clone();
        b.diplo.as_mut().unwrap().betrayals[AETH] = 1;
        assert_ne!(b.state_hash(), h, "a betrayal count");
        let mut w = c.clone();
        w.diplo.as_mut().unwrap().war[AETH] = Some(WarTarget { target: TOLL, until: 3 });
        assert_ne!(w.state_hash(), h, "a war");
        let mut w2 = w.clone();
        w2.diplo.as_mut().unwrap().war[AETH] = Some(WarTarget { target: TOLL, until: 4 });
        assert_ne!(w2.state_hash(), w.state_hash(), "a war's length");
        let mut l = c.clone();
        let rec = DiplomacyRecord { round: 0, actor: AETH, act: Act::Court { with: TOLL }, accepted: false };
        l.diplo.as_mut().unwrap().log.push(rec);
        assert_ne!(l.state_hash(), h, "the record of actions");
        // What a nation did this round is not campaign state between rounds.
        let mut a = c.clone();
        a.diplo.as_mut().unwrap().acted[AETH] = 1;
        assert_eq!(a.state_hash(), h);
    }

    #[test]
    fn without_diplomacy_there_is_no_diplomatic_ai() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data");
        let content = Content::load_from_dir(&dir).unwrap();
        let cc = CampaignContent::shipped(&content).unwrap();
        let mut c = Campaign::new(content, cc, 1, None).unwrap();
        c.nations[AETH].treasury = 1000;
        assert_eq!(decide(&c), None);
    }
}
