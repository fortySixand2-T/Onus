//! L2 integration tests for **B2 AC3** — side-balanced sampling.
//!
//! The batch already varies the *faction slot* by playing every ordered pair:
//! `(a, b)` and `(b, a)` swap who thinks first and whose RNG stream is whose.
//! It does not vary the *geography*: slot A has always spawned at the left-hand
//! base. Those are two different axes, and until now only one of them moved, so
//! a left-side edge would have been indistinguishable from strategy strength.
//!
//! What is encoded here:
//!
//!   - **orientation moves the bases and nothing else** — in
//!     [`Orientation::Swapped`] faction A spawns where B did and vice versa,
//!     with the seed, the slot order and the strategy-to-slot assignment
//!     untouched (asserted against the spawned positions themselves);
//!   - **the default is unchanged** — `MatchSettings::default()` is `Normal`
//!     and still plays the pinned pre-B2 fixture;
//!   - **the batch plays both** — N·N·K·2 rows, each `(a, b, seed,
//!     orientation)` exactly once, in a stated deterministic order, with the
//!     orientation recoverable from every record;
//!   - **determinism** — the same batch twice is identical, in and out of
//!     process;
//!   - **the balancing balances** — over a mirror matchup played in both
//!     orientations, a purely positional edge cancels between the faction
//!     slots, and the tally can report the raw per-orientation counts as well
//!     as the corrected aggregate.

use std::process::Command;

use bevy::prelude::*;

use onus::batch::{self, BatchSettings};
use onus::headless::{self, MatchSettings, Orientation};
use onus::sim::content::Content;
use onus::sim::economy::{Building, ProductionQueue, UnitDefIdx};
use onus::sim::spatial::Faction;
use onus::sim::{AiCommanders, CommandLog, Position, ResourceNode, StateHashLog};

fn content() -> Content {
    headless::content().expect("assets/data/*.ron parse into sim structs")
}

fn built(settings: &MatchSettings) -> App {
    headless::ai_vs_ai(content(), settings).expect("the settings name only shipped strategies")
}

/// Every HQ in the world, by faction.
fn hqs(app: &mut App) -> Vec<(Faction, Vec2)> {
    let mut v: Vec<(Faction, Vec2)> = app
        .world_mut()
        .query_filtered::<(&Faction, &Position), (With<Building>, With<ProductionQueue>)>()
        .iter(app.world())
        .map(|(f, p)| (*f, p.0))
        .collect();
    v.sort_by_key(|(f, _)| headless::SIDES.iter().position(|s| s == f).unwrap());
    v
}

// ---- orientation moves the bases, and only the bases -----------------------

#[test]
fn swapping_the_orientation_swaps_which_base_each_faction_spawns_at() {
    let mut normal = built(&MatchSettings::default().with_seed(4));
    let mut swapped = built(&MatchSettings::default()
        .with_seed(4)
        .with_orientation(Orientation::Swapped));

    let n = hqs(&mut normal);
    let s = hqs(&mut swapped);
    assert_eq!(
        n,
        vec![
            (Faction::A, Vec2::new(-750.0, 0.0)),
            (Faction::B, Vec2::new(750.0, 0.0)),
        ],
        "Normal is the pre-B2 geometry"
    );
    assert_eq!(
        s,
        vec![
            (Faction::A, Vec2::new(750.0, 0.0)),
            (Faction::B, Vec2::new(-750.0, 0.0)),
        ],
        "Swapped puts each faction at the other's base"
    );
}

#[test]
fn swapping_reflects_the_whole_fixture_not_just_the_hq() {
    // Nodes and starting workers follow their base, so the swapped world is the
    // x-mirror of the normal one: same set of positions, opposite factions.
    let each = |o: Orientation| -> Vec<(String, Vec2)> {
        let mut app = built(&MatchSettings::default().with_seed(4).with_orientation(o));
        let mut v: Vec<(String, Vec2)> = app
            .world_mut()
            .query::<(&UnitDefIdx, &Faction, &Position)>()
            .iter(app.world())
            .map(|(_, f, p)| (format!("{f:?}"), p.0))
            .collect();
        let nodes: Vec<(String, Vec2)> = app
            .world_mut()
            .query::<(&ResourceNode, &Position)>()
            .iter(app.world())
            .map(|(_, p)| ("node".to_string(), p.0))
            .collect();
        v.extend(nodes);
        v.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.x.total_cmp(&b.1.x)).then(a.1.y.total_cmp(&b.1.y)));
        v
    };
    let normal = each(Orientation::Normal);
    let swapped = each(Orientation::Swapped);
    assert!(!normal.is_empty());
    // Reflecting the swapped world in x recovers the normal one exactly.
    let reflected: Vec<(String, Vec2)> = {
        let mut v: Vec<(String, Vec2)> = swapped
            .iter()
            .map(|(k, p)| (k.clone(), Vec2::new(-p.x, p.y)))
            .collect();
        v.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.x.total_cmp(&b.1.x)).then(a.1.y.total_cmp(&b.1.y)));
        v
    };
    assert_eq!(reflected, normal, "Swapped must be the map reflected in x");
}

