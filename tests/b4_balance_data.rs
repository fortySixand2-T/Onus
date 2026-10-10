//! B4 turtle, step 2: `balance --data DIR` measures a candidate RON without
//! touching the tracked tree.
//!
//! The flag is opt-in. Without it the binary reads the shipped `assets/data`
//! exactly as before. With it, the report must carry the *scratch* content's
//! fingerprint, so a scratch reading can never be quoted as the shipped one, and
//! a directory that does not load must fail the run rather than fall back to the
//! shipped data.

use std::path::{Path, PathBuf};
use std::process::Command;

use onus::headless;
use onus::report::BalanceReport;
use onus::sim::content::Content;

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("onus_b4_data_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A copy of the shipped data with every wave sent as an attack-move.
fn attack_move_everywhere(dir: &Path) {
    let shipped = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data");
    for f in ["units.ron", "resources.ron", "strategies.ron"] {
        let text = std::fs::read_to_string(shipped.join(f)).unwrap();
        let text = if f == "strategies.ron" {
            assert!(text.contains("wave_order: move,"));
            text.replace("wave_order: move,", "wave_order: attack_move,")
        } else {
            text
        };
        std::fs::write(dir.join(f), text).unwrap();
    }
}

fn run(extra: &[&str], report: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_balance"))
        .args(["--only", "rush,turtle", "--tick-cap", "1"])
        .args(extra)
        .arg("--report")
        .arg(report)
        .output()
        .expect("binary runs")
}

#[test]
fn data_dir_is_the_content_the_report_was_played_on() {
    let dir = scratch("am");
    attack_move_everywhere(&dir);
    let report = dir.join("report.ron");
    let out = run(&["--data", dir.to_str().unwrap()], &report);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let r = BalanceReport::read(&report).expect("the binary wrote a readable report");

    let scratch_hash = Content::load_from_dir(&dir).unwrap().fingerprint().hash();
    let shipped_hash = headless::content().unwrap().fingerprint().hash();
    assert_ne!(scratch_hash, shipped_hash, "the scratch data must differ from the shipped data");
    assert_eq!(r.content_hash, scratch_hash, "the report names the content it was played on");
    assert_eq!(r.outcomes.matches, 8);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn without_data_the_shipped_content_is_played() {
    let dir = scratch("shipped");
    let report = dir.join("report.ron");
    let out = run(&[], &report);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let r = BalanceReport::read(&report).unwrap();
    assert_eq!(r.content_hash, headless::content().unwrap().fingerprint().hash());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_data_dir_that_does_not_load_fails_the_run_and_never_falls_back() {
    let dir = scratch("bad");
    let missing = dir.join("no_such_dir");
    let report = dir.join("report.ron");
    let out = run(&["--data", missing.to_str().unwrap()], &report);
    assert!(!out.status.success(), "a bad --data must not exit 0");
    assert!(out.stdout.is_empty(), "nothing was played, so nothing is printed");
    assert!(!report.exists(), "no report may be written from a fallback");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("no_such_dir"), "the error names the directory: {err}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn data_needs_a_value() {
    let out = Command::new(env!("CARGO_BIN_EXE_balance"))
        .args(["--only", "rush", "--data"])
        .output()
        .expect("binary runs");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--data needs a value"));
}
