//! The simulation core — **render-free** and headless-testable. This is the
//! unit the harness judges (see BUILD_PLAN.md). It imports only ECS, math, and
//! time; no rendering types (`Color`/`Sprite`/`Transform`/`Window`) appear here,
//! so the sim stays a pure, deterministic function of its state and commands.
//!
//! Presentation and interaction (selection, colors, cursor) live in `client`;
//! Bevy is a thin driver that spawns sprites and calls these systems.

use bevy::ecs::prelude::*;
use bevy::math::Vec2;
use bevy::time::{Fixed, Time};
use std::collections::VecDeque;

pub mod combat;
pub mod content;
pub mod economy;
pub mod pathfind;
pub mod spatial;
pub use combat::{AttackCooldown, Casualties, Engaging, Health, Target};
pub use content::Content;
pub use economy::{
    Building, Carrying, GatherPhase, ProductionQueue, QueuedUnit, Stockpiles, UnitDefIdx,
};
pub use pathfind::{astar, FlowField, TileGrid};
pub use spatial::{
    brute_force_nearest_enemy, random_layout, Faction, SpatialGrid, SplitMix64, Unit,
};

// ---- tunables --------------------------------------------------------------

// Move speed used to be the global `SPEED` constant. **Retired in M4b**: Speed
// is one of the four stats, so it is per-unit RON data
// (`speed * mvp_combat.speed_per_point`, see `combat::move_speed`) and a unit
// without a definition simply does not move.

/// Distance at which a moving unit snaps to its target and stops.
pub const STOP_EPS: f32 = 1.0;

// ---- components ------------------------------------------------------------

/// Sim-space position — the source of truth for where a thing is.
#[derive(Component)]
pub struct Position(pub Vec2);

/// The silhouette class of a unit — sim data, loaded from `units.ron`
/// (`mvp_kind`) and written by the sim when it spawns a unit, never patched in
/// later by the presentation layer. Its presence marks an entity as a
/// commandable unit;
/// resource nodes carry no `UnitKind`. Purely sim data — visuals (color/size)
/// for a kind live in `client`.
#[derive(Component, Clone, Copy, PartialEq, Eq, Debug, serde::Deserialize)]
pub enum UnitKind {
    Worker,
    Soldier,
    Scout,
}

impl UnitKind {
    pub fn label(self) -> &'static str {
        match self {
            UnitKind::Worker => "Worker",
            UnitKind::Soldier => "Soldier",
            UnitKind::Scout => "Scout",
        }
    }
}

/// A gatherable resource node.
#[derive(Component)]
pub struct ResourceNode {
    pub amount: u32,
}

/// Where a unit is currently ordered to move. Removed on arrival.
#[derive(Component)]
pub struct MoveTarget(pub Vec2);

/// The deposit a unit is assigned to gather from. Read by `economy::gather`.
#[derive(Component)]
pub struct GatherTarget(pub Entity);

// ---- resources -------------------------------------------------------------

/// Player intents awaiting application in `FixedUpdate`. The seed of M5's
/// tick-tagged, loggable command stream. (Named `Order` to avoid Bevy's
/// prelude `Command` trait.)
#[derive(Resource, Default)]
pub struct CommandQueue(pub VecDeque<Order>);

/// A sim-affecting order. Emitted by input in `Update`, applied in `FixedUpdate`.
pub enum Order {
    MoveTo {
        units: Vec<Entity>,
        dest: Vec2,
    },
    Gather {
        units: Vec<Entity>,
        node: Entity,
        node_pos: Vec2,
    },
    /// Place a building (index into `Content::buildings`) — costs Alloy.
    Place {
        faction: Faction,
        building: usize,
        pos: Vec2,
    },
    /// Train a unit (index into `Content::units`) at a building — costs Alloy.
    /// The *building's* faction pays; the order carries no issuer, so ownership
    /// is unchecked. Deferred to M4c with the second commander (BUILD_PLAN.md).
    Train {
        building: Entity,
        unit: usize,
    },
}

/// Counters for the once-per-second sim-tick vs. frame report. `sim_ticks` is
/// bumped in the sim; `frames`/`elapsed` are driven by the render-side reporter.
#[derive(Resource, Default)]
pub struct RateReport {
    pub sim_ticks: u32,
    pub frames: u32,
    pub elapsed: f32,
}

// ---- systems ---------------------------------------------------------------

