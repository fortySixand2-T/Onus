//! Critic probes for the B3.5 armour/tempo checkbox (F-031).
//!
//! The deliverable under review is a *measurement document*, so these tests pin
//! the structural facts every number in it rests on, plus the content state the
//! document claims ("the revert is the absence of a diff"):
//!
//!   - `mvp_combat.mitigation_per_armor` is **2** in the shipped content, and
//!     the five mass probes are knob-identical at `attack_at_army: 10` (F-018) —
//!     if either moves, F-031's tables describe something that is not shipped;
//!   - a pentagon link's sample size **is** a seed count: four decided matches
//!     per seed (two slot orderings x two spawn orientations, pooled by
//!     `WinMatrix` into one cell). Every CI in F-031's tables assumes this;
//!   - the `mvp` strategy is untouched by the tempo pass, so B1's
//!     number-for-number pin still means what it meant;
//!   - `Tally::length_quantile` is computed over **every** match, timeouts
//!     included — i.e. the quantiles `balance` prints are not a decided-only
//!     distribution. F-031 states its quantiles are "decided matches only ...
//!     so every number here is comparable with F-029's and F-030's"; on a batch
//!     with timeouts the two bases differ, and this test names the difference;
//!   - a link with censored matches is only readable if its verdict survives the
//!     worst case for the censored cells (`arclight > bulwark` at 100.0% of 88
//!     decided with 12 timeouts: 88/100 even if every capped match is charged to
//!     the predator as a loss).

use onus::batch::{MatchRecord, MatchResult, ProductionCounts, Tally};
use onus::headless::Orientation;
use onus::metrics::WinMatrix;
use onus::sim::content::Content;
use onus::sim::spatial::Faction;

fn shipped() -> Content {
    onus::headless::content().expect("the shipped assets/data load")
}

/// The five mass probes, in cycle order.
const PROBES: [&str; 5] = [
    "mass_sentinel",
    "mass_ripper",
    "mass_arclight",
    "mass_bulwark",
    "mass_ravager",
];

// ---- the content F-031's tables describe ------------------------------------

/// The headline decision: mitigation 1 is REVERTED, so the shipped scaling is
/// still 2. The revert is the absence of a diff, and this asserts that absence.
#[test]
fn the_shipped_combat_scaling_keeps_mitigation_per_armor_at_two() {
    let c = shipped();
    assert_eq!(
        c.combat.mitigation_per_armor, 2,
        "F-031 reverts mitigation 1: the shipped value is 2"
    );
}

/// Every row of F-031 is a reading of the *shipped* commitment threshold, and
/// the five probes must stay knob-identical or the pentagon measures the knobs
/// instead of the units (F-018). Only the barracks building and the massed unit
/// may differ.
#[test]
fn the_five_mass_probes_are_knob_identical_at_attack_at_army_ten() {
    let c = shipped();
    for id in PROBES {
        let s = c.strategy(id).unwrap_or_else(|| panic!("{id} is shipped"));
        assert_eq!(s.attack_at_army, 10, "{id}: the shipped threshold is 10");
    }
    let first = c.strategy(PROBES[0]).expect("shipped");
    for id in &PROBES[1..] {
        let s = c.strategy(id).expect("shipped");
        assert_eq!(s.think_interval_ticks, first.think_interval_ticks, "{id}");
        assert_eq!(s.worker_target, first.worker_target, "{id}");
        assert_eq!(s.attack_at_army, first.attack_at_army, "{id}");
        assert_eq!(s.attack_interval_ticks, first.attack_interval_ticks, "{id}");
        assert_eq!(s.attack_spread, first.attack_spread, "{id}");
        assert_eq!(s.queue_depth, first.queue_depth, "{id}");
        assert_eq!(s.barracks.len(), first.barracks.len(), "{id}: line count");
        for (b, b0) in s.barracks.iter().zip(&first.barracks) {
            assert_eq!(b.at_tick, b0.at_tick, "{id}: opening tick");
            assert_eq!(b.offset, b0.offset, "{id}: opening offset");
        }
        assert_eq!(s.army.len(), 1, "{id} masses exactly one unit");
    }
}

/// B1's `the_default_strategy_is_the_old_mvp_ai_number_for_number` must still
/// mean what it meant: the tempo pass left `mvp` alone.
#[test]
fn the_mvp_strategy_is_untouched_by_the_tempo_pass() {
    let c = shipped();
    let s = c.strategy("mvp").expect("the default is shipped");
    assert_eq!(s.think_interval_ticks, 30);
    assert_eq!(s.worker_target, 6);
    assert_eq!(s.attack_at_army, 3);
    assert_eq!(s.attack_interval_ticks, 600);
    assert_eq!(s.attack_spread, 60.0);
    assert_eq!(s.barracks.len(), 1, "the MVP opener opens one barracks");
    assert_eq!(s.barracks[0].building, "foundry");
    assert_eq!(s.barracks[0].at_tick, 300);
    assert_eq!(s.barracks[0].offset, 130.0);
    let army: Vec<(&str, u32)> = s.army.iter().map(|i| (i.unit.as_str(), i.count)).collect();
    assert_eq!(army, vec![("sentinel", 2), ("bulwark", 1)]);
    assert_eq!(c.default_strategy, "mvp");
}

// ---- the structural claim under every CI in F-031 ----------------------------

fn record(a: &str, b: &str, seed: u64, orientation: Orientation, result: MatchResult) -> MatchRecord {
    MatchRecord {
        strategies: [a.to_string(), b.to_string()],
        seed,
        result,
        orientation,
        ticks: 1000,
        produced: ProductionCounts::zeroed(&shipped()),
    }
}

