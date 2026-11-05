//! Critic probes, B3 third review (after the second critic BLOCK). Each test
//! names the property it holds K2 to; a red test is a finding.
//!
//! The rule under test (user decision): K2 FAILs if any reading, pooled or
//! per mirror, resolves FAIL; it PASSes only if every reading PASSes; it is
//! undetermined otherwise.
//!
//! - a fair pool with one open mirror (open on the left base only);
//! - a FAIL on one mirror's left base, with another mirror open ahead of it in
//!   matrix order (FAIL is never masked by an open reading);
//! - the combination rule itself, including an open pool beside passing
//!   mirrors, which the real gate can hardly produce;
//! - zero mirrors; a mirror played only to timeouts (zero decided);
//! - mirrors sitting exactly on, just inside and just outside the +/-5 edges;
//! - an oracle over a grid: every seat reading's status is exactly what its
//!   own interval says against 45-55%;
//! - the printed K2 line never says PASS while a mirror is open.

use onus::batch::{BatchSettings, MatchRecord, MatchResult, ProductionCounts};
use onus::gate::{GateSpec, KillGate, Reading, Rule, Status};
use onus::headless::{self, Orientation, SIM_HZ};
use onus::report::BalanceReport;
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

/// The left base wins both (Normal: A is left; Swapped: B is left). Slot A 1 of 2.
fn left_both(s: &str, seed: u64) -> [MatchRecord; 2] {
    [
        rec(s, s, seed, Orientation::Normal, A, 6 * MIN),
        rec(s, s, seed, Orientation::Swapped, B, 6 * MIN),
    ]
}

fn right_both(s: &str, seed: u64) -> [MatchRecord; 2] {
    [
        rec(s, s, seed, Orientation::Normal, B, 6 * MIN),
        rec(s, s, seed, Orientation::Swapped, A, 6 * MIN),
    ]
}

/// Left base 1 of 2; slot A takes both (`a_both`) or neither.
fn split(s: &str, seed: u64, a_both: bool) -> [MatchRecord; 2] {
    let w = if a_both { A } else { B };
    [
        rec(s, s, seed, Orientation::Normal, w, 6 * MIN),
        rec(s, s, seed, Orientation::Swapped, w, 6 * MIN),
    ]
}

/// A mirror whose left base wins on `pct`% of `seeds` seeds (slot A exactly 50%).
fn left_leaning(s: &str, seeds: u64, pct: u64) -> Vec<MatchRecord> {
    (0..seeds)
        .flat_map(|seed| if seed % 100 < pct { left_both(s, seed) } else { right_both(s, seed) })
        .collect()
}

/// A mirror whose slot A wins on `k` of every 20 seeds (left base exactly 50%).
fn slot_a_at(s: &str, seeds: u64, k: u64) -> Vec<MatchRecord> {
    (0..seeds).flat_map(|seed| split(s, seed, seed % 20 < k)).collect()
}

