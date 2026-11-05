//! Critic probes for B3 AC3/AC4 (kill gate, report). Each test states the
//! property it holds the diff to; a red test is a finding, not a style nit.
//!
//! - K3 must not read PASS when almost no match terminates in the 5-8 min
//!   target ("matches terminate in target").
//! - K2 must not read PASS when a single mirror is resolved far outside
//!   50% +/- tolerance ("mirrors within tolerance of 50%").
//! - A strategy that loses every matchup and whose row is resolved far below
//!   the mirror image of the 65% bar must be surfaced by name (critic probe:
//!   "a strictly-losing strategy is surfaced by name").
//! - F-038's "the row-mean interval is conservative" must hold for unequal
//!   cell sizes.
//! - Running the test suite must not overwrite the default report path in the
//!   package root.
//! - The binary's report is byte-identical across two runs of one batch.

use std::process::Command;

use onus::batch::{MatchRecord, MatchResult, ProductionCounts};
use onus::gate::{GateSpec, KillGate, Reading, Rule, Status};
use onus::headless::{self, Orientation, SIM_HZ};
use onus::report::{BalanceReport, DEFAULT_REPORT_PATH};
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

/// `x` vs `y` on one seed: both slot orders, both orientations; `x` wins
/// `x_wins` of the four.
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

/// A seat-fair mirror seed (slot A 1 of 2, left base 1 of 2).
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

fn shipped() -> Content {
    headless::content().expect("content loads")
}

fn gate(records: &[MatchRecord]) -> KillGate {
    KillGate::of(&shipped(), records, &GateSpec::default())
}

/// Broken property: K3 "matches terminate in target". 47% of matches end at
/// 2:00, 47% at 12:00, 6% inside 5-8 min, no timeouts, 4 800 matches. The
/// median sits in the gap, so the median-only reading PASSes K3 although 94%
/// of matches end outside the target.
#[test]
fn probe_k3_does_not_pass_when_six_percent_of_matches_are_in_target() {
    let mut recs = Vec::new();
    for seed in 0..600 {
        recs.extend(fair_mirror("x", seed));
        recs.extend(fair_mirror("y", seed));
        recs.extend(pair_seed("x", "y", seed, 2));
    }
    for (i, r) in recs.iter_mut().enumerate() {
        r.ticks = match i % 100 {
            0..=5 => 6 * MIN,
            k if k % 2 == 0 => 2 * MIN,
            _ => 12 * MIN,
        };
    }
    let g = gate(&recs);
    let in_band = g.termination.band_share.value.unwrap();
    assert!(in_band < 0.07, "fixture: {in_band}");
    assert_eq!(g.termination.timeout_rate.status, Status::Pass, "fixture: no timeouts");
    assert_ne!(
        g.termination.status,
        Status::Pass,
        "K3 reads PASS with {:.1}% of matches inside 5-8 min (below {:?}, beyond {:?})",
        100.0 * in_band,
        g.termination.below.interval,
        g.termination.beyond.interval
    );
    assert_ne!(g.status, Status::Pass);
}

/// Broken property: K2 "mirrors within tolerance of 50%". Mirror `x` is won
/// by slot A every time and mirror `y` by slot B every time, 1 200 decided
/// matches each. Each mirror is resolved 50 points outside tolerance; the
/// pooled slot-A and left-base shares are both exactly 50%, so K2 PASSes.
#[test]
fn probe_k2_does_not_pass_when_one_mirror_is_resolved_outside_tolerance() {
    let mut recs = Vec::new();
    for seed in 0..600 {
        recs.push(rec("x", "x", seed, Orientation::Normal, A, 6 * MIN));
        recs.push(rec("x", "x", seed, Orientation::Swapped, A, 6 * MIN));
        recs.push(rec("y", "y", seed, Orientation::Normal, B, 6 * MIN));
        recs.push(rec("y", "y", seed, Orientation::Swapped, B, 6 * MIN));
        recs.extend(pair_seed("x", "y", seed, 2));
    }
    let g = gate(&recs);
    let x = &g.seat.mirrors[0];
    assert_eq!(x.strategy, "x");
    assert_eq!(x.slot_a.value, Some(1.0));
    assert_eq!(x.slot_a.status, Status::Fail, "the per-mirror data is resolved");
    assert_ne!(
        g.seat.status,
        Status::Pass,
        "K2 PASSes while mirror `x` is won by slot A in 1200 of 1200 (pooled slot A {:?}, left {:?})",
        g.seat.slot_a.value,
        g.seat.left_spawn.value
    );
    assert_ne!(g.status, Status::Pass);
}

