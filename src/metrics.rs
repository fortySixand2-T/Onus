//! Balance metrics (B3): what a batch of [`MatchRecord`]s *says*.
//!
//! A pure function of records — no `App`, no sim, no stepping. Anything here
//! can be computed from a batch just played or from a report read back from
//! disk, and gives the same answer both ways.
//!
//! ## The win-rate matrix
//!
//! [`WinMatrix`] is `W[i][j] = P(s_i beats s_j)`. Three rules decide every
//! number in it (F-024):
//!
//! - **Both slot orderings aggregate.** For `i != j`, cell `(i, j)` counts
//!   every record where `i` played `j`: `i` in slot A *and* `i` in slot B, in
//!   both orientations. That is what cancels the slot and spawn edges B2
//!   measured, and it makes the matrix complementary: whenever both are
//!   defined, `W[i][j] + W[j][i] = 1` — exactly, because wins are accumulated
//!   as integer half-wins and divided once.
//! - **The three outcomes are not interchangeable.** A
//!   [`MatchResult::Decided`] is a whole win for whoever played the surviving
//!   faction; a [`MatchResult::MutualLoss`] is a decided draw, half a win to
//!   each side; a [`MatchResult::Timeout`] is *undecided* — excluded from the
//!   rate and counted on its own ([`Cell::n_timeout`]).
//! - **Undecided is undefined.** A cell with no decided match has no rate
//!   (`None`), never 0.5. An all-timeout matchup reported as 0.5 would read as
//!   perfectly balanced, which is exactly the lie B3 exists to refuse.
//!
//! ## Order is an outcome
//!
//! Row and column labels are read out of [`MatchRecord::strategies`], never
//! from a caller's list, in **order of first appearance** in the slice (slot A
//! before slot B within a record). For a [`crate::batch::run_batch`] result
//! that is RON order. No map is iterated to produce it.
//!
//! ## Match length
//!
//! [`LengthDistribution`] places a batch against the design's 5–8 minute
//! target ([`LengthBand`]) on two bases it never mixes: **decided** matches
//! (band counts, band share, decided percentiles) and **all** matches with a
//! timeout entered at its cap (the timeout rate, and [`crate::batch::Tally`]'s
//! censored percentiles, F-031).

use serde::{Deserialize, Serialize};

use crate::batch::{MatchRecord, MatchResult};
use crate::headless::SIM_HZ;
use crate::sim::spatial::Faction;

/// Half-wins for one decided match's winner: a whole win is two halves, so a
/// mutual loss is representable as an integer.
const WIN: u32 = 2;
/// A mutual loss: half a win to each side.
const HALF: u32 = 1;
/// The two-sided 95% normal quantile, for [`Cell::wilson_interval`]. 95% is the
/// confidence every interval quoted in FINDINGS (F-031) is quoted at; keeping
/// one constant keeps the code and the ledger talking about the same width.
const WILSON_Z: f64 = 1.959_963_985_3;

/// One cell of a [`WinMatrix`]: the row strategy's record against the column
/// strategy.
///
/// Off the diagonal, `half_wins` are the **row** strategy's, counted from
/// either slot. **On the diagonal** (a mirror, `i` vs `i`) "does `i` beat `i`"
/// has no meaning — a mirror always beats itself — so the diagonal cell is
/// defined as **the slot-A share of that mirror**: `half_wins` are slot A's,
/// a mutual loss is a half, timeouts are excluded. That is the quantity the
/// claim "mirror diagonal ≈ 0.5" is actually about: that neither seat of an
/// identical matchup has an edge.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cell {
    /// The row side's wins, in half-win units (win = 2, mutual loss = 1).
    pub half_wins: u32,
    /// Matches the sim decided — a win either way or a mutual loss. The
    /// denominator of the rate.
    pub n_decided: u32,
    /// Matches that ran into the tick cap. Not in the rate; reported beside it.
    pub n_timeout: u32,
}

