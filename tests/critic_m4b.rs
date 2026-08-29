//! M4b adversarial probes (critic). Judged against the M4b ACs only:
//!   AC1 health + attack; engage the nearest enemy in range (M2), path via M3,
//!       death despawns;
//!   AC2 4-stat model (Offense/Armor/Defense/Speed);
//!   AC3 nemesis: +30% damage ignoring armor iff `attacker.nemesis ==
//!       defender.id`.
//! Probes: damage/armor math, nemesis iff, exactly-once death, HP never
//! underflows, determinism, content-as-data.
//!
//! Everything drives the *shipped* sim chain (`onus::add_sim_systems`).

use std::path::PathBuf;

use bevy::prelude::*;

use onus::sim::combat::{damage_per_hit, move_speed, AttackCooldown, Casualties, Health, Target};
use onus::sim::content::Content;
use onus::sim::economy::{Stockpiles, UnitDefIdx};
use onus::sim::spatial::Faction;
use onus::sim::{CommandQueue, Engaging, MoveTarget, Order, Position, RateReport};

// ---- harness ---------------------------------------------------------------

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data")
}

fn content() -> Content {
    Content::load_from_dir(&data_dir()).expect("content loads")
}

fn sim_app() -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .insert_resource(Time::<Fixed>::from_hz(60.0))
        .insert_resource(content())
        .init_resource::<CommandQueue>()
        .init_resource::<RateReport>()
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

fn def_of(app: &App, id: &str) -> usize {
    app.world()
        .resource::<Content>()
        .unit_index(id)
        .unwrap_or_else(|| panic!("unit `{id}`"))
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

fn hp(app: &App, e: Entity) -> Option<u32> {
    app.world().get::<Health>(e).map(|h| h.current)
}

fn pos(app: &App, e: Entity) -> Option<Vec2> {
    app.world().get::<Position>(e).map(|p| p.0)
}

fn alive(app: &App, e: Entity) -> bool {
    app.world().get_entity(e).is_ok()
}

fn set_hp(app: &mut App, e: Entity, v: u32) {
    app.world_mut().get_mut::<Health>(e).unwrap().current = v;
}

fn push(app: &mut App, order: Order) {
    app.world_mut()
        .resource_mut::<CommandQueue>()
        .0
        .push_back(order);
}

/// Write a mutated copy of the content pair and try to load it.
fn load_mutated(name: &str, from: &str, to: &str) -> Result<Content, String> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/critic_m4b_content")
        .join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let units = std::fs::read_to_string(data_dir().join("units.ron")).unwrap();
    assert!(units.contains(from), "anchor `{from}` missing from units.ron");
    std::fs::write(dir.join("units.ron"), units.replace(from, to)).unwrap();
    std::fs::copy(data_dir().join("resources.ron"), dir.join("resources.ron")).unwrap();
    Content::load_from_dir(&dir).map_err(|e| e.to_string())
}

fn mutated(name: &str, from: &str, to: &str) -> Content {
    load_mutated(name, from, to).expect("edited content should still be valid")
}

// ---- P1: auto-engagement must never override a commander's order -----------

/// The spec's engagement is *auto*-engagement; input owns the unit's orders
/// (CLAUDE.md: "Input never mutates sim state directly — it emits Orders the sim
/// consumes"). A unit that is mid-chase and is then told to move somewhere else
/// must obey the commander. `Order::MoveTo` sets a fresh `MoveTarget`; nothing
/// clears the combat-owned `Engaging` marker, so combat keeps treating the
/// player's order as its own and overwrites it every tick.
#[test]
fn an_explicit_move_order_beats_an_in_progress_auto_chase() {
    let mut app = sim_app();
    let hunter = spawn_unit(&mut app, "sentinel", Faction::A, Vec2::ZERO);
    let _prey = spawn_unit(&mut app, "worker", Faction::B, Vec2::new(150.0, 0.0));

    step(&mut app);
    assert!(
        app.world().get::<Engaging>(hunter).is_some(),
        "precondition: the sentinel auto-engaged the worker"
    );

    // The commander orders a retreat in the opposite direction.
    let dest = Vec2::new(-400.0, 0.0);
    push(
        &mut app,
        Order::MoveTo {
            units: vec![hunter],
            dest,
        },
    );
    step(&mut app);
    assert_eq!(
        app.world().get::<MoveTarget>(hunter).map(|m| m.0),
        Some(dest),
        "the commander's destination must survive the same tick's combat pass"
    );

    tick(&mut app, 60);
    let x = pos(&app, hunter).unwrap().x;
    assert!(
        x < -100.0,
        "the unit must retreat, not resume chasing: x = {x}"
    );
}

