//! Deterministic replay (M5). **Render-free** — ECS + math + plain file IO,
//! exactly like [`content`](crate::sim::content); no `AssetServer`, no render
//! types, nothing that reads a clock.
//!
//! Three pieces, and the contract between them is the whole milestone:
//!
//! 1. **Every command carries the tick it applies on.** The queue holds
//!    [`Command`](crate::sim::Command)s, and [`apply_commands`] applies only the
//!    ones due on the sim's own tick counter.
//! 2. **[`CommandLog`] records every command the sim consumed**, with that tick
//!    and its attribution. It is the *input* half of the sim's state: a match is
//!    `(starting world, seed, log)`, and nothing else.
//! 3. **[`state_hash`] is the canonical hash of the sim's state.** One
//!    definition, in the sim, so replay verification and the critic's probes
//!    cannot drift apart into two different notions of "identical".
//!
//! A replay is then: rebuild the starting world, insert the log as a
//! [`ReplaySource`], and run. [`feed_replay`] pushes each tick's commands back
//! onto the same queue the mouse and the AI write to, so the replay is driven
//! through the sim's ordinary command path — there is no replay-only route into
//! the sim, exactly as there is no AI-only one (F-009).

use std::path::Path;

use bevy::ecs::prelude::*;
use bevy::math::Vec2;
use serde::{Deserialize, Serialize};

use crate::sim::spatial::Faction;
use crate::sim::{Attribution, Order};

/// Format version of a persisted [`MatchLog`]. Bumped when the on-disk shape
/// changes; loading refuses a version it does not know, because a log it cannot
/// read exactly is a log it cannot replay at all.
pub const LOG_FORMAT_VERSION: u32 = 1;

// ---- the loggable form of an order -----------------------------------------

/// An [`Order`] in the form that goes to disk.
///
/// Two deliberate differences from `Order`:
/// - entities are their raw `Entity::to_bits()`, converted back through
///   `Entity::try_from_bits` (a *checked* boundary — a corrupt log yields an
///   error, never a panic). Entity ids are reproducible because spawn order is;
/// - there is no `By` wrapper: the signature has already been resolved into an
///   [`Attribution`] by the time a command is applied, and that is what is
///   stored beside the order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum LoggedOrder {
    MoveTo {
        units: Vec<u64>,
        dest: (f32, f32),
    },
    Gather {
        units: Vec<u64>,
        node: u64,
        node_pos: (f32, f32),
    },
    Place {
        faction: Faction,
        building: usize,
        pos: (f32, f32),
    },
    Train {
        building: u64,
        unit: usize,
    },
}

fn xy(v: Vec2) -> (f32, f32) {
    (v.x, v.y)
}

fn vec2((x, y): (f32, f32)) -> Vec2 {
    Vec2::new(x, y)
}

impl LoggedOrder {
    /// The loggable form of an order. `None` only for [`Order::By`], which the
    /// queue has already peeled off before a command is ever applied — a
    /// signature is an [`Attribution`], not an order.
    pub fn of(order: &Order) -> Option<Self> {
        Some(match order {
            Order::MoveTo { units, dest } => LoggedOrder::MoveTo {
                units: units.iter().map(|e| e.to_bits()).collect(),
                dest: xy(*dest),
            },
            Order::Gather {
                units,
                node,
                node_pos,
            } => LoggedOrder::Gather {
                units: units.iter().map(|e| e.to_bits()).collect(),
                node: node.to_bits(),
                node_pos: xy(*node_pos),
            },
            Order::Place {
                faction,
                building,
                pos,
            } => LoggedOrder::Place {
                faction: *faction,
                building: *building,
                pos: xy(*pos),
            },
            Order::Train { building, unit } => LoggedOrder::Train {
                building: building.to_bits(),
                unit: *unit,
            },
            Order::By { .. } => return None,
        })
    }

    /// Back to an `Order`. Checked: an entity id the world cannot represent is
    /// an error, not a panic (`Entity::from_bits` panics; `try_from_bits` does
    /// not).
    pub fn to_order(&self) -> Result<Order, String> {
        let ent = |bits: u64| {
            Entity::try_from_bits(bits).ok_or_else(|| format!("log: invalid entity bits {bits}"))
        };
        let ents = |bits: &[u64]| bits.iter().map(|b| ent(*b)).collect::<Result<Vec<_>, _>>();
        Ok(match self {
            LoggedOrder::MoveTo { units, dest } => Order::MoveTo {
                units: ents(units)?,
                dest: vec2(*dest),
            },
            LoggedOrder::Gather {
                units,
                node,
                node_pos,
            } => Order::Gather {
                units: ents(units)?,
                node: ent(*node)?,
                node_pos: vec2(*node_pos),
            },
            LoggedOrder::Place {
                faction,
                building,
                pos,
            } => Order::Place {
                faction: *faction,
                building: *building,
                pos: vec2(*pos),
            },
            LoggedOrder::Train { building, unit } => Order::Train {
                building: ent(*building)?,
                unit: *unit,
            },
        })
    }

