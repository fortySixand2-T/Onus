//! L2 integration tests for **B1 AC3** — the authored probe set in
//! `assets/data/strategies.ron`: five knob-identical "mass one unit" probes,
//! two cross-domain synthesis builds, an all-in rush and a turtle.
//!
//! A strategy that *loads* but cannot *play* is worthless to B3, so most of
//! what is encoded here is behavioural, through the shipped sim chain:
//!
//!   - **coverage** — every combat unit in the roster is massed by exactly one
//!     `mass_*` probe, and appears in at least one strategy's army. Derived
//!     from [`Content`], never from a hardcoded list of five names: a sixth
//!     combat unit added to `units.ron` must break this test, not slip past it;
//!   - **comparability** — the five `mass_*` entries are equal field by field
//!     in everything except the barracks they open and the unit they mass
//!     (F-018: otherwise B3's pentagon assertion measures the knobs);
//!   - **playability** — every strategy in the set, iterated from the content,
//!     places all of its barracks inside a tick budget, trains units, and the
//!     units it trains are exactly the prefix of its own repeating build order.
//!     A probe that cannot afford its opening, or waits forever on a barracks
//!     it never places, fails here;
//!   - **differentiation** — the rush commits earlier than the turtle, and the
//!     turtle fields more workers and a larger force before its first wave.
//!
//! Everything runs headless (`MinimalPlugins`) through the *shipped* sim chain
//! (`onus::add_sim_systems`), on the *shipped* content — these tests are about
//! what ships, so they build no fixture strategies.

use std::path::PathBuf;

use bevy::prelude::*;

use onus::sim::combat::{Casualties, Health};
use onus::sim::content::{Content, StrategyDef};
use onus::sim::economy::{Building, ProductionQueue, Stockpiles, UnitDefIdx};
use onus::sim::spatial::Faction;
use onus::sim::{
    AiAction, AiCommanders, AiJournal, CommandQueue, Position, RateReport, ResourceNode,
};

// ---- harness ----------------------------------------------------------------

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

