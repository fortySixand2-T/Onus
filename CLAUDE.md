# Onus — RTS in Bevy 0.19

**Canonical build spec, milestones, harness, and agent orchestration live in
[BUILD_PLAN.md](BUILD_PLAN.md).** This file is the quick-reference loaded into
context each session — keep it short; don't duplicate the milestone list here.

## Environment

Ubuntu deps (CI / Linux dev):

```bash
sudo apt-get install -y g++ pkg-config libx11-dev libasound2-dev libudev-dev libxkbcommon-dev libwayland-dev
```

On the Mac dev box, Rust is installed via rustup (`~/.cargo`); run
`source "$HOME/.cargo/env"` before `cargo`. Bevy runs natively on macOS — the
apt packages above are Linux-only. First build is slow (5–15 min); for fast dev
rebuilds use `cargo run --features bevy/dynamic_linking` (+ `mold` on Linux).

## Invariants (don't regress)

- Sim owns truth and is **render-free** (`src/sim/`, ECS + math + time only); Bevy is a thin driver.
- Sim runs in `FixedUpdate` @ 60 Hz (`Time::<Fixed>::from_hz(60.0)`); presentation in `Update`. `Transform` is written from sim `Position`, never read as truth.
- Input never mutates sim directly — it emits `Order`s applied in `FixedUpdate`. Selection is client-side (`client` module), not sim.
- Determinism (for M5/M6): seeded RNG stepped only in the sim; no wall-clock in sim logic; stable iteration order wherever outcomes depend on it; never let HashMap iteration order affect sim outcomes.
- Bevy 0.19: spawn component tuples (`Camera2d`, `Sprite::from_color`, `Transform::from_xyz`); the old `*Bundle` types are gone.

## Layout

- `src/sim/` — render-free simulation core (the harness-judged unit); L1 unit tests co-located.
- `src/client.rs` — client-side state & presentation constants (selection, colors, sizes).
- `src/input.rs` · `src/ui.rs` · `src/setup.rs` — the Bevy driver.
- `src/lib.rs` — `build_app()`; `src/main.rs` — thin entry.
- `tests/` — L2 headless integration (`MinimalPlugins`); `benches/` — L4 criterion.
