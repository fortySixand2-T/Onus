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

## Harness (the judge)

Everything below is gated on this. Build it before M2.

- Sim is a library with **no render deps**; Bevy is a thin driver that calls `step`. Runs headless.
- Gate = `cargo test` + `cargo clippy` + differential oracles (naive vs optimized) + golden replay (command log + per-tick state hash) in CI.
- "Done" = harness green **and** critic finds nothing. Never consensus.

Layout (as of M1):
- `src/sim/` — render-free simulation core (ECS + math + time only). The judged unit.
- `src/client.rs` — client-side state & presentation constants (selection markers, colors, sizes).
- `src/input.rs`, `src/ui.rs`, `src/setup.rs` — the Bevy driver (may use render types).
- `src/lib.rs` — `build_app()`; `src/main.rs` — thin entry.
- `tests/` — L2 headless integration (`MinimalPlugins`); `benches/` — L4 criterion; L1 unit tests co-located in `src/sim/`.
- Planned (documented, not yet wired): auto-test hook (fmt/clippy/test on Stop), `/test-*` authoring skills, a determinism-audit subagent. Revisit before M5.

## Build orchestration

The harness is the judge; agents only propose. Keep the substrate boring: git + files + harness runs, no message bus.

Roles:
- **Planner** — milestone → frozen file list + AC tests + interface contracts, before any code.
- **Implementer** — single writer for the milestone. Test-first. Commit-per-AC.
- **Critic** — adversarial, isolated (see below). Tries to break the diff.
- **Historian** — maintains `FINDINGS.md` (F-series) and keeps `CLAUDE.md` coherent.

Rules:
- Single writer per milestone. No parallel coders on one tree.
- Test-first: every change ships a test that failed before and passes after.
- Ensembles only at design forks (M3, M6): parallel impls behind one interface, pick by benchmark, ledger the choice.

## Critic subagent — isolation contract

- **Fresh context per milestone.** Sees ONLY: the milestone spec + ACs + probes, the invariants (Bevy note + Harness section), the public interface contracts, the committed diff under review, and the harness output.
- **Never sees:** the implementer's reasoning/justifications, or any prior agent's approval. It judges the diff against the spec, nothing else. Independence is the entire point.
- **Job:** try to break it. Produce (a) new tests that fail on the current diff, (b) any invariant violations, (c) a verdict PASS/BLOCK. Output is code + a short findings list — not opinions.
- **Blocks on:** any invariant violation, any new failing test, or any claimed speedup that doesn't actually hold.
- **Loop:** implementer reaches harness-green → critic runs isolated → hands failing tests back as diffs → implementer fixes → repeat until the critic adds nothing new → commit-per-AC + F-series entry.

## Subagent files

Two Claude Code subagents live in `.claude/agents/` (project scope, commit them):

- `implementer.md` (`rts-implementer`) — single writer for the current milestone; test-first; commit-per-AC.
- `critic.md` (`rts-critic`) — adversarial reviewer, isolated; writes failing tests under `tests/critic/`, never touches `src/`.

Wiring caveat that makes or breaks it: a subagent's fresh context provides the isolation, **but the orchestrator must hand the critic only the diff + milestone spec** — never paste in the implementer's transcript or reasoning. Do that and you've rebuilt the echo chamber.

## Tests (acceptance criteria)

### M0 — fixed-timestep spine ✅ done
- [x] Window opens with a `Camera2d`.
- [x] One entity with `Position`, `Velocity`, `Sprite`, `Transform`.
- [x] Motion integrated in `FixedUpdate` at 60 Hz; bounces off bounds.
- [x] `Transform` synced from `Position` in `Update`.
- [x] Log shows ~60 sim ticks vs monitor frame count per second.

> Note: M1 replaced the single bouncing demo entity with pre-placed commandable
> units; the `Velocity` component and bounce logic were retired. The tick/frame
> report and the `FixedUpdate`/`Update` split remain.

### M1 — input & selection ✅ done
Real RTS interaction: left-click **selects**; units come from production later
(M4), so M1 **pre-places** them. Selection is client-side presentation; only
movement/gather orders touch the sim (command queue → `FixedUpdate`).
- [x] Cursor resolves to a world position.
- [x] Units pre-placed at startup across three types (real spawning deferred to M4).
- [x] Left-click selects the unit under the cursor; click empty ground deselects.
- [x] Box-drag selects all own units inside the rectangle.
- [x] Double-click a unit selects all units of that type (camera static in M1 ⇒ all on-screen).
- [x] Shift+click adds/removes a single unit; shift takes precedence over double-click.
- [x] Right-click on ground moves the selection there and stops; on a resource, assigns a gather target (gather loop lands in M4).
- [x] Selecting a resource shows a minimal options panel (debug text).
- [x] Input never mutates sim state directly — clicks emit `Order`s consumed in `FixedUpdate`.

Critic probes: unit stops within epsilon (no overshoot/orbit); click/box/double/shift
selection resolve to the correct entity set; box-drag never selects resource nodes;
right-click routes move-vs-gather by what's under the cursor; input→sim path touches
no sim component directly (`Selected` is client-side, not sim).

### M2 — scale + spatial index
- [ ] Spawn 1000+ units across two factions.
- [ ] Naive nearest-enemy pass, with timing logged.
- [ ] Replace with a spatial grid; identical result, faster.
- [ ] Before/after timing recorded.

Critic probes: grid nearest-enemy == brute-force nearest-enemy over random seeds (differential); empty/edge cells handled; claimed speedup actually holds.

### M3 — pathfinding
- [ ] Tile grid with blocked cells.
- [ ] A* path; unit follows waypoints around obstacles.
- [ ] Group move to one destination uses a flow field, not N× A*.

Critic probes: path uses only walkable, contiguous cells and reaches the goal; returns none iff unreachable; flow field agrees with A* on reachability; group cost < N× A*.

### M4 — economy, combat, AI
- [ ] Resource gather/deposit loop.
- [ ] Building placement consumes the resource.
- [ ] Health + attack; engages nearest enemy in range; death despawns.
- [ ] Scripted AI builds and attacks on a timer.

Critic probes: resource conserved (gathered == deposited + carried); building cost deducted exactly once; death despawns exactly once; AI is deterministic given the seed.

### M5 — deterministic replay
- [ ] `Command` enum tagged with a target tick; applied only in `FixedUpdate`.
- [ ] Command log persisted to disk.
- [ ] Replay reproduces identical final state (per-tick state hash matches).
- [ ] Seeded RNG stepped only inside the sim.

Critic probes: same seed + same log ⇒ identical per-tick hash; commands never apply off their scheduled tick; no HashMap/iteration-order nondeterminism affects outcomes; no wall-clock in sim.

### M6 — lockstep networking (optional)
- [ ] Commands exchanged between two clients.
- [ ] A tick advances only when all inputs for it are present.
- [ ] Periodic state-hash exchange detects desync.

Critic probes: injected divergence is caught by the hash exchange; a tick never advances on missing input; determinism holds cross-process.
