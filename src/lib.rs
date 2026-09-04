//! Onus — RTS in Bevy 0.19.
//!
//! Library root. Layers, innermost first:
//! - [`sim`]  — render-free simulation core; the unit the harness judges.
//! - [`client`] — client-side state & presentation constants (selection, colors).
//! - [`input`] / [`ui`] / [`setup`] — the Bevy driver: turn input into orders,
//!   render sim state, and spawn the world. May use render types; `sim` may not.
//!
//! Architecture (see BUILD_PLAN.md): the sim owns truth and runs in `FixedUpdate`
//! at 60 Hz; presentation is written from it in `Update`. Input emits
//! [`sim::Order`]s onto a queue applied only in the sim.

use bevy::ecs::schedule::ScheduleLabel;
use bevy::prelude::*;

pub mod client;
pub mod input;
pub mod replay_io;
pub mod setup;
pub mod sim;
pub mod ui;

use crate::client::*;
use crate::sim::*;

/// Build the full game app: window, renderer, resources, and the shipped
/// systems. `main` just calls `.run()` on this. The sim half of the schedule is
/// [`add_sim_systems`] — the *single* definition of the sim chain, shared with
/// the headless tests so the two can never drift.
pub fn build_app() -> App {
    // Content is data: costs/stats/starting Alloy come from assets/data/*.ron,
    // never from constants. Loaded render-free (plain file IO) before startup so
    // the driver and the sim see the same definitions.
    let content = Content::load_default().expect("assets/data/*.ron load");
    let starting_alloy = content.economy.starting_alloy;
    // Replay logging is a *feature*, not the game: a malformed `replay.ron`
    // disables it and says so, where malformed `units.ron` is fatal. Content is
    // load-bearing (there is no match without it); where a log file goes is not
    // worth refusing to start over.
    let replay_config = replay_io::ReplayConfig::load_default().unwrap_or_else(|e| {
        error!("{e} — replay logging disabled for this run");
        replay_io::ReplayConfig::default()
    });

    let mut app = App::new();
    app.add_plugins(DefaultPlugins)
        .insert_resource(Time::<Fixed>::from_hz(60.0))
        .insert_resource(content)
        .init_resource::<CursorWorld>()
        .init_resource::<DragState>()
        .init_resource::<ClickTracker>()
        .init_resource::<CommandQueue>()
        .init_resource::<RateReport>()
        .insert_resource(Stockpiles::starting(starting_alloy))
        .add_systems(Startup, setup::setup)
        .add_systems(
            Update,
            (
                input::update_cursor,
                input::selection.after(input::update_cursor),
                // Input stops when the match does. Not a sim rule — the sim
                // would refuse to apply these anyway, since the chain is gated
                // off — but an order pushed onto a queue nothing will ever
                // drain accumulates for the rest of the session.
                input::emit_commands
                    .after(input::update_cursor)
                    .run_if(sim::victory::match_running),
                input::emit_build_commands
                    .after(input::update_cursor)
                    .run_if(sim::victory::match_running),
                ui::attach_visuals,
                ui::sync_transform,
                ui::draw_selection,
                ui::update_options_panel,
                ui::report_rates,
            ),
        );
    add_sim_systems(&mut app, FixedUpdate);
    add_replay_writer(&mut app, replay_config);
    app
}

/// Register the replay writer on `app`. **The one definition of when a match's
/// log reaches disk**, shared by the shipped app and the headless tests for the
/// same reason [`add_sim_systems`] is (F-004): a writer that only exists in a
/// test harness is a writer the game does not have.
///
/// Both systems run in `Last`, after the tick they are reacting to: the sim
/// decides the match in `FixedUpdate`, and this sees the decision in the same
/// frame. Neither is in the sim chain — the writer reads sim state and reads a
/// clock, and the sim may do neither with it.
pub fn add_replay_writer(app: &mut App, config: replay_io::ReplayConfig) {
    app.insert_resource(replay_io::ReplayWriter::new(config));
    app.add_systems(
        Last,
        (
            replay_io::write_on_decision,
            replay_io::write_on_exit.after(replay_io::write_on_decision),
        ),
    );
}

