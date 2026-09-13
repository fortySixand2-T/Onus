//! L2 integration tests for **B2 AC2** — the batch runner behind
//! `src/bin/balance.rs`.
//!
//! The bin is a CLI wrapper; the loop it wraps lives in [`onus::batch`] so it
//! can be judged here. What is encoded:
//!
//!   - **coverage** — every *ordered* pair of strategies, mirrors included, is
//!     played once per seed, and the set of pairs is read out of `Content` (RON
//!     order) rather than a list written down twice;
//!   - **the cap** — a match that does not decide by the tick cap is a
//!     `Timeout`: no winner, no panic, and *not* the same thing as a draw;
//!   - **no over-stepping** — a decided match stops on the tick it was decided,
//!     rather than running on to the cap against a frozen world;
//!   - **determinism** — the same batch run twice yields identical records in
//!     identical order (B2's critic probe);
//!   - **fallibility** — a selection naming a strategy the content does not
//!     have is refused, never defaulted (B1's rule).
//!
//! Every test here runs on a *subset* with a small cap: the real 100×K batch is
//! the bin's job, not the suite's.

use onus::batch::{self, BatchSettings, MatchResult};
use onus::headless::{self, MatchSettings};
use onus::sim::content::Content;
use onus::sim::spatial::Faction;
use onus::sim::MatchState;

fn content() -> Content {
    headless::content().expect("assets/data/*.ron parse into sim structs")
}

/// A small, fixed slice of the roster — enough for a cartesian product without
/// paying for 100 matches per test.
const SUBSET: [&str; 3] = ["rush", "turtle", "mass_ripper"];

fn subset_settings(cap: u32, seeds: u32) -> BatchSettings {
    BatchSettings::default()
        .with_only(SUBSET.iter().map(|s| s.to_string()).collect())
        .with_tick_cap(cap)
        .with_seeds(seeds)
}

fn run(settings: &BatchSettings) -> Vec<batch::MatchRecord> {
    batch::run_batch(&content(), settings, &mut |_| {}).expect("only shipped strategy names")
}

// ---- coverage ---------------------------------------------------------------

#[test]
fn every_ordered_pair_including_mirrors_is_played_once_per_seed() {
    let seeds = 2;
    let records = run(&subset_settings(3, seeds));
    let n = SUBSET.len();
    assert_eq!(records.len(), n * n * seeds as usize);

    for k in 0..seeds {
        let seed = batch::seed_at(BatchSettings::default().seed_base, k);
        let mut pairs: Vec<(&str, &str)> = records
            .iter()
            .filter(|r| r.seed == seed)
            .map(|r| (r.strategies[0].as_str(), r.strategies[1].as_str()))
            .collect();
        assert_eq!(pairs.len(), n * n, "seed {seed} plays the whole product");
        pairs.sort_unstable();
        pairs.dedup();
        assert_eq!(pairs.len(), n * n, "no matchup is played twice per seed");
        for a in SUBSET {
            for b in SUBSET {
                assert!(
                    pairs.contains(&(a, b)),
                    "{a} vs {b} was never played (mirrors count)"
                );
            }
        }
    }
    // Mirrors are in there explicitly.
    assert!(records
        .iter()
        .any(|r| r.strategies[0] == r.strategies[1] && r.strategies[0] == "rush"));
}

#[test]
fn the_unfiltered_batch_is_read_out_of_the_content_in_ron_order() {
    let c = content();
    // One tick each: this is about *which* matchups are generated, not how they
    // play out.
    let records = run(&BatchSettings::default().with_tick_cap(1).with_seeds(1));
    let names: Vec<&str> = c.strategies.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(
        records.len(),
        names.len() * names.len(),
        "every ordered pair of every strategy the content ships"
    );
    let played: Vec<(&str, &str)> = records
        .iter()
        .map(|r| (r.strategies[0].as_str(), r.strategies[1].as_str()))
        .collect();
    let expected: Vec<(&str, &str)> = names
        .iter()
        .flat_map(|a| names.iter().map(move |b| (*a, *b)))
        .collect();
    assert_eq!(played, expected, "RON order, row-major, no map order");
}

// ---- the cap ----------------------------------------------------------------

#[test]
fn a_capped_match_times_out_with_no_winner_and_no_panic() {
    let records = run(&subset_settings(10, 1));
    assert_eq!(records.len(), 9);
    for r in &records {
        assert_eq!(
            r.result,
            MatchResult::Timeout,
            "{} vs {} decided in ten ticks?",
            r.strategies[0],
            r.strategies[1]
        );
        assert_eq!(r.winner(), None);
        assert_eq!(r.ticks, 10, "a timed-out match played exactly the cap");
    }
}

