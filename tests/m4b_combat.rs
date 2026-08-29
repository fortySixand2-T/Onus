//! L2 integration tests for M4b (combat: 4-stat + nemesis).
//!
//! Encodes the M4b acceptance criteria and the critic probes:
//!   AC1 — health + attack; a unit engages the **nearest enemy in range** (the
//!         M2 spatial index picks it), paths to it with M3, and death despawns;
//!   AC2 — the 4-stat model: Offense = damage/hit, Armor = flat mitigation per
//!         hit, Defense = HP pool, Speed = movement (per-unit, from the RON);
//!   AC3 — nemesis: +30% damage **ignoring armor** iff attacker.nemesis == the
//!         defender's id.
//!
//! Everything runs headless (`MinimalPlugins`) through the *shipped* sim chain
//! (`onus::add_sim_systems`) — no render types, no hand-rolled system list
//! (F-004), one fixed timestep handed to the sim per step (F-003).

use std::path::PathBuf;

use bevy::prelude::*;

use onus::sim::combat::{AttackCooldown, Casualties, Engaging, Health, Target};
use onus::sim::content::Content;
use onus::sim::economy::{Stockpiles, UnitDefIdx};
use onus::sim::spatial::{brute_force_nearest_enemy, Faction, SplitMix64, Unit};
use onus::sim::{CommandQueue, MoveTarget, Order, Position, RateReport, TileGrid};

// ---- harness ----------------------------------------------------------------

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data")
}

fn content() -> Content {
    Content::load_from_dir(&data_dir()).expect("assets/data/*.ron parse into sim structs")
}

/// A headless app running the shipped sim chain on `Update`, so one
/// `step()` == exactly one 60 Hz sim tick.
fn sim_app() -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .insert_resource(Time::<Fixed>::from_hz(60.0))
        .insert_resource(content())
        .init_resource::<CommandQueue>()
        .init_resource::<RateReport>()
        .init_resource::<Casualties>()
        .insert_resource(Stockpiles::default());
    onus::add_sim_systems(&mut app, Update);
    app
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

/// Spawn a unit of `id` — exactly what the sim's own production path builds:
/// position, definition index, kind, faction and a full HP pool from the RON.
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

fn hp(app: &App, e: Entity) -> Option<u32> {
    app.world().get::<Health>(e).map(|h| h.current)
}

fn pos(app: &App, e: Entity) -> Option<Vec2> {
    app.world().get::<Position>(e).map(|p| p.0)
}

fn alive(app: &App, e: Entity) -> bool {
    app.world().get_entity(e).is_ok()
}

/// Damage `attacker` deals to `defender` per hit, computed from the RON the same
/// way the sim does — the tests state the numbers independently below.
fn dmg(app: &App, attacker: &str, defender: &str) -> u32 {
    let c = app.world().resource::<Content>();
    onus::sim::combat::damage_per_hit(
        c,
        c.unit_index(attacker).unwrap(),
        c.unit_index(defender).unwrap(),
    )
}

// ---- AC1: health, attack, engagement, death --------------------------------

