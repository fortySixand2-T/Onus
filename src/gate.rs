//! The kill-criteria gate (B3): PASS / FAIL / undetermined, with the numbers
//! behind each.
//!
//! DESIGN_BRIEF's kill criteria, as BALANCE_PLAN states them for B3:
//!
//! - **K1 strength** — no strategy (and so no unit, through its mass probe)
//!   wins more than 65% *regardless of counter*: a [`WinMatrix`] row mean, the
//!   mean over every opponent, above 65%.
//! - **K2 seat bias** — mirrors are within a tolerance of 50%: neither the
//!   faction slot nor the spawn base picks the winner of an identical matchup.
//! - **K3 termination** — matches terminate in the 5–8 minute target: the
//!   **band share** (in-band ÷ decided, B3.5's design metric) is at least
//!   `min_band_share`, and at most `max_timeout_rate` of all matches hit the
//!   cap. A median inside the band is not enough — a batch split 47% short /
//!   6% in band / 47% long has its median in the gap and almost nothing in
//!   target (critic B3). The median and the before/after shares (a timeout
//!   counts as *after*) are read with intervals and reported as context; they
//!   are never part of the status (F-038).
//!
//! The thresholds are harness configuration ([`GateSpec`]), stated once and
//! recorded in FINDINGS (F-038), never content.
//!
//! ## The data decides, or the gate says it cannot
//!
//! Every gated number is a [`Reading`]: a point estimate **and** the 95%
//! Wilson interval around it, and its [`Status`] is read off the interval,
//! never the point — the same rule as the pentagon's [`crate::pentagon::Verdict`]
//! (F-034). Against a threshold `t`:
//!
//! - "at most `t`": **PASS** if the whole interval is at or below `t`,
//!   **FAIL** if the whole interval is above it, otherwise **undetermined**;
//! - "at least `t`": the mirror image;
//! - "within `c ± tol`": **PASS** if the interval lies inside the tolerance
//!   band, **FAIL** if it lies entirely outside it, otherwise undetermined.
//!
//! A criterion PASSes only if every one of its readings does; one FAIL fails
//! it; anything else is **undetermined**. A reading with no data (nothing
//! decided, no mirror played) is undetermined, never a pass: an all-timeout run
//! cannot read as balanced.
//!
//! ## Clustering
//!
//! The matches behind a reading are not independent. One `(pair, seed)` is
//! played in both orientations and both slot orders (4 matches for a pair of
//! two strategies, 2 for a mirror), on the same map with the same seeded
//! streams, and F-031 measured a design effect of 1.51 for that 4-match
//! cluster — an intra-cluster correlation of `ρ = 0.51 / 3 = 0.17`. Each
//! reading counts its own clusters and uses the standard design effect
//! `deff = 1 + (m − 1)·ρ` for its mean cluster size `m`, then computes Wilson
//! on the effective sample `n / deff`. A batch whose records all share one
//! seed is discounted heavily, as it should be. **Mirror (K2) readings** are
//! the exception: `ρ = 0.17` gives a 2-match mirror cluster only 1.17, but the
//! slot-A share measured on a mirror-only batch has a design effect of 1.38
//! with the seed as the cluster (1.34 by `(strategy, seed)`), so seat readings
//! use at least [`MIRROR_DEFF`] (F-038).
//!
//! Pure: a function of `(&Content, &[MatchRecord], &GateSpec)`. No map is
//! iterated; every list is in matrix (first-appearance) order.

use serde::{Deserialize, Serialize};

use crate::batch::{MatchRecord, MatchResult};
use crate::metrics::{wilson_bounds, LengthBand, LengthDistribution, WinMatrix};
use crate::pentagon::mass_strategy;
use crate::sim::content::Content;
use crate::sim::spatial::Faction;

/// One criterion's (or reading's) outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    /// The interval clears the threshold: the data says the criterion holds.
    Pass,
    /// The interval is entirely on the wrong side: the data says it fails.
    Fail,
    /// The interval straddles the threshold, or there is no data. Never a pass.
    Undetermined,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Status::Pass => "PASS",
            Status::Fail => "FAIL",
            Status::Undetermined => "undetermined",
        }
    }

    /// FAIL if any part fails; PASS only if there is at least one part and
    /// every part passes; otherwise undetermined.
    pub fn all(parts: impl IntoIterator<Item = Status>) -> Status {
        let mut any = false;
        let mut all_pass = true;
        for s in parts {
            any = true;
            match s {
                Status::Fail => return Status::Fail,
                Status::Pass => {}
                Status::Undetermined => all_pass = false,
            }
        }
        if any && all_pass {
            Status::Pass
        } else {
            Status::Undetermined
        }
    }
}

