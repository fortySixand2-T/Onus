//! Critic probes for **B3 AC1** — the win-rate matrix.
//!
//! Written from the spec only (BALANCE_PLAN.md B3 checkbox 1 + the AC's
//! requirements). Every probe here should pass per the spec.
//!
//! Attack lines: slot-to-strategy attribution (which complementarity cannot
//! see), mirror double counting, asymmetric sampling, undefined propagation,
//! degenerate input, order independence, a brute-force differential oracle over
//! random records, an injected-imbalance fixture played for real, and the
//! `balance` binary on an all-timeout run.

use onus::batch::{self, BatchSettings, MatchRecord, MatchResult, ProductionCounts};
use onus::headless::{self, MatchSettings, Orientation};
use onus::metrics::{Cell, WinMatrix};
use onus::sim::spatial::Faction;

const A_WINS: MatchResult = MatchResult::Decided(Faction::A);
const B_WINS: MatchResult = MatchResult::Decided(Faction::B);
const DRAW: MatchResult = MatchResult::MutualLoss;
const CAPPED: MatchResult = MatchResult::Timeout;

fn r(a: &str, b: &str, result: MatchResult, o: Orientation) -> MatchRecord {
    MatchRecord {
        strategies: [a.to_string(), b.to_string()],
        seed: 7,
        result,
        orientation: o,
        ticks: 10,
        produced: ProductionCounts::default(),
    }
}

fn n(a: &str, b: &str, result: MatchResult) -> MatchRecord {
    r(a, b, result, Orientation::Normal)
}

fn c(m: &WinMatrix, row: &str, col: &str) -> Cell {
    *m.get(row, col).unwrap_or_else(|| panic!("({row},{col}) exists"))
}

fn rate(m: &WinMatrix, row: &str, col: &str) -> Option<f64> {
    m.get(row, col).and_then(|c| c.rate())
}

// ---- attribution: the transpose complementarity cannot catch -----------------------

#[test]
fn slot_b_winner_is_credited_to_the_slot_b_strategy_in_both_orientations() {
    for o in Orientation::ALL {
        // i sits in slot B and wins.
        let m = WinMatrix::of(&[r("j", "i", B_WINS, o)]);
        assert_eq!(rate(&m, "i", "j"), Some(1.0), "{o:?}: i (slot B) won");
        assert_eq!(rate(&m, "j", "i"), Some(0.0), "{o:?}: j (slot A) lost");
        assert_eq!(c(&m, "i", "j").half_wins, 2);
        assert_eq!(c(&m, "j", "i").half_wins, 0);
    }
}

#[test]
fn slot_a_loser_is_debited_to_the_slot_a_strategy_in_both_orientations() {
    for o in Orientation::ALL {
        // i sits in slot A and loses.
        let m = WinMatrix::of(&[r("i", "j", B_WINS, o)]);
        assert_eq!(rate(&m, "i", "j"), Some(0.0), "{o:?}: i (slot A) lost");
        assert_eq!(rate(&m, "j", "i"), Some(1.0), "{o:?}: j (slot B) won");
        // And the slot-A winner.
        let m = WinMatrix::of(&[r("i", "j", A_WINS, o)]);
        assert_eq!(rate(&m, "i", "j"), Some(1.0), "{o:?}: i (slot A) won");
        assert_eq!(rate(&m, "j", "i"), Some(0.0));
    }
}

#[test]
fn attribution_is_not_transposed_on_an_asymmetric_three_way_table() {
    // x beats y 3/3 from mixed slots; y beats z 2/2; z beats x 1 of 2.
    let recs = [
        n("x", "y", A_WINS),
        n("y", "x", B_WINS),
        r("y", "x", B_WINS, Orientation::Swapped),
        n("z", "y", B_WINS),
        r("y", "z", A_WINS, Orientation::Swapped),
        n("x", "z", B_WINS),
        n("z", "x", B_WINS),
    ];
    let m = WinMatrix::of(&recs);
    assert_eq!(rate(&m, "x", "y"), Some(1.0));
    assert_eq!(rate(&m, "y", "x"), Some(0.0));
    assert_eq!(rate(&m, "y", "z"), Some(1.0));
    assert_eq!(rate(&m, "z", "y"), Some(0.0));
    assert_eq!(rate(&m, "z", "x"), Some(0.5));
    assert_eq!(rate(&m, "x", "z"), Some(0.5));
    // Index-based access agrees with id-based access (no row/col swap in `rate`).
    let (x, y) = (m.index("x").unwrap(), m.index("y").unwrap());
    assert_eq!(m.rate(x, y), Some(1.0));
    assert_eq!(m.rate(y, x), Some(0.0));
    assert_eq!(m.cell(x, y).unwrap().half_wins, 6);
    assert_eq!(m.row_mean(x).mean, Some(0.75));
    assert_eq!(m.row_mean(y).mean, Some(0.5));
    assert_eq!(m.row_mean(m.index("z").unwrap()).mean, Some(0.25));
}

// ---- mirrors ---------------------------------------------------------------------------

#[test]
fn mirrors_feed_only_the_diagonal_exactly_once() {
    let recs = [
        n("i", "i", A_WINS),
        r("i", "i", B_WINS, Orientation::Swapped),
        n("i", "i", B_WINS),
        n("i", "i", DRAW),
        n("i", "i", CAPPED),
        n("i", "j", A_WINS),
        n("j", "j", A_WINS),
    ];
    let m = WinMatrix::of(&recs);
    let d = c(&m, "i", "i");
    assert_eq!(d.n_decided, 4, "four decided mirrors, not eight");
    assert_eq!(d.n_timeout, 1);
    assert_eq!(d.half_wins, 3, "slot A's half-wins: win + loss + loss + draw");
    assert_eq!(m.diagonal(m.index("i").unwrap()), Some(3.0 / 8.0));
    // Off-diagonal cells see only the one cross record.
    assert_eq!(c(&m, "i", "j"), Cell { half_wins: 2, n_decided: 1, n_timeout: 0 });
    assert_eq!(c(&m, "j", "i"), Cell { half_wins: 0, n_decided: 1, n_timeout: 0 });
    // Row means ignore the diagonal entirely.
    assert_eq!(m.row_mean(m.index("i").unwrap()).mean, Some(1.0));
    assert_eq!(m.row_mean(m.index("i").unwrap()).cells, 1);
    assert_eq!(m.row_mean(m.index("j").unwrap()).mean, Some(0.0));
    assert_eq!(m.row_mean(m.index("j").unwrap()).cells, 1);
    assert_eq!(m.decided(), 6);
    assert_eq!(m.timeouts(), 1);
}

