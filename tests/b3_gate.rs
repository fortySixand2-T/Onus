//! L2 integration tests for **B3 AC3** — the kill-criteria gate.
//!
//! [`onus::gate::KillGate`] reads a batch against the three kill criteria
//! (K1 no strategy wins more than 65% regardless of counter, K2 mirrors within
//! tolerance of 50%, K3 the median match terminates in the 5–8 minute band
//! with few timeouts) and returns
//! PASS / FAIL / undetermined for each, read off a clustered 95% Wilson
//! interval — never off a point estimate. What is encoded here:
//!
//!   - **the interval decides**: a reading PASSes only when its whole interval
//!     clears the threshold, FAILs only when its whole interval is on the
//!     wrong side, and is undetermined otherwise — including with no data;
//!   - **PASS is reachable** (a large, fair, in-band synthetic batch passes all
//!     three) and **each criterion can FAIL on its own**;
//!   - **seat bias** is caught by slot and by spawn base, a fair mirror set at
//!     two seeds is undetermined, and a batch with no mirror cannot pass K2;
//!   - **a dominant or strictly losing strategy is surfaced by name**;
//!   - **an all-timeout run is flagged and fails**, never reads as balanced;
//!   - **clustering widens the interval**: the same counts over fewer seeds are
//!     less certain, and the row-mean interval reduces to the cell interval
//!     for a single opponent;
//!   - **an injected imbalance** — one unit's offense multiplied in a content
//!     fixture, played for real — fails K1 and names the strategy, even
//!     against that unit's designed counter;
//!   - **the shipped content's reading** on the B3 pentagon batch is pinned
//!     (F-039) — a measurement, not a target.

use onus::batch::{self, BatchSettings, MatchRecord, MatchResult, ProductionCounts};
use onus::gate::{design_effect, GateSpec, KillGate, Reading, Rule, Status};
use onus::headless::{self, MatchSettings, Orientation, SIM_HZ};
use onus::metrics::{LengthBand, WinMatrix};
use onus::sim::content::Content;
use onus::sim::spatial::Faction;

const A: MatchResult = MatchResult::Decided(Faction::A);
const B: MatchResult = MatchResult::Decided(Faction::B);
const DRAW: MatchResult = MatchResult::MutualLoss;
const CAPPED: MatchResult = MatchResult::Timeout;
const SIX_MIN: u32 = 6 * 60 * SIM_HZ;

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

/// One seed of a perfectly seat-fair mirror of `s`: in total, slot A wins one
/// of two and the left base wins one of two (alternating which by seed).
fn fair_mirror(s: &str, seed: u64) -> [MatchRecord; 2] {
    if seed.is_multiple_of(2) {
        // Normal: A (left) wins. Swapped: B (left) wins.
        [
            rec(s, s, seed, Orientation::Normal, A, SIX_MIN),
            rec(s, s, seed, Orientation::Swapped, B, SIX_MIN),
        ]
    } else {
        // Normal: B (right) wins. Swapped: A (right) wins.
        [
            rec(s, s, seed, Orientation::Normal, B, SIX_MIN),
            rec(s, s, seed, Orientation::Swapped, A, SIX_MIN),
        ]
    }
}

/// One seed of `x` vs `y`, both slot orders and both orientations, `x`
/// winning `x_wins` of the four (0..=4), every match six minutes long.
fn pair_seed(x: &str, y: &str, seed: u64, x_wins: u32) -> Vec<MatchRecord> {
    let mut out = Vec::new();
    let mut k = 0;
    for (a, b) in [(x, y), (y, x)] {
        for o in Orientation::ALL {
            let x_slot = if a == x { Faction::A } else { Faction::B };
            let y_slot = if a == x { Faction::B } else { Faction::A };
            let winner = if k < x_wins { x_slot } else { y_slot };
            out.push(rec(a, b, seed, o, MatchResult::Decided(winner), SIX_MIN));
            k += 1;
        }
    }
    out
}

