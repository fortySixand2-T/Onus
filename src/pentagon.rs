//! The pentagon assertion (B3): does the designed counter cycle actually hold?
//!
//! DESIGN_BRIEF says `Sentinel > Ripper > Arclight > Bulwark > Ravager >
//! Sentinel`. This module never says that. It **derives** the cycle by walking
//! each unit's `nemesis` link in `units.ron`, maps each unit of the cycle to
//! the strategy that masses it, and reads that pair's [`WinMatrix`] cell. A
//! pentagon hardcoded in Rust would keep passing after someone edited the RON —
//! the one failure that would make this whole assertion worthless (F-025).
//!
//! Pure: a function of (`&Content`, `&WinMatrix`). No `App`, no stepping, no
//! wall-clock, no map iteration — the link order is the walk of the cycle from
//! a canonical start (the first nemesis-bearing unit in RON order), so a report
//! is a stable, comparable artifact that B4 can diff across tuning passes.
//!
//! ## It reports; it does not gate
//!
//! PASS/FAIL against the kill criteria is a later checkbox. Here every link
//! carries its own [`Verdict`], and the four states are kept apart on purpose:
//!
//! - [`Verdict::Holds`] — the predator wins, and the **sample says so**: its
//!   rate is above 0.5 and the whole 95% interval around it is too. A dead-even
//!   matchup is not a counter.
//! - [`Verdict::Fails`] — the predator loses and the whole interval is below
//!   0.5. That is the sim doing its job: the stats or the nemesis magnitude are
//!   wrong.
//! - [`Verdict::Undetermined`] — a rate exists, but its interval **straddles**
//!   0.5: this many matches cannot tell the link from a coin flip. Distinct
//!   from `Fails`, and that distinction is the whole point (F-031): at 8
//!   decided matches the 95% half-width is 28 points, so a bare `rate > 0.5`
//!   records a coin flip as a broken design — which it did, twice, in F-025 and
//!   F-030 for a link that reproduces at 64-72% once the sample is 100.
//! - [`Verdict::Undefined`] — nothing was **decided** (an unplayed or
//!   all-timeout matchup): there is no rate at all, which is a different thing
//!   from a rate too noisy to read. Not 0.5, and not a hold: an all-timeout run
//!   must be flagged, not reported as balanced.
//! - [`Verdict::NoStrategy`] — the instrument is incomplete: some unit of the
//!   cycle has no strategy that masses it, so the matchup was never measurable.
//!   A gap in the probe set, not a reading about the game.

use std::fmt;

use crate::batch::MatchRecord;
use crate::metrics::WinMatrix;
use crate::sim::content::{Content, StrategyDef};

/// Why a roster's `nemesis` links do not form one closed cycle over every
/// nemesis-bearing unit. Reported, never panicked on: malformed content is a
/// finding about the data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CycleError {
    /// No unit names a nemesis at all — there is no pentagon to assert.
    NoNemesis,
    /// A unit is its own prey.
    SelfNemesis(String),
    /// A unit's `nemesis` names something that is not a unit.
    UnknownPrey { unit: String, prey: String },
    /// The walk reached a unit with no nemesis: the chain does not close.
    Open { at: String },
    /// The walk re-entered the chain somewhere other than its start — a lasso,
    /// not a cycle.
    Lasso { at: String },
    /// The walk closed, but left nemesis-bearing units out: several disjoint
    /// cycles, not one pentagon.
    Partial {
        /// The closed cycle the walk found, from the canonical start.
        cycle: Vec<String>,
        /// Nemesis-bearing units it does not contain, in RON order.
        missing: Vec<String>,
    },
}

impl fmt::Display for CycleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CycleError::NoNemesis => write!(f, "no unit names a nemesis: there is no counter cycle"),
            CycleError::SelfNemesis(u) => write!(f, "unit `{u}` is its own nemesis"),
            CycleError::UnknownPrey { unit, prey } => {
                write!(f, "unit `{unit}` names unknown nemesis `{prey}`")
            }
            CycleError::Open { at } => {
                write!(f, "the nemesis chain does not close: `{at}` has no nemesis")
            }
            CycleError::Lasso { at } => {
                write!(f, "the nemesis chain re-enters itself at `{at}`, forming a lasso")
            }
            CycleError::Partial { cycle, missing } => write!(
                f,
                "the nemesis chain closes over {:?} but leaves {:?} out",
                cycle, missing
            ),
        }
    }
}

impl std::error::Error for CycleError {}

