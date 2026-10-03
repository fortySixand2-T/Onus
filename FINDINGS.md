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

**Extension (M4b).** The same class resurfaced in combat and the M4b critic
caught it: `damage_per_hit` widened the nemesis multiply to `u64` and then cast
back with a plain `as u32`, which *wraps*. At offense 1e9 the "+30% bonus"
came out as `1_288_490_187` against an unmitigated base of `4_294_967_295` — a
70% penalty (`the_nemesis_bonus_is_never_smaller_than_the_base_damage`). Two
fixes, because either alone is half a fix: the cast now saturates
(`.min(u32::MAX as u64)`), **and** `Content::validate` bounds every design stat
by the data-declared `mvp_combat.max_stat` and rejects any scaling whose peak HP
or peak nemesis hit does not fit the `u32` the sim counts in. Validation that
admits values the arithmetic cannot represent is the actual defect; saturation
is only the backstop for content built in memory. Evidence:
`cargo test --test critic_m4b` and
`out_of_scale_stats_are_rejected_and_the_bonus_never_wraps`, commit `d5f54f7`.

**Extension 2 (M4b, second critic pass).** The *guard* was itself unguarded
arithmetic: `validate` proved representability with
`max_stat * damage_per_offense * mult_milli` in raw `u64` over three unbounded
RON fields. Two faces of one defect, and the builds disagreed about which
content was legal — the worst possible outcome for a determinism project:
`max_stat: 4000000000` + `damage_per_offense: 4000000000` **panicked** in debug
(where `load_from_dir` promises an `Err`), while
`max_stat: 134217728`, `damage_per_offense: 67108864`, `damage_mult: 2.048`
multiplied to exactly `2^64`, wrapped to `peak_damage == 0`, and **loaded** in
release — admitting a roster whose base damage (9_007_199_254_740_992) only the
saturating backstop could evaluate, which is precisely what the check exists to
forbid. Fix: the validator uses `checked_mul` throughout and treats `None`
(does not fit `u64`) exactly like a product that does not fit `u32` — a
rejection. Peak HP, base damage, nemesis damage and armor mitigation are all
proven this way. The same audit added `is_finite` to every float tunable
(`f32::INFINITY > 0.0` is true, so a bare `> 0.0` admitted it) and made
`Health::from_def` use `saturating_mul` like the rest of the derived stats.

The general rule this ledger has now paid for three times: **an arithmetic
check written in the same unchecked arithmetic it is checking is not a check.**
Validators use `checked_*`; runtime derivations saturate; and a rejection is an
`Err` in every build profile. Evidence: `tests/critic_m4b.rs`
(`a_degenerate_scale_is_rejected_not_overflowed_inside_the_validator`,
`accepted_content_never_needs_a_saturating_hit`) plus
`a_scale_the_validator_cannot_multiply_is_an_error_not_a_panic`; both
`cargo test` and `cargo test --release` are green — the release run is
load-bearing here, since the wrap is invisible in debug. Commit `d861f23`.

**Extension 3 (M4b, third critic pass).** The last raw cast on the damage path:
`mult_milli` was `(damage_mult * 1000.0).round() as u32` over a float `validate`
only required to be finite and `>= 1.0`. `damage_mult: 5000000.0` loaded
happily, saturated to `u32::MAX` per-mille, and the sim applied ≈4_294_967×
instead of the stated 5_000_000× — and the representability proof above then
read *through* that saturated stand-in, bounding the content against a number
the data never contained. Fix: `NemesisBonus::milli_exact` returns
`round(damage_mult * 1000)` only when it is finite and fits `u32` (computed in
`f64`, so the shipped `1.3f32` still rounds to exactly 1300), `validate` rejects
content where it is `None`, and the peak-damage bound is computed from that
checked value. For anything the loader accepts, `mult_milli` *is* the exact
per-mille, so the formula documented in `units.ron` holds as written rather
than approximately. Evidence:
`tests/critic_m4b.rs::a_nemesis_multiplier_is_rejected_or_applied_as_written`
and `a_nemesis_multiplier_is_exact_or_refused` (which also pins 1.0/1.15/2.0),
commit `b48be41`.

## F-006 — Combat damage is integer arithmetic; the design stats are scaled in data (M4b)

**Wall hit.** The roster's stats are a 1-10 *design* scale (`offense: 4`,
`defense: 9`) and the nemesis bonus is a float (`damage_mult: 1.3`). Neither is
usable as-is: a 1-10 HP pool dies to one hit, and a float multiplier in the
per-hit path makes damage a function of the FPU — the exact failure mode M5's
per-tick state hash exists to catch.

**Measurement.** Two decisions, both pinned by tests rather than by argument:

| Question | Choice | Pinned by |
|---|---|---|
| 1-10 → sim numbers | `mvp_combat` block in `units.ron`: `hp_per_defense: 20`, `damage_per_offense: 5`, `mitigation_per_armor: 2`, `speed_per_point: 36.0` | `defense_is_the_hp_pool_and_offense_is_damage_per_hit`, `armor_is_flat_mitigation_per_hit`, `movement_speed_comes_from_the_units_ron` |
| ×1.3 rounding | integer per-mille: `floor(base * 1300 / 1000)`, one `f32::round` at load in `NemesisBonus::mult_milli` | `the_nemesis_multiplier_is_integer_per_mille`, `nemesis_adds_30_percent_and_ignores_armor` |

`speed_per_point: 36.0` is not arbitrary: the retired global `sim::SPEED` was
180 u/s and the Worker's `speed` is 5, so 180/5 = 36 turns the constant into
per-unit data **without moving any M4a timing** — the whole M4a economy suite
(22 tests) passes unchanged across the retirement.

**Decision.** Every number combat multiplies is RON data, and the per-hit path
is `u32`/`u64` only. `damage_per_hit` is the single place damage is decided
(sim and tests call the same function); the one float→int conversion happens on
a constant at load. `Content::validate` rejects a unit with no HP pool, no
speed, offense without a cadence/reach, or a cadence/reach without offense — a
missing `mvp_attack_ticks` is a load error, never a silent `0` that would let a
unit fire every tick.

**Evidence.** `cargo test --test m4b_combat` (15/15) and `cargo test --lib
combat` (5/5) on the box, commits `4509ab1` / `92db62b` / `ffe6ef1`. The AC3
tests were run red first with the nemesis branch stubbed out
(`left: 12, right: 26`) and green after.

## F-007 — Combat resolves from a start-of-tick snapshot, and chasing needs a leash (M4b)

**Wall hit.** Two failure modes that only appear once units can die:
1. If attackers mutate HP as they are iterated, the *order* they are iterated in
   decides who dies — and a unit killed early in the tick never swings back.
   With ECS archetype order that is not even stable, which is exactly the
   "iteration order affects outcomes" invariant M5 forbids.
2. `an_attacker_paths_around_a_wall_to_reach_its_target` failed on the first
   run: the attacker acquired a target 192 units away (inside the 220 engage
   radius), started around the wall, and *dropped* it — walking the detour put
   the straight-line distance at ~295. It then re-acquired, turned back, and
   oscillated at the obstacle forever.

**Decision.**
1. One tick of combat is computed from a snapshot taken before anything is
   written: targets, distances and HP all come from the start-of-tick state,
   damage accumulates in a local ledger, and a single final pass writes HP back
   and despawns the dead. Processing order therefore cannot change the outcome
   (entities are still walked in ascending `Entity::to_bits()` so the *commands*
   are stable), two units that kill each other on one tick both die, and each
   death is applied — and credited to `Casualties` — exactly once.
2. Engagement takes two radii, both data: `engage_range` (220) is the radius in
   which an *idle* unit picks a fight; `pursue_range` (400) is the leash a unit
   already chasing keeps until it gives up. `validate` requires
   `pursue_range >= engage_range`.

**Evidence.** `a_mutual_kill_on_one_tick_despawns_both_exactly_once` (both die,
`Casualties::total() == 2`, and still 2 sixty ticks later),
`hp_never_underflows_on_overkill`, `a_battle_is_deterministic_across_identical_runs`
(20 mixed units, 900 ticks, byte-identical survivor state) and the wall test
(the attacker's cell is asserted walkable on *every* tick of the approach), all
in `tests/m4b_combat.rs`, commit `4509ab1`. Reproduce: `cargo test --test m4b_combat`.

## F-008 — A component that asserts ownership must be maintained by its owner alone (M4b)

**Wall hit.** M4b's combat rule "a unit on a gather job never auto-engages" read
the `GatherTarget` component as the claim *"the economy owns this unit"*. The
claim was not maintained: `Order::Gather` stamped `GatherTarget`/`GatherPhase`
onto **every** entity in the order (right-clicking a deposit with a mixed
selection sends it to soldiers too), and `economy::gather` skipped a
non-gatherer with `if !def.gathers { continue; }` without ever clearing it. The
marker therefore stuck forever, and the moment combat started reading it, one
mixed-selection right-click **permanently disarmed every combat unit in the
selection** — they never acquired a target, never fired, never defended
themselves, until some unrelated `MoveTo` happened to strip the component. AC1
broken by a component that lied. Found by the M4b critic; the stale-marker jank
was pre-existing M4a behaviour that was harmless only while nothing read it.

**Decision.** The claim is now true by construction, fixed at both ends rather
than at the reader:
- `apply_commands` gates the gather half of `Order::Gather` on the unit's
  `gathers` flag from the RON — a unit that cannot gather is never given a job
  (it still obeys the *move* half of the order, which is what the commander
  meant);
- `economy::gather` **clears** any marker it declines to service instead of
  skipping past it, and its query takes `Carrying` as `Option` precisely so it
  can see — and take back — a marker held by an entity that has no business
  with one.

Generalised: if system A's behaviour depends on a component that means "system B
owns this entity", then B must be the only writer of that component *and* must
release it whenever the claim stops holding. A reader can never make a stale
claim true.

**Evidence.** `tests/critic_m4b.rs::a_gather_order_does_not_permanently_disarm_a_non_gathering_soldier`
(red before, green after) plus, in our suite,
`a_gather_order_to_a_mixed_selection_leaves_the_soldier_armed` (the soldier
fires on the order tick, still walks to the node, and the real gatherer's loop
still banks Alloy) and `the_economy_clears_a_gather_marker_it_will_not_service`.
Reproduce: `cargo test --test critic_m4b --test m4b_combat`.

**Extension (M4c) — the pairing is now structural, not a convention.** F-008's
fix left "always write and release `GatherTarget` + `GatherPhase` together" as a
rule every call site had to remember; combat reads the target alone, so a lone
target is still a permanently disarmed unit. A rule maintained by discipline is
a rule that a new call site (an AI issuing gather orders) will eventually break.
So the pairing is now enforced by the type system and by having exactly one
implementation of each direction:

- `GatherPhase` is a **required component** of `GatherTarget`
  (`#[require(GatherPhase)]`, default `ToNode`), so *whoever* writes the claim —
  the order path, a test, a future system — writes at least a phase with it;
- `economy::release_gather_job` is the only place either half is removed, and
  every release site in `src/` calls it.

**Evidence.** `tests/m4c_ai.rs::a_gather_claim_can_never_be_written_without_its_phase`
(inserting the target alone still yields a phase) and
`a_gather_claim_is_always_released_as_a_pair`, plus the per-tick invariant in
`the_ai_puts_its_workers_on_a_deposit_and_banks_alloy` (600 ticks of AI mining,
asserting `has_target == has_phase` every tick). Commit: M4c order-ownership.

**Extension 2 (M4c critic, twice) — the claim carries a scheduling constraint,
and a doc must quantify over what was verified.** `#[require(GatherPhase)]`
covers *insertion* only; a bare `remove::<GatherPhase>()` still leaves a lone
`GatherTarget`, which `economy::gather` cannot see (its query needs both halves)
while other systems still read the surviving target. The release side is now
structural too: `economy::repair_gather_claims` sweeps every entity holding
exactly one half and drops the claim.

**A sweep is worth exactly its position in the schedule.** The first version ran
after `apply_commands` and was documented as running "before the tick's gather
and combat passes" — and then concluded "a half-claim therefore cannot survive
into any reader". The premise named two passes; the conclusion quantified over
all readers, and there was a third: `ai::ai_commanders` reads `GatherTarget` to
decide which workers are idle, and it ran *upstream* of the sweep, so a lone
target was read as a live job and the worker sat unemployed for a whole
`think_interval_ticks`. Same defect, one reader further out — F-008's third
recurrence, caught because the doc's own absolute claim invited the check.

So the rule for this component is now explicit, and this entry is where it lives:

- **`GatherTarget` has an ordering constraint.** `economy::repair_gather_claims`
  runs first among the systems that play the match (only `victory::match_watch`
  precedes it, and it reads no claim).
- **The readers it protects are exactly the ones ordered after it**, today:
  `ai::ai_commanders` (who is idle), `economy::gather` (run the job),
  `combat::combat` (the economy owns this unit).
- **Any new reader of `GatherTarget` must be ordered after the sweep.** A reader
  placed before it sees half-claims, and no amount of `#[require]` or
  single-release discipline will save it. This is also why the sweep is a
  separate system rather than folded into `gather`.

And the process lesson, which cost three passes: *state a doc claim over exactly
what you verified.* "Cannot survive into `gather` or `combat`" would have been
true and would have made the missing reader obvious; "cannot survive into any
reader" was false and stopped the next person from checking. Where a fix depends
on system order, the constraint belongs in FINDINGS — the `src/` comment alone
did not stop a new reader from being added upstream of it.

**Evidence.** Red: `tests/critic_m4c.rs::a_split_gather_claim_is_still_read_as_a_job_by_the_ai`
and `a_worker_with_a_half_claim_is_not_left_idle_for_a_whole_think_interval`
(`FAILED. 25 passed; 2 failed`). Green after the reorder, plus, in our suite,
`the_ai_never_reads_a_half_claim_as_a_job`,
`no_reader_ever_observes_a_half_claim_during_a_live_match` (a claim split every
7th tick of a live AI match; no half-claim ever survives a tick and the economy
keeps banking) and — the direction the reorder could break, since the sweep now
precedes `apply_commands` — `a_claim_created_this_tick_survives_the_next_ticks_sweep`
and `the_sweep_never_confiscates_a_real_gather_job`.

## F-009 — An order with no issuer is a capability, not an intent (M4c)

**Wall hit.** Through M4b every `Order` was anonymous: `Order::Train` charged the
*targeted building's* faction, and `MoveTo`/`Gather` commanded whatever entities
they named. With one commander that is invisible; the moment M4c put a second
commander on the field it becomes "spend the enemy's Alloy, fill the enemy's
production queue, walk the enemy's army off a cliff". The queue was a bag of
capabilities — holding an `Entity` *was* the authority to command it.

Two smaller versions of the same shape came with it: `apply_commands` used
`Commands::entity(e).insert(..)`, which **panics** on an already-despawned
entity (unreachable while the only producer was a live query in the same tick;
immediately reachable once an AI issues orders against entities it remembered),
and the sim had no notion of "who is playing" at all.

**Decision.** Orders carry their issuer, and ownership is checked in the one
place orders are applied.

- `Order::By { issuer, order }`, built with `.issued_by(faction)`, is a
  *signature* around an order. `apply_commands` peels it, then refuses: a
  `Train` against another faction's building, a `MoveTo`/`Gather` naming another
  faction's units (per unit — a mixed list still commands the issuer's own), and
  a `Place` for a faction other than the signer.
- An order signed twice by different factions is voided rather than resolved to
  either: a signature that can be overwritten is not a signature.
- A wrapper variant, not a field on every variant, so an **unsigned** order stays
  expressible. Unsigned means *self-signed*: attributed to whatever it touches,
  which is exactly the pre-second-commander behaviour, which is why fixtures
  that drive one faction's economy directly still work. That is only safe
  because nothing in `src/` emits one — pinned by
  `every_order_emitted_in_src_is_signed`, a source-level test that reads every
  paren-balanced `push_back(<expr>)` in `src/` and requires an `Order`
  expression to be signed.
- Every order path takes `Commands::get_entity` + `try_insert`, so an order
  against a dead entity is inert instead of fatal.

The scripted AI then needs **no** privileged path into the sim: it pushes signed
orders onto the same `CommandQueue` the mouse writes to, and is charged and
refused by the same code. Anything the AI can do, a player could have done.

**Evidence.** Red first: with the signature type present but the checks removed,
`tests/m4c_ai.rs` fails 6/9 (`a_train_order_against_another_factions_building_is_refused`,
`a_place_order_cannot_build_for_another_faction`, `a_gather_order_never_tasks_another_factions_worker`,
`a_move_order_commands_only_the_issuers_own_units`, `a_doubly_signed_order_is_refused`,
`a_gather_claim_can_never_be_written_without_its_phase`). Green after, with
`a_train_order_against_ones_own_building_still_trains` and
`an_unsigned_order_is_self_signed` guarding the other direction, and
`the_ai_commands_only_its_own_side` asserting it per tick for 3000 ticks.
Reproduce: `cargo test --test m4c_ai`.

**Extension (M4c critic) — "self-signed" was a label, not a property.** The
first cut resolved an unsigned order to `None` and had `commandable` return
`true` for every entity it named. That is not "attributed to what it touches",
it is *unchecked*: one `Order::MoveTo { units: [a_unit, b_unit] }` commanded both
factions at once, which no commander may do. Worse, the only thing standing
behind the claim was `every_order_emitted_in_src_is_signed` — a **text scan** for
the substring `Order` in the pushed expression, evaded by binding the order to a
local first, by a helper that returns one, by a macro, or by an alias. A test
that scans source text for a spelling is not a guarantee about values.

Both are now closed in the types and in the check:

- The queue's element type is `SignedOrder` — an `Order` **plus** its
  `Attribution` (`By(faction)` / `SelfSigned` / `Void`). `CommandQueue.0` is an
  `OrderQueue` whose `push_back` takes `impl Into<SignedOrder>`, so attribution
  happens once, at the boundary, for every producer: there is no way to enqueue
  an order carrying no attribution at all, whatever the call site is spelled
  like.
- `SelfSigned` is a *checked* mode. `subject_issuer` derives the issuer from the
  entities the order names: one faction (plus unowned entities) ⇒ that faction;
  **two factions ⇒ the order is refused whole**, because there is no commander it
  could have come from and half-applying it is precisely the "commands both
  sides" bug. A *signed* order naming both sides still commands the signer's own
  units and ignores the rest — it says who it is from, so the foreign entries are
  noise rather than ambiguity.
- Signatures that disagree resolve to `Void`, which `apply_commands` drops.

The text-scan test was **deleted**: keeping it would imply the weaker check still
carries weight. What it was standing in for — "a producer that forgets to sign
cannot do damage" — is now true by construction, since a forgotten signature
yields a coherently self-signed order that can only command one side.

The residual limit, stated plainly: `Order::issued_by` must keep returning
`Order` and `CommandQueue.0.push_back` must keep accepting a bare `Order`,
because `tests/critic_m4a.rs`, `critic_m4b.rs` and `critic_m4c.rs` construct and
push order values literally (`fn push(app: &mut App, order: Order)`, and
`push(&mut app, o.issued_by(Faction::A))`). So an unsigned `Order` *value*
remains constructible; what is no longer possible is an unsigned order reaching
the sim **unattributed**, or any order — signed, unsigned or re-signed —
commanding two factions.