/// A balanced round robin over `ids` across `seeds` seeds: every pair split
/// 2–2 per seed, every mirror seat-fair, every match six minutes.
fn balanced(ids: &[&str], seeds: u64) -> Vec<MatchRecord> {
    let mut out = Vec::new();
    for seed in 0..seeds {
        for (i, x) in ids.iter().enumerate() {
            out.extend(fair_mirror(x, seed));
            for y in &ids[i + 1..] {
                out.extend(pair_seed(x, y, seed, 2));
            }
        }
    }
    out
}

fn shipped() -> Content {
    headless::content().expect("assets/data/*.ron parse into sim structs")
}

fn gate(records: &[MatchRecord]) -> KillGate {
    KillGate::of(&shipped(), records, &GateSpec::default())
}

// ---- the machinery ----------------------------------------------------------

#[test]
fn the_default_thresholds_are_the_stated_ones() {
    let s = GateSpec::default();
    assert_eq!(s.max_strength, 0.65);
    assert_eq!(s.mirror_tolerance, 0.05);
    assert_eq!(s.band, LengthBand::default());
    assert_eq!(s.min_band_share, 0.5);
    assert_eq!(s.max_timeout_rate, 0.05);
    assert_eq!(s.icc, 0.17, "F-031: deff 1.51 at 4 matches a cluster");
}

#[test]
fn status_combines_fail_first_and_pass_only_when_everything_passes() {
    use Status::*;
    assert_eq!(Status::all([Pass, Pass]), Pass);
    assert_eq!(Status::all([Pass, Undetermined]), Undetermined);
    assert_eq!(Status::all([Undetermined, Fail, Pass]), Fail);
    assert_eq!(Status::all([]), Undetermined, "nothing measured is not a pass");
    assert_eq!(Pass.label(), "PASS");
    assert_eq!(Fail.label(), "FAIL");
    assert_eq!(Undetermined.label(), "undetermined");
}

#[test]
fn a_reading_is_judged_by_its_interval_not_its_point() {
    // 30 of 100 independent: interval ~[0.22, 0.40].
    let at_most = |t| Reading::proportion(60, 100, 100, 0.0, Rule::AtMost(t)).status;
    assert_eq!(at_most(0.5), Status::Pass);
    assert_eq!(at_most(0.35), Status::Undetermined, "the point is below, the interval is not");
    assert_eq!(at_most(0.2), Status::Fail);
    let at_least = |t| Reading::proportion(60, 100, 100, 0.0, Rule::AtLeast(t)).status;
    assert_eq!(at_least(0.2), Status::Pass);
    assert_eq!(at_least(0.35), Status::Undetermined);
    assert_eq!(at_least(0.5), Status::Fail);
    let within = |c, t| Reading::proportion(60, 100, 100, 0.0, Rule::Within { centre: c, tolerance: t }).status;
    assert_eq!(within(0.3, 0.15), Status::Pass);
    assert_eq!(within(0.3, 0.05), Status::Undetermined);
    assert_eq!(within(0.5, 0.05), Status::Fail);
}

#[test]
fn no_data_is_undetermined_under_every_rule() {
    for rule in [
        Rule::AtMost(0.65),
        Rule::AtLeast(0.5),
        Rule::Within { centre: 0.5, tolerance: 0.05 },
    ] {
        let r = Reading::proportion(0, 0, 0, 0.17, rule);
        assert_eq!(r.value, None);
        assert_eq!(r.interval, None);
        assert_eq!(r.status, Status::Undetermined, "{rule:?}");
    }
}

#[test]
fn the_design_effect_is_one_plus_m_minus_one_rho() {
    assert_eq!(design_effect(100, 100, 0.17), 1.0, "one match a cluster");
    assert!((design_effect(200, 100, 0.17) - 1.17).abs() < 1e-12, "a mirror seed");
    assert!((design_effect(400, 100, 0.17) - 1.51).abs() < 1e-12, "F-031's pair seed");
    assert_eq!(design_effect(400, 100, 0.0), 1.0);
    assert_eq!(design_effect(0, 0, 0.17), 1.0);
}

