//! L2 integration tests for M4c (scripted AI + win condition).
//!
//! Encodes the M4c acceptance criteria, the critic probes, and the three items
//! BUILD_PLAN.md inherited into this milestone:
//!   AC1 — a scripted AI gathers, builds a Barracks, trains a mixed force and
//!         attack-moves, on a **tick** timer, deterministic given the seed;
//!   AC2 — destroying the enemy HQ ends the match (≤ ~8 min = 28_800 ticks);
//!   inherited — orders carry their issuer (cross-faction orders are refused),
//!         orders against despawned entities never panic, and a gather claim is
//!         never written or released without its phase.
//!
//! Everything runs headless (`MinimalPlugins`) through the *shipped* sim chain
//! (`onus::add_sim_systems`) — no render types, no hand-rolled system list
//! (F-004), one fixed timestep handed to the sim per step (F-003).

use std::path::PathBuf;

use bevy::prelude::*;

use onus::sim::combat::{Casualties, Health};
use onus::sim::content::Content;
use onus::sim::economy::{Building, ProductionQueue, Stockpiles, UnitDefIdx};
use onus::sim::spatial::Faction;
use onus::sim::{
    AiAction, AiCommanders, AiJournal, Attribution, CommandQueue, GatherPhase, GatherTarget,
    MatchOutcome, MatchState, MoveTarget, Order, Position, RateReport, ResourceNode,
};

// ---- harness ----------------------------------------------------------------

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data")
}

fn content() -> Content {
    Content::load_from_dir(&data_dir()).expect("assets/data/*.ron parse into sim structs")
}

/// A headless app running the shipped sim chain on `Update`, so one
/// `step()` == exactly one 60 Hz sim tick.
fn sim_app_with_alloy(alloy: u32) -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .insert_resource(Time::<Fixed>::from_hz(60.0))
        .insert_resource(content())
        .init_resource::<CommandQueue>()
        .init_resource::<RateReport>()
        .init_resource::<Casualties>()
        .insert_resource(Stockpiles::starting(alloy));
    onus::add_sim_systems(&mut app, Update);
    app
}