// ---- P2: the nemesis multiply must never wrap ------------------------------

/// The bonus is "+30% damage ignoring armor": for every input it must be at
/// least the unmitigated base. The widened `u64` multiply is cast back with
/// `as u32`, which truncates instead of saturating (every other step in the
/// formula uses `saturating_*`), so a large enough offense turns the bonus into
/// a penalty.
#[test]
fn the_nemesis_bonus_is_never_smaller_than_the_base_damage() {
    let mut c = content();
    let (bulwark, ravager) = (
        c.unit_index("bulwark").unwrap(),
        c.unit_index("ravager").unwrap(),
    );
    for offense in [1u32, 10, 1_000, 1_000_000, 1_000_000_000] {
        c.units[bulwark].offense = offense;
        let base = offense.saturating_mul(c.combat.damage_per_offense);
        let dealt = damage_per_hit(&c, bulwark, ravager);
        assert!(
            dealt >= base,
            "offense {offense}: nemesis dealt {dealt} < unmitigated base {base}"
        );
    }
}

// ---- P3: nemesis applies iff the defender is the declared prey --------------

/// Exhaustive over the roster: the set of ordered pairs whose damage differs
/// from plain `base - armor*mitigation` must be exactly the set of declared
/// nemesis pairs, and each of those must equal `floor(base * 1300 / 1000)` with
/// armor fully bypassed.
#[test]
fn nemesis_applies_to_exactly_the_declared_pairs_and_bypasses_armor() {
    let c = content();
    let milli = c.nemesis_bonus.mult_milli() as u64;
    let mut boosted_pairs = Vec::new();
    let mut declared_pairs = Vec::new();
    for (a, au) in c.units.iter().enumerate() {
        for (d, du) in c.units.iter().enumerate() {
            let base = au.offense * c.combat.damage_per_offense;
            let mitigation = du.armor * c.combat.mitigation_per_armor;
            let plain = base.saturating_sub(mitigation);
            let dealt = damage_per_hit(&c, a, d);
            if au.nemesis.as_deref() == Some(du.id.as_str()) {
                declared_pairs.push((au.id.clone(), du.id.clone()));
                let expect = (base as u64 * milli / 1_000) as u32;
                assert_eq!(
                    dealt, expect,
                    "{} -> {}: nemesis damage must be floor(base*{milli}/1000), armor ignored",
                    au.id, du.id
                );
                assert!(
                    dealt >= base,
                    "{} -> {}: armor must be bypassed, not mitigated",
                    au.id, du.id
                );
            }
            if dealt != plain {
                boosted_pairs.push((au.id.clone(), du.id.clone()));
            }
        }
    }
    assert!(!declared_pairs.is_empty(), "the roster declares nemeses");
    assert_eq!(
        boosted_pairs, declared_pairs,
        "only the declared prey may see anything other than the plain armor math"
    );
}

// ---- P4: exactly-once death, kill credit -----------------------------------

