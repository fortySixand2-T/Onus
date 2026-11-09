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
    /// and the player (who has no answer in C5), refuse everything.
    pub fn accepts(&self, n: usize, from: usize, offer: Offer) -> bool {
        let Some(dc) = self.diplomacy_content() else { return false };
        if n == from || Some(n) == self.player || dc.abstains(n) || !self.nations[n].alive {
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
            Decision::Betray { .. } | Decision::Incite { .. } => Err("not played yet".to_string()),
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
