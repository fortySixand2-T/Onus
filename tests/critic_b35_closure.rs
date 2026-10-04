//! Adversarial critic probes for the **B3.5 closure** (F-032 .. F-036).
//!
//! Written from the spec and the ledger claims, independently of the
//! implementer's tests:
//!   1. `Cell::wilson_interval` against an independent oracle (the score
//!      inequality solved as a quadratic, and by bisection), over every
//!      `(half_wins, n)` up to n = 120;
//!   2. `Verdict` boundaries swept exhaustively through `PentagonReport`:
//!      zero decided (all-timeout) is `Undefined` and never 0.5, n = 1 either
//!      way, exactly 0.5, the first hold / first fail at each n, and timeouts
//!      never entering the interval;
//!   3. F-035's claim that every opening it trimmed was **dead**: the shipped
//!      (trimmed) content and the pre-trim openings restored in memory (same
//!      binary, same turtle threshold, `synth_triad` back in its old order)
//!      must play bit-identical matches — same journal, same end tick, same
//!      result, same canonical state hash — in head-to-head play, both
//!      orientations, on seeds the closure never measured;
//!   4. F-036's claim that the turtle commits latest **on every seed** (the
//!      ledger says the commit ticks are seed-independent), not just seed 4.

use std::sync::Mutex;

use onus::batch::{seed_at, MatchRecord, MatchResult, ProductionCounts};
use onus::headless::{self, MatchSettings, Orientation, DEFAULT_TICK_CAP};
use onus::metrics::{Cell, WinMatrix};
use onus::pentagon::{PentagonReport, Verdict};
use onus::sim::content::{BarracksOpening, Content};
use onus::sim::spatial::Faction;
use onus::sim::{state_hash, AiAction, AiJournal, MatchState};

// ---- 1. Wilson against an independent oracle ---------------------------------

/// The exact two-sided 95% normal quantile (not the 10-digit literal in src).
const Z: f64 = 1.959_963_984_540_054;

/// Wilson bounds as the two roots of `n (p - x)^2 = z^2 x (1 - x)` in `x`:
/// `(n + z^2) x^2 - (2 n p + z^2) x + n p^2 = 0`. A different algebraic route
/// from the textbook centre/half-width form.
fn oracle_quadratic(p: f64, n: f64) -> (f64, f64) {
    let z2 = Z * Z;
    let a = n + z2;
    let b = -(2.0 * n * p + z2);
    let c = n * p * p;
    let disc = (b * b - 4.0 * a * c).max(0.0).sqrt();
    let lo = (-b - disc) / (2.0 * a);
    let hi = (-b + disc) / (2.0 * a);
    (lo.max(0.0), hi.min(1.0))
}

/// The same bounds by bisection on the score statistic, no algebra at all.
fn oracle_bisect(p: f64, n: f64) -> (f64, f64) {
    let inside = |x: f64| n * (p - x) * (p - x) <= Z * Z * x * (1.0 - x) + 1e-15;
    let solve = |mut out: f64, mut inn: f64| {
        for _ in 0..200 {
            let mid = 0.5 * (out + inn);
            if inside(mid) {
                inn = mid;
            } else {
                out = mid;
            }
        }
        0.5 * (out + inn)
    };
    let lo = if p == 0.0 { 0.0 } else { solve(0.0, p) };
    let hi = if p == 1.0 { 1.0 } else { solve(1.0, p) };
    (lo, hi)
}

#[test]
fn wilson_matches_two_independent_oracles_for_every_cell_up_to_n_120() {
    for n in 1u32..=120 {
        for half_wins in 0..=2 * n {
            let cell = Cell {
                half_wins,
                n_decided: n,
                n_timeout: 7,
            };
            let p = half_wins as f64 / (2.0 * n as f64);
            assert_eq!(cell.rate(), Some(p));
            let (lo, hi) = cell.wilson_interval().expect("decided");
            let (qlo, qhi) = oracle_quadratic(p, n as f64);
            let (blo, bhi) = oracle_bisect(p, n as f64);
            for (name, (olo, ohi)) in [("quadratic", (qlo, qhi)), ("bisection", (blo, bhi))] {
                assert!(
                    (lo - olo).abs() < 1e-8 && (hi - ohi).abs() < 1e-8,
                    "{half_wins}/2 of {n}: src [{lo:.10}, {hi:.10}] vs {name} [{olo:.10}, {ohi:.10}]"
                );
            }
            assert!(lo <= p + 1e-12 && p <= hi + 1e-12, "{half_wins}/2 of {n}: p outside");
            assert!((0.0..=1.0).contains(&lo) && (0.0..=1.0).contains(&hi));
        }
    }
}