/// The counter cycle stated by `content`'s `nemesis` links: unit ids, each
/// predator followed by its prey, starting at the **first nemesis-bearing unit
/// in RON order** and closing back onto it.
///
/// Every nemesis-bearing unit must appear exactly once, or the roster does not
/// state one closed pentagon and the reason comes back as a [`CycleError`].
pub fn nemesis_cycle(content: &Content) -> Result<Vec<String>, CycleError> {
    let prey_of = |id: &str| -> Option<&str> {
        content
            .units
            .iter()
            .find(|u| u.id == id)
            .and_then(|u| u.nemesis.as_deref())
    };
    // Every stated link is checked first, in RON order, so a malformed link is
    // named even when the walk would never reach it.
    for u in &content.units {
        let Some(prey) = u.nemesis.as_deref() else {
            continue;
        };
        if prey == u.id {
            return Err(CycleError::SelfNemesis(u.id.clone()));
        }
        if !content.units.iter().any(|other| other.id == prey) {
            return Err(CycleError::UnknownPrey {
                unit: u.id.clone(),
                prey: prey.to_string(),
            });
        }
    }
    let start = content
        .units
        .iter()
        .find(|u| u.nemesis.is_some())
        .ok_or(CycleError::NoNemesis)?
        .id
        .clone();

    let mut cycle = vec![start.clone()];
    loop {
        let current = cycle.last().expect("non-empty").clone();
        let Some(prey) = prey_of(&current) else {
            return Err(CycleError::Open { at: current });
        };
        if prey == start {
            break;
        }
        if cycle.iter().any(|c| c == prey) {
            return Err(CycleError::Lasso { at: prey.to_string() });
        }
        cycle.push(prey.to_string());
    }
    let missing: Vec<String> = content
        .units
        .iter()
        .filter(|u| u.nemesis.is_some() && !cycle.iter().any(|c| c == &u.id))
        .map(|u| u.id.clone())
        .collect();
    if !missing.is_empty() {
        return Err(CycleError::Partial { cycle, missing });
    }
    Ok(cycle)
}

/// The strategy that masses `unit`: the first strategy in RON order whose army
/// build order names `unit` and nothing else.
///
/// **By build order, never by name.** A strategy called `mass_bulwark` that
/// built rippers would be a silent mislabel, and matching the substring
/// `bulwark` would find it; reading the army cannot.
pub fn mass_strategy<'c>(content: &'c Content, unit: &str) -> Option<&'c StrategyDef> {
    content
        .strategies
        .iter()
        .find(|s| !s.army.is_empty() && s.army.iter().all(|item| item.unit == unit))
}

/// What one predicted counter did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The predator's rate over its prey is above 0.5 and so is the whole 95%
    /// interval around it.
    Holds,
    /// The predator's rate is at or below 0.5 and the whole 95% interval is
    /// **below** 0.5 — the predicted counter demonstrably does not win.
    Fails,
    /// A rate exists, but its 95% interval contains 0.5: the sample cannot tell
    /// this link from a coin flip. Not a hold, and **not** a failed design.
    Undetermined,
    /// Nothing was decided: no rate exists at all. Never 0.5.
    Undefined,
    /// One of the two units has no strategy that masses it: unmeasurable.
    NoStrategy,
}

impl Verdict {
    /// How this reads in one word, for a table.
    pub fn label(self) -> &'static str {
        match self {
            Verdict::Holds => "holds",
            Verdict::Fails => "FAILS",
            Verdict::Undetermined => "undetermined",
            Verdict::Undefined => "undefined",
            Verdict::NoStrategy => "no probe",
        }
    }
}

/// One link of the cycle: a predicted counter, the matchup that tests it, and
/// what that matchup said.
#[derive(Clone, Debug, PartialEq)]
pub struct Link {
    /// Unit id of the predicted counter.
    pub predator: String,
    /// Unit id of its prey.
    pub prey: String,
    /// The strategy massing the predator, if the probe set has one.
    pub predator_strategy: Option<String>,
    /// The strategy massing the prey, if the probe set has one.
    pub prey_strategy: Option<String>,
    /// `W[predator_strategy][prey_strategy]` — the **predator's** rate, never
    /// the transpose.
    pub rate: Option<f64>,
    /// The 95% Wilson interval around [`Link::rate`] — how wide the reading is,
    /// carried beside it so a verdict can never be quoted without its error.
    pub interval: Option<(f64, f64)>,
    /// Decided matches behind the rate.
    pub n_decided: u32,
    /// Matches of this pair that hit the tick cap. Not in the rate.
    pub n_timeout: u32,
    pub verdict: Verdict,
}

impl fmt::Display for Link {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let rate = match self.rate {
            Some(r) => format!("{:>5.1}%", 100.0 * r),
            None => "   -- ".to_string(),
        };
        // The interval prints with the rate, always: a pentagon table that
        // quoted a percentage without its width is what F-031 is about.
        let ci = match self.interval {
            Some((lo, hi)) => format!(" [{:.1}, {:.1}]", 100.0 * lo, 100.0 * hi),
            None => String::new(),
        };
        write!(
            f,
            "{:>8} > {:<8} {rate}{ci}  {:>3} decided, {} timeouts  {}",
            self.predator,
            self.prey,
            self.n_decided,
            self.n_timeout,
            self.verdict.label(),
        )
    }
}