/// The spine of AC1: a unit standing next to an enemy hits it every
/// `mvp_attack_ticks` for exactly `damage_per_hit`, and the corpse despawns on
/// the killing tick.
#[test]
fn a_unit_attacks_the_enemy_in_range_until_it_dies_and_despawns() {
    let mut app = sim_app();
    // Bulwark's nemesis is the Ravager, so vs a Ripper this is plain math:
    // offense 4 -> 20 damage, Ripper armor 1 -> 2 mitigation, 18 per hit.
    let attacker = spawn_unit(&mut app, "bulwark", Faction::A, Vec2::new(0.0, 0.0));
    let victim = spawn_unit(&mut app, "ripper", Faction::B, Vec2::new(10.0, 0.0));

    let per_hit = dmg(&app, "bulwark", "ripper");
    assert_eq!(per_hit, 18, "20 offense damage - 2 armor mitigation");
    let full = hp(&app, victim).expect("victim has an HP pool");
    assert_eq!(full, 60, "Ripper defense 3 -> 60 HP");

    let attack_ticks = {
        let c = app.world().resource::<Content>();
        c.unit("bulwark").unwrap().mvp_attack_ticks
    };
    assert!(attack_ticks > 0, "combat units declare an attack cadence");

    // First hit lands on the first tick in range.
    step(&mut app);
    assert_eq!(hp(&app, victim), Some(full - per_hit));
    // ...and not again until the cadence elapses.
    tick(&mut app, attack_ticks - 1);
    assert_eq!(hp(&app, victim), Some(full - per_hit), "cadence respected");
    step(&mut app);
    assert_eq!(hp(&app, victim), Some(full - 2 * per_hit), "second hit");

    let hits_to_kill = full.div_ceil(per_hit);
    // Ticks from the first hit to the last: (hits - 1) cadences.
    tick(&mut app, (hits_to_kill - 2) * attack_ticks - 1);
    assert!(alive(&app, victim), "still alive one tick before the kill");
    step(&mut app);
    assert!(!alive(&app, victim), "death despawns the entity");
    assert!(alive(&app, attacker), "the attacker is untouched");
    assert_eq!(app.world().resource::<Casualties>().total(), 1);
}

/// Engagement: an idle unit walks to the nearest enemy that is inside its
/// engage range and kills it. Movement is the sim's, driven by a `MoveTarget`
/// the combat system sets — input never had to order anything.
#[test]
fn an_idle_unit_closes_on_the_nearest_enemy_and_kills_it() {
    let mut app = sim_app();
    let hunter = spawn_unit(&mut app, "sentinel", Faction::A, Vec2::new(0.0, 0.0));
    let prey = spawn_unit(&mut app, "worker", Faction::B, Vec2::new(180.0, 0.0));

    step(&mut app);
    assert!(
        app.world().get::<MoveTarget>(hunter).is_some(),
        "an enemy inside engage range is chased"
    );
    assert_eq!(
        app.world().get::<Target>(hunter).map(|t| t.0),
        Some(prey),
        "the engaged target is recorded in sim state"
    );

    tick(&mut app, 600);
    assert!(!alive(&app, prey), "the hunter closed and killed it");
    let d = pos(&app, hunter).unwrap().distance(Vec2::new(180.0, 0.0));
    let range = {
        let c = app.world().resource::<Content>();
        c.unit("sentinel").unwrap().mvp_attack_range
    };
    assert!(
        d <= range + 1.0,
        "it stopped at attack range, not on top ({d})"
    );
    assert!(
        app.world().get::<MoveTarget>(hunter).is_none(),
        "with no enemy left, the chase order is dropped"
    );
}

/// An enemy beyond the engage range is left alone: no chase, no target.
#[test]
fn an_enemy_outside_engage_range_is_ignored() {
    let mut app = sim_app();
    let engage = content().combat.engage_range;
    let hunter = spawn_unit(&mut app, "sentinel", Faction::A, Vec2::ZERO);
    let far = spawn_unit(
        &mut app,
        "worker",
        Faction::B,
        Vec2::new(engage + 50.0, 0.0),
    );

    tick(&mut app, 30);
    assert!(app.world().get::<MoveTarget>(hunter).is_none(), "no chase");
    assert!(app.world().get::<Target>(hunter).is_none(), "no target");
    assert_eq!(hp(&app, far), Some(60), "and no damage at that distance");
}