impl Cell {
    /// The row side's win rate over decided matches, or `None` if nothing
    /// was decided — an unplayed cell and an all-timeout cell are both
    /// unknown, never 0.5. Tell them apart with [`Cell::played`].
    pub fn rate(&self) -> Option<f64> {
        (self.n_decided > 0).then(|| self.half_wins as f64 / (WIN * self.n_decided) as f64)
    }

    /// The **95% Wilson score interval** around [`Cell::rate`], or `None` if
    /// nothing was decided.
    ///
    /// Why an interval at all, and why this one: a rate read off a handful of
    /// matches is a number with a width, and B3's pentagon was reading 8-match
    /// cells as statements about the design (F-031 — `sentinel > ripper` was
    /// recorded as a broken link at 4 of 8, and reproduces at 64-72% once the
    /// sample is 100). The Wilson score interval is the standard choice for a
    /// proportion at small `n` and near 0 or 1, where the normal approximation
    /// is worst and where these cells actually live; it never leaves `[0, 1]`
    /// and it is defined at 0/n and n/n, which a Wald interval is not.
    ///
    /// Pure arithmetic on `(half_wins, n_decided)` — no sampling, no clock, so
    /// two runs of the same batch get the same interval to the bit. The
    /// matches behind a cell are **not** independent (a seed contributes four
    /// correlated matches; F-031 measured a design effect of 1.51), so this is
    /// an optimistic width, not a conservative one.
    pub fn wilson_interval(&self) -> Option<(f64, f64)> {
        let p = self.rate()?;
        Some(wilson_bounds(p, self.n_decided as f64))
    }

    /// The row side's wins as a number of matches (a mutual loss is 0.5).
    pub fn wins(&self) -> f64 {
        self.half_wins as f64 / WIN as f64
    }

    /// Every match this cell counts, decided or not.
    pub fn played(&self) -> u32 {
        self.n_decided + self.n_timeout
    }

    fn add(&mut self, row_half_wins: Option<u32>) {
        match row_half_wins {
            Some(h) => {
                self.n_decided += 1;
                self.half_wins += h;
            }
            None => self.n_timeout += 1,
        }
    }
}

/// The 95% Wilson score interval for a proportion `p` observed over `n`
/// trials — [`Cell::wilson_interval`]'s arithmetic, exposed so the kill gate
/// ([`crate::gate`]) reads its intervals with the same `z` and the same
/// expression. `n` is real so an **effective** sample (`n / design effect`)
/// can be passed. Clamped to `[0, 1]`. `n` must be positive.
pub fn wilson_bounds(p: f64, n: f64) -> (f64, f64) {
    let z2 = WILSON_Z * WILSON_Z;
    let denom = 1.0 + z2 / n;
    let centre = (p + z2 / (2.0 * n)) / denom;
    let half = (WILSON_Z / denom) * (p * (1.0 - p) / n + z2 / (4.0 * n * n)).sqrt();
    ((centre - half).max(0.0), (centre + half).min(1.0))
}

/// A strategy's overall strength: the mean of its **defined off-diagonal**
/// cell rates, and how many cells that is over — a mean of one cell is visibly
/// not a mean of nine.
///
/// The mirror is excluded (it measures seat bias, not strength) and so is any
/// undefined cell. A row with no defined off-diagonal cell has no mean.
/// A mean of *cell rates*, not of pooled matches: each opponent is one reading
/// of strength, however many of its matches were decided.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RowMean {
    pub mean: Option<f64>,
    pub cells: usize,
}

/// The win-rate matrix of a set of match records. See the module docs for the
/// rules, and [`Cell`] for what the diagonal means.
///
/// `Default` is the empty matrix — what an empty slice produces: no rows, no
/// defined cells, and every lookup `None`. There is no roster to name, so it
/// is not a matrix of zeros.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WinMatrix {
    /// Strategy ids, in first-appearance order. Row `i` and column `i` are both
    /// `ids[i]`.
    ids: Vec<String>,
    /// `ids.len()²` cells, row-major.
    cells: Vec<Cell>,
}