fn fair_set(ids: &[&str], seeds: u64) -> Vec<MatchRecord> {
    let mut v = Vec::new();
    for seed in 0..seeds {
        for s in ids {
            v.extend(fair_mirror(s, seed));
        }
    }
    v
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

const NINE: [&str; 9] = ["f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8", "f9"];

// ------------------------------------------------------------ open mirror

/// A fair, resolved pool and nine resolved-fair mirrors; the tenth mirror's
/// slot A is resolved fair but its left base reads 54% with an interval that
/// straddles 55%. One reading is open, so K2 must not PASS — and it is not a
/// FAIL either.
#[test]
fn k2_a_fair_pool_with_one_mirror_open_on_its_left_base_is_undetermined() {
    let mut recs = fair_set(&NINE, 600);
    recs.extend(left_leaning("open", 600, 54));
    let g = gate(&recs);
    assert_eq!(g.seat.slot_a.status, Status::Pass, "fixture: pool slot A resolved fair");
    assert_eq!(g.seat.left_spawn.status, Status::Pass, "fixture: pool left base resolved fair");
    for s in NINE {
        let m = mirror(&g, s);
        assert_eq!((m.slot_a.status, m.left_spawn.status), (Status::Pass, Status::Pass), "{s}");
    }
    let o = mirror(&g, "open");
    assert_eq!(o.slot_a.status, Status::Pass);
    assert!((o.left_spawn.value.unwrap() - 0.54).abs() < 1e-12);
    assert_eq!(o.left_spawn.status, Status::Undetermined);
    assert_eq!(g.seat.status, Status::Undetermined, "one open reading holds K2 undetermined");
    assert_ne!(g.status, Status::Pass);
}

/// Feeding the open mirror more fair-leaning data resolves it, and only then
/// does K2 PASS: the PASS is reachable and is not stuck.
#[test]
fn k2_passes_once_the_open_mirror_resolves_inside_tolerance() {
    let mut recs = fair_set(&NINE, 600);
    // 52% left over 4 000 seeds: interval ~[50.5, 53.5], inside 45-55.
    recs.extend(left_leaning("open", 4000, 52));
    let g = gate(&recs);
    assert_eq!(mirror(&g, "open").left_spawn.status, Status::Pass);
    assert_eq!(g.seat.status, Status::Pass);
}

// ------------------------------------------------------------ FAIL not masked

/// The pool is exactly fair on both readings, an open mirror sits *before* the
/// failing one in matrix order, and the failing mirror fails on its left base
/// only (slot A exactly 50%). FAIL must win over the open reading.
#[test]
fn k2_a_left_base_only_fail_behind_an_open_mirror_still_fails() {
    let mut recs = Vec::new();
    // `open` first in matrix order: slot A 14 of 20, left 10 of 20 (open).
    for seed in 0..10u64 {
        recs.extend(split("open", seed, seed < 7));
    }
    // `bad` left base always wins; `good` right base always wins, so the pool's
    // left base is exactly fair. Slot A exactly 50% in both.
    for seed in 0..600 {
        recs.extend(left_both("bad", seed));
        recs.extend(right_both("good", seed));
    }
    let g = gate(&recs);
    let ids: Vec<&str> = g.seat.mirrors.iter().map(|m| m.strategy.as_str()).collect();
    assert_eq!(ids, ["open", "bad", "good"]);
    assert_eq!(mirror(&g, "open").slot_a.status, Status::Undetermined);
    assert_eq!(mirror(&g, "bad").slot_a.value, Some(0.5));
    assert_eq!(mirror(&g, "bad").slot_a.status, Status::Pass);
    assert_eq!(mirror(&g, "bad").left_spawn.status, Status::Fail);
    assert_eq!(g.seat.status, Status::Fail, "an open reading must not mask a resolved FAIL");
    assert_eq!(g.status, Status::Fail);
}

// ------------------------------------------------------------ the rule itself

/// K2 is `Status::all` over pool + per mirror: an open pool beside all-PASS
/// mirrors is undetermined; any FAIL anywhere is FAIL; no readings is not PASS.
#[test]
fn k2_combination_rule_truth_table() {
    use Status::{Fail as F, Pass as P, Undetermined as U};
    // pool open, every mirror PASS
    assert_eq!(Status::all([U, U, P, P, P, P, P, P]), U);
    assert_eq!(Status::all([P, U, P, P, P, P]), U);
    // a FAIL anywhere, after an open one, among passes
    assert_eq!(Status::all([P, P, U, U, P, F]), F);
    assert_eq!(Status::all([U, F, P, P]), F);
    assert_eq!(Status::all([P, P, P, P]), P);
    assert_eq!(Status::all(std::iter::empty::<Status>()), U);
}

// ------------------------------------------------------------ no data

/// Zero mirrors in a big, fair, in-band off-diagonal batch: K2 has nothing to
/// read and must not PASS (nor FAIL).
#[test]
fn k2_zero_mirrors_is_undetermined() {
    let mut recs = Vec::new();
    for seed in 0..600u64 {
        for (a, b) in [("x", "y"), ("y", "x")] {
            for o in Orientation::ALL {
                let w = if o == Orientation::Normal { A } else { B };
                recs.push(rec(a, b, seed, o, w, 6 * MIN));
            }
        }
    }
    let g = gate(&recs);
    assert!(g.seat.mirrors.is_empty());
    assert_eq!(g.seat.slot_a.n, 0);
    assert_eq!(g.seat.status, Status::Undetermined);
    assert_ne!(g.status, Status::Pass);
}

/// A mirror that was played but never decided (every match timed out) has
/// no reading. Beside nine resolved-fair mirrors it must still hold K2 open:
/// it has a row, its readings are undetermined, and K2 is not PASS.
#[test]
fn k2_a_mirror_with_zero_decided_matches_holds_k2_undetermined() {
    let mut recs = fair_set(&NINE, 600);
    for seed in 0..40u64 {
        for o in Orientation::ALL {
            recs.push(rec("stall", "stall", seed, o, MatchResult::Timeout, 15 * MIN));
        }
    }
    let g = gate(&recs);
    assert_eq!(g.seat.slot_a.status, Status::Pass, "fixture: pool resolved fair");
    let s = mirror(&g, "stall");
    assert_eq!(s.slot_a.n, 0);
    assert_eq!(s.slot_a.value, None);
    assert_eq!(s.slot_a.status, Status::Undetermined);
    assert_eq!(s.left_spawn.status, Status::Undetermined);
    assert_eq!(g.seat.status, Status::Undetermined, "an unread mirror never lets K2 PASS");
}

/// Every mirror played only to timeouts: pool and rows have no data.
#[test]
fn k2_all_mirrors_timed_out_is_undetermined() {
    let mut recs = Vec::new();
    for seed in 0..100u64 {
        for o in Orientation::ALL {
            recs.push(rec("x", "x", seed, o, MatchResult::Timeout, 15 * MIN));
        }
    }
    let g = gate(&recs);
    assert_eq!(g.seat.mirrors.len(), 1);
    assert_eq!(g.seat.slot_a.n, 0);
    assert_eq!(g.seat.status, Status::Undetermined);
}

// ------------------------------------------------------------ tolerance edges

/// A mirror whose slot A sits exactly on 55% or 45% is never resolved either
/// way however large the sample (its interval straddles the edge): K2 is
/// undetermined. Just inside (54%, 46%) at that size PASSes; just outside
/// (56%, 44%) FAILs.
#[test]
fn k2_mirrors_at_and_around_the_tolerance_edges() {
    let seeds = 20_000;
    let case = |k: u64| {
        let mut recs = fair_set(&["f1", "f2"], 2000);
        recs.extend(slot_a_at("edge", seeds, k));
        let g = gate(&recs);
        let e = mirror(&g, "edge").clone();
        assert!((e.slot_a.value.unwrap() - k as f64 / 20.0).abs() < 1e-12);
        (e.slot_a.status, g.seat.status)
    };
    // exactly on the edges
    assert_eq!(case(11), (Status::Undetermined, Status::Undetermined), "55% exactly");
    assert_eq!(case(9), (Status::Undetermined, Status::Undetermined), "45% exactly");
    // fixture resolution at 40 000 decided: half-width ~0.57 points
    let inside = |k: u64| (0..seeds).flat_map(|s| split("edge", s, s % 50 < k)).collect::<Vec<_>>();
    let run = |recs: Vec<MatchRecord>| {
        let mut all = fair_set(&["f1", "f2"], 2000);
        all.extend(recs);
        let g = gate(&all);
        (mirror(&g, "edge").slot_a.status, g.seat.status)
    };
    assert_eq!(run(inside(27)), (Status::Pass, Status::Pass), "54%");
    assert_eq!(run(inside(23)), (Status::Pass, Status::Pass), "46%");
    assert_eq!(run(inside(28)), (Status::Fail, Status::Fail), "56%");
    assert_eq!(run(inside(22)), (Status::Fail, Status::Fail), "44%");
}

/// Oracle: for every seat reading over a grid of (successes, n), the status
/// is exactly what its own interval says against [45%, 55%]: PASS iff the
/// interval is inside (inclusive), FAIL iff it is wholly outside (strict),
/// undetermined otherwise. And a reading whose point is outside the band is
/// never PASS; one whose point is inside is never FAIL.
#[test]
fn every_seat_reading_status_matches_its_interval_oracle() {
    let rule = Rule::Within { centre: 0.5, tolerance: 0.05 };
    let mut seen = [0u32; 3];
    for n in (1u32..=4000).step_by(37) {
        let clusters = n.div_ceil(2);
        for h in (0..=2 * n as u64).step_by(((n / 40).max(1)) as usize) {
            let r = Reading::proportion_at_least(h, n, clusters, 0.17, 1.38, rule);
            let (lo, hi) = r.interval.expect("n > 0");
            let p = r.value.unwrap();
            let oracle = if lo >= 0.45 && hi <= 0.55 {
                Status::Pass
            } else if lo > 0.55 || hi < 0.45 {
                Status::Fail
            } else {
                Status::Undetermined
            };
            assert_eq!(r.status, oracle, "h {h} n {n} p {p} [{lo}, {hi}]");
            if !(0.45..=0.55).contains(&p) {
                assert_ne!(r.status, Status::Pass, "h {h} n {n}");
            } else {
                assert_ne!(r.status, Status::Fail, "h {h} n {n}");
            }
            assert!(lo <= p + 1e-12 && p <= hi + 1e-12, "interval contains its point: h {h} n {n} p {p} [{lo}, {hi}]");
            seen[match r.status {
                Status::Pass => 0,
                Status::Fail => 1,
                Status::Undetermined => 2,
            }] += 1;
        }
    }
    assert!(seen.iter().all(|&c| c > 0), "grid reaches all three statuses: {seen:?}");
}

// ------------------------------------------------------------ the printed report

/// The stdout table: with one mirror open, the K2 header line must read
/// undetermined, never PASS, and the RON must carry the same status.
#[test]
fn the_printed_k2_line_is_not_pass_while_a_mirror_is_open() {
    let mut recs = fair_set(&NINE, 600);
    recs.extend(left_leaning("open", 600, 54));
    let r = BalanceReport::of(&shipped(), &BatchSettings::default().with_seeds(600), &recs);
    assert_eq!(r.gate.seat.status, Status::Undetermined);
    let text = r.to_string();
    let k2 = text.lines().find(|l| l.trim_start().starts_with("K2 seat")).expect("K2 line");
    assert!(k2.contains("undetermined"), "{k2}");
    assert!(!k2.contains("PASS"), "{k2}");
    let back = BalanceReport::from_ron(&r.to_ron().unwrap()).unwrap();
    assert_eq!(back.gate.seat.status, Status::Undetermined);
}
