# MVP Plan — First Playable Battle

**Goal:** the battle layer, standing alone — a single **1v1 skirmish vs scripted AI** on the full counter pentagon — playable end-to-end and good enough to **test the fun hypothesis**. Everything else (campaign, diplomacy, nations beyond light, 3-resource economy, netcode) is post-MVP.

This is the bridge doc; detail lives elsewhere:
- game content → `DESIGN_BRIEF.md`
- engineering milestones + orchestration → `BUILD_PLAN.md`

---

## Scope

**In:**
- Small **seeded-fair** map, **1v1 vs scripted AI**.
- **One resource (Alloy) funds everything** — a single gather loop (workers mine deposits → deposit at HQ).
- **HQ + all 3 barracks** (Foundry / Gene-Vats / Aether Spire), all **5 units + Worker**.
- **Combat:** 4-stat model (speed / offense / defense / armor) + nemesis **+30%, ignoring armor**.
- Movement/pathfinding and unit-scale sufficient for a real fight (the M2 spatial index, M3 pathfinding).
- **Scripted AI:** gathers, builds a mixed army, attack-moves.
- **Win:** destroy the enemy **HQ**. Target match ≤ ~8 min.

**Out (post-MVP):** the entire campaign layer (nations-map, rounds, diplomacy, dominance), the 3-resource economy, bespoke nations, fog of war, replay-hardening + netcode (M5/M6), 3D, audio.

### The one decision this resolves: "Alloy only" means currency, not roster

The brief's MVP note said "Alloy only." That's ambiguous, and the fun hypothesis needs **all 5 units** (a 2-unit economy isn't a pentagon). So for the MVP: **Alloy is the single currency that funds all 5 units and all buildings** — one gather loop, but the whole pentagon is buildable and testable. The 3-resource domain *coupling* is the first post-MVP expansion. (Data implication: MVP unit costs are an Alloy table; the per-domain resource costs already in `units.ron` are the post-MVP target.)

### First playable is a mirror

Start with the **same nation on both sides**. That isolates the pentagon **and** is exactly the balance sim's measurement config — so the validation instrument falls out for free. Light nation asymmetry is the first extension *after* the loop proves fun.

---

## Path to the MVP (from M1 ✅ done)

1. **Harness first** — headless sim as a library, `cargo test`, differential-oracle + batch-runner scaffold. The judge everything gates on.
2. **M2 — scale + spatial index.** Many units, spatial grid, differential-tested vs brute force.
3. **M3 — pathfinding.** A* + flow field on the seeded map.
4. **M4 — economy, combat, AI.** Alloy loop; all 5 units from 3 barracks (Alloy-costed); 4-stat + nemesis combat; scripted AI; destroy-HQ. Loads `units.ron`. **← the game becomes playable here.**
5. **Hand-play check.** Does a single fight feel like a *decision*? First fun read.
6. **Balance sim.** Batch AI-vs-AI (mirror): win-rate symmetry, match-length distribution, unit/build dominance vs the kill criteria. Balance-as-backtesting.

---

## Definition of done (the fun gate)

The MVP is done — and the hypothesis validated — when:
- a hand-played match feels like meaningful composition / position decisions, not a stat check; **and**
- the balance sim shows **no unit winning >65% regardless of counter** (kill criteria), with matches terminating in the target length.

**Pass** → begin MVP+ (light nations, then the 3-resource economy).
**Fail** → the pentagon or the numbers are wrong: retune the *data* (not code) and re-run the sim. This is why content is data.

---

## Determinism note

The MVP needs **single-machine** determinism only (seeded RNG in the sim, stable iteration order) — enough for the balance sim and hand-replays. Full replay-hardening (M5) and cross-machine lockstep (M6) stay post-MVP, but the invariants that make them possible hold from day one.
