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
