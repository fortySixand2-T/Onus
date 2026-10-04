//! `balance_report.ron` (B3): one batch's whole reading, machine-readable.
//!
//! A [`BalanceReport`] is a pure function of `(content, settings, records)`:
//! the win matrix with every cell's sample and interval, the pentagon verdicts
//! with their intervals, the match-length distribution on both bases, the
//! production totals, and the kill-criteria gate with every number behind each
//! status. The balance binary prints it and writes it as RON; a later run, or
//! a tool, reads it back with [`BalanceReport::from_ron`] and gets the same
//! value — that round trip is tested, and nothing in it depends on a map's
//! iteration order: every list is in matrix (first-appearance) or cycle
//! order.
//!
//! The file is a **build artifact** (gitignored): it describes one run on one
//! content fingerprint, which it carries, so a report can never be quoted
//! against data it was not computed from.

use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::batch::{self, BatchSettings, MatchRecord, Tally};
use crate::gate::{GateSpec, KillGate, Reading, Status};
use crate::headless::SIM_HZ;
use crate::metrics::{LengthDistribution, LengthSummary, WinMatrix};
use crate::pentagon::{Link, PentagonReport};
use crate::sim::content::Content;

/// Bumped whenever a field changes meaning or shape.
pub const REPORT_SCHEMA: u32 = 1;

/// The conventional name for the report (`balance --report balance_report.ron`,
/// gitignored). The binary writes a report only when given `--report PATH`.
pub const DEFAULT_REPORT_PATH: &str = "balance_report.ron";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BalanceReport {
    pub schema: u32,
    /// [`Content::fingerprint`]'s hash and its human summary.
    pub content_hash: u64,
    pub content_summary: String,
    pub settings: ReportSettings,
    pub outcomes: Outcomes,
    pub matrix: MatrixReport,
    pub pentagon: PentagonSection,
    pub length: LengthSummary,
    /// Units built over the whole batch, both sides, by unit id.
    pub production: Vec<(String, u32)>,
    pub gate: KillGate,
}

/// The batch that was run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReportSettings {
    pub seeds: u32,
    pub seed_base: u64,
    pub tick_cap: u32,
    pub only: Option<Vec<String>>,
}

/// Raw outcome counts ([`Tally`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Outcomes {
    pub matches: u32,
    pub a_wins: u32,
    pub b_wins: u32,
    pub mutual_losses: u32,
    pub timeouts: u32,
    pub left_wins: u32,
    pub right_wins: u32,
}

/// One matrix cell: the counts, and the rate and interval they give.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CellReport {
    pub half_wins: u32,
    pub n_decided: u32,
    pub n_timeout: u32,
    pub rate: Option<f64>,
    pub interval: Option<(f64, f64)>,
}

/// One row's strength: the row mean over its defined off-diagonal cells.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RowReport {
    pub strategy: String,
    pub mean: Option<f64>,
    pub cells: u32,
}

/// The win matrix. `cells[i][j]` is `ids[i]`'s record against `ids[j]`; the
/// diagonal is the slot-A share of the mirror.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MatrixReport {
    pub ids: Vec<String>,
    pub cells: Vec<Vec<CellReport>>,
    pub row_means: Vec<RowReport>,
}

/// The pentagon: every link with its interval and verdict, or why there is no
/// cycle to assert.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PentagonSection {
    pub links: Vec<Link>,
    pub holding: u32,
    pub cycle_error: Option<String>,
}

impl BalanceReport {
    pub fn of(content: &Content, settings: &BatchSettings, records: &[MatchRecord]) -> Self {
        Self::with_spec(content, settings, records, &GateSpec::default())
    }

