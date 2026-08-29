//! Onus — RTS in Bevy 0.19.
//!
//! M0: the fixed-timestep spine.
//! - Simulation state (`Position`, `Velocity`) is integrated in `FixedUpdate` at 60 Hz.
//! - `Transform` is *written from* the sim in `Update`, never read as truth.
//! - A once-per-second report compares sim ticks against rendered frames.

use bevy::prelude::*;

/// Half-extents of the play field, in world units. The bouncing entity is kept
/// inside `[-HALF_W, HALF_W] x [-HALF_H, HALF_H]` (adjusted for its own size).
const HALF_W: f32 = 300.0;
const HALF_H: f32 = 200.0;
/// Side length of the unit's square sprite, in world units.
const SPRITE: f32 = 20.0;

/// Simulation-space position. The source of truth for where an entity is.
#[derive(Component)]
struct Position(Vec2);

/// Simulation-space velocity, in world units per second.
#[derive(Component)]
struct Velocity(Vec2);

/// Counters for the once-per-second sim-tick vs. frame-rate report.
#[derive(Resource, Default)]
struct RateReport {
    sim_ticks: u32,
    frames: u32,
    elapsed: f32,
}

fn main() {
    App::new()
        .add_plugins(DefaultPlugins)
        // Sim runs at a fixed 60 Hz, independent of render frame rate.
        .insert_resource(Time::<Fixed>::from_hz(60.0))
        .init_resource::<RateReport>()
        .add_systems(Startup, setup)
        .add_systems(FixedUpdate, integrate_motion)
        .add_systems(Update, (sync_transform, report_rates))
        .run();
}

fn setup(mut commands: Commands) {
    commands.spawn(Camera2d);

    // One unit: sim state (Position/Velocity) plus its render representation
    // (Sprite/Transform). Transform starts matching Position and is kept in
    // sync by `sync_transform` — it is output, never input.
    commands.spawn((
        Position(Vec2::ZERO),
        Velocity(Vec2::new(140.0, 90.0)),
        Sprite::from_color(Color::srgb(0.2, 0.7, 1.0), Vec2::splat(SPRITE)),
        Transform::from_xyz(0.0, 0.0, 0.0),
    ));
}

/// Integrate motion in the fixed-timestep sim, bouncing off the field bounds.
fn integrate_motion(
    time: Res<Time<Fixed>>,
    mut report: ResMut<RateReport>,
    mut query: Query<(&mut Position, &mut Velocity)>,
) {
    report.sim_ticks += 1;
    let dt = time.delta_secs();
    let max_x = HALF_W - SPRITE * 0.5;
    let max_y = HALF_H - SPRITE * 0.5;

    for (mut pos, mut vel) in &mut query {
        pos.0 += vel.0 * dt;

        // Clamp to bounds and reflect velocity on contact.
        if pos.0.x > max_x {
            pos.0.x = max_x;
            vel.0.x = -vel.0.x;
        } else if pos.0.x < -max_x {
            pos.0.x = -max_x;
            vel.0.x = -vel.0.x;
        }
        if pos.0.y > max_y {
            pos.0.y = max_y;
            vel.0.y = -vel.0.y;
        } else if pos.0.y < -max_y {
            pos.0.y = -max_y;
            vel.0.y = -vel.0.y;
        }
    }
}

/// Copy sim-space `Position` into the render `Transform`. One-way, every frame.
fn sync_transform(mut query: Query<(&Position, &mut Transform)>) {
    for (pos, mut transform) in &mut query {
        transform.translation.x = pos.0.x;
        transform.translation.y = pos.0.y;
    }
}

/// Once per (wall-clock) second, log how many sim ticks ran vs. frames drawn.
/// Sim ticks should hover near 60; frames track the monitor's refresh rate.
fn report_rates(time: Res<Time>, mut report: ResMut<RateReport>) {
    report.frames += 1;
    report.elapsed += time.delta_secs();
    if report.elapsed >= 1.0 {
        info!(
            "sim ticks: {} | frames: {}",
            report.sim_ticks, report.frames
        );
        report.sim_ticks = 0;
        report.frames = 0;
        report.elapsed = 0.0;
    }
}
