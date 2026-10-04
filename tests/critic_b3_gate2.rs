//! Critic probes, B3 re-review (after the first critic BLOCK). Each test
//! names the property it holds the gate to; a red test is a finding.
//!
//! - K2 "mirrors within tolerance of 50%": a per-mirror FAIL on the left base
//!   (not slot A) must fail K2; a mirror whose own interval excludes 50% and
//!   whose point sits ~15 points off must not read PASS; a mirror known from
//!   six matches, all to slot A, must not read PASS ("too little data never
//!   reads PASS").
//! - K3 band share: other splits, timeouts dominating, too few seeds.
//! - `losing`: every-matchup losses at 45%, a row resolved below 35% with one
//!   winning cell, and naming never gates.
//! - Row-mean interval: `max_mean_variance` against a brute-force oracle, and
//!   Monte Carlo coverage at extreme unequal cell sizes.
//! - The gate's statuses and readings do not depend on record order.

use onus::batch::{MatchRecord, MatchResult, ProductionCounts};
use onus::gate::{max_mean_variance, GateSpec, KillGate, Reading, Rule, Status};
use onus::headless::{self, Orientation, SIM_HZ};
use onus::sim::content::Content;
use onus::sim::spatial::Faction;

const A: MatchResult = MatchResult::Decided(Faction::A);
const B: MatchResult = MatchResult::Decided(Faction::B);
const MIN: u32 = 60 * SIM_HZ;

fn rec(a: &str, b: &str, seed: u64, o: Orientation, result: MatchResult, ticks: u32) -> MatchRecord {
    MatchRecord {
        strategies: [a.to_string(), b.to_string()],
        seed,
        result,
        orientation: o,
        ticks,
        produced: ProductionCounts::default(),
    }
}

fn pair_seed(x: &str, y: &str, seed: u64, x_wins: u32) -> Vec<MatchRecord> {
    let mut out = Vec::new();
    let mut k = 0;
    for (a, b) in [(x, y), (y, x)] {
        for o in Orientation::ALL {
            let x_slot = if a == x { Faction::A } else { Faction::B };
            let y_slot = if a == x { Faction::B } else { Faction::A };
            let w = if k < x_wins { x_slot } else { y_slot };
            out.push(rec(a, b, seed, o, MatchResult::Decided(w), 6 * MIN));
            k += 1;
        }
    }
    out
}

/// Seat-fair mirror seed: slot A 1 of 2, left base 1 of 2.
fn fair_mirror(s: &str, seed: u64) -> [MatchRecord; 2] {
    if seed.is_multiple_of(2) {
        [
            rec(s, s, seed, Orientation::Normal, A, 6 * MIN),
            rec(s, s, seed, Orientation::Swapped, B, 6 * MIN),
        ]
    } else {
        [
            rec(s, s, seed, Orientation::Normal, B, 6 * MIN),
            rec(s, s, seed, Orientation::Swapped, A, 6 * MIN),
        ]
    }
}

/// Normal: A is left. Swapped: B is left.
fn mirror_left_wins_both(s: &str, seed: u64) -> [MatchRecord; 2] {
    [
        rec(s, s, seed, Orientation::Normal, A, 6 * MIN),
        rec(s, s, seed, Orientation::Swapped, B, 6 * MIN),
    ]
}

fn mirror_right_wins_both(s: &str, seed: u64) -> [MatchRecord; 2] {
    [
        rec(s, s, seed, Orientation::Normal, B, 6 * MIN),
        rec(s, s, seed, Orientation::Swapped, A, 6 * MIN),
    ]
}

/// One left win and one right win, slot A taking both (`a_both`) or neither.
fn mirror_split(s: &str, seed: u64, a_both: bool) -> [MatchRecord; 2] {
    let w = if a_both { A } else { B };
    [
        rec(s, s, seed, Orientation::Normal, w, 6 * MIN),
        rec(s, s, seed, Orientation::Swapped, w, 6 * MIN),
    ]
}

fn shipped() -> Content {
    headless::content().expect("content loads")
}

