//! Critic probes for **B4** (adversarial review; not implementer tests).
//!
//! 1. F-032 licence, re-run independently: the pre-g1 `assets/data` (d50f9a1)
//!    under this binary must reproduce the F-041 values of
//!    `critic_b35_ac0b::critic_shipped_matchups_are_bit_identical`; the pre-B4
//!    `assets/data` (main) under `--features no-opening-reservation` must
//!    reproduce the pre-B4 values.
//! 2. The reservation rule: workers are not held; a lost barracks is due again
//!    and holds the army until it is re-placed.
//! 3. The grace bound in `b4_reservation`: a differential against a stricter,
//!    stockpile-aware bound (funds actually held when the opening came due),
//!    which must excuse no opening the shipped bound does not.

// Under the proof switch only the pre-B4 test is compiled.
#![cfg_attr(feature = "no-opening-reservation", allow(dead_code, unused_imports))]

use std::path::{Path, PathBuf};

use bevy::prelude::*;

use onus::batch::seed_at;
use onus::headless::{self, MatchSettings, DEFAULT_TICK_CAP};
use onus::sim::combat::{Casualties, Health};
use onus::sim::content::{BarracksOpening, Content};
use onus::sim::economy::{Building, ProductionQueue, Stockpiles, UnitDefIdx};
use onus::sim::spatial::Faction;
use onus::sim::{
    AiAction, AiCommanders, AiJournal, Carrying, CommandQueue, MatchState, Position, RateReport,
    ResourceNode,
};

// ---- harness (mirrors critic_b35_ac0b's, byte for byte where it matters) ----

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/critic_b4_data")
        .join(name)
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

