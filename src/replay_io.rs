//! Writing a match's command log to disk — **driver-side, not sim** (Phase 2).
//!
//! M5 gave the sim a replay log and the ability to save and load one; nothing
//! shipped ever wrote one, so the feature was reachable only from tests. This
//! module is the shipped writer, and it lives outside `sim` on purpose:
//!
//! - **it reads a wall clock.** A filename wants a timestamp; sim logic may
//!   never read a clock (F-003). The clock is here, the sim cannot see it, and
//!   the name it produces never enters sim state, never reaches a hash and
//!   never changes what the sim does — pinned by test, not by care;
//! - **it touches the filesystem** on a schedule of its own, driven by *what
//!   the sim decided* rather than by anything the sim asks for.
//!
//! The sim's side of the contract is untouched: the log is
//! [`sim::replay::CommandLog`], and what goes to disk goes through
//! [`MatchLog::to_ron`], which refuses a log this build could not read back.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use bevy::prelude::*;
use serde::Deserialize;

use crate::sim::replay::CommandLog;
use crate::sim::MatchState;

/// Where the shipped app puts replay logs. Content-as-data, in
/// `assets/data/replay.ron` — see that file for why it is not part of
/// [`Content`](crate::sim::content::Content).
#[derive(Resource, Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ReplayConfig {
    /// Off by default: a log per run grows without bound.
    pub enabled: bool,
    pub dir: String,
    pub prefix: String,
    /// How many already-taken filenames to walk before giving up rather than
    /// overwriting one.
    pub max_collisions: u32,
}

impl Default for ReplayConfig {
    /// The configuration of an app that has no `replay.ron`: no replay logging.
    fn default() -> Self {
        Self {
            enabled: false,
            dir: "replays".to_string(),
            prefix: "onus".to_string(),
            max_collisions: 64,
        }
    }
}

impl ReplayConfig {
    /// Path of the config inside a data directory.
    pub const FILE: &'static str = "replay.ron";

    pub fn load_default() -> Result<Self, String> {
        Self::load_from_dir(Path::new(crate::sim::content::DATA_DIR))
    }

    /// Load `<dir>/replay.ron`.
    ///
    /// **A missing file is not an error** — it is an app with no replay logging
    /// configured, which is the default and the off state. A file that exists
    /// and does not parse *is* an error: a missing optional config is a state, a
    /// broken one is a mistake, and silently treating a typo as "off" would make
    /// the feature look broken instead of misconfigured.
    pub fn load_from_dir(dir: &Path) -> Result<Self, String> {
        let path = dir.join(Self::FILE);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(format!("replay config: cannot read {}: {e}", path.display())),
        };
        ron::from_str(&text).map_err(|e| format!("replay config: cannot parse {}: {e}", path.display()))
    }
}

/// Load the replay configuration, **reporting** a broken one rather than
/// silently falling back to "off".
///
/// This is the seam the reporting is testable through — and the reason it is a
/// function rather than three lines in `build_app` is that a diagnostic is only
/// a diagnostic if something can observe it. `tracing` drops events emitted
/// before a subscriber exists, so `build_app` calls this *after*
/// `DefaultPlugins`, and a test calls it under a subscriber of its own and
/// reads back what was emitted.
///
/// A missing file is not reported: that is the off state, not a mistake.
pub fn load_config_or_report(dir: &Path) -> ReplayConfig {
    match ReplayConfig::load_from_dir(dir) {
        Ok(config) => config,
        Err(e) => {
            error!("{e} — replay logging disabled for this run");
            ReplayConfig::default()
        }
    }
}

/// Where the writer's timestamp comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Clock {
    /// The wall clock. Legal *here*; the sim has no such thing.
    Wall,
    /// A fixed reading, so a test can put two matches in the same second and
    /// watch what the writer does about it.
    Fixed(u64),
}

/// What the writer did, if it has acted. It acts **at most once per app**: the
/// outcome is recorded and never revisited, so a decided match that keeps
/// ticking cannot rewrite the file, an `AppExit` after a decision cannot write a
/// second copy, and a failure cannot repeat itself once a frame forever.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteOutcome {
    Written(PathBuf),
    Failed(String),
    /// Enabled, asked to write, and the log was empty of any content stamp —
    /// nothing was ever recorded, so there is nothing to replay.
    NothingToWrite,
}

/// The shipped replay writer: the configuration, plus the single outcome.
#[derive(Resource, Debug)]
pub struct ReplayWriter {
    config: ReplayConfig,
    clock: Clock,
    outcome: Option<WriteOutcome>,
    /// How many times the writer has been *asked* to act. The latch is what
    /// keeps `outcome` at one; this counts the asks, so a test can prove the
    /// asking really happened repeatedly and the latch is doing the work.
    asks: u32,
}

