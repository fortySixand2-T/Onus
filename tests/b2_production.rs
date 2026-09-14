//! L2 integration tests for **B2 AC4** — what a match record records.
//!
//! Winner and length were already on [`batch::MatchRecord`]; the new fact is
//! **units produced per side**, and it is recorded *per unit type* because the
//! kill criterion B3 has to evaluate ("no unit winning >65% regardless of
//! counter") is a statement about units, not about strategies.
//!
//! What is encoded here:
//!
//!   - **a spawn is what counts** — [`Produced`] rises when `production`
//!     actually spawns a unit, for the right faction and the right unit type,
//!     exactly once; an order that is refused or that never finishes moves
//!     nothing;
//!   - **the fixture's starting workers are not production** — they were
//!     placed by the harness, not built by the sim, and a batch that counted
//!     them would report three free workers a side in every row;
//!   - **produced is not "survived"** — a unit that is built and then killed
//!     still counts as produced; production and [`Casualties`] are independent
//!     counters;
//!   - **a record is interpretable on its own** — its counts carry the unit
//!     ids they are keyed by, so a column can never be read against the wrong
//!     unit;
//!   - **determinism** — the same settings give the same counts, and a
//!     re-run batch gives identical records including production;
//!   - **every outcome carries them** — decided, mutual loss and timeout.

use bevy::prelude::*;

use onus::batch::{self, BatchSettings, MatchResult, ProductionCounts};
use onus::headless::{self, MatchSettings, Orientation};
use onus::sim::combat::{Casualties, Health};
use onus::sim::content::Content;
use onus::sim::economy::{
    enqueue_unit, Building, Produced, ProductionQueue, Stockpiles, UnitDefIdx,
};
use onus::sim::spatial::Faction;
use onus::sim::{CommandQueue, Position, RateReport};

fn content() -> Content {
    headless::content().expect("assets/data/*.ron parse into sim structs")
}

/// The headless fixture's starting workers per side — placed by the harness,
/// never by `production`.
const STARTING_WORKERS: u32 = 3;

/// Units of each type alive right now, per faction slot, indexed by unit def.
fn alive(app: &mut App, c: &Content) -> [Vec<u32>; 2] {
    let mut out = [vec![0u32; c.units.len()], vec![0u32; c.units.len()]];
    let mut q = app.world_mut().query::<(&UnitDefIdx, &Faction)>();
    for (def, faction) in q.iter(app.world()) {
        let slot = usize::from(*faction == Faction::B);
        out[slot][def.0] += 1;
    }
    out
}

// ---- a spawn is what counts -------------------------------------------------

/// Nothing is produced before the sim produces anything: the three starting
/// workers a side exist, and the counter is still zero.
#[test]
fn starting_workers_are_not_counted_as_production() {
    let c = content();
    let mut app = headless::ai_vs_ai(c.clone(), &MatchSettings::default()).expect("default match");
    let worker = c.unit_index("worker").expect("a worker unit");

    let produced = app.world().resource::<Produced>().clone();
    for f in headless::SIDES {
        assert_eq!(produced.total(f), 0, "{f:?} has produced nothing yet");
        assert_eq!(produced.count(f, worker), 0);
    }
    let alive = alive(&mut app, &c);
    assert_eq!(
        [alive[0][worker], alive[1][worker]],
        [STARTING_WORKERS, STARTING_WORKERS],
        "the fixture's workers are on the map but were not produced"
    );

    // And it only rises once the sim spawns something: the count is zero until
    // the first spawn and non-zero after it.
    let mut first_spawn = None;
    for t in 1..=1_800 {
        headless::tick(&mut app, 1);
        if app.world().resource::<Produced>().grand_total() > 0 {
            first_spawn = Some(t);
            break;
        }
    }
    let first_spawn = first_spawn.expect("30s of the default strategy builds something");
    let produced = app.world().resource::<Produced>().clone();
    // One tick's worth of spawns, not a backlog counted at once: the two sides
    // are symmetric here and finish their first unit on the same tick, so at
    // most one each.
    for f in headless::SIDES {
        assert!(
            produced.total(f) <= 1,
            "{f:?} counted {} units on the first spawn tick ({first_spawn})",
            produced.total(f)
        );
    }
    assert!(produced.grand_total() >= 1);
}