fn matchup(c: Content, seed: u64, a: &str, b: &str, alloy: u32) -> App {
    let commanders = AiCommanders::matchup(&c, seed, &[(Faction::A, a), (Faction::B, b)])
        .expect("both strategies are named");
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

fn hash_trace(app: &mut App, samples: u32, every: u32) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for _ in 0..samples {
        tick(app, every);
        h ^= onus::sim::state_hash(app.world_mut());
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn journal_digest(app: &App) -> u64 {
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

const CASES: [(u64, &str, &str); 8] = [
    (1, "mvp", "mvp"),
    (2, "mvp", "rush"),
    (3, "rush", "mvp"),
    (4, "synth_triad", "turtle"),
    (5, "turtle", "synth_triad"),
    (6, "mass_arclight", "mass_ripper"),
    (7, "mass_ripper", "mass_arclight"),
    (8, "synth_steel_flesh", "mass_bulwark"),
];

fn shipped_matchups(dir: &Path) -> Vec<(u64, u64)> {
    CASES
        .iter()
        .map(|(seed, a, b)| {
            let c = Content::load_from_dir(dir).expect("fixture content loads");
            let mut app = matchup(c, *seed, a, b, 300);
            let h = hash_trace(&mut app, 120, 60);
            (h, journal_digest(&app))
        })
        .collect()
}

// ---- 1. the F-032 licence, re-run ------------------------------------------

/// The pre-g1 RON under the B4 binary reproduces F-041's pins (so g1's re-pin
/// moved because the data moved).
#[cfg(not(feature = "no-opening-reservation"))]
#[test]
fn critic_pre_g1_ron_reproduces_the_f041_shipped_matchups() {
    let got = shipped_matchups(&fixture("pre_g1"));
    let f041: [(u64, u64); 8] = [
        (0x47c76bb871e46efb, 0xbd416bd4be86da11),
        (0x79c341576e931fc4, 0x2a25c84bba78866a),
        (0xba80ea4ba6cacdea, 0x74973f97c60a686b),
        (0x622c06205abf7772, 0x65f9917da21d98f2),
        (0x1aa33ab2d6a04527, 0xa6552ff89996a930),
        (0x54b9d0fd2c5779be, 0x2dbb7d71846b917e),
        (0x241e74a2e363c562, 0xf6546287f161a392),
        (0x3835febbbac676e3, 0xb54435b778763907),
    ];
    assert_eq!(got, f041.to_vec());
}

/// The pre-B4 RON under the proof switch reproduces the pre-B4 pins (so the
/// switch is the pre-B4 commander and F-041's re-pin is the reservation).
#[cfg(feature = "no-opening-reservation")]
#[test]
fn critic_pre_b4_ron_under_the_switch_reproduces_the_pre_b4_shipped_matchups() {
    let got = shipped_matchups(&fixture("pre_b4"));
    let pre_b4: [(u64, u64); 8] = [
        (0x47c76bb871e46efb, 0xbd416bd4be86da11),
        (0x79c341576e931fc4, 0x2a25c84bba78866a),
        (0xba80ea4ba6cacdea, 0x74973f97c60a686b),
        (0xe94d2708dca81c49, 0x4666a3b3563b8890),
        (0xb7d0e39bdfb0757c, 0x926206a7e2150e39),
        (0x283fcec8bb5ebf41, 0xc7328be1b0e09085),
        (0x1bc7d81e51c54e99, 0x90d87cb290ad3f58),
        (0x478a4be3e491483e, 0xc9f9d38ba031b30b),
    ];
    assert_eq!(got, pre_b4.to_vec());
}

// ---- 2. the rule -------------------------------------------------------------

fn opening(building: &str, at_tick: u32, offset: f32) -> BarracksOpening {
    BarracksOpening {
        building: building.to_string(),
        at_tick,
        offset,
    }
}

fn hmatch(c: Content, a: &str, b: &str, seed: u64) -> App {
    let s = MatchSettings::default().with_seed(seed).with_strategies(a, b);
    headless::ai_vs_ai(c, &s).expect("named")
}

fn placements(app: &App, f: Faction) -> Vec<(u32, usize)> {
    app.world()
        .resource::<AiJournal>()
        .for_faction(f)
        .into_iter()
        .filter_map(|(t, a)| match a {
            AiAction::PlaceBarracks { building, .. } => Some((t, building)),
            _ => None,
        })
        .collect()
}

fn over(app: &App) -> bool {
    app.world().resource::<MatchState>().is_over()
}

/// Workers are not held: a script whose worker target is still unmet keeps
/// training workers while a due opening is unplaced.
#[cfg(not(feature = "no-opening-reservation"))]
#[test]
fn critic_workers_still_train_while_an_opening_is_due() {
    let mut c = headless::content().unwrap();
    let i = c.strategy_index("mass_bulwark").unwrap();
    c.strategies[i].worker_target = 20;
    c.strategies[i].barracks = vec![
        opening("foundry", 300, 130.0),
        opening("foundry", 600, 165.0),
        opening("foundry", 900, 200.0),
    ];
    let mut app = hmatch(c, "mass_bulwark", "mass_sentinel", seed_at(0, 0));
    let mut t = 0;
    while t < 12_000 && !over(&app) && placements(&app, Faction::A).len() < 3 {
        headless::step(&mut app);
        t += 1;
    }
    let placed: Vec<u32> = placements(&app, Faction::A).iter().map(|(t, _)| *t).collect();
    assert!(placed.len() >= 2, "fixture: second opening placed ({placed:?})");
    let journal = app.world().resource::<AiJournal>().for_faction(Faction::A);
    let in_window = journal.iter().any(|(t, a)| {
        matches!(a, AiAction::TrainWorker { .. }) && *t >= 600 && *t < placed[1]
    });
    assert!(
        in_window,
        "no worker trained between the second opening coming due (600) and its \
         placement ({}): the worker step is being held",
        placed[1]
    );
}

/// A barracks that is lost is due again: after all three lines stand, one is
/// destroyed; the army must train nothing until it is re-placed, and it must be
/// re-placed.
#[cfg(not(feature = "no-opening-reservation"))]
#[test]
fn critic_a_lost_line_is_due_again_and_holds_the_army_until_rebuilt() {
    let mut c = headless::content().unwrap();
    let foundry = c.building_index("foundry").unwrap();
    let i = c.strategy_index("mass_bulwark").unwrap();
    c.strategies[i].barracks = vec![
        opening("foundry", 300, 130.0),
        opening("foundry", 600, 165.0),
        opening("foundry", 900, 200.0),
    ];
    let mut app = hmatch(c, "mass_bulwark", "mass_sentinel", seed_at(0, 0));
    let mut t = 0;
    while t < DEFAULT_TICK_CAP && !over(&app) && placements(&app, Faction::A).len() < 3 {
        headless::step(&mut app);
        t += 1;
    }
    assert_eq!(placements(&app, Faction::A).len(), 3, "fixture: three lines");
    // Let a few units train, then lose the highest-entity foundry.
    headless::tick(&mut app, 600);
    assert!(!over(&app), "fixture: match still running");
    let victim = {
        let w = app.world_mut();
        let mut q = w.query::<(Entity, &Building, &Faction)>();
        let mut fs: Vec<Entity> = q
            .iter(w)
            .filter(|(_, b, f)| b.def == foundry && **f == Faction::A)
            .map(|(e, _, _)| e)
            .collect();
        fs.sort_by_key(|e| e.to_bits());
        *fs.last().expect("A owns a foundry")
    };
    app.world_mut().despawn(victim);
    let lost_at_len = app.world().resource::<AiJournal>().0.len();
    let mut steps = 0;
    while steps < 20_000 && !over(&app) && placements(&app, Faction::A).len() < 4 {
        headless::step(&mut app);
        steps += 1;
    }
    let journal = app.world().resource::<AiJournal>().0.clone();
    let after: Vec<_> = journal[lost_at_len..]
        .iter()
        .filter(|(_, f, _)| *f == Faction::A)
        .collect();
    let replace = after
        .iter()
        .position(|(_, _, a)| matches!(a, AiAction::PlaceBarracks { .. }));
    let replace = replace.expect("the lost line was never re-placed");
    assert!(
        !after[..replace]
            .iter()
            .any(|(_, _, a)| matches!(a, AiAction::TrainArmy { .. })),
        "the army trained while the lost line was due: {:?}",
        &after[..replace]
    );
}

// ---- 3. the grace bound --------------------------------------------------------

fn grace(c: &Content, strategy: &str, building: &str) -> u32 {
    let g = c.units.iter().find(|u| u.gathers).unwrap();
    let w = c.strategy(strategy).unwrap().worker_target;
    let cost = c.building(building).unwrap().alloy_cost;
    (cost * g.mvp_gather_ticks).div_ceil(w * g.mvp_carry_capacity)
}

struct Run {
    end: u32,
    /// alloy banked + alloy in A's/B's workers' hands, per step.
    funds: Vec<[u32; 2]>,
    placed: [Vec<(u32, usize)>; 2],
}

fn mirror(c: &Content, id: &str) -> Run {
    let n = c.strategy(id).unwrap().barracks.len();
    let mut app = hmatch(c.clone(), id, id, seed_at(0, 1));
    let mut funds = Vec::new();
    let mut t = 0;
    let fund = |app: &mut App| -> [u32; 2] {
        let s = app.world().resource::<Stockpiles>();
        let mut out = [s.alloy(Faction::A), s.alloy(Faction::B)];
        let w = app.world_mut();
        let mut q = w.query::<(&Carrying, &Faction)>();
        for (cr, f) in q.iter(w) {
            out[if *f == Faction::A { 0 } else { 1 }] += cr.0;
        }
        out
    };
    funds.push(fund(&mut app));
    while t < DEFAULT_TICK_CAP
        && !over(&app)
        && !(placements(&app, Faction::A).len() >= n && placements(&app, Faction::B).len() >= n)
    {
        headless::step(&mut app);
        t += 1;
        funds.push(fund(&mut app));
    }
    Run {
        end: t,
        funds,
        placed: [placements(&app, Faction::A), placements(&app, Faction::B)],
    }
}

/// A stockpile-aware bound: an unplaced opening is excused only if, starting
/// from the funds the side actually held when it came due, max income could not
/// bank the unplaced openings' cost before the end. Stricter than `grace`.
#[cfg(not(feature = "no-opening-reservation"))]
#[test]
fn critic_a_stockpile_aware_bound_excuses_the_same_openings() {
    let c = headless::content().unwrap();
    let g = c.units.iter().find(|u| u.gathers).unwrap();
    let mut report = Vec::new();
    let mut bad = Vec::new();
    for s in c.strategies.iter().filter(|s| s.barracks.len() > 1) {
        let r = mirror(&c, &s.id);
        report.push(format!("{}: end {} placed A {:?} B {:?}", s.id, r.end, r.placed[0], r.placed[1]));
        for (fi, placed) in r.placed.iter().enumerate() {
            let mut owed = 0u32;
            for (k, o) in s.barracks.iter().enumerate().skip(placed.len()) {
                owed += c.building(&o.building).unwrap().alloy_cost;
                let at = (o.at_tick as usize).min(r.funds.len() - 1);
                let have = r.funds[at][fi];
                let need = owed.saturating_sub(have);
                let tight =
                    (need * g.mvp_gather_ticks).div_ceil(s.worker_target * g.mvp_carry_capacity);
                let loose = grace(&c, &s.id, &o.building);
                report.push(format!(
                    "  side{fi} unplaced {k} ({} @ {}): funds at due {have}, tight {tight}, grace {loose}",
                    o.building, o.at_tick
                ));
                if o.at_tick < r.end && o.at_tick + tight <= r.end {
                    bad.push(format!(
                        "{} side{fi} opening {k}: due {} with {have} in hand; bankable by {} \
                         at max income, match ran to {}",
                        s.id,
                        o.at_tick,
                        o.at_tick + tight,
                        r.end
                    ));
                }
            }
        }
    }
    println!("{}", report.join("\n"));
    assert!(bad.is_empty(), "{}\n{}", bad.join("\n"), report.join("\n"));
}