**Evidence.** Red: `tests/critic_m4c.rs::one_unsigned_order_cannot_command_both_factions_at_once`
failed (`FAILED. 14 passed; 1 failed`) with the old `commandable(.., None, ..) =>
true`. Green after, plus, in our suite,
`an_unsigned_order_naming_two_factions_is_refused_whole` (both list orders, both
list orderings, neither unit commanded),
`a_signed_order_naming_two_factions_still_commands_its_own`,
`the_queue_can_only_hold_attributed_orders`, and — the direction a too-strict fix
would break — `every_legitimate_order_still_applies_under_coherent_self_signing`,
which runs a 6000-tick AI match and requires all four order variants (gather,
train worker, place barracks, train army) to still land.

## F-010 — Ending a match is a run condition, not a flag every system checks (M4c)

**Wall hit.** "Win = destroy the enemy HQ; the match then terminates" has three
ways to go wrong, and all three are determinism bugs rather than gameplay bugs:
the end fires twice (or fires on a different tick depending on which HQ the
query yields first); the sim keeps running afterwards, so the *recorded* outcome
stops matching the state; or the end check declares a winner in every fixture
that only spawns one side's base, freezing 100+ existing tests.

**Decision.**
- The outcome is **sim state** (`MatchState`, holding the tick, whether the
  match is contested, and `Option<MatchOutcome>`), not a driver flag. The driver
  reads it; the headless AI-vs-AI run reads the same thing.
- The check counts standing victory buildings per faction into a fixed-size
  `[u32; 2]` and decides from the two counts — no iteration order, no early
  return on "the first HQ I found". Both HQs falling on one tick is a *draw*,
  not a race.
- Termination is a **run condition**: the whole play chain is
  `.run_if(match_running)` and only the check itself runs afterwards. Nothing
  can move, spend or shoot after the result is recorded, which makes "exactly
  once" and "the recorded outcome stays true" the same statement.
- A match becomes decidable only once both sides have had a victory building at
  the same time (`engaged`). Every one-sided fixture in M1–M4b keeps running.
- What counts as the victory building is content (`victory: true` in
  `mvp_buildings`), and `Content::validate` requires exactly one — zero makes the
  match unwinnable, two make "the enemy HQ" ambiguous.

**Measurement (the ≤ ~8 min target).** AI-vs-AI on the symmetric fixture, three
seeds, decision tick: seed 1 → 4880 (A), seed 7 → 4625 (B), seed 99 → 4852 (B).
That is 77–81 s of sim time against a 28 800-tick (8 min) budget, so the target
holds with ~6× headroom; the test asserts the budget and prints the measurement
rather than hardcoding a length.

**Evidence.** Red first: with `match_end` unregistered and buildings excluded
from the combat snapshot, 6 tests fail (including "no decision in 28800 ticks").
Green after: `destroying_the_enemy_hq_ends_the_match`,
`nothing_runs_after_the_match_is_decided` (per-tick freeze of position, HP, the
sim tick counter, Alloy and production for 300 ticks past the end),
`losing_both_hqs_on_one_tick_is_a_draw_in_either_order`,
`a_match_with_only_one_hq_never_ends`,
`an_ai_vs_ai_match_is_decided_within_eight_minutes`, and
`an_ai_vs_ai_match_replays_identically_from_its_seed`.
Reproduce: `cargo test --test m4c_ai`.

## F-011 — A command log cannot address entities by `Entity` (M5)

**Wall hit.** M5's replay was implemented the obvious way: log every applied
command with the tick it applied on, storing entities as `Entity::to_bits()`,
then feed the log back into a freshly built copy of the starting world. The
first full test —
`a_replay_of_the_persisted_log_reproduces_the_match_tick_for_tick`, an
AI-vs-AI match compared hash for hash — reproduced **299 ticks exactly** and
then diverged. Not a rounding difference, not a phase shift: at tick 300 both
sides place their Barracks, and the recording's two new buildings came out at
entity ids `…62`/`…63` while the replay's came out at `…61`/`…62`.

**Measurement.** Counting entities per tick in the two apps showed the replay
one ahead **before either had stepped**: 32 entities in the recording's world,
33 in the replay's. The single difference between them was `insert_resource`.
In Bevy 0.19 a resource *is* an entity, so **adding one resource shifts every
entity id the world hands out afterwards** — and a replay app has, by
construction, at least one resource the recording did not (`ReplaySource`, and
usually `StateHashLog` too). Every entity that existed before the first tick
still matched, which is exactly why the failure hid for 299 ticks: only
*newly spawned* things (a placed building, a trained unit) landed on shifted
ids, and a log naming them then commanded the wrong ones — silently, because
`Entity` bits from another app are still perfectly valid `Entity` bits.

**Decision.** Entity ids are an **ECS allocation detail, not sim state**. The
sim now issues its own coordinate:

- `SimId(u64)` — a component assigned by `replay::identify`, an exclusive
  system that gives every *thing in the world* (anything with a `Position`) the
  next id in sequence, new entities in ascending `Entity::to_bits()` order (the
  stable-order convention of F-007). `SimIds` is the registry, a `Vec` indexed
  by id — never a map.
- `identify` runs **twice** in the one chain: at the head (everything the tick
  reads has an identity) and at the tail (everything the tick *created* has one
  before that tick is hashed, and before the next tick's orders can name it).
  It is idempotent.
- The command log stores `SimId`s, and the state hash is **keyed** on them.
  Both are therefore functions of the sim's own spawn sequence and of nothing
  else about the app they run in.
- A logged id the registry has never issued is an error, not a guess; a log
  naming an *unidentified* entity (`SimId::UNIDENTIFIED`, a bare fixture entity
  with no `Position`) is refused at load, because no replay can resolve it.

The general rule, and it will apply again at M6 (a command crossing a process
boundary cannot carry an `Entity` either): **anything that leaves the tick —
to disk, to a hash, to another machine — must be addressed in the sim's own
coordinates.** An identifier the engine allocates is only meaningful inside the
one `World` that allocated it.

**Evidence.** Red: the replay diverging at tick 300 (`Some(300)` from
`StateHashLog::first_divergence`), plus the entity-count probe (32 vs 33 before
the first tick). Green after, over a full match:
`a_replay_reaches_the_same_verdict_on_the_same_tick` (seed 7 decides on tick
4625; the replay reaches the same verdict on the same tick, every tick's hash
equal), `a_replay_of_the_persisted_log_reproduces_the_match_tick_for_tick`
(3000 ticks through a file), and the direct probe
`sim_ids_and_the_state_hash_survive_a_difference_in_entity_allocation` — two
apps whose entity ids provably differ (asserted) agree on every `SimId` and on
the state hash. The direction a vacuous pass would hide:
`a_log_missing_one_command_replays_into_a_different_match`. Reproduce:
`cargo test --test m5_replay`.

## F-012 — The tick tag is on the command, and the state hash is opt-in (M5)

**Wall hit / decisions.** Three M5 choices worth their own record, each with
what pinned it:

| Question | Choice | Pinned by |
|---|---|---|
| Where does the tick tag live? | On the *envelope*, not the order: `Command = SignedOrder + CommandTick`, and `OrderQueue` holds `Command`s. Input pushes `Asap` (a click has no tick of its own; the sim stamps it), a replay pushes `At(tick)`. An order with no place in the tick stream is unrepresentable, exactly as M4c made an order with no attribution unrepresentable. | `a_command_scheduled_for_a_future_tick_applies_on_exactly_that_tick`, `an_unscheduled_command_is_applied_on_the_next_tick` |
| A command whose tick has passed? | **Dropped and counted**, never applied late. Applying one a tick late is the divergence the whole milestone exists to rule out, so a missed tick is a lost command, not a rescheduled one. | `a_command_whose_tick_has_passed_is_dropped_not_applied_late` |
| Log format | RON — already the project's content format, so a log is readable and diffable with no new dependency. **Writing is a validation boundary**: a non-finite coordinate has no round-tripping spelling, so `to_ron` refuses it rather than writing a log that would replay as something else (F-005's rule, applied to serialization). Reading refuses an unknown format version and a log whose ticks run backwards. | `every_coordinate_survives_the_file_exactly` (subnormals, both zeros, the extremes, compared by `to_bits`), `a_log_that_could_not_be_read_back_is_refused_when_written`, `an_unreadable_log_is_an_error_not_a_panic` |

**Measurement (why the state hash is opt-in).** `cargo bench --bench
replay_hash` on the box (release), the AI-vs-AI fixture:

| Bench | Median |
|---|---|
| 600 ticks, no `StateHashLog` | 70.3 ms (0.117 ms/tick) |
| 600 ticks, hashing every tick | 93.4 ms (0.156 ms/tick) |
| one `state_hash` of a 14-thing world | 14.8 µs |

(Re-measured after the M5 critic's second pass. The ratio moves with machine
load — runs on the box have come out between ~1.23× and ~1.33× — so the figure
to hold on to is the ~15 µs per hash and the *shape*, not the third digit.)

So hashing every tick costs **a quarter to a third of a tick** even on a
14-entity fixture —
it walks ~19 component queries and sorts the rows, and it builds each
`QueryState` per call. That is the right price for a replay check or a desync
probe and the wrong one for every shipped tick, so `record_state_hash` does
nothing unless a `StateHashLog` resource has been inserted. (Not an
optimization claim: the hash has no faster variant here, only a switch. If M6
needs it every tick, the query states want caching, and that will need its own
before/after.)

**One canonical hash.** The critics have been writing their own state hash
since M2 (`tests/critic_m4b.rs`, `tests/critic_m4c.rs` each carry one covering
position, HP and stockpiles). `sim::state_hash` is now the definition, in the
sim: it covers position, health, ownership, unit/building definitions, carried
and banked Alloy, both halves of the gather claim, move and combat targets,
attack cooldowns, resource nodes, production queues (item *positions*
included), casualties and match state — as rows sorted by `(SimId, field tag)`,
with floats hashed by exact bits, never bucketed. It deliberately excludes the
AI journal and the command log: those are records *about* a run, and a faithful
replay has neither. Pinned in both directions by
`the_state_hash_covers_every_piece_of_state_it_claims_to` (ten single-field
mutations, each of which must move it — including a one-bit change to a
coordinate) and `the_state_hash_ignores_what_is_not_sim_state`.

