//! The headless match: one constructor for an AI-vs-AI game with no window.
//!
//! This is **driver-level, not sim** (it builds a Bevy `App` with
//! `MinimalPlugins` and installs the shipped sim chain), but it is render-free
//! and runs on a headless box. It lives here so the bench, B2's batch runner
//! and the tests share *one* definition of "the standard match": a fixture that
//! is hand-rolled in three places is three fixtures, and a balance number is
//! only comparable to another number produced by the same setup.
//!
//! The shape is M4c's AI-vs-AI fixture, lifted unchanged out of
//! `benches/replay_hash.rs`: two HQs 1500 apart, a deposit above each base,
//! three workers a side, the content's starting Alloy, one commander per side.
//!
//! It is parameterised by [`MatchSettings`] rather than by positional flags, so
//! the later B2 checkboxes (spawn orientation, tick cap) add a *field* and
//! every existing caller keeps compiling with the same behaviour.

use std::path::PathBuf;

use bevy::prelude::*;

use crate::sim::ai::UnknownStrategy;
use crate::sim::combat::{Casualties, Health};
use crate::sim::content::{Content, ContentError};
use crate::sim::economy::{Building, ProductionQueue, Stockpiles, UnitDefIdx};
use crate::sim::spatial::Faction;
use crate::sim::{
    AiCommanders, CommandLog, CommandQueue, Position, RateReport, ResourceNode, StateHashLog,
};

/// The sides of a match, in faction-slot order — the order commanders act in,
/// and the order every per-side array here is indexed by. Never insertion
/// order, and never a map's.
pub const SIDES: [Faction; 2] = [Faction::A, Faction::B];

/// The sim's fixed rate, in ticks per second — the one number every duration
/// in this module and in [`crate::batch`] is expressed in. It is the rate the
/// shipped game runs at (`Time::<Fixed>::from_hz`), stated once so a "tick
/// budget" can be written as a *time* and read back as one.
pub const SIM_HZ: u32 = 60;

/// Where each side's base stands. Both bases are described here, in one place,
/// because B2's side-balanced sampling will need to *swap* them — that
/// checkbox changes this function and nothing else.
fn base_of(faction: Faction) -> Vec2 {
    match faction {
        Faction::A => Vec2::new(-750.0, 0.0),
        Faction::B => Vec2::new(750.0, 0.0),
    }
}

/// How deep each side's starting deposit is. Effectively unbounded for a match
/// of MVP length: the fixture measures strategy, not node exhaustion.
const NODE_AMOUNT: u32 = 100_000;
/// Workers each side opens with.
const STARTING_WORKERS: u32 = 3;

/// Everything that varies between headless matches. `Default` is the fixture
/// the M5 bench measured: seed 0, both sides on the content's default strategy,
/// no per-tick hashing.
///
/// Grown by adding fields, never by adding parameters to [`ai_vs_ai`].
#[derive(Clone, Debug)]
pub struct MatchSettings {
    /// The match seed: fixes both commanders' RNG streams and stamps the
    /// [`CommandLog`].
    pub seed: u64,
    /// The strategy each side plays, indexed by faction slot (A, B). `None`
    /// means "the content's default strategy" — the same commander M4c built.
    /// A name is resolved against the content at construction and **refused**
    /// if unknown (see [`ai_vs_ai`]).
    pub strategies: [Option<String>; 2],
    /// Record one `state_hash` per tick into a [`StateHashLog`]. Opt-in
    /// because it costs a tick's worth of work (F-013's measurement); it reads
    /// the world and never writes it, so it cannot change a match.
    pub hashing: bool,
    /// How many ticks a match may run before the runner gives up on it and
    /// records a [`crate::batch::MatchResult::Timeout`]. Harness
    /// configuration, not game content, so it lives here and not in RON — and
    /// it is nothing to do with the sim: [`ai_vs_ai`] never reads it. A cap is
    /// a statement about how long an observer is willing to watch, which is
    /// why a capped match is *undecided* rather than drawn.
    pub tick_cap: u32,
}

/// The budget for one match: **eight minutes of play**, the top of the 5-8 min
/// target arc in DESIGN_BRIEF. Written as a duration times the sim's own rate
/// rather than as `28_800`, so the number explains itself and follows
/// [`SIM_HZ`] if the rate ever moves.
pub const DEFAULT_MATCH_SECS: u32 = 8 * 60;
/// [`DEFAULT_MATCH_SECS`] in sim ticks.
pub const DEFAULT_TICK_CAP: u32 = DEFAULT_MATCH_SECS * SIM_HZ;