/// Every unit standing at the end of an uncontested opening is either a
/// starting worker or a counted production — per faction *and* per type.
#[test]
fn a_produced_unit_is_counted_once_for_the_right_faction_and_type() {
    let c = content();
    // A mixed build, played long enough to train several kinds of unit but
    // short enough that the two bases (1500 apart) have not met: nothing has
    // died, so "alive" and "produced" are directly comparable.
    let settings = MatchSettings::default().with_strategies("mass_ripper", "turtle");
    let mut app = headless::ai_vs_ai(c.clone(), &settings).expect("shipped names");
    headless::tick(&mut app, 1_800);

    assert_eq!(
        app.world().resource::<Casualties>().total(),
        0,
        "no one has died yet, so alive == produced + starting"
    );
    let worker = c.unit_index("worker").expect("a worker unit");
    let alive = alive(&mut app, &c);
    let produced = app.world().resource::<Produced>().clone();
    let mut any = 0u32;
    for (slot, f) in headless::SIDES.into_iter().enumerate() {
        for (unit, standing) in alive[slot].iter().copied().enumerate() {
            let starting = if unit == worker { STARTING_WORKERS } else { 0 };
            assert_eq!(
                standing,
                produced.count(f, unit) + starting,
                "{f:?} {}: alive {standing} vs produced {} (+{starting} starting)",
                c.units[unit].id,
                produced.count(f, unit),
            );
            any += produced.count(f, unit);
        }
        assert_eq!(
            produced.total(f),
            produced.counts(f).iter().sum::<u32>(),
            "the per-faction total is the sum of its columns"
        );
    }
    assert!(any > 0, "half a minute of play produced something");
    // A mass strategy builds its own unit; that unit is counted under its own
    // id, not under someone else's.
    let ripper = c.unit_index("ripper").expect("a ripper unit");
    assert!(
        produced.count(Faction::A, ripper) > 0,
        "mass_ripper built no rippers in 30s: {:?}",
        produced.counts(Faction::A)
    );
}

// ---- a headless app with the shipped chain, for hand-driven orders ----------

fn sim_app(content: Content, stock: Stockpiles) -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .insert_resource(Time::<Fixed>::from_hz(60.0))
        .insert_resource(content)
        .init_resource::<CommandQueue>()
        .init_resource::<RateReport>()
        .insert_resource(stock);
    onus::add_sim_systems(&mut app, Update);
    app
}

fn step(app: &mut App, n: u32) {
    for _ in 0..n {
        let dt = app.world().resource::<Time<Fixed>>().timestep();
        app.world_mut().resource_mut::<Time<Fixed>>().advance_by(dt);
        app.update();
    }
}

/// Spawn an HQ for `faction` and return it.
fn hq(app: &mut App, faction: Faction) -> Entity {
    let def = app
        .world()
        .resource::<Content>()
        .building_index("hq")
        .expect("content has an `hq` building");
    app.world_mut()
        .spawn((
            Position(Vec2::ZERO),
            Building { def },
            faction,
            ProductionQueue::default(),
        ))
        .id()
}

/// Order `unit` from `base` through the sim's own `enqueue_unit` (charge once,
/// then queue). Returns what the sim decided: `false` is a *refusal*, and a
/// refusal spends nothing and queues nothing.
fn order_unit(app: &mut App, base: Entity, faction: Faction, unit: usize) -> bool {
    let c = app.world().resource::<Content>().clone();
    let hq_def = c.building_index("hq").expect("content has an `hq` building");
    let mut stock = *app.world().resource::<Stockpiles>();
    let ok = {
        let mut base = app.world_mut().entity_mut(base);
        let mut queue = base.get_mut::<ProductionQueue>().expect("the hq has a queue");
        enqueue_unit(&c, &mut stock, hq_def, faction, &mut queue, unit)
    };
    if ok {
        *app.world_mut().resource_mut::<Stockpiles>() = stock;
    }
    ok
}

/// An order the sim refuses (no Alloy) never enters a queue, and so can never
/// be counted — production is measured at the spawn, not at the order.
#[test]
fn a_refused_order_is_never_counted() {
    let c = content();
    let worker = c.unit_index("worker").expect("a worker unit");
    let mut app = sim_app(c.clone(), Stockpiles::default()); // no Alloy at all
    let base = hq(&mut app, Faction::A);

    assert!(
        !order_unit(&mut app, base, Faction::A, worker),
        "an empty stockpile cannot pay for a worker"
    );
    step(&mut app, 600);
    let produced = app.world().resource::<Produced>().clone();
    assert_eq!(produced.total(Faction::A), 0, "a refusal produced nothing");
    assert_eq!(produced.count(Faction::A, worker), 0);
}

