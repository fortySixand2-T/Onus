//! L2 integration tests for **B3 AC1** — the win-rate matrix.
//!
//! `W[i][j] = P(s_i beats s_j)`, computed by [`onus::metrics::WinMatrix`] as a
//! pure function of a slice of [`MatchRecord`]s. What is encoded:
//!
//!   - **both slot orderings aggregate** — `i` in slot A vs `j` in slot B and
//!     `i` in slot B vs `j` in slot A both land in `W[i][j]`, in both
//!     orientations;
//!   - **complementarity** — for `i != j`, `W[i][j] + W[j][i] = 1` exactly
//!     (in integer half-wins), whenever both are defined;
//!   - **the three outcomes differ** — a `Decided` result is a whole win, a
//!     `MutualLoss` is half a win to each side, a `Timeout` is excluded from
//!     the rate and counted on its own;
//!   - **undecided is undefined** — a cell with no decided match is `None`,
//!     never 0.5; an all-timeout run has no defined cell and no defined row
//!     mean (B3's critic probe: *flagged, not reported as balanced*);
//!   - **the mirror diagonal** is the slot-A share of the mirror;
//!   - **row means** exclude the diagonal and undefined cells and say how many
//!     cells they average;
//!   - **labels and order come from the records**, first appearance, which for
//!     a `run_batch` result is RON order;
//!   - **empty input** is an explicit empty matrix.
//!
//! Almost everything runs on synthetic records so every expected value is
//! known exactly; one test plays a real (small) batch end to end.

use onus::batch::{self, BatchSettings, MatchRecord, MatchResult, ProductionCounts};
use onus::headless::{self, Orientation};
use onus::metrics::{Cell, WinMatrix};
use onus::sim::spatial::Faction;

const A_WINS: MatchResult = MatchResult::Decided(Faction::A);
const B_WINS: MatchResult = MatchResult::Decided(Faction::B);
const DRAW: MatchResult = MatchResult::MutualLoss;
const CAPPED: MatchResult = MatchResult::Timeout;

fn rec(a: &str, b: &str, result: MatchResult) -> MatchRecord {
    oriented(a, b, result, Orientation::Normal)
}

fn oriented(a: &str, b: &str, result: MatchResult, orientation: Orientation) -> MatchRecord {
    MatchRecord {
        strategies: [a.to_string(), b.to_string()],
        seed: 0,
        result,
        orientation,
        ticks: 1,
        produced: ProductionCounts::default(),
    }
}

fn idx(m: &WinMatrix, id: &str) -> usize {
    m.index(id).unwrap_or_else(|| panic!("`{id}` is a row of the matrix"))
}

fn cell<'m>(m: &'m WinMatrix, row: &str, col: &str) -> &'m Cell {
    m.get(row, col)
        .unwrap_or_else(|| panic!("({row}, {col}) is a cell of the matrix"))
}

/// Every defined off-diagonal pair sums to exactly one win per decided match,
/// in integer half-wins, and to 1.0 in the float rate.
fn assert_complementary(m: &WinMatrix) -> usize {
    let mut checked = 0;
    for i in 0..m.len() {
        for j in 0..m.len() {
            if i == j {
                continue;
            }
            let ij = m.cell(i, j).expect("in range");
            let ji = m.cell(j, i).expect("in range");
            assert_eq!(ij.n_decided, ji.n_decided, "({i},{j}) counts the same matches");
            assert_eq!(ij.n_timeout, ji.n_timeout, "({i},{j}) counts the same timeouts");
            match (ij.rate(), ji.rate()) {
                (Some(x), Some(y)) => {
                    assert_eq!(
                        ij.half_wins + ji.half_wins,
                        2 * ij.n_decided,
                        "W[{i}][{j}] + W[{j}][{i}] must be exactly 1"
                    );
                    assert!((x + y - 1.0).abs() < 1e-12, "W[{i}][{j}]={x} W[{j}][{i}]={y}");
                    checked += 1;
                }
                (None, None) => {}
                other => panic!("({i},{j}) defined on one side only: {other:?}"),
            }
        }
    }
    checked
}

