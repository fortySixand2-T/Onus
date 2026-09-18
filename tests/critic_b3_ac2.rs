//! Critic probes for **B3 AC2** — the pentagon assertion.
//!
//! Every probe here is one that *should* pass per the spec:
//!
//!   - the cycle is derived from `units.ron`'s `nemesis` links, for an
//!     arbitrary valid cycle, not only for the shipped one or its reverse;
//!   - reordering `Content::units` without touching a link cannot change any
//!     link, its direction or its verdict;
//!   - a link's rate is the predator's over the prey, and `of_records` agrees
//!     with `of(WinMatrix::of(..))`;
//!   - the four verdicts are kept apart: exactly 0.5 fails (including a cell
//!     of nothing but decided draws), a hair above 0.5 holds, undecided is
//!     `Undefined`, an unmeasurable link is `NoStrategy`;
//!   - **an all-undefined or all-`NoStrategy` report never reads as balanced**;
//!   - every malformed shape is a distinct reported error, never a panic, and
//!     never a plausible-looking cycle;
//!   - the mapping is by build order: count > 1 masses, mixed and empty armies
//!     do not, an out-of-cycle probe does not displace a real one, and a
//!     duplicate is resolved to first-in-RON deterministically;
//!   - an injected imbalance played for real moves the report.

use onus::batch::{self, BatchSettings, MatchRecord, MatchResult, ProductionCounts};
use onus::headless::{self, Orientation};
use onus::metrics::WinMatrix;
use onus::pentagon::{self, CycleError, PentagonReport, Verdict};
use onus::sim::content::{ArmyItem, Content};
use onus::sim::spatial::Faction;

/// The shipped cycle, in the canonical walk order (`units.ron` lists the
/// Bulwark first among nemesis-bearing units).
const SHIPPED: [(&str, &str); 5] = [
    ("bulwark", "ravager"),
    ("ravager", "sentinel"),
    ("sentinel", "ripper"),
    ("ripper", "arclight"),
    ("arclight", "bulwark"),
];

