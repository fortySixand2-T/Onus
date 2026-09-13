//! Critic probes for **B1 AC3** — the authored probe set in
//! `assets/data/strategies.ron`: five knob-identical mass-unit probes, two
//! cross-domain synthesis builds, an all-in rush and a turtle.
//!
//! Written independently of `tests/b1_probe_set.rs`. Each probe here is one the
//! spec requires to pass, and each attacks a way the set could be *unfit to
//! measure the pentagon* while still loading and "playing":
//!
//!   - comparability checked **structurally** (every field of `StrategyDef`,
//!     including any added later), not by an enumerated list;
//!   - "it massed its unit" read off the **world** (live entities by def), not
//!     off the commander's own journal of orders;
//!   - every strategy reaching its own `attack_at_army` *before* victory stops
//!     the sim chain, so no threshold assertion is vacuous;
//!   - every barracks a strategy opens is one it demonstrably trains from;
//!   - the whole set replaying bit-identically for a fixed (strategy, seed),
//!     and a head-to-head pair likewise;
//!   - the M5 namespace-collision probe still exercising the collision it names.

use std::path::PathBuf;

use bevy::prelude::*;

use onus::sim::combat::{Casualties, Health};
use onus::sim::content::{Content, StrategyDef};
use onus::sim::economy::{Building, ProductionQueue, Stockpiles, UnitDefIdx};
use onus::sim::spatial::Faction;
use onus::sim::{
    state_hash, AiAction, AiCommanders, AiJournal, CommandQueue, MatchState, Position, RateReport,
    ResourceNode,
};

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data")
}