fn gate(records: &[MatchRecord]) -> KillGate {
    KillGate::of(&shipped(), records, &GateSpec::default())
}

fn mirror<'g>(g: &'g KillGate, s: &str) -> &'g onus::gate::MirrorRow {
    g.seat.mirrors.iter().find(|m| m.strategy == s).expect("mirror row")
}

// ---------------------------------------------------------------- K2

/// K2 per-mirror FAIL on the **left base**, slot A exactly fair everywhere,
/// pooled left base exactly 50%: mirror `x` is won by the left base every
/// time, mirror `y` by the right base every time.
#[test]
fn k2_a_left_base_per_mirror_fail_fails_k2_with_a_fair_pool() {
    let mut recs = Vec::new();
    for seed in 0..600 {
        recs.extend(mirror_left_wins_both("x", seed));
        recs.extend(mirror_right_wins_both("y", seed));
    }
    let g = gate(&recs);
    assert_eq!(g.seat.slot_a.value, Some(0.5));
    assert_eq!(g.seat.left_spawn.value, Some(0.5));
    assert_eq!(mirror(&g, "x").left_spawn.status, Status::Fail);
    assert_eq!(g.seat.status, Status::Fail, "left-base per-mirror FAIL must fail K2");
    assert_ne!(g.status, Status::Pass);
}

/// A mirror 7 points off with a huge sample is resolved outside +/-5 and
/// FAILs K2 even when eight fair mirrors pull the pool back inside.
#[test]
fn k2_a_mirror_resolved_at_57_percent_fails_k2() {
    let mut recs = Vec::new();
    for seed in 0..5000u64 {
        for s in ["f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8"] {
            recs.extend(fair_mirror(s, seed));
        }
        // z: slot A wins 57% (both matches to A on 57% of seeds...
        // use a split mirror: left/right one each, slot A both or neither).
        recs.extend(mirror_split("z", seed, seed % 100 < 57));
    }
    let g = gate(&recs);
    let z = mirror(&g, "z");
    assert!((z.slot_a.value.unwrap() - 0.57).abs() < 1e-9);
    assert_eq!(g.seat.slot_a.status, Status::Pass, "fixture: pooled slot A in tolerance");
    assert_eq!(z.slot_a.status, Status::Fail);
    assert_eq!(g.seat.status, Status::Fail);
}

/// Broken property: K2 PASS asserts every mirror is within 50% +/- 5. Nine
/// seat-fair mirrors and one, `z`, won by the left base 80 of 124 (64.5%) at
/// F-039's per-mirror size (62 seeds). `z`'s own 95% interval is ~[54.2,
/// 73.6]: it excludes 50% outright and sits almost wholly beyond 55%. The pool
/// is 51.5% and inside tolerance, so the diff reads K2 **PASS**: the
/// undetermined mirror is silently passed. This is F-039's shipped
/// `mass_arclight` left-base row (64.9% [54.6, 74.0]) under a K2 PASS.
#[test]
fn k2_does_not_pass_while_a_mirror_reads_64_percent_left_with_an_interval_excluding_50() {
    let mut recs = Vec::new();
    for seed in 0..62u64 {
        for s in ["f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8", "f9"] {
            recs.extend(fair_mirror(s, seed));
        }
        // z: 18 seeds left-left (36 left), 44 seeds split (44 left) = 80 of
        // 124; slot A balanced (18 + 22 + 0 of the split seeds' 88).
        if seed < 18 {
            recs.extend(mirror_left_wins_both("z", seed));
        } else {
            recs.extend(mirror_split("z", seed, seed % 2 == 0));
        }
    }
    let g = gate(&recs);
    let z = mirror(&g, "z");
    let p = z.left_spawn.value.unwrap();
    assert!((p - 80.0 / 124.0).abs() < 1e-9, "fixture: {p}");
    let (lo, hi) = z.left_spawn.interval.unwrap();
    assert!(lo > 0.5, "fixture: z's left-base interval excludes 50% ({lo:.3}, {hi:.3})");
    assert_eq!(g.seat.left_spawn.status, Status::Pass, "fixture: pool in tolerance");
    assert_ne!(
        g.seat.status,
        Status::Pass,
        "K2 PASS ('mirrors within 50% +/- 5') while mirror z's left base is {:.1}% [{:.1}, {:.1}]",
        100.0 * p,
        100.0 * lo,
        100.0 * hi
    );
}

