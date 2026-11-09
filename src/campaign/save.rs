//! A campaign save (C4 AC11): the seed plus the log of the player's
//! decisions, round by round, on M5's replay pattern. The AI's decisions and
//! every battle are recomputed (the campaign is deterministic), so a save
//! is small and a load is a replay.
//!
//! As with a match log ([`crate::sim::replay::MatchLog`]):
//! - the format version is checked at both boundaries: writing refuses a
//!   version this build does not produce, reading one it does not
//!   understand;
//! - the content is stamped: the battle content's fingerprint and the
//!   campaign content's. A save is only meaningful against the content it
//!   was played with, so another content is refused, loudly, before any
//!   round is replayed;
//! - the per-round state hashes are kept, and a load checks every round
//!   against them, stopping at the first that diverges.

use serde::{Deserialize, Serialize};

use crate::sim::content::{Content, ContentFingerprint};

use super::battle::Resolver;
use super::content::CampaignContent;
use super::{Campaign, Decision};

/// The save format this build writes and reads.
pub const CAMPAIGN_LOG_VERSION: u32 = 1;

/// A saved campaign: everything needed to play it again to the round it
/// was saved at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CampaignLog {
    pub version: u32,
    pub seed: u64,
    /// The nation the player plays, by id (`None`: an AI-only campaign).
    pub player: Option<String>,
    /// The battle content it was played with.
    pub content: ContentFingerprint,
    /// The campaign content's fingerprint (`CampaignContent::fingerprint`).
    pub campaign: u64,
    /// The player's decisions, one list per round played (empty when he
    /// held, and always for an AI-only campaign).
    pub rounds: Vec<Vec<Decision>>,
    /// The state hash after each round: the replay is checked against them.
    pub hashes: Vec<u64>,
}

impl CampaignLog {
    /// One predicate for both boundaries: a round count and a hash count
    /// that agree.
    pub fn validate(&self) -> Result<(), String> {
        if self.rounds.len() != self.hashes.len() {
            return Err(format!(
                "campaign save: {} rounds of decisions but {} round hashes",
                self.rounds.len(),
                self.hashes.len()
            ));
        }
        if !self.content.is_known() {
            return Err("campaign save: no content was stamped on it".to_string());
        }
        Ok(())
    }

    /// Serialize to RON. Refuses what [`from_ron`](Self::from_ron) refuses.
    pub fn to_ron(&self) -> Result<String, String> {
        if self.version != CAMPAIGN_LOG_VERSION {
            return Err(format!(
                "campaign save: refusing to write format version {} (this build writes \
                 {CAMPAIGN_LOG_VERSION})",
                self.version
            ));
        }
        self.validate()?;
        ron::ser::to_string_pretty(self, ron::ser::PrettyConfig::default())
            .map_err(|e| format!("campaign save: serialize: {e}"))
    }

    pub fn from_ron(text: &str) -> Result<Self, String> {
        let log: CampaignLog = ron::from_str(text).map_err(|e| format!("campaign save: parse: {e}"))?;
        if log.version != CAMPAIGN_LOG_VERSION {
            return Err(format!(
                "campaign save: format version {} is not {CAMPAIGN_LOG_VERSION}",
                log.version
            ));
        }
        log.validate()?;
        Ok(log)
    }

    /// Write to `path`.
    pub fn save(&self, path: &std::path::Path) -> Result<(), String> {
        let text = self.to_ron()?;
        std::fs::write(path, text).map_err(|e| format!("campaign save: write {}: {e}", path.display()))
    }

    /// Read from `path` (format checks only; [`matches`](Self::matches) is
    /// the content check, and [`Campaign::resume`] runs both).
    pub fn load(path: &std::path::Path) -> Result<Self, String> {
        let text =
            std::fs::read_to_string(path).map_err(|e| format!("campaign save: read {}: {e}", path.display()))?;
        Self::from_ron(&text)
    }

    /// Was this save played with `content` and `campaign`? The fingerprints
    /// are compared; the summary only tells a human what changed.
    pub fn matches(&self, content: &Content, campaign: &CampaignContent) -> Result<(), String> {
        let mine = content.fingerprint();
        if self.content.hash() != mine.hash() {
            return Err(format!(
                "campaign save: played with different battle content: the save has [{}], this \
                 build has [{}]. Its battles would not be the ones it recorded, so it is refused",
                self.content, mine
            ));
        }
        if self.campaign != campaign.fingerprint() {
            return Err(format!(
                "campaign save: played with different campaign content (save {:#018x}, this build \
                 {:#018x}), so it is refused",
                self.campaign,
                campaign.fingerprint()
            ));
        }
        Ok(())
    }
}

impl Campaign {
    /// This campaign's save: its seed, its player and his decisions so far.
    pub fn log(&self) -> CampaignLog {
        CampaignLog {
            version: CAMPAIGN_LOG_VERSION,
            seed: self.seed,
            player: self.player.map(|p| self.content.nations[p].id.clone()),
            content: self.content.fingerprint(),
            campaign: self.campaign.fingerprint(),
            rounds: self.decisions.clone(),
            hashes: self.round_hashes.clone(),
        }
    }

    /// Load a save: refuse another content or format, then replay every
    /// round with its logged decisions on `resolver`, checking each round's
    /// state hash; the first round that diverges stops the load.
    pub fn resume(
        content: Content,
        campaign: CampaignContent,
        log: &CampaignLog,
        resolver: &mut dyn Resolver,
    ) -> Result<Campaign, String> {
        if log.version != CAMPAIGN_LOG_VERSION {
            return Err(format!(
                "campaign save: format version {} is not {CAMPAIGN_LOG_VERSION}",
                log.version
            ));
        }
        log.validate()?;
        log.matches(&content, &campaign)?;
        let mut c = Campaign::new(content, campaign, log.seed, log.player.as_deref())
            .map_err(|e| format!("campaign save: {e}"))?;
        for (i, (ds, &want)) in log.rounds.iter().zip(&log.hashes).enumerate() {
            c.play_round(ds, resolver)
                .map_err(|e| format!("campaign save: round {} refused on replay: {e}", i + 1))?;
            let got = c.state_hash();
            if got != want {
                return Err(format!(
                    "campaign save: round {} diverged on replay (hash {got:#018x}, the save has \
                     {want:#018x})",
                    i + 1
                ));
            }
        }
        Ok(c)
    }
}
