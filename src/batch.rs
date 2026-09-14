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
//! ## Side-balanced sampling
//!
//! Every `(a, b, seed)` is played in **both** [`Orientation`]s, on the same
//! seed: the seed is the control, only the geography differs. Ordered pairs
//! already vary the faction slot; orientation varies the ground, so a left-hand
//! spawn advantage lands once on each slot and cancels in the aggregate instead
//! of masquerading as strategy strength (F-021). Each record says which
//! orientation it was played in, so the raw asymmetry stays auditable rather
//! than being silently averaged away.
//!
//! ## Order is an outcome
//!
//! The matchup list is built by walking [`Content::strategies`] in RON order,
//! never a map, and the batch is played sequentially in that order. The rule,
//! stated once: **seed-major, then RON row-major, then orientation** — for each
//! seed, for each row `a`, for each column `b`, the match is played on the
//! normal map and then on the swapped one. The record vector *is* the report's
//! row order, so it has to be reproducible — re-running the batch must produce the identical
//! list. That is also why this loop is not parallel.

use crate::headless::{self, MatchSettings, Orientation};
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
    /// Which spawn orientation this match was played in. Recorded, not
    /// implied: a row whose geography cannot be recovered cannot be audited,
    /// and B3 aggregates over orientation pairs.
    pub orientation: Orientation,
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

    /// Did the winner hold the **left-hand** base? `None` if nobody won.
    ///
    /// The bridge from a slot-keyed result to a positional one: in
    /// [`Orientation::Normal`] the left base is [`Faction::A`]'s, in
    /// [`Orientation::Swapped`] it is [`Faction::B`]'s.
    pub fn winner_at_left(&self) -> Option<bool> {
        self.winner().map(|f| f == self.orientation.left())
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
        orientation: settings.orientation,
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
/// Every matchup is played in **both** spawn orientations on the same seed, so
/// for `N` strategies and `K` seeds this is `N·N·K·2` records — seed-major,
/// then RON row-major, then orientation ([`Orientation::ALL`] order).
/// Sequential and deterministic: identical on every re-run.
pub fn run_batch(
    content: &Content,
    settings: &BatchSettings,
    progress: &mut dyn FnMut(&MatchRecord),
) -> Result<Vec<MatchRecord>, UnknownStrategy> {
    let roster = roster(content, settings.only.as_deref())?;
    let mut records = Vec::with_capacity(
        roster.len() * roster.len() * settings.seeds as usize * Orientation::ALL.len(),
    );
    for k in 0..settings.seeds {
        let seed = seed_at(settings.seed_base, k);
        for a in &roster {
            for b in &roster {
                for orientation in Orientation::ALL {
                    let match_settings = MatchSettings::default()
                        .with_seed(seed)
                        .with_strategies(a, b)
                        .with_tick_cap(settings.tick_cap)
                        .with_orientation(orientation);
                    let record = run_match(content, &match_settings)?;
                    progress(&record);
                    records.push(record);
                }
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
    /// Wins by **spawn position** rather than by faction slot: `[0]` is the
    /// left-hand base, `[1]` the right-hand one. This is the raw positional
    /// asymmetry — the thing side-balanced sampling exists to expose and then
    /// cancel.
    pub spawn_wins: [usize; 2],
    /// The same counts split by orientation, in [`Orientation::ALL`] order, so
    /// a report can print what each half of the sample did next to the
    /// balanced aggregate instead of averaging the asymmetry away unseen.
    pub by_orientation: [SplitTally; 2],
    /// Ticks played, ascending — the raw material for the median and the
    /// spread.
    pub lengths: Vec<u32>,
}

/// One orientation's slice of a [`Tally`] — the same counters, minus the
/// length distribution (lengths are pooled across orientations; the split
/// exists to show *who won where*).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SplitTally {
    pub total: usize,
    pub decided: usize,
    pub mutual_losses: usize,
    pub timeouts: usize,
    pub wins: [usize; 2],
}

impl SplitTally {
    fn add(&mut self, r: &MatchRecord) {
        self.total += 1;
        match r.result {
            MatchResult::Decided(f) => {
                self.decided += 1;
                self.wins[side_index(f)] += 1;
            }
            MatchResult::MutualLoss => self.mutual_losses += 1,
            MatchResult::Timeout => self.timeouts += 1,
        }
    }
}

/// The index of a faction in [`headless::SIDES`] — slot order, never a map's.
fn side_index(f: Faction) -> usize {
    headless::SIDES
        .iter()
        .position(|s| *s == f)
        .expect("SIDES lists every faction")
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
            if let Some(left) = r.winner_at_left() {
                t.spawn_wins[usize::from(!left)] += 1;
            }
            t.by_orientation[r.orientation.index()].add(r);
            t.lengths.push(r.ticks);
        }
        t.lengths.sort_unstable();
        t
    }

    /// The share of *decided* matches won from the left-hand base, or `None`
    /// if nothing was decided. 0.5 means the map is not picking the winner.
    pub fn left_spawn_rate(&self) -> Option<f32> {
        let n = self.spawn_wins[0] + self.spawn_wins[1];
        (n > 0).then(|| self.spawn_wins[0] as f32 / n as f32)
    }

    /// The share of decided matches won by faction slot A — the side-balanced
    /// figure, since every matchup is played from both spawns. `None` if
    /// nothing was decided.
    pub fn slot_a_rate(&self) -> Option<f32> {
        let n = self.wins[0] + self.wins[1];
        (n > 0).then(|| self.wins[0] as f32 / n as f32)
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
        oriented(result, ticks, Orientation::Normal)
    }

    fn oriented(result: MatchResult, ticks: u32, orientation: Orientation) -> MatchRecord {
        MatchRecord {
            strategies: ["a".into(), "b".into()],
            seed: 0,
            result,
            orientation,
            ticks,
        }
    }

    /// A win from the left-hand base counts as a left win in *either*
    /// orientation — that is the whole point of recording the orientation.
    #[test]
    fn spawn_wins_are_positional_and_slot_wins_are_not() {
        let t = Tally::of(&[
            oriented(MatchResult::Decided(Faction::A), 10, Orientation::Normal),
            oriented(MatchResult::Decided(Faction::B), 10, Orientation::Swapped),
        ]);
        assert_eq!(t.wins, [1, 1], "one win per slot");
        assert_eq!(t.spawn_wins, [2, 0], "both won from the left base");
        assert_eq!(t.left_spawn_rate(), Some(1.0));
        assert_eq!(t.slot_a_rate(), Some(0.5));
        assert_eq!(t.by_orientation[0].wins, [1, 0]);
        assert_eq!(t.by_orientation[1].wins, [0, 1]);
        assert_eq!(t.by_orientation[0].total, 1);
        assert_eq!(t.by_orientation[1].total, 1);

        // Undecided rows are counted, but never as a positional win.
        let u = Tally::of(&[
            oriented(MatchResult::Timeout, 5, Orientation::Swapped),
            oriented(MatchResult::MutualLoss, 5, Orientation::Swapped),
        ]);
        assert_eq!(u.spawn_wins, [0, 0]);
        assert_eq!(u.left_spawn_rate(), None);
        assert_eq!(u.slot_a_rate(), None);
        assert_eq!(u.by_orientation[1].timeouts, 1);
        assert_eq!(u.by_orientation[1].mutual_losses, 1);
        assert_eq!(u.by_orientation[0].total, 0);
    }

    #[test]
    fn a_record_reports_the_spawn_its_winner_held() {
        let normal = oriented(MatchResult::Decided(Faction::A), 1, Orientation::Normal);
        assert_eq!(normal.winner_at_left(), Some(true));
        let swapped = oriented(MatchResult::Decided(Faction::A), 1, Orientation::Swapped);
        assert_eq!(swapped.winner_at_left(), Some(false));
        assert_eq!(
            oriented(MatchResult::Timeout, 1, Orientation::Normal).winner_at_left(),
            None
        );
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