/// Symmetry: the interval of `k` is the mirror of the interval of `n - k`.
#[test]
fn wilson_is_symmetric_about_a_half() {
    for n in 1u32..=80 {
        for h in 0..=2 * n {
            let a = Cell { half_wins: h, n_decided: n, n_timeout: 0 }.wilson_interval().unwrap();
            let b = Cell { half_wins: 2 * n - h, n_decided: n, n_timeout: 0 }
                .wilson_interval()
                .unwrap();
            assert!((a.0 - (1.0 - b.1)).abs() < 1e-12 && (a.1 - (1.0 - b.0)).abs() < 1e-12);
        }
    }
}

#[test]
fn timeouts_never_create_an_interval_or_move_one() {
    for t in [0u32, 1, 9, 1000] {
        let c = Cell { half_wins: 0, n_decided: 0, n_timeout: t };
        assert_eq!(c.rate(), None, "{t} timeouts: no rate, never 0.5");
        assert_eq!(c.wilson_interval(), None, "{t} timeouts: no interval");
    }
    let base = Cell { half_wins: 9, n_decided: 6, n_timeout: 0 }.wilson_interval();
    for t in [1u32, 50, 10_000] {
        let c = Cell { half_wins: 9, n_decided: 6, n_timeout: t };
        assert_eq!(c.wilson_interval(), base, "timeouts are not sample");
    }
}

// ---- 2. Verdict boundaries, through the public report ------------------------

fn rec(a: &str, b: &str, result: MatchResult) -> MatchRecord {
    MatchRecord {
        strategies: [a.to_string(), b.to_string()],
        seed: 0,
        result,
        orientation: Orientation::Normal,
        ticks: 1,
        produced: ProductionCounts::default(),
    }
}

/// `half_wins` for `mass_sentinel` over `mass_ripper` out of `n` decided (as
/// wins, at most one mutual loss, and losses), plus `timeouts` capped matches.
/// Half the wins are recorded from slot B so slot cannot matter.
fn sentinel_vs_ripper(half_wins: u32, n: u32, timeouts: u32) -> Vec<MatchRecord> {
    let wins = half_wins / 2;
    let draws = half_wins % 2;
    let losses = n - wins - draws;
    let mut out = Vec::new();
    for i in 0..wins {
        if i % 2 == 0 {
            out.push(rec("mass_sentinel", "mass_ripper", MatchResult::Decided(Faction::A)));
        } else {
            out.push(rec("mass_ripper", "mass_sentinel", MatchResult::Decided(Faction::B)));
        }
    }
    for _ in 0..draws {
        out.push(rec("mass_sentinel", "mass_ripper", MatchResult::MutualLoss));
    }
    for _ in 0..losses {
        out.push(rec("mass_ripper", "mass_sentinel", MatchResult::Decided(Faction::A)));
    }
    for _ in 0..timeouts {
        out.push(rec("mass_sentinel", "mass_ripper", MatchResult::Timeout));
    }
    out
}

fn sentinel_link(content: &Content, records: &[MatchRecord]) -> onus::pentagon::Link {
    let report = PentagonReport::of(content, &WinMatrix::of(records)).expect("cycle");
    report
        .links()
        .iter()
        .find(|l| l.predator == "sentinel")
        .expect("sentinel > ripper is a shipped link")
        .clone()
}