/// Broken property: too little data never reads PASS. Mirror `z` was
/// decided 6 times, all to slot A; nothing else is known about it. Its
/// interval is undetermined only because n is tiny, and K2 PASSes on the
/// pool of nine other, well-measured mirrors.
#[test]
fn k2_does_not_pass_a_mirror_known_from_six_matches_all_to_slot_a() {
    let mut recs = Vec::new();
    for seed in 0..62u64 {
        for s in ["f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8", "f9"] {
            recs.extend(fair_mirror(s, seed));
        }
    }
    for seed in 0..3u64 {
        recs.extend(mirror_split("z", seed, true));
    }
    let g = gate(&recs);
    let z = mirror(&g, "z");
    assert_eq!(z.slot_a.value, Some(1.0));
    assert_eq!(z.slot_a.n, 6);
    assert_eq!(g.seat.slot_a.status, Status::Pass, "fixture: pool in tolerance");
    assert_ne!(
        g.seat.status,
        Status::Pass,
        "K2 PASS with mirror z won by slot A 6 of 6 ({:?})",
        z.slot_a.interval
    );
}

// ---------------------------------------------------------------- K3

fn fair_pairs_with_ticks(seeds: u64, ticks_of: impl Fn(usize) -> (MatchResult, u32)) -> Vec<MatchRecord> {
    let mut recs = Vec::new();
    for seed in 0..seeds {
        recs.extend(fair_mirror("x", seed));
        recs.extend(fair_mirror("y", seed));
        recs.extend(pair_seed("x", "y", seed, 2));
    }
    for (i, r) in recs.iter_mut().enumerate() {
        let (res, t) = ticks_of(i);
        r.ticks = t;
        if res == MatchResult::Timeout {
            r.result = res;
        }
    }
    recs
}

/// 49% in band, the rest split short/long, 4 800 matches: never PASS; at 40%
/// and 30% in band, resolved, it must FAIL.
#[test]
fn k3_band_share_splits_below_half_never_pass() {
    for (in_pct, must_fail) in [(49usize, false), (40, true), (30, true)] {
        let recs = fair_pairs_with_ticks(600, |i| {
            let k = i % 100;
            let t = if k < in_pct {
                6 * MIN
            } else if k % 2 == 0 {
                3 * MIN
            } else {
                10 * MIN
            };
            (A, t)
        });
        let g = gate(&recs);
        assert_ne!(g.termination.status, Status::Pass, "{in_pct}% in band");
        if must_fail {
            assert_eq!(g.termination.band_share.status, Status::Fail, "{in_pct}% in band");
            assert_eq!(g.termination.status, Status::Fail, "{in_pct}% in band");
        }
    }
}

/// Everything decided lands in band, but 90% of matches time out: the band
/// share is 100% and must not carry K3.
#[test]
fn k3_fails_when_timeouts_dominate_even_with_every_decided_match_in_band() {
    let recs = fair_pairs_with_ticks(600, |i| {
        if i % 10 == 0 {
            (A, 6 * MIN)
        } else {
            (MatchResult::Timeout, 15 * MIN)
        }
    });
    let g = gate(&recs);
    assert_eq!(g.termination.band_share.value, Some(1.0));
    assert_eq!(g.termination.timeout_rate.status, Status::Fail);
    assert_eq!(g.termination.status, Status::Fail);
    assert_ne!(g.status, Status::Pass);
}

/// The band is inclusive at 5:00 and 8:00 and nothing outside it counts.
#[test]
fn k3_band_edges() {
    let recs = fair_pairs_with_ticks(600, |i| match i % 4 {
        0 => (A, 5 * MIN),
        1 => (A, 8 * MIN),
        2 => (A, 5 * MIN - 1),
        _ => (A, 8 * MIN + 1),
    });
    let g = gate(&recs);
    assert_eq!(g.termination.band_share.value, Some(0.5));
}