impl ReplayWriter {
    pub fn new(config: ReplayConfig) -> Self {
        Self {
            config,
            clock: Clock::Wall,
            outcome: None,
            asks: 0,
        }
    }

    /// A writer whose timestamp is fixed — for tests that need two matches to
    /// land in the same second.
    pub fn with_fixed_clock(config: ReplayConfig, secs: u64) -> Self {
        Self {
            clock: Clock::Fixed(secs),
            ..Self::new(config)
        }
    }

    pub fn config(&self) -> &ReplayConfig {
        &self.config
    }

    pub fn outcome(&self) -> Option<&WriteOutcome> {
        self.outcome.as_ref()
    }

    /// The file this app wrote, if it wrote one.
    pub fn written(&self) -> Option<&Path> {
        match &self.outcome {
            Some(WriteOutcome::Written(p)) => Some(p.as_path()),
            _ => None,
        }
    }

    /// Why the write failed, if it did. Also reported through `error!`, which is
    /// where a player running the game sees it; a write failure never takes the
    /// game down — a lost replay is not worth a lost match.
    pub fn error(&self) -> Option<&str> {
        match &self.outcome {
            Some(WriteOutcome::Failed(e)) => Some(e.as_str()),
            _ => None,
        }
    }

    pub fn asks(&self) -> u32 {
        self.asks
    }

    fn now(&self) -> u64 {
        match self.clock {
            Clock::Fixed(s) => s,
            // The wall clock, in the driver, for a filename. Before the epoch
            // is not a case worth a branch: it is 0.
            Clock::Wall => SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        }
    }

    /// Write the log, unless this writer has already acted.
    ///
    /// Everything after the latch is best-effort and *reported*: a failure is
    /// recorded in [`outcome`](Self::outcome) and logged at `error!` level. The
    /// one thing it will never do is overwrite an existing log — see
    /// [`claim_path`].
    fn write_once(&mut self, log: &CommandLog) {
        self.asks = self.asks.saturating_add(1);
        if self.outcome.is_some() || !self.config.enabled {
            return;
        }
        // Nothing has been recorded at all: no content stamp means no sim ever
        // played this log, and `load_for` would refuse it. Say so rather than
        // writing a file that cannot be replayed.
        if !log.log().content.is_known() {
            self.outcome = Some(WriteOutcome::NothingToWrite);
            return;
        }
        // The front door: `to_ron` refuses a log this build could not read back
        // (a non-finite coordinate, an unidentified entity, a poisoned content
        // stamp). Refusing here is the point — a file we cannot load is worse
        // than no file.
        let text = match log.log().to_ron() {
            Ok(t) => t,
            Err(e) => {
                error!("replay log not written: {e}");
                self.outcome = Some(WriteOutcome::Failed(e));
                return;
            }
        };
        let dir = PathBuf::from(&self.config.dir);
        if let Err(e) = std::fs::create_dir_all(&dir) {
            let msg = format!("replay log not written: cannot create {}: {e}", dir.display());
            error!("{msg}");
            self.outcome = Some(WriteOutcome::Failed(msg));
            return;
        }
        let stem = format!("{}-{}-seed{}", self.config.prefix, self.now(), log.seed());
        self.outcome = Some(match claim_path(&dir, &stem, self.config.max_collisions) {
            Ok((path, mut file)) => match write_or_discard(&path, &mut file, &text) {
                Ok(()) => {
                    info!("replay log written: {}", path.display());
                    WriteOutcome::Written(path)
                }
                Err(msg) => {
                    error!("{msg}");
                    WriteOutcome::Failed(msg)
                }
            },
            Err(e) => {
                error!("{e}");
                WriteOutcome::Failed(e)
            }
        });
    }
}

/// Claim an unused filename under `dir`, and return it with the handle that
/// claimed it.
///
/// **A filename is a coordinate, and this one is not injective**: two matches
/// can finish in the same second with the same seed, and two runs of one seed
/// certainly can. So the name is never trusted. Each candidate is claimed with
/// `create_new`, which fails if the file exists — an atomic check-and-claim, so
/// even two processes racing cannot both take one name — and the writer walks
/// `-1`, `-2`, ... until it claims one. Running out is reported and loses the
/// *new* log; overwriting would lose an *old* one, which is strictly worse.
fn claim_path(dir: &Path, stem: &str, max_collisions: u32) -> Result<(PathBuf, std::fs::File), String> {
    let mut last = None;
    for n in 0..=max_collisions {
        let name = if n == 0 {
            format!("{stem}.ron")
        } else {
            format!("{stem}-{n}.ron")
        };
        let path = dir.join(name);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                last = Some(path);
                continue;
            }
            Err(e) => {
                return Err(format!(
                    "replay log not written: cannot create {}: {e}",
                    path.display()
                ))
            }
        }
    }
    Err(format!(
        "replay log not written: {} names from `{stem}.ron` to `{}` are all taken; \
         refusing to overwrite an existing log",
        max_collisions + 1,
        last.map(|p| p.display().to_string()).unwrap_or_default()
    ))
}

