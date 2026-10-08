//! L2 integration tests for **B3 AC2** — the pentagon assertion.
//!
//! The designed cycle is `Sentinel > Ripper > Arclight > Bulwark > Ravager >
//! Sentinel`. [`onus::pentagon`] does not know that: it **derives** the cycle
//! by walking each unit's `nemesis` link in `units.ron`, maps each unit to the
//! strategy that masses it *by that strategy's build order*, and reads the
//! matching [`WinMatrix`] cell. What is encoded here:
//!
//!   - **the cycle comes from the data** — a content whose `nemesis` links form
//!     a different cycle yields that cycle, not the shipped one. A pentagon
//!     hardcoded in Rust would keep passing after someone edited the RON, which
//!     is the one failure that makes the whole assertion worthless;
//!   - **the shipped content yields exactly the designed cycle** — five links,
//!     each unit once as predator and once as prey, closed;
//!   - **a malformed cycle is reported, not panicked on** — an open chain, a
//!     self-nemesis, a cycle that leaves nemesis-bearing units out;
//!   - **hold / fail / undetermined / undefined are four states, not a bool**,
//!     and the verdict is read off the rate's **95% interval**, not the rate:
//!     65 of 100 holds, 35 of 100 fails, exactly 0.5 is undetermined, so is
//!     75% of 4 decided matches (the same 75% holds over 40), and no decided
//!     match at all is undefined. A coin flip is not a broken design (F-031);
//!   - **direction** — a link's rate is the predator's win rate over the prey,
//!     never the transpose;
//!   - **strategy mapping** is by build order, and a unit with no mass strategy
//!     is a reported gap, not a panic;
//!   - **the real batch** on the five mass probes reports what the sim actually
//!     does — three links holding today and two undetermined at 8 decided
//!     matches a link (F-034); none fails. This test asserts the machinery, and
//!     records the readings as the measurements they are: tuning is B4's job.
//!
//! Everything but the last test runs on synthetic records, so every expected
//! value is exact.

use onus::batch::{self, BatchSettings, MatchRecord, MatchResult, ProductionCounts};
use onus::headless::{self, Orientation};
use onus::metrics::WinMatrix;
use onus::pentagon::{CycleError, PentagonReport, Verdict};
use onus::sim::content::Content;
use onus::sim::spatial::Faction;

const A_WINS: MatchResult = MatchResult::Decided(Faction::A);
const CAPPED: MatchResult = MatchResult::Timeout;

/// The designed cycle, as DESIGN_BRIEF states it. Written here, in the test,
/// on purpose: the library must derive it from content, so the expectation has
/// to live somewhere the library cannot read.
const DESIGNED: [(&str, &str); 5] = [
    ("sentinel", "ripper"),
    ("ripper", "arclight"),
    ("arclight", "bulwark"),
    ("bulwark", "ravager"),
    ("ravager", "sentinel"),
];

fn shipped() -> Content {
    headless::content().expect("assets/data/*.ron parse into sim structs")
}

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

/// `wins` wins for `a` over `b` and `losses` the other way, all in slot A so
/// the fixture stays readable; plus `timeouts` capped matches.
fn series(a: &str, b: &str, wins: u32, losses: u32, timeouts: u32) -> Vec<MatchRecord> {
    let mut out = Vec::new();
    out.extend((0..wins).map(|_| rec(a, b, A_WINS)));
    out.extend((0..losses).map(|_| rec(b, a, A_WINS)));
    out.extend((0..timeouts).map(|_| rec(a, b, CAPPED)));
    out
}

/// Set `unit`'s nemesis in a loaded content — the fixture knob. `Content`'s
/// fields are public data; nothing here re-validates, because the pentagon
/// module is a pure function of whatever content it is handed.
fn set_nemesis(content: &mut Content, unit: &str, prey: Option<&str>) {
    let u = content
        .units
        .iter_mut()
        .find(|u| u.id == unit)
        .unwrap_or_else(|| panic!("`{unit}` is a shipped unit"));
    u.nemesis = prey.map(str::to_string);
}

fn links_of(report: &PentagonReport) -> Vec<(String, String)> {
    report
        .links()
        .iter()
        .map(|l| (l.predator.clone(), l.prey.clone()))
        .collect()
}