#[test]
fn clustering_widens_the_interval() {
    let independent = Reading::proportion(120, 100, 100, 0.17, Rule::AtMost(0.65));
    let clustered = Reading::proportion(120, 100, 25, 0.17, Rule::AtMost(0.65));
    let w = |r: &Reading| {
        let (lo, hi) = r.interval.unwrap();
        hi - lo
    };
    assert!(w(&clustered) > w(&independent));
    assert!((clustered.n_eff - 100.0 / 1.51).abs() < 1e-9);
}

#[test]
fn a_row_mean_over_one_cell_has_that_cells_wilson_interval() {
    let recs: Vec<MatchRecord> = (0..7)
        .map(|s| rec("x", "y", s, Orientation::Normal, A, SIX_MIN))
        .chain((7..10).map(|s| rec("x", "y", s, Orientation::Normal, B, SIX_MIN)))
        .collect();
    let m = WinMatrix::of(&recs);
    let spec = GateSpec { icc: 0.0, ..GateSpec::default() };
    let g = KillGate::of(&shipped(), &recs, &spec);
    assert_eq!(g.strength.rows[0].strategy, "x");
    assert_eq!(g.strength.rows[0].strength.value, Some(0.7));
    assert_eq!(
        g.strength.rows[0].strength.interval,
        m.cell(0, 1).unwrap().wilson_interval(),
        "k = 1, ρ = 0: the row interval is the cell's, to the bit"
    );
}

// ---- PASS is reachable, and each criterion fails on its own -----------------

#[test]
fn a_large_fair_in_band_batch_passes_every_criterion() {
    let g = gate(&balanced(&["x", "y", "z"], 600));
    assert_eq!(g.strength.status, Status::Pass, "{:#?}", g.strength);
    assert_eq!(g.seat.status, Status::Pass, "{:#?}", g.seat);
    assert_eq!(g.termination.status, Status::Pass, "{:#?}", g.termination);
    assert_eq!(g.status, Status::Pass);
    assert!(g.strength.failing.is_empty());
    assert!(g.strength.dominant.is_empty());
    assert!(g.strength.losing.is_empty());
    assert_eq!(g.seat.slot_a.value, Some(0.5));
    assert_eq!(g.seat.left_spawn.value, Some(0.5));
    assert_eq!(g.seat.slot_a.n, 3 * 600 * 2);
    assert_eq!(g.seat.slot_a.clusters, 3 * 600);
    assert!(!g.termination.all_timeout);
    assert_eq!(g.termination.decided_median, Some(SIX_MIN));
}

#[test]
fn the_same_fair_batch_at_two_seeds_decides_nothing() {
    let g = gate(&balanced(&["x", "y", "z"], 2));
    assert_eq!(g.strength.status, Status::Undetermined);
    assert_eq!(g.seat.status, Status::Undetermined);
    assert_ne!(g.status, Status::Pass, "a small batch is never a clean bill");
}

#[test]
fn a_slot_biased_mirror_set_fails_seat_bias() {
    let mut recs = Vec::new();
    for seed in 0..600 {
        // Slot A wins 7 of every 10 mirrors; the base is split evenly.
        for k in 0..10u64 {
            let o = if k.is_multiple_of(2) { Orientation::Normal } else { Orientation::Swapped };
            let r = if k < 7 { A } else { B };
            recs.push(rec("x", "x", seed * 10 + k, o, r, SIX_MIN));
        }
    }
    let g = gate(&recs);
    assert_eq!(g.seat.slot_a.status, Status::Fail);
    assert!((g.seat.slot_a.value.unwrap() - 0.7).abs() < 1e-12);
    assert_eq!(g.seat.status, Status::Fail);
    assert_eq!(g.status, Status::Fail);
}

