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
- [x] **Separate the cap from the target band** (decided 2026-09-20). The 8-min cap and the
      5–8 min band are the same number, so any realistic spread has its tail *censored*: at the
      shipped cap the tuned candidate read 10.8% timeouts and left `bulwark > ravager`
      **undefined** (0 decided), while the same content at a 20-min cap read median 6:32,
      max 10:45 and **0 timeouts** (F-029). A cap is an anti-stalemate backstop, not a
      statement of design intent. Raise `DEFAULT_MATCH_SECS` (`src/headless.rs`) to **15 min**
      — the one Rust line the RON-only rule bends for, and it is harness config, not content —
      and report **"% of decided matches inside the 5–8 min band"** as the design metric, with
      the timeout rate kept separately as the stalemate signal.
- [x] **The armour grind: a misdiagnosis, and a measured negative** (closed 2026-10-02,
      F-030 + F-031). The premise of this box was wrong. The arithmetic is real — a Bulwark
      mitigates `armor 9 × mitigation_per_armor 2 = 18` against its own `offense 4 ×
      damage_per_offense 5 = 20` — but it was never what clipped the heavy class. Tick-by-tick
      instrumentation of the Bulwark mirror found **two 25-unit armies, zero casualties, both
      HQs untouched at ten minutes**: the armies were not fighting at all. The length came from
      `attack_at_army: 20` against a 5× slower economy, so a match was "time to assemble twenty
      units" and the first wave home ended it. Fixed instead by **dropping commitment
      thresholds** (probes 20 → 10) and **raising HQ HP** (`building_hp_per_defense` 40 → 420),
      so a loser rebuilds and fights again; density went from 9.9 casualties a match to 30.4.
      `mitigation_per_armor` 2 → 1 was then measured twice *in the regime where fights happen*
      and **rejected both times**: it cuts heavy fights 18–22% and the light end 1%, but costs
      ~7–9 points of band share (36.5% → 29.1% on matched seeds, 2.2 SE) by compressing matches
      *below* the 5-min floor while taking almost nothing off the cap, and it does not help the
      pentagon either. **Unmet remainder, deferred to B4:** the residual censoring is the
      glass-cannon Arclight (`offense 9 / defense 2 / armor 2`) against armour — `mass_arclight`
      vs `mass_ravager` 18/100 and vs `mass_bulwark` 14/100 reach the cap. No cell is
      `Undefined`, and the worst survives charging every timeout to the predator as a loss
      (88.0%, CI [80.2, 93.0]), so B3 has a readable matrix — but that residue is a unit-stat
      question, not a tempo one.
