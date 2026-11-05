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
| `b1_probe_set::every_strategy_places_its_barracks_and_builds_its_own_order` | 6 000 | `DEFAULT_TICK_CAP` | an opening not up by the cap goes up in no match that will be played. `MIN_TRAINED = 2` kept: a floor against a token unit, not a rate. **Still red at the cap** (`mass_bulwark` places 1 of 3 foundries): not a horizon problem, see F-035 |
| `critic_b1_ac3::every_mass_versus_mass_cell_resolves_in_both_orientations` | 20 000 | `DEFAULT_TICK_CAP` | the test defines a hole as a cell that *times out*, and timeout means the match cap. Measured: the `mass_bulwark` mirror on seed 7 now decides near tick 23 500, past the old horizon; all 25 cells resolve inside the cap on seed 7 (release). One seed only: F-031 measures 14-18% of `mass_arclight` vs armoured matches reaching the cap across seeds, so this green is a sample, not a proof that no hole exists |

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

*Superseded for the turtle by F-036: at `attack_at_army` 26 the turtle first attacks at 19 320, after mass_bulwark.*

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

## F-035 — The extra openings never go up: B3.5's economy starves the tech step (B3.5 closure, closed)

**Status: closed — option 3, "scripts match reality"; see *Resolution* at the
end of this entry, which also corrects the solo measurement below (it missed
`mass_ripper`'s second Gene-Vats).** Gating test:
`b1_probe_set::every_strategy_places_its_barracks_and_builds_its_own_order`,
which F-033 moved to the match cap and which is still red there. Raising the
horizon cannot fix it, and the assertion was not weakened.

Measured solo, seed 4, release, 54 000 ticks (the match is over well before
the cap, and nothing is placed after it):

| probe | openings placed | over at | trained | opening cost | unit cost(s) |
|---|---|---|---|---|---|
| mvp | 1/1 | 8 418 | 6 | 150 | 70, 110 |
| rush | 1/1 | 4 129 | 6 | 150 | 40 |
| synth_triad | 4/4 (last at 7 740) | 16 127 | 12 | 150, 150, 200, 150 | 70, 40, 80 |
| turtle | **3/4** | 14 531 | 18 | 150 x4 | 70, 40, 110 |
| synth_steel_flesh | **2/4** | 12 251 | 12 | 150 x4 | 70, 40, 110 |
| mass_ripper | **1/3** | 9 013 | 12 | 150 x3 | 40 |
| mass_sentinel | **1/3** | 13 099 | 12 | 150 x3 | 70 |
| mass_arclight | **1/3** | 16 022 | 12 | 200 x3 | 80 |
| mass_ravager | **1/3** | 17 079 | 12 | 150 x3 | 90 |
| mass_bulwark | **1/3** | 23 319 | 13 | 150 x3 | 110 |

Mechanism, from `src/sim/ai.rs`: each decision runs tech before army, and an
opening is placed only when `budget >= cost`. Every unit in these build orders
is cheaper than the opening. Once the first barracks is up, the army step
spends the stockpile whenever it reaches the next unit's price and the line is
free. At B3.5's worker load (2) the income never lets the stockpile climb from
a unit's price to 150-200 between two army spends, so the second and third
openings are never affordable. This follows from the outcome itself: tech runs
first, so any decision past an opening's `at_tick` with the stockpile at its
cost would have placed it. The measured peak stockpile (296-302) is the
starting stockpile; the probe did not record the post-placement peak
separately.

Consequences:
- F-030's "three production lines" for the mass probes **do not happen** in
  the shipped content. Every mass probe plays on one barracks.
  `MASS_PROBE_BARRACKS = 3` and the knob-identity tests still pass, because
  they read the scripts and not the realised openings.
- The probes are still mutually comparable (F-018): all five realise the same
  one opening.
- The B3.5 pentagon reading (F-034) was taken on these realised one-barracks
  armies, so it is a reading of what the sim does, not of what the scripts
  intend.

Options, none taken (content is final for this closure, and the AI priority is
a design call):
1. Content: make the openings affordable, for example with cheaper extra
   barracks, later `at_tick`s paired with a reserve, or a different income.
2. AI: have the army step reserve the next due opening's cost (`tick >=
   at_tick` and not yet standing) before it trains. This changes every
   strategy with more than one opening and moves goldens.
3. Spec: accept one realised barracks, drop the duplicate openings from the
   mass probes and set `MASS_PROBE_BARRACKS` back to 1. This changes the
   F-030 rationale.

### Resolution (closed): the scripts list what the sim places

**Decision (the user's): option 3.** Every opening that never goes up is removed
from `strategies.ron`, so each script lists exactly the placements the sim
makes, in the order it makes them. Multi-barracks as a capability (the army step
reserving the Alloy of a due opening) is deferred to B4.

**Correction to the table above: `mass_ripper` does place a second barracks.**
The solo fixture ends the match the moment the probe levels the inert enemy HQ —
`mass_ripper` at tick 9 013 — and the sim stops thinking once a match is
decided, so "measured to 54 000 ticks" above really meant "to the end of the solo
match". In head-to-head play the Ripper's second Gene-Vats goes up at **tick
9 330**, on every seed and orientation measured. The Ripper costs 40 Alloy, the
cheapest body in the game, so it is the one probe whose income outruns its
spending and whose stockpile climbs to 150. No probe ever placed a third
opening. This was found by the proof obligation, not by inspection: the first
trim also cut the Ripper's second Gene-Vats, and
`b3_pentagon::the_real_batch_reports_what_the_sim_actually_does`'s pinned
`sentinel > ripper` moved 0.50 -> 0.75. That trim was not committed.

**What the B3.5 measurements were taken on, therefore:** the Ripper readings
(F-029 to F-034, every cell with `mass_ripper` in it) were taken on a
**two-line** Ripper army from tick 9 330 onward; the other four mass probes on
**one** barracks. Neither changes under this closure: the scripts now describe
exactly those armies, so the measurements stand as readings of the shipped
content.

**The comparison (release, same binary, old `strategies.ron` vs the first trim
that cut every probe to one opening).** Each match played under both data sets,
diffed on length, winner and the full placement list of both sides:

| roster | seeds | matches | differ | of which winner flips |
|---|---|---|---|---|
| the five `mass_*` probes (the `b3_pentagon` batch: `seed_at(0, k)`, k = 0, 1, both orientations) | 2 | 100 | **32** — every non-mirror match with `mass_ripper` in it; the other 68 are identical | 7 |
| `mvp`, `mass_bulwark`, `mass_arclight`, `synth_steel_flesh`, `synth_triad`, `rush`, `turtle` (all ordered pairings, both orientations) | 1 (`seed_at(0, 0)`) | 98 | **0** | 0 |

The Ripper mirror does not differ because it is decided before tick 9 330. In
the second row the trimmed `turtle` and `synth_steel_flesh` openings, the dropped
second and third openings of `mass_bulwark` and `mass_arclight`, and the
`synth_triad` reorder change nothing.

**What shipped (`c68a4c9`):**

| strategy | openings before | openings after | note |
|---|---|---|---|
| `mass_bulwark`, `mass_sentinel`, `mass_ravager`, `mass_arclight` | 3 | 1 | |
| `mass_ripper` | 3 | **2** | the second Gene-Vats (at_tick 600, offset 165) is real; the third is not |
| `synth_steel_flesh` | 4 | 2 | Foundry, Gene-Vats |
| `synth_triad` | 4 | 4 | **reordered** to placement order: the second Gene-Vats (150, placed ~5 190) before the Aether Spire (200, placed ~7 740). Each entry keeps its own at_tick and offset, so the k-th placement of a building gets the same offset as before |
| `turtle` | 4 | 3 | Foundry, Gene-Vats, Foundry |
| `mvp`, `rush` | 1 | 1 | unchanged |

**The golden-unchanged proof.** `cargo test --release --no-fail-fast` on the
box with the shipped trim and no Rust change: **847 passed, 1 failed**. The one
failure is `critic_b1_ac3::the_rush_is_the_earliest_and_the_turtle_the_latest_of_the_whole_set`,
which F-036 handles and which reads no golden. Every per-tick `state_hash`
golden, every journal pin and the `b3_pentagon` rate pins pass at their old
values; the gating test is green.

**Test changes, none of them loosening:**
- F-018's knob identity (`b1_probe_set::the_mass_probes_are_knob_identical`,
  `critic_b1_ac3::the_mass_probes_are_identical_in_every_field_of_the_struct`,
  `critic_b35_armour::the_five_mass_probes_are_knob_identical_at_attack_at_army_ten`):
  the opening **count** may differ, and only for `mass_ripper`, named
  (`b1_probe_set::MASS_PROBE_OPENING_EXCEPTIONS`). Every opening the probes
  share is still compared tick for tick and offset for offset, and every other
  field is still asserted identical; the struct-wide `Debug` comparison cuts
  each probe to its shared first opening and compares everything else. Each
  site says B4's opening reservation should make the five identical again.
- `MASS_PROBE_BARRACKS` is 1 again.
- The gating test plays **unopposed** (no enemy HQ, so the match cannot end)
  and asserts the match is still running after `DEFAULT_TICK_CAP` ticks.
  Against the inert HQ every strategy ended its match long before the cap
  (`mass_ripper` at 9 013), so "by the match cap" was never what it tested, and
  the Ripper's real second opening could not appear in it.

## F-036 — The turtle was no longer the late pole; `attack_at_army` 15 -> 26 (B3.5 closure)

**Gating test:** `critic_b1_ac3::the_rush_is_the_earliest_and_the_turtle_the_latest_of_the_whole_set`
(B1's design intent: the rush commits earliest, the turtle latest). Red since
B3.5's tempo pass: the mass probes' `attack_at_army` went 3 -> 10 (F-030), and at
B3.5's income the turtle's 15 no longer kept it behind them.

**Measured first-attack ticks** (solo, release, 54 000-tick budget). They are
identical on seeds 1, 2, 3, 4, 5, 7, 11 and 23 (the seed only turns the
placement direction) and identical before and after F-035's trim:

| strategy | first attack (tick) |
|---|---|
| rush | 750 |
| mvp | 4 440 |
| mass_ripper | 7 830 |
| synth_steel_flesh | 10 830 |
| mass_sentinel | 11 460 |
| **turtle @ 15** | **13 470** |
| mass_arclight | 14 130 |
| synth_triad | 14 670 |
| mass_ravager | 15 060 |
| mass_bulwark | 18 750 — the slowest non-turtle |

**The turtle's commit tick against its threshold** (seeds 4 and 7, identical):

| `attack_at_army` | first attack (tick) | force | margin over 18 750 |
|---|---|---|---|
| 16 | 14 760 | 16 | short |
| 18 | 14 910 | 18 | short |
| 20 | 16 950 | 21 | short |
| 22 | 17 130 | 22 | short by 1 620 |
| 24 | 19 140 | 25 | +390 ticks (6.5 s) |
| **26** | **19 320** | 26 | **+570 ticks (9.5 s)** |

23 and 25 were not measured. The guess of ~22 was not enough; 24 clears by only
390 ticks, so **26** is the value shipped (the user's choice). One RON number;
no Rust change.

**Goldens: none moved, so none were recomputed.** `cargo test --release
--no-fail-fast` with `attack_at_army: 26`: **848 passed, 0 failed**. The gating
test is green, and every per-tick `state_hash` golden, journal pin and
`b3_pentagon` pin passes at its existing value: no pinned fixture plays the
turtle past its first wave. The F-032 licence was therefore not needed. Its
precondition holds anyway: no Rust changed, and the same binary with
`attack_at_army: 15` passes the same goldens (F-035's 847 / 1 run, whose one
failure is this test).

## F-037 — Production totals: the schema is the union of the records' headers (B3)

**Wall.** `batch::production_totals` took its column header from
`records.first()` and read every later record through it (noted, not fixed, in
F-024). Two silent losses followed. A first record carrying an **unlabelled**
block (`ProductionCounts::default()` — what every synthetic record carries)
gave an empty header, so the whole batch totalled *nothing*: measured on a
played `rush` vs `mass_ripper` record behind an unlabelled one, the totals were
`[]` instead of `worker 2, ripper 3, ...`. And any unit a later record named but
the first did not was dropped from the table.

**The two options, and the one taken.** The AC offered: derive the schema from
the union of the record keys, or refuse an unlabelled record.
**Union**, for three reasons:

- **It is lossless.** `ProductionCounts`'s fields are private and its only
  constructors are `of` and `zeroed` (both fully labelled) and `Default` (empty
  header, empty counts). An unlabelled block therefore *cannot* carry a count, so
  skipping it drops nothing — there is no information a refusal would protect.
- **Refusing would turn a fact into an error.** Synthetic records are
  unlabelled by design across the B2/B3 suites, and
  `critic_b2_ac4::probe_an_unlabelled_row_contributes_zero_not_a_misaligned_column`
  already pins that an unlabelled row contributes zero next to a played one; a
  `Result` would weaken that pinned behaviour into a refusal.
- **It also closes the second hole**, which a refusal would not: records from
  different rosters are summed by name with every name kept.

**Rule.** Columns appear in first-appearance order — record order, then that
record's header order — by a linear scan, never a map, so the output is a
function of the records alone. For a `run_batch` result every record has the
same header, so the order is still content (RON) order. Summing goes through
`by_unit`, i.e. by name, so two headers can never be added misaligned. A batch
with no labelled record totals nothing (no roster to name).

**Gating test:** `tests/b3_totals.rs` —
`an_unlabelled_first_record_no_longer_drops_the_batch` (red before: `left: []`)
and `the_schema_is_the_union_of_every_records_unit_ids` (red before: `phantom`
missing). No sim change, no RON change, no golden moved.

## F-038 — The kill gate: thresholds, the clustered interval, and the seed-count power calculation (B3)

**What is gated.** `gate::KillGate` reads a batch against DESIGN_BRIEF's kill
criteria as BALANCE_PLAN states them for B3. DESIGN_BRIEF's own line is still
an *example* placeholder (`<e.g., "if any unit wins >65% regardless of
counter, the pentagon is broken">`), so the thresholds below are this
finding's choice, stated once in `GateSpec::default()` and pinned by
`b3_gate::the_default_thresholds_are_the_stated_ones`. They are harness
configuration, not content.

| criterion | reading(s) | bar |
|---|---|---|
| K1 strength | each strategy's row mean (mean over opponents, mirror excluded) | at most 65% |
| K2 seat bias | pooled slot-A share of decided mirrors; pooled left-base share; the same two shares for each mirror | 50% +/- 5 points, every one |
| K3 termination | band share (in-band / decided) | at least 50% |
| | timeout rate (timeouts / all) | at most 5% |
| | *reported, not gated:* share of all ending before 5:00; share ending after 8:00 or timing out; decided median | — |

**The interval decides, as for the pentagon (F-034).** Every reading carries a
95% Wilson interval (the same `z` and expression as `Cell::wilson_interval`,
now shared as `metrics::wilson_bounds`). PASS needs the whole interval on the
right side of the bar, FAIL the whole interval on the wrong side, and anything
else, including no data, is **undetermined**. A criterion passes only if every
one of its readings passes. One FAIL fails it. The gate's status combines the
three criteria the same way. An all-timeout run therefore FAILs K3 (100%
timeouts; its band share is undetermined, nothing was decided) and leaves K1/K2 undetermined, and it carries an
`all_timeout` flag. It can never read as balanced.

**Clustering.** One `(pair, seed)` cluster is up to 4 matches (2 slot orders x
2 orientations; 2 for a mirror) on one map with one set of seeded streams.
F-031 measured deff 1.51 at m = 4, so ICC rho = 0.51/3 = **0.17**
(`gate::F031_ICC`). Each reading counts its own distinct clusters (unordered
pair + seed; no map) and uses deff = 1 + (m - 1) rho at its mean cluster size
m, then takes Wilson on n_eff = n / deff. That gives ~1.40 for K3 on a mixed
batch. **Mirror readings are the exception** *(corrected after the B3 critic)*:
rho = 0.17 gives a 2-match mirror cluster deff 1.17, but measured on F-039's
62-seed mirror-only batch (1 235 decided) the slot-A share has:
- deff **1.381** with the **seed** as the cluster (robust cluster variance over
  binomial variance, 62 clusters);
- deff 1.344 with `(strategy, seed)` as the cluster (620 clusters), i.e. a
  mirror ICC of ~0.34, twice F-031's;
- deff 1.08 with the strategy as the cluster (10 clusters).

The left-base share measures 0.93 by seed, 0.66 by `(strategy, seed)` and
2.17 by strategy. The last is on only 10 clusters, so it is noise-dominated,
and the per-mirror readings test strategy-level base effects directly.
K2 uses the conservative seed-clustered value: every seat reading's deff is
at least `gate::MIRROR_DEFF` = 1.38 (`GateSpec::mirror_design_effect`;
`b3_gate::mirror_readings_use_the_measured_seed_clustered_design_effect`). A row mean (K1) is a mean of
k cells, with variance sum(p_i(1-p_i) w_i) / k^2, where w_i = deff_i / n_i.

*(Corrected after the B3 critic.)* The first version took Wilson at the mean
on n_eff = k^2 / sum(w_i), i.e. variance p(1-p) sum(w_i) / k^2, and called it
conservative. That holds for equal cells only. With unequal w_i, the true
variance exceeds it when the near-50% cells are the small ones. For cells of
10 at 50% and 1 000 at 100%, its
interval was narrower than the normal interval on the true variance
(`critic_b3_gate::probe_the_row_mean_interval_is_at_least_as_wide_as_the_true_one`).

The fix uses V(mu), the **largest** variance of a mean of these k cells over
all cell rates averaging mu. It is a concave maximisation, solved by KKT
water-filling, p_i = clamp((1 - lambda / w_i) / 2, 0, 1) (`gate::max_mean_variance`).
The interval is the hull of two parts:
- **the normal interval mean +/- z sqrt(V(mean)).** It contains the normal
  interval on the true variance, whatever the cell rates, because the true
  rates average `mean`;
- **the score interval {mu : |mean - mu| <= z sqrt(V(mu))}.** This is Wilson's
  construction on the worst case. It keeps the interval defined at 0% and 100%.

With equal cells, both parts are closed form: Wilson and the normal interval
at n_eff. For k = 1 the row interval therefore contains the cell's Wilson
interval rather than equalling it.

Pinned by:
- `b3_gate::the_row_mean_interval_contains_the_normal_interval_on_the_true_variance`
  (seven cell configurations, sizes 2 to 1 000);
- `b3_gate::a_row_mean_over_one_cell_contains_that_cells_wilson_and_normal_intervals`.

The cost is width. Near 50% at small n the normal part is a little wider than
Wilson's (about 2 points at n_eff ~20). A borderline row therefore reads
undetermined a little more often, never PASS more often.

**Names, not just a status.** K1 lists the strategies whose row is resolved
above 65% (`failing`), the ones whose every opponent cell is resolved above
50% (`dominant`), and the strictly weak ones (`losing`), all in matrix order.
Each row also carries the unit it masses (`pentagon::mass_strategy`), so
"unit win rate" reads off the probe row.

**`losing` is the mirror image of K1's bar.** *(Corrected after the B3
critic; the first version named a strategy `losing` only if every one of its
cells was resolved below 50%, so a strategy that loses every matchup but splits
one too few matches to resolve it went unnamed —
`critic_b3_gate::probe_a_strategy_losing_every_matchup_is_named`, where `low`
reads 12.5% with its interval topping out at 28.1%.)* A strategy is named `losing` when either:
- its row interval lies wholly below `1 − max_strength` (35%), the same
  interval read against the symmetric bar; or
- every opponent cell is resolved below 50% (the old rule, kept).

It is **named, not gated.** The spec's K1 is "no unit/strategy wins >65%
regardless of counter": it bounds strength from above only, and asks for
weak strategies to be *surfaced by name*, not to fail the gate. A strategy too
weak to use is a balance problem B4 owns, but not a kill criterion as written;
gating it would add a criterion the plan does not have. The table tags the row
`LOSING` and prints `losing (named, not gated): ...`; the RON carries
`strength.losing`. The bar moves with `max_strength`. Tests:
`b3_gate::a_row_resolved_below_the_mirror_bar_is_named_losing_but_not_gated`,
`b3_report::a_strictly_weak_strategy_is_tagged_in_the_table_and_named_in_the_report`.

