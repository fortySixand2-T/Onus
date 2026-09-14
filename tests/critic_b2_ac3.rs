//! Adversarial probes for **B2 AC3** — side-balanced sampling.
//!
//! Written against the checkbox and the requirements it was given, not against
//! the implementation:
//!
//!   - `Swapped` must be a reflection of the *whole* fixture (bases, deposits,
//!     starting workers), not a relabelling and not an HQ-only move;
//!   - orientation must move the geography and nothing else — same seed, same
//!     slot order, same strategy-to-slot assignment, same commander RNG
//!     streams, same spawn order;
//!   - `MatchSettings::default()` must still be the pre-B2 geometry;
//!   - the batch must play every `(a, b, seed)` in both orientations exactly
//!     once, seed-major -> RON row-major -> orientation, with `only` filtering
//!     and never reordering;
//!   - the positional accounting (`winner_at_left`, `spawn_wins`,
//!     `left_spawn_rate`, `slot_a_rate`) must agree with where the winner's HQ
//!     actually stood, and must never book an undecided row as a spawn win;
//!   - the CLI must refuse out-of-range numbers rather than wrap or truncate,
//!     document and honour cap precedence, and treat `--help` as a request.

use std::collections::BTreeMap;
use std::process::Command;

use bevy::prelude::*;

use onus::batch::{self, BatchSettings, MatchRecord, MatchResult};
use onus::headless::{self, MatchSettings, Orientation};
use onus::sim::combat::Health;
use onus::sim::content::Content;
use onus::sim::economy::{Building, ProductionQueue, UnitDefIdx};
use onus::sim::spatial::Faction;
use onus::sim::{AiCommanders, CommandLog, MatchState, Position, ResourceNode, StateHashLog};

fn content() -> Content {
    headless::content().expect("assets/data/*.ron load")
}

fn built(settings: &MatchSettings) -> App {
    headless::ai_vs_ai(content(), settings).expect("shipped strategy names")
}

/// Every positioned entity in the world, tagged by *what it is* and *whose it
/// is* — enough that a reflection cannot hide behind a coarse key.
fn fixture(app: &mut App) -> Vec<(String, [f32; 2])> {
    let mut v: Vec<(String, [f32; 2])> = Vec::new();
    for (f, p) in app
        .world_mut()
        .query_filtered::<(&Faction, &Position), (With<Building>, With<ProductionQueue>)>()
        .iter(app.world())
        .map(|(f, p)| (*f, p.0))
        .collect::<Vec<_>>()
    {
        v.push((format!("hq/{f:?}"), [p.x, p.y]));
    }
    for (f, p, idx, hp) in app
        .world_mut()
        .query::<(&Faction, &Position, &UnitDefIdx, &Health)>()
        .iter(app.world())
        .map(|(f, p, i, h)| (*f, p.0, i.0, format!("{h:?}")))
        .collect::<Vec<_>>()
    {
        v.push((format!("unit/{f:?}/def{idx}/{hp}"), [p.x, p.y]));
    }
    for (n, p) in app
        .world_mut()
        .query::<(&ResourceNode, &Position)>()
        .iter(app.world())
        .map(|(n, p)| (n.amount, p.0))
        .collect::<Vec<_>>()
    {
        v.push((format!("node/{n}"), [p.x, p.y]));
    }
    v.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then(a.1[0].total_cmp(&b.1[0]))
            .then(a.1[1].total_cmp(&b.1[1]))
    });
    v
}

// ---- is `Swapped` a true reflection? ---------------------------------------

/// Reflecting the normal world in x must reproduce the swapped world *exactly*,
/// entity for entity, with every tag (faction, unit def, hp, node amount)
/// preserved. A deposit or a worker left behind while its base moves is not a
/// reflection.
#[test]
fn probe_swapped_is_the_whole_fixture_reflected_in_x() {
    for seed in [0u64, 4, 99] {
        let mut normal = built(&MatchSettings::default().with_seed(seed));
        let mut swapped = built(&MatchSettings::default()
            .with_seed(seed)
            .with_orientation(Orientation::Swapped));
        let n = fixture(&mut normal);
        let s = fixture(&mut swapped);
        assert!(n.len() >= 2 * (1 + 1 + 3), "fixture is not empty: {}", n.len());
        assert_eq!(n.len(), s.len(), "the two orientations spawn different worlds");
        let mut reflected: Vec<(String, [f32; 2])> = n
            .iter()
            .map(|(k, p)| (k.clone(), [-p[0], p[1]]))
            .collect();
        reflected.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then(a.1[0].total_cmp(&b.1[0]))
                .then(a.1[1].total_cmp(&b.1[1]))
        });
        assert_eq!(reflected, s, "seed {seed}: Swapped is not the x-mirror of Normal");
    }
}