impl WinMatrix {
    /// Build the matrix from `records`. Every record lands in exactly the
    /// cells it belongs to: an off-diagonal record in `(a, b)` and `(b, a)`,
    /// a mirror in `(a, a)` once.
    pub fn of(records: &[MatchRecord]) -> Self {
        let mut ids: Vec<String> = Vec::new();
        for r in records {
            for id in &r.strategies {
                if !ids.iter().any(|known| known == id) {
                    ids.push(id.clone());
                }
            }
        }
        let n = ids.len();
        let mut m = WinMatrix {
            cells: vec![Cell::default(); n * n],
            ids,
        };
        for r in records {
            let a = m.index(&r.strategies[0]).expect("collected above");
            let b = m.index(&r.strategies[1]).expect("collected above");
            // Half-wins for slot A and slot B; `None` for an undecided match.
            let (for_a, for_b) = match r.result {
                MatchResult::Decided(Faction::A) => (Some(WIN), Some(0)),
                MatchResult::Decided(Faction::B) => (Some(0), Some(WIN)),
                MatchResult::MutualLoss => (Some(HALF), Some(HALF)),
                MatchResult::Timeout => (None, None),
            };
            // A mirror is one cell, credited to slot A (the diagonal's
            // definition); otherwise each side's row gets its own share.
            m.cells[a * n + b].add(for_a);
            if a != b {
                m.cells[b * n + a].add(for_b);
            }
        }
        m
    }

