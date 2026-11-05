//! C3 AC8 — **the gates on the three-resource economy.** A batch plays the
//! economy its settings name (Alloy only by default, as every batch before
//! C3), and the B3 kill gate (K1/K2/K3) and the C2 nation cross gate are
//! re-read on the three-resource economy. The readings are measurements
//! (`--ignored`, release), recorded in FINDINGS, not pinned here.

use onus::batch::{
    self, BatchSettings, Fixture, MatchRecord, MatchResult, NationAxis, NationRecord,
};
use onus::gate::{GateSpec, KillGate, NationGate, NationSpec};
use onus::headless::{self, Orientation};
use onus::pentagon::PentagonReport;
use onus::sim::content::Content;
use onus::sim::three::EconomyMode;

fn content() -> Content {
    headless::content().expect("content loads")
}

const CAP: u32 = 4_000;

#[test]
fn a_batch_is_alloy_only_unless_it_names_the_economy() {
    assert_eq!(BatchSettings::default().economy, EconomyMode::Alloy);
    let c = content();
    let only = vec!["mass_ripper".to_string()];
    let alloy = BatchSettings::default()
        .with_only(only.clone())
        .with_tick_cap(CAP);
    let three = alloy.clone().with_economy(EconomyMode::ThreeResource);
    let a = batch::run_batch(&c, &alloy, &mut |_| {}).unwrap();
    let t = batch::run_batch(&c, &three, &mut |_| {}).unwrap();
    // Each record is the match its settings name, played for real.
    let s = headless::MatchSettings::default()
        .with_seed(batch::seed_at(0, 0))
        .with_strategies("mass_ripper", "mass_ripper")
        .with_tick_cap(CAP)
        .with_orientation(Orientation::ALL[0]);
    assert_eq!(a[0], batch::run_match(&c, &s).unwrap());
    let s3 = s.with_economy(EconomyMode::ThreeResource);
    assert_eq!(t[0], batch::run_match(&c, &s3).unwrap());
    // ...and the two economies are not the same match (Rippers cost Biomass).
    assert_ne!(a[0].produced, t[0].produced);
}

#[test]
fn a_nation_batch_plays_the_economy_it_names() {
    let c = content();
    let f = Fixture::new(("forgeborn", "mass_sentinel"), ("verdant", "mass_sentinel"));
    let settings = BatchSettings::default()
        .with_tick_cap(CAP)
        .with_economy(EconomyMode::ThreeResource);
    let recs = batch::run_fixtures(&c, std::slice::from_ref(&f), &settings, &mut |_| {}).unwrap();
    let s = f
        .settings(batch::seed_at(0, 0), Orientation::ALL[0], CAP)
        .with_economy(EconomyMode::ThreeResource);
    assert_eq!(recs[0].record, batch::run_match(&c, &s).unwrap());
}

