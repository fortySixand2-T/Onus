//! `balance` — the headless balance batch (B2).
//!
//! Plays every ordered matchup of the shipped strategies (mirrors included)
//! across K seeds, each match running to its decision or to a tick cap, and
//! prints a human summary. A thin wrapper: the loop is [`onus::batch`], which
//! is where the behaviour is tested.
//!
//! ```text
//! balance [--seeds K] [--seed-base N] [--tick-cap T] [--minutes M] [--only a,b,c]
//! ```
//!
//! - `--seeds K`      how many seeds every matchup is played on (default 1)
//! - `--seed-base N`  the base the per-match seeds derive from (default 0)
//! - `--tick-cap T`   per-match tick budget (default 8 min at 60 Hz = 28_800)
//! - `--minutes M`    the same budget expressed in minutes of play
//! - `--only a,b,c`   restrict the roster; an unknown id is refused, not ignored
//!
//! Progress goes to **stderr** (a hundred matches is a long silence otherwise);
//! the summary goes to stdout. The machine-readable `balance_report.ron` is a
//! later checkbox — nothing here writes a file.

use std::process::ExitCode;

use onus::batch::{self, BatchSettings, MatchResult, Tally};
use onus::headless::{self, SIM_HZ};

fn usage() -> &'static str {
    "usage: balance [--seeds K] [--seed-base N] [--tick-cap T] [--minutes M] [--only a,b,c]"
}

/// Parse argv into [`BatchSettings`]. Every flag is refused rather than
/// guessed: a typo that silently halves the batch would be a wrong number
/// reported as a right one.
fn parse(args: &[String]) -> Result<BatchSettings, String> {
    let mut settings = BatchSettings::default();
    let mut i = 0;
    while i < args.len() {
        let flag = args[i].as_str();
        let value = || {
            args.get(i + 1)
                .cloned()
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        let number = |what: &str| -> Result<u64, String> {
            value()?.parse::<u64>().map_err(|e| format!("{what}: {e}"))
        };
        match flag {
            "--seeds" => settings.seeds = number("--seeds")? as u32,
            "--seed-base" => settings.seed_base = number("--seed-base")?,
            "--tick-cap" => settings.tick_cap = number("--tick-cap")? as u32,
            "--minutes" => settings.tick_cap = number("--minutes")? as u32 * 60 * SIM_HZ,
            "--only" => {
                settings.only = Some(value()?.split(',').map(|s| s.trim().to_string()).collect())
            }
            "--help" | "-h" => return Err(usage().to_string()),
            other => return Err(format!("unknown flag `{other}`\n{}", usage())),
        }
        i += 2;
    }
    if settings.seeds == 0 {
        return Err("--seeds must be at least 1".to_string());
    }
    if settings.tick_cap == 0 {
        return Err("--tick-cap must be at least 1".to_string());
    }
    Ok(settings)
}

fn mmss(ticks: u32) -> String {
    let secs = ticks / SIM_HZ;
    format!("{}:{:02}", secs / 60, secs % 60)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let settings = match parse(&args) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };

    let content = match headless::content() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("content: {e}");
            return ExitCode::FAILURE;
        }
    };
    let roster = match batch::roster(&content, settings.only.as_deref()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };

    let total = roster.len() * roster.len() * settings.seeds as usize;
    eprintln!(
        "balance: {} strategies x {} seeds = {total} matches, cap {} ticks ({})",
        roster.len(),
        settings.seeds,
        settings.tick_cap,
        mmss(settings.tick_cap),
    );

    let mut done = 0usize;
    let records = match batch::run_batch(&content, &settings, &mut |r| {
        done += 1;
        eprintln!(
            "[{done}/{total}] {} vs {} seed {} -> {} in {} ticks ({})",
            r.strategies[0],
            r.strategies[1],
            r.seed,
            match r.result {
                MatchResult::Decided(f) => format!("{f:?} wins"),
                MatchResult::MutualLoss => "draw (both HQs)".to_string(),
                MatchResult::Timeout => "timeout".to_string(),
            },
            r.ticks,
            mmss(r.ticks),
        );
    }) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };

    let t = Tally::of(&records);
    println!("matches      {}", t.total);
    println!("  A wins     {}", t.wins[0]);
    println!("  B wins     {}", t.wins[1]);
    println!(
        "  draws      {} (both HQs fell on one tick)",
        t.mutual_losses
    );
    println!(
        "  timeouts   {} ({:.0}% hit the {} cap)",
        t.timeouts,
        100.0 * t.timeout_rate(),
        mmss(settings.tick_cap)
    );
    if let Some(median) = t.median_length() {
        let q = |x| t.length_quantile(x).map(mmss).unwrap_or_default();
        println!(
            "length       min {} p25 {} median {} p75 {} max {}",
            q(0.0),
            q(0.25),
            mmss(median),
            q(0.75),
            q(1.0),
        );
    }
    if t.total > 0 && t.timeouts == t.total {
        println!(
            "WARNING: every match hit the cap. This batch measures nothing about \
             balance — no matchup was decided."
        );
    }
    ExitCode::SUCCESS
}
