//! Critic probes for **B2 AC4** — "record per match: winner, length in ticks,
//! units produced per side".
//!
//! Written against the spec, not against the implementation. Three groups:
//!
//!   1. **the recomputed goldens** — this AC moved three pinned journal
//!      digests in `tests/b1_matchup.rs` and rewrote the digest function in a
//!      prior critic's file. The claim is that installing one more sim
//!      resource shifts every entity id afterwards without changing a single
//!      decision. These probes test that claim as a *perturbation control*
//!      (an unrelated extra resource must behave the same way), re-pin the
//!      state hashes independently of `b1_matchup.rs`, and check the
//!      relabelled digest still has teeth;
//!   2. **`Produced` correctness** — one spawn, one count, right slot, right
//!      type, under simultaneity, resize and death; plus a conservation
//!      oracle (`alive == starting + produced - lost`) over a real match;
//!   3. **self-labelling and B3 reachability** — a record read on its own, and
//!      a per-unit win rate computed from records alone.

use std::path::PathBuf;

use bevy::prelude::*;

use onus::batch::{self, BatchSettings, MatchResult, ProductionCounts};
use onus::headless::{self, MatchSettings, Orientation};
use onus::sim::combat::{Casualties, Health};
use onus::sim::content::Content;
use onus::sim::economy::{
    enqueue_unit, Building, Produced, ProductionQueue, Stockpiles, UnitDefIdx,
};
use onus::sim::spatial::Faction;
use onus::sim::{
    AiAction, AiCommanders, AiJournal, CommandQueue, MatchState, Position, RateReport, ResourceNode,
};

// ---- harness ---------------------------------------------------------------

fn content() -> Content {
    Content::load_from_dir(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data"))
        .expect("assets/data/*.ron parse into sim structs")
}

fn step(app: &mut App) {
    let dt = app.world().resource::<Time<Fixed>>().timestep();
    app.world_mut().resource_mut::<Time<Fixed>>().advance_by(dt);
    app.update();
}

fn tick(app: &mut App, n: u32) {
    for _ in 0..n {
        step(app);
    }
}

/// A resource the sim knows nothing about — the perturbation. In Bevy a
/// resource is an entity, so installing one before anything is spawned shifts
/// every entity id the world hands out afterwards, and nothing else.
#[derive(Resource, Default)]
struct UnrelatedBallast(#[allow(dead_code)] u64);

/// The b1_matchup golden fixture, rebuilt here so this file's pins do not
/// depend on that file. `ballast` installs [`UnrelatedBallast`] *before* the
/// fixture spawns: the same kind of perturbation `Produced` was.
fn golden_fixture(c: Content, commanders: AiCommanders, ballast: bool) -> App {
    let alloy = c.economy.starting_alloy;
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .insert_resource(Time::<Fixed>::from_hz(60.0))
        .insert_resource(c)
        .init_resource::<CommandQueue>()
        .init_resource::<RateReport>()
        .init_resource::<Casualties>()
        .insert_resource(Stockpiles::starting(alloy));
    if ballast {
        app.init_resource::<UnrelatedBallast>();
    }
    onus::add_sim_systems(&mut app, Update);
    for (faction, base) in [
        (Faction::A, Vec2::new(-750.0, 0.0)),
        (Faction::B, Vec2::new(750.0, 0.0)),
    ] {
        let def = app.world().resource::<Content>().building_index("hq").unwrap();
        app.world_mut().spawn((
            Position(base),
            Building { def },
            faction,
            ProductionQueue::default(),
        ));
        app.world_mut().spawn((
            Position(base + Vec2::new(0.0, 250.0)),
            ResourceNode { amount: 100_000 },
        ));
        for i in 0..3 {
            let (idx, kind, hp) = {
                let c = app.world().resource::<Content>();
                let idx = c.unit_index("worker").unwrap();
                (idx, c.units[idx].mvp_kind, Health::from_def(c, idx))
            };
            app.world_mut().spawn((
                Position(base + Vec2::new(0.0, 20.0 * i as f32)),
                UnitDefIdx(idx),
                kind,
                faction,
                hp,
            ));
        }
    }
    app.insert_resource(commanders);
    app
}

/// The **old** digest: raw `Entity` bits, exactly what the pre-AC4 goldens
/// hashed. Kept here as the control, not as a pin.
fn raw_bits_digest(app: &App) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for (t, f, a) in &app.world().resource::<AiJournal>().0 {
        for b in format!("{t}|{f:?}|{a:?}").as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100_0000_01b3);
        }
    }
    h
}

