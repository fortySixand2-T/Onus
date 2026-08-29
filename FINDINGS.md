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
| 1000      | 2.1556 ms           | 453.05 µs                        | **4.76×** |
| 2000      | 15.871 ms           | 907.22 µs                        | **17.5×** |

Naive grows ~quadratically (2.16 → 15.9 ms) while the grid grows ~linearly
(0.45 → 0.91 ms), so the speedup widens with N — exactly the O(n²) → ~O(n) story.

**Decision.** Replace the brute-force pass with `SpatialGrid` (uniform grid,
ring-expanding query) as the nearest-enemy path. Keep `brute_force_nearest_enemy`
as the differential oracle: the grid must return the byte-identical answer
(ties broken to smallest index), verified over 80 seeds and the edge cases.

**Evidence.** The table above (real criterion medians on the box). Correctness is
pinned by `grid_matches_brute_over_many_seeds` and the edge-case tests; the
speedup *mechanism* is pinned deterministically (not just wall-clock) by
`grid_visits_far_fewer_candidates_than_brute` — at N=2000 the grid evaluates
fewer than 1/10 the distances the naive scan does. Reproduce:
`cargo bench --bench nearest_enemy` and `cargo test --lib spatial`.