- [x] **Tune only RON** (`units.ron` HQ HP / costs / `mvp_combat` scaling, `resources.ron`
      economy, `strategies.ron` tempo — **never Rust**) to bring the decided-match median
      into 5–8 min with few timeouts — now with `queue_depth` and barracks count among the
      levers (the mass probes must all take the same count, or they stop being comparable). Ledger the
      levers tried, the one kept, and the before/after length distribution.
      **Result** (F-029 → F-030 → F-031): median **1:16 → 6:21–6:36**, inside the band on every
      independent sample, at **2–3% timeouts**, with density up from 10.6 to ~44 units built a
      match — so F-026's trap (a longer clock bought by a smaller army) is absent. Levers kept:
      `mvp_carry_capacity` 10 → 2, three barracks per mass probe (**scripted, not realised** —
      F-035: under this economy no probe affords a third opening and only `mass_ripper` a
      second, at tick ~9 330; the scripts now list only what is placed, so the Ripper readings
      are of a two-line army and the other probes' of one barracks), `attack_at_army` 3 → 10,
      `building_hp_per_defense` 40 → 420. Rejected with numbers: HQ HP as a lengthener,
      `hp_per_defense` 20/14/10, `mitigation_per_armor` 1, `mvp_gather_ticks` 120. `mvp` is
      deliberately untuned (its pinning test would change meaning); the cost is recorded.
      **The band itself is not met and cannot be by tempo:** only **31–38%** of decided matches
      land in 5–8 min, with **39–44% finishing under 5:00** because the Ripper and Sentinel
      mirrors end at 2:29 and 3:38 and no commitment threshold lengthens a mirror. That floor is
      B4's to lift.
- [x] **Re-run the batch and re-check the pentagon at the new length.** Report whether the
      F-025 broken link persists or was a short-game artifact.
      **Answer: it was an artifact of the short game, and of an 8-match sample.** `bulwark >
      ravager`, pinned by F-025 at exactly 0.0%, measures **82.5–85.7%** — the link is not
      broken, it was *reversed* by the old tempo. At 25 seeds / 1 250 matches the cycle reads
      **4 holding + 1 undetermined** (`ripper > arclight` 93%, `arclight > bulwark` 96.5–100%,
      `bulwark > ravager` 82.5–85.7%, `sentinel > ripper` 64–72%); **`ravager > sentinel` is a
      coin flip** — five readings pool to 54.9%, CI [50.2, 59.5] — and settling it to ±5 needs
      ~150 seeds once a seed's 4 correlated matches are discounted. F-030's `sentinel > ripper`
      FAILS was itself sampling noise. **Standing lesson (F-031):** a CI excluding 50% on one
      seed base is a hypothesis, not a verdict — reproduce on a disjoint base before writing
      "holds", and note that `PentagonReport::holding()` is a bare `rate > 0.5` with no interval,
      which is why `tests/b3_pentagon.rs` should assert with one.

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
- [x] Match-length distribution (median, % hitting the cap) vs the 5–8 min target.
      **Result**: `metrics::LengthDistribution` — band counts, band share (in-band ÷ decided)
      and p0/10/25/50/75/90/100 over **decided** matches only; timeout rate (÷ all) and the
      same percentiles over **all** matches, timeouts at the cap, beside them (`tests/b3_length.rs`).
- [x] **Kill-criteria PASS/FAIL** (from DESIGN_BRIEF): no strategy/unit win-rate >65%
      regardless of counter; mirrors within tolerance of 50%; matches terminate in target.
      **The mirror ~50% assertion lives here** (BALANCE_PLAN lists it under B2's probes, but
      B2 only made the sampling side-balanced; nothing asserts the rate). Size the seed count
      from a stated power calculation — enough to detect a few-percent seat bias, not to
      rubber-stamp one.
      **Result** (F-038, F-039): `gate::KillGate`, status read off clustered Wilson intervals
      (ICC 0.17; mirrors at the measured seed-clustered deff 1.38). Detecting a 5-pt seat
      bias takes 1 080 decided mirrors and passing +/-5 takes 1 451: 73 full-roster seeds,
      infeasible on the box, so it ran as a mirror-only batch. K2 (pooled, plus any
      per-mirror FAIL) **PASSes** at 62 seeds: slot A 50.9% [47.6, 54.1], left 51.0%
      [47.7, 54.3]; detectable ~4.7 pts (z-test), gate FAILs at ~9.6 pts.
      K3 gates band share (in band / decided, at least 50%) and timeouts (at most 5%); the
      median is reported context (corrected after the B3 critic). The 5-probe batch reads
      **FAIL** on K3: band share 36% [25.9, 47.6].
- [x] Emit a stdout table + a machine-readable `balance_report.ron` (gitignored artifact).
      **Result** (F-040): `report::BalanceReport` holds the win matrix, the pentagon verdicts with
      intervals, the length distribution and the kill gate. It round-trips through RON, and
      `balance` prints it and, on request, writes it (`--report PATH`, opt-in). The first full-roster run (4 seeds,
      800 matches) reads **FAIL**: `turtle` is dominant at 99.3% [94.9, 100.0], and K3 FAILs on
      band share 43.8% [39.7, 48.0] (median 5:33, timeouts 1.8% PASS).
      `rush` (6.2%) is named `losing`: row interval wholly below 35%, named, not gated (F-038).
- [x] **Harden `batch::production_totals`** (B3 is its consumer): derive the column schema
      from the union of record keys, or refuse an unlabelled record — today it takes its
      header from `records.first()` and silently drops every later row's production if that
      row is unlabelled.
      **Result** (F-037): union of the records' headers, first-appearance order, summed by
      name; an unlabelled block cannot carry a count, so skipping it is lossless
      (`tests/b3_totals.rs`).

Critic probes: an injected imbalance (a deliberately broken multiplier fixture) makes the
gate FAIL; a strictly-dominant or strictly-losing strategy is surfaced by name; an
all-timeout run is flagged, not reported as balanced.

## B4 — First balance pass (the tuning loop)

- [ ] Run the harness; read the matrix + pentagon assertion.
- [ ] Tune **only RON** (`mvp_combat`, unit costs, `nemesis_bonus`, timings) toward the
      criteria; re-run; iterate.
- [ ] Ledger F-016+: the failing matchup(s), the change made, before/after win rates,
      final kill-criteria status.
- [ ] **Multi-barracks as a real capability (open, from F-035).** The AI places an opening
      only when the stockpile covers its cost, and its army step spends the stockpile on
      anything cheaper first, so under B3.5's economy openings past the first rarely go up
      (only `mass_ripper`'s second does). Add opening reservation — the army step holds back
      the Alloy of a due opening — so scripted production lines are built. It changes every
      multi-opening strategy and moves goldens, so it lands with its own proof. Then make the
      five mass probes' opening lists identical again (empty
      `b1_probe_set::MASS_PROBE_OPENING_EXCEPTIONS` and drop the per-probe exceptions in
      `critic_b1_ac3` / `critic_b35_armour`).

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