// ---- asymmetric sampling ---------------------------------------------------------------

#[test]
fn uneven_slot_and_orientation_counts_combine_exactly() {
    use Orientation::*;
    let recs = [
        // (i, j) five times, orientations 4:1, mixed outcomes.
        r("i", "j", A_WINS, Normal),
        r("i", "j", A_WINS, Normal),
        r("i", "j", B_WINS, Normal),
        r("i", "j", DRAW, Normal),
        r("i", "j", CAPPED, Swapped),
        // (j, i) once.
        r("j", "i", DRAW, Swapped),
    ];
    let m = WinMatrix::of(&recs);
    let ij = c(&m, "i", "j");
    let ji = c(&m, "j", "i");
    // i: 2 wins + 1 loss + half + half = 3 wins over 5 decided.
    assert_eq!(ij, Cell { half_wins: 6, n_decided: 5, n_timeout: 1 });
    assert_eq!(ji, Cell { half_wins: 4, n_decided: 5, n_timeout: 1 });
    assert_eq!(ij.half_wins + ji.half_wins, 2 * ij.n_decided, "integer complementarity");
    assert_eq!(ij.rate().unwrap() + ji.rate().unwrap(), 1.0);
    assert_eq!(ij.rate(), Some(0.6));
}

// ---- undefined propagation -------------------------------------------------------------

#[test]
fn a_row_whose_only_defined_cell_is_its_mirror_has_no_mean() {
    let recs = [
        n("i", "i", A_WINS),
        n("i", "j", CAPPED),
        n("k", "i", CAPPED),
        n("j", "k", A_WINS),
    ];
    let m = WinMatrix::of(&recs);
    let i = m.index("i").unwrap();
    assert_eq!(m.diagonal(i), Some(1.0));
    let rm = m.row_mean(i);
    assert_eq!((rm.mean, rm.cells), (None, 0));
    assert_eq!(rate(&m, "i", "j"), None);
    assert_eq!(rate(&m, "k", "i"), None);
    // j and k each have exactly one defined off-diagonal cell.
    assert_eq!(m.row_mean(m.index("j").unwrap()), onus::metrics::RowMean { mean: Some(1.0), cells: 1 });
    assert_eq!(m.row_mean(m.index("k").unwrap()), onus::metrics::RowMean { mean: Some(0.0), cells: 1 });
    // i's row in row_means() is undefined too.
    let rms = m.row_means();
    assert_eq!(rms[i].0, "i");
    assert_eq!(rms[i].1.mean, None);
}

#[test]
fn unplayed_cell_and_all_timeout_cell_are_both_none_and_distinguishable() {
    let m = WinMatrix::of(&[n("a", "b", CAPPED), n("b", "c", A_WINS)]);
    assert_eq!(rate(&m, "a", "b"), None);
    assert_eq!(c(&m, "a", "b").played(), 1);
    assert_eq!(rate(&m, "a", "c"), None);
    assert_eq!(c(&m, "a", "c").played(), 0);
    assert_eq!(rate(&m, "a", "a"), None);
    assert_eq!(m.defined_cells(), 2);
    let a = m.index("a").unwrap();
    assert_eq!((m.row_mean(a).mean, m.row_mean(a).cells), (None, 0));
}

#[test]
fn an_all_timeout_synthetic_run_defines_nothing_anywhere() {
    let ids = ["p", "q", "r"];
    let mut recs = vec![];
    for a in ids {
        for b in ids {
            for o in Orientation::ALL {
                recs.push(r(a, b, CAPPED, o));
            }
        }
    }
    let m = WinMatrix::of(&recs);
    assert_eq!(m.defined_cells(), 0);
    for i in 0..m.len() {
        for j in 0..m.len() {
            assert_eq!(m.rate(i, j), None);
        }
        assert_eq!(m.row_mean(i).mean, None);
        assert_eq!(m.row_mean(i).cells, 0);
    }
    assert_eq!(m.decided(), 0);
    assert_eq!(m.timeouts(), recs.len() as u32);
}

#[test]
fn a_dominant_row_with_undefined_cells_reports_the_true_cell_count() {
    let recs = [
        n("top", "b", A_WINS),
        n("c", "top", B_WINS),
        n("top", "d", CAPPED),
        n("e", "top", CAPPED),
        n("b", "c", DRAW),
        n("d", "e", A_WINS),
    ];
    let m = WinMatrix::of(&recs);
    let top = m.index("top").unwrap();
    assert_eq!(m.row_mean(top), onus::metrics::RowMean { mean: Some(1.0), cells: 2 });
    // The strictly-losing reading is by name too.
    let bottom = WinMatrix::of(&[n("loser", "x", B_WINS), n("y", "loser", A_WINS), n("x", "y", DRAW)]);
    let li = bottom.index("loser").unwrap();
    assert_eq!(bottom.row_mean(li), onus::metrics::RowMean { mean: Some(0.0), cells: 2 });
    let name_of_min = bottom
        .row_means()
        .into_iter()
        .filter_map(|(id, rm)| rm.mean.map(|v| (id, v)))
        .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
        .unwrap()
        .0;
    assert_eq!(name_of_min, "loser");
}

// ---- row mean definition ---------------------------------------------------------------