**K2: every reading, pooled and per mirror, must PASS.** *(Corrected twice
after the B3 critics. The first version gated only the pool, so two mirrors
with opposite, fully resolved seat edges pooled to 50% and PASSed —
`critic_b3_gate::probe_k2_does_not_pass_when_one_mirror_is_resolved_outside_tolerance`.
The second let a resolved per-mirror FAIL fail K2 but let the pool alone decide
PASS, ignoring a mirror that was merely open. That broke this module's own rule
("a criterion PASSes only if every one of its readings does") and "too little
data never reads PASS": a mirror won by slot A 6 of 6, or one reading 64.5%
with an interval excluding 50%, sat under a K2 PASS —
`critic_b3_gate2::k2_does_not_pass_a_mirror_known_from_six_matches_all_to_slot_a`,
`critic_b3_gate2::k2_does_not_pass_while_a_mirror_reads_64_percent_left_with_an_interval_excluding_50`.
The rule below is the user's decision.)* K2's readings are slot A and left
base, pooled and for each mirror, and K2 is `Status::all` of them:
- **FAIL** if the pool or any mirror resolves outside 50% +/- 5 (its whole
  interval outside tolerance: the data says that seat is biased).
- **PASS** only if the pool **and every mirror** resolve inside tolerance.
- **Undetermined** otherwise, in particular whenever any mirror is open.

Multiplicity still explains *why* an open mirror is common, and why it is not
a FAIL: per mirror, a full-roster seed gives 2 matches, so ten mirrors are
each read on a tenth of the pool. For a truly fair mirror a FAIL needs a
> 1.96-SE excursion *past* a 5-point margin, so twenty such readings carry a
family-wise false-FAIL rate below 20 x 2.5% and, at the sizes run here, far
below it. What multiplicity no longer does is license a PASS: an open mirror
holds K2 undetermined, and the price is the per-mirror batch sized below
(`b3_gate::an_open_mirror_row_holds_k2_undetermined_until_it_resolves_fair`,
`b3_gate::a_resolved_per_mirror_fail_fails_seat_bias_even_when_the_pool_is_fair`).

**Why K3 is the band share.** *(Corrected after the B3 critic; the first
version gated the median and made band share advisory, which was a moved
goalpost.)* "Matches terminate in target" is a statement about matches, not
about the middle one. The critic's fixture splits 47% short / 6% in band / 47%
long: its median sits in the gap and the median-only K3 read PASS with 94% of
matches outside 5–8 min (`critic_b3_gate::probe_k3_does_not_pass_when_six_percent_of_matches_are_in_target`).
Band share (in-band / decided) is the metric B3.5 itself defined as the design
metric; the timeout rate is its stalemate signal. So K3 gates exactly those
two: band share at least 50% with its clustered Wilson interval, and timeouts at
most 5%. The decided median and the before/after shares are still read with
intervals and printed as context, never folded into the status
(`b3_gate::the_band_share_gates_k3_even_when_the_median_is_in_band`). On
today's knobs band share is ~36–44% (F-031, F-040), so **shipped K3 reads
FAIL**. That is the honest reading of a problem B3.5 deferred to B4, not a
reason to lower the bar. There is no switch to make band share advisory.

### The power calculation

*(Corrected after the B3 critic: the first version used the mirror deff 1.17
from F-031's ICC. It is redone here at the measured 1.38.)*

Seat bias is a proportion against 0.5. With alpha = 0.05 two-sided and power
0.8 (z = 1.960, 0.842), detecting a true share of 0.5 + delta needs
n_eff = ((1.960 x 0.5 + 0.842 x sqrt(p1(1 - p1))) / delta)^2 decided mirror
matches. The decided count is n = n_eff x **1.38** (measured mirror deff, above). A
full-roster seed (10 strategies) plays **20 mirror matches**: 10 mirrors x 2
orientations, i.e. 2 decided matches per mirror per seed, minus timeouts.

| seat bias to detect | n_eff | decided mirrors | full-roster seeds | full-roster matches |
|---|---|---|---|---|
| 10 points | 194 | 268 | 14 | 2 800 |
| 7 points | 398 | 550 | 28 | 5 600 |
| **5 points** | **783** | **1 080** | **54** | **10 800** |
| 3 points | 2 178 | 3 006 | 151 | 30 200 |
| 2 points | 4 903 | 6 767 | 339 | 67 800 |

**PASSing K2 is harder than detecting a bias.** Even when the true share is
exactly 0.5, the whole 95% interval must fit inside +/-5 points. That needs
half-width <= 5 with 80% probability, so n_eff >= ((1.960 + 1.282) x 0.5 /
0.05)^2 = **1 051**. That is **1 451** decided mirrors, or **73 full-roster
seeds** (14 600 matches).

**What the 62-seed mirror batch buys** (F-039 section 2: 1 235 decided, so
n_eff = 1 235 / 1.38 = 895). Two different numbers:
- **The z-test detectable bias is ~4.7 points.** A two-sided test at alpha
  0.05 rejects 50% with 80% power when the true share is 50% +/- 4.7.
- **The gate itself FAILs with 80% power only at ~9.6 points.** K2 FAILs when
  the whole Wilson interval lies outside 45–55%, i.e. at an observed share
  >= 58.3% at this n_eff. The true share that reaches it 80% of the time is
  59.6%, a 9.6-point bias (the critic's normal-approximation figure is ~9.3).
  Between ~5 and ~9.6 points the gate reads **undetermined**, never PASS.
  That is the property that matters: a bias the batch cannot resolve does not
  pass.

At 62 seeds the **pool** PASSes: slot A 50.9% [47.6, 54.1], left base 51.0%
[47.7, 54.3], and no mirror is resolved outside tolerance. **K2 reads
undetermined**, because every per-mirror reading is still open, the closest
being `mass_arclight`'s left base at 64.9% [54.6, 74.0] (F-039 section 2).

**What settling K2 per mirror costs** (same framework as the pooled PASS
size, per reading, at deff 1.38 and 2 decided matches per mirror per seed;
fair seats assumed):

| target | n_eff per mirror | decided per mirror | mirror-only seeds | mirror-only matches | CPU-hours at 3.45 s |
|---|---|---|---|---|---|
| one mirror's reading PASSes +/-5 with 80% probability | 1 051 | 1 451 | 726 | 14 520 | ~14 |
| all 20 readings PASS jointly with 80% probability (each at 98.9%) | 2 025 | 2 795 | 1 398 | 27 960 | ~27 |

The joint row multiplies the 20 readings' probabilities as if independent.
They are not (slot A and left base share matches), but within a mirror the two
are uncorrelated when orientations are balanced, and for any correlation among
roughly normal readings Šidák's inequality makes the product a lower bound on
the joint probability, so the seed count errs high. Match counts are seeds × 20
(10 mirrors × 2 orderings).

Settling K2 as PASS therefore takes a **~1 400-seed mirror-only batch,
~28 000 matches**, roughly 23x the 62-seed batch. It was not run. A
resolution the other way is far cheaper: if `mass_arclight`'s left base is
truly ~65%, its lower bound clears 55% with 80% power at n_eff ~183, i.e.
~252 decided or ~126 seeds of that one mirror (it has 62), about 130 more
matches. That re-read is the cheap next step, and it would most likely turn
K2 into a FAIL rather than a PASS.

**Throughput, measured on the box** (release, `nice 19`, shared with the trading
agents): the B3 pentagon batch played 100 matches serially in 332 s, i.e.
**3.3 s a match**. That makes 62 full-roster seeds ~11.4 CPU-hours, and 128
seeds (3 points) ~23.5. On a box that runs live trading, neither is a modest
batch. **So the full-roster size is infeasible here, and the K2 question is
sized separately.** K2 reads only mirrors, so a **mirror-only** batch spends
every match on it. 62 seeds x 10 mirrors x 2 orientations = **1 240 matches**
(~1.1 CPU-hours) was sized for the n that can PASS the **pool** +/-5 points at
deff 1.17. At the measured 1.38 that needs 73 seeds; 62 gave a pooled PASS
anyway, because the observed shares sit near 51% (above). It cannot settle K2,
which now needs every mirror resolved (above). It detects a ~4.7-point bias at 80% power
(z-test). A 3-point bias stays out of reach (3 006 decided mirrors, ~2.9
CPU-hours). It is the next step if the 62-seed reading is ever close.
K1 and K3 are read on roster batches. A row mean over k opponents at s seeds
has n_eff ~ k x 4s / 1.51, so its half-width is about 0.98 / sqrt(n_eff). On
the five probes (k = 4) that is ~+/-20 points at 2 seeds and ~+/-13 at 5. On
the full roster (k = 9) it is ~+/-9 at 5 seeds. A row has to sit that far past
65% to FAIL. F-039 states the size behind each reading.

**Gating tests:** `tests/b3_gate.rs` covers 24 tests: the rules, the design
effect, PASS reachable, each criterion failing alone, K2 by slot and by base,
names, all-timeout, determinism under reordering, the injected imbalance and
the shipped pin (F-039).

## F-039 — What the kill gate says about the shipped content (B3)

No RON changed for any of this. These are measurements, read on two batches,
each sized for what it can decide.

### 1. The B3 pentagon batch: five mass probes, 2 seeds (100 matches) — undetermined

This is `b3_pentagon`'s real batch, pinned in
`b3_gate::the_shipped_reading_on_the_pentagon_batch`.

| criterion | reading | status |
|---|---|---|
| K1 `mass_bulwark` | 37.5% [20.4, 58.5] over 4 opponents, n 32, n_eff 21 | PASS |
| K1 `mass_sentinel` | 65.6% [44.5, 82.0] | undetermined |
| K1 `mass_ripper` | 68.8% [47.5, 84.3] | undetermined |
| K1 `mass_ravager` | 46.9% [27.8, 66.9] | undetermined |
| K1 `mass_arclight` | 31.3% [15.7, 52.5] | PASS |
| K2 slot A | 65.0% [41.6, 82.9] of 20 decided mirrors | undetermined |
| K2 left base | 45.0% [24.6, 67.2] | undetermined |
| K3 ends before 5:00 | 34% [24.1, 45.5] of 100 | PASS |
| K3 ends after 8:00 or times out | 30% [20.6, 41.4] | PASS |
| K3 timeouts | 0 of 100, [0, 5.09] | undetermined |
| K3 band share | 36% [25.9, 47.6] of decided | **FAIL** vs 50% |
| decided median | 6:36 | |

**Gate: FAIL** *(corrected after the B3 critic: this read "undetermined" while
K3 gated the median; the before/after rows above are now reported context)*.
K3 fails on band share, resolved below 50%. Nothing else fails. On these probes
the median match is inside 5–8 minutes, but most matches are not. The rest is a
statement about 100 matches:
- The two strongest probes (Sentinel 65.6%, Ripper 68.8%) sit on the 65% bar
  with ~+/-19 points of interval.
- Zero timeouts in 100 cannot certify a rate below 5%, because the upper bound
  is 5.09%.
- The 13/20 slot-A reading is noise at n_eff 17.

The band share reproduces F-031's ~31–38% ceiling. K3 now gates on it, so
the ceiling FAILs K3. Raising it is B4's job (F-038).

### 2. A mirror-only seat-bias batch: every strategy, 62 seeds (1 240 matches) — K2 undetermined

K2 is the one criterion F-038's power calculation sizes, so this run spent
every match on it:
- Each of the 10 strategies plays its own mirror in both orientations on
  `seed_at(0, k)` for k < 62, at the shipped 15-min cap.
- It is a scratch harness, not committed: `run_match` in 3 threads, records
  re-sorted into seed/roster/orientation order, then `KillGate::of`.
- Wall time was 1 425 s, i.e. **3.45 CPU-s a match**, consistent with F-038's
  3.3.

*(Re-read after the B3 critic with the corrected gate: deff 1.38 and
per-mirror FAIL gating, F-038. The batch was replayed and its outcomes are
identical, match for match. The first version's numbers were slot A [47.8,
53.9] and left [48.0, 54.0] at n_eff 1 057.)*

| reading | value | 95% interval | n (decided) | deff | n_eff | status |
|---|---|---|---|---|---|---|
| slot A share (pool) | **50.9%** | [47.6, 54.1] | 1 235 | 1.38 | 895 | PASS |
| left-base share (pool) | **51.0%** | [47.7, 54.3] | 1 235 | 1.38 | 895 | PASS |

**K2 reads undetermined.** *(Corrected after the second B3 critic: this read
"K2 PASSes" while the pool alone decided PASS; F-038 now gates every
per-mirror reading.)* The pool is inside +/-5 points and no mirror is resolved
outside tolerance, but every per-mirror reading is open, so K2 cannot PASS.
Settling it needs a ~1 400-seed mirror-only batch (F-038), not run.
At this n, a z-test would detect a ~4.7-point seat bias with 80% power. The
gate itself FAILs one with 80% power only at ~9.6 points (F-038). A 3-point
bias is not excluded (that needs ~3 006 decided mirrors).

The per-mirror readings, each of which K2 now gates, are all undetermined
at 121–124 matches each, with intervals ~+/-10 points:

| mirror | slot A | left base |
|---|---|---|
| `mvp` | 41.1% [31.5, 51.5] | 52.4% [42.2, 62.4] |
| `rush` | 58.1% [47.7, 67.7] | 51.6% [41.4, 61.7] |
| `mass_arclight` | 48.8% [38.7, 59.0] | **64.9% [54.6, 74.0]** |
| others | 47.6–54.8% | 43.8–55.7% |

**`mass_arclight`'s left base is the closest to a FAIL.** Its lower bound is
54.6%, 0.4 points inside the 55% edge. It agrees with the left-base share's
by-strategy design effect of 2.17 (F-038), which hints at strategy-level
geography. With twenty per-mirror readings, one at ~2.9 SE is unusual but not
decisive. It is the first row to re-read: ~64 more seeds of that one mirror
would most likely resolve it, and resolving it outside tolerance FAILs K2
(F-038).

Five of the 1 240 matches timed out, all in the two synth mirrors (3 + 2). Four
were mutual losses.

**Do not read K3 off this batch.** Its median is 4:24 and 73% end before 5:00,
because a mirror-only set over-weights the fast Ripper/Sentinel/rush mirrors.
K3 belongs to a roster batch (section 1, and the full-roster report in F-040).

### What this leaves open

- **K1 on the full roster.** The probe batch cannot see the
  `rush`/`turtle`/`mvp`/synth rows. F-040 reads it from the first
  `balance_report.ron`.
- **The timeout bar.** At ~2–3% timeouts (F-031), resolving "at most 5%" needs
  roughly n_eff >= 150–300. That is a few hundred roster matches, which F-040's
  batch provides.
- **K2 per mirror.** Every mirror is open at 62 seeds, so K2 is undetermined.
  Settling it as PASS needs ~1 400 mirror-only seeds; `mass_arclight`'s left
  base is the cheap re-read that could FAIL it first (F-038).

## F-040 — The first full-roster report: the turtle is dominant, the gate FAILs (B3)

*Corrected after the B3 critic (K3 gates band share, F-038).*

No RON changed. This is the reading F-039 promised. It is the report that
`balance --seeds 4 --report balance_report.ron` writes:
- same seeds (`seed_at(0, k)`, k < 4), same roster order, same orientation
  order, shipped 15-min cap;
- played by a scratch harness (not committed) that ran `run_match` in 3 threads
  and re-sorted the records into the bin's seed/roster/orientation order before
  `BalanceReport::of`;
- 800 matches (10 x 10 ordered pairs x 2 orientations x 4 seeds) in 1 715 s
  wall. That is ~6.4 CPU-s a match, against F-038's 3.3, because it shared the
  box with a debug `cargo test` run.

**Gate: FAIL.** K1 fails on one strategy, named by the gate: **`turtle`**.
K3 fails on band share (43.8%, resolved below 50%).

**Confirmed with the committed bin** (after the second B3 critic). On the
committed tree, `balance --seeds 4 --report /tmp/onus_b3_final_report.ron`
ran serially on the box (`nice 19`, shared with the trading agents and a
debug/release `cargo test`): 800 matches in 49:10 wall (3.7 s a match),
42 MB peak RSS, exit 0. Its tables match the scratch harness's line for line
(the same win matrix, cell samples and pentagon), and so does its kill gate:
- **FAIL**: K1 on `turtle` 99.3% [94.9, 100.0], DOMINANT;
- K3 on band share 43.8% [39.7, 48.0] of 786 decided (timeouts 1.8%
  [0.9, 3.2] PASS);
- `rush` 6.2% [1.4, 13.0] named `losing (named, not gated)`;
- K2 undetermined (pool n_eff 58; every mirror n 8).

The bin plays the matches the harness did and gates them the same way.

### K1 — strength (FAIL)

*(Re-read after the B3 critic fixes, same seeds: the 800 records came back
match-for-match identical, so the matrix, the K3 reading and every status are
unchanged. The intervals below are the final method's — row means on the
worst-case variance (F-038), mirrors at deff 1.38 — and are a little wider
than first printed.)*

| strategy | row mean | 95% interval (n_eff ~95) | status |
|---|---|---|---|
| `turtle` | **99.3%** | [94.9, 100.0] | **FAIL, DOMINANT** (beats all 9 opponents) |
| `mass_sentinel` | 68.3% | [58.4, 77.7] | undetermined |
| `mass_ripper` | 68.1% | [58.2, 77.4] | undetermined |
| `synth_steel_flesh` | 54.1% | [43.9, 64.4] | PASS |
| `synth_triad` | 54.1% | [44.1, 64.2] | PASS |
| `mass_arclight` | 47.9% | [37.8, 58.1] | PASS |
| `mass_ravager` | 45.0% | [34.8, 55.1] | PASS |
| `mass_bulwark` | 30.5% | [21.2, 40.4] | PASS |
| `mvp` | 26.4% | [17.5, 36.0] | PASS |
| `rush` | 6.2% | [1.4, 13.0] | PASS, **LOSING** (named, not gated) |

The turtle's off-diagonal cells are 93.8–100%. This is not new; the gate is
the first thing to *fail* on it:
- F-029/F-030 read turtle row means of 88.9–90.3% and called them "B4's
  business".
- F-036 raised its `attack_at_army` from 15 to 26 for B1's ordering test.
  Its strength was not re-read then.

No RON change was made: B3 only measures. Tuning the turtle is B4.

`rush`, at 6.2%, is named **`losing`**: its row interval lies wholly below
35%. *(Corrected after the B3 critic. This paragraph first said `rush` was
"not strictly losing" because it takes 37.5% of `mass_bulwark` and 18.8% of
`synth_triad`. Both are losses, so it loses every matchup; the old rule missed
it only because those two cells were not resolved below 50% at 16 matches
each.)* Naming does not gate (F-038): K1 bounds strength from above. Lifting
the rush is B4's.

`mass_sentinel` and `mass_ripper` sit just above 65%, with intervals that
straddle it. Resolving them needs roughly n_eff >= 400, about 16+ seeds. They
are the next rows to watch.

### K2 — seat (undetermined here, and on F-039's batch)

Slot A is 61.3% [48.4, 72.7] and left base 48.8% [36.4, 61.3], at n 80 and
n_eff 58 (mirror deff 1.38). Every per-mirror row has 8 matches. The interval is too wide to
judge, which is the outcome F-038's power calculation predicted for a roster
batch. F-039's mirror-only batch (n_eff 895 at deff 1.38) is the K2 reading of record:
its pool PASSes but K2 is undetermined, because every mirror is open
(corrected after the second B3 critic).

### K3 — length (FAIL on band share)

| reading | value | 95% interval | status |
|---|---|---|---|
| band share | **43.8%** of 786 decided | [39.7, 48.0] | **FAIL** (at least 50%) |
| timeouts | **1.8%** (14 of 800) | [0.9, 3.2] | **PASS** (at most 5%) |
| decided median | 5:33 | | context: in band |
| ends before 5:00 | 39.6% of all | [35.6, 43.8] | context, not gated |
| ends after 8:00 or times out | 17.4% of all | [14.4, 20.8] | context, not gated |

Decided quantiles are p10 2:54, p25 4:15, p50 5:33, p75 6:49 and p90 9:30. The
all-match basis differs from these only in the tail (p90 9:56, p100 at the
15:00 cap).

The timeout bar that F-039 left open is now resolved: the upper bound of 3.2%
is under 5%. The band share is above F-031's ~31–38% probe ceiling, but its
whole interval is under 50%: fewer than half of decided matches land in 5–8
minutes, so K3 FAILs. The shortfall is mostly short games (39.6% end before
5:00). Lengthening them is B4's question (F-038).

### Pentagon at 4 seeds

| link | rate | 95% interval | sample | verdict |
|---|---|---|---|---|
| bulwark > ravager | 93.3% | [70.2, 98.8] | 15 decided, 1 timeout | holds |
| ravager > sentinel | 43.8% | [23.1, 66.8] | | undetermined |
| sentinel > ripper | 68.8% | [44.4, 85.8] | | undetermined |
| ripper > arclight | 93.8% | | | holds |
| arclight > bulwark | 100% | | | holds |

That is 3 hold and 2 undetermined; none fails.

### The report file is opt-in

*(After the B3 critic.)* The first version wrote `balance_report.ron` to the
working directory on every run. Four earlier tests run the bin without
`--report`, so every `cargo test` replaced a real batch's report in the
package root with a 1-tick toy one. Two fixes were possible:
- **Have the tests pass temp paths.** That fixes the suite but not the hazard:
  any quick hand run (`--tick-cap 1`) would still clobber the file. It would
  also mean editing two critic-authored test files.
- **Make the file opt-in** (`--report PATH`). The tables always print to
  stdout, and the file is written only where asked. This is the one chosen.

`balance_report.ron` stays the conventional, gitignored name
(`report::DEFAULT_REPORT_PATH`), and `--report` without a value is still an
error. Pinned by `b3_report::without_report_the_binary_prints_the_tables_and_writes_no_file`
and `critic_b3_gate::probe_running_the_bin_as_the_suite_does_leaves_the_package_root_report_alone`.

## F-041 — Opening reservation: scripted production lines are built (B4, multi-barracks)

F-035 found that openings after the first rarely go up. The tech step places an
opening only when the stockpile covers its cost, and the army step spends the
stockpile first on anything cheaper. B3.5 therefore cut every script down to
what the sim actually placed. B4 adds the missing rule to the commander and
restores the scripts.

### The rule (`sim::ai`, tech and army steps)

- An opening is **due** once `tick >= at_tick` and it is not standing. It is
  counted the same way as before (B3.5 AC0b), so a barracks that is lost
  becomes due again.
- The first due opening that the tech walk cannot pay for, in RON order, has
  its Alloy **reserved**. The army step trains only if
  `budget >= unit cost + reserve`. **While an opening is due, the army waits.**
- **The tech walk itself is unchanged.** A later, cheaper opening that is due
  and affordable still goes up past an unaffordable one, and it still takes
  the only RNG draw. That is the skip rule which
  `critic_b35_ac0b::critic_a_skipped_expensive_opening_does_not_eat_the_draw`
  pins.
  - A first version also held the reserve back from later openings, which put
    the openings up strictly in RON order. It broke that critic test, so it
    was dropped. `synth_triad` therefore keeps F-035's listed order: its
    second Gene-Vats (150) still goes up before its Aether Spire (200).
- The worker step is not held, because workers are the income that pays for
  the opening.
- An opening whose `at_tick` is still ahead reserves nothing. A one-opening
  script, and any script once all its openings stand, plays exactly as it did
  before B4.
- Determinism: the rule reads only the script (RON order), the counted
  standing barracks and the stockpile. It adds no RNG draw and no map
  iteration.

### Restored scripts (opening lists only — no stat, cost, nemesis or threshold moved)

- Every `mass_*` probe has three identical lines again: 300/130, 600/165 and
  900/200.
  - `b1_probe_set::MASS_PROBE_BARRACKS = 3`.
  - `MASS_PROBE_OPENING_EXCEPTIONS` is empty.
  - The per-probe cut in `critic_b1_ac3` and the line-count exception in
    `critic_b35_armour` are gone.
- `synth_steel_flesh` has four openings again (adds Foundry 1200/195 and
  Gene-Vats 1500/225).
- `turtle` has four openings again (adds Gene-Vats 2400/210).
- `turtle` keeps F-036's `attack_at_army: 26`.

### Tests (`tests/b4_reservation.rs`)

| test | without the reservation | with it |
|---|---|---|
| `a_three_line_script_builds_all_three_lines_in_a_real_match` (gating) | red: 1 of 3 Foundries in a 25 359-tick match | green |
| `every_multi_opening_strategy_places_its_whole_script_in_a_head_to_head` | red: `mass_ripper` placed 1 of 2 | green |
| `the_army_trains_nothing_while_an_opening_is_due` | red: trained at tick 2 460 with the second Foundry (due 600) unplaced | green |
| `a_cheaper_later_opening_still_goes_up_and_the_reserved_one_follows` | red: 1 of 3 placed | green |
| `an_opening_not_yet_due_changes_nothing` | green | green (confinement) |

### The golden proof (F-032 standard)

The proof switch is the Cargo feature `no-opening-reservation`. It compiles
the reservation out: `const RESERVING = !cfg!(..)`, which gates the single
line that sets the reserve. Nothing else differs. No shipped build turns it
on. A content knob was rejected for two reasons:
- it would add a `StrategyDef` field, which `critic_m5`'s fingerprint
  coverage would then require;
- it would change the data under proof.

Command, on the box, on the final B4 `src/` and `Cargo.toml`, with
`assets/data/strategies.ron` and every test file this change edits put back
to HEAD (071aad9):
- the files: `b1_probe_set`, `critic_b1_ac3`, `critic_b35_armour`,
  `critic_b35_ac0`, `critic_b35_ac0b`, `b3_gate`, `b3_pentagon`;
- the command:
  `CARGO_BUILD_JOBS=3 nice -n 19 cargo test --release --no-fail-fast --features no-opening-reservation -- --test-threads=2`

**Result: 936 passed, 4 failed, 1 ignored.**
- The four failures are exactly the four new B4 tests that need the
  reservation.
- Every pre-B4 golden and pin passes at its old value: the `state_hash`
  traces, the journal digests, the pentagon and gate batch readings, and the
  F-036 ordering ticks.

So the new binary with the switch on is the pre-B4 commander. Everything
that moves below moves because of the reservation and the restored lists.

### Goldens and pins re-pinned (old → new)

`critic_b35_ac0::depth_one_replays_the_pre_change_tree_tick_for_tick` covers
4 000 ticks per row. Each cell is trace / end / journal:

| matchup, seed | pre-B4 (B3.5, F-032) | B4 |
|---|---|---|
| mvp v mvp, 4 | `3fc03b8fe6b85f9c` / `25156a333a60d5f5` / `55675b7844c3d493` | unchanged |
| mass_ripper v mass_arclight, 7 | `28b235776567bfdb` / `2712cca3a3f9623d` / `75541b9be9a704af` | `cb0d0cefd1c35cee` / `b52ef814cef8f447` / `8fc66471b75cccd4` |
| mass_arclight v mass_ripper, 7 | `ed81f894ea6404c5` / `e34df6330cc3de55` / `7b14173f43fbd987` | `3771c88168bd1d1e` / `21e24911388b5397` / `c96cb7bdb5808ccb` |
| rush v turtle, 13 | `150c965cb92bc5dd` / `1bcc0d24269d5401` / `a0aafca7c1569ab8` | `06942a8403be0db2` / `9d84c955b59db380` / `4a8a3944ddad8438` |
| turtle v rush, 13 | `cfa20a8b9f637c8c` / `f57569c0425baf71` / `feb9e8af864c9d83` | `007df9983eb19101` / `f073adc204087164` / `9f41c2f1759a59fd` |
| synth_triad v mass_ravager, 101 | `b1f07889d96577ea` / `13452c110f3c1aa3` / `3c49146fdbaa1138` | `86b641af01e15e6b` / `531069917b244e5d` / `c624d8e697c1d7c8` |
| mass_bulwark v mass_sentinel, 55 | `8d6e712a54adb2bd` / `5135af6559226198` / `04a1178d2cad2459` | `2b0e2f236b69728a` / `5bc08751984d65e2` / `5979ac905d17e069` |
| mass_sentinel v mass_bulwark, 55 | `3759cc67de4c88c5` / `1a882b6c65ee06ee` / `d16f832427aac821` | `64e1c8d4d36836ac` / `4ecd0c5380f1157a` / `a82aa83505b2a07f` |
| synth_steel_flesh v rush, 99 | `506d4c5a7aafe94e` / `d8cbe7b2962d19ed` / `8a9b279965598ebb` | `694a4cc5e47de61a` / `b536edde395aa937` / `086d9db263577fd8` |

`critic_b35_ac0b::critic_shipped_matchups_are_bit_identical` covers 7 200
ticks. Each cell is hash / journal:

| # | matchup, seed | pre-B4 | B4 |
|---|---|---|---|
| 0-2 | mvp v mvp 1, mvp v rush 2, rush v mvp 3 | unchanged | unchanged |
| 3 | synth_triad v turtle, 4 | `e94d2708dca81c49` / `4666a3b3563b8890` | `622c06205abf7772` / `65f9917da21d98f2` |
| 4 | turtle v synth_triad, 5 | `b7d0e39bdfb0757c` / `926206a7e2150e39` | `1aa33ab2d6a04527` / `a6552ff89996a930` |
| 5 | mass_arclight v mass_ripper, 6 | `283fcec8bb5ebf41` / `c7328be1b0e09085` | `54b9d0fd2c5779be` / `2dbb7d71846b917e` |
| 6 | mass_ripper v mass_arclight, 7 | `1bc7d81e51c54e99` / `90d87cb290ad3f58` | `241e74a2e363c562` / `f6546287f161a392` |
| 7 | synth_steel_flesh v mass_bulwark, 8 | `478a4be3e491483e` / `c9f9d38ba031b30b` | `3835febbbac676e3` / `b54435b778763907` |

Only the matchups with a multi-opening side move. The `mvp`/`rush` rows,
which have one opening each, are bit-identical, as the confinement predicts.

`b3_pentagon::the_real_batch_reports_what_the_sim_actually_does` (five mass
probes, 2 seeds, 8 matches a link):

| link | B3.5 | B4 |
|---|---|---|
| bulwark > ravager | 87.5% holds | 87.5% holds |
| ravager > sentinel | 62.5% undetermined | 25.0% [7.1, 59.1] undetermined |
| sentinel > ripper | 50.0% undetermined | 62.5% undetermined |
| ripper > arclight | 100% holds | 75.0% undetermined |
| arclight > bulwark | 100% holds | 83.3% (6 decided, 2 timeouts) undetermined |
| total | 3 hold, 2 undetermined | 1 holds, 4 undetermined, 0 fail |

`b3_gate::the_shipped_reading_on_the_pentagon_batch` (the same 100 matches):

| reading | B3 (F-039) | B4 |
|---|---|---|
| K1 rows (bulwark / sentinel / ripper / ravager / arclight) | 37.5 / 65.6 / 68.8 / 46.9 / 31.3 | 26.0 / 78.1 / 68.8 / 34.4 / 42.7 |
| K1 status | undetermined | undetermined (sentinel [57.0, 95.7] and ripper straddle 65%) |
| K2 slot A / left base | 65% / 45% (n 20) | 55% / 55% (n 20), undetermined |
| K3 decided median | 23 790 (6:36) | 22 962 (6:22) |
| K3 timeouts | 0 of 100 | 2 of 100, undetermined |
| K3 band share | 36%, **FAIL** | 55.1% [43.5, 66.1], undetermined |
| before 5:00 / after 8:00 | 34% / 30% | 10% / 36% |
| gate | **FAIL** | undetermined |

### What the restored lines did to the B3.5 design tests (left red, not re-tuned)

The brief froze every RON value except the opening lists, so the five tests
below stay red. Each one is a design reading, not a golden, and none is
weakened. They are phase 2's targets.

1. **`b35_tempo::the_decided_match_median_is_in_the_five_to_eight_minute_band`**
   - The poles batch (`mass_bulwark`, `mvp`, `rush`) now has a decided median
     of **2:44** (9 844 ticks), against the 5–8 minute band.
   - Batch: 2:05 2:07 2:18 2:24 2:34 2:34 2:35 2:43 2:43 2:44 2:44 4:13 6:10
     7:52 7:53 7:53 8:21 8:29.
   - Three real Foundries reach `attack_at_army` 10 much faster than one
     Foundry did. F-029/F-030 calibrated the threshold against an army that,
     per F-035, was being built on one line.
2. **The F-036 ordering, turtle latest.**
   - Failing: `critic_b1_ac3::the_rush_is_the_earliest_and_the_turtle_the_latest_of_the_whole_set`
     (solo, seed 4) and
     `critic_b35_closure::the_turtle_commits_last_on_every_seed_with_the_claimed_margin`.
   - The turtle now first commits at **tick 20 580** (19 320 at F-036).
     `mass_bulwark` now commits at **23 820**, later than the turtle.
   - Mirror commit ticks on seed `seed_at(0x70A71E, 0)`:

     | strategy | commit tick |
     |---|---|
     | `rush` | 750 |
     | `mvp` | 4 440 |
     | `mass_ripper` | 11 280 |
     | `synth_steel_flesh` | 14 100 |
     | `synth_triad` | 14 670 |
     | `mass_sentinel` | 16 500 |
     | `mass_ravager` | 20 130 |
     | `turtle` | 20 580 |
     | `mass_arclight` | 20 910 |
     | `mass_bulwark` | 23 820 |

   - In a mirror, a mass probe now holds its army for three openings of a
     150–200 building, so the slow-economy probes (bulwark, arclight) commit
     later than the turtle.
   - Rush is still the earliest. `b1_probe_set::the_rush_commits_early_and_the_turtle_masses_first`,
     which compares only rush and turtle, is green.
   - Per the brief, the turtle was not re-tuned.
3. **`critic_b1_ac3::every_mass_versus_mass_cell_resolves_in_both_orientations`**:
   `mass_bulwark` vs `mass_arclight` does not resolve within 54 000 ticks.
   This is the same pairing behind the two timeouts in the pentagon batch.
4. **`critic_b3_ac2::a_crippled_probe_breaks_the_link_it_cripples`**: its
   baseline premise, "arclight > bulwark holds on shipped content" (1 seed),
   now reads undetermined, so the injected-imbalance check has no holding
   link to break.

The reservation is the capability the B4 plan asked for. These four
readings are what that capability does to a tempo tuned without it. Restoring
them is B4's RON tuning loop: thresholds and timings, plus a decision on
whether the poles' ordering is still the design. They are recorded here and
were not touched.

## F-042 — The B4 baseline: the turtle still FAILs K1, K3 is no longer failing, one pentagon link breaks (B4)

This is the reading B4's tuning loop starts from. It was taken on the
post-reservation build (F-041, commit 3c878f7), on the box, single-threaded
under `nice 19`. **No RON value is tuned yet.** The only content change since
F-040 is the restored opening lists (F-041).

Command:
`./target/release/balance --seeds 4 --report /tmp/onus_b4_baseline.ron`

- Seeds `seed_at(0, k)` for k < 4: the same seeds, roster order, orientation
  order and 15-min cap as F-040.
- 800 matches in 37:23 wall, 2.8 s a match, 42 MB peak RSS, exit 0.

**Gate: FAIL.** K1 fails on `turtle` only.
- K3 is undetermined. At F-040 it was FAIL.
- K2 is undetermined, as at F-040.

### K1 — strength (FAIL, on `turtle`)

| strategy | B4 row mean [95%] | status | F-040 |
|---|---|---|---|
| `turtle` | **88.2%** [80.2, 94.7] | **FAIL** | 99.3% [94.9, 100.0], FAIL, DOMINANT |
| `mass_sentinel` | 66.0% [56.0, 75.5] | undetermined | 68.3% [58.4, 77.7], undetermined |
| `rush` | 62.5% [52.5, 72.2] | undetermined | 6.2% [1.4, 13.0], PASS, LOSING |
| `mvp` | 59.7% [49.7, 69.6] | undetermined | 26.4% [17.5, 36.0], PASS |
| `synth_triad` | 52.5% [42.5, 62.5] | PASS | 54.1% [44.1, 64.2], PASS |
| `mass_ripper` | 43.1% [33.1, 53.1] | PASS | 68.1% [58.2, 77.4], undetermined |
| `mass_arclight` | 40.4% [30.3, 50.6] | PASS | 47.9% [37.8, 58.1], PASS |
| `synth_steel_flesh` | 38.9% [29.0, 49.0] | PASS | 54.1% [43.9, 64.4], PASS |
| `mass_ravager` | 34.8% [25.2, 44.8] | PASS | 45.0% [34.8, 55.1], PASS |
| `mass_bulwark` | **14.0%** [7.0, 22.4] | PASS, **LOSING** | 30.5% [21.2, 40.4], PASS |

All rows have n 137–144 in 36 clusters and n_eff 92–95.

- **Failing: `turtle`.** It is no longer *dominant*. It takes 37.5% of
  `rush`, and every other cell is 68.8–100%.
- **Losing (named, not gated): `mass_bulwark`**, which replaces F-040's
  `rush`. Its only winning cell is its pentagon prey `mass_ravager`
  (86.7%). Against everything else it takes 0–14.3%, including 14.3%
  against its predator `mass_arclight`.
- **The rush is no longer losing.** Three real lines on every mass probe
  delay their armies. A mass probe now holds its army for three openings, as
  the commit ticks in F-041 show. The rush, with one opening, now beats
  `mass_ripper`, `mass_arclight` and `synth_triad` 100% and `mass_bulwark`
  87.5%.
- `mass_ripper` dropped from 68.1% to 43.1%.
- The rows to watch at the 65% bar are still `mass_sentinel` (66.0%, its
  interval straddling the bar) and now `rush`.

### K2 — seat (undetermined)

| reading | B4 | F-040 |
|---|---|---|
| slot A (pool, 80 mirrors) | 50.0% [37.5, 62.5], n_eff 58 | 61.3% [48.4, 72.7] |
| left base (pool) | 50.0% [37.5, 62.5] | 48.8% [36.4, 61.3] |
| per mirror (8 each) | all undetermined | all undetermined |

**The K2 re-read of `mass_arclight`.** F-039 named this mirror's left base
as the reading closest to a FAIL: 64.9% [54.6, 74.0] on 62 seeds of the
pre-B4 content. This run played the mirror alone:
- `balance --only mass_arclight --seeds 126 --report /tmp/onus_b4_arclight.ron`;
- that is F-039's 62 seeds plus 64 more, k < 126;
- 252 matches, 14:42 wall, exit 0.

| reading | B4, 126 seeds (252 decided, 0 timeouts) | F-039, 62 seeds (pre-B4) |
|---|---|---|
| left base | **52.4%** [45.2, 59.5], n_eff 183 | 64.9% [54.6, 74.0] |
| slot A | 53.2% [45.9, 60.3] | 48.8% [38.7, 59.0] |
| status | undetermined (not resolved outside 50 +/- 5) | undetermined (0.4 pts from FAIL) |

Split by seed range (from the bin's per-match log; left base):
- k < 62 (F-039's seeds): 58.9% (73 of 124);
- k >= 62 (the 64 new seeds): 46.1% (59 of 128).

The 64 new seeds pull the reading back toward 50%. The near-FAIL does not
recur, and K2 does not fail on this mirror.

Caveat: this is not a pure re-read of F-039's sample. The content changed
(arclight now builds three Spires), so the first 62 seeds are new matches on
the same seeds. It also still does not *pass*: the interval reaches 59.5%,
past the 55% edge. Settling K2 as PASS still needs F-038's ~1 400 mirror
seeds. Do not read K3 off this batch: it is mirror-only, and every match is
6:15–11:47.

### K3 — length (undetermined; was FAIL)

| reading | B4 | F-040 |
|---|---|---|
| band share | **48.5%** of 792 decided, [44.3, 52.7], undetermined | 43.8% of 786, [39.7, 48.0], **FAIL** |
| timeouts | 1.0% (8 of 800), [0.4, 2.2], PASS | 1.8% (14), [0.9, 3.2], PASS |
| decided median | 5:58 | 5:33 |
| ends before 5:00 (of all) | 31.8% [28.0, 35.7] | 39.6% [35.6, 43.8] |
| ends after 8:00 or times out | 20.2% [17.1, 23.8] | 17.4% [14.4, 20.8] |

- Decided quantiles: p10 2:21, p25 3:30, p50 5:58, p75 7:18, p90 9:05,
  p100 14:37.
- The band share's interval now straddles 50%. The short-game share fell
  8 points.
- All 8 timeouts involve `mass_arclight` or `mass_bulwark`. By unordered
  matchup:

  | matchup | timeouts |
  |---|---|
  | arclight v synth_steel_flesh | 4 |
  | bulwark v arclight | 2 |
  | bulwark v ravager | 1 |
  | arclight v synth_triad | 1 |

  These are the same stalls behind F-041's red
  `every_mass_versus_mass_cell_resolves` (bulwark v arclight).

### Pentagon at 4 seeds

| link | B4 | F-040 |
|---|---|---|
| bulwark > ravager | 86.7% [62.1, 96.3], 15 decided, 1 timeout, holds | 93.3%, holds |
| ravager > sentinel | **25.0% [10.2, 49.5], 16 decided, FAILS** | 43.8%, undetermined |
| sentinel > ripper | 56.2% [33.2, 76.9], undetermined | 68.8%, undetermined |
| ripper > arclight | 62.5% [38.6, 81.5], undetermined | 93.8%, holds |
| arclight > bulwark | 85.7% [60.1, 96.0], 14 decided, 2 timeouts, holds | 100%, holds |

That is 2 hold, 2 undetermined and **1 fails**. It is the first link read as
broken since F-031. `ravager > sentinel` was F-031's coin flip, and with
three lines each the Sentinel wins it outright.

### What B4's tuning loop inherits

1. **The turtle** fails K1 at 88.2%. Lifting it is the one gating failure.
2. **`ravager > sentinel`** breaks the pentagon.
3. **`mass_bulwark` is losing** at 14.0%.
4. **K3** is one resolved band-share reading away from PASS or FAIL. The
   F-041 tempo test reads 2:44 on its three-pole batch.
5. **The five design tests F-041 left red** are part of this loop:
   - tempo median;
   - turtle-latest ordering (two tests);
   - bulwark v arclight resolution;
   - crippled-probe baseline.

No RON was changed for this reading.

## F-043 — B4 tuning, phase 2: the RON search, budget exhausted, nothing kept (B4)

This entry records the RON-only search that followed F-042. The compute budget
(about 8 box CPU-hours) ran out before any candidate met the gate. **No RON is
changed by this entry.** The shipped content is still F-042's, and so are its
readings and its five red design tests.

Method:
- Candidates were whole `units.ron` / `strategies.ron` copies, swapped into
  the box tree one at a time and restored afterwards.
- Content loads at runtime from `assets/data`, so nothing was rebuilt.
- Screens used three batches:
  - the B3.5 poles batch, `--only mass_bulwark,mvp,rush --seeds 1`, which is
    the `b35_tempo` fixture;
  - `--only` subsets;
  - the 1-seed roster.
- The five red tests were run in release against each surviving candidate.
- The best candidate was then confirmed on the 4-seed roster.

### Constraints the search found (tests that pin RON values)

These values cannot move without editing a test, so they were held:
- `mvp` is pinned number for number (`b1_strategies`, `critic_b35_armour`).
- The mass probes' `attack_at_army` is 10 and `mitigation_per_armor` is 2
  (`critic_b35_armour`).
- Offense and armor of bulwark, ravager, sentinel, arclight and ripper, and
  `damage_per_offense` / `nemesis_bonus`, are pinned through exact
  `damage_per_hit` values (`src/sim/combat.rs` L1 tests, `critic_m4b`).
- `mvp_gather_ticks: 90` is string-matched by `critic_m4a`.

### The economy is the clock

An AI-journal trace of the F-042 content shows how slowly a mass probe
builds. On the default seed, `mass_bulwark` with 6 workers and lines at
300 / 600 / 900 trains one Bulwark roughly every 1 830 ticks. That is one
unit's cost in income, so three lines cannot be fed. Each later line also
costs 150 that would otherwise be army.

`mvp` against `mass_bulwark`: `mvp` attacks at tick 4 440 with three units.
`mass_bulwark` has one Bulwark (trained at 330), and its second comes at
9 120. `mvp` levels the HQ by 2:30–3:10. That is the
`b35_tempo` red: 11 of the 18 poles matches end under 5:00.

### Screens (poles batch: decided median tick at index 9, in band 18 000..28 800 = PASS)

| try | change on F-042 content | poles median | note |
|---|---|---|---|
| s1a | bulwark 90 Alloy / 1 200 train | 9 196 | `mvp` itself gets faster (it trains a Bulwark) |
| s1d | bulwark 80 / 1 100 | 7 989 | worse |
| s2a / s2b | `building_hp_per_defense` 700 / 900 | 11 663 / 12 698 | stretches the long games past 8:00, not the short ones |
| o1 | mass probes' 2nd/3rd lines at 3 000 / 6 000 | 15 205 | `mvp` still wins in one orientation |
| w9 + o1 | o1, plus mass `worker_target` 9 | 22 196 | PASS |
| w9 + o1 + bulwark attack 70 ticks | | 21 595 | PASS; all five reds green apart from the closure re-pin |

A 16-cell grid was run over worker 6/9 × bulwark cost × bulwark attack ×
openings. Only the o1 cells reached the band. Without the extra workers, only
`w6 + o1 + attack 70` did so (21 441).

### Roster screens (1 seed, 200 matches each; ± about 20 points per row)

| cand | content (cumulative intent) | reds | K1 worst | K3 band | links | rejected because |
|---|---|---|---|---|---|---|
| c1 | w9+o1+atk70, turtle worker 9, ravager 75/1 000, steel_flesh 2nd pair at 3 000 / 6 000 | not run | mass_sentinel 80.6 | 41.7% | no fail | mass probes beat every non-mass script (mvp 25, rush 19) |
| c2 | c1 with mass workers 6 | — | turtle 83.3 | 48.0% | bulwark>ravager 25 | turtle |
| c5 | c2 with mass workers 9, turtle worker 7 / attack 20, ravager 80/1 100 | 5/5 (closure re-pin) | — | — | — | (rosters as c6) |
| c6 | c5 with synth workers 10 | — | mass_sentinel 75.0 | **35.6% FAIL** | no fail | K3; turtle fixed (38.9) |
| c7a | c6 with building HP 520, rush workers 4 | — | mass_sentinel 77.8 | 38.9% FAIL | no fail | building HP barely moves the median (+12 s); rush workers change nothing |
| d1 | mass workers 6, mass lines 300 / 9 000 / 15 000, rest as c4 | 4/5 | — | — | — | arclight>bulwark undetermined (bulwark attack 70 makes the bulwark kill a 40-HP Arclight in 3 hits) |
| d3 | d1 with arclight defense 3 | 1/5 | — | — | — | stalls |
| **d2** | d1 with the bulwark attack back to 90 | **5/5** (closure re-pin) | turtle 71.3 | 39.9% FAIL | bulwark>ravager 0/4 | best on the reds; confirmed below |
| d4 | d2 with ripper 50/800 and ravager 90/1 100 | not run | turtle 80.6, triad 79.6 | 37.1% FAIL | no fail | worse everywhere; rush 5.6 |

Other notes:
- `c3` (mass workers 6, bulwark defense 10) and `c4` (mass workers 8),
  both on the c5 line, failed `b35_tempo`'s density floor and the
  `mass_bulwark`/`mass_arclight` seed-7 stall.

### The best candidate (d2), 4 seeds, 800 matches

The d2 diff against the shipped content:
- `ravager`: 90 → 80 Biomass, train 1 200 → 1 100.
- All five `mass_*`: 2nd/3rd lines 600 / 900 → 9 000 / 15 000. Still
  knob-identical.
- `synth_steel_flesh`: 3rd/4th lines 1 200 / 1 500 → 3 000 / 6 000.
- `turtle`: `worker_target` 12 → 7, `attack_at_army` 26 → 20.

The five red design tests under d2 (release):
- `b35_tempo` median: green. The density test stays green.
- rush-earliest / turtle-latest: green.
- every mass×mass cell resolves: green.
- the crippled-probe premise: green.
- The closure: the ordering and margin hold on the first seed (turtle
  26 190, latest other `mass_bulwark` 23 820). Only the F-036 pin
  (19 320 / 18 750) is red.

**Gate: FAIL** (F-042: FAIL).

| reading | d2 | F-042 |
|---|---|---|
| K1 | **FAIL on `mass_ripper` 77.6% [68.2, 86.0]** | FAIL on `turtle` 88.2% [80.2, 94.7] |
| turtle | 68.3% [58.3, 77.8] undetermined | 88.2% FAIL |
| mass_sentinel | 66.9% [56.8, 76.5] undetermined | 66.0% undetermined |
| mass_bulwark | 21.6% [13.3, 30.9] LOSING | 14.0% LOSING |
| rush | 23.6% [15.1, 33.1] LOSING | 62.5% |
| K3 band share | **33.9% [30.1, 38.0] FAIL** (median 6:17; 281 below, 237 above) | 48.5% [44.3, 52.7] undetermined |
| timeouts | 2.0% PASS | 1.0% PASS |
| K2 | undetermined, slot A 57.5% | undetermined, 50.0% |
| bulwark > ravager | 57.1% undetermined | 86.7% holds |
| ravager > sentinel | 37.5% [18.5, 61.4] undetermined | **25.0% FAILS** |
| sentinel > ripper | 37.5% undetermined | 56.2% undetermined |
| ripper > arclight | 93.8% holds | 62.5% undetermined |
| arclight > bulwark | 93.8% holds | 85.7% holds |

The rest of d2's rows: `mvp` 37.5, `mass_ravager` 52.7, `mass_arclight`
50.4, `synth_steel_flesh` 45.3, `synth_triad` 56.0.

What d2 achieves and what it costs:
- It turns the five design tests green and removes the pentagon FAIL. It
  also takes the turtle off K1.
- It makes `mass_ripper` the K1 failure.
- It spreads match length both ways. `rush` loses almost every cell inside
  3 minutes, and the slow mass lines push the mass×mass cells past 8:00. K3
  goes from undetermined to FAIL.
- It was not kept.

### What is needed next

1. **K3 is the binding constraint.** Under d2 the decided median is
   centred (6:17), but the spread is too wide.
   - The short tail is mostly `rush` (32 of its 40 matches end under 5:00 in
     c7a) and `mvp`.
   - The long tail is mass×mass.
   - The levers that moved length here (workers, line timing) move a mass
     probe's strength just as much.
   - The levers that would move length alone are pinned by tests (gather
     ticks, mitigation) or barely move it (building HP).
2. **`mass_ripper` (77.6%).** A ripper nerf by cost (d4) knocked `rush`
   and the synths down with it, because they share the unit.
3. The next search should start from d2. Steps worth a screen:
   - a ripper train-time nerf only;
   - `rush` `attack_interval_ticks` / think interval, so the rush stops
     feeding single rippers into a defended base;
   - a `mass_*` `attack_interval_ticks` change, to shorten mass×mass
     sieges.

## F-044 — Unpinning the balance values from the tests (B4, test-only)

F-043 found that tests pinning shipped balance values (damage numbers,
`mitigation_per_armor`, the mass probes' `attack_at_army`, `mvp_gather_ticks`)
blocked the combat levers. This entry removes those pins. Following F-033
(`kill_budget(content, …)`), each rewritten test now derives its expected
value from the loaded RON.

**Nothing in the sim or the content changes.**
- The two `src/sim` edits are both inside `#[cfg(test)] mod tests`.
- No RON changes.
- One new test-support file, `tests/support/ron_field.rs`. It reads a field's
  literal text from the shipped RON, so a mutation anchor such as
  `damage_mult: 1.3,` follows a re-tune instead of pinning it.

**How meaning is kept.**
- A combat test still checks the arithmetic. Its expectation is the
  documented formula, evaluated in the test from content inputs, never by
  calling `damage_per_hit`:
  - `offense*damage_per_offense − armor*mitigation_per_armor`, saturating;
  - for nemesis: `floor(base*mult_milli/1000)`, with the armor rule applied;
  - `defense*hp_per_defense`;
  - `round(damage_mult*1000)`, from the decimal *as written*.
- Probe values that only need to differ from the shipped value are chosen
  relative to it, e.g. `damage_per_offense` becomes shipped−2.
- Fixtures that relied on today's stats now state their precondition: at
  least three hits to kill, a prey with armor, and a non-nemesis pair.

**Left pinned on purpose:**
- the `mvp` identity pins (b1_strategies, critic_b1, m4c_ai,
  `the_mvp_strategy_is_untouched_by_the_tempo_pass`);
- per-tick goldens and measured readings (they move under F-032);
- design claims such as the five F-041 reds.

Some literal anchors remain on values outside the B4 lever set, and are still
pinned:
- the worker's `mvp_alloy_cost: 10` in the M5/M6/P2 fingerprint and replay
  probes;
- `starting_alloy: 300` in critic_b1 and critic_m4a's `starting_alloy`
  probes;
- the HQ's `mvp_defense: 10, mvp_armor: 4` in critic_m4c;
- the strategies' `mvp` anchors.

If B4 needs one of these levers, its anchors have to be unpinned first.

### The rewritten tests

Each row gives the test, what it pinned before, what it derives now, and the
mutations that turn it red. Mutation ids refer to the table below.

**`src/sim/combat.rs` unit tests**

| Test | Pinned | Now derives | Red under |
|---|---|---|---|
| `damage_is_offense_minus_flat_armor` | 37 | plain formula, arclight→ravager | M1 |
| `armor_can_absorb_a_hit_but_never_heals` | worker→bulwark 0 (vacuous: worker offense 0) | in-memory armed worker vs bulwark armor > hit | M1 |
| `nemesis_boosts_by_30_percent_and_skips_armor` | 26 / 10 / 35−18 | nemesis and plain formulas | M1, M2, M5 |
| `the_nemesis_multiplier_is_integer_per_mille` | `mult_milli == 1300`; a tautological 13/10 loop | `round(damage_mult*1000)`; the floor checked through `damage_per_hit` on an in-memory probe | M2, M3, M4 |
| `health_pool_comes_from_defense` | (180,180) | `defense*hp_per_defense` | M6 |

**`src/sim/content.rs`**

| Test | Pinned | Now derives | Red under |
|---|---|---|---|
| `lookups_resolve_by_id_in_ron_order` | arclight offense 9 | offense read off the RON text | M7 |

**`m4a_economy`**

| Test | Pinned | Now derives | Red under |
|---|---|---|---|
| `loads_units_and_resources_from_ron` | bulwark (2,4,9,9), mult 1.3 | each field read off the RON text | M7 |
| `unbuildable_or_free_content_is_rejected_at_load` | anchor `mvp_alloy_cost: 70,` | sentinel field, read | M14 |

**`m4b_combat`**

| Test | Pinned | Now derives | Red under |
|---|---|---|---|
| `a_unit_attacks_the_enemy_in_range_until_it_dies_and_despawns` | 18 per hit, 60 HP | formula; `defense*hp_per_defense` | M1, M6 |
| `an_enemy_outside_engage_range_is_ignored` | worker 60 HP | `defense*hp_per_defense` | M6 |
| `nemesis_adds_30_percent_and_ignores_armor` | 1300 | the RON decimal ×1000, rounded | M2, M3, M5 |
| `the_nemesis_bonus_applies_in_a_live_fight` | `base*13/10` | `base*mult_milli/1000` plus the armor rule | M2, M5 |
| `a_nemesis_multiplier_is_exact_or_refused` | anchor `1.3` | field read; shipped case plus a 1.3 case | M2, M3, M5, M19 |
| `out_of_scale_stats_are_rejected_and_the_bonus_never_wraps` | anchor `speed: 2, offense: 4,` | bulwark field, read | M13 |
| `a_scale_the_validator_cannot_multiply_is_an_error_not_a_panic` | four literal anchors | fields, read | M18 |

**`critic_m4b`**

| Test | Pinned | Now derives | Red under |
|---|---|---|---|
| `the_stat_scaling_is_read_from_the_ron_not_hardcoded` | 10, 15; anchors | formula on the edited factor (moved off its shipped value) | M1, M6 |
| `the_nemesis_bonus_is_read_from_the_ron_and_floors` | 1150 / 23 / 18 | mult moved to 1.15 (1.25 if 1.15 ships); `ignore_armor` flipped; formula | M2, M3, M5 |
| `degenerate_or_missing_combat_content_is_rejected_at_load` | nine literal anchors | fields read; leash = engage/2 | M12 |
| `max_stat_is_data_and_bounds_exactly_the_design_scale` | `max_stat: 10`, sentinel offense 6, "arclight 9" | max_stat+1; the roster's top stat −1 | M13 |
| `a_degenerate_scale_is_rejected_not_overflowed_inside_the_validator`, `accepted_content_never_needs_a_saturating_hit` | anchors | fields, read | M18 |
| `a_nemesis_multiplier_is_rejected_or_applied_as_written` | anchor | field, read | M19 |
| `every_accepted_multiplier_is_applied_exactly_as_written` | anchor | field, read | M3 |
| `the_shipped_multiplier_is_exactly_1300_per_mille` (name kept) | 1300 | the RON decimal ×1000 in f64 | M3 |

**`critic_m4a` (pass2)**

| Test | Pinned | Now derives | Red under |
|---|---|---|---|
| `a_one_tick_gather_lands_on_the_first_mining_tick`, `gather_and_production_honour_the_ron_tick_counts_identically` | anchors `mvp_gather_ticks: 90`, `mvp_train_ticks: 600` | worker fields, read | M8 |
| `every_charge_follows_the_edited_ron_price` | anchors worker 10, foundry 150 | fields, read | M9 |
| `validation_rejects_every_class_of_unplayable_content` | foundry 150, sentinel 900, ranges | fields, read | M11 |
| `missing_starting_alloy…`, `starting_alloy_comes_from_ron…` | anchor `starting_alloy: 300` | field, read | not mutated (no failure under p1) |

**Other test files**

| Test | Pinned | Now derives | Red under |
|---|---|---|---|
| `critic_m4c::targeting_matches_the_oracle_with_half_claims_on_the_field` | assumed nobody dies on tick 1 | oracle read with the sim's own death rule (a unit whose nearest enemy died holds no `Target`); at least half the field still checked | M20 |
| `critic_m5::content_that_would_break_the_round_trip_cannot_be_loaded` | foundry `alloy_cost: 150` | cost from content | M15 |
| `m5_replay::a_duplicated_content_id_is_impossible_to_load` | foundry `alloy_cost: 150` | cost from content | M15 |
| `m5_replay::the_fingerprint_covers_the_whole_content_exactly` | `36.0` → `36.000004` | shipped value → next f32 | M17 |
| `m4c_ai::the_victory_target_must_be_exactly_one_building` | foundry 150 | cost from content | see the note on M16 |
| `critic_b35_ac0b::critic_unaffordable_placements_consume_no_randomness`, `critic_a_skipped_expensive_opening_does_not_eat_the_draw` | 160 Alloy | Foundry cost + 10, asserting it buys exactly one Foundry and no Spire | M10 |
| `critic_b35_armour::the_shipped_combat_scaling_keeps_mitigation_per_armor_off_the_reverted_one` (was `…_at_two`) | mitigation == 2 | the F-031 decision: mitigation ≠ 1 | R1 |
| `critic_b35_armour::the_five_mass_probes_are_knob_identical_at_one_shipped_threshold` (was `…_at_attack_at_army_ten`) | each probe's threshold == 10 | every probe equals probe 0, and the threshold is ≥ 1; the other knob-identity checks are unchanged | R2 |

### Mutations

Each mutation was applied alone in the box tree. The tests it guards were run
in debug, then the file was restored and `touch`ed.

| Id | Mutation |
|---|---|
| M1 | Plain hits ignore mitigation. |
| M2 | Nemesis is checked in reverse (the prey is boosted). |
| M3 | `milli_exact` floors instead of rounding. |
| M4 | The nemesis boost uses ceiling division instead of floor. |
| M5 | `ignore_armor` is inverted. |
| M6 | The HP pool is `defense*hp_per_defense + 1`. |
| M7 | The loader swaps offense and speed. |
| M8 | A load lands one tick late. |
| M9 | A placement charges one Alloy over the RON price. |
| M10 | The AI reads a barracks price as 11 higher. |
| M11 | A zero training time is accepted. |
| M12 | `pursue_range < engage_range` is accepted. |
| M13 | The `max_stat` bounds are off. |
| M14 | A free unit is accepted. |
| M15 | Duplicate ids are accepted. |
| M16 | Two victory buildings are accepted. |
| M17 | The fingerprint rounds `speed_per_point`. |
| M18 | The representability check accepts everything. |
| M19 | An unrepresentable multiplier saturates instead of being refused. |
| M20 | Targeting picks the next row instead of the nearest enemy. |
| R1 | RON `mitigation_per_armor: 1`. |
| R2 | `mass_ripper` `attack_at_army` +1. |

**Result: 52 of 54 test×mutation pairs went red.** Two stayed green:

- **M3 × `loads_units_and_resources_from_ron`.** Expected: that test reads
  `damage_mult` as a float and never touches per-mille, so it does not guard
  rounding.
- **M16 × `the_victory_target_must_be_exactly_one_building`.** Making the
  Foundry a second victory building is already refused by the separate rule
  "a strategy's barracks may not be the victory target". So the
  `victory.len() != 1` check is not the one exercised here. This weakness
  predates F-044: the rewrite only reads the Foundry's cost. It is recorded
  here and not fixed in a balance milestone.

`armor_can_absorb_a_hit_but_never_heals` was vacuous before this entry: the
Worker's offense is 0, so it went green under M1. It is now built on an armed
in-memory Worker and goes red under M1.

### The two suite proofs

**Proof 1: the shipped RON reads exactly as before.** Release, on the final
Step A tree: **935 passed, 5 failed, 1 ignored**. The 5 are the same F-041
reds:
- `b35_tempo` median;
- `critic_b1_ac3` mass×mass resolution;
- `critic_b1_ac3` rush/turtle ordering;
- `critic_b35_closure`;
- `critic_b3_ac2` crippled probe.

**Proof 2: the tests follow the content.** I ran a scratch candidate `p1`
that perturbs almost every lever at once:
- bulwark off/def/arm 5/8/8;
- sentinel off 7, arm 4, cost 75;
- ripper arm 2, train 700;
- ravager off 8, arm 3;
- arclight off 8;
- gather 85 ticks;
- Foundry 160;
- `damage_per_offense` 6, `mitigation_per_armor` 3, `hp_per_defense` 22,
  `damage_mult` 1.4;
- mass `attack_at_army` 11.

Results:
- **Before the rewrite:** 85 failures.
- **After the rewrite:** 918 passed, 22 failed. None of the 22 is a content
  pin. They are:
  - the per-tick and state-hash goldens: b1_matchup, b1_strategies,
    b2_headless, b2_orientation, b35_parallel ×2, b35_queue_depth ×2,
    critic_b1_ac2, critic_b2_ac4, critic_b35_ac0, critic_b35_ac0b bit-identity;
  - the measured readings: b3_gate, b3_pentagon, b35_tempo density;
  - the behavioural design claims that any final RON must satisfy:
    - b1_probe_set and b4_reservation (`synth_steel_flesh` places its script in
      order);
    - the five F-041 reds.

The shipped RON was restored afterwards and checked byte for byte.

## F-045 — B4 tuning, phase 3: from d2 with the combat levers freed; budget exhausted, nothing kept (B4)

This entry records the Step B search that followed F-044.
- It started from F-043's best candidate `d2`.
- F-044 had freed the combat levers.
- It ran on the box after hours under `nice 19`.

**No RON is changed by this entry.** The shipped content is still F-042's,
with F-042's readings and the five F-041 reds.

**Method.**
- Each candidate is a full RON copy.
- Each one ran in its own private mount namespace:
  `unshare -rm` with a bind mount over `assets/data`.
- So candidates ran in parallel, and the shared tree's RON never changed.
- Screens were 1-seed rosters (`balance --seeds 1`, 200 matches). That is
  ±20 points on a row, so they are directional only.
- The best screen was confirmed on F-042's 4-seed roster.

**Compute.**
- Exploration: 23 one-seed rosters.
  - About 3.6 h if each had run alone on the box (≈ 9.5 min each).
  - The OS billed about 2.2x that, because the box has 4 physical cores and
    8 hyperthreads and `balance` runs Bevy's thread pools.
  - Round 1 ran 7 at once and oversubscribed the box; later rounds ran 4 at
    once.
- Confirmation: one 4-seed roster (≈ 40 min) and two red-test runs.

**Tags used in the tables.**
- *band*: matches ending in 5:00–8:00, as a share of all matches (timeouts
  count as long).
- *rip*: the `mass_ripper` row.
- *tur*: the `turtle` row.

### Screens (1 seed)

Reference, `d2` at 1 seed: band 39.5%, rip 75.0, tur 69.4.

**Round 1, single levers on d2:**

| cand | change | band | rip | tur | note |
|---|---|---|---|---|---|
| e1 | turtle `worker_target` 9, `attack_at_army` 22 | 45.5 | 72.2 | 80.6 | |
| e2 | turtle 10 / 24 | **50.5** | 72.2 | 80.6 | 30 of 38 turtle games in band |
| e3 | ripper 50 Alloy | 33.5 | 58.3 | 77.8 | turtle games go long |
| e4 | bulwark 95 Alloy / 1 300 ticks | 45.5 | 69.4 | 77.8 | `mass_bulwark` 38.9, no longer losing |
| e5 | `mvp_gather_ticks` 110 | 37.5 | 69.4 | 55.6 | shifts every match right; a 0-hold pentagon |
| e6 | mass `attack_at_army` 8 (all five) | 36.0 | 66.7 | 69.4 | `sentinel > ripper` fails |
| e7 | ripper train 900 | 37.5 | 69.4 | 63.9 | |

**Round 2, combining:**

| cand | change | band | rip | tur |
|---|---|---|---|---|
| f1 | e2 + e4 | 54.5 | 72.2 | 80.6 |
| f2 | f1 + ripper offense 5 | 54.0 | 66.7 | 88.9 |
| f3 | f1 + turtle army 22 | 54.0 | 69.4 | 83.3 |
| f4 | f2 + turtle army 22 | 52.0 | 63.9 | 91.7 |

**Round 3, on f1:**

| cand | change | band | rip | tur | note |
|---|---|---|---|---|---|
| **g1** | ripper `mvp_attack_ticks` 40 → 50 | **55.5** | 69.4 | 80.6 | |
| g2 | ripper `mvp_attack_ticks` 55 | 55.5 | 66.7 | 83.3 | |
| g3 | mass army 12 | 53.0 | 63.9 | 88.9 | |
| g4 | g3 + ripper 50 ticks | 52.5 | 44.4 | 88.9 | 3 timeouts in `arclight > bulwark` |

**Rounds 4–5, the turtle's script on g1** (`worker_target` / `attack_at_army`
unless stated):

| cand | turtle | band | tur |
|---|---|---|---|
| h1 | 8 / 20 | 50.5 | 83.3 |
| h2 | 8 / 24 | 46.0 | 80.6 |
| h3 | 9 / 20 | 54.5 | 77.8 |
| h4 | 3rd/4th lines at 6 000 / 9 000 | 57.0 | 100.0 |
| i1 | 7 / 16 | 49.0 | 77.8 |
| i2 | 7 / 18 | 48.5 | 72.2 |
| i3 | 8 / 16 | 51.0 | 75.0 |
| i4 | 8 / 18 | 52.5 | 80.6 |

**Red-test checks on the finalists** (release, private mount):
- **h3** fails turtle-latest. On the first closure seed the turtle commits at
  20 730, after `mass_arclight` (20 910) and `mass_bulwark` (21 120).
- **j1** (turtle 9 / 22) clears `mass_bulwark` by only 270 ticks.
- **g1** clears it by 660 (21 780 vs 21 120). F-036's margin was 570.

### What the screens show

1. **The ripper's attack rate was the outlier.**
   - At 40 ticks a 40-Alloy Ripper dealt about 1.1 damage per second per Alloy.
   - Every other unit dealt 0.12–0.43.
   - That made `mass_ripper` strong (77.6% in d2) and HQ kills fast (a short
     tail).
   - At 50 ticks: mass_ripper 61.1% at 4 seeds, and the HQ siege takes longer.
   - A cost nerf (e3, and d4 in F-043) also hit `rush`, the synths and the
     turtle's timing. The attack-rate nerf did not.
2. **Bulwark at 95 Alloy / 1 300 ticks.**
   - `mass_bulwark` is no longer the losing row: 35.4% at 4 seeds, down from
     21.6 losing.
   - Its matches move into the band: 98 of 152 at 4 seeds.
3. **Turtle 10 / 24 puts its matches back in the band.**
   - Under d2 (7 / 20) they ran long: 91 of 152 past 8:00.
   - Under g1 123 of 152 are in band.
4. **The turtle's strength does not respond to its own script.**
   - Every worker/army mix tried landed between 72% and 100%.
   - It wins one way: a single large wave at 6:15–7:30 into mass probes of
     10 units, about 4/4 per cell.
   - It loses only to ripper hits in the first 2–3 minutes, in one seat.
   - Fewer workers delay it and lengthen its games. A smaller army moves its
     commit before `mass_bulwark` and breaks turtle-latest.
   - So on these levers the turtle is a pure trade between K1 and both K3 and
     the F-036 claim. It needs a lever outside its own script.

### g1 confirmed (4 seeds, 800 matches, ≈ 40 min wall, single process)

Command:
`balance --seeds 4 --report /tmp/onus_b4_g1.ron`, on the same seeds as F-042.

**The g1 diff against the shipped content:**
- `bulwark`: 110 → 95 Alloy, train 1 500 → 1 300.
- `ripper`: `mvp_attack_ticks` 40 → 50.
- `ravager`: 90 → 80 Alloy, train 1 200 → 1 100 (from d2).
- All five `mass_*`: 2nd/3rd lines 600 / 900 → 9 000 / 15 000 (from d2; still
  knob-identical).
- `synth_steel_flesh`: 3rd/4th lines 1 200 / 1 500 → 3 000 / 6 000 (from d2).
- `turtle`: `worker_target` 12 → 10, `attack_at_army` 26 → 24.

**Gate: FAIL**, the same as F-042, on K1 `turtle` only.

| reading | g1 | d2 (F-043) | F-042 shipped |
|---|---|---|---|
| K1 | **FAIL, `turtle` 84.0% [75.4, 91.4]** | FAIL, `mass_ripper` 77.6% | FAIL, `turtle` 88.2% |
| `mass_sentinel` | 61.8% undetermined | 66.9% | 66.0% |
| `mass_ripper` | 61.1% undetermined | 77.6% FAIL | 43.1% |
| `synth_triad` | 58.2% undetermined | 56.0% | 52.5% |
| `mass_bulwark` | 35.4% PASS | 21.6% LOSING | 14.0% LOSING |
| `rush` | 19.4% PASS, LOSING | 23.6% LOSING | 62.5% |
| `mvp` | 34.7% PASS | 37.5% | 59.7% |
| K3 band share | **50.6% [46.5, 54.8] undetermined** | 33.9% FAIL | 48.5% undetermined |
| timeouts | 1.5% PASS | 2.0% | 1.0% |
| decided median | 6:23 | 6:17 | 5:58 |
| K2 | undetermined; slot A 58.1%, left 61.9% (n_eff 58) | undetermined | undetermined |
| bulwark > ravager | 81.2% holds | 57.1% | 86.7% holds |
| ravager > sentinel | 37.5% undetermined | 37.5% | **25.0% FAILS** |
| sentinel > ripper | 56.2% undetermined | 37.5% | 56.2% |
| ripper > arclight | 87.5% holds | 93.8% holds | 62.5% |
| arclight > bulwark | 100% holds | 93.8% holds | 85.7% holds |

The other g1 rows: `mass_ravager` 50.8, `mass_arclight` 51.3,
`synth_steel_flesh` 43.2. All PASS.

**The five F-041 red tests under g1** (release, private mount):
- `b35_tempo` median: green; its density test stays green.
- rush-earliest / turtle-latest: green.
- every mass×mass cell resolves: green.
- the crippled-probe premise: green.
- The closure: the ordering holds on the first seed (turtle 21 780 vs
  `mass_bulwark` 21 120, a 660-tick margin). Only F-036's measured pin
  (19 320 / 18 750) is red, and it moves under F-032's licence.
  - The test stops at the first seed's pin, so seeds 1 and 2 were not read.
  - A re-pin would need F-036's "seed-independent" claim re-measured on all
    three seeds.

### Why it was not kept

The brief's bar was:
- the five reds green;
- K1 and K3 not FAIL;
- no pentagon FAIL.

g1 meets every part except K1: the turtle still FAILs, at 84.0%. It is the
best reading of B4 so far:
- K3 no longer FAILs;
- the pentagon has no FAIL;
- `mass_ripper` and `mass_bulwark` are fixed;
- 4 of the 5 reds are green, and the 5th is a measured pin.

The budget ran out with the gate still FAIL, so per the brief it is reported,
not shipped. The candidate is recorded here number for number, so it can be
installed as is.

### What is needed next

1. **The turtle needs a lever outside its own script.** Candidates:
   - give the mass probes more economy (knob-identical `worker_target` 7–8);
     this changes their strength too;
   - a defender's advantage, so a single large wave does not roll a base;
     building HP barely moved length in F-043, but it was never screened for
     strength;
   - accept the turtle as the dominant pole and re-ask whether K1 should gate
     a deliberately extreme probe. That is a gate change, so it is for the
     human, not B4's loop.
2. **K3 is now a seed-count question.** 50.6% [46.5, 54.8] needs about four
   times the sample to resolve.

### The full gate on the committed tree (f4e5409, shipped RON)

The shipped RON was verified by md5 before and after every run:
units `6c288837…`, strategies `28e1eb8a…`.

| step | result |
|---|---|
| `cargo clippy --all-targets -- -D warnings` | exit 0 |
| `cargo test` (debug) | 935 passed, 5 failed, 1 ignored |
| `cargo test --release` | 935 passed, 5 failed, 1 ignored |
| `cargo bench --no-run` | exit 0 |

The 5 failures are F-041's reds, the same in both profiles:
- `b35_tempo` median;
- `critic_b1_ac3` mass×mass;
- `critic_b1_ac3` rush/turtle;
- `critic_b35_closure` turtle;
- `critic_b3_ac2` crippled.

**A stale-build trap, recorded so it is not repeated.** The first debug run
read 905 passed and 35 failed (nemesis on every pair, replay writers that never
decided, moved goldens). A rerun gave the same 35, so the failure was
deterministic.

The cause was not the code, and not the content:
- The debug `libonus` rlib had been built at 2026-10-05 20:54:20 from a
  mutated `src/sim/combat.rs`, during F-044's mutation proof.
- The restored source came back with an older mtime (rsync `-a` keeps the
  Mac's mtime), so cargo saw the source as older than the rlib and never
  rebuilt it.
- Release had been rebuilt since, which is why only debug showed it.

After `touch` on `src/**/*.rs`, the debug suite read 935 / 5, and the rerun of
clippy, release and bench above is from that fresh build.

Rule: after any source mutation on the box, `touch` the restored files
before the next build in *every* profile, not just the one used next.

## F-046 — g1 ships: the golden proof, the re-pin, the closure on every seed, and what the gate says (B4)

The user chose to keep F-045's best candidate `g1` and to carry the turtle
forward as a new AC. This entry records:
- the install;
- the F-032-standard proof that every moved golden moved because the data
  moved;
- the re-pin, old → new;
- the closure re-measured on all three of its seeds;
- the gate reading of the shipped content;
- one F-041 claim that g1 breaks, which is left red for a decision.

### The install

`units.ron` and `strategies.ron` are F-045's g1, number for number: `units.ron`
is byte-identical to the candidate (md5 `4944484a…`). `strategies.ron` differs
only in comments, and is code-identical once comments are stripped. The
comments that quoted the old prices, worker count, line timings and commit
ticks were brought up to date. DESIGN_BRIEF's roster now reads Bulwark
95 Alloy and Ravager 80, and a note under the table gives the three timing
changes. The brief states no script timings, so none were added.

### The precondition: no Rust changed

`git diff 049e9ed ed07d82 -- src benches Cargo.toml Cargo.lock` is empty, and
over `tests` the only change is a one-line comment in `tests/b4_reservation.rs`
(critic, B4). 049e9ed was F-044's test-only unpinning, and everything after it up to
the install is FINDINGS, BALANCE_PLAN, CHANGELOG and the two RON files. The
library every test links is the one F-044 shipped, so the only input that can
move a hash is `assets/data`.

### The proof, both ways, both profiles

Content loads at runtime from `CARGO_MANIFEST_DIR/assets/data`, so the swap is
a file change, not a rebuild. Every run below was preceded by `touch` on
`src/**/*.rs`, the tests and the swapped RON. That was deliberate, after
F-045's stale debug rlib: an `rsync -a`-restored file can look older than the
last build.

| run | binary | `assets/data` | pins | result |
|---|---|---|---|---|
| P1 | current (debug, fresh build) | **pre-g1** | old | **935 passed, 5 failed, 1 ignored** |
| P2 | current (release, fresh build) | **pre-g1** | old | **935 passed, 5 failed, 1 ignored**, the same five |
| N1 | current (debug) | g1 | old | 926 passed, 14 failed, 1 ignored |
| N2 | current (debug and release) | g1 | re-pinned | see the full gate below |

P1 and P2 are F-045's full-gate runs at d50f9a1, whose RON is the pre-g1 RON.
**No golden is among their five failures.** All five are F-041's reds, each a
claim about content, not a pin:
- `b35_tempo` median;
- `critic_b1_ac3` mass×mass;
- `critic_b1_ac3` rush/turtle;
- `critic_b35_closure` turtle;
- `critic_b3_ac2` crippled.

Every golden-bearing suite passes at its old, unedited pin under this binary.

N1's 14 failures on g1 with the old pins:

| kind | tests |
|---|---|
| per-tick `state_hash` / journal goldens (9) | `b1_matchup` default; `b1_strategies` one-barracks default; `b35_parallel` default + single-opening; `b35_queue_depth` default + depth-one; `critic_b2_ac4` probe; `critic_b35_ac0` depth-one tree; `critic_b35_ac0b` shipped matchups |
| a per-box replay pin under `target/` (1) | `critic_b1_ac2` non-default pair, a file pin written by an earlier process; deleted so the next run re-pins it, as its message says |
| measured readings pinned so behaviour shows (3) | `b3_gate` shipped reading; `b3_pentagon` real batch; `critic_b35_closure` F-036 ticks |
| **a claim** (1) | `b4_reservation::every_multi_opening_strategy_places_its_whole_script_in_a_head_to_head`, below |

Four of F-041's five reds are green on g1 with no edit:
- `b35_tempo` median;
- `critic_b1_ac3` mass×mass;
- `critic_b1_ac3` rush/turtle;
- `critic_b3_ac2` crippled.

### The re-pin, old → new

The values were recomputed on the box under g1, in release; the full gate
below confirms them in debug. Each hash was read from the failing assertion
and re-run until green.

**The default matchup at 3 000 ticks** (state in `b1_matchup`,
`b35_parallel`, `b35_queue_depth`, `critic_b2_ac4`; seeds 4 and 11 in
`b1_strategies`; journal in the first three):

| seed | state, old → new | journal, old → new |
|---|---|---|
| 4 | `0xbd74941fb3cae489` → `0x7bc8e24c34c6b32f` | `0x55675b7844c3d493` → `0x22a11bed2624375f` |
| 11 | `0x87008a7d696dd45c` → `0x8542718f99cf7112` | `0xb27b6a664addcfd7` → `0xea6b737b371b6c53` |
| 23 | `0x0fe52558759f6817` → `0x0c79bef6a75d654d` | `0xb86b2ad6d1ed0efa` → `0x31975c1962b79ace` |

**The single-opening fixture** (`b35_parallel` and `b35_queue_depth`):

| match | state, old → new | journal, old → new |
|---|---|---|
| ripper vs bulwark, seed 4 | `0xbc761268e37bdfa6` → `0x8eb242d5fdd1d6ac` | `0x501ed9cfa3548f9b` → `0x4e1604bd46e099f6` |
| bulwark vs ripper, seed 11 | `0x9b7ab550477efe7b` → `0xca63fc4a377554a9` | `0x8bc810501741a3b9` → `0x565da2a66530936a` |

**`critic_b35_ac0`'s depth-one tree** (trace, end state, journal):

| row | trace | end | journal |
|---|---|---|---|
| mvp v mvp, 4 | `3fc03b8f…5f9c` → `8d19e60b…f4d5` | `25156a33…d5f5` → `988ac0b0…e4d2` | `55675b78…d493` → `c2d3d58c…75d5` |
| mass_ripper v mass_arclight, 7 | `cb0d0cef…5cee` → `28b23577…bfdb` | `b52ef814…f447` → `2712cca3…623d` | `8fc66471…ccd4` → `75541b9b…04af` |
| mass_arclight v mass_ripper, 7 | `3771c881…1d1e` → `ed81f894…04c5` | `21e24911…5397` → `e34df633…de55` | `c96cb7bd…8ccb` → `7b14173f…d987` |
| rush v turtle, 13 | `06942a84…0db2` → `dbe8948f…d721` | `9d84c955…b380` → `f7c034ea…e631` | unchanged |
| turtle v rush, 13 | `007df998…9101` → `78316d1b…475d` | `f073adc2…7164` → `7b1744a8…8f92` | `9f41c2f1…59fd` → `69bd3de0…157e` |
| synth_triad v mass_ravager, 101 | `86b641af…5e6b` → `f9be9ff5…c897` | `53106991…4e5d` → `32502549…c768` | `c624d8e6…d7c8` → `7021b0e7…d4b7` |
| mass_bulwark v mass_sentinel, 55 | `2b0e2f23…728a` → `82e3cb31…563c` | `5bc08751…65e2` → `8df30a7f…cbb0` | `5979ac90…e069` → `ace7bc94…174a` |
| mass_sentinel v mass_bulwark, 55 | `64e1c8d4…36ac` → `8d3e9fbc…d682` | `4ecd0c53…157a` → `8035691a…a491` | `a82aa835…a07f` → `eafeec45…59c7` |
| synth_steel_flesh v rush, 99 | `694a4cc5…e61a` → `d4f44e22…03ae` | `b536edde…a937` → `16f170ed…7b61` | unchanged |

The full values are in the test. Note that `mvp v mvp`'s whole-match journal
and the default matchup's 3 000-tick journal were one value before
(`0x5567…d493`) and now differ (`0xc2d3…` against `0x22a1…`). Under g1 the
mvp match takes a decision after tick 3 000 that it did not take before. The
two pins are now separate numbers.

**`critic_b35_ac0b`'s shipped matchups:** all eight rows moved. The F-041
values are kept in the row comments. Rows 5-6 (`mass_arclight` v
`mass_ripper`, both orientations) land back exactly on their **pre-F-041**
values. g1 puts a mass probe's second line at tick 9 000, past the probe's
7 200-tick horizon, so inside that window each side stands one line, as it did
before the opening reservation. That is an independent check that the move is
the scripts' timing and nothing else.

**The measured readings:**

| pin | F-041 | g1 (F-046) |
|---|---|---|
| `b3_gate` K1 rows (bulwark / sentinel / ripper / ravager / arclight) | 26.0 / 78.1 / 68.8 / 34.4 / 42.7 | 28.1 / 56.3 / 68.8 / 43.8 / 53.1 (arclight now undetermined) |
| `b3_gate` K2 slot A / left base | 55.0 / 55.0 | 72.5 / 67.5 (undetermined) |
| `b3_gate` K3 median / band share / timeouts / below / beyond | 6:22 / 55.1% / 2% / 10% / 36% | 7:00 / 41.8% / 2% / 25% / 34% (undetermined) |
| `b3_gate` overall | undetermined | undetermined |
| `b3_pentagon` 2-seed links (b>rav / rav>s / s>rip / rip>arc / arc>b) | 87.5 H / 25.0 U / 62.5 U / 75.0 U / 83.3 U | 75.0 U / 50.0 U / 37.5 U / 87.5 H / 100 H |
| `b3_pentagon` holds / undetermined / fails | 1 / 4 / 0 | 2 / 3 / 0 |

`b3_pentagon`'s claim, that no link reads as broken design, holds unchanged:
0 fails.

### The closure, on every seed it checks

`critic_b35_closure::the_turtle_commits_last_on_every_seed_with_the_claimed_margin`
was re-measured in release on all three of its seeds (`seed_at(0x0070_A71E, k)`
for k = 0, 1, 2). It was not run to the first failure only.

| seed | turtle | latest other | margin |
|---|---|---|---|
| `0x11316781aee2febd` | 21 780 | `mass_bulwark` 21 120 | 660 |
| `0x183b0b169d9d1dd9` | 21 780 | `mass_bulwark` 21 120 | 660 |
| `0x6682cfae81d28347` | 21 780 | `mass_bulwark` 21 120 | 660 |

The ordering holds on every seed, and the ticks are seed-independent, as
F-036 claimed. The full commit order is identical on all three seeds:

| strategy | commit tick |
|---|---|
| `rush` | 750 |
| `mvp` | 3 990 |
| `mass_ripper` | 7 830 |
| `synth_steel_flesh` | 13 710 |
| `mass_sentinel` | 13 950 |
| `synth_triad` | 14 670 |
| `mass_ravager` | 15 870 |
| `mass_arclight` | 20 910 |
| `mass_bulwark` | 21 120 |
| `turtle` | 21 780 |

This pin is a **re-measurement, not an F-032 move**. It was already red under the pre-g1
RON (one of F-041's reds), so F-032's licence does not cover it. Its design claim (turtle
strictly last) is still asserted and green (critic, B4).
The pin moves from (19 320, 18 750) to (21 780, 21 120), and the margin grows
from 570 to 660 ticks. The assertions are unchanged: turtle strictly last, and
the exact pair on every seed. The stale "19 320" comments in `b1_probe_set`,
`critic_b1_ac3` and `strategies.ron` now say 21 780. The F-033 history lines
that say 18 750 were B3.5 facts and stay.

### The g1 reading (F-045's 4-seed confirmation; the shipped content)

The kill gate is **FAIL, on K1 `turtle` only**:

| reading | g1 (shipped) | F-042 (pre-g1) |
|---|---|---|
| K1 | **FAIL, `turtle` 84.0% [75.4, 91.4]** | FAIL, `turtle` 88.2% |
| K2 | undetermined | undetermined |
| K3 | undetermined: band share 50.6% [46.5, 54.8], timeouts 1.5%, median 6:23 | undetermined: 48.5% |
| pentagon | 3 hold / 2 undetermined / **0 fail** | 2 hold / 2 undetermined / 1 fail |

The weak rows:
- **`rush` is LOSING at 19.4%.** It was 62.5% at F-042. The cause is not
  isolated. The likely contributors are all in g1:
  - the rush's own unit, the Ripper, hits less often (cadence 40 → 50);
  - Bulwark and Ravager are cheaper and quicker to stand, so its one-body
    waves meet defenders sooner.

  Separating them would take a one-change-at-a-time screen, which was not
  run.
- `mass_bulwark` is the next weakest at 35.4%. That is a PASS, not flagged
  losing; it was 14.0% LOSING at F-042.

### One F-041 claim g1 breaks — left red, for a decision

`b4_reservation::every_multi_opening_strategy_places_its_whole_script_in_a_head_to_head`
claims that every shipped multi-opening script places **all** its openings
before the match ends, in its own mirror (seed `seed_at(0, 1)`). That is the
gating test of BALANCE_PLAN's ticked Multi-barracks box. Under g1:
- a `mass_sentinel` mirror ends at tick 15 519;
- its third line came due at tick 15 000;
- with 519 ticks of slow income the reservation had not yet covered the
  Foundry's 150 Alloy, so it placed `[1, 1]` of `[1, 1, 1]`.

The reservation still works: the test that pins it on a three-line fixture,
and the "trains nothing while an opening is due" test, are green. What changed
is that g1 schedules the third line late enough for a match to end first.

The test was **not** edited. Weakening it to "every opening due early enough"
would change a claim. The options are:
- re-time g1's late lines so they come due well inside a match, which is a
  content change and needs a re-measure;
- restate the claim, which is a decision about the Multi-barracks AC.

Either way it is for the user.

### The full gate (N2)

Run on the box after `touch` of `src/**/*.rs`, `tests/`, `benches/` and the RON files (mtime
trap, F-045), with the shipped g1 RON (`units.ron` md5 `4944484a47bb444108db8fa89969d0a8`,
`strategies.ron` md5 `917704bb96bdb987eab73b9a334965de`):

| step | result |
|---|---|
| `cargo clippy --all-targets -- -D warnings` | clean (exit 0) |
| `cargo test` (debug) | 939 passed, 1 failed, 1 ignored |
| `cargo test --release` | 939 passed, 1 failed, 1 ignored |
| `cargo bench --no-run` | builds (exit 0) |

The one failure, in both profiles, is the claim test
`tests/b4_reservation.rs::every_multi_opening_strategy_places_its_whole_script_in_a_head_to_head`:
"`mass_sentinel` (A) placed [1, 1] of its scripted [1, 1, 1] by tick 15519 (match over: true)".
On g1 the mass_sentinel mirror ends at tick 15 519, before the third line (scripted at 15 000)
is placed. This is a test *claim*, not a golden, so it was not edited: whether the claim
should hold for a match that ends early, or g1's 15 000 line should move, is the user's call.
Every golden and every measured pin is green. The expected "0 failed" is therefore not met by
exactly this one test.

### The claim, bounded (the user's decision)

The user chose to **bound** the whole-script claim, not drop it, and to leave every RON
value alone. `every_multi_opening_strategy_places_its_whole_script_in_a_head_to_head` now
asserts, for every multi-opening strategy in its own mirror (`seed_at(0, 1)`, both sides):
- what was placed is a **prefix of the script, in script order**;
- every unplaced opening has `at_tick + grace > end tick`, with a message naming the
  opening, its `at_tick`, `grace` and the end tick. Equivalently, every opening due at
  least `grace` ticks before the end is placed;
- **non-vacuity:** at least one strategy places more than one opening on both sides.

`grace` is per opening and **derived, not chosen**. It is the fewest ticks the side can
take to bank the opening's Alloy from an empty stockpile:
`ceil(alloy_cost * mvp_gather_ticks / (worker_target * mvp_carry_capacity))`. Every
term is read from the loaded `Content`: the building's cost, the strategy's
`worker_target`, and the gathering unit's carry and gather ticks.
- This is the best-case income: at most `worker_target` gatherers, each delivering at
  most one load per harvest.
- Walking to the deposit and back is left out, because the deposit distance is fixture
  geometry, not content. Leaving it out only lowers `grace`.
- So `grace` is a lower bound on banking the cost *from an empty stockpile*. It is not a
  bound on the delay from `at_tick` to placement: every first opening goes up at its
  `at_tick` on starting Alloy. It excuses the fewest openings an income model can, which
  keeps the claim as strict as an honest bound allows. The critic checked it against a
  stricter bound (the Alloy each side actually held when the opening came due). That bound
  excuses exactly the same openings.
- **Non-vacuity guard (critic, B4).** At first the guard asked only that some strategy place
  more than one opening. `synth_triad` did that even before B4. It now asks that at least
  one **mass probe** (the strategies F-035 showed stuck on one line) place more than one
  opening on both sides of its mirror. So the guard fails if a retune makes the mass mirrors
  too short for the reservation to be exercised.

On g1 a Foundry's grace with six workers is 1 125 ticks.
- `mass_sentinel`'s third line is due at 15 000 + 1 125 = 16 125, after its mirror ends
  at 15 519. It is excused.
- Its second line is due at 9 000 + 1 125 = 10 125, well inside the match. It is still
  required, and it is placed.

**Proof.** On the box, on g1:
- the bounded test passes in debug and release;
- with `--features no-opening-reservation` it is still **red**: "`mass_bulwark` (A) left
  opening 1 (`foundry`, at_tick 9000) unplaced, but with grace 1125 it was due by tick
  10125 and the match ran to tick 20923 (match over: true); placed [1] of [1, 1, 1]". The
  bound does not hide the failure the reservation fixes. The other three reservation
  tests are red there too, as in F-041.

The full gate after the change was run on the box. Before it, `touch` was run on `src`,
`tests`, `benches` and the RON files:

| step | result |
|---|---|
| `cargo clippy --all-targets -- -D warnings` | clean (exit 0) |
| `cargo test` (debug) | **940 passed, 0 failed, 1 ignored** |
| `cargo test --release` | **940 passed, 0 failed, 1 ignored** |
| `cargo bench --no-run` | builds (exit 0) |

## F-047 — Why the turtle wins: the step-0 measurement (B4)

Step 0 of the turtle plan (BALANCE_PLAN, "Plan (user go-ahead 2026-10-09)"): measure only, with
no behaviour change. **No `src/`, RON or golden is touched.** The instrument is
`tests/b4_turtle_diag.rs`. It plays the standard headless match, reads the world after every
tick (positions, the `AiJournal`, `AttackCooldown`, `Target`, building `Health`) and writes
nothing back, so every match is the match the balance batch plays.

**Sample.** `turtle` against each of the other 9 shipped strategies, in both seats, on the balance
report's first two seeds (`seed_at(0, 0)`, `seed_at(0, 1)`), on the normal map. That is 36
matches, about 115 s in release, so the table test is `#[ignore]`d. Run it with
`cargo test --release --test b4_turtle_diag -- --ignored --nocapture`. One full match
(turtle vs `mass_bulwark`) runs un-ignored as the fast structural check. Shipped g1 content.

**Definitions.** T is the turtle and D is its opponent. "Armed" means `offense > 0`.
- **Home radius** = the farthest barracks `offset` in any shipped script (225) + `engage_range`
  (220) = **445** world units. Both terms are read from content. The bases are 1 500 apart.
- **T's first wave**: the units whose `MoveTarget` equals the `dest` of T's first `Attack`.
  In every match this set equals the journal's `force`, and this is asserted.
- **Arrival**: the first tick a wave member is within `engage_range` of the enemy HQ.
- **A hit**: a tick on which a unit's `AttackCooldown` is reset to its `mvp_attack_ticks`. It
  is classified by the `Target` the unit held. Damage is `damage_per_hit*` for that pair.
- **D's fight at T's base**: the window from D's first `Attack` to T's first `Attack`.
  "Contact" counts the ticks a D unit has a T armed unit within `engage_range`. "On order"
  means the unit holds a `MoveTarget` without `Engaging`, which is a commander's order.

### T's wave at D's base (means per opponent over 2 seeds x 2 seats)

D h/f/d means D's armed units at home / in the field / dead.

| opponent | T wins | wave | D h/f/d at T's commit | D h/f/d at arrival | trade T lost / D lost |
|---|---|---|---|---|---|
| mvp | 3/4 | 24 | 2.0 / 0.0 / 20.0 | 1.3 / 0.7 / 20.0 | 1.3 / 1.0 |
| mass_bulwark | 4/4 | 24 | 0.0 / **10.0** / 0.0 | 0.0 / **9.8** / 0.2 | 1.5 / 0.5 |
| mass_sentinel | 4/4 | 24 | 6.0 / 0.0 / 10.0 | 6.0 / 0.0 / 10.0 | 10.2 / 0.2 |
| mass_ripper | 2/4 | 24 | 3.5 / 0.0 / 30.0 | 4.0 / 0.0 / 30.0 | 1.0 / 3.0 |
| mass_ravager | 4/4 | 24 | 3.5 / 0.0 / 10.0 | 3.8 / 0.0 / 10.0 | 8.2 / 2.8 |
| mass_arclight | 4/4 | 24 | 0.8 / 0.0 / 10.0 | 0.8 / 0.0 / 10.2 | 0.8 / 0.5 |
| synth_steel_flesh | 4/4 | 24 | 2.2 / 0.0 / 18.2 | 2.2 / 0.0 / 18.5 | 9.0 / 3.0 |
| synth_triad | 4/4 | 24 | 2.8 / 1.5 / 16.5 | 2.5 / 0.8 / 17.5 | 10.5 / 2.8 |
| rush | 1/4 | 24 | 0.0 / 0.0 / 21.0 | 0.0 / 0.0 / 21.0 | 0.0 / 0.0 |

The means cover only the matches in which T committed. In the 6 matches it lost (`mvp` 1,
`mass_ripper` 2, `rush` 3), all in seat A, it died to an early hit before it had a wave.

**Reading.** When the turtle's 24 arrive, D's army is **already dead, not in the field**. For
8 of 9 opponents, D's field count at arrival is 0.0 to 0.8. At most 0.8 to 6.0 units are
home. Nearly every armed unit D ever fielded died before the turtle moved, and the
before-commit columns below say where. **D sent them at the turtle, and they died within the
home radius of T's HQ**:

| opponent | D units sent before T's commit | dead by then | died near T's HQ |
|---|---|---|---|
| mvp | 17.2 | 15.0 | 14.2 |
| mass_sentinel / ravager / arclight | 10.0 | 10.0 | 10.0 |
| mass_ripper | 22.0 | 17.2 | 17.2 |
| synth_steel_flesh | 18.2 | 18.2 | 18.2 |
| synth_triad | 18.0 | 16.5 | 16.5 |
| rush | 11.8 | 9.2 | 8.0 |

The one exception is **`mass_bulwark`**. Its 10 Bulwarks were ordered out at tick 21 121.
They walk at speed 2, so at the turtle's commit (21 781) and at its arrival they are all still
on the road, and the turtle's wave meets an empty base: **field, 4 of 4 matches**.

### D's wave at T's base: the decisive fight (means)

Dal / Tal are D's sent units and T's armed units alive at D's arrival, and D HP / T HP are
their HP pools. D hits are D's sent units' hits on T's army, workers and buildings. The last
two columns are damage to units.

| opponent | Dal | Tal | D HP | T HP | D lost | T lost | contact on order | D hits army / worker / bldg | T hits | D dmg | T dmg |
|---|---|---|---|---|---|---|---|---|---|---|---|
| mvp | 3.0 | 1.0 | 380 | 100 | 15.0 | 5.2 | 22% | 29.8 / 30.0 / 118.8 | 134.2 | 605 | 1 918 |
| mass_sentinel | 9.8 | 10.0 | 975 | 960 | 10.0 | 3.5 | 24% | 27.0 / 11.2 / 14.5 | 67.5 | 878 | 1 208 |
| mass_ripper | 9.8 | 2.8 | 585 | 205 | 17.2 | 5.2 | 18% | 35.5 / 49.0 / 174.0 | 46.0 | 862 | 1 415 |
| mass_ravager | 9.8 | 14.0 | 1 560 | 1 360 | 10.0 | 3.5 | 45% | 16.5 / 7.2 / 13.2 | 83.0 | 533 | 1 877 |
| mass_arclight | 9.0 | 22.0 | 360 | 2 130 | 10.0 | 2.5 | 73% | 13.0 / 0.5 / 0.5 | 52.5 | 583 | 1 778 |
| synth_steel_flesh | 8.8 | 10.8 | 885 | 1 005 | 18.2 | 3.5 | **85%** | 22.0 / 1.0 / 2.5 | 134.0 | 465 | 2 350 |
| synth_triad | 8.2 | 13.2 | 555 | 1 315 | 16.5 | 3.0 | 75% | 12.5 / 4.8 / 3.8 | 75.8 | 405 | 1 879 |
| rush | 1.0 | 0.0 | 60 | 0 | 9.2 | 2.0 | 47% | 8.8 / 14.0 / 150.8 | 19.5 | 199 | 711 |

This is where the turtle wins. **At rough parity the attacker is wiped out and the turtle
loses about a third as many.**
- `mass_sentinel`: 10 v 10, 975 v 960 HP. D lost 10, T lost 3.5.
- `mass_ravager`: D had *more* HP (1 560 v 1 360). D lost 10, T lost 3.5.
- `synth_steel_flesh`: 9 v 11, about equal HP. D lost 18, T lost 3.5.

Only `mass_arclight` is simply outnumbered: it commits at 20 911, when the turtle already
stands 22. `mass_ripper` is the reverse case. It *outnumbers* the turtle at arrival, 9.8 to
2.8, yet across its three groups it loses 17.2 of 22 to the turtle's 5.2. 86% of its hits go
into buildings and workers. That is enough to raze the HQ only in the two seat-A matches
(the 4 200 and 9 240 building HP), and those are the cells the turtle loses. Two mechanisms
show in the hit columns.
1. **Attackers under a commander order do not fight back.** 18% to 85% of D's contact ticks
   (a T unit within `engage_range`) are spent on the commander's `MoveTo`. The share is
   highest for the synths, whose mixed speeds string the wave out on the road. Over the
   fight D lands 0.16x to 0.77x T's hits on armed targets, and 0.20x to 0.73x its damage.
2. **The attackers' hits go to the wrong targets.** `mass_sentinel` puts 49% of its hits into
   workers and buildings, and `mass_ripper` 86%. T's idle defenders have no such choice:
   their nearest enemy is the attacker.

Trickling (many small attacks) is real only for `mvp` (attack at 3, 6 to 17 orders) and
`rush` (attack at 1). The turtle already wins those cells least often (3/4 and 1/4). The
mass probes and synths attack as one group of 9 to 10: 2 or 3 orders, re-sending the same
units.

### The code responsible

- `src/sim/combat.rs:423`, `else if row.engaging || !row.has_move_target`. A unit holding
  a commander's `MoveTarget` never chases. It hits only if its nearest enemy is already
  within its own `mvp_attack_range` (line 411), and otherwise keeps walking to `dest` while
  being shot. An idle defender acquires anything within `engage_range` (220) and chases it
  out to `pursue_range` (400). `apply_commands` removes `Engaging` on every `MoveTo`
  (`src/sim/mod.rs:779`), so each re-send also cancels any chase in progress.
- `src/sim/combat.rs:345` and `:383`. The target is the **nearest enemy row**, and the
  layout holds every row: units, unarmed workers and buildings alike. An attacker walking
  into a base spends hits on the HQ, the barracks and the miners. A defender's nearest
  enemy is the attacker.
- `src/sim/ai.rs:589` to `:613`. Every wave sends every armed unit, and there is no defend
  step. So a side that has attacked has nothing at home, and only the side that has *not*
  attacked fights from the idle (chasing, well-targeted) posture. F-046 shows the turtle
  commits last against every opponent, so it is the one strategy that always defends
  first and attacks second. It collects the defender's exchange every time, and then rolls
  a base whose army it has already killed.

### Which lever the pattern points at

- **Mostly in the field → home defence.** Only `mass_bulwark` (4 of 36 matches). A recall
  helps only that cell, and at speed 2 a recalled Bulwark army is slow to get home.
- **Trickling and dying piecemeal → attack cohesion.** Only `mvp` and `rush`, which are
  already the turtle's weakest cells. The mass probes and synths already leave as one group.
- **At home but still crushed → armed buildings.** Not the pattern. D has almost nothing at
  home when T arrives. Armed buildings would also arm the turtle's base during the decisive
  fight, which is the fight the turtle already wins.
- **Something else: the attacker's engagement rule.** The decisive fight is D's wave dying
  at parity in the turtle's base, before the turtle ever moves.

**Recommendation: an attack-move engagement rule, which is not one of the three
pre-approved levers.** A unit on an AI attack order would acquire and fight enemy *armed*
units within `engage_range`, as an idle unit does, and then resume its march to `dest`. The
nearest-target choice would prefer armed units over workers and buildings. The evidence:
- the parity exchanges of 10 : 3.5 (`mass_sentinel`, `mass_ravager`) and 18 : 3.5
  (`synth_steel_flesh`);
- 18% to 85% of contact time spent marching under an order;
- up to 86% of attacker hits spent on non-combatants.

The rule is symmetric. It would equally sharpen the turtle's own wave, but that wave
already meets near-empty bases (the trade table above), so the change removes the edge
the turtle alone collects.

Implementing it changes `sim::combat` targeting (and possibly an order flag set by
`sim::ai`). It needs no new RNG, it keeps the snapshot and entity-bits ordering, and any
new number (e.g. an attack-move acquisition range) goes in the RON. It sits outside step 1's
named levers, so **it needs the user's go-ahead.** If the user prefers to stay within the
three, the data rank them home defence (one cell), then cohesion (two already-weak cells),
then armed buildings (likely counterproductive). None of them addresses the fight that
decides the matchup.

**Caveats.**
- 36 matches on the normal map only. These are mechanism readings, not win rates.
- A hit whose target died on that tick is classified by the target the unit held the
  previous tick.
- The D-fight window runs to T's commit, so for `mvp` and `rush` it also takes in later
  trickles.

### The gate

Run on the box after `touch` of `src`, `tests`, `benches` and the RON files (mtime trap,
F-045), with the shipped g1 RON unchanged:

| step | result |
|---|---|
| `cargo clippy --all-targets -- -D warnings` | clean (exit 0) |
| `cargo test` (debug) | **945 passed, 0 failed, 2 ignored** |
| `cargo test --release` | **945 passed, 0 failed, 2 ignored** |
| `cargo bench --no-run` | builds (exit 0) |

The baseline at 88436a9 is 944 passed / 1 ignored: F-046's 940, plus the four critic
probes in `critic_b4_probes`. The difference is exactly this entry's two tests:
- the one-match structural check passes;
- the 36-match table is the second ignored test.

## F-048 — Attack-move built (step 1); measured with `attack_move` everywhere, bar not met (B4)

**Context.** The user chose attack-move (2026-10-09, from F-047). Step 1 builds it behind a
content switch with every shipped strategy on `move`, which must reproduce today's goldens.
Step 2 measures it with `attack_move` on every strategy, in scratch content outside the
tracked tree, against the B4 turtle bar:
- turtle under 65% or undetermined;
- no new K1 FAIL and no pentagon FAIL;
- K3 not FAIL;
- the turtle still commits last.

It allows at most 3 adjustments of the mechanism's own number.

### What was built (step 1, ec57cce; its own number, 708a8a4)

- **`Order::AttackMove { units, dest }`** (`sim::mod`, `sim::combat`). Each unit gets
  `AttackMoveDest(dest)` and a move target. While marching, it acquires enemy *armed* units
  within `mvp_combat.attack_move_range`, preferring them over workers and buildings. The
  tie-break is the existing one: nearest, then smallest snapshot index, ordered by entity
  bits. It chases on the `pursue_range` leash (`Engaging`). When the target dies or the
  leash breaks, it resumes the march to `dest`.
- **Unchanged:**
  - idle units keep the nearest-target rule;
  - `MoveTo` is unchanged (a pure move);
  - no new RNG and no HashMap.
- **State hash.** Rows for `AttackMoveDest` and `Engaging` are added only when those
  components are present, so `move` content hashes exactly as before.
- **Replay.** `LoggedOrder::AttackMove` is in the command log and replays.
- **Journal.** The AI journal's `AiAction::Attack` is deliberately unchanged, because its
  Debug text is in the goldens. The command log records which order a wave got.
- **`wave_order: move | attack_move`.** A required per-strategy field in `strategies.ron`.
  The AI issues each wave per the field. Every shipped strategy is `move`. The field is in
  the content fingerprint.
- **`mvp_combat.attack_move_range`** (708a8a4). A required RON number in the
  fingerprint. Validation requires it to be finite, at least the longest unit
  `mvp_attack_range` (120, the arclight), and at most `pursue_range` (400). Shipped at 220,
  equal to `engage_range`. It is used only by attack-movers.
- **Step 1 tests.**
  - `tests/b4_attack_move.rs` (16 tests):
    - an attack-mover engages an armed enemy that a `MoveTo` unit walks past;
    - it resumes its march and reaches `dest` after the fight;
    - it prefers an armed unit over a nearer worker or building;
    - idle units keep the pure nearest-target rule;
    - an attack-moving wave still razes the HQ when nothing armed is left;
    - a `MoveTo`/`Gather` order ends an attack-move;
    - `MoveTo` stays a pure move beside an enemy in engage range;
    - every shipped strategy ships `wave_order: move`;
    - a missing `wave_order` is a load error, not a default;
    - `attack_move` parses and moves the fingerprint;
    - the AI issues the wave order its strategy names;
    - an attack-move match is deterministic and replays tick for tick;
    - the four `attack_move_range` tests: required; out of bounds refused (both bounds
      inclusive, NaN refused); moves the fingerprint; acquires only inside the range (red
      before combat read the field).
  - Two spatial L1 tests in `sim::spatial` for the new filtered nearest-enemy query: it
    matches a filtered brute force, and ties break to the smallest index.

**Golden proof.** With every strategy on `move`, the only test-tree diffs are 30 added
`wave_order: move` lines in RON fixtures and inline fixture strings (b1_matchup,
b1_strategies, b35_queue_depth, b35_parallel, critic_b1, critic_b1_ac2, critic_b35_ac0,
critic_b35_ac0b, and `critic_b4_data/{pre_g1,pre_b4}/strategies.ron`). Six of them are single-line format
strings re-emitted with the field inserted. The rest of the diff is the commented
`attack_move_range: 220.0` line in the two `critic_b4_data/*/units.ron` copies, as in
the shipped `assets/data/units.ron`. No assertion, golden hash, pinned journal or
expected value was touched, and every existing test passes.

### Tooling (ec1f14d)

- **`balance --data DIR`** is opt-in. It loads the content from DIR, and a DIR that does
  not load fails the run; it never falls back to the shipped content. The report's
  `content_hash` is the DIR's. Covered by `tests/b4_balance_data.rs` (4 tests).
- **`tests/b4_turtle_diag.rs`** reads `ONUS_DIAG_DATA`, follows `AttackMoveDest` as the
  wave destination, and adds an `#[ignore]` reading, `commit_order_on_the_diag_content`.

### The measurement (box, release, `--seeds 4`, 800 matches each)

Each candidate is the shipped g1 RON with `wave_order: attack_move` on all 10 strategies.
They differ only in `attack_move_range`:
- c0 = 220 (the first measurement, no range field; behaviour is identical to 220);
- a1 = 170;
- a2 = 120 (the validation floor);
- a3 = 400 (the ceiling, equal to `pursue_range`).

The scratch directories are `~/onus_scratch/am{,_r170,_r120,_r400}` on the box.

| reading | F-046 g1 (move) | c0 220 | a1 170 | a2 120 | a3 400 |
|---|---|---|---|---|---|
| turtle | **84.0 FAIL** [75.4, 91.4] | 49.1 [38.9, 59.2] | 49.7 [39.6, 59.8] | 53.4 [43.4, 63.5] | 48.5 [38.4, 58.6] |
| mvp | 34.7 | 66.7 U | 66.7 U | 66.7 U | 66.7 U |
| mass_bulwark | 35.4 | 8.6 LOSING | 7.8 LOSING | 2.2 LOSING | 7.4 LOSING |
| mass_sentinel | 61.8 | **78.7 FAIL** [69.4, 86.9] | **77.8 FAIL** | **78.5 FAIL** | **79.2 FAIL** [70.0, 87.3] |
| mass_ripper | 61.1 | **100.0 FAIL, DOMINANT** [96.1, 100] | **100.0 FAIL, DOM.** | **100.0 FAIL, DOM.** | **100.0 FAIL, DOM.** |
| mass_ravager | 50.8 | 68.2 U | 70.8 U | 68.5 U | 74.4 U [64.3, 83.6] |
| mass_arclight | 51.3 | 4.6 LOSING | 4.8 LOSING | 11.1 LOSING | 5.1 LOSING |
| synth_steel_flesh | 43.2 | 42.0 | 40.7 | 39.0 | 37.4 |
| synth_triad | 58.2 | 27.9 | 27.0 | 25.8 | 27.2 |
| rush | 19.4 LOSING | 54.2 | 54.9 | 54.9 | 54.2 |
| **K1** | FAIL (turtle) | **FAIL** (sentinel, ripper) | **FAIL** (same) | **FAIL** (same) | **FAIL** (same) |
| **K2** | undetermined | undetermined | **FAIL** (sentinel mirror, left base 0.0 [0, 39.9]) | undetermined | **FAIL** (ravager mirror, left base 0.0 [0, 43.1]) |
| **K3** band share | 50.6 U | **32.2 FAIL** [28.4, 36.3] | **32.5 FAIL** | **32.8 FAIL** | **33.4 FAIL** [29.5, 37.5] |
| timeouts | — | 3.9% | 3.4% | 3.5% | 4.6% |
| decided median | — | 5:01 | 5:00 | 4:54 | 5:01 |
| pentagon (hold / U / fail) | 3 / 2 / 0 | 1 / 2 / **2** | 1 / 2 / **2** | 2 / 1 / **2** | 1 / 2 / **2** |
| bulwark > ravager | 81.2 H | **0.0 FAILS** | **0.0 FAILS** | **0.0 FAILS** | **0.0 FAILS** |
| ravager > sentinel | 37.5 U | 66.7 U | 68.8 U | 56.2 U | 62.5 U |
| sentinel > ripper | 56.2 U | **0.0 FAILS** | **0.0 FAILS** | **0.0 FAILS** | **0.0 FAILS** |
| ripper > arclight | 87.5 H | 100 H | 100 H | 100 H | 100 H |
| arclight > bulwark | 100 H | 41.7 U | 42.9 U | 93.3 H | 46.2 U |

U = undetermined. The rush mirror is 0 of 8 decided under attack-move in every candidate
(all timeouts).

**Against the bar.**

| bar item | c0 | a1 | a2 | a3 |
|---|---|---|---|---|
| turtle under 65% or undetermined | pass | pass | pass | pass |
| no new K1 FAIL | **fail** (sentinel, ripper) | **fail** | **fail** | **fail** |
| no pentagon FAIL | **fail** (2) | **fail** (2) | **fail** (2) | **fail** (2) |
| K3 not FAIL | **fail** | **fail** | **fail** | **fail** |
| turtle commits last | pass | pass | pass | pass |

**No candidate passes.** Step 2 is not met, and the adjustment budget (3) is spent.

**Turtle commits last: holds, and structurally so.** With `commit_order_on_the_diag_content`
on c0, the slot-A first-`Attack` ticks in each mirror are identical to F-046 on all three
closure seeds:
- rush 750, mvp 3990, mass_ripper 7830, synth_steel_flesh 13710, mass_sentinel 13950;
- synth_triad 14670, mass_ravager 15870, mass_arclight 20910, mass_bulwark 21120;
- turtle 21780.

This cannot differ for any candidate. Attack-move acts only after a side's first `Attack`,
and in a mirror nothing interacts before the first wave.

### The F-047 diagnostic re-run under c0 (36 matches)

**Turtle wins 16 of 36** (F-047: 30 of 36). By opponent:
- 0 of 4: mvp, mass_ripper, rush;
- 1 of 4: mass_sentinel, mass_ravager;
- 3 of 4: synth_steel_flesh, synth_triad (the fourth triad match is a cap);
- 4 of 4: mass_bulwark, mass_arclight.

The defender's wave no longer walks through fire: its "order%" (contact time spent
marching) drops to 0 for most opponents. The trade at the turtle's base, as mean armed
lost (D lost vs turtle lost):

| opponent | F-047 (move) | c0 (attack_move) |
|---|---|---|
| mass_sentinel | 10 vs 3.5 | 9.5 vs 20.8 |
| mass_ravager | 10 vs 3.5 | 9.2 vs 22.2 |
| synth_steel_flesh | 18.2 vs 3.5 | 29.8 vs 28.8 |
| synth_triad | 16.5 vs 3.0 | 31.5 vs 29.0 |
| mass_ripper | 17.2 vs 5.2 | 1.2 vs 3.5 |

`mass_bulwark`'s 10 still never reach the turtle's base. They meet the turtle's wave on
the road and lose 11 to the turtle's 5.2, so bulwark (speed 2, the latest non-turtle
committer) is still crushed.

### Reading

- **Attack-move fixes the turtle, the cell it was chosen for.** Turtle 84.0 FAIL becomes
  roughly 49 to 53 PASS, and the parity exchanges in F-047 invert.
- **It flips the tempo of the whole field.** Under `move`, committing last won, because
  the defender collected the exchange. Under `attack_move` the attacker collects it, so
  early committers win:
  - mass_ripper reaches 100%, and mass_sentinel and mvp land around 67 to 79%;
  - rush reaches 54% and is no longer LOSING;
  - the late committers collapse: bulwark 2 to 9%, arclight 5 to 11%.

  That flip breaks two pentagon edges. Bulwark > ravager and sentinel > ripper go to 0%,
  because the prey commits first and wins the exchange. Matches also end before the band:
  about 47% of all matches end before 5:00, and K3 FAILs.
- **The range is not the lever.** From the floor (120) to the leash (400), every gated
  reading moves within its interval, and no FAIL changes status. The effect comes from the
  engagement rule itself (fight while marching, armed first), not from how far an
  attack-mover looks.
- **The F-047 recommendation was right about the mechanism and wrong about the outcome.**
  It predicted that a symmetric rule would remove only the turtle's edge. Instead it
  moved the edge to the earliest committers. Fixing this would take levers outside this
  step's budget: unit numbers, commit timing per strategy, or a mixed `wave_order` per
  strategy. Each is a content decision the user owns.

**Fallback (per BALANCE_PLAN):** bring these numbers and the gate question to the user.
Nothing ships. Every shipped strategy stays `wave_order: move`, so shipped behaviour and
every golden are unchanged, and step 3 (re-pin) is not started. The open question: should
K1 gate a deliberately extreme probe? Should attack-move ship for some strategies only?
Or should the turtle be addressed another way?

### The gate

Run on the box after `touch` of `src`, `tests`, `benches` and the RON files (mtime trap,
F-045):

| step | result |
|---|---|
| `cargo clippy --all-targets -- -D warnings` | clean (exit 0) |
| `cargo test` (debug) | **967 passed, 0 failed, 3 ignored** |
| `cargo test --release` | **967 passed, 0 failed, 3 ignored** |
| `cargo bench --no-run` | builds (exit 0) |

The count against F-047 (945 / 2) is +22 passed and +1 ignored:
- 16 in `b4_attack_move`;
- 2 spatial L1;
- 4 in `b4_balance_data`;
- the extra ignored test is `commit_order_on_the_diag_content`.

## F-049 — Attack-move ships everywhere, on retuned RON: step 2b passes at 4 seeds with s56 (B4)

**Context.** The user's call (2026-10-10, BALANCE_PLAN step 2b, 757498a): ship
`wave_order: attack_move` on all 10 strategies and retune the RON around it, starting
from F-048's c0 (g1 plus attack_move everywhere, `attack_move_range` 220). The step-2
bar applies in full:
- turtle under 65% or undetermined;
- no K1 FAIL and no pentagon FAIL;
- K3 not FAIL;
- the turtle still commits last.

Only RON may change. The five mass probes stay knob-identical. The budget is at most 10
candidates confirmed at 4 seeds, screened cheaply first (1 or 2 seeds). If no candidate
passes by the cap: stop, install nothing, report the best.

**Outcome.** **s56 passes at 4 seeds**: K1 undetermined (highest row `mass_ripper`
70.8 [60.6, 80.2]); turtle 32.3% PASS; pentagon 4 hold / 1 undetermined / 0 fail; K3
undetermined (band share 48.8% [44.6, 53.1], timeouts 4.2%); the turtle commits last
(22 980, 6 900 ticks after `synth_triad`, on all three closure seeds). s55 passes too.
Six of the ten 4-seed confirmations were used. s56 is installed, and its goldens and
measured pins are re-pinned. **It breaks 10 tests' claims or premises**, left red for a
decision. Among them is M4c's 8-minute claim: the `mvp` mirror now runs past 8:00 on 3 of 21
seeds. The five-probe `b3_gate` batch's K3 also reads FAIL. Both are set out below.

### How the screen was run

- Every candidate is a scratch content directory on the box (`~/onus_scratch/sN`),
  read by `balance --data DIR` (F-048's tooling). No tracked file changed until install.
- Screens are 1 seed (200 matches) or 2 seeds (400); confirmations are `--seeds 4`
  (800 matches), release. 2-seed runs use the first two seeds of the 4-seed runs.
- Each candidate is its parent plus the listed change; `b0` is F-048's c0 (shipped g1
  RON, all 10 strategies on `attack_move`, `attack_move_range` 220).
- Under 4 seeds (n_eff about 90-95 per row) a K1 row FAILs from about 74.5% and K3's
  band share FAILs below about 46%. 1-seed screens can show FAIL at 1 seed for rows
  that only reach "undetermined" at 4.

### The candidates (row %, 1 seed unless marked)

Columns: mvp, mass_bulwark, mass_sentinel, mass_ripper, mass_ravager, mass_arclight,
synth_steel_flesh, synth_triad, rush, turtle; then K3 band share and timeouts.
F = the row FAILs K1. The pentagon column lists only links that do not hold.
"nem" = nemesis `damage_mult`; "bhp" = `building_hp_per_defense`; "probe lines" = the
probes' second and third openings; "probe atk" = the probes' `attack_at_army`.

| cand | change (on its parent) | mvp | bul | sen | rip | rav | arc | stl | tri | rsh | TUR | K3 | TO | pentagon not holding |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| F-048 c0 (4 s) | — | 66.7 | 8.6 | **78.7F** | **100F** | 68.2 | 4.6 | 42.0 | 27.9 | 54.2 | 49.1 | **32.2F** | 3.9 | bul>rav 0 F, sen>rip 0 F |
| s1 | b0 + nem 2.0, ripper train 720→1000, bhp 420→630 | 77.8 | 13.9 | 72.2 | **88.9F** | 61.1 | 11.1 | 47.2 | 41.7 | 13.9 | 72.2 | **35.1F** | 3.0 | bul>rav 50 U, sen>rip 0 F |
| s2 | b0 + nem 2.0, ripper train 1000 | 77.8 | 13.9 | 75.0 | **88.9F** | 61.1 | 11.1 | 47.2 | 41.7 | 13.9 | 69.4 | **35.1F** | 3.0 | bul>rav 50 U, sen>rip 0 F |
| s3 | s2 + ripper attack_ticks 50→70 | 77.8 | 16.7 | 77.8 | **88.9F** | 66.7 | 11.1 | 47.2 | 41.7 | 11.1 | 61.1 | **41.5F** | 2.5 | bul>rav 50 U, sen>rip 0 F |
| s4 | s3 + probe lines 9000/15000→18000/24000 | 77.8 | 29.6 | **88.9F** | 69.4 | 75.0 | 13.9 | 28.7 | 38.9 | 11.1 | 66.7 | **35.9F** | 4.0 | — |
| s5 | s4 + bhp 1260 | 77.8 | 29.6 | **86.1F** | 58.3 | 71.3 | 11.1 | 40.7 | 41.7 | 11.1 | 72.2 | 47.3 | 6.0 | — (K2 FAIL) |
| s6 | s5 + sentinel cost 70→85 | 66.7 | 33.3 | 71.3 | 75.0 | 75.0 | 25.0 | 27.8 | 41.7 | 11.1 | 73.1 | 44.6 | 8.0 | sen>rip 0 U (K2 FAIL) |
| s7 | s4 + bhp 840, sentinel 77, ravager cost 80→85 | 77.8 | 30.6 | 75.0 | 75.0 | 69.4 | 13.9 | 33.3 | 38.9 | 11.1 | 75.0 | 45.7 | 6.0 | sen>rip 0 U |
| s8 | s7 + turtle workers 10→9 | 77.8 | 33.3 | 75.0 | 77.8 | 75.0 | 13.9 | 33.3 | 38.9 | 11.1 | 63.9 | 44.6 | 7.0 | sen>rip 0 U |
| s9 | s7 + bhp 1050, arclight defense 2→3 | 66.7 | 30.6 | 75.0 | 75.0 | 66.7 | 22.2 | 33.3 | 41.7 | 11.1 | 77.8 | 47.9 | 6.0 | sen>rip 0 U (K2 FAIL) |
| s10 | s7 + arclight cost 80→70, defense 3 | 66.7 | 30.6 | 75.0 | 75.0 | 69.4 | 36.1 | 25.0 | 38.9 | 11.1 | 72.2 | 46.3 | 6.0 | sen>rip 0 U |
| s11 | s10 + turtle workers 9, steel attack_at 9→13 | 66.7 | 25.0 | 75.0 | 77.8 | 72.2 | 36.1 | 42.6 | 44.4 | 11.1 | 49.1 | 46.8 | 5.0 | sen>rip 0 U |
| s12 | s11 + triad attack_at 9→12 | 66.7 | 25.0 | 75.0 | 77.8 | 72.2 | 36.1 | 48.1 | 33.3 | 11.1 | 54.6 | 50.0 | 5.0 | sen>rip 0 U |
| **s12 (4 s)** | confirmation 1 | 66.7 | 26.5 | **83.4F** | **75.9F** | **77.1F** | 37.6 | 38.3 | 38.6 | 11.1 | 44.7 | 51.1 | 6.4 | sen>rip 57.1 U |
| s13 | s12 + ripper train 1150 | 66.7 | 25.0 | 82.4 | 59.3 | 75.0 | 36.1 | 42.6 | 41.7 | 11.1 | 60.2 | 45.3 | 4.0 | sen>rip 66.7 U |
| s14 | s13 + sentinel 80, steel/triad workers 8→9 | 66.7 | 25.0 | 72.2 | 58.3 | 72.2 | 33.3 | 59.3 | 50.0 | 11.1 | 51.9 | 44.3 | 4.0 | sen>rip 0 F |
| s15 | s13 + sentinel attack_ticks 60→66, steel/triad workers 9, bhp 735 | 66.7 | 22.2 | 79.6 | 50.9 | 72.2 | 33.3 | 59.3 | 50.0 | 11.1 | 54.6 | 43.7 | 5.0 | sen>rip 66.7 U |
| s16 | s14 + bulwark offense 4→5 | 55.6 | 56.5 | 72.2 | 44.4 | 72.2 | 33.3 | 54.6 | 50.0 | 11.1 | 50.0 | **38.4F** | 5.0 | sen>rip 0 F |
| s17 | s16 + ripper offense 6→5 | 55.6 | 61.1 | 83.3 | 22.2 | 83.3 | 38.9 | 47.2 | 50.0 | 11.1 | 47.2 | **40.6F** | 6.5 | sen>rip 100 U |
| s18 | s16 + arclight attack_ticks 100→85, bhp 700 | 55.6 | 56.5 | 72.2 | 44.4 | 72.2 | 41.7 | 49.1 | 50.0 | 11.1 | 47.2 | **33.5F** | 4.5 | sen>rip 0 F |
| s19 | s18 + sentinel 77 | 55.6 | 63.9 | 79.6 | 39.8 | 72.2 | 41.7 | 38.9 | 50.0 | 11.1 | 47.2 | **39.8F** | 4.5 | sen>rip 66.7 U |
| s20 | s19 + probe workers 6→7, ravager 90 | 55.6 | 69.4 | **88.9F** | 36.1 | 72.2 | 44.4 | 38.9 | 38.9 | 11.1 | 44.4 | 46.9 | 2.0 | — (K2 FAIL) |
| s21 | s18 + probe workers 7, ravager 90, ripper train 1300 | 55.6 | 72.2 | **88.9F** | 22.2 | **86.1F** | 41.7 | 44.4 | 36.1 | 11.1 | 41.7 | 50.3 | 3.5 | — |
| s22 | s19 + arclight attack_ticks 100, defense 4 | 55.6 | 63.9 | 79.6 | 39.8 | 72.2 | 41.7 | 38.9 | 47.2 | 11.1 | 50.0 | **36.6F** | 4.5 | sen>rip 66.7 U |
| s23 | s22 + probe workers 7 | 55.6 | 69.4 | **88.9F** | 27.8 | **88.9F** | 49.1 | 36.1 | 30.6 | 11.1 | 42.6 | 49.7 | 2.5 | — (K2 FAIL) |
| s24 | s22 + probe atk 10→8 | 55.6 | 72.2 | **88.9F** | 25.0 | **88.9F** | 50.0 | 38.9 | 27.8 | 11.1 | 41.7 | 51.8 | 2.5 | — |
| s25 | s24 + sentinel 82, ravager 90 | 55.6 | 72.2 | **88.9F** | 33.3 | **86.1F** | 50.0 | 36.1 | 30.6 | 11.1 | 36.1 | 49.5 | 3.0 | — |
| s26 | s24 + sentinel 90, ravager 95 | 55.6 | 82.4 | 72.2 | 30.6 | 82.4 | 52.8 | 42.6 | 30.6 | 11.1 | 39.8 | 52.6 | 5.0 | — |
| s27 | s24 + sentinel attack_ticks 70, ravager attack_ticks 85 | 55.6 | 72.2 | **88.9F** | 25.0 | 83.3 | 50.0 | 41.7 | 30.6 | 11.1 | 41.7 | 51.0 | 3.0 | — |
| s28 | s26 + ripper train 1000 | 55.6 | 82.4 | 66.7 | 72.2 | 68.5 | 50.0 | 36.1 | 22.2 | 11.1 | 35.2 | 46.8 | 7.0 | sen>rip 50 U |
| s29 | s26 + bulwark train 1300→1500, ravager train 1100→1250 | 55.6 | 82.4 | 72.2 | 36.1 | 77.8 | 52.8 | 41.7 | 30.6 | 11.1 | 39.8 | 49.7 | 4.5 | — |
| s30 | s28 + bulwark cost 95→110 | 55.6 | 48.1 | 83.3 | 83.3 | 75.0 | 52.8 | 26.9 | 38.9 | 11.1 | 25.0 | 43.0 | 7.0 | sen>rip 50 U |
| s31 | s28 + bulwark armor 9→8 | 55.6 | 82.4 | 69.4 | 72.2 | 69.4 | 50.0 | 36.1 | 22.2 | 11.1 | 31.5 | 44.9 | 7.5 | sen>rip 50 U |
| s32 | s28 + bulwark cost 100 | 55.6 | 80.6 | 69.4 | 72.2 | 75.0 | 50.0 | 25.0 | 30.6 | 11.1 | 30.6 | 44.7 | 6.0 | sen>rip 50 U |
| s33 | s28 + bulwark cost 104 | 55.6 | 61.1 | 80.6 | 79.6 | 72.2 | 52.8 | 25.0 | 34.3 | 11.1 | 27.8 | 43.0 | 7.0 | sen>rip 50 U |
| s34 | s28 + steel/triad atk back to 9 | 55.6 | 79.6 | 66.7 | 72.2 | 68.5 | 50.0 | 25.0 | 36.1 | 11.1 | 35.2 | 44.9 | 7.5 | sen>rip 50 U |
| s35 | s32 + steel/triad atk 9 | 55.6 | 75.0 | 66.7 | 72.2 | 75.0 | 50.0 | 19.4 | 44.4 | 11.1 | 30.6 | 44.9 | 6.5 | sen>rip 50 U |
| s36 | s26 + ripper train 1075, bulwark cost 100 | 55.6 | 80.6 | 75.0 | 44.4 | **86.1F** | 50.0 | 30.6 | 30.6 | 11.1 | 36.1 | 47.9 | 4.0 | — |
| s37 | s26 + ripper train 1075, bulwark attack_ticks 90→96 | 55.6 | 82.4 | 75.0 | 47.2 | 83.3 | 52.8 | 42.6 | 22.2 | 11.1 | 27.8 | 48.4 | 5.0 | — |
| s38 | s28 + arclight cost 60 | 55.6 | 82.4 | 55.6 | 72.2 | 60.2 | 75.0 | 25.0 | 30.6 | 11.1 | 32.4 | 43.1 | 6.0 | sen>rip 50 U |
| s39 | s28 + arclight defense 6 | 55.6 | 79.6 | 55.6 | 72.2 | 57.4 | 77.8 | 33.3 | 25.0 | 11.1 | 32.4 | 43.1 | 6.0 | sen>rip 50 U |
| s40 | s28 + arclight cost 65 | 55.6 | 82.4 | 55.6 | 69.4 | 68.5 | 66.7 | 25.0 | 33.3 | 11.1 | 32.4 | 44.1 | 6.0 | sen>rip 50 U |
| s41 | s38 + sentinel 84 | 55.6 | 83.3 | 77.8 | 66.7 | 58.3 | 63.9 | 22.2 | 30.6 | 11.1 | 30.6 | 43.0 | 3.5 | — |
| s42 | s40 + bulwark speed 2→1 | 55.6 | 77.8 | 61.1 | 69.4 | 74.1 | 66.7 | 23.1 | 41.7 | 11.1 | 19.4 | **40.7F** | 5.5 | sen>rip 50 U |
| s43 | s40 + bulwark attack_ticks 102 | 55.6 | 80.6 | 61.1 | 69.4 | 69.4 | 66.7 | 25.9 | 33.3 | 11.1 | 26.9 | **40.3F** | 4.5 | sen>rip 50 U |
| s44 | s40 + probe atk 9 | 55.6 | 77.8 | 50.0 | 75.9 | 62.0 | 68.5 | 25.0 | 30.6 | 11.1 | 43.5 | **40.9F** | 7.0 | sen>rip 0 U |
| s45 | s44 + bulwark cost 100 | 55.6 | 64.8 | 50.0 | 76.9 | 64.8 | 68.5 | 33.3 | 38.9 | 11.1 | 36.1 | **38.1F** | 5.5 | sen>rip 0 U |
| s46 | s40 + bulwark offense 5→4 (shipped) | 66.7 | 36.1 | 69.4 | 69.4 | 69.4 | 66.7 | 38.9 | 41.7 | 11.1 | 30.6 | 42.6 | 6.0 | sen>rip 50 U |
| s47 | s26 + bulwark offense 4, arclight cost 65 | 66.7 | 41.7 | 77.8 | 33.3 | **86.1F** | 66.7 | 34.3 | 47.2 | 11.1 | 35.2 | 49.5 | 3.0 | — |
| s48 | s46 + bhp 840 | 66.7 | 33.3 | 66.7 | 69.4 | 69.4 | 66.7 | 38.9 | 44.4 | 11.1 | 33.3 | 46.6 | 5.5 | sen>rip 50 U |
| **s48 (4 s)** | confirmation 2 | 66.7 | 48.1 | 69.9 | 70.4 | **77.7F** | 66.7 | 25.7 | 42.4 | 11.1 | 21.3 | 46.5 | 4.1 | sen>rip 41.7 U |
| s49 | s46 + bhp 980 | 66.7 | 33.3 | 66.7 | 69.4 | 69.4 | 66.7 | 38.9 | 44.4 | 11.1 | 33.3 | 47.6 | 6.5 | sen>rip 50 U (K2 FAIL) |
| s50 | s48 + ripper train 1040 | 66.7 | 33.3 | 66.7 | 72.2 | 69.4 | 66.7 | 38.9 | 41.7 | 11.1 | 33.3 | 48.9 | 5.0 | sen>rip 50 U |
| **s50 (4 s)** | confirmation 3 | 66.7 | 46.0 | 69.4 | 71.0 | **78.3F** | 66.7 | 27.2 | 41.7 | 11.1 | 22.0 | 49.0 | 4.6 | sen>rip 37.5 U |
| s51 | s50 + sentinel 87 | 66.7 | 36.1 | **88.9F** | 63.9 | 66.7 | 55.6 | 28.7 | 47.2 | 11.1 | 35.2 | 50.5 | 3.0 | — |
| s52 (2 s) | s50 + ravager train 1100→1200 | 66.7 | 42.1 | 69.4 | 66.7 | 77.8 | 66.7 | 29.2 | 41.7 | 11.1 | 28.8 | 49.7 | 5.0 | sen>rip 50 U |
| s53 (2 s) | s50 + ravager armor 4→3 | 66.7 | 42.1 | 69.4 | 72.2 | 72.2 | 66.7 | 29.2 | 41.7 | 11.1 | 28.8 | 49.5 | 4.5 | sen>rip 50 U |
| **s53 (4 s)** | confirmation 4 | 66.7 | 46.0 | 69.4 | 73.6 | **75.0F** | 66.7 | 27.2 | 41.7 | 11.1 | 22.7 | 49.1 | 4.2 | sen>rip 37.5 U |
| s54 (2 s) | s50 + ravager cost 95→100 | 66.7 | 42.1 | 69.4 | 72.2 | 59.7 | 77.8 | 30.6 | 41.7 | 11.1 | 28.8 | 45.6 | 5.2 | sen>rip 50 U (arclight now beats ravager) |
| s55 (2 s) | s53 + turtle workers 9→10 | 66.7 | 41.7 | 69.4 | 70.8 | 69.4 | 65.3 | 29.2 | 40.1 | 11.1 | 36.3 | 48.3 | 4.8 | sen>rip 50 U |
| **s55 (4 s)** | confirmation 5: **passes** | 66.7 | 45.0 | 69.4 | 71.5 | 72.4 | 66.0 | 25.1 | 40.9 | 11.1 | 31.8 | 48.7 | 4.2 | sen>rip 37.5 U |
| s56 (2 s) | s55 + steel workers 9→10 | 66.7 | 40.3 | 69.4 | 69.4 | 65.3 | 63.9 | 43.1 | 34.5 | 11.1 | 36.3 | 47.8 | 4.8 | sen>rip 50 U |
| **s56 (4 s)** | confirmation 6: **passes; installed** | 66.7 | 42.1 | 69.4 | 70.8 | 68.3 | 65.3 | 41.5 | 32.5 | 11.1 | 32.3 | 48.8 | 4.2 | sen>rip 37.5 U |

The 4-seed K1 intervals of the rows that decided the screen:

| cand (4 s) | sentinel | ripper | ravager | turtle | K2 |
|---|---|---|---|---|---|
| s12 | 83.4 [74.3, 91.3] F | 75.9 [65.9, 84.9] F | 77.1 [67.7, 85.6] F | 44.7 [34.2, 55.1] | U |
| s48 | 69.9 [60.0, 79.2] | 70.4 [60.4, 79.8] | 77.7 [68.2, 86.2] F | 21.3 [12.9, 30.8] | U |
| s50 | 69.4 [59.3, 79.0] | 71.0 [60.9, 80.5] | 78.3 [68.9, 86.6] F | 22.0 [13.5, 31.6] | U |
| s53 | 69.4 [59.3, 79.0] | 73.6 [63.6, 82.8] | 75.0 [65.5, 83.7] F | 22.7 [14.1, 32.3] | U |
| s55 | 69.4 [59.3, 79.0] | 71.5 [61.4, 80.9] | 72.4 [62.7, 81.4] | 31.8 [22.2, 41.9] | U |
| s56 | 69.4 [59.3, 79.0] | 70.8 [60.6, 80.2] | 68.3 [58.3, 77.6] | 32.3 [22.7, 42.4] | U |


### The install (s56; step 3)

The shipped `assets/data` is now s56, which differs from the scratch directory only in
comments and in the `cost:` amounts, synced to `mvp_alloy_cost` (Sentinel 90, Ravager 95,
Arclight 65). `cost:` is read by the content fingerprint only (`src/sim/content.rs`), never
by play, and the fingerprint reaches only the net handshake, replay checks and reports,
never the RNG. So every scratch reading above is a reading of the installed content.

Net change against g1, all RON:
- **every strategy** `wave_order: move` -> `attack_move` (all 10, `mvp` included);
- **Ripper** `mvp_train_ticks` 720 -> 1 040, `mvp_attack_ticks` 50 -> 70;
- **Sentinel** cost 70 -> 90;
- **Ravager** cost 80 -> 95, armor 4 -> 3;
- **Arclight** cost 80 -> 65, defense 2 -> 4;
- **`building_hp_per_defense`** 420 -> 840;
- **nemesis `damage_mult`** 1.3 -> 2.0;
- **mass probes** (all five, still knob-identical): second and third lines at
  18 000 / 24 000 (were 9 000 / 15 000), `attack_at_army` 10 -> 8;
- **`synth_steel_flesh`** `worker_target` 8 -> 10, `attack_at_army` 9 -> 13;
- **`synth_triad`** `worker_target` 8 -> 9, `attack_at_army` 9 -> 12;
- `turtle`, `rush`, `mvp`: unchanged apart from `wave_order`.

DESIGN_BRIEF's roster table, nemesis line and timings note carry the new numbers.

**One test re-stated deliberately.** `b4_attack_move::every_shipped_strategy_ships_wave_order_move`
asserted the step-1 promise that the shipped set stays on `move`. The user's step-2b
decision reverses that promise, so it is now
`every_shipped_strategy_ships_wave_order_attack_move`, which asserts `AttackMove` for all 10
strategies and for the `mvp` commander. This changes what the test claims, on the user's
decision; it is the only claim this install re-states. Two fixtures in the same file
followed mechanically: the missing-`wave_order` load error now strips the
`attack_move` line, and the parse/fingerprint test builds its scratch all-`move` copy by
rewriting `attack_move` -> `move`. `b4_balance_data`'s scratch copy also flips the other
way now (shipped is attack-move, the scratch is all-`move`); only the difference matters.

### The golden licence

The shipped (pre-F-049) RON at 757498a, whose `src/` is unchanged since 708a8a4, under a
fresh release build: `cargo test --release` = **61 binaries, 967 passed, 0 failed,
3 ignored**. Every golden was green on the data it was pinned on. Swapping in s56 under that
identical binary turned 27 tests red in release. No Rust changed, so every move below is
the data's.

### The re-pin (F-046's pattern: what an assertion means is unchanged)

**Hash/journal goldens: 11 tests in 9 files, 60 distinct values.** These were re-pinned with
F-046's iterative tool: run, read the panic's left/right, substitute, repeat until green.
Each file's doc comment gains a "B4 re-pin (F-049)" note.
- `b1_matchup::the_default_matchup_is_byte_for_byte_what_it_was_before_ac2`
- `b1_strategies::the_one_barracks_default_replays_exactly_as_it_did_before_b1`
- `b2_headless::default_settings_reproduce_the_bench_fixture_tick_for_tick` (whole table and
  fold; building HP moves the hash from tick 1)
- `b2_orientation::the_default_orientation_is_normal_and_still_the_pinned_fixture`
- `b35_parallel` x2, `b35_queue_depth` x2
- `critic_b2_ac4::probe_the_state_hash_goldens_are_byte_identical_to_the_pre_ac2_values`
- `critic_b35_ac0::depth_one_replays_the_pre_change_tree_tick_for_tick`
- `critic_b35_ac0b::critic_shipped_matchups_are_bit_identical`, with the F-046 values kept in
  the row comments.

`critic_b1_ac2::a_non_default_pair_replays_bit_identically_across_runs_and_profiles` keeps a
per-box pin file under `target/`. As its message says, the stale file was deleted so the
next run re-pins it. That is not a source edit.

Old -> new, per file (the same value can be shared by several files):

| test file | old -> new |
|---|---|
| `b1_matchup` | `7bc8e24c34c6b32f` -> `6e9cc8ac0fd0e90c`; `22a11bed2624375f` -> `630d08d298d50b1c`; `8542718f99cf7112` -> `df969b5873d14961`; `ea6b737b371b6c53` -> `ca162e0ed2e77060`; `0c79bef6a75d654d` -> `87076f9e2b7e30aa`; `31975c1962b79ace` -> `7b889d1d8b7ac6dd` |
| `b1_strategies` | `7bc8e24c34c6b32f` -> `6e9cc8ac0fd0e90c`; `8542718f99cf7112` -> `df969b5873d14961` |
| `b2_headless` | `5addc3afc88c09ee` -> `b129cc22e5618e3e`; `e6cf18953a100975` -> `210a73c9e3425e35`; `f3869a1a5ddc98c6` -> `974c857030ac9936`; `8bfd07059b1453a0` -> `6a1342e3e1e7df70`; `13edd185edb004f0` -> `b831a73e3a762990`; `2b3039ab98739900` -> `b40f43ef8be65098`; `496070b2de2d41f8` -> `ba727a7eaa140578` |
| `b2_orientation` | `5addc3afc88c09ee` -> `b129cc22e5618e3e`; `e6cf18953a100975` -> `210a73c9e3425e35` |
| `b35_parallel` | `7bc8e24c34c6b32f` -> `6e9cc8ac0fd0e90c`; `22a11bed2624375f` -> `630d08d298d50b1c`; `8542718f99cf7112` -> `df969b5873d14961`; `ea6b737b371b6c53` -> `ca162e0ed2e77060`; `0c79bef6a75d654d` -> `87076f9e2b7e30aa`; `31975c1962b79ace` -> `7b889d1d8b7ac6dd`; `8eb242d5fdd1d6ac` -> `cc2096b7f788dc4a`; `4e1604bd46e099f6` -> `08f56bc6de2ab7b4`; `ca63fc4a377554a9` -> `19b94db06baebdf6`; `565da2a66530936a` -> `4f597b34fee19e78` |
| `b35_queue_depth` | `7bc8e24c34c6b32f` -> `6e9cc8ac0fd0e90c`; `22a11bed2624375f` -> `630d08d298d50b1c`; `8542718f99cf7112` -> `df969b5873d14961`; `ea6b737b371b6c53` -> `ca162e0ed2e77060`; `0c79bef6a75d654d` -> `87076f9e2b7e30aa`; `31975c1962b79ace` -> `7b889d1d8b7ac6dd`; `8eb242d5fdd1d6ac` -> `cc2096b7f788dc4a`; `4e1604bd46e099f6` -> `08f56bc6de2ab7b4`; `ca63fc4a377554a9` -> `19b94db06baebdf6`; `565da2a66530936a` -> `4f597b34fee19e78` |
| `critic_b2_ac4` | `7bc8e24c34c6b32f` -> `6e9cc8ac0fd0e90c`; `8542718f99cf7112` -> `df969b5873d14961`; `0c79bef6a75d654d` -> `87076f9e2b7e30aa` |
| `critic_b35_ac0` | `8d19e60bb624f4d5` -> `122b5699ad122bfc`; `988ac0b05fb1e4d2` -> `ac5e2292d45ebd4b`; `c2d3d58c3ca975d5` -> `c92fdcbe4f9d281d`; `28b235776567bfdb` -> `b9472fb5411c6dcc`; `2712cca3a3f9623d` -> `2138bfe75ff677c7`; `75541b9be9a704af` -> `d7f6485502730ff1`; `ed81f894ea6404c5` -> `a86b0a9df5b1d200`; `e34df6330cc3de55` -> `00e56049f19ebdad`; `7b14173f43fbd987` -> `4c8e30704eed488d`; `dbe8948f7c95d721` -> `407a10d1831301e5`; `f7c034ea9460e631` -> `79e54bddf468c58a`; `4a8a3944ddad8438` -> `86873f0a2a7119cd`; `78316d1bd13e475d` -> `db7e2f9da1f9ae6f`; `7b1744a8507e8f92` -> `5bff597baa5baa0f`; `69bd3de060a0157e` -> `75d8e6ecf9467ee2`; `f9be9ff5a82fc897` -> `731e86fcf784b5d0`; `325025491fd4c768` -> `66f2e19fb4c988fe`; `7021b0e774dfd4b7` -> `801b0fbb0fcff562`; `82e3cb31e9ff563c` -> `845e0fa011fd5a7e`; `8df30a7f01d1cbb0` -> `dcb806b8a64cf47a`; `ace7bc9450c9174a` -> `1d63eed06c663f9c`; `8d3e9fbc4067d682` -> `e101367a90e31496`; `8035691a7f4fa491` -> `2f5a3438e5fb5c44`; `eafeec45199159c7` -> `39fbe144215ce066`; `d4f44e22198203ae` -> `654ff36574a027e3`; `16f170ed047d7b61` -> `1343dc77f4427ffc`; `086d9db263577fd8` -> `d6f9f3cc4535497a` |
| `critic_b35_ac0b` | `d9fc20717a9c7a09` -> `2b79483c4ed36f08`; `77fbd21abf3a658e` -> `9264527b032ae3c3`; `5fe3253c75fdb8b9` -> `5264d24a0693ef32`; `9a5d49efafe5ae11` -> `6b12056c043b4be6`; `c608429484e3d26b` -> `caf1942d0e325f79`; `67f981b6e54827e4` -> `16260fc0ec6b4dd0`; `8f2794062a59e1c8` -> `749cafbf375b1f02`; `f3b6fdb1d901298c` -> `df870f13d6b9ac40`; `53537967a9552cf9` -> `cc391fdfc5334c6c`; `545bcce9611fcab0` -> `ef1fc0dcee332c8b`; `283fcec8bb5ebf41` -> `1778b8dda92b50f1`; `c7328be1b0e09085` -> `16224cd8579b67dd`; `1bc7d81e51c54e99` -> `499005a61d854898`; `90d87cb290ad3f58` -> `5eaf0d9af1cfb640`; `b2b7f6a5a8309eb1` -> `bc80a2cd27f9d081`; `3df823464ecb4d3f` -> `790207dcbd5cacf7` |

**Measured pins (re-measured; the earlier values are kept in the comments):**

| test | was (F-046) | now (F-049) |
|---|---|---|
| `b3_pentagon::the_real_batch_reports_what_the_sim_actually_does` | 2 hold / 3 undetermined; bul>rav 0.750 U, rav>sen 0.500 U, sen>rip 0.375 U, rip>arc 0.875 H, arc>bul 1.000 H | **4 hold / 1 undetermined / 0 fail**; bul>rav, rav>sen, rip>arc, arc>bul 1.000 H; sen>rip 0.500 U (4 decided, 4 timeouts) |
| `b3_gate::the_shipped_reading_on_the_pentagon_batch` K1 | 28.1 P / 56.3 U / 68.8 U / 43.8 P / 53.1 U | 50.0 U / **37.5 P** / 62.5 U / 50.0 U / 50.0 U (bul/sen/rip/rav/arc) |
| same, K2 | slot A 72.5, left 67.5, n 20 | slot A 56.25, left 43.75, n 16 (4 mirrors time out) |
| same, K3 | median 7:00; TO 2%; band 41.8% U; below 25 / beyond 34 | median **8:12**; TO 8% U; band **32.6% [22.7, 44.4] FAIL**; below 11 P / beyond 59 U |
| same, whole gate | Undetermined | **Fail** (K3 band share), as at B3 |
| `critic_b35_closure::the_turtle_commits_last_on_every_seed_with_the_claimed_margin` | 21 780 vs 21 120 | **22 980 vs 16 080** (`synth_triad`), margin 6 900, on all three seeds |
| `critic_b35_ac0b::critic_the_measured_throughput_numbers_reproduce` | 9 / 27 / 9 | 6 / 18 / 6 (the Ripper trains in 1 040, not 720; count still multiplies, depth still does not) |

**The `b3_gate` pentagon-batch K3 now reads FAIL.** This is the five-mass-probe batch at
2 seeds. The bar's batch, all 10 strategies at 4 seeds, reads K3 undetermined (48.8%
[44.6, 53.1]). The mass-probe mirrors and near-mirrors run long on attack-move content
(59% end after 8:00). The test pins what the batch does, and it pinned B3's FAIL the same
way, so re-pinning keeps its meaning. It is still a regression on that batch, flagged
here for the critic and the user.

Comment-only: `b1_probe_set` and `critic_b1_ac3` quote the turtle's commit tick (now
22 980). `b4_reservation` records the re-check below.

### Claims and premises the content breaks: left red, for a decision (10 tests)

None of these was edited. They split into one design claim and several fixtures whose
premise was "this match decides quickly".

**The M4c 8-minute claim (3 tests):**
- `m4c_ai::an_ai_vs_ai_match_is_decided_within_eight_minutes` (seed 1);
- `critic_m4c::every_seed_decides_inside_eight_minutes_on_a_dead_hq` (seed 1);
- `critic_m4c::ai_vs_ai_matches_are_decided_by_a_dead_hq_across_many_seeds` (seed 8).

The default match is the `mvp` mirror. A scratch probe played it on all 21 seeds those
three tests name, to 54 000 ticks, on three content directories:

| content | min | median | max | over 8:00 |
|---|---|---|---|---|
| s56 (installed) | 4:25 | 6:47 | 10:44 | **3 of 21** (seeds 1, 8, 21) |
| s56, `building_hp_per_defense` 630 | 4:00 | 6:26 | 10:03 | 2 of 21 |
| s56, `mvp` on `wave_order: move` | 3:08 | 3:19 | 3:36 | 0 of 21 |

On g1 with `move` the mirror decided inside 8:00 on every seed. Attack-move turns it into a
long trade; doubling building HP adds a little. Putting `mvp` alone back on `move` fixes the
claim, but it contradicts "every strategy uses attack_move" and moves the `mvp` row.

**`critic_b1_ac3::every_mass_versus_mass_cell_resolves_in_both_orientations`:**
`mass_ripper` vs `mass_sentinel` never resolves in 54 000 ticks. This is the same link that
reads 4 decided and 4 timeouts in the pentagon batch. A claim about the content.

**Fixture premises (6 tests).** Each picked a match because it decided quickly, and that
match no longer does:
- `critic_b2_ac2::probe_cap_boundary_is_inclusive_of_the_deciding_tick` and
  `::probe_recorded_ticks_agree_with_the_sims_own_counters`: the DECIDER fixture is the
  `rush` mirror on seed 0, which never decides on attack-move (one-Ripper streams meet
  midfield and trade);
- `critic_p2::no_configuration_of_the_writer_changes_a_single_tick_of_the_sim`: seed 7's
  fixture never decides inside its tick count;
- `m5_replay::everything_in_a_live_match_is_identified_and_the_log_resolves`: fewer than 21
  commands in 1 500 ticks of `ai_vs_ai(4)`;
- `critic_b2_ac4::probe_b3_metrics_are_computable_from_the_records_alone`: no Ripper inside
  its 1 200-tick cap (the Ripper now trains in 1 040);
- `b2_orientation::mirrors_are_side_balanced_across_the_two_orientations`: the `rush`
  mirror times out 6 times, and the test asserts that every mirror decides.

**Options**, none of them taken:
- **(a)** Keep the content and restate the M4c target and the critic_b1_ac3 claim. That is
  a decision about the MVP's definition of done, for the user.
- **(b)** Put `mvp` back on `move`. This fixes M4c (3:19 median) but moves the `mvp` K1 row
  and contradicts the 2b decision. It needs a re-measure.
- **(c)** Re-pick the fixtures of the six premise-only tests: a deciding pair, a longer tick
  count, a later cap. This changes no claim, but these are critic-authored tests, so the
  critic should agree.
- **(d)** Look for another retune that keeps the `mvp` mirror under 8:00. 4 of the 10 4-seed
  confirmations remain.

### Re-checks

- **Turtle-latest closure, 3 seeds:** the turtle commits at 22 980, last, with
  `synth_triad` next at 16 080, on all three seeds. The full order is identical on every
  seed: rush 1 065, mvp 4 650, mass_ripper 8 730, mass_arclight 9 510, mass_sentinel 11 730,
  mass_ravager 12 660, mass_bulwark 12 870, synth_steel_flesh 15 510, synth_triad 16 080,
  turtle 22 980.
- **`b4_reservation`, the bounded claim:** green. All eight multi-opening strategies place
  their whole script on both sides of their `seed_at(0, 1)` mirror, so the grace bound
  excuses nothing:
  - mass probes at 300 / ~18 000-20 430 / 24 000-27 090;
  - synth_steel_flesh 300 / 3 060 / 5 010 / 6 930;
  - synth_triad 300 / 3 060 / 4 950 / 7 200;
  - turtle 600 / 3 060 / 5 010 / 6 510.
  Non-vacuity holds: all five mass probes place more than one line.
- **The F-047 diagnostic, re-run** (`b4_turtle_diag`, 36 matches, release, 198 s). The
  turtle now wins **10 of 36** (F-047, on g1 with `move`: 30 of 36):
  - rush 4/4, mass_bulwark 3/4, mass_ravager 2/4, synth_steel_flesh 1/4;
  - mvp, mass_sentinel, mass_ripper, mass_arclight and synth_triad 0/4.

  F-047's mechanism is gone. Then, D's waves walked in on order and died near the
  turtle's HQ ("contact on order" 18-85%). Now that figure is 0% for every armed opponent
  (5% for rush), and those waves raze the turtle's buildings: mean building HP lost
  13 000-32 000 against mvp, sentinel, ripper, ravager, arclight and triad. In all 26 of
  its losses the turtle never commits its 24.

### Gate

On the box, s56 installed with the re-pins above:

| step | result |
|---|---|
| `cargo clippy --all-targets -- -D warnings` | clean |
| `cargo test --no-fail-fast` (debug) | 61 binaries, **957 passed, 10 failed**, 3 ignored |
| `cargo test --release --no-fail-fast` | 61 binaries, **957 passed, 10 failed**, 3 ignored |
| `cargo bench --no-run` | builds |

The 10 failures are the same in both profiles. They are exactly the claim and premise reds
listed above, and none is a golden or a measured pin. 957 + 10 = 967, the licence run's total.
The expected "0 failed" is **not met**, so step 3 stays unticked until those 10 are decided.

### Lessons

- Attack-move fixed what F-047 diagnosed: waves fight back on the road. It also moved
  every other tempo. Early committers stopped dying for free, so the retune had to slow
  the Ripper, raise the Sentinel's price and push the probes' later lines back. Nearly
  every lever in the table moved several rows at once.
- A 1-seed screen is cheap but noisy around 65%. Every 4-seed confirmation that failed
  (s12, s48, s50, s53) failed on a row the screen had read as only undetermined, at 69 to
  75% on 1 or 2 seeds. That row then rose 3 to 9 points at 4 seeds (`mass_ravager`
  69.4 -> 77.7 on s48). The confirmations, not the screens, picked the winner.
- The bar measured the 10-strategy batch. It did not measure the M4c 8-minute claim or the
  "decides quickly" fixtures of older tests. The install surfaced 10 such reds that no
  screen could see. Next time, add the default-mirror length to the screen.

## F-050 — Why some s56 matches never end: a legitimate deadlock, not an attack-move bug (B4)

**Context.** F-049 installed s56 and left 10 tests red. Three of them involve matches that
never end. The user's call (2026-10-10) was to diagnose those stalemates before deciding
anything else. No test, claim or RON value was changed for this entry.

**Method.** A scratch sampler ran on the box in release mode. It was an ignored test, deleted
afterwards and never committed. It replays each fixture exactly and, every N ticks, prints
the following for each side:
- Alloy, workers and every building's HP and queue length;
- every armed unit's position, HP, `MoveTarget`, `Engaging`, `AttackMoveDest` and `Target`
  (the distance to the target and whether it is a unit or a building);
- every `AiAction::Attack` and `TrainArmy` since the last sample.

There were four kinds of runs:
- full-length runs at 600 or 1 200 ticks per sample;
- one zoom at 60 ticks over the first `mass_ripper` wave;
- one zoom at 5 ticks over the first `rush` duel;
- one control run with `rush` alone switched to `wave_order: move`.

**Verdict: (ii), a legitimate deadlock, in every case.** The units march, acquire, close to
weapon range, stand and trade exactly as `combat()` specifies. None of the attack-move bug
patterns appears in any sample. None oscillates at the leash or a wall. None re-acquires
an unreachable target. Every wave arrives. None loses its order. None stands idle at its
destination beside a building. The matches stall because of the content's arithmetic and
the strategies' thresholds. So no fix was made.

### 1. The `rush` mirror (critic_b2_ac2 DECIDER, seed 0; b2_orientation's 6 timeouts)

The full run on seed 0 lasted 54 000 ticks and timed out:
- Both HQs are at **8 400 / 8 400 in all 90 samples**. No armed unit ever targets a building.
- Each side has 3 workers. The economy pays for one Ripper (40 Alloy, 1 040 ticks) at a time,
  so 142 of 180 side-samples show 0 armed units and 38 show 1. There are never 2.
- Every Ripper meets its mirror at midfield (x from -38 to -76), and the two units kill each
  other on the same tick.

The 5-tick zoom of the first duel shows the cycle:

| tick | A Ripper | B Ripper |
|---|---|---|
| 1 170 | (-165,-52), marching, `am`=(745,-38) | (52,38), marching, `am`=(-788,38) |
| 1 175 | acquires B at d183, chases (`E`) | acquires A at d183, chases |
| 1 190 | in range (d32 < 40), stands: hp 60 -> 32 | the same: hp 60 -> 32 |
| 1 260 | hp 4 | hp 4 |
| 1 330 | dead | dead |

The arithmetic: Ripper damage is 6×5 − 1×2 = 28 against an HP of 60. That is three hits,
every 70 ticks, on the same tick for both sides. `combat.rs` resolves both blows from one
snapshot ("two units that kill each other on one tick both die", lines 22-31). The mirror
matters here: the same script, the same economy and the same think interval mean both
Rippers are trained on the same tick and arrive together. The AI re-sends the order every
120 ticks with a new spread (`ai.rs` 594-608), including once mid-duel at 1 185. That is
harmless: `apply_commands` drops `Engaging` (`mod.rs` 803-821), and `combat()` re-acquires
the same target within the same tick from `attack_move_range` (`combat.rs` 417-427). By 1 190
the two units are standing and trading.

In the orientation runs, `rush` timed out in **6 of 6** (3 seeds × 2 orientations).
`mass_ripper` decided in 6 of 6.

**Control:** the same s56 RON and binary on seed 0, with only `rush` switched to
`wave_order: move`, **decides at tick 8 631 (A)**. A plain mover with a `MoveTarget` does not
stand to fight (`combat.rs` 463-483: only an engaging unit or an attack-mover stands or chases), so the two
Rippers walk past each other to the HQs. The stalemate is therefore what attack-move means
for a 1-unit-at-a-time mirror: an even trade, repeated forever.

### 2. `mass_ripper` vs `mass_sentinel` (critic_b1_ac3 fixture, seed 7)

This run also lasted 54 000 ticks and timed out. Sampled every 1 200 ticks:
- A attacks in **8 waves** of 8 Rippers, at 8 730, 17 130, 22 230, 27 000, 32 430, 37 830,
  43 170 and 48 570. Every wave dies at B's base.
- B's armed count peaks at **7**, never reaching its `attack_at_army` of 8. So B never sends
  a wave at all.
- B's HQ took **396 damage in the whole match** (8 400 -> 8 004 at about 33 000, the one wave
  that also killed 2 workers). A's HQ was never touched.
- B is limited by Alloy (the Sentinel costs 90; B's balance was between 0 and 144 at every
  sample). A was train-limited until its later Gene-Vats went up: it banked 466 Alloy by
  18 000, then spent down to 4-40.

The 60-tick zoom of wave 1 shows B 6 v A 8:
- The Rippers march as a unit and acquire the nearest Sentinel at d290 on the pursue leash.
- They close to d35, inside the Ripper's 40 range, and stand.
- From 9 060 to 9 480 they lose one Ripper per Sentinel volley and kill one Sentinel per
  Ripper volley. **A ends 8 -> 0, B 6 -> 2.**

The cause is the nemesis link:
- A Sentinel hits a Ripper for 6×5×2.0 = 60, armour ignored, which equals the Ripper's HP:
  one shot, one kill.
- A Ripper hits a Sentinel for 6×5 − 5×2 = 20 against 100 HP: five hits.

Both sides focus the nearest target, with ties broken by entity bits (`combat.rs` 355, 417),
so both overkill. That is the M2/M3 targeting model, not something attack-move introduced.

The cycle is therefore stable:
- each wave kills 4-5 Sentinels;
- B rebuilds to 5-7 before the next wave;
- B never reaches 8 to counter-attack;
- the HQ is never reached in force.

This is the counter-pentagon doing what it should, plus a threshold that the defender's
economy cannot reach between waves. It matches F-049's 4 decided and 4 timeouts on this
link in the pentagon batch.

### 3. Related: the `mvp` mirror (critic_p2 seed 7; M4c seeds 1 and 8)

These matches are long but they do end, so they are context rather than one of the three
stalemates. On the critic_p2 fixture (default commanders):

| seed | decided at |
|---|---|
| 7 | 27 485 (B) |
| 1 | 30 127 (B) |
| 8 | 38 621 (A, 10:44) |

On seed 8 the sides trade 2-3-unit waves evenly to about 26 000 (A's HQ 8 048, B's 8 400).
Then A wins a trade, camps B's Foundry and kills each new Sentinel as it appears. B shows 0
armed in every sample from 25 800 on, and its Foundry falls by about 32 400. The HQ then
goes 8 356 -> 0 in about 6 000 ticks. This is again correct attack-move behaviour: it takes
the nearest armed target, falls back to the nearest building, and stands in range. Here it
is just slow.

### Consequence

There is no sim fix to make, so no golden moves and nothing is re-measured. The 10 reds and
options (a)-(d) in F-049 stand as they were. Two of the three stalemates come from content
and thresholds:
- the `rush` mirror, a 1-Ripper economy trading 1-for-1 on simultaneous resolution;
- `mass_ripper` vs `mass_sentinel`, a nemesis wall below `attack_at_army`.

Both are design questions for the user, not defects.

## F-051 — The 10 s56 reds resolved: claims restated, fixtures re-picked (B4)

**Context.** F-049 installed s56 and left 10 tests red. F-050 found the stalemates behind
them to be legitimate deadlocks. The user's decision (2026-10-11) was to restate the claims
and re-pick the fixtures, with no RON or sim change, so s56 stands as measured. Each
restated claim is a measured bound that still guards the original intent, and each new
fixture was chosen by a stated rule. The four restated tests do assert a changed claim,
recorded old -> new below. The re-picked fixtures keep their assertions' meaning. All measurements were
release runs on the box, using a scratch probe that was deleted afterwards.

### Claims restated (4 tests)

**M4c's 8-minute claim** (`m4c_ai::an_ai_vs_ai_match_is_decided_within_eight_minutes`,
`critic_m4c::ai_vs_ai_matches_are_decided_by_a_dead_hq_across_many_seeds`,
`critic_m4c::every_seed_decides_inside_eight_minutes_on_a_dead_hq`).

