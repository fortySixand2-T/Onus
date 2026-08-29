# Changelog

- [2026-08-28] Created: Cargo.toml — Bevy 0.19 project manifest for the Onus RTS
- [2026-08-28] Created: src/main.rs — M0 fixed-timestep spine (Camera2d, bouncing unit, sim/render split, rate report)
- [2026-08-28] Created: .gitignore — ignore /target build dir
- [2026-08-28] Modified: src/main.rs — M1: app wiring, pre-placed units + resources, options panel
- [2026-08-28] Created: src/core.rs — M1: components, resources, tunables, Order command type
- [2026-08-28] Created: src/input.rs — M1: cursor→world, selection (click/box/double/shift), command emit
- [2026-08-28] Created: src/sim.rs — M1: apply_commands, movement, step_toward
- [2026-08-28] Created: src/ui.rs — M1: transform sync, selection gizmos, options panel, rate report
- [2026-08-28] Created: src/tests.rs — M1: headless integration tests for selection & commands