#[test]
fn a_timeout_is_not_a_mutual_loss_draw() {
    // The two are distinct values, and only one of them means "decided".
    assert_ne!(MatchResult::Timeout, MatchResult::MutualLoss);
    assert!(MatchResult::MutualLoss.is_decided());
    assert!(!MatchResult::Timeout.is_decided());
    assert_eq!(MatchResult::MutualLoss.winner(), None);
    assert_eq!(MatchResult::Decided(Faction::A).winner(), Some(Faction::A));

    // A capped run reports timeouts, never draws.
    let records = run(&subset_settings(10, 1));
    assert!(records.iter().all(|r| r.result == MatchResult::Timeout));
    assert_eq!(
        records
            .iter()
            .filter(|r| r.result == MatchResult::MutualLoss)
            .count(),
        0,
        "a stalemate must not be counted as a fair draw"
    );
    let tally = batch::Tally::of(&records);
    assert_eq!(tally.timeouts, 9);
    assert_eq!(tally.mutual_losses, 0);
    assert_eq!(tally.decided, 0);
}

// ---- a decided match stops where it was decided ------------------------------

#[test]
fn a_decided_match_is_not_stepped_past_its_decision() {
    let c = content();
    let settings = MatchSettings::default()
        .with_seed(DECIDED_SEED)
        .with_strategies(DECIDED_MATCHUP.0, DECIDED_MATCHUP.1)
        .with_tick_cap(DECIDED_CAP);
    let record = batch::run_match(&c, &settings).expect("shipped strategy names");
    assert!(
        record.result.is_decided(),
        "the pinned matchup must actually decide, or this test proves nothing"
    );
    assert!(
        record.ticks < DECIDED_CAP,
        "decided at {} of a {DECIDED_CAP}-tick cap",
        record.ticks
    );

    // The reference: play the same match by hand and see which tick the sim
    // decided on. `ticks` counts the steps the runner took, so equality here is
    // the claim that it stopped on the deciding tick rather than running on to
    // the cap against a frozen world.
    let mut app = headless::ai_vs_ai(c, &settings).expect("shipped strategy names");
    let mut steps = 0u32;
    let outcome = loop {
        headless::step(&mut app);
        steps += 1;
        if let Some(o) = app.world().resource::<MatchState>().outcome() {
            break o;
        }
        assert!(steps < DECIDED_CAP, "reference match failed to decide");
    };
    assert_eq!(record.ticks, steps);
    assert_eq!(
        record.ticks,
        outcome.tick + 1,
        "`ticks` is the count of ticks played; `MatchOutcome::tick` is the \
         zero-based index of the last of them"
    );
    assert_eq!(record.winner(), outcome.winner);
}

// ---- determinism -------------------------------------------------------------

#[test]
fn the_same_batch_twice_yields_identical_records_in_identical_order() {
    let settings = subset_settings(5, 2);
    let a = run(&settings);
    let b = run(&settings);
    assert_eq!(a, b, "a re-run of the whole batch must be identical");
    assert_eq!(a.len(), 18);
}

#[test]
fn seeds_are_derived_from_the_base_and_are_distinct() {
    let settings = subset_settings(3, 4).with_seed_base(1234);
    let records = run(&settings);
    let mut seeds: Vec<u64> = records.iter().map(|r| r.seed).collect();
    seeds.sort_unstable();
    seeds.dedup();
    assert_eq!(seeds.len(), 4, "K seeds, all different");
    let derived: Vec<u64> = (0..4).map(|k| batch::seed_at(1234, k)).collect();
    let mut sorted = derived.clone();
    sorted.sort_unstable();
    assert_eq!(seeds, sorted);
    // Deriving is a pure function of the base, not of a clock.
    assert_eq!(
        derived,
        (0..4).map(|k| batch::seed_at(1234, k)).collect::<Vec<_>>()
    );
    assert_ne!(derived[0], batch::seed_at(5678, 0));
}

// ---- fallibility -------------------------------------------------------------

#[test]
fn an_unknown_selected_strategy_is_refused_not_defaulted() {
    let err = batch::run_batch(
        &content(),
        &BatchSettings::default().with_only(vec!["rush".into(), "trutle".into()]),
        &mut |_| {},
    )
    .expect_err("a typo must not silently play a different roster");
    assert_eq!(err.id, "trutle");
    assert!(err.to_string().contains("trutle"));
}

// ---- progress reporting ------------------------------------------------------

#[test]
fn every_finished_match_is_reported_to_the_progress_callback() {
    let mut seen = Vec::new();
    let records = batch::run_batch(&content(), &subset_settings(3, 1), &mut |r| {
        seen.push(r.clone())
    })
    .expect("shipped strategy names");
    assert_eq!(seen, records, "progress sees every match, in batch order");
}

/// A matchup, seed and cap pinned because it *decides* — the "stopped at the
/// decision" test is vacuous against a stalemate. Chosen from a release run of
/// the bin (see the report): the rush ends this one well inside the cap.
const DECIDED_MATCHUP: (&str, &str) = ("rush", "turtle");
const DECIDED_SEED: u64 = 0;
const DECIDED_CAP: u32 = 28_800;
