//! L2 integration tests for **B3 AC (match-length distribution vs the 5–8 min
//! target)** — [`onus::metrics::LengthDistribution`].
//!
//! Two bases, never mixed, and every number says which it is on:
//!
//!   - **decided** — matches the sim ended (a win or a mutual loss). The
//!     decided quantiles, and the band counts / band share, are over these
//!     only. A timeout never finished, so it has no length to put in a band.
//!   - **all** — every match, a timeout entered at the tick it was stopped
//!     (the cap). This is the basis `batch::Tally::length_quantile` has always
//!     used (F-031 pinned that it includes capped matches); it is reported
//!     beside the decided basis, never instead of it, because a censored
//!     median is flattered by short games and a censored tail is clipped at
//!     the cap.
//!
//! The band share is **in-band ÷ decided**, and the timeout rate **timeouts ÷
//! all**: two numbers, two denominators, reported separately. The band is
//! inclusive at both ends (5:00 and 8:00 exactly are in band).
//!
//! Quantiles are nearest-rank (`ceil(p·n/100)`), in **integer** percent, so a
//! p10 of ten matches is the first of them and not, via `0.1f32 · 10 =
//! 1.0000000149`, the second.

use onus::batch::{self, BatchSettings, MatchRecord, MatchResult, ProductionCounts, Tally};
use onus::headless::{self, Orientation, SIM_HZ};
use onus::metrics::{LengthBand, LengthDistribution, REPORTED_PERCENTILES};
use onus::sim::spatial::Faction;

const MIN: u32 = 60 * SIM_HZ;
const A_WINS: MatchResult = MatchResult::Decided(Faction::A);
const B_WINS: MatchResult = MatchResult::Decided(Faction::B);
const DRAW: MatchResult = MatchResult::MutualLoss;
const CAPPED: MatchResult = MatchResult::Timeout;

fn rec(result: MatchResult, ticks: u32) -> MatchRecord {
    MatchRecord {
        strategies: ["a".into(), "b".into()],
        seed: 0,
        result,
        orientation: Orientation::Normal,
        ticks,
        produced: ProductionCounts::default(),
    }
}

#[test]
fn the_default_band_is_five_to_eight_minutes_inclusive() {
    let band = LengthBand::default();
    assert_eq!(band, LengthBand::minutes(5, 8));
    assert_eq!((band.min_ticks, band.max_ticks), (5 * MIN, 8 * MIN));
    assert!(!band.contains(5 * MIN - 1));
    assert!(band.contains(5 * MIN));
    assert!(band.contains(8 * MIN));
    assert!(!band.contains(8 * MIN + 1));
}

/// A hand-built batch where every number is known: four decided matches
/// (one below, two in, one above the band), a mutual loss in band, and two
/// timeouts at a 15-minute cap.
#[test]
fn decided_and_all_bases_are_separate_and_hand_checkable() {
    let cap = 15 * MIN;
    let records = vec![
        rec(A_WINS, 2 * MIN),     // below
        rec(B_WINS, 5 * MIN),     // in (edge)
        rec(A_WINS, 8 * MIN),     // in (edge)
        rec(DRAW, 6 * MIN),       // in: a mutual loss is decided
        rec(B_WINS, 10 * MIN),    // above
        rec(CAPPED, cap),
        rec(CAPPED, cap),
    ];
    let d = LengthDistribution::of(&records, LengthBand::default());
    assert_eq!((d.total, d.decided, d.timeouts), (7, 5, 2));
    assert_eq!((d.below_band, d.in_band, d.above_band), (1, 3, 1));
    assert_eq!(d.band_share(), Some(3.0 / 5.0), "in-band over DECIDED, not over all");
    assert_eq!(d.timeout_rate(), Some(2.0 / 7.0), "timeouts over ALL");

    // Decided basis: 2, 5, 6, 8, 10 minutes.
    assert_eq!(d.decided_percentile(0), Some(2 * MIN));
    assert_eq!(d.decided_percentile(50), Some(6 * MIN));
    assert_eq!(d.decided_percentile(90), Some(10 * MIN));
    assert_eq!(d.decided_percentile(100), Some(10 * MIN));
    // All basis: 2, 5, 6, 8, 10, 15, 15 — the timeouts sit at the cap.
    assert_eq!(d.all_percentile(50), Some(8 * MIN));
    assert_eq!(d.all_percentile(90), Some(cap));
    assert_eq!(d.all_percentile(100), Some(cap));

    // The all basis is exactly what `Tally` has always printed.
    let t = Tally::of(&records);
    for p in [0u32, 25, 50, 75, 100] {
        assert_eq!(
            d.all_percentile(p),
            t.length_quantile(p as f32 / 100.0),
            "p{p}: the all basis must agree with Tally's"
        );
    }
    assert_ne!(
        d.decided_percentile(50),
        t.median_length(),
        "the two medians differ on a batch with timeouts (F-031) — the fixture must show it"
    );
}