// ---- hand-computed cells ---------------------------------------------------------

#[test]
fn hand_computed_cells_are_exact() {
    // x vs y, from every seat:
    //   x(A) beats y            -> x +1
    //   x(A) loses to y         -> y +1
    //   y(A) loses to x(B)      -> x +1
    //   y(A) vs x(B) mutual     -> x +1/2, y +1/2
    //   x(A) vs y timeout       -> excluded, counted
    //   y(A) vs x(B) timeout    -> excluded, counted
    // decided 4, x has 2.5, y has 1.5, timeouts 2.
    let records = vec![
        rec("x", "y", A_WINS),
        rec("x", "y", B_WINS),
        rec("y", "x", B_WINS),
        rec("y", "x", DRAW),
        rec("x", "y", CAPPED),
        oriented("y", "x", CAPPED, Orientation::Swapped),
    ];
    let m = WinMatrix::of(&records);
    assert_eq!(m.ids(), ["x", "y"]);

    let xy = cell(&m, "x", "y");
    assert_eq!(xy.n_decided, 4);
    assert_eq!(xy.n_timeout, 2);
    assert_eq!(xy.half_wins, 5, "2.5 wins in half-win units");
    assert_eq!(xy.wins(), 2.5);
    assert_eq!(xy.rate(), Some(2.5 / 4.0));

    let yx = cell(&m, "y", "x");
    assert_eq!(yx.n_decided, 4);
    assert_eq!(yx.n_timeout, 2);
    assert_eq!(yx.half_wins, 3);
    assert_eq!(yx.rate(), Some(1.5 / 4.0));

    // No mirror was played: the diagonal is unplayed, hence undefined.
    for id in ["x", "y"] {
        let d = cell(&m, id, id);
        assert_eq!((d.n_decided, d.n_timeout, d.half_wins), (0, 0, 0));
        assert_eq!(d.rate(), None);
        assert_eq!(m.diagonal(idx(&m, id)), None);
    }
    assert_eq!(assert_complementary(&m), 2);
}

#[test]
fn a_three_strategy_matrix_matches_a_hand_table() {
    // p beats q twice (once from each slot); q beats r from slot B once and
    // loses to r from slot A once; r vs p is one mutual loss and one timeout.
    let records = vec![
        rec("p", "q", A_WINS),
        rec("q", "p", B_WINS),
        rec("q", "r", B_WINS),
        rec("r", "q", A_WINS),
        rec("r", "p", DRAW),
        rec("p", "r", CAPPED),
    ];
    let m = WinMatrix::of(&records);
    assert_eq!(m.ids(), ["p", "q", "r"]);
    let expect = |row: &str, col: &str, half: u32, dec: u32, to: u32, r: Option<f64>| {
        let c = cell(&m, row, col);
        assert_eq!(
            (c.half_wins, c.n_decided, c.n_timeout, c.rate()),
            (half, dec, to, r),
            "W[{row}][{col}]"
        );
    };
    expect("p", "q", 4, 2, 0, Some(1.0));
    expect("q", "p", 0, 2, 0, Some(0.0));
    expect("q", "r", 0, 2, 0, Some(0.0));
    expect("r", "q", 4, 2, 0, Some(1.0));
    expect("r", "p", 1, 1, 1, Some(0.5));
    expect("p", "r", 1, 1, 1, Some(0.5));
    assert_eq!(assert_complementary(&m), 6);
}

// ---- both slot orderings aggregate -------------------------------------------------