/// A unit whose training never finishes is not a unit. The counter moves on
/// the tick the spawn happens and not one tick earlier.
#[test]
fn an_unfinished_order_is_not_counted_until_it_spawns() {
    let c = content();
    let worker = c.unit_index("worker").expect("a worker unit");
    let train_ticks = c.units[worker].mvp_train_ticks;
    assert!(train_ticks > 1, "the fixture needs a unit that takes time");

    let mut app = sim_app(c.clone(), Stockpiles::starting(10_000));
    let base = hq(&mut app, Faction::A);
    assert!(order_unit(&mut app, base, Faction::A, worker));

    // One tick short of the spawn: paid for, queued, still not produced.
    step(&mut app, train_ticks);
    assert_eq!(
        app.world().resource::<Produced>().total(Faction::A),
        0,
        "an order in flight is not a unit"
    );
    step(&mut app, 1);
    assert_eq!(
        app.world().resource::<Produced>().total(Faction::A),
        1,
        "the tick the unit spawns is the tick it is counted"
    );
    assert_eq!(app.world().resource::<Produced>().count(Faction::A, worker), 1);
    assert_eq!(
        app.world().resource::<Produced>().total(Faction::B),
        0,
        "the other side produced nothing"
    );

    // And the count does not drift afterwards.
    step(&mut app, 100);
    assert_eq!(app.world().resource::<Produced>().total(Faction::A), 1);
}

/// "How many were built" is not "how many survived": killing a produced unit
/// moves [`Casualties`], never [`Produced`].
#[test]
fn a_produced_unit_that_dies_still_counts_as_produced() {
    let c = content();
    let worker = c.unit_index("worker").expect("a worker unit");
    let mut app = sim_app(c.clone(), Stockpiles::starting(10_000));
    let base = hq(&mut app, Faction::A);
    assert!(order_unit(&mut app, base, Faction::A, worker));
    step(&mut app, c.units[worker].mvp_train_ticks + 1);
    assert_eq!(app.world().resource::<Produced>().count(Faction::A, worker), 1);

    // Kill it: the harness despawns it directly, which is the strongest form of
    // "it is no longer alive".
    let victim = {
        let mut q = app.world_mut().query::<(Entity, &UnitDefIdx)>();
        let e = q
            .iter(app.world())
            .map(|(e, _)| e)
            .next()
            .expect("the produced worker exists");
        e
    };
    app.world_mut().entity_mut(victim).despawn();
    step(&mut app, 10);

    let mut q = app.world_mut().query::<&UnitDefIdx>();
    assert_eq!(q.iter(app.world()).count(), 0, "nothing is alive");
    assert_eq!(
        app.world().resource::<Produced>().count(Faction::A, worker),
        1,
        "production is a record of what was built, not of what survived"
    );
    assert_eq!(
        app.world().resource::<Casualties>().total(),
        0,
        "the two counters are independent"
    );
    // Health is what the sim kills with; the fixture's despawn stands in for it.
    assert!(Health::from_def(&c, worker).max > 0);
}

// ---- the record ------------------------------------------------------------

fn short(a: &str, b: &str, cap: u32) -> MatchSettings {
    MatchSettings::default()
        .with_strategies(a, b)
        .with_tick_cap(cap)
}

/// A record's columns carry the unit ids they are keyed by, so they cannot be
/// read against the wrong roster — and the ids are the content's, in content
/// order.
#[test]
fn a_record_maps_its_counts_to_the_right_unit_ids() {
    let c = content();
    let record = batch::run_match(&c, &short("mass_ripper", "turtle", 1_800)).expect("shipped");
    let ids: Vec<&str> = record.produced.unit_ids().iter().map(|s| s.as_str()).collect();
    let expected: Vec<&str> = c.units.iter().map(|u| u.id.as_str()).collect();
    assert_eq!(ids, expected, "the header is the content's unit order");

    // Looked up by id, the count matches the column at that unit's index.
    for (i, id) in expected.iter().enumerate() {
        for f in headless::SIDES {
            assert_eq!(record.produced.get(f, id), record.produced.counts(f)[i]);
        }
    }
    // An id the content does not have is zero, not a panic and not a shifted
    // column.
    assert_eq!(record.produced.get(Faction::A, "no_such_unit"), 0);
    // The record agrees with the world it was played in.
    let mut app = headless::ai_vs_ai(c.clone(), &short("mass_ripper", "turtle", 1_800))
        .expect("shipped");
    headless::tick(&mut app, record.ticks);
    let world_counts = app.world().resource::<Produced>().clone();
    for f in headless::SIDES {
        for unit in 0..c.units.len() {
            assert_eq!(record.produced.counts(f)[unit], world_counts.count(f, unit));
        }
        assert_eq!(record.produced.total(f), world_counts.total(f));
    }
    assert!(record.produced.total(Faction::A) > 0);
    let ripper = c.unit_index("ripper").expect("a ripper unit");
    assert_eq!(
        record.produced.get(Faction::A, "ripper"),
        record.produced.counts(Faction::A)[ripper]
    );
}