/// The gate's thresholds. Harness configuration, not content (F-038).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GateSpec {
    /// K1: the highest row mean (win rate over every opponent) allowed.
    pub max_strength: f64,
    /// K2: how far from 50% a mirror's slot or spawn share may sit.
    pub mirror_tolerance: f64,
    /// K3: the target length band.
    pub band: LengthBand,
    /// K3: the band-share bar (in-band ÷ decided).
    pub min_band_share: f64,
    /// K3: the most matches that may hit the cap.
    pub max_timeout_rate: f64,
    /// Intra-cluster correlation of one `(pair, seed)`'s matches; see the
    /// module docs. 0 treats every match as independent.
    pub icc: f64,
    /// K2: the design effect of a mirror seat reading, at least. Measured, not
    /// derived from `icc` (F-038); a batch whose own `icc`-based effect is
    /// larger keeps that.
    pub mirror_design_effect: f64,
}

impl Default for GateSpec {
    fn default() -> Self {
        Self {
            max_strength: 0.65,
            mirror_tolerance: 0.05,
            band: LengthBand::default(),
            min_band_share: 0.5,
            max_timeout_rate: 0.05,
            icc: F031_ICC,
            mirror_design_effect: MIRROR_DEFF,
        }
    }
}

/// F-031's measured design effect, 1.51, on a cluster of 4 matches:
/// `ρ = (1.51 − 1) / (4 − 1) = 0.17`.
pub const F031_ICC: f64 = 0.17;

/// The slot-A share's design effect over a mirror-only batch, measured with the
/// **seed** as the cluster (robust variance / binomial variance), 62 seeds,
/// 1 235 decided mirrors: 1.381 (F-038). F-031's ICC understates it for
/// mirrors (1.17 at 2 matches a cluster; the `(strategy, seed)`-cluster
/// measurement is 1.34), so seat readings use this, the conservative one.
pub const MIRROR_DEFF: f64 = 1.38;

/// A proportion read off a batch, with the interval that decides its status.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Reading {
    /// The point estimate; `None` with no observations.
    pub value: Option<f64>,
    /// 95% Wilson interval on the effective sample.
    pub interval: Option<(f64, f64)>,
    /// Observations behind it (decided matches, or all matches for a timeout
    /// rate).
    pub n: u32,
    /// Distinct `(pair, seed)` clusters those observations fall in.
    pub clusters: u32,
    /// `n / n_eff`.
    pub design_effect: f64,
    /// The sample size the interval is computed on.
    pub n_eff: f64,
    pub status: Status,
}

/// A threshold rule for [`Reading::judge`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Rule {
    AtMost(f64),
    AtLeast(f64),
    Within { centre: f64, tolerance: f64 },
}

impl Rule {
    fn judge(self, interval: Option<(f64, f64)>) -> Status {
        let Some((lo, hi)) = interval else {
            return Status::Undetermined;
        };
        match self {
            Rule::AtMost(t) if hi <= t => Status::Pass,
            Rule::AtMost(t) if lo > t => Status::Fail,
            Rule::AtLeast(t) if lo >= t => Status::Pass,
            Rule::AtLeast(t) if hi < t => Status::Fail,
            Rule::Within { centre, tolerance }
                if lo >= centre - tolerance && hi <= centre + tolerance =>
            {
                Status::Pass
            }
            Rule::Within { centre, tolerance }
                if lo > centre + tolerance || hi < centre - tolerance =>
            {
                Status::Fail
            }
            _ => Status::Undetermined,
        }
    }
}

/// `1 + (m − 1)·ρ` for `n` observations in `clusters` clusters.
pub fn design_effect(n: u32, clusters: u32, icc: f64) -> f64 {
    if n == 0 || clusters == 0 {
        return 1.0;
    }
    let m = n as f64 / clusters as f64;
    1.0 + (m - 1.0).max(0.0) * icc
}

impl Reading {
    /// A proportion of `half_successes / (2·n)` (halves, so a mutual loss is
    /// representable), over `clusters` clusters, judged by `rule`.
    pub fn proportion(half_successes: u64, n: u32, clusters: u32, icc: f64, rule: Rule) -> Self {
        Self::proportion_at_least(half_successes, n, clusters, icc, 1.0, rule)
    }