    /// Strategy ids, row (and column) order.
    pub fn ids(&self) -> &[String] {
        &self.ids
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// The row/column of strategy `id`, if any record named it. A linear scan:
    /// the roster is small and nothing here may depend on hash order.
    pub fn index(&self, id: &str) -> Option<usize> {
        self.ids.iter().position(|s| s == id)
    }

    /// Cell `(i, j)`, or `None` if either index is out of range.
    pub fn cell(&self, i: usize, j: usize) -> Option<&Cell> {
        let n = self.len();
        (i < n && j < n).then(|| &self.cells[i * n + j])
    }

    /// Cell `(row, col)` by strategy id.
    pub fn get(&self, row: &str, col: &str) -> Option<&Cell> {
        self.cell(self.index(row)?, self.index(col)?)
    }

    /// `W[i][j]`: `None` if out of range or undecided.
    pub fn rate(&self, i: usize, j: usize) -> Option<f64> {
        self.cell(i, j)?.rate()
    }

    /// The mirror reading for strategy `i`: slot A's share of its decided
    /// mirror matches. ≈ 0.5 means neither seat has an edge.
    pub fn diagonal(&self, i: usize) -> Option<f64> {
        self.rate(i, i)
    }

    /// Row `i`'s overall strength — see [`RowMean`]. Out of range is an
    /// undefined mean over zero cells.
    ///
    /// **A function of the multiset of the row's defined cells, never of their
    /// order** (F-024). Column order is first appearance in the records, and
    /// float addition is not associative, so summing `f64` rates in column
    /// order would let a reordering of the same batch move a mean across a
    /// gate threshold. Instead the mean is computed **exactly** as a rational
    /// from each cell's integers and converted to `f64` once.
    pub fn row_mean(&self, i: usize) -> RowMean {
        let cells: Vec<&Cell> = (0..self.len())
            .filter(|&j| j != i)
            .filter_map(|j| self.cell(i, j))
            .filter(|c| c.n_decided > 0)
            .collect();
        RowMean {
            mean: exact_mean(&cells),
            cells: cells.len(),
        }
    }

    /// Every row's [`RowMean`], labelled, in matrix order.
    pub fn row_means(&self) -> Vec<(String, RowMean)> {
        (0..self.len())
            .map(|i| (self.ids[i].clone(), self.row_mean(i)))
            .collect()
    }

    /// Cells with a rate, diagonal included. Zero means the batch decided
    /// nothing and the matrix says nothing about balance.
    pub fn defined_cells(&self) -> usize {
        self.cells.iter().filter(|c| c.rate().is_some()).count()
    }

    /// Decided **records** the matrix was built from (each counted once, not
    /// once per cell it lands in).
    pub fn decided(&self) -> u32 {
        self.per_record(|c| c.n_decided)
    }

    /// Timed-out **records** the matrix was built from.
    pub fn timeouts(&self) -> u32 {
        self.per_record(|c| c.n_timeout)
    }

    /// Sum a per-cell count back to records: the upper triangle holds each
    /// off-diagonal record once, the diagonal each mirror once.
    fn per_record(&self, f: impl Fn(&Cell) -> u32) -> u32 {
        let n = self.len();
        (0..n)
            .flat_map(|i| (i..n).map(move |j| (i, j)))
            .map(|(i, j)| f(&self.cells[i * n + j]))
            .sum()
    }
}

/// The mean of the cells' rates: the `f64` **nearest** the exact rational
/// mean (ties to even). `None` for no cells.
///
/// `mean = (1/k) · Σ h_i / (2·n_i)`. It is computed over a common denominator
/// `D = k · Π 2·n_i` with an arbitrary-precision integer ([`Big`]), then
/// rounded once by long division ([`nearest`]). Integer addition and
/// multiplication are exact and commutative, so the result is a function of the
/// multiset of cells — never of column order — and there is **no fallback
/// path**: an earlier `u128` version overflowed (and fell back to a float sum)
/// once the lcm of the cells' denominators passed ~2^128/k, which with timeouts
/// making per-cell `n_decided` differ is reachable at realistic scale (F-024).
///
/// Cost: `k` cells of at most 33-bit denominators give a numerator of at most
/// `33·k + 32` bits; `k` multiplications per term, `k` terms — trivial for any
/// roster a batch can play.
fn exact_mean(cells: &[&Cell]) -> Option<f64> {
    if cells.is_empty() {
        return None;
    }
    let dens: Vec<u64> = cells.iter().map(|c| WIN as u64 * c.n_decided as u64).collect();
    let mut num = Big::from(0);
    for (i, c) in cells.iter().enumerate() {
        let mut term = Big::from(c.half_wins as u64);
        for (j, &d) in dens.iter().enumerate() {
            if j != i {
                term.mul_small(d);
            }
        }
        num.add(&term);
    }
    let mut den = Big::from(cells.len() as u64);
    for &d in &dens {
        den.mul_small(d);
    }
    Some(nearest(num, &den))
}

/// The `f64` nearest `num / den`, ties to even, for `0 <= num <= den`, `den > 0`
/// (a mean of rates lies in `[0, 1]`).
///
/// Long division, one bit at a time: skip the leading zero bits after the
/// binary point (at most `den`'s bit length of them), take 53 significant bits
/// and a guard bit, and let the remainder be the sticky bit. The significand is
/// below 2^53 and the scale a power of two no smaller than 2^-(bits + 54), far
/// above the subnormal range for any denominator that fits in memory, so the
/// final multiplication is exact.
fn nearest(num: Big, den: &Big) -> f64 {
    use std::cmp::Ordering;
    match num.cmp(den) {
        Ordering::Equal => return 1.0,
        Ordering::Greater => unreachable!("a mean of rates is at most 1"),
        Ordering::Less => {}
    }
    if num.is_zero() {
        return 0.0;
    }
    let mut r = num;
    // The next bit of the quotient, and the remainder updated past it.
    let next_bit = |r: &mut Big| {
        r.shl1();
        if r.cmp(den) != Ordering::Less {
            r.sub(den);
            true
        } else {
            false
        }
    };
    let mut exp: i32 = 0; // value = significand · 2^-exp
    loop {
        exp += 1;
        if next_bit(&mut r) {
            break;
        }
    }
    let mut sig: u64 = 1;
    for _ in 0..52 {
        exp += 1;
        sig = (sig << 1) | u64::from(next_bit(&mut r));
    }
    let guard = next_bit(&mut r);
    let sticky = !r.is_zero();
    if guard && (sticky || sig & 1 == 1) {
        sig += 1; // may reach 2^53: still exact as an f64
    }
    let mut scale = 1.0f64;
    for _ in 0..exp {
        scale *= 0.5;
    }
    sig as f64 * scale
}

/// A minimal unsigned big integer — little-endian `u32` limbs, no leading zero
/// limbs — with only what [`exact_mean`] and [`nearest`] need.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Big(Vec<u32>);

impl From<u64> for Big {
    fn from(v: u64) -> Self {
        let mut b = Big(vec![v as u32, (v >> 32) as u32]);
        b.trim();
        b
    }
}

impl Big {
    fn trim(&mut self) {
        while self.0.last() == Some(&0) {
            self.0.pop();
        }
    }

