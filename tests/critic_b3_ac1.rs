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
