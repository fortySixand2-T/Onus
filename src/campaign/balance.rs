//! The campaign balance sim (C4 AC12): balance-as-backtesting one layer up.
//! Many AI-only campaigns are played to their end, and two things are read:
//! does **each nation win sometimes**, and does a campaign's **length** land
//! on the C0 target (`rules.length_target`, 15-25 rounds)?
//!
//! The readings (F-089, provisional):
//! - a campaign's length is the round it ended on; an undecided one (the
//!   round cap came first) counts at the cap, so it pulls the reading long,
//!   not out of it;
//! - "lands on the target" is the **median** length inside the target;
//! - "wins sometimes" is at least one won campaign per nation.
//!
//! Deterministic: a campaign is a pure function of its seed, and a batch
//! reports its campaigns in seed order however many threads played them.

use crate::sim::content::Content;

use super::battle::Resolver;
use super::content::CampaignContent;
use super::{Campaign, CampaignOutcome};

/// How one AI-only campaign ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CampaignResult {
    pub seed: u64,
    /// The winner (`None`: undecided at the round cap).
    pub winner: Option<usize>,
    /// The round it ended on.
    pub rounds: u32,
}

/// Play one AI-only campaign on `seed` to its end.
pub fn play_ai_campaign(
    content: &Content,
    campaign: &CampaignContent,
    seed: u64,
    resolver: &mut dyn Resolver,
) -> Result<CampaignResult, String> {
    let mut c = Campaign::new(content.clone(), campaign.clone(), seed, None).map_err(|e| e.to_string())?;
    while !c.is_over() {
        c.play_round(&[], resolver).map_err(|e| format!("seed {seed}: {e}"))?;
    }
    let winner = match c.outcome() {
        Some(CampaignOutcome::Won { nation }) => Some(nation),
        _ => None,
    };
    Ok(CampaignResult { seed, winner, rounds: c.round() })
}

/// A batch of AI-only campaigns, in seed order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BalanceReport {
    /// The nations, by id, in content order.
    pub nations: Vec<String>,
    pub results: Vec<CampaignResult>,
    /// The C0 length target and the round cap the batch was played under.
    pub min_rounds: u32,
    pub max_rounds: u32,
    pub round_cap: u32,
}

/// One campaign's result, or why it could not be played.
type Played = Result<CampaignResult, String>;

/// Play one campaign per seed. `threads` (at least 1) campaigns at a time,
/// each with its own resolver from `make`; the report is in seed order.
pub fn balance_batch<R, F>(
    content: &Content,
    campaign: &CampaignContent,
    seeds: &[u64],
    threads: usize,
    make: F,
) -> Result<BalanceReport, String>
where
    R: Resolver,
    F: Fn() -> R + Sync,
{
    let threads = threads.max(1);
    let mut slots: Vec<Option<Played>> = vec![None; seeds.len()];
    std::thread::scope(|s| {
        // Campaign `i` goes to worker `i % threads`: a fixed split.
        let mut parts: Vec<Vec<(usize, &mut Option<Played>)>> =
            (0..threads).map(|_| Vec::new()).collect();
        for (i, slot) in slots.iter_mut().enumerate() {
            parts[i % threads].push((i, slot));
        }
        for part in parts {
            let make = &make;
            s.spawn(move || {
                let mut r = make();
                for (i, slot) in part {
                    *slot = Some(play_ai_campaign(content, campaign, seeds[i], &mut r));
                }
            });
        }
    });
    let results = slots.into_iter().map(|r| r.expect("every campaign was played")).collect::<Result<_, _>>()?;
    let lt = &campaign.rules.length_target;
    Ok(BalanceReport {
        nations: content.nations.iter().map(|n| n.id.clone()).collect(),
        results,
        min_rounds: lt.min_rounds,
        max_rounds: lt.max_rounds,
        round_cap: campaign.rules.round_cap,
    })
}

impl BalanceReport {
    /// Campaigns won, per nation (content order).
    pub fn wins(&self) -> Vec<u32> {
        let mut w = vec![0; self.nations.len()];
        for r in &self.results {
            if let Some(n) = r.winner {
                w[n] += 1;
            }
        }
        w
    }

    /// Campaigns the round cap ended.
    pub fn undecided(&self) -> u32 {
        self.results.iter().filter(|r| r.winner.is_none()).count() as u32
    }

    /// The median length, in rounds (the lower middle of an even count).
    pub fn median_rounds(&self) -> Option<u32> {
        let mut l: Vec<u32> = self.results.iter().map(|r| r.rounds).collect();
        l.sort_unstable();
        l.get(l.len().saturating_sub(1) / 2).copied()
    }

    /// Campaigns whose length is inside the target.
    pub fn in_target(&self) -> u32 {
        self.results.iter().filter(|r| (self.min_rounds..=self.max_rounds).contains(&r.rounds)).count() as u32
    }

    /// AC12's two readings: `Ok` when every nation won at least once and the
    /// median length is inside the target; otherwise every miss, by name.
    pub fn verdict(&self) -> Result<(), String> {
        let mut miss = Vec::new();
        for (n, w) in self.nations.iter().zip(self.wins()) {
            if w == 0 {
                miss.push(format!("{n} never won"));
            }
        }
        match self.median_rounds() {
            Some(m) if (self.min_rounds..=self.max_rounds).contains(&m) => {}
            Some(m) => miss.push(format!(
                "median length {m} rounds is outside {}..={}",
                self.min_rounds, self.max_rounds
            )),
            None => miss.push("no campaign was played".to_string()),
        }
        if miss.is_empty() { Ok(()) } else { Err(miss.join("; ")) }
    }

    /// The reading, one line per nation, then the lengths.
    pub fn summary(&self) -> String {
        let n = self.results.len() as u32;
        let mut s = format!("{n} AI-only campaigns\n");
        for (id, w) in self.nations.iter().zip(self.wins()) {
            s += &format!("  {id:<12} won {w:>3} ({:.1}%)\n", 100.0 * w as f64 / n.max(1) as f64);
        }
        s += &format!("  undecided    {:>3} (round cap {})\n", self.undecided(), self.round_cap);
        let mut l: Vec<u32> = self.results.iter().map(|r| r.rounds).collect();
        l.sort_unstable();
        s += &format!(
            "  length: median {:?}, min {:?}, max {:?}; {} of {n} inside {}..={}\n",
            self.median_rounds(),
            l.first(),
            l.last(),
            self.in_target(),
            self.min_rounds,
            self.max_rounds
        );
        s
    }
}