    fn is_zero(&self) -> bool {
        self.0.is_empty()
    }

    fn mul_small(&mut self, m: u64) {
        let mut out = vec![0u32; self.0.len() + 2];
        for (shift, part) in [(0, m & 0xFFFF_FFFF), (1, m >> 32)] {
            let mut carry = 0u64;
            for (i, &limb) in self.0.iter().enumerate() {
                let t = out[i + shift] as u64 + limb as u64 * part + carry;
                out[i + shift] = t as u32;
                carry = t >> 32;
            }
            let mut k = self.0.len() + shift;
            while carry > 0 {
                let t = out[k] as u64 + carry;
                out[k] = t as u32;
                carry = t >> 32;
                k += 1;
            }
        }
        self.0 = out;
        self.trim();
    }

    fn add(&mut self, other: &Big) {
        if self.0.len() < other.0.len() {
            self.0.resize(other.0.len(), 0);
        }
        let mut carry = 0u64;
        for i in 0..self.0.len() {
            let t = self.0[i] as u64 + other.0.get(i).copied().unwrap_or(0) as u64 + carry;
            self.0[i] = t as u32;
            carry = t >> 32;
        }
        if carry > 0 {
            self.0.push(carry as u32);
        }
    }

    /// `self -= other`; requires `self >= other`.
    fn sub(&mut self, other: &Big) {
        let mut borrow = 0i64;
        for i in 0..self.0.len() {
            let t = self.0[i] as i64 - other.0.get(i).copied().unwrap_or(0) as i64 - borrow;
            self.0[i] = t.rem_euclid(1 << 32) as u32;
            borrow = i64::from(t < 0);
        }
        debug_assert_eq!(borrow, 0, "Big::sub underflow");
        self.trim();
    }

    fn shl1(&mut self) {
        let mut carry = 0u32;
        for limb in &mut self.0 {
            let next = *limb >> 31;
            *limb = (*limb << 1) | carry;
            carry = next;
        }
        if carry > 0 {
            self.0.push(carry);
        }
    }

    fn cmp(&self, other: &Big) -> std::cmp::Ordering {
        self.0
            .len()
            .cmp(&other.0.len())
            .then_with(|| self.0.iter().rev().cmp(other.0.iter().rev()))
    }
}

// ---- match length -----------------------------------------------------------

/// The percentiles every length report prints, in this order.
pub const REPORTED_PERCENTILES: [u32; 7] = [0, 10, 25, 50, 75, 90, 100];

/// The design's target length for a match — DESIGN_BRIEF's 5–8 minute arc —
/// in sim ticks, **inclusive at both ends**.
///
/// Harness configuration, like the tick cap (F-020, F-029): it says what a
/// report compares against, not how the game plays, so it is not content.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LengthBand {
    pub min_ticks: u32,
    pub max_ticks: u32,
}