/// A **solo** match: the standard headless AI-vs-AI fixture shape, but only
/// Faction A has a commander. The probe under test plays its whole script
/// against an inert base, so what the test reads is the *strategy's* economy
/// and build order and not the outcome of a fight — an opponent that killed the
/// probe's HQ would stop it thinking and look exactly like a stalled script.
fn solo(id: &str, seed: u64) -> App {
    let c = content();
    let commanders = AiCommanders::matchup(&c, seed, &[(Faction::A, id)])
        .unwrap_or_else(|e| panic!("the shipped set must name `{id}`: {e}"));
    let alloy = c.economy.starting_alloy;
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .insert_resource(Time::<Fixed>::from_hz(60.0))
        .insert_resource(c)
        .init_resource::<CommandQueue>()
        .init_resource::<RateReport>()
        .init_resource::<Casualties>()
        .insert_resource(Stockpiles::starting(alloy));
    onus::add_sim_systems(&mut app, Update);
    for (faction, base) in [
        (Faction::A, Vec2::new(-750.0, 0.0)),
        (Faction::B, Vec2::new(750.0, 0.0)),
    ] {
        let def = app
            .world()
            .resource::<Content>()
            .building_index("hq")
            .unwrap();
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

/// Faction A's tick-tagged action trace.
fn trace(app: &App) -> Vec<(u32, AiAction)> {
    app.world().resource::<AiJournal>().for_faction(Faction::A)
}

/// The buildings A placed, as ids, in the order it placed them.
fn placed(app: &App) -> Vec<String> {
    let c = app.world().resource::<Content>();
    trace(app)
        .into_iter()
        .filter_map(|(_, a)| match a {
            AiAction::PlaceBarracks { building, .. } => Some(c.buildings[building].id.clone()),
            _ => None,
        })
        .collect()
}

/// The army units A put into production, as ids, in build-order sequence.
fn trained(app: &App) -> Vec<String> {
    let c = app.world().resource::<Content>();
    trace(app)
        .into_iter()
        .filter_map(|(_, a)| match a {
            AiAction::TrainArmy { unit } => Some(c.units[unit].id.clone()),
            _ => None,
        })
        .collect()
}

/// A's first attack wave: `(tick, force)`, or `None` if it never committed.
fn first_attack(app: &App) -> Option<(u32, u32)> {
    trace(app).into_iter().find_map(|(t, a)| match a {
        AiAction::Attack { force, .. } => Some((t, force)),
        _ => None,
    })
}

/// How many workers A owned at `at` — the three it started with plus every one
/// it queued before that tick.
fn workers_by(app: &App, at: u32) -> u32 {
    3 + trace(app)
        .into_iter()
        .filter(|(t, a)| *t < at && matches!(a, AiAction::TrainWorker { .. }))
        .count() as u32
}

// ---- roster-derived helpers -------------------------------------------------

/// The combat units of the roster: exactly the units the sim will send to
/// fight (`offense > 0` is the filter `ai::think` itself uses). Derived, so a
/// sixth combat unit changes what these tests demand.
fn combat_units(c: &Content) -> Vec<String> {
    c.units
        .iter()
        .filter(|u| u.offense > 0)
        .map(|u| u.id.clone())
        .collect()
}

/// The distinct units a strategy's build order names, in RON order.
fn army_units(s: &StrategyDef) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for item in &s.army {
        if !out.contains(&item.unit) {
            out.push(item.unit.clone());
        }
    }
    out
}

/// The `mass_*` probes, paired with the single unit each one commits to.
/// A `mass_*` entry that names more than one unit is not a mass probe and is
/// reported here rather than quietly excluded.
fn mass_probes(c: &Content) -> Vec<(String, String)> {
    c.strategies
        .iter()
        .filter(|s| s.id.starts_with("mass_"))
        .map(|s| {
            let units = army_units(s);
            assert_eq!(
                units.len(),
                1,
                "`{}` is a mass probe but masses {units:?}",
                s.id
            );
            (s.id.clone(), units[0].clone())
        })
        .collect()
}

// ---- coverage ---------------------------------------------------------------

/// Every combat unit in the roster is massed by **exactly one** `mass_*` probe,
/// and no probe masses a unit twice. Both directions are asserted from the
/// roster, so adding a sixth combat unit to `units.ron` fails this test until a
/// probe for it exists.
#[test]
fn every_combat_unit_is_massed_by_exactly_one_probe() {
    let c = content();
    let roster = combat_units(&c);
    assert!(roster.len() >= 5, "the roster lost units: {roster:?}");

    let probes = mass_probes(&c);
    assert_eq!(
        probes.len(),
        roster.len(),
        "there are {} mass probes for {} combat units: {probes:?}",
        probes.len(),
        roster.len()
    );
    for unit in &roster {
        let for_unit: Vec<&String> = probes
            .iter()
            .filter(|(_, u)| u == unit)
            .map(|(id, _)| id)
            .collect();
        assert_eq!(
            for_unit.len(),
            1,
            "`{unit}` is massed by {for_unit:?}, not by exactly one probe"
        );
    }
    // And each probe commits to a real combat unit out of a single barracks.
    for (id, unit) in &probes {
        assert!(roster.contains(unit), "`{id}` masses non-combatant `{unit}`");
        let s = c.strategy(id).unwrap();
        assert_eq!(
            s.barracks.len(),
            1,
            "`{id}` hard-commits to one unit but opens {} barracks",
            s.barracks.len()
        );
    }
}

/// Every combat unit appears in at least one strategy's army — the weaker,
/// whole-set statement, which stays true however the probes are later named.
#[test]
fn every_combat_unit_is_built_by_some_strategy() {
    let c = content();
    for unit in combat_units(&c) {
        let by: Vec<&str> = c
            .strategies
            .iter()
            .filter(|s| s.army.iter().any(|i| i.unit == unit))
            .map(|s| s.id.as_str())
            .collect();
        assert!(
            !by.is_empty(),
            "no strategy in the set ever builds `{unit}` — the pentagon has a hole"
        );
    }
}

// ---- comparability (F-018) --------------------------------------------------

/// The five mass probes must differ in **nothing** but the barracks they open
/// and the unit they mass. Asserted field by field so a later edit to one of
/// them fails loudly rather than silently re-scaling B3's pentagon.
#[test]
fn the_mass_probes_are_knob_identical() {
    let c = content();
    let probes: Vec<&StrategyDef> = mass_probes(&c)
        .iter()
        .map(|(id, _)| c.strategy(id).unwrap())
        .collect();
    let (first, rest) = probes.split_first().expect("there are mass probes");
    for s in rest {
        let who = format!("`{}` vs `{}`", s.id, first.id);
        assert_eq!(
            s.think_interval_ticks, first.think_interval_ticks,
            "{who}: think_interval_ticks differs"
        );
        assert_eq!(
            s.worker_target, first.worker_target,
            "{who}: worker_target differs"
        );
        assert_eq!(
            s.attack_at_army, first.attack_at_army,
            "{who}: attack_at_army differs"
        );
        assert_eq!(
            s.attack_interval_ticks, first.attack_interval_ticks,
            "{who}: attack_interval_ticks differs"
        );
        assert_eq!(
            s.attack_spread, first.attack_spread,
            "{who}: attack_spread differs"
        );
        // One opening each, at the same tick and the same distance from home.
        assert_eq!(s.barracks.len(), first.barracks.len(), "{who}: barracks count differs");
        assert_eq!(
            s.barracks[0].at_tick, first.barracks[0].at_tick,
            "{who}: the opening tick differs"
        );
        assert_eq!(
            s.barracks[0].offset, first.barracks[0].offset,
            "{who}: the barracks offset differs"
        );
        // One army entry each, of the same size: only the unit id differs.
        assert_eq!(s.army.len(), 1, "{who}: a mass probe has one army entry");
        assert_eq!(s.army[0].count, first.army[0].count, "{who}: the army count differs");
        assert_eq!(
            s.cycle_len(),
            first.cycle_len(),
            "{who}: the build-order cycle length differs"
        );
    }
    // ...and they really are five *different* units out of the roster's
    // barracks, not five names for one build.
    let units: Vec<String> = probes.iter().map(|s| s.army[0].unit.clone()).collect();
    let mut sorted = units.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), units.len(), "two probes mass the same unit: {units:?}");
}

