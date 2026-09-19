# BALANCE_PLAN.md — Balance harness & fun gate

The step the ladder skipped. The engine is complete and bit-deterministic; what's
missing is the instrument that validates the *design*: does the counter pentagon
actually hold, and is a match fun? B1–B4 build the balance sim (code, critic-gated);
B5 names the human fun gate (not automatable).

**Reuses what already exists:** the headless `ai_vs_ai(seed, hashing) -> App` in
`benches/replay_hash.rs`, `sim::victory::{MatchOutcome, MatchState, match_running}`,
`sim::content::Content` (the `mvp_ai` block in `units.ron`), `sim::ai::AiCommander`,
seeded `SplitMix64`, and `sim::replay::state_hash` for reproducibility.

Same rules as BUILD_PLAN: one AC per commit, `rts-implementer` builds, `rts-critic`
reviews on diff + spec only, decisions go to FINDINGS (F-016+). Balance changes live
in RON, never in Rust.

## Why this comes before anything new

The stats in `units.ron` are still placeholders by their own comment ("the balance sim
tunes the real numbers later") — and that sim was never built. Nothing has confirmed the
pentagon isn't degenerate, and no one has played it. Nations and the campaign layer all
rest on a core that is unvalidated. This closes the gap between "the ladder is closed"
and "the game is good."

---

## B1 — Strategies as data (prerequisite)

One scripted AI can't measure balance — it runs one build order (and today `mvp_ai`
only opens `foundry`, so it never fields 3 of the 5 units). Generalize it into a *set*
of named strategies that, between them, exercise every unit and the whole pentagon.

- [x] Promote `mvp_ai` → a `strategies` set in data (`assets/data/strategies.ron` or a
      `strategies:` list), same schema per entry, extended so a strategy may open
      **multiple barracks** and build across domains.
- [x] `AiCommander` constructible from any named strategy; a match takes a
      (strategy, strategy) pair, one per side.
- [x] Author the probe set: **five "mass-unit" strategies** (mass bulwark / sentinel /
      ripper / ravager / arclight — each hard-commits to one unit), **≥2 mixed
      "synthesis" builds**, one **all-in worker/early rush**, one **turtle**. All must
      be buildable in the Alloy-only MVP economy.

Critic probes: a strategy naming a unit its barracks can't produce is refused at load;
the two sides stay independent; same (strategy, seed) replays bit-identically.

## B2 — Headless batch runner (bin)

- [x] Lift `ai_vs_ai` out of the bench into `onus::` so bench, bin, and tests share one
      headless-match constructor (no duplicate match-setup code).
- [x] `src/bin/balance.rs`: play every ordered matchup (all strategy pairs incl. mirrors)
      across K seeds; each match runs headless to termination or a **tick cap**
      (`8 min * 60 Hz = 28_800`; cap → draw/timeout).
- [x] **Side-balanced sampling:** play each matchup in both spawn orientations (or
      seed-randomize spawn and verify), so a left/first-mover edge can't masquerade as
      strategy strength.
- [x] Record per match: winner (A / B / draw), length in ticks, units produced per side.

Critic probes: a capped match records a draw, never panics; **a strategy mirrored
against itself is ~50% across seeds** (else spawn/turn bias — a blocker, it confounds
every other number); re-running the whole batch yields an identical report (determinism
via `state_hash`).

## B3.5 — Tempo first (resequenced 2026-09-18)

**Inserted ahead of B3's remaining ACs, by decision.** B3 AC1 (matrix) and AC2 (pentagon)
are built and critic-PASSED, but they were measured on a batch whose **decided-match median
is ~1:16 against the brief's 5–8 min target**. A pentagon computed on opening-length games
is a statement about openings, so F-025's broken `bulwark > ravager` link — and every row
mean — is provisional until the arc is right.

- [x] **Production depth becomes data** (prerequisite, decided 2026-09-18). RON-only tuning
      cannot reach the band: the commander trains one unit at a time per barracks
      (`if b.queued == 0`, `src/sim/ai.rs`) and duplicate barracks are refused at load, so a
      mass probe's throughput is exactly one unit per `mvp_train_ticks`. Scaling train times
      therefore lengthens the clock by *shrinking the army* — measured: median 5:28 but 12%
      timeouts and pentagon cells decided 6 of 16 (F-026, branch `b3.5-tempo-attempt`).
      Add `queue_depth` to `StrategyDef` so the cap on units-in-production is content, not a
      Rust constant. **Ship it at `queue_depth: 1`**, which must reproduce today's behaviour
      bit-for-bit — every pinned `state_hash` golden unchanged — so the capability lands
      provably neutral and the balance change that follows is separable from it.