#[test]
fn a_win_from_either_slot_counts_toward_the_same_cell() {
    let from_a = WinMatrix::of(&[rec("i", "j", A_WINS)]);
    let from_b = WinMatrix::of(&[rec("j", "i", B_WINS)]);
    let both = WinMatrix::of(&[rec("i", "j", A_WINS), rec("j", "i", B_WINS)]);

    assert_eq!(cell(&from_a, "i", "j").rate(), Some(1.0));
    assert_eq!(cell(&from_b, "i", "j").rate(), Some(1.0));
    let c = cell(&both, "i", "j");
    assert_eq!((c.half_wins, c.n_decided), (4, 2), "two wins over two matches");
    assert_eq!(cell(&both, "j", "i").rate(), Some(0.0));
}

#[test]
fn orientation_does_not_split_a_cell() {
    let m = WinMatrix::of(&[
        oriented("i", "j", A_WINS, Orientation::Normal),
        oriented("i", "j", A_WINS, Orientation::Swapped),
        oriented("j", "i", A_WINS, Orientation::Normal),
        oriented("j", "i", B_WINS, Orientation::Swapped),
    ]);
    let c = cell(&m, "i", "j");
    assert_eq!((c.half_wins, c.n_decided, c.n_timeout), (6, 4, 0));
    assert_eq!(c.rate(), Some(0.75));
    assert_eq!(assert_complementary(&m), 2);
}

// ---- the outcomes are not interchangeable -------------------------------------------

#[test]
fn a_mutual_loss_is_half_a_win_to_each_side() {
    let m = WinMatrix::of(&[rec("i", "j", DRAW)]);
    for (r, c) in [("i", "j"), ("j", "i")] {
        let cell = cell(&m, r, c);
        assert_eq!((cell.half_wins, cell.n_decided, cell.n_timeout), (1, 1, 0));
        assert_eq!(cell.wins(), 0.5);
        assert_eq!(cell.rate(), Some(0.5), "a decided draw is a defined 0.5");
    }
}

#[test]
fn a_timeout_is_excluded_from_the_rate_but_counted() {
    let with = WinMatrix::of(&[rec("i", "j", A_WINS), rec("i", "j", CAPPED), rec("j", "i", CAPPED)]);
    let without = WinMatrix::of(&[rec("i", "j", A_WINS)]);
    let c = cell(&with, "i", "j");
    assert_eq!(c.rate(), Some(1.0), "timeouts do not dilute the rate");
    assert_eq!(c.rate(), cell(&without, "i", "j").rate());
    assert_eq!((c.n_decided, c.n_timeout), (1, 2));
    assert_eq!(c.played(), 3);
}

#[test]
fn an_all_timeout_cell_is_undefined_not_balanced() {
    let m = WinMatrix::of(&[rec("i", "j", CAPPED), rec("j", "i", CAPPED), rec("i", "i", CAPPED)]);
    for (r, c) in [("i", "j"), ("j", "i"), ("i", "i")] {
        let cell = cell(&m, r, c);
        assert_eq!(cell.rate(), None, "W[{r}][{c}] must not read as 0.5");
        assert_eq!(cell.n_decided, 0);
        assert!(cell.n_timeout > 0, "the timeouts are still visible");
    }
    assert_eq!(m.diagonal(idx(&m, "i")), None);
}

#[test]
fn an_all_timeout_run_has_no_defined_cell_and_no_defined_mean() {
    let mut records = Vec::new();
    for a in ["u", "v", "w"] {
        for b in ["u", "v", "w"] {
            for o in Orientation::ALL {
                records.push(oriented(a, b, CAPPED, o));
            }
        }
    }
    let m = WinMatrix::of(&records);
    assert_eq!(m.len(), 3);
    assert!(!m.is_empty());
    assert_eq!(m.defined_cells(), 0, "nothing was decided, so nothing is known");
    assert_eq!(m.timeouts(), records.len() as u32);
    assert_eq!(m.decided(), 0);
    for i in 0..m.len() {
        let mean = m.row_mean(i);
        assert_eq!(mean.mean, None, "row {i} must not read as balanced");
        assert_eq!(mean.cells, 0);
        for j in 0..m.len() {
            assert_eq!(m.rate(i, j), None);
        }
    }
    // Every off-diagonal cell counts both slot orderings in both orientations;
    // a mirror cell counts each of its records once.
    assert_eq!(cell(&m, "u", "v").n_timeout, 4);
    assert_eq!(cell(&m, "u", "u").n_timeout, 2);
}

