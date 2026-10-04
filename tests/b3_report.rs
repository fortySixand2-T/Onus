//! L2 integration tests for **B3 AC4** — the stdout table and
//! `balance_report.ron`.
//!
//! [`onus::report::BalanceReport`] is one batch's whole reading: the win
//! matrix with every cell's sample and interval, the pentagon verdicts with
//! their intervals, the match-length distribution on both bases, production,
//! and every kill criterion as PASS / FAIL / undetermined with its numbers.
//! What is encoded here:
//!
//!   - **the RON round-trips**: written and read back, the report is equal to
//!     itself, through a string and through a file;
//!   - **it carries what it claims**: matrix cells match [`WinMatrix`], the
//!     pentagon has its five links with intervals, the gate is the gate;
//!   - **order is the records'**, never a map's: two builds of the same batch
//!     serialize to the same bytes;
//!   - **the binary writes it** (to `--report PATH`, atomically) and prints the
//!     length and kill-gate tables; an all-timeout run's report and table both
//!     say FAIL and ALL TIMEOUT;
//!   - **the artifact is gitignored**.

use std::path::PathBuf;
use std::process::Command;

use onus::batch::{BatchSettings, MatchRecord, MatchResult, ProductionCounts};
use onus::gate::Status;
use onus::headless::{self, Orientation, SIM_HZ};
use onus::metrics::WinMatrix;
use onus::report::{BalanceReport, DEFAULT_REPORT_PATH, REPORT_SCHEMA};
use onus::sim::content::Content;
use onus::sim::spatial::Faction;

const A: MatchResult = MatchResult::Decided(Faction::A);
const B: MatchResult = MatchResult::Decided(Faction::B);
const CAPPED: MatchResult = MatchResult::Timeout;

fn shipped() -> Content {
    headless::content().expect("content loads")
}

fn rec(a: &str, b: &str, seed: u64, o: Orientation, result: MatchResult, ticks: u32) -> MatchRecord {
    MatchRecord {
        strategies: [a.to_string(), b.to_string()],
        seed,
        result,
        orientation: o,
        ticks,
        produced: ProductionCounts::default(),
    }
}

/// A small mixed batch over two probes: decided both ways, a mutual loss, a
/// timeout, mirrors, and lengths on both sides of the band.
fn batch() -> Vec<MatchRecord> {
    let min = 60 * SIM_HZ;
    let mut v = Vec::new();
    for seed in 0..3 {
        v.push(rec("mass_sentinel", "mass_ripper", seed, Orientation::Normal, A, 6 * min));
        v.push(rec("mass_ripper", "mass_sentinel", seed, Orientation::Swapped, B, 3 * min));
        v.push(rec("mass_ripper", "mass_sentinel", seed, Orientation::Normal, A, 9 * min));
        v.push(rec("mass_sentinel", "mass_sentinel", seed, Orientation::Normal, MatchResult::MutualLoss, 7 * min));
        v.push(rec("mass_ripper", "mass_ripper", seed, Orientation::Swapped, CAPPED, 15 * min));
    }
    v
}

fn settings() -> BatchSettings {
    BatchSettings::default()
        .with_seeds(3)
        .with_only(vec!["mass_sentinel".into(), "mass_ripper".into()])
}

#[test]
fn the_report_round_trips_through_ron() {
    let r = BalanceReport::of(&shipped(), &settings(), &batch());
    let text = r.to_ron().expect("serializes");
    let back = BalanceReport::from_ron(&text).expect("parses");
    assert_eq!(back, r);
    assert_eq!(back.to_ron().unwrap(), text, "and re-serializes to the same bytes");
}