/// Nearest rank in integer arithmetic: p10 of ten lengths is the first.
#[test]
fn percentiles_are_exact_nearest_rank() {
    let records: Vec<MatchRecord> = (1..=10).map(|k| rec(A_WINS, k * 100)).collect();
    let d = LengthDistribution::of(&records, LengthBand::default());
    assert_eq!(d.decided_percentile(10), Some(100));
    assert_eq!(d.decided_percentile(11), Some(200));
    assert_eq!(d.decided_percentile(90), Some(900));
    assert_eq!(d.decided_percentile(91), Some(1000));
    assert_eq!(d.decided_percentile(0), Some(100), "p0 is the minimum");
    // Record order is irrelevant.
    let mut rev = records.clone();
    rev.reverse();
    assert_eq!(LengthDistribution::of(&rev, LengthBand::default()), d);
}

#[test]
fn an_empty_batch_has_no_length_and_no_rates() {
    let d = LengthDistribution::of(&[], LengthBand::default());
    assert_eq!((d.total, d.decided, d.timeouts), (0, 0, 0));
    assert_eq!(d.band_share(), None);
    assert_eq!(d.timeout_rate(), None, "no matches is not a 0% timeout rate");
    assert_eq!(d.decided_percentile(50), None);
    assert_eq!(d.all_percentile(50), None);
}

/// An all-timeout batch has a timeout rate of 100% and **no** decided length:
/// no median, no band share — never a band share of 0 or a median at the cap.
#[test]
fn an_all_timeout_batch_has_no_decided_length() {
    let records = vec![rec(CAPPED, 900), rec(CAPPED, 900)];
    let d = LengthDistribution::of(&records, LengthBand::default());
    assert_eq!(d.timeout_rate(), Some(1.0));
    assert_eq!(d.band_share(), None);
    assert_eq!(d.decided_percentile(50), None);
    assert_eq!(d.all_percentile(50), Some(900), "the all basis still sees the cap");
    assert!(d.is_all_timeout());
    assert!(!LengthDistribution::of(&[rec(A_WINS, 1)], LengthBand::default()).is_all_timeout());
    assert!(!LengthDistribution::of(&[], LengthBand::default()).is_all_timeout());
}

/// The summary carries both bases, labelled, at the reported percentiles.
#[test]
fn the_summary_reports_both_bases_at_every_reported_percentile() {
    let records = vec![rec(A_WINS, 6 * MIN), rec(CAPPED, 15 * MIN)];
    let d = LengthDistribution::of(&records, LengthBand::default());
    let s = d.summary();
    assert_eq!(REPORTED_PERCENTILES, [0, 10, 25, 50, 75, 90, 100]);
    let pcts = |v: &[(u32, Option<u32>)]| v.iter().map(|(p, _)| *p).collect::<Vec<_>>();
    assert_eq!(pcts(&s.decided_quantiles), REPORTED_PERCENTILES.to_vec());
    assert_eq!(pcts(&s.all_quantiles), REPORTED_PERCENTILES.to_vec());
    for (p, v) in &s.decided_quantiles {
        assert_eq!(*v, d.decided_percentile(*p));
    }
    for (p, v) in &s.all_quantiles {
        assert_eq!(*v, d.all_percentile(*p));
    }
    assert_eq!(s.band_share, Some(1.0));
    assert_eq!(s.timeout_rate, Some(0.5));
    assert_eq!((s.total, s.decided, s.timeouts), (2, 1, 1));
    assert_eq!((s.below_band, s.in_band, s.above_band), (0, 1, 0));
}

/// On a real batch the counts reconcile with the records, recounted here by
/// hand, and the decided median is a length some decided match actually had.
#[test]
fn a_real_batch_reconciles_with_its_records() {
    let content = headless::content().expect("content");
    let settings = BatchSettings::default()
        .with_only(vec!["rush".into(), "mass_ripper".into()])
        .with_seeds(1);
    let records = batch::run_batch(&content, &settings, &mut |_| {}).expect("names");
    let band = LengthBand::default();
    let d = LengthDistribution::of(&records, band);
    let decided: Vec<u32> = records
        .iter()
        .filter(|r| r.result.is_decided())
        .map(|r| r.ticks)
        .collect();
    assert_eq!(d.total as usize, records.len());
    assert_eq!(d.decided as usize, decided.len());
    assert_eq!(d.decided + d.timeouts, d.total);
    assert_eq!(d.below_band + d.in_band + d.above_band, d.decided);
    let count = |f: &dyn Fn(u32) -> bool| decided.iter().filter(|&&t| f(t)).count() as u32;
    assert_eq!(d.below_band, count(&|t| t < band.min_ticks));
    assert_eq!(d.in_band, count(&|t| band.contains(t)));
    assert_eq!(d.above_band, count(&|t| t > band.max_ticks));
    match d.decided_percentile(50) {
        Some(m) => assert!(decided.contains(&m)),
        None => assert!(decided.is_empty()),
    }
}