#[test]
fn row_mean_is_mean_of_cell_rates_with_unequal_n() {
    // s vs t: 9 wins of 9. s vs u: 0 of 1. Pooled would be 0.9; cell mean is 0.5.
    let mut recs: Vec<MatchRecord> = (0..9).map(|_| n("s", "t", A_WINS)).collect();
    recs.push(n("u", "s", A_WINS));
    let m = WinMatrix::of(&recs);
    assert_eq!(m.row_mean(m.index("s").unwrap()).mean, Some(0.5));
    assert_eq!(m.row_mean(m.index("s").unwrap()).cells, 2);
}

// ---- degenerate input ------------------------------------------------------------------

#[test]
fn degenerate_inputs_do_not_panic_and_do_nothing_clever() {
    // A single record.
    let m = WinMatrix::of(&[n("a", "b", A_WINS)]);
    assert_eq!(m.ids(), ["a", "b"]);
    assert_eq!(m.defined_cells(), 2);
    // A strategy that appears only in slot B is still a row.
    let m = WinMatrix::of(&[n("a", "onlyb", B_WINS), n("a", "onlyb", A_WINS)]);
    assert_eq!(m.ids(), ["a", "onlyb"]);
    assert_eq!(rate(&m, "onlyb", "a"), Some(0.5));
    // Casing and whitespace are distinct ids.
    let m = WinMatrix::of(&[n("Rush", "rush", A_WINS), n("rush ", "rush", B_WINS), n("", "rush", DRAW)]);
    assert_eq!(m.ids(), ["Rush", "rush", "rush ", ""]);
    assert_eq!(rate(&m, "Rush", "rush"), Some(1.0));
    assert_eq!(rate(&m, "rush", "rush "), Some(1.0));
    assert_eq!(rate(&m, "", "rush"), Some(0.5));
    assert_eq!(rate(&m, "Rush", "Rush"), None);
    // Duplicates are counted, not deduped.
    let one = n("a", "b", A_WINS);
    let m = WinMatrix::of(&[one.clone(), one.clone(), one]);
    assert_eq!(c(&m, "a", "b"), Cell { half_wins: 6, n_decided: 3, n_timeout: 0 });
    assert_eq!(m.decided(), 3);
    // Out-of-range lookups are None, never a panic.
    assert!(m.cell(usize::MAX, 0).is_none());
    assert_eq!(m.rate(0, usize::MAX), None);
    assert_eq!(m.row_mean(usize::MAX).mean, None);
    assert_eq!(m.diagonal(99), None);
    assert!(m.get("a", "zzz").is_none());
}

#[test]
fn rate_is_exact_float_division_at_large_counts() {
    let cell = Cell { half_wins: 1 << 30, n_decided: 1 << 30, n_timeout: 0 };
    assert_eq!(cell.rate(), Some(0.5));
    let cell = Cell { half_wins: 1, n_decided: 1_000_000_000, n_timeout: 5 };
    assert_eq!(cell.rate(), Some(1.0 / 2_000_000_000.0));
    let cell = Cell { half_wins: 3, n_decided: 2, n_timeout: 0 };
    assert_eq!(cell.rate(), Some(0.75), "no integer truncation");
}

// ---- differential oracle + order independence ------------------------------------------

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn random_records(seed: u64, count: usize, ids: &[&str]) -> Vec<MatchRecord> {
    let mut g = Lcg(seed);
    (0..count)
        .map(|_| {
            let a = ids[g.below(ids.len() as u64) as usize];
            let b = ids[g.below(ids.len() as u64) as usize];
            let res = match g.below(7) {
                0..=2 => A_WINS,
                3..=4 => B_WINS,
                5 => DRAW,
                _ => CAPPED,
            };
            let o = if g.below(3) == 0 { Orientation::Swapped } else { Orientation::Normal };
            r(a, b, res, o)
        })
        .collect()
}

fn gcd(mut a: u128, mut b: u128) -> u128 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// The exact mean of fractions `num/den`, reduced, as `(p, q)`; `None` if empty.
/// Checked arithmetic: an overflow is a panic in the oracle, never a wrap.
fn exact_mean(fracs: &[(u128, u128)]) -> Option<(u128, u128)> {
    if fracs.is_empty() {
        return None;
    }
    let (mut p, mut q) = (0u128, 1u128);
    for &(n, d) in fracs {
        let np = p.checked_mul(d).and_then(|x| x.checked_add(n.checked_mul(q)?)).expect("u128 sum");
        let nq = q.checked_mul(d).expect("u128 denominator");
        let g = gcd(np, nq).max(1);
        (p, q) = (np / g, nq / g);
    }
    let nq = q.checked_mul(fracs.len() as u128).expect("u128 denominator");
    let g = gcd(p, nq).max(1);
    Some((p / g, nq / g))
}

/// The f64 nearest `p / q` for `0 <= p <= q`, round-half-to-even, by binary long
/// division in integers (so the remainder `r < q` doubles without overflow as
/// long as `q < 2^127`).
fn nearest_f64(p: u128, q: u128) -> f64 {
    assert!(q > 0 && p <= q && q < (1u128 << 126));
    if p == 0 {
        return 0.0;
    }
    if p == q {
        return 1.0;
    }
    let (mut r, mut m, mut bits, mut k) = (p, 0u64, 0u32, 0u32);
    while bits < 53 {
        r *= 2;
        k += 1;
        let bit = r >= q;
        if bit {
            r -= q;
        }
        if m > 0 || bit {
            m = m * 2 + bit as u64;
            bits += 1;
        }
    }
    // Guard bit and sticky remainder.
    r *= 2;
    let guard = r >= q;
    if guard {
        r -= q;
    }
    if guard && (r != 0 || m & 1 == 1) {
        m += 1; // may reach 2^53, still exactly representable
    }
    let mut v = m as f64;
    for _ in 0..k {
        v *= 0.5; // exact: no subnormals at these magnitudes
    }
    v
}