#[test]
fn the_report_round_trips_through_a_file() {
    let dir = std::env::temp_dir().join(format!("onus_b3_report_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("balance_report.ron");
    let r = BalanceReport::of(&shipped(), &settings(), &batch());
    r.write(&path).expect("writes");
    assert_eq!(BalanceReport::read(&path).expect("reads"), r);
    let leftovers: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p != &path)
        .collect();
    assert!(leftovers.is_empty(), "no temp file left behind: {leftovers:?}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn the_report_carries_the_matrix_pentagon_length_and_gate() {
    let content = shipped();
    let recs = batch();
    let r = BalanceReport::of(&content, &settings(), &recs);
    let m = WinMatrix::of(&recs);

    assert_eq!(r.schema, REPORT_SCHEMA);
    assert_eq!(r.content_hash, content.fingerprint().hash());
    assert_eq!(r.settings.seeds, 3);
    assert_eq!(r.outcomes.matches, 15);
    assert_eq!(r.outcomes.timeouts, 3);
    assert_eq!(r.outcomes.mutual_losses, 3);

    assert_eq!(r.matrix.ids, m.ids());
    for i in 0..m.len() {
        for j in 0..m.len() {
            let c = m.cell(i, j).copied().unwrap_or_default();
            let got = &r.matrix.cells[i][j];
            assert_eq!((got.half_wins, got.n_decided, got.n_timeout), (c.half_wins, c.n_decided, c.n_timeout));
            assert_eq!(got.rate, c.rate());
            assert_eq!(got.interval, c.wilson_interval());
        }
    }
    assert_eq!(r.matrix.row_means.len(), 2);

    assert_eq!(r.pentagon.links.len(), 5, "the shipped cycle");
    assert_eq!(r.pentagon.cycle_error, None);
    let measured: Vec<_> = r.pentagon.links.iter().filter(|l| l.rate.is_some()).collect();
    assert_eq!(measured.len(), 1, "only sentinel > ripper was played");
    assert_eq!(measured[0].predator, "sentinel");
    assert!(measured[0].interval.is_some(), "a verdict is never quoted without its interval");

    assert_eq!((r.length.total, r.length.decided, r.length.timeouts), (15, 12, 3));
    assert_eq!(r.length.in_band, 6, "the 6:00 and 7:00 matches");

    assert_eq!(r.gate, onus::gate::KillGate::of(&content, &recs, &Default::default()));
    assert_ne!(r.gate.status, Status::Pass, "15 matches decide nothing");
}

#[test]
fn the_report_is_a_function_of_the_records() {
    let a = BalanceReport::of(&shipped(), &settings(), &batch()).to_ron().unwrap();
    let b = BalanceReport::of(&shipped(), &settings(), &batch()).to_ron().unwrap();
    assert_eq!(a, b);
}

#[test]
fn the_tables_never_start_a_line_with_a_matrix_row_marker() {
    let r = BalanceReport::of(&shipped(), &settings(), &batch());
    let text = r.to_string();
    for line in text.lines() {
        assert!(!line.starts_with('['), "{line}");
    }
    for needle in [
        "length",
        "kill gate",
        "K1 strength",
        "K2 seat",
        "K3 length",
        "before band",
        "after/capped",
        "timeouts",
        "band share",
        "reported, not gated",
        "at least 50% of decided",
        "p50",
    ] {
        assert!(text.contains(needle), "`{needle}` missing:\n{text}");
    }
}

#[test]
fn an_all_timeout_report_says_fail_and_all_timeout() {
    let recs: Vec<MatchRecord> = (0..4)
        .map(|s| rec("rush", "turtle", s, Orientation::Normal, CAPPED, 1))
        .collect();
    let r = BalanceReport::of(&shipped(), &BatchSettings::default(), &recs);
    assert!(r.gate.termination.all_timeout);
    assert_eq!(r.gate.status, Status::Fail);
    assert_eq!(r.length.decided, 0);
    let text = r.to_string();
    assert!(text.contains("kill gate    FAIL"), "{text}");
    assert!(text.contains("ALL TIMEOUT"), "{text}");
}

#[test]
fn the_artifact_is_gitignored() {
    assert_eq!(DEFAULT_REPORT_PATH, "balance_report.ron");
    let ignore = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/.gitignore")).unwrap();
    assert!(
        ignore.lines().any(|l| l.trim() == "/balance_report.ron"),
        ".gitignore must list /balance_report.ron:\n{ignore}"
    );
}

fn tmp_report(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("onus_b3_{tag}_{}.ron", std::process::id()))
}

#[test]
fn the_binary_writes_the_report_it_prints() {
    let path = tmp_report("bin");
    let out = Command::new(env!("CARGO_BIN_EXE_balance"))
        .args(["--only", "rush,turtle", "--tick-cap", "1", "--report"])
        .arg(&path)
        .output()
        .expect("binary runs");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let r = BalanceReport::read(&path).expect("the binary wrote a readable report");
    std::fs::remove_file(&path).unwrap();
    assert_eq!(r.outcomes.matches, 8);
    assert_eq!(r.settings.tick_cap, 1);
    assert_eq!(r.settings.only, Some(vec!["rush".to_string(), "turtle".to_string()]));
    assert!(r.gate.termination.all_timeout);
    assert_eq!(r.gate.status, Status::Fail);
    assert!(stdout.contains(&r.to_string()), "stdout carries the report's tables:\n{stdout}");
    assert!(stdout.contains(&format!("report       {}", path.display())), "{stdout}");
}

#[test]
fn a_missing_report_path_is_refused_before_anything_runs() {
    let out = Command::new(env!("CARGO_BIN_EXE_balance"))
        .args(["--only", "rush", "--report"])
        .output()
        .expect("binary runs");
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--report needs a value"));
}

#[test]
fn an_unwritable_report_path_fails_the_run() {
    let path = std::env::temp_dir()
        .join(format!("onus_b3_no_such_dir_{}", std::process::id()))
        .join("balance_report.ron");
    let out = Command::new(env!("CARGO_BIN_EXE_balance"))
        .args(["--only", "rush", "--tick-cap", "1", "--report"])
        .arg(&path)
        .output()
        .expect("binary runs");
    assert!(!out.status.success(), "a report that was not written must not exit 0");
    assert!(String::from_utf8_lossy(&out.stderr).contains("report"));
}

/// The report file is opt-in: without `--report` the bin prints its tables and
/// writes nothing, so a toy run (the suite's `--tick-cap 1` invocations, or a
/// quick probe by hand) can never overwrite a real batch's report (critic B3).
#[test]
fn without_report_the_binary_prints_the_tables_and_writes_no_file() {
    let dir = std::env::temp_dir().join(format!("onus_b3_cwd_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_balance"))
        .args(["--only", "rush", "--tick-cap", "1"])
        .current_dir(&dir)
        .output()
        .expect("binary runs");
    let written = dir.join(DEFAULT_REPORT_PATH).exists();
    let leftovers = std::fs::read_dir(&dir).unwrap().count();
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(!written, "no --report, no file");
    assert_eq!(leftovers, 0, "nothing at all written into the working directory");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("kill gate"), "the tables still print:\n{stdout}");
    assert!(!stdout.contains("report       "), "no report line without a report:\n{stdout}");
}

/// A strictly weak strategy is tagged in the table and named in the report,
/// and the table says the name does not gate (critic B3, F-038).
#[test]
fn a_strictly_weak_strategy_is_tagged_in_the_table_and_named_in_the_report() {
    let min = 60 * SIM_HZ;
    let mut recs = Vec::new();
    for seed in 0..40 {
        for o in Orientation::ALL {
            recs.push(rec("mass_sentinel", "rush", seed, o, A, 6 * min));
            recs.push(rec("rush", "mass_sentinel", seed, o, B, 6 * min));
        }
    }
    let r = BalanceReport::of(&shipped(), &BatchSettings::default(), &recs);
    assert_eq!(r.gate.strength.losing, ["rush"]);
    let back = BalanceReport::from_ron(&r.to_ron().unwrap()).unwrap();
    assert_eq!(back.gate.strength.losing, ["rush"], "the RON carries the name");
    let text = r.to_string();
    let row = text.lines().find(|l| l.trim_start().starts_with("rush ")).expect("rush row");
    assert!(row.contains("LOSING"), "{row}");
    assert!(text.contains("losing (named, not gated): rush"), "{text}");
}