fn sim_app() -> App {
    sim_app_with_alloy(0)
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

fn push(app: &mut App, order: Order) {
    app.world_mut()
        .resource_mut::<CommandQueue>()
        .0
        .push_back(order);
}

fn spawn_unit(app: &mut App, id: &str, faction: Faction, pos: Vec2) -> Entity {
    let (idx, kind, hp) = {
        let c = app.world().resource::<Content>();
        let idx = c.unit_index(id).unwrap_or_else(|| panic!("unit `{id}`"));
        (idx, c.units[idx].mvp_kind, Health::from_def(c, idx))
    };
    app.world_mut()
        .spawn((Position(pos), UnitDefIdx(idx), kind, faction, hp))
        .id()
}

fn spawn_building(app: &mut App, id: &str, faction: Faction, pos: Vec2) -> Entity {
    let def = {
        let c = app.world().resource::<Content>();
        c.building_index(id)
            .unwrap_or_else(|| panic!("building `{id}`"))
    };
    app.world_mut()
        .spawn((
            Position(pos),
            Building { def },
            faction,
            ProductionQueue::default(),
        ))
        .id()
}

fn alloy(app: &App, f: Faction) -> u32 {
    app.world().resource::<Stockpiles>().alloy(f)
}

fn unit_index(app: &App, id: &str) -> usize {
    app.world().resource::<Content>().unit_index(id).unwrap()
}

fn queued(app: &App, building: Entity) -> usize {
    app.world()
        .get::<ProductionQueue>(building)
        .map(|q| q.items.len())
        .unwrap_or(0)
}

// ---- inherited 1: orders carry their issuer ---------------------------------

/// `Order::Train` used to charge the *targeted building's* faction, whoever
/// issued it. With a second commander on the field that is a way to spend the
/// enemy's Alloy (and fill their queue). A signed order against someone else's
/// building must be refused outright.
#[test]
fn a_train_order_against_another_factions_building_is_refused() {
    let mut app = sim_app_with_alloy(1_000);
    let hq = spawn_building(&mut app, "hq", Faction::B, Vec2::ZERO);
    let worker = unit_index(&app, "worker");
    push(
        &mut app,
        Order::Train {
            building: hq,
            unit: worker,
        }
        .issued_by(Faction::A),
    );
    step(&mut app);
    assert_eq!(alloy(&app, Faction::B), 1_000, "B paid for A's order");
    assert_eq!(
        alloy(&app, Faction::A),
        1_000,
        "A was charged for a refusal"
    );
    assert_eq!(queued(&app, hq), 0, "A queued a unit in B's HQ");
}

/// The property the ownership check could break: a commander's order against
/// its **own** building still trains, and is charged exactly once.
#[test]
fn a_train_order_against_ones_own_building_still_trains() {
    let mut app = sim_app_with_alloy(1_000);
    let hq = spawn_building(&mut app, "hq", Faction::B, Vec2::ZERO);
    let worker = unit_index(&app, "worker");
    let cost = app.world().resource::<Content>().units[worker].mvp_alloy_cost;
    push(
        &mut app,
        Order::Train {
            building: hq,
            unit: worker,
        }
        .issued_by(Faction::B),
    );
    step(&mut app);
    assert_eq!(queued(&app, hq), 1, "the owner's own order was refused");
    assert_eq!(alloy(&app, Faction::B), 1_000 - cost);
}

/// Movement orders are ownership-checked per unit: a mixed list commands only
/// the issuer's own units, and leaves the enemy's alone.
#[test]
fn a_move_order_commands_only_the_issuers_own_units() {
    let mut app = sim_app();
    let mine = spawn_unit(&mut app, "ripper", Faction::A, Vec2::ZERO);
    let theirs = spawn_unit(&mut app, "ripper", Faction::B, Vec2::new(1_000.0, 0.0));
    push(
        &mut app,
        Order::MoveTo {
            units: vec![mine, theirs],
            dest: Vec2::new(500.0, 500.0),
        }
        .issued_by(Faction::A),
    );
    step(&mut app);
    assert!(
        app.world().get::<MoveTarget>(mine).is_some(),
        "the issuer's own unit ignored the order"
    );
    assert!(
        app.world().get::<MoveTarget>(theirs).is_none(),
        "a commander moved the enemy's unit"
    );
}

/// Same for `Order::Gather`: neither half of the order (the claim or the move)
/// may land on another faction's worker.
#[test]
fn a_gather_order_never_tasks_another_factions_worker() {
    let mut app = sim_app();
    let node_pos = Vec2::new(100.0, 0.0);
    let node = app
        .world_mut()
        .spawn((Position(node_pos), ResourceNode { amount: 500 }))
        .id();
    let theirs = spawn_unit(&mut app, "worker", Faction::B, Vec2::ZERO);
    push(
        &mut app,
        Order::Gather {
            units: vec![theirs],
            node,
            node_pos,
        }
        .issued_by(Faction::A),
    );
    step(&mut app);
    assert!(app.world().get::<GatherTarget>(theirs).is_none());
    assert!(app.world().get::<MoveTarget>(theirs).is_none());
}

/// `Order::Place` names the faction that gets the building; it must be the
/// faction that signed the order, or nothing is placed and nothing is charged.
#[test]
fn a_place_order_cannot_build_for_another_faction() {
    let mut app = sim_app_with_alloy(1_000);
    let foundry = app
        .world()
        .resource::<Content>()
        .building_index("foundry")
        .unwrap();
    push(
        &mut app,
        Order::Place {
            faction: Faction::B,
            building: foundry,
            pos: Vec2::ZERO,
        }
        .issued_by(Faction::A),
    );
    step(&mut app);
    let mut q = app.world_mut().query::<&Building>();
    assert_eq!(q.iter(app.world()).count(), 0, "a building was placed");
    assert_eq!(alloy(&app, Faction::B), 1_000);
    assert_eq!(alloy(&app, Faction::A), 1_000);
}

/// A signature cannot be laundered: an order signed by two different factions is
/// refused rather than resolved to either of them.
#[test]
fn a_doubly_signed_order_is_refused() {
    let mut app = sim_app_with_alloy(1_000);
    let hq = spawn_building(&mut app, "hq", Faction::B, Vec2::ZERO);
    let worker = unit_index(&app, "worker");
    push(
        &mut app,
        Order::Train {
            building: hq,
            unit: worker,
        }
        .issued_by(Faction::B)
        .issued_by(Faction::A),
    );
    step(&mut app);
    assert_eq!(queued(&app, hq), 0, "a re-signed order was executed");
    assert_eq!(alloy(&app, Faction::B), 1_000);
}

// ---- inherited 2: orders against dead entities -------------------------------

/// An AI issues orders against entities it remembered from an earlier tick; by
/// the time the order is applied the entity may be dead. `Commands::entity`
/// panics on a despawned entity — every order path must tolerate it.
#[test]
fn orders_against_a_despawned_entity_are_ignored_not_fatal() {
    let mut app = sim_app();
    let node_pos = Vec2::new(100.0, 0.0);
    let node = app
        .world_mut()
        .spawn((Position(node_pos), ResourceNode { amount: 500 }))
        .id();
    let ghost = spawn_unit(&mut app, "worker", Faction::A, Vec2::ZERO);
    let alive = spawn_unit(&mut app, "worker", Faction::A, Vec2::new(0.0, 10.0));
    app.world_mut().entity_mut(ghost).despawn();
    push(
        &mut app,
        Order::MoveTo {
            units: vec![ghost, alive],
            dest: Vec2::new(50.0, 0.0),
        }
        .issued_by(Faction::A),
    );
    push(
        &mut app,
        Order::Gather {
            units: vec![ghost],
            node,
            node_pos,
        }
        .issued_by(Faction::A),
    );
    let worker = unit_index(&app, "worker");
    push(
        &mut app,
        Order::Train {
            building: ghost,
            unit: worker,
        }
        .issued_by(Faction::A),
    );
    step(&mut app); // must not panic
    assert!(
        app.world().get::<MoveTarget>(alive).is_some(),
        "the live unit in the same order lost its command"
    );
}

// ---- inherited 3: the claim is a pair (F-008) --------------------------------

/// A lone `GatherTarget` disarms a unit forever (combat reads it alone, the
/// economy can only release the pair). The pairing must be structural: writing
/// the claim alone still yields a phase.
#[test]
fn a_gather_claim_can_never_be_written_without_its_phase() {
    let mut app = sim_app();
    let node = app
        .world_mut()
        .spawn((Position(Vec2::ZERO), ResourceNode { amount: 10 }))
        .id();
    let e = spawn_unit(&mut app, "ripper", Faction::A, Vec2::ZERO);
    app.world_mut().entity_mut(e).insert(GatherTarget(node));
    assert!(
        app.world().get::<GatherPhase>(e).is_some(),
        "a claim was written without its phase"
    );
}

/// And the release is a pair too: when the economy drops a job neither half is
/// left behind (here: a soldier that can never gather).
#[test]
fn a_gather_claim_is_always_released_as_a_pair() {
    let mut app = sim_app();
    let node = app
        .world_mut()
        .spawn((Position(Vec2::ZERO), ResourceNode { amount: 500 }))
        .id();
    let soldier = spawn_unit(&mut app, "ripper", Faction::A, Vec2::ZERO);
    app.world_mut()
        .entity_mut(soldier)
        .insert((GatherTarget(node), GatherPhase::ToNode));
    tick(&mut app, 3);
    assert!(app.world().get::<GatherTarget>(soldier).is_none());
    assert!(app.world().get::<GatherPhase>(soldier).is_none());
}

/// The compatibility rule, stated: an **unsigned** order is self-signed — the
/// queue attributes it at the boundary and the sim derives its issuer from what
/// it names, so a fixture that drives one side's economy directly keeps working.
/// (This is what keeps the ownership check from silently disowning `Faction::B`
/// fixtures; the *cross*-faction case is refused, two tests below.)
#[test]
fn an_unsigned_order_is_self_signed() {
    let mut app = sim_app_with_alloy(1_000);
    let hq = spawn_building(&mut app, "hq", Faction::B, Vec2::ZERO);
    let worker = unit_index(&app, "worker");
    let cost = app.world().resource::<Content>().units[worker].mvp_alloy_cost;
    let unit = spawn_unit(&mut app, "ripper", Faction::B, Vec2::new(300.0, 0.0));
    push(
        &mut app,
        Order::Train {
            building: hq,
            unit: worker,
        },
    );
    push(
        &mut app,
        Order::MoveTo {
            units: vec![unit],
            dest: Vec2::ZERO,
        },
    );
    step(&mut app);
    assert_eq!(queued(&app, hq), 1, "an unsigned order was disowned");
    assert_eq!(alloy(&app, Faction::B), 1_000 - cost, "the owner paid");
    assert!(app.world().get::<MoveTarget>(unit).is_some());
}

// ---- AC1: the scripted AI ---------------------------------------------------

/// A one-sided match fixture: an AI commander for `Faction::A` with an HQ, a
/// deposit and three workers, and a passive enemy HQ to aim at. Everything the
/// AI does from here is its own script.
fn ai_match(seed: u64) -> App {
    let mut app = sim_app_with_alloy(starting_alloy());
    let hq = spawn_building(&mut app, "hq", Faction::A, Vec2::ZERO);
    let _ = hq;
    spawn_building(&mut app, "hq", Faction::B, Vec2::new(1_500.0, 0.0));
    app.world_mut().spawn((
        Position(Vec2::new(250.0, 0.0)),
        ResourceNode { amount: 100_000 },
    ));
    for i in 0..3 {
        spawn_unit(
            &mut app,
            "worker",
            Faction::A,
            Vec2::new(0.0, 20.0 * i as f32),
        );
    }
    app.insert_resource(AiCommanders::new(seed, &[Faction::A]));
    app
}

fn starting_alloy() -> u32 {
    content().economy.starting_alloy
}

fn journal(app: &App, f: Faction) -> Vec<(u32, AiAction)> {
    app.world().resource::<AiJournal>().for_faction(f)
}

fn my_buildings(app: &mut App, faction: Faction, id: &str) -> usize {
    let def = app
        .world()
        .resource::<Content>()
        .building_index(id)
        .unwrap();
    let mut q = app.world_mut().query::<(&Building, &Faction)>();
    q.iter(app.world())
        .filter(|(b, f)| b.def == def && **f == faction)
        .count()
}

fn units_of(app: &mut App, faction: Faction) -> Vec<usize> {
    let mut q = app.world_mut().query::<(&UnitDefIdx, &Faction)>();
    let mut v: Vec<usize> = q
        .iter(app.world())
        .filter(|(_, f)| **f == faction)
        .map(|(d, _)| d.0)
        .collect();
    v.sort_unstable();
    v
}

/// AC1, first clause: the AI mines. Every idle worker it owns is put on the
/// deposit, and the claim it hands out is a *pair* it never loses (per-tick
/// invariant, so a one-tick flicker is a failure, not a rounding error).
#[test]
fn the_ai_puts_its_workers_on_a_deposit_and_banks_alloy() {
    let mut app = ai_match(11);
    let workers: Vec<Entity> = {
        let mut q = app.world_mut().query::<(Entity, &UnitDefIdx)>();
        q.iter(app.world()).map(|(e, _)| e).collect()
    };
    let interval = content().ai.think_interval_ticks;
    tick(&mut app, interval + 1);
    for w in &workers {
        assert!(
            app.world().get::<GatherTarget>(*w).is_some(),
            "a worker was left idle after the first decision"
        );
    }
    // And it stays a pair for the whole run.
    for t in 0..600 {
        step(&mut app);
        for w in &workers {
            let has_target = app.world().get::<GatherTarget>(*w).is_some();
            let has_phase = app.world().get::<GatherPhase>(*w).is_some();
            assert_eq!(has_target, has_phase, "tick {t}: half a claim");
        }
    }
    assert!(
        alloy(&app, Faction::A) > 0,
        "600 ticks of mining banked nothing"
    );
}

/// AC1, second clause: it opens with exactly one Barracks — the one the RON
/// names — and never before the tick the script allows.
#[test]
fn the_ai_builds_one_barracks_and_not_before_its_tick() {
    let script = content().ai;
    // The MVP opener opens exactly one barracks; `opening` is that one entry.
    assert_eq!(script.barracks.len(), 1, "the default strategy changed shape");
    let opening = &script.barracks[0];
    let mut app = ai_match(11);
    tick(&mut app, opening.at_tick);
    assert_eq!(
        my_buildings(&mut app, Faction::A, &opening.building),
        0,
        "the barracks went up before the script's tick"
    );
    tick(&mut app, 4_000);
    assert_eq!(
        my_buildings(&mut app, Faction::A, &opening.building),
        1,
        "the AI built no barracks, or more than one"
    );
    // The only randomness in the placement is its direction: it lands exactly
    // the opening's `offset` from the HQ.
    let placed = journal(&app, Faction::A)
        .into_iter()
        .find_map(|(_, a)| match a {
            AiAction::PlaceBarracks { pos, .. } => Some(pos),
            _ => None,
        })
        .expect("a barracks was placed");
    assert!(
        (placed.length() - opening.offset).abs() < 0.01,
        "the barracks is {} from the HQ, not {}",
        placed.length(),
        opening.offset
    );
}

/// AC1, third clause: it trains a **mixed** force, in the RON build order.
#[test]
fn the_ai_trains_the_ron_build_order_and_the_force_is_mixed() {
    let c = content();
    let mut app = ai_match(11);
    tick(&mut app, 12_000);
    let trained: Vec<usize> = journal(&app, Faction::A)
        .into_iter()
        .filter_map(|(_, a)| match a {
            AiAction::TrainArmy { unit } => Some(unit),
            _ => None,
        })
        .collect();
    assert!(
        trained.len() >= 4,
        "only {} army orders in 12k ticks",
        trained.len()
    );
    for (n, unit) in trained.iter().enumerate() {
        let want = c.ai.army_at(n as u32).and_then(|id| c.unit_index(id));
        assert_eq!(Some(*unit), want, "build order diverged at position {n}");
    }
    let force = units_of(&mut app, Faction::A);
    let kinds: std::collections::BTreeSet<usize> = force
        .iter()
        .copied()
        .filter(|u| c.units[*u].offense > 0)
        .collect();
    assert!(
        kinds.len() >= 2,
        "the force is not mixed: {kinds:?} (units {force:?})"
    );
}

/// AC1, fourth clause: it attack-moves at the enemy HQ — the force is given the
/// destination and actually closes on it.
#[test]
fn the_ai_attack_moves_its_force_at_the_enemy_hq() {
    let mut app = ai_match(11);
    let mut attack = None;
    for _ in 0..12_000 {
        step(&mut app);
        if let Some(a) = journal(&app, Faction::A)
            .into_iter()
            .find_map(|(t, a)| match a {
                AiAction::Attack { dest, force } => Some((t, dest, force)),
                _ => None,
            })
        {
            attack = Some(a);
            break;
        }
    }
    let (_, dest, force) = attack.expect("the AI never attacked in 12k ticks");
    assert!(force >= content().ai.attack_at_army);
    assert!(
        dest.distance(Vec2::new(1_500.0, 0.0)) <= content().ai.attack_spread * 1.5,
        "the wave was aimed at {dest}, not at the enemy HQ"
    );
    // It closes: the army's distance to the enemy HQ falls.
    let army_distance = |app: &mut App| -> f32 {
        let c = content();
        let mut q = app.world_mut().query::<(&Position, &UnitDefIdx, &Faction)>();
        let ds: Vec<f32> = q
            .iter(app.world())
            .filter(|(_, d, f)| **f == Faction::A && c.units[d.0].offense > 0)
            .map(|(p, _, _)| p.0.distance(Vec2::new(1_500.0, 0.0)))
            .collect();
        ds.iter().copied().fold(f32::INFINITY, f32::min)
    };
    let before = army_distance(&mut app);
    tick(&mut app, 600);
    let after = army_distance(&mut app);
    assert!(
        after < before,
        "the force did not close on the enemy HQ ({before} -> {after})"
    );
}

/// "On a timer": every decision lands on a multiple of `think_interval_ticks`,
/// counted in **ticks** — there is no other clock in the AI.
#[test]
fn the_ai_acts_only_on_its_think_interval() {
    let interval = content().ai.think_interval_ticks;
    let mut app = ai_match(11);
    tick(&mut app, 6_000);
    let acts = journal(&app, Faction::A);
    assert!(!acts.is_empty());
    for (t, a) in acts {
        assert_eq!(t % interval, 0, "decision {a:?} taken off-timer at tick {t}");
    }
}

/// The AC1 determinism probe: same seed ⇒ the same actions, tick for tick. The
/// comparison is on the *decisions* (the journal), not merely on the state they
/// happen to produce.
#[test]
fn the_ai_is_deterministic_given_the_seed() {
    let run = |seed: u64| {
        let mut app = ai_match(seed);
        tick(&mut app, 6_000);
        let acts = journal(&app, Faction::A);
        let stock = alloy(&app, Faction::A);
        let force = units_of(&mut app, Faction::A);
        (acts, stock, force)
    };
    let a = run(11);
    let b = run(11);
    assert_eq!(a.0, b.0, "same seed, different actions");
    assert_eq!((a.1, a.2), (b.1, b.2), "same seed, different outcome");

    // A different seed moves what the seed actually decides: the barracks spoke.
    let c = run(12);
    let spoke = |acts: &[(u32, AiAction)]| {
        acts.iter().find_map(|(_, a)| match a {
            AiAction::PlaceBarracks { pos, .. } => Some(*pos),
            _ => None,
        })
    };
    assert_ne!(
        spoke(&a.0),
        spoke(&c.0),
        "the seed does not reach the AI's choices"
    );
}

/// The AI is a commander, not a cheat: it never touches the other side's units,
/// buildings or Alloy — it can only issue orders, and its orders are signed.
#[test]
fn the_ai_commands_only_its_own_side() {
    let mut app = ai_match(11);
    let theirs = spawn_unit(&mut app, "worker", Faction::B, Vec2::new(40.0, 0.0));
    let start_b = alloy(&app, Faction::B);
    for t in 0..3_000 {
        step(&mut app);
        assert!(
            app.world().get::<GatherTarget>(theirs).is_none(),
            "tick {t}: the AI put the enemy's worker to work"
        );
        assert_eq!(
            alloy(&app, Faction::B),
            start_b,
            "tick {t}: the AI moved the enemy's Alloy"
        );
    }
    assert_eq!(
        my_buildings(&mut app, Faction::B, &content().ai.barracks[0].building),
        0,
        "the AI built in the enemy's name"
    );
}

/// No wall-clock and no hash-map iteration anywhere in the sim — the two ways a
/// deterministic AI stops being deterministic. Source-level, so it also catches
/// the next module.
#[test]
fn the_sim_reads_no_clock_and_iterates_no_hash_map() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sim");
    let mut offenders = Vec::new();
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("read src/sim")
        .map(|e| e.expect("entry").path())
        .filter(|p| p.extension().is_some_and(|e| e == "rs"))
        .collect();
    files.sort();
    for path in files {
        let text = std::fs::read_to_string(&path).expect("read source");
        for needle in [
            "Instant",
            "SystemTime",
            "Time<Real>",
            "elapsed(",
            "HashMap",
            "HashSet",
        ] {
            // Comments say the words; code is what matters, so only count lines
            // that are not comments.
            for line in text.lines() {
                let code = line.split("//").next().unwrap_or("");
                if code.contains(needle) {
                    offenders.push(format!("{}: {}", path.display(), line.trim()));
                }
            }
        }
    }
    assert!(offenders.is_empty(), "nondeterminism in the sim: {offenders:?}");
}