/// The **new** digest: entities relabelled by first appearance.
fn relabelled_digest(app: &App) -> u64 {
    let mut seen: Vec<u64> = Vec::new();
    let mut label = |e: Entity| -> usize {
        let bits = e.to_bits();
        match seen.iter().position(|b| *b == bits) {
            Some(i) => i,
            None => {
                seen.push(bits);
                seen.len() - 1
            }
        }
    };
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for (t, f, a) in &app.world().resource::<AiJournal>().0 {
        let action = match *a {
            AiAction::Gather { unit, node } => {
                format!("Gather{{unit:{},node:{}}}", label(unit), label(node))
            }
            AiAction::TrainWorker { at } => format!("TrainWorker{{at:{}}}", label(at)),
            other => format!("{other:?}"),
        };
        for b in format!("{t}|{f:?}|{action}").as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100_0000_01b3);
        }
    }
    h
}

// ---- 1. the recomputed goldens ---------------------------------------------

/// The perturbation control. An extra resource the sim never reads shifts the
/// entity ids the fixture is built from — and must move **nothing else**:
/// the per-tick state hash is identical and the relabelled journal digest is
/// identical, while the digest that hashed raw entity bits moves. That is the
/// whole account under which the three journal goldens were recomputed; if
/// the raw digest did *not* move here, the goldens moved for another reason.
#[test]
fn probe_an_unrelated_resource_shifts_entity_ids_and_nothing_else() {
    for seed in [4u64, 11, 23] {
        let mut plain = golden_fixture(
            content(),
            AiCommanders::new(seed, &[Faction::A, Faction::B]),
            false,
        );
        let mut shifted = golden_fixture(
            content(),
            AiCommanders::new(seed, &[Faction::A, Faction::B]),
            true,
        );
        // The perturbation really does shift the ids the fixture gets.
        let ids = |app: &mut App| -> Vec<u64> {
            let mut q = app.world_mut().query_filtered::<Entity, With<Position>>();
            let mut v: Vec<u64> = q.iter(app.world()).map(|e| e.to_bits()).collect();
            v.sort_unstable();
            v
        };
        assert_ne!(
            ids(&mut plain),
            ids(&mut shifted),
            "seed {seed}: installing a resource did not move any entity id — then the \
             recomputed journal goldens have some other cause"
        );

        tick(&mut plain, 3_000);
        tick(&mut shifted, 3_000);

        assert_eq!(
            onus::sim::state_hash(plain.world_mut()),
            onus::sim::state_hash(shifted.world_mut()),
            "seed {seed}: an extra resource changed sim truth"
        );
        assert_eq!(
            relabelled_digest(&plain),
            relabelled_digest(&shifted),
            "seed {seed}: the relabelled digest is not invariant to the shift it was \
             rewritten to be invariant to"
        );
        assert_ne!(
            raw_bits_digest(&plain),
            raw_bits_digest(&shifted),
            "seed {seed}: the raw-entity-bits digest did NOT move under the shift, so \
             that is not why the goldens were recomputed"
        );
    }
}

/// The state goldens are sim truth and must be byte-identical to the pre-AC2
/// values, in a file the implementer does not own. Pinned here independently
/// of `tests/b1_matchup.rs`.
#[test]
fn probe_the_state_hash_goldens_are_byte_identical_to_the_pre_ac2_values() {
    for (seed, state_golden) in [
        (4u64, 0xa71f_64ca_d502_03e9u64),
        (11, 0x5b39_8ee4_7854_23dc),
        (23, 0xf4b5_7d1c_3c3f_2af7),
    ] {
        let mut app = golden_fixture(
            content(),
            AiCommanders::new(seed, &[Faction::A, Faction::B]),
            false,
        );
        tick(&mut app, 3_000);
        assert_eq!(
            onus::sim::state_hash(app.world_mut()),
            state_golden,
            "seed {seed}: the default match's state moved"
        );
    }
}

