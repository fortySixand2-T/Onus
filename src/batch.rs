//! The balance batch: play every ordered matchup across K seeds, headless.
//!
//! This is the loop behind `src/bin/balance.rs`. It lives in the library, not
//! in `main`, because it is the thing worth testing: the bin is argument
//! parsing and printing around [`run_batch`].
//!
//! **Render-free driver code, not sim.** It builds matches with
//! [`crate::headless`] and reads [`MatchState`]; it never writes sim state and
//! holds no RNG of its own — [`seed_at`] is a pure function of the base.
//!
//! ## A cap is not a draw
//!
//! [`MatchOutcome::winner`]`== None` means both HQs fell on the *same tick*: a
//! decided, legitimate draw the sim reached on its own. A match that runs into
//! the tick cap was never decided at all — the observer stopped watching. They
//! are different facts about the game and this module keeps them apart
//! ([`MatchResult::MutualLoss`] vs [`MatchResult::Timeout`]), because B3 has to
//! report "% hitting the cap" and flag an all-timeout run instead of reporting
//! a hundred stalemates as a hundred fair draws (F-020).
//!
//! ## Order is an outcome
//!
//! The matchup list is built by walking [`Content::strategies`] in RON order,
//! never a map, and the batch is played sequentially in that order (seed outer,
//! then row, then column). The record vector *is* the report's row order, so it
//! has to be reproducible — re-running the batch must produce the identical
//! list. That is also why this loop is not parallel.

use crate::headless::{self, MatchSettings};
use crate::sim::ai::UnknownStrategy;
use crate::sim::content::Content;
use crate::sim::spatial::Faction;
use crate::sim::MatchState;

/// How one match ended.
///
/// Three cases, not two: see the module docs on why a timeout is kept distinct
/// from a mutual loss.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatchResult {
    /// One side's HQ was destroyed; the named faction is the survivor.
    Decided(Faction),
    /// Both HQs fell on the same tick — a draw the sim actually reached.
    MutualLoss,
    /// The tick cap was hit with the match still undecided. Not a draw: an
    /// unfinished match.
    Timeout,
}

impl MatchResult {
    /// The winner, if there is one. `None` for both a mutual loss and a
    /// timeout — which is exactly why callers that care about the difference
    /// must match on the variant rather than on this.
    pub fn winner(self) -> Option<Faction> {
        match self {
            MatchResult::Decided(f) => Some(f),
            _ => None,
        }
    }

    /// Did the *sim* end this match? True for a mutual loss, false for a
    /// timeout.
    pub fn is_decided(self) -> bool {
        !matches!(self, MatchResult::Timeout)
    }
}

/// One played match. Deliberately small for this checkbox — the per-side
/// production counts and the `balance_report.ron` shape belong to the later B2
/// checkboxes, and are added as *fields*, not as a second record type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MatchRecord {
    /// Who played, by faction slot: `[0]` is [`Faction::A`], `[1]` is
    /// [`Faction::B`]. Names, not indices: every number B3 prints is keyed by
    /// strategy name.
    pub strategies: [String; 2],
    /// The seed this match was played on.
    pub seed: u64,
    pub result: MatchResult,
    /// Sim ticks actually played — the number of steps the runner took. For a
    /// decided match this is `MatchOutcome::tick + 1` (the outcome's tick is
    /// the zero-based index of the last tick played); for a timeout it is
    /// exactly the cap.
    pub ticks: u32,
}

impl MatchRecord {
    pub fn winner(&self) -> Option<Faction> {
        self.result.winner()
    }

    /// The match length in seconds of play, at the sim's fixed rate.
    pub fn seconds(&self) -> f32 {
        self.ticks as f32 / headless::SIM_HZ as f32
    }
}

/// What to play. `Default` is the whole roster, one seed from base 0, at the
/// default cap.
#[derive(Clone, Debug)]
pub struct BatchSettings {
    /// How many seeds every matchup is played on.
    pub seeds: u32,
    /// The base the per-match seeds are derived from ([`seed_at`]).
    pub seed_base: u64,
    /// Per-match tick budget; see [`headless::DEFAULT_TICK_CAP`].
    pub tick_cap: u32,
    /// Restrict the roster to these strategy ids, in *content* order (not in
    /// the order they are listed here). `None` plays every strategy the
    /// content ships — so a strategy added to `strategies.ron` is picked up
    /// with no code change. An id the content does not have is refused.
    pub only: Option<Vec<String>>,
}