// ---- AC1: the script is content, and bad content is refused at load ---------

/// Write a mutated copy of the content pair and try to load it.
fn load_mutated(name: &str, from: &str, to: &str) -> Result<Content, String> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/m4c_content")
        .join(name);
    std::fs::create_dir_all(&dir).unwrap();
    // The edit lands in whichever shipped file states the anchor — the unit
    // stats in `units.ron`, the AI script in `strategies.ron` (B1).
    let mut hit = 0;
    for file in ["units.ron", "resources.ron", "strategies.ron"] {
        let text = std::fs::read_to_string(data_dir().join(file)).unwrap();
        let edited = if text.contains(from) {
            hit += 1;
            text.replace(from, to)
        } else {
            text
        };
        std::fs::write(dir.join(file), edited).unwrap();
    }
    assert_eq!(hit, 1, "anchor `{from}` is in {hit} shipped files, not exactly 1");
    Content::load_from_dir(&dir).map_err(|e| e.to_string())
}

/// The script has no silent defaults: an AI script the sim cannot run is an
/// `Err` at load, in **both** profiles (a check written in the arithmetic it is
/// checking would pass in debug by panicking and pass content in release —
/// F-005). Each case below is a way the AI would otherwise fail silently: a
/// commander that thinks every tick, one whose barracks cannot make what its
/// build order asks for, one with no build order at all, and counts that wrap.
#[test]
fn an_unrunnable_ai_script_is_refused_at_load() {
    for (name, from, to) in [
        ("think0", "think_interval_ticks: 30", "think_interval_ticks: 0"),
        ("wave0", "attack_interval_ticks: 600", "attack_interval_ticks: 0"),
        ("workers0", "worker_target: 6", "worker_target: 0"),
        ("army0", "attack_at_army: 3", "attack_at_army: 0"),
        ("nobarracks", "(building: \"foundry\", at_tick", "(building: \"nonesuch\", at_tick"),
        ("hqbarracks", "(building: \"foundry\", at_tick", "(building: \"hq\", at_tick"),
        (
            "wrongunit",
            "(unit: \"sentinel\", count: 2)",
            "(unit: \"ripper\", count: 2)",
        ),
        (
            "unknownunit",
            "(unit: \"sentinel\", count: 2)",
            "(unit: \"nonesuch\", count: 2)",
        ),
        (
            "count0",
            "(unit: \"sentinel\", count: 2)",
            "(unit: \"sentinel\", count: 0)",
        ),
        (
            "overflow",
            "(unit: \"sentinel\", count: 2)",
            "(unit: \"sentinel\", count: 4294967295)",
        ),
        ("offset", "offset: 130.0", "offset: 0.0"),
        ("spread", "attack_spread: 60.0", "attack_spread: -1.0"),
    ] {
        let got = load_mutated(name, from, to);
        assert!(
            got.is_err(),
            "`{name}` ({from} -> {to}) loaded, but the sim cannot run it"
        );
    }
}