/// A digest made invariant to entity identity could have been made invariant
/// to things that matter. It must still separate: a different seed, a
/// different strategy on one side, the sides swapped, and a trace one action
/// shorter.
#[test]
fn probe_the_relabelled_digest_still_detects_a_real_behavioural_change() {
    let play = |seed: u64, a: &str, b: &str, ticks: u32| -> u64 {
        let c = content();
        let pair = AiCommanders::matchup(&c, seed, &[(Faction::A, a), (Faction::B, b)])
            .expect("shipped strategy names");
        let mut app = golden_fixture(c, pair, false);
        tick(&mut app, ticks);
        relabelled_digest(&app)
    };
    let base = play(4, "mass_ripper", "turtle", 1_800);
    assert_ne!(base, play(11, "mass_ripper", "turtle", 1_800), "a different seed");
    assert_ne!(base, play(4, "mass_bulwark", "turtle", 1_800), "a different A strategy");
    assert_ne!(base, play(4, "mass_ripper", "rush", 1_800), "a different B strategy");
    assert_ne!(base, play(4, "turtle", "mass_ripper", 1_800), "the sides swapped");
    // A shorter trace, chosen so the two traces really do differ in length.
    let trace_len = |ticks: u32| -> usize {
        let c = content();
        let pair =
            AiCommanders::matchup(&c, 4, &[(Faction::A, "mass_ripper"), (Faction::B, "turtle")])
                .expect("shipped");
        let mut app = golden_fixture(c, pair, false);
        tick(&mut app, ticks);
        app.world().resource::<AiJournal>().0.len()
    };
    assert!(trace_len(600) < trace_len(1_800), "the fixture keeps deciding things");
    assert_ne!(base, play(4, "mass_ripper", "turtle", 600), "a shorter trace");

    // And a single altered decision is caught: drop one journal entry.
    let c = content();
    let pair = AiCommanders::matchup(&c, 4, &[(Faction::A, "mass_ripper"), (Faction::B, "turtle")])
        .expect("shipped");
    let mut app = golden_fixture(c, pair, false);
    tick(&mut app, 1_800);
    let whole = relabelled_digest(&app);
    assert_eq!(whole, base, "the same match twice is the same digest");
    let n = app.world().resource::<AiJournal>().0.len();
    assert!(n > 2, "the fixture took some decisions");
    app.world_mut().resource_mut::<AiJournal>().0.remove(n / 2);
    assert_ne!(
        relabelled_digest(&app),
        whole,
        "removing a decision from the trace did not change the digest"
    );
}

// ---- 2. `Produced` correctness ---------------------------------------------

/// A headless app with the shipped sim chain, for hand-driven orders.
fn sim_app(c: Content, stock: Stockpiles) -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .insert_resource(Time::<Fixed>::from_hz(60.0))
        .insert_resource(c)
        .init_resource::<CommandQueue>()
        .init_resource::<RateReport>()
        .insert_resource(stock);
    onus::add_sim_systems(&mut app, Update);
    app
}

fn spawn_building(app: &mut App, faction: Faction, id: &str, at: Vec2) -> Entity {
    let def = app
        .world()
        .resource::<Content>()
        .building_index(id)
        .unwrap_or_else(|| panic!("content has a `{id}` building"));
    app.world_mut()
        .spawn((
            Position(at),
            Building { def },
            faction,
            ProductionQueue::default(),
        ))
        .id()
}

fn order(app: &mut App, base: Entity, building: &str, faction: Faction, unit: usize) -> bool {
    let c = app.world().resource::<Content>().clone();
    let def = c.building_index(building).expect("a known building");
    let mut stock = *app.world().resource::<Stockpiles>();
    let ok = {
        let mut e = app.world_mut().entity_mut(base);
        let mut q = e.get_mut::<ProductionQueue>().expect("a queue");
        enqueue_unit(&c, &mut stock, def, faction, &mut q, unit)
    };
    if ok {
        *app.world_mut().resource_mut::<Stockpiles>() = stock;
    }
    ok
}

