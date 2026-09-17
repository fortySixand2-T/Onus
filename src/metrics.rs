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
    pub fn row_mean(&self, i: usize) -> RowMean {
        let rates: Vec<f64> = (0..self.len())
            .filter(|&j| j != i)
            .filter_map(|j| self.rate(i, j))
            .collect();
        let cells = rates.len();
        RowMean {
            mean: (cells > 0).then(|| rates.iter().sum::<f64>() / cells as f64),
            cells,
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