    /// [`Reading::proportion`] with the design effect floored at `min_deff`.
    pub fn proportion_at_least(
        half_successes: u64,
        n: u32,
        clusters: u32,
        icc: f64,
        min_deff: f64,
        rule: Rule,
    ) -> Self {
        let deff = design_effect(n, clusters, icc).max(min_deff);
        let n_eff = n as f64 / deff;
        let value = (n > 0).then(|| half_successes as f64 / (2.0 * n as f64));
        let interval = value.map(|p| wilson_bounds(p, n_eff));
        Reading {
            value,
            interval,
            n,
            clusters,
            design_effect: deff,
            n_eff,
            status: rule.judge(interval),
        }
    }

    /// A **mean of independent proportions** (a row mean): `mean` over cells
    /// of sizes `ns` in `clusters` clusters, judged by `rule`.
    ///
    /// Cell `i` contributes variance `p_i(1−p_i)·w_i / k²` with
    /// `w_i = deff_i / n_i`. Only the mean is known, so the interval is built
    /// on `V(μ)`, the **largest** variance any cell rates averaging `μ` could
    /// have ([`max_mean_variance`]), and is the hull of two intervals:
    ///
    /// - the normal interval `mean ± z·√V(mean)`. The true rates average
    ///   `mean`, so `V(mean)` is at least the true variance: this **contains
    ///   the normal interval on the true variance**, whatever the cell rates
    ///   and however unequal the cell sizes (critic B3: a cell of 10 beside
    ///   one of 1 000);
    /// - the score interval, every `μ` with `|mean − μ| ≤ z·√V(μ)`. This is
    ///   Wilson's construction on the worst case, so it stays defined at 0%
    ///   and 100%, where the normal interval collapses.
    ///
    /// With equal `w_i` (and so for one cell), `V(μ) = μ(1−μ)/n_eff` and the two
    /// parts are Wilson's and the normal interval at `mean` on
    /// `n_eff = k² / Σ w_i`, computed in closed form. `n_eff` is reported as
    /// `k² / Σ w_i` in every case.
    pub fn mean_of(
        mean: Option<f64>,
        ns: &[u32],
        clusters: &[u32],
        icc: f64,
        rule: Rule,
    ) -> Self {
        let w: Vec<f64> = ns
            .iter()
            .zip(clusters)
            .filter(|(n, _)| **n > 0)
            .map(|(&n, &c)| design_effect(n, c, icc) / n as f64)
            .collect();
        let k = w.len() as f64;
        let inv: f64 = w.iter().sum();
        let n: u32 = ns.iter().sum();
        let n_eff = if inv > 0.0 { k * k / inv } else { 0.0 };
        let equal = w.windows(2).all(|p| p[0] == p[1]);
        let interval = match mean {
            Some(p) if n_eff > 0.0 && equal => {
                Some(hull(wilson_bounds(p, n_eff), normal(p, p * (1.0 - p) / n_eff)))
            }
            Some(p) if n_eff > 0.0 => Some(hull(
                worst_case_score_interval(p, &w),
                normal(p, max_mean_variance(p, &w)),
            )),
            _ => None,
        };
        Reading {
            value: mean.filter(|_| n_eff > 0.0),
            interval,
            n,
            clusters: clusters.iter().sum(),
            design_effect: if n_eff > 0.0 { n as f64 / n_eff } else { 1.0 },
            n_eff,
            status: rule.judge(interval),
        }
    }
}