/// Exactly one building is the victory target — zero makes the match
/// unwinnable, two make "the enemy HQ" ambiguous.
#[test]
fn the_victory_target_must_be_exactly_one_building() {
    assert!(load_mutated("novictory", "dropoff: true, victory: true", "dropoff: true").is_err());
    assert!(load_mutated(
        "twovictory",
        "alloy_cost: 150, produces: [\"bulwark\"",
        "alloy_cost: 150, victory: true, produces: [\"bulwark\""
    )
    .is_err());
}

/// The script really is data: change the RON and the commander's behaviour
/// changes with it — no Rust constant shadows it.
#[test]
fn the_ai_script_comes_from_the_ron_not_from_constants() {
    let edited = load_mutated(
        "slow",
        "think_interval_ticks: 30",
        "think_interval_ticks: 97",
    )
    .expect("still valid content");
    assert_eq!(edited.ai.think_interval_ticks, 97);
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .insert_resource(Time::<Fixed>::from_hz(60.0))
        .insert_resource(edited)
        .init_resource::<CommandQueue>()
        .init_resource::<RateReport>()
        .init_resource::<Casualties>()
        .insert_resource(Stockpiles::starting(starting_alloy()));
    onus::add_sim_systems(&mut app, Update);
    spawn_building(&mut app, "hq", Faction::A, Vec2::ZERO);
    app.world_mut().spawn((
        Position(Vec2::new(250.0, 0.0)),
        ResourceNode { amount: 100_000 },
    ));
    spawn_unit(&mut app, "worker", Faction::A, Vec2::ZERO);
    app.insert_resource(AiCommanders::new(3, &[Faction::A]));
    tick(&mut app, 1_000);
    let acts = journal(&app, Faction::A);
    assert!(!acts.is_empty(), "the edited script never acted");
    for (t, a) in acts {
        assert_eq!(t % 97, 0, "decision {a:?} at tick {t} ignores the edited RON");
    }
}

// ---- AC2: destroy the enemy HQ, and the match terminates --------------------

fn outcome(app: &App) -> Option<MatchOutcome> {
    app.world().resource::<MatchState>().outcome()
}