#[test]
fn the_nearest_float_helper_is_itself_correct() {
    assert_eq!(nearest_f64(1, 2), 0.5);
    assert_eq!(nearest_f64(1, 3), 1.0 / 3.0);
    assert_eq!(nearest_f64(2, 3), 2.0 / 3.0);
    assert_eq!(nearest_f64(1, 10), 0.1);
    assert_eq!(nearest_f64(7, 10), 0.7);
    assert_eq!(nearest_f64(61, 112), 61.0 / 112.0);
    for q in 1..300u128 {
        for p in 0..=q {
            // For p, q < 2^53 the IEEE division is itself correctly rounded.
            assert_eq!(nearest_f64(p, q), p as f64 / q as f64, "{p}/{q}");
        }
    }
    assert_eq!(exact_mean(&[(3, 5), (7, 10), (1, 5)]), Some((1, 2)));
    assert_eq!(exact_mean(&[]), None);
}

/// Brute force: for each (row, col) scan every record, written independently
/// of the implementation (strategy-centric, rational arithmetic).
fn oracle(recs: &[MatchRecord], row: &str, col: &str) -> (u32, u32, u32) {
    let (mut wins2, mut dec, mut to) = (0u32, 0u32, 0u32);
    for rec in recs {
        let [s0, s1] = &rec.strategies;
        if row == col {
            if !(s0 == row && s1 == row) {
                continue;
            }
            match rec.result {
                MatchResult::Timeout => to += 1,
                MatchResult::MutualLoss => {
                    dec += 1;
                    wins2 += 1
                }
                MatchResult::Decided(f) => {
                    dec += 1;
                    if f == Faction::A {
                        wins2 += 2
                    }
                }
            }
            continue;
        }
        let row_slot = if s0 == row && s1 == col {
            Faction::A
        } else if s1 == row && s0 == col {
            Faction::B
        } else {
            continue;
        };
        match rec.result {
            MatchResult::Timeout => to += 1,
            MatchResult::MutualLoss => {
                dec += 1;
                wins2 += 1
            }
            MatchResult::Decided(f) => {
                dec += 1;
                if f == row_slot {
                    wins2 += 2
                }
            }
        }
    }
    (wins2, dec, to)
}

#[test]
fn matrix_matches_a_brute_force_oracle_on_random_records() {
    let ids = ["alpha", "beta", "gamma", "delta", "eps"];
    for seed in 1..40u64 {
        let recs = random_records(seed, 1 + (seed as usize * 37) % 400, &ids);
        let m = WinMatrix::of(&recs);
        let mut decided = 0;
        let mut timeouts = 0;
        for rec in &recs {
            if rec.result == CAPPED {
                timeouts += 1
            } else {
                decided += 1
            }
        }
        assert_eq!(m.decided(), decided, "seed {seed}");
        assert_eq!(m.timeouts(), timeouts, "seed {seed}");
        for &row in m.ids().iter().map(|s| s.as_str()).collect::<Vec<_>>().iter() {
            let mut means = vec![];
            for col in m.ids() {
                let (w2, d, t) = oracle(&recs, row, col);
                let got = c(&m, row, col);
                assert_eq!(
                    (got.half_wins, got.n_decided, got.n_timeout),
                    (w2, d, t),
                    "seed {seed} W[{row}][{col}]"
                );
                let want = (d > 0).then(|| w2 as f64 / (2 * d) as f64);
                assert_eq!(got.rate(), want);
                if row != col {
                    let other = c(&m, col, row);
                    assert_eq!(other.n_decided, got.n_decided);
                    assert_eq!(other.n_timeout, got.n_timeout);
                    assert_eq!(other.half_wins + got.half_wins, 2 * got.n_decided);
                    if d > 0 {
                        // Exact rate, as integers: w2 / (2 d).
                        means.push((w2 as u128, 2 * d as u128));
                    }
                }
            }
            let rm = m.row_mean(m.index(row).unwrap());
            assert_eq!(rm.cells, means.len());
            // The f64 nearest the exact rational mean — no float sum anywhere.
            let want_mean = exact_mean(&means).map(|(p, q)| nearest_f64(p, q));
            assert_eq!(rm.mean, want_mean, "seed {seed} row {row}");
        }
    }
}

#[test]
fn shuffling_changes_only_the_label_order() {
    let ids = ["one", "two", "three", "four"];
    let recs = random_records(99, 300, &ids);
    let base = WinMatrix::of(&recs);
    let mut g = Lcg(12345);
    for _ in 0..10 {
        let mut shuffled = recs.clone();
        for k in (1..shuffled.len()).rev() {
            let j = g.below(k as u64 + 1) as usize;
            shuffled.swap(k, j);
        }
        let m = WinMatrix::of(&shuffled);
        // Order is first appearance, slot A before slot B.
        let mut expect: Vec<String> = vec![];
        for rec in &shuffled {
            for s in &rec.strategies {
                if !expect.contains(s) {
                    expect.push(s.clone());
                }
            }
        }
        assert_eq!(m.ids(), expect.as_slice());
        for a in base.ids() {
            for b in base.ids() {
                assert_eq!(base.get(a, b), m.get(a, b), "W[{a}][{b}]");
            }
            let (bi, mi) = (base.index(a).unwrap(), m.index(a).unwrap());
            assert_eq!(base.row_mean(bi), m.row_mean(mi));
        }
    }
}

// ---- real play: injected imbalance and all-timeout -------------------------------------