/// Every entity a faction owns must sit on that faction's own side of the map
/// in both orientations: nothing may be stranded across the midline.
#[test]
fn probe_nothing_is_stranded_on_the_far_side_of_the_map() {
    for o in Orientation::ALL {
        let mut app = built(&MatchSettings::default().with_orientation(o));
        let owned: Vec<(Faction, Vec2)> = app
            .world_mut()
            .query::<(&Faction, &Position)>()
            .iter(app.world())
            .map(|(f, p)| (*f, p.0))
            .collect();
        assert!(!owned.is_empty());
        for (f, p) in owned {
            let want_left = f == o.left();
            assert_eq!(
                p.x < 0.0,
                want_left,
                "{o:?}: {f:?} owns an entity at {p:?} on the wrong side"
            );
        }
        // The deposits are one per side, and they straddle the midline too.
        let mut nodes: Vec<f32> = app
            .world_mut()
            .query_filtered::<&Position, With<ResourceNode>>()
            .iter(app.world())
            .map(|p| p.0.x)
            .collect();
        nodes.sort_by(f32::total_cmp);
        assert_eq!(nodes, vec![-750.0, 750.0], "{o:?}: deposits are not mirrored");
    }
}

/// `Swapped` must move the *factions*, not relabel a static map: in both
/// orientations the same two bases exist, and A occupies the other one.
#[test]
fn probe_swapped_moves_the_factions_and_keeps_the_map() {
    let hq = |o: Orientation, want: Faction| -> Vec2 {
        let mut app = built(&MatchSettings::default().with_orientation(o));
        let v: Vec<(Faction, Vec2)> = app
            .world_mut()
            .query_filtered::<(&Faction, &Position), With<Building>>()
            .iter(app.world())
            .map(|(f, p)| (*f, p.0))
            .collect();
        v.iter().find(|(f, _)| *f == want).expect("an HQ").1
    };
    assert_eq!(hq(Orientation::Normal, Faction::A), Vec2::new(-750.0, 0.0));
    assert_eq!(hq(Orientation::Normal, Faction::B), Vec2::new(750.0, 0.0));
    assert_eq!(hq(Orientation::Swapped, Faction::A), Vec2::new(750.0, 0.0));
    assert_eq!(hq(Orientation::Swapped, Faction::B), Vec2::new(-750.0, 0.0));
    assert_eq!(Orientation::Normal.left(), Faction::A);
    assert_eq!(Orientation::Swapped.left(), Faction::B);
}

// ---- does orientation leak beyond geometry? ---------------------------------

/// The two orientations of one seed are a *controlled pair*: same commanders,
/// same RNG streams, same slot order, same strategy assignment, same log seed.
/// If orientation perturbed any of these the pair would not be controlled and
/// the balancing would be void.
#[test]
fn probe_orientation_does_not_perturb_the_commanders_or_the_seed() {
    let c = content();
    let read = |o: Orientation, seed: u64| -> (Vec<(String, u64, u32)>, u64) {
        let app = built(&MatchSettings::default()
            .with_seed(seed)
            .with_strategies("rush", "turtle")
            .with_orientation(o));
        let cs: Vec<(String, u64, u32)> = app
            .world()
            .resource::<AiCommanders>()
            .commanders()
            .iter()
            .map(|cm| (cm.strategy(&c).id.clone(), cm.rng_state(), cm.tick()))
            .collect();
        (cs, app.world().resource::<CommandLog>().seed())
    };
    for seed in [0u64, 7, 12345] {
        let a = read(Orientation::Normal, seed);
        let b = read(Orientation::Swapped, seed);
        assert_eq!(a, b, "seed {seed}: orientation moved something other than the map");
        assert_eq!(
            a.0.iter().map(|c| c.0.as_str()).collect::<Vec<_>>(),
            vec!["rush", "turtle"],
            "slot order / strategy assignment changed"
        );
        assert_eq!(a.1, seed);
    }
    // Non-vacuous: a different seed *does* move the streams.
    assert_ne!(read(Orientation::Normal, 0).0, read(Orientation::Normal, 1).0);
}