- **Old:** every seed is decided inside 8:00 (28 800 ticks).
- **New**, over the same seeds each test already names:
  1. every match is decided, with a winner, by a dead HQ (only the winner still has one)
     within the match cap (`DEFAULT_TICK_CAP`, 54 000 = 15:00);
  2. the median decided length (upper median) is <= 8:00;
  3. at most one match in four, rounded up, runs past 8:00.
- **Strictness:** "decided by a dead HQ" stays strict. `m4c_ai` now also checks the standing
  HQs, which it did not before.

The measurement is the `mvp` mirror on s56 (F-049/F-050), per test:

| test | seeds | median | over 8:00 | bound |
|---|---|---|---|---|
| `m4c_ai` | 1, 7, 99 | 27 485 | 1 (seed 1: 30 127) | 1 |
| `critic_m4c` many seeds | 9 | 24 446 | 2 (seed 8: 38 621; seed 21: 32 418) | 3 |
| `critic_m4c` wider sweep | 12 | 25 842 | 1 (seed 1) | 3 |

Pooled over the 21 distinct seeds: median 6:47, 3 of 21 (14%) over 8:00, longest 10:44.

Why one in four:
- It is about 11 points above the pooled 14%.
- It is the smallest round share that leaves headroom wherever the seed count allows. At
  20%, the 9-seed test (measured 2, bound ceil(1.8) = 2) would have none.