    pub fn with_spec(
        content: &Content,
        settings: &BatchSettings,
        records: &[MatchRecord],
        spec: &GateSpec,
    ) -> Self {
        let t = Tally::of(records);
        let m = WinMatrix::of(records);
        let fp = content.fingerprint();
        let n = m.len();
        let cells = (0..n)
            .map(|i| {
                (0..n)
                    .map(|j| {
                        let c = m.cell(i, j).copied().unwrap_or_default();
                        CellReport {
                            half_wins: c.half_wins,
                            n_decided: c.n_decided,
                            n_timeout: c.n_timeout,
                            rate: c.rate(),
                            interval: c.wilson_interval(),
                        }
                    })
                    .collect()
            })
            .collect();
        let row_means = m
            .row_means()
            .into_iter()
            .map(|(strategy, r)| RowReport {
                strategy,
                mean: r.mean,
                cells: r.cells as u32,
            })
            .collect();
        let pentagon = match PentagonReport::of(content, &m) {
            Ok(p) => PentagonSection {
                holding: p.holding() as u32,
                links: p.links().to_vec(),
                cycle_error: None,
            },
            Err(e) => PentagonSection {
                links: Vec::new(),
                holding: 0,
                cycle_error: Some(format!("{e}")),
            },
        };
        BalanceReport {
            schema: REPORT_SCHEMA,
            content_hash: fp.hash(),
            content_summary: fp.summary().to_string(),
            settings: ReportSettings {
                seeds: settings.seeds,
                seed_base: settings.seed_base,
                tick_cap: settings.tick_cap,
                only: settings.only.clone(),
            },
            outcomes: Outcomes {
                matches: t.total as u32,
                a_wins: t.wins[0] as u32,
                b_wins: t.wins[1] as u32,
                mutual_losses: t.mutual_losses as u32,
                timeouts: t.timeouts as u32,
                left_wins: t.spawn_wins[0] as u32,
                right_wins: t.spawn_wins[1] as u32,
            },
            matrix: MatrixReport {
                ids: m.ids().to_vec(),
                cells,
                row_means,
            },
            pentagon,
            length: LengthDistribution::of(records, spec.band).summary(),
            production: batch::production_totals(records),
            gate: KillGate::of(content, records, spec),
        }
    }

    /// Pretty RON, deterministic for a given report.
    pub fn to_ron(&self) -> Result<String, ron::Error> {
        ron::ser::to_string_pretty(self, ron::ser::PrettyConfig::default())
    }

    pub fn from_ron(s: &str) -> Result<Self, ron::error::SpannedError> {
        ron::from_str(s)
    }

    /// Write the RON to `path` atomically: a sibling temp file, then a rename,
    /// so a reader never sees half a report and a failed write never leaves a
    /// truncated one where the last good report was.
    pub fn write(&self, path: &Path) -> std::io::Result<()> {
        let text = self
            .to_ron()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        // Process-unique, so two runs writing the same path never share a
        // temp file; the rename is the only step a reader can observe.
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(format!(".{}.tmp", std::process::id()));
        let tmp = std::path::PathBuf::from(tmp);
        let result = std::fs::write(&tmp, text).and_then(|()| std::fs::rename(&tmp, path));
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        result
    }

    pub fn read(path: &Path) -> std::io::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        Self::from_ron(&text).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }
}

fn mmss(ticks: u32) -> String {
    let secs = ticks / SIM_HZ;
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// A whole percentage for a threshold (`65%`), so a threshold never prints
/// like a measured rate.
fn pct0(x: f64) -> String {
    format!("{:.0}%", 100.0 * x)
}

/// A reading as `value [lo, hi] n=N n_eff=E`, or `--` with no data.
fn reading(r: &Reading) -> String {
    match (r.value, r.interval) {
        (Some(v), Some((lo, hi))) => format!(
            "{:.1}% [{:.1}, {:.1}]  n {} in {} clusters, n_eff {:.0}",
            100.0 * v,
            100.0 * lo,
            100.0 * hi,
            r.n,
            r.clusters,
            r.n_eff
        ),
        _ => format!("--  n {}", r.n),
    }
}

fn status(s: Status) -> &'static str {
    s.label()
}