/// Spawn order (and therefore entity id order, which the sim's stable
/// iteration rests on) must not depend on orientation.
#[test]
fn probe_spawn_order_is_identical_across_orientations() {
    let ids = |o: Orientation| -> Vec<(String, String)> {
        let mut app = built(&MatchSettings::default()
            .with_seed(3)
            .with_orientation(o));
        let mut v: Vec<(String, String)> = app
            .world_mut()
            .query::<(Entity, &Faction, &Position)>()
            .iter(app.world())
            .map(|(e, f, _)| (format!("{:?}", e.index()), format!("{f:?}")))
            .collect();
        v.sort();
        v
    };
    assert_eq!(
        ids(Orientation::Normal),
        ids(Orientation::Swapped),
        "orientation changed which entity id each faction's spawns got"
    );
}

/// Orientation is geography, so it must not reach the sim as a concept: no sim
/// module may name it.
#[test]
fn probe_orientation_never_enters_the_sim() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/sim");
    let mut stack = vec![dir];
    let mut checked = 0usize;
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).expect("src/sim is readable") {
            let p = e.expect("a dir entry").path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                let src = std::fs::read_to_string(&p).expect("readable");
                assert!(
                    !src.contains("Orientation"),
                    "{}: the sim must not know about spawn orientation",
                    p.display()
                );
                checked += 1;
            }
        }
    }
    assert!(checked > 3, "only {checked} sim files scanned");
}

// ---- the default is untouched ----------------------------------------------

/// `Normal` must genuinely be the old geometry (A on the left), and the default
/// must play bit-for-bit what an explicitly-Normal match plays — while the
/// swapped one is a different match.
#[test]
fn probe_the_default_is_the_old_geometry_not_a_self_consistent_swap() {
    assert_eq!(MatchSettings::default().orientation, Orientation::Normal);
    let mut app = built(&MatchSettings::default());
    let a_hq = app
        .world_mut()
        .query_filtered::<(&Faction, &Position), With<Building>>()
        .iter(app.world())
        .find(|(f, _)| **f == Faction::A)
        .map(|(_, p)| p.0)
        .expect("A has an HQ");
    assert!(a_hq.x < 0.0, "the default must spawn A at the left base, got {a_hq:?}");

    let log = |s: MatchSettings| {
        let mut app = built(&s.with_hashing(true));
        headless::tick(&mut app, 90);
        app.world().resource::<StateHashLog>().0.clone()
    };
    let d = log(MatchSettings::default().with_seed(4));
    assert_eq!(
        d,
        log(MatchSettings::default().with_seed(4).with_orientation(Orientation::Normal)),
        "default != explicit Normal"
    );
    assert_ne!(
        d,
        log(MatchSettings::default().with_seed(4).with_orientation(Orientation::Swapped)),
        "Swapped hashes the same as Normal"
    );
    assert_eq!(d.len(), 90);
}

// ---- the batch: completeness, order, filtering ------------------------------

const PAIR: [&str; 2] = ["rush", "turtle"];

fn small(cap: u32, seeds: u32, only: &[&str]) -> BatchSettings {
    BatchSettings::default()
        .with_only(only.iter().map(|s| s.to_string()).collect())
        .with_tick_cap(cap)
        .with_seeds(seeds)
}

fn run(s: &BatchSettings) -> Vec<MatchRecord> {
    batch::run_batch(&content(), s, &mut |_| {}).expect("shipped names")
}

fn key(r: &MatchRecord) -> (String, String, u64, usize) {
    (
        r.strategies[0].clone(),
        r.strategies[1].clone(),
        r.seed,
        r.orientation.index(),
    )
}