/// The pentagon assertion's result: one [`Link`] per predicted counter, in
/// cycle order from the canonical start.
#[derive(Clone, Debug, PartialEq)]
pub struct PentagonReport {
    links: Vec<Link>,
}

impl PentagonReport {
    /// Derive the cycle from `content` and read each link off `matrix`.
    pub fn of(content: &Content, matrix: &WinMatrix) -> Result<Self, CycleError> {
        let cycle = nemesis_cycle(content)?;
        let links = (0..cycle.len())
            .map(|i| {
                let predator = cycle[i].clone();
                let prey = cycle[(i + 1) % cycle.len()].clone();
                Link::read(content, matrix, predator, prey)
            })
            .collect();
        Ok(PentagonReport { links })
    }

    /// The same, straight from a batch's records.
    pub fn of_records(content: &Content, records: &[MatchRecord]) -> Result<Self, CycleError> {
        Self::of(content, &WinMatrix::of(records))
    }

    /// Every link, in cycle order.
    pub fn links(&self) -> &[Link] {
        &self.links
    }

    fn count(&self, v: Verdict) -> usize {
        self.links.iter().filter(|l| l.verdict == v).count()
    }

    /// Links whose predicted counter actually won its matchup.
    pub fn holding(&self) -> usize {
        self.count(Verdict::Holds)
    }

    /// Links whose predicted counter did **not** win — the findings B4 tunes.
    pub fn failing(&self) -> usize {
        self.count(Verdict::Fails)
    }

    /// Links whose interval straddles 0.5 — read, but not readable as either a
    /// counter or a broken one. These are a **sample-size** report, not a design
    /// one: more seeds move them, retuning need not.
    pub fn undetermined(&self) -> usize {
        self.count(Verdict::Undetermined)
    }

    /// Links with no decided match behind them.
    pub fn undefined(&self) -> usize {
        self.count(Verdict::Undefined)
    }

    /// Links that could not be measured because a unit has no mass strategy.
    pub fn gaps(&self) -> usize {
        self.count(Verdict::NoStrategy)
    }

    /// Cycle units with no strategy massing them, in cycle order — the probe
    /// set's gaps, named.
    pub fn missing_strategies(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for l in &self.links {
            for (unit, strategy) in [
                (&l.predator, &l.predator_strategy),
                (&l.prey, &l.prey_strategy),
            ] {
                if strategy.is_none() && !out.iter().any(|u| u == unit) {
                    out.push(unit.clone());
                }
            }
        }
        out
    }

    /// Did the whole designed cycle reproduce? Only if every link holds — an
    /// undefined or unmeasured link is not a hold.
    pub fn all_hold(&self) -> bool {
        !self.links.is_empty() && self.holding() == self.links.len()
    }
}

impl fmt::Display for PentagonReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for l in &self.links {
            writeln!(f, "  {l}")?;
        }
        write!(
            f,
            "  {} of {} predicted counters hold",
            self.holding(),
            self.links.len()
        )
    }
}

impl Link {
    fn read(content: &Content, matrix: &WinMatrix, predator: String, prey: String) -> Link {
        let id_of = |unit: &str| mass_strategy(content, unit).map(|s| s.id.clone());
        let predator_strategy = id_of(&predator);
        let prey_strategy = id_of(&prey);
        // The cell is read predator-row, prey-column: the predator's win rate
        // over its prey. Transposing it is the one thing this whole module is
        // easiest to get backwards.
        let cell = match (&predator_strategy, &prey_strategy) {
            (Some(p), Some(q)) => matrix.get(p, q).copied(),
            _ => None,
        };
        let rate = cell.and_then(|c| c.rate());
        let interval = cell.and_then(|c| c.wilson_interval());
        // The interval decides, not the point estimate: a cell whose interval
        // contains 0.5 is a cell this many matches cannot read, and calling it
        // `Fails` would publish sampling noise as a design verdict (F-031).
        let verdict = match (&predator_strategy, &prey_strategy, interval) {
            (None, _, _) | (_, None, _) => Verdict::NoStrategy,
            (_, _, None) => Verdict::Undefined,
            (_, _, Some((lo, _))) if lo > 0.5 => Verdict::Holds,
            (_, _, Some((_, hi))) if hi < 0.5 => Verdict::Fails,
            _ => Verdict::Undetermined,
        };
        Link {
            predator,
            prey,
            predator_strategy,
            prey_strategy,
            rate,
            interval,
            n_decided: cell.map(|c| c.n_decided).unwrap_or(0),
            n_timeout: cell.map(|c| c.n_timeout).unwrap_or(0),
            verdict,
        }
    }
}
