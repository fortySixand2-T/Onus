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