/// Buildings are killable, on the same 1-10 design scale as units: the pool is
/// `mvp_defense * mvp_combat.building_hp_per_defense` and armor is flat
/// mitigation per hit. Without this there is no win condition to check.
#[test]
fn a_building_takes_damage_from_the_ron_and_falls() {
    let c = content();
    let hq_def = c.building_index("hq").unwrap();
    let pool = c.buildings[hq_def].mvp_defense * c.combat.building_hp_per_defense;
    let per_hit = c.units[c.unit_index("ripper").unwrap()].offense * c.combat.damage_per_offense
        - c.buildings[hq_def].mvp_armor * c.combat.mitigation_per_armor;

    let mut app = sim_app();
    let hq = spawn_building(&mut app, "hq", Faction::B, Vec2::ZERO);
    spawn_building(&mut app, "hq", Faction::A, Vec2::new(2_000.0, 0.0));
    let attacker = spawn_unit(&mut app, "ripper", Faction::A, Vec2::new(30.0, 0.0));
    let _ = attacker;
    step(&mut app);
    assert_eq!(
        app.world().get::<Health>(hq).map(|h| (h.current, h.max)),
        Some((pool - per_hit, pool)),
        "a building's pool/mitigation do not match the RON"
    );
    // It dies, exactly once, after the right number of hits.
    let cadence = c.units[c.unit_index("ripper").unwrap()].mvp_attack_ticks;
    let hits_needed = pool.div_ceil(per_hit);
    tick(&mut app, cadence * hits_needed);
    assert!(
        app.world().get_entity(hq).is_err(),
        "the HQ survived {hits_needed} hits of {per_hit} into a {pool} pool"
    );
    // A razed building is not a unit casualty.
    assert_eq!(app.world().resource::<Casualties>().lost(Faction::B), 0);
}

/// A building never *attacks*: it is a target only.
#[test]
fn a_building_never_attacks() {
    let mut app = sim_app();
    spawn_building(&mut app, "hq", Faction::A, Vec2::ZERO);
    spawn_building(&mut app, "hq", Faction::B, Vec2::new(2_000.0, 0.0));
    let victim = spawn_unit(&mut app, "worker", Faction::B, Vec2::new(20.0, 0.0));
    let full = app.world().get::<Health>(victim).unwrap().current;
    tick(&mut app, 600);
    assert_eq!(
        app.world().get::<Health>(victim).map(|h| h.current),
        Some(full),
        "a building shot at a unit standing next to it"
    );
}

/// AC2: destroying the enemy HQ ends the match — with the right winner, on the
/// tick it happened, and the sim stops.
#[test]
fn destroying_the_enemy_hq_ends_the_match() {
    let mut app = sim_app();
    let enemy_hq = spawn_building(&mut app, "hq", Faction::B, Vec2::ZERO);
    spawn_building(&mut app, "hq", Faction::A, Vec2::new(3_000.0, 0.0));
    spawn_unit(&mut app, "ripper", Faction::A, Vec2::new(30.0, 0.0));
    let mut decided = None;
    for t in 0..3_000u32 {
        step(&mut app);
        let hq_alive = app.world().get_entity(enemy_hq).is_ok();
        match outcome(&app) {
            None => assert!(hq_alive, "tick {t}: the HQ fell and the match ran on"),
            Some(o) => {
                assert!(!hq_alive);
                decided = Some((t, o));
                break;
            }
        }
    }
    let (t, o) = decided.expect("the match never ended");
    assert_eq!(o.winner, Some(Faction::A), "the wrong side won");
    assert_eq!(o.tick, t, "the outcome is tagged with the wrong tick");
}

/// ...and after it ends, nothing changes: the outcome is written exactly once,
/// and the rest of the sim is off (per-tick, so a single stray tick is caught).
#[test]
fn nothing_runs_after_the_match_is_decided() {
    let mut app = sim_app_with_alloy(1_000);
    spawn_building(&mut app, "hq", Faction::B, Vec2::ZERO);
    let my_hq = spawn_building(&mut app, "hq", Faction::A, Vec2::new(3_000.0, 0.0));
    let killer = spawn_unit(&mut app, "ripper", Faction::A, Vec2::new(30.0, 0.0));
    // A worker walking somewhere, so "the sim froze" is observable.
    let walker = spawn_unit(&mut app, "worker", Faction::A, Vec2::new(3_000.0, 0.0));
    let worker = unit_index(&app, "worker");
    push(
        &mut app,
        Order::MoveTo {
            units: vec![walker],
            dest: Vec2::new(3_000.0, 5_000.0),
        }
        .issued_by(Faction::A),
    );
    for _ in 0..3_000 {
        step(&mut app);
        if outcome(&app).is_some() {
            break;
        }
    }
    let decided = outcome(&app).expect("the match never ended");
    let frozen_pos = app.world().get::<Position>(walker).map(|p| p.0);
    let frozen_hp = app.world().get::<Health>(killer).map(|h| h.current);
    let frozen_tick = app.world().resource::<MatchState>().tick();
    // Try to keep playing: an order after the end must do nothing at all.
    push(
        &mut app,
        Order::Train {
            building: my_hq,
            unit: worker,
        }
        .issued_by(Faction::A),
    );
    for t in 0..300 {
        step(&mut app);
        assert_eq!(outcome(&app), Some(decided), "tick {t}: the outcome moved");
        assert_eq!(
            app.world().resource::<MatchState>().tick(),
            frozen_tick,
            "tick {t}: the sim clock ran on after the match"
        );
        assert_eq!(
            app.world().get::<Position>(walker).map(|p| p.0),
            frozen_pos,
            "tick {t}: a unit moved after the match ended"
        );
        assert_eq!(
            app.world().get::<Health>(killer).map(|h| h.current),
            frozen_hp
        );
        assert_eq!(alloy(&app, Faction::A), 1_000, "Alloy was spent after the end");
        assert_eq!(queued(&app, my_hq), 0, "production ran after the end");
    }
}

/// A one-sided fixture (only one faction has an HQ — every M4a/M4b test) is not
/// a decided match: the sim must keep running.
#[test]
fn a_match_with_only_one_hq_never_ends() {
    let mut app = sim_app();
    spawn_building(&mut app, "hq", Faction::A, Vec2::ZERO);
    tick(&mut app, 500);
    assert!(outcome(&app).is_none(), "a solitary HQ won a match by itself");
    assert!(!app.world().resource::<MatchState>().engaged());
}

/// Mutual destruction on the same tick is a draw, and the check does not depend
/// on which HQ the query happens to see first: spawning them in either order
/// gives the same answer.
#[test]
fn losing_both_hqs_on_one_tick_is_a_draw_in_either_order() {
    let run = |a_first: bool| {
        let mut app = sim_app();
        // Two HQs, each with an enemy Ripper the same distance away, so the two
        // kills land on the same tick.
        let (p0, p1) = (Vec2::ZERO, Vec2::new(4_000.0, 0.0));
        if a_first {
            spawn_building(&mut app, "hq", Faction::A, p0);
            spawn_building(&mut app, "hq", Faction::B, p1);
        } else {
            spawn_building(&mut app, "hq", Faction::B, p1);
            spawn_building(&mut app, "hq", Faction::A, p0);
        }
        spawn_unit(&mut app, "ripper", Faction::B, p0 + Vec2::new(30.0, 0.0));
        spawn_unit(&mut app, "ripper", Faction::A, p1 + Vec2::new(30.0, 0.0));
        for _ in 0..3_000 {
            step(&mut app);
            if outcome(&app).is_some() {
                break;
            }
        }
        outcome(&app).expect("no decision")
    };
    let first = run(true);
    assert_eq!(first.winner, None, "simultaneous loss is a draw");
    assert_eq!(first, run(false), "the end check depends on spawn order");
}