impl LengthBand {
    /// `lo..=hi` whole minutes of play at [`SIM_HZ`].
    pub const fn minutes(lo: u32, hi: u32) -> Self {
        Self {
            min_ticks: lo * 60 * SIM_HZ,
            max_ticks: hi * 60 * SIM_HZ,
        }
    }

    pub fn contains(&self, ticks: u32) -> bool {
        (self.min_ticks..=self.max_ticks).contains(&ticks)
    }
}

impl Default for LengthBand {
    /// DESIGN_BRIEF's 5–8 minutes.
    fn default() -> Self {
        Self::minutes(5, 8)
    }
}

/// How long a batch's matches ran, on **two bases that are never mixed**.
///
/// - **decided** — matches the sim ended ([`MatchResult::is_decided`]: a win
///   or a mutual loss). The decided percentiles and the band counts are over
///   these only: a timeout never finished, so it has no length to place in a
///   band.
/// - **all** — every match, a timeout entered at the tick it was stopped (the
///   cap). This is [`crate::batch::Tally`]'s basis (F-031); a censored median
///   is flattered by short games and a censored tail is clipped at the cap, so
///   it is reported *beside* the decided basis, never instead of it.
///
/// [`LengthDistribution::band_share`] is in-band ÷ **decided**;
/// [`LengthDistribution::timeout_rate`] is timeouts ÷ **all**. Two rates, two
/// denominators, reported separately (B3.5: band share is the design metric,
/// the timeout rate the stalemate signal).
///
/// Percentiles are nearest-rank, `ceil(p·n/100)`, in integer arithmetic — a
/// length some match actually had, and no float rounding in the rank.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LengthDistribution {
    pub band: LengthBand,
    pub total: u32,
    /// Matches the sim ended, mutual losses included.
    pub decided: u32,
    pub timeouts: u32,
    /// Decided matches shorter than the band.
    pub below_band: u32,
    /// Decided matches inside the band (inclusive).
    pub in_band: u32,
    /// Decided matches longer than the band.
    pub above_band: u32,
    /// Decided lengths, ascending.
    decided_lengths: Vec<u32>,
    /// Every length, timeouts at their cap, ascending.
    all_lengths: Vec<u32>,
}

impl LengthDistribution {
    pub fn of(records: &[MatchRecord], band: LengthBand) -> Self {
        let mut d = LengthDistribution {
            band,
            total: 0,
            decided: 0,
            timeouts: 0,
            below_band: 0,
            in_band: 0,
            above_band: 0,
            decided_lengths: Vec::new(),
            all_lengths: Vec::with_capacity(records.len()),
        };
        for r in records {
            d.total += 1;
            d.all_lengths.push(r.ticks);
            if !r.result.is_decided() {
                d.timeouts += 1;
                continue;
            }
            d.decided += 1;
            d.decided_lengths.push(r.ticks);
            if r.ticks < band.min_ticks {
                d.below_band += 1;
            } else if r.ticks > band.max_ticks {
                d.above_band += 1;
            } else {
                d.in_band += 1;
            }
        }
        d.decided_lengths.sort_unstable();
        d.all_lengths.sort_unstable();
        d
    }

    /// In-band share of **decided** matches; `None` if nothing was decided —
    /// an all-timeout batch has no band share, not a band share of zero.
    pub fn band_share(&self) -> Option<f64> {
        (self.decided > 0).then(|| self.in_band as f64 / self.decided as f64)
    }

    /// Timeouts over **all** matches; `None` for an empty batch.
    pub fn timeout_rate(&self) -> Option<f64> {
        (self.total > 0).then(|| self.timeouts as f64 / self.total as f64)
    }

    /// Every match hit the cap: the batch measured nothing about the game.
    pub fn is_all_timeout(&self) -> bool {
        self.total > 0 && self.timeouts == self.total
    }

    /// The `p`-th percentile (`0..=100`) of **decided** match length, ticks.
    pub fn decided_percentile(&self, p: u32) -> Option<u32> {
        nearest_rank(&self.decided_lengths, p)
    }