/// Exactly N*N*K*2 rows, each `(a, b, seed, orientation)` once, in the stated
/// order: seed-major, then RON row-major, then orientation.
#[test]
fn probe_the_batch_is_exactly_the_product_in_the_stated_order() {
    let c = content();
    let k = 2u32;
    let rows = run(&small(2, k, &PAIR));
    let ids: Vec<&str> = c
        .strategies
        .iter()
        .map(|s| s.id.as_str())
        .filter(|id| PAIR.contains(id))
        .collect();
    let n = ids.len();
    assert_eq!(n, 2);
    assert_eq!(rows.len(), n * n * k as usize * 2);

    let mut expected: Vec<(String, String, u64, usize)> = Vec::new();
    for kk in 0..k {
        let seed = batch::seed_at(BatchSettings::default().seed_base, kk);
        for a in &ids {
            for b in &ids {
                for o in Orientation::ALL {
                    expected.push((a.to_string(), b.to_string(), seed, o.index()));
                }
            }
        }
    }
    let got: Vec<(String, String, u64, usize)> = rows.iter().map(key).collect();
    assert_eq!(got, expected, "row order is not seed-major -> RON -> orientation");

    let mut uniq = got.clone();
    uniq.sort();
    uniq.dedup();
    assert_eq!(uniq.len(), got.len(), "a cell was played twice");
    // Both orientations are present in equal number, per matchup.
    let mut per: BTreeMap<(String, String, u64), Vec<usize>> = BTreeMap::new();
    for r in &rows {
        per.entry((r.strategies[0].clone(), r.strategies[1].clone(), r.seed))
            .or_default()
            .push(r.orientation.index());
    }
    assert_eq!(per.len(), n * n * k as usize);
    for (cell, mut os) in per {
        os.sort();
        assert_eq!(os, vec![0, 1], "{cell:?} was not played in both orientations");
    }
}

/// `only` filters; it never reorders, and the order it is typed in is
/// irrelevant — including the orientation column.
#[test]
fn probe_only_filters_without_reordering_under_both_orientations() {
    let forward = run(&small(2, 1, &["rush", "turtle"]));
    let backward = run(&small(2, 1, &["turtle", "rush"]));
    assert_eq!(forward, backward, "the flag's typing order reached the rows");
    let full = run(&BatchSettings::default().with_tick_cap(2).with_seeds(1));
    let expect: Vec<&MatchRecord> = full
        .iter()
        .filter(|r| {
            PAIR.contains(&r.strategies[0].as_str()) && PAIR.contains(&r.strategies[1].as_str())
        })
        .collect();
    assert_eq!(expect.len(), 8, "2x2 matchups x 2 orientations");
    assert_eq!(forward.iter().collect::<Vec<_>>(), expect);
}

// ---- positional accounting --------------------------------------------------

/// The differential oracle: `winner_at_left()` must agree with where the
/// winner's HQ *actually stood* in the world that was played, in both
/// orientations.
#[test]
fn probe_winner_at_left_agrees_with_the_winning_hq_position() {
    let c = content();
    let mut checked = 0usize;
    for o in Orientation::ALL {
        let s = MatchSettings::default()
            .with_seed(batch::seed_at(0, 0))
            .with_strategies("rush", "turtle")
            .with_tick_cap(headless::DEFAULT_TICK_CAP)
            .with_orientation(o);
        let record = batch::run_match(&c, &s).expect("shipped names");
        // Replay the same match and read the geometry out of the world.
        let mut app = headless::ai_vs_ai(content(), &s).expect("shipped names");
        let mut ticks = 0u32;
        while ticks < s.tick_cap && app.world().resource::<MatchState>().outcome().is_none() {
            headless::step(&mut app);
            ticks += 1;
        }
        assert_eq!(ticks, record.ticks, "{o:?}: the replay diverged from the record");
        let outcome = app.world().resource::<MatchState>().outcome();
        assert_eq!(
            outcome.and_then(|x| x.winner),
            record.winner(),
            "{o:?}: the record's winner is not the sim's"
        );
        if let Some(w) = record.winner() {
            // The winner's base is the one it spawned at, which is the base the
            // orientation assigned it.
            let base_x = if w == o.left() { -750.0 } else { 750.0 };
            assert_eq!(
                record.winner_at_left(),
                Some(base_x < 0.0),
                "{o:?}: winner {w:?} spawned at x={base_x} but winner_at_left says otherwise"
            );
            assert_eq!(record.winner_at_left(), Some(w == o.left()));
            checked += 1;
        }
    }
    assert!(checked > 0, "no decided match to check the oracle against");
}