/// A fixture that can never build an army: every barracks opening is pushed
/// past any tick cap. Played for real against `rush`, from both slots and in
/// both orientations, the matrix must credit `rush` with every decided match.
#[test]
fn an_injected_crippled_strategy_is_surfaced_as_strictly_losing_by_name() {
    let mut content = headless::content().expect("content loads");
    let mut crippled = content
        .strategies
        .iter()
        .find(|s| s.id == "rush")
        .expect("rush ships")
        .clone();
    crippled.id = "crippled".into();
    for b in &mut crippled.barracks {
        b.at_tick = u32::MAX;
    }
    content.strategies.push(crippled);

    let mut recs = vec![];
    for (a, b) in [("rush", "crippled"), ("crippled", "rush")] {
        for o in Orientation::ALL {
            let settings = MatchSettings::default()
                .with_seed(3)
                .with_strategies(a, b)
                .with_orientation(o);
            let rec = batch::run_match(&content, &settings).expect("names resolve");
            // Ground truth for the slot mapping, independent of the result:
            // the crippled side builds no combat unit.
            let crippled_slot = if a == "crippled" { Faction::A } else { Faction::B };
            let rush_slot = if a == "rush" { Faction::A } else { Faction::B };
            assert!(rec.produced.total(rush_slot) > rec.produced.total(crippled_slot));
            assert_eq!(rec.result, MatchResult::Decided(rush_slot), "{a} vs {b} {o:?}: rush must win");
            recs.push(rec);
        }
    }
    let m = WinMatrix::of(&recs);
    assert_eq!(m.ids(), ["rush", "crippled"]);
    assert_eq!(c(&m, "rush", "crippled"), Cell { half_wins: 8, n_decided: 4, n_timeout: 0 });
    assert_eq!(rate(&m, "crippled", "rush"), Some(0.0));
    let means = m.row_means();
    assert_eq!(means[0], ("rush".to_string(), onus::metrics::RowMean { mean: Some(1.0), cells: 1 }));
    assert_eq!(means[1], ("crippled".to_string(), onus::metrics::RowMean { mean: Some(0.0), cells: 1 }));
}

#[test]
fn a_real_all_timeout_batch_defines_nothing() {
    let content = headless::content().expect("content loads");
    let settings = BatchSettings::default()
        .with_only(vec!["rush".into(), "turtle".into()])
        .with_seeds(1)
        .with_tick_cap(1);
    let recs = batch::run_batch(&content, &settings, &mut |_| {}).expect("names");
    assert!(recs.iter().all(|r| r.result == CAPPED));
    let m = WinMatrix::of(&recs);
    assert_eq!(m.len(), 2);
    assert_eq!(m.defined_cells(), 0);
    for i in 0..2 {
        assert_eq!(m.row_mean(i).mean, None);
    }
    assert_eq!(m.timeouts(), 8);
}

#[test]
fn the_balance_binary_flags_an_all_timeout_run_and_prints_no_rate() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_balance"))
        .args(["--only", "rush,turtle", "--tick-cap", "1"])
        .output()
        .expect("binary runs");
    assert!(out.status.success(), "exit: {:?}", out.status);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("WARNING: no cell of the matrix is defined"),
        "matrix warning missing:\n{stdout}"
    );
    let start = stdout.find("win rate").expect("matrix printed");
    let matrix = &stdout[start..];
    assert!(!matrix.contains("50.0"), "an undecided matrix printed a 50%:\n{matrix}");
    assert!(!matrix.contains('%') || matrix.contains("--"), "{matrix}");
    for line in matrix.lines().filter(|l| l.starts_with('[')) {
        assert!(!line.contains('.'), "a rate was printed in an all-timeout matrix: {line}");
    }
}

/// Row strength is a property of the *set* of records: the mean of a row's
/// defined off-diagonal cell rates cannot depend on which opponent happened to
/// appear first in the slice. Here `s` has rates 3/5, 7/10 and 1/5 — an exact
/// mean of 1/2 — and the only difference between the two slices is record
/// order. A strength gate (`mean >= 0.5`, B3's later checkboxes) must not flip
/// on a reordering of the same batch.
#[test]
fn a_row_mean_does_not_depend_on_record_order() {
    let block = |opp: &str, wins: usize, draws: usize, losses: usize| -> Vec<MatchRecord> {
        let mut v = vec![];
        v.extend((0..wins).map(|_| n("s", opp, A_WINS)));
        v.extend((0..draws).map(|_| n("s", opp, DRAW)));
        v.extend((0..losses).map(|_| n("s", opp, B_WINS)));
        v
    };
    let o60 = block("o60", 3, 0, 2); // 3/5
    let o70 = block("o70", 3, 1, 1); // 7/10
    let o20 = block("o20", 1, 0, 4); // 1/5

    let first: Vec<MatchRecord> = [o60.clone(), o70.clone(), o20.clone()].concat();
    let second: Vec<MatchRecord> = [o20, o60, o70].concat();
    let m1 = WinMatrix::of(&first);
    let m2 = WinMatrix::of(&second);
    for a in m1.ids() {
        for b in m1.ids() {
            assert_eq!(m1.get(a, b), m2.get(a, b), "cells are order-free");
        }
    }
    let s1 = m1.row_mean(m1.index("s").unwrap());
    let s2 = m2.row_mean(m2.index("s").unwrap());
    assert_eq!(s1.cells, 3);
    assert_eq!(
        s1.mean, s2.mean,
        "the same records in another order give a different strength for `s`"
    );
}

/// The fixed requirement: a row mean is the `f64` nearest the exact rational
/// mean of its defined off-diagonal cells. Rates 3/5, 7/10, 1/5 (exact mean
/// 1/2) summed as floats in column order o60, o70, o20 give
/// 0.49999999999999994; the nearest float to 1/2 is 0.5, in every order.
/// Likewise 1/12, 1/2, 11/12 (half-wins 1/12, 6/12, 11/12 over 6 matches).
#[test]
fn a_row_mean_is_the_nearest_float_to_the_exact_rational_mean() {
    let block = |opp: &str, wins: usize, draws: usize, losses: usize| -> Vec<MatchRecord> {
        let mut v = vec![];
        v.extend((0..wins).map(|_| n("s", opp, A_WINS)));
        v.extend((0..draws).map(|_| n("s", opp, DRAW)));
        v.extend((0..losses).map(|_| n("s", opp, B_WINS)));
        v
    };
    let fixtures: [Vec<Vec<MatchRecord>>; 2] = [
        vec![block("o60", 3, 0, 2), block("o70", 3, 1, 1), block("o20", 1, 0, 4)],
        vec![block("h06", 3, 0, 3), block("h11", 5, 1, 0), block("h01", 0, 1, 5)],
    ];
    for blocks in fixtures {
        // Every column order of the three opponents.
        for perm in [[0, 1, 2], [0, 2, 1], [1, 0, 2], [1, 2, 0], [2, 0, 1], [2, 1, 0]] {
            let recs: Vec<MatchRecord> = perm.iter().flat_map(|&k| blocks[k].clone()).collect();
            let m = WinMatrix::of(&recs);
            let rm = m.row_mean(m.index("s").unwrap());
            assert_eq!(rm.cells, 3);
            assert_eq!(rm.mean, Some(0.5), "order {perm:?}");
        }
    }
}