fn shipped() -> Content {
    headless::content().expect("assets/data/*.ron parse")
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

/// `wins` decided wins for `a` over `b`, `losses` the other way, `draws`
/// mutual losses and `timeouts` capped matches.
fn series(a: &str, b: &str, wins: u32, losses: u32, draws: u32, timeouts: u32) -> Vec<MatchRecord> {
    let mut out = Vec::new();
    out.extend((0..wins).map(|_| rec(a, b, MatchResult::Decided(Faction::A))));
    out.extend((0..losses).map(|_| rec(b, a, MatchResult::Decided(Faction::A))));
    out.extend((0..draws).map(|_| rec(a, b, MatchResult::MutualLoss)));
    out.extend((0..timeouts).map(|_| rec(a, b, MatchResult::Timeout)));
    out
}

fn set_nemesis(content: &mut Content, unit: &str, prey: Option<&str>) {
    content
        .units
        .iter_mut()
        .find(|u| u.id == unit)
        .unwrap_or_else(|| panic!("`{unit}` is a shipped unit"))
        .nemesis = prey.map(str::to_string);
}

fn pairs(report: &PentagonReport) -> Vec<(String, String)> {
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
        .unwrap_or_else(|| panic!("`{predator}` predates something"))
}

// ---- the cycle is data, for an arbitrary cycle -------------------------------

/// Not the shipped cycle and not its reverse: a third permutation. A module
/// that derived "the pentagon" from anything but the links would have to get
/// this one wrong.
#[test]
fn an_arbitrary_valid_cycle_is_derived_link_for_link() {
    let wanted: [(&str, &str); 5] = [
        ("bulwark", "sentinel"),
        ("sentinel", "arclight"),
        ("arclight", "ripper"),
        ("ripper", "ravager"),
        ("ravager", "bulwark"),
    ];
    let mut content = shipped();
    for (predator, prey) in wanted {
        set_nemesis(&mut content, predator, Some(prey));
    }
    let report = PentagonReport::of(&content, &WinMatrix::default()).expect("closed");
    let got = pairs(&report);
    assert_eq!(got.len(), 5);
    for (predator, prey) in wanted {
        assert!(
            got.iter().any(|(p, q)| p == predator && q == prey),
            "link {predator} > {prey} must be derived, got {got:?}"
        );
    }
    for (predator, prey) in SHIPPED {
        assert!(
            !got.iter().any(|(p, q)| p == predator && q == prey),
            "the shipped link {predator} > {prey} must be gone"
        );
    }
}

/// Reordering `Content::units` while leaving every `nemesis` link alone is a
/// no-op on the pentagon: the same five links, the same directions, the same
/// verdicts. (Link *order* may rotate with the canonical start; the reading
/// may not.)
#[test]
fn reordering_units_cannot_change_a_link_or_a_verdict() {
    let content = shipped();
    let records = shipped_records(&[(3, 1, 0, 0); 5]);
    let matrix = WinMatrix::of(&records);
    let base = PentagonReport::of(&content, &matrix).expect("closed");

    let mut shuffled = shipped();
    shuffled.units.reverse();
    let other = PentagonReport::of(&shuffled, &matrix).expect("still closed");

    let mut a = pairs(&base);
    let mut b = pairs(&other);
    a.sort();
    b.sort();
    assert_eq!(a, b, "the same links, whatever order the units are listed in");
    for l in other.links() {
        let same = link(&base, &l.predator);
        assert_eq!(l.prey, same.prey, "direction must not depend on RON order");
        assert_eq!(l.rate, same.rate);
        assert_eq!(l.verdict, same.verdict);
    }
    assert_eq!(other.holding(), base.holding());
}

// ---- direction ---------------------------------------------------------------

#[test]
fn an_asymmetric_fixture_pins_the_direction_and_the_two_constructors_agree() {
    let content = shipped();
    // bulwark > ravager, won 3 of 4 by the *predator*. Transposed it is 0.25
    // and the verdict flips, so this fixture cannot pass a transposed reading.
    let records = series("mass_bulwark", "mass_ravager", 3, 1, 0, 0);
    let report = PentagonReport::of(&content, &WinMatrix::of(&records)).expect("closed");
    let l = link(&report, "bulwark");
    assert_eq!(l.prey, "ravager");
    assert_eq!(l.rate, Some(0.75), "the predator's rate, never the transpose");
    assert_eq!(l.verdict, Verdict::Holds);
    assert_eq!(l.n_decided, 4);

    let from_records = PentagonReport::of_records(&content, &records).expect("closed");
    assert_eq!(from_records, report, "of_records == of(WinMatrix::of(..))");

    // And the same records read against a content that names the opposite
    // predator must fail.
    let mut flipped = shipped();
    for (predator, prey) in SHIPPED {
        set_nemesis(&mut flipped, prey, Some(predator));
    }
    let rev = PentagonReport::of(&flipped, &WinMatrix::of(&records)).expect("closed");
    let l = link(&rev, "ravager");
    assert_eq!(l.prey, "bulwark");
    assert_eq!(l.rate, Some(0.25));
    assert_eq!(l.verdict, Verdict::Fails);
}

// ---- the four verdicts -------------------------------------------------------

/// Records for every shipped link, `(wins, losses, draws, timeouts)` each.
fn shipped_records(per_link: &[(u32, u32, u32, u32); 5]) -> Vec<MatchRecord> {
    let mut out = Vec::new();
    for ((predator, prey), &(w, l, d, t)) in SHIPPED.iter().zip(per_link) {
        out.extend(series(
            &format!("mass_{predator}"),
            &format!("mass_{prey}"),
            w,
            l,
            d,
            t,
        ));
    }
    out
}

#[test]
fn exactly_half_fails_and_a_hair_above_half_holds() {
    let content = shipped();
    let records = shipped_records(&[
        (1024, 1024, 0, 0), // exactly 0.5
        (1025, 1023, 0, 0), // 0.50048828125 — the smallest margin this denominator allows
        (0, 0, 8, 0),       // nothing but decided draws: exactly 0.5, and decided
        (1023, 1025, 0, 0), // just below
        (2, 0, 1, 0),       // 2.5 / 3
    ]);
    let report = PentagonReport::of(&content, &WinMatrix::of(&records)).expect("closed");

    assert_eq!(link(&report, "bulwark").rate, Some(0.5));
    assert_eq!(
        link(&report, "bulwark").verdict,
        Verdict::Fails,
        "a dead-even matchup is not a counter"
    );
    let hair = link(&report, "ravager");
    assert!(hair.rate.unwrap() > 0.5 && hair.rate.unwrap() < 0.501, "{hair:?}");
    assert_eq!(hair.verdict, Verdict::Holds, "strictly above half holds");
    let draws = link(&report, "sentinel");
    assert_eq!(draws.rate, Some(0.5), "eight mutual losses are eight decided draws");
    assert_eq!(draws.n_decided, 8);
    assert_eq!(draws.n_timeout, 0);
    assert_eq!(draws.verdict, Verdict::Fails);
    assert_eq!(link(&report, "ripper").verdict, Verdict::Fails);
    assert_eq!(link(&report, "arclight").rate, Some(2.5 / 3.0));
    assert_eq!(link(&report, "arclight").verdict, Verdict::Holds);

    assert_eq!(report.holding(), 2);
    assert_eq!(report.failing(), 3);
    assert_eq!(report.undefined(), 0);
    assert_eq!(report.gaps(), 0);
    assert!(!report.all_hold());
}

/// The B3 probe, in this checkbox's clothing: nothing decided must never read
/// as a pentagon that holds.
#[test]
fn an_all_undefined_report_never_reads_as_balanced() {
    let content = shipped();
    for records in [
        Vec::new(),                          // nothing played
        shipped_records(&[(0, 0, 0, 5); 5]), // every match hit the cap
    ] {
        let report = PentagonReport::of(&content, &WinMatrix::of(&records)).expect("closed");
        assert_eq!(report.links().len(), 5);
        assert_eq!(report.undefined(), 5);
        assert_eq!(report.holding(), 0);
        assert_eq!(report.failing(), 0, "undecided is not a failure either");
        assert!(!report.all_hold(), "an all-timeout run is not a balanced pentagon");
        for l in report.links() {
            assert_eq!(l.rate, None, "never 0.5");
        }
    }
}

/// And neither must a report with no instrument behind it at all.
#[test]
fn an_all_no_strategy_report_never_reads_as_balanced() {
    let mut content = shipped();
    // Every strategy whose army names exactly one unit, not only the `mass_*`
    // five: `rush` is a ripper-only build under another name, and the mapping
    // reads build orders, not ids.
    content
        .strategies
        .retain(|s| s.army.len() != 1 || s.army[0].unit == "worker");
    // Records exist and every rate would be a win — but nothing maps.
    let records = shipped_records(&[(4, 0, 0, 0); 5]);
    let report = PentagonReport::of(&content, &WinMatrix::of(&records)).expect("closed");
    assert_eq!(report.gaps(), 5, "every link is unmeasurable");
    assert_eq!(report.holding(), 0);
    assert_eq!(report.failing(), 0);
    assert!(!report.all_hold(), "no probe set is not a balanced pentagon");
    for l in report.links() {
        assert_eq!(l.verdict, Verdict::NoStrategy);
        assert_eq!(l.rate, None);
        assert_eq!(l.n_decided, 0);
    }
    let mut missing = report.missing_strategies();
    missing.sort();
    assert_eq!(
        missing,
        vec!["arclight", "bulwark", "ravager", "ripper", "sentinel"]
    );
}

/// Four links holding and one unmeasurable is not five holding.
#[test]
fn a_single_gap_blocks_all_hold() {
    let mut content = shipped();
    // `rush` masses rippers too, so both ripper-only builds must go for the
    // ripper to be a real gap.
    content
        .strategies
        .retain(|s| s.id != "mass_ripper" && s.id != "rush");
    let records = shipped_records(&[(4, 0, 0, 0); 5]);
    let report = PentagonReport::of(&content, &WinMatrix::of(&records)).expect("closed");
    assert_eq!(report.gaps(), 2, "both links touching the ripper");
    assert_eq!(report.holding(), 3);
    assert!(!report.all_hold());
    assert_eq!(report.missing_strategies(), vec!["ripper".to_string()]);
}

// ---- malformed shapes: a distinct error each, never a panic ------------------

#[test]
fn each_malformed_shape_has_its_own_error() {
    // Prey that is a building, not a unit.
    let mut c = shipped();
    set_nemesis(&mut c, "ripper", Some("foundry"));
    assert_eq!(
        PentagonReport::of(&c, &WinMatrix::default()).unwrap_err(),
        CycleError::UnknownPrey {
            unit: "ripper".to_string(),
            prey: "foundry".to_string()
        }
    );

    // A link into a unit that names no prey: the chain opens there.
    let mut c = shipped();
    set_nemesis(&mut c, "ravager", Some("worker"));
    assert_eq!(
        PentagonReport::of(&c, &WinMatrix::default()).unwrap_err(),
        CycleError::Open { at: "worker".to_string() }
    );

    // A lasso whose start is *off* the cycle: the worker is first in RON order,
    // so the walk starts outside the pentagon and re-enters it.
    let mut c = shipped();
    set_nemesis(&mut c, "worker", Some("sentinel"));
    assert_eq!(
        PentagonReport::of(&c, &WinMatrix::default()).unwrap_err(),
        CycleError::Lasso { at: "sentinel".to_string() }
    );

    // Two units pointing at the same prey: not a cycle either.
    let mut c = shipped();
    set_nemesis(&mut c, "ravager", Some("ripper"));
    let err = PentagonReport::of(&c, &WinMatrix::default()).unwrap_err();
    assert!(
        matches!(err, CycleError::Lasso { .. } | CycleError::Partial { .. }),
        "a doubled prey is a named malformation, got {err:?}"
    );

    // A unit that is its own prey.
    let mut c = shipped();
    set_nemesis(&mut c, "worker", Some("worker"));
    assert_eq!(
        PentagonReport::of(&c, &WinMatrix::default()).unwrap_err(),
        CycleError::SelfNemesis("worker".to_string())
    );

    // Every error prints the unit it is about.
    let mut c = shipped();
    set_nemesis(&mut c, "bulwark", None);
    let err = PentagonReport::of(&c, &WinMatrix::default()).unwrap_err();
    assert!(err.to_string().contains("bulwark"), "{err}");
}

/// Malformed content must be reported identically on every call — no map
/// iteration deciding which complaint comes back.
#[test]
fn a_malformed_report_is_deterministic() {
    let mut c = shipped();
    set_nemesis(&mut c, "ripper", Some("chimera"));
    set_nemesis(&mut c, "arclight", Some("arclight"));
    let first = PentagonReport::of(&c, &WinMatrix::default()).unwrap_err();
    for _ in 0..8 {
        assert_eq!(PentagonReport::of(&c, &WinMatrix::default()).unwrap_err(), first);
    }
}

/// A two-unit cycle is a closed cycle, so it is derived — but two units that
/// counter each other can never both hold, because the matrix is
/// complementary. It must not read as a balanced pentagon.
#[test]
fn a_two_unit_cycle_is_derived_and_cannot_all_hold() {
    let mut c = shipped();
    for u in ["sentinel", "ripper", "arclight"] {
        set_nemesis(&mut c, u, None);
    }
    set_nemesis(&mut c, "bulwark", Some("ravager"));
    set_nemesis(&mut c, "ravager", Some("bulwark"));
    let cycle = pentagon::nemesis_cycle(&c).expect("a closed 2-cycle");
    assert_eq!(cycle, vec!["bulwark".to_string(), "ravager".to_string()]);

    let records = series("mass_bulwark", "mass_ravager", 3, 1, 0, 0);
    let report = PentagonReport::of(&c, &WinMatrix::of(&records)).expect("closed");
    assert_eq!(report.links().len(), 2);
    assert_eq!(link(&report, "bulwark").rate, Some(0.75));
    assert_eq!(link(&report, "ravager").rate, Some(0.25));
    assert!(!report.all_hold(), "mutual counters cannot both hold");
}

// ---- the mapping is the build order ------------------------------------------

#[test]
fn mass_strategy_reads_counts_and_refuses_mixed_or_empty_armies() {
    // count > 1 still masses.
    let mut c = shipped();
    let s = c
        .strategies
        .iter_mut()
        .find(|s| s.id == "mass_bulwark")
        .expect("shipped");
    s.army = vec![ArmyItem { unit: "bulwark".to_string(), count: 7 }];
    assert_eq!(
        pentagon::mass_strategy(&c, "bulwark").map(|s| s.id.as_str()),
        Some("mass_bulwark")
    );

    // Two entries naming the same unit still mass it.
    let mut c = shipped();
    let s = c
        .strategies
        .iter_mut()
        .find(|s| s.id == "mass_bulwark")
        .expect("shipped");
    s.army = vec![
        ArmyItem { unit: "bulwark".to_string(), count: 1 },
        ArmyItem { unit: "bulwark".to_string(), count: 2 },
    ];
    assert_eq!(
        pentagon::mass_strategy(&c, "bulwark").map(|s| s.id.as_str()),
        Some("mass_bulwark")
    );

    // A mixed army masses nothing.
    let mut c = shipped();
    let s = c
        .strategies
        .iter_mut()
        .find(|s| s.id == "mass_bulwark")
        .expect("shipped");
    s.army = vec![
        ArmyItem { unit: "bulwark".to_string(), count: 1 },
        ArmyItem { unit: "sentinel".to_string(), count: 1 },
    ];
    assert!(
        pentagon::mass_strategy(&c, "bulwark").is_none(),
        "the only bulwark-only build is gone, so there is no bulwark probe"
    );
    assert_eq!(
        pentagon::mass_strategy(&c, "sentinel").map(|s| s.id.as_str()),
        Some("mass_sentinel"),
        "and the mixed build is not the sentinel's probe either"
    );

    // An empty army masses nothing.
    let mut c = shipped();
    let s = c
        .strategies
        .iter_mut()
        .find(|s| s.id == "mass_bulwark")
        .expect("shipped");
    s.army.clear();
    assert!(pentagon::mass_strategy(&c, "bulwark").is_none());
}

/// The shipped instrument: each cycle unit maps to its own `mass_*` probe —
/// the knob-identical five, not `rush` (which is also a ripper-only build, and
/// would be picked up if it came first in RON order).
#[test]
fn the_shipped_mapping_is_the_five_knob_identical_probes() {
    let content = shipped();
    for (predator, _) in SHIPPED {
        assert_eq!(
            pentagon::mass_strategy(&content, predator).map(|s| s.id.as_str()),
            Some(format!("mass_{predator}").as_str()),
            "the {predator} link must be measured with the {predator} probe"
        );
    }
    let report = PentagonReport::of(&content, &WinMatrix::default()).expect("closed");
    for l in report.links() {
        assert_eq!(
            l.predator_strategy.as_deref(),
            Some(format!("mass_{}", l.predator).as_str())
        );
    }
}

#[test]
fn an_out_of_cycle_probe_does_not_displace_a_real_one() {
    let mut c = shipped();
    let mut worker_probe = c
        .strategies
        .iter()
        .find(|s| s.id == "mass_sentinel")
        .expect("shipped")
        .clone();
    worker_probe.id = "mass_worker".to_string();
    worker_probe.army = vec![ArmyItem { unit: "worker".to_string(), count: 1 }];
    c.strategies.insert(0, worker_probe);

    for (predator, _) in SHIPPED {
        assert_eq!(
            pentagon::mass_strategy(&c, predator).map(|s| s.id.as_str()),
            Some(format!("mass_{predator}").as_str()),
            "the {predator} probe is unchanged"
        );
    }
    let report = PentagonReport::of(&c, &WinMatrix::default()).expect("closed");
    assert!(
        report.links().iter().all(|l| l.predator != "worker" && l.prey != "worker"),
        "a probe for a unit outside the cycle is irrelevant to the pentagon"
    );
}

#[test]
fn a_duplicate_probe_resolves_to_the_first_in_ron_order_every_time() {
    let mut c = shipped();
    let mut twin = c
        .strategies
        .iter()
        .find(|s| s.id == "mass_bulwark")
        .expect("shipped")
        .clone();
    twin.id = "alt_bulwark".to_string();
    c.strategies.insert(0, twin);

    for _ in 0..8 {
        assert_eq!(
            pentagon::mass_strategy(&c, "bulwark").map(|s| s.id.as_str()),
            Some("alt_bulwark"),
            "first in RON order wins, deterministically"
        );
    }
    let report = PentagonReport::of(&c, &WinMatrix::default()).expect("closed");
    assert_eq!(
        link(&report, "bulwark").predator_strategy.as_deref(),
        Some("alt_bulwark")
    );
}

// ---- an injected imbalance, played for real ----------------------------------

/// Arclight > Bulwark holds on the shipped content. Cripple the Arclight probe
/// — its Aether Spire never opens, so it fields no army — and the same link
/// must stop holding. The end-to-end check that the report reads the sim and
/// not a fixture.
#[test]
fn a_crippled_probe_breaks_the_link_it_cripples() {
    let probes = vec!["mass_arclight".to_string(), "mass_bulwark".to_string()];
    let settings = BatchSettings::default().with_only(probes).with_seeds(1);

    let content = shipped();
    let healthy = batch::run_batch(&content, &settings, &mut |_| {}).expect("shipped names");
    let healthy = PentagonReport::of_records(&content, &healthy).expect("closed");
    let before = link(&healthy, "arclight");
    assert_eq!(before.prey, "bulwark");
    assert_eq!(
        before.verdict,
        Verdict::Holds,
        "baseline: the arclight beats the bulwark ({:?})",
        before.rate
    );

    let mut broken = shipped();
    let s = broken
        .strategies
        .iter_mut()
        .find(|s| s.id == "mass_arclight")
        .expect("shipped");
    for opening in &mut s.barracks {
        opening.at_tick = 200_000; // never, inside any tick cap
    }
    let records = batch::run_batch(&broken, &settings, &mut |_| {}).expect("shipped names");
    let report = PentagonReport::of_records(&broken, &records).expect("closed");
    let after = link(&report, "arclight");
    assert_ne!(
        after.verdict,
        Verdict::Holds,
        "an arclight that never opens its barracks cannot counter anything (rate {:?})",
        after.rate
    );
    assert!(
        after.rate.map(|r| r < before.rate.unwrap()).unwrap_or(true),
        "the injected imbalance must move the reading down: {:?} -> {:?}",
        before.rate,
        after.rate
    );
}

/// The same records give the same report, twice, structurally.
#[test]
fn the_report_is_a_pure_function_of_content_and_records() {
    let content = shipped();
    let records = shipped_records(&[(3, 1, 0, 1); 5]);
    let a = PentagonReport::of_records(&content, &records).expect("closed");
    let b = PentagonReport::of_records(&content, &records).expect("closed");
    assert_eq!(a, b);
    assert_eq!(a.to_string(), b.to_string());
}