#[test]
fn verdict_is_exactly_the_oracle_interval_classification_for_every_cell_up_to_n_60() {
    let content = headless::content().expect("content");
    for n in 1u32..=60 {
        let mut first_hold = None;
        let mut last_fail = None;
        for h in 0..=2 * n {
            let p = h as f64 / (2.0 * n as f64);
            let (lo, hi) = oracle_quadratic(p, n as f64);
            assert!((lo - 0.5).abs() > 1e-7 && (hi - 0.5).abs() > 1e-7, "too close to call");
            let want = if lo > 0.5 {
                Verdict::Holds
            } else if hi < 0.5 {
                Verdict::Fails
            } else {
                Verdict::Undetermined
            };
            for timeouts in [0u32, 5] {
                let l = sentinel_link(&content, &sentinel_vs_ripper(h, n, timeouts));
                assert_eq!(l.n_decided, n);
                assert_eq!(l.n_timeout, timeouts);
                assert_eq!(l.rate, Some(p));
                assert_eq!(l.verdict, want, "{h}/2 of {n} (+{timeouts} timeouts), [{lo}, {hi}]");
                // A verdict is never stronger than its interval allows.
                let (ilo, ihi) = l.interval.expect("decided");
                match l.verdict {
                    Verdict::Holds => assert!(ilo > 0.5 && p > 0.5),
                    Verdict::Fails => assert!(ihi < 0.5 && p < 0.5),
                    Verdict::Undetermined => assert!(ilo <= 0.5 && ihi >= 0.5),
                    v => panic!("{v:?} on a decided cell"),
                }
            }
            if want == Verdict::Holds && first_hold.is_none() {
                first_hold = Some(h);
            }
            if want == Verdict::Fails {
                last_fail = Some(h);
            }
        }
        // n = 1 .. 3 can never resolve anything: a sweep is still a coin flip
        // (3/3 is [43.9, 100]). From n = 4 a clean sweep resolves (4/4 is
        // [51.0, 100]) — Wilson, not a rule of thumb.
        if n <= 3 {
            assert_eq!(first_hold, None, "n={n}: a hold from {n} matches");
            assert_eq!(last_fail, None, "n={n}: a fail from {n} matches");
        }
        if n == 4 {
            assert_eq!(first_hold, Some(8), "4/4 holds");
            assert_eq!(last_fail, Some(0), "0/4 fails");
        }
        if n == 8 {
            // F-034's n = 8: 7/8 holds, 6/8 does not.
            assert_eq!(first_hold, Some(14), "7/8 is the first hold at n = 8");
        }
        // Exactly half is never resolved, at any n.
        let l = sentinel_link(&content, &sentinel_vs_ripper(n, n, 0));
        assert_eq!(l.rate, Some(0.5));
        assert_eq!(l.verdict, Verdict::Undetermined, "exactly half at n={n}");
    }
}

#[test]
fn zero_decided_is_undefined_never_half_and_n_one_is_undetermined() {
    let content = headless::content().expect("content");
    // Nothing at all, and nothing but timeouts.
    for t in [0u32, 1, 12] {
        let l = sentinel_link(&content, &sentinel_vs_ripper(0, 0, t));
        assert_eq!(l.verdict, Verdict::Undefined, "{t} timeouts");
        assert_eq!(l.rate, None, "never 0.5");
        assert_eq!(l.interval, None);
        assert_eq!(l.n_decided, 0);
        assert_eq!(l.n_timeout, t);
    }
    // One decided match, each way, and one mutual loss.
    for (h, rate) in [(2u32, 1.0), (0, 0.0), (1, 0.5)] {
        let l = sentinel_link(&content, &sentinel_vs_ripper(h, 1, 3));
        assert_eq!(l.rate, Some(rate));
        assert_eq!(l.verdict, Verdict::Undetermined, "one match is no verdict ({rate})");
    }
    // An all-undefined report is not balanced.
    let report = PentagonReport::of(&content, &WinMatrix::of(&[])).expect("cycle");
    assert_eq!(report.undefined(), 5);
    assert_eq!(report.undetermined(), 0);
    assert!(!report.all_hold());
}

// ---- 3. F-035: the trimmed openings were dead --------------------------------

