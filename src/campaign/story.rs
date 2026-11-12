//! The campaign's story (C6 AC1): `assets/data/story.ron`, parsed and
//! validated against the battle and campaign content. **Render-free**: plain
//! file IO and serde; the client shows the beats, this module only says
//! which fire.
//!
//! A beat fires once, the first time its trigger holds, read off the
//! campaign's public state after a round (the start beat before round 1);
//! beats that fire together fire in file order. Reading the story changes
//! nothing: it is not campaign state, it is not hashed, and a save does not
//! carry it (it is recomputed from the campaign).
//!
//! The triggers (F-099 S11):
//! - `Start`: the Rupture, before round 1.
//! - `CapitalTaken(nation)`: the player's capture absorbed `nation` (its
//!   capital fell to him), read from the battle history.
//! - `AnyCapitalTaken`: the same, for any nation no other beat names.
//! - `RankReached(r)`: a General of the player's first holds rank `r`.
//! - `Won`: the player won the campaign (domination or diplomacy).
//! - `Lost`: the player lost his capital.

use std::fmt;
use std::path::Path;

use ron::extensions::Extensions;
use serde::Deserialize;

use crate::sim::content::Content;

use super::content::CampaignError;
use super::{Campaign, CampaignContent, CampaignOutcome};

/// The shipped story file's name in the data directory.
pub const STORY_FILE: &str = "story.ron";

/// A trigger as the file names it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
enum TriggerDef {
    Start,
    CapitalTaken(String),
    AnyCapitalTaken,
    RankReached(u32),
    Won,
    Lost,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct BeatDef {
    id: String,
    trigger: TriggerDef,
    title: String,
    text: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoryFile {
    beats: Vec<BeatDef>,
}

/// When a beat fires, resolved to indices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// The Rupture: before round 1.
    Start,
    /// The player's capture absorbed this nation (a `Content::nations`
    /// index).
    CapitalTaken(usize),
    /// The player's capture absorbed a nation no other beat names.
    AnyCapitalTaken,
    /// A General of the player's first holds this rank.
    RankReached(usize),
    /// The player won.
    Won,
    /// The player lost.
    Lost,
}

impl fmt::Display for Trigger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Trigger::Start => write!(f, "Start"),
            Trigger::CapitalTaken(n) => write!(f, "CapitalTaken({n})"),
            Trigger::AnyCapitalTaken => write!(f, "AnyCapitalTaken"),
            Trigger::RankReached(r) => write!(f, "RankReached({r})"),
            Trigger::Won => write!(f, "Won"),
            Trigger::Lost => write!(f, "Lost"),
        }
    }
}

/// One story beat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Beat {
    pub id: String,
    pub trigger: Trigger,
    pub title: String,
    pub text: String,
}

/// The story: its beats, in file order.
#[derive(Debug, Clone)]
pub struct Story {
    beats: Vec<Beat>,
}

impl Story {
    /// Load `story.ron` from a data directory, validated.
    pub fn load_from_dir(
        dir: &Path,
        content: &Content,
        campaign: &CampaignContent,
    ) -> Result<Self, CampaignError> {
        let path = dir.join(STORY_FILE);
        let text = std::fs::read_to_string(&path).map_err(|e| CampaignError::Io {
            path: path.display().to_string(),
            msg: e.to_string(),
        })?;
        Self::from_ron(&text, content, campaign).map_err(|e| match e {
            CampaignError::Parse { msg, .. } => CampaignError::Parse {
                path: path.display().to_string(),
                msg,
            },
            other => other,
        })
    }

    /// Load the shipped story from the crate's own `assets/data`.
    pub fn shipped(content: &Content, campaign: &CampaignContent) -> Result<Self, CampaignError> {
        Self::load_from_dir(
            &std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data"),
            content,
            campaign,
        )
    }

    /// Parse and validate story RON text.
    pub fn from_ron(
        text: &str,
        content: &Content,
        campaign: &CampaignContent,
    ) -> Result<Self, CampaignError> {
        let opts = ron::Options::default().with_default_extension(Extensions::IMPLICIT_SOME);
        let file: StoryFile = opts.from_str(text).map_err(|e| CampaignError::Parse {
            path: "<story>".to_string(),
            msg: e.to_string(),
        })?;
        Self::resolve(file, content, campaign)
    }

