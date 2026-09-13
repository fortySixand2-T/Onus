//! Adversarial probes for **B2 AC2** — `src/bin/balance.rs` plays every ordered
//! matchup across K seeds, each match to termination or a tick cap.
//!
//! These are independent of the implementer's `tests/b2_batch.rs`: every claim
//! is re-derived here from the sim's own counters (`MatchState::tick`,
//! `MatchOutcome::tick`, per-tick `state_hash`) rather than from the runner's
//! own bookkeeping, and every boundary is probed from both sides.

use std::process::Command;

use onus::batch::{self, BatchSettings, MatchResult, MatchRecord};
use onus::headless::{self, MatchSettings, DEFAULT_TICK_CAP, SIM_HZ};
use onus::sim::content::Content;
use onus::sim::replay::StateHashLog;
use onus::sim::MatchState;

fn content() -> Content {
    headless::content().expect("shipped content loads")
}

/// A matchup that decides quickly, so a boundary can be probed either side of
/// the exact deciding tick without paying for a full eight minutes of sim.
const DECIDER: (&str, &str) = ("rush", "rush");
const DECIDER_SEED: u64 = 0;

fn decider(cap: u32) -> MatchSettings {
    MatchSettings::default()
        .with_seed(DECIDER_SEED)
        .with_strategies(DECIDER.0, DECIDER.1)
        .with_tick_cap(cap)
}

/// The tick count a decided match *actually* takes, measured by hand.
fn measure_decider() -> (u32, u32) {
    let mut app = headless::ai_vs_ai(content(), &decider(DEFAULT_TICK_CAP)).expect("known names");
    let mut steps = 0u32;
    loop {
        headless::step(&mut app);
        steps += 1;
        if let Some(o) = app.world().resource::<MatchState>().outcome() {
            return (steps, o.tick);
        }
        assert!(steps < DEFAULT_TICK_CAP, "the pinned matchup must decide");
    }
}

// ---- the off-by-one ---------------------------------------------------------

/// `ticks` is the number of ticks played. Cross-checked against two *sim-owned*
/// counters, not against the runner: the match's own tick counter (which stops
/// when the match is over) and the zero-based tick the outcome was stamped
/// with. A silent +/-1 here biases every length statistic B3 prints.
#[test]
fn probe_recorded_ticks_agree_with_the_sims_own_counters() {
    let (steps, outcome_tick) = measure_decider();
    assert_eq!(steps, outcome_tick + 1, "sim self-consistency precondition");

    let c = content();
    let record = batch::run_match(&c, &decider(DEFAULT_TICK_CAP)).expect("known names");
    assert!(record.result.is_decided());
    assert_eq!(record.ticks, steps, "recorded length == ticks actually played");
    assert_eq!(record.ticks, outcome_tick + 1);
    assert_eq!(record.seconds(), steps as f32 / SIM_HZ as f32);

    // And the same identity for a timeout: a capped match played exactly `cap`
    // ticks, and the sim agrees it played that many.
    let cap = 97;
    let timed_out = batch::run_match(&c, &decider(cap)).expect("known names");
    assert_eq!(timed_out.result, MatchResult::Timeout);
    assert_eq!(timed_out.ticks, cap);
    let mut app = headless::ai_vs_ai(content(), &decider(cap)).expect("known names");
    headless::tick(&mut app, cap);
    assert_eq!(app.world().resource::<MatchState>().tick(), cap);
    assert_eq!(app.world().resource::<MatchState>().outcome(), None);
}

// ---- the cap boundary -------------------------------------------------------

