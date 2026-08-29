# FINDINGS.md — F-series ledger

One entry per scaling or design decision: the wall hit, what was measured, the decision, the evidence.

<!-- F-001: <title> — wall / measurement / decision / evidence -->

## F-001 — Nearest-enemy: brute force → uniform spatial grid (M2)

**Wall hit.** At battle scale the sim must find each unit's nearest enemy. The
naive pass compares every unit against every other of the opposite faction —
O(n²). With ~1–2k units that pass dominates a tick and won't hold as counts grow.

**Measurement.** Criterion bench `nearest_enemy` on the Ubuntu box (release), the
full "nearest enemy for every unit" pass over a deterministic uniform layout in a
2000×2000 world (seed `0xA11CE`), grid cell ≈ one unit per cell:

| N (units) | Naive (brute O(n²)) | Grid (build + query, end-to-end) | Speedup |
|----------:|--------------------:|---------------------------------:|--------:|
| 1000      | 2.156 ms            | 449.8 µs                         | **~4.8×** |
| 2000      | 15.84 ms            | 902.5 µs                         | **~17.6×** |

Naive grows superlinearly (2.16 → 15.84 ms, ~7.3× for 2× units) while the grid
grows ~linearly (449.8 → 902.5 µs, ~2× for 2× units), so the speedup widens with
N — exactly the O(n²) → ~O(n) story.

**Decision.** Replace the brute-force pass with `SpatialGrid` (uniform grid,
ring-expanding query) as the nearest-enemy path (introduced in commit
`020b12a`). Keep `brute_force_nearest_enemy` as the differential oracle: the grid
must return the byte-identical answer (ties broken to smallest index), verified
over 80 seeds and the edge cases.

**Evidence.** The table above (criterion medians on the box). Correctness is
pinned by the differential test `grid_matches_brute_over_many_seeds` (grid ==
brute force over 80 seeds, commit `020b12a`) plus the edge-case tests; the
speedup *mechanism* is pinned deterministically (not just wall-clock) by
`grid_visits_far_fewer_candidates_than_brute` — at N=2000 the grid evaluates
fewer than 1/10 the distances the naive scan does. Reproduce:
`cargo bench --bench nearest_enemy` and `cargo test --lib spatial`.

## F-002 — Group move: N× A* → one flow field (M3)

**Wall hit.** A group move sends many selected units to a *single* destination.
The naive implementation runs one A* search per unit — N independent searches
over the same grid toward the same goal — which is N× redundant work that scales
with the group size on every order.

**Measurement.** Deterministic node-expansion count (nodes popped/settled from
the frontier — the reproducible figure, not wall-clock) on a 48×48 obstacle field
(20% blocked, seed `0xC0FFEE`), goal at the far corner, the group = every
reachable cell as a unit start. Pinned by the L2 test
`group_flow_field_costs_far_less_than_n_times_astar`:

| Group (N units) | N× A* (nodes expanded) | One flow field, BFS (nodes expanded) | Work ratio |
|----------------:|-----------------------:|-------------------------------------:|-----------:|
| 1817            | 581,420                | 1,818                                | **~320×** |