/// The engaged target is the M2 nearest enemy — differential against the
/// brute-force oracle over many seeded layouts.
#[test]
fn the_engaged_target_matches_the_brute_force_nearest_enemy() {
    for seed in 0..40u64 {
        let mut rng = SplitMix64::new(0xB0A7 ^ seed);
        let mut app = sim_app();
        let mut layout: Vec<Unit> = Vec::new();
        let mut ents: Vec<Entity> = Vec::new();
        for i in 0..24 {
            let p = Vec2::new(rng.range_f32(-120.0, 120.0), rng.range_f32(-120.0, 120.0));
            let faction = if i % 2 == 0 { Faction::A } else { Faction::B };
            // Workers: no attacks, so nothing dies and the layout is stable.
            ents.push(spawn_unit(&mut app, "worker", faction, p));
            layout.push(Unit { pos: p, faction });
        }
        // One armed unit whose target we check.
        let p = Vec2::new(rng.range_f32(-120.0, 120.0), rng.range_f32(-120.0, 120.0));
        let hunter = spawn_unit(&mut app, "sentinel", Faction::A, p);
        layout.push(Unit {
            pos: p,
            faction: Faction::A,
        });
        let hunter_i = layout.len() - 1;

        step(&mut app);

        let expect = brute_force_nearest_enemy(&layout, hunter_i)
            .map(|j| ents[j])
            .filter(|_| true);
        let engaged = app.world().get::<Target>(hunter).map(|t| t.0);
        let engage = content().combat.engage_range;
        let in_range = expect.is_some_and(|_| {
            let j = brute_force_nearest_enemy(&layout, hunter_i).unwrap();
            layout[hunter_i].pos.distance(layout[j].pos) <= engage
        });
        if in_range {
            assert_eq!(engaged, expect, "seed {seed}: engaged the nearest enemy");
        }
    }
}

/// Two units that kill each other on the same tick both die, each counted
/// exactly once — no double-despawn, no double kill credit.
#[test]
fn a_mutual_kill_on_one_tick_despawns_both_exactly_once() {
    let mut app = sim_app();
    let a = spawn_unit(&mut app, "arclight", Faction::A, Vec2::ZERO);
    let b = spawn_unit(&mut app, "arclight", Faction::B, Vec2::new(20.0, 0.0));
    // Both one hit from death: each will land its blow on the same tick.
    for e in [a, b] {
        app.world_mut().get_mut::<Health>(e).unwrap().current = 1;
    }

    step(&mut app);
    assert!(!alive(&app, a) && !alive(&app, b), "both die on the tick");
    assert_eq!(
        app.world().resource::<Casualties>().total(),
        2,
        "two deaths, counted once each"
    );
    assert_eq!(app.world().resource::<Casualties>().lost(Faction::A), 1);
    assert_eq!(app.world().resource::<Casualties>().lost(Faction::B), 1);

    tick(&mut app, 60);
    assert_eq!(
        app.world().resource::<Casualties>().total(),
        2,
        "the dead are not killed again on later ticks"
    );
}

/// HP is a pool that floors at zero: an overkill hit never wraps around.
#[test]
fn hp_never_underflows_on_overkill() {
    let mut app = sim_app();
    let victim = spawn_unit(&mut app, "worker", Faction::B, Vec2::new(15.0, 0.0));
    let _killer = spawn_unit(&mut app, "arclight", Faction::A, Vec2::ZERO);
    app.world_mut().get_mut::<Health>(victim).unwrap().current = 1;

    step(&mut app);
    assert!(!alive(&app, victim), "overkill kills");
    assert_eq!(app.world().resource::<Casualties>().total(), 1);
    tick(&mut app, 10);
    assert_eq!(app.world().resource::<Casualties>().total(), 1);
}

/// Combat paths with M3: with a wall between them, the attacker walks *around*
/// (never through a blocked cell) and still reaches and kills its target.
#[test]
fn an_attacker_paths_around_a_wall_to_reach_its_target() {
    let mut app = sim_app();
    // 10x10 cells of 32 world units, origin at the world origin. A wall on
    // column 5 from y=0..8 leaves one gap at y=9.
    let mut grid = TileGrid::new(10, 10, 32.0, Vec2::ZERO);
    for y in 0..9 {
        grid.set_blocked(5, y, true);
    }
    let start = grid.cell_center(grid.idx(2, 2));
    let goal = grid.cell_center(grid.idx(8, 2));
    app.insert_resource(grid);

    let hunter = spawn_unit(&mut app, "sentinel", Faction::A, start);
    let prey = spawn_unit(&mut app, "worker", Faction::B, goal);

    for _ in 0..1200 {
        step(&mut app);
        if let Some(p) = pos(&app, hunter) {
            let g = app.world().resource::<TileGrid>();
            let cell = g.cell_at(p);
            assert!(g.is_walkable(cell), "the attacker never enters a wall cell");
        }
        if !alive(&app, prey) {
            break;
        }
    }
    assert!(
        !alive(&app, prey),
        "it got around the wall and killed the target"
    );
}