/// Two buildings of the same faction finishing on the same tick are two
/// counts, in the right two slots — not one, and not four.
#[test]
fn probe_two_buildings_finishing_on_one_tick_count_once_each() {
    let c = content();
    let worker = c.unit_index("worker").expect("worker");
    let bulwark = c.unit_index("bulwark").expect("bulwark");
    let mut app = sim_app(c.clone(), Stockpiles::starting(100_000));
    let hq = spawn_building(&mut app, Faction::A, "hq", Vec2::new(-500.0, 0.0));
    let foundry = spawn_building(&mut app, Faction::A, "foundry", Vec2::new(-400.0, 0.0));
    assert!(order(&mut app, hq, "hq", Faction::A, worker));
    // Start the slower one early enough that both land on the same tick.
    let (wt, bt) = (c.units[worker].mvp_train_ticks, c.units[bulwark].mvp_train_ticks);
    assert!(order(&mut app, foundry, "foundry", Faction::A, bulwark));
    let before = wt.min(bt);
    tick(&mut app, before);
    // Run to well past both, then check the totals exactly.
    tick(&mut app, wt.max(bt) + 5);
    let p = app.world().resource::<Produced>().clone();
    assert_eq!(p.count(Faction::A, worker), 1, "one worker, counted once");
    assert_eq!(p.count(Faction::A, bulwark), 1, "one bulwark, counted once");
    assert_eq!(p.total(Faction::A), 2);
    assert_eq!(p.total(Faction::B), 0, "the other side built nothing");
    // And the world agrees.
    let mut q = app.world_mut().query::<(&UnitDefIdx, &Faction)>();
    let alive: Vec<usize> = q
        .iter(app.world())
        .filter(|(_, f)| **f == Faction::A)
        .map(|(d, _)| d.0)
        .collect();
    assert_eq!(alive.len(), 2, "two units on the map, two counted");
}

/// Both factions finishing on the same tick land in their own columns.
#[test]
fn probe_both_factions_finishing_on_one_tick_are_not_cross_credited() {
    let c = content();
    let worker = c.unit_index("worker").expect("worker");
    let mut app = sim_app(c.clone(), Stockpiles::starting(100_000));
    let a = spawn_building(&mut app, Faction::A, "hq", Vec2::new(-500.0, 0.0));
    let b = spawn_building(&mut app, Faction::B, "hq", Vec2::new(500.0, 0.0));
    assert!(order(&mut app, a, "hq", Faction::A, worker));
    assert!(order(&mut app, b, "hq", Faction::B, worker));
    tick(&mut app, c.units[worker].mvp_train_ticks + 1);
    let p = app.world().resource::<Produced>().clone();
    assert_eq!(p.count(Faction::A, worker), 1, "A built exactly its own");
    assert_eq!(p.count(Faction::B, worker), 1, "B built exactly its own");
    assert_eq!(p.grand_total(), 2);
}

/// A queue with two items of the same type counts twice, once per spawn — and
/// never before the second one lands.
#[test]
fn probe_a_two_deep_queue_counts_once_per_spawn() {
    let c = content();
    let worker = c.unit_index("worker").expect("worker");
    let t = c.units[worker].mvp_train_ticks;
    let mut app = sim_app(c.clone(), Stockpiles::starting(100_000));
    let hq = spawn_building(&mut app, Faction::A, "hq", Vec2::new(0.0, 0.0));
    assert!(order(&mut app, hq, "hq", Faction::A, worker));
    assert!(order(&mut app, hq, "hq", Faction::A, worker));
    tick(&mut app, t + 1);
    assert_eq!(
        app.world().resource::<Produced>().count(Faction::A, worker),
        1,
        "the second item is still training"
    );
    tick(&mut app, t + 1);
    assert_eq!(
        app.world().resource::<Produced>().count(Faction::A, worker),
        2,
        "both items spawned, both counted"
    );
    tick(&mut app, 200);
    assert_eq!(app.world().resource::<Produced>().total(Faction::A), 2, "no drift");
}

/// Building only a **late** unit in the roster must not disturb the earlier
/// columns: the counter is indexed by unit-def index, and a sparse column is
/// still read correctly (and off the end reads 0, never a panic).
#[test]
fn probe_a_late_roster_unit_does_not_disturb_earlier_columns() {
    let c = content();
    let last = c.units.len() - 1;
    let id = c.units[last].id.clone();
    let barracks = c.units[last].barracks.clone().expect("the last unit has a barracks");
    let mut app = sim_app(c.clone(), Stockpiles::starting(100_000));
    let b = spawn_building(&mut app, Faction::B, &barracks, Vec2::new(0.0, 0.0));
    assert!(order(&mut app, b, &barracks, Faction::B, last), "{id} is orderable");
    tick(&mut app, c.units[last].mvp_train_ticks + 1);
    let p = app.world().resource::<Produced>().clone();
    assert_eq!(p.count(Faction::B, last), 1);
    for u in 0..last {
        assert_eq!(p.count(Faction::B, u), 0, "unit {u} was never built");
    }
    assert_eq!(p.total(Faction::B), 1, "exactly one unit, of one type");
    assert_eq!(p.count(Faction::B, last + 7), 0, "off the roster reads zero");
    assert_eq!(p.count(Faction::A, last), 0, "the other side built nothing");
    // Snapshotted into a record's block, the column is still at the right index.
    let counts = ProductionCounts::of(&c, app.world().resource::<Produced>());
    assert_eq!(counts.counts(Faction::B).len(), c.units.len(), "padded to the roster");
    assert_eq!(counts.get(Faction::B, &id), 1);
    assert_eq!(counts.counts(Faction::B)[last], 1);
}