/// One seed, everything in band, nothing capped: too little to PASS K3.
#[test]
fn k3_one_seed_all_in_band_is_not_pass() {
    let recs = fair_pairs_with_ticks(1, |_| (A, 6 * MIN));
    let g = gate(&recs);
    assert_ne!(g.termination.status, Status::Pass);
    assert_ne!(g.status, Status::Pass);
}

// ---------------------------------------------------------------- losing

/// `low` loses every matchup at 45% with a large sample: each cell resolved
/// below 50%, so it is strictly losing and must be named, and naming alone
/// must not fail the gate.
#[test]
fn losing_every_matchup_at_45_percent_is_named_and_not_gated() {
    let mut recs = Vec::new();
    for seed in 0..1000u64 {
        for s in ["a", "b", "low"] {
            recs.extend(fair_mirror(s, seed));
        }
        // 9 of every 20 matches over five seeds: 45%.
        let w = [2, 2, 2, 2, 1][(seed % 5) as usize];
        recs.extend(pair_seed("low", "a", seed, w));
        recs.extend(pair_seed("low", "b", seed, w));
        recs.extend(pair_seed("a", "b", seed, 2));
    }
    let g = gate(&recs);
    let low = g.strength.rows.iter().find(|r| r.strategy == "low").unwrap();
    assert!((low.strength.value.unwrap() - 0.45).abs() < 1e-9);
    assert!(g.strength.losing.iter().any(|s| s == "low"), "losing = {:?}", g.strength.losing);
    assert_ne!(g.strength.status, Status::Fail, "naming a weak row must not fail K1");
}

/// `low`'s row is 30% with a large sample, resolved below 35%, although it
/// wins one matchup (60%). Named by the row bar.
#[test]
fn a_row_resolved_at_30_percent_with_one_winning_cell_is_named() {
    let mut recs = Vec::new();
    for seed in 0..1000u64 {
        // cells: low vs a 15%, low vs b 15%, low vs c 60% -> 30%.
        let w15 = if seed % 20 < 12 { 1 } else { 0 }; // 12/80 = 15%
        let w60 = if seed % 5 < 2 { 3 } else { 2 }; // 12/20 = 60%
        recs.extend(pair_seed("low", "a", seed, w15));
        recs.extend(pair_seed("low", "b", seed, w15));
        recs.extend(pair_seed("low", "c", seed, w60));
        recs.extend(pair_seed("a", "b", seed, 2));
        recs.extend(pair_seed("a", "c", seed, 2));
        recs.extend(pair_seed("b", "c", seed, 2));
    }
    let g = gate(&recs);
    let low = g.strength.rows.iter().find(|r| r.strategy == "low").unwrap();
    assert!((low.strength.value.unwrap() - 0.30).abs() < 1e-9, "{:?}", low.strength.value);
    assert!(g.strength.losing.iter().any(|s| s == "low"), "losing = {:?}", g.strength.losing);
}

// ---------------------------------------------------------------- row mean