// ============================================================================================
// Re-review of the exact row mean (arbitrary-precision sum + nearest rounding).
// `Big`/`nearest` are private, so every probe below reaches them through
// `WinMatrix::row_mean`, against oracles that do not share their method:
// the u128 long-division helper above (inside u128), and exact values computed
// offline with Python's `fractions.Fraction` (whose `float()` is correctly
// rounded) for sums past u128.
// ============================================================================================

/// Records giving `row` exactly `h` half-wins over `n` decided matches against
/// `opp`. `flip` seats `row` in slot B instead of A, so both slot paths feed
/// the cells the mean is built from.
fn scored_cell(row: &str, opp: &str, h: u32, n: u32, flip: bool) -> Vec<MatchRecord> {
    assert!(h <= 2 * n);
    let (wins, draws) = (h / 2, h % 2);
    let losses = n - wins - draws;
    let (win, loss) = if flip { (B_WINS, A_WINS) } else { (A_WINS, B_WINS) };
    let seat = |res| if flip { n_rec(opp, row, res) } else { n_rec(row, opp, res) };
    let mut v = Vec::with_capacity(n as usize);
    v.extend((0..wins).map(|_| seat(win)));
    v.extend((0..draws).map(|_| seat(DRAW)));
    v.extend((0..losses).map(|_| seat(loss)));
    v
}

fn n_rec(a: &str, b: &str, res: MatchResult) -> MatchRecord {
    n(a, b, res)
}

fn row_records(cells: &[(u32, u32)]) -> Vec<MatchRecord> {
    cells
        .iter()
        .enumerate()
        .flat_map(|(k, &(h, n))| scored_cell("s", &format!("o{k}"), h, n, k % 3 == 1))
        .collect()
}

fn s_mean(recs: &[MatchRecord]) -> onus::metrics::RowMean {
    let m = WinMatrix::of(recs);
    m.row_mean(m.index("s").expect("s is a row"))
}

fn shuffle(recs: &mut [MatchRecord], g: &mut Lcg) {
    for k in (1..recs.len()).rev() {
        let j = g.below(k as u64 + 1) as usize;
        recs.swap(k, j);
    }
}