/// Two attackers kill one victim on the same tick: one despawn, one casualty,
/// credited to the *loser's* faction.
#[test]
fn two_attackers_killing_on_one_tick_produce_exactly_one_casualty() {
    let mut app = sim_app();
    let a1 = spawn_unit(&mut app, "arclight", Faction::A, Vec2::new(0.0, 0.0));
    let a2 = spawn_unit(&mut app, "arclight", Faction::A, Vec2::new(0.0, 10.0));
    let victim = spawn_unit(&mut app, "ripper", Faction::B, Vec2::new(100.0, 0.0));

    let per_hit = damage_per_hit(
        app.world().resource::<Content>(),
        def_of(&app, "arclight"),
        def_of(&app, "ripper"),
    );
    let full = hp(&app, victim).unwrap();
    assert!(
        2 * per_hit >= full && per_hit < full,
        "precondition: it takes both attackers, on one tick ({per_hit}x2 vs {full})"
    );

    step(&mut app);
    assert!(!alive(&app, victim), "the victim dies on that tick");
    assert!(alive(&app, a1) && alive(&app, a2));
    let cas = *app.world().resource::<Casualties>();
    assert_eq!(cas.total(), 1, "one death, one casualty (no double credit)");
    assert_eq!(cas.lost(Faction::B), 1, "credited to the side that lost it");
    assert_eq!(cas.lost(Faction::A), 0);

    // And it stays exactly one on subsequent ticks.
    tick(&mut app, 10);
    assert_eq!(app.world().resource::<Casualties>().total(), 1);
}

/// Four simultaneous deaths across two independent duels: each despawns once.
#[test]
fn simultaneous_mutual_kills_despawn_each_unit_exactly_once() {
    let mut app = sim_app();
    let mut pair = |x: f32| {
        let s = spawn_unit(&mut app, "sentinel", Faction::A, Vec2::new(x, 0.0));
        let r = spawn_unit(&mut app, "ravager", Faction::B, Vec2::new(x + 40.0, 0.0));
        (s, r)
    };
    let (s1, r1) = pair(0.0);
    let (s2, r2) = pair(5_000.0);

    let c = content();
    let s_hits = damage_per_hit(&c, def_of(&app, "sentinel"), def_of(&app, "ravager"));
    let r_hits = damage_per_hit(&c, def_of(&app, "ravager"), def_of(&app, "sentinel"));
    for (s, r) in [(s1, r1), (s2, r2)] {
        set_hp(&mut app, s, r_hits);
        set_hp(&mut app, r, s_hits);
    }

    step(&mut app);
    for e in [s1, r1, s2, r2] {
        assert!(!alive(&app, e), "every mortally wounded unit died");
    }
    let cas = *app.world().resource::<Casualties>();
    assert_eq!(cas.total(), 4, "four deaths, counted once each");
    assert_eq!((cas.lost(Faction::A), cas.lost(Faction::B)), (2, 2));
    assert_eq!(
        app.world_mut().query::<&UnitDefIdx>().iter(app.world()).len(),
        0,
        "the field is empty"
    );
}

// ---- P5: sim state must not reference despawned entities -------------------

/// `Target` is documented as inspectable sim state. On the killing tick the
/// attacker is given `Target(victim)` while the victim is despawned in the same
/// command flush, leaving a dangling entity reference in sim state.
#[test]
fn no_target_component_points_at_a_despawned_entity() {
    let mut app = sim_app();
    let _a = spawn_unit(&mut app, "arclight", Faction::A, Vec2::new(0.0, 0.0));
    let _b = spawn_unit(&mut app, "ripper", Faction::B, Vec2::new(100.0, 0.0));

    for t in 0..400u32 {
        step(&mut app);
        let dangling: Vec<_> = app
            .world_mut()
            .query::<(Entity, &Target)>()
            .iter(app.world())
            .filter(|(_, tgt)| app.world().get_entity(tgt.0).is_err())
            .map(|(e, tgt)| (e, tgt.0))
            .collect();
        assert!(
            dangling.is_empty(),
            "tick {t}: sim state points at despawned entities: {dangling:?}"
        );
    }
}

// ---- P6: HP invariants ------------------------------------------------------