- The 3-seed test has none either way. Its only non-trivial bound is 1 of 3.

The median <= 8:00 is the guard on the design intent. The tail bound catches drift: a
retune that pushes more of the mirror past 8:00 turns these tests red.

The claim is annotated, not rewritten, at `MVP_PLAN.md` (Win line) and `BUILD_PLAN.md`
(M4c), as "Restated 2026-10-11 by user decision, F-050/F-051", with old -> new.

**`critic_b1_ac3::every_mass_versus_mass_cell_resolves_in_both_orientations`.**

- **Old:** all 25 ordered mass-vs-mass cells resolve on seed 7.
- **New:** every cell resolves except those in `MEASURED_DEADLOCK_CELLS`. Each listed cell
  is asserted to still time out, so if it ever resolves the test goes red and the list
  must be revisited. The test also asserts that every exception names a real cell.
- **The list is one ordered cell:** (`mass_ripper` A, `mass_sentinel` B), with F-050's
  reason in the comment: nemesis one-shot, defender short of Alloy and below
  `attack_at_army`.
- **The reverse is not excepted:** (`mass_sentinel` A, `mass_ripper` B) resolves at 48 738.

All 25 cells were measured on s56, seed 7, to the cap. 24 resolve, from 13 937
(`arclight` v `bulwark`) to 53 978 (the `mass_bulwark` mirror, 22 ticks inside the cap).
That last one is a fragile green worth knowing about.