#[test]
fn orientation_leaves_the_slots_the_seed_and_the_strategies_alone() {
    let c = content();
    let settings = MatchSettings::default()
        .with_seed(12)
        .with_strategies("rush", "turtle")
        .with_orientation(Orientation::Swapped);
    let app = built(&settings);
    // Faction slot order is unchanged, and so is who plays what.
    let names: Vec<&str> = app
        .world()
        .resource::<AiCommanders>()
        .commanders()
        .iter()
        .map(|cm| cm.strategy(&c).id.as_str())
        .collect();
    assert_eq!(names, vec!["rush", "turtle"], "orientation is not a slot swap");
    assert_eq!(headless::SIDES, [Faction::A, Faction::B]);
    assert_eq!(app.world().resource::<CommandLog>().seed(), 12);
}

#[test]
fn orientation_actually_changes_the_match() {
    let s = MatchSettings::default()
        .with_seed(4)
        .with_strategies("rush", "turtle")
        .with_hashing(true);
    let hashes = |o: Orientation| {
        let mut app = built(&s.clone().with_orientation(o));
        headless::tick(&mut app, 120);
        app.world().resource::<StateHashLog>().0.clone()
    };
    assert_ne!(
        hashes(Orientation::Normal),
        hashes(Orientation::Swapped),
        "a swapped map that hashes identically is not a swap"
    );
}

// ---- the default is the pinned fixture --------------------------------------

#[test]
fn the_default_orientation_is_normal_and_still_the_pinned_fixture() {
    assert_eq!(MatchSettings::default().orientation, Orientation::Normal);
    // The AC1 pin, re-asserted here: the default fixture's first ticks are the
    // pre-B2 bench fixture's.
    let mut app = built(&MatchSettings::default().with_seed(4).with_hashing(true));
    headless::tick(&mut app, 10);
    let log = &app.world().resource::<StateHashLog>().0;
    assert_eq!(log[0], 0x9a74_d7ad_acad_19be);
    assert_eq!(log[9], 0x624e_8c15_e192_2eb5);
}

// ---- the batch plays both orientations --------------------------------------

const SUBSET: [&str; 3] = ["rush", "turtle", "mass_ripper"];

fn subset(cap: u32, seeds: u32) -> BatchSettings {
    BatchSettings::default()
        .with_only(SUBSET.iter().map(|s| s.to_string()).collect())
        .with_tick_cap(cap)
        .with_seeds(seeds)
}

fn run(settings: &BatchSettings) -> Vec<batch::MatchRecord> {
    batch::run_batch(&content(), settings, &mut |_| {}).expect("only shipped strategy names")
}

#[test]
fn the_batch_plays_every_matchup_in_both_orientations_exactly_once() {
    let seeds = 2u32;
    let rows = run(&subset(3, seeds));
    let n = SUBSET.len();
    assert_eq!(rows.len(), n * n * seeds as usize * 2);

    let mut keys: Vec<(String, String, u64, Orientation)> = rows
        .iter()
        .map(|r| {
            (
                r.strategies[0].clone(),
                r.strategies[1].clone(),
                r.seed,
                r.orientation,
            )
        })
        .collect();
    let len = keys.len();
    keys.sort_by(|a, b| {
        (&a.0, &a.1, a.2, a.3.index()).cmp(&(&b.0, &b.1, b.2, b.3.index()))
    });
    keys.dedup();
    assert_eq!(keys.len(), len, "some (a, b, seed, orientation) played twice");
    assert_eq!(
        rows.iter()
            .filter(|r| r.orientation == Orientation::Swapped)
            .count(),
        rows.len() / 2,
        "half the rows are the swapped map"
    );
}