/// Conservation over a real, decided match: every unit alive at the end is a
/// starting worker or a production, minus what died. This is the differential
/// oracle for "one spawn, one count" — it cannot pass if production
/// over-counts or under-counts anywhere in a full match.
#[test]
fn probe_alive_equals_starting_plus_produced_minus_lost() {
    let c = content();
    let settings = MatchSettings::default()
        .with_strategies("rush", "mass_ripper")
        .with_tick_cap(6_000);
    let mut app = headless::ai_vs_ai(c.clone(), &settings).expect("shipped names");
    let mut ticks = 0;
    while ticks < 6_000 && app.world().resource::<MatchState>().outcome().is_none() {
        headless::step(&mut app);
        ticks += 1;
    }
    let produced = app.world().resource::<Produced>().clone();
    let lost = *app.world().resource::<Casualties>();
    assert!(produced.grand_total() > 0, "the match built nothing to check");
    assert!(lost.total() > 0, "the match killed nothing to check");

    let mut q = app.world_mut().query::<(&UnitDefIdx, &Faction)>();
    let mut alive = [0u32; 2];
    for (_, f) in q.iter(app.world()) {
        alive[usize::from(*f == Faction::B)] += 1;
    }
    for (slot, f) in headless::SIDES.iter().enumerate() {
        assert_eq!(
            alive[slot] as i64,
            3 + produced.total(*f) as i64 - lost.lost(*f) as i64,
            "{f:?}: alive {} != 3 starting + produced {} - lost {}",
            alive[slot],
            produced.total(*f),
            lost.lost(*f)
        );
    }
}

/// A real mutual loss — both HQs destroyed on the same tick — is a draw, and
/// the production it built is still there to report.
#[test]
fn probe_a_real_mutual_loss_keeps_its_production() {
    let c = content();
    let settings = MatchSettings::default()
        .with_strategies("rush", "rush")
        .with_tick_cap(3_000);
    let mut app = headless::ai_vs_ai(c.clone(), &settings).expect("shipped");
    headless::tick(&mut app, 1_800);
    let built = app.world().resource::<Produced>().clone();
    assert!(built.grand_total() > 0, "the opening built something");

    // Raze both HQs on the same tick: remove every victory building at once,
    // which is exactly "both sides lost their HQ on the same tick".
    let hqs: Vec<Entity> = {
        let victory: Vec<usize> = {
            let c = app.world().resource::<Content>();
            c.buildings
                .iter()
                .enumerate()
                .filter(|(_, b)| b.victory)
                .map(|(i, _)| i)
                .collect()
        };
        let mut q = app.world_mut().query::<(Entity, &Building)>();
        q.iter(app.world())
            .filter(|(_, b)| victory.contains(&b.def))
            .map(|(e, _)| e)
            .collect()
    };
    assert_eq!(hqs.len(), 2, "the fixture has one HQ a side");
    for e in hqs {
        app.world_mut().entity_mut(e).despawn();
    }
    headless::tick(&mut app, 5);
    let outcome = app
        .world()
        .resource::<MatchState>()
        .outcome()
        .expect("losing every building decides the match");
    assert_eq!(outcome.winner, None, "both sides lost: a draw, not a win");
    assert_eq!(
        *app.world().resource::<Produced>(),
        built,
        "a mutual loss did not erase or change what was built"
    );
    let counts = ProductionCounts::of(&c, app.world().resource::<Produced>());
    assert_eq!(counts.unit_ids().len(), c.units.len());
    assert_eq!(counts.total(Faction::A) + counts.total(Faction::B), built.grand_total());
}