#[test]
fn a_spawn_biased_mirror_set_fails_seat_bias_even_with_fair_slots() {
    let mut recs = Vec::new();
    for seed in 0..600 {
        // The left base wins both: slot A wins Normal, slot B wins Swapped.
        recs.push(rec("x", "x", seed, Orientation::Normal, A, SIX_MIN));
        recs.push(rec("x", "x", seed, Orientation::Swapped, B, SIX_MIN));
    }
    let g = gate(&recs);
    assert_eq!(g.seat.slot_a.value, Some(0.5));
    assert_eq!(g.seat.slot_a.status, Status::Pass);
    assert_eq!(g.seat.left_spawn.value, Some(1.0));
    assert_eq!(g.seat.left_spawn.status, Status::Fail);
    assert_eq!(g.seat.status, Status::Fail);
}

#[test]
fn a_mutual_loss_is_half_a_seat_win() {
    let recs: Vec<MatchRecord> = (0..4)
        .map(|s| rec("x", "x", s, Orientation::Normal, DRAW, SIX_MIN))
        .collect();
    let g = gate(&recs);
    assert_eq!(g.seat.slot_a.value, Some(0.5));
    assert_eq!(g.seat.left_spawn.value, Some(0.5));
    assert_eq!(g.seat.slot_a.n, 4);
}

#[test]
fn a_batch_without_mirrors_cannot_pass_seat_bias() {
    let recs: Vec<MatchRecord> = (0..600).flat_map(|s| pair_seed("x", "y", s, 2)).collect();
    let g = gate(&recs);
    assert_eq!(g.seat.slot_a.n, 0);
    assert_eq!(g.seat.status, Status::Undetermined);
    assert!(g.seat.mirrors.is_empty());
    assert_ne!(g.status, Status::Pass);
}

#[test]
fn per_mirror_rows_are_reported_in_matrix_order_and_not_gated() {
    let mut recs = balanced(&["x", "y"], 600);
    // One mirror leans 60/40 on slot A; the pooled reading still passes.
    for seed in 600..620 {
        recs.push(rec("y", "y", seed, Orientation::Normal, A, SIX_MIN));
    }
    let g = gate(&recs);
    let ids: Vec<&str> = g.seat.mirrors.iter().map(|m| m.strategy.as_str()).collect();
    assert_eq!(ids, ["x", "y"]);
    assert_eq!(g.seat.mirrors[0].slot_a.n, 1200);
    assert_eq!(g.seat.mirrors[1].slot_a.n, 1220);
    assert_eq!(g.seat.status, Status::Pass);
}

// ---- K1: dominance, surfaced by name ----------------------------------------

#[test]
fn a_strictly_dominant_and_a_strictly_losing_strategy_are_named() {
    let mut recs = Vec::new();
    for seed in 0..30 {
        recs.extend(pair_seed("top", "mid", seed, 4));
        recs.extend(pair_seed("top", "low", seed, 4));
        recs.extend(pair_seed("mid", "low", seed, 4));
    }
    let g = gate(&recs);
    assert_eq!(g.strength.dominant, ["top"]);
    assert_eq!(g.strength.losing, ["low"]);
    assert_eq!(g.strength.failing, ["top"], "100% over 240 matches clears 65%");
    assert_eq!(g.strength.status, Status::Fail);
    assert_eq!(g.status, Status::Fail);
    let mid = &g.strength.rows[1];
    assert_eq!(mid.strategy, "mid");
    assert_eq!(mid.strength.value, Some(0.5));
    assert!(!mid.dominant && !mid.losing);
}

#[test]
fn a_strong_row_at_a_small_sample_is_undetermined_not_failing() {
    let mut recs = Vec::new();
    for seed in 0..2 {
        recs.extend(pair_seed("top", "low", seed, 3));
    }
    let g = gate(&recs);
    assert_eq!(g.strength.rows[0].strength.value, Some(0.75));
    assert_eq!(g.strength.rows[0].strength.status, Status::Undetermined);
    assert!(g.strength.failing.is_empty());
    assert!(g.strength.dominant.is_empty(), "6 of 8 is not resolved above 50%");
}