/// Determinism: the same seeded battle, run twice, ends byte-identically.
#[test]
fn a_battle_is_deterministic_across_identical_runs() {
    fn run(seed: u64) -> Vec<(u64, u32, [u32; 2])> {
        let mut app = sim_app();
        let mut rng = SplitMix64::new(seed);
        let roster = ["bulwark", "sentinel", "ripper", "ravager", "arclight"];
        for i in 0..20 {
            let id = roster[(rng.next_u64() % roster.len() as u64) as usize];
            let faction = if i % 2 == 0 { Faction::A } else { Faction::B };
            let p = Vec2::new(rng.range_f32(-150.0, 150.0), rng.range_f32(-150.0, 150.0));
            spawn_unit(&mut app, id, faction, p);
        }
        tick(&mut app, 900);
        let mut out: Vec<(u64, u32, [u32; 2])> = app
            .world_mut()
            .query::<(Entity, &Health, &Position)>()
            .iter(app.world())
            .map(|(e, h, p)| (e.to_bits(), h.current, [p.0.x.to_bits(), p.0.y.to_bits()]))
            .collect();
        out.sort_unstable();
        out
    }
    let a = run(0xC0FFEE);
    let b = run(0xC0FFEE);
    assert_eq!(a, b, "same seed, same battle");
    assert!(!a.is_empty(), "somebody survived (the test is not vacuous)");
}

// ---- AC2: the 4-stat model --------------------------------------------------

/// Defense is the HP pool and Offense is damage per hit, both scaled from the
/// RON — no Rust constants.
#[test]
fn defense_is_the_hp_pool_and_offense_is_damage_per_hit() {
    let c = content();
    for u in &c.units {
        let expect = u.defense * c.combat.hp_per_defense;
        let idx = c.unit_index(&u.id).unwrap();
        assert_eq!(
            Health::from_def(&c, idx).max,
            expect,
            "{} HP pool = defense x hp_per_defense",
            u.id
        );
        assert!(expect > 0, "{} has a real HP pool", u.id);
    }
    // Offense scales damage: against a zero-armor defender, damage == offense x
    // damage_per_offense.
    let worker = c.unit_index("worker").unwrap(); // armor 0
    for u in &c.units {
        let idx = c.unit_index(&u.id).unwrap();
        if c.units[idx].nemesis.as_deref() == Some("worker") {
            continue;
        }
        assert_eq!(
            onus::sim::combat::damage_per_hit(&c, idx, worker),
            u.offense * c.combat.damage_per_offense,
            "{} damage = offense x damage_per_offense against 0 armor",
            u.id
        );
    }
}

/// Armor is *flat mitigation per hit*, not a percentage, and never turns a hit
/// into healing.
#[test]
fn armor_is_flat_mitigation_per_hit() {
    let c = content();
    let arclight = c.unit_index("arclight").unwrap();
    // Arclight's nemesis is the Bulwark, so pick non-prey defenders here.
    for def_id in ["sentinel", "ripper", "ravager"] {
        let d = c.unit_index(def_id).unwrap();
        let base = c.units[arclight].offense * c.combat.damage_per_offense;
        let mitigation = c.units[d].armor * c.combat.mitigation_per_armor;
        assert_eq!(
            onus::sim::combat::damage_per_hit(&c, arclight, d),
            base - mitigation,
            "flat subtraction vs {def_id}"
        );
    }
    // A hit that armor fully absorbs deals zero, never a wrapped-around amount.
    let worker = c.unit_index("worker").unwrap();
    let bulwark = c.unit_index("bulwark").unwrap();
    assert_eq!(
        onus::sim::combat::damage_per_hit(&c, worker, bulwark),
        0,
        "an unarmed worker cannot hurt a Bulwark"
    );
}