/// Differential oracle: `max_mean_variance` against a brute-force grid
/// search over every cell-rate vector averaging `mu`, k = 2 and 3, weights
/// spanning five orders of magnitude.
#[test]
fn max_mean_variance_matches_a_brute_force_oracle() {
    let weight_sets: [&[f64]; 6] = [
        &[1.0, 1.0],
        &[0.1, 0.001],
        &[1.0, 1e-5],
        &[0.5, 0.25, 0.001],
        &[1e-4, 1.0, 0.3],
        &[0.2, 0.2, 0.2],
    ];
    for w in weight_sets {
        let k = w.len();
        for &mu in &[0.01, 0.1, 0.33, 0.5, 0.75, 0.97] {
            let target = mu * k as f64;
            let mut best = 0.0f64;
            let steps = 2000;
            let var = |p: &[f64]| -> f64 {
                w.iter().zip(p).map(|(wi, pi)| wi * pi * (1.0 - pi)).sum::<f64>() / (k * k) as f64
            };
            if k == 2 {
                for i in 0..=steps {
                    let p1 = i as f64 / steps as f64;
                    let p2 = target - p1;
                    if (0.0..=1.0).contains(&p2) {
                        best = best.max(var(&[p1, p2]));
                    }
                }
            } else {
                let s3 = 400;
                for i in 0..=s3 {
                    for j in 0..=s3 {
                        let p1 = i as f64 / s3 as f64;
                        let p2 = j as f64 / s3 as f64;
                        let p3 = target - p1 - p2;
                        if (0.0..=1.0).contains(&p3) {
                            best = best.max(var(&[p1, p2, p3]));
                        }
                    }
                }
            }
            let f = max_mean_variance(mu, w);
            assert!(f + 1e-12 >= best, "w {w:?} mu {mu}: function {f} < brute {best}");
            assert!(f <= best * 1.01 + 1e-9, "w {w:?} mu {mu}: function {f} > brute {best}");
        }
    }
}

/// splitmix64, deterministic.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
    fn binomial(&mut self, n: u32, p: f64) -> u32 {
        (0..n).filter(|_| self.unit() < p).count() as u32
    }
}

/// Coverage: the row-mean interval (icc 0, every match its own cluster)
/// covers the true mean of the cell rates at least ~95% of the time, at
/// extreme unequal cell sizes and rates. The small cells are kept at n >= 10,
/// where a single cell's own Wilson interval covers at nominal; below that
/// the binomial's discreteness, not the row-mean construction, decides.
#[test]
fn the_row_mean_interval_covers_the_true_mean_at_extreme_unequal_sizes() {
    let configs: [(&[u32], &[f64]); 5] = [
        (&[20, 4000], &[0.5, 0.99]),
        (&[15, 15, 3000], &[0.3, 0.7, 0.02]),
        (&[10, 1000], &[0.5, 1.0]),
        (&[25, 600, 6000], &[0.45, 0.8, 0.6]),
        (&[12, 30, 300, 3000], &[0.5, 0.5, 0.9, 0.1]),
    ];
    let mut rng = Rng(0x0B3C_0FFE);
    for (ns, ps) in configs {
        let truth = ps.iter().sum::<f64>() / ps.len() as f64;
        let trials = 3000;
        let mut covered = 0;
        for _ in 0..trials {
            let rates: Vec<f64> = ns
                .iter()
                .zip(ps)
                .map(|(&n, &p)| rng.binomial(n, p) as f64 / n as f64)
                .collect();
            let mean = rates.iter().sum::<f64>() / rates.len() as f64;
            let r = Reading::mean_of(Some(mean), ns, ns, 0.0, Rule::AtMost(0.65));
            let (lo, hi) = r.interval.unwrap();
            if lo - 1e-12 <= truth && truth <= hi + 1e-12 {
                covered += 1;
            }
        }
        let cov = covered as f64 / trials as f64;
        assert!(cov >= 0.935, "ns {ns:?} ps {ps:?}: coverage {cov:.3}");
    }
}

/// The row-mean interval always contains its own point estimate.
#[test]
fn the_row_mean_interval_contains_the_estimate() {
    for (mean, ns) in [
        (0.0, vec![1u32, 100_000]),
        (1.0, vec![1, 100_000]),
        (0.5, vec![1, 1, 1]),
        (0.999, vec![7, 100_000, 3]),
        (0.001, vec![100_000, 2]),
    ] {
        let r = Reading::mean_of(Some(mean), &ns, &ns, 0.17, Rule::AtMost(0.65));
        let (lo, hi) = r.interval.unwrap();
        assert!(lo <= mean && mean <= hi, "{mean} not in [{lo}, {hi}] for {ns:?}");
        assert!((0.0..=1.0).contains(&lo) && (0.0..=1.0).contains(&hi));
    }
}

// ---------------------------------------------------------------- order