fn seeds(var: &str, default: u32) -> u32 {
    std::env::var(var)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

fn probes(c: &Content) -> Vec<String> {
    PentagonReport::of_records(c, &[])
        .unwrap()
        .links()
        .iter()
        .filter_map(|l| l.predator_strategy.clone())
        .collect()
}

/// The B3 kill gate on the B3 pentagon batch (the five mass probes), played on
/// the three-resource economy. Release, `--ignored --nocapture`.
#[test]
#[ignore = "measurement: run in release with --ignored --nocapture"]
fn the_three_resource_b3_reading() {
    let c = content();
    let n = seeds("ONUS_C3_B3_SEEDS", 2);
    let settings = BatchSettings::default()
        .with_only(probes(&c))
        .with_seeds(n)
        .with_economy(EconomyMode::ThreeResource);
    let t0 = std::time::Instant::now();
    let recs: Vec<MatchRecord> = batch::run_batch(&c, &settings, &mut |_| {}).unwrap();
    let g = KillGate::of(&c, &recs, &GateSpec::default());
    let timeouts = recs
        .iter()
        .filter(|r| r.result == MatchResult::Timeout)
        .count();
    println!(
        "B3 on three-resource: {} matches, {timeouts} timeouts, seeds {n}",
        recs.len()
    );
    for r in &g.strength.rows {
        let (lo, hi) = r.strength.interval.unwrap_or((f64::NAN, f64::NAN));
        println!(
            "  K1 {:<14} {:>5.1}% [{:.1}, {:.1}] {}",
            r.strategy,
            100.0 * r.strength.value.unwrap_or(f64::NAN),
            100.0 * lo,
            100.0 * hi,
            r.strength.status.label()
        );
    }
    println!("K1: {}", g.strength.status.label());
    let iv = |v: Option<(f64, f64)>| {
        v.map_or("--".into(), |(a, b)| {
            format!("[{:.1}, {:.1}]", 100.0 * a, 100.0 * b)
        })
    };
    println!(
        "K2: {}  slot A {:?} {} (n {})  left base {:?} {}",
        g.seat.status.label(),
        g.seat.slot_a.value,
        iv(g.seat.slot_a.interval),
        g.seat.slot_a.n,
        g.seat.left_spawn.value,
        iv(g.seat.left_spawn.interval)
    );
    let k3 = &g.termination;
    println!("K3: {}  median {:?} ticks, timeouts {:?} {}, band share {:?} {} {}, below {:?}, beyond {:?}",
        k3.status.label(), k3.decided_median, k3.timeout_rate.value, k3.timeout_rate.status.label(),
        k3.band_share.value, iv(k3.band_share.interval), k3.band_share.status.label(),
        k3.below.value, k3.beyond.value);
    // Per cell: the win matrix, timeouts and what each side trained (the
    // probe's own unit, mean per match), to see what an economy number moves.
    let m = onus::metrics::WinMatrix::of(&recs);
    for (i, a) in m.ids().iter().enumerate() {
        let cells: Vec<String> = (0..m.len())
            .map(|j| {
                let b = &m.ids()[j];
                let cell: Vec<&MatchRecord> = recs
                    .iter()
                    .filter(|r| &r.strategies[0] == a && &r.strategies[1] == b)
                    .collect();
                let to = cell
                    .iter()
                    .filter(|r| r.result == MatchResult::Timeout)
                    .count();
                let unit = |s: &str| s.trim_start_matches("mass_").to_string();
                let built = |f: onus::sim::spatial::Faction, s: &str| {
                    cell.iter()
                        .map(|r| r.produced.get(f, &unit(s)))
                        .sum::<u32>() as f64
                        / cell.len().max(1) as f64
                };
                format!(
                    "{}% t{to} {:.0}v{:.0}",
                    m.rate(i, j)
                        .map_or("--".into(), |r| format!("{:.0}", 100.0 * r)),
                    built(onus::sim::spatial::Faction::A, a),
                    built(onus::sim::spatial::Faction::B, b)
                )
            })
            .collect();
        println!("    {:<14} {}", a, cells.join(" | "));
    }
    println!("{} s", t0.elapsed().as_secs());
}

/// The C2 nation cross gate (every ordered pair of nations, the five mass
/// probes) on the three-resource economy, where each nation plays its real
/// lean. Release, `--ignored --nocapture`.
#[test]
#[ignore = "measurement: run in release with --ignored --nocapture"]
fn the_three_resource_nation_cross_reading() {
    let c = content();
    let n = seeds("ONUS_C3_C2_SEEDS", 3);
    let settings = BatchSettings::default()
        .with_only(probes(&c))
        .with_seeds(n)
        .with_economy(EconomyMode::ThreeResource);
    let t0 = std::time::Instant::now();
    let mut k = 0;
    let recs: Vec<NationRecord> =
        batch::run_nation_batch(&c, &settings, None, NationAxis::Cross, &mut |r| {
            k += 1;
            if k % 100 == 0 {
                eprintln!(
                    "[{k}] {:?} {:?} {}s",
                    r.nations,
                    r.record.result,
                    t0.elapsed().as_secs()
                );
            }
        })
        .unwrap();
    let g = NationGate::of(&c, &recs, &[], &NationSpec::default()).unwrap();
    let timeouts = recs
        .iter()
        .filter(|r| r.record.result == MatchResult::Timeout)
        .count();
    println!(
        "C2 cross on three-resource: strength {}, {} matches, {timeouts} timeouts, seeds {n}",
        g.strength_status.label(),
        recs.len()
    );
    for r in &g.strength {
        let (lo, hi) = r.strength.interval.unwrap_or((f64::NAN, f64::NAN));
        println!(
            "  {:<12} {:>5.1}% [{:.1}, {:.1}] {}",
            r.nation,
            100.0 * r.strength.value.unwrap_or(f64::NAN),
            100.0 * lo,
            100.0 * hi,
            r.strength.status.label()
        );
    }
    let relabelled: Vec<MatchRecord> = recs.iter().map(NationRecord::by_nation).collect();
    let m = onus::metrics::WinMatrix::of(&relabelled);
    for (i, id) in m.ids().iter().enumerate() {
        let cells: Vec<String> = (0..m.len())
            .map(|j| {
                m.rate(i, j)
                    .map_or("  --".into(), |r| format!("{:>4.0}", 100.0 * r))
            })
            .collect();
        println!("    {:<12} {}", id, cells.join(" "));
    }
    println!("{} s", t0.elapsed().as_secs());
}
