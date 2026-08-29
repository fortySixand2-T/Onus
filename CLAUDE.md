# RTS in Bevy — Build Instructions

Rust + Bevy 0.19. Build one milestone at a time. Each checkbox is one commit and must pass before moving on.

## Environment (Ubuntu)

```bash
sudo apt-get install -y g++ pkg-config libx11-dev libasound2-dev libudev-dev libxkbcommon-dev libwayland-dev
cargo new rts_game && cd rts_game
cargo add bevy@0.19
cargo run
```

First build is slow (5–15 min). For fast dev rebuilds: `cargo run --features bevy/dynamic_linking` plus the `mold` linker (dev only).

## Bevy 0.19 (don't regress)

- Spawn component tuples — `Camera2d`, `Sprite::from_color(Color::srgb(..), Vec2::new(..))`, `Transform::from_xyz(..)`. The old `*Bundle` types are removed.
- Simulation runs in `FixedUpdate` (`Time::<Fixed>::from_hz(60.0)`); rendering runs in `Update`. Sim owns its own state; `Transform` is written from it, never read as truth.

## Tests (acceptance criteria)

### M0 — fixed-timestep spine
- [ ] Window opens with a `Camera2d`.
- [ ] One entity with `Position`, `Velocity`, `Sprite`, `Transform`.
- [ ] Motion integrated in `FixedUpdate` at 60 Hz; bounces off bounds.
- [ ] `Transform` synced from `Position` in `Update`.
- [ ] Log shows ~60 sim ticks vs monitor frame count per second.

### M1 — input & spawning
- [ ] Cursor resolves to a world position.
- [ ] Left-click spawns a unit; selection works.
- [ ] Right-click sets a target; unit moves to it and stops.
- [ ] Input never mutates sim state directly.

### M2 — scale + spatial index
- [ ] Spawn 1000+ units across two factions.
- [ ] Naive nearest-enemy pass, with timing logged.
- [ ] Replace with a spatial grid; identical result, faster.
- [ ] Before/after timing recorded.

### M3 — pathfinding
- [ ] Tile grid with blocked cells.
- [ ] A* path; unit follows waypoints around obstacles.
- [ ] Group move to one destination uses a flow field, not N× A*.

### M4 — economy, combat, AI
- [ ] Resource gather/deposit loop.
- [ ] Building placement consumes the resource.
- [ ] Health + attack; engages nearest enemy in range; death despawns.
- [ ] Scripted AI builds and attacks on a timer.

### M5 — deterministic replay
- [ ] `Command` enum tagged with a target tick; applied only in `FixedUpdate`.
- [ ] Command log persisted to disk.
- [ ] Replay reproduces identical final state (per-tick state hash matches).
- [ ] Seeded RNG stepped only inside the sim.

### M6 — lockstep networking (optional)
- [ ] Commands exchanged between two clients.
- [ ] A tick advances only when all inputs for it are present.
- [ ] Periodic state-hash exchange detects desync.