/// The largest variance of a mean of `k = w.len()` independent proportions
/// whose rates average `mu`: `max Σ w_i p_i(1−p_i) / k²` subject to
/// `Σ p_i = k·mu`, `0 ≤ p_i ≤ 1`. Concave, so the KKT point is the maximum:
/// `p_i = clamp((1 − λ/w_i)/2, 0, 1)` with `λ` found by bisection (`Σ p_i` is
/// decreasing in `λ`). Equal weights give every `p_i = mu`.
pub fn max_mean_variance(mu: f64, w: &[f64]) -> f64 {
    let k = w.len() as f64;
    if k == 0.0 || mu <= 0.0 || mu >= 1.0 {
        return 0.0;
    }
    let target = mu * k;
    let rates = |lambda: f64| w.iter().map(move |&wi| ((1.0 - lambda / wi) / 2.0).clamp(0.0, 1.0));
    let top = w.iter().cloned().fold(0.0, f64::max);
    let (mut lo, mut hi) = (-top, top);
    for _ in 0..200 {
        let mid = 0.5 * (lo + hi);
        if rates(mid).sum::<f64>() > target {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let lambda = 0.5 * (lo + hi);
    w.iter().zip(rates(lambda)).map(|(wi, p)| wi * p * (1.0 - p)).sum::<f64>() / (k * k)
}

/// `p ± z·√var`, clamped to `[0, 1]`.
fn normal(p: f64, var: f64) -> (f64, f64) {
    let half = crate::metrics::WILSON_Z * var.max(0.0).sqrt();
    ((p - half).max(0.0), (p + half).min(1.0))
}

fn hull(a: (f64, f64), b: (f64, f64)) -> (f64, f64) {
    (a.0.min(b.0), a.1.max(b.1))
}

/// Every `μ` in `[0, 1]` with `|p − μ| ≤ z·√V(μ)`, `V` = [`max_mean_variance`].
/// `V` is concave, so each side has a single crossing, found by bisection.
fn worst_case_score_interval(p: f64, w: &[f64]) -> (f64, f64) {
    let z = crate::metrics::WILSON_Z;
    let inside = |mu: f64| (p - mu).abs() <= z * max_mean_variance(mu, w).sqrt();
    let edge = |mut out: f64, mut inn: f64| {
        if inside(out) {
            return out;
        }
        for _ in 0..200 {
            let mid = 0.5 * (out + inn);
            if inside(mid) {
                inn = mid;
            } else {
                out = mid;
            }
        }
        inn
    };
    (edge(0.0, p), edge(1.0, p))
}

/// K1 for one strategy.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StrengthRow {
    pub strategy: String,
    /// The unit this strategy masses, if it is a mass probe: K1's "unit win
    /// rate" is its probe's row.
    pub unit: Option<String>,
    /// Defined off-diagonal cells behind the mean.
    pub cells: u32,
    /// Off-diagonal cells in the row (opponents played).
    pub opponents: u32,
    /// The row mean, judged "at most `max_strength`".
    pub strength: Reading,
    /// Beats **every** opponent, each cell's interval above 50%.
    pub dominant: bool,
    /// Strictly weak: loses to every opponent (each cell's interval below
    /// 50%), or its row interval lies wholly below `1 − max_strength` — the
    /// mirror image of K1's bar. Named, not gated: K1 bounds strength from
    /// above only (F-038).
    pub losing: bool,
}

/// K1: no strategy wins more than `max_strength` regardless of counter.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Strength {
    pub status: Status,
    pub max_strength: f64,
    /// Every strategy, in matrix order.
    pub rows: Vec<StrengthRow>,
    /// Strategies whose strength is resolved above the threshold, by name.
    pub failing: Vec<String>,
    /// Strategies that beat every opponent, by name.
    pub dominant: Vec<String>,
    /// Strictly weak strategies ([`StrengthRow::losing`]), by name. Reported,
    /// not gated.
    pub losing: Vec<String>,
}

/// One mirror's seat readings. A **resolved FAIL** here fails K2; an
/// undetermined one does not block a pooled PASS (multiplicity, F-038).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MirrorRow {
    pub strategy: String,
    /// Slot A's share of this mirror, judged against the tolerance.
    pub slot_a: Reading,
    /// The left-hand base's share of this mirror, judged the same way.
    pub left_spawn: Reading,
}

/// K2: mirrors are within tolerance of 50%, by slot and by spawn base.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SeatBias {
    pub status: Status,
    pub tolerance: f64,
    /// Slot A's share of every decided mirror (turn order / RNG stream).
    pub slot_a: Reading,
    /// The left-hand base's share of every decided mirror (geography).
    pub left_spawn: Reading,
    /// Per mirror, in matrix order. A resolved FAIL in any of them fails K2.
    pub mirrors: Vec<MirrorRow>,
}

/// K3: matches terminate in the target band.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Termination {
    pub status: Status,
    pub band: LengthBand,
    pub min_band_share: f64,
    pub max_timeout_rate: f64,
    /// Reported, not gated: matches decided before the band ÷ all, judged
    /// "at most 50%" (the median match is not too short).
    pub below: Reading,
    /// Reported, not gated: matches decided after the band, plus timeouts,
    /// ÷ all, judged "at most 50%" (the median match is not too long).
    pub beyond: Reading,
    /// Gated: in-band ÷ decided, judged "at least `min_band_share`".
    pub band_share: Reading,
    /// Gated: timeouts ÷ all, judged "at most `max_timeout_rate`".
    pub timeout_rate: Reading,
    /// Median of decided matches, ticks (reported).
    pub decided_median: Option<u32>,
    /// Every match hit the cap: nothing about the game was measured.
    pub all_timeout: bool,
}