fn link<'r>(report: &'r PentagonReport, predator: &str) -> &'r onus::pentagon::Link {
    report
        .links()
        .iter()
        .find(|l| l.predator == predator)
        .unwrap_or_else(|| panic!("`{predator}` is a predator of the cycle"))
}

// ---- the cycle comes from the data ------------------------------------------

#[test]
fn the_shipped_content_yields_the_designed_cycle() {
    let content = shipped();
    let cycle = onus::pentagon::nemesis_cycle(&content).expect("the shipped cycle is closed");
    assert_eq!(cycle.len(), 5, "five combat units in the pentagon");

    let report = PentagonReport::of(&content, &WinMatrix::default()).expect("closed cycle");
    let mut got = links_of(&report);
    let mut want: Vec<(String, String)> = DESIGNED
        .iter()
        .map(|(p, q)| (p.to_string(), q.to_string()))
        .collect();
    got.sort();
    want.sort();
    assert_eq!(got, want, "the shipped links are the designed pentagon");

    // Closed, and every unit exactly once in each role.
    let order = links_of(&report);
    for w in order.windows(2) {
        assert_eq!(w[0].1, w[1].0, "each link's prey is the next link's predator");
    }
    assert_eq!(
        order.last().unwrap().1,
        order.first().unwrap().0,
        "the cycle closes"
    );
    let mut predators: Vec<&String> = order.iter().map(|(p, _)| p).collect();
    let mut prey: Vec<&String> = order.iter().map(|(_, q)| q).collect();
    predators.sort();
    prey.sort();
    assert_eq!(predators, prey, "each unit once as predator and once as prey");
    predators.dedup();
    assert_eq!(predators.len(), 5, "no unit repeats");
}

#[test]
fn the_cycle_follows_the_data_not_the_shipped_roster() {
    // Reverse every link: bulwark > arclight > ripper > sentinel > ravager >
    // bulwark. A hardcoded pentagon would still report the shipped one.
    let mut content = shipped();
    for (predator, prey) in DESIGNED {
        set_nemesis(&mut content, prey, Some(predator));
    }
    let report = PentagonReport::of(&content, &WinMatrix::default()).expect("still closed");
    let got: Vec<(String, String)> = links_of(&report);
    for (predator, prey) in DESIGNED {
        assert!(
            got.iter().any(|(p, q)| p == prey && q == predator),
            "the reversed link {prey} > {predator} must be derived"
        );
        assert!(
            !got.iter().any(|(p, q)| p == predator && q == prey),
            "the shipped link {predator} > {prey} must be gone"
        );
    }
}

#[test]
fn the_link_order_is_canonical_and_deterministic() {
    let content = shipped();
    // The walk starts at the first nemesis-bearing unit in RON order.
    let first = content
        .units
        .iter()
        .find(|u| u.nemesis.is_some())
        .expect("the shipped roster has nemesis links");
    let report = PentagonReport::of(&content, &WinMatrix::default()).expect("closed");
    assert_eq!(report.links()[0].predator, first.id);
    let again = PentagonReport::of(&content, &WinMatrix::default()).expect("closed");
    assert_eq!(links_of(&report), links_of(&again));
}

// ---- a malformed cycle is reported, not panicked on --------------------------

#[test]
fn an_open_chain_is_reported() {
    let mut content = shipped();
    set_nemesis(&mut content, "ravager", None);
    let err = PentagonReport::of(&content, &WinMatrix::default()).expect_err("no longer closed");
    assert_eq!(err, CycleError::Open { at: "ravager".to_string() });
    assert!(err.to_string().contains("ravager"), "{err}");
}

#[test]
fn a_self_nemesis_is_reported() {
    let mut content = shipped();
    set_nemesis(&mut content, "ripper", Some("ripper"));
    let err = PentagonReport::of(&content, &WinMatrix::default()).expect_err("degenerate");
    assert_eq!(err, CycleError::SelfNemesis("ripper".to_string()));
}