    /// The `p`-th percentile of **all** match lengths, timeouts at the cap.
    pub fn all_percentile(&self, p: u32) -> Option<u32> {
        nearest_rank(&self.all_lengths, p)
    }

    /// Decided lengths, ascending — for interval computations over the order
    /// statistics.
    pub fn decided_lengths(&self) -> &[u32] {
        &self.decided_lengths
    }

    /// The serializable reading: counts, both rates, and both bases at
    /// [`REPORTED_PERCENTILES`].
    pub fn summary(&self) -> LengthSummary {
        let at = |f: &dyn Fn(u32) -> Option<u32>| {
            REPORTED_PERCENTILES.iter().map(|&p| (p, f(p))).collect()
        };
        LengthSummary {
            band: self.band,
            total: self.total,
            decided: self.decided,
            timeouts: self.timeouts,
            below_band: self.below_band,
            in_band: self.in_band,
            above_band: self.above_band,
            band_share: self.band_share(),
            timeout_rate: self.timeout_rate(),
            decided_quantiles: at(&|p| self.decided_percentile(p)),
            all_quantiles: at(&|p| self.all_percentile(p)),
        }
    }
}

/// A [`LengthDistribution`] reduced to what a report carries. Every quantile
/// list is `(percent, ticks)`, `None` where the basis is empty.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LengthSummary {
    pub band: LengthBand,
    pub total: u32,
    pub decided: u32,
    pub timeouts: u32,
    pub below_band: u32,
    pub in_band: u32,
    pub above_band: u32,
    /// In-band ÷ decided.
    pub band_share: Option<f64>,
    /// Timeouts ÷ all.
    pub timeout_rate: Option<f64>,
    /// Over decided matches only.
    pub decided_quantiles: Vec<(u32, Option<u32>)>,
    /// Over every match, timeouts entered at the cap.
    pub all_quantiles: Vec<(u32, Option<u32>)>,
}