/// The shipped content with every opening F-035 removed put back, and
/// `synth_triad` back in its pre-trim order. Everything else (turtle at 26
/// included) is the shipped content, so any difference is the trim's.
fn untrimmed(shipped: &Content) -> Content {
    let mut c = shipped.clone();
    let o = |b: &str, at: u32, off: f32| BarracksOpening {
        building: b.to_string(),
        at_tick: at,
        offset: off,
    };
    for s in &mut c.strategies {
        let restore: Vec<BarracksOpening> = match s.id.as_str() {
            "mass_bulwark" | "mass_sentinel" => {
                vec![o("foundry", 600, 165.0), o("foundry", 900, 200.0)]
            }
            "mass_ravager" => vec![o("gene_vats", 600, 165.0), o("gene_vats", 900, 200.0)],
            "mass_arclight" => {
                vec![o("aether_spire", 600, 165.0), o("aether_spire", 900, 200.0)]
            }
            "mass_ripper" => vec![o("gene_vats", 900, 200.0)],
            "synth_steel_flesh" => vec![o("foundry", 1200, 195.0), o("gene_vats", 1500, 225.0)],
            "turtle" => vec![o("gene_vats", 2400, 210.0)],
            _ => vec![],
        };
        s.barracks.extend(restore);
        if s.id == "synth_triad" {
            // shipped: foundry, gv(900), gv(1800), spire(1500) -> old order.
            assert_eq!(s.barracks.len(), 4);
            assert_eq!(s.barracks[3].building, "aether_spire");
            s.barracks.swap(2, 3);
        }
    }
    if let Some(d) = c.strategies.iter().find(|s| s.id == c.default_strategy) {
        c.ai = d.clone();
    }
    c
}

#[derive(Debug, PartialEq)]
struct Played {
    ticks: u32,
    outcome: Option<Option<Faction>>,
    hash: u64,
    journal: [Vec<(u32, AiAction)>; 2],
}

fn play(content: &Content, a: &str, b: &str, seed: u64, o: Orientation) -> Played {
    let settings = MatchSettings::default()
        .with_seed(seed)
        .with_strategies(a, b)
        .with_orientation(o);
    let mut app = headless::ai_vs_ai(content.clone(), &settings).expect("known strategies");
    let mut ticks = 0;
    let mut outcome = None;
    while ticks < DEFAULT_TICK_CAP {
        headless::step(&mut app);
        ticks += 1;
        if let Some(out) = app.world().resource::<MatchState>().outcome() {
            outcome = Some(out.winner);
            break;
        }
    }
    let journal = {
        let j = app.world().resource::<AiJournal>();
        [j.for_faction(Faction::A), j.for_faction(Faction::B)]
    };
    Played {
        ticks,
        outcome,
        hash: state_hash(app.world_mut()),
        journal,
    }
}

fn placements(p: &Played, f: usize) -> usize {
    p.journal[f]
        .iter()
        .filter(|(_, a)| matches!(a, AiAction::PlaceBarracks { .. }))
        .count()
}

/// Run every ordered pairing of `roster` on `seeds`, both orientations, under
/// the shipped and the untrimmed content, three worker threads. Returns the
/// list of matches that differ.
fn differential(roster: &[String], seeds: &[u64]) -> Vec<String> {
    let shipped = headless::content().expect("content");
    let old = untrimmed(&shipped);
    let mut jobs = Vec::new();
    for &seed in seeds {
        for a in roster {
            for b in roster {
                for o in [Orientation::Normal, Orientation::Swapped] {
                    jobs.push((a.clone(), b.clone(), seed, o));
                }
            }
        }
    }
    let jobs = Mutex::new(jobs);
    let diffs = Mutex::new(Vec::new());
    let threads: usize = std::env::var("CRITIC_THREADS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3);
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| loop {
                let Some((a, b, seed, o)) = jobs.lock().unwrap().pop() else {
                    break;
                };
                let new = play(&shipped, &a, &b, seed, o);
                let was = play(&old, &a, &b, seed, o);
                if new != was {
                    diffs.lock().unwrap().push(format!(
                        "{a} vs {b} seed {seed:#x} {o:?}: trimmed ends {} {:?} with {}+{} \
                         placements; untrimmed ends {} {:?} with {}+{} placements",
                        new.ticks,
                        new.outcome,
                        placements(&new, 0),
                        placements(&new, 1),
                        was.ticks,
                        was.outcome,
                        placements(&was, 0),
                        placements(&was, 1),
                    ));
                }
            });
        }
    });
    let mut d = diffs.into_inner().unwrap();
    d.sort();
    d
}