#[test]
fn a_short_cycle_leaving_units_out_is_reported() {
    // bulwark <-> ravager becomes a 2-cycle; sentinel/ripper/arclight still
    // name prey but are no longer reachable from it.
    let mut content = shipped();
    set_nemesis(&mut content, "ravager", Some("bulwark"));
    let err = PentagonReport::of(&content, &WinMatrix::default()).expect_err("partial");
    match err {
        CycleError::Partial { cycle, missing } => {
            assert_eq!(cycle, vec!["bulwark".to_string(), "ravager".to_string()]);
            assert_eq!(
                missing,
                vec!["sentinel".to_string(), "ripper".to_string(), "arclight".to_string()],
                "the units left out, in RON order"
            );
        }
        other => panic!("expected a partial cycle, got {other:?}"),
    }
}

#[test]
fn a_lasso_is_reported() {
    // sentinel's prey becomes ravager: the walk bulwark > ravager > sentinel
    // re-enters at ravager, which is not the start.
    let mut content = shipped();
    set_nemesis(&mut content, "sentinel", Some("ravager"));
    let err = PentagonReport::of(&content, &WinMatrix::default()).expect_err("not a cycle");
    assert_eq!(err, CycleError::Lasso { at: "ravager".to_string() });
}

#[test]
fn an_unknown_prey_is_reported() {
    let mut content = shipped();
    set_nemesis(&mut content, "bulwark", Some("chimera"));
    let err = PentagonReport::of(&content, &WinMatrix::default()).expect_err("dangling");
    assert_eq!(
        err,
        CycleError::UnknownPrey {
            unit: "bulwark".to_string(),
            prey: "chimera".to_string()
        }
    );
}

#[test]
fn a_roster_with_no_nemesis_at_all_is_reported() {
    let mut content = shipped();
    for u in &mut content.units {
        u.nemesis = None;
    }
    let err = PentagonReport::of(&content, &WinMatrix::default()).expect_err("no cycle");
    assert_eq!(err, CycleError::NoNemesis);
}

// ---- hold / fail / undetermined / undefined ----------------------------------

/// Records for every shipped link at the given rate-defining (wins, losses,
/// timeouts), using the mass strategies.
fn pentagon_records(per_link: &[(u32, u32, u32); 5]) -> Vec<MatchRecord> {
    let mut out = Vec::new();
    for ((predator, prey), &(w, l, t)) in DESIGNED.iter().zip(per_link) {
        out.extend(series(
            &format!("mass_{predator}"),
            &format!("mass_{prey}"),
            w,
            l,
            t,
        ));
    }
    out
}

/// The four readable states, on one batch: a link holds only when its whole
/// interval clears half, fails only when the whole interval is under it, and
/// otherwise says so.
#[test]
fn holds_only_when_the_whole_interval_clears_half() {
    let content = shipped();
    // sentinel 65/100 (holds), ripper 50/100 (undetermined, exactly 0.5),
    // arclight 35/100 (fails), bulwark all-timeout (undefined),
    // ravager 100/100 of 7 (holds — 7 of 7 is [64.6, 100]).
    let records = pentagon_records(&[(65, 35, 0), (50, 50, 0), (35, 65, 0), (0, 0, 4), (7, 0, 0)]);
    let report =
        PentagonReport::of(&content, &WinMatrix::of(&records)).expect("the shipped cycle");

    assert_eq!(link(&report, "sentinel").verdict, Verdict::Holds);
    assert_eq!(link(&report, "sentinel").rate, Some(0.65));
    assert_eq!(link(&report, "ripper").verdict, Verdict::Undetermined);
    assert_eq!(link(&report, "ripper").rate, Some(0.5));
    assert_eq!(link(&report, "arclight").verdict, Verdict::Fails);
    assert_eq!(link(&report, "arclight").rate, Some(0.35));
    assert_eq!(link(&report, "bulwark").verdict, Verdict::Undefined);
    assert_eq!(link(&report, "bulwark").rate, None, "never 0.5");
    assert_eq!(link(&report, "bulwark").interval, None, "no rate, no interval");
    assert_eq!(link(&report, "bulwark").n_decided, 0);
    assert_eq!(link(&report, "bulwark").n_timeout, 4);
    assert_eq!(link(&report, "ravager").verdict, Verdict::Holds);
    assert_eq!(link(&report, "ravager").rate, Some(1.0));
    assert_eq!(link(&report, "ravager").n_decided, 7);

    assert_eq!(report.holding(), 2);
    assert_eq!(report.failing(), 1);
    assert_eq!(report.undetermined(), 1);
    assert_eq!(report.undefined(), 1);
    assert_eq!(report.gaps(), 0);
    assert_eq!(report.links().len(), 5);
    assert!(!report.all_hold());
}