/// Nearest rank: the value at rank `ceil(p·n/100)` (1-based, at least 1).
fn nearest_rank(sorted: &[u32], p: u32) -> Option<u32> {
    if sorted.is_empty() {
        return None;
    }
    let n = sorted.len() as u64;
    let rank = (p.min(100) as u64 * n).div_ceil(100).clamp(1, n);
    sorted.get(rank as usize - 1).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn big(v: u128) -> Big {
        let mut b = Big::from((v >> 64) as u64);
        b.mul_small(1 << 32);
        b.mul_small(1 << 32);
        b.add(&Big::from(v as u64));
        b
    }

    /// A cell with `wins` of `n` decided, and nothing else.
    fn decided(wins: u32, n: u32) -> Cell {
        Cell {
            half_wins: wins * WIN,
            n_decided: n,
            n_timeout: 0,
        }
    }

    /// Every interval FINDINGS F-031 publishes, recomputed here to one decimal
    /// place. If this drifts, either the code is wrong or the ledger is — and
    /// the whole point of the pentagon's interval is that the two agree.
    #[test]
    fn the_wilson_interval_reproduces_every_one_f031_quotes() {
        let cases: [(u32, u32, f64, f64); 8] = [
            // F-030's `sentinel > ripper`: 4 of 8 "FAILS" — half-width 28.
            (4, 8, 21.5, 78.5),
            // The 8-seed table: holds / undetermined, and the two 100% cells.
            (22, 32, 51.4, 82.0),
            (17, 32, 36.4, 69.1),
            (31, 32, 84.3, 99.4),
            (30, 30, 88.6, 100.0),
            (29, 32, 75.8, 96.8),
            // The 25-seed `sentinel > ripper`, and the pooled coin flip.
            (71, 99, 62.2, 79.6),
            (236, 430, 50.2, 59.5),
        ];
        for (wins, n, lo, hi) in cases {
            let (got_lo, got_hi) = decided(wins, n).wilson_interval().expect("decided");
            assert_eq!(
                ((1000.0 * got_lo).round() / 10.0, (1000.0 * got_hi).round() / 10.0),
                (lo, hi),
                "{wins}/{n}"
            );
        }
    }

    /// An interval needs a decided match to exist, a mutual loss counts as half
    /// a win in it, and it never leaves `[0, 1]` however extreme the cell.
    #[test]
    fn an_interval_exists_exactly_when_a_rate_does_and_stays_in_range() {
        let nothing = Cell::default();
        assert_eq!(nothing.rate(), None);
        assert_eq!(nothing.wilson_interval(), None);
        let all_timeout = Cell {
            half_wins: 0,
            n_decided: 0,
            n_timeout: 9,
        };
        assert_eq!(all_timeout.wilson_interval(), None, "undecided is not 50/50");

        // Eight decided draws: the rate is exactly 0.5 and the interval is the
        // same one 4-of-8 wins gets — a dead-even cell, not a readable one.
        let draws = Cell {
            half_wins: 8 * HALF,
            n_decided: 8,
            n_timeout: 0,
        };
        assert_eq!(draws.rate(), Some(0.5));
        assert_eq!(draws.wilson_interval(), decided(4, 8).wilson_interval());

        for (wins, n) in [(0, 1), (1, 1), (0, 300), (300, 300)] {
            let (lo, hi) = decided(wins, n).wilson_interval().expect("decided");
            assert!((0.0..=1.0).contains(&lo) && (0.0..=1.0).contains(&hi), "{wins}/{n}");
            assert!(lo <= hi);
        }
        // The interval contains its own point estimate, and narrows with n.
        let (lo8, hi8) = decided(6, 8).wilson_interval().expect("decided");
        let (lo80, hi80) = decided(60, 80).wilson_interval().expect("decided");
        assert!(lo8 < 0.75 && 0.75 < hi8);
        assert!(hi80 - lo80 < hi8 - lo8, "80 matches is narrower than 8");
    }

    /// Expected values are Python `float(Fraction(n, d))` — correctly rounded.
    #[test]
    fn nearest_is_correctly_rounded_ties_to_even() {
        let cases: [(u128, u128, f64); 10] = [
            (0, 7, 0.0),
            (5, 5, 1.0),
            (1, 3, 0.3333333333333333),
            (2, 3, 0.6666666666666666),
            (1, 10, 0.1),
            (7, 10, 0.7),
            // Exactly halfway, significand even: rounds down.
            ((1 << 53) + 1, 1 << 54, 0.5),
            // Exactly halfway, significand odd: rounds up.
            ((1 << 53) + 3, 1 << 54, 0.5000000000000002),
            // Just above halfway (sticky bit): rounds down only below halfway.
            ((1 << 60) + 1, 1 << 61, 0.5),
            ((1 << 100) - 1, (1 << 127) + 5, 7.450580596923828e-09),
        ];
        for (n, d, want) in cases {
            assert_eq!(nearest(big(n), &big(d)), want, "{n}/{d}");
        }
    }

    #[test]
    fn big_arithmetic_matches_u128() {
        let a: u128 = 0xDEAD_BEEF_1234_5678_9ABC_DEF0_0FED_CBA9;
        let mut x = big(a >> 80);
        x.mul_small(0xFFFF_FFFF_FFF1);
        assert_eq!(x, big((a >> 80) * 0xFFFF_FFFF_FFF1));
        let mut y = big(a >> 2);
        y.add(&big(a >> 2));
        assert_eq!(y, big((a >> 2) * 2));
        y.sub(&big(a >> 3));
        assert_eq!(y, big((a >> 2) * 2 - (a >> 3)));
        let mut z = big(a >> 1);
        z.shl1();
        assert_eq!(z, big((a >> 1) << 1));
        assert_eq!(big(5).cmp(&big(1 << 70)), std::cmp::Ordering::Less);
        assert!(Big::from(0).is_zero());
    }
}