### Fixtures re-picked (6 tests)

**`critic_b2_ac2`** (`probe_cap_boundary_is_inclusive_of_the_deciding_tick`,
`probe_recorded_ticks_agree_with_the_sims_own_counters`).

- **Change:** `DECIDER` goes from (`rush`, `rush`) to (`mass_arclight`, `rush`). The seed
  stays 0.
- **Premise:** "a matchup that decides quickly".
- **Rule:** the ordered pair of shipped strategies that decides fastest on seed 0.
- **Measured:** all 100 ordered pairs:
  - (`mass_arclight`, `rush`) decides at 12 291 (B wins);
  - next are (`mvp`, `rush`) at 13 259 and (`rush`, `mvp`) at 13 301;
  - 3 pairs time out: `rush` mirror, (`mass_ripper`, `mass_sentinel`),
    (`mass_ripper`, `synth_triad`).
- The other `decider()` users in the file (degenerate caps, tick-cap isolation,
  determinism) take any matchup and stay green.

**`critic_p2::no_configuration_of_the_writer_changes_a_single_tick_of_the_sim`.**

- **Change:** `TICKS` goes from 12 000 to 30 000. The seed stays 7.
- **Premise:** the horizon clears the decision, so the writers really write (F-033).
- **Rule:** F-033's rule, unchanged: the first multiple of 6 000 past the decision. It gave
  12 000 for 9 498 then; seed 7 now decides at 27 485, so it gives 30 000.