The flow field expands each reachable cell exactly once (1818 ≈ the reachable
cell count), independent of N; N× A* re-expands the shared region N times, so the
gap widens with the group size. Wall-clock corroboration (criterion
`group_move_to_one_dest` on the box, release): N× A* **71.5 ms** vs one flow field
**58.0 µs** — **~1230×** (A* also carries per-node binary-heap overhead the BFS
doesn't, so the time ratio exceeds the node ratio).

**Decision.** A group move to one destination computes **one** flow field
(Dijkstra/BFS from the goal over the grid) that every unit follows via
`FlowField::next`, instead of N separate A* searches (commit `362a314`). A* is kept
for single-unit / distinct-goal pathing and as the reachability oracle: the flow
field must agree with A* on reachability (a non-goal cell has a flow direction iff
A* finds a path from it).

**Evidence.** The table above (deterministic node counts, the primary figure) and
the criterion medians. Correctness/agreement is pinned by
`flow_field_reachability_agrees_with_astar` (flow reachability == A* reachability
over every cell of a sealed-pocket map and a random field) and
`flow_field_next_steps_lead_to_goal` (following `next` reaches the goal in exactly
the field distance over walkable, edge-adjacent cells). Reproduce:
`cargo test --test m3_pathfind` and `cargo bench --bench pathfind`.

## F-003 — `Time<Fixed>` outside `FixedUpdate` leaks wall-clock into the sim (M4a)

**Wall hit.** The first M4a economy tests drove the sim chain from `Update` with
`app.update()` and read `Time::<Fixed>::delta_secs()` in `movement`. Two
byte-identical runs of the same setup diverged:
`(alloy, in_deposits, carried) = (150, 50, 50)` vs `(150, 100, 0)` after 900
steps — a whole gather trip out of phase.

**Measurement.** `gather_loop_is_deterministic_across_identical_runs` (five
workers, one deposit, no RNG) failed reproducibly. Cause: `Time<Fixed>` only has
its `delta` set when Bevy's fixed-update accumulator actually consumes a step,
and that accumulator is fed from **real elapsed time**. Systems reading
`Time<Fixed>` outside the `FixedMain` loop therefore see a delta of `0` for a
wall-clock-dependent number of frames — the sim's step size became a function of
how fast the machine ran.

**Decision.** The sim's notion of a tick is the fixed timestep, never a measured
duration. The headless harness now advances time explicitly —
`Time::<Fixed>::advance_by(timestep)` once per step, then `app.update()` — so one
`step()` is exactly one 60 Hz sim tick, and the shipped app keeps its sim systems
in `FixedUpdate` where the delta is the timestep by construction. Nothing in
`src/sim/` reads a clock other than `Time<Fixed>`; the economy itself counts
ticks (`mvp_gather_ticks`, `mvp_train_ticks` from the RON), not seconds.

**Evidence.** `gather_loop_is_deterministic_across_identical_runs` and
`placement_and_production_are_deterministic` (both in `tests/m4a_economy.rs`,
commits `ea0fdc8` / `63a4c33`) fail before the fix and pass after; the
tick-counted conservation test asserts the invariant on *every* tick, so a
drifting step size shows up as a phase difference immediately. Reproduce:
`cargo test --test m4a_economy`.

## F-004 — A system that only exists in the test harness is not shipped (M4a)

**Wall hit.** M4a's `training_a_unit_charges_once_at_order_time_and_spawns_on_completion`
passed while the shipped game was broken: `build_app()` registered
`(apply_commands, gather, movement)` on `FixedUpdate`, and `economy::production`
— the only system that advances a production queue and spawns the paid-for unit
— was registered *nowhere in `src/`*. It ran solely inside the test's own
`econ_app()`. In the real binary a Train order deducted Alloy and the unit never
arrived: charged, never delivered, never refunded. Found by the M4a critic.

**Measurement.** `tests/critic_m4a.rs::shipped_app_schedules_the_production_system`
reads `src/lib.rs` and asserts the registered `FixedUpdate` chain contains
`production`; it failed against the committed diff and passes now. The failure
mode is structural, not numeric: two hand-written system lists (one shipped, one
in tests) drift, and the test suite grades the copy it wrote itself.

**Decision.** There is exactly **one** definition of the sim chain —
`onus::add_sim_systems(app, schedule)` in `src/lib.rs`. `build_app()` installs it
on `FixedUpdate`; the headless tests install the same function on `Update`
(where they can hand the sim one fixed timestep per step, per F-003). No test may
hand-roll the list. A duplicated list is the bug; sharing the definition is the
fix.

**Evidence.** `cargo test --test critic_m4a` (6/6) and `--test m4a_economy`
(22/22) on the box, commit `5eb3919`. Reproduce: `cargo test`.

## F-005 — "Saturating" arithmetic is resource destruction (M4a)

**Wall hit.** `Stockpiles::add` used `saturating_add`, so a deposit that crossed
the `u32` ceiling silently vanished — while the module doc claimed Alloy is only
ever *moved*. The critic's ledger showed 6 Alloy destroyed
(`left: 4294967385, right: 4294967391`).

**Decision.** `add` now returns the amount **accepted** (`amount.min(u32::MAX -
balance)`) and the gather loop subtracts only that from the worker's `Carrying`;
the remainder stays in hand and the worker waits at the drop-off. Conservation
holds by construction rather than by staying below a magnitude. Same principle
applies to every future counter: cap the *intake*, never drop the difference.

**Evidence.** `tests/critic_m4a.rs::a_deposit_into_a_near_full_stockpile_destroys_no_alloy`
asserts `banked + carried + in-deposit` invariant on every tick starting from
`u32::MAX - 4`; red before, green after (commit `93c472a`).