/// Across a long melee no living unit may hold 0 HP, HP above its pool, or a
/// wrapped value.
#[test]
fn hp_stays_within_zero_exclusive_and_max_inclusive() {
    let mut app = sim_app();
    let ids = ["bulwark", "sentinel", "ripper", "ravager", "arclight"];
    for (i, id) in ids.iter().enumerate() {
        let x = i as f32 * 25.0;
        spawn_unit(&mut app, id, Faction::A, Vec2::new(x, 0.0));
        spawn_unit(&mut app, id, Faction::B, Vec2::new(x, 40.0));
    }
    for t in 0..900u32 {
        step(&mut app);
        let bad: Vec<_> = app
            .world_mut()
            .query::<(Entity, &Health)>()
            .iter(app.world())
            .filter(|(_, h)| h.current == 0 || h.current > h.max)
            .map(|(e, h)| (e, h.current, h.max))
            .collect();
        assert!(bad.is_empty(), "tick {t}: bad HP {bad:?}");
    }
}

// ---- P7: engagement boundaries ---------------------------------------------

/// `engage_range` is inclusive on acquisition and exclusive one epsilon beyond;
/// `mvp_attack_range` is inclusive for the hit itself.
#[test]
fn engage_and_attack_ranges_are_inclusive_at_their_boundary() {
    let engage = content().combat.engage_range;

    // Exactly at the engage radius: acquired.
    let mut app = sim_app();
    let h = spawn_unit(&mut app, "sentinel", Faction::A, Vec2::ZERO);
    spawn_unit(&mut app, "worker", Faction::B, Vec2::new(engage, 0.0));
    step(&mut app);
    assert!(
        app.world().get::<Target>(h).is_some(),
        "an enemy at exactly engage_range is engaged"
    );

    // Just beyond it: untouched.
    let mut app = sim_app();
    let h = spawn_unit(&mut app, "sentinel", Faction::A, Vec2::ZERO);
    spawn_unit(&mut app, "worker", Faction::B, Vec2::new(engage + 0.5, 0.0));
    step(&mut app);
    assert!(app.world().get::<Target>(h).is_none(), "not engaged");
    assert!(app.world().get::<MoveTarget>(h).is_none(), "not chased");

    // Exactly at attack range: hits without moving.
    let mut app = sim_app();
    let range = content().unit("sentinel").unwrap().mvp_attack_range;
    let h = spawn_unit(&mut app, "sentinel", Faction::A, Vec2::ZERO);
    let v = spawn_unit(&mut app, "worker", Faction::B, Vec2::new(range, 0.0));
    let full = hp(&app, v).unwrap();
    let per_hit = damage_per_hit(
        app.world().resource::<Content>(),
        def_of(&app, "sentinel"),
        def_of(&app, "worker"),
    );
    step(&mut app);
    assert_eq!(hp(&app, v), Some(full - per_hit), "hit at exactly reach");
    assert_eq!(pos(&app, h), Some(Vec2::ZERO), "no step taken");
}

// ---- P8: the leash ----------------------------------------------------------

/// A prey that outruns its hunter is dropped once past `pursue_range`, and the
/// hunter then stands still instead of oscillating at the leash edge.
#[test]
fn a_hunter_drops_a_prey_that_outruns_the_leash_and_does_not_oscillate() {
    let mut app = sim_app();
    let hunter = spawn_unit(&mut app, "bulwark", Faction::A, Vec2::ZERO);
    let prey = spawn_unit(&mut app, "worker", Faction::B, Vec2::new(100.0, 0.0));
    push(
        &mut app,
        Order::MoveTo {
            units: vec![prey],
            dest: Vec2::new(20_000.0, 0.0),
        },
    );
    step(&mut app);
    assert!(app.world().get::<Engaging>(hunter).is_some(), "gave chase");

    tick(&mut app, 600);
    let gap = pos(&app, hunter).unwrap().distance(pos(&app, prey).unwrap());
    assert!(
        gap > content().combat.pursue_range,
        "precondition: the prey got away ({gap})"
    );
    assert!(app.world().get::<Engaging>(hunter).is_none(), "leash broke");
    assert!(app.world().get::<MoveTarget>(hunter).is_none());
    assert!(app.world().get::<Target>(hunter).is_none());

    let parked = pos(&app, hunter).unwrap();
    tick(&mut app, 120);
    assert_eq!(pos(&app, hunter), Some(parked), "no oscillation after giving up");
}

// ---- P9: determinism under archetype perturbation ---------------------------