/// With `tick_cap = T` (T = the tick the match decides on), the match must be
/// recorded as **decided**, not as a timeout; with `T - 1` it must be a
/// timeout; with `T + 1` it must be decided at T. An off-by-one at the cap
/// turns real results into stalemates (or the reverse).
#[test]
fn probe_cap_boundary_is_inclusive_of_the_deciding_tick() {
    let (t, _) = measure_decider();
    let c = content();

    let at = batch::run_match(&c, &decider(t)).expect("known names");
    assert!(
        at.result.is_decided(),
        "a match deciding on the last permitted tick is decided, not a timeout"
    );
    assert_eq!(at.ticks, t);

    let short = batch::run_match(&c, &decider(t - 1)).expect("known names");
    assert_eq!(
        short.result,
        MatchResult::Timeout,
        "one tick short of the decision is an unfinished match"
    );
    assert_eq!(short.ticks, t - 1);

    let long = batch::run_match(&c, &decider(t + 1)).expect("known names");
    assert_eq!(long.result, at.result);
    assert_eq!(long.ticks, t, "a decided match is not stepped past its decision");
}

/// Degenerate caps are total: no panic, no winner, `ticks == cap`.
#[test]
fn probe_degenerate_caps_are_total() {
    let c = content();
    for cap in [0u32, 1, 2] {
        let r = batch::run_match(&c, &decider(cap)).expect("known names");
        assert_eq!(r.result, MatchResult::Timeout, "cap {cap}");
        assert_eq!(r.ticks, cap, "cap {cap}");
        assert_eq!(r.winner(), None);
        assert!(!r.result.is_decided());
    }
    // And through the batch loop, not just the single-match entry point.
    let rows = batch::run_batch(
        &content(),
        &BatchSettings::default()
            .with_only(vec!["rush".into()])
            .with_tick_cap(0),
        &mut |_| {},
    )
    .expect("known names");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].result, MatchResult::Timeout);
    assert_eq!(rows[0].ticks, 0);
}

/// A timeout and a mutual loss are both winner-less and both *different*: B3
/// has to be able to say "% hit the cap" without conflating it with draws.
#[test]
fn probe_timeout_is_never_conflated_with_a_mutual_loss() {
    assert_ne!(MatchResult::Timeout, MatchResult::MutualLoss);
    assert_eq!(MatchResult::Timeout.winner(), MatchResult::MutualLoss.winner());
    assert!(MatchResult::MutualLoss.is_decided());
    assert!(!MatchResult::Timeout.is_decided());

    let rows = batch::run_batch(
        &content(),
        &BatchSettings::default()
            .with_only(vec!["rush".into(), "turtle".into()])
            .with_tick_cap(4),
        &mut |_| {},
    )
    .expect("known names");
    let t = batch::Tally::of(&rows);
    assert_eq!(t.total, 4);
    assert_eq!(t.timeouts, 4);
    assert_eq!(t.mutual_losses, 0, "a stalemate is not a draw");
    assert_eq!(t.decided, 0);
    assert_eq!(t.timeout_rate(), 1.0);
}

// ---- the cap is harness config, not sim input -------------------------------

/// `ai_vs_ai` must ignore `tick_cap` entirely: the cap is how long an observer
/// watches, not an input to the game. Compared by per-tick `state_hash`, so a
/// single differing entity would show.
#[test]
fn probe_tick_cap_does_not_reach_the_sim() {
    let hashes = |cap: u32| {
        let s = decider(cap).with_hashing(true);
        let mut app = headless::ai_vs_ai(content(), &s).expect("known names");
        headless::tick(&mut app, 240);
        app.world().resource::<StateHashLog>().0.clone()
    };
    let a = hashes(0);
    let b = hashes(DEFAULT_TICK_CAP);
    assert_eq!(a.len(), 240);
    assert_eq!(a, b, "the cap changed the game");
}