/// **The sample size is part of the verdict.** The same 75% that holds over 40
/// decided matches is `Undetermined` over 4 — and `Undetermined` is neither a
/// hold nor a failure. This is F-031's lesson in one assertion: a bare
/// `rate > 0.5` recorded an 8-match coin flip as a broken design, twice
/// (F-025's and F-030's `sentinel > ripper`, which reproduces at 64-72% once
/// the sample is 100).
#[test]
fn the_same_rate_on_too_few_matches_is_undetermined_not_a_verdict() {
    let content = shipped();
    for (wins, losses) in [(3u32, 1u32), (30, 10)] {
        let records = pentagon_records(&[(wins, losses, 0); 5]);
        let report = PentagonReport::of(&content, &WinMatrix::of(&records)).expect("cycle");
        let n = wins + losses;
        for l in report.links() {
            assert_eq!(l.rate, Some(0.75), "the rate is the same at n={n}");
            assert_eq!(l.n_decided, n);
        }
        if n == 4 {
            assert_eq!(report.undetermined(), 5, "75% of 4 is [30.1, 95.4]");
            assert_eq!(report.holding(), 0);
            assert_eq!(report.failing(), 0, "undetermined is not a failed design");
            assert_eq!(report.undefined(), 0, "undetermined is not undefined");
            assert!(!report.all_hold(), "a coin-flip-wide reading is not a pentagon");
        } else {
            assert_eq!(report.holding(), 5, "75% of 40 is [59.8, 85.8]");
            assert_eq!(report.undetermined(), 0);
            assert!(report.all_hold());
        }
    }
}

#[test]
fn every_link_holding_is_the_whole_pentagon() {
    let content = shipped();
    let records = pentagon_records(&[(30, 10, 0); 5]);
    let report = PentagonReport::of(&content, &WinMatrix::of(&records)).expect("cycle");
    assert_eq!(report.holding(), 5);
    assert!(report.all_hold());
    for l in report.links() {
        assert_eq!(l.verdict, Verdict::Holds);
        assert_eq!(l.rate, Some(0.75));
    }
}

#[test]
fn an_empty_batch_is_five_undefined_links_not_a_balanced_pentagon() {
    let content = shipped();
    let report = PentagonReport::of(&content, &WinMatrix::default()).expect("cycle");
    assert_eq!(report.links().len(), 5);
    assert_eq!(report.undefined(), 5);
    assert_eq!(report.holding(), 0);
    assert!(!report.all_hold(), "nothing decided is not a pentagon that holds");
    for l in report.links() {
        assert_eq!(l.rate, None);
    }
}

#[test]
fn an_all_timeout_batch_is_undefined_not_balanced() {
    let content = shipped();
    let records = pentagon_records(&[(0, 0, 6); 5]);
    let report = PentagonReport::of(&content, &WinMatrix::of(&records)).expect("cycle");
    assert_eq!(report.undefined(), 5);
    assert!(!report.all_hold());
    for l in report.links() {
        assert_eq!(l.rate, None);
        assert_eq!(l.n_timeout, 6);
    }
}

// ---- direction ---------------------------------------------------------------

#[test]
fn a_links_rate_is_the_predators_not_the_preys() {
    let content = shipped();
    // sentinel beats ripper 30 of 40: transposed this would be 0.25 and fail.
    // (40, not 4, so the interval resolves either way — the direction is the
    // subject here, not the sample size.)
    let records = series("mass_sentinel", "mass_ripper", 30, 10, 0);
    let report = PentagonReport::of(&content, &WinMatrix::of(&records)).expect("cycle");
    let l = link(&report, "sentinel");
    assert_eq!(l.predator, "sentinel");
    assert_eq!(l.prey, "ripper");
    assert_eq!(l.predator_strategy.as_deref(), Some("mass_sentinel"));
    assert_eq!(l.prey_strategy.as_deref(), Some("mass_ripper"));
    assert_eq!(l.rate, Some(0.75), "the predator's rate, not 0.25");
    assert_eq!(l.verdict, Verdict::Holds);
    assert_eq!(l.n_decided, 40);

    // The transpose is the losing reading, and it is *not* what a link reports.
    let m = WinMatrix::of(&records);
    assert_eq!(m.get("mass_ripper", "mass_sentinel").unwrap().rate(), Some(0.25));
}