/// "A pentagon cell accumulates 4 decided matches per seed (2 orientations x the
/// two orderings `WinMatrix` pools), so the sample size is a seed count."
///
/// Built exactly the way `batch::run_batch` emits rows — every ordered pair,
/// every seed, both orientations — and read off the matrix cell.
#[test]
fn a_pentagon_link_pools_four_matches_per_seed() {
    for seeds in 1u32..=5 {
        let mut records = Vec::new();
        for k in 0..seeds as u64 {
            for (a, b) in [("mass_sentinel", "mass_ripper"), ("mass_ripper", "mass_sentinel")] {
                for orientation in Orientation::ALL {
                    records.push(record(a, b, k, orientation, MatchResult::Decided(Faction::A)));
                }
            }
        }
        let m = WinMatrix::of(&records);
        let cell = m
            .get("mass_sentinel", "mass_ripper")
            .expect("the off-diagonal cell exists");
        assert_eq!(
            cell.played(),
            4 * seeds,
            "{seeds} seeds must give a link 4 x {seeds} matches"
        );
        assert_eq!(cell.n_decided, 4 * seeds);
    }
    // The sizes F-031 quotes, spelled out: 2 seeds -> 8, 8 -> 32, 25 -> 100.
    for (seeds, n) in [(2u32, 8u32), (8, 32), (25, 100)] {
        assert_eq!(4 * seeds, n, "{seeds} seeds is n={n} per link");
    }
}

/// A mirror is one cell credited once per match, so a mirror's sample is *half*
/// an off-diagonal link's at the same seed count. F-031 reads mirror lengths
/// (Ripper 2:29, Sentinel 3:38); this pins the pooling that differs there.
#[test]
fn a_mirror_cell_pools_two_matches_per_seed_not_four() {
    let mut records = Vec::new();
    for k in 0..4u64 {
        for orientation in Orientation::ALL {
            records.push(record(
                "mass_ripper",
                "mass_ripper",
                k,
                orientation,
                MatchResult::Decided(Faction::A),
            ));
        }
    }
    let m = WinMatrix::of(&records);
    let cell = m.get("mass_ripper", "mass_ripper").expect("the diagonal");
    assert_eq!(cell.played(), 8, "4 seeds x 2 orientations, counted once");
}

// ---- what the printed length distribution is actually over -------------------

/// F-031 says its quantiles are `Tally::length_quantile`'s definition, "decided
/// matches only", and that this makes them comparable with the printed numbers
/// of F-029/F-030. `Tally` pushes **every** record's ticks, timeouts included,
/// so on a batch with timeouts the printed distribution is not decided-only:
/// here the printed max is the cap while the decided max is 1 000 ticks.
#[test]
fn the_printed_length_quantiles_include_capped_matches() {
    let cap = 54_000;
    let mut records = Vec::new();
    for k in 0..9u64 {
        let mut r = record(
            "mass_bulwark",
            "mass_ravager",
            k,
            Orientation::Normal,
            MatchResult::Decided(Faction::A),
        );
        r.ticks = 1_000;
        records.push(r);
    }
    let mut capped = record(
        "mass_bulwark",
        "mass_ravager",
        9,
        Orientation::Normal,
        MatchResult::Timeout,
    );
    capped.ticks = cap;
    records.push(capped);

    let t = Tally::of(&records);
    assert_eq!(t.decided, 9);
    assert_eq!(t.timeouts, 1);
    assert_eq!(
        t.length_quantile(1.0),
        Some(cap),
        "the printed distribution counts the capped match at the cap"
    );
    assert_eq!(
        t.length_quantile(0.9),
        Some(1_000),
        "p90 of ten matches is the ninth, still a decided one"
    );
}

// ---- censoring: is a 100% cell with timeouts readable? ----------------------

/// `arclight > bulwark` reads 100.0% over 88 decided with 12 capped. A cell that
/// loses 12% of its matches to the cap is only legitimately "holds" if the
/// verdict survives charging every capped match to the predator as a loss. It
/// does: 88/100 = 88%, whose 95% Wilson interval still excludes 50%.
#[test]
fn the_censored_arclight_bulwark_cell_holds_even_if_every_timeout_is_a_loss() {
    let worst = 88.0 / 100.0;
    assert!(worst > 0.5, "worst-case rate {worst} must still be a hold");
    let (lo, hi) = wilson(88, 100);
    assert!(
        lo > 0.5,
        "worst-case Wilson interval [{lo:.3}, {hi:.3}] must exclude 50%"
    );
    // And the reverse check: the same censoring at F-030's sample size decides
    // nothing — 4 of 8 has a 95% interval of [21.5, 78.5].
    let (lo8, hi8) = wilson(4, 8);
    assert!(
        lo8 < 0.5 && hi8 > 0.5,
        "8 matches cannot call a link either way: [{lo8:.3}, {hi8:.3}]"
    );
}

/// Wilson score interval, 95%.
fn wilson(k: u32, n: u32) -> (f64, f64) {
    let z = 1.959_963_985_f64;
    let (k, n) = (f64::from(k), f64::from(n));
    let p = k / n;
    let d = 1.0 + z * z / n;
    let centre = (p + z * z / (2.0 * n)) / d;
    let half = z * (p * (1.0 - p) / n + z * z / (4.0 * n * n)).sqrt() / d;
    ((centre - half).max(0.0), (centre + half).min(1.0))
}