/// A tally must never book an undecided row as a spawn win, and its two rates
/// must share the *decided* denominator.
#[test]
fn probe_the_tally_never_miscounts_an_undecided_row() {
    let rec = |result: MatchResult, o: Orientation| MatchRecord {
        strategies: ["x".into(), "x".into()],
        seed: 1,
        result,
        orientation: o,
        ticks: 10,
    };
    let rows = vec![
        rec(MatchResult::Decided(Faction::A), Orientation::Normal),
        rec(MatchResult::Decided(Faction::A), Orientation::Swapped),
        rec(MatchResult::Decided(Faction::B), Orientation::Normal),
        rec(MatchResult::Timeout, Orientation::Normal),
        rec(MatchResult::Timeout, Orientation::Swapped),
        rec(MatchResult::MutualLoss, Orientation::Normal),
        rec(MatchResult::MutualLoss, Orientation::Swapped),
    ];
    let t = batch::Tally::of(&rows);
    assert_eq!(t.total, 7);
    assert_eq!(t.decided, 3);
    assert_eq!(t.wins, [2, 1]);
    // Positional: A@Normal is left, A@Swapped is right, B@Normal is right.
    assert_eq!(t.spawn_wins, [1, 2]);
    assert_eq!(
        t.spawn_wins[0] + t.spawn_wins[1],
        t.decided,
        "an undecided row was booked as a spawn win"
    );
    assert_eq!(t.wins[0] + t.wins[1], t.decided);
    assert_eq!(t.left_spawn_rate(), Some(1.0 / 3.0));
    assert_eq!(t.slot_a_rate(), Some(2.0 / 3.0));
    assert_eq!(t.timeouts, 2);
    assert_eq!(t.mutual_losses, 2);
    assert_eq!(t.by_orientation[0].total, 4);
    assert_eq!(t.by_orientation[1].total, 3);
    assert_eq!(t.by_orientation[0].wins, [1, 1]);
    assert_eq!(t.by_orientation[1].wins, [1, 0]);
    assert_eq!(
        t.by_orientation[0].total + t.by_orientation[1].total,
        t.total
    );
    // An all-undecided batch reports no rate at all rather than 0%.
    let u = batch::Tally::of(&[
        rec(MatchResult::Timeout, Orientation::Normal),
        rec(MatchResult::MutualLoss, Orientation::Swapped),
    ]);
    assert_eq!(u.left_spawn_rate(), None);
    assert_eq!(u.slot_a_rate(), None);
    assert_eq!(u.spawn_wins, [0, 0]);
}

/// Slot wins and spawn wins are the *same* decided matches, re-bucketed: the
/// mapping is `slot -> position` through the row's own orientation. Checked
/// against real batch rows rather than synthetic ones.
#[test]
fn probe_spawn_wins_are_a_rebucketing_of_the_same_decided_matches() {
    let rows = run(&small(headless::DEFAULT_TICK_CAP, 1, &PAIR));
    let t = batch::Tally::of(&rows);
    assert_eq!(t.wins[0] + t.wins[1], t.spawn_wins[0] + t.spawn_wins[1]);
    let left: usize = rows
        .iter()
        .filter(|r| r.winner().is_some_and(|f| f == r.orientation.left()))
        .count();
    assert_eq!(t.spawn_wins[0], left);
    let a: usize = rows
        .iter()
        .filter(|r| r.winner() == Some(Faction::A))
        .count();
    assert_eq!(t.wins[0], a);
    assert!(t.decided > 0, "nothing was decided; the probe measured nothing");
}

// ---- determinism ------------------------------------------------------------

#[test]
fn probe_a_rerun_is_identical_in_process_with_orientation_in_the_mix() {
    let s = small(4, 2, &PAIR);
    let a = run(&s);
    let b = run(&s);
    assert_eq!(a, b);
    assert_eq!(a.len(), 16);
}

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

#[test]
fn probe_the_cli_is_identical_out_of_process_including_progress() {
    let args = ["--only", "rush,turtle", "--seeds", "2", "--tick-cap", "4"];
    let (ok, out1, err1) = balance(&args);
    assert!(ok, "{err1}");
    let (ok2, out2, err2) = balance(&args);
    assert!(ok2);
    assert_eq!(out1, out2, "the report is not reproducible");
    assert_eq!(err1, err2, "the progress stream is not reproducible");
    assert!(out1.contains("matches      16"), "{out1}");
    assert!(err1.contains("[16/16]"), "{err1}");
    assert!(err1.contains("[normal]") && err1.contains("[swapped]"), "{err1}");
}

// ---- CLI range, precedence, help -------------------------------------------