/// The whole gate.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct KillGate {
    pub spec: GateSpec,
    pub status: Status,
    pub strength: Strength,
    pub seat: SeatBias,
    pub termination: Termination,
}

/// A `(pair, seed)` cluster key: the pair as matrix indices, smaller first.
type Key = (usize, usize, u64);

fn key(m: &WinMatrix, r: &MatchRecord) -> Key {
    let a = m.index(&r.strategies[0]).expect("matrix built from these records");
    let b = m.index(&r.strategies[1]).expect("matrix built from these records");
    (a.min(b), a.max(b), r.seed)
}

/// Distinct keys, sorted — a set without a map.
fn distinct(mut keys: Vec<Key>) -> Vec<Key> {
    keys.sort_unstable();
    keys.dedup();
    keys
}

impl KillGate {
    pub fn of(content: &Content, records: &[MatchRecord], spec: &GateSpec) -> Self {
        let matrix = WinMatrix::of(records);
        let strength = strength(content, records, &matrix, spec);
        let seat = seat_bias(records, &matrix, spec);
        let termination = termination(records, &matrix, spec);
        let status = Status::all([strength.status, seat.status, termination.status]);
        KillGate {
            spec: spec.clone(),
            status,
            strength,
            seat,
            termination,
        }
    }
}

fn strength(content: &Content, records: &[MatchRecord], m: &WinMatrix, spec: &GateSpec) -> Strength {
    let decided_keys = distinct(
        records
            .iter()
            .filter(|r| r.result.is_decided())
            .map(|r| key(m, r))
            .collect(),
    );
    let clusters_of = |i: usize, j: usize| -> u32 {
        let (a, b) = (i.min(j), i.max(j));
        decided_keys.iter().filter(|k| k.0 == a && k.1 == b).count() as u32
    };
    let rows: Vec<StrengthRow> = (0..m.len())
        .map(|i| {
            let id = &m.ids()[i];
            let mut ns = Vec::new();
            let mut cs = Vec::new();
            let mut opponents = 0u32;
            let mut wins_all = true;
            let mut loses_all = true;
            for j in (0..m.len()).filter(|&j| j != i) {
                let c = m.cell(i, j).copied().unwrap_or_default();
                if c.played() == 0 {
                    continue;
                }
                opponents += 1;
                if c.n_decided == 0 {
                    wins_all = false;
                    loses_all = false;
                    continue;
                }
                let cl = clusters_of(i, j);
                let cell = Reading::proportion(c.half_wins as u64, c.n_decided, cl, spec.icc, Rule::AtMost(0.5));
                let (lo, hi) = cell.interval.expect("decided");
                wins_all &= lo > 0.5;
                loses_all &= hi < 0.5;
                ns.push(c.n_decided);
                cs.push(cl);
            }
            let mean = m.row_mean(i);
            let strength = Reading::mean_of(mean.mean, &ns, &cs, spec.icc, Rule::AtMost(spec.max_strength));
            let decided_opponents = ns.len() as u32;
            let loses_every_matchup = opponents > 0 && decided_opponents == opponents && loses_all;
            let row_below_mirror_bar = strength.interval.is_some_and(|(_, hi)| hi < 1.0 - spec.max_strength);
            StrengthRow {
                strategy: id.clone(),
                unit: content
                    .units
                    .iter()
                    .find(|u| mass_strategy(content, &u.id).is_some_and(|s| &s.id == id))
                    .map(|u| u.id.clone()),
                cells: decided_opponents,
                opponents,
                strength,
                dominant: opponents > 0 && decided_opponents == opponents && wins_all,
                losing: loses_every_matchup || row_below_mirror_bar,
            }
        })
        .collect();
    let names = |f: &dyn Fn(&StrengthRow) -> bool| -> Vec<String> {
        rows.iter().filter(|r| f(r)).map(|r| r.strategy.clone()).collect()
    };
    Strength {
        status: Status::all(rows.iter().map(|r| r.strength.status)),
        max_strength: spec.max_strength,
        failing: names(&|r| r.strength.status == Status::Fail),
        dominant: names(&|r| r.dominant),
        losing: names(&|r| r.losing),
        rows,
    }
}

