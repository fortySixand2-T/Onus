//! `balance` — the headless balance batch (B2).
//!
//! Plays every ordered matchup of the shipped strategies (mirrors included)
//! across K seeds, each match running to its decision or to a tick cap, and
//! prints a human summary. A thin wrapper: the loop is [`onus::batch`], which
//! is where the behaviour is tested.
//!
//! Every matchup is played in **both spawn orientations** on the same seed, so
//! the batch is `N x N x K x 2` matches and a left-hand-base advantage cannot
//! be read as strategy strength. The summary prints the raw per-orientation
//! counts next to the balanced aggregate.
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
//! - `--help`         print this usage on stdout and exit successfully
//!
//! `--tick-cap` and `--minutes` set the same budget: **the last one on the
//! command line wins**. Out-of-range numbers are refused outright rather than
//! truncated or wrapped — a cap that was silently reduced would be reported as
//! if it had been enforced.
//!
//! Progress goes to **stderr** (a hundred matches is a long silence otherwise);
//! the summary goes to stdout. The machine-readable `balance_report.ron` is a
//! later checkbox — nothing here writes a file.

use std::process::ExitCode;

use onus::batch::{self, BatchSettings, MatchRecord, MatchResult, Tally};
use onus::headless::{self, Orientation, SIM_HZ};

fn usage() -> &'static str {
    "usage: balance [--seeds K] [--seed-base N] [--tick-cap T] [--minutes M] [--only a,b,c] [--help]\n\
     \n\
     every matchup is played in both spawn orientations, so the batch is\n\
     N x N x K x 2 matches.\n\
     --tick-cap and --minutes set the same budget: the last one given wins.\n\
     out-of-range values are refused, never truncated."
}

/// What the command line asked for. `--help` is a *request*, not an error: it
/// succeeds on stdout, while a mistyped flag still fails on stderr.
enum Request {
    Help,
    Run(Box<BatchSettings>),
}

/// Parse argv into a [`Request`]. Every flag is refused rather than guessed: a
/// typo that silently halves the batch would be a wrong number reported as a
/// right one, and so would a count that wrapped or truncated on its way into a
/// `u32`.
fn parse(args: &[String]) -> Result<Request, String> {
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
        // A count that does not fit a u32 is refused, not truncated: `--seeds
        // 4294967297` must not quietly run one seed and report that it ran
        // 4294967297.
        let count = |what: &str| -> Result<u32, String> {
            let n = number(what)?;
            u32::try_from(n).map_err(|_| format!("{what}: {n} is out of range (max {})", u32::MAX))
        };
        match flag {
            "--seeds" => settings.seeds = count("--seeds")?,
            "--seed-base" => settings.seed_base = number("--seed-base")?,
            "--tick-cap" => settings.tick_cap = count("--tick-cap")?,
            "--minutes" => {
                // `M * 60 * SIM_HZ` overflows a u32 for M above ~1.2 million:
                // in debug that panics, in release it wraps to a cap the run
                // would then claim to have enforced. Compute wide, then refuse.
                let m = number("--minutes")?;
                let ticks = m
                    .checked_mul(60)
                    .and_then(|s| s.checked_mul(SIM_HZ as u64))
                    .and_then(|t| u32::try_from(t).ok())
                    .ok_or_else(|| {
                        format!(
                            "--minutes: {m} is out of range (max {})",
                            u32::MAX / (60 * SIM_HZ)
                        )
                    })?;
                settings.tick_cap = ticks;
            }
            "--only" => {
                settings.only = Some(value()?.split(',').map(|s| s.trim().to_string()).collect())
            }
            "--help" | "-h" => return Ok(Request::Help),
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
    Ok(Request::Run(Box::new(settings)))
}

/// A percentage in parentheses, or nothing at all when there is no rate to
/// print — an empty sample must not be reported as 0%.
fn rate(label: &str, r: Option<f32>) -> String {
    match r {
        Some(r) => format!("{label}{:.1}%)", 100.0 * r),
        None => String::new(),
    }
}

fn mmss(ticks: u32) -> String {
    let secs = ticks / SIM_HZ;
    format!("{}:{:02}", secs / 60, secs % 60)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let settings = match parse(&args) {
        Ok(Request::Help) => {
            println!("{}", usage());
            return ExitCode::SUCCESS;
        }
        Ok(Request::Run(s)) => *s,
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

    let total =
        roster.len() * roster.len() * settings.seeds as usize * Orientation::ALL.len();
    eprintln!(
        "balance: {} strategies x {} seeds x {} orientations = {total} matches, cap {} ticks ({})",
        roster.len(),
        settings.seeds,
        Orientation::ALL.len(),
        settings.tick_cap,
        mmss(settings.tick_cap),
    );

    let mut done = 0usize;
    let records = match batch::run_batch(&content, &settings, &mut |r| {
        done += 1;
        eprintln!(
            "[{done}/{total}] {} vs {} seed {} [{}] -> {} in {} ticks ({})",
            r.strategies[0],
            r.strategies[1],
            r.seed,
            r.orientation.name(),
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
    println!("spawn        left {} right {}{}",
        t.spawn_wins[0],
        t.spawn_wins[1],
        rate(" (left ", t.left_spawn_rate()),
    );
    for o in Orientation::ALL {
        let s = &t.by_orientation[o.index()];
        println!(
            "  {:<8}   A {} B {} draws {} timeouts {} of {}",
            o.name(),
            s.wins[0],
            s.wins[1],
            s.mutual_losses,
            s.timeouts,
            s.total
        );
    }
    let mirrors: Vec<MatchRecord> = records
        .iter()
        .filter(|r| r.strategies[0] == r.strategies[1])
        .cloned()
        .collect();
    if !mirrors.is_empty() {
        // The side-balance reading: a mirror is the same script on both sides,
        // so a slot split away from 50% here is spawn or turn-order bias, not
        // strategy. Both orientations are played, so the aggregate is the
        // corrected figure and the per-orientation lines above it are the raw
        // asymmetry.
        let m = Tally::of(&mirrors);
        println!(
            "mirrors      {} matches, A {} B {}{} | spawn left {} right {}{}",
            m.total,
            m.wins[0],
            m.wins[1],
            rate(" (A ", m.slot_a_rate()),
            m.spawn_wins[0],
            m.spawn_wins[1],
            rate(" (left ", m.left_spawn_rate()),
        );
        for o in Orientation::ALL {
            let s = &m.by_orientation[o.index()];
            println!(
                "  {:<8}   A {} B {} draws {} timeouts {} of {}",
                o.name(),
                s.wins[0],
                s.wins[1],
                s.mutual_losses,
                s.timeouts,
                s.total
            );
        }
    }
    // What the batch actually *built*, per unit — the check that the probe set
    // fields the units it claims to. A strategy that never produces its own
    // unit is a row of zeros here, visible before any win rate is computed.
    let produced = batch::production_totals(&records);
    if !produced.is_empty() {
        let total: u32 = produced.iter().map(|(_, n)| n).sum();
        println!("produced     {total} units (both sides, whole batch)");
        for (id, n) in &produced {
            let share = if total > 0 {
                format!(" ({:.1}%)", 100.0 * *n as f32 / total as f32)
            } else {
                String::new()
            };
            println!("  {id:<10} {n}{share}");
        }
    }
    if t.total > 0 && t.timeouts == t.total {
        println!(
            "WARNING: every match hit the cap. This batch measures nothing about \
             balance — no matchup was decided."
        );
    }
    ExitCode::SUCCESS
}