**`m5_replay::everything_in_a_live_match_is_identified_and_the_log_resolves`.**

- **Change:** the window goes from 1 500 to 2 000 ticks. The seed stays 4.
- **Premise:** the window holds a real log, more than 20 commands.
- **Measured:** 20 commands at 1 500. The count first exceeds 20 at 1 681 and is 24 at 2 000.
- **Rule:** the first multiple of 500 past the crossing.

**`critic_b2_ac4::probe_b3_metrics_are_computable_from_the_records_alone`.**

- **Change:** the cap goes from 1 200 to 1 800.
- **Premise:** each mass strategy has produced its unit. The Ripper now trains in 1 040, so
  there is none at 1 200.
- **Measured:** the batch (seed 1, 8 rows) at 300-tick steps: 0 Rippers and 0 Sentinels at
  1 200; 8 of each at 1 500 and 1 800; 16 at 2 700.
- **Rule:** the first step at which both appear (1 500), plus one step of margin.

**`b2_orientation::mirrors_are_side_balanced_across_the_two_orientations`.**

- **Change:** `MIRRORS` goes from [`rush`, `mass_ripper`] to [`mvp`, `mass_ripper`].
- **Why `rush` had to go:** it times out in all 6 of the test's matches (3 seeds × 2
  orientations).
- **Options considered:**
  - Excluding timeouts from the balance count would leave `rush` named while contributing
    zero cells. The sample would silently halve and the name would claim a measurement
    that never happened.
  - Using a mirror that decides keeps every named mirror contributing every cell, and keeps
    `t.timeouts == 0` meaningful. This is the more honest choice, so it was taken.
- **Rule:** the mirror with zero timeouts on these 6 matches and the lowest median length,
  `mass_ripper` excepted.
- **Measured** over all 10 mirrors:
  - `mvp` 27 562;
  - `synth_steel_flesh` 28 758;
  - `synth_triad` 28 778;
  - `mass_ripper` about 33 100;
  - `turtle` 33 154;
  - `mass_ravager` and `mass_arclight` 46 000-49 000;
  - `rush` (6 of 6), `mass_bulwark` (2 of 6) and `mass_sentinel` (2 of 6) time out.

### Gate

The full gate ran on the box on 2026-10-12, 13:05-14:21 PT, with the box tree equal to
6937212:

- `cargo clippy --all-targets -- -D warnings`: clean.
- Debug tests: 967 passed, 0 failed, 3 ignored.
- Release tests: 967 passed, 0 failed, 3 ignored.
- `cargo bench --no-run`: builds.

**Run limits (new).** An earlier debug run at 4 test threads crashed the box on
2026-10-11. s56's longer matches drove load to 20.6, and the box most likely thrashed
memory, since it has about 1 GB of swap. The journal stops with no OOM line, and the
reboot needed an ext4 fsck. From now on, gate runs use these limits:

- 2 test threads, `CARGO_BUILD_JOBS=2` and `nice -n 19`;
- a watchdog that kills the gate's process group if MemAvailable drops below 6 GB or
  load rises above 8. (Ubuntu 20.04 runs cgroup v1, so a systemd `MemoryMax` is not
  enforced.)

This run never tripped the watchdog. The lowest MemAvailable was 26 212 MB and the highest
load was 3.07.

**Resolution.** The 10 reds that F-049 left open are resolved, the gate is green, and B4
Step 3 is ticked. The critic and the merge follow.

## F-052 — Campaign design, C0 draft: persistent armies, cities, General and retreat (C0)

**Context.** `CAMPAIGN_PLAN.md` (merged 2026-11-02) starts with C0, nation and campaign design
on paper. B5, the human fun check, has not run yet because the user cannot play at present.
C0 builds nothing, so the B5 gate still holds for C1 onward.

**User decisions (2026-11-02):**
1. **Armies persist, as in Age of Empires:** units lost are lost, and survivors carry over to
   the next battle.
2. **Nations hold cities**, and battles are fought over cities.
3. **A battle is won by killing the enemy General or making the enemy retreat.**

**What they change:**
- **The battle layer itself.** Today's only win condition is destroying the HQ, and every match
  starts from a fresh base. The decisions add a General unit, a Retreat order, battle setup
  from an army roster, and a battle outcome that reports survivors. These are battle-layer
  features, so they get their own milestone, **C1**, ahead of nations. The later milestones
  shift down one: nations C2, three resources C3, campaign core C4, diplomacy C5, client C6,
  fun gate C7.
- **Skirmish mode keeps today's rules,** so the B-series goldens and the balance tooling stay
  valid. The golden licence ("skirmish reproduces today") is an AC in C1.
- **Retreat is the army-saving move.** Because armies persist, a lost battle need not destroy
  an army. That makes the AI's retreat threshold a balance knob, and it lives in RON.
- **Without reinforcement, armies only shrink.** The draft therefore recommends a per-city
  levy each round.

**Recommended, awaiting review** (`DESIGN_BRIEF.md`, "Campaign design (C0 draft)"):
- A fixed 16-city map: you 1, Forgeborn 4, Verdant 4, Aetherkin 3, Tollmarch 2, Ashen Reach 2.
- Unit HP carries over between battles, with healing when an army holds a round in its own
  city.
- Destroying the HQ also takes the city.
- AI-vs-AI battles are auto-resolved by the headless sim, with survivors written back.
- Taking a capital absorbs that nation and its art, with a cap on how many arts stack.
- Diplomacy:
  - standing runs Hostile, Neutral, Tied, Allied;
  - at most one ally;
  - betrayal drops your standing with every nation by one step;
  - destabilizing needs Tied standing with two nations;
  - neighbours' notice of you grows with your size.
- Your General is the heir. If he dies, the battle is lost and he is wounded for N rounds,
  but the campaign is not lost.
- Length: 15–25 rounds, about 2–3 hours.
- Six nation sheets, with trait intents and Alloy-only stand-ins.

**Risks the campaign sim must measure (C4):**
- Persistent armies plus a levy per city can snowball, as big nations grow bigger.
- A heavy loss can death-spiral, with no army and too few cities to rebuild.
- Absorbed arts can stack into a dominant build.

Each risk has a knob in RON: the levy size, the healing rate, the trait cap and the retreat
threshold.

**Open:**
- The names of your house and its capital.
- The trait cap.
- The `nations.ron` and `campaign.ron` field sketch.

**Update (2026-11-02):** the user decided that the General **buffs nearby units** with an aura,
whose radius and bonus are RON data. That closes the General's-role question. C1 gains an AC
for it.

**Update 2 (2026-11-02):** the user decided that **each nation's General gives a different kind
of buff.** The draft gives one aura per nation, matched to its doctrine:
- your house: Synthesis, which scales with the number of domains mixed nearby;
- Forgeborn: Bulwark (+ defense and armor);
- Verdant: Frenzy (faster attacks);
- Aetherkin: Focus (+ offense);
- Tollmarch: Logistics (+ speed);
- the Ashen Reach: Endurance (regeneration).

Auras are RON stat-modifier lists and never touch the nemesis bonus, so the pentagon's hard
counters survive. The risk is that a strong aura becomes a nation's whole identity in balance
terms. C2's gate measures each aura against the pentagon and the strength band.

**Update 3 (2026-11-02):** the user decided that **taking a capital absorbs its aura with a
second General.** The conqueror gains a General carrying that nation's aura, who leads a new
army, up to six in total. The recommendation is one General per army and one army per side in
a battle, so auras never stack. The campaign sim must measure the snowball risk, since each
capital adds both cities and an army. C4 gains an AC for it.

**Update 4 (2026-11-02):** the user decided two more questions.
- **Generals can be veterans.** Surviving battles raises a General's rank, and each rank
  strengthens his aura, with a cap on the top rank. A replacement General starts at rank 0,
  which makes retreating to save a veteran a real choice.
- **Siege damage persists between rounds.** A city can be worn down over several assaults.
  The recommendation is that unattacked cities repair a set amount each round, destroyed
  buildings need rebuilding, and a conqueror inherits the damage.

C1 now includes building HP in battle setup and outcome. C4 persists it.