/// Broken property: "a strictly-losing strategy is surfaced by name". `low`
/// loses every one of its three matchups (0%, 0%, 37.5%); its row mean is
/// 12.5% and resolved below 35% (the mirror image of the 65% bar). The
/// shipped analogue is `rush` (6.2%, every cell below 50%, F-040).
#[test]
fn probe_a_strategy_losing_every_matchup_is_named() {
    let mut recs = Vec::new();
    for seed in 0..4u64 {
        recs.extend(pair_seed("a", "low", seed, 4));
        recs.extend(pair_seed("b", "low", seed, 4));
        // low takes 2, 2, 1, 1 of 4 from c: 6 of 16 = 37.5%.
        recs.extend(pair_seed("low", "c", seed, if seed < 2 { 2 } else { 1 }));
        recs.extend(pair_seed("a", "b", seed, 2));
        recs.extend(pair_seed("a", "c", seed, 2));
        recs.extend(pair_seed("b", "c", seed, 2));
    }
    let g = gate(&recs);
    let low = g.strength.rows.iter().find(|r| r.strategy == "low").unwrap();
    assert_eq!(low.strength.value, Some(0.125));
    let (_, hi) = low.strength.interval.unwrap();
    assert!(hi < 0.35, "fixture: row resolved below 35%, hi {hi}");
    assert!(
        g.strength.losing.iter().any(|s| s == "low"),
        "`low` loses 3 of 3 matchups, row 12.5% (hi {hi:.3}), yet losing = {:?}",
        g.strength.losing
    );
}

/// Broken claim (F-038, `Reading::mean_of` docs): the row-mean interval is
/// "conservative". With unequal cell sizes it is narrower than the normal
/// interval on the true variance of the mean. Cells: 5 of 10 (50%) and 1000
/// of 1000 (100%); mean 75%; true sd of the mean = sqrt(0.25/10)/2 = 0.0791.
#[test]
fn probe_the_row_mean_interval_is_at_least_as_wide_as_the_true_one() {
    let r = Reading::mean_of(Some(0.75), &[10, 1000], &[10, 1000], 0.0, Rule::AtMost(0.65));
    let (lo, hi) = r.interval.unwrap();
    let true_sd = (0.5f64 * 0.5 / 10.0).sqrt() / 2.0;
    let normal_width = 2.0 * 1.959_963_985 * true_sd;
    assert!(
        hi - lo >= 0.95 * normal_width,
        "row-mean interval [{lo:.3}, {hi:.3}] width {:.3} < true-variance width {normal_width:.3}",
        hi - lo
    );
}

/// Broken hygiene: the pre-existing tests that run `balance` without
/// `--report` (b2_orientation, critic_b2_ac2, critic_b2_ac3, critic_b3_ac1)
/// now write `balance_report.ron` into the package root, overwriting a real
/// batch's report with a toy one. This replays one of those invocations and
/// checks the package-root report is left alone.
#[test]
fn probe_running_the_bin_as_the_suite_does_leaves_the_package_root_report_alone() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(DEFAULT_REPORT_PATH);
    let before = std::fs::read(&root).ok();
    let out = Command::new(env!("CARGO_BIN_EXE_balance"))
        .args(["--only", "rush", "--tick-cap", "1"])
        .output()
        .expect("binary runs");
    assert!(out.status.success());
    let after = std::fs::read(&root).ok();
    assert_eq!(
        before.is_some(),
        after.is_some(),
        "a test run created {} in the package root",
        root.display()
    );
    assert!(before == after, "a test run overwrote {}", root.display());
}

/// Determinism: two runs of the same real batch write byte-identical reports.
#[test]
fn probe_the_bin_report_is_byte_identical_across_runs() {
    let dir = std::env::temp_dir();
    let run = |tag: &str| {
        let path = dir.join(format!("onus_critic_b3_{tag}_{}.ron", std::process::id()));
        let out = Command::new(env!("CARGO_BIN_EXE_balance"))
            .args(["--only", "rush,mass_ripper", "--seeds", "1", "--tick-cap", "9000", "--report"])
            .arg(&path)
            .output()
            .expect("binary runs");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let bytes = std::fs::read(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        // The last line names the (per-run) path; everything else must match.
        let stdout = String::from_utf8(out.stdout).unwrap().replace(&path.display().to_string(), "PATH");
        (bytes, stdout)
    };
    let (a, sa) = run("a");
    let (b, sb) = run("b");
    assert_eq!(a, b, "report bytes differ across runs");
    assert_eq!(sa, sb, "stdout differs across runs");
    let text = String::from_utf8(a).unwrap();
    let r = BalanceReport::from_ron(&text).unwrap();
    assert_eq!(r.to_ron().unwrap(), text, "real report re-serializes to the same bytes");
}