impl Default for MatchSettings {
    fn default() -> Self {
        Self {
            seed: 0,
            strategies: [None, None],
            hashing: false,
            tick_cap: DEFAULT_TICK_CAP,
        }
    }
}

impl MatchSettings {
    pub fn with_seed(mut self, seed: u64) -> Self {
        self.seed = seed;
        self
    }

    /// Name the strategy each side plays: `a` for [`Faction::A`], `b` for
    /// [`Faction::B`].
    pub fn with_strategies(mut self, a: &str, b: &str) -> Self {
        self.strategies = [Some(a.to_string()), Some(b.to_string())];
        self
    }

    pub fn with_hashing(mut self, hashing: bool) -> Self {
        self.hashing = hashing;
        self
    }

    pub fn with_tick_cap(mut self, tick_cap: u32) -> Self {
        self.tick_cap = tick_cap;
        self
    }
}

/// Load the shipped content from the crate's own `assets/data`, independent of
/// the working directory — a bench, a test binary and `src/bin/balance.rs` are
/// launched from wherever cargo feels like, and a balance run that silently
/// fails to find the roster is worse than one that does not start.
pub fn content() -> Result<Content, ContentError> {
    Content::load_from_dir(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data"))
}

/// Build a headless AI-vs-AI match on `content`, per `settings`.
///
/// Returns [`UnknownStrategy`] if either side names a strategy the content does
/// not have — **never** a silently defaulted match. Every figure B2 and B3
/// print is keyed by strategy name, so a typo has to stop the caller rather
/// than mislabel a row.
///
/// The app is built but not stepped: drive it with [`step`] / [`tick`].
pub fn ai_vs_ai(content: Content, settings: &MatchSettings) -> Result<App, UnknownStrategy> {
    // Resolve names *before* anything is spawned, so a refusal costs nothing
    // and cannot leave a half-built match behind.
    let commanders = match &settings.strategies {
        [None, None] => AiCommanders::new(settings.seed, &SIDES),
        [a, b] => {
            let default = content.default_strategy.clone();
            let named: Vec<(Faction, &str)> = SIDES
                .iter()
                .zip([a, b])
                .map(|(f, s)| (*f, s.as_deref().unwrap_or(default.as_str())))
                .collect();
            AiCommanders::matchup(&content, settings.seed, &named)?
        }
    };

    let alloy = content.economy.starting_alloy;
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .insert_resource(Time::<Fixed>::from_hz(SIM_HZ as f64))
        .insert_resource(content)
        .init_resource::<CommandQueue>()
        .init_resource::<RateReport>()
        .init_resource::<Casualties>()
        .insert_resource(Stockpiles::starting(alloy));
    // The *shipped* sim chain, in `Update` on a manually advanced fixed clock:
    // a headless app steps one tick per `app.update()` (see [`step`]).
    crate::add_sim_systems(&mut app, Update);

    for faction in SIDES {
        let base = base_of(faction);
        let def = app
            .world()
            .resource::<Content>()
            .building_index("hq")
            .expect("content has an `hq` building");
        app.world_mut().spawn((
            Position(base),
            Building { def },
            faction,
            ProductionQueue::default(),
        ));
        app.world_mut().spawn((
            Position(base + Vec2::new(0.0, 250.0)),
            ResourceNode {
                amount: NODE_AMOUNT,
            },
        ));
        for i in 0..STARTING_WORKERS {
            let (idx, kind, hp) = {
                let c = app.world().resource::<Content>();
                let idx = c.unit_index("worker").expect("content has a `worker` unit");
                (idx, c.units[idx].mvp_kind, Health::from_def(c, idx))
            };
            app.world_mut().spawn((
                Position(base + Vec2::new(0.0, 20.0 * i as f32)),
                UnitDefIdx(idx),
                kind,
                faction,
                hp,
            ));
        }
    }

    app.insert_resource(commanders);
    app.insert_resource(CommandLog::new(settings.seed));
    if settings.hashing {
        app.insert_resource(StateHashLog::default());
    }
    Ok(app)
}

/// Advance the match by exactly one sim tick.
///
/// Headless apps have no winit loop to drive the fixed clock, so the tick is
/// advanced by hand and then the app updated once. This lives beside the
/// constructor so no caller can drift on what "one tick" means.
pub fn step(app: &mut App) {
    let dt = app.world().resource::<Time<Fixed>>().timestep();
    app.world_mut().resource_mut::<Time<Fixed>>().advance_by(dt);
    app.update();
}

/// Advance the match by `n` sim ticks.
pub fn tick(app: &mut App, n: u32) {
    for _ in 0..n {
        step(app);
    }
}