/// `--seeds` / `--tick-cap` must accept every `u32` and refuse anything above
/// it, rather than truncating with `as u32`. The acceptance side is probed
/// without actually running four billion seeds: a later, unrelated refusal
/// proves parsing got past the number.
#[test]
fn probe_counts_are_refused_above_u32_not_truncated() {
    for flag in ["--seeds", "--tick-cap"] {
        let (ok, out, err) = balance(&[flag, "4294967296", "--only", "nosuchstrategy"]);
        assert!(!ok, "{flag} 2^32 was accepted");
        assert!(out.is_empty(), "{flag}: {out}");
        assert!(err.contains("out of range"), "{flag} truncated instead: {err}");
        assert!(err.contains(flag), "{flag}: the refusal does not name the flag: {err}");

        // u32::MAX parses; the run then fails for the *other* reason.
        let (ok, out, err) = balance(&[flag, "4294967295", "--only", "nosuchstrategy"]);
        assert!(!ok);
        assert!(out.is_empty());
        assert!(
            !err.contains("out of range"),
            "{flag} u32::MAX must be in range: {err}"
        );
        assert!(err.contains("nosuchstrategy"), "{flag}: {err}");
    }
    // Degenerate counts are still refused.
    for args in [["--seeds", "0"], ["--tick-cap", "0"], ["--seeds", "-1"]] {
        let (ok, out, err) = balance(&args);
        assert!(!ok, "{args:?} accepted");
        assert!(out.is_empty(), "{args:?}: {out}");
        assert!(!err.is_empty());
    }
}

/// `--minutes M` must not overflow `M * 60 * SIM_HZ`: panicking in debug or
/// wrapping in release are both "a cap that was never enforced, reported as if
/// it had been". The boundary is exact.
#[test]
fn probe_minutes_refuses_at_the_overflow_boundary_and_accepts_below_it() {
    let max_minutes = u32::MAX / (60 * 60);
    assert_eq!(max_minutes, 1_193_046);
    // The last minute count that fits is accepted (it gets as far as the
    // roster refusal).
    let (ok, out, err) = balance(&["--minutes", "1193046", "--only", "nosuchstrategy"]);
    assert!(!ok);
    assert!(out.is_empty());
    assert!(!err.contains("out of range"), "the boundary was refused: {err}");
    assert!(err.contains("nosuchstrategy"), "{err}");
    // One more minute overflows u32 and must be refused, not wrapped.
    for m in ["1193047", "4294967296", "18446744073709551615"] {
        let (ok, out, err) = balance(&["--minutes", m, "--only", "nosuchstrategy"]);
        assert!(!ok, "--minutes {m} accepted");
        assert!(out.is_empty(), "{m}: {out}");
        assert!(err.contains("out of range"), "--minutes {m} wrapped: {err}");
        assert!(err.contains("--minutes"), "{m}: {err}");
    }
    // And a real run at a large-but-valid minute count announces the exact cap.
    let (ok, _, err) = balance(&["--only", "rush", "--minutes", "1193046", "--tick-cap", "3"]);
    assert!(ok, "{err}");
    assert!(err.contains("cap 3 ticks"), "{err}");
}

/// The precedence rule the usage text states — last flag wins — must be what
/// the run does, in both orders, and the announced cap must be the enforced
/// one.
#[test]
fn probe_cap_precedence_is_last_flag_wins_in_both_orders() {
    let cap_of = |err: &str| -> u32 {
        err.lines()
            .find_map(|l| l.split("cap ").nth(1)?.split(' ').next()?.parse().ok())
            .unwrap_or_else(|| panic!("no cap announced: {err}"))
    };
    let played = |err: &str| -> Vec<u32> {
        err.lines()
            .filter_map(|l| l.split(" in ").nth(1)?.split(' ').next()?.parse::<u32>().ok())
            .collect()
    };
    let (ok, _, err) = balance(&["--only", "rush", "--minutes", "1", "--tick-cap", "7"]);
    assert!(ok, "{err}");
    assert_eq!(cap_of(&err), 7, "--tick-cap was last: {err}");
    let p = played(&err);
    assert_eq!(p.len(), 2, "one row per orientation: {err}");
    assert!(p.iter().all(|t| *t <= 7), "a match ran past the cap: {p:?}");

    let (ok, _, err) = balance(&["--only", "rush", "--tick-cap", "7", "--minutes", "1"]);
    assert!(ok, "{err}");
    assert_eq!(cap_of(&err), 3600, "--minutes was last: {err}");

    // Repeating one flag is also last-wins.
    let (ok, _, err) = balance(&["--only", "rush", "--tick-cap", "9", "--tick-cap", "2"]);
    assert!(ok, "{err}");
    assert_eq!(cap_of(&err), 2, "{err}");
}