/// Drain the command queue, turning intents into per-entity sim components.
/// Runs before [`movement`] so orders take effect on the same tick.
pub fn apply_commands(
    mut queue: ResMut<CommandQueue>,
    content: Res<Content>,
    mut stock: ResMut<Stockpiles>,
    mut producers: Query<(&Building, &Faction, &mut ProductionQueue)>,
    mut commands: Commands,
) {
    while let Some(cmd) = queue.0.pop_front() {
        match cmd {
            Order::MoveTo { units, dest } => {
                for e in units {
                    // A move order cancels gathering, but a carried load is
                    // kept (it stays "in flight" — Alloy is never destroyed).
                    // It also cancels an auto-engagement: `Engaging` marks a
                    // move *combat* issued, so leaving it on would let the same
                    // tick's combat pass overwrite the commander's destination.
                    // Orders come from the commander; the sim only ever
                    // auto-chases a unit that has none.
                    commands
                        .entity(e)
                        .insert(MoveTarget(dest))
                        .remove::<GatherTarget>()
                        .remove::<GatherPhase>()
                        .remove::<Engaging>();
                }
            }
            Order::Gather {
                units,
                node,
                node_pos,
            } => {
                for e in units {
                    // Move to the node and start the gather loop (`economy`).
                    // `insert_if_new` on `Carrying` so re-tasking a worker that
                    // is already holding a load never zeroes that load.
                    // Also an explicit order, so it likewise ends any chase.
                    commands
                        .entity(e)
                        .insert(MoveTarget(node_pos))
                        .insert(GatherTarget(node))
                        .insert(GatherPhase::ToNode)
                        .remove::<Engaging>()
                        .insert_if_new(Carrying(0));
                }
            }

            // Spending orders. Both charge Alloy in exactly one place
            // (`Stockpiles::try_spend`); a rejected order changes nothing.
            Order::Place {
                faction,
                building,
                pos,
            } => {
                economy::place_building(
                    &content,
                    &mut stock,
                    &mut commands,
                    faction,
                    building,
                    pos,
                );
            }
            Order::Train { building, unit } => {
                if let Ok((b, faction, mut queue)) = producers.get_mut(building) {
                    economy::enqueue_unit(&content, &mut stock, b.def, *faction, &mut queue, unit);
                }
            }
        }
    }
}

/// Advance `pos` toward `target` by at most `step`. Returns the new position and
/// whether the unit arrived (within one step or `STOP_EPS`). Pure — unit-tested.
pub fn step_toward(pos: Vec2, target: Vec2, step: f32) -> (Vec2, bool) {
    let to = target - pos;
    let dist = to.length();
    if dist <= step.max(STOP_EPS) {
        (target, true)
    } else {
        (pos + to / dist * step, false)
    }
}

/// Integrate units toward their `MoveTarget`, snapping and stopping on arrival.
/// Each unit moves at **its own** speed — `speed * mvp_combat.speed_per_point`
/// from `units.ron` (M4b) — so a Bulwark and a Ripper cover different ground in
/// the same tick. `delta_secs` is the fixed timestep by construction (F-003).
pub fn movement(
    time: Res<Time<Fixed>>,
    content: Res<Content>,
    mut report: ResMut<RateReport>,
    mut query: Query<(Entity, &mut Position, &MoveTarget, &UnitDefIdx)>,
    mut commands: Commands,
) {
    report.sim_ticks += 1;
    let dt = time.delta_secs();

    for (e, mut pos, target, def) in &mut query {
        let step = combat::move_speed(&content, def.0) * dt;
        let (next, arrived) = step_toward(pos.0, target.0, step);
        pos.0 = next;
        if arrived {
            commands.entity(e).remove::<MoveTarget>();
        }
    }
}

// L1 unit test: the movement math in isolation (see BUILD_PLAN.md / TESTING).
#[cfg(test)]
mod tests {
    use super::step_toward;
    use bevy::math::Vec2;

    #[test]
    fn step_toward_snaps_on_arrival() {
        // Overshoot within one step → snap to target and report arrival.
        let (pos, arrived) = step_toward(Vec2::ZERO, Vec2::new(1.0, 0.0), 100.0);
        assert!(arrived && pos == Vec2::new(1.0, 0.0));

        // Far away → advance exactly `step` along the direction, not arrived.
        let (pos, arrived) = step_toward(Vec2::ZERO, Vec2::new(100.0, 0.0), 10.0);
        assert!(!arrived);
        assert!((pos.x - 10.0).abs() < 1e-4 && pos.y.abs() < 1e-4);
    }
}