**The M4c carry-over, closed.** M4c left open that a `src/` emitter forgetting
`.issued_by(..)` self-signs rather than failing loudly (safe — F-009 — but not
something a shipped producer should do), and noted the check would have to be
about *values*, not about the spelling of call sites. The command log is that
record: `every_command_a_shipped_match_applies_is_signed` runs a 3000-tick
match, asserts **per tick** that the queue holds no unsigned order, and then
asserts that every command in the log was `Attribution::By(_)` — and finishes
by pushing an unsigned order by hand and requiring the log to record it as
`SelfSigned`, so the test can actually fail. `SelfSigned` stays constructible
(M1-M4 fixtures push bare orders and F-009's residual limit still stands); what
is now checked is that nothing shipped produces one.

**Extension (M5 critic) — four ways the milestone's guarantees were narrower
than their prose, and one thing left open on purpose.**

1. **A file-granular allowlist cannot express a per-system rule.** Admitting
   `sim/replay.rs` to F-008's reader allowlist admitted a file that also
   contained `replay::identify`, registered *before* `repair_gather_claims` —
   and the assertion written to justify the admission enumerated only the
   systems that already ran after the sweep. Two fixes: `identify` now runs
   after the sweep (it reads no claim, and the only ordering it needs is
   "before anything that addresses an entity by `SimId`"), and the ordering
   check is **derived from the chain** rather than hand-listed — it walks the
   systems registered in `src/lib.rs`, resolves each to its function body, and
   requires that none registered before the sweep so much as names
   `GatherTarget`/`GatherPhase`/`SplitClaim`. Demonstrated by putting
   `identify` back ahead of the sweep with a `&GatherPhase` query in it: the
   new check fails by name, the old enumeration passed. The rule generalises:
   *a guard whose assertion lists the cases that pass is not a guard.*

2. **A comment's position is part of its meaning.** The M5 prose was appended
   to the bottom of the F-008 comment block, which left "any new reader of the
   claim belongs after **this** system" sitting directly above `identify`
   rather than above the sweep — one system too early, and per (1) nothing
   would have caught someone who followed it. Each comment now sits above the
   system it describes. Fifth defect in this project to hide behind a doc
   comment; the pattern is now specific enough to name: *prose that says "this"
   is only true where it is.*

3. **A write-side check that admits what the read side refuses is not a
   check.** `to_ron` enforced finite coordinates; `from_ron` also refused
   backwards ticks and commands naming `SimId::UNIDENTIFIED` — a value the sim
   itself mints. So the sim could write a log that would then never load: the
   log destroyed exactly when it is wanted. `MatchLog::validate` is now one
   predicate both boundaries call. (F-005's rule, one boundary further out.)

4. **The state hash missed the state that has not fired yet.** `take_due`
   retains `At(t > now)` commands across ticks — sim-owned state, written by
   the sim, read by it later — and no row covered them; two worlds differing by
   one pending command hashed equal until it applied, which for an M6 desync
   check is "in sync, right up until divergence". Same shape, weaker, for
   `SimIds::issued()`: a drifted registry is invisible until the next spawn.
   Both are hashed now — pending commands keyed by *queue position*, since the
   order they will apply in is state too.

**Known-open, deliberately (for M6 to close, not to inherit silently):**

- **A log has no fingerprint of the content it was recorded against.**
  `MatchLog` carries a format version and a seed, but `Place { building }` and
  `Train { unit }` are *indices* into `Content`. Content is data and those
  indices will move; a log replayed against a reordered `units.ron` silently
  builds different things, with no error anywhere. Suggested closure: hash the
  loaded content (ids in RON order plus the stats the sim reads) into the log
  and refuse a replay whose content hash differs.
- **The log records a command by the tick it *applied* on, not the tick it was
  queued on.** So a producer that scheduled far ahead would make a recording
  hold a command that the replay of that recording does not — identical worlds,
  different hashes, until it fires. Nothing does today, and that is enforced,
  not assumed: `nothing_in_src_schedules_a_command_ahead_of_the_tick_it_applies_on`
  requires `push_at` to have exactly one caller in `src/` (the replay, pushing
  for the current tick). M6 introduces ahead-scheduling by construction, and
  must record the queued tick alongside the applied one — and decide what to do
  with commands queued but never applied, which are in no log at all.

**Extension 2 (M5 critic, pass 2) — four defects, all of them in pass 1's fix
code.** Third milestone running where the fixes out-defected the original
implementation, so it is recorded here as a standing property of this project
rather than as a run of bad luck: *a fix is new code written under time
pressure against a spec written by the person who just got it wrong.* It needs
the same two tests everything else does — one for the property it establishes,
one for the property it might break — and the second is the one that keeps
catching things.

1. **A guard that skips in silence is worse than the narrow one it replaced.**
   The generated F-008 walker mapped `sim::<name>` ⇒ `src/sim/mod.rs` and
   `continue`d past anything it could not find. `sim::match_running` is
   *re-exported* there and defined in `victory.rs`, so it was skipped without a
   word — while the comment claimed the check covered every pre-sweep system
   "in any file". The list it replaced at least panicked on a name it did not
   know. Resolution is now by definition (search every `.rs` under `src/sim/`
   for `pub fn <name>(`, require exactly one, require a body that reaches its
   closing brace), every failure is a named test failure, and run conditions are
   checked as pre-sweep code because a condition is evaluated before what it
   gates. **The rule: a checker's failure mode must be "fail", never
   "continue" — and what a checker cannot resolve is the exact thing that will
   be moved into the dangerous position later.**

2. **The justification for moving a shipped system was false** ("the sweep is
   the chain's first system" — `victory::match_watch` precedes it), written in
   the same commit that added the F-012 rule about prose that says *this*.
   Sixth defect of the class.

3. **Unifying two boundaries without fixing the producer moved the failure
   earlier and made it worse.** `validate` refuses `SimId::UNIDENTIFIED`;
   `apply_commands` still *minted* it for any entity it could not resolve —
   including one that merely died before its command applied — so a latent
   *read* failure became a live *write* failure: the sim could record a log it
   could never save, exactly in the case (a replay whose world has moved) where
   the log is the report. The fix is at the producer: `SimIds` is a two-way
   registry (`Vec` forward, ordered `BTreeMap` backward), so a despawned entity
   is still nameable, and `id_for` issues an id for anything that lacks one.
   **A validator and its producer are one design; tightening either alone just
   relocates the defect.**

4. **A desync check must not invent a divergence.** The pending-command rows
   hashed *every* queued command, including `Asap` ones. In a running sim an
   `Asap` command is drained before any hash is taken — but once the match is
   decided the chain is gated off, so a click on a finished match sits in the
   queue forever with no causal reach, and the "frozen sim" hash moved on every
   later tick. Two M6 peers, one of whose players clicked, would report a desync
   that does not exist. The rows now cover only commands held for a *later*
   tick, which is what the doc always claimed. The driver half is fixed too:
   `input::emit_commands`/`emit_build_commands` are gated on `match_running`, so
   a finished match stops filling a queue nothing will drain.

**Extension 3 (M5 critic, pass 3) — two boundary fixes, each correct, jointly
wrong.** Pass 2's F3 forbade *recording* `SimId::UNIDENTIFIED` (a log the sim's
own validator refuses). The fix routed the log through a `SimIds::id_for` that
issued an id on demand — and `issued()` is a hashed row, added by pass 1's F4.
So **writing the log mutated hashed state**, and the replay path never performs
that mutation: `feed_replay` resolves read-only. A recording whose order named
something outside the world (a bare entity with no `Position`, which `identify`
never sees) issued `SimId(4)` while writing the log; the replay of that same log
could not resolve id 4, dropped the command, issued nothing — and the next
building placed took id 4 in the replay and id 5 in the recording. The log
validated, saved and loaded perfectly. Every hash from that tick on diverged,
and every later command named a different thing.

Three things worth keeping from it:

- **It was worse than the defect it fixed.** Pass 2's F3 was a log that would
  not *save* — loud, at the write, where something could still be done. The fix
  removed the loud failure and left a silent wrong replay. When a fix converts a
  loud failure into a quiet one, that is a regression even if the original
  symptom is gone.
- **It reopened F-011's own named failure mode through a different door.**
  F-011 said a log keyed on a coordinate that moves "commands the wrong units,
  silently". The coordinate no longer moved with the ECS allocator; it moved
  with *whether an order named something the registry had not seen*.
- **Seventh doc-comment defect**: `id_for` asserted "a bare fixture entity …
  gets a real id here rather than a sentinel: it is a thing an order named, so
  it is a thing the log can name." The log could name it. The replay could not
  resolve it.

**Decision.** An order can only command what the sim can **name**. A name is a
`SimId`, and ids are issued in exactly one place — `replay::identify`, which the
record path and the replay path run identically. `apply_commands` therefore
takes the registry as `Res`, not `ResMut`, and:

- drops an unnameable entity from a list order (`MoveTo`, `Gather`), leaving the
  rest of the order to stand, exactly as a signed order naming another faction's
  units still commands the issuer's own (F-009);
- refuses whole an order whose single subject is unnameable (`Gather`'s node,
  `Train`'s building);
- decides all of this from the **registry alone**, never from whether a log is
  present, so logging can never change what the sim does;
- counts what it refused in `CommandLog::unnameable()` — zero for anything a
  shipped producer emits.

What is logged is then exactly what was applied, in both directions.

**The invariant this milestone hands to M6** — more general than the repro, and
the thing to hold new code to: **the record path and the replay path must mutate
sim state identically, or not at all.** A log carries commands; it does not carry
the side effects of *writing* it. Anything the recording does that the replay
cannot repeat is a desync with a delayed fuse, and the hash will report it as a
divergence in something innocent-looking several hundred ticks later.

**Evidence.** Red first, by restoring the lazy-issuance version: three probes
fail (`an_order_naming_something_the_world_never_held_is_dropped_from_it`,
`the_record_and_replay_paths_grow_the_registry_identically`,
`only_identify_can_issue_a_sim_id`). Green after, together with the structural
guard that keeps it closed — `apply_commands` must take `Option<Res<SimIds>>`,
`assign` must have exactly one call site and that site must lie inside
`identify`, and no `id_for` may exist. 361 tests, debug and release.

## F-013 — A log is only meaningful against its content, and only complete if it records what failed (Phase 1)

**Wall hit.** M5 shipped with three known-open items in its log format, all of
the same family: things the format could not express, each of which M6 would
have leaned on silently.

1. `MatchLog` carried a format version and a seed but **no fingerprint of the
   content**. `Place { building: usize }` / `Train { unit: usize }` were indices
   into `Content`, and "content is data" guarantees those indices move: a log
   replayed against a reordered `units.ron` builds different things, with no
   error anywhere.
2. A command was recorded by the tick it **applied** on, so a schedule was
   unrepresentable and a command that never applied appeared in no log at all.

**Decision.** One format bump (`LOG_FORMAT_VERSION: 2`), three changes, and no
migration shim — a version-1 log is refused by name. A shim would have to invent
the one thing the old format is missing (which content it was recorded against),
and inventing it is precisely the silent wrong replay the fingerprint exists to
prevent. Old logs are re-recordable; a guess is not.

| Item | Choice | Pinned by |
|---|---|---|
| Content fingerprint | `Content::fingerprint()` over the **whole** deserialized content — floats by `to_bits`, strings length-prefixed, every list in RON order, no map. Stamped by the sim (`replay::stamp_content`), not by the caller. | `the_fingerprint_covers_the_whole_content_exactly`, `every_log_the_sim_records_matches_the_content_it_played` |
| Where it is checked | `load_for`/`from_ron_for` at the front door; plain `load`/`from_ron` stay format-only so a refused log is still *readable*; `feed_replay` refuses a mismatched `ReplaySource` outright as the backstop. | `a_log_recorded_against_other_content_is_refused`, `the_sim_refuses_to_replay_a_log_from_other_content` |
| Content named by | **Stable ids**, resolved through `Content` at replay time. Refusal on an unknown id, never a substituted index; `matches_content` proves every id resolves before a replay starts. | `a_log_still_names_the_same_things_after_the_roster_is_reordered`, `a_log_naming_content_this_build_does_not_have_is_refused` |
| Schedule | Each entry records the `CommandTick` it was pushed with, and a replay re-pushes on **that**, not on the tick it applied. | `a_replay_re_pushes_on_the_recorded_schedule_and_re_records_the_same_log` |
| Commands that failed | A command that missed its tick is logged with `CommandFate::Late` — the log is an account of the match, not of what worked. | `a_command_that_missed_its_tick_is_logged_with_its_schedule_and_reason` |

**The fingerprint's scope, argued rather than assumed.** It covers fields the
MVP sim never reads (the post-MVP `cost` block, `name`s), so an edit that could
not have changed behaviour still invalidates a stored log. That is the intended
trade: a false rejection is loud, immediate and recoverable — the log still
loads for inspection and says what changed — while a false accept is a silently
different match. A narrower scope would also have to be *revised* every time a
field starts being read, which is a rule that is true when written and false a
milestone later (this ledger has four entries about exactly that).

**Producer and validator, again.** Every new validator here got its producer
fixed in the same change, because M5 paid for the alternative twice: the sim
stamps its own fingerprint (so no log it writes is unstamped), and
`apply_commands` refuses an order naming content this build lacks (so no log it
writes can name an unresolvable id). And the M5 invariant still holds under the
new resolution step — record and replay resolve through the same `Content` and
do the same thing with a failure.

**Evidence.** 387 tests green in debug and release; `cargo test --test m5_replay`
56/56. Each item was run red first (the fingerprint check removed; indices in
place of ids; late commands unlogged).

**Deliberately still open.** Ahead-scheduling itself is M6's to introduce: the
format can now express `(schedule, applied tick)` but nothing in `src/` pushes
a command for a future tick, and `nothing_in_src_schedules_a_command_ahead_of_
the_tick_it_applies_on` keeps it that way. When M6 adds it, two things follow —
the *queued* tick becomes worth recording (so the pending-queue rows of the
state hash agree between a recording and its replay), and a command queued but
never applied still appears in no log.

**Extension (Phase 1 critic) — two more coordinates that were not what their
prose said.**

1. **1b replaced a positional coordinate with a string one and never proved the
   string was injective.** `unit_index`/`building_index` return the *first*
   match and `Content::validate` had no duplicate-id rule, so duplicated content
   loaded happily: a recorded `Place` naming the second `foundry` (999 Alloy)
   replayed as the first (150). The log validated, its fingerprint matched
   exactly, every id resolved — and the replay was a different match. The sim
   never notices, because it runs on indices; only a replay does.

   Duplicate ids are now **impossible to load** (per namespace: units,
   buildings, resources; an id shared *across* namespaces stays legal, because
   every reference in the data and in the log says which kind it means).

   This is the **third** log coordinate this project shipped without proving it
   injective — `Entity::to_bits` (F-011), the lazily-issued `SimId` (F-012 ext.
   3), and now the content id. The standing question, to be answered *before*
   anything is keyed on a coordinate rather than after a critic asks: **is this
   injective, and what enforces that?** Note where the answer keeps landing: in
   the loader, as a refusal, not in the reader as a check.

2. **The stamp described the first content the sim ever saw, not the content the
   commands were taken under.** `stamp_content` stamped once, justified by
   "content cannot change mid-match, and if it somehow did, the first stamp is
   the one the recorded commands were taken under" — false in its second clause,
   and written in the diff whose whole subject is "the log describes the content
   it was played with". Eighth doc-comment defect of this shape.

   The stamp now follows the content actually in use, and a change marks the log
   as describing **no single content** (`MatchLog::content_changed`), which
   `validate` and `matches_content` both refuse — loud at the write, which is
   the escape clause the producer/validator rule allows for something no shipped
   configuration does.

**And a probe-writing lesson the critic handed back, worth more than the fixes:
an assertion that something did not happen is vacuous unless you also assert the
machinery ran.** Its version of the late-command probe pins `rejection() ==
None` and `cursor() == 2` alongside "the replay did not apply it", because with
the 1a backstop in place a refused replay feeds nothing and passes the negative
assertion trivially. Every "X did not happen" assertion in this project should
be read with that question attached.

## F-014 — A feature nothing ships is a feature that does not exist (Phase 2)

**Wall hit.** M5 spent four critic passes making a replay log correct — tick
tags, persistence, a canonical state hash, `SimId` addressing, a content
fingerprint, stable ids, schedules and fates — and **no run of the game ever
wrote one**. Every path to disk was a test. That is the same shape as F-004
(`economy::production` registered only in a test harness), one layer out: there,
a system the game did not run; here, a whole feature the game could not reach.

**Decision.** The driver writes the log, and does it through one registration,
`add_replay_writer`, that the shipped `build_app` installs and the tests use —
the same rule as `add_sim_systems`, for the same reason.

| Question | Choice | Why |
|---|---|---|
| Where the config lives | `assets/data/replay.ron`, driver-side, **not** in `Content` | A fingerprint is a claim about the *match*; a directory name is not part of one. In `Content` it would be hashed (the standing every-field guard), so turning logging on would invalidate every replay already on disk. It also keeps a path out of sim state — one step from a filename reaching a hash. |
| Default | **Off** | A log per run is unbounded growth on disk. A missing `replay.ron` is off; a malformed one is an error, because a missing optional config is a state and a broken one is a mistake. |
| When | On **match decided**, and on exit | A crash or force-quit after a finished match must not lose the finished match — which is the one worth keeping. |
| How often | **Exactly once per app**, by recording one outcome and never revisiting it | Makes "a decided match that keeps ticking does not rewrite" and "an exit after a decision does not write twice" the same statement. The probe asserts the writer was *asked* 300+ times, so the latch is demonstrably what holds. |
| On failure | Reported on the writer and at `error!`; never fatal, never retried | A lost replay is not worth a lost match. Not retrying is what keeps a disk error from becoming one line of log per frame. |
| Clock | Wall clock **in the driver only** | The sim may not read one (F-003). Pinned three ways: the same match hashes identically with no writer, a wall-clock writer and a fixed-clock writer; and no `SystemTime`/`ReplayWriter`/path is named anywhere under `src/sim/`. |

**Filenames are a coordinate, and this one is not injective.** Two matches can
finish in the same second with the same seed. The fourth time this project has
had to answer "is this coordinate unique, and what enforces that?" — and the
first time it was answered *before* shipping the thing keyed on it. The answer
here is different from the previous three, and worth recording as a second
pattern: where a coordinate **can** be made injective (`SimId`, unique content
ids) the fix is a refusal at admission; where it **cannot** (a filename, which
the outside world owns), do not trust it — `create_new` makes the *claim*
atomic, the writer walks a suffix until it claims an unused name, and running
out is reported rather than resolved by overwriting somebody else's log.

**Evidence.** `tests/p2_log_writer.rs`, 13 probes; the four load-bearing ones
run red first by stubbing out the latch, the no-clobber claim and the
validating front door individually. The headline probe is end-to-end: a
shipped-shape app plays an AI-vs-AI match to its decision, and the file it
leaves is read back through `MatchLog::load_for` and replayed to the same
verdict on the same tick with an identical per-tick hash trace. 425 tests green
in debug and release.

**Deliberately deferred.** Nothing *reads* a log back in the shipped binary —
there is no "play this replay" entry point, because there is no UI or CLI for
one and inventing either would be building ahead. The sim side has been able to
do it since M5 (`ReplaySource`), and the headless tests do it; wiring it to a
user gesture belongs with whatever menu M6 or the campaign layer brings.

**Extension (Phase 2 critic) — a diagnostic emitted before anything can hear it.**

`build_app` reported a malformed `replay.ron` with `error!` five lines before
`DefaultPlugins` installed the `tracing` subscriber. `tracing` does not buffer
pre-subscriber events — it evaluates the message and drops it — so a typo in the
config was observably identical to `enabled: false`: the feature looked broken
instead of saying it was misconfigured, which is precisely the state three of
this project's own comments said must not exist.

Ninth defect here behind prose asserting a property the code lacked, and the
**fourth where the false part was the justification rather than the claim**.
"Disables it and says so" was half true, and the failing half was the half the
design rested on. The rule that follows, and it is a rule about *probes* rather
than about code: **when the prose says a condition is reported, the test that
must exist is one that observes the report — not one that observes the
condition.** A capturing subscriber makes that a two-line assertion; nothing
weaker distinguishes "the code calls `error!`" from "somebody is told".

*And its corollary, which cost a second pass.* The **positional** guard shipped
alongside that probe — "no diagnostic macro appears in `build_app` above
`add_plugins`" — went vacuous the instant the fix moved the reporting into a
helper: it then scanned a body with no macros in it and passed on the empty set,
reading as protection while guarding nothing. Two rules follow, and they are
about how a guard is *written* and how it is *verified*:

- **A structural guard must assert that it resolved something.** This one now
  resolves `build_app`'s callees, asks whether any of them can report
  (transitively, through this crate), asserts that its classifier recognises the
  one reporter on the startup path, and asserts that it found the call that
  matters. Any of those failing is a loud failure, not a silent pass.
- **Red-verify against the mutation the defect would actually arrive as.** The
  original guard was "verified" by pasting an `error!` back into `build_app` —
  something nobody would do. The defect arrived, both times, by *moving the
  call*; that is the mutation the guard is now checked against, and the one it
  fails on by name.

Two smaller things fixed alongside, both about not leaving things behind:

- **A failed write removes its own partial file.** A fragment cannot be mistaken
  for a log (`load` refuses truncated RON), but it holds a *name*, and the
  collision walk steps over taken names forever — so repeated failures would eat
  a bounded budget and eventually deny a working write. This does not weaken
  "never overwrite somebody else's log": the fragment is neither somebody
  else's nor a log, and the cleanup only ever touches the path claimed with
  `create_new` in the same call.
- **Scratch cleanup on `Drop`.** Cleaning up on the success path only means
  littering exactly when a run went wrong — which is when nobody looks. Guards
  remove the file, and the last one out removes the directory.

  *Correction (Phase 2 critic pass 2).* This bullet originally said "everywhere",
  which was false when written: only `tests/p2_log_writer.rs` and
  `tests/m5_replay.rs` — the two files this implementer owns — had been
  converted, and the critic's own suite was still leaving hundreds of paths in
  `/tmp`. It is true as of the critic's pass-2 fix to its files. The correction
  is recorded rather than silently edited because it is the same defect this
  entry exists to describe — a claim quantified over more than was checked — and
  it appeared *in the entry recording that lesson*. State what was verified, and
  when.

## F-015 — Lockstep: the queue's *order* is state, and a stall is where local input escapes (M6)

**Wall hit.** Two peers apply the same commands on the same ticks. Everything
M0–M5 built exists to make that checkable — `SimId` because an `Entity` cannot
cross a process boundary (F-011), one canonical `state_hash` because two notions
of "identical" is none, the content fingerprint because two rosters cannot
produce one match. Two defects turned up anyway, and both were found by tests
that no single-process harness could have run.

**1. The order of a turn's commands is part of the state.** The first cut pushed
each peer's commands into the sim's queue as they were produced or as they
arrived. Both peers then had the same commands for tick T in a *different order*
— and the state hash counts the queue by position (F-012 ext.), so they hashed
differently and filed a desync against each other for nothing. The link now
buffers both sides' turns and pushes them as one canonical sequence (faction A's,
then faction B's, each in its own issue order), identical on both peers by
construction rather than by timing. **A lockstep peer must decide the order of a
turn, not inherit it from the network.**

**2. A stall is where local input escapes.** Local commands were taken out of the
sim's queue only on the frame that *sent* a turn. The frame that resumes a
stalled sim sends no turn — the tick has not advanced, so there is no new turn
number — and the command sat in the queue as `Asap` until the resumed tick
applied it **locally, on one peer only**. Two processes desynced at a different
tick every run. Local commands are now drained every pump into a pending buffer;
nothing local can reach the sim unscheduled.

**What found it.** Not the twelve in-process probes — they were green. Two
`App`s in one process share an allocator, a parsed `Content` and every static;
two *processes* share a socket and some files. The cross-process test failed
intermittently, and the peers' own command logs (dumped to disk, diffed)
identified the escaping command in one line. **The critic probe said
"determinism holds cross-process" for a reason: an in-process lockstep test is
necessary and not sufficient.**

**Decisions worth keeping.**

| Question | Choice |
|---|---|
| What the sim knows | One resource, `TickGate`, and a run condition. No socket, no peer, no clock, no `async` — pinned by `the_sim_knows_nothing_about_the_network`. |
| What runs on a stalled tick | **Nothing.** The gate holds the whole chain, wider than `match_running`: on a stalled tick the tick counter must not advance, the outcome must not be decided and no hash may be recorded, because a hash for a tick that did not happen is a desync report against an innocent peer. |
| Transport | `std::net` TCP, length-prefixed RON, non-blocking. No new dependency (the project has three), and no async runtime in a codebase whose thesis is a dependency-light deterministic core. Non-blocking because a lockstep implementation that blocks the renderer is one nobody can watch. |
| Peer identity | `Faction` — a two-valued enum, so the id space is injective by construction, and the handshake refuses a peer claiming the same side. The fifth time this project has asked "is this coordinate injective?", and the first where the answer was "the type already guarantees it". |
| Turn identity | The tick a turn applies on: monotonic, one per peer per tick, and the peer that sends two for one tick is refused by the same `is_none_or(|last| turn > last)` latch that stops a stalled frame sending twice. |

**Deliberately deferred.** No matchmaking, no reconnection, no lobby, no UI for
any of it; the shipped binary still starts a local match. `netpeer` is the only
thing that plays a networked one, which is enough to prove the property and
nothing more. A peer that drops is reported and the match stops — there is no
resume, because there is nowhere to resume *to* without a lobby.

**Extension (M6 critic) — the milestone's headline test could not tell a seeded
match from a constant.**

`the_same_match_played_twice_across_processes_is_the_same_match` asserted that
two runs of one seed agree, and its doc explained why that was not vacuous:
*"a different seed is a different one, so the first assertion cannot be passing
on a constant."* It never played a different seed, and **could not have**:
`netpeer` installed `AiCommanders::default()` — no commanders — so the only
seeded thing in the sim never ran, and the seed reached nothing `state_hash`
observes. Five seeds, one hash. The test would have passed with the seed wired
to a literal.

Tenth defect here behind prose asserting a property the code lacks, fifth where
the false part is the *justification*, and the third vacuity catch — this one on
the milestone's headline claim. The rule that keeps failing to be applied is not
subtle, so state it as a procedure rather than a principle: **for every "X and Y
agree" test, write the "X and Z differ" test in the same commit, and make the
second one fail before you believe the first.**

**Fix, in the fixture.** `netpeer` is now seeded twice over, on purpose:

- its **starting layout** comes from `sim::random_layout(3, seed ^ slot, ..)`,
  the sim's own generator, so a *short* run already distinguishes two seeds;
- each peer runs **its own side's `AiCommanders`**, seeded from the match seed
  and the faction slot, so a longer run also exercises seeded *decisions* (the
  commander's first random choice is where to put its barracks, at
  `mvp_ai.barracks_at_tick`).

Both peers compute both sides' layouts from the same numbers, so the layout is
match setup rather than local randomness. The control runs at **two horizons**
(120 and 600 ticks) because each seeded element could rot on its own: a long-only
control would not notice the layout going constant, a short-only one would not
notice the commander going deaf.

**What using the real AI required, and why it belongs in `src/`.** The scripted
commander runs *inside* the sim chain — after the link's frame-time drain and
before `apply_commands` — so its orders were applied locally, on the peer that
thought of them, and never crossed the wire. The link now drains the queue
twice: at the top of the frame, and again between the commanders and the
application (`net::collect_local`). Nothing local reaches `apply_commands`
unscheduled. That is the same defect as M6's stall escape, one producer further
in, and it is why the fixture change could not be fixture-only.

**A gate hole, recorded because it caused the second half of this round.** The
clippy check that reported "0 errors" was `cargo clippy --lib --tests --benches`
**without** `-D warnings`, so a lint that fails the real gate counted as a
warning and was invisible. The gate is `cargo clippy --all-targets -- -D
warnings`, with exactly one known exception (`tests/critic_m3.rs:234`, a
critic-owned file this implementer may not edit). *A gate run with different
flags than the gate is not the gate.*

---

## F-016 — A build order is an *order*: promoting one script into a set of strategies (B1)

**Why this is a finding and not a rename.** `mvp_ai` was a single block in
`units.ron` with `barracks: String` — one building, so three of the five units
were unreachable by any AI and the pentagon could never be measured. B1 makes it
`strategies.ron`: a *set* of named entries, each opening a **list** of barracks.
Three decisions came out of doing that, and each is a place the next reader could
reasonably have chosen otherwise.

**1. The default is a name the loader resolves, not a position.** `Content.ai`
stays (a large closed-milestone surface reads it), but it is now the *resolved*
entry named by `strategies.ron`'s `default:` field, cloned once at load. The
alternative — "the first entry is the default" — makes reordering a data file a
behaviour change, and reordering a list is exactly what authoring the probe set
(AC3) will do. Content whose `default` names no strategy is refused.

**2. Validation is over the whole set, not the entry in use.** Every strategy is
checked — unknown ids, a barracks that is not a building or is the victory
target, the same building opened twice, an army entry naming a unit **none of
that strategy's own barracks can produce**. An unreachable strategy is only
unreachable until the day a match names it, and a set the loader half-checks is a
set whose errors surface as a commander that silently never builds. Every message
names the offending strategy by id, because with ten entries "army names unknown
unit `x`" is not a diagnosis.

**3. The cursor waits for its own barracks; it does not skip ahead.** With
several barracks, the next unit of the build order is trained at whichever opened
barracks produces it — and if that one is busy or not yet up, the commander
*waits* rather than stepping past it to something it can afford. Skipping would
make the build order a wish list whose realised composition depends on queue
timing, and the balance sim measures compositions: "mass Ripper" has to actually
mass Rippers. The cost is that a stalled barracks stalls the whole order, which
is a property the B2 batch runner can see (units produced per side) rather than a
silent one.

**The refactor is proved behaviour-preserving, not asserted to be.** A one-entry
barracks list must replay bit-identically to the pre-B1 build — the RNG stream
especially, which draws one angle per placement and only when the placement
actually happens. The gate is a golden state hash taken from the pre-B1 binary at
tick 3_000 of the standard AI-vs-AI fixture for two seeds
(`the_one_barracks_default_replays_exactly_as_it_did_before_b1`). A refactor of
the sim that cannot point at a pre-refactor number is a re-tune nobody noticed.

## F-017 — A commander carries a strategy *index*, and an unknown name is refused (B1 AC2)

`AiCommander` now holds `Option<usize>` — an index into `Content::strategies`
(stable RON order), `None` meaning "this content's default". A name is resolved
once, at construction; the decision path never compares strings, and the type
stays `Copy`-cheap.

**Refusal, not fallback, in both directions.** `AiCommander::with_strategy` /
`AiCommanders::matchup` return `Err(UnknownStrategy { id, faction })` when a name
is unknown: B2/B3 key every recorded row by strategy pair, so a typo that
silently played the default would mislabel its own data — the one failure mode a
balance report cannot survive. For the same reason, resolving an index that is
out of range for the running content *panics* rather than falling back to the
default: an index can only be foreign if the commanders were built against
different content than the match runs, and quietly playing the default there
produces a result filed under a strategy nobody ran.

**The seed derivation deliberately did not change.** A commander's stream is
still a function of (match seed, faction slot) *only* — never of its strategy —
so one seed means the same map opening across every matchup and a difference
between two matchups is attributable to the scripts. The closed M5/M6 seed-control
probes depend on this too, and `a_matchup_seeds_exactly_as_the_default_constructor_does`
pins it.

**`think_interval_ticks` became per-commander.** It was read from `content.ai` in
`ai_commanders` before `think` was called; left there, two strategies with
different APMs would both have thought on the default's cadence, and every APM in
the probe set would have been a decorative number.
`each_side_thinks_on_its_own_cadence` is the test that would have caught it.

**Behaviour-preserving, proved against the pre-AC2 binary.** The default matchup
is pinned by golden `state_hash` *and* a golden `AiJournal` digest at tick 3_000
for three seeds, and naming `mvp` on both sides is asserted to play exactly the
match that defaulting to it plays
(`the_default_matchup_is_byte_for_byte_what_it_was_before_ac2`).

## F-018 — The five mass probes must be knob-identical, or the pentagon measures the knobs (B1 AC3)

**What the probe set is for.** `strategies.ron` now carries nine entries beyond
the default: five `mass_*` probes (one per combat unit), two `synth_*` builds
that cross domains, an all-in `rush` and a `turtle`. They are not personalities
for a player to meet — they are the *instrument* B3 reads the counter pentagon
off. That changes what "good" means for them.

**The comparability rule.** The five `mass_*` entries differ **only** in the
barracks they open and the unit they mass. Same `think_interval_ticks`, same
`worker_target`, same opening `at_tick` and `offset`, same `attack_at_army`,
`attack_interval_ticks` and `attack_spread`. If any of those diverged, then
`W[mass_sentinel][mass_ripper] > 0.5` would no longer be a statement about
Sentinel versus Ripper — it would be a statement about whichever knob differed,
and B3's pentagon assertion (the one test this whole plan exists to run) would
be reading its own tuning back to itself. `the_mass_probes_are_knob_identical`
asserts it field by field, so the day someone "fixes" one probe the instrument
says so instead of quietly re-scaling.

The one asymmetry left standing is a real one from the design brief, not a knob:
an Aether Spire costs 200 Alloy where a Foundry or Gene-Vats costs 150, so
`mass_arclight` opens later in practice. That is an economy fact about Energy
tech and belongs in the measurement.

**A literal worker rush is unbuildable, so the all-in is an early rush.** The
Worker has `offense: 0`, `mvp_attack_ticks: 0`, `mvp_attack_range: 0.0`: it
cannot damage a unit or an HQ, and `ai::think` only ever sends `offense > 0`
units at the enemy. Giving the Worker an offense value to make the probe
literal would be a *design* change smuggled in as balance tooling. `rush` is
therefore the playable form of the same idea — `worker_target: 1`, the opening
at tick 0, the cheapest unit in the game (Ripper, 40), `attack_at_army: 1`,
120-tick waves. It commits at tick ~750 where `turtle` commits at ~6000.

**Loading is not playing.** A strategy that parses but stalls — cannot afford
its opening, or waits forever on a barracks it never places — is worthless to
B3 and would show up there as a mysterious row of timeouts. So every entry in
the shipped set, *iterated from the content rather than listed by hand*, plays a
headless solo match and must place all of its barracks, train real units, and
produce exactly the prefix of its own repeating build order
(`every_strategy_places_its_barracks_and_builds_its_own_order`). The same
iteration is what makes a sixth combat unit added to `units.ron` fail the
coverage test instead of slipping through unprobed.

**Measurement, for B2/B3.** In the solo fixture (one commander, an inert
opponent, seed 4) the whole set is comfortably solvent — nothing is marginal.
First attack ticks: rush 750, mass_ripper 2580, mass_sentinel 3120,
synth_steel_flesh 3240, synth_triad 3390, mass_arclight 3660, mvp 3720,
mass_ravager 4020, mass_bulwark 4920, turtle 6000. Alloy is *piling up* in every
run (570–1620 banked at the end), so production is limited by `mvp_train_ticks`
at a single barracks, not by the economy: the AI trains one unit at a time and
waits for the queue. B4 should expect army sizes in the single digits over a
28_800-tick cap, and that is a tempo question for the *content*, not a bug in
the probes.

---

## F-019 — The headless match is driver code, and "lifted unchanged" has to be provable (B2 AC1)

**Where it lives: `src/headless.rs`, not `src/sim/`.** The constructor builds a
Bevy `App`, adds `MinimalPlugins` and installs the shipped sim chain. That is
app assembly, and the sim's standing rule is that it is ECS + math + time with
no plugin or `App` construction in it — a sim module that knows how to build an
app is a sim module a renderer can reach through. It stays render-free and runs
headless on the box, so it costs the balance runner nothing to have it one layer
out.

**Settings struct, not positional flags.** The bench's fixture was
`ai_vs_ai(seed, hashing)`; B2 alone adds a strategy pair and a spawn
orientation, and B3 will want a tick cap. Four positional arguments of which two
are `bool` is a call site nobody can read. `MatchSettings` (`Clone + Debug +
Default`, builder setters) means each later checkbox adds a *field*, and every
existing caller keeps compiling with unchanged behaviour. `Default` is defined
to be the M5 bench fixture: seed 0, both sides on the content's default
strategy, no hashing.

**Naming a strategy stays fallible through the lift.** `ai_vs_ai` returns
`Result<App, UnknownStrategy>` and resolves both names *before* anything is
spawned — no `unwrap` buried in the constructor, no half-built match left behind
by a refusal. F-017's rule survives the move: every number B2/B3 print is keyed
by strategy name, so a typo must stop the caller rather than mislabel a row.

**"I lifted it unchanged" is demonstrated, not asserted.** Before touching the
bench, its own `ai_vs_ai(4, true)` was run for 600 ticks and its per-tick
`state_hash` captured — six pinned ticks plus an FNV fold of all 600, so no tick
in between can drift unseen. Those constants are the gate on the default
settings (`default_settings_reproduce_the_bench_fixture_tick_for_tick`). A
fixture-shape test (positions, node depth, worker count, starting Alloy,
commanders) reads the same thing in human terms, but the hashes are what make
the equivalence a fact rather than a claim about what was intended.

**Both bases are described in one function.** `base_of(faction)` is the only
place spawn geometry exists, because B2's side-balanced sampling is exactly
"swap these two" — that checkbox should change one function and nothing else.

## F-020 — A cap is not a draw: the batch runner's three-valued result (B2 AC2)

BALANCE_PLAN's checkbox says "cap → draw/timeout", and the obvious reading is
that they are the same thing: nobody won, call it a draw. The runner refuses
that reading and carries three values — `Decided(Faction)`, `MutualLoss`,
`Timeout`.

`MatchOutcome::winner == None` is a fact the *sim* established: both HQs fell on
the same tick, the match ended, and it ended even. A capped match established
nothing. The two sides were still playing; an observer stopped watching. Folding
them together makes a batch of stalemates arrive at B3 looking like a batch of
fair games, and B3's own probes require the opposite — it must report "% hitting
the cap" and **flag an all-timeout run rather than reporting it as balanced**. A
win rate computed over matches that never finished is a number about the cap,
not about the roster. So the distinction is load-bearing at the type level:
`MatchResult::winner()` returns `None` for both, and `is_decided()` is the thing
callers must ask.

**The cap is harness configuration, not content.** It lives on `MatchSettings`
(default `8 min * SIM_HZ`, overridable from the CLI), never in RON: `units.ron`
describes the game, and how long an operator is willing to watch is not part of
the game. It is written as a duration times the sim's rate so the constant
explains itself, and `ai_vs_ai` never reads it — the sim does not know it is
being timed.

**The batch order is an outcome.** The matchup list is a walk over
`Content::strategies` in RON order (never a map, never a name list written down
a second time — a strategy added to `strategies.ron` is played with no code
change), and the batch runs seed-major, then row-major, sequentially. That
vector *is* the report's row order, and B2's critic probe is that re-running the
whole batch is identical; unordered parallel collection is the cheapest way to
lose that, so the loop is not parallel. It does not need to be: 200 matches
(10 strategies, both orders, 2 seeds) run in **2m09s** in release on the box.

**First look at the instrument.** That run decided every one of its 200 matches
— **zero timeouts** — with a median length of **1:16** and a maximum of 4:45.
The 8-minute cap is nowhere near binding; the problem is the other end. The
5-8 min arc DESIGN_BRIEF targets is not what the sim plays: matches end in
around a fifth of it. That, and the mirror asymmetry visible in the same run
(A 94 / B 106 overall, and `mvp` vs itself won by B on the sampled seed), are
readings for the side-balance checkbox and for B3/B4 — recorded here, not acted
on.

## F-021 — Orientation flips the geography; ordered pairs flip the slot (B2 AC3)

The mirror lean the first batch showed (B won 34 of 59 decided mirrors, 57.6%)
has two possible causes that look identical in the record: the **faction slot**
(who thinks first, whose RNG stream is whose) and the **spawn position** (who
starts at the left-hand base). The instrument could not tell them apart, because
only one of the two ever moved — every ordered pair was played, so the slot
varied, but slot A had always spawned at `(-750, 0)`.

So the two axes are separated rather than merged. `Orientation::{Normal,
Swapped}` changes *only* which base each faction spawns at; the slot order, the
strategy-to-slot assignment and the seed are untouched. Everything else in the
fixture (the deposit, the starting workers) is placed relative to its base, so
`Swapped` is exactly the map reflected in x — the same two scripts on reflected
ground, and a test asserts precisely that reflection. The batch plays every
`(a, b, seed)` in both orientations **on the same seed**: the seed is the
control, the geometry is the variable. Row order gains a third, innermost key —
seed-major, then RON row-major, then orientation — and the row count is
`N·N·K·2`.

**A record that cannot name its geography cannot be audited.** `MatchRecord`
carries the orientation, and `Tally` keeps `spawn_wins` (which *base* won) and
`by_orientation` next to the slot counts, so a report prints the raw asymmetry
beside the corrected number instead of averaging it into invisibility. That is
the difference between cancelling a bias and hiding it: a positional edge that
survives reflection is a finding B3 needs, not noise.

**Why not seed-randomize the spawn instead.** Randomizing would also balance in
expectation, but it costs the pairing: with both orientations of the same seed
played, each `(matchup, seed)` is its own controlled experiment and the
positional effect is recoverable exactly, from two rows that differ in one
variable. Randomization converts a measurable quantity into sampling noise.

**Measured — the lean was noise, but the instrument now shows both axes.**
240 mirror matches in release on the box (10 mirrors x 12 seeds x 2
orientations, 238 decided, zero timeouts):

| reading | count | rate |
| --- | --- | --- |
| slot A wins (side-balanced aggregate) | 129 / 238 | **54.2%** (z 1.30, p 0.19) |
| wins from the left-hand base | 125 / 238 | **52.5%** (z 0.78, p 0.44) |
| slot A in `normal` (A on the left) | 67 / 118 | 56.8% |
| slot A in `swapped` (A on the right) | 62 / 120 | 51.7% |

The 57.6%-B mirror lean that motivated this checkbox does not reproduce: on a
larger seed set the single-orientation half leans the *other* way (A 56.8%), and
the positional split is 52.5% left — no demonstrated spawn bias at this sample
size. What survives reflection is a small **slot** lean (A 54.2%), which is a
turn-order question, not a geography one, and which ordered pairs cannot cancel
for a mirror (a mirror's ordered pair is itself). Not significant at n = 238;
B3 should keep watching it.

Two per-strategy readings show the decomposition earning its keep. `mass_arclight`
wins 17 of 24 mirrors **from the left base** (70.8%, z 2.04) — consistent across
both orientations (normal A 9/12, swapped B 8/12) and invisible to a
single-orientation run, which would have reported a tidy A 9 / B 3. `mvp` wins 17
of 24 by **slot** (A 70.8%), equally in both orientations (8/12 and 9/12) — a
turn-order edge that reflection does not touch. One is geography, one is the
slot, and only playing both axes tells them apart.

**Cost.** The full batch doubles: 400 matches (10 strategies, ordered pairs,
2 seeds, 2 orientations) in **4m21s** release on the box, against 2m09s for the
200-match single-orientation run — linear in matches, as expected. Still zero
timeouts, median length 1:16, max 4:45 (F-020's "matches are far shorter than
the 5-8 min target" reading is unchanged).

## F-022 — Production is counted at the spawn, not at the order (B2 AC4)

**Wall.** A match record said who won and how long it took. B3 has to evaluate a
kill criterion from DESIGN_BRIEF — *"no unit winning >65% regardless of
counter"* — which is a statement about **units**, not strategies. Nothing in the
record named a unit, and nothing in the sim counted one.

**The choice that matters.** There are two places a "unit produced" could be
counted: where training is **ordered** (`enqueue_unit`) and where the unit
actually **spawns** (`production`). They are not the same number:

- an order is refused outright when the faction cannot pay for it;
- an order that *is* paid for sits in a `ProductionQueue` for
  `mvp_train_ticks`, and a match can end — decided or capped — with items still
  in flight.

Counting orders would therefore report units that never stood on the map, and a
timeout would inflate exactly the strategies with the longest build items. So
the counter is incremented inside `production`, on the tick the entity is
spawned, in the same stable entity-ordered loop that spawns it. "Produced"
means precisely **"existed at some point"**.

**Consequences, stated so B3 does not have to guess.** Production is *not*
survival: a unit that is built and then killed is counted in `Produced` and in
`Casualties` both, and the two resources are independent. And the headless
fixture's three starting workers a side are placed by the harness, not by
`production`, so they are not production — a batch that counted them would
report three free workers in every row.

**Shape.** `sim::economy::Produced` is modelled on `combat::Casualties`: a
sim-owned resource, incremented at the site of the event, installed with the
chain (F-004). Counts only, never an `Entity` — a count is comparable across
runs and app configurations where raw entity bits are not (F-011). Indexed by
faction slot, then by index into `Content::units` (RON order), never a map.

**The record carries its own header.** `batch::ProductionCounts` snapshots
`Produced` and ships the unit ids alongside the columns (`Arc<[String]>`, shared
across a batch's rows). A bare `Vec<u32>` whose meaning depends on remembering
the content's unit order is a mislabel waiting to happen, and every figure B3
prints is keyed by a unit name.

**Evidence.** No per-tick `state_hash` moved: the hash covers entity components,
not resources, and the pinned goldens (`b2_orientation`'s AC1 fixture pin, the
B1 golden hashes, the M5/M6 replay tests) are untouched and green.

## F-023 — "Side-balancing bounds the slot split" is not a theorem (B2 AC3 follow-up)

**Wall.** `mirrors_are_side_balanced_across_the_two_orientations` ended with
`dev(slot wins) <= dev(spawn wins)`, documented as a consequence of playing both
orientations. It is not. It holds only if the edge is purely positional; an edge
that follows the **slot** (turn order) survives reflection untouched and
falsifies it — `critic_b2_ac3.rs` already carried that counterexample.

**Measurement.** The claim is already false on real data: at 12 seeds
`mass_ripper` mirrors give slot deviation 6 against spawn deviation 2, and `mvp`
gives 10 against 2. The shipped test passed only because it used `mass_ripper`
at 3 seeds — a latent flake that would have fired the moment anyone raised the
seed count.

**Decision.** Assert what side-balanced sampling actually guarantees. Every
(mirror, seed) **cell** is played once per orientation, so for a cell decided in
both games exactly one of these holds: the same *slot* won twice (then the bases
differed — the cell is positionally even), or the same *base* won twice (then
the slots differed — the cell is even by slot). Hence the slot split comes only
from slot-persistent cells and the positional split only from base-persistent
ones, each exactly `2 x |imbalance in cells|`. Both identities are asserted per
cell. The observed deviations are printed, not asserted.

## F-024 — A timeout is not half a win; an undecided cell is not 0.5 (B3 AC1)

**Wall.** B3 turns match records into `W[i][j] = P(s_i beats s_j)`. A record
ends one of three ways (F-020), and the obvious reduction — "winner gets 1,
anything else is 0.5 each" — makes a matchup that never finishes read as a
perfectly balanced 50%. An all-timeout batch would then produce a flawless
matrix. That is the exact failure B3's critic probe names.

**Decisions** (`metrics::WinMatrix`, a pure function of `&[MatchRecord]`):

- **Decided** is a whole win for whoever played the surviving faction.
  **MutualLoss** is a decided draw: half a win to each side. **Timeout** is
  *undecided*: excluded from the rate and counted beside it (`n_timeout`).
- A cell with **zero decided matches has no rate** (`None`). An unplayed cell and
  an all-timeout cell are both unknown; `played()` tells them apart. A row with
  no defined off-diagonal cell has no mean.
- **Both slot orderings aggregate** into one cell, in both orientations, so the
  slot and spawn edges B2 measured cancel. Wins are accumulated as integer
  half-wins (win = 2, mutual loss = 1) and divided once, so for `i != j`
  `W[i][j] + W[j][i] = 1` holds exactly in integers — asserted on synthetic data
  and on a real batch.
- **The diagonal is the slot-A share of the mirror.** "Does `i` beat `i`" is
  vacuous; the claim "mirror ≈ 0.5" is really about seat bias, so that is what
  the diagonal measures. It is excluded from row means.
- **Row mean = mean of defined off-diagonal cell rates**, reported with its cell
  count: one reading per opponent, not pooled matches (pooling would let the
  most-decided opponent dominate a strategy's strength).
- **Labels and order come from the records**, first appearance (slot A before
  slot B), a linear scan — never a caller's list, never a map. For a
  `run_batch` result that is RON order. An empty slice is the empty matrix.

**Row means are exact (critic follow-up).** The first cut summed the cells'
`f64` rates in column order. Column order is first appearance in the records
and float addition is not associative, so the same batch in another order gave
`0.49999999999999994` instead of `0.5` — a strength that depends on record order
could pass or fail a `> 0.65` or "within tolerance of 0.5" gate on nothing but
ordering. Rejected. The requirement is now: **a row mean is the `f64` nearest
the exact rational mean of its defined off-diagonal cells** (ties to even).

Implementation: `mean = (1/k) Σ h_i / (2·n_i)` is summed over the common
denominator `k · Π 2·n_i` in a small arbitrary-precision integer and rounded
once by bit-by-bit long division (53 bits + guard + sticky). Integer arithmetic
is exact and commutative, so the mean is a function of the multiset of cells.

A `u128` rational with a canonical-order float fallback was tried first and
**rejected, because the fallback was reachable**. The sum stays below 2^128 for
any timeout pattern only while `k·(2N)^k < 2^128` (`N` = largest per-cell
`n_decided`): for a 10-strategy roster (`k = 9`) that is `2N ≲ 14,900`, about
1,860 seeds; for a 20-strategy roster (`k = 19`) about 11 seeds. Timeouts make
`n_decided` differ per cell, so the lcm of the denominators really does grow
toward the product. Converting a `u128` fraction to `f64` by shifting was also
not nearest once either term passed 2^53 — reachable with nine cells at about
100 seeds. The test `a_row_mean_beyond_u128_is_still_the_nearest_float_and_order_free`
(30 prime denominators, a 161-bit exact denominator) fails on that version
(`…018`) and passes on this one (`…019`, the nearest). There is no fallback now.
Cost is trivial: `k` terms of `k` 33-bit multiplications each.

**Noted, not fixed.** `batch::production_totals` takes its column header from
`records.first()` and silently reads every later row through it; the matrix
deliberately does not repeat that (every record contributes its own labels).

**First reading** (release, full roster, `--seeds 4`, 800 matches, 16 decided per
off-diagonal cell, 8 per mirror, 0 timeouts). Row means: turtle 90.3%,
synth_triad 78.5%, mass_sentinel 68.8%, synth_steel_flesh 68.1%, mass_ravager
47.9%, mass_arclight 47.9%, mvp 41.0%, mass_ripper 36.1%, rush 21.5%,
mass_bulwark 0.0% (loses every decided match to every opponent, rush included).
Recorded as an observation for B3's later checkboxes and B4; nothing was tuned.

## F-025 — The pentagon is derived from `units.ron`, and one link is broken (B3 AC2)

**The cycle is content, not Rust.** `onus::pentagon::nemesis_cycle` walks each
unit's `nemesis` link and returns the closed cycle, starting at the first
nemesis-bearing unit in RON order (today `bulwark`). Nothing in the crate
states `Sentinel > Ripper > Arclight > Bulwark > Ravager > Sentinel`; the only
copy of that sentence outside DESIGN_BRIEF is in `tests/b3_pentagon.rs`, where
the library cannot read it. A pentagon hardcoded in Rust would keep passing
after someone edited the RON — the one failure mode that would make this whole
assertion worthless. The same rule applies to the unit → strategy mapping:
`mass_strategy` finds the strategy whose **army build order** names one unit
and nothing else, never the strategy whose *name* contains the unit's, so a
`mass_bulwark` that quietly built rippers would not be mistaken for the probe.

A malformed roster is reported, not panicked on: an open chain, a self-nemesis,
an unknown prey, a lasso, and a cycle that closes while leaving other
nemesis-bearing units out are five distinct `CycleError`s.

**Four verdicts, not a bool.** `Holds` is *strictly* above 0.5 (a dead-even
matchup is not a counter); `Fails` is a defined rate at or below 0.5;
`Undefined` is no decided match (an all-timeout link is undefined, never 0.5 —
F-024's rule, carried through); `NoStrategy` is a gap in the probe set, which
is a fact about the instrument, not a reading about the game.

**The measurement** (release, the five mass probes, `--seeds 4`, 100 matches,
16 decided per link, 0 timeouts). Four of five predicted counters hold:

| link | rate | verdict |
|------|-----:|---------|
| bulwark > ravager  |   0.0% | **FAILS** |
| ravager > sentinel |  93.8% | holds |
| sentinel > ripper  | 100.0% | holds |
| ripper > arclight  |  75.0% | holds |
| arclight > bulwark | 100.0% | holds |

`mass_bulwark` does not win a single decided match against `mass_ravager` — nor
against anyone else (row mean 0.0%, matching the full-roster reading in F-024).
The Bulwark is not merely failing its counter; it is the weakest unit in the
game, and its +30% nemesis bonus vs the Ravager is nowhere near enough to
overcome that. That is the sim doing its job. **Nothing was tuned here**: this
AC reports, it does not gate and it does not fix. The candidate levers (Bulwark
cost 110 vs Ravager 90, its 2 Speed, the `nemesis_bonus.damage_mult`) are B4's,
in RON only.

## F-027 — The throughput cap was content all along (B3.5 AC0)

**What moved.** `StrategyDef` gains a required `queue_depth`, and the army step
in `sim::ai::think` now reads `(b.queued as u32) < script.queue_depth` where it
read `b.queued == 0`. The depth comes off **the commander's own strategy** (the
script it was named with, B1 AC2), never `content.ai`, so two sides in one match
may run different depths. Still **one order per decision**: the commander tops
its queue up by one, so a depth of 3 fills over three decisions and the
per-decision `budget` still commits at most one unit's Alloy. The field is
required (no `#[serde(default)]`, like `mvp_attack_ticks`) and `validate`
refuses `0` by name — depth 0 is "never train", content the sim cannot run.

**Shipped neutral, and proved so.** All ten strategies ship `queue_depth: 1`.
At depth 1 the new condition *is* the old one (`queued < 1` ⇔ `queued == 0`),
and no pinned per-tick `state_hash` golden anywhere in the suite was edited or
recomputed. Three independent neutrality readings:

- `b1_matchup`'s three pre-AC2 default-matchup goldens (seeds 4 / 11 / 23) still
  hold, and `b35_queue_depth` re-asserts the same three numbers through its own
  harness;
- two *named* depth-1 fixture matchups were captured from the **pre-change
  binary** — 30 per-tick `state_hash` samples folded into one number, plus the
  `AiJournal` digest — and replay byte-identically after the change
  (`0xff87018409ace43e` / `0x4e1604bd46e099f6`, `0xc0a77e736876e285` /
  `0x565da2a66530936a`);
- `critic_b1_ac2`'s cross-process pin recomputed the same 25 hashes before and
  after the change (its stored pin was a stale artifact of the abandoned tempo
  attempt and was deleted, not edited).

The one hash that *does* move is `Content::fingerprint` — deliberately: a
content **schema** change is a content change, and a log recorded under a
strategies file with no `queue_depth` must not replay against one that has it.
`ContentFingerprint` is not a per-tick state hash and nothing pins its value;
every consumer computes it from the content in hand, so nothing broke. The
standing guard `critic_m5::the_fingerprint_reads_every_field_of_every_content_struct`
reads `StrategyDef`'s fields off the source, so the new field had to be hashed
to keep it green — it was, next to `attack_spread`.

**What depth does and does not buy the tuning run.** `economy::production` ticks
only the **head** of a queue: a barracks builds one unit at a time whatever the
depth. So `queue_depth` is not parallel production and does not multiply
throughput — it buys back the ticks a barracks stands **idle** between a unit
popping and its commander's next decision (and lets income be committed ahead
rather than sitting in the stockpile). The size of that gap is set by
`think_interval_ticks` against `mvp_train_ticks`: at the shipped cadence (30
ticks vs a 720-tick Ripper) the gap is ~4% of the cycle; at a slow cadence (300
ticks) it is up to ~29%, which is the regime `b35_queue_depth`'s horizon test
measures — and reads: over a **7 200-tick (2-minute)** horizon, same script,
same unit, mirrored geography, neither side attacking, the depth-1 side
finished **7** units and the depth-3 side **9** (+29%, the gap almost exactly).
Before the change the same fixture gave 7 and 7. **Depth alone will therefore not lengthen a match into the 5–8 minute
band**; it is the lever that stops *longer train times* from simply shrinking
the army, which is the trap F-026 walked into. The tuning run should expect to
move `queue_depth` and train times together, and it now can.

## F-028 — Barracks count is the throughput lever (B3.5 AC0b)

**What moved.** Two Rust rules, no content. (1) `Content::validate` no longer
refuses a strategy whose `barracks` list names one building twice: an opening is
a *placement*, so N entries mean N buildings, each with its own `at_tick` and
`offset` (two identical entries are legal — the seeded RNG picks each one's
direction, so they do not land on top of each other, and no geometry rule was
added). (2) `sim::ai::think` counts instead of searching. The tech step walks the
openings in RON order keeping, per def, `(owned, walked)`; the k-th opening of a
def is already standing iff `owned > walked`, otherwise it is placed when
`tick >= at_tick` and the per-decision budget covers it — and *only then* is an
RNG draw taken, so a placement the commander cannot afford still consumes no
randomness (the B1 AC1 probe). The army step considers **every** barracks the
commander owns whose def its script opens and which can produce the wanted unit,
and picks the one with the **shallowest queue, ties broken by ascending
`Entity::to_bits()`** — the snapshot is already sorted by entity bits, so a
`min_by_key` on queue length *is* that rule and no query or archetype order can
reach the choice. Still **one army order per decision**.
`economy::production` was not touched.

**Why count and not depth.** F-027 measured `queue_depth` at 7 / 8 / 8 units over
6 000 ticks for depths 1 / 3 / 8: a queue's *head* is the only item that
advances, so a barracks is one production line whatever it holds, and depth buys
back only the idle gap between a pop and the next decision. Barracks count
multiplies the lines. Measured here, same script, same unit, mirrored geography,
ample Alloy, neither side attacking, over a **7 200-tick (2-minute)** horizon:

| lever | units finished |
| --- | --- |
| 1 barracks, `queue_depth: 1` | **9** |
| 3 barracks, `queue_depth: 1` | **27** |
| 1 barracks, `queue_depth: 3` | **9** |

Three lines is three times the army; three-deep on one line is the same army.
That is the whole finding: **the tuning run's throughput lever is the number of
openings, and `queue_depth` is a second-order smoother on top of it.** Note for
that run: `b1_probe_set::every_combat_unit_is_massed_by_exactly_one_probe`
asserts each mass probe opens exactly **one** barracks, and the probes must all
take the same count or they stop being comparable — so widening them is a
single, uniform edit to `strategies.ron` plus that one number.

**Shipped neutral, and proved so.** `strategies.ron` is unchanged — every
strategy still opens each building once — and with one opening per def the
counted tech step reduces to "do I have one?" and the shallowest-queue pick
reduces to the single candidate the old `find` returned. **No pinned per-tick
`state_hash` golden anywhere in the suite was edited or recomputed.** Three
readings: `b1_matchup`'s three pre-AC2 default-matchup goldens (seeds 4 / 11 /
23) still hold and are re-asserted through this AC's own harness; and two named
single-opening fixture matchups, whose 30-sample per-tick hash folds and journal
digests were captured from the **pre-change binary** in this run, replay
byte-identically (`0xff87018409ace43e` / `0x4e1604bd46e099f6`,
`0xc0a77e736876e285` / `0x565da2a66530936a` — the same numbers F-027's fixtures
produced, the geometry being identical).

**Two closed-milestone assertions were retired**, both of which pinned the rule
this AC deletes: the `"the same barracks twice"` case of
`b1_strategies::unrunnable_strategies_are_refused_at_load` and the
`"the same building opened twice"` case of
`critic_b1::every_broken_non_default_strategy_is_refused_by_name`. Every other
refusal in both lists stands, and `b35_parallel` re-asserts them (unknown
building, the victory building as a barracks, an army unit no opened barracks
can produce — repeats and all, no barracks at all, a zero offset on the repeat,
`queue_depth: 0`) plus the new positive: a strategy opening one building three
times now loads.

## F-029 — The arc is tunable in RON, and the 8-minute cap is the wall (B3.5 AC1)

**What this entry is.** The tuning checkbox: move the decided-match median from
~1:16 into DESIGN_BRIEF's 5-8 minute band **in RON only**, with the army *bigger*
rather than smaller (the trap F-026 fell into). Every number below was measured
on the box in release with `src/bin/balance`, at the shipped 28 800-tick
(8:00) cap unless the row says otherwise; no Rust was touched anywhere in this
run. **The result is a candidate, not a pass:** the tuning reaches the band,
and it breaks a designed property of the instrument while doing it. Both halves
are the finding.

### Before

Shipped content, whole roster (10 strategies x 10 x 2 seeds x 2 orientations =
400 matches), decided-only:

| min | p25 | median | p75 | p90 | max | timeouts | combat units built / match |
|---|---|---|---|---|---|---|---|
| 0:28 | 1:05 | **1:16** | 1:46 | 2:04 | 4:45 | 0 / 400 | 10.6 |

The five mass probes alone (5 x 5 x 2 seeds x 2 orientations = 100): min 0:50,
p25 1:05, median **1:19**, p75 1:31, max 2:24, 0 timeouts, 7.7 combat units per
match. A pentagon computed on that is a statement about openings.

### The levers, in the order they were tried

Each row is a batch of the five mass probes, 2 seeds, both orientations (100
matches), at the shipped cap. Changes are cumulative down the table except
where a row says "reverted"; "units" is combat units built per match, both
sides.

| # | change | median | p25 | p75 | timeouts | units | verdict |
|---|---|---|---|---|---|---|---|
| base | shipped | 1:19 | 1:05 | 1:31 | 0 | 7.7 | — |
| L1 | mass probes 1 -> 3 barracks (F-028's lever) | 1:03 | 0:51 | 1:07 | 1 | 10.2 | kept (density; it *shortens* the clock) |
| L2 | + `building_hp_per_defense` 40 -> 160 | 1:27 | 1:13 | 1:34 | 2 | 22.1 | **reject**: +24s for a 4x HQ, and the tail grows |
| L3 | HQ HP reverted; `attack_at_army` 3 -> 10 | 1:29 | 1:09 | 1:54 | 0 | 20.3 | kept |
| L4 | + `mvp_carry_capacity` 10 -> 4 (income x0.4) | 2:18 | 1:35 | 2:51 | 0 | 18.2 | kept |
| L5 | `attack_at_army` 16 | 3:15 | 2:12 | 3:58 | 0 | 29.3 | kept |
| L6 | `attack_at_army` 24 | 4:34 | 3:02 | 5:27 | 5 | 43.2 | kept, then re-cut (L9) |
| L7 | + `hp_per_defense` 20 -> 14 | 4:34 | 3:02 | 5:27 | 1 | 42.8 | **reject**: body identical, effect inside noise |
| L8 | + `mvp_carry_capacity` 4 -> 3 | 5:53 | 3:34 | 7:02 | 8 | 41.1 | kept |
| L9 | `attack_at_army` 20 | 5:01 | 3:05 | 6:04 | 7 | 35.5 | kept |
| L10 | + `mitigation_per_armor` 2 -> 1 | 5:01 | 3:05 | 6:04 | 4 | 34.8 | **reject** (see M1) |
| L11 | `attack_at_army` 24, mitigation 1 | 5:53 | 3:34 | 7:02 | 8 | 41.1 | — |
| L12 | + `hp_per_defense` 14 -> 10 | 5:53 | 3:34 | 7:02 | 8 | 41.1 | **reject**: 3 of 100 matches changed at all |
| L13 | combat scaling all reverted, `attack_at_army` 22 | 5:28 | 3:19 | 6:32 | 11 | 38.6 | — |
| L14 | + `attack_interval_ticks` 600 -> 300 | 5:28 | 3:19 | 6:32 | 8 | 38.0 | kept |
| C1 | `attack_at_army` 20, 4 seeds (200 matches) | 5:02 | 3:06 | 6:04 | 11 (5.5%) | 35.1 | kept |
| C3 | + `building_hp_per_defense` 40 -> 120 | 5:04 | 3:07 | 6:07 | 11 (11%) | 35.5 | **reject**: floor unmoved, tail fattened |
| M1 | final content + `mitigation_per_armor` 1 | 6:32 | 4:23 | 7:47 | 16 (16%) | — | **reject**: does not unstick the grind |

**What the table says.** Three knobs move the clock and one of them is not a
clock at all:

- **Barracks count** (F-028) buys *army*, not time — it makes matches shorter
  and much denser. It is what keeps the tuning out of F-026's trap: every later
  row lengthens the game with the army growing, not shrinking.
- **`mvp_carry_capacity`** is the economy clock. Income is loads/second times
  the load, an army is a fixed number of Alloy, so this sets how many minutes a
  force takes to assemble. 10 -> 4 -> 3 -> 2 is most of the length here.
- **`attack_at_army`** is the commitment threshold: it decides how much of that
  income is on the field when the decisive fight happens. It moved the median
  from 1:29 to 5:53 by itself and raised density with it.
- **`mvp_combat` scaling is nearly inert at batch level.** `hp_per_defense`
  20 -> 14 -> 10 and `mitigation_per_armor` 2 -> 1 changed 3, 13 and 0 matches of
  100 respectively; the quantiles did not move at all. Match length here is set
  by how long an army takes to *assemble*, not by how long it takes to die.
  Every combat-scaling change was therefore reverted, which also keeps the
  pentagon's own dials out of a tempo tuning.
- **HQ HP is not a lengthener either.** Quadrupling it (L2) bought 24 seconds
  when armies were small, and tripling it at the tuned length (C3) moved the
  median by 2 seconds while doubling the timeout rate: a 20-unit army chews any
  HQ in seconds, so the knob only adds to matches that are already long.

### What was kept

`assets/data/units.ron`
- `mvp_carry_capacity` **10 -> 2** (a 5x slower economy; the one number).

`assets/data/strategies.ron`
- the five `mass_*` probes: **3 openings each** of their own barracks
  (at_tick 300/600/900, offset 130/165/200), `attack_at_army` **3 -> 20**,
  `attack_interval_ticks` **600 -> 300**. Knob-identical, all five, F-018.
- `synth_steel_flesh`: 4 lines (2 Foundry + 2 Gene-Vats), `attack_at_army` 16,
  interval 300. `synth_triad`: 4 lines across all three domains,
  `attack_at_army` 16, interval 300. `turtle`: 4 lines, `attack_at_army` 28,
  interval 600 — still the latest, largest commitment in the set.
- `rush` unchanged: it is the pole, and its identity is the tick-0 opening and
  the one-body attack.
- **`mvp` unchanged, deliberately.** B1 pins the default strategy field for
  field as the faithful promotion of M4c's `mvp_ai`
  (`b1_strategies::the_default_strategy_is_the_old_mvp_ai_number_for_number`,
  and `m4c_ai` reads it as "the default AI" with one barracks). Retuning it
  would change what those assertions *mean*, not just their values, so it was
  reverted and left alone. The consequence is real and is a question for the
  next checkbox: the shipped default now plays the slow economy on the old fast
  tempo.

### After

Whole roster, 2 seeds, both orientations, 400 matches, shipped 28 800 cap,
decided-only:

| min | p25 | median | p75 | p90 | max | timeouts | combat units / match |
|---|---|---|---|---|---|---|---|
| 0:28 | 4:23 | **5:17** | 6:05 | 6:59 | 7:51 | 43 / 400 (10.8%) | 32.7 |

(357 decided of 400. The 0:28 floor is the `rush` mirror — two all-ins meeting
at the door — and is the same floor the shipped content had.)

An intermediate reading worth keeping, because it is the price of leaving `mvp`
untuned: with `mvp` on three Foundries and `attack_at_army: 12`, the same batch
read decided median 5:08, p25 4:24, p75 6:03, **29 / 400 (7.25%) timeouts** and
33 units per match. Reverting `mvp` to its pinned numbers cost 3.5 points of
timeout rate and left the default strategy with two cells it cannot decide at
all (row mean over 7 cells, not 9). The default AI is now the one script in the
roster that commits three units into a five-minute economy.

Density: **units built** 32.7 per match over the 400-match batch (before: 10.6), and on
a 16-match probe of named matchups, **33.6 units built and 9.9 casualties** per
match. The casualty number carries a caveat and it is the honest half of this
entry: it ranges from 0 to 47. `synth_triad` vs `synth_steel_flesh` trades 47
bodies over eight minutes and `mass_sentinel` vs `mass_ripper` trades 21, but
`mass_arclight` vs `mass_sentinel` builds 37 units and loses **2**, and the
`turtle` mirror builds 60 and loses 7. So the army is unambiguously bigger than
before (F-026's failure mode is not present) but a good part of the added time
is two armies *assembling*, not two armies trading. Making the fight itself the
long part is a stat question (engagement ranges, damage-to-HP), which is B4's.

### The cap binds, and that is the blocker

The band's top and the runner's cap are the same eight minutes, so a
distribution centred in the band loses its upper tail to the cap. Measured on
the kept content, **the five mass probes on a raised 20-minute cap** (2 seeds,
100 matches; a diagnostic run, never a shipped setting):

| min | p25 | median | p75 | p90 | max | timeouts |
|---|---|---|---|---|---|---|
| 4:22 | 4:23 | **6:32** | 7:47 | 8:33 | 10:45 | 0 / 100 |

Every one of those hundred matches decides — by 10:45 at the latest. So the
probe set's true arc is 6:32, comfortably inside the band, and **16 of its 100
matches exceed the 8-minute cap**: at the shipped cap they are recorded as
timeouts, not as stalemates. The clipped 16 are one class: `mass_bulwark`
mirrors, `mass_bulwark` vs `mass_ravager` both ways, and `mass_ravager`
mirrors — the heavy-armour grind, where `armor * mitigation_per_armor` eats
most of a hit (Bulwark on Bulwark is 20 damage against 18 mitigation).

Two designed properties fail because of it, and neither can be edited without
changing what it means:

- `critic_b1_ac3::every_mass_versus_mass_cell_resolves_in_both_orientations`
  ("a cell that times out is a hole in the pentagon, and a matrix of holes
  cannot support the assertion B3 exists to make"): the Bulwark mirror does not
  resolve inside its 20 000-tick horizon, and does not resolve inside the
  28 800-tick match cap either. Raising the horizon past the cap would keep the
  test green while the hole stays in B3's matrix.
- `b3_pentagon::the_real_batch_reports_what_the_sim_actually_does`: at the new
  length the reading is **2 of 5 links holding**, with `bulwark > ravager`
  **undefined** (0 decided, 8 timeouts) and `ravager > sentinel` and
  `sentinel > ripper` newly failing. F-025's one broken link was not a
  short-game artifact; at the long length the instrument reads worse, and one
  link cannot be read at all.

Attempts to unstick the grind inside this checkbox's remit all failed: armour
mitigation halved (M1) leaves 16 timeouts; unit HP cut by 30% and by 50%
changed almost no match. **The grind is a unit-stat problem (B4's pass), not a
tempo one** — which is exactly what F-025 said about the Bulwark before the
clock was touched.

### The suite: what moved, and why the gate is **not** green

`cargo test --release --no-fail-fast` on the tuned content: **811 passed, 25
failed** across 17 test binaries (`b35_tempo`'s two new tests are among the
passes). The failures fall into three piles, and the third is why this entry
stops rather than finishing:

1. **Pinned per-tick `state_hash` goldens — content-driven, recomputable, and
   deliberately not recomputed here.** Every one of them moved, because a RON
   change moves every hash by construction (no Rust was touched: `git diff` on
   `src/` and `benches/` is empty). **The proof was run**: with the
   pre-change `assets/data` restored under the *post-change* binary (identical
   Rust — `git diff f6a2aeb -- src benches` is empty), `b1_matchup`,
   `b2_headless`, `b2_orientation`, `b35_parallel` and `b35_queue_depth` —
   51 tests, every golden-bearing one in the suite's B-series — pass at their
   **old** pinned values, unedited. Old data, old numbers; new data, new
   numbers; nothing in between. The three distinct
   fixtures behind them, old -> new, read straight off the failures:

   | fixture (who pins it) | old | new |
   |---|---|---|
   | pre-AC2 default matchup, seed 4 (`b1_matchup`, `b1_strategies`, `b35_queue_depth`, `b35_parallel`) | `0xa71f_64ca_d502_03e9` | `0xbb69_254d_5833_7869` |
   | pre-B2 bench fixture, tick 300 (`b2_headless`, `b2_orientation`) | `0xa5b4_138c_f475_fd00` | `0xb832_456b_5a74_b590` |
   | `solo_ripper` vs `solo_bulwark` / `depth_ripper` vs `depth_bulwark`, seed 4 (`b35_parallel`, `b35_queue_depth`) | `0xff87_0184_09ac_e43e` | `0xfec2_0c1e_d0c2_f806` |

   The rest — seeds 11 and 23 of the default matchup, the other bench ticks and
   the fold, the journal digests, and the cross-process pins under
   `critic_b1_ac2` / `critic_b2_ac4` / `critic_b35_ac0` / `critic_b35_ac0b` —
   were left un-recomputed on purpose: re-pinning thirty goldens to a
   candidate that pile 3 may force to be re-scaled means recomputing thirty
   numbers twice, and buries the blocker in a large mechanical diff.

2. **Fixture and budget values that legitimately change, meaning intact.**
   `b1_probe_set::every_strategy_eventually_attacks` and
   `critic_b1_ac3::every_strategy_commits_before_the_match_can_stop_it` give a
   strategy 12 000 ticks (3:20) to commit, and a probe that masses twenty units
   on the new economy commits at about 15 700; `critic_b1::a_placement_the_
   commander_cannot_afford_consumes_no_randomness` builds a poor commander whose
   budget no longer buys a barracks; `m4a_economy`, `critic_m4a` and
   `critic_m4b` anchor on the old worker load (`the probe's anchor text still
   exists`, `probe assumes a multi-Alloy load`, `the worker never picked up a
   load` — the probe expects 10 Alloy and gets 2); and
   `critic_p2::no_configuration_of_the_writer_changes_a_single_tick_of_the_sim`
   reports `the fixture never decided`, its match horizon predating a
   five-minute arc. Each of those is a number to re-measure
   against the new content, and each keeps its meaning (a commitment budget
   under the 28 800 cap is still "before the match can stop it").
   `b1_probe_set`'s own probe-count assertion was already updated in this diff:
   `MASS_PROBE_BARRACKS = 3` replaces a hard-coded 1, and the knob-identity test
   now compares **every** opening's tick and offset rather than only the first.

3. **Two assertions that cannot be re-valued without changing what they
   say** — the blocker:
   - `critic_b1_ac3::every_mass_versus_mass_cell_resolves_in_both_orientations`
     (see above): the Bulwark grind does not resolve inside the match cap, so
     raising the test's horizon past 28 800 would make the test pass while the
     hole stays in B3's matrix.
   - `b1_strategies::the_default_strategy_is_the_old_mvp_ai_number_for_number`
     would have had to change if `mvp` were tuned. It was not tuned, so this one
     passes — at the cost recorded above (10.8% timeouts instead of 7.25%, and
     a default strategy with two undecidable cells).

**Not run, therefore not claimed:** release-profile tests, `cargo clippy
--all-targets -- -D warnings` in either profile, and `cargo bench --no-run`.
The debug suite is red by construction while piles 1 and 3 stand.

### Verdict

The AC's number is reachable in RON: median **5:17** over the whole roster,
10.8% timeouts, with 33 units built and ~10 casualties a match — an arc in the band with
real armies in it, which is what F-026 could not do. But it is reached by
letting the slowest matchup class run past the runner's cap, which takes the
pentagon from "one broken link" to "two links broken and one unreadable".
Whether to ship it, re-scale it down (the whole roster at median 4:24 keeps
timeouts at 3.25% but leaves the band), or fix the Bulwark's stats first (B4)
is a decision above this checkbox. **Stopped here rather than editing the
assertions that say so.**

## F-030 — The matches were never fights; the cap was censoring them (B3.5, after the cap decision)

**What changed since F-029.** Two things were authorised: the match cap moves
from 8 to **15 minutes** (`DEFAULT_MATCH_SECS`, the single Rust line the
RON-only rule bends for — `git diff main -- src` shows that constant and its
comment and nothing else), and the armour/damage relation may be retuned in RON
so heavy matchups decide on their own. The design metric becomes **the share of
decided matches inside the 5-8 minute band**, with the timeout rate kept beside
it as the stalemate signal.

Raising the cap alone did what it was predicted to do: F-029's content, replayed
at 15 minutes, decides **every** match — 100 of 100 probe matches, median 6:32,
max 10:45, and the pentagon's `bulwark > ravager` cell comes back from
*undefined* to a measured 0.0%. The 8-minute cap had been censoring, not
catching.

### The diagnostic that redirected the whole tuning

Before touching armour, one heavy matchup was instrumented tick by tick
(`mass_bulwark` mirror, seed 0, sampled every 3 600 ticks):

```
t=0      units [3, 3]    HQ [400, 400]   casualties A 0 B 0
t=3600   units [7, 7]    HQ [400, 400]   casualties A 0 B 0
...
t=36000  units [25, 25]  HQ [400, 400]   casualties A 0 B 0
t=38551  decided
```

**Ten minutes, two full armies, zero casualties, both HQs untouched.** The
armour arithmetic was never the binding constraint: the armies were not
fighting at all. F-029's tuning had bought its length with
`attack_at_army: 20` against a five-times-slower economy, so a match was
*"time to assemble twenty units"* — unit cost divided by income — and the first
wave to arrive ended the game. That also explains F-029's other readings: the
cheapest unit dominated the matrix (`mass_ripper` row mean 95.8%), and armour
changes moved nothing. The A/B proves it: `mitigation_per_armor` 2 -> 1 on that
content changed the batch's max from **10:45 to 10:44** and left production
identical to the unit. You cannot tune a fight that is not happening.

### The re-tune: commit early, make the base hard

The arc has to come from armies *meeting repeatedly*, not from a single
assembled doomstack, so the two knobs moved the other way:

- **commitment thresholds down** — the five probes from `attack_at_army: 20` to
  **10** (knob-identical, all five), `synth_*` 16 -> 9, `turtle` 28 -> 15;
- **base durability up** — `building_hp_per_defense` 40 -> **420**, so an HQ is
  4 200 HP and survives waves: the loser of a fight gets to rebuild and fight
  again instead of losing the match to the first wave that arrives.

Measured, five mass probes, 2 seeds, 100 matches, 15-minute cap:

| run | change | median | in 5-8 band | timeouts | pentagon |
|---|---|---|---|---|---|
| (F-029 content at 15 min) | — | 6:32 | 48% | 0 | 2/5, all cells decided |
| C1b | `attack_at_army` 8, HQ 1 200 | 3:26 | 18% | 0 | 4/5 |
| C2 | HQ 2 400 | 4:13 | 35% | 0 | 5/5 |
| C3b | HQ 3 600 | 5:37 | 42% | 0 | 5/5 |
| C4 | HQ 3 000, `attack_at_army` 10 | 4:48 | 35% | 0 | 4/5 |
| P1 | + `mvp_gather_ticks` 90 -> 120 | 6:06 | 27% | 0 | **reject**: a slower economy stretches the expensive armies most and *widens* the spread |
| **Na** | **HQ 4 200, `attack_at_army` 10 (kept)** | **6:36** | **36%** | **0** | 4/5, every cell decided |

Whole roster (10 strategies, 2 seeds, both orientations, 400 matches):

| run | median | p25 | p75 | p90 | in 5-8 band | timeouts |
|---|---|---|---|---|---|---|
| FULL1 (`attack_at_army` 8, HQ 3 600) | 4:29 | 3:44 | 6:03 | 8:28 | 30.5% | 6/400 (1.5%) |
| FULL2 (HQ 4 200) | 4:36 | 3:50 | 6:19 | 8:55 | 30.5% | 7/400 (1.75%) |
| **FULL3 (kept)** | **5:05** | 4:14 | 7:10 | 9:57 | **31.5%** | **9/400 (2.25%)** |

**Density, and this is the point:** 37.1 combat units built and **30.4
casualties** per match over an 18-match probe of named matchups, against
F-029's 33.6 built and **9.9** lost. Three times the trading for the same army
size. The same `mass_bulwark` mirror that spent ten minutes with zero
casualties now decides at 6:33 with 16 bodies lost, and `mass_bulwark` vs
`mass_ravager` at 6:46 with 20.

### The armour lever: measured, and *not* taken

Re-run in the new regime, where fights actually happen,
`mitigation_per_armor` 2 -> 1 does exactly what the arithmetic predicts — it
hits armour and leaves the swarm alone (mean length per pentagon cell):

| cell | mitigation 2 | mitigation 1 | change |
|---|---|---|---|
| arclight / bulwark | 11:39 | 9:33 | **-18%** |
| bulwark / sentinel | 7:43 | 6:05 | **-21%** |
| bulwark / ripper | 6:35 | 5:07 | **-22%** |
| bulwark / ravager | 7:47 | 7:19 | -6% |
| ripper mirror | 2:29 | 2:27 | -1% |
| sentinel mirror | 3:40 | 3:37 | -1% |

So the lever works and does not distort the light end. **It was still not
kept**, on the length criterion the checkbox is judged by: with the thresholds
and base durability re-cut, the heavy class already decides on its own — every
cell decided, 0 timeouts, slowest cell 11:39 inside a 15-minute cap, and the
Bulwark mirror trades 16 bodies — while taking the armour change costs band
share (36% -> 30% on the probes; 31% at HQ 4 800, tried as compensation). The
decision is the band, not the pentagon: for the record, mitigation 1 *also*
moved the pentagon from 4/5 to 3/5, and that played no part in keeping 2. **One
number reverses this** (`mvp_combat.mitigation_per_armor`) if a later pass
would rather have shorter heavy fights than a wider band.

### The pentagon at the new length, every cell decided (observation)

Whole roster, 400 matches, 8 decided per link, **0 timeouts in any pentagon
cell**:

| link | rate | sample |
|---|---|---|
| bulwark > ravager | 87.5% | 8 decided, 0 timeouts — **holds** |
| ravager > sentinel | 62.5% | 8 decided, 0 timeouts — holds |
| sentinel > ripper | 50.0% | 8 decided, 0 timeouts — **FAILS** (exactly even is not a counter, F-025) |
| ripper > arclight | 100.0% | 8 decided, 0 timeouts — holds |
| arclight > bulwark | 100.0% | 8 decided, 0 timeouts — holds |

Four of five, and the failing one is *measurable* — which is the whole point of
the exercise. Row means run from `rush` 4.2% to `turtle` 88.9%; both are outside
B3's 65% kill-criterion and are B4's business, not this checkbox's. Note how
much the reading moves with tempo (F-029's content read 2/5 with one cell
undefined; C2/C3b read 5/5): **a pentagon is a statement about a tempo**, and it
should be re-read whenever the arc changes.

## F-031 — The armour question, closed: mitigation 1 earns its band share once the commitment threshold is re-walked (B3.5)

**What this entry is.** F-030 measured `mvp_combat.mitigation_per_armor` 2 -> 1,
found that it shortens heavy fights 18-22% and leaves the light end alone, and
then **reverted it** because at the kept commitment threshold
(`attack_at_army: 10`) it cost band share — 36% -> 30% on the five mass probes.
That revert rested on an incomplete search: shortening the long tail *narrows*
the distribution, and a narrower distribution with a low median can be
re-centred by lengthening, which `attack_at_army` does. This entry completes the
search over that knob, **at both mitigation settings** (a one-sided walk would
only prove that `attack_at_army` matters), and settles whether F-030's revert
was right.

**Method.** Every row is `src/bin/balance` in release on the box, five mass
probes, both spawn orientations, 15-minute cap:
`balance --seeds K --minutes 15 --only mass_bulwark,mass_sentinel,mass_ripper,mass_ravager,mass_arclight`.
`attack_at_army` moves on all five probes together (knob identity, F-018) and on
the `synth_*` / `turtle` scripts in proportion to F-030's ratios (`synth` = 0.9x,
`turtle` = 1.5x, integer-truncated); `mvp` and `rush` are untouched throughout.
Quantiles are `Tally::length_quantile`'s definition (`ceil(q*n)`) over **every
match in the batch, capped matches included** — `Tally::of` in `src/batch.rs`
pushes `r.ticks` for every record, so a timeout contributes its full 15:00 to the
length distribution. (The critic pinned this as
`the_printed_length_quantiles_include_capped_matches`; an earlier draft of this
entry wrongly described the basis as "decided matches only".) The two bases
coincide only on a zero-timeout batch, and diverge measurably once there are
timeouts — on the shipped content at 400 matches with 11 timeouts, decided-only
median/p90/max is **6:21 / 10:57 / 14:48** against the printed **6:26 / 11:35 /
15:00**; at 1 250 matches with 41 timeouts, decided-only median **5:37** against
printed **6:10**. **Consequence to carry forward: every tail statistic quoted
below (p90, max, "identical tails") includes capped matches**, so a content with
more timeouts is flattered in the median and penalised in the tail by the same
censoring. "band" is the share of **decided** matches in 5:00-8:00 (that one *is*
decided-only); p90 is computed from the same per-match log on the printed basis. "units" is every unit both sides built
across the batch (workers included), the F-026 density guard.

### The walk, 2 seeds (100 matches per row)

| row | mit | `attack_at_army` | median | p25 | p75 | p90 | max | **band** | timeouts | units |
|---|---|---|---|---|---|---|---|---|---|---|
| base (F-030 kept) | 2 | 10 | 6:36 | 4:42 | 8:57 | 10:39 | 14:42 | **36%** | 0 | 4621 |
| A1 | 1 | 10 | 5:44 | 4:27 | 8:00 | 9:56 | 14:08 | 30% | 0 | 4261 |
| A2 | 1 | 12 | 5:30 | 4:15 | 8:12 | 9:41 | 12:01 | 42% | 1 | 4092 |
| A3 | 1 | 14 | 6:14 | 3:27 | 9:11 | 9:54 | 14:26 | 30% | 0 | 4261 *(suspect — see note)* |
| A4 | 1 | 16 | 5:30 | 3:46 | 7:06 | 10:30 | 12:59 | **50%** | 0 | 3932 |
| A5 | 1 | 18 | 6:05 | 4:06 | 7:17 | 8:33 | 14:38 | **51%** | 0 | 4014 |

| A6 | 1 | 20 | 6:43 | 4:29 | 7:59 | 8:43 | 11:05 | 43% | 0 | 4207 |
| A7 | 1 | 22 | 7:21 | 4:50 | 8:41 | 9:32 | 12:02 | 28% | 0 | 4487 |

**Correction to the `units` column.** The base row's `units` is **4621**, not the
4261 an earlier draft recorded: 4261 is row A1's value, and it had been copied
into the base row (and, identically, into A3, which is why that cell is flagged
suspect above and should be re-read before it is used). `units` is F-026's
army-density guard, so a wrong number there is a wrong guard — the base row's
density is *higher* than A1's, not equal to it, which strengthens rather than
weakens the reading below.

A1 reproduces F-030's reverted reading exactly (30%), which is the check that
this is the same measurement. The walk has an interior optimum at
`attack_at_army` **16-18**: band share 30 -> 42 -> 30 -> 50 -> 51 -> 43 -> 28,
and the tail tightens with it (matches over 8:00: 26 at A=10, **13** at A=18).
So the premise holds — mitigation 1 plus a higher commitment threshold clears
the kept content's 36%.

### The counterfactual: the same walk at mitigation 2

A one-sided walk cannot tell the armour change from the threshold change, so the
sweep was re-run with `mitigation_per_armor` left at 2 — **over the top of the
range only.** An earlier draft called it "the *identical* sweep"; it was not.
A=12 and A=14 were never run at mitigation 2, and the pooled "+2 ± 2.5 points"
below uses **only A=16 and A=18**. The conclusion holds on those two thresholds;
the method sentence claiming a matched full sweep does not.

| row | mit | `attack_at_army` | median | p25 | p75 | p90 | max | **band** | timeouts | units |
|---|---|---|---|---|---|---|---|---|---|---|
| B1 | 2 | 16 | 6:15 | 3:48 | 7:41 | 11:09 | 14:21 | 44% | 1 | 4316 |
| B2 | 2 | 18 | 6:09 | 4:08 | 7:53 | 10:13 | 13:21 | 46% | 1 | 4201 |
| B3 | 2 | 20 | 6:45 | 4:30 | 8:01 | 8:59 | 13:44 | 36% | 0 | 4345 |
| B4 | 2 | 22 | 7:23 | 4:51 | 8:43 | 9:33 | 12:12 | 28% | 0 | 4526 |

**Most of the gain was the threshold, not the armour.** Mitigation 2 peaks in
the same place (46% at A=18) and for the same reason. The armour change is worth
about **5 points of band share** on top of that (51% vs 46%, 50% vs 44%) — which
at 100 matches is roughly one standard error of the difference and therefore not
yet a result. Hence the decisive run below.

### The decisive run: 400 matches per setting, at the walk's optimum

`attack_at_army: 18`, five probes, 8 seeds x both orientations (3 shards on
seed bases 0/1/2, `seed_at` mixes the base so the three are different seed
sets), 400 matches per setting:

| mit | median | p25 | p75 | p90 | max | **band** | timeouts | over 8:00 | under 5:00 |
|---|---|---|---|---|---|---|---|---|---|
| 1 | 6:05 | 4:07 | 7:51 | **10:03** | 14:49 | **47.4%** (186/392) | **8 (2.00%)** | 63 | 143 |
| 2 | 6:09 | 4:10 | 7:55 | 10:19 | 14:53 | 42.6% (164/385) | 15 (3.75%) | 82 | 139 |

Mitigation 1 wins every column it should: +4.8 points of band share, half the
timeout rate, 63 over-length matches instead of 82. The band-share gap is ~1.3
standard errors of the difference (SE ≈ 3.6 points at n≈390), so on band share
alone the armour change is a *consistent small positive* rather than a proven
one — it reads +5 at A=16 (50 vs 44), +5 at A=18 on 100 matches (51 vs 46) and
+4.8 at A=18 on 400. The timeout halving is the sharper signal, and it is the
mechanism F-030 already measured: mitigation 1 shortens exactly the matches that
were running long.

### And here is what the walk costs: the pentagon degrades monotonically with the threshold

The same batches, read as pentagon links (predator > prey, pooled over both
orderings and orientations — `WinMatrix`'s own definition):

| `attack_at_army` | 10 (F-030 kept) | 12 | 14 | 16 | 18 | 20 | 22 |
|---|---|---|---|---|---|---|---|
| links holding, mit 2 | **4/5** | — | — | 3/5 | 2/5 | 2/5 | 2/5 |

A caution on this table that the rest of this entry earns: every cell but A=18 is
an **n=8-per-link** reading — the exact sample size this entry declares unable to
support a verdict — and `holding()` is a bare `rate > 0.5` count with no interval
(the critic pinned this: `a_holds_verdict_says_nothing_about_the_interval`). Only
A=18 is at 400 matches. The A=10 cell also disagrees with the 400-match and
1 250-match re-reads below (4/5 here; 4 holding + 1 undetermined there). So read
the *direction* — the threshold costs links — and not the individual counts.
| links holding, mit 1 | 3/5 | 3/5 | 3/5 | 2/5 | 2/5 | 2/5 | 2/5 |

At `attack_at_army: 18`, on 400 matches per setting, the reading is the same
collapse at both mitigation settings — and it is not a sampling artefact:

| link | mit 1, n=400 | mit 2, n=400 |
|---|---|---|
| sentinel > ripper | **0.0%** (n=32, CI [0.0, 10.7]) | **0.0%** (n=32, CI [0.0, 10.7]) |
| ripper > arclight | 100.0% (n=32) | 100.0% (n=32) |
| arclight > bulwark | 100.0% (n=30, 2 timeouts) | 100.0% (n=30, 2 timeouts) |
| bulwark > ravager | **13.8%** (n=29, 3 timeouts) | **16.0%** (n=25, 7 timeouts) |
| ravager > sentinel | **0.0%** (n=30, 2 timeouts) | 26.7% (n=30, 2 timeouts) |

Every cell is still *readable* (no cell is undefined), but three of five designed
counters are now decisively inverted with the CI excluding 50%, against the kept
content's four confirmed links (plus one undetermined). The cause is the threshold, not the armour: both columns read
the same. Raising `attack_at_army` makes a match "assemble eighteen bodies and
commit", and at that size the cheap fast swarm (Ripper, every `x vs ripper` cell
decides at ~4:08) runs away with the matrix — F-030's own diagnosis of F-029's
content, reappearing one knob later.

**So the two things the band metric wants from this knob are opposed:** band
share peaks (47%) exactly where the counter-pentagon stops being readable as a
cycle, and the pentagon reads best (4 links confirmed) at the threshold with the
lowest band share (36%).

### The baseline, re-measured at 400 matches — and it was never 36%

F-030's 36% was a 100-match reading. The kept content on the same 400-match
sample as the candidates:

| content | median | p25 | p75 | p90 | max | **band** | timeouts | over 8:00 | under 5:00 | pentagon |
|---|---|---|---|---|---|---|---|---|---|---|
| **kept (mit 2, A=10)** | 6:25 | 4:30 | 7:45 | 10:06 | 14:43 | **38.0%** (149/392) | 8 (2.00%) | 93 | 150 | **4 + 1?** |
| mit 1, A=18 | 6:05 | 4:07 | 7:51 | 10:03 | 14:49 | 47.4% (186/392) | 8 (2.00%) | 63 | 143 | 2/5 |

Two corrections to F-030 fall straight out of this, both from sample size:

- the kept content's band share is **38%**, not 36%, and its timeout rate is
  **2.00%, not 0** — the 100-match probe batch simply had no capped match in it;
- the **spread is the same**. p90 10:06 vs 10:03, max 14:43 vs 14:49. Mitigation
  1 does narrow the distribution, but the threshold that pays for its band share
  widens it back by exactly as much. The candidate's only real spread win is the
  over-8:00 count (63 vs 93).

So the honest ledger of the candidate is **+9.4 points of band share, identical
timeouts, identical tails, and three of five designed counters inverted.** (The
"identical tails" reading is on the printed quantile basis, which **includes the
capped matches at their full 15:00** — see Method. With equal timeout counts on
both sides, 8 and 8, the comparison is still apples-to-apples; it would not be
against a content with a different timeout rate.)

### The pentagon's sample size, and F-030's `sentinel > ripper`

F-030 recorded `sentinel > ripper` as **FAILS at exactly 50.0% over 8 decided
matches**. 8 matches cannot tell 50% from 65%: the 95% Wilson interval on 4/8 is
**[21.5, 78.5]**, which contains every rate anyone would care about. A pentagon
cell accumulates 4 decided matches per seed (2 orientations x the two orderings
`WinMatrix` pools), so the sample size is a seed count, and the seed count was 2.

At **8 seeds (n = 32 per link)** on the kept content the link is not even close
to even:

| link | rate | n | 95% Wilson CI | verdict |
|---|---|---|---|---|
| sentinel > ripper | **68.8%** | 32 | [51.4, 82.0] | **holds** (CI excludes 50%) |
| ripper > arclight | 96.9% | 32 | [84.3, 99.4] | holds |
| arclight > bulwark | 100.0% | 30 (+2 to) | [88.6, 100.0] | holds |
| bulwark > ravager | 90.6% | 32 | [75.8, 96.8] | holds |
| ravager > sentinel | 53.1% | 32 | [36.4, 69.1] | **undetermined** — the CI straddles 50% |

**The kept content reads 4 holding + 1 undetermined, not 4 broken-one.** F-030's
one failing link was a sampling artefact of reading a 50/50-looking cell off
eight matches; the real coin-flip in the cycle is `ravager > sentinel`, and at
n=32 it cannot be called either way. That is the number that needs the seeds, so
the run below raises it — **and at 430 pooled matches it is still undetermined
(54.9%, CI [50.2, 59.5]), so this reading is the one that survived.**

### The armour change's own effect, at 400 matches per cell of the 2x2

| `attack_at_army` | mit 1 band | mit 2 band | mit 1 - mit 2 | mit 1 timeouts | mit 2 timeouts | mit 1 p90 | mit 2 p90 | mit 1 units/match | mit 2 units/match |
|---|---|---|---|---|---|---|---|---|---|
| 16 | 47.1% | **48.0%** | **-0.9** | 5 (1.25%) | 8 (2.00%) | 10:33 | 11:08 | 40.4 | 43.5 |
| 18 | **47.4%** | 42.6% | **+4.8** | 8 (2.00%) | 15 (3.75%) | 10:03 | 10:19 | 42.3 | 44.3 |

Pooled over the two thresholds the armour change is worth **+2 points of band
share with a standard error of about 2.5** — it is not distinguishable from
nothing. The n=100 rows that made it look like +5 were one standard error of
sampling. What *does* survive the larger sample is the mechanism F-030 named:
mitigation 1 consistently cuts the timeout rate (5 vs 8 at A=16, 8 vs 15 at
A=18) and shaves the top of the distribution, because it shortens exactly the
heavy matchups that were running into the cap. It buys **tail**, not band.

### The isolated A/B, at the kept threshold and 400 matches: mitigation 1 loses

F-030 ran this comparison on 100 matches (36% -> 30%) and reverted on it. At four
times the sample, with the threshold left exactly where the shipped content has
it (`attack_at_army: 10`):

| content | median | p25 | p75 | p90 | max | **band** | timeouts | under 5:00 | units/match | pentagon |
|---|---|---|---|---|---|---|---|---|---|---|
| mit **2**, A=10 (shipped) | 6:25 | 4:30 | 7:45 | 10:06 | 14:43 | **38.0%** | 8 (2.00%) | 150 | 44.4 | **4 + 1?** |
| mit **1**, A=10 | 5:06 | 4:23 | 7:16 | 9:54 | 14:22 | **28.5%** | 7 (1.75%) | 191 | 41.4 | 3/5 |

**-9.5 points of band share** (SE of the difference ~3.4, so ~2.8 SE: this one
*is* a result, not noise), and the mechanism is visible in the last two columns —
mitigation 1 pushes 41 more matches *below* 5:00 while removing only one from the
cap. It compresses the distribution downward past the band's floor. F-030 read
the same effect at a quarter of the sample and called it correctly.

### The pentagon, sized properly: 25 seeds, 1 250 matches, 4 holding + 1 undetermined

**Why 25 seeds.** A pentagon cell collects 4 decided matches per seed (two spawn
orientations x the two orderings `WinMatrix` pools into one cell), so the link
sample size *is* a seed count. 25 seeds puts **~100 decided matches per link**,
whose 95% Wilson half-width is about 10 points: enough to call a link that is
really 65% (80% power needs n≈85 for a 15-point deviation from 50%) and enough to
refuse one that is really even. 8 matches — F-030's sample — has a half-width of
28 and can refuse nothing. Sharper than ~±10 gets expensive fast: ±5 needs ~400
matches per link, i.e. 100 seeds and about 4 CPU-hours per reading — **and that
seed count is optimistic: see the clustering correction below, which puts it
nearer 150.** Note also what ~±10 buys and what it does not: it can confirm a
link that is really 65%, but on a link that is really ~55% it returns
*undetermined*, which is exactly what happened to `ravager > sentinel`.

Kept content, `balance --seeds 25 --minutes 15 --only <the five probes>` (as
three shards on seed bases 10/11/12), **1 250 matches**:

| link | rate | n decided | timeouts | 95% Wilson CI | verdict |
|---|---|---|---|---|---|
| sentinel > ripper | **71.7%** | 99 | 1 | [62.2, 79.6] | **holds** |
| ripper > arclight | 93.0% | 100 | 0 | [86.3, 96.6] | holds |
| arclight > bulwark | 100.0% | 88 | 12 | [95.8, 100.0] | holds (survives worst-case censoring — see below) |
| bulwark > ravager | 85.7% | 98 | 2 | [77.4, 91.3] | holds |
| ravager > sentinel | 62.9% *(this sample)* | 97 | 3 | [53.0, 71.8] | **UNDETERMINED — does not reproduce, see below** |

**Four of five hold; `ravager > sentinel` is undetermined.** The four links in the
table above reproduce across independent seed bases and can be stated as results:

| link | this reading | independent re-read | verdict |
|---|---|---|---|
| bulwark > ravager | 85.7% | 82.5% | holds |
| sentinel > ripper | 71.7% | 64.0% | holds |
| ripper > arclight | 93.0% | 93.0% | holds |
| arclight > bulwark | 100.0% | 96.5% | holds (and see the censoring note below) |

**`ravager > sentinel` does not.** Read on five different seed bases it gives
62.5% (n=8), 70.0% (n=30), **62.9% (n=97 — the row above, seed bases 10/11/12)**,
**46.5% (n=99, base 500)** and **52.6% (n=196, base 900; Wilson [45.6, 59.4], and
[43.9, 61.1] once the seed clustering is accounted for — design effect 1.51)**.
**Pooled: 236/430 = 54.9%, 95% CI [50.2, 59.5]** — a band that straddles 50 and
is consistent with a coin flip. One sample reading 62.9% with a CI that excludes
50% is what sampling variation looks like at n≈100; it is not a result.

So the earlier draft of this entry was wrong on three counts, and they are
withdrawn here:

- **withdrawn:** "five of five, every CI excluding 50%". The shipped content reads
  **4 holding + 1 undetermined**;
- **withdrawn:** "the designed counter-pentagon is intact in the shipped content".
  Four of its five links are confirmed; the fifth is unmeasured either way, so
  the *cycle* is not established — a cycle needs all five;
- **withdrawn:** "both links F-030 and F-025 reported broken were sampling
  noise". Only **`sentinel > ripper`** was (it reproduces at 71.7% / 64.0%).
  Nothing here shows `ravager > sentinel` is fine; it shows nobody knows.

**This entry's own 8-seed section had it right** and the 25-seed section
overturned it on a single sample. That section said, of n=32: "the real coin-flip
in the cycle is `ravager > sentinel`, and at n=32 it cannot be called either
way." That was the correct reading, and moving to n=97 did not earn the right to
replace it — **it is the same sampling error F-031 exists to correct in F-030**,
committed one sample size later by this entry. Lesson, bluntly: a CI that
excludes 50% on *one* seed base is a hypothesis, not a finding; reproduce on a
disjoint seed base before writing "holds".

**What settling it would cost.** ±5 points on a link needs **~400 decided matches
per link** — and that is the optimistic count, because the four matches a seed
contributes to a cell are correlated (same map, same seeded RNG stream): the
measured design effect on this link is **1.51**, so a seed's 4 matches are worth
roughly 2.6 independent trials. Budget ~150 seeds, not 100, for a ±5 reading.

**A 100.0% cell with 12% censoring needs one more line to be readable at all**,
and here it is: charge **all 12** capped matches to the predator as losses and
`arclight > bulwark` is still **88/100 = 88.0%, CI [80.2, 93.0]** — a hold under
the worst case the censoring allows. (The critic pinned this as
`the_censored_arclight_bulwark_cell_holds_even_if_every_timeout_is_a_loss`.)
Without that line a one-sided interval on a 100.0% cell says nothing about what
the 12 missing matches could have done.

Length on the same 1 250 matches (the kept content's most reliable arc reading
to date): min 2:27, p25 4:29, **median 6:23**, p75 8:02, p90 11:13, max 14:54,
**band 34.7%** (420/1212), **38 timeouts (3.04%)**, 45.5 units built per match.
(Independent re-reads of band share on this content are **36.5% (n=389)** and
**31.4% (n=1 209)**, so see the range correction in the verdict below.)

That timeout rate is the other correction to F-030, which reported 0 on 100
matches, and it is **worse than this entry first recorded**: at 1 250 matches
**41 timeouts, 3.28%**, concentrated harder than reported — `mass_arclight` vs
`mass_ravager` **18 of 100** and vs `mass_bulwark` **14 of 100**, vs
`mass_sentinel` 5 of 100, `bulwark`-`ravager` 3 of 100, `ravager`-`sentinel`
1 of 100, every other cell 0. The *class* and the diagnosis below (glass-cannon
Arclight against armour) reproduce; the **magnitude is 14-18% of a cell, not
12%**. Every cell is still *readable* (82 decided in the worst), so the matrix is
not holed, but the heavy grind is not gone — it is rarer. **State this as the
AC's unmet remainder:** the 5-8 minute arc is not clean while one sixth of a cell
cannot finish, and it is deferred to B4 with the stat question below. **That residue is the live item, and it is a unit-stat
question**: the Arclight is `offense 9 / defense 2 / armor 2`, a glass cannon that
cannot finish an armoured line before the armoured line's mitigation eats its
damage, so the two sides rebuild forever. It is B4's, exactly as F-025 and F-029
both concluded.

### Verdict: REVERTED. F-030's call was right, and this is now a closed question.

`mvp_combat.mitigation_per_armor` stays at **2**. Nothing in `assets/data/` is
changed by this entry — the revert is the absence of a diff, and
`git diff main -- src benches` still shows `DEFAULT_MATCH_SECS` and its comment
and nothing else.

The three things the completed search establishes, none of which was available
from F-030's single row:

1. **At the shipped threshold the armour change is a measured loss**: 28.5% band
   against 38.0%, on 400 matches a side, ~2.8 SE. It pushes matches *below* the
   band's floor (191 under 5:00 against 150) far faster than it pulls them off
   the cap (7 timeouts against 8).
2. **Where band share is higher, the threshold earned it, not the armour.**
   `attack_at_army` has an interior optimum at 16-18 worth ~9 points of band
   share (38% -> 47-48%), and at that optimum mitigation 1 vs 2 reads -0.9 at
   A=16 and +4.8 at A=18: **+2 ± 2.5 points pooled, i.e. nothing.** Taking the
   armour change to "unlock" the threshold is a misreading of which knob moved.
3. **And the threshold's 9 points are not for sale anyway**: the pentagon
    degrades monotonically along it — 4/5 at A=10, 3/5 at 16, 2/5 at 18 and above
    (and see the caution on that table: only A=18 is at 400 matches; the rest are
    n=8-per-link, so the *monotonicity* is weaker evidence than the endpoints),
   *identically at both mitigation settings* — because at 18 bodies a side the
   cheap swarm runs away with the matrix (every `x vs mass_ripper` cell decides
   at ~4:08 and `sentinel > ripper` inverts to 0.0%). A batch that cannot read
   the counter-pentagon is the instrument B3 exists to build, broken.

So the band-share ceiling of this knob set is real and the content sits near it:
~31-38% of decided probe matches inside 5-8 minutes (readings: 31.4% at n=1 209,
34.7% at n=1 212, 36.5% at n=389, 38.0% at n=392 — an earlier draft's "best
reading ~34.7-38%" quoted only the optimistic half of that spread), with ~39%
*below* 5:00
because the Ripper and Sentinel mirrors decide in 2:29 and 3:38 and no commitment
threshold lengthens them. **Lifting the floor is a unit-stat problem, not a
tempo one** — the same conclusion the armour grind reaches from the other end,
and the same destination: B4.

What F-030 left as an invitation — "one number reverses this" — is withdrawn.
One number does not reverse it: at the shipped threshold mitigation 1 is 9.5
points worse, and at any threshold where it is not worse, it is not better
either.

### One consequence for the suite, flagged not fixed

`b3_pentagon::the_real_batch_reports_what_the_sim_actually_does` fails on this
branch's content (`cargo test --release --test b3_pentagon`: 17 passed, 1
failed — `left: Holds, right: Fails` at `tests/b3_pentagon.rs:477`). It pins
F-025's reading: `holding() == 4` with `bulwark > ravager` **at exactly 0.0%**.
Under the B3.5 content that link is 85.7% over 98 decided matches, so the pin is
a content-driven value change, due for re-measurement with the rest of them.

Two notes for whoever re-pins it, both from this entry:

- **the new value is 4 of 5 links holding, with a different failing link than
  F-025's.** On the test's own batch (`BatchSettings::default()`, seed base 0,
  `.with_seeds(2)`, 100 matches) the shipped content reads `bulwark > ravager`
  **87.5%** — so F-025's failing link now holds — while `sentinel > ripper` lands
  on **exactly 50.0% of 8 decided matches**, which `Verdict` reports as `Fails`.
  `holding()` is therefore still **4**, which is why the failure lands at
  `tests/b3_pentagon.rs:477` (the `bulwark > ravager` verdict) and *not* at line
  474's `assert_eq!(report.holding(), 4)` — that assert passes, and the reported
  failure line is itself the proof that `holding() == 4` on this content. The
  walk table above reads 4/5 for this content too. **Do not re-pin this as 5/5**;
  the 1 250-match reading is 4 holding + 1 undetermined, not 5 holding, and the
  2-seed batch is a different (and under-powered) reading again;
- the test takes its verdict from **2 seeds — 8 decided matches per link — and
  that sample cannot support the word `Fails`** (95% half-width 28 points). It is
  the same under-powered reading that put a wrong `FAILS` in F-030 and (on the
  evidence of the reproducing 64-72% `sentinel > ripper` and 82-86%
  `bulwark > ravager` links above) a wrong one in F-025. **This is the
  recommendation that matters:** if the pin is rewritten, it should either raise
  its seed count or assert the verdict with its interval, so a 50/50-looking cell
  is reported as *undetermined* rather than as a broken design. On the shipped
  content that is exactly the cell the test currently trips over —
  `sentinel > ripper` at 50.0% of 8 — and the link's larger-sample reading
  (64-72%) says the 8-match `Fails` is an artefact, not a design failure.

### Reproducing the numbers

Every row above is `src/bin/balance` in release and nothing else; the batches are
named by their flags, so each is one command. `balance` already prints median,
p25, p75 and max (`Tally::length_quantile`), the timeout count, production totals
and the pentagon table; **band share and p90 are not printed**, and were computed
from the per-match progress lines `balance` writes to stderr
(`[n/total] a vs b seed s [orient] -> result in T ticks (m:ss)`) with a throwaway
script, using `length_quantile`'s own quantile definition (`ceil(q*n)`) and its
own basis (all matches, timeouts included — see Method).

**One cross-check claimed here is withdrawn.** An earlier draft said "the parsed
median reproduces the printed one on every batch". That check is only valid on a
batch with **zero timeouts**, which is why it "worked" on the 100-match walk rows
and could not have worked on the 400- and 1 250-match batches: those have 8-41
capped matches, and on a decided-only basis their medians differ from the printed
ones by 5s to 33s (6:21 vs 6:26 at 400; 5:37 vs 6:10 at 1 250). Treat it as
withdrawn for every batch with a timeout in it. The surviving reproduction check
is that **row A1 reproduces F-030's 30%** band share. A batch split into shards on seed bases
10/11/12 is three such commands; `seed_at` mixes the base, so the shards are
different seed sets rather than overlapping ones. If band share becomes a
standing report rather than a one-off reading, it belongs in `Tally` where it can
be tested — which is a B3 checkbox ("match-length distribution vs the 5-8 minute
target"), not this entry's.

## F-032 — The goldens moved because the data moved, and here is the proof (B3.5 closure, item 4)

**What this entry is.** BALANCE_PLAN's B3.5 box ends with a licence and a
condition: "Because content is data, a RON change moves every pinned per-tick
`state_hash` golden. With **no Rust touched**, any golden that moves is
content-driven by construction — that is the argument that licenses recomputing
them, and **it must be demonstrated, not asserted**." F-029 ran that
demonstration for five B-series binaries at an earlier content state and
deliberately left ~30 goldens un-recomputed. This entry redoes the proof for the
*final* B3.5 content, over the **whole** suite rather than the B-series, and then
recomputes.

### The precondition: the Rust really is identical

`git diff main -- src benches` at `d8f95cd` is exactly two hunks of one file:
`src/headless.rs`'s `DEFAULT_MATCH_SECS` (8 min -> 15 min), its derived
`DEFAULT_TICK_CAP`, and the comment explaining why (the cap decision, F-029).
Nothing in `src/sim/`, nothing in `benches/`. A hash is a function of sim state,
so the only thing on this branch that *can* move one is `assets/data`.

`DEFAULT_MATCH_SECS` itself cannot move a per-tick hash: it is a stopping
condition on the batch runner, not an input to any sim system, and every golden
here is pinned at a tick (300, 600, ...) or a fold over ticks far below either
cap. The suite demonstrates this too — see below: the goldens pass *unchanged*
with the new cap compiled in and the old data loaded.

### The proof, run both ways and in both profiles

`Content` is loaded at runtime from `CARGO_MANIFEST_DIR/assets/data`
(`headless::content`), so the swap is a file copy, not a rebuild — which is also
the only reason this proof is cheap. (`touch`ed after every copy regardless: an
`rsync -a`-restored file can look older than the last build and silently not be
rebuilt. It has produced a false green in this project before.)

| run | binary | `assets/data` | result |
|---|---|---|---|
| P1 | post-change (debug) | **`main`'s** | **839 passed, 5 failed** |
| P2 | post-change (release) | **`main`'s** | **839 passed, 5 failed** — the same five |
| N1 | post-change (debug) | post-change | see the re-pin table below |
| N2 | post-change (release) | post-change | identical to N1 |

**Not one golden is among P1/P2's failures.** All five are assertions this branch
*wrote about the new content*, and each fails holding the old value in its hand:

| test | file | says |
|---|---|---|
| `every_combat_unit_is_massed_by_exactly_one_probe` | `b1_probe_set` | `mass_bulwark` opens 1 barracks, wanted 3 |
| `the_mass_probes_are_knob_identical` | `b1_probe_set` | a mass probe opens 1 production line, wanted 3 |
| `the_five_mass_probes_are_knob_identical_at_attack_at_army_ten` | `critic_b35_armour` | `attack_at_army` is 3, wanted 10 |
| `the_decided_match_median_is_in_the_five_to_eight_minute_band` | `b35_tempo` | median **1:16**, outside the band |
| `the_matches_are_dense_enough_to_be_fights` | `b35_tempo` | **6** combat units a match, under 18 |

So: **old data, old numbers; new data, new numbers; nothing in between.** Every
golden-bearing suite in the tree — not only F-029's five — passes at its old,
unedited pin under the new binary: `b1_matchup`, `b1_strategies`, `b2_headless`,
`b2_orientation`, `b2_production`, `b35_parallel`, `b35_queue_depth`,
`critic_b1_ac2`, `critic_b2_ac1`, `critic_b2_ac4`, `critic_b35_ac0`,
`critic_b35_ac0b`, `critic_m4b`, `critic_m4c`, `critic_m5`, `critic_m6`,
`m5_replay`, `m6_cross_process`, `m6_lockstep`, `p2_log_writer`. And so does
every *budget* the next section re-measures, and `b3_pentagon`'s F-025 pin: under
`main`'s data the pentagon still reads `bulwark > ravager` at 0.0%. That is the
whole licence, and it is now a measurement rather than an argument.

### The re-pin, old → new

Forty-two numbers across nine suites. Recomputed on the box under B3.5's content
(`units.ron` md5 `6c28883759fc7eee792deefcdad223f8`), identical in debug and
release, which is its own determinism check.

**The default matchup at 3 000 ticks** — pinned independently in four files
(`b1_matchup`, `b35_parallel`, `b35_queue_depth`, `critic_b2_ac4` for the state,
`b1_strategies` for seeds 4 and 11):

| seed | state, old → new | journal, old → new |
|---|---|---|
| 4 | `0xa71f64cad50203e9` → `0xbd74941fb3cae489` | `0xe78eebdc5c2ca733` → `0x55675b7844c3d493` |
| 11 | `0x5b398ee4785423dc` → `0x87008a7d696dd45c` | `0x00b7f8d8713fe467` → `0xb27b6a664addcfd7` |
| 23 | `0xf4b57d1c3c3f2af7` → `0x0fe52558759f6817` | `0x46821006f2fae62a` → `0xb86b2ad6d1ed0efa` |

**The bench fixture** (`b2_headless`, two of the ticks re-asserted in
`b2_orientation`):

| tick | old → new |
|---|---|
| 1 | `0x9a74d7adacad19be` → `0x5addc3afc88c09ee` |
| 10 | `0x624e8c15e1922eb5` → `0xe6cf18953a100975` |
| 60 | `0x3f80afb2a96f1736` → `0xf3869a1a5ddc98c6` |
| 120 | `0x545c43c242aa59f0` → `0x8bfd07059b1453a0` |
| 300 | `0xa5b4138cf475fd00` → `0x13edd185edb004f0` |
| 600 | `0x1007e832729309b0` → `0x2b3039ab98739900` |
| fold of all 600 | `0x606003707bc19408` → `0x496070b2de2d41f8` |

**The neutrality pairs** — the same two matches under two names, pinned in
`b35_parallel` (`solo_*`) and `b35_queue_depth` (`depth_*`); both files carried
identical numbers before and carry identical numbers after, which is itself a
check that the two capabilities really are the same fixture:

| matchup | trace, old → new | journal, old → new |
|---|---|---|
| ripper vs bulwark, seed 4 | `0xff87018409ace43e` → `0xbc761268e37bdfa6` | `0x4e1604bd46e099f6` → `0x501ed9cfa3548f9b` |
| bulwark vs ripper, seed 11 | `0xc0a77e736876e285` → `0x9b7ab550477efe7b` | `0x565da2a66530936a` → `0x8bc810501741a3b9` |

**`critic_b35_ac0`'s nine depth-1 rows** (trace over 4 000 ticks, end state,
journal) and **`critic_b35_ac0b`'s eight shipped-matchup pairs** (120-sample
trace over 7 200 ticks, journal) moved in all 27 + 16 values; the new tables are
in the test files, each with the licence recorded at the site.

**The cross-process pin** (`critic_b1_ac2`) is not a literal in a file but a
`target/critic_b1_ac2/pins/*.txt` written by the first process to run the
fixture. Its panic message says to delete it to re-pin, and that is what was
done — the pin re-forms from the new data on the next run, and the assertion
(two processes must agree) is untouched.

## F-033 — Six horizons were numbers, not budgets (B3.5 closure, item 5)

B3.5's slower economy (worker load 10 → 2) stretched everything in time, and six
assertions were holding a horizon that used to be generous and no longer is. The
rule applied to each: **keep what the assertion means, re-derive the number.**

| assertion | old | new | why that number |
|---|---|---|---|
| `b1_probe_set::every_strategy_eventually_attacks` | 12 000 | `DEFAULT_TICK_CAP` (54 000) | "eventually" *means* "inside the match it will be played in". Measured latest committer: `mass_bulwark` at tick 18 750 |
| `b1_probe_set::the_rush_commits_early_and_the_turtle_masses_first` | 12 000 | `DEFAULT_TICK_CAP` | the turtle's first wave is now at 13 470, past the old horizon |
| `critic_b1_ac3::every_strategy_commits_before_the_match_can_stop_it` | 12 000 | `DEFAULT_TICK_CAP` | the assertion is literally about the match cap; derived from it, not from a number that happens to pass |
| `critic_b1_ac3::each_mass_probe_fields_an_army_of_its_own_unit` | 12 000 | `DEFAULT_TICK_CAP` | same, for the same measured 18 750 |
| `critic_b1::a_placement_the_commander_cannot_afford_consumes_no_randomness` | 3 000 | 12 000 | measured: the poor commander (20 starting Alloy) first affords its opening at tick 3 450; the rich one places at 300 |
| `critic_p2::no_configuration_of_the_writer_changes_a_single_tick_of_the_sim` | 4 800 | 12 000 | measured: seed 7 is decided at tick 9 498, and the probe's own vacuity check requires being past the decision |

Measured commitment ticks on the shipped set, solo, seed 4 (first attack / tick
the match ended):

| probe | first attack | over at |
|---|---|---|
| rush | 750 | 4 129 |
| mvp | 4 440 | 8 418 |
| mass_ripper | 7 830 | 9 013 |
| synth_steel_flesh | 10 830 | 12 251 |
| mass_sentinel | 11 460 | 13 099 |
| turtle | 13 470 | 14 531 |
| mass_arclight | 14 130 | 16 022 |
| synth_triad | 14 670 | 16 127 |
| mass_ravager | 15 060 | 17 079 |
| mass_bulwark | 18 750 | 23 319 |

Five more fixtures were anchored to *content values* rather than horizons, and
are now read from the content instead of pinned, so the next re-tune cannot turn
a probe into a silent no-op: the worker's carry capacity (`m4a_economy`'s loader
mutation anchor, `critic_m4a`'s "room for less than one load", `critic_m4b`'s
two carried-load assertions), the building HP scale (`critic_m4c`, 40 → 420) and
the time one Ripper needs to level an HQ (`m4c_ai`, three loops that pinned
3 000 ticks against a pool that grew ten-fold — now a `kill_budget(content,
attacker, building)` derived from HP, damage, mitigation and attack period).

## F-034 — A coin flip is not a broken counter: the pentagon needed a third verdict (B3.5 closure)

`PentagonReport` had two readings for a link with data: `rate > 0.5` was `Holds`,
anything else `Fails`. `b3_pentagon::the_real_batch_reports_what_the_sim_actually
_does` reads **eight** decided matches per link (5 x 5 probes x 2 seeds x 2
orientations, of which 8 land on each pentagon link). Eight matches put roughly
**±28 points** of two-sided 95% interval around a rate. So that test was
reporting, as *broken design*, links whose data cannot distinguish 45% from 55%
— and F-031 had just measured one of them (`ravager > sentinel`, pooled 54.9%,
CI [50.2, 59.5] over 430 matches) as a genuine coin flip.

The fix is a third verdict, not a bigger batch:

| verdict | means |
|---|---|
| `Holds` | the whole interval is above a half — the counter resolves |
| `Fails` | the whole interval is below a half — the counter is backwards |
| `Undetermined` | the interval straddles a half — **the sample cannot call it** |
| `Undefined` | no decided matches (every match timed out) — unchanged |
| `NoStrategy` | the unit has no mass probe — unchanged |

`Undetermined` and `Undefined` are deliberately distinct: "we measured and it is
too close to call" is not "we have no measurement". The interval is Wilson's
score interval at z = 1.959963985, on `Cell::n_decided` (timeouts are not
sample, per F-024), and `Cell::wilson_interval` is pinned in `src/metrics.rs`
against the eight intervals F-031 quotes.

What this changed in the reading of the shipped pentagon at eight matches a link:

| link | decided | rate | 95% interval | verdict |
|---|---|---|---|---|
| arclight > bulwark | 8 | 100.0% | [67.6, 100.0] | holds |
| ripper > arclight | 8 | 100.0% | [67.6, 100.0] | holds |
| bulwark > ravager | 8 | 87.5% | [52.9, 97.8] | holds |
| ravager > sentinel | 8 | 62.5% | [30.6, 86.3] | undetermined |
| sentinel > ripper | 8 | 50.0% | [21.5, 78.5] | undetermined |

**Three hold, two are undetermined, and nothing fails** — the first reading in
the project's history with no link called broken. F-025's `bulwark > ravager` at
0.0% is now the pentagon's strongest resolved hold at this sample size; the two
undetermined cells are undetermined for two different reasons. `ravager >
sentinel` is genuinely close: F-031's 430-match run puts it at 54.9%, CI [50.2,
59.5]. `sentinel > ripper` is **not** close: F-031 and the critic reproduced it
at 64.0% and 71.7% on ~100 matches each. It reads undetermined here only because
n = 8 cannot call anything short of a near-sweep (a 6/8 still has a lower bound
under 50%), not because the link is in doubt. The old test
would have re-pinned them as 5/5 holding, which would have been a *stronger*
claim than the data supports in the same breath as deleting a true one.

Blast radius, handled deliberately rather than by loosening: `critic_b3_ac2`'s
`exactly_half_fails_and_a_hair_above_half_holds` pinned the old rule by name and
is now `every_way_of_being_even_reads_undetermined_not_failed`; several synthetic
fixtures used n = 4 (interval ±35 points) and were scaled x10 so each test's
original intent survives the new rule; and a new test pins the difference head
on — the same 75% rate reads `Undetermined` at n = 4 and `Holds` at n = 40.