fn seat_bias(records: &[MatchRecord], m: &WinMatrix, spec: &GateSpec) -> SeatBias {
    let rule = Rule::Within {
        centre: 0.5,
        tolerance: spec.mirror_tolerance,
    };
    let mirrors: Vec<&MatchRecord> = records
        .iter()
        .filter(|r| r.strategies[0] == r.strategies[1] && r.result.is_decided())
        .collect();
    // Half-wins to slot A and to the left-hand base.
    let half = |r: &MatchRecord, side: Faction| -> u64 {
        match r.result {
            MatchResult::Decided(f) if f == side => 2,
            MatchResult::MutualLoss => 1,
            _ => 0,
        }
    };
    let pooled = |of: &dyn Fn(&MatchRecord) -> u64| {
        let n = mirrors.len() as u32;
        let clusters = distinct(mirrors.iter().map(|r| key(m, r)).collect()).len() as u32;
        let s: u64 = mirrors.iter().map(|r| of(r)).sum();
        Reading::proportion_at_least(s, n, clusters, spec.icc, spec.mirror_design_effect, rule)
    };
    let slot_a = pooled(&|r| half(r, Faction::A));
    let left_spawn = pooled(&|r| half(r, r.orientation.left()));
    let per_mirror: Vec<MirrorRow> = (0..m.len())
        .filter_map(|i| {
            let id = &m.ids()[i];
            let own: Vec<&&MatchRecord> = mirrors.iter().filter(|r| &r.strategies[0] == id).collect();
            let played = m.cell(i, i).map(|c| c.played()).unwrap_or(0);
            if played == 0 {
                return None;
            }
            let clusters = distinct(own.iter().map(|r| key(m, r)).collect()).len() as u32;
            let n = own.len() as u32;
            let read = |of: &dyn Fn(&MatchRecord) -> u64| {
                let s: u64 = own.iter().map(|r| of(r)).sum();
                Reading::proportion_at_least(s, n, clusters, spec.icc, spec.mirror_design_effect, rule)
            };
            Some(MirrorRow {
                strategy: id.clone(),
                slot_a: read(&|r| half(r, Faction::A)),
                left_spawn: read(&|r| half(r, r.orientation.left())),
            })
        })
        .collect();
    // The pooled readings decide PASS; a per-mirror reading can only FAIL
    // the criterion, when its own interval lies wholly outside tolerance.
    // An undetermined mirror is left open: ten mirrors at 2 matches a seed
    // cannot each be resolved to +/-5 points (F-038).
    let mirror_fail = per_mirror
        .iter()
        .any(|r| r.slot_a.status == Status::Fail || r.left_spawn.status == Status::Fail);
    let pooled_status = Status::all([slot_a.status, left_spawn.status]);
    SeatBias {
        status: if mirror_fail { Status::Fail } else { pooled_status },
        tolerance: spec.mirror_tolerance,
        slot_a,
        left_spawn,
        mirrors: per_mirror,
    }
}

fn termination(records: &[MatchRecord], m: &WinMatrix, spec: &GateSpec) -> Termination {
    let d = LengthDistribution::of(records, spec.band);
    let clusters = |decided_only: bool| {
        distinct(
            records
                .iter()
                .filter(|r| !decided_only || r.result.is_decided())
                .map(|r| key(m, r))
                .collect(),
        )
        .len() as u32
    };
    let all = clusters(false);
    let of_all = |count: u32, rule| Reading::proportion(2 * count as u64, d.total, all, spec.icc, rule);
    let below = of_all(d.below_band, Rule::AtMost(0.5));
    let beyond = of_all(d.above_band + d.timeouts, Rule::AtMost(0.5));
    let timeout_rate = of_all(d.timeouts, Rule::AtMost(spec.max_timeout_rate));
    let band_share = Reading::proportion(
        2 * d.in_band as u64,
        d.decided,
        clusters(true),
        spec.icc,
        Rule::AtLeast(spec.min_band_share),
    );
    Termination {
        status: Status::all([band_share.status, timeout_rate.status]),
        band: spec.band,
        min_band_share: spec.min_band_share,
        max_timeout_rate: spec.max_timeout_rate,
        below,
        beyond,
        band_share,
        timeout_rate,
        decided_median: d.decided_percentile(50),
        all_timeout: d.is_all_timeout(),
    }
}