/// The two sections the binary prints after the matrix and the pentagon:
/// match length, then the kill-criteria gate. Every line is indented or
/// starts with a section word — no line begins with `[`, which is the
/// matrix's row marker.
impl fmt::Display for BalanceReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let l = &self.length;
        writeln!(
            f,
            "length       target {}-{} (inclusive); decided-only and all-match bases are separate",
            mmss(l.band.min_ticks),
            mmss(l.band.max_ticks)
        )?;
        writeln!(
            f,
            "  decided    {} of {}: below {} in {} above {}",
            l.decided, l.total, l.below_band, l.in_band, l.above_band
        )?;
        let share = |r: Option<f64>| r.map(|x| format!("{:.1}%", 100.0 * x)).unwrap_or_else(|| "--".into());
        writeln!(f, "  in band    {} of decided", share(l.band_share))?;
        writeln!(f, "  timeouts   {} of all matches", share(l.timeout_rate))?;
        let qs = |v: &[(u32, Option<u32>)]| -> String {
            v.iter()
                .map(|(p, t)| format!(" p{p} {}", t.map(mmss).unwrap_or_else(|| "--".into())))
                .collect()
        };
        writeln!(f, "  decided   {}", qs(&l.decided_quantiles))?;
        writeln!(f, "  all       {}  (timeouts at the cap)", qs(&l.all_quantiles))?;

        let g = &self.gate;
        writeln!(f, "kill gate    {}  (status read off clustered 95% Wilson intervals; F-038)", status(g.status))?;
        let k1 = &g.strength;
        writeln!(
            f,
            "  K1 strength  {}  no row mean above {} regardless of counter",
            status(k1.status),
            pct0(k1.max_strength)
        )?;
        let w = k1.rows.iter().map(|r| r.strategy.len()).max().unwrap_or(0).max(6);
        for r in &k1.rows {
            let mut tags = String::new();
            if r.dominant {
                tags.push_str("  DOMINANT");
            }
            if r.losing {
                tags.push_str("  LOSING");
            }
            writeln!(
                f,
                "    {:<w$}  {:<12} {}  over {} of {} opponents{tags}",
                r.strategy,
                status(r.strength.status),
                reading(&r.strength),
                r.cells,
                r.opponents,
            )?;
        }
        for (label, names) in [("failing", &k1.failing), ("dominant", &k1.dominant), ("losing", &k1.losing)] {
            if !names.is_empty() {
                writeln!(f, "    {label}: {}", names.join(", "))?;
            }
        }
        let k2 = &g.seat;
        writeln!(
            f,
            "  K2 seat      {}  mirrors within 50% +/- {} by slot and by spawn, pooled and per mirror",
            status(k2.status),
            pct0(k2.tolerance)
        )?;
        writeln!(f, "    slot A      {:<12} {}", status(k2.slot_a.status), reading(&k2.slot_a))?;
        writeln!(f, "    left base   {:<12} {}", status(k2.left_spawn.status), reading(&k2.left_spawn))?;
        for m in &k2.mirrors {
            writeln!(
                f,
                "    mirror {:<w$}  slot A {} {}  left base {} {}",
                m.strategy,
                status(m.slot_a.status),
                reading(&m.slot_a),
                status(m.left_spawn.status),
                reading(&m.left_spawn)
            )?;
        }
        writeln!(
            f,
            "    (a mirror resolved outside tolerance FAILs K2; an undetermined one does not block PASS)"
        )?;
        let k3 = &g.termination;
        writeln!(
            f,
            "  K3 length    {}  at least {} of decided matches end in {}-{}, at most {} timeouts",
            status(k3.status),
            pct0(k3.min_band_share),
            mmss(k3.band.min_ticks),
            mmss(k3.band.max_ticks),
            pct0(k3.max_timeout_rate)
        )?;
        writeln!(
            f,
            "    band share    {:<12} {}  (at least {} of decided)",
            status(k3.band_share.status),
            reading(&k3.band_share),
            pct0(k3.min_band_share)
        )?;
        writeln!(f, "    timeouts      {:<12} {}", status(k3.timeout_rate.status), reading(&k3.timeout_rate))?;
        writeln!(
            f,
            "    before band   {:<12} {}  (of all; reported, not gated)",
            status(k3.below.status),
            reading(&k3.below)
        )?;
        writeln!(
            f,
            "    after/capped  {:<12} {}  (of all; reported, not gated)",
            status(k3.beyond.status),
            reading(&k3.beyond)
        )?;
        writeln!(
            f,
            "    decided median {}",
            k3.decided_median.map(mmss).unwrap_or_else(|| "--".into())
        )?;
        if k3.all_timeout {
            writeln!(f, "    ALL TIMEOUT: every match hit the cap; nothing about balance was measured")?;
        }
        Ok(())
    }
}
