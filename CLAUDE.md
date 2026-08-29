# CLAUDE.md — Onus RTS project constitution

Rust + Bevy 0.19 RTS, built as a learning vehicle for system design at scale.
Deterministic sim core first; game and campaign layered on top.

## Read before implementing
- `MVP_PLAN.md` — what we build first (the 1v1 battle layer) and the definition of done.
- `BUILD_PLAN.md` — milestone ladder (M0–M6), harness, orchestration, critic contract. **Canonical.**
- `DESIGN_BRIEF.md` — game content (roster, counter-pentagon, economy, campaign vision).
  Stats live in `assets/data/*.ron`.

**Status:** M0 (fixed-timestep spine) and M1 (input & selection) done. Next: M2 (scale + spatial index).
Build/test run on the Ubuntu box (`rohith@10.0.0.54`, clone at `~/projects/Games/Onus`); the Mac is
for `cargo run` viewing only (the box is headless).

## Environment

Ubuntu deps (box / CI / Linux):

```bash
sudo apt-get install -y g++ pkg-config libx11-dev libasound2-dev libudev-dev libxkbcommon-dev libwayland-dev
```

Rust is installed via rustup (`~/.cargo`); run `source "$HOME/.cargo/env"` before `cargo`. First build
is slow (5–15 min, all of Bevy); incremental builds cache. The box builds/tests everything headless
(L2 tests use `MinimalPlugins`) but **cannot display the game window** — viewing stays on the Mac.

## Invariants (never violate)

- Sim owns truth and is **render-free** (`src/sim/`, ECS + math + time only); Bevy is a thin driver.
- Sim runs in `FixedUpdate` @ 60 Hz (`Time::<Fixed>::from_hz(60.0)`); rendering in `Update`.
  `Transform` is written from sim `Position`, never read as truth.
- Input never mutates sim state directly — it emits `Order`s the sim consumes in `FixedUpdate`.
  Selection is client-side (`client` module), not sim.
- Determinism: seeded RNG stepped only in the sim; no wall-clock in sim logic; stable iteration
  order where outcomes depend on it; never let HashMap iteration order affect outcomes.
- **Content is data:** unit/economy values load from `assets/data/*.ron`. Change the brief + RON,
  never hardcoded constants.
- Bevy 0.19: spawn component tuples (`Camera2d`, `Sprite::from_color`, `Transform::from_xyz`);
  the old `*Bundle` types are gone.

## Layout

- `src/sim/` — render-free simulation core (the harness-judged unit); must run headless for tests
  and the balance sim. L1 unit tests co-located.
- `src/client.rs` — client-side state & presentation constants (selection, colors, sizes).
- `src/input.rs` · `src/ui.rs` · `src/setup.rs` — the Bevy driver (may use render types).
- `src/lib.rs` — `build_app()`; `src/main.rs` — thin entry.
- `tests/` — L2 headless integration (`MinimalPlugins`); `benches/` — L4 criterion.
- `assets/data/*.ron` — unit/resource content, deserialized by the sim at startup (M4).

## Workflow

- One milestone at a time (first with unchecked ACs in `BUILD_PLAN.md`). Never build ahead.
- Test-first: every change ships a test that failed before and passes after.
- The harness is the judge; "done" = harness green **and** critic finds nothing — never consensus.
- Build with the `rts-implementer` subagent, then review with `rts-critic` (isolated: diff + spec
  only). Single writer per milestone.
- Commit per AC. Log scaling/design decisions in `FINDINGS.md` (F-series).