#[test]
fn reversing_the_data_reverses_which_side_must_win() {
    // Same records; the content now says ripper's prey is sentinel. The
    // verdict must flip, because the predicted counter flipped.
    let mut content = shipped();
    for (predator, prey) in DESIGNED {
        set_nemesis(&mut content, prey, Some(predator));
    }
    let records = series("mass_sentinel", "mass_ripper", 30, 10, 0);
    let report = PentagonReport::of(&content, &WinMatrix::of(&records)).expect("cycle");
    let l = link(&report, "ripper");
    assert_eq!(l.prey, "sentinel");
    assert_eq!(l.rate, Some(0.25));
    assert_eq!(l.verdict, Verdict::Fails);
}

// ---- strategy mapping --------------------------------------------------------

#[test]
fn a_mass_strategy_is_found_by_its_build_order_not_its_name() {
    let content = shipped();
    for (predator, prey) in DESIGNED {
        for unit in [predator, prey] {
            let s = onus::pentagon::mass_strategy(&content, unit)
                .unwrap_or_else(|| panic!("a strategy masses `{unit}`"));
            assert!(
                s.army.iter().all(|i| i.unit == unit) && !s.army.is_empty(),
                "`{}` masses only `{unit}`",
                s.id
            );
        }
    }
    // Rename every strategy: the mapping must survive, because it reads the
    // build order.
    let mut renamed = shipped();
    for (i, s) in renamed.strategies.iter_mut().enumerate() {
        s.id = format!("s{i}");
    }
    let s = onus::pentagon::mass_strategy(&renamed, "bulwark").expect("still found");
    assert!(s.id.starts_with('s'), "renamed: {}", s.id);
    assert!(s.army.iter().all(|i| i.unit == "bulwark"));

    // A mixed build is not a mass strategy for any of its units.
    let mvp = content.strategy("mvp").expect("shipped");
    assert!(mvp.army.len() > 1, "the mvp opener is mixed");
    for item in &mvp.army {
        let found = onus::pentagon::mass_strategy(&content, &item.unit).expect("a mass probe");
        assert_ne!(found.id, mvp.id);
    }
}

#[test]
fn a_cycle_unit_with_no_mass_strategy_is_a_reported_gap() {
    let mut content = shipped();
    content.strategies.retain(|s| s.id != "mass_bulwark");
    let records = pentagon_records(&[(30, 10, 0); 5]);
    let report = PentagonReport::of(&content, &WinMatrix::of(&records)).expect("cycle");

    // Both links that touch bulwark lose a side.
    let arclight = link(&report, "arclight"); // prey = bulwark
    assert_eq!(arclight.prey_strategy, None);
    assert_eq!(arclight.verdict, Verdict::NoStrategy);
    assert_eq!(arclight.rate, None);
    let bulwark = link(&report, "bulwark"); // predator = bulwark
    assert_eq!(bulwark.predator_strategy, None);
    assert_eq!(bulwark.verdict, Verdict::NoStrategy);

    assert_eq!(report.gaps(), 2);
    assert_eq!(report.holding(), 3);
    assert!(!report.all_hold());
    assert_eq!(
        report.missing_strategies(),
        vec!["bulwark".to_string()],
        "the gap is named by the unit that has no mass probe"
    );
}

// ---- a real batch ------------------------------------------------------------