/// Turning per-tick hashing on is observation, not play: it must not change a
/// single production count.
#[test]
fn probe_production_is_independent_of_hashing() {
    let c = content();
    let base = MatchSettings::default()
        .with_strategies("mass_arclight", "mass_sentinel")
        .with_tick_cap(1_500);
    let off = batch::run_match(&c, &base.clone().with_hashing(false)).expect("shipped");
    let on = batch::run_match(&c, &base.with_hashing(true)).expect("shipped");
    assert_eq!(off.produced, on.produced, "hashing changed what was built");
    assert_eq!(off.ticks, on.ticks);
    assert!(off.produced.total(Faction::A) + off.produced.total(Faction::B) > 0);
}

// ---- 3. self-labelling and B3 reachability ---------------------------------

/// A `Default` block is *unlabelled and empty*, never a wrong count under a
/// right name: every real unit id reads 0, and both totals are 0.
#[test]
fn probe_an_unlabelled_block_never_reports_a_wrong_count() {
    let c = content();
    let empty = ProductionCounts::default();
    assert!(empty.unit_ids().is_empty(), "an unplayed row names no roster");
    for f in headless::SIDES {
        assert_eq!(empty.total(f), 0);
        assert!(empty.counts(f).is_empty());
        for u in &c.units {
            assert_eq!(empty.get(f, &u.id), 0, "{} on an empty block", u.id);
        }
        assert_eq!(empty.count(f, 0), 0);
        assert_eq!(empty.count(f, 99), 0);
    }
    assert_eq!(empty.by_unit(Faction::A).count(), 0);
}

/// A record read *on its own*, with no memory of content order: the ids it
/// names and the counts it gives must agree with the roster it was played on,
/// and a lookup by id must never silently pick a neighbouring column.
#[test]
fn probe_a_record_is_interpretable_without_the_content() {
    let c = content();
    let r = batch::run_match(
        &c,
        &MatchSettings::default()
            .with_strategies("mass_ripper", "mass_bulwark")
            .with_tick_cap(2_400),
    )
    .expect("shipped");
    // Reading the row by name only.
    let by_name: u32 = r.produced.unit_ids().iter().map(|id| r.produced.get(Faction::A, id)).sum();
    assert_eq!(by_name, r.produced.total(Faction::A), "the named columns are the row");
    let pairs: Vec<(&str, u32)> = r.produced.by_unit(Faction::A).collect();
    assert_eq!(pairs.len(), r.produced.unit_ids().len());
    assert_eq!(pairs.iter().map(|(_, n)| n).sum::<u32>(), r.produced.total(Faction::A));
    // A mass strategy built its own unit, under its own name.
    assert!(r.produced.get(Faction::A, "ripper") > 0, "mass_ripper built no rippers");
    assert!(r.produced.get(Faction::B, "bulwark") > 0, "mass_bulwark built no bulwarks");
    // Ids are unique, so a name can only mean one column.
    let mut ids = r.produced.unit_ids().to_vec();
    ids.sort();
    let before = ids.len();
    ids.dedup();
    assert_eq!(ids.len(), before, "a duplicated id would make a name ambiguous");
}