/// `--help` is a request: success, on stdout, nothing on stderr. A typo is
/// still an error: failure, on stderr, nothing on stdout.
#[test]
fn probe_help_succeeds_on_stdout_and_typos_still_fail() {
    for flag in ["--help", "-h"] {
        let (ok, out, err) = balance(&[flag]);
        assert!(ok, "{flag} exited with failure");
        assert!(out.contains("usage: balance"), "{flag}: {out}");
        assert!(err.is_empty(), "{flag} wrote to stderr: {err}");
        assert!(
            out.contains("last one given wins"),
            "{flag} does not document cap precedence: {out}"
        );
    }
    // Help asked for after other flags is still help, and still costs nothing.
    let (ok, out, err) = balance(&["--seeds", "2", "--help"]);
    assert!(ok, "{err}");
    assert!(out.contains("usage: balance"), "{out}");
    assert!(err.is_empty(), "{err}");

    for args in [vec!["--halp"], vec!["--seeds"], vec!["-x", "1"]] {
        let (ok, out, err) = balance(&args);
        assert!(!ok, "{args:?} was accepted");
        assert!(out.is_empty(), "{args:?} wrote to stdout: {out}");
        assert!(!err.is_empty(), "{args:?} failed silently");
    }
}

// ---- what side-balancing actually guarantees --------------------------------

/// The real guarantee of playing both orientations on the same seed: an edge
/// that is *purely positional* lands once on each slot, so it cancels exactly
/// in the slot split while staying fully visible in the positional one.
#[test]
fn probe_a_purely_positional_edge_cancels_exactly_in_the_slot_split() {
    // Six cells, each played in both orientations, with the left base always
    // winning — the extreme positional bias.
    let mut rows = Vec::new();
    for cell in 0..6u64 {
        for o in Orientation::ALL {
            rows.push(MatchRecord {
                strategies: ["x".into(), "x".into()],
                seed: cell,
                result: MatchResult::Decided(o.left()),
                orientation: o,
                ticks: 100,
            });
        }
    }
    let t = batch::Tally::of(&rows);
    assert_eq!(t.spawn_wins, [12, 0], "the raw positional edge must stay visible");
    assert_eq!(t.left_spawn_rate(), Some(1.0));
    assert_eq!(t.wins, [6, 6], "a purely positional edge must cancel exactly");
    assert_eq!(t.slot_a_rate(), Some(0.5));
    // ...and the per-orientation halves each show the full asymmetry.
    assert_eq!(t.by_orientation[0].wins, [6, 0]);
    assert_eq!(t.by_orientation[1].wins, [0, 6]);
}

/// The converse, and the reason "the slot split is no further from even than
/// the positional split" is *not* a theorem: an edge that follows the slot
/// rather than the map survives reflection untouched, and the slot split is
/// then further from even than the positional one. The tally must report both
/// faithfully rather than smoothing the slot edge away.
#[test]
fn probe_a_turn_order_edge_survives_reflection_and_is_reported_as_such() {
    let mut rows = Vec::new();
    for cell in 0..6u64 {
        for o in Orientation::ALL {
            rows.push(MatchRecord {
                strategies: ["x".into(), "x".into()],
                seed: cell,
                result: MatchResult::Decided(Faction::A),
                orientation: o,
                ticks: 100,
            });
        }
    }
    let t = batch::Tally::of(&rows);
    assert_eq!(t.wins, [12, 0], "a slot edge must not be cancelled by reflection");
    assert_eq!(t.slot_a_rate(), Some(1.0));
    assert_eq!(t.spawn_wins, [6, 6], "a slot edge is positionally even");
    assert_eq!(t.left_spawn_rate(), Some(0.5));
    let dev = |a: usize, b: usize| (a as i64 - b as i64).abs();
    assert!(
        dev(t.wins[0], t.wins[1]) > dev(t.spawn_wins[0], t.spawn_wins[1]),
        "orientation cannot bound the slot split: this is a counterexample"
    );
}