fn content() -> Content {
    Content::load_from_dir(&data_dir()).expect("assets/data/*.ron parse")
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

/// A match fixture: mirror-symmetric bases, one deposit and three workers each,
/// and a commander for whichever factions `sides` names. Both sides get exactly
/// the same economy and the same map, so nothing about a probe's result comes
/// from its fixture.
fn fixture(sides: &[(Faction, &str)], seed: u64) -> App {
    let c = content();
    let commanders = AiCommanders::matchup(&c, seed, sides).expect("the set names these");
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

fn solo(id: &str, seed: u64) -> App {
    fixture(&[(Faction::A, id)], seed)
}

fn trace(app: &App, f: Faction) -> Vec<(u32, AiAction)> {
    app.world().resource::<AiJournal>().for_faction(f)
}

fn trained_of(app: &App, f: Faction) -> Vec<String> {
    let c = app.world().resource::<Content>();
    trace(app, f)
        .into_iter()
        .filter_map(|(_, a)| match a {
            AiAction::TrainArmy { unit } => Some(c.units[unit].id.clone()),
            _ => None,
        })
        .collect()
}

fn first_attack(app: &App, f: Faction) -> Option<(u32, u32)> {
    trace(app, f).into_iter().find_map(|(t, a)| match a {
        AiAction::Attack { force, .. } => Some((t, force)),
        _ => None,
    })
}

/// The combat units faction `f` has **alive in the world** right now, counted
/// by unit id. Read from the ECS, never from the journal: an order that was
/// logged but never became a unit must not count as "massed".
fn live_army(app: &mut App, f: Faction) -> Vec<(String, usize)> {
    let ids: Vec<String> = {
        let mut q = app
            .world_mut()
            .query::<(&UnitDefIdx, &Faction)>()
            .iter(app.world())
            .filter(|(_, fac)| **fac == f)
            .map(|(d, _)| d.0)
            .collect::<Vec<_>>();
        q.sort_unstable();
        let c = app.world().resource::<Content>();
        q.into_iter()
            .filter(|&d| c.units[d].offense > 0)
            .map(|d| c.units[d].id.clone())
            .collect()
    };
    let mut out: Vec<(String, usize)> = Vec::new();
    for id in ids {
        match out.iter_mut().find(|(n, _)| *n == id) {
            Some((_, n)) => *n += 1,
            None => out.push((id, 1)),
        }
    }
    out
}

fn strategy_ids(c: &Content) -> Vec<String> {
    c.strategies.iter().map(|s| s.id.clone()).collect()
}

fn mass_ids(c: &Content) -> Vec<String> {
    c.strategies
        .iter()
        .filter(|s| s.id.starts_with("mass_"))
        .map(|s| s.id.clone())
        .collect()
}

// ---- the set is what the spec asked for -------------------------------------

/// Five mass-unit probes (one per combat unit), at least two synthesis builds,
/// a rush and a turtle — the literal shopping list of the AC.
#[test]
fn the_set_contains_every_probe_the_spec_asks_for() {
    let c = content();
    let combat: Vec<&str> = c
        .units
        .iter()
        .filter(|u| u.offense > 0)
        .map(|u| u.id.as_str())
        .collect();
    assert_eq!(combat.len(), 5, "the roster is not five combat units: {combat:?}");
    let mass = mass_ids(&c);
    assert_eq!(mass.len(), 5, "not five mass probes: {mass:?}");
    for unit in &combat {
        let owners: Vec<&String> = mass
            .iter()
            .filter(|id| c.strategy(id).unwrap().army.iter().all(|a| &a.unit == unit))
            .collect();
        assert_eq!(owners.len(), 1, "`{unit}` is hard-committed to by {owners:?}");
    }
    let synth = c
        .strategies
        .iter()
        .filter(|s| s.id.starts_with("synth_"))
        .count();
    assert!(synth >= 2, "fewer than two synthesis builds: {synth}");
    for required in ["rush", "turtle"] {
        assert!(c.strategy(required).is_some(), "the set has no `{required}`");
    }
}

// ---- comparability, structurally --------------------------------------------

/// The five mass probes must differ in **nothing** but the barracks they open
/// and the unit they mass — and that has to hold for *every* field of
/// `StrategyDef`, including ones added after this test was written. So the
/// whole struct is compared through its `Debug` rendering with exactly those
/// three strings masked out. A field-by-field list can be outgrown; this
/// cannot.
#[test]
fn the_mass_probes_are_identical_in_every_field_of_the_struct() {
    let c = content();
    let normalised: Vec<(String, String)> = mass_ids(&c)
        .into_iter()
        .map(|id| {
            let s: &StrategyDef = c.strategy(&id).unwrap();
            assert_eq!(s.barracks.len(), 1, "`{id}` is not a single-barracks probe");
            assert_eq!(s.army.len(), 1, "`{id}` is not a single-entry build order");
            let text = format!("{s:?}")
                .replace(&format!("\"{}\"", s.id), "\"<ID>\"")
                .replace(&format!("\"{}\"", s.barracks[0].building), "\"<BARRACKS>\"")
                .replace(&format!("\"{}\"", s.army[0].unit), "\"<UNIT>\"");
            (id, text)
        })
        .collect();
    let (first_id, first) = &normalised[0];
    for (id, text) in &normalised[1..] {
        assert_eq!(
            text, first,
            "`{id}` and `{first_id}` differ in a field other than the barracks \
             and the massed unit — B3's pentagon would measure that field, not \
             the units"
        );
    }
}

// ---- every probe plays, measured off the world -------------------------------

/// Each mass probe really **masses**: by the time it commits, its army is made
/// of its own unit and nothing else, and it has at least `attack_at_army` of
/// them alive. Counted from live entities, not from the journal of orders.
#[test]
fn each_mass_probe_fields_an_army_of_its_own_unit() {
    let c = content();
    for id in mass_ids(&c) {
        let want_unit = c.strategy(&id).unwrap().army[0].unit.clone();
        let want_n = c.strategy(&id).unwrap().attack_at_army as usize;
        let mut app = solo(&id, 4);
        let mut committed = None;
        for t in 0..12_000u32 {
            step(&mut app);
            if committed.is_none() && first_attack(&app, Faction::A).is_some() {
                committed = Some(t);
                break;
            }
        }
        let at = committed.unwrap_or_else(|| panic!("`{id}` never committed"));
        let army = live_army(&mut app, Faction::A);
        assert_eq!(
            army.len(),
            1,
            "`{id}` committed at tick {at} with a mixed army {army:?} — it is not a mass probe"
        );
        assert_eq!(army[0].0, want_unit, "`{id}` massed `{}`", army[0].0);
        assert!(
            army[0].1 >= want_n,
            "`{id}` committed at tick {at} with {} live {want_unit}s, under its own \
             threshold {want_n}",
            army[0].1
        );
    }
}

/// Every strategy in the shipped set reaches its own `attack_at_army` **while
/// the match is still running** — a threshold asserted after the sim chain has
/// stopped at victory would be a claim about nothing.
#[test]
fn every_strategy_commits_before_the_match_can_stop_it() {
    const BUDGET: u32 = 12_000;
    let c = content();
    for id in strategy_ids(&c) {
        let want = c.strategy(&id).unwrap().attack_at_army;
        let mut app = solo(&id, 4);
        let mut over_at: Option<u32> = None;
        for t in 0..BUDGET {
            step(&mut app);
            if over_at.is_none() && app.world().resource::<MatchState>().is_over() {
                over_at = Some(t);
            }
        }
        let (at, force) = first_attack(&app, Faction::A)
            .unwrap_or_else(|| panic!("`{id}` never attacked in {BUDGET} ticks"));
        assert!(force >= want, "`{id}` attacked with {force} under its threshold {want}");
        if let Some(over) = over_at {
            assert!(
                at <= over,
                "`{id}` first attacked at tick {at} but the sim chain stopped at {over} — \
                 the probe's threshold was never reached in a running match"
            );
        }
    }
}

/// Every barracks a strategy opens is one it demonstrably **trains from** in a
/// real run. An opening that is placed and never used is 150-200 Alloy of dead
/// weight that distorts that strategy's economy for every B3 row it appears in.
#[test]
fn every_strategy_trains_out_of_every_barracks_it_opens() {
    const BUDGET: u32 = 12_000;
    let c = content();
    for id in strategy_ids(&c) {
        let s = c.strategy(&id).unwrap();
        let mut app = solo(&id, 4);
        tick(&mut app, BUDGET);
        let built = trained_of(&app, Faction::A);
        for opening in &s.barracks {
            let def = c.building_index(&opening.building).unwrap();
            assert!(
                built.iter().any(|u| {
                    c.unit_index(u).is_some_and(|idx| c.produces(def, idx))
                }),
                "`{id}` opened `{}` and trained nothing from it in {BUDGET} ticks \
                 (built {built:?})",
                opening.building
            );
        }
    }
}

/// The realised composition is the strategy's own ratio, not just "it built
/// something": over a full run the counts per unit are the prefix of the
/// repeating build order, unit for unit.
#[test]
fn the_realised_composition_is_the_build_orders_own_ratio() {
    const BUDGET: u32 = 12_000;
    let c = content();
    for id in strategy_ids(&c) {
        let s = c.strategy(&id).unwrap();
        let mut app = solo(&id, 4);
        tick(&mut app, BUDGET);
        let built = trained_of(&app, Faction::A);
        assert!(built.len() >= 3, "`{id}` trained only {built:?} — it stalls");
        let want: Vec<String> = (0..built.len() as u32)
            .map(|n| s.army_at(n).expect("a non-empty order").to_string())
            .collect();
        assert_eq!(built, want, "`{id}` built a composition its order never asked for");
    }
}

// ---- the two poles are two probes, not one ----------------------------------

/// `rush` is the **earliest** committer of the whole set and `turtle` the
/// latest, and the turtle brings the bigger first wave. Compared against every
/// other strategy, not only against each other.
#[test]
fn the_rush_is_the_earliest_and_the_turtle_the_latest_of_the_whole_set() {
    const BUDGET: u32 = 12_000;
    let c = content();
    let mut commits: Vec<(String, u32, u32)> = Vec::new();
    for id in strategy_ids(&c) {
        let mut app = solo(&id, 4);
        tick(&mut app, BUDGET);
        let (at, force) =
            first_attack(&app, Faction::A).unwrap_or_else(|| panic!("`{id}` never attacked"));
        commits.push((id, at, force));
    }
    let rush = commits.iter().find(|(id, _, _)| id == "rush").unwrap();
    let turtle = commits.iter().find(|(id, _, _)| id == "turtle").unwrap();
    for (id, at, _) in &commits {
        if id != "rush" {
            assert!(
                rush.1 < *at,
                "`rush` committed at {} and `{id}` at {at} — the rush is not the early one",
                rush.1
            );
        }
        if id != "turtle" {
            assert!(
                turtle.1 > *at,
                "`turtle` committed at {} and `{id}` at {at} — the turtle is not the late one",
                turtle.1
            );
        }
    }
    assert!(
        turtle.2 > rush.2,
        "turtle's first wave is {} and the rush's {} — the turtle is not massing",
        turtle.2,
        rush.2
    );
}

// ---- determinism -------------------------------------------------------------

/// The whole set replays bit-identically for a fixed (strategy, seed): two runs
/// agree on the canonical `state_hash` at every tick. A probe set that drifted
/// would make every B3 cell a different match each time it was measured.
#[test]
fn every_strategy_replays_tick_for_tick_on_the_same_seed() {
    const BUDGET: u32 = 1_500;
    for id in strategy_ids(&content()) {
        let mut a = solo(&id, 11);
        let mut b = solo(&id, 11);
        for t in 0..BUDGET {
            step(&mut a);
            step(&mut b);
            let (ha, hb) = (state_hash(a.world_mut()), state_hash(b.world_mut()));
            assert_eq!(ha, hb, "`{id}` diverged at tick {t}");
        }
        assert_eq!(trace(&a, Faction::A), trace(&b, Faction::A), "`{id}`'s trace drifted");
    }
}

/// ...and a head-to-head pair likewise, which is the shape B3 actually runs.
#[test]
fn a_head_to_head_pair_replays_tick_for_tick_on_the_same_seed() {
    const BUDGET: u32 = 2_000;
    let pair = [(Faction::A, "mass_sentinel"), (Faction::B, "mass_ripper")];
    let mut a = fixture(&pair, 23);
    let mut b = fixture(&pair, 23);
    for t in 0..BUDGET {
        step(&mut a);
        step(&mut b);
        assert_eq!(
            state_hash(a.world_mut()),
            state_hash(b.world_mut()),
            "the pair diverged at tick {t}"
        );
    }
}

// ---- fitness for B3: the matrix must carry signal ----------------------------

/// Every mass-vs-mass cell B3 will fill must **resolve** — a decided match
/// inside the horizon, in both orientations. A cell that times out is a hole in
/// the pentagon, and a matrix of holes cannot support the assertion B3 exists
/// to make.
#[test]
fn every_mass_versus_mass_cell_resolves_in_both_orientations() {
    const BUDGET: u32 = 20_000;
    let c = content();
    let mass = mass_ids(&c);
    for a in &mass {
        for b in &mass {
            let mut app = fixture(&[(Faction::A, a), (Faction::B, b)], 7);
            let mut outcome = None;
            for _ in 0..BUDGET {
                step(&mut app);
                if let Some(o) = app.world().resource::<MatchState>().outcome() {
                    outcome = Some(o);
                    break;
                }
            }
            let o = outcome
                .unwrap_or_else(|| panic!("`{a}` vs `{b}` never resolved in {BUDGET} ticks"));
            assert!(o.winner.is_some(), "`{a}` vs `{b}` ended with no winner");
        }
    }
}

// ---- audit of the tests/m5_replay.rs edit ------------------------------------

/// The M5 probe renames a *building* to a *unit's* id to prove the two
/// namespaces stay separate. That edit now has to touch `strategies.ron` too.
/// This re-derives the collision from scratch and checks it still has its full
/// force: the building and the unit both exist under the id `worker`, each
/// lookup finds its own, and the shipped strategies still open the renamed
/// barracks by its new name.
#[test]
fn the_namespace_collision_still_collides_after_the_strategies_rename() {
    let dir = std::env::temp_dir().join(format!("critic-b1-ac3-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    for file in ["units.ron", "resources.ron", "strategies.ron"] {
        let text = std::fs::read_to_string(data_dir().join(file)).expect("read");
        let text = match file {
            "units.ron" => text.replace("id: \"gene_vats\"", "id: \"worker\""),
            "strategies.ron" => text.replace("building: \"gene_vats\"", "building: \"worker\""),
            _ => text,
        };
        std::fs::write(dir.join(file), text).expect("write");
    }
    let c = Content::load_from_dir(&dir).expect("the collided content still loads");
    let building = c.building_index("worker").expect("a building named worker");
    let unit = c.unit_index("worker").expect("a unit named worker");
    assert_eq!(c.buildings[building].id, "worker");
    assert_eq!(c.units[unit].id, "worker");
    // The collision is real: the building still makes what the Gene-Vats made,
    // and the unit is still the gatherer.
    assert!(c.produces(building, c.unit_index("ripper").unwrap()));
    assert!(c.units[unit].gathers);
    // ...and the strategies that opened the Gene-Vats now open `worker`, so the
    // probe exercises a *used* collided id, not a dangling one.
    assert!(
        c.strategies
            .iter()
            .any(|s| s.barracks.iter().any(|b| b.building == "worker")),
        "no strategy opens the collided building — the rename is untested weight"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