    fn resolve(
        f: StoryFile,
        content: &Content,
        campaign: &CampaignContent,
    ) -> Result<Self, CampaignError> {
        let bad = |msg: String| Err(CampaignError::Invalid { msg: format!("story: {msg}") });
        let mut beats: Vec<Beat> = Vec::with_capacity(f.beats.len());
        for b in f.beats {
            if b.id.trim().is_empty() {
                return bad("a beat has an empty id".to_string());
            }
            if beats.iter().any(|o| o.id == b.id) {
                return bad(format!("beat id `{}` is repeated", b.id));
            }
            if b.title.trim().is_empty() {
                return bad(format!("beat `{}` has an empty title", b.id));
            }
            if b.text.trim().is_empty() {
                return bad(format!("beat `{}` has an empty text", b.id));
            }
            let trigger = match &b.trigger {
                TriggerDef::Start => Trigger::Start,
                TriggerDef::Won => Trigger::Won,
                TriggerDef::Lost => Trigger::Lost,
                TriggerDef::AnyCapitalTaken => Trigger::AnyCapitalTaken,
                TriggerDef::CapitalTaken(n) => match content.nations.iter().position(|x| &x.id == n) {
                    Some(i) => Trigger::CapitalTaken(i),
                    None => return bad(format!("beat `{}` names an unknown nation `{n}`", b.id)),
                },
                TriggerDef::RankReached(r) => {
                    let r = *r as usize;
                    if r == 0 {
                        return bad(format!("beat `{}`: rank 0 is where every General starts", b.id));
                    }
                    if r >= campaign.ranks.len() {
                        return bad(format!(
                            "beat `{}`: rank {r} does not exist (the campaign has ranks 0..{})",
                            b.id,
                            campaign.ranks.len() - 1
                        ));
                    }
                    Trigger::RankReached(r)
                }
            };
            if let Some(o) = beats.iter().find(|o| o.trigger == trigger) {
                return bad(format!("beats `{}` and `{}` share the trigger {trigger}", o.id, b.id));
            }
            beats.push(Beat {
                id: b.id,
                trigger,
                title: b.title,
                text: b.text,
            });
        }
        let count = |f: &dyn Fn(&Trigger) -> bool| beats.iter().filter(|b| f(&b.trigger)).count();
        if count(&|t| *t == Trigger::Start) != 1 {
            return bad("exactly one beat must open the campaign (Start)".to_string());
        }
        if count(&|t| *t == Trigger::Won) != 1 {
            return bad("exactly one beat must close a won campaign (Won)".to_string());
        }
        if count(&|t| matches!(t, Trigger::CapitalTaken(_) | Trigger::AnyCapitalTaken)) == 0 {
            return bad("no vengeance beat (CapitalTaken or AnyCapitalTaken)".to_string());
        }
        if count(&|t| matches!(t, Trigger::RankReached(_))) == 0 {
            return bad("no glory beat (RankReached)".to_string());
        }
        Ok(Story { beats })
    }

    /// The beats, in file order.
    pub fn beats(&self) -> &[Beat] {
        &self.beats
    }

    /// The beat with `trigger`, if any.
    pub fn beat_for(&self, trigger: Trigger) -> Option<usize> {
        self.beats.iter().position(|b| b.trigger == trigger)
    }

    /// Does `trigger` hold for `campaign` now?
    fn holds(&self, trigger: Trigger, campaign: &Campaign) -> bool {
        let Some(player) = campaign.player() else {
            return trigger == Trigger::Start;
        };
        let took = |nation: usize| {
            campaign
                .history()
                .iter()
                .any(|b| b.absorbed && b.attacker == player && b.defender == nation)
        };
        match trigger {
            Trigger::Start => true,
            Trigger::CapitalTaken(n) => took(n),
            Trigger::AnyCapitalTaken => (0..campaign.nations().len())
                .filter(|&n| self.beat_for(Trigger::CapitalTaken(n)).is_none())
                .any(took),
            Trigger::RankReached(r) => campaign
                .armies()
                .iter()
                .filter(|a| a.nation == player && a.general.present())
                .any(|a| campaign.campaign().rank_of(a.general.xp) >= r),
            Trigger::Won => campaign.outcome() == Some(CampaignOutcome::Won { nation: player }),
            Trigger::Lost => campaign.outcome() == Some(CampaignOutcome::PlayerLost),
        }
    }
}

/// Which beats have fired: client-side bookkeeping, never campaign state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoryProgress {
    fired: Vec<bool>,
}

impl StoryProgress {
    /// Nothing fired yet.
    pub fn new(story: &Story) -> Self {
        StoryProgress {
            fired: vec![false; story.beats.len()],
        }
    }

    /// Per beat, has it fired.
    pub fn fired(&self) -> &[bool] {
        &self.fired
    }

    /// Fire every beat whose trigger now holds and has not fired, in file
    /// order; returns their indices.
    pub fn advance(&mut self, story: &Story, campaign: &Campaign) -> Vec<usize> {
        let mut now = Vec::new();
        for (k, b) in story.beats.iter().enumerate() {
            if !self.fired[k] && story.holds(b.trigger, campaign) {
                self.fired[k] = true;
                now.push(k);
            }
        }
        now
    }
}