    /// Every float this order carries — the values a persisted log has to
    /// reproduce bit for bit.
    fn floats(&self) -> Vec<f32> {
        match self {
            LoggedOrder::MoveTo { dest, .. } => vec![dest.0, dest.1],
            LoggedOrder::Gather { node_pos, .. } => vec![node_pos.0, node_pos.1],
            LoggedOrder::Place { pos, .. } => vec![pos.0, pos.1],
            LoggedOrder::Train { .. } => vec![],
        }
    }
}

/// One command as it was applied: the tick, who it was held to, and what it
/// said.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoggedCommand {
    pub tick: u32,
    pub attribution: Attribution,
    pub order: LoggedOrder,
}

/// A whole match's command stream, plus the seed that drives the sim's RNG.
/// This *is* the replay: the pair (starting world, `MatchLog`) determines every
/// tick that follows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MatchLog {
    pub version: u32,
    pub seed: u64,
    pub commands: Vec<LoggedCommand>,
}

impl Default for MatchLog {
    fn default() -> Self {
        Self {
            version: LOG_FORMAT_VERSION,
            seed: 0,
            commands: Vec::new(),
        }
    }
}

impl MatchLog {
    pub fn new(seed: u64) -> Self {
        Self {
            seed,
            ..Default::default()
        }
    }

    /// The commands due on `tick`, in the order they were applied.
    pub fn at(&self, tick: u32) -> impl Iterator<Item = &LoggedCommand> {
        self.commands.iter().filter(move |c| c.tick == tick)
    }

    /// Serialize to RON — the format the rest of the project's data already
    /// uses, so a log is readable and diffable with no new dependency.
    ///
    /// **Checked at the boundary.** A non-finite coordinate (`NaN`, `±inf`) has
    /// no round-tripping RON spelling, so a log containing one could be written
    /// and then replayed as something else. That is a rejection, not a
    /// best-effort write: a log that cannot be read back exactly is not a log.
    pub fn to_ron(&self) -> Result<String, String> {
        if self.version != LOG_FORMAT_VERSION {
            return Err(format!(
                "log: refusing to write format version {} (this build writes {LOG_FORMAT_VERSION})",
                self.version
            ));
        }
        for c in &self.commands {
            for f in c.order.floats() {
                if !f.is_finite() {
                    return Err(format!(
                        "log: non-finite coordinate {f} in the command at tick {}",
                        c.tick
                    ));
                }
            }
        }
        ron::ser::to_string_pretty(self, ron::ser::PrettyConfig::default())
            .map_err(|e| format!("log: serialize: {e}"))
    }

    pub fn from_ron(text: &str) -> Result<Self, String> {
        let log: MatchLog = ron::from_str(text).map_err(|e| format!("log: parse: {e}"))?;
        if log.version != LOG_FORMAT_VERSION {
            return Err(format!(
                "log: format version {} is not {LOG_FORMAT_VERSION}",
                log.version
            ));
        }
        // Ticks are non-decreasing by construction (one drain per tick, in
        // order). A log that is not is either corrupt or hand-edited, and
        // replaying it would silently drop everything out of order — an error
        // is the honest answer.
        let mut last = 0u32;
        for c in &log.commands {
            if c.tick < last {
                return Err(format!(
                    "log: tick {} follows tick {last} — commands are out of order",
                    c.tick
                ));
            }
            last = c.tick;
            c.order.to_order()?;
        }
        Ok(log)
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let text = self.to_ron()?;
        std::fs::write(path, text).map_err(|e| format!("log: write {}: {e}", path.display()))
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("log: read {}: {e}", path.display()))?;
        Self::from_ron(&text)
    }
}

/// The match's command log — sim-owned state, appended to in exactly one place
/// ([`apply_commands`](crate::sim::apply_commands)).
#[derive(Resource, Debug, Default)]
pub struct CommandLog {
    log: MatchLog,
    /// Commands dropped because their tick had already gone by. Never applied
    /// late; counted so a desync has a number attached to it.
    late: u32,
}

impl CommandLog {
    /// A log for a match played with `seed`.
    pub fn new(seed: u64) -> Self {
        Self {
            log: MatchLog::new(seed),
            late: 0,
        }
    }

    pub fn log(&self) -> &MatchLog {
        &self.log
    }

    pub fn commands(&self) -> &[LoggedCommand] {
        &self.log.commands
    }

    pub fn seed(&self) -> u64 {
        self.log.seed
    }

    pub fn late(&self) -> u32 {
        self.late
    }

    pub(crate) fn record(&mut self, tick: u32, attribution: Attribution, order: &Order) {
        if let Some(order) = LoggedOrder::of(order) {
            self.log.commands.push(LoggedCommand {
                tick,
                attribution,
                order,
            });
        }
    }

    pub(crate) fn record_late(&mut self, late: u32) {
        self.late = self.late.saturating_add(late);
    }
}