/// Write `text` into the file this writer just claimed, and **remove the file if
/// the write does not complete**.
///
/// A half-written log is not a log: `MatchLog::load` refuses truncated RON, so
/// nothing could ever mistake it for one. Left in place it would still be a
/// fragment *holding a name* — and the collision walk steps over taken names
/// forever, so a repeatedly failing write would eat a bounded name budget and
/// eventually deny a working write. The general rule ("never delete somebody
/// else's log") is not in tension with this: the fragment is not somebody
/// else's and it is not a log. This function only ever removes the path it was
/// handed, which the caller claimed with `create_new` in the same call — an old
/// log can never be reached from here.
///
/// If the cleanup itself fails, both failures are reported: the operator needs
/// to know a name is now held by a fragment.
fn write_or_discard(path: &Path, file: &mut std::fs::File, text: &str) -> Result<(), String> {
    let Err(e) = file.write_all(text.as_bytes()).and_then(|()| file.flush()) else {
        return Ok(());
    };
    let mut msg = format!("replay log not written: {}: {e}", path.display());
    if let Err(cleanup) = std::fs::remove_file(path) {
        msg.push_str(&format!(
            " (and the partial file could not be removed: {cleanup}; the name is \
             now held by a fragment)"
        ));
    }
    Err(msg)
}

/// Write the log the moment the match is decided.
///
/// Not only on exit: a crash or a force-quit after a finished match would lose
/// it, and the finished match is exactly the one worth keeping.
pub fn write_on_decision(
    state: Res<MatchState>,
    log: Res<CommandLog>,
    mut writer: ResMut<ReplayWriter>,
) {
    if state.is_over() {
        writer.write_once(&log);
    }
}

/// ...and on the way out, for a session that ends before a decision. Latched
/// against the same outcome, so an exit after a decided match writes nothing.
pub fn write_on_exit(
    mut exits: MessageReader<AppExit>,
    log: Res<CommandLog>,
    mut writer: ResMut<ReplayWriter>,
) {
    if exits.read().next().is_some() {
        writer.write_once(&log);
    }
}

// L1 unit tests: the file-level behaviour that has no ECS in it.
#[cfg(test)]
mod tests {
    use super::*;

    /// A write that fails leaves **no file**, so a failing writer cannot eat the
    /// collision-walk's name budget one fragment at a time.
    ///
    /// The failure is forced by handing `write_or_discard` a handle opened
    /// read-only: `write_all` fails deterministically, on every platform, with
    /// no full disk required.
    #[test]
    fn a_write_that_fails_leaves_no_file_behind() {
        let dir = std::env::temp_dir().join(format!("onus-rio-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        let path = dir.join("claimed.ron");

        // Claimed exactly as the writer claims it, then reopened read-only so
        // the write cannot succeed.
        let (claimed, _handle) = claim_path(&dir, "claimed", 0).expect("claim");
        assert_eq!(claimed, path);
        assert!(path.exists(), "the claim did not create the file");
        let mut read_only = OpenOptions::new().read(true).open(&path).expect("reopen");

        let err = write_or_discard(&path, &mut read_only, "(version: 2)")
            .expect_err("a read-only handle must fail to write");
        assert!(err.contains("not written"), "unhelpful message: {err}");
        assert!(
            !path.exists(),
            "a failed write left a fragment holding the name"
        );

        // ...and the same path is claimable again afterwards, which is the
        // point: the budget was not consumed.
        let (again, _) = claim_path(&dir, "claimed", 0).expect("the name is free again");
        assert_eq!(again, path);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The direction that could break: a write that succeeds keeps its file,
    /// with exactly the bytes it was given.
    #[test]
    fn a_write_that_succeeds_keeps_its_file() {
        let dir = std::env::temp_dir().join(format!("onus-rio-ok-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        let (path, mut file) = claim_path(&dir, "kept", 0).expect("claim");
        write_or_discard(&path, &mut file, "(version: 2)").expect("write");
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "(version: 2)"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