// ---- the mirror diagonal ---------------------------------------------------------------

#[test]
fn the_diagonal_is_the_slot_a_share_of_the_mirror() {
    // Mirror of m: A wins 3, B wins 1, one mutual loss, two timeouts.
    // Slot-A share = (3 + 0.5) / 5 = 0.7.
    let records = vec![
        rec("m", "m", A_WINS),
        oriented("m", "m", A_WINS, Orientation::Swapped),
        rec("m", "m", A_WINS),
        rec("m", "m", B_WINS),
        rec("m", "m", DRAW),
        rec("m", "m", CAPPED),
        oriented("m", "m", CAPPED, Orientation::Swapped),
    ];
    let m = WinMatrix::of(&records);
    assert_eq!(m.ids(), ["m"]);
    let d = cell(&m, "m", "m");
    assert_eq!((d.half_wins, d.n_decided, d.n_timeout), (7, 5, 2));
    assert_eq!(d.rate(), Some(0.7));
    assert_eq!(m.diagonal(0), Some(0.7));

    // A balanced mirror reads 0.5; an all-B mirror reads 0.0 (not 1.0 — "a
    // strategy always beats itself" is not what the diagonal measures).
    let even = WinMatrix::of(&[rec("m", "m", A_WINS), rec("m", "m", B_WINS)]);
    assert_eq!(even.diagonal(0), Some(0.5));
    let b_side = WinMatrix::of(&[rec("m", "m", B_WINS), rec("m", "m", B_WINS)]);
    assert_eq!(b_side.diagonal(0), Some(0.0));
}

#[test]
fn the_diagonal_does_not_leak_into_off_diagonal_cells() {
    let m = WinMatrix::of(&[rec("i", "i", A_WINS), rec("i", "j", B_WINS)]);
    let ij = cell(&m, "i", "j");
    assert_eq!((ij.half_wins, ij.n_decided), (0, 1));
    let ii = cell(&m, "i", "i");
    assert_eq!((ii.half_wins, ii.n_decided), (2, 1));
    let jj = cell(&m, "j", "j");
    assert_eq!(jj.played(), 0);
}

// ---- row means ---------------------------------------------------------------------------

#[test]
fn row_means_exclude_the_diagonal_and_undefined_cells() {
    // s: beats t (1.0), draws u (0.5), all-timeout vs v (None); a lopsided
    // mirror (slot A always) that must not pull the mean.
    let records = vec![
        rec("s", "s", A_WINS),
        rec("s", "s", A_WINS),
        rec("s", "t", A_WINS),
        rec("u", "s", DRAW),
        rec("s", "v", CAPPED),
        rec("v", "s", CAPPED),
    ];
    let m = WinMatrix::of(&records);
    assert_eq!(m.ids(), ["s", "t", "u", "v"]);

    let s = m.row_mean(idx(&m, "s"));
    assert_eq!(s.cells, 2, "t and u are defined; the diagonal and v are not counted");
    assert_eq!(s.mean, Some(0.75));

    let t = m.row_mean(idx(&m, "t"));
    assert_eq!((t.mean, t.cells), (Some(0.0), 1));
    let u = m.row_mean(idx(&m, "u"));
    assert_eq!((u.mean, u.cells), (Some(0.5), 1));
    let v = m.row_mean(idx(&m, "v"));
    assert_eq!((v.mean, v.cells), (None, 0), "only timeouts: no strength to report");

    // `row_means` is the same, labelled, in matrix order.
    let all = m.row_means();
    let names: Vec<&str> = all.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(names, ["s", "t", "u", "v"]);
    assert_eq!(all[0].1, s);
    assert_eq!(all[3].1, v);
}