/// Same settings, same content ⇒ same counts; and a re-run batch is identical
/// row for row, production included.
#[test]
fn production_counts_are_deterministic() {
    let c = content();
    let s = short("rush", "mass_ripper", 1_200);
    let a = batch::run_match(&c, &s).expect("shipped");
    let b = batch::run_match(&c, &s).expect("shipped");
    assert_eq!(a, b, "the same match twice is the same record");
    assert!(a.produced.total(Faction::A) + a.produced.total(Faction::B) > 0);

    let settings = BatchSettings::default()
        .with_only(["rush", "turtle"].iter().map(|s| s.to_string()).collect())
        .with_tick_cap(900)
        .with_seeds(1);
    let first = batch::run_batch(&c, &settings, &mut |_| {}).expect("shipped");
    let second = batch::run_batch(&c, &settings, &mut |_| {}).expect("shipped");
    assert_eq!(first, second, "a re-run batch is identical, production included");
    assert!(first.iter().any(|r| r.produced.total(Faction::A) > 0));
}

/// A capped (undecided) match still says what was built in it — the length and
/// the winner are not the only facts a row carries.
#[test]
fn a_timeout_record_carries_production() {
    let c = content();
    let r = batch::run_match(&c, &short("turtle", "turtle", 1_500)).expect("shipped");
    assert_eq!(r.result, MatchResult::Timeout);
    assert_eq!(r.ticks, 1_500);
    assert!(
        r.produced.total(Faction::A) > 0 || r.produced.total(Faction::B) > 0,
        "a 25-second opening built nothing"
    );
    assert_eq!(r.produced.unit_ids().len(), c.units.len());
}

/// A decided match carries the production of the whole match, up to the tick it
/// was decided on.
#[test]
fn a_decided_record_carries_production() {
    let c = content();
    let settings = MatchSettings::default().with_strategies("rush", "turtle");
    let r = batch::run_match(&c, &settings).expect("shipped");
    assert!(r.result.is_decided(), "this matchup decides within the cap");
    assert!(r.winner().is_some());
    let built: u32 = headless::SIDES.iter().map(|f| r.produced.total(*f)).sum();
    assert!(built > 0, "a whole match built nothing");
    // The winner built at least one unit of its own: a decided match is not
    // won by the starting workers alone in this fixture.
    assert!(r.produced.total(r.winner().expect("a winner")) > 0);
}

/// Every [`MatchResult`] variant carries counts. `Decided` and `Timeout` are
/// reachable from a played match above; `MutualLoss` is not reachable from the
/// shipped fixture on demand (it needs both HQs to fall on the same tick), so
/// it is covered at the record's own constructor — the one every branch of
/// `run_match` goes through.
#[test]
fn every_outcome_carries_production() {
    let c = content();
    let settings = short("rush", "turtle", 900);
    let counts = ProductionCounts::zeroed(&c);
    for result in [
        MatchResult::Decided(Faction::A),
        MatchResult::MutualLoss,
        MatchResult::Timeout,
    ] {
        let r = batch::MatchRecord::new(&c, &settings, result, 900, counts.clone());
        assert_eq!(r.result, result);
        assert_eq!(r.produced.unit_ids().len(), c.units.len());
        assert_eq!(r.produced.total(Faction::A), 0);
    }
}

/// The batch's summary line: what the whole run built, per unit, in content
/// order — the number the bin prints.
#[test]
fn a_batch_totals_production_per_unit_in_content_order() {
    let c = content();
    let settings = BatchSettings::default()
        .with_only(["rush", "mass_ripper"].iter().map(|s| s.to_string()).collect())
        .with_tick_cap(900)
        .with_seeds(1);
    let rows = batch::run_batch(&c, &settings, &mut |_| {}).expect("shipped");
    let totals = batch::production_totals(&rows);
    let ids: Vec<&str> = totals.iter().map(|(id, _)| id.as_str()).collect();
    let expected: Vec<&str> = c.units.iter().map(|u| u.id.as_str()).collect();
    assert_eq!(ids, expected);
    let summed: u32 = totals.iter().map(|(_, n)| n).sum();
    let by_row: u32 = rows
        .iter()
        .map(|r| r.produced.total(Faction::A) + r.produced.total(Faction::B))
        .sum();
    assert_eq!(summed, by_row, "the totals are the rows, summed");
    assert!(summed > 0);
    assert_eq!(batch::production_totals(&[]), Vec::new());
}

/// Orientation does not change *what* a record reports: both orientations of a
/// mirror carry a full, labelled production block.
#[test]
fn both_orientations_record_production() {
    let c = content();
    for o in Orientation::ALL {
        let s = short("mass_ripper", "mass_ripper", 1_200).with_orientation(o);
        let r = batch::run_match(&c, &s).expect("shipped");
        assert_eq!(r.orientation, o);
        assert_eq!(r.produced.unit_ids().len(), c.units.len());
        assert!(r.produced.total(Faction::A) + r.produced.total(Faction::B) > 0);
    }
}