/// `(tag, expected f64 bits, cells as (half_wins, n_decided))`. Expected values
/// are `float(sum(Fraction(h, 2n)) / k)`. The unreduced common denominator
/// `k * prod(2n)` is 153-273 bits; the reduced one 78-196 bits. Tags: `random`;
/// `low`/`high` (means near 0 / near 1); `mid` (exact value within 0.002 ulp of
/// the midpoint between two floats: the guard/sticky decision); `exact` (within
/// 0.002 ulp above a float); `next` (within 0.002 ulp below the next float).
type Fixture = (&'static str, u64, &'static [(u32, u32)]);
const BEYOND_U128: &[Fixture] = &[
    ("random", 0x3FE200554A2B2A76, &[(349, 193), (325, 541), (222, 809), (1033, 528), (780, 397), (896, 754), (1173, 827), (1025, 767), (1254, 725), (497, 697), (177, 404), (550, 596), (1307, 703), (843, 614), (853, 802), (829, 693), (496, 280), (93, 197), (1222, 637), (696, 457), (981, 692), (31, 344), (24, 227), (50, 523), (488, 482), (416, 415)]),
    ("random", 0x3FE204A985D9D68C, &[(228, 200), (1202, 696), (719, 460), (119, 275), (384, 204), (1273, 740), (287, 370), (1011, 745), (18, 282), (193, 720), (174, 227), (686, 377), (170, 270), (339, 641), (29, 158), (772, 512), (756, 544), (916, 707), (404, 597), (787, 599), (471, 303), (845, 635), (285, 333), (360, 248), (639, 333)]),
    ("random", 0x3FE204688A4D661E, &[(497, 267), (271, 353), (336, 353), (1356, 786), (1510, 889), (109, 332), (404, 269), (276, 738), (462, 437), (263, 208), (670, 466), (812, 837), (1062, 644), (170, 629), (573, 552), (988, 881)]),
    ("random", 0x3FDA8B0506974A24, &[(261, 152), (178, 900), (49, 322), (1543, 894), (551, 510), (308, 601), (243, 398), (31, 639), (235, 282), (49, 249), (107, 286), (315, 677), (66, 566), (800, 496), (387, 230), (652, 445), (294, 519), (9, 459), (99, 256), (1284, 789), (945, 832), (976, 560), (546, 787), (246, 151), (202, 396), (452, 596), (420, 787)]),
    ("random", 0x3FDE613359719675, &[(239, 172), (751, 738), (278, 576), (185, 216), (418, 278), (252, 651), (696, 425), (414, 391), (181, 832), (166, 221), (773, 639), (1132, 784), (83, 541), (99, 162), (235, 760), (202, 589), (1329, 670), (1099, 766), (661, 618), (321, 662), (862, 877), (579, 531), (308, 172), (288, 690), (265, 261), (987, 666), (424, 833)]),
    ("random", 0x3FDEB029C2072C38, &[(381, 428), (763, 899), (191, 855), (166, 577), (51, 154), (328, 616), (318, 371), (1304, 801), (432, 485), (748, 632), (243, 174), (85, 257), (271, 245), (1266, 758), (1243, 846), (856, 497), (1099, 649), (90, 532), (498, 556), (326, 179), (992, 767), (77, 383), (930, 644), (514, 615), (59, 613), (195, 457), (328, 199)]),
    ("random", 0x3FE0F6460F81F53F, &[(492, 498), (233, 173), (344, 335), (557, 302), (264, 245), (574, 287), (241, 466), (248, 651), (24, 491), (266, 226), (12, 378), (698, 466), (741, 398), (389, 596), (154, 323), (937, 512), (1043, 547), (1010, 705), (61, 820), (818, 794)]),
    ("random", 0x3FDED3D86C605A27, &[(188, 840), (382, 248), (78, 667), (878, 578), (570, 405), (1212, 706), (114, 527), (336, 551), (268, 321), (627, 609), (464, 789), (292, 244), (497, 822), (208, 857), (617, 374), (495, 564), (689, 486), (562, 893), (849, 451)]),
    ("low", 0x3F8A99137917A463, &[(5, 627), (7, 411), (7, 340), (11, 566), (10, 394), (8, 223), (0, 418), (28, 622), (3, 255), (13, 276), (7, 265), (19, 386), (15, 323), (15, 335), (11, 514), (12, 461), (12, 671), (8, 230), (33, 727), (3, 877), (1, 593), (22, 635), (16, 509), (6, 619)]),
    ("high", 0x3FEFA349AD135394, &[(628, 317), (1629, 833), (972, 491), (1444, 740), (686, 344), (1094, 555), (933, 478), (655, 330), (978, 493), (507, 259), (488, 246), (721, 363), (705, 355), (1473, 746), (1320, 665), (1492, 753), (1609, 806), (712, 358), (429, 218), (1289, 656), (1660, 833), (1017, 521), (1612, 807), (1406, 707)]),
    ("mid", 0x3FE31793C8B5CE36, &[(294, 424), (949, 488), (380, 251), (454, 270), (224, 446), (194, 157), (324, 492), (305, 260), (139, 110), (432, 240), (155, 266), (922, 500), (218, 200), (359, 339), (123, 118), (771, 396), (19, 105), (595, 470)]),
    ("mid", 0x3FD87494BA7E25B3, &[(14, 475), (345, 384), (148, 479), (271, 230), (167, 286), (575, 452), (341, 346), (148, 366), (270, 599), (364, 582), (169, 156), (700, 376), (10, 110), (334, 434), (85, 147), (232, 417), (804, 548), (452, 575), (184, 312)]),
    ("mid", 0x3FE1F7427D1988BB, &[(224, 382), (710, 410), (327, 209), (278, 469), (369, 285), (186, 265), (637, 584), (291, 326), (186, 227), (373, 273), (133, 228), (198, 127), (908, 491), (265, 306), (205, 171), (255, 180), (834, 594), (348, 502)]),
    ("exact", 0x3FE1CCDFD7491BB3, &[(807, 406), (203, 438), (478, 243), (424, 591), (733, 495), (455, 249), (238, 127), (331, 291), (16, 480), (241, 360), (50, 333), (740, 378), (120, 551), (82, 296), (441, 484), (281, 264), (791, 554), (255, 137)]),
    ("exact", 0x3FDF27A171BD159E, &[(454, 295), (319, 355), (22, 164), (773, 456), (95, 474), (138, 353), (107, 291), (98, 228), (109, 123), (687, 399), (627, 346), (306, 251), (283, 287), (584, 388), (772, 450), (31, 410)]),
    ("exact", 0x3FDE1E42711C9DB5, &[(204, 218), (140, 502), (581, 379), (336, 360), (283, 296), (479, 381), (506, 267), (305, 388), (468, 558), (360, 296), (84, 121), (37, 249), (345, 298), (448, 485), (68, 237), (831, 503), (164, 317), (173, 104), (310, 533), (123, 197), (368, 416)]),
    ("next", 0x3FE21B62894A782A, &[(706, 529), (347, 421), (203, 254), (489, 544), (873, 467), (2, 326), (782, 421), (524, 505), (108, 572), (223, 196), (921, 554), (94, 130), (272, 450), (305, 255), (160, 168), (277, 220), (700, 465), (617, 501), (449, 262), (956, 522)]),
    ("next", 0x3FE2A9742C613EC8, &[(302, 303), (382, 196), (324, 237), (484, 438), (12, 104), (294, 454), (999, 510), (526, 474), (159, 124), (244, 138), (480, 499), (97, 139), (347, 225), (363, 225), (38, 196), (200, 136), (318, 289), (427, 383)]),
    ("next", 0x3FD9B43A3F75B6EE, &[(331, 507), (103, 168), (209, 475), (667, 554), (658, 366), (293, 164), (229, 195), (49, 254), (394, 310), (31, 163), (271, 464), (120, 532), (691, 447), (356, 299), (15, 102), (795, 468), (11, 265), (46, 573), (400, 432), (17, 108), (442, 466)]),
];

#[test]
fn row_mean_beyond_u128_matches_offline_exact_values() {
    let mut g = Lcg(0xB16);
    for (tag, want_bits, cells) in BEYOND_U128 {
        let want = f64::from_bits(*want_bits);
        let mut recs = row_records(cells);
        let rm = s_mean(&recs);
        assert_eq!(rm.cells, cells.len(), "{tag}");
        assert_eq!(rm.mean.map(f64::to_bits), Some(*want_bits), "{tag}: got {:?}, want {want:?}", rm.mean);
        // Order independence at this size: bit-identical under shuffles.
        for _ in 0..2 {
            shuffle(&mut recs, &mut g);
            assert_eq!(s_mean(&recs).mean.map(f64::to_bits), Some(*want_bits), "{tag} shuffled");
        }
    }
}