impl Default for BatchSettings {
    fn default() -> Self {
        Self {
            seeds: 1,
            seed_base: 0,
            tick_cap: headless::DEFAULT_TICK_CAP,
            only: None,
        }
    }
}

impl BatchSettings {
    pub fn with_seeds(mut self, seeds: u32) -> Self {
        self.seeds = seeds;
        self
    }

    pub fn with_seed_base(mut self, base: u64) -> Self {
        self.seed_base = base;
        self
    }

    pub fn with_tick_cap(mut self, cap: u32) -> Self {
        self.tick_cap = cap;
        self
    }

    pub fn with_only(mut self, only: Vec<String>) -> Self {
        self.only = Some(only);
        self
    }
}

/// The seed of the `k`-th match of a batch based at `base`.
///
/// A pure function — no generator state lives out here, and no clock is read,
/// so `--seed-base N --seeds K` names exactly one batch forever. The body is
/// SplitMix64's finalizer over `base + (k+1)·φ⁻¹`: consecutive `k` must not
/// give commanders neighbouring, correlated streams (their own seeding xors
/// the faction slot into the match seed), and an unmixed `base + k` would.
pub fn seed_at(base: u64, k: u32) -> u64 {
    let mut z = base.wrapping_add((k as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15));
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Play one headless match to its decision or to `settings.tick_cap`.
///
/// Returns [`UnknownStrategy`] if either side names a strategy the content does
/// not ship — the same refusal [`headless::ai_vs_ai`] makes, passed through.
///
/// The loop stops on the tick the sim decided: the rest of the sim chain is
/// gated on `match_running`, so stepping a decided match only burns time
/// against a frozen world.
pub fn run_match(
    content: &Content,
    settings: &MatchSettings,
) -> Result<MatchRecord, UnknownStrategy> {
    let mut app = headless::ai_vs_ai(content.clone(), settings)?;
    let mut ticks = 0u32;
    let mut result = MatchResult::Timeout;
    while ticks < settings.tick_cap {
        headless::step(&mut app);
        ticks += 1;
        if let Some(outcome) = app.world().resource::<MatchState>().outcome() {
            result = match outcome.winner {
                Some(f) => MatchResult::Decided(f),
                None => MatchResult::MutualLoss,
            };
            break;
        }
    }
    let default = content.default_strategy.as_str();
    let name = |s: &Option<String>| s.clone().unwrap_or_else(|| default.to_string());
    Ok(MatchRecord {
        strategies: [name(&settings.strategies[0]), name(&settings.strategies[1])],
        seed: settings.seed,
        result,
        ticks,
    })
}

/// The strategy ids this batch plays, in content (RON) order.
///
/// `only` is a *filter*, not an ordering: selecting `["turtle", "rush"]` plays
/// the same batch, in the same order, as selecting `["rush", "turtle"]`, so the
/// report's rows do not depend on how the flag was typed.
pub fn roster(content: &Content, only: Option<&[String]>) -> Result<Vec<String>, UnknownStrategy> {
    let Some(only) = only else {
        return Ok(content.strategies.iter().map(|s| s.id.clone()).collect());
    };
    for id in only {
        if content.strategy_index(id).is_none() {
            return Err(UnknownStrategy {
                id: id.clone(),
                faction: None,
            });
        }
    }
    Ok(content
        .strategies
        .iter()
        .filter(|s| only.iter().any(|id| id == &s.id))
        .map(|s| s.id.clone())
        .collect())
}

/// Play every ordered matchup (mirrors included) of the selected roster, on
/// each of `settings.seeds` derived seeds.
///
/// `progress` is called once per finished match, in batch order — the bin uses
/// it to keep a long run from being silent. It observes; it cannot steer.
///
/// Sequential and deterministic: for `N` strategies and `K` seeds this is
/// `N·N·K` records, seed-major then row-major, identical on every re-run.
pub fn run_batch(
    content: &Content,
    settings: &BatchSettings,
    progress: &mut dyn FnMut(&MatchRecord),
) -> Result<Vec<MatchRecord>, UnknownStrategy> {
    let roster = roster(content, settings.only.as_deref())?;
    let mut records = Vec::with_capacity(roster.len() * roster.len() * settings.seeds as usize);
    for k in 0..settings.seeds {
        let seed = seed_at(settings.seed_base, k);
        for a in &roster {
            for b in &roster {
                let match_settings = MatchSettings::default()
                    .with_seed(seed)
                    .with_strategies(a, b)
                    .with_tick_cap(settings.tick_cap);
                let record = run_match(content, &match_settings)?;
                progress(&record);
                records.push(record);
            }
        }
    }
    Ok(records)
}

/// The human summary of a batch: how matches ended and how long they ran.
///
/// The win-rate matrix and the kill-criteria gate are B3's; this is only enough
/// to read a run at a glance and to see, immediately, whether the instrument
/// produced games or stalemates.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tally {
    pub total: usize,
    /// Matches one side won.
    pub decided: usize,
    /// Draws the sim reached: both HQs down on one tick.
    pub mutual_losses: usize,
    /// Matches that hit the cap undecided.
    pub timeouts: usize,
    pub wins: [usize; 2],
    /// Ticks played, ascending — the raw material for the median and the
    /// spread.
    pub lengths: Vec<u32>,
}