#[test]
fn the_batch_order_is_seed_major_then_ron_row_major_then_orientation() {
    let c = content();
    let k = 2u32;
    let rows = run(&subset(2, k));
    let ids: Vec<&str> = c
        .strategies
        .iter()
        .map(|s| s.id.as_str())
        .filter(|id| SUBSET.contains(id))
        .collect();
    let mut i = 0;
    for kk in 0..k {
        let seed = batch::seed_at(BatchSettings::default().seed_base, kk);
        for a in &ids {
            for b in &ids {
                for o in Orientation::ALL {
                    let r = &rows[i];
                    assert_eq!(
                        (r.strategies[0].as_str(), r.strategies[1].as_str()),
                        (*a, *b),
                        "row {i}"
                    );
                    assert_eq!(r.seed, seed, "row {i} is not seed-major");
                    assert_eq!(r.orientation, o, "row {i} orientation is innermost");
                    i += 1;
                }
            }
        }
    }
    assert_eq!(i, rows.len());
}

#[test]
fn the_same_batch_twice_is_identical_including_orientation() {
    let s = subset(5, 2);
    assert_eq!(run(&s), run(&s));
}

fn balance_stdout(args: &[&str]) -> String {
    let o = Command::new(env!("CARGO_BIN_EXE_balance"))
        .args(args)
        .output()
        .expect("the balance bin runs");
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn the_report_is_reproducible_out_of_process() {
    let args = ["--only", "rush,turtle", "--seeds", "1", "--tick-cap", "4"];
    let first = balance_stdout(&args);
    assert!(
        first.contains("matches      8"),
        "4 ordered pairs x 2 orientations: {first}"
    );
    assert_eq!(first, balance_stdout(&args), "the report is not reproducible");
}

// ---- the balancing balances -------------------------------------------------

/// The measurement this checkbox exists for. A mirror matchup is the same
/// script on both sides, so the only thing that can separate the two slots is
/// the map — and playing both orientations on the same seed makes a purely
/// positional edge land once on each slot. Small here (2 mirrors x 3 seeds x 2
/// orientations = 12 decided matches); the full 10-mirror figure is the bin's.
#[test]
fn mirrors_are_side_balanced_across_the_two_orientations() {
    const MIRRORS: [&str; 2] = ["rush", "mass_ripper"];
    const SEEDS: u32 = 3;
    let c = content();
    let mut records = Vec::new();
    for k in 0..SEEDS {
        let seed = batch::seed_at(0, k);
        for id in MIRRORS {
            for o in Orientation::ALL {
                let s = MatchSettings::default()
                    .with_seed(seed)
                    .with_strategies(id, id)
                    .with_orientation(o);
                records.push(batch::run_match(&c, &s).expect("shipped names"));
            }
        }
    }
    assert_eq!(records.len(), MIRRORS.len() * SEEDS as usize * 2);
    let t = batch::Tally::of(&records);
    assert_eq!(t.timeouts, 0, "an undecided mirror measures nothing");

    // The raw positional counts: which *base* won, regardless of slot.
    let left = t.spawn_wins[0];
    let right = t.spawn_wins[1];
    assert_eq!(left + right, t.decided);

    // The corrected figure: each (mirror, seed) is played once with A on the
    // left and once with A on the right, so an edge that is purely positional
    // contributes one win to each slot. The slot split must therefore be no
    // further from even than the positional split is.
    let dev = |a: usize, b: usize| (a as i64 - b as i64).abs();
    assert!(
        dev(t.wins[0], t.wins[1]) <= dev(left, right),
        "side-balancing made the slot split worse than the raw positional \
         split: slots {:?}, spawns [{left}, {right}]",
        t.wins
    );
}

/// The tally has to be able to *show* the asymmetry, not just cancel it: the
/// per-orientation counts are kept, so a report can print the raw numbers next
/// to the corrected one.
#[test]
fn the_tally_keeps_the_per_orientation_counts() {
    let rows = run(&subset(3, 1));
    let t = batch::Tally::of(&rows);
    assert_eq!(t.total, SUBSET.len() * SUBSET.len() * 2);
    assert_eq!(t.by_orientation[0].total, t.total / 2);
    assert_eq!(t.by_orientation[1].total, t.total / 2);
    assert_eq!(
        t.by_orientation[0].wins[0] + t.by_orientation[1].wins[0],
        t.wins[0]
    );
    // Every row in a per-orientation sub-tally came from that orientation.
    let normal: Vec<batch::MatchRecord> = rows
        .iter()
        .filter(|r| r.orientation == Orientation::Normal)
        .cloned()
        .collect();
    assert_eq!(batch::Tally::of(&normal).total, t.by_orientation[0].total);
}

/// Which spawn a win belongs to is a property of the record, derivable without
/// re-running anything.
#[test]
fn a_record_knows_which_spawn_its_winner_held() {
    assert_eq!(Orientation::Normal.left(), Faction::A);
    assert_eq!(Orientation::Swapped.left(), Faction::B);
    let rows = run(&subset(headless::DEFAULT_TICK_CAP, 1));
    for r in &rows {
        match r.winner() {
            Some(f) => assert_eq!(r.winner_at_left(), Some(f == r.orientation.left())),
            None => assert_eq!(r.winner_at_left(), None),
        }
    }
    assert!(rows.iter().any(|r| r.winner_at_left().is_some()));
}

// ---- CLI robustness (defects found by the AC2 critic) ------------------------

fn balance(args: &[&str]) -> (bool, String, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_balance"))
        .args(args)
        .output()
        .expect("the balance bin runs");
    (
        o.status.success(),
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

/// The cap the header announces, parsed back out of the run's own stderr.
fn announced_cap(stderr: &str) -> u32 {
    stderr
        .lines()
        .find_map(|l| l.split("cap ").nth(1)?.split(' ').next()?.parse().ok())
        .unwrap_or_else(|| panic!("no cap in header: {stderr}"))
}

/// Out-of-range numbers are refused, never truncated or wrapped. `--minutes`
/// used to compute `M * 60 * 60` in `u32` (panic in debug, silent wrap in
/// release) and the counts used to be `as u32` casts, so `--seeds 4294967297`
/// ran one seed and reported that it had run 4294967297.
#[test]
fn out_of_range_numbers_are_refused_not_truncated() {
    for args in [
        vec!["--seeds", "4294967297"],
        vec!["--tick-cap", "4294967297"],
        vec!["--minutes", "4294967297"],
        vec!["--minutes", "1193047"], // 1_193_046 minutes is the last that fits
        vec!["--minutes", "18446744073709551615"],
    ] {
        let (ok, stdout, stderr) = balance(&args);
        assert!(!ok, "{args:?} was accepted");
        assert!(stdout.is_empty(), "{args:?} wrote to stdout: {stdout}");
        assert!(
            stderr.contains("out of range"),
            "{args:?} did not say why: {stderr}"
        );
    }
    // ...and the largest in-range minute count is still accepted, so the
    // refusal is a boundary and not a blanket.
    let (ok, _, stderr) = balance(&["--only", "rush", "--minutes", "1193046", "--seeds", "0"]);
    assert!(!ok, "only --seeds 0 should have been refused");
    assert!(stderr.contains("--seeds"), "{stderr}");
}

/// `--tick-cap` and `--minutes` set the same budget; the documented rule is
/// last-flag-wins, and the announced cap is the one actually enforced.
#[test]
fn the_last_cap_flag_wins_and_is_the_cap_that_is_announced() {
    let (ok, _, stderr) = balance(&["--only", "rush", "--tick-cap", "5", "--minutes", "1"]);
    assert!(ok, "{stderr}");
    assert_eq!(announced_cap(&stderr), 60 * 60, "--minutes came last");

    let (ok, _, stderr) = balance(&["--only", "rush", "--minutes", "1", "--tick-cap", "5"]);
    assert!(ok, "{stderr}");
    assert_eq!(announced_cap(&stderr), 5, "--tick-cap came last");
    // And it is enforced: a 5-tick cap cannot play more than 5 ticks.
    for played in stderr
        .lines()
        .filter_map(|l| l.split(" in ").nth(1)?.split(' ').next()?.parse::<u32>().ok())
    {
        assert!(played <= 5, "played {played} ticks past a 5-tick cap");
    }
}

/// `--help` is a request, not a mistake: it succeeds on stdout. A mistyped flag
/// still fails on stderr.
#[test]
fn help_succeeds_on_stdout_and_documents_the_cap_precedence() {
    for flag in ["--help", "-h"] {
        let (ok, stdout, stderr) = balance(&[flag]);
        assert!(ok, "{flag} exited with failure");
        assert!(stdout.contains("usage: balance"), "{flag}: {stdout}");
        assert!(
            stdout.contains("last one given wins"),
            "{flag} does not document --tick-cap/--minutes precedence: {stdout}"
        );
        assert!(
            stdout.contains("orientations"),
            "{flag} does not mention the doubled batch: {stdout}"
        );
        assert!(stderr.is_empty(), "{flag} wrote to stderr: {stderr}");
    }
    let (ok, stdout, stderr) = balance(&["--halp"]);
    assert!(!ok, "an unknown flag must still fail");
    assert!(stdout.is_empty(), "{stdout}");
    assert!(stderr.contains("unknown flag"), "{stderr}");
}