fn seeds(default: &[u64]) -> Vec<u64> {
    match std::env::var("CRITIC_SEEDS").ok().and_then(|s| s.parse::<u32>().ok()) {
        Some(k) => (0..k).map(|i| seed_at(0x0C21_7B35, i)).collect(),
        None => default.to_vec(),
    }
}

/// F-035: "every opening that never goes up is removed" — so restoring them
/// must not change a single match. Whole roster, every ordered pairing, both
/// orientations, on seeds the closure did not use.
///
/// Ignored by default for cost (two seeds is 800 full matches, ~15 min in
/// release). Run it with
/// `cargo test --release --test critic_b35_closure -- --ignored`, and widen it
/// with `CRITIC_SEEDS=<k>` (the B3.5 closure review ran k = 5: 2 000 matches,
/// 1 000 pairs, zero differences).
#[test]
#[ignore = "800 full matches; run explicitly in release"]
fn restoring_the_trimmed_openings_changes_no_match_of_the_whole_roster() {
    let shipped = headless::content().expect("content");
    let roster: Vec<String> = shipped.strategies.iter().map(|s| s.id.clone()).collect();
    let seeds = seeds(&[seed_at(0x0C21_7B35, 0), seed_at(0x0C21_7B35, 1)]);
    let diffs = differential(&roster, &seeds);
    assert!(
        diffs.is_empty(),
        "{} matches differ once the 'dead' openings are restored:\n{}",
        diffs.len(),
        diffs.join("\n")
    );
}

// ---- 4. F-036: the turtle is the latest committer on every seed ---------------

/// Slot A's first attack in an `id` mirror. Both sides run the same script on
/// a mirrored map, so nothing interacts before the first wave is issued and the
/// commit tick is the one the solo fixture measures.
fn first_attack_mirror(content: &Content, id: &str, seed: u64) -> Option<(u32, u32)> {
    let settings = MatchSettings::default()
        .with_seed(seed)
        .with_strategies(id, id);
    let mut app = headless::ai_vs_ai(content.clone(), &settings).expect("known");
    for _ in 0..DEFAULT_TICK_CAP {
        headless::step(&mut app);
        let j = app.world().resource::<AiJournal>();
        if let Some(hit) = j.for_faction(Faction::A).into_iter().find_map(|(t, a)| match a {
            AiAction::Attack { force, .. } => Some((t, force)),
            _ => None,
        }) {
            return Some(hit);
        }
        if app.world().resource::<MatchState>().is_over() {
            return None;
        }
    }
    None
}

#[test]
fn the_turtle_commits_last_on_every_seed_with_the_claimed_margin() {
    let c = headless::content().expect("content");
    let ids: Vec<String> = c.strategies.iter().map(|s| s.id.clone()).collect();
    for k in 0..3u32 {
        let seed = seed_at(0x0070_A71E, k);
        let mut commits: Vec<(String, u32)> = Vec::new();
        for id in &ids {
            let (at, _) = first_attack_mirror(&c, id, seed)
                .unwrap_or_else(|| panic!("`{id}` never attacked on seed {seed:#x}"));
            commits.push((id.clone(), at));
        }
        let turtle = commits.iter().find(|(id, _)| id == "turtle").unwrap().1;
        let latest_other = commits
            .iter()
            .filter(|(id, _)| id != "turtle")
            .max_by_key(|(_, t)| *t)
            .unwrap();
        assert!(
            turtle > latest_other.1,
            "seed {seed:#x}: turtle commits at {turtle}, `{}` at {} — {commits:?}",
            latest_other.0,
            latest_other.1
        );
        // F-036: 19 320 vs 18 750 — a 570-tick margin, seed-independent.
        assert_eq!(
            (turtle, latest_other.1),
            (19_320, 18_750),
            "seed {seed:#x}: F-036's measured ticks are not seed-independent: {commits:?}"
        );
    }
}