impl Tally {
    pub fn of(records: &[MatchRecord]) -> Self {
        let mut t = Tally {
            total: records.len(),
            ..Default::default()
        };
        for r in records {
            match r.result {
                MatchResult::Decided(Faction::A) => {
                    t.decided += 1;
                    t.wins[0] += 1;
                }
                MatchResult::Decided(Faction::B) => {
                    t.decided += 1;
                    t.wins[1] += 1;
                }
                MatchResult::MutualLoss => t.mutual_losses += 1,
                MatchResult::Timeout => t.timeouts += 1,
            }
            t.lengths.push(r.ticks);
        }
        t.lengths.sort_unstable();
        t
    }

    /// Fraction of matches that ran into the cap. B3 turns this into a gate;
    /// here it is the one number that says whether a run means anything.
    pub fn timeout_rate(&self) -> f32 {
        if self.total == 0 {
            return 0.0;
        }
        self.timeouts as f32 / self.total as f32
    }

    /// The `q`-quantile of match length in ticks (`q` in `0.0..=1.0`), by the
    /// textbook **nearest-rank** definition: the smallest value at or below
    /// which at least a `q` fraction of the batch falls, i.e. rank
    /// `ceil(q·N)`. `None` for an empty batch.
    ///
    /// Integer ranks, no interpolation: a quantile of match lengths should be
    /// a length some match actually had.
    pub fn length_quantile(&self, q: f32) -> Option<u32> {
        if self.lengths.is_empty() {
            return None;
        }
        let n = self.lengths.len();
        let rank = (q.clamp(0.0, 1.0) * n as f32).ceil() as usize;
        self.lengths.get(rank.clamp(1, n) - 1).copied()
    }

    pub fn median_length(&self) -> Option<u32> {
        self.length_quantile(0.5)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(result: MatchResult, ticks: u32) -> MatchRecord {
        MatchRecord {
            strategies: ["a".into(), "b".into()],
            seed: 0,
            result,
            ticks,
        }
    }

    #[test]
    fn seed_at_is_pure_and_spreads() {
        assert_eq!(seed_at(0, 0), seed_at(0, 0));
        let seeds: Vec<u64> = (0..8).map(|k| seed_at(0, k)).collect();
        let mut sorted = seeds.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), 8, "eight distinct seeds");
        // A different base is a different batch.
        assert!((0..8).all(|k| seed_at(0, k) != seed_at(1, k)));
    }

    #[test]
    fn a_tally_keeps_timeouts_apart_from_draws() {
        let t = Tally::of(&[
            rec(MatchResult::Decided(Faction::A), 100),
            rec(MatchResult::Decided(Faction::B), 200),
            rec(MatchResult::MutualLoss, 300),
            rec(MatchResult::Timeout, 400),
        ]);
        assert_eq!(t.total, 4);
        assert_eq!(t.decided, 2);
        assert_eq!(t.wins, [1, 1]);
        assert_eq!(t.mutual_losses, 1);
        assert_eq!(t.timeouts, 1);
        assert_eq!(t.timeout_rate(), 0.25);
        // Nearest rank over four lengths: the median is the 2nd of them.
        assert_eq!(t.median_length(), Some(200));
        assert_eq!(t.length_quantile(0.0), Some(100));
        assert_eq!(t.length_quantile(0.25), Some(100));
        assert_eq!(t.length_quantile(0.75), Some(300));
        assert_eq!(t.length_quantile(1.0), Some(400));
    }

    #[test]
    fn an_empty_tally_has_no_length_and_no_timeouts() {
        let t = Tally::of(&[]);
        assert_eq!(t.median_length(), None);
        assert_eq!(t.timeout_rate(), 0.0);
    }

    #[test]
    fn seconds_is_ticks_at_the_sim_rate() {
        assert_eq!(rec(MatchResult::Timeout, headless::SIM_HZ).seconds(), 1.0);
    }
}