/// The pre-B2 default fixture must not have moved: AC1 pinned it.
#[test]
fn probe_default_match_settings_still_the_pinned_fixture() {
    let d = MatchSettings::default();
    assert_eq!(d.seed, 0);
    assert_eq!(d.strategies, [None, None]);
    assert!(!d.hashing);
    assert_eq!(d.tick_cap, DEFAULT_TICK_CAP);
    assert_eq!(DEFAULT_TICK_CAP, 8 * 60 * SIM_HZ);
    assert_eq!(DEFAULT_TICK_CAP, 28_800);
    // The default fixture plays what it played before the cap field existed.
    let mut app = headless::ai_vs_ai(content(), &MatchSettings::default().with_hashing(true))
        .expect("the default fixture needs no names");
    headless::tick(&mut app, 120);
    let pinned = app.world().resource::<StateHashLog>().0.clone();
    let mut again = headless::ai_vs_ai(
        content(),
        &MatchSettings::default().with_hashing(true).with_tick_cap(7),
    )
    .expect("known names");
    headless::tick(&mut again, 120);
    assert_eq!(pinned, again.world().resource::<StateHashLog>().0);
}

// ---- coverage and ordering --------------------------------------------------

/// N x N x K rows for the full roster read out of `Content`, every (a, b, seed)
/// exactly once, mirrors present, in RON order seed-major then row-major.
#[test]
fn probe_full_roster_coverage_is_complete_and_ordered() {
    let c = content();
    let n = c.strategies.len();
    assert!(n >= 2, "a one-strategy roster would make this vacuous");
    let k = 2u32;
    let rows = batch::run_batch(
        &c,
        &BatchSettings::default().with_tick_cap(2).with_seeds(k),
        &mut |_| {},
    )
    .expect("content's own names");
    assert_eq!(rows.len(), n * n * k as usize);

    let mut keys: Vec<(String, String, u64)> = rows
        .iter()
        .map(|r| (r.strategies[0].clone(), r.strategies[1].clone(), r.seed))
        .collect();
    let expected_len = keys.len();
    keys.sort();
    keys.dedup();
    assert_eq!(keys.len(), expected_len, "some (a, b, seed) played twice");

    let ids: Vec<&str> = c.strategies.iter().map(|s| s.id.as_str()).collect();
    let mut i = 0;
    for kk in 0..k {
        let seed = batch::seed_at(0, kk);
        for a in &ids {
            for b in &ids {
                let r = &rows[i];
                assert_eq!((r.strategies[0].as_str(), r.strategies[1].as_str()), (*a, *b), "row {i}");
                assert_eq!(r.seed, seed, "row {i} seed-major order");
                i += 1;
            }
        }
    }
    assert_eq!(
        rows.iter().filter(|r| r.strategies[0] == r.strategies[1]).count(),
        n * k as usize,
        "one mirror per strategy per seed"
    );
}

/// `only` filters, it never reorders — and the rows it keeps are exactly the
/// corresponding rows of an unfiltered batch.
#[test]
fn probe_only_filters_without_reordering() {
    let c = content();
    let full = batch::run_batch(
        &c,
        &BatchSettings::default().with_tick_cap(2).with_seeds(1),
        &mut |_| {},
    )
    .expect("names");
    let pick = ["turtle", "rush"];
    let forward = batch::run_batch(
        &c,
        &BatchSettings::default()
            .with_tick_cap(2)
            .with_only(pick.iter().map(|s| s.to_string()).collect()),
        &mut |_| {},
    )
    .expect("names");
    let mut reversed_pick: Vec<String> = pick.iter().rev().map(|s| s.to_string()).collect();
    reversed_pick.push("rush".to_string()); // a duplicate must not duplicate rows
    let backward = batch::run_batch(
        &c,
        &BatchSettings::default().with_tick_cap(2).with_only(reversed_pick),
        &mut |_| {},
    )
    .expect("names");
    assert_eq!(forward, backward, "the flag's spelling changed the report");

    let expect: Vec<&MatchRecord> = full
        .iter()
        .filter(|r| pick.contains(&r.strategies[0].as_str()) && pick.contains(&r.strategies[1].as_str()))
        .collect();
    assert_eq!(expect.len(), 4);
    let got: Vec<&MatchRecord> = forward.iter().collect();
    assert_eq!(got, expect, "filtering must not reorder or alter rows");
}

