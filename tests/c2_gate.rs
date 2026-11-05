//! C2 AC4 — **the nation criterion**: each nation's cross-nation strength is
//! 35-65%, and the pentagon still holds under every nation pair (C0's
//! trait-magnitude bound). The criterion's arithmetic is tested here on
//! synthetic records; the shipped reading is a measurement
//! (`the_shipped_nation_reading`, `--ignored`, recorded in FINDINGS).

use onus::batch::{
    self, BatchSettings, Fixture, MatchRecord, MatchResult, NationAxis, NationRecord,
    ProductionCounts,
};
use onus::gate::{NationGate, NationSpec, Status};
use onus::headless::{self, Orientation};
use onus::pentagon::{self, PentagonReport};
use onus::sim::content::Content;
use onus::sim::spatial::Faction;

fn content() -> Content {
    headless::content().expect("content loads")
}

fn rec(nations: [&str; 2], strategies: [&str; 2], seed: u64, o: Orientation, result: MatchResult) -> NationRecord {
    NationRecord {
        nations: [nations[0].into(), nations[1].into()],
        record: MatchRecord {
            strategies: [strategies[0].into(), strategies[1].into()],
            seed,
            result,
            orientation: o,
            ticks: 20_000,
            produced: ProductionCounts::default(),
        },
    }
}

/// A cross batch over `nations` on strategy `s`: every ordered pair, `seeds`
/// seeds, both orientations; `wins(a, b, seed, o)` says whether slot A won.
fn cross(nations: &[&str], seeds: u64, wins: impl Fn(&str, &str, u64, Orientation) -> bool) -> Vec<NationRecord> {
    let mut out = Vec::new();
    for seed in 0..seeds {
        for a in nations {
            for b in nations.iter().filter(|b| *b != a) {
                for o in Orientation::ALL {
                    let w = if wins(a, b, seed, o) { Faction::A } else { Faction::B };
                    out.push(rec([a, b], ["s", "s"], seed, o, MatchResult::Decided(w)));
                }
            }
        }
    }
    out
}

/// Every nation exactly even: each pairing split by orientation.
fn even(_: &str, _: &str, _: u64, o: Orientation) -> bool {
    o == Orientation::ALL[0]
}

/// The pentagon batch for `pairs` where the predator always wins, except
/// on `broken` (predator nation, prey nation, predator unit) where the prey does.
fn pentagon_records(c: &Content, nations: &[String], seeds: u64, broken: Option<(&str, &str, &str)>) -> Vec<NationRecord> {
    let fixtures = pentagon::nation_pentagon_fixtures(c, nations).unwrap();
    let cycle = PentagonReport::of_records(c, &[]).unwrap();
    let mut out = Vec::new();
    for seed in 0..seeds {
        for f in &fixtures {
            for o in Orientation::ALL {
                let [a, b] = &f.sides;
                // Which slot holds the predator of its link?
                let link = cycle
                    .links()
                    .iter()
                    .find(|l| {
                        let (p, q) = (l.predator_strategy.as_ref().unwrap(), l.prey_strategy.as_ref().unwrap());
                        (&a.strategy == p && &b.strategy == q) || (&a.strategy == q && &b.strategy == p)
                    })
                    .unwrap();
                let pred_slot = if Some(&a.strategy) == link.predator_strategy.as_ref() { 0 } else { 1 };
                let (pn, qn) = (&f.sides[pred_slot].nation, &f.sides[1 - pred_slot].nation);
                let flipped = broken.is_some_and(|(bp, bq, unit)| pn == bp && qn == bq && link.predator == unit);
                let winner_slot = if flipped { 1 - pred_slot } else { pred_slot };
                let w = [Faction::A, Faction::B][winner_slot];
                out.push(rec(
                    [&a.nation, &b.nation],
                    [&a.strategy, &b.strategy],
                    seed,
                    o,
                    MatchResult::Decided(w),
                ));
            }
        }
    }
    out
}