- [x] **Parallel production** (prerequisite, decided 2026-09-19). Measured: `queue_depth` buys
      back only the idle gap between a pop and the next decision — 7 units at depth 1, 8 at
      depth 3, 8 at depth 8 over 6 000 ticks (F-027) — because `economy::production` advances
      only the queue *head*, so one barracks builds one unit at a time whatever the queue
      holds. Army size is therefore capped by **barracks count**, and load-time validation
      refuses a strategy that opens the same building twice (B1 AC1). Allow repeated openings,
      each with its own `at_tick`/`offset`, and have the commander place all of them and train
      across every barracks that can produce the unit. **Ship the data unchanged** (one opening
      per building, as today), so the capability lands behaviour-neutral and every pinned
      `state_hash` golden is untouched — the tuning that follows is then separable from it.
- [ ] **Tune only RON** (`units.ron` HQ HP / costs / `mvp_combat` scaling, `resources.ron`
      economy, `strategies.ron` tempo — **never Rust**) to bring the decided-match median
      into 5–8 min with few timeouts — now with `queue_depth` and barracks count among the
      levers (the mass probes must all take the same count, or they stop being comparable). Ledger the
      levers tried, the one kept, and the before/after length distribution.
- [ ] **Re-run the batch and re-check the pentagon at the new length.** Report whether the
      F-025 broken link persists or was a short-game artifact.

Because content is data, a RON change moves every pinned per-tick `state_hash` golden. With
**no Rust touched**, any golden that moves is content-driven by construction — that is the
argument that licenses recomputing them, and it must be demonstrated, not asserted.

Then B3's remaining ACs are computed on valid-length matches.

## B3 — Metrics, report, kill-criteria gate

- [x] **Win-rate matrix** `W[i][j] = P(s_i beats s_j)`; row means = overall strength;
      mirror diagonal ≈ 0.5.
- [x] **The pentagon assertion** — the core test. The five mass-unit strategies should
      reproduce the designed cycle:
      `Sentinel > Ripper > Arclight > Bulwark > Ravager > Sentinel`.
      Report, for each predicted counter, whether it actually wins its matchup (>50%).
      A predicted counter that *loses* means the stats or the +30% nemesis magnitude are
      wrong — that's the sim doing its job.
- [ ] Match-length distribution (median, % hitting the cap) vs the 5–8 min target.
- [ ] **Kill-criteria PASS/FAIL** (from DESIGN_BRIEF): no strategy/unit win-rate >65%
      regardless of counter; mirrors within tolerance of 50%; matches terminate in target.
      **The mirror ~50% assertion lives here** (BALANCE_PLAN lists it under B2's probes, but
      B2 only made the sampling side-balanced; nothing asserts the rate). Size the seed count
      from a stated power calculation — enough to detect a few-percent seat bias, not to
      rubber-stamp one.
- [ ] Emit a stdout table + a machine-readable `balance_report.ron` (gitignored artifact).
- [ ] **Harden `batch::production_totals`** (B3 is its consumer): derive the column schema
      from the union of record keys, or refuse an unlabelled record — today it takes its
      header from `records.first()` and silently drops every later row's production if that
      row is unlabelled.

Critic probes: an injected imbalance (a deliberately broken multiplier fixture) makes the
gate FAIL; a strictly-dominant or strictly-losing strategy is surfaced by name; an
all-timeout run is flagged, not reported as balanced.

## B4 — First balance pass (the tuning loop)

- [ ] Run the harness; read the matrix + pentagon assertion.
- [ ] Tune **only RON** (`mvp_combat`, unit costs, `nemesis_bonus`, timings) toward the
      criteria; re-run; iterate.
- [ ] Ledger F-016+: the failing matchup(s), the change made, before/after win rates,
      final kill-criteria status.

## B5 — The human fun gate (not automatable — and required)

The sim proves **balance**, not **fun**. This gate is irreducible; only you can run it.

- [ ] Play real matches (the client, not headless): does a single fight feel like a
      *decision*? Do the counters *read* on screen? Is the 5–8 min arc satisfying, or a
      stalemate / coin-flip?
- [ ] Write a short fun-read into DESIGN_BRIEF or FINDINGS: what lands, what's flat, what
      to change.
- [ ] **Go / No-Go:** only if the core is *both* balanced and fun does it make sense to
      build nations / the campaign. Balanced-but-not-fun is a *design* fix (loop, tempo),
      not a reason to add features.

---

## Kickoff (Claude Code)

> Read BALANCE_PLAN.md, MVP_PLAN.md, and DESIGN_BRIEF.md. Implement **B1** as
> `rts-implementer` — promote `mvp_ai` into a `strategies` set and author the probe set
> (five mass-unit + mixed + rush + turtle) — test-first, then run `rts-critic`
> (diff + spec only) before commit. Stop at each checkbox for review.