// ---- AC2: a full AI-vs-AI match, and how long it takes ----------------------

/// A symmetric 1v1: two scripted commanders, mirrored bases and deposits.
fn ai_vs_ai(seed: u64) -> App {
    let mut app = sim_app_with_alloy(starting_alloy());
    for (faction, base) in [
        (Faction::A, Vec2::new(-750.0, 0.0)),
        (Faction::B, Vec2::new(750.0, 0.0)),
    ] {
        spawn_building(&mut app, "hq", faction, base);
        app.world_mut().spawn((
            Position(base + Vec2::new(0.0, 250.0)),
            ResourceNode { amount: 100_000 },
        ));
        for i in 0..3 {
            spawn_unit(&mut app, "worker", faction, base + Vec2::new(0.0, 20.0 * i as f32));
        }
    }
    app.insert_resource(AiCommanders::new(seed, &[Faction::A, Faction::B]));
    app
}

/// AC2's design target: an AI-vs-AI match reaches a decision inside ~8 minutes
/// of sim time (8 * 60 * 60 = 28_800 ticks). Measured, not hardcoded — the
/// budget is the assertion, the length is reported.
#[test]
fn an_ai_vs_ai_match_is_decided_within_eight_minutes() {
    const BUDGET: u32 = 8 * 60 * 60;
    let mut lengths = Vec::new();
    for seed in [1u64, 7, 99] {
        let mut app = ai_vs_ai(seed);
        let mut decided = None;
        for _ in 0..BUDGET {
            step(&mut app);
            if let Some(o) = outcome(&app) {
                decided = Some(o);
                break;
            }
        }
        let o = decided.unwrap_or_else(|| {
            panic!("seed {seed}: no decision in {BUDGET} ticks (~8 min)");
        });
        lengths.push((seed, o.tick, o.winner));
    }
    println!("match lengths (seed, tick, winner): {lengths:?}");
    for (seed, ticks, _) in &lengths {
        assert!(
            *ticks <= BUDGET,
            "seed {seed}: {ticks} ticks is over the ~8 min target"
        );
    }
}

/// The whole match is deterministic given the seed: same seed ⇒ same journal,
/// same winner, same tick. (A different seed is allowed to differ; what must not
/// differ is a rerun.)
#[test]
fn an_ai_vs_ai_match_replays_identically_from_its_seed() {
    let run = |seed: u64| {
        let mut app = ai_vs_ai(seed);
        for _ in 0..8 * 60 * 60 {
            step(&mut app);
            if outcome(&app).is_some() {
                break;
            }
        }
        (
            outcome(&app),
            app.world().resource::<AiJournal>().0.len(),
            journal(&app, Faction::A),
            journal(&app, Faction::B),
            app.world().resource::<Casualties>().total(),
        )
    };
    let a = run(7);
    assert!(a.0.is_some(), "the match did not finish");
    assert_eq!(a, run(7), "the same seed produced a different match");
}

// ---- critic pass 1: when a loss lands must not decide *whether* it counts ---

/// The property the fix establishes: a match is contested from its *starting*
/// configuration, so an HQ destroyed on the very first tick ends the match like
/// any other. (Latching "both sides have an HQ" from what was left standing at
/// the end of a tick made the first tick undecidable.)
#[test]
fn an_hq_lost_on_the_first_tick_decides_the_match() {
    let mut app = sim_app();
    spawn_building(&mut app, "hq", Faction::A, Vec2::new(-300.0, 0.0));
    let doomed = spawn_building(&mut app, "hq", Faction::B, Vec2::ZERO);
    app.world_mut()
        .entity_mut(doomed)
        .insert(Health { current: 1, max: 400 });
    spawn_unit(&mut app, "arclight", Faction::A, Vec2::new(-60.0, 0.0));
    step(&mut app);
    assert!(
        app.world().get_entity(doomed).is_err(),
        "fixture: the HQ was supposed to fall on tick 0"
    );
    assert_eq!(
        outcome(&app).map(|o| (o.winner, o.tick)),
        Some((Some(Faction::A), 0)),
        "an HQ lost on tick 0 left the match undecidable"
    );
}

/// Differential form: the same loss, at every delay, decides the same way — only
/// the tick it is recorded on moves.
#[test]
fn the_decision_does_not_depend_on_which_tick_the_loss_lands_on() {
    let decide = |delay: u32| {
        let mut app = sim_app();
        spawn_building(&mut app, "hq", Faction::A, Vec2::new(-300.0, 0.0));
        let doomed = spawn_building(&mut app, "hq", Faction::B, Vec2::ZERO);
        app.world_mut()
            .entity_mut(doomed)
            .insert(Health { current: 1, max: 400 });
        tick(&mut app, delay);
        spawn_unit(&mut app, "arclight", Faction::A, Vec2::new(-60.0, 0.0));
        tick(&mut app, 300);
        outcome(&app).map(|o| o.winner)
    };
    for delay in [0, 1, 2, 7, 50] {
        assert_eq!(
            decide(delay),
            Some(Some(Faction::A)),
            "the loss stopped counting at delay {delay}"
        );
    }
}

/// The property the fix could break: a one-sided fixture (M1-M4b's whole
/// suite) must still never terminate — asserted **per tick**, not at the end.
#[test]
fn a_one_sided_fixture_never_terminates_on_any_tick() {
    let mut app = sim_app();
    spawn_building(&mut app, "hq", Faction::A, Vec2::ZERO);
    let walker = spawn_unit(&mut app, "worker", Faction::A, Vec2::ZERO);
    push(
        &mut app,
        Order::MoveTo {
            units: vec![walker],
            dest: Vec2::new(0.0, 10_000.0),
        }
        .issued_by(Faction::A),
    );
    let mut moved = 0.0f32;
    for t in 0..600 {
        let before = app.world().get::<Position>(walker).map(|p| p.0).unwrap();
        step(&mut app);
        let after = app.world().get::<Position>(walker).map(|p| p.0).unwrap();
        moved += after.distance(before);
        assert!(
            outcome(&app).is_none(),
            "tick {t}: a solitary HQ decided a match"
        );
        assert!(!app.world().resource::<MatchState>().engaged());
    }
    assert!(moved > 0.0, "the sim was frozen, so 'still running' proves nothing");
}

/// And the other side of that: a second HQ arriving late makes the match
/// contested from then on — the latch is not a one-shot chance at tick 0.
#[test]
fn a_second_hq_arriving_late_makes_the_match_decidable() {
    let mut app = sim_app();
    spawn_building(&mut app, "hq", Faction::A, Vec2::new(-300.0, 0.0));
    tick(&mut app, 100);
    assert!(!app.world().resource::<MatchState>().engaged());
    let doomed = spawn_building(&mut app, "hq", Faction::B, Vec2::ZERO);
    app.world_mut()
        .entity_mut(doomed)
        .insert(Health { current: 1, max: 400 });
    spawn_unit(&mut app, "arclight", Faction::A, Vec2::new(-60.0, 0.0));
    tick(&mut app, 300);
    assert_eq!(outcome(&app).map(|o| o.winner), Some(Some(Faction::A)));
}

// ---- critic pass 1: a split gather claim cannot survive a tick --------------