#[test]
fn an_opponent_with_nothing_decided_blocks_dominant() {
    let mut recs = Vec::new();
    for seed in 0..30 {
        recs.extend(pair_seed("top", "mid", seed, 4));
        recs.push(rec("top", "low", seed, Orientation::Normal, CAPPED, 54_000));
    }
    let g = gate(&recs);
    assert_eq!(g.strength.rows[0].opponents, 2);
    assert_eq!(g.strength.rows[0].cells, 1);
    assert!(g.strength.dominant.is_empty(), "never beat `low` — not dominant");
    assert_eq!(g.strength.failing, ["top"]);
}

#[test]
fn a_mass_probe_row_is_labelled_with_its_unit() {
    let recs: Vec<MatchRecord> = (0..2).flat_map(|s| pair_seed("mass_ripper", "rush", s, 2)).collect();
    let g = gate(&recs);
    assert_eq!(g.strength.rows[0].unit.as_deref(), Some("ripper"));
    assert_eq!(g.strength.rows[1].unit, None, "rush is not a mass probe");
}

// ---- K3: termination, and the all-timeout run -------------------------------

#[test]
fn an_all_timeout_run_is_flagged_and_fails_never_balanced() {
    let recs: Vec<MatchRecord> = (0..50)
        .flat_map(|s| {
            [
                rec("x", "y", s, Orientation::Normal, CAPPED, 54_000),
                rec("x", "x", s, Orientation::Normal, CAPPED, 54_000),
            ]
        })
        .collect();
    let g = gate(&recs);
    assert!(g.termination.all_timeout);
    assert_eq!(g.termination.timeout_rate.value, Some(1.0));
    assert_eq!(g.termination.beyond.value, Some(1.0), "a timeout did not terminate in target");
    assert_eq!(g.termination.beyond.status, Status::Fail);
    assert_eq!(g.termination.status, Status::Fail);
    assert_eq!(g.termination.band_share.status, Status::Undetermined, "nothing decided");
    assert_eq!(g.termination.decided_median, None);
    assert_eq!(g.strength.status, Status::Undetermined);
    assert_eq!(g.seat.status, Status::Undetermined);
    assert_eq!(g.status, Status::Fail);
}

#[test]
fn short_matches_fail_the_band_and_capped_ones_fail_the_timeout_rate() {
    let mut short = balanced(&["x", "y"], 600);
    for r in &mut short {
        r.ticks = 3 * 60 * SIM_HZ;
    }
    let g = gate(&short);
    assert_eq!(g.termination.below.value, Some(1.0));
    assert_eq!(g.termination.below.status, Status::Fail);
    assert_eq!(g.termination.beyond.status, Status::Pass);
    assert_eq!(g.termination.band_share.value, Some(0.0));
    assert_eq!(g.termination.band_share.status, Status::Fail);
    assert_eq!(g.termination.timeout_rate.status, Status::Pass);
    assert_eq!(g.termination.status, Status::Fail);

    let mut capped = balanced(&["x", "y"], 600);
    for r in capped.iter_mut().step_by(5) {
        r.result = CAPPED;
        r.ticks = 54_000;
    }
    let g = gate(&capped);
    assert!((g.termination.timeout_rate.value.unwrap() - 0.2).abs() < 1e-3);
    assert_eq!(g.termination.timeout_rate.status, Status::Fail);
    assert_eq!(g.termination.band_share.status, Status::Pass);
    assert_eq!(g.termination.beyond.status, Status::Pass, "20% past the band is not the median");
    assert_eq!(g.termination.status, Status::Fail);

    let mut long = balanced(&["x", "y"], 600);
    for r in &mut long {
        r.ticks = 10 * 60 * SIM_HZ;
    }
    let g = gate(&long);
    assert_eq!(g.termination.beyond.value, Some(1.0));
    assert_eq!(g.termination.beyond.status, Status::Fail);
    assert_eq!(g.termination.timeout_rate.status, Status::Pass);
    assert_eq!(g.termination.status, Status::Fail);
}