/// B3 must be computable from these records alone: a win-rate matrix by
/// strategy, and per-unit production keyed by unit id, with no side table.
#[test]
fn probe_b3_metrics_are_computable_from_the_records_alone() {
    let c = content();
    let settings = BatchSettings::default()
        .with_only(
            ["mass_ripper", "mass_sentinel"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
        )
        .with_tick_cap(1_200)
        .with_seeds(1);
    let rows = batch::run_batch(&c, &settings, &mut |_| {}).expect("shipped");
    assert!(!rows.is_empty());

    // Win-rate matrix, from the rows only.
    let ids = ["mass_ripper", "mass_sentinel"];
    let mut played = 0u32;
    for (i, a) in ids.iter().enumerate() {
        for (j, b) in ids.iter().enumerate() {
            let cell: Vec<&batch::MatchRecord> = rows
                .iter()
                .filter(|r| r.strategies[0] == *a && r.strategies[1] == *b)
                .collect();
            assert!(!cell.is_empty(), "({a}, {b}) was never played");
            played += cell.len() as u32;
            let decided = cell.iter().filter(|r| r.result.is_decided()).count();
            let _ = (i, j, decided);
        }
    }
    assert_eq!(played as usize, rows.len(), "every row belongs to a matchup cell");

    // Per-unit production, keyed by id, without knowing content order.
    let mut per_unit: Vec<(String, u32)> = Vec::new();
    for r in &rows {
        for (id, n) in r.produced.by_unit(Faction::A).chain(r.produced.by_unit(Faction::B)) {
            match per_unit.iter_mut().find(|(k, _)| k == id) {
                Some((_, acc)) => *acc += n,
                None => per_unit.push((id.to_string(), n)),
            }
        }
    }
    let total: u32 = per_unit.iter().map(|(_, n)| n).sum();
    assert!(total > 0, "the batch built nothing");
    assert_eq!(
        total,
        batch::production_totals(&rows).iter().map(|(_, n)| n).sum::<u32>(),
        "the shipped totals and a by-id recount disagree"
    );
    // Every unit a mass strategy is named for shows up somewhere in the batch.
    let ripper = per_unit.iter().find(|(k, _)| k == "ripper").map(|(_, n)| *n);
    assert!(ripper.unwrap_or(0) > 0, "no rippers in a mass_ripper batch");
}

/// Summing by *name* is what stops two headers being added together
/// misaligned: a row with no roster must contribute **zero**, never someone
/// else's column. (Where the unlabelled row sits in the slice is a separate
/// question — see the note in the critic report.)
#[test]
fn probe_an_unlabelled_row_contributes_zero_not_a_misaligned_column() {
    let c = content();
    let played = batch::run_match(
        &c,
        &MatchSettings::default()
            .with_strategies("rush", "turtle")
            .with_tick_cap(1_200),
    )
    .expect("shipped");
    let real_total: u32 = played.produced.total(Faction::A) + played.produced.total(Faction::B);
    assert!(real_total > 0);

    let unlabelled = batch::MatchRecord {
        produced: ProductionCounts::default(),
        ..played.clone()
    };
    let rows = vec![played.clone(), unlabelled];
    let totals = batch::production_totals(&rows);
    let summed: u32 = totals.iter().map(|(_, n)| n).sum();
    assert_eq!(
        summed, real_total,
        "an unlabelled row was added to a played one as if it had counts"
    );
    let ids: Vec<&str> = totals.iter().map(|(id, _)| id.as_str()).collect();
    let expected: Vec<&str> = c.units.iter().map(|u| u.id.as_str()).collect();
    assert_eq!(ids, expected, "the totals are still named by the played roster");
}

/// Orientation is a reflection of the map, not a change of plan: the same
/// (strategies, seed) must build the same things in both orientations.
#[test]
fn probe_a_reflected_match_builds_the_same_things() {
    let c = content();
    let base = MatchSettings::default()
        .with_strategies("mass_ripper", "mass_ripper")
        .with_tick_cap(1_500);
    let n = batch::run_match(&c, &base.clone().with_orientation(Orientation::Normal)).expect("ok");
    let s = batch::run_match(&c, &base.with_orientation(Orientation::Swapped)).expect("ok");
    assert_eq!(
        n.produced, s.produced,
        "reflecting the map changed what each side built"
    );
    assert_eq!(n.ticks, s.ticks, "reflecting the map changed the match length");
    assert_eq!(n.result, s.result);
}

/// A timeout row and a decided row carry the same *kind* of block: fully
/// labelled, full roster width. No branch of `run_match` reports less.
#[test]
fn probe_every_branch_reports_a_full_width_block() {
    let c = content();
    let rows = [
        batch::run_match(&c, &MatchSettings::default().with_strategies("turtle", "turtle").with_tick_cap(600))
            .expect("shipped"),
        batch::run_match(&c, &MatchSettings::default().with_strategies("rush", "turtle"))
            .expect("shipped"),
    ];
    assert_eq!(rows[0].result, MatchResult::Timeout);
    assert!(rows[1].result.is_decided());
    for r in &rows {
        assert_eq!(r.produced.unit_ids().len(), c.units.len(), "full roster width");
        for f in headless::SIDES {
            assert_eq!(r.produced.counts(f).len(), c.units.len());
        }
        let ids: Vec<&str> = r.produced.unit_ids().iter().map(|s| s.as_str()).collect();
        let expected: Vec<&str> = c.units.iter().map(|u| u.id.as_str()).collect();
        assert_eq!(ids, expected);
    }
}