/// Every entity in the world holds either both halves of the economy's claim or
/// neither. The invariant readers depend on, checked directly.
fn split_claims(app: &mut App) -> Vec<Entity> {
    let mut q = app
        .world_mut()
        .query::<(Entity, Option<&GatherTarget>, Option<&GatherPhase>)>();
    q.iter(app.world())
        .filter(|(_, t, p)| t.is_some() != p.is_some())
        .map(|(e, _, _)| e)
        .collect()
}

/// The property the fix establishes: `#[require]` only covers *insertion*, so a
/// bare `remove::<GatherPhase>()` used to leave a lone `GatherTarget` — the
/// economy could never see the unit again (its query needs both halves) and
/// combat still refused to let it fight. The economy now sweeps split claims
/// before anything reads them, so the unit is released and **fights again**.
#[test]
fn a_split_gather_claim_is_swept_and_the_unit_re_arms() {
    let mut app = sim_app();
    let node = app
        .world_mut()
        .spawn((Position(Vec2::new(30.0, 0.0)), ResourceNode { amount: 500 }))
        .id();
    // An armed gatherer, so "disarmed forever" is observable: give the worker
    // teeth via content, not by planting components.
    let soldier = spawn_unit(&mut app, "ripper", Faction::A, Vec2::ZERO);
    let enemy = spawn_unit(&mut app, "worker", Faction::B, Vec2::new(20.0, 0.0));
    // Plant a complete claim by hand (the shape F-008 names), then split it.
    app.world_mut()
        .entity_mut(soldier)
        .insert((GatherTarget(node), GatherPhase::ToNode));
    app.world_mut().entity_mut(soldier).remove::<GatherPhase>();
    assert_eq!(split_claims(&mut app), vec![soldier], "fixture: not split");

    let full = app.world().get::<Health>(enemy).unwrap().current;
    step(&mut app);
    assert!(
        split_claims(&mut app).is_empty(),
        "a split claim survived a whole tick"
    );
    assert!(
        app.world().get::<GatherTarget>(soldier).is_none(),
        "the lone half of the claim was left on the unit"
    );
    tick(&mut app, 120);
    assert!(
        !app.world().get_entity(enemy).is_ok() || app.world().get::<Health>(enemy).unwrap().current < full,
        "the unit was still disarmed by a claim the economy could not see"
    );
}

/// The mirror image: a lone `GatherPhase` is a phase for a job that no longer
/// exists, and is swept the same way.
#[test]
fn a_lone_gather_phase_is_swept_too() {
    let mut app = sim_app();
    let w = spawn_unit(&mut app, "worker", Faction::A, Vec2::ZERO);
    app.world_mut().entity_mut(w).insert(GatherPhase::ToNode);
    step(&mut app);
    assert!(app.world().get::<GatherPhase>(w).is_none());
    assert!(split_claims(&mut app).is_empty());
}

/// The property the sweep could break: a **healthy** gatherer's job is never
/// confiscated, and no tick of a full mine → return → bank loop ever shows a
/// split claim. Per tick, so a one-tick flicker fails here.
#[test]
fn the_sweep_never_confiscates_a_real_gather_job() {
    let mut app = sim_app();
    let node_pos = Vec2::new(200.0, 0.0);
    let node = app
        .world_mut()
        .spawn((Position(node_pos), ResourceNode { amount: 1_000 }))
        .id();
    spawn_building(&mut app, "hq", Faction::A, Vec2::ZERO);
    let w = spawn_unit(&mut app, "worker", Faction::A, Vec2::ZERO);
    push(
        &mut app,
        Order::Gather {
            units: vec![w],
            node,
            node_pos,
        }
        .issued_by(Faction::A),
    );
    for t in 0..1_200 {
        step(&mut app);
        assert!(
            split_claims(&mut app).is_empty(),
            "tick {t}: the claim was split mid-loop"
        );
        assert!(
            app.world().get::<GatherTarget>(w).is_some(),
            "tick {t}: a real gatherer's job was confiscated"
        );
    }
    assert!(
        alloy(&app, Faction::A) > 0,
        "the worker kept its job but never banked: the loop is dead"
    );
}

// ---- critic pass 1: "self-signed" has to be a property, not a label --------

/// The property the fix establishes. "Self-signed" used to mean "unchecked":
/// `commandable` returned `true` for every entity of an unsigned order, so one
/// `MoveTo` naming a unit of each faction commanded **both** sides at once. An
/// unsigned order now derives its issuer from what it names, and an order that
/// names two factions has no issuer it could have come from — so it is refused
/// *whole*, not half-applied.
#[test]
fn an_unsigned_order_naming_two_factions_is_refused_whole() {
    let mut app = sim_app();
    let mine = spawn_unit(&mut app, "ripper", Faction::A, Vec2::ZERO);
    let theirs = spawn_unit(&mut app, "ripper", Faction::B, Vec2::new(1_000.0, 0.0));
    let node_pos = Vec2::new(-500.0, 0.0);
    let node = app
        .world_mut()
        .spawn((Position(node_pos), ResourceNode { amount: 500 }))
        .id();
    // Both entity-list orders, unsigned, naming both sides.
    push(
        &mut app,
        Order::MoveTo {
            units: vec![mine, theirs],
            dest: Vec2::new(500.0, 500.0),
        },
    );
    push(
        &mut app,
        Order::Gather {
            units: vec![theirs, mine],
            node,
            node_pos,
        },
    );
    step(&mut app);
    for (who, e) in [("A", mine), ("B", theirs)] {
        assert!(
            app.world().get::<MoveTarget>(e).is_none(),
            "{who}'s unit was commanded by an order that named both factions"
        );
        assert!(app.world().get::<GatherTarget>(e).is_none());
    }
    // Order of the list must not matter either: the refusal is not "the first
    // one wins".
    push(
        &mut app,
        Order::MoveTo {
            units: vec![theirs, mine],
            dest: Vec2::new(500.0, 500.0),
        },
    );
    step(&mut app);
    assert!(app.world().get::<MoveTarget>(mine).is_none());
    assert!(app.world().get::<MoveTarget>(theirs).is_none());
}

/// A *signed* order naming both factions is not refused whole — it commands the
/// signer's own units and simply ignores the enemy's. (The two rules differ on
/// purpose: a signed order says who it is from, so the foreign entries are
/// noise; an unsigned one does not, so there is nothing to trust.)
#[test]
fn a_signed_order_naming_two_factions_still_commands_its_own() {
    let mut app = sim_app();
    let mine = spawn_unit(&mut app, "ripper", Faction::A, Vec2::ZERO);
    let theirs = spawn_unit(&mut app, "ripper", Faction::B, Vec2::new(1_000.0, 0.0));
    push(
        &mut app,
        Order::MoveTo {
            units: vec![mine, theirs],
            dest: Vec2::new(500.0, 500.0),
        }
        .issued_by(Faction::A),
    );
    step(&mut app);
    assert!(app.world().get::<MoveTarget>(mine).is_some());
    assert!(app.world().get::<MoveTarget>(theirs).is_none());
}