#[derive(Component)]
struct Tag;

/// Outcomes must not depend on ECS iteration order. Same entities, same spawn
/// order, but alternating units carry an extra component so the query walks a
/// different archetype order.
#[test]
fn combat_outcome_is_independent_of_archetype_iteration_order() {
    fn run(tagged: bool) -> Vec<(u64, Option<u32>)> {
        let mut app = sim_app();
        let roster = [
            ("bulwark", Faction::A, Vec2::new(0.0, 0.0)),
            ("ripper", Faction::B, Vec2::new(30.0, 0.0)),
            ("sentinel", Faction::A, Vec2::new(0.0, 40.0)),
            ("ravager", Faction::B, Vec2::new(35.0, 40.0)),
            ("arclight", Faction::A, Vec2::new(-20.0, 20.0)),
            ("ripper", Faction::B, Vec2::new(60.0, 20.0)),
        ];
        let mut es = Vec::new();
        for (i, (id, f, p)) in roster.iter().enumerate() {
            let e = spawn_unit(&mut app, id, *f, *p);
            if tagged && i % 2 == 0 {
                app.world_mut().entity_mut(e).insert(Tag);
            }
            es.push(e);
        }
        tick(&mut app, 600);
        let cas = *app.world().resource::<Casualties>();
        let mut out: Vec<(u64, Option<u32>)> =
            es.iter().map(|&e| (e.to_bits(), hp(&app, e))).collect();
        out.push((u64::MAX, Some(cas.total())));
        out
    }
    assert_eq!(run(false), run(true), "iteration order changed the outcome");
    assert_eq!(run(false), run(false), "same input, same outcome");
}

// ---- P10: the 4-stat scaling really is data --------------------------------

/// Every scaling factor must come from `mvp_combat`, not Rust. Change the RON,
/// the numbers change by exactly that factor.
#[test]
fn the_stat_scaling_is_read_from_the_ron_not_hardcoded() {
    let base = content();
    let bulwark = base.unit_index("bulwark").unwrap();
    let ripper = base.unit_index("ripper").unwrap();

    let hp2 = mutated("hp40", "hp_per_defense: 20,", "hp_per_defense: 40,");
    assert_eq!(
        Health::from_def(&hp2, bulwark).max,
        2 * Health::from_def(&base, bulwark).max,
        "the HP pool scales with mvp_combat.hp_per_defense"
    );

    let d3 = mutated("dmg3", "damage_per_offense: 5,", "damage_per_offense: 3,");
    // bulwark offense 4 -> 12, ripper armor 1 * 2 -> 2.
    assert_eq!(damage_per_hit(&d3, bulwark, ripper), 10);

    let m5 = mutated("mit5", "mitigation_per_armor: 2,", "mitigation_per_armor: 5,");
    // bulwark offense 4 * 5 = 20, ripper armor 1 * 5 = 5.
    assert_eq!(damage_per_hit(&m5, bulwark, ripper), 15);

    let s72 = mutated("spd", "speed_per_point: 36.0,", "speed_per_point: 72.0,");
    assert_eq!(
        move_speed(&s72, bulwark),
        2.0 * move_speed(&base, bulwark),
        "Speed is `speed * mvp_combat.speed_per_point`"
    );
}

/// The nemesis multiplier and its armor rule are data too, and the rounding rule
/// is floor of an integer per-mille.
#[test]
fn the_nemesis_bonus_is_read_from_the_ron_and_floors() {
    let c = mutated("mult115", "damage_mult: 1.3,", "damage_mult: 1.15,");
    let (b, r) = (
        c.unit_index("bulwark").unwrap(),
        c.unit_index("ravager").unwrap(),
    );
    assert_eq!(c.nemesis_bonus.mult_milli(), 1_150);
    // base 20; floor(20 * 1150 / 1000) = 23; armor still ignored.
    assert_eq!(damage_per_hit(&c, b, r), 23);

    let c = mutated("keeparmor", "ignore_armor: true,", "ignore_armor: false,");
    // floor(20 * 1.3) = 26, then the Ravager's 4 armor * 2 = 8 is subtracted.
    assert_eq!(damage_per_hit(&c, b, r), 18);
}

