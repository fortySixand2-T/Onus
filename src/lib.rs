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
    // Content is data: costs/stats come from assets/data/*.ron, never from
    // constants. Loaded render-free (plain file IO) before startup so the
    // driver and the sim see the same definitions.
    let content = Content::load_default().expect("assets/data/*.ron load");

    let mut app = App::new();
    app.add_plugins(DefaultPlugins)
        .insert_resource(Time::<Fixed>::from_hz(60.0))
        .insert_resource(content)
        .init_resource::<CursorWorld>()
        .init_resource::<DragState>()
        .init_resource::<ClickTracker>()
        .init_resource::<CommandQueue>()
        .init_resource::<RateReport>()
        .init_resource::<Stockpiles>()
        .add_systems(Startup, setup::setup)
        .add_systems(
            Update,
            (
                input::update_cursor,
                input::selection.after(input::update_cursor),
                input::emit_commands.after(input::update_cursor),
                input::emit_build_commands.after(input::update_cursor),
                ui::attach_visuals,
                ui::sync_transform,
                ui::draw_selection,
                ui::update_options_panel,
                ui::report_rates,
            ),
        );
    add_sim_systems(&mut app, FixedUpdate);
    app
}

/// Register the sim chain on `schedule`. **The one definition of what the sim
/// runs and in what order**: the shipped app installs it on `FixedUpdate`, and
/// the headless tests install it on `Update` (where they can hand the sim
/// exactly one fixed timestep per step). Nothing may hand-roll this list — that
/// is how a system like `production` ends up tested but never shipped.
///
/// Order matters: orders are applied (and paid for) first, then production
/// advances queues, then gatherers decide where to go, then everything moves.
pub fn add_sim_systems(app: &mut App, schedule: impl ScheduleLabel) {
    app.add_systems(
        schedule,
        (
            sim::apply_commands,
            sim::economy::production,
            sim::economy::gather,
            sim::movement,
        )
            .chain(),
    );
}