/// Statuses, names and every reading (matched by strategy name) are the
/// same whatever order the records arrive in.
#[test]
fn the_gate_does_not_depend_on_record_order() {
    let mut recs = Vec::new();
    for seed in 0..30u64 {
        for s in ["a", "b", "c", "low"] {
            recs.extend(fair_mirror(s, seed));
        }
        recs.extend(pair_seed("a", "low", seed, 4));
        recs.extend(pair_seed("b", "low", seed, 3));
        recs.extend(pair_seed("c", "low", seed, (seed % 3) as u32 + 2));
        recs.extend(pair_seed("a", "b", seed, (seed % 2) as u32 + 1));
        recs.extend(pair_seed("a", "c", seed, 3));
        recs.extend(pair_seed("b", "c", seed, 2));
    }
    for (i, r) in recs.iter_mut().enumerate() {
        r.ticks = [3, 6, 7, 9, 15][i % 5] * MIN;
    }
    let g1 = gate(&recs);
    let mut rev = recs.clone();
    rev.reverse();
    let mut rng = Rng(7);
    let mut shuf = recs.clone();
    for i in (1..shuf.len()).rev() {
        let j = (rng.next() % (i as u64 + 1)) as usize;
        shuf.swap(i, j);
    }
    let close = |a: &Reading, b: &Reading, what: &str| {
        assert_eq!(a.status, b.status, "{what}");
        assert_eq!(a.n, b.n, "{what}");
        assert_eq!(a.clusters, b.clusters, "{what}");
        let (al, ah) = a.interval.unwrap();
        let (bl, bh) = b.interval.unwrap();
        assert!((al - bl).abs() < 1e-9 && (ah - bh).abs() < 1e-9, "{what}");
        assert!((a.value.unwrap() - b.value.unwrap()).abs() < 1e-12, "{what}");
    };
    for other in [gate(&rev), gate(&shuf)] {
        assert_eq!(g1.status, other.status);
        assert_eq!(g1.strength.status, other.strength.status);
        assert_eq!(g1.seat.status, other.seat.status);
        assert_eq!(g1.termination.status, other.termination.status);
        let sorted = |v: &Vec<String>| {
            let mut v = v.clone();
            v.sort();
            v
        };
        assert_eq!(sorted(&g1.strength.losing), sorted(&other.strength.losing));
        assert_eq!(sorted(&g1.strength.dominant), sorted(&other.strength.dominant));
        assert_eq!(sorted(&g1.strength.failing), sorted(&other.strength.failing));
        for r in &g1.strength.rows {
            let o = other.strength.rows.iter().find(|x| x.strategy == r.strategy).unwrap();
            close(&r.strength, &o.strength, &r.strategy);
            assert_eq!((r.dominant, r.losing), (o.dominant, o.losing));
        }
        for m in &g1.seat.mirrors {
            let o = other.seat.mirrors.iter().find(|x| x.strategy == m.strategy).unwrap();
            close(&m.slot_a, &o.slot_a, &m.strategy);
            close(&m.left_spawn, &o.left_spawn, &m.strategy);
        }
        close(&g1.seat.slot_a, &other.seat.slot_a, "slot a");
        close(&g1.seat.left_spawn, &other.seat.left_spawn, "left");
        close(&g1.termination.band_share, &other.termination.band_share, "band");
        close(&g1.termination.timeout_rate, &other.termination.timeout_rate, "timeouts");
    }
}

// ---------------------------------------------------------------- all-timeout

/// An all-timeout run is never PASS, and is flagged.
#[test]
fn an_all_timeout_run_is_flagged_and_not_pass() {
    let recs = fair_pairs_with_ticks(100, |_| (MatchResult::Timeout, 15 * MIN));
    let g = gate(&recs);
    assert!(g.termination.all_timeout);
    assert_eq!(g.termination.status, Status::Fail);
    assert_ne!(g.status, Status::Pass);
    assert_ne!(g.strength.status, Status::Pass);
    assert_ne!(g.seat.status, Status::Pass);
}