**Update 5 (2026-11-02):** the user made three decisions and rejected one rule.
- **At most 3 absorbed Generals.**
- **The rejected rule:** abstract "absorbed traits" (the draft's "gain its art") are dropped,
  because holding a buff from conquest made no sense. Conquest grants the cities, plus a
  General who carries the aura onto the field.
- **More armies mean more Generals.** Several armies can converge on one battle to overwhelm
  a defender. This replaces the draft's "one army per side".
- **Moving takes turns, and mobility depends on unit type.** An army's march points per round
  equal its slowest unit's Speed, the same stat the battle uses (Bulwark 2 to Ripper 9).
  Roads have lengths. Heavy armies hit hard but arrive late.

Two consequences:
- A side loses a battle when all its Generals are dead.
- Auras of different kinds stack, and auras of the same kind do not *(rec)*.

The C0 data shape (`units.ron` additions, `nations.ron`, `campaign.ron`) is drafted in the
brief, not in `assets/data`, so the shipped content and its fingerprint are untouched.

**Update 6 (2026-11-02):** the user decided two more questions.
- **Extra Generals of your own house can be raised** at a city, for a levy cost set in RON.
- **Nation perks apply only to that nation's own armies and cities.** An absorbed General
  brings his aura, but not his old nation's perks.

**Update 7 (2026-11-02): C0 closed.** The user delegated the names: **House Vael** for the
player's house (close to "veil", for the veiled truth of the Rupture), **Cinderhold** for its
capital, and **the Heir of Vael** for its first General. C0 has no open questions left; its
boxes are ticked, and the *(rec)* items stand unless the user changes them. Next is C1
(campaign battle rules), which waits for the B5 Go.

**Update 8 (2026-11-02): the B5 wait is lifted.** The user said: "start c1 now, don't wait for
b5". So C1, the campaign battle rules, begins before the human fun check. B5 stays open and
still runs when the user can play. If it finds the core loop flat, that can revise C1 and
everything after it. This is a recorded exception to `BALANCE_PLAN.md`'s B5 Go/No-Go, not a
removal of it.


## F-053 — The General: its own list in `units.ron`, opt-in battle rules, a skirmish untouched (C1)

**Context.** C1 AC1 asks for a General, one per side; a side whose General dies loses. The
golden licence (AC8) asks that the default skirmish reproduce today's pinned hashes exactly.

**Decisions.**
- **Generals are their own list** (`units.ron` `generals`), not entries in `combat`. They share
  the 4-stat model and the per-unit `mvp_attack_ticks` / `mvp_attack_range`, but carry no cost,
  no barracks and **no nemesis** (`deny_unknown_fields`, so a `nemesis:` on a General is a load
  error, not a silently ignored key). A General's id may not collide with a roster unit's.
- **One index space, two lookups.** A General's `UnitDefIdx` is `units.len() + k`.
  `Content::unit_def(i)` resolves either list and is what combat and movement read (HP, damage,
  speed, armed-ness). Everything that *buys or commands an army* — `Train`, production, gather,
  the AI's force list — keeps reading `content.units`, so a General can never be trained, never
  gathers and never joins a wave. `content.units.len()` stays 6 (rally spokes, the pentagon and
  `m4a_economy` read it).
- **Opt-in by resource.** A battle is `MatchSettings::with_battle(BattleSetup)`. It spawns the
  Generals after the skirmish start (General at `base + front * mvp_battle.general_front`) and
  inserts `BattleState`; every battle system is `run_if(resource_exists::<BattleState>)`. No
  resource is `init_resource`d in `build_app`, so the pinned allocation order (critic_p2) holds.
- **Deciding.** `battle_watch` latches at the start of a tick which losses a side can suffer
  (fielded a General / held an HQ), mirroring `match_watch` (F-010). `battle_end` runs just
  before `match_end` and decides from fixed per-side counts: all Generals gone → `GeneralsFell`;
  else HQ gone → `VictoryBuildingLost`; both sides losing on one tick is a draw. It writes
  `MatchState` through `conclude` (same tick bookkeeping as `match_end`), so the chain stops
  exactly as for a skirmish result. A side that never fielded a General cannot lose one.
- **Hash.** New state-hash tags `GENERAL` (23) and `BATTLE_STATE` (24) emit rows only in a
  battle, so a skirmish hashes as before.
- **Refusal.** `headless::battle` returns `MatchSetupError::{Strategy, Battle}`; an unknown
  General or content with no `mvp_battle` block is refused before anything spawns.
  `ai_vs_ai` keeps its `UnknownStrategy` error type (every existing caller) and panics on a
  bad battle setup.

**Not yet.** Replaying a battle from a `CommandLog` is not supported: the log does not carry
the `BattleSetup` (that belongs to C4's campaign state).

**Addendum (AC2, several Generals per side).** `BattleSetup::with_general` may be called
more than once per side. The Generals stand in a line across the front, centred on the
single-General spot and spaced `mvp_battle.formation_spacing` apart, in setup order. A side loses
on `GeneralsFell` only when **all** of its fielded Generals are gone. Bevy 0.19 entity bits are
not spawn order, so the sim never relies on spawn order for an outcome (the report's General
fates are in setup order, and everything else is sorted by `SimId`), and the tests find
Generals by definition or position, never by entity.

## F-054 — Auras: a per-tick bonus field, integer rules, kinds stack and one kind does not (C1)

**Context.** C1 AC3: each General type carries a RON aura (a radius plus stat modifiers);
the synthesists scale by distinct domains nearby; six kinds; never the nemesis bonus;
different kinds stack, the same kind does not.

**Decisions.**
- **Shape (RON).** `aura: (radius, modifiers: [(stat, add | mul)], scaling: Flat |
  PerDistinctDomain(max))`. Stats: `Offense`, `Defense`, `Armor`, `Speed`, `Regen` take `add`
  (design points, 1..=max_stat); `AttackTicks` takes `mul` in (0, 1), held as per-mille.
  There is **no nemesis stat**: `stat: Nemesis` is an unknown variant and a load error, and
  `deny_unknown_fields` refuses any extra key. Each stat at most once per aura.
- **Rules (integer only).** Offense and Armor add design points to the per-hit rule. Defense
  never changes the HP pool (a pool that grows and shrinks as a unit walks in and out of an
  aura would heal or kill it by walking); it scales what gets through by
  `defense / (defense + bonus)`, floored. Speed adds design points. AttackTicks: the interval
  set after a hit is `ticks * milli / 1000`, floored, at least 1. Regen heals `add` HP every
  `mvp_battle.regen_interval_ticks` (60), on ticks that are a multiple of it, up to the pool.
  The nemesis multiplier and its armor rule are applied exactly as before; an offense bonus
  enters the base the multiplier is applied to (the multiplier itself is untouched).
- **Who.** Friendly units **other than Generals** within the radius. A General's own stats are
  his kit; an aura is for his army. Workers in range get it too (they carry no domain, so they
  never raise the Synthesis count).
- **Kinds.** Kind = General type. Per unit and kind, the strongest scaling factor (all
  Generals of one type share one aura, so "strongest" is the largest factor); then the kinds
  fold together: adds sum, AttackTicks factors multiply. A factor `n` applies the modifiers
  `n` times (adds × n; `mul` to the n-th power).
- **Synthesis.** Distinct domains (resources.ron `powers` → `domain`) among the friendly
  non-General units in the radius, capped at `max` (1..=number of domains). An empty or
  worker-only radius gives ×0, so a pure army gets ×1 and the full mix ×3.
- **Where.** `battle::auras` builds the `AuraField` (entity-sorted) after gather and before
  combat; combat (damage, cooldown), movement (speed) and `battle::regen` (after combat) read
  it. In a skirmish there is no `AuraField`: combat and movement read `StatBonus::NONE`, and
  the `_with` functions given `NONE` are the plain functions exactly (tested for every pair).
- **Hash.** `AuraField` is derived from positions each tick, so it is not hashed on its own.

**Not yet.** Ranks (`general_ranks`, `aura_mult`) are C4. The aura is not drawn on the field
(P3 visibility) — that is client work.

## F-055 — Retreat: a signed order, a per-tick withdrawal, escapees recorded with their HP (C1)

**Context.** C1 AC4: retreat is an `Order`; the side's units head for its entry edge; the
battle ends as a retreat once the withdrawal finishes; the outcome records who got off.

**Decisions.**
- **`Order::Retreat { faction }`**, signed like `Place` (a commander withdraws only its own
  army), logged as `LoggedOrder::Retreat` (a new variant, so older logs read unchanged) and
  digested as tag 6. Without a `BattleState` (a skirmish) it is logged and does nothing.
  Once ordered, a retreat is not called back.
- **Entry edge.** The line `mvp_battle.entry_edge_offset` (150) behind the side's base,
  square to the base-to-front axis. A retreating unit walks straight out to the nearest
  point of it; on or beyond it, it has **escaped**.
- **`battle::withdraw`**, right after `apply_commands`, so it overrides any other order every
  tick (the AI or a player cannot turn a retreating unit around). Ascending entity order. It
  records each escapee (side, `SimId`, `UnitDefIdx`, HP, max HP, General flag, tick), then
  despawns it; others get `MoveTarget(exit)` with any chase, attack-move and gather job
  dropped. Retreating units still hit what comes into reach on the way (combat's normal rule
  for a moving unit), but never stop to chase. The side's production queues are cleared:
  nothing new joins a withdrawal. Buildings stay.
- **Ending.** In `battle_end`: a side with retreat ordered and no unit left on the field has
  lost with cause `Retreated`. Order of causes: all Generals dead **with none escaped** →
  `GeneralsFell`; HQ gone → `VictoryBuildingLost`; then `Retreated`. A General who escaped did
  not fall, so a withdrawal that gets its General out is a retreat, and one whose General is
  killed on the way is a General lost. `BattleResult.escaped` is the crossing list.
- **Hash.** Retreat flags, edges and the escapee list are in `BattleState::hash_words`.

## F-056 — Battle setup from a roster: a given army and given buildings replace the skirmish start (C1)

**Context.** C1 AC5: a side can start a battle with a given General, units with HP and
buildings with HP (siege damage persists, F-052), instead of the skirmish start.

**Decisions.**
- **`Roster { units, buildings }`** on a side of the `BattleSetup` (`with_roster`). Units are
  roster ids with an optional HP (`unit_hp`), buildings likewise (`building_hp`); no HP means
  full. A side with a roster gets **no** skirmish HQ and no workers. Its resource deposit
  still spawns, so a roster that includes a worker can still gather. A side without a roster
  keeps today's start, unchanged.
- **Checked before anything spawns.** `BattleSetup::check` refuses an unknown unit or building
  id, a General id in the unit list (Generals go in `generals`; `unit_index` never resolves
  one), and HP 0 or above the RON pool (`BadHp { id, side, hp, max }`).
- **Layout is data.** `mvp_battle.formation_rank` (8 per rank) and `building_spacing` (160)
  join `formation_spacing` (30). Units stand in ranks one spacing behind the Generals' line,
  each further rank one spacing nearer the base, centred across the front. Buildings stand in
  a line across the base: the first on it, the rest alternating either side. Spawn order is
  the list order (buildings, then units), after the Generals, so entity allocation is a
  function of the setup alone.
- **HP.** A unit always gets a `Health` (current = given or full). A building gets an explicit
  `Health` only when its HP is given. Otherwise combat takes its pool from the RON, as in a
  skirmish.
- **No HQ in a roster** means the side cannot lose by HQ: `victory_seen` stays false, so a
  field battle is decided by Generals or a retreat.

**Not yet.** Rosters come from the campaign layer (C2+). The map's geography is the headless
fixture's two bases; real battlefields are C3.

## F-057 — The battle report: survivors, standing buildings and each General's fate (C1)

**Context.** C1 AC6: the outcome reports survivors. Per side, it gives each unit's type and
HP, building HP, and whether each General survived. This is what the campaign layer (C2+)
writes back onto its persistent armies and cities.

**Decisions.**
- **`BattleResult.sides: [SideReport; 2]`** (`report(side)`), built in `battle_end` on the
  state the tick ended in, so it is the same tick as the outcome.
  - `units`: every unit on the field (Generals included, flagged) plus every escapee (flagged
    `escaped`, with the HP it left with), sorted by `SimId`. A sim id is stable across
    processes, and an entity id is not (F-041).
  - `buildings`: every standing building with its HP (the RON pool if it was never hit),
    sorted by `SimId`.
  - `generals`: one `GeneralFate { unit, survived, escaped }` per General the setup fielded,
    in setup order, so a fallen General still has an entry.
- **Fielded Generals are tracked from `install`.** `BattleState` keeps `(entity, side, def,
  escaped)` for each one. `withdraw` sets `escaped` when he crosses the edge. A General
  survived if he is still on the field or escaped.
- **Hash.** The fielded list (side, def, escaped flag; no entity bits) and the report are in
  `BattleState::hash_words`. That covers battle mode only, so a skirmish hash is untouched.

**Not yet.** Veteran XP and kills per General are C4. Applying the report to a campaign army
is C2.

## F-058 — The battle AI: guard the General, retreat on a RON share, fight without a city (C1)

**Context.** C1 AC7 says the AI protects its General and retreats on a data-driven
threshold. The numbers are in `units.ron` `mvp_general_ai` (`guard_radius: 150.0`,
`retreat_at_army_lost: 0.6`), as the brief sketches them. They are validated (radius finite
and > 0, share in (0, 1]) and required wherever there are Generals.

**Decisions.**
- **Opt-in.** The commander reads `Option<Res<BattleState>>`. In a skirmish every new branch
  is dead, and the script plays, RNG draws included, exactly as before. Steps 1–4 (mine,
  workers, tech, army) moved verbatim into `economy()`. The wave step reads the same
  snapshot as before.
- **Retreat (step 0).** The commander keeps `peak_army`, the most combat units (roster units
  with offense > 0; never a General or a gatherer) it has seen at one decision. Once
  `(peak - now) * 1000 >= peak * retreat_milli` it issues `Order::Retreat` for its own side,
  journals `BattleAiAction::Retreat`, and from then on decides nothing (the withdrawal commands its
  units). The comparison is integer, using thousandths rounded once from the RON float. Peak,
  not the starting roster, is the yardstick: an army that grew and then lost 60% of its best
  is beaten.
- **Guard (step 5a).** If an enemy unit with offense > 0 (an enemy General counts; a worker
  does not) is within `guard_radius` of one of its Generals, the first such General by
  entity, the commander attack-moves its whole army onto him in place of this decision's
  wave, and journals `BattleAiAction::Guard { general, force }`.
- **A journal of its own.** Guard and retreat go to `BattleAiJournal` (`BattleAiAction`),
  inserted only by `battle::install`, not as new `AiAction` variants. The skirmish action
  trace and every exhaustive match over `AiAction` stay as they were, and a skirmish world
  holds no extra resource (critic_p2's allocation order).
- **No city.** In a battle, a side with no HQ still guards and attacks. It cannot grow, so
  its wave size is `min(attack_at_army, peak_army)`. With no enemy HQ, the wave goes for the
  enemy's first General. A skirmish commander without an HQ still does nothing, as before.

**Not yet.** The brief's other trigger, the enemy outnumbering it by a set ratio, is not
built. The share-lost threshold is the one the AC names. A General does not move himself.

## F-059 — The golden licence: no re-pin, and a skirmish cannot see the campaign layer (C1)

**Context.** C1 AC8 lets the skirmish keep its pinned results only if they reproduce exactly.
A re-pin needs its own finding and a human sign-off.

**Result: no re-pin.** Every existing golden passes unchanged on the C1 tree: b1_matchup,
b1_strategies, b35_queue_depth, b35_parallel, critic_b2_ac4 (state hashes), critic_b35_ac0,
critic_b35_ac0b, b2_headless, b2_orientation, critic_b2_ac2, m5_replay (recorded replays) and
critic_p2 (allocation order). m4c_ai (the AI's skirmish script and journal) passes unchanged too.

**Why it holds, and the test that says so (`tests/c1_golden.rs`).**
- The default `MatchSettings` has no battle. A skirmish world holds no `BattleState`,
  `AuraField` or `BattleAiJournal` and no `General`, so every battle system's `run_if` is false,
  the new hash tags emit nothing, and the new AI branches are dead.
- The data changes are additive: the `generals` list, `mvp_battle` and `mvp_general_ai` blocks.
  `content.units` is unchanged. The content **fingerprint** covers the new blocks when they are
  present, so the shipped content fingerprints differently from pre-C1 content, and a replay log
  recorded before C1 is refused rather than replayed against different rules.
  Content with no Generals and no battle blocks fingerprints exactly as before. No test pins a
  fingerprint value, so this is not a re-pin.
- The test cuts everything from the `// ---- C1: the campaign battle layer` marker out of
  `units.ron` and plays the same skirmishes (seeds 0, 4 and 11 for 1800 ticks; rush vs turtle
  and mass_ripper vs mass_bulwark for 3600) on both contents. The per-tick state hashes, the AI
  journal length and the outcome tick are identical.

## F-060 — The B3 gate on campaign battles: it runs, and it reads Fail on K1, K2 and K3 (C1)

**Context.** C1 AC9 asks for the B3 kill-criteria gate to run on campaign-style battles
(a General per side, the battle AI's guard and retreat on), and for B5 to be re-read by a
human playing them. This entry is the first half. **The B5 human re-read is not done; the AC9
box stays unticked until it is.**

**Method.** `tests/c1_balance.rs` `b3_gate_on_campaign_battles` (ignored; run with
`cargo test --release --test c1_balance -- --ignored --nocapture`). It uses the B3 pentagon
batch: the five mass probes, every ordered pair, both orientations, seeds 0–5, so 300 matches.
Seed k fields the k-th General kind on **both** sides, a mirror of Generals, so every aura is
played and the strategies are what differs. It uses the skirmish start, the default
`GateSpec`, the same `KillGate::of` and the same `MatchRecord`s. The suite keeps a small batch
(`campaign_battles_feed_the_b3_gate_and_every_decision_has_a_battle_cause`, 8 matches) that
checks every decided match has a battle loss cause for the loser and none for the winner, and
that the gate reads a battle batch as it reads a skirmish one. Run time was 596 s, release,
on the box.

**Reading (2026-11-02, the C1 tree).** Status: **Fail**.

| | value | interval | status |
|---|---|---|---|
| K1 mass_sentinel | 52.1% | [39.8, 64.4] | pass |
| K1 mass_ripper | **87.5%** | [77.2, 95.6] | **FAIL, dominant** |
| K1 mass_arclight | 71.9% | [59.8, 82.9] | undetermined |
| K1 mass_bulwark | 28.1% | [17.1, 40.2] | pass |
| K1 mass_ravager | 10.4% | [2.9, 20.3] | pass, losing |
| K2 slot A | 47.5% (n 60) | | undetermined |
| K2 left base | **24.2%** | | **FAIL** |
| K3 decided median | 9960 ticks (2:46) | | |
| K3 timeouts | 0% | | pass |
| K3 band share (5–8 min) | **0.7%** | | **FAIL** |
| K3 below / beyond band | 96% / 3.3% | | reported |

Loss causes over the 300 matches: Generals fell 192, HQ lost 0, retreated 109, timeouts 0. Each
General kind decided all 50 of its matches. Mean decided lengths were synthesis 11480,
bulwark 11847, frenzy 11849, focus 11829, logistics 10843 and endurance 11384 ticks.

**What it says.**
- **K3: battles are short, by construction.** A General stands `general_front` (80) in front of
  his base, so the first wave that breaks through reaches him well before it could raze an HQ.
  No battle ended on an HQ. 96% end before the 5-minute mark. The 5–8 minute band was set for
  a skirmish decided by razing (DESIGN_BRIEF), and whether a campaign battle should meet it is
  a design question for B5 and C4, not something to tune here.
- **Retreat is a real outcome.** About a third of losses (109 of 301 loss causes) are the AI
  withdrawing at 60% of its peak army lost (`retreat_at_army_lost`). That is the outcome a
  campaign carries forward with survivors (F-057).
- **K1: mass_ripper dominates**, as it already does in skirmish readings (F-048 shows it at
  100% in every attack-move candidate). The General layer does not cause it. Ravager is the
  weak probe here.
- **K2: the left base won 24% of the 60 mirrors.** Slot A is even (47.5%), so the bias is not
  faction order. Skirmish readings have shown per-mirror left-base FAILs before (F-048: the
  sentinel and ravager mirrors at 0%), because a deterministic mirror tends to give the same
  winner across seeds. With a General as the target, a mirror is decided by a single early
  fight, which makes that tendency stronger. The battle geometry is symmetric: `SideGround`
  fronts point at x = 0 from bases at x = ±750, and the entry edges mirror. **Not investigated
  further in C1.** It is flagged for B5 and the critic.

**Update 1 (2026-11-02, after F-061's mutual-retreat rule). Re-run, same batch and seeds.**
Status: **Fail** (K1 and K3). **K2 is now undetermined.**

| | first reading | after F-061 |
|---|---|---|
| K1 mass_sentinel | 52.1% pass | 53.1% [40.9, 65.4] undetermined |
| K1 mass_ripper | 87.5% FAIL, dominant | **87.0% [76.6, 95.3] FAIL, dominant** |
| K1 mass_arclight | 71.9% undetermined | 69.3% [57.1, 80.6] undetermined |
| K1 mass_bulwark | 28.1% pass | 28.1% [17.1, 40.2] pass |
| K1 mass_ravager | 10.4% pass, losing | 12.5% [4.4, 22.8] pass, losing |
| K2 slot A | 47.5% undetermined | 46.7% (n 60) undetermined |
| K2 left base | **24.2% FAIL** | **46.7% undetermined** |
| K3 median / band share | 9960 ticks / 0.7% FAIL | 9960 ticks / **0.7% FAIL** (96% below, 3.3% beyond, 0 timeouts) |
| Loss causes | Generals fell 192, HQ 0, retreated 109 | Generals fell 192, HQ 0, **retreated 157** |

Mean decided lengths per General kind barely moved (10847 to 11860 ticks). The loss causes
now total 349 over 300 decided matches, so 49 matches had two losers. Before the fix there
was 1. About 48 battles were **mutual retreats** that the old rule had handed to the slower
withdrawer. That accounts for the **K2 left-base FAIL in the first reading**: which side's
withdrawal finished last depended on geometry, not play. With the rule fixed, the seat bias is
gone. The K1 and K3 readings stand as interpreted above. No content was tuned.

**Not done (AC9).** The B5 human re-read on campaign battles. No content was tuned against
this reading. Balance changes are data and design decisions for B5.

## F-061 — C1 critic fixes: retreats finish on any front, a mutual retreat has no winner (C1)

**Context.** The C1 critic returned FAIL. Its probes are landed as `tests/critic_c1.rs`.

**1. A retreat now finishes on any front (HIGH).** A retreating unit walks to
`Edge::exit_for(pos)`, the nearest point of its entry edge, and lands there only up to float
rounding. When the front is not axis-aligned, or the base is at a non-round coordinate,
`beyond()` came out near -1e-6. The test `beyond >= 0` then never held, and the unit was
re-ordered every tick forever. The escape test is now `Edge::reached`: on or beyond the edge
within `Edge::ON_EDGE = 0.01` world units. This is a numerical tolerance, not a gameplay
number. It is far above the rounding error and far below a tick's movement or anything
visible. So it is a named constant in `battle.rs`, not RON. The result is deterministic (pure
f32 arithmetic on the sim's own state). Gate: `critic_retreat_finishes_on_a_diagonal_front`
(eight fronts, 10 to 333 degrees) and `critic_retreat_finishes_on_an_axis_front_with_odd_base`.

**2. A mutual retreat crowns no one (MEDIUM). Coordinator's decision, recorded here.**
`BattleState` now keeps the tick each side ordered its retreat (`retreat_ordered`, hashed).
- Only the **earliest** order counts. A side that ordered later still withdraws, but it never
  loses by retreat. The earlier side is the retreating loser once its withdrawal is complete,
  even if the later side got off first. The battle waits for the earlier side.
- Two orders on **one tick** are a **mutual retreat**. The battle ends when both withdrawals
  are complete: `winner: None`, both sides `LossCause::Retreated`, and every escapee of both
  sides listed. A General falling during the withdrawal still loses that side the battle as
  `GeneralsFell`, which is checked first, as before.
- **How C4 reads a mutual retreat:** the attack failed and the defender keeps the city. Both
  armies survive as the report lists them, so a mutual retreat is a failed siege, not a draw
  that changes ownership.
- Gates: `critic_a_mutual_retreat_does_not_crown_the_slower_withdrawer`, and in
  `tests/c1_retreat.rs`
  `a_mutual_retreat_on_one_tick_has_no_winner_and_both_sides_escapees` and
  `of_two_retreats_the_earlier_order_loses_even_if_the_later_side_is_off_first`.

**3 and 4. Aura tests (LOW).** `tests/c1_aura.rs` now places its enemy *inside* the radius
(it stood outside, so the side filter was untested there).
`critic_an_enemy_inside_the_radius_gets_no_aura`,
`critic_synthesis_ignores_enemy_domains` and
`critic_synthesis_cap_binds_when_below_the_domain_count` (RON max 2, three domains, x2) are in
the suite.

**5a. A large roster starts on the field.** Ranks ran back one `formation_spacing` each from
just behind the Generals, so a ninth row (more than 64 units at rank 8) stood behind the
entry edge. The layout now fits `floor((general_front + entry_edge_offset) /
formation_spacing) - 1` ranks, at least 1. That is 6 with the shipped numbers, and the last
rank stands at least one spacing inside the edge. A bigger roster widens every rank to
`ceil(units / ranks)` instead of adding ranks. Up to 48 units, the layout is unchanged. Gate:
`a_large_roster_starts_entirely_on_the_field` (8 to 250 units, all on the field, no shared
spot).

**5b. Auras skip Generals.** This is kept. DESIGN_BRIEF now says so: an aura buffs friendly
non-General units.

**5c. `BattleSetup` is not in `MatchLog`.** This is left for C4/C6, as F-053's "Not yet" says:
a battle cannot be replayed from its log until the log carries the setup (Generals, rosters
and ground). Until then, a battle's log cannot rebuild the battle it recorded.

**Addendum (critic re-review, LOW): the layout is refused when it leaves no room for a rank.**
The rank count was forced to at least 1, so when `general_front + entry_edge_offset <
2 * formation_spacing` the first rank, one spacing behind the Generals, stood less than one
spacing inside the entry edge, or beyond it. With front 10, spacing 30 and offset 10, every
roster unit spawned 10 past its own edge. **Coordinator's decision:** content validation now
refuses such an `mvp_battle` block, with an error naming `general_front`, `entry_edge_offset` and
`formation_spacing`. So "at least one rank, standing at least one spacing inside the edge"
always holds, and `spawn_roster` keeps a `debug_assert!` on it. The shipped numbers (80 + 150
against 2 x 30) are unaffected. Gates in `tests/c1_roster.rs`:
- `a_layout_with_no_room_for_a_rank_is_refused_at_load`: front 10, spacing 30 and offset 10 are
  refused, and so is the near-boundary case 20 + 39.5 < 60.
- `a_layout_exactly_two_spacings_deep_loads_and_keeps_every_unit_on_the_field`: 20 + 40 ==
  2 x 30 loads, and 1, 8 and 30 units all start at least one spacing inside the edge.

## F-062 — C2 starts ahead of B5, by the user's decision (C2)

**Context.** `CAMPAIGN_PLAN.md` gates C1 onward on B5 Go, the human play test, which has not
run. The user overrode that gate for C1 on 2026-11-02 (F-052 onward). On 2026-11-03 the user
said "start c2".

**Decision.** C2 (nations in the 1v1, light traits on Alloy) starts now, on the same terms as
C1: B5 still runs when the user can play, and its findings can revise C1, C2 and later
milestones. The C2 heading in `CAMPAIGN_PLAN.md` now says "started 2026-11-03, ahead of B5 by
user decision", and the plan's gate section records the override.

**What this does not change.** C1's open AC (the B5 human re-read of campaign battles) stays
open. Nothing in C2 re-pins a golden: the "no nation" default must reproduce today's results
exactly (C2 AC2).

## F-063 — Nations as data: how the traits reach the sim (C2 AC1)

**Context.** C2 AC1 asks for `nations.ron` with C0's roster, traits as data-driven modifiers
(never code per nation), and selection through `MatchSettings`, one nation per side, default none.

**Decisions.**
- **Traits rewrite a side's copy of the content.** `nation::SideRules::build` clones the roster
  `UnitDef`s and `BuildingDef`s and applies the nation's traits in file order. Targets are
  `Unit(id)`, `Barracks(id)` (everything a building trains), `Workers`, `CombatUnits`,
  `Building(id)` and `Buildings`. Stats are a `mul` (cost, train, gather and attack ticks,
  building HP) or an `add` (carry, reach, speed, offense, defense, armor). A `mul` is held as
  integer per-mille and applied as round-half-up(v * m / 1000), with no float in the sim. The
  traited copy must pass the same load checks as the base content (max_stat, non-zero
  cost/train/carry, reach within engage and attack-move range, no combat stat on a
  non-combatant, no carry on a non-gatherer). Otherwise the file is refused at load.
- **The layer exists only when it is used.** `NationRules` is an optional resource, inserted
  by `headless::battle` only when a side names a nation (the C1 pattern; critic_p2's "no
  init_resource" check still holds). Every per-faction read goes through `nation::Defs`. With
  no `NationRules`, `Defs` returns exactly the old `Content` lookup, so the golden licence
  (AC2) holds by construction. The per-tick state hash emits a new row tag (`NATIONS = 25`)
  only when the resource exists, and the content fingerprint mixes in a "nations" section only
  when `nations.ron` is present.
- **Stand-in traits (Alloy only):** House none (its edge is the Synthesis aura); Forgeborn
  foundry units cost x0.90; Verdant gene-vats units train x0.90; Aetherkin Arclight reach +12;
  Tollmarch workers carry +1; Ashen Reach building HP x1.15. These are first guesses; the AC4
  gate tunes them.
- **Strategies are orthogonal.** A nation names its battle-personality script, but
  `MatchSettings` keeps nations and strategies separate. A side with no named strategy plays the
  content default whatever its nation, so a nation axis never silently changes a strategy
  matchup. The batch (AC3) chooses which pairing to run.
- **Not modelled:** a RepairRate trait (the sim has no repair) and build time (buildings are
  instant). Diplomatic disposition is campaign-layer data (C5) and is not in this file yet.

**Known gaps.**
- `MatchLog` records no nations, the same gap as F-061's `BattleSetup`. ~~A replay of a nation
  match against the wrong content or settings is caught by the state hash on its first tick
  (the NATIONS rows), not rebuilt.~~ **Overstated, corrected in F-069:** nothing compared
  hashes during a replay, so the replay ran, unrefused, as a different match (the critic's
  test diverged at tick 0). The log now records the nations and the replay installs them.
- `BattleSetup::check` validates a roster building's HP against the base pool, so a campaign
  roster cannot name HP above the base maximum even for Ashen Reach.

**One older test's premise moved, no value re-pinned.** `tests/b1_strategies.rs`
`the_fingerprint_covers_every_strategy_and_every_field` compared an edited copy of the content
(units, resources and a strategies file) against the fingerprint of the **full** shipped content.
The shipped content now also holds `nations.ron`, which the fingerprint sees (a nation match
depends on it), so the full content and the three-file copy are different content. The copy
cannot include `nations.ron` either, because a nation names shipped strategies and would not
load beside an edited set. The test's base is now the shipped strategies through the same
three-file harness. Its claims are unchanged: every edit moves the hash, and an untouched copy
hashes the same as the base. This was the only failure in the full debug suite on the AC1 tree.

**Gates:** `tests/c2_nations.rs` (17 tests).

## F-064 — The golden licence: no re-pin, and a no-nation match cannot see the nations (C2)

**Claim.** With no nation named (the default, or `with_nations(None, None)`), a match plays
exactly as it did before C2. No golden was re-pinned. The pinned tests (b1, b2, b35,
critic_b2_ac4's state hashes, m5's replays, critic_p2's allocation order, c1_golden) pass
unchanged in the full debug suite on the AC1 tree.

**What `tests/c2_golden.rs` adds.** The shipped content, and the same content with
`nations.ron` removed (the content before C2), play the same matches. They match tick for tick
and hash for hash, with the same AI journal length and end tick, and no `NationRules` resource:
- seeds 0, 4 and 11 at 1 800 ticks;
- rush/turtle, mass_ripper/mass_bulwark and synth_triad/mvp at 3 600 ticks;
- a C1 campaign battle (bulwark and frenzy Generals) at 3 000 ticks;
- seed 2 rush/mvp played to its decision inside the default cap.

Naming no nation explicitly is the default, hash for hash.

**Red, shown by mutation** (both reverted, on the box):
- `NationRules::resolve` returns empty rules for `[None, None]`. The skirmish test fails
  (`NationRules` present). The content comparisons alone cannot see this, since both runs carry
  the mutation; the pinned state-hash goldens are what guard it.
- `resolve` falls back to the content's second nation (Forgeborn) when none is named. Four of
  the five tests fail: skirmish, named strategies, campaign battle and the decided match.

## F-065 — The batch runner's nation axis: mirror, cross and personality (C2)

**Context.** C2 AC3 asks the batch runner for a nation axis. Mirror matchups should keep
isolating unit balance, and cross-nation matchups should test faction balance.

**Decisions.**
- **A nation batch plays fixtures.** A fixture is a nation and a strategy per slot.
  `batch::run_fixtures` plays each fixture on every seed in both orientations: seed-major, then
  fixture order, then orientation. It runs sequentially, and checks every name before the first
  match. `MatchRecord` is unchanged, so every older test and reader still holds. A
  `NationRecord` carries the record plus the two nation ids.
  `NationRecord::by_nation` relabels a record's rows by nation, so the nation matrix is read by
  the same `WinMatrix` and gate arithmetic as the strategy matrix.
- **Three axes** (`NationAxis`, all lists in content order, filters never reorder):
  - **mirror**: each nation against itself, on every ordered strategy pair. The traits are
    the same on both sides and cancel, so this is unit balance under each nation.
  - **cross**: every ordered pair of different nations, on each strategy played by both
    sides. The strategy is the control, so this is faction balance from the traits alone.
  - **personality**: every ordered pair of different nations, each playing its own script
    (`NationDef::strategy`). This is how a player meets them: traits and script together.
- **The cross axis, not personality, is faction balance.** Personality strength is mostly
  strategy strength: F-049's table has the `rush` row at about 11%, and rush is Verdant's
  script. Trait tuning cannot fix that within C0's bounds, and base stats may not be retuned
  for C2. Personality is played and reported beside the gate (F-066), but not gated.
- **The `balance` binary** gains `--nations mirror|cross|personality` and
  `--only-nations a,b`. It prints the tally, the nation matrix and the strategy matrix of the
  same records. `--report` is refused with `--nations` (the report file belongs to the strategy
  batch), and so is `--only-nations` without `--nations`.

**Gates** (`tests/c2_batch.rs`, 12 tests):
- fixture counts and order for each axis;
- content order, and filters never reorder;
- unknown names are refused before any match;
- every row equals the same match played directly;
- a re-run is identical;
- the traitless House mirror equals the plain strategy batch, record for record;
- traits are visible on the cross axis: Verdant on rush trains 6 rippers by tick 6 000 against
  House's 5, and the House mirror trains 5 and 5;
- the nation matrix is read from relabelled records;
- the binary plays and refuses as documented.

## F-066 — The nation gate: it runs, and it reads Fail. Tollmarch's +1 carry is +50% per trip (C2)

**Context.** C2 AC4 asks for a nation criterion with two parts. Each nation's cross-nation
strength must be 35–65%, and the pentagon must still hold under every nation pair. Tuning
happens in `nations.ron` only, inside the brief's bounds, and base unit stats are never
retuned.

**Decisions.**
- **`gate::NationGate`** reads two batches through the B3 arithmetic:
  - **Strength** comes from the **cross** axis (F-065). The same strategy is on both sides, so
    a nation's row is what its traits are worth. Mirrors and timeouts are excluded. A row is
    the mean of its cells (`Reading::mean_of`, Wilson with the F-031 design effect, ICC 0.17),
    judged `Within` 50% ± 15 points. The clusters are the distinct (strategy, seed) per pair.
    - **Pass** needs the whole interval inside the band.
    - **Fail** needs the whole interval outside it.
    - Anything else is undetermined.
  - **The pentagon** comes from `pentagon::nation_pentagon_fixtures`: every ordered pair of
    nations (mirrors included) plays each measurable link's probe pair, predator nation
    against prey nation, in both slots. Each pair's records go through
    `PentagonReport::of_records`. Holds is Pass, Fails is Fail, and anything else is
    undetermined. The pentagon passes only if every link holds under every pair.
  - The gate is `Status::all` of the two parts.
- **The probes** are the five mass strategies, one per unit, taken from the derived cycle's
  predator strategies. A mass strategy is the cleanest control: one unit type, so a trait on
  that unit or its barracks shows up undiluted.
- **Personality strength** (each nation on its own script) is played and printed, but not
  gated (F-065). It is strategy-dominated: Verdant's rush is 20%.
- `tests/c2_gate.rs` has 8 synthetic tests (Pass, a dominant nation, a weak nation, thin
  samples, mirrors and timeouts excluded, one broken pair, fixture count and order). The
  shipped reading is `the_shipped_nation_reading`: ignored, release, with `ONUS_C2_SEEDS` and
  `ONUS_C2_PART`.

**The reading** (release, on the box, 2026-11-03, shipped stand-in traits).

*Cross strength* (gated): 3 seeds, 900 matches, 64 timeouts, 4 367 s. Result: **FAIL**.

| nation | strength | interval | n / n_eff | status |
|---|---|---|---|---|
| house | 31.7% | [25.0, 38.7] | 271 / 187 | undetermined |
| forgeborn | 50.6% | [43.6, 57.6] | 294 / 196 | PASS |
| verdant | 45.1% | [38.0, 52.3] | 272 / 187 | PASS |
| aetherkin | 43.3% | [36.2, 50.5] | 272 / 187 | PASS |
| tollmarch | 94.7% | [90.6, 97.8] | 300 / 199 | **FAIL** |
| ashen_reach | 34.6% | [27.7, 41.7] | 263 / 184 | undetermined |

The nation matrix (row against column, %, in the same order):

```
house          --   32   40   38    2   46
forgeborn      68   --   59   58    2   67
verdant        60   41   --   48   20   57
aetherkin      62   42   52   --    2   59
tollmarch      98   98   80   98   --   98
ashen_reach    54   33   43   41    2   --
```

*The pentagon under every nation pair* (gated): 2 seeds, 1 440 matches, 68 timeouts, 4 888 s.
Result: **FAIL**.
- It fails in exactly the five pairs where **Tollmarch is the prey of another nation**. There,
  rav>sen reads 0% (8/8 decided against the predator) in every pair, and sen>rip reads 0% in
  four of the five (it holds under Forgeborn). Out-gathering by half again overturns a hard
  counter.
- With Tollmarch as the predator, and in the Tollmarch mirror, every link holds.
- Every other link holds in every other pair, except **sen>rip**. That link is undetermined
  (4 decided, 4 timeouts; 3/5 with Ashen Reach as predator) in 16 pairs, including the
  traitless House/House mirror. This is F-049's baseline sen>rip, which no trait causes and
  no trait can fix.
- Pairs with every link holding: 15 of 36 (5 Fail, 16 undetermined).

*Personality strength* (reported, not gated): 2 seeds, 120 matches, 1 timeout, 316 s.
- House 37.1%, Forgeborn 80.0%, Verdant 20.0%, Aetherkin 62.5%, Tollmarch 77.5%, Ashen Reach
  22.9%. All are undetermined.
- The cells are near 0 or 100: a script pair decides the matchup.

**Why it cannot pass by magnitudes alone.** No tuning was made, so `nations.ron` is unchanged.
- **Tollmarch.** The brief fixes its stand-in as "+x worker carry", and `CarryCapacity` is an
  integer `add`. F-029 cut base carry from 10 to 2, so the smallest non-zero x (+1) is a 50%
  larger load per trip. That is the 94.7% row and the five broken pentagons. The only smaller
  magnitude is 0, which deletes the perk. Getting under the band needs a decision outside
  "tune the magnitude":
  - express "workers carry more" as a `GatherTicks` `mul` (a fine-grained income lever);
  - pair it with a cost;
  - or make carry fractional.
  Any of these is the user's decision.
- **House Vael.** Its brief stand-in is "Synthesis" (a bonus beside other-domain units). The
  trait vocabulary cannot express that, so its trait list is `[]` (F-063). Without Tollmarch
  its row averages about 39% (32, 40, 38, 46 — read off the matrix, unweighted). With
  intervals near ±7 points that reads undetermined at best. The traitless nation will sit
  below the traited field until Synthesis exists.
- **The rest, read off the matrix without the Tollmarch column:**
  - Forgeborn is about 63%, near the top edge. Its foundry discount (0.90) is the first
    candidate to soften.
  - Verdant is about 52% and Aetherkin about 54%.
  - Ashen Reach is about 43%. Its building HP (1.15) is the candidate to raise.
  These are the next magnitudes to try once Tollmarch's perk is settled. Tuning them first
  would be measured against a field Tollmarch distorts.
- **The pentagon's sen>rip** is undetermined in the traitless mirror. So "holds under every
  pair" cannot read Pass on today's base content even with every trait at zero. Base stats
  are out of C2's reach.

AC4 stays **unticked**. The criterion, its tests and the measurement are committed.

## F-067 — The client picks the nations before the first tick, and shows them by colour and name (C2)

**Context.** C2 AC5 asks for two things. The player picks a nation, and sees the opponent's
nation at a glance (pillar P3: a colour plus a name label). The driver must stay thin: no sim
logic in `src/ui.rs`, `src/input.rs` or `src/setup.rs`.

**Decisions.**
- **A pre-match pick, behind the sim's own tick gate.** `setup` inserts a client-side
  `NationPick` and closes the `TickGate` (M6's gate; `may_tick` gates the whole chain). Nothing
  is played while the player picks, so the match's tick 0 is still the first tick it plays.
  - Left/Right cycles your nation and Down/Up the opponent's, through "No nation" and every
    nation in content order.
  - Enter marks the pick chosen. `input::pick_nations` only ever writes `NationPick`.
- **Applying the pick is match setup, not an order.** `setup::start_match` is an exclusive
  system that runs once, when the pick is chosen and before the first tick:
  - it installs the nations through `sim::nation::install`, the same function a headless match
    uses, so the start gets its side's HP pools;
  - when the opponent has a nation, it gives the opponent's commander that nation's personality
    (`NationDef::strategy`), seeded from `MATCH_SEED` as before;
  - it opens the gate.
  After that the pick is `Playing` and the keys do nothing. "No nation" on both sides inserts
  no `NationRules` and keeps the default AI, which is the match as it was before C2.
- **What is shown is read off the sim once playing.** The two labels (you top-left, the
  opponent top-right) and the building tint read `NationRules::nation_of`; before the start they
  show the pick. Colour is `nations.ron`'s `colour`. No nation wears the old neutral
  `BUILDING_COLOR`. Units are filled by kind, so each unit of a side with a nation gets a ring in
  its nation's colour (`ui::draw_nation_marks`, a `Gizmos` system).
- **The train line prices what the player's nation pays.** It reads `Defs::roster_unit`, so a
  Forgeborn foundry shows the discounted price.
- **Order emitters wait for the start** (`client::match_started`). An order clicked during the
  pick would otherwise queue for tick 1.
- **One registration.** `add_nation_pick` is shared by `build_app` and the tests (F-004). The
  pick's entities and resources are spawned **last** in `setup`, so nothing older moves in
  allocation order (F-011). `critic_p2`'s ordering guard still holds.

**Gates** (`tests/c2_client.rs`, 7 tests, under `MinimalPlugins` through the shipped `setup`,
`add_nation_pick` and sim chain):
- the cycle order and its wrap;
- the match waits on the pick: the gate is shut, 30 frames play no tick, and there are no rules;
- the picked nations are the match's nations, and the opponent's AI plays its personality;
- Ashen Reach's HQ gets the traited pool;
- once playing, the keys change nothing;
- both labels carry name and nation colour, and the HQs wear it;
- no nation on both sides is today's match;
- the panel's train price is Forgeborn's.
A mutation demo turned the building tint neutral and kept the default AI: two tests went red.

**Needs a human on the Mac.** The layout and legibility of the labels and picker, whether the
six colours tell the sides apart on the map, and the unit rings (gizmos need the render
plugins). `cargo run` now opens on the pick screen; press Enter to play.

**Known gap, not fixed here (outside C2 AC5's files).** The command log carries no nations.
A replay written from a nation match would replay without its traits. ~~The state hash includes
the nations (tag 25), so a hash check would fail loudly at tick 0 rather than silently
diverge.~~ **Overstated, corrected in F-069:** only a caller that compares two hash logs sees
that divergence. The replay itself accepted the log and played a different match. Fixed in
F-069: the log names its nations and the replay applies or refuses them.

## F-068 — C2 critic fixes: a roster's HP is checked against its side's traited pool (C2)

**Defect (critic, Medium-High).** `BattleSetup::check` checked a roster's HP against the
**base** pools, but `spawn_roster` fields it under the side's **traited** pool.
- A nation whose traits shrink a pool could start a unit above its maximum. A Defense −1
  sentinel started at 100/80, and a BuildingHp ×0.5 foundry at 5040/2520.
- That broke the `current <= max` invariant every HP reader assumes.

**Fix.** `BattleSetup::check_with(content, rules)` checks HP against `Defs::unit_health` and
`Defs::building_health` of the side, and `check` is now `check_with(content, None)`.
`headless::battle` resolves the nations first, then checks the battle with them, before
anything is spawned. Out-of-pool HP is **refused** (`BadHp`, naming the side's pool), never
clamped: a trimmed roster would hide a campaign bug that wrote an HP the side cannot have.

**Gates.** The critic's tests are landed as `tests/critic_c2.rs` and
`tests/critic_c2_client.rs`.
- `a_roster_unit_never_starts_above_its_sides_pool` and
  `a_roster_building_never_starts_above_its_sides_pool` failed before the fix. They now expect
  the refusal, with the traited max in the error, and an admitted roster at that max with
  `current <= max`.

## F-069 — C2 critic fixes: a nation match's log records its nations, and a replay rebuilds them (C2)

**Defect (critic, Medium).** `MatchLog` carried the seed, the content fingerprint and the
commands, but not the nations. A Forgeborn-vs-Tollmarch log replayed into a world built from
its seed played a no-nation match. Nothing refused it, and it diverged at tick 0. F-063's and
F-067's notes said the state hash would catch this; it only does for a caller that compares
two hash logs (both notes are now marked as corrected).

**Fix.**
- `MatchLog::nations: [Option<String>; 2]`, per faction slot. `#[serde(default)]`, and not
  written when both are none, so a pre-C2 log reads as a no-nation match and a no-nation match
  writes the same bytes as before. `LOG_FORMAT_VERSION` stays 2: the change only adds a field.
- **The sim stamps the field itself** in `replay::stamp_content`, from `NationRules`
  (`nation::ids`), the same way it stamps the content fingerprint. A log never depends on a
  caller remembering to stamp it. If the nations change after the first stamp, the log is
  marked `content_changed` and refused at both boundaries, like a content change.
- `MatchLog::matches_content` refuses a log naming a nation the content lacks. That check
  covers the front door (`load_for` / `from_ron_for`) and `feed_replay`'s backstop.
- **`replay::apply_log_nations`** is an exclusive system that runs once, just before
  `feed_replay`, when a `ReplaySource` exists. It handles four cases:
  - It refuses a log that does not match the content.
  - It accepts a world that already plays the log's nations.
  - In a world with no nations at tick 0, it installs the log's nations through
    `nation::install`. This is the path headless setup and the client's `start_match` also use.
  - It refuses anything else: other nations, or a match already under way. A refused replay
    feeds nothing.

**Gates.**
- `tests/critic_c2.rs` `a_nation_match_log_replays_faithfully_or_is_refused` failed before
  the fix (diverged at tick 0). It now replays faithfully, without a refusal.
- `tests/c2_nations.rs`:
  - `a_nation_match_log_names_its_nations_and_replays_them_faithfully` covers both sides
    (1 200 ticks) and one side. The replay comes hash for hash from a seed-only world, and
    the replay's own log names the same nations.
  - `a_no_nation_log_is_written_as_before_and_a_pre_c2_log_reads_as_no_nations`.
  - `a_log_naming_a_nation_this_content_lacks_is_refused` covers both the front door and the
    backstop. The refused replay feeds nothing and installs nothing.
  - `a_log_whose_nations_disagree_with_the_world_is_refused` covers both directions. A world
    whose setup already named the log's nations is accepted.
- The M5/M6/P2 replay suites and the c1/c2 goldens pass unchanged.

## F-070 — C2 critic fixes: every trait read site has a test that sees the untraited table (C2)

**Defect (critic, Low-Medium).** Two kinds of read site took a side's traited definitions, but
no test failed if either was switched back to the untraited `content` lookup:
- combat's defender side (`src/sim/combat.rs`, `defs.unit(prey_side, ..)` and
  `defs.building(prey_side, ..)`);
- the AI's prices (`src/sim/ai.rs`), for the worker, the opening barracks and the army unit.

**Gates, each shown red by its mutation on the box** (every site switched alone to `content`,
then restored):
- Unit defender: already guarded. critic_c2's
  `offense_and_armor_traits_reach_combat_on_both_ends_of_a_hit` fails under the mutation, since
  A's Armor +1 stops mitigating B's hit.
- Building defender: `c2_nations` `a_building_armor_trait_mitigates_hits_on_its_side_only`.
  A sentinel of each side hits the other side's foundry. A's foundry, with a Buildings
  Armor +1 trait, takes the mitigated hit, and B's takes the plain one.
- Worker and army price: `the_ai_prices_workers_and_army_at_its_sides_traited_cost`. A's
  workers and combat units cost ×100, so in 4 000 ticks A never orders either (it still builds
  a barracks), while B orders both. The worker mutation and the army mutation each fail it.
- Barracks price: `the_ai_prices_its_barracks_at_its_sides_traited_cost`. A's buildings cost
  ×100, so A never places a barracks in 4 000 ticks; B does.

No sim code changed: the sites were correct, they were only unguarded.

## F-071 — C2 critic fixes: BuildingHp rounds half up, like every other `mul` (C2)

**Defect (critic, Low).** `nations.ron` documents a `mul` as `round(v * m / 1000)`, applied
half up. BuildingHp instead composed its per-mille factors with truncating division and then
truncated the pool. A BuildingHp of 1.004 gave the HQ 8433 HP, where the rule gives
8433.6 → 8434.

**Fix.** Each building's pool starts at its base (`mvp_defense * building_hp_per_defense`).
Every BuildingHp trait then scales it in RON order with `scale_milli`, the helper every other
`mul` stat uses, so each trait rounds once, half up. The shipped Ashen Reach ×1.15 pools are
exact products (9660, 5796, 4830), so nothing shipped moved, and the goldens and the C2 suites
pass unchanged.

**Gate.** `c2_nations` `a_building_hp_trait_rounds_half_up_like_every_mul` fails before the
fix (8433 vs 8434). It covers ×1.004 on every building, where 8400 and 4200 round up and 5040
rounds down, plus two traits composing in RON order.

## F-072 — Tollmarch's stand-in trait is now faster gathering, not a bigger load (C2, user decision)

**Why.** Tollmarch's stand-in was `Workers CarryCapacity +1`. On a base carry of 2 that is +50%
Alloy per trip, and it read 94.7% cross-nation strength, the gate's only Fail (F-066). An `add`
on a value of 2 cannot be tuned in steps smaller than 50%. The user chose a gather-time
multiplier instead.

**Change.** The brief comes first (DESIGN_BRIEF Tollmarch sheet: speciality "workers gather
faster", stand-in "workers gather x% faster"), then the data: nations.ron changes Tollmarch's
trait to `(applies_to: Workers, stat: GatherTicks, mul: 0.90)`. A load takes round(90 × 0.90)
= 81 ticks instead of 90. The walk is unchanged, so the income gain per trip is under 10%.
`GatherTicks` already existed as a data-driven trait stat, and `economy::gather` already read
it through `Defs::roster_unit`. No sim code changed and there is no per-nation code.

**Gates.**
- `c2_nations` `tollmarch_workers_gather_faster` replaces `tollmarch_workers_carry_more`. It
  failed against the old RON (no `mul` on the trait). In a Tollmarch-vs-none match, A's
  longest harvest is 81 ticks and B's is 90, and both sides carry the base load.
- `c2_golden` (no-nation matches identical to pre-C2 content) passes unchanged.

The gate reading with this trait is in F-074.

## F-073 — House Vael's real trait: the mixed-army (Synthesis) trait kind (C2, user decision)

**Why.** House shipped with `traits: []`. Its edge was the Synthesis aura of its General, and a
skirmish has no General, so House played the 1v1 with no edge at all. It read 31.7% cross
strength, below the 35–65% band (F-066). The user chose a real Synthesis trait.

**The trait kind is data, with no per-nation code.**
- `NationDef::synthesis: Option<SynthesisDef>` holds `radius`, `modifiers` (C1's
  `AuraModifier`) and `max`.
- On every tick, each unit of the side **with a domain** gets `modifiers` × the number of
  distinct domains, **other than its own**, among the side's own non-General units within
  `radius`, capped at `max`.
- A mono-domain army gets nothing. Enemies never count. Workers and Generals neither count nor
  gain, since they have no domain.
- `#[serde(default)]`: a nation without the line has no such trait.

**Reusing C1.**
- The domain count is one helper, `battle::distinct_domains`. C1's `PerDistinctDomain` aura
  now calls it with no exception; the trait calls it with the unit's own domain excepted.
- The bonus is computed by the same `battle::auras` system into the same `AuraField`, as a kind
  of its own that stacks with any General's aura. Combat, movement and regen already read the
  field, so no read site changed.
- The modifiers are validated by the same check as a General's aura
  (`content::check_aura_modifiers`, extracted from the aura validation with its messages
  unchanged). There is no nemesis stat, so the trait cannot touch the hard counters, and
  `stat: Nemesis` is a load error.
- Also refused at load: `max` outside `1..=(domains − 1)`, and a radius that is not finite and
  positive.

**What changes, and when.**
- `nation::install` inserts the `AuraField` only when a side's nation has the trait, so the
  aura system runs in a skirmish for that match alone. A no-nation match, or one whose nations
  lack the trait, is untouched: no field, no new system run, and the same fingerprint.
- The content fingerprint mixes the trait in only when present.

**Data.** The brief comes first (DESIGN_BRIEF House sheet). In nations.ron, House has
`synthesis: (radius: 160.0, modifiers: [(stat: Offense, add: 1)], max: 2)`. That is +1 Offense
per other domain nearby, so +2 for a full Machine/Flesh/Energy mix. House's AI script,
`synth_triad`, trains Sentinel, Ripper and Arclight in turn, so its army is mixed.

**Gates (`tests/c2_nations.rs`, red before: no `synthesis` field).**
- `synthesis_is_active_beside_other_domains_and_scales_per_domain`: one other domain gives +1;
  two give +2, and a second Machine unit adds nothing new; a neighbour beyond the radius
  doesn't count.
- `synthesis_is_inactive_in_a_mono_domain_army`: two Machine units and a worker get nothing.
- `synthesis_counts_no_more_domains_than_its_max`: `max: 1` among three domains gives +1.
- `synthesis_counts_only_friends_and_only_its_own_side_gains`: a lone A unit among B's mixed
  units gets nothing, and B, which has no trait, gets nothing.
- `only_a_synthesis_nation_installs_the_field_and_the_hash_sees_the_trait`.
- `an_unplayable_synthesis_is_refused_at_load` covers seven cases, Nemesis included.
- `the_shipped_house_vael_has_the_synthesis_trait`.
- `c2_golden` (no-nation matches) and the C1 aura suites pass unchanged.

The gate reading with this trait is in F-074.

## F-074 — C2 nation gate after the two user decisions: readings and RON tuning steps (C2)

The coordinator allowed trait magnitudes in `nations.ron` to be tuned so that every nation
lands in 35–65% cross strength, with each step and its reading recorded here. The bound is
CAMPAIGN_PLAN's rule: no trait may move a pentagon link out of "holds", or a nation's
cross-nation strength outside 35–65%. All runs: `tests/c2_gate.rs`, release, 2 build jobs, `nice -n 19`.

**Step 0: the traits as shipped by F-072 and F-073.** House has Synthesis +1 Offense per other domain (max 2,
radius 160). Tollmarch gathers at ×0.90, Forgeborn's Foundry units cost ×0.90 and Ashen Reach's buildings have ×1.15 HP.
Cross, 3 seeds, 900 matches, 69 timeouts, 5104 s: **Fail**.

| nation | strength | 95% interval | reading |
|---|---|---|---|
| house | 34.1% | [27.3, 41.2] | undetermined |
| forgeborn | 59.6% | [52.6, 66.5] | undetermined |
| verdant | 46.5% | [39.4, 53.7] | holds |
| aetherkin | 49.8% | [42.6, 56.9] | holds |
| tollmarch | 73.0% | [66.4, 79.2] | **Fail** |
| ashen_reach | 37.0% | [30.0, 44.2] | undetermined |

Row vs column win %: house 32/40/38/14/46. Forgeborn 68/59/58/47/67. Verdant 60/41/48/27/57.
Aetherkin 62/42/52/34/59. Tollmarch 86/53/73/66/86. Ashen Reach 54/33/43/41/14.

- Tollmarch's carry trait (F-066) read 94.7%. Faster gathering reads 73.0%, which is less but still too strong.
- **House's row is the same as before its trait existed** (32, 40, 38, 46 against the four nations
  that did not change). That is structural, not a magnitude problem. The cross axis plays the five
  mono-unit `mass_*` probes on both sides, so a House army there never has a second domain. A
  mixed-army trait is therefore never active on that axis. No Synthesis magnitude can move House's cross
  number. The only things that move it are the other nations' traits.
- Personality part, 2 seeds, 120 matches, reported and not gated, 359 s. This is where Synthesis
  shows: House plays `synth_triad`. Before the trait House read 37.1% (F-066). Now it reads **52.1%**
  [35.2, 69.1]. The other nations read: Forgeborn 80.0%, Verdant 20.0%, Aetherkin 80.0%, Tollmarch 36.4% and Ashen Reach 31.4%. All are
  undetermined, with n ≈ 40 each. Personality is dominated by script-vs-script lopsidedness, so it is not a nation measure.

**Step 1: Tollmarch GatherTicks 0.90 → 0.95 (90 → 86 ticks, round half up), Forgeborn Foundry AlloyCost 0.90 → 0.95, Ashen Reach
BuildingHp 1.15 → 1.30.** Cross, 3 seeds, 900 matches, 94 timeouts, 5332 s: **Fail**.

| nation | strength | 95% interval | reading |
|---|---|---|---|
| house | 34.5% | [27.7, 41.6] | undetermined |
| forgeborn | 52.9% | [45.8, 59.9] | holds |
| verdant | 47.3% | [40.1, 54.4] | holds |
| aetherkin | 50.5% | [43.3, 57.7] | holds |
| tollmarch | 76.3% | [69.8, 82.3] | **Fail** |
| ashen_reach | 38.5% | [31.4, 45.9] | undetermined |

Matrix rows: house 40/40/38/9/45. Forgeborn 60/51/49/46/58. Verdant 60/49/48/24/55. Aetherkin
62/51/52/30/58. Tollmarch 91/54/76/70/91. Ashen Reach 55/42/45/42/9.

- Forgeborn moved into the band, from 59.6% to 52.9%.
- Ashen Reach gained only 1.5 points for +15% building HP. Building HP barely matters when two mass
  armies meet in the field.
- **Tollmarch did not come down.** Halving the trait (9 ticks saved down to 4) read 73.0% before and 76.3% now, which is
  within noise. The mass probes are economy mirrors: both sides run the same script, so any steady income edge
  decides the race the same way every time, and the size of the edge hardly matters. One tick, 1.1%, is the smallest step
  `GatherTicks` can take on a base of 90.

**Step 2: Tollmarch GatherTicks 0.95 → 0.98 (90 → 88 ticks).** Everything else is as in step 1. Cross, 3 seeds, 900
matches, 112 timeouts, 5389 s: **undetermined**. Every nation except House reads Pass.

| nation | strength | 95% interval | reading |
|---|---|---|---|
| house | 40.3% | [33.2, 47.6] | undetermined |
| forgeborn | 55.3% | [48.2, 62.3] | holds |
| verdant | 52.1% | [44.8, 59.3] | holds |
| aetherkin | 55.3% | [48.1, 62.5] | holds |
| tollmarch | 52.4% | [45.2, 59.6] | holds |
| ashen_reach | 44.7% | [37.4, 52.0] | holds |

Matrix rows: house 40/40/38/38/45. Forgeborn 60/51/49/58/58. Verdant 60/49/48/48/55. Aetherkin
62/51/52/54/58. Tollmarch 62/42/52/46/60. Ashen Reach 55/42/45/42/40.

- **Tollmarch is a cliff, not a slope.** At 86 gather ticks it reads 76.3%. At 88 it reads 52.4%. Two ticks
  separate "wins every mirror race" from "in the band". Its row against the other four nations went from
  91/54/76/70 to 62/42/52/46. 0.98 is the value shipped. A smaller perk (0.99, 89 ticks) is the only step left
  below it.
- **House is the one nation not passing.** Its interval [33.2, 47.6] crosses the 35% line. Its row is
  unchanged against the four nations whose traits did not move (40, 40, 38, 45). It lost to Tollmarch at 38 instead of 9, and that
  is the whole gain. As step 0 found, the cross probes never field a second domain, so House plays them
  traitless. No `nations.ron` magnitude of House's own can lift it. It would come into the band only if the other
  five nations' perks were made weaker until a traitless nation draws even with them. That is a different design call
  from "tune within the bounds", so it was not made.
- **Stopped here.** Cross does not read Pass, so the pentagon part was not run, as instructed. That part cannot read
  Pass in any case: F-066 shows sen>rip undetermined in the traitless House/House mirror, the F-049
  baseline. **AC4 stays unticked.** Two things are needed, and both are the user's decision:
  - a cross probe that lets a mixed-army trait act, or a cross-only perk for House;
  - a pentagon criterion that is not held hostage by the baseline sen>rip link.

## F-075 — C3 starts ahead of B5, by the user's decision (C3)

**Context.** `CAMPAIGN_PLAN.md` gates C1 onward on B5 Go, the human play test, which has not
run. The user overrode that gate for C1 on 2026-11-02 (F-052 onward) and for C2 on 2026-11-03
(F-062). On 2026-11-04 the user said "start c3".

**Decision.** C3 (the three-resource economy: Biomass grown, Aether channelled) starts now, on
the same terms as C1 and C2: B5 still runs when the user can play, and its findings can revise
C1, C2, C3 and later milestones. The C3 heading in `CAMPAIGN_PLAN.md` now says "started
2026-11-04, ahead of B5 by user decision", the plan's gate section records the override, and
C3's acceptance criteria are written down as checkboxes.

The three-resource economy is opt-in (`MatchSettings`, the client's pick), like nations and
battles. The default stays Alloy-only.

**What this does not change.** C1's open AC (the B5 human re-read) and C2's open AC (the nation
cross gate, F-074) stay open. Nothing in C3 re-pins a golden: the Alloy-only default must
reproduce today's results exactly (C3 golden licence). The Alloy-only stand-in traits keep
working in Alloy-only mode.

## F-076 — The three-resource economy is a mode a match opts into (C3 AC1, AC2)

**Context.** C3 adds Biomass (grown) and Aether (channelled) beside Alloy. The golden licence says
the default match must reproduce today's pinned hashes exactly, with no re-pin.

**Decision.**
- **A mode, not a content switch.** `EconomyMode { Alloy (default), ThreeResource }` is a field
  of `MatchSettings`, like `nations` and `battle`. The numbers live in an optional
  `three_resource` block in `resources.ron`. They are mixed into the content fingerprint only
  when the block is present, as C1's and C2's sections are.
- **The resource is the mode.** `three::install` inserts a `ThreeEconomy` resource, and every
  three-resource system (`grow`, `channel`) runs with `run_if(resource_exists::<ThreeEconomy>)`.
  It is installed before the nations, because a nation's traits depend on the mode (AC4).
- **The banks.** `Stockpiles` gains a Biomass and an Aether array per faction. The Alloy
  `STOCKPILES` state-hash row is untouched. The new banks, the economy's tick and its growth
  remainders are hashed under a new tag (26), and each well under another (27), only when
  `ThreeEconomy` exists. An Alloy-only match therefore emits exactly the rows it emitted
  before C3.
- **Wells spawn last.** Entity order is part of a match (F-011). The wells are spawned after
  everything else in setup (after a battle's layer too), and only in three-resource mode.
- **Biomass.** Every `growth.interval_ticks` (60), each standing source building grows its
  `per_interval` for its side: the HQ 1 (a floor), each Gene-Vats 2. Growth stops at the
  side's storage cap, the sum of its sources' `storage` (HQ 200, Vats 250). Growth is summed in
  per-mille, so a nation's growth factor (AC4) never rounds per building. Starting banks are
  Biomass 100 and Aether 0.
- **Aether.** Three ley-wells at RON sites in the frame of the two HQs: `along` the line from
  A's HQ to B's, and `side` units across it. A home well each at (0.2, -220) and (0.8, 220),
  and the contested middle at (0.5, 0). The layout is point-symmetric, so both orientations of
  one map get the same wells, and the same RON lays the wells out on the client's map
  (different HQ positions) too. "Held" means at least one of the side's units or buildings is
  within `hold_radius` (110) and no enemy unit or building is. A held well banks 2 Aether per
  60 ticks of holding. Progress pauses while the well is empty or contested, and restarts from
  zero when the holder changes.

**What this does not change.** Alloy-only play: no resource is inserted, no system runs, no
row is hashed, no entity is spawned. `c1_golden`, `c2_golden` and `critic_b35_ac0` pass
unchanged.

## F-077 — Each nation has two trait lists, one per economy (C3 AC4)

**Context.** C2's traits are Alloy-only stand-ins for the brief's domain leans (F-063 onward).
C3 must play the real leans in a three-resource match without changing a single Alloy-only
outcome.

**Decision.**
- **Two lists.** `NationDef.traits` stays the Alloy-only list. A new optional
  `three_resource_traits` is played **instead of** it in a three-resource match.
  `SideRules::build_in(content, nation, mode)` and `NationRules::resolve_in(.., mode)` pick the
  list. `build` and `resolve` are the Alloy-only ones, as before. `nation::install` reads the
  world's mode, so the economy is installed first.
- **Three new stats**, three-resource only, all `mul`:
  - `Cost`: a unit's brief `cost`, in its own resource.
  - `Growth`: a growth source's Biomass, kept as a per-building per-mille table and summed per
    side before rounding (F-076).
  - `AlloyYield` (>= 1, gatherers only): the Alloy a deposited load banks. The bonus is
    accumulated per-mille per side in `ThreeEconomy` and minted at the stockpile. It is
    hashed with the economy's other remainders, and a full stockpile refuses it. This is the
    one place Alloy is minted rather than moved, and only in a three-resource match.
- **A trait that cannot act is a load error.** The three new stats in `traits` are refused
  ("acts only in the three-resource economy"). `AlloyCost` on units in `three_resource_traits`
  is refused too ("Alloy-only": a unit's Alloy price is not charged there). `AlloyCost` on
  buildings is allowed in both, since buildings are priced in Alloy in both. The loader builds
  every nation in both modes.
- **The shipped leans** (`nations.ron`; magnitudes are subject to the AC8 gate):
  - House Vael: none (Synthesis plays in both).
  - Forgeborn: Foundry units `Cost` 0.95, buildings `BuildingHp` 1.15.
  - Verdant: buildings `Growth` 1.15, Gene-Vats units `TrainTicks` 0.90.
  - Aetherkin: Aether Spire units `Cost` 0.90, Arclight `AttackRange` +12.
  - Tollmarch: workers `AlloyYield` 1.10, `GatherTicks` 0.98.
  - Ashen Reach: buildings `BuildingHp` 1.30.
- **Fingerprint.** A nation's three-resource list is mixed in only when it is non-empty, so
  content without one fingerprints as before.

**What this does not change.** Alloy-only play resolves the same list it did in C2, so the
goldens, `c2_nations` and the nation gate inputs are unchanged.

## F-078 — The AI in the three-resource economy: per-bank budgets and sticky well detachments (C3 AC5)

**Context.** In a three-resource match the scripted AI must pay each unit in its own resource,
hold its home well and contest the middle one. Alloy-only AI traces must stay unchanged.

**Decision.**
- **Per-bank budgets.** A decision's budget is per bank (Alloy, Biomass, Aether). A unit is
  priced through `Defs::unit_price`: Alloy in an Alloy-only match, its own resource in a
  three-resource one. Buildings are always Alloy. The opening reservation (B4, F-041) is Alloy
  held for a barracks, so it holds back only Alloy-priced units. In Alloy-only every unit is
  Alloy-priced, so the commander decides number for number as before. The Gene-Vats come from
  the script's openings, as every barracks does.
- **Detachments from RON.** `three_resource.ai: (keepers: 1, guards: 2)`.
  - **Keepers.** Gatherers kept standing on the home well, which is the well nearest the HQ.
    They are out of mining and out of the `worker_target` count, so the economy replaces them.
  - **Guards.** Fighters (never a General) sent to the nearest well that is neither home nor
    the enemy's home: the middle. They go back to the home well while an enemy is on it, which
    means it is contested or held by the enemy. A keeper standing there turns an enemy's take
    into a contest. Guards are out of the attack waves. They move by attack-move, so they fight
    on the way.
  - A unit within `hold_radius / 2` of its well, or already ordered there, is not re-ordered.
- **Sticky, not "lowest entity".** In Bevy 0.19 `Entity::to_bits` does not order by spawn: the
  newest entity sorts first. Picking "the lowest-bits worker" every decision therefore relieved
  the keeper each time a worker was trained. That was 4 keepers in 2400 ticks, each walking
  out while the last walked back. The detachments are now commander state (`AiCommander.keepers`
  and `.guards`). A member stays until it dies or stops fitting, and vacancies are filled in
  the stable entity order. This is deterministic and not hashed; the AI is not replayed, its
  orders are.
- **Trace.** Two new `AiAction`s, `Keep { unit, well }` and `Guard { well, force }`. They are
  emitted only in a three-resource match.

**What this does not change.** Alloy-only: no wells, so there are no posts and every unit is
Alloy-priced. `b1_matchup`'s journal pins, `m4c_ai`, `critic_m4c`, `c1_ai`, the goldens and
`c2_nations` all pass unchanged.

## F-079 — A match log records its economy, and a replay applies it or refuses (C3 AC7)

**Context.** The golden licence: the Alloy-only default reproduces today's pinned results, with
no re-pin, and a match log carries its economy so a replay can play it back or refuse it.

**Decision.**
- **The log field is optional.** `MatchLog.economy: EconomyMode`, `#[serde(default)]`, and it is
  skipped when Alloy only. An Alloy-only log is byte for byte what it was before C3, and a
  pre-C3 log (no field) reads as Alloy only. The sim stamps it the way it stamps the nations
  (`stamp_content`): from whether `ThreeEconomy` exists on the first stamp. A later difference
  marks the log `content_changed`.
- **Content check.** `matches_content` refuses a three-resource log against content without
  a `three_resource` block, before the fingerprint check, with a message that names the
  economy. The fingerprint already covers the block when it is present (F-076).
- **The replay applies the economy at tick 0, before `identify`.** `replay::apply_log_economy`
  runs right after the gather-claim sweep, where every replay system sits (F-008, pinned by
  `critic_m5`), and before `identify` and `apply_log_nations`. The wells are entities and must
  exist when `identify` numbers the field on tick 0 (F-011). In an
  Alloy-set-up world at tick 0 it installs the economy (`three::install`), re-installs the
  nations so their three-resource lists apply, and spawns the wells framed by the two HQs
  (`three::hq_frame`). That is the same layout a headless three-resource match gets, since
  `headless` now frames its wells by `hq_frame` too, falling back to the bases when a side has
  no HQ.
- **Refusals.** A log of the other economy than the world plays is refused when the world is
  three-resource and the log is Alloy only, and also when the match is already under way
  (tick > 0). Nothing of a refused log is fed (cursor 0), and nothing is installed.

**Fixture fixes (the full debug suite found them; not pins).**
- `m5_replay` and `critic_b1_ac3` rename `gene_vats` in `units.ron`. Since AC1,
  `resources.ron`'s growth sources also name it, so both fixtures now rename that line too.
- `critic_m5`'s shared-id fixture renames the resource `aether` to `worker`, on the grounds
  that nothing resolves `aether`. Since AC3, Arclight's `cost` resolves it (load-checked when
  the `three_resource` block is present), so the fixture renames that reference too. The
  collision is now tested at a reference site as well.

**Golden status.** No pin moved: c1_golden, c2_golden, critic_b35_ac0, b1_matchup's journal pins,
m4c, c1_ai, c2_nations, m5_replay, critic_m6, p2_log_writer and b4_attack_move all pass unchanged.
`tests/c3_golden.rs` adds that an Alloy-only match plays identically with the three-resource
content and without it (state hash per tick, AI journal and outcome, skirmish and nations).

## F-080 — The client picks the economy, shows the three banks and draws the wells (C3 AC6)

**Decision. Kept thin.**
- **The pick.** `NationPick.economy` defaults to Alloy only. `E` (`ECONOMY_KEY`) toggles it on
  the pick screen. The screen shows an `[E] Economy: ...` line only when the content has a
  `three_resource` block, and without one the toggle stays on Alloy only. Like the nations,
  it is client state until Enter.
- **The start.** `start_match` hands the choice to the sim's own setup functions, in the order
  a headless match uses: `three::install`, then `nation::install` (so the nations get their
  three-resource lists), then `three::spawn_wells` framed by `three::hq_frame`, last. All of
  it runs before the first tick, and the tick gate is still shut. No order and no input reach
  the sim.
- **The panel.** The bank line reads `Alloy: n` in an Alloy-only match, as before, and
  `Alloy: a   Biomass: b   Aether: c` in a three-resource one. Training prices come from
  `Defs::unit_price`, so they are what the player's nation pays and in which bank. Alloy only
  shows the bare number as before (`(90)`); three-resource shows it with its bank (`(40 Biomass)`).
- **The wells.** `ui::show_wells` (Update) gives each well a sprite the first time it sees it.
  After that it sets the sprite's colour from the sim's `WellState` through `client::well_color`:
  - empty: violet;
  - held: the holder's nation colour, or its side colour (you blue, the opponent red) when the
    side has no nation;
  - contested: white.

  `ui::draw_well_rings` draws the `hold_radius` in the same colour (gizmos). Both read the sim
  and write nothing back.

**Tests.** `tests/c3_client.rs`, headless under `MinimalPlugins` through the shipped `setup`,
pick systems and sim chain:
- the toggle, and content without the block;
- an Alloy start is today's match (no economy, no wells, today's bank line);
- a three-resource start installs the banks, the nations' three-resource tables and one
  well per site at its framed position;
- the panel's three banks and a Ripper priced in Biomass;
- every well's sprite colour follows its state, and the palette is four distinct colours;
- a well turns A's colour when an A unit stands on it, and contested when a B unit joins.

**Human checks on the Mac (`cargo run`). The box cannot show a window.**
1. The pick screen shows `[E] Economy: Alloy only`, and E flips it to `Three resources` and back.
2. With Alloy only, the match looks and plays as before: no wells, and the panel shows only `Alloy:`.
3. With three resources, three violet well squares sit along and across the line between the
   HQs, each with a ring of its hold radius.
4. Walk a unit onto a well: the square and ring take your nation's colour, and Aether climbs
   in the panel. The opponent's keeper turns its home well its colour. When both sides stand on
   one, it turns white.
5. The bank line `Alloy / Biomass / Aether` fits in the panel. Biomass grows on its own and
   stops at the cap.
6. Select a Gene-Vats or an Aether Spire: the train line prices Rippers in Biomass and Arclights
   in Aether, and the colours tell the sides and the empty and contested states apart.

## F-081 — The B3 and C2 gates re-read on the three-resource economy (C3 AC8)

**Decision.** `BatchSettings.economy` (default `EconomyMode::Alloy`, so every batch before C3 is
unchanged) is handed to every match of `run_batch` and `run_fixtures`. The gates are then read
on `EconomyMode::ThreeResource` by the `--ignored` measurements in `tests/c3_gate.rs`. These are
readings, not pins. No RON was tuned in this step. Run on the cloud container (4 cores), release.

**Tests.** `tests/c3_gate.rs`: a batch is Alloy only unless it names the economy, and each record
is the match its settings name, played for real in either economy (and the two economies differ,
because Rippers cost Biomass). A nation batch plays the economy it names. Both pass.

**B3 kill gate on three resources** (the five mass probes, 2 seeds, 100 matches, 13 timeouts, 1443 s).

| | value | 95% interval | status |
|---|---|---|---|
| K1 mass_bulwark | 50.0% | [27.3, 72.7] | undetermined |
| K1 mass_sentinel | 75.0% | [53.8, 93.4] | undetermined |
| K1 mass_ripper | 25.0% | [5.2, 47.7] | pass |
| K1 mass_ravager | 75.0% | [53.8, 93.4] | undetermined |
| K1 mass_arclight | 25.0% | [6.6, 46.2] | pass |
| K2 slot A | 54.5% (n 11) | [24.6, 81.5] | undetermined |
| K2 left base | 27.3% | [8.2, 61.1] | undetermined |
| K3 decided median | 28363 ticks (7:53) | | |
| K3 timeouts | **13%** | | **FAIL** |
| K3 band share (5–8 min) | 43.7% | [32.1, 56.0] | undetermined |
| K3 below / beyond band | 8% / 54% | | reported |

Row vs column (win %, timeouts, mean of the probe's own unit trained A v B):
bulwark 75 t0 28v25 | 0 16v20 | 100 t2 27v99 | 100 20v29 | 0 9v13.
sentinel 100 21v18 | -- t4 32v32 | 100 12v21 | 0 12v14 | 100 15v13.
ripper 0 t2 87v24 | 0 21v12 | 33 t1 72v72 | 0 39v20 | 100 17v7.
ravager 0 32v21 | 100 15v12 | 100 19v37 | -- t4 51v51 | 100 21v13.
arclight 100 14v9 | 0 12v15 | 0 7v17 | 0 14v21 | 50 18v17.

- **No probe is dominant.** K1 is undetermined only because 2 seeds give 20 matches per probe. The
  intervals are wide and none sits above the 65% line.
- **Matches are much longer.** The three-resource economy moves the median into the 5–8 minute band
  (54% now run beyond it). This is the direction the skirmish K3 wanted. The cost is timeouts: 13%,
  and they are concentrated in the mirrors (sentinel/sentinel 4 of 4, ravager/ravager 4 of 4) and in
  Ripper cells. Ripper armies are slow to grow on passive Biomass: the ripper mirror trains 72 a side
  and still times out in 1 of 3.
- **Ripper falls from the Alloy-only top to 25%.** Ripper is now paid in storage-capped Biomass,
  so its mass is throttled. Arclight is paid in Aether from contested wells and reads 25% as well.
  Sentinel and Ravager lead at 75%. Ravager is also Biomass-paid, so the Biomass price alone
  does not explain Ripper's fall; it loses its old prey cells (0% v bulwark, sentinel, ravager).
- **Status: not yet a pass.** The K3 timeout rate is the one hard Fail. More seeds would settle
  K1 and K2. The tuning levers are RON (Biomass `per_interval` and storage, well `per_interval`),
  and no base combat stat needs to move.

**C2 nation cross gate on three resources** (every ordered pair of nations, the five mass probes,
**1 seed** to fit the cloud session, 300 matches, **104 timeouts**, 5299 s). Strength: **undetermined**.

| nation | strength | 95% interval | reading |
|---|---|---|---|
| house | 30.2% | [15.9, 45.6] | undetermined |
| forgeborn | 50.1% | [34.7, 65.5] | undetermined |
| verdant | 64.7% | [51.4, 77.3] | undetermined |
| aetherkin | 52.0% | [36.5, 67.5] | undetermined |
| tollmarch | 69.7% | [56.1, 82.3] | undetermined |
| ashen_reach | 33.4% | [18.6, 49.0] | undetermined |

Row vs column win %: house 30/25/30/21/44. Forgeborn 70/38/50/23/70. Verdant 75/62/62/50/73.
Aetherkin 70/50/38/36/67. Tollmarch 79/77/50/64/79. Ashen Reach 56/30/27/33/21.

- **Nothing reads Fail, nothing reads Pass.** One seed leaves every interval about 30 points wide.
  Three seeds (as F-074) would be needed to settle it.
- **Tollmarch is again the strongest** (69.7%). Its three-resource trait is richer Alloy mines plus
  faster gathering, and in the mono-probe mirrors a steady income edge decides the race, as in
  F-074. **Verdant** (Biomass grows faster) is next at 64.7%. House (30.2%) and Ashen Reach (33.4%)
  are low, for the same reasons F-074 gives: Synthesis never acts on the mono-probe cross axis, and
  building HP barely matters when two mass armies meet in the field.
- **A third of matches time out** (104 of 300), against 69–112 of 900 under Alloy only (F-074). This
  matches the B3 K3 reading above: the three-resource economy is slower, and any match-length or nation
  verdict is noisy until it is tuned.

**AC8 status.** The AC asks for the gates to be re-run on the three-resource economy and recorded. That
is done, so the AC is ticked. **Neither gate reads Pass.** The next step is the user's call. The candidates are:
- more seeds;
- RON tuning of the economy numbers (Biomass `per_interval`/storage, well `per_interval`) to bring the
  timeouts down;
- then Tollmarch's and Verdant's three-resource trait sizes, each step recorded as F-074 did.

Base unit combat stats are not to be retuned.

## F-082 — Three-resource tuning step 1: Biomass ×2 tried and reverted (C3 AC8)

**Probe.** A scratch harness (not shipped) played the three probes whose cells timed out most in
F-081: `mass_sentinel`, `mass_ripper` and `mass_ravager`, against each other. 1 seed, 18 matches
per variant, tick cap 54000 (15:00).

| variant | timeouts | decided median |
|---|---|---|
| as shipped (HQ 1/200, Vats 2/250, guards 2) | 5 | 24065 |
| **Biomass ×2 (HQ 2/400, Vats 4/500)** | **3** | 19719 |
| guards 0 | 1 | 31191 |
| Biomass ×2 and guards 0 | 1 | 29043 |

- Doubling Biomass stops the Ravager mirror timing out (2 of 2 → 0 of 2). It speeds up decided
  matches.
- **Almost every timeout left is the Sentinel mirror.** Sentinels are Alloy-paid, and the Sentinel
  mirror times out in **2 of 4** even under Alloy only (seeds 0–1). So that stall is the
  mirror itself, made worse by the well detachments. It is not something a Biomass number moves.
- Dropping the guards removes it, but then the AI never contests the middle well. That is AI
  behaviour (C3 AC5), not economy, so it is not shipped here.

**Tried:** `resources.ron` `three_resource.growth` set to HQ `per_interval` 1 → 2, `storage` 200 → 400,
and Gene-Vats `per_interval` 2 → 4, `storage` 250 → 500.

**Full B3 re-read on Biomass ×2** (5 mass probes, 2 seeds, 100 matches, 1136 s): **worse. Reverted.**

| | as shipped (F-081) | Biomass ×2 |
|---|---|---|
| timeouts | 13% FAIL | **14% FAIL** |
| K1 | undetermined | **FAIL: mass_ravager 100.0% [81.1, 100.0]** |
| K1 others | bulwark 50, sentinel 75, ripper 25, arclight 25 | bulwark 25, sentinel 75, ripper 25, arclight 25 |
| K2 | undetermined | undetermined (slot A 53.3%, n 15) |
| K3 median / band share | 28363 / 43.7% | 20639 / 34.9% FAIL (25% below, 45% beyond) |

- The timeouts did not fall. They moved:
  - the Ravager mirror now ends (0 of 4 time out);
  - Bulwark v Ravager now stalls (3 of 4: a Ravager army of about 94 cannot crack a Bulwark wall);
  - the Sentinel mirror still times out 4 of 4.
- Doubled Biomass makes Ravager, the Biomass-paid heavy, dominant. The 18-match probe missed this
  because it left Bulwark and Arclight out. A probe that leaves out a predator cannot judge a K1 move.
- One Biomass-cost probe step on its own is not a lever on the timeouts. Ripper and Ravager are both
  Biomass-paid, so Biomass speed shifts the pentagon before it shortens matches.
- Also tried, on the 3-probe harness on top of Biomass ×2: AI `guards` 2 → 1. Result: 4 timeouts of 18, no better.

**Shipped: nothing.** The RON is back to F-081's values. The finding for the user:
- **The dominant timeout is the Sentinel mirror.** It stalls 2 of 4 even under Alloy only, and 4 of 4
  with the well detachments.
- Only AI `guards: 0` removed it (1 timeout of 18). That changes C3 AC5's behaviour (the AI contests the
  middle well), so it is the user's call, not an economy tune.