/// Register the sim chain on `schedule`. **The one definition of what the sim
/// runs and in what order**: the shipped app installs it on `FixedUpdate`, and
/// the headless tests install it on `Update` (where they can hand the sim
/// exactly one fixed timestep per step). Nothing may hand-roll this list — that
/// is how a system like `production` ends up tested but never shipped.
///
/// Order matters: the gather-claim sweep runs first, so no reader downstream
/// can observe a split claim (F-008); then the scripted commanders decide (so an AI order is
/// applied on the tick it is taken, exactly like a click that lands before the
/// tick boundary), then orders are applied (and paid for), then production
/// advances queues, then gatherers decide where to go, then combat picks its
/// targets and fires, then everything moves — so a chase order issued this tick
/// is followed on this tick. The win check comes last, on the state the tick
/// actually ended in, and is the one system that keeps running after the match
/// is over (to keep saying it is over).
pub fn add_sim_systems(app: &mut App, schedule: impl ScheduleLabel) {
    // Sim-owned state that the chain *requires* is installed with the chain, for
    // the same reason the chain itself is defined once (F-004): a caller that
    // has to remember a resource is a caller that will eventually forget one.
    // (Content and the starting Stockpiles are match setup — the caller's.)
    app.init_resource::<Casualties>();
    app.init_resource::<sim::AiCommanders>();
    app.init_resource::<sim::AiJournal>();
    app.init_resource::<sim::MatchState>();
    app.init_resource::<sim::replay::CommandLog>();
    app.init_resource::<sim::replay::SimIds>();
    app.add_systems(
        schedule,
        (
            // Is this a contested match? Observed *before* the tick is played,
            // so an HQ lost on the first tick decides like any other (F-010).
            sim::victory::match_watch,
            // Playing the match — off the moment it is decided, so nothing keeps
            // running that could change the recorded outcome.
            (
                // **First, ahead of every reader of `GatherTarget`** (F-008):
                // the AI reads it to decide who is idle, the economy runs the
                // job, combat reads it as "the economy owns this one". A
                // half-claim swept here therefore reaches none of them. Any new
                // reader of the claim belongs after this system — see F-008.
                sim::economy::repair_gather_claims,
                // Every thing in the world carries the sim's own stable id, so
                // the command log and the state hash address entities by
                // something that does not move when the app's configuration
                // does (F-011). What it *needs* is to run before everything
                // that addresses an entity by `SimId` — `apply_commands` logs
                // by it, `feed_replay` resolves by it. It is placed after the
                // sweep rather than before it because it reads no gather claim
                // and nothing between the two spawns: keeping the pre-sweep
                // stretch as short as it can be is what keeps the F-008 rule
                // above cheap to check per system. Runs here *and* at the
                // tail — see below.
                sim::replay::identify,
                // The scripted commanders — **unless this is a replay**, where
                // their decisions are already in the log as orders and letting
                // them think again would double every one of them.
                sim::ai::ai_commanders
                    .run_if(not(resource_exists::<sim::replay::ReplaySource>)),
                // A replay's only producer: this tick's logged commands, pushed
                // onto the same queue every other producer writes to.
                sim::replay::feed_replay
                    .run_if(resource_exists::<sim::replay::ReplaySource>),
                sim::apply_commands,
                sim::economy::production,
                sim::economy::gather,
                sim::combat::combat,
                sim::movement,
            )
                .chain()
                .run_if(sim::victory::match_running),
            // Stamp the command log with the content this match is played with,
            // so a log can never be replayed against a different roster (and so
            // no caller has to remember to do it). **Ungated**, so a match
            // decided on its first tick still stamps the log it recorded, and
            // ordered after the sweep like everything else in `replay` (F-008's
            // pre-sweep stretch stays empty of anything but the win watch).
            sim::replay::stamp_content,
            // Deciding it: always runs, and writes the outcome exactly once.
            sim::victory::match_end,
            // The tail identification pass: whatever this tick spawned (a
            // placed building, a trained unit) gets its id before the tick is
            // hashed, and before the next tick's orders can name it.
            sim::replay::identify,
            // The tick's state hash, on the state the tick ended in — including
            // the ticks after the match is over, so a frozen sim is visibly
            // frozen. Does nothing unless a `StateHashLog` was inserted.
            sim::replay::record_state_hash,
        )
            .chain(),
    );
}
