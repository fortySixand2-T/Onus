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

use crate::batch::{MatchRecord, MatchResult};
use crate::sim::spatial::Faction;

/// Half-wins for one decided match's winner: a whole win is two halves, so a
/// mutual loss is representable as an integer.
const WIN: u32 = 2;
/// A mutual loss: half a win to each side.
const HALF: u32 = 1;

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