#[test]
fn a_row_mean_is_the_mean_of_cell_rates_not_of_pooled_matches() {
    // d beats e 3 of 3, loses to f 0 of 1. Pooled that is 3/4; the mean of
    // cell rates — one strength reading per opponent — is 0.5.
    let records = vec![
        rec("d", "e", A_WINS),
        rec("d", "e", A_WINS),
        rec("e", "d", B_WINS),
        rec("d", "f", B_WINS),
    ];
    let m = WinMatrix::of(&records);
    let d = m.row_mean(idx(&m, "d"));
    assert_eq!((d.mean, d.cells), (Some(0.5), 2));
}

#[test]
fn a_strictly_dominant_and_a_strictly_losing_strategy_are_identifiable_by_name() {
    let names = ["alpha", "beta", "gamma", "delta"];
    let mut records = Vec::new();
    for a in names {
        for b in names {
            for o in Orientation::ALL {
                // alpha wins every match it plays except its mirror; delta
                // loses every match it plays; beta vs gamma splits.
                let result = if a == b {
                    if o == Orientation::Normal { A_WINS } else { B_WINS }
                } else if a == "alpha" || b == "delta" {
                    A_WINS
                } else if b == "alpha" || a == "delta" {
                    B_WINS
                } else if o == Orientation::Normal {
                    A_WINS
                } else {
                    B_WINS
                };
                records.push(oriented(a, b, result, o));
            }
        }
    }
    let m = WinMatrix::of(&records);
    let means = m.row_means();
    let find = |id: &str| means.iter().find(|(n, _)| n == id).map(|(_, r)| *r).unwrap();
    assert_eq!(find("alpha").mean, Some(1.0));
    assert_eq!(find("alpha").cells, 3);
    assert_eq!(find("delta").mean, Some(0.0));
    assert_eq!(find("beta").mean, Some(0.5));
    assert_eq!(find("gamma").mean, Some(0.5));

    // Which rows win every defined off-diagonal cell, and which lose every one,
    // read straight off the matrix by name.
    let dominant: Vec<&str> = means
        .iter()
        .filter(|(_, r)| r.mean == Some(1.0))
        .map(|(n, _)| n.as_str())
        .collect();
    let losing: Vec<&str> = means
        .iter()
        .filter(|(_, r)| r.mean == Some(0.0))
        .map(|(n, _)| n.as_str())
        .collect();
    assert_eq!(dominant, ["alpha"]);
    assert_eq!(losing, ["delta"]);
    for i in 0..m.len() {
        assert_eq!(m.diagonal(i), Some(0.5), "each mirror splits by slot");
    }
    assert_eq!(assert_complementary(&m), 12);
}

// ---- labels and order ------------------------------------------------------------------

#[test]
fn row_order_is_first_appearance_in_the_records() {
    // Neither alphabetical nor slot-A-first: "zeta" appears first (slot A of
    // row 0), then "alpha" (slot B of row 0), then "mid".
    let m = WinMatrix::of(&[
        rec("zeta", "alpha", A_WINS),
        rec("mid", "zeta", B_WINS),
        rec("alpha", "mid", DRAW),
    ]);
    assert_eq!(m.ids(), ["zeta", "alpha", "mid"]);
    assert_eq!(m.index("mid"), Some(2));
    assert_eq!(m.index("nope"), None);
    assert!(m.get("zeta", "nope").is_none());
    assert!(m.cell(0, 3).is_none());
    assert_eq!(m.rate(3, 0), None);

    // Reordering the input reorders the labels — and nothing else.
    let n = WinMatrix::of(&[
        rec("alpha", "mid", DRAW),
        rec("mid", "zeta", B_WINS),
        rec("zeta", "alpha", A_WINS),
    ]);
    assert_eq!(n.ids(), ["alpha", "mid", "zeta"]);
    for r in ["zeta", "alpha", "mid"] {
        for c in ["zeta", "alpha", "mid"] {
            assert_eq!(m.get(r, c), n.get(r, c), "W[{r}][{c}] is order-independent");
        }
    }
    // The same input twice is the same matrix.
    assert_eq!(
        m,
        WinMatrix::of(&[
            rec("zeta", "alpha", A_WINS),
            rec("mid", "zeta", B_WINS),
            rec("alpha", "mid", DRAW),
        ])
    );
}

