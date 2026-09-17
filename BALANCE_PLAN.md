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

## B3 — Metrics, report, kill-criteria gate

- [ ] **Win-rate matrix** `W[i][j] = P(s_i beats s_j)`; row means = overall strength;
      mirror diagonal ≈ 0.5.
- [ ] **The pentagon assertion** — the core test. The five mass-unit strategies should
      reproduce the designed cycle:
      `Sentinel > Ripper > Arclight > Bulwark > Ravager > Sentinel`.
      Report, for each predicted counter, whether it actually wins its matchup (>50%).
      A predicted counter that *loses* means the stats or the +30% nemesis magnitude are
      wrong — that's the sim doing its job.
- [ ] Match-length distribution (median, % hitting the cap) vs the 5–8 min target.
- [ ] **Kill-criteria PASS/FAIL** (from DESIGN_BRIEF): no strategy/unit win-rate >65%
      regardless of counter; mirrors within tolerance of 50%; matches terminate in target.
- [ ] Emit a stdout table + a machine-readable `balance_report.ron` (gitignored artifact).

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
