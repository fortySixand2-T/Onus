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
    /// Per nation: it takes no diplomatic action and accepts no offer (the
    /// balance sim's lever, never in the file).
    abstain: Vec<bool>,
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
            abstain: vec![false; n],
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

    /// Nation `n` takes no diplomatic action and accepts no offer.
    pub fn abstains(&self, n: usize) -> bool {
        self.abstain[n]
    }

    /// The same diplomacy, with nation `n` abstaining (the balance sim's
    /// lever: a nation that does without diplomacy).
    pub fn with_abstaining(mut self, n: usize) -> Self {
        self.abstain[n] = true;
        self
    }

    /// An FNV hash of every parsed value and the abstentions, never 0.
    pub fn fingerprint(&self) -> u64 {
        let mut h = Fnv::default();
        h.u64(self.file_fp);
        for &a in &self.abstain {
            h.u64(a as u64);
        }
        h.finish()
    }
}