// ---- P11: load-time validation of the new content ---------------------------

#[test]
fn degenerate_or_missing_combat_content_is_rejected_at_load() {
    for (name, from, to) in [
        ("hp0", "hp_per_defense: 20,", "hp_per_defense: 0,"),
        ("dmg0", "damage_per_offense: 5,", "damage_per_offense: 0,"),
        ("mit0", "mitigation_per_armor: 2,", "mitigation_per_armor: 0,"),
        ("spd0", "speed_per_point: 36.0,", "speed_per_point: 0.0,"),
        ("leash", "pursue_range: 400.0,", "pursue_range: 100.0,"),
        ("nocadence", "mvp_attack_ticks: 90,", ""),
        ("noblock", "mvp_combat: (", "mvp_combat_typo: ("),
        ("weakmult", "damage_mult: 1.3,", "damage_mult: 0.5,"),
        ("badprey", "nemesis: \"ravager\"", "nemesis: \"nobody\""),
    ] {
        assert!(
            load_mutated(name, from, to).is_err(),
            "`{name}` must be rejected at load, not silently defaulted"
        );
    }
}

// ---- P12: the global SPEED constant is retired ------------------------------

#[test]
fn no_global_speed_constant_survives_in_src() {
    let mut offenders = Vec::new();
    for entry in walk(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")) {
        let text = std::fs::read_to_string(&entry).unwrap();
        for line in text.lines() {
            let l = line.trim_start();
            if l.starts_with("//") {
                continue;
            }
            if l.contains("const SPEED") {
                offenders.push(format!("{}: {line}", entry.display()));
            }
        }
    }
    assert!(offenders.is_empty(), "sim::SPEED not retired: {offenders:?}");

    // ...and each unit really moves at its own rate.
    let c = content();
    let mut app = sim_app();
    let dt = 1.0 / 60.0;
    for id in ["worker", "bulwark", "sentinel", "ripper", "ravager", "arclight"] {
        let e = spawn_unit(&mut app, id, Faction::A, Vec2::ZERO);
        app.world_mut()
            .entity_mut(e)
            .insert(MoveTarget(Vec2::new(100_000.0, 0.0)));
        step(&mut app);
        let expect = c.unit(id).unwrap().speed as f32 * c.combat.speed_per_point * dt;
        let moved = pos(&app, e).unwrap().x;
        assert!(
            (moved - expect).abs() < 1e-2,
            "{id}: moved {moved}, expected {expect}"
        );
        app.world_mut().entity_mut(e).despawn();
    }
}

fn walk(dir: &PathBuf) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
    out.sort();
    out
}

// ---- P13: the cooldown never lets a unit fire faster than its cadence -------

#[test]
fn a_unit_never_fires_faster_than_its_ron_cadence() {
    let mut app = sim_app();
    let cadence = content().unit("ripper").unwrap().mvp_attack_ticks;
    let a = spawn_unit(&mut app, "ripper", Faction::A, Vec2::ZERO);
    let v = spawn_unit(&mut app, "bulwark", Faction::B, Vec2::new(20.0, 0.0));
    // Keep the victim topped up so it never dies; count when HP drops.
    let full = hp(&app, v).unwrap();
    let mut hit_ticks = Vec::new();
    for t in 0..600u32 {
        step(&mut app);
        if hp(&app, v) != Some(full) {
            hit_ticks.push(t);
            set_hp(&mut app, v, full);
        }
        // The attacker must not be able to hit while its cooldown is nonzero.
        if let Some(cd) = app.world().get::<AttackCooldown>(a) {
            assert!(cd.0 <= cadence, "tick {t}: cooldown {} > cadence", cd.0);
        }
    }
    assert!(hit_ticks.len() > 3, "it did attack: {hit_ticks:?}");
    for w in hit_ticks.windows(2) {
        assert_eq!(
            w[1] - w[0],
            cadence,
            "hits must be exactly {cadence} ticks apart: {hit_ticks:?}"
        );
    }
}