#[test]
fn row_mean_inside_u128_matches_the_long_division_oracle() {
    let mut g = Lcg(0xD1FF);
    for round in 0..300 {
        let k = 1 + g.below(7) as usize;
        let cells: Vec<(u32, u32)> = (0..k)
            .map(|_| {
                let n = 1 + g.below(if round % 2 == 0 { 40 } else { 2500 }) as u32;
                let h = match g.below(6) {
                    0 => 0,
                    1 => 2 * n,
                    2 => n,
                    _ => g.below(2 * n as u64 + 1) as u32,
                };
                (h, n)
            })
            .collect();
        let fracs: Vec<(u128, u128)> = cells.iter().map(|&(h, n)| (h as u128, 2 * n as u128)).collect();
        let (p, q) = exact_mean(&fracs).unwrap();
        let want = nearest_f64(p, q);
        let rm = s_mean(&row_records(&cells));
        assert_eq!(rm.mean.map(f64::to_bits), Some(want.to_bits()), "round {round} cells {cells:?}");
    }
}

/// Denominators built from powers of two land exactly on limb boundaries
/// (`k * prod(2n)` = 2^32, 2^64, 2^96 times small factors), and `n = 2^15 - 1`
/// / `2^16 - 1` put all-ones limbs through the carries.
#[test]
fn row_mean_at_limb_boundaries_matches_the_oracle() {
    let fixtures: &[&[(u32, u32)]] = &[
        &[(1, 32768), (65535, 32768)],
        &[(1, 32768), (3, 32768), (65535, 32768)],
        &[(1, 32768), (3, 32768), (5, 32768), (65535, 32768)],
        &[(1, 32767), (32767, 32767), (65533, 32767), (2, 32767)],
        &[(1, 65535), (131069, 65535), (7, 32768)],
        &[(0, 32768), (0, 32767), (0, 65535)],
        &[(65536, 32768), (65534, 32767), (131070, 65535)],
        &[(1, 1), (1, 32768), (65533, 32767), (1, 65535)],
    ];
    for cells in fixtures {
        let fracs: Vec<(u128, u128)> = cells.iter().map(|&(h, n)| (h as u128, 2 * n as u128)).collect();
        let (p, q) = exact_mean(&fracs).unwrap();
        let want = nearest_f64(p, q);
        let rm = s_mean(&row_records(cells));
        assert_eq!(rm.mean.map(f64::to_bits), Some(want.to_bits()), "cells {cells:?}");
    }
}

#[test]
fn exact_zero_one_and_half_over_many_coprime_cells() {
    let ns: Vec<u32> = (0..40).map(|k| 101 + 2 * k).collect(); // odd, many coprime
    let zero: Vec<(u32, u32)> = ns.iter().map(|&n| (0, n)).collect();
    assert_eq!(s_mean(&row_records(&zero)).mean.map(f64::to_bits), Some(0.0f64.to_bits()));
    let one: Vec<(u32, u32)> = ns.iter().map(|&n| (2 * n, n)).collect();
    assert_eq!(s_mean(&row_records(&one)).mean, Some(1.0));
    let half: Vec<(u32, u32)> = ns.iter().map(|&n| (n, n)).collect();
    assert_eq!(s_mean(&row_records(&half)).mean, Some(0.5));
    // Complementary pairs around 1/2 over coprime n: exactly 1/2.
    let mut pairs = vec![];
    for (k, &n) in ns.iter().enumerate() {
        let h = (k as u32 * 37 + 1) % (2 * n);
        pairs.push((h, n));
        pairs.push((2 * n - h, n));
    }
    assert_eq!(s_mean(&row_records(&pairs)).mean, Some(0.5));
    // One win in one cell, everything else lost: the smallest positive mean
    // here is exactly 1 / (2 * 101 * 40), strictly positive, never rounded to 0.
    let mut tiny = zero.clone();
    tiny[0] = (1, ns[0]);
    assert_eq!(s_mean(&row_records(&tiny)).mean, Some(nearest_f64(1, 2 * 101 * 40)));
}

/// `run_batch`-shaped input at scale: every ordered pair of a roster, both
/// orientations, uneven decided counts and timeouts per cell. Nothing panics,
/// every row mean matches the oracle where the oracle fits, and `row_means()`
/// is cheap.
#[test]
fn row_means_on_large_rosters_do_not_panic_and_are_cheap() {
    for (roster, per) in [(10usize, 400u64), (40, 12)] {
        let ids: Vec<String> = (0..roster).map(|i| format!("st{i}")).collect();
        let mut g = Lcg(roster as u64 * 7919);
        let mut recs = vec![];
        for a in &ids {
            for b in &ids {
                let count = per / 2 + g.below(per);
                for _ in 0..count {
                    let res = match g.below(9) {
                        0..=3 => A_WINS,
                        4..=6 => B_WINS,
                        7 => DRAW,
                        _ => CAPPED,
                    };
                    let o = if g.below(2) == 0 { Orientation::Normal } else { Orientation::Swapped };
                    recs.push(r(a, b, res, o));
                }
            }
        }
        let m = WinMatrix::of(&recs);
        let t = std::time::Instant::now();
        let means = m.row_means();
        let elapsed = t.elapsed();
        eprintln!("roster {roster}: {} records, row_means() in {elapsed:?}", recs.len());
        assert!(elapsed < std::time::Duration::from_secs(2), "row_means took {elapsed:?}");
        assert_eq!(means.len(), roster);
        if roster == 10 {
            let mut checked = 0;
            for (i, (_, rm)) in means.iter().enumerate() {
                let fracs: Vec<(u128, u128)> = (0..roster)
                    .filter(|&j| j != i)
                    .filter_map(|j| m.cell(i, j))
                    .filter(|c| c.n_decided > 0)
                    .map(|c| (c.half_wins as u128, 2 * c.n_decided as u128))
                    .collect();
                // Nine cells of ~11-bit denominators: past u128 unreduced, but the
                // oracle reduces after every step; skip only if it cannot fit.
                let want = std::panic::catch_unwind(|| exact_mean(&fracs).map(|(p, q)| nearest_f64(p, q)));
                if let Ok(want) = want {
                    assert_eq!(rm.mean, want, "row {i}");
                    checked += 1;
                }
            }
            eprintln!("roster 10: {checked} of {roster} rows checked against the u128 oracle");
            assert!(checked > 0);
        }
    }
}
