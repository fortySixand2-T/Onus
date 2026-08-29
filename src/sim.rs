//! The simulation: runs in `FixedUpdate` at 60 Hz and owns truth.
//! Consumes queued commands and integrates movement.

use bevy::prelude::*;

use crate::core::*;

/// Drain the command queue, turning intents into per-entity sim components.
/// Runs before [`movement`] so orders take effect on the same tick.
pub fn apply_commands(mut queue: ResMut<CommandQueue>, mut commands: Commands) {
    while let Some(cmd) = queue.0.pop_front() {
        match cmd {
            Order::MoveTo { units, dest } => {
                for e in units {
                    commands
                        .entity(e)
                        .insert(MoveTarget(dest))
                        .remove::<GatherTarget>();
                }
            }
            Order::Gather {
                units,
                node,
                node_pos,
            } => {
                for e in units {
                    // Move to the node and record the gather intent. The actual
                    // gather/deposit loop arrives in M4.
                    commands
                        .entity(e)
                        .insert(MoveTarget(node_pos))
                        .insert(GatherTarget(node));
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
pub fn movement(
    time: Res<Time<Fixed>>,
    mut report: ResMut<RateReport>,
    mut query: Query<(Entity, &mut Position, &MoveTarget)>,
    mut commands: Commands,
) {
    report.sim_ticks += 1;
    let step = SPEED * time.delta_secs();

    for (e, mut pos, target) in &mut query {
        let (next, arrived) = step_toward(pos.0, target.0, step);
        pos.0 = next;
        if arrived {
            commands.entity(e).remove::<MoveTarget>();
        }
    }
}