#[test]
fn labels_are_read_from_the_records_only() {
    // A strategy that never appears in a record is not a row, however many
    // the content ships.
    let m = WinMatrix::of(&[rec("rush", "turtle", A_WINS)]);
    assert_eq!(m.ids(), ["rush", "turtle"]);
    assert_eq!(m.len(), 2);
    assert_eq!(m.decided(), 1);
    assert_eq!(m.timeouts(), 0);
}

// ---- empty input -------------------------------------------------------------------------

#[test]
fn empty_input_is_an_explicit_empty_matrix() {
    let m = WinMatrix::of(&[]);
    assert!(m.is_empty());
    assert_eq!(m.len(), 0);
    assert!(m.ids().is_empty());
    assert!(m.row_means().is_empty());
    assert_eq!(m.defined_cells(), 0);
    assert_eq!(m.decided(), 0);
    assert_eq!(m.timeouts(), 0);
    assert!(m.cell(0, 0).is_none());
    assert_eq!(m.rate(0, 0), None);
    assert_eq!(m.diagonal(0), None);
    assert_eq!(m.row_mean(0).mean, None);
    assert_eq!(m.row_mean(0).cells, 0);
    assert_eq!(m, WinMatrix::default());
}

// ---- a real batch --------------------------------------------------------------------------

/// A pair pinned by B2 to decide inside the default cap (`b2_batch.rs`), so the
/// real matrix has defined off-diagonal cells and the identity is not vacuous.
const REAL_SUBSET: [&str; 2] = ["rush", "turtle"];

#[test]
fn a_real_batch_yields_a_complementary_matrix_in_ron_order() {
    let content = headless::content().expect("assets/data/*.ron parse into sim structs");
    // Deliberately listed out of RON order: `only` is a filter, and the matrix
    // must come out in content order anyway.
    let settings = BatchSettings::default()
        .with_only(REAL_SUBSET.iter().rev().map(|s| s.to_string()).collect())
        .with_seeds(1);
    let records = batch::run_batch(&content, &settings, &mut |_| {}).expect("shipped names");
    let m = WinMatrix::of(&records);

    let ron: Vec<String> = batch::roster(&content, Some(&REAL_SUBSET.map(String::from)))
        .expect("shipped names");
    assert_eq!(m.ids(), ron.as_slice(), "rows in RON order");

    // Every record is accounted for exactly once per cell it belongs to:
    // off-diagonal records land in two cells, mirrors in one.
    let mut total = 0u32;
    for i in 0..m.len() {
        for j in 0..m.len() {
            let c = m.cell(i, j).unwrap();
            total += if i == j { 2 * c.played() } else { c.played() };
        }
    }
    assert_eq!(total, 2 * records.len() as u32);
    assert_eq!(m.decided() + m.timeouts(), records.len() as u32);

    let rush = idx(&m, "rush");
    let turtle = idx(&m, "turtle");
    let c = m.cell(rush, turtle).unwrap();
    assert_eq!(c.played(), 4, "two slot orderings x two orientations");
    assert!(c.n_decided > 0, "the pinned pair decides, so the check below bites");
    assert!(assert_complementary(&m) > 0);

    // Deterministic: the same records give the same matrix.
    assert_eq!(m, WinMatrix::of(&records));
}

// ---- row means are order-free (critic finding on AC1) ------------------------------

/// Records putting `row` against `opp` so that `row` scores exactly `half_wins`
/// half-wins over `n` decided matches, from slot A.
fn scored(row: &str, opp: &str, half_wins: u32, n: u32) -> Vec<MatchRecord> {
    assert!(half_wins <= 2 * n);
    let wins = half_wins / 2;
    let draws = half_wins % 2;
    let mut v = Vec::new();
    v.extend((0..wins).map(|_| rec(row, opp, A_WINS)));
    v.extend((0..draws).map(|_| rec(row, opp, DRAW)));
    v.extend((0..n - wins - draws).map(|_| rec(row, opp, B_WINS)));
    v
}