/// 30% of matches end at 3:00, 40% at 6:00, 30% at 10:00: the median match is
/// in the band but only 40% of decided matches are. K3 is the band share
/// (B3.5's design metric), so it FAILs; the median and the before/after
/// shares are reported context and do not rescue it (critic B3, F-038).
#[test]
fn the_band_share_gates_k3_even_when_the_median_is_in_band() {
    let mut recs = balanced(&["x", "y"], 600);
    for (i, r) in recs.iter_mut().enumerate() {
        r.ticks = match i % 10 {
            0..=2 => 3 * 60 * SIM_HZ,
            3..=6 => 6 * 60 * SIM_HZ,
            _ => 10 * 60 * SIM_HZ,
        };
    }
    let g = gate(&recs);
    assert!((g.termination.below.value.unwrap() - 0.3).abs() < 1e-3);
    assert!((g.termination.beyond.value.unwrap() - 0.3).abs() < 1e-3);
    assert_eq!(g.termination.below.status, Status::Pass);
    assert_eq!(g.termination.beyond.status, Status::Pass);
    assert_eq!(g.termination.band_share.status, Status::Fail);
    assert_eq!(g.termination.timeout_rate.status, Status::Pass);
    assert_eq!(g.termination.decided_median, Some(6 * 60 * SIM_HZ));
    assert_eq!(g.termination.status, Status::Fail, "40% in band is not 'terminate in target'");
    assert_eq!(g.status, Status::Fail);
}

// ---- order and determinism ---------------------------------------------------

#[test]
fn the_gate_is_a_pure_function_of_the_record_set() {
    let mut recs = Vec::new();
    for seed in 0..40 {
        recs.extend(pair_seed("top", "mid", seed, 4));
        recs.extend(pair_seed("mid", "low", seed, (seed % 5) as u32));
        recs.extend(fair_mirror("mid", seed));
    }
    let a = gate(&recs);
    assert_eq!(a, gate(&recs));
    let mut rev = recs.clone();
    rev.reverse();
    let b = gate(&rev);
    // Row order follows first appearance; every reading is order-free.
    assert_eq!(a.status, b.status);
    for row in &a.strength.rows {
        let other = b.strength.rows.iter().find(|r| r.strategy == row.strategy).unwrap();
        assert_eq!(row, other);
    }
    assert_eq!(a.seat.slot_a, b.seat.slot_a);
    assert_eq!(a.seat.left_spawn, b.seat.left_spawn);
    assert_eq!(a.termination, b.termination);
}

// ---- real batches ------------------------------------------------------------

fn play(content: &Content, pairs: &[(&str, &str)], seeds: &[u64]) -> Vec<MatchRecord> {
    let mut out = Vec::new();
    for &seed in seeds {
        for &(x, y) in pairs {
            for (a, b) in [(x, y), (y, x)] {
                for o in Orientation::ALL {
                    let s = MatchSettings::default()
                        .with_seed(seed)
                        .with_strategies(a, b)
                        .with_orientation(o);
                    out.push(batch::run_match(content, &s).expect("names resolve"));
                }
            }
        }
    }
    out
}

/// The critic's probe: break one multiplier in a content fixture and the gate
/// must FAIL, naming the strategy. Ripper's offense is quadrupled; played
/// against its designed counter (`sentinel > ripper`) and one other probe, the
/// ripper mass probe wins regardless of counter.
#[test]
fn an_injected_imbalance_fails_the_gate_and_names_the_strategy() {
    let mut content = shipped();
    let ripper = content.units.iter_mut().find(|u| u.id == "ripper").expect("ripper ships");
    ripper.offense *= 4;
    let recs = play(
        &content,
        &[("mass_ripper", "mass_sentinel"), ("mass_ripper", "mass_bulwark")],
        &[0, 1],
    );
    let g = KillGate::of(&content, &recs, &GateSpec::default());
    let row = &g.strength.rows[0];
    assert_eq!(row.strategy, "mass_ripper");
    assert_eq!(row.unit.as_deref(), Some("ripper"));
    assert_eq!(row.strength.value, Some(1.0), "{:#?}", g.strength);
    assert_eq!(g.strength.failing, ["mass_ripper"]);
    assert_eq!(g.strength.dominant, ["mass_ripper"]);
    assert_eq!(g.strength.status, Status::Fail);
    assert_eq!(g.status, Status::Fail);

    // The same pairs on shipped content do not fail K1: the failure is the
    // injected multiplier, not the fixture's shape.
    let base = shipped();
    let recs = play(
        &base,
        &[("mass_ripper", "mass_sentinel"), ("mass_ripper", "mass_bulwark")],
        &[0, 1],
    );
    let g = KillGate::of(&base, &recs, &GateSpec::default());
    assert!(
        !g.strength.failing.contains(&"mass_ripper".to_string()),
        "{:#?}",
        g.strength.rows[0]
    );
}