// ---- the synthesis builds span domains --------------------------------------

/// At least one synthesis build crosses two barracks and at least one crosses
/// all of them, and each really trains out of every barracks it opens — the
/// multi-barracks schema exercised by shipped content, not only by fixtures.
#[test]
fn the_synthesis_builds_span_domains() {
    let c = content();
    let placeable = c.buildings.iter().filter(|b| !b.victory).count();
    let synth: Vec<&StrategyDef> = c
        .strategies
        .iter()
        .filter(|s| s.id.starts_with("synth_"))
        .collect();
    assert!(
        synth.len() >= 2,
        "the set needs at least two synthesis builds, found {}",
        synth.len()
    );
    assert!(
        synth.iter().any(|s| s.barracks.len() >= 2),
        "no synthesis build crosses two barracks"
    );
    assert!(
        synth.iter().any(|s| s.barracks.len() == placeable),
        "no synthesis build spans all {placeable} barracks"
    );
    // Every barracks a synthesis build opens is one its army actually uses;
    // an opening nothing is trained from is 150 Alloy of nothing.
    for s in &synth {
        for opening in &s.barracks {
            let def = c.building_index(&opening.building).unwrap();
            assert!(
                army_units(s).iter().any(|u| {
                    c.unit_index(u).is_some_and(|idx| c.produces(def, idx))
                }),
                "`{}` opens `{}` but trains nothing from it",
                s.id,
                opening.building
            );
        }
    }
}

// ---- playability: every probe actually plays --------------------------------

