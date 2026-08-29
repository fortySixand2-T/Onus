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

use bevy::prelude::*;

pub mod client;
pub mod input;
pub mod setup;
pub mod sim;
pub mod ui;

use crate::client::*;
use crate::sim::*;

/// Build the full game app: window, renderer, resources, and the M0–M1 systems.
/// `main` just calls `.run()` on this; tests build their own headless apps.
pub fn build_app() -> App {
    let mut app = App::new();
    app.add_plugins(DefaultPlugins)
        .insert_resource(Time::<Fixed>::from_hz(60.0))
        .init_resource::<CursorWorld>()
        .init_resource::<DragState>()
        .init_resource::<ClickTracker>()
        .init_resource::<CommandQueue>()
        .init_resource::<RateReport>()
        .add_systems(Startup, setup::setup)
        .add_systems(
            Update,
            (
                input::update_cursor,
                input::selection.after(input::update_cursor),
                input::emit_commands.after(input::update_cursor),
                ui::sync_transform,
                ui::draw_selection,
                ui::update_options_panel,
                ui::report_rates,
            ),
        )
        .add_systems(FixedUpdate, (sim::apply_commands, sim::movement).chain());
    app
}