fn ids(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn the_spec_is_c0s_band() {
    let s = NationSpec::default();
    assert_eq!((s.min_strength, s.max_strength), (0.35, 0.65));
}

#[test]
fn even_nations_and_a_holding_pentagon_pass() {
    let c = content();
    let nations = ["house", "forgeborn", "verdant"];
    let x = cross(&nations, 40, even);
    let p = pentagon_records(&c, &ids(&nations), 3, None);
    let g = NationGate::of(&c, &x, &p, &NationSpec::default()).unwrap();
    assert_eq!(g.strength.len(), 3);
    for r in &g.strength {
        assert_eq!(r.strength.value, Some(0.5), "{}", r.nation);
        assert_eq!(r.strength.status, Status::Pass, "{}: {:?}", r.nation, r.strength.interval);
        assert_eq!(r.opponents, 2);
    }
    assert_eq!(g.strength_status, Status::Pass);
    // Every ordered pair, mirrors included.
    assert_eq!(g.pentagon.len(), 9);
    assert!(g.pentagon.iter().all(|p| p.status == Status::Pass && p.links.len() == 5));
    assert_eq!(g.pentagon_status, Status::Pass);
    assert_eq!(g.status, Status::Pass);
    assert!(g.failing.is_empty());
}

#[test]
fn a_dominant_nation_fails_and_is_named() {
    let c = content();
    let nations = ["house", "forgeborn", "verdant"];
    // Forgeborn wins three matches in four; the others split evenly, so
    // they sit at 37.5%: inside the band's interval reach, not named.
    let x = cross(&nations, 40, |a, b, seed, o| match (a, b) {
        ("forgeborn", _) => seed % 4 != 0,
        (_, "forgeborn") => seed % 4 == 0,
        _ => even(a, b, seed, o),
    });
    let g = NationGate::of(&c, &x, &[], &NationSpec::default()).unwrap();
    assert_eq!(g.failing, ids(&["forgeborn"]), "{:#?}", g.strength);
    let row = |id: &str| g.strength.iter().find(|r| r.nation == id).unwrap().strength.clone();
    assert_eq!(row("forgeborn").value, Some(0.75));
    assert_eq!(row("house").value, Some(0.375));
    assert_eq!(g.strength_status, Status::Fail);
    assert_eq!(g.status, Status::Fail);
    // No pentagon batch: that half is undetermined, never a pass.
    assert_eq!(g.pentagon_status, Status::Undetermined);
}

#[test]
fn a_weak_nation_fails_too() {
    let c = content();
    let nations = ["house", "forgeborn", "verdant"];
    // Verdant wins one match in four: below the band, and named.
    let x = cross(&nations, 40, |a, b, seed, o| match (a, b) {
        ("verdant", _) => seed % 4 == 0,
        (_, "verdant") => seed % 4 != 0,
        _ => even(a, b, seed, o),
    });
    let g = NationGate::of(&c, &x, &[], &NationSpec::default()).unwrap();
    assert_eq!(g.failing, ids(&["verdant"]), "{:#?}", g.strength);
    assert_eq!(g.strength_status, Status::Fail);
}

#[test]
fn a_thin_sample_is_undetermined_not_a_pass() {
    let c = content();
    let x = cross(&["house", "forgeborn"], 1, even);
    let g = NationGate::of(&c, &x, &[], &NationSpec::default()).unwrap();
    assert!(g.strength.iter().all(|r| r.strength.status == Status::Undetermined));
    assert_eq!(g.status, Status::Undetermined);
}

#[test]
fn mirrors_and_timeouts_do_not_count_toward_strength() {
    let c = content();
    let mut x = cross(&["house", "forgeborn"], 40, even);
    for seed in 0..40 {
        x.push(rec(["house", "house"], ["s", "s"], seed, Orientation::ALL[0], MatchResult::Decided(Faction::A)));
        x.push(rec(["house", "forgeborn"], ["s", "s"], seed, Orientation::ALL[0], MatchResult::Timeout));
    }
    let g = NationGate::of(&c, &x, &[], &NationSpec::default()).unwrap();
    for r in &g.strength {
        assert_eq!(r.strength.value, Some(0.5), "{}", r.nation);
    }
}

#[test]
fn a_link_broken_under_one_nation_pair_fails_that_pair_only() {
    let c = content();
    let nations = ids(&["house", "aetherkin"]);
    let cycle = PentagonReport::of_records(&c, &[]).unwrap();
    let unit = cycle.links()[0].predator.clone();
    let p = pentagon_records(&c, &nations, 3, Some(("aetherkin", "house", &unit)));
    let x = cross(&["house", "aetherkin"], 40, even);
    let g = NationGate::of(&c, &x, &p, &NationSpec::default()).unwrap();
    assert_eq!(g.pentagon_status, Status::Fail);
    assert_eq!(g.status, Status::Fail);
    for pair in &g.pentagon {
        let broken = pair.predator_nation == "aetherkin" && pair.prey_nation == "house";
        assert_eq!(pair.status, if broken { Status::Fail } else { Status::Pass }, "{pair:?}");
        if broken {
            assert_eq!(pair.links[0].verdict, pentagon::Verdict::Fails);
            assert!(pair.links[1..].iter().all(|l| l.verdict == pentagon::Verdict::Holds));
        }
    }
}

#[test]
fn the_pentagon_fixtures_field_each_probe_under_every_ordered_pair() {
    let c = content();
    let nations = ids(&["house", "forgeborn", "ashen_reach"]);
    let f = pentagon::nation_pentagon_fixtures(&c, &nations).unwrap();
    let links = PentagonReport::of_records(&c, &[]).unwrap().links().len();
    assert_eq!(f.len(), 3 * 3 * links * 2);
    let report = PentagonReport::of_records(&c, &[]).unwrap();
    let first = &report.links()[0];
    let (ps, qs) = (first.predator_strategy.clone().unwrap(), first.prey_strategy.clone().unwrap());
    assert_eq!(f[0], Fixture::new(("house", &ps), ("house", &qs)));
    // The predator's nation is the outer loop: (house, forgeborn) comes
    // before (forgeborn, house).
    assert_eq!(f[2 * links], Fixture::new(("house", &ps), ("forgeborn", &qs)));
    assert_eq!(f[2 * links + 1], Fixture::new(("forgeborn", &qs), ("house", &ps)));
    // Both slot orders of each (link, pair).
    assert_eq!(f[0].sides[0], f[1].sides[1]);
    assert_eq!(f[0].sides[1], f[1].sides[0]);
}

/// **The shipped reading** (C2 AC4): the cross batch (gated strength), the
/// pentagon batch under every nation pair (gated), and the personality batch
/// (reported, not gated). `ONUS_C2_SEEDS` (default 2) seeds each;
/// `ONUS_C2_PART` = `cross`, `pentagon` or `personality` runs one part (default
/// all). Long; release, `--ignored --nocapture`.
#[test]
#[ignore = "measurement: run in release with --ignored --nocapture"]
fn the_shipped_nation_reading() {
    let c = content();
    let seeds: u32 = std::env::var("ONUS_C2_SEEDS").ok().and_then(|s| s.parse().ok()).unwrap_or(2);
    let part = std::env::var("ONUS_C2_PART").unwrap_or_default();
    let run = |p: &str| part.is_empty() || part == p;
    let settings = BatchSettings::default().with_seeds(seeds);
    let nations = batch::nation_roster(&c, None).unwrap();
    let spec = NationSpec::default();
    let t0 = std::time::Instant::now();
    let mut n = 0;
    let mut tick = |r: &NationRecord| {
        n += 1;
        if n % 50 == 0 {
            eprintln!("[{n}] {:?} {:?} {:?} {}s", r.nations, r.record.strategies, r.record.result, t0.elapsed().as_secs());
        }
    };
    let strength_table = |label: &str, records: &[NationRecord]| {
        let g = NationGate::of(&c, records, &[], &spec).unwrap();
        let timeouts = records.iter().filter(|r| r.record.result == MatchResult::Timeout).count();
        println!("{label}: strength {}, {} matches, {timeouts} timeouts, seeds {seeds}", g.strength_status.label(), records.len());
        for r in &g.strength {
            let (lo, hi) = r.strength.interval.unwrap_or((f64::NAN, f64::NAN));
            println!("  {:<12} {:>5.1}% [{:.1}, {:.1}] n {} n_eff {:.0} {}", r.nation,
                100.0 * r.strength.value.unwrap_or(f64::NAN), 100.0 * lo, 100.0 * hi,
                r.strength.n, r.strength.n_eff, r.strength.status.label());
        }
        // The nation matrix itself, row against column.
        let relabelled: Vec<MatchRecord> = records.iter().map(NationRecord::by_nation).collect();
        let m = onus::metrics::WinMatrix::of(&relabelled);
        for (i, id) in m.ids().iter().enumerate() {
            let cells: Vec<String> = (0..m.len())
                .map(|j| m.rate(i, j).map_or("  --".into(), |r| format!("{:>4.0}", 100.0 * r)))
                .collect();
            println!("    {:<12} {}", id, cells.join(" "));
        }
    };
    if run("cross") {
        let probes: Vec<String> = PentagonReport::of_records(&c, &[])
            .unwrap()
            .links()
            .iter()
            .filter_map(|l| l.predator_strategy.clone())
            .collect();
        let cross_settings = settings.clone().with_only(probes);
        let cross = batch::run_nation_batch(&c, &cross_settings, None, NationAxis::Cross, &mut tick).unwrap();
        strength_table("cross (gated)", &cross);
    }
    if run("pentagon") {
        let fixtures = pentagon::nation_pentagon_fixtures(&c, &nations).unwrap();
        let pent = batch::run_fixtures(&c, &fixtures, &settings, &mut tick).unwrap();
        let g = NationGate::of(&c, &[], &pent, &spec).unwrap();
        let timeouts = pent.iter().filter(|r| r.record.result == MatchResult::Timeout).count();
        println!("pentagon (gated): {}, {} matches, {timeouts} timeouts, seeds {seeds}", g.pentagon_status.label(), pent.len());
        for p in &g.pentagon {
            let line: Vec<String> = p
                .links
                .iter()
                .map(|l| format!("{}>{} {:.0}% {}/{} {}", &l.predator[..3], &l.prey[..3],
                    100.0 * l.rate.unwrap_or(f64::NAN), l.n_decided, l.n_timeout, l.verdict.label()))
                .collect();
            println!("  {:>11} / {:<11} {:<12} | {}", p.predator_nation, p.prey_nation, p.status.label(), line.join(", "));
        }
    }
    if run("personality") {
        let personality = batch::run_nation_batch(&c, &settings, None, NationAxis::Personality, &mut tick).unwrap();
        strength_table("personality (reported, not gated)", &personality);
    }
    println!("{} s", t0.elapsed().as_secs());
}