/// The five mass probes, on **two seeds** — 5 x 5 x 2 seeds x 2 orientations =
/// 100 matches, which is the smallest run that gives every pentagon link a
/// sample in both slots and both orientations while keeping the suite quick.
#[test]
fn the_real_batch_reports_what_the_sim_actually_does() {
    let content = shipped();
    let probes: Vec<String> = DESIGNED
        .iter()
        .map(|(p, _)| format!("mass_{p}"))
        .collect();
    let settings = BatchSettings::default()
        .with_only(probes.clone())
        .with_seeds(2);
    let records = batch::run_batch(&content, &settings, &mut |_| {}).expect("shipped names");
    let matrix = WinMatrix::of(&records);
    let report = PentagonReport::of(&content, &matrix).expect("the shipped cycle is closed");

    assert_eq!(report.links().len(), 5);
    assert_eq!(report.gaps(), 0, "every pentagon unit has a mass probe");
    for l in report.links() {
        assert_eq!(
            l.n_decided + l.n_timeout,
            8,
            "{} vs {}: two slot orderings x two orientations x two seeds",
            l.predator,
            l.prey
        );
    }

    // The measurement as of B4 (F-041), and what eight matches can honestly
    // say about it. F-025's reading — four links holding and `bulwark >
    // ravager` losing outright at 0.0% — is gone: the armour re-tune (F-031)
    // turned that link into the pentagon's strongest resolved hold. B3.5 pinned
    // three holds here with each mass probe standing one line (F-035); B4's
    // opening reservation stands all three scripted lines, the matches run
    // faster and four links drop to `Undetermined` — `ravager > sentinel` is now
    // 2 of 8, under a half but not resolvably so. Eight decided matches put
    // ~±28 points of 95% interval around every rate, so a link only reads
    // `Holds` or `Fails` here when it is lopsided enough to clear 50% with that
    // interval; otherwise it reads `Undetermined`, which is a statement about the
    // sample, not the design. The B4 baseline batch (F-042) is the design
    // reading; this test pins what *this* batch does, link by link, so a
    // behavioural regression still shows.
    //
    // B4's tuned content (g1, F-046) moves it again: `arclight > bulwark` and
    // `ripper > arclight` resolve as holds, the other three stay undetermined,
    // and nothing fails. (F-041: one hold, four undetermined.)
    assert_eq!(report.failing(), 0, "no link reads as broken design:\n{report}");
    assert_eq!(report.holding(), 2, "two links resolve as holds:\n{report}");
    assert_eq!(report.undetermined(), 3, "three links the sample cannot call:\n{report}");
    assert_eq!(report.undefined(), 0, "every link has decided matches");

    // Link by link: the rate, and whether eight matches can resolve it.
    #[rustfmt::skip]
    let expected: &[(&str, f64, Verdict)] = &[
        ("bulwark",  0.750,     Verdict::Undetermined),  // > ravager  — 0.875 holds at F-041; 0.0% at B3
        ("ravager",  0.500,     Verdict::Undetermined),  // > sentinel — 0.250 at F-041; 0.625 at B3.5
        ("sentinel", 0.375,     Verdict::Undetermined),  // > ripper   — 0.625 at F-041; 0.500 at B3.5
        ("ripper",   0.875,     Verdict::Holds),         // > arclight — 0.750 at F-041; 1.000 at B3.5
        ("arclight", 1.000,     Verdict::Holds),         // > bulwark  — 5/6 at F-041 (2 timeouts); 1.000 at B3.5
    ];
    for (predator, rate, verdict) in expected {
        let l = link(&report, predator);
        assert_eq!(
            l.rate,
            Some(*rate),
            "{} > {}: the win rate moved\n{report}",
            l.predator,
            l.prey
        );
        assert_eq!(l.verdict, *verdict, "{} > {}\n{report}", l.predator, l.prey);
        // ...and the verdict really is the interval's doing: an undetermined
        // link is one whose interval straddles a half, a resolved one is not.
        let (lo, hi) = l.interval.expect("a decided link has an interval");
        match verdict {
            Verdict::Undetermined => assert!(
                lo <= 0.5 && hi >= 0.5,
                "{} > {}: called undetermined on an interval [{lo:.3}, {hi:.3}] that does \
                 not straddle a half",
                l.predator,
                l.prey
            ),
            Verdict::Holds => assert!(
                lo > 0.5,
                "{} > {}: called a hold on an interval [{lo:.3}, {hi:.3}] that reaches \
                 below a half",
                l.predator,
                l.prey
            ),
            v => panic!("unexpected expectation {v:?}"),
        }
    }
}