/// What the gate says about the shipped content on the B3 pentagon batch (the
/// five mass probes, two seeds — `b3_pentagon`'s real batch). A measurement,
/// pinned so a behavioural change shows; F-039 records it and what it can and
/// cannot decide at this size.
#[test]
fn the_shipped_reading_on_the_pentagon_batch() {
    let content = shipped();
    let probes: Vec<String> = ["sentinel", "ripper", "arclight", "bulwark", "ravager"]
        .iter()
        .map(|u| format!("mass_{u}"))
        .collect();
    let settings = BatchSettings::default().with_only(probes).with_seeds(2);
    let recs = batch::run_batch(&content, &settings, &mut |_| {}).expect("shipped names");
    let g = KillGate::of(&content, &recs, &GateSpec::default());
    // K1: no row is resolved either way above 65%. The two strongest rows sit
    // right at the bar (65.6%, 68.8%) with ~±19 points of interval; the two
    // weakest are resolved *below* it. Undetermined, not PASS.
    let rows: Vec<(&str, Option<f64>, Status)> = g
        .strength
        .rows
        .iter()
        .map(|r| (r.strategy.as_str(), r.strength.value, r.strength.status))
        .collect();
    assert_eq!(
        rows,
        [
            ("mass_bulwark", Some(0.375), Status::Pass),
            ("mass_sentinel", Some(0.65625), Status::Undetermined),
            ("mass_ripper", Some(0.6875), Status::Undetermined),
            ("mass_ravager", Some(0.46875), Status::Undetermined),
            ("mass_arclight", Some(0.3125), Status::Pass),
        ]
    );
    assert!(g.strength.failing.is_empty());
    assert!(g.strength.dominant.is_empty());
    assert!(g.strength.losing.is_empty());
    assert_eq!(g.strength.status, Status::Undetermined);

    // K2: 20 decided mirrors, slot A 13 of 20, left base 9 of 20 — both
    // intervals ~±20 points wide. Undetermined: F-038 sizes the run that can
    // decide it.
    assert_eq!(g.seat.slot_a.n, 20);
    assert_eq!(g.seat.slot_a.value, Some(0.65));
    assert_eq!(g.seat.left_spawn.value, Some(0.45));
    assert_eq!(g.seat.status, Status::Undetermined);

    // K3.
    assert_eq!(g.termination.decided_median, Some(23_790), "6:36");
    assert_eq!(g.termination.timeout_rate.value, Some(0.0));
    assert_eq!(g.termination.timeout_rate.status, Status::Undetermined, "0 of 100 cannot certify <5%");
    assert_eq!(g.termination.band_share.value, Some(0.36));
    assert_eq!(g.termination.band_share.status, Status::Fail, "36% of decided in band, resolved below 50%");
    // Reported context: the median match is resolved inside the band (34% end
    // before 5:00 and 30% after 8:00), which does not rescue the band share.
    assert_eq!(g.termination.below.value, Some(0.34));
    assert_eq!(g.termination.below.status, Status::Pass);
    assert_eq!(g.termination.beyond.value, Some(0.3));
    assert_eq!(g.termination.beyond.status, Status::Pass);
    assert_eq!(g.termination.status, Status::Fail, "the band share is resolved below 50%");

    // The whole gate: K3 fails (B3.5's band-share ceiling, deferred to B4).
    assert_eq!(g.status, Status::Fail);
}