/// The core of the AC. **Every** strategy in the shipped set — iterated from
/// the content, never enumerated by hand — plays a solo match to a tick budget
/// and must: place all of its barracks, train units of its own army, and
/// produce exactly the prefix of its own repeating build order.
#[test]
fn every_strategy_places_its_barracks_and_builds_its_own_order() {
    const BUDGET: u32 = 6_000;
    const MIN_TRAINED: usize = 2;

    let ids: Vec<String> = content().strategies.iter().map(|s| s.id.clone()).collect();
    assert!(ids.len() >= 9, "the probe set is short: {ids:?}");
    for id in &ids {
        let mut app = solo(id, 4);
        tick(&mut app, BUDGET);
        let c = content();
        let s = c.strategy(id).unwrap();

        // 1. every opening went up, once, inside the budget.
        let want: Vec<String> = s.barracks.iter().map(|b| b.building.clone()).collect();
        assert_eq!(
            placed(&app),
            want,
            "`{id}` did not place its barracks within {BUDGET} ticks (it cannot \
             afford its opening, or its build order is stalled behind it)"
        );

        // 2. it trained a real army, not one token unit.
        let built = trained(&app);
        assert!(
            built.len() >= MIN_TRAINED,
            "`{id}` trained only {built:?} in {BUDGET} ticks — it stalls"
        );

        // 3. the composition is its build order, in order: the n-th unit it
        //    trained is the n-th unit of its repeating cycle. This catches both
        //    "built something it never asked for" and "skipped ahead past a
        //    barracks it was waiting on".
        let want_order: Vec<String> = (0..built.len() as u32)
            .map(|n| s.army_at(n).expect("a non-empty build order").to_string())
            .collect();
        assert_eq!(
            built, want_order,
            "`{id}` produced a composition its build order never asked for"
        );
    }
}

/// Every strategy commits: inside a generous budget each one sends at least one
/// wave. A probe that masses forever and never attacks would make every B3
/// matchup involving it a timeout.
#[test]
fn every_strategy_eventually_attacks() {
    const BUDGET: u32 = 12_000;
    let ids: Vec<String> = content().strategies.iter().map(|s| s.id.clone()).collect();
    for id in &ids {
        let mut app = solo(id, 4);
        tick(&mut app, BUDGET);
        let (at, force) = first_attack(&app).unwrap_or_else(|| {
            panic!("`{id}` never attacked in {BUDGET} ticks — it cannot reach its own \
                    attack_at_army threshold")
        });
        let want = content().strategy(id).unwrap().attack_at_army;
        assert!(
            force >= want,
            "`{id}` attacked at tick {at} with {force} units, under its own threshold {want}"
        );
    }
}

// ---- differentiation: the two poles really are two poles --------------------

/// The rush and the turtle are opposite ends of the same axis, and the sim has
/// to show it: the rush commits far earlier, the turtle fields more workers and
/// a bigger force before its first wave. Two probes that play the same are one
/// probe with two names.
#[test]
fn the_rush_commits_early_and_the_turtle_masses_first() {
    const BUDGET: u32 = 12_000;
    let mut rush = solo("rush", 4);
    tick(&mut rush, BUDGET);
    let mut turtle = solo("turtle", 4);
    tick(&mut turtle, BUDGET);

    let (rush_at, rush_force) = first_attack(&rush).expect("the rush must attack");
    let (turtle_at, turtle_force) = first_attack(&turtle).expect("the turtle must attack");

    assert!(
        rush_at < turtle_at,
        "the rush committed at {rush_at} and the turtle at {turtle_at} — the rush is not a rush"
    );
    assert!(
        turtle_force > rush_force,
        "the turtle attacked with {turtle_force} and the rush with {rush_force} — \
         the turtle is not massing"
    );
    assert!(
        workers_by(&turtle, turtle_at) > workers_by(&rush, rush_at),
        "the turtle fielded {} workers to the rush's {} — the economies are the same",
        workers_by(&turtle, turtle_at),
        workers_by(&rush, rush_at)
    );
    // The rush is all-in by construction: it never out-economies the default.
    let c = content();
    assert!(
        c.strategy("rush").unwrap().worker_target < c.ai.worker_target,
        "the rush keeps more workers than the MVP default"
    );
    assert!(
        c.strategy("turtle").unwrap().worker_target > c.ai.worker_target,
        "the turtle keeps no more workers than the MVP default"
    );
}