fn primes(count: usize) -> Vec<u32> {
    let mut out = Vec::new();
    let mut k = 2u32;
    while out.len() < count {
        if (2..k).take_while(|d| d * d <= k).all(|d| !k.is_multiple_of(d)) {
            out.push(k);
        }
        k += 1;
    }
    out
}

/// The same records under several permutations: reversed, rotated, and
/// interleaved by a fixed stride. None of them may move a row mean by a bit.
fn permutations(records: &[MatchRecord]) -> Vec<Vec<MatchRecord>> {
    let mut out = vec![records.to_vec()];
    out.push(records.iter().rev().cloned().collect());
    let mut rotated = records.to_vec();
    rotated.rotate_left(records.len() / 3);
    out.push(rotated);
    let n = records.len();
    let stride = (1..n).rev().find(|s| (2..=*s).all(|d| !(s.is_multiple_of(d) && n.is_multiple_of(d)))).unwrap_or(1);
    out.push((0..n).map(|k| records[(k * stride) % n].clone()).collect());
    out
}

#[test]
fn a_row_mean_over_many_cells_is_exact_and_order_free() {
    // 60 opponents in 30 pairs; each pair has rates w/2p and (2p-w)/2p for a
    // distinct prime p, so the true mean is exactly 1/2 while every partial
    // float sum in column order wanders off it.
    let mut records = Vec::new();
    for (k, p) in primes(30).into_iter().enumerate() {
        let w = (k as u32 * 13 + 5) % (2 * p);
        records.extend(scored("s", &format!("lo{k}"), w, p));
        records.extend(scored("s", &format!("hi{k}"), 2 * p - w, p));
    }
    for perm in permutations(&records) {
        let m = WinMatrix::of(&perm);
        let s = m.row_mean(idx(&m, "s"));
        assert_eq!(s.cells, 60);
        assert_eq!(s.mean, Some(0.5), "exactly one half, whatever the order");
    }
}

#[test]
fn a_row_mean_beyond_u128_is_still_the_nearest_float_and_order_free() {
    // Rates 1/2p over 30 distinct primes: no denominators cancel, and the exact
    // mean has a 161-bit denominator — past u128, past any fixed-width shortcut.
    // Its nearest f64 is 0.03082994321422019 (computed with Python's
    // `fractions.Fraction`); a naive float sum gives ...018 in ascending-prime
    // order and ...0185 in descending order. Nearest, on every path.
    const NEAREST: f64 = 0.03082994321422019;
    let ps = primes(30);
    let mut records = Vec::new();
    for (k, &p) in ps.iter().enumerate() {
        records.extend(scored("s", &format!("o{k}"), 1, p));
    }
    for perm in permutations(&records) {
        let m = WinMatrix::of(&perm);
        let s = m.row_mean(idx(&m, "s"));
        assert_eq!(s.cells, 30);
        assert_eq!(s.mean, Some(NEAREST));
    }
}

#[test]
fn row_means_are_order_free_on_a_small_table() {
    // 3/5, 7/10, 1/5: exactly 1/2, in any opponent order.
    let blocks = [scored("s", "o60", 6, 5), scored("s", "o70", 7, 5), scored("s", "o20", 2, 5)];
    for order in [[0, 1, 2], [0, 2, 1], [1, 0, 2], [1, 2, 0], [2, 0, 1], [2, 1, 0]] {
        let records: Vec<MatchRecord> = order.iter().flat_map(|&k| blocks[k].clone()).collect();
        let m = WinMatrix::of(&records);
        assert_eq!(m.row_mean(idx(&m, "s")).mean, Some(0.5), "order {order:?}");
    }
}