/// Filtering must not change *results* either, on a cap long enough to decide.
#[test]
fn probe_filtered_rows_are_the_same_matches_that_a_wider_batch_plays() {
    let c = content();
    let wide = batch::run_batch(
        &c,
        &BatchSettings::default()
            .with_only(vec!["rush".into(), "turtle".into(), "mass_ripper".into()]),
        &mut |_| {},
    )
    .expect("names");
    let narrow = batch::run_batch(
        &c,
        &BatchSettings::default().with_only(vec!["mass_ripper".into(), "rush".into()]),
        &mut |_| {},
    )
    .expect("names");
    let expect: Vec<&MatchRecord> = wide
        .iter()
        .filter(|r| r.strategies.iter().all(|s| s == "rush" || s == "mass_ripper"))
        .collect();
    assert_eq!(narrow.iter().collect::<Vec<_>>(), expect);
}

// ---- determinism ------------------------------------------------------------

#[test]
fn probe_a_rerun_of_the_batch_is_identical() {
    let s = BatchSettings::default()
        .with_only(vec!["rush".into(), "turtle".into()])
        .with_tick_cap(600)
        .with_seeds(2)
        .with_seed_base(99);
    let a = batch::run_batch(&content(), &s, &mut |_| {}).expect("names");
    let b = batch::run_batch(&content(), &s, &mut |_| {}).expect("names");
    assert_eq!(a, b);
    assert_eq!(a.len(), 8);
}

/// Determinism at the tick level, not just at the record level: two runs of the
/// same match agree on every per-tick `state_hash`.
#[test]
fn probe_the_same_match_hashes_identically_tick_for_tick() {
    let s = decider(400).with_hashing(true);
    let run = || {
        let mut app = headless::ai_vs_ai(content(), &s).expect("names");
        headless::tick(&mut app, 400);
        app.world().resource::<StateHashLog>().0.clone()
    };
    let a = run();
    let b = run();
    assert_eq!(a.first_divergence_index(&b), None);
}

trait Div {
    fn first_divergence_index(&self, other: &Self) -> Option<usize>;
}
impl Div for Vec<u64> {
    fn first_divergence_index(&self, other: &Self) -> Option<usize> {
        StateHashLog(self.clone()).first_divergence(&StateHashLog(other.clone()))
    }
}

/// `seed_at` is a pure function with no hidden state: the same `(base, k)`
/// always gives the same seed, call order does not matter, distinct `k` never
/// collide, and distinct bases never collide.
#[test]
fn probe_seed_at_is_pure_and_collision_free() {
    let out_of_order = batch::seed_at(7, 500);
    let all: Vec<u64> = (0..2000).map(|k| batch::seed_at(7, k)).collect();
    assert_eq!(all[500], out_of_order, "call order changed a seed");
    assert_eq!(batch::seed_at(7, 500), out_of_order);
    let mut sorted = all.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), all.len(), "two k collided on one seed");
    for k in 0..64 {
        assert_ne!(batch::seed_at(0, k), batch::seed_at(1, k));
        assert_ne!(batch::seed_at(u64::MAX, k), batch::seed_at(0, k));
    }
}

/// The progress callback sees every finished match exactly once, in batch
/// order, and cannot steer the batch.
#[test]
fn probe_progress_observes_every_match_exactly_once_and_changes_nothing() {
    let s = BatchSettings::default()
        .with_only(vec!["rush".into(), "turtle".into()])
        .with_tick_cap(30)
        .with_seeds(2);
    let mut seen: Vec<MatchRecord> = Vec::new();
    let with_cb = batch::run_batch(&content(), &s, &mut |r| {
        seen.push(r.clone());
        // A callback that mutates its own world must not reach the batch.
        let _ = r.winner();
    })
    .expect("names");
    let silent = batch::run_batch(&content(), &s, &mut |_| {}).expect("names");
    assert_eq!(seen, with_cb, "progress order == record order, once each");
    assert_eq!(with_cb, silent, "the callback changed the results");
    assert_eq!(seen.len(), 8);
}