/// Speed is per-unit data. The global `sim::SPEED` constant is retired: a fast
/// unit and a slow one cover different ground in the same number of ticks, in
/// the ratio their RON `speed` values dictate.
#[test]
fn movement_speed_comes_from_the_units_ron() {
    let mut app = sim_app();
    let slow = spawn_unit(&mut app, "bulwark", Faction::A, Vec2::ZERO); // speed 2
    let fast = spawn_unit(&mut app, "ripper", Faction::A, Vec2::ZERO); // speed 9
    let dest = Vec2::new(5000.0, 0.0);
    for e in [slow, fast] {
        app.world_mut().entity_mut(e).insert(MoveTarget(dest));
    }
    tick(&mut app, 60); // one second

    let c = content();
    let (s, f) = (
        c.unit("bulwark").unwrap().speed as f32,
        c.unit("ripper").unwrap().speed as f32,
    );
    let scale = c.combat.speed_per_point;
    assert!(
        (pos(&app, slow).unwrap().x - s * scale).abs() < 1.0,
        "the Bulwark covers speed x speed_per_point in one second"
    );
    assert!(
        (pos(&app, fast).unwrap().x - f * scale).abs() < 1.0,
        "and the Ripper covers its own, larger, distance"
    );
}

// ---- AC3: the nemesis override ---------------------------------------------

/// +30% damage, ignoring armor, iff the defender is the attacker's declared
/// prey. Integer math throughout: floor(base * 1300 / 1000).
#[test]
fn nemesis_adds_30_percent_and_ignores_armor() {
    let c = content();
    let milli = c.nemesis_bonus.mult_milli();
    assert_eq!(milli, 1300, "+30% held as integer per-mille");
    assert!(c.nemesis_bonus.ignore_armor);

    for u in &c.units {
        let a = c.unit_index(&u.id).unwrap();
        let Some(prey_id) = u.nemesis.as_deref() else {
            continue;
        };
        let prey = c.unit_index(prey_id).expect("nemesis names a real unit");
        let base = u.offense * c.combat.damage_per_offense;
        let expect = (base as u64 * milli as u64 / 1000) as u32;
        assert_eq!(
            onus::sim::combat::damage_per_hit(&c, a, prey),
            expect,
            "{} vs its prey {prey_id}: +30%, armor ignored",
            u.id
        );
        // Strictly better than the armored, unbuffed hit it would otherwise be.
        let mitigation = c.units[prey].armor * c.combat.mitigation_per_armor;
        assert!(
            expect > base.saturating_sub(mitigation),
            "the override actually helps"
        );
        // And every *other* defender is unaffected by this unit's nemesis.
        for other in &c.units {
            if other.id == *prey_id {
                continue;
            }
            let o = c.unit_index(&other.id).unwrap();
            let flat = base.saturating_sub(other.armor * c.combat.mitigation_per_armor);
            assert_eq!(
                onus::sim::combat::damage_per_hit(&c, a, o),
                flat,
                "{} vs non-prey {}: plain armor math",
                u.id,
                other.id
            );
        }
    }
}

/// The override shows up in the fight, not just in the formula: the Bulwark
/// kills its prey (Ravager, armor 4) faster than a Sentinel of the same armor
/// class would be hurt — measured as HP removed per hit in a live sim.
#[test]
fn the_nemesis_bonus_applies_in_a_live_fight() {
    let mut app = sim_app();
    // Bulwark's nemesis is the Ravager.
    let prey = spawn_unit(&mut app, "ravager", Faction::B, Vec2::new(20.0, 0.0));
    let _bulwark = spawn_unit(&mut app, "bulwark", Faction::A, Vec2::ZERO);
    let full = hp(&app, prey).unwrap();

    step(&mut app);
    let dealt = full - hp(&app, prey).unwrap();
    let c = content();
    let base = c.unit("bulwark").unwrap().offense * c.combat.damage_per_offense;
    assert_eq!(dealt, base * 13 / 10, "nemesis damage, armor ignored");
    assert!(
        dealt > base - c.unit("ravager").unwrap().armor * c.combat.mitigation_per_armor,
        "and it beats the armored hit"
    );
}