/// The command queue can only hold *attributed* orders: pushing a bare `Order`
/// attributes it at the boundary, signing gives `By`, and a stack of signatures
/// that disagree is `Void` — which the sim drops.
#[test]
fn the_queue_can_only_hold_attributed_orders() {
    let mut app = sim_app();
    let e = spawn_unit(&mut app, "ripper", Faction::A, Vec2::ZERO);
    let mk = |issuer: Option<Faction>| Order::MoveTo {
        units: vec![e],
        dest: Vec2::new(issuer.map_or(1.0, |_| 2.0), 0.0),
    };
    let mut q = app.world_mut().resource_mut::<CommandQueue>();
    q.0.push_back(mk(None));
    q.0.push_back(mk(Some(Faction::A)).issued_by(Faction::A));
    q.0.push_back(mk(None).issued_by(Faction::A).issued_by(Faction::B));
    let got: Vec<Attribution> = q.0.iter().map(|o| o.attribution()).collect();
    assert_eq!(
        got,
        vec![
            Attribution::SelfSigned,
            Attribution::By(Faction::A),
            Attribution::Void,
        ]
    );
}

/// The property the fix could break, end to end and **per tick**: every
/// legitimate same-faction order still works. A full AI match exercises all four
/// order variants — gather, train (worker), place (barracks), attack-move — so
/// if coherent self-signing were too strict, the AI would stop functioning: no
/// mining, no barracks, no army, no attack.
#[test]
fn every_legitimate_order_still_applies_under_coherent_self_signing() {
    let mut app = ai_match(11);
    // A player-signed order in the same match, re-issued every tick: the sim
    // must keep obeying it while the AI plays.
    let mut saw = (false, false, false, false);
    for t in 0..6_000u32 {
        step(&mut app);
        for (_, a) in journal(&app, Faction::A) {
            match a {
                AiAction::Gather { .. } => saw.0 = true,
                AiAction::TrainWorker { .. } => saw.1 = true,
                AiAction::PlaceBarracks { .. } => saw.2 = true,
                AiAction::TrainArmy { .. } => saw.3 = true,
                AiAction::Attack { .. } => {}
            }
        }
        // Per tick: the AI's own units are never disowned by the ownership
        // check — a worker on a job keeps it, and Alloy keeps flowing.
        assert!(
            app.world().resource::<Stockpiles>().alloy(Faction::A) < u32::MAX / 2,
            "tick {t}: the economy went backwards"
        );
    }
    assert_eq!(
        saw,
        (true, true, true, true),
        "an order variant stopped applying (gather, train worker, place, train army)"
    );
    assert!(
        my_buildings(&mut app, Faction::A, &content().ai.barracks[0].building) == 1,
        "the barracks order stopped applying"
    );
    assert!(
        alloy(&app, Faction::A) > 0 || !units_of(&mut app, Faction::A).is_empty(),
        "the match stopped functioning entirely"
    );
}

// ---- critic pass 2: the sweep must precede *every* reader of the claim ------

/// The property the fix establishes. The sweep is worth its position in the
/// schedule and nothing more: it used to sit after `ai_commanders`, which reads
/// `GatherTarget` to decide who is idle, so a lone target was still read as a
/// live job by that one reader and the worker sat unemployed for a whole
/// `think_interval_ticks`. Sweeping first means no reader sees a half-claim.
#[test]
fn the_ai_never_reads_a_half_claim_as_a_job() {
    let mut app = sim_app();
    spawn_building(&mut app, "hq", Faction::A, Vec2::ZERO);
    let node = app
        .world_mut()
        .spawn((
            Position(Vec2::new(200.0, 0.0)),
            ResourceNode { amount: 10_000 },
        ))
        .id();
    let split = spawn_unit(&mut app, "worker", Faction::A, Vec2::new(0.0, 20.0));
    app.world_mut()
        .entity_mut(split)
        .insert((GatherTarget(node), GatherPhase::ToNode));
    app.world_mut().entity_mut(split).remove::<GatherPhase>();
    app.insert_resource(AiCommanders::new(1, &[Faction::A]));

    // One tick: swept, then seen as idle, then re-tasked — all before the tick
    // ends, because the sweep is upstream of the commander.
    step(&mut app);
    assert!(
        journal(&app, Faction::A)
            .iter()
            .any(|(_, a)| matches!(a, AiAction::Gather { unit, .. } if *unit == split)),
        "the commander read a lone GatherTarget as a job and skipped the worker"
    );
    assert!(
        app.world().get::<GatherTarget>(split).is_some()
            && app.world().get::<GatherPhase>(split).is_some(),
        "the worker ended the tick with anything other than a whole claim"
    );
}

/// The invariant behind that, asserted where it actually has to hold: **at the
/// point each reader runs**, not merely at the tick boundary. `repair` is the
/// first system that plays the match, so a claim planted split between ticks is
/// already whole-or-gone by the time anything reads it — checked per tick over a
/// live match with an AI, a gather loop and combat all running.
#[test]
fn no_reader_ever_observes_a_half_claim_during_a_live_match() {
    let mut app = ai_match(11);
    let enemy = spawn_unit(&mut app, "ripper", Faction::B, Vec2::new(120.0, 0.0));
    let _ = enemy;
    for t in 0..900u32 {
        // Plant the F-008 shape between ticks, on whichever worker is on a job.
        let victim: Option<Entity> = {
            let mut q = app.world_mut().query::<(Entity, &GatherTarget)>();
            q.iter(app.world()).map(|(e, _)| e).next()
        };
        if let Some(e) = victim {
            if t % 7 == 0 {
                app.world_mut().entity_mut(e).remove::<GatherPhase>();
            }
        }
        step(&mut app);
        assert!(
            split_claims(&mut app).is_empty(),
            "tick {t}: a half claim was still on the field at the tick boundary"
        );
    }
    assert!(
        alloy(&app, Faction::A) > 0,
        "the economy died: the sweep is eating real jobs, not repairing splits"
    );
}

/// The property the reordering could break. `repair_gather_claims` now runs
/// *before* `apply_commands`, so a claim created this tick is only swept on the
/// next one — that is sound exactly because every writer writes the pair, and
/// this pins it: a freshly ordered gather job survives the following tick's
/// sweep, and a full mine → return → bank loop is never interrupted.
#[test]
fn a_claim_created_this_tick_survives_the_next_ticks_sweep() {
    let mut app = sim_app();
    let node_pos = Vec2::new(150.0, 0.0);
    let node = app
        .world_mut()
        .spawn((Position(node_pos), ResourceNode { amount: 1_000 }))
        .id();
    spawn_building(&mut app, "hq", Faction::A, Vec2::ZERO);
    let w = spawn_unit(&mut app, "worker", Faction::A, Vec2::ZERO);
    push(
        &mut app,
        Order::Gather {
            units: vec![w],
            node,
            node_pos,
        }
        .issued_by(Faction::A),
    );
    step(&mut app); // the order lands (after the sweep)
    assert!(
        app.world().get::<GatherTarget>(w).is_some(),
        "the order never took"
    );
    step(&mut app); // the next tick's sweep runs first — it must not confiscate
    assert!(
        app.world().get::<GatherTarget>(w).is_some() && app.world().get::<GatherPhase>(w).is_some(),
        "the sweep confiscated a job created on the previous tick"
    );
    let mut banked = false;
    for t in 0..900 {
        step(&mut app);
        assert!(
            app.world().get::<GatherTarget>(w).is_some(),
            "tick {t}: the loop lost its job to the sweep"
        );
        banked |= alloy(&app, Faction::A) > 0;
    }
    assert!(banked, "the worker kept its job but never banked");
}