/// B1's rule survives the batch: an unknown name is refused at every entry
/// point, never defaulted.
#[test]
fn probe_unknown_names_are_refused_at_every_entry_point() {
    let c = content();
    assert!(batch::roster(&c, Some(&["nope".to_string()])).is_err());
    assert!(batch::run_match(
        &c,
        &MatchSettings::default().with_strategies("rush", "nope").with_tick_cap(2)
    )
    .is_err());
    assert!(batch::run_batch(
        &c,
        &BatchSettings::default().with_only(vec!["nope".into()]),
        &mut |_| panic!("no match may be played before the refusal")
    )
    .is_err());
}

// ---- the CLI ----------------------------------------------------------------

fn balance(args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_balance"))
        .args(args)
        .output()
        .expect("the balance bin runs");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Bad input is refused with a non-zero exit and *nothing on stdout*: stdout is
/// the report channel and must not carry diagnostics.
#[test]
fn probe_cli_refuses_bad_input_without_polluting_stdout() {
    for args in [
        vec!["--nope"],
        vec!["--only", "trutle"],
        vec!["--seeds"],
        vec!["--seeds", "abc"],
        vec!["--seeds", "0"],
        vec!["--tick-cap", "0"],
        vec!["--tick-cap", "-1"],
        vec!["rush"],
    ] {
        let (ok, stdout, stderr) = balance(&args);
        assert!(!ok, "{args:?} was accepted");
        assert!(stdout.is_empty(), "{args:?} wrote to stdout: {stdout}");
        assert!(!stderr.is_empty(), "{args:?} said nothing on stderr");
    }
}

/// A real (tiny) run: summary on stdout, progress on stderr, and the two do not
/// mix.
#[test]
fn probe_cli_run_separates_report_from_progress() {
    let (ok, stdout, stderr) = balance(&["--only", "rush", "--seeds", "1", "--tick-cap", "5"]);
    assert!(ok, "stderr: {stderr}");
    assert!(stdout.contains("matches"), "{stdout}");
    assert!(stdout.contains("timeouts"), "{stdout}");
    assert!(!stdout.contains("[1/1]"), "progress leaked into stdout: {stdout}");
    assert!(stderr.contains("[1/1]"), "no progress on stderr: {stderr}");
    // The same run twice is the same report, out of process.
    let (_, stdout2, _) = balance(&["--only", "rush", "--seeds", "1", "--tick-cap", "5"]);
    assert_eq!(stdout, stdout2, "the report is not reproducible");
}

/// Whatever the precedence between `--tick-cap` and `--minutes`, the cap the
/// run announces must be the cap it actually enforces.
#[test]
fn probe_cli_cap_flags_are_self_consistent() {
    for args in [
        vec!["--only", "rush", "--tick-cap", "5", "--minutes", "0"],
        vec!["--only", "rush", "--minutes", "0", "--tick-cap", "5"],
    ] {
        let (ok, stdout, stderr) = balance(&args);
        if !ok {
            // Refusing a zero-minute budget is fine; silently playing a
            // zero-tick match is not.
            assert!(stdout.is_empty(), "{args:?}");
            continue;
        }
        let cap: u32 = stderr
            .lines()
            .find_map(|l| l.split("cap ").nth(1)?.split(' ').next()?.parse().ok())
            .unwrap_or_else(|| panic!("no cap in header: {stderr}"));
        assert!(cap > 0, "{args:?} announced a zero cap and ran anyway");
        let played: u32 = stderr
            .lines()
            .find_map(|l| l.split(" in ").nth(1)?.split(' ').next()?.parse().ok())
            .unwrap_or_else(|| panic!("no match line: {stderr}"));
        assert!(played <= cap, "{args:?} played {played} ticks past a {cap} cap");
        assert!(ok);
    }
}