/// The commander outranks the sim: a move order issued to a unit that is
/// *already* auto-chasing cancels the chase on the tick it is applied, and the
/// unit goes where it was told. (The earlier tests only covered a unit that had
/// never engaged, which is how the mid-chase case slipped through.)
#[test]
fn a_move_order_cancels_an_auto_chase_already_in_progress() {
    let mut app = sim_app();
    let hunter = spawn_unit(&mut app, "sentinel", Faction::A, Vec2::ZERO);
    let prey = spawn_unit(&mut app, "worker", Faction::B, Vec2::new(150.0, 0.0));

    step(&mut app);
    assert!(
        app.world().get::<Engaging>(hunter).is_some(),
        "precondition: the sentinel is chasing"
    );

    let dest = Vec2::new(-400.0, 0.0);
    app.world_mut()
        .resource_mut::<CommandQueue>()
        .0
        .push_back(Order::MoveTo {
            units: vec![hunter],
            dest,
        });
    step(&mut app);
    assert!(
        app.world().get::<Engaging>(hunter).is_none(),
        "the order ends the chase"
    );
    assert_eq!(
        app.world().get::<MoveTarget>(hunter).map(|m| m.0),
        Some(dest),
        "and the destination survives the same tick's combat pass"
    );

    tick(&mut app, 120);
    assert!(
        pos(&app, hunter).unwrap().x < -100.0,
        "the unit retreats instead of resuming the chase"
    );
    assert!(alive(&app, prey), "and it never got back into reach");
}

/// Stats the per-hit arithmetic cannot represent are refused at load, and the
/// nemesis multiply saturates rather than wrapping if one ever reaches it: a
/// +30% bonus is never allowed to come out smaller than the base hit.
#[test]
fn out_of_scale_stats_are_rejected_and_the_bonus_never_wraps() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/m4b_scale_content");
    std::fs::create_dir_all(&dir).unwrap();
    let units = std::fs::read_to_string(data_dir().join("units.ron")).unwrap();
    std::fs::write(
        dir.join("units.ron"),
        units.replace("speed: 2, offense: 4,", "speed: 2, offense: 4000000000,"),
    )
    .unwrap();
    std::fs::copy(data_dir().join("resources.ron"), dir.join("resources.ron")).unwrap();
    assert!(
        Content::load_from_dir(&dir).is_err(),
        "an offense far outside the design scale must not load"
    );

    // And in memory, where nothing was validated, the bonus still cannot invert.
    let mut c = content();
    let (bulwark, ravager) = (
        c.unit_index("bulwark").unwrap(),
        c.unit_index("ravager").unwrap(),
    );
    for offense in [1u32, 10, 1_000_000_000] {
        c.units[bulwark].offense = offense;
        let base = offense.saturating_mul(c.combat.damage_per_offense);
        assert!(
            onus::sim::combat::damage_per_hit(&c, bulwark, ravager) >= base,
            "offense {offense}: the nemesis bonus must never be a penalty"
        );
    }
}

/// A cooldown component is sim state, not a wall-clock timer: it counts ticks.
#[test]
fn the_attack_cadence_counts_ticks() {
    let mut app = sim_app();
    let a = spawn_unit(&mut app, "bulwark", Faction::A, Vec2::ZERO);
    let _b = spawn_unit(&mut app, "ripper", Faction::B, Vec2::new(10.0, 0.0));
    let cadence = content().unit("bulwark").unwrap().mvp_attack_ticks;

    step(&mut app);
    assert_eq!(
        app.world().get::<AttackCooldown>(a).map(|c| c.0),
        Some(cadence),
        "firing sets the cooldown to the RON cadence"
    );
    step(&mut app);
    assert_eq!(
        app.world().get::<AttackCooldown>(a).map(|c| c.0),
        Some(cadence - 1),
        "and it ticks down once per sim tick"
    );
}

// ---- shipped, not just tested (F-004) --------------------------------------

#[test]
fn the_combat_system_is_registered_in_the_shipped_sim_chain() {
    let lib = std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs"))
        .expect("src/lib.rs");
    assert_eq!(
        lib.matches("sim::combat::combat").count(),
        1,
        "combat must be registered exactly once, in add_sim_systems"
    );
}
