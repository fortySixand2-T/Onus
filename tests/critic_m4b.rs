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

// ============================================================================
// Critic pass 2 — probes on the fix diff (12e9ba0..HEAD).
// ============================================================================

use onus::sim::spatial::{brute_force_nearest_enemy, Unit};

/// Live snapshot of every unit, sorted by entity bits — the same order the
/// combat snapshot uses, so index i here is index i there.
fn layout(app: &mut App) -> Vec<(Entity, Unit)> {
    let mut v: Vec<(Entity, Unit)> = app
        .world_mut()
        .query::<(Entity, &Position, &Faction)>()
        .iter(app.world())
        .map(|(e, p, f)| {
            (
                e,
                Unit {
                    pos: p.0,
                    faction: *f,
                },
            )
        })
        .collect();
    v.sort_unstable_by_key(|(e, _)| e.to_bits());
    v
}

fn targets(app: &mut App) -> Vec<(Entity, Entity)> {
    let mut v: Vec<(Entity, Entity)> = app
        .world_mut()
        .query::<(Entity, &Target)>()
        .iter(app.world())
        .map(|(e, t)| (e, t.0))
        .collect();
    v.sort_unstable_by_key(|(e, _)| e.to_bits());
    v
}

/// Whole-world fingerprint at a tick boundary, including the engagement graph.
fn state_hash(app: &mut App) -> u64 {
    let mut rows: Vec<(u64, u32, u32, u64, u32)> = app
        .world_mut()
        .query::<(Entity, &Position, Option<&Health>, Option<&Target>, Option<&AttackCooldown>)>()
        .iter(app.world())
        .map(|(e, p, h, t, cd)| {
            (
                e.to_bits(),
                p.0.x.to_bits(),
                p.0.y.to_bits(),
                t.map(|t| t.0.to_bits()).unwrap_or(u64::MAX),
                h.map(|h| h.current).unwrap_or(u32::MAX) ^ (cd.map(|c| c.0).unwrap_or(u32::MAX) << 1),
            )
        })
        .collect();
    rows.sort_unstable();
    let mut acc = 0xcbf2_9ce4_8422_2325u64;
    for r in rows {
        for w in [r.0, r.1 as u64, r.2 as u64, r.3, r.4 as u64] {
            acc ^= w;
            acc = acc.wrapping_mul(0x1000_0000_01b3);
        }
    }
    acc
}

// ---- P16-P21: the commander owns the unit's orders, on every tick ----------

/// The fix removes `Engaging` when an order arrives. Probe the *boundary*: the
/// order is issued on the very tick the chase would begin, so the unit has no
/// `Engaging` yet and the order must still win.
#[test]
fn an_order_issued_on_the_tick_a_chase_would_begin_wins() {
    let mut app = sim_app();
    let hunter = spawn_unit(&mut app, "sentinel", Faction::A, Vec2::ZERO);
    let _prey = spawn_unit(&mut app, "worker", Faction::B, Vec2::new(150.0, 0.0));
    let dest = Vec2::new(-500.0, 0.0);
    push(
        &mut app,
        Order::MoveTo {
            units: vec![hunter],
            dest,
        },
    );
    step(&mut app);
    assert!(
        app.world().get::<Engaging>(hunter).is_none(),
        "combat must not start a chase on a unit that was just ordered"
    );
    assert_eq!(
        app.world().get::<MoveTarget>(hunter).map(|m| m.0),
        Some(dest),
        "the commanded destination must survive the tick's combat pass"
    );
}

/// The order must keep winning on *every* subsequent tick, not just the one it
/// arrived on: a fix that only clears `Engaging` once would let combat
/// re-acquire and overwrite the destination one tick later.
#[test]
fn the_commanded_destination_survives_every_later_tick() {
    let mut app = sim_app();
    let hunter = spawn_unit(&mut app, "sentinel", Faction::A, Vec2::new(0.0, 0.0));
    // Prey parked right beside the commanded path, permanently in engage range.
    let _prey = spawn_unit(&mut app, "worker", Faction::B, Vec2::new(-150.0, 40.0));
    step(&mut app);
    assert!(
        app.world().get::<Engaging>(hunter).is_some(),
        "precondition: the sentinel auto-chased first"
    );
    let dest = Vec2::new(-500.0, 0.0);
    push(
        &mut app,
        Order::MoveTo {
            units: vec![hunter],
            dest,
        },
    );
    for t in 0..300u32 {
        step(&mut app);
        if pos(&app, hunter).unwrap().distance(dest) <= onus::sim::STOP_EPS {
            return; // arrived: the order was honoured start to finish
        }
        assert!(
            app.world().get::<Engaging>(hunter).is_none(),
            "tick {t}: combat re-acquired a unit that has standing orders"
        );
        assert_eq!(
            app.world().get::<MoveTarget>(hunter).map(|m| m.0),
            Some(dest),
            "tick {t}: combat overwrote the commanded destination"
        );
    }
    panic!("the unit never reached the commanded destination");
}

/// An order issued on the exact tick the chased target dies: combat's
/// "nothing left to chase" branch must not take the commander's `MoveTarget`
/// with it. `hunter` is mid-chase; `killer` lands the fatal blow on the same
/// tick the order is applied.
#[test]
fn an_order_issued_on_the_tick_the_target_dies_is_kept() {
    let mut app = sim_app();
    let hunter = spawn_unit(&mut app, "sentinel", Faction::A, Vec2::new(150.0, 0.0));
    let killer = spawn_unit(&mut app, "sentinel", Faction::A, Vec2::new(50.0, 0.0));
    let prey = spawn_unit(&mut app, "worker", Faction::B, Vec2::ZERO);
    step(&mut app);
    assert!(
        app.world().get::<Engaging>(hunter).is_some(),
        "precondition: the hunter is chasing"
    );
    // Arm the killer for this tick and make the next hit lethal.
    set_hp(&mut app, prey, 1);
    app.world_mut()
        .entity_mut(killer)
        .insert(AttackCooldown(0));
    let dest = Vec2::new(600.0, 0.0);
    push(
        &mut app,
        Order::MoveTo {
            units: vec![hunter],
            dest,
        },
    );
    step(&mut app);
    assert!(!alive(&app, prey), "precondition: the prey died on this tick");
    assert!(
        app.world().get::<Engaging>(hunter).is_none(),
        "the order cancelled the chase"
    );
    assert_eq!(
        app.world().get::<MoveTarget>(hunter).map(|m| m.0),
        Some(dest),
        "the death pass must not drop the commander's destination"
    );
    assert!(
        app.world().get::<Target>(hunter).is_none(),
        "and no engagement may survive pointing at the dead prey"
    );
}

/// A `Gather` order is an explicit order too: it must cancel a chase in flight
/// and its destination must survive the same tick's combat pass.
#[test]
fn a_gather_order_also_cancels_a_chase_in_progress() {
    let mut app = sim_app();
    let hunter = spawn_unit(&mut app, "sentinel", Faction::A, Vec2::ZERO);
    let _prey = spawn_unit(&mut app, "worker", Faction::B, Vec2::new(150.0, 0.0));
    let node_pos = Vec2::new(-300.0, 0.0);
    let node = app
        .world_mut()
        .spawn((
            Position(node_pos),
            onus::sim::ResourceNode { amount: 500 },
        ))
        .id();
    step(&mut app);
    assert!(app.world().get::<Engaging>(hunter).is_some(), "precondition");
    push(
        &mut app,
        Order::Gather {
            units: vec![hunter],
            node,
            node_pos,
        },
    );
    step(&mut app);
    assert!(
        app.world().get::<Engaging>(hunter).is_none(),
        "a gather order must end the chase"
    );
    assert_eq!(
        app.world().get::<MoveTarget>(hunter).map(|m| m.0),
        Some(node_pos),
        "the gather destination must survive the combat pass"
    );
}

/// Order / re-engage / order again, many times over. Every order must win on
/// the tick it lands, no matter what combat state the unit was in.
#[test]
fn repeated_order_and_reengage_cycles_never_let_combat_win() {
    let mut app = sim_app();
    let hunter = spawn_unit(&mut app, "sentinel", Faction::A, Vec2::ZERO);
    let _prey = spawn_unit(&mut app, "worker", Faction::B, Vec2::new(120.0, 0.0));
    for cycle in 0..6u32 {
        // Let it re-acquire on its own for a few ticks.
        tick(&mut app, 5);
        let dest = Vec2::new(-200.0 - cycle as f32 * 5.0, 30.0 * cycle as f32);
        push(
            &mut app,
            Order::MoveTo {
                units: vec![hunter],
                dest,
            },
        );
        step(&mut app);
        assert!(
            app.world().get::<Engaging>(hunter).is_none(),
            "cycle {cycle}: chase not cancelled"
        );
        assert_eq!(
            app.world().get::<MoveTarget>(hunter).map(|m| m.0),
            Some(dest),
            "cycle {cycle}: destination overwritten by combat"
        );
    }
}

// ---- P22-P24: the load-time bound and the saturating backstop must agree ---

fn load_with_combat(name: &str, edits: &[(&str, &str)]) -> Result<Content, String> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/critic_m4b_pass2")
        .join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let mut units = std::fs::read_to_string(data_dir().join("units.ron")).unwrap();
    for (from, to) in edits {
        assert!(units.contains(from), "anchor `{from}` missing");
        units = units.replace(from, to);
    }
    std::fs::write(dir.join("units.ron"), units).unwrap();
    std::fs::copy(data_dir().join("resources.ron"), dir.join("resources.ron")).unwrap();
    Content::load_from_dir(&dir).map_err(|e| e.to_string())
}

/// `max_stat` is data: the shipped roster must fit under it, a stat above it
/// must be refused, and lowering it below a shipped stat must refuse the roster.
#[test]
fn max_stat_is_data_and_bounds_exactly_the_design_scale() {
    assert!(content().combat.max_stat >= 10, "the 1-10 scale must fit");
    assert!(
        load_with_combat("stat_11", &[("speed: 7, offense: 6,", "speed: 7, offense: 11,")]).is_err(),
        "a stat above the declared scale must be rejected"
    );
    assert!(
        load_with_combat("max_8", &[("max_stat: 10,", "max_stat: 8,")]).is_err(),
        "lowering max_stat below a shipped stat (arclight offense 9) must reject"
    );
    assert!(
        load_with_combat("max_10", &[("max_stat: 10,", "max_stat: 10,  ")]).is_ok(),
        "the shipped roster must still load"
    );
}

/// The loader's job is to *reject* content it cannot represent, and rejection
/// means `Err` — not an arithmetic overflow inside the validator itself. The
/// peak-damage check multiplies three unbounded data fields in `u64`.
#[test]
fn a_degenerate_scale_is_rejected_not_overflowed_inside_the_validator() {
    let r = load_with_combat(
        "degenerate_scale",
        &[
            ("max_stat: 10,", "max_stat: 4000000000,"),
            ("damage_per_offense: 5,", "damage_per_offense: 4000000000,"),
        ],
    );
    assert!(
        r.is_err(),
        "content whose peak damage cannot be represented must be rejected"
    );
}

/// The load-time bound and the saturating backstop must agree about what is
/// legal: content the loader *accepts* must never need saturation to evaluate.
#[test]
fn accepted_content_never_needs_a_saturating_hit() {
    // max_stat * damage_per_offense * mult_milli = 2^27 * 2^26 * 2048 = 2^64.
    let c = load_with_combat(
        "wrap_to_zero",
        &[
            ("max_stat: 10,", "max_stat: 134217728,"),
            ("damage_per_offense: 5,", "damage_per_offense: 67108864,"),
            ("damage_mult: 1.3,", "damage_mult: 2.048,"),
            ("hp_per_defense: 20,", "hp_per_defense: 1,"),
        ],
    );
    let Ok(c) = c else {
        return; // rejected — the validator and the backstop agree.
    };
    let bulwark = c.unit_index("bulwark").unwrap();
    let mut c2 = c.clone();
    c2.units[bulwark].offense = c.combat.max_stat; // legal by the loader's own bound
    let base = (c2.units[bulwark].offense as u64) * (c2.combat.damage_per_offense as u64);
    assert!(
        base <= u32::MAX as u64,
        "the loader accepted a scale whose base damage ({base}) cannot be \
         represented in the u32 the sim counts in — the bound and the \
         saturating backstop disagree about what is legal"
    );
}

// ---- P25-P28: the deferred `Target` write must not perturb the sim ---------

/// Differential oracle: every published engagement must be the brute-force
/// nearest enemy computed on the *start-of-tick* snapshot. Deferring the
/// `Target` write must not have changed who gets picked.
#[test]
fn every_published_target_is_the_brute_force_nearest_enemy() {
    let mut app = sim_app();
    let roster = ["bulwark", "sentinel", "ripper", "ravager", "arclight"];
    let mut rng = onus::sim::SplitMix64::new(0xA5A5_1234);
    for k in 0..10 {
        let f = if k % 2 == 0 { Faction::A } else { Faction::B };
        let x = (rng.next_u64() % 400) as f32 - 200.0 + if k % 2 == 0 { -60.0 } else { 60.0 };
        let y = (rng.next_u64() % 400) as f32 - 200.0 + k as f32 * 0.37;
        spawn_unit(&mut app, roster[k % roster.len()], f, Vec2::new(x, y));
    }
    for t in 0..600u32 {
        let before = layout(&mut app);
        step(&mut app);
        for (e, tgt) in targets(&mut app) {
            let i = before
                .iter()
                .position(|(x, _)| *x == e)
                .unwrap_or_else(|| panic!("tick {t}: target holder {e:?} not in snapshot"));
            let units: Vec<Unit> = before.iter().map(|(_, u)| *u).collect();
            let expect = brute_force_nearest_enemy(&units, i)
                .map(|j| before[j].0)
                .unwrap_or_else(|| panic!("tick {t}: {e:?} has a target but no enemy existed"));
            assert_eq!(
                tgt, expect,
                "tick {t}: {e:?} engaged {tgt:?}, nearest enemy was {expect:?}"
            );
        }
    }
}

/// No `Target` may dangle, and none may be spuriously dropped: a living unit
/// whose start-of-tick nearest enemy is still alive and inside engage range
/// must end the tick holding that engagement.
#[test]
fn a_multi_kill_melee_leaves_no_dangling_and_no_missing_engagement() {
    let mut app = sim_app();
    let mut ids = Vec::new();
    for k in 0..8u32 {
        let a = spawn_unit(
            &mut app,
            "ripper",
            Faction::A,
            Vec2::new(-30.0, k as f32 * 9.0),
        );
        let b = spawn_unit(
            &mut app,
            "ripper",
            Faction::B,
            Vec2::new(30.0, k as f32 * 9.0 + 1.0),
        );
        ids.push(a);
        ids.push(b);
    }
    // Everyone one hit from death, so a tick kills many at once.
    for e in &ids {
        set_hp(&mut app, *e, 1);
    }
    let engage = app.world().resource::<Content>().combat.engage_range;
    for t in 0..300u32 {
        let before = layout(&mut app);
        step(&mut app);
        for (e, tgt) in targets(&mut app) {
            assert!(
                alive(&app, tgt),
                "tick {t}: {e:?} holds a Target on the despawned {tgt:?}"
            );
            assert!(alive(&app, e), "tick {t}: dead {e:?} still holds a Target");
        }
        // No spurious drops.
        let units: Vec<Unit> = before.iter().map(|(_, u)| *u).collect();
        let held = targets(&mut app);
        for (i, (e, u)) in before.iter().enumerate() {
            if !alive(&app, *e) {
                continue;
            }
            let Some(j) = brute_force_nearest_enemy(&units, i) else {
                continue;
            };
            if !alive(&app, before[j].0) || u.pos.distance(before[j].1.pos) > engage {
                continue;
            }
            assert!(
                held.iter().any(|(h, _)| h == e),
                "tick {t}: living {e:?} lost its engagement with the living, \
                 in-range {:?}",
                before[j].0
            );
        }
        if before.len() <= 1 {
            break;
        }
    }
}

/// Determinism, including the engagement graph: identical setups produce
/// identical per-tick state hashes.
#[test]
fn per_tick_state_hashes_match_across_identical_runs() {
    fn run() -> Vec<u64> {
        let mut app = sim_app();
        let roster = ["bulwark", "sentinel", "ripper", "ravager", "arclight"];
        for k in 0..10usize {
            let f = if k % 2 == 0 { Faction::A } else { Faction::B };
            let x = if k % 2 == 0 { -80.0 } else { 80.0 } + k as f32 * 3.1;
            spawn_unit(&mut app, roster[k % 5], f, Vec2::new(x, k as f32 * 11.0));
        }
        (0..400)
            .map(|_| {
                step(&mut app);
                state_hash(&mut app)
            })
            .collect()
    }
    assert_eq!(run(), run(), "the sim is not deterministic tick-for-tick");
}

/// Conservation: the casualty ledger equals the number of entities that
/// actually left the world, on every tick.
#[test]
fn casualties_equal_the_units_that_actually_despawned() {
    let mut app = sim_app();
    let roster = ["bulwark", "sentinel", "ripper", "ravager", "arclight"];
    let mut spawned = 0u32;
    for k in 0..10usize {
        let f = if k % 2 == 0 { Faction::A } else { Faction::B };
        let x = if k % 2 == 0 { -40.0 } else { 40.0 };
        spawn_unit(&mut app, roster[k % 5], f, Vec2::new(x, k as f32 * 7.0));
        spawned += 1;
    }
    for t in 0..1200u32 {
        step(&mut app);
        let alive_now = layout(&mut app).len() as u32;
        let lost = app.world().resource::<Casualties>().total();
        assert_eq!(
            spawned - alive_now,
            lost,
            "tick {t}: {} units gone but {lost} casualties recorded",
            spawned - alive_now
        );
    }
}

// ---- P28-P31: critic pass 3 — the gather exclusion and the last raw cast ----

use onus::sim::ResourceNode;

/// AC1 says a unit engages the nearest enemy in range. The new gather rule
/// excludes a unit that is *on a gather job* — but `Order::Gather` is accepted
/// for any entity, and `input::emit_commands` sends it to the whole selection
/// (right-clicking a node with a mixed group). A unit whose definition has
/// `gathers: false` gets a `GatherTarget` it can never act on: `economy::gather`
/// skips it (`if !def.gathers { continue; }`) *without clearing the component*.
/// So the marker sticks forever and the soldier is permanently disarmed — it
/// never acquires a target, never fires, and stands there being killed. The
/// rule must key on "has a job the economy is actually running", not "carries a
/// GatherTarget".
#[test]
fn a_gather_order_does_not_permanently_disarm_a_non_gathering_soldier() {
    // Control: no order at all — the soldier engages, as AC1 requires.
    let mut ctl = sim_app();
    let a0 = spawn_unit(&mut ctl, "ripper", Faction::A, Vec2::new(0.0, 0.0));
    let b0 = spawn_unit(&mut ctl, "worker", Faction::B, Vec2::new(30.0, 0.0));
    let full = hp(&ctl, b0).unwrap();
    tick(&mut ctl, 120);
    assert!(
        !alive(&ctl, b0) || hp(&ctl, b0).unwrap() < full,
        "control: an idle soldier next to an enemy must fight"
    );
    assert!(alive(&ctl, a0));

    // Now the same soldier, handed a Gather order it can never execute.
    let mut app = sim_app();
    let soldier = spawn_unit(&mut app, "ripper", Faction::A, Vec2::new(0.0, 0.0));
    let enemy = spawn_unit(&mut app, "worker", Faction::B, Vec2::new(30.0, 0.0));
    let node = app
        .world_mut()
        .spawn((Position(Vec2::new(0.0, 0.0)), ResourceNode { amount: 500 }))
        .id();
    push(
        &mut app,
        Order::Gather {
            units: vec![soldier],
            node,
            node_pos: Vec2::new(0.0, 0.0),
        },
    );
    let full = hp(&app, enemy).unwrap();
    tick(&mut app, 300);

    assert!(
        !alive(&app, enemy) || hp(&app, enemy).unwrap() < full,
        "a soldier that cannot gather was permanently disarmed by a Gather order: \
         300 ticks next to an enemy and it never fired"
    );
}

/// The exclusion must remove the gatherer as an *attacker*, never as a
/// *target*: a worker mid-harvest is still killable (AC1 — death despawns).
#[test]
fn a_gathering_unit_is_still_a_valid_target() {
    let mut app = sim_app();
    let worker = spawn_unit(&mut app, "worker", Faction::A, Vec2::new(0.0, 0.0));
    let killer = spawn_unit(&mut app, "ripper", Faction::B, Vec2::new(30.0, 0.0));
    let node = app
        .world_mut()
        .spawn((Position(Vec2::new(0.0, 0.0)), ResourceNode { amount: 500 }))
        .id();
    push(
        &mut app,
        Order::Gather {
            units: vec![worker],
            node,
            node_pos: Vec2::new(0.0, 0.0),
        },
    );
    tick(&mut app, 200);
    assert!(
        !alive(&app, worker),
        "a gathering worker must still be killable"
    );
    assert!(alive(&app, killer));
    assert_eq!(app.world().resource::<Casualties>().lost(Faction::A), 1);
}

/// An armed gatherer whose job *ends* re-arms: the exclusion is scoped to the
/// job, not permanent. Here the node is emptied, so `economy::gather` drops the
/// job and the unit must go back to engaging.
#[test]
fn a_gatherer_re_arms_when_the_job_ends() {
    let mut c = content();
    let w = c.unit_index("worker").unwrap();
    c.units[w].offense = 6;
    c.units[w].mvp_attack_ticks = 40;
    c.units[w].mvp_attack_range = 40.0;
    c.units[w].mvp_carry_capacity = 10;
    // A harmless enemy, so what this measures is our gatherer re-arming and not
    // a duel it loses first.
    let mut dummy = c.units[w].clone();
    dummy.id = "bystander".to_string();
    dummy.offense = 0;
    dummy.mvp_attack_ticks = 0;
    dummy.mvp_attack_range = 0.0;
    dummy.gathers = false;
    c.units.push(dummy);
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .insert_resource(Time::<Fixed>::from_hz(60.0))
        .insert_resource(c)
        .init_resource::<CommandQueue>()
        .init_resource::<RateReport>()
        .insert_resource(Stockpiles::default());
    onus::add_sim_systems(&mut app, Update);

    let worker = spawn_unit(&mut app, "worker", Faction::A, Vec2::new(0.0, 0.0));
    let enemy = spawn_unit(&mut app, "bystander", Faction::B, Vec2::new(30.0, 0.0));
    // A node with nothing in it: the first harvest ends the job on the spot.
    let node = app
        .world_mut()
        .spawn((Position(Vec2::new(0.0, 0.0)), ResourceNode { amount: 0 }))
        .id();
    push(
        &mut app,
        Order::Gather {
            units: vec![worker],
            node,
            node_pos: Vec2::new(0.0, 0.0),
        },
    );
    let full = hp(&app, enemy).unwrap();
    tick(&mut app, 300);
    assert!(
        !alive(&app, enemy) || hp(&app, enemy).unwrap() < full,
        "the gather job ended; the unit must be armed again"
    );
}

/// `NemesisBonus::mult_milli` is a **raw saturating cast of an unbounded RON
/// float** — the last one on the damage path. `validate` only requires
/// `damage_mult` to be finite and >= 1.0, so `damage_mult: 5000000.0` is
/// "legal", but `(5e6 * 1000.0).round() as u32` saturates to `u32::MAX` and the
/// sim quietly applies a 4_294_967.295x multiplier instead. The loader and the
/// arithmetic disagree about what the data means: either the value must be
/// rejected, or the documented formula
/// `floor(base * round(damage_mult * 1000) / 1000)` must hold.
#[test]
fn a_nemesis_multiplier_is_rejected_or_applied_as_written() {
    let big = 5_000_000.0f64;
    let Ok(c) = load_mutated("huge_mult", "damage_mult: 1.3,", "damage_mult: 5000000.0,") else {
        return; // rejected at load — also a correct answer.
    };
    let (attacker, defender) = (
        c.unit_index("ripper").unwrap(),
        c.unit_index("arclight").unwrap(),
    );
    let base = c.units[attacker].offense as f64 * c.combat.damage_per_offense as f64;
    let want = (base * big).floor();
    let got = damage_per_hit(&c, attacker, defender) as f64;
    assert_eq!(
        got, want,
        "content the loader called legal is applied with a silently truncated \
         multiplier (wanted {want}, got {got})"
    );
}

// ---- P32-P44: critic pass 4 — the gather-ownership fix and the multiplier ----

use onus::sim::economy::{Building, Carrying, GatherPhase, ProductionQueue};
use onus::sim::GatherTarget;

fn has_job(app: &App, e: Entity) -> bool {
    app.world().get::<GatherTarget>(e).is_some()
}

fn phase(app: &App, e: Entity) -> Option<GatherPhase> {
    app.world().get::<GatherPhase>(e).copied()
}

fn carried(app: &App, e: Entity) -> u32 {
    app.world().get::<Carrying>(e).map(|c| c.0).unwrap_or(0)
}

fn node_amount(app: &App, e: Entity) -> u32 {
    app.world().get::<ResourceNode>(e).map(|n| n.amount).unwrap_or(0)
}

fn spawn_node(app: &mut App, pos: Vec2, amount: u32) -> Entity {
    app.world_mut()
        .spawn((Position(pos), ResourceNode { amount }))
        .id()
}

fn spawn_hq(app: &mut App, pos: Vec2, faction: Faction) -> Entity {
    let def = app
        .world()
        .resource::<Content>()
        .building_index("hq")
        .expect("hq");
    app.world_mut()
        .spawn((
            Position(pos),
            Building { def },
            faction,
            ProductionQueue::default(),
        ))
        .id()
}

/// P32. `Order::Gather` now looks the unit's definition up in a read-only query.
/// An entity with no `UnitDefIdx` at all must simply take the move half: it may
/// never be handed a gather claim the economy can never see (the economy's
/// worker query *requires* `UnitDefIdx`, so a marker planted on such an entity
/// is unreleasable — the exact F-008 shape).
#[test]
fn a_gather_order_to_an_entity_without_a_definition_hands_out_no_claim() {
    let mut app = sim_app();
    let e = app
        .world_mut()
        .spawn((Position(Vec2::ZERO), Faction::A))
        .id();
    let node = spawn_node(&mut app, Vec2::new(100.0, 0.0), 500);
    push(
        &mut app,
        Order::Gather {
            units: vec![e],
            node,
            node_pos: Vec2::new(100.0, 0.0),
        },
    );
    tick(&mut app, 5);
    assert!(
        !has_job(&app, e) && phase(&app, e).is_none(),
        "an entity with no definition was given an unreleasable gather claim"
    );
    assert!(
        app.world().get::<MoveTarget>(e).is_some(),
        "the move half of the order must still apply"
    );
}

/// P33. Same, for a definition index that is out of range: `content.units.get`
/// returns `None`, so the sim has no data saying this thing gathers.
#[test]
fn a_gather_order_with_an_out_of_range_definition_hands_out_no_claim() {
    let mut app = sim_app();
    let e = app
        .world_mut()
        .spawn((Position(Vec2::ZERO), UnitDefIdx(9_999), Faction::A))
        .id();
    let node = spawn_node(&mut app, Vec2::new(100.0, 0.0), 500);
    push(
        &mut app,
        Order::Gather {
            units: vec![e],
            node,
            node_pos: Vec2::new(100.0, 0.0),
        },
    );
    tick(&mut app, 5);
    assert!(
        !has_job(&app, e) && phase(&app, e).is_none(),
        "an out-of-range definition index was given a gather claim"
    );
}

/// P34. A claim planted straight onto a soldier — bypassing `Order::Gather`
/// entirely — must be taken back by the economy *before* combat can read it, on
/// the very tick it appears. F-008: the owner releases a claim that does not
/// hold; the reader is never allowed to act on a stale one.
#[test]
fn a_planted_claim_on_a_soldier_is_taken_back_before_combat_reads_it() {
    let mut app = sim_app();
    let soldier = spawn_unit(&mut app, "ripper", Faction::A, Vec2::ZERO);
    let enemy = spawn_unit(&mut app, "worker", Faction::B, Vec2::new(30.0, 0.0));
    let node = spawn_node(&mut app, Vec2::ZERO, 500);
    app.world_mut()
        .entity_mut(soldier)
        .insert((GatherTarget(node), GatherPhase::ToNode));
    let full = hp(&app, enemy).unwrap();

    step(&mut app);
    assert!(
        !has_job(&app, soldier) && phase(&app, soldier).is_none(),
        "the economy did not take back a claim it will never service"
    );
    assert!(
        !alive(&app, enemy) || hp(&app, enemy).unwrap() < full,
        "the planted claim disarmed the soldier for the tick it existed"
    );
}

/// P35. A claim planted on a *building* and on a *resource node*: neither is
/// something the economy's worker query can ever see, so neither may be left
/// holding a claim, and — more importantly — the widened (`Option<&mut
/// Carrying>`) query must not have started touching non-units. The building's
/// production queue and the node's contents must be untouched.
#[test]
fn a_planted_claim_never_makes_the_economy_touch_a_non_unit() {
    let mut app = sim_app();
    let node = spawn_node(&mut app, Vec2::ZERO, 500);
    let hq = spawn_hq(&mut app, Vec2::ZERO, Faction::A);
    app.world_mut()
        .entity_mut(hq)
        .insert((GatherTarget(node), GatherPhase::ToNode));
    app.world_mut()
        .entity_mut(node)
        .insert((GatherTarget(node), GatherPhase::ToNode));
    tick(&mut app, 30);
    assert_eq!(node_amount(&app, node), 500, "a node was mined by nobody");
    assert_eq!(
        app.world().resource::<Stockpiles>().alloy(Faction::A),
        0,
        "Alloy was minted for an entity that never gathered"
    );
    assert_eq!(carried(&app, hq), 0, "a building was given hands");
    assert_eq!(carried(&app, node), 0, "a deposit was given hands");
}

/// P36. AC1 for the mixed selection, across the whole walk: the soldier keeps
/// the *move* half of the order, fires on the tick the order lands, never
/// acquires a gather claim, arrives at the node, and — its order discharged —
/// goes back to auto-engaging. Meanwhile the worker in the same order gathers.
#[test]
fn a_soldier_in_a_mixed_gather_order_walks_and_keeps_firing() {
    let mut app = sim_app();
    let node_pos = Vec2::new(120.0, 0.0);
    let soldier = spawn_unit(&mut app, "ripper", Faction::A, Vec2::new(0.0, 0.0));
    let worker = spawn_unit(&mut app, "worker", Faction::A, Vec2::new(0.0, 20.0));
    // An unarmed enemy: what this measures is the soldier still shooting, not
    // who wins a duel.
    let enemy = spawn_unit(&mut app, "worker", Faction::B, Vec2::new(20.0, 0.0));
    let node = spawn_node(&mut app, node_pos, 500);
    let _hq = spawn_hq(&mut app, Vec2::new(0.0, 20.0), Faction::A);
    push(
        &mut app,
        Order::Gather {
            units: vec![soldier, worker],
            node,
            node_pos,
        },
    );
    let full = hp(&app, enemy).unwrap();
    step(&mut app);
    assert!(
        !has_job(&app, soldier),
        "the soldier was handed a gather claim"
    );
    assert!(has_job(&app, worker), "the worker lost its gather claim");
    assert!(
        hp(&app, enemy).unwrap() < full,
        "the soldier did not fire on the tick the order landed"
    );
    let mut reached_node = false;
    for t in 0..600 {
        step(&mut app);
        assert!(
            !has_job(&app, soldier),
            "tick {t}: a gather claim appeared on the soldier"
        );
        assert_eq!(carried(&app, soldier), 0, "tick {t}: the soldier gathered");
        if let Some(p) = pos(&app, soldier) {
            reached_node |= p.distance(node_pos) < 1.0;
        }
    }
    assert!(
        reached_node,
        "the soldier never obeyed the move half of the order"
    );
    assert!(
        !alive(&app, enemy),
        "the soldier never re-engaged after its order was discharged"
    );
    assert!(
        node_amount(&app, node) < 500,
        "the worker's loop stopped working"
    );
}

/// P37. Alloy conservation (F-005) across the full loop, with the new
/// clear-the-claim pass running every tick and a mixed selection driving it.
/// `stockpile + carried + still-in-deposit` is invariant at every tick.
#[test]
fn alloy_is_conserved_across_a_mixed_selection_gather_loop() {
    let mut app = sim_app();
    let node_pos = Vec2::new(400.0, 0.0);
    let node = spawn_node(&mut app, node_pos, 1_000);
    let _hq = spawn_hq(&mut app, Vec2::ZERO, Faction::A);
    let mut units = Vec::new();
    for i in 0..3 {
        units.push(spawn_unit(
            &mut app,
            "worker",
            Faction::A,
            Vec2::new(0.0, i as f32 * 6.0),
        ));
    }
    for i in 0..2 {
        units.push(spawn_unit(
            &mut app,
            "bulwark",
            Faction::A,
            Vec2::new(0.0, -6.0 * (i + 1) as f32),
        ));
    }
    push(
        &mut app,
        Order::Gather {
            units: units.clone(),
            node,
            node_pos,
        },
    );
    let total = 1_000u32;
    let mut banked_ever = 0u32;
    for t in 0..2_000 {
        step(&mut app);
        let stock = app.world().resource::<Stockpiles>().alloy(Faction::A);
        let hands: u32 = units.iter().map(|&e| carried(&app, e)).sum();
        let left = node_amount(&app, node);
        assert_eq!(
            stock + hands + left,
            total,
            "tick {t}: Alloy was minted or destroyed (stock {stock} + hands \
             {hands} + node {left})"
        );
        assert_eq!(
            app.world().resource::<Stockpiles>().alloy(Faction::B),
            0,
            "tick {t}: the other faction was paid"
        );
        banked_ever = banked_ever.max(stock);
    }
    assert!(banked_ever > 0, "no Alloy was ever banked: the loop is dead");
}

/// P38. The clearing pass must never confiscate a *real* gatherer's job or its
/// carried load: a worker mid-return, re-tasked to the same node on the very
/// tick it is holding Alloy, keeps every unit of it.
#[test]
fn a_carrying_worker_is_never_stripped_by_the_clearing_pass() {
    let mut app = sim_app();
    let node_pos = Vec2::new(200.0, 0.0);
    let node = spawn_node(&mut app, node_pos, 1_000);
    let _hq = spawn_hq(&mut app, Vec2::ZERO, Faction::A);
    let w = spawn_unit(&mut app, "worker", Faction::A, node_pos);
    push(
        &mut app,
        Order::Gather {
            units: vec![w],
            node,
            node_pos,
        },
    );
    // Walk it up to a full load.
    for _ in 0..200 {
        step(&mut app);
        if carried(&app, w) > 0 {
            break;
        }
    }
    assert_eq!(carried(&app, w), 10, "the worker never picked up a load");
    // Re-task every tick while it walks home; the load must survive all of it.
    for t in 0..120 {
        push(
            &mut app,
            Order::Gather {
                units: vec![w],
                node,
                node_pos,
            },
        );
        step(&mut app);
        let banked = app.world().resource::<Stockpiles>().alloy(Faction::A);
        assert_eq!(
            carried(&app, w) + banked,
            10,
            "tick {t}: the load was destroyed by re-tasking"
        );
        assert!(
            has_job(&app, w),
            "tick {t}: a real gatherer's job was confiscated"
        );
    }
}

/// P39. Re-issuing the order every tick to a non-gatherer must not make the
/// claim flicker on and off (a marker that exists for part of a tick is a
/// marker combat can read). The soldier is never on a job at any tick boundary,
/// and it keeps fighting throughout.
#[test]
fn a_repeated_gather_order_never_oscillates_a_soldiers_claim() {
    let mut app = sim_app();
    let soldier = spawn_unit(&mut app, "ripper", Faction::A, Vec2::ZERO);
    let enemy = spawn_unit(&mut app, "bulwark", Faction::B, Vec2::new(20.0, 0.0));
    let node = spawn_node(&mut app, Vec2::ZERO, 500);
    let full = hp(&app, enemy).unwrap();
    for t in 0..200 {
        push(
            &mut app,
            Order::Gather {
                units: vec![soldier],
                node,
                node_pos: Vec2::ZERO,
            },
        );
        step(&mut app);
        assert!(
            !has_job(&app, soldier) && phase(&app, soldier).is_none(),
            "tick {t}: the claim flickered onto a unit that cannot gather"
        );
        if !alive(&app, enemy) {
            break;
        }
    }
    assert!(
        !alive(&app, enemy) || hp(&app, enemy).unwrap() < full,
        "200 re-issued gather orders disarmed the soldier"
    );
}

/// P41. Exactness of the nemesis multiplier. For every multiplier the loader
/// *accepts*, `mult_milli` must equal `round(damage_mult * 1000)` computed from
/// the stored `f32` — never the `u32::MAX` stand-in — and per-hit damage must
/// equal `floor(base * milli / 1000)` exactly. Values it cannot represent must
/// be refused, not silently substituted.
#[test]
fn every_accepted_multiplier_is_applied_exactly_as_written() {
    use onus::sim::content::NemesisBonus;
    let cases = [
        ("m_one", "1.0", 1.0f32),
        ("m_1p0005", "1.0005", 1.0005f32),
        ("m_1p0625", "1.0625", 1.0625f32),
        ("m_1p15", "1.15", 1.15f32),
        ("m_1p3", "1.3", 1.3f32),
        ("m_two", "2.0", 2.0f32),
        ("m_4e6", "4000000.0", 4_000_000.0f32),
        ("m_4294967p5", "4294967.5", 4_294_967.5f32),
        ("m_1e30", "1e30", 1e30f32),
    ];
    for (name, text, value) in cases {
        let loaded = load_mutated(name, "damage_mult: 1.3,", &format!("damage_mult: {text},"));
        let Ok(c) = loaded else { continue }; // a refusal is a correct answer
        let want = (value as f64 * 1_000.0f64).round();
        assert!(
            want.is_finite() && want <= u32::MAX as f64,
            "`{text}` was accepted but cannot be held as an integer per-mille"
        );
        assert_eq!(
            c.nemesis_bonus.milli_exact(),
            Some(want as u32),
            "`{text}`: milli_exact disagrees with round(mult * 1000)"
        );
        assert_eq!(
            c.nemesis_bonus.mult_milli(),
            want as u32,
            "`{text}`: mult_milli disagrees with milli_exact for accepted content"
        );
        assert_ne!(
            c.nemesis_bonus.mult_milli(),
            u32::MAX,
            "`{text}`: accepted content is applied with the saturated stand-in"
        );
        // Bulwark preys on the Ravager; armor is skipped, so damage is exactly
        // floor(base * milli / 1000).
        let (a, d) = (
            c.unit_index("bulwark").unwrap(),
            c.unit_index("ravager").unwrap(),
        );
        let base = c.units[a].offense as u64 * c.combat.damage_per_offense as u64;
        let want_dmg = base * want as u64 / NemesisBonus::MULT_SCALE as u64;
        assert_eq!(
            damage_per_hit(&c, a, d) as u64,
            want_dmg,
            "`{text}`: per-hit damage is not the documented formula"
        );
    }
}

/// P42. The shipped multiplier is exactly 1300 per-mille, and the widening to
/// `f64` did not move it.
#[test]
fn the_shipped_multiplier_is_exactly_1300_per_mille() {
    let c = content();
    assert_eq!(c.nemesis_bonus.milli_exact(), Some(1_300));
    assert_eq!(c.nemesis_bonus.mult_milli(), 1_300);
}

/// P43. Nemesis is an *iff* over the whole roster, still, with the checked
/// multiplier in place: bonus + armor bypass exactly when
/// `attacker.nemesis == defender.id`, plain flat-armor math otherwise.
#[test]
fn nemesis_remains_an_iff_over_every_roster_pair() {
    let c = content();
    let milli = c.nemesis_bonus.milli_exact().expect("accepted content") as u64;
    for a in 0..c.units.len() {
        for d in 0..c.units.len() {
            let base = c.units[a].offense as u64 * c.combat.damage_per_offense as u64;
            let mit = c.units[d].armor as u64 * c.combat.mitigation_per_armor as u64;
            let is_nem = c.units[a].nemesis.as_deref() == Some(c.units[d].id.as_str());
            let want = if is_nem {
                let boosted = base * milli / 1_000;
                if c.nemesis_bonus.ignore_armor {
                    boosted
                } else {
                    boosted.saturating_sub(mit)
                }
            } else {
                base.saturating_sub(mit)
            };
            assert_eq!(
                damage_per_hit(&c, a, d) as u64,
                want,
                "{} -> {}",
                c.units[a].id,
                c.units[d].id
            );
            if is_nem {
                assert!(
                    damage_per_hit(&c, a, d) as u64 >= base,
                    "{} preys on {} but is punished by the bonus",
                    c.units[a].id,
                    c.units[d].id
                );
            }
        }
    }
}

/// P44. Determinism regression over a world that exercises *both* halves of the
/// fix at once: economy claims being handed out and taken back while combat
/// runs. Per-tick hashes and the Alloy ledger must be identical across runs and
/// independent of archetype iteration order.
#[test]
fn a_mixed_economy_and_combat_run_is_deterministic_and_order_independent() {
    #[derive(Component)]
    struct Tag(#[allow(dead_code)] u64);

    fn run(tagged: bool) -> (Vec<u64>, Vec<(u32, u32)>) {
        let mut app = sim_app();
        let node_pos = Vec2::new(260.0, 0.0);
        let node = spawn_node(&mut app, node_pos, 800);
        let def = app
            .world()
            .resource::<Content>()
            .building_index("hq")
            .unwrap();
        app.world_mut().spawn((
            Position(Vec2::ZERO),
            Building { def },
            Faction::A,
            ProductionQueue::default(),
        ));
        let ids = ["worker", "bulwark", "ripper", "sentinel", "worker", "ravager"];
        let mut mine = Vec::new();
        for (i, id) in ids.iter().enumerate() {
            let f = if i % 2 == 0 { Faction::A } else { Faction::B };
            let e = spawn_unit(
                &mut app,
                id,
                f,
                Vec2::new(i as f32 * 31.0 - 60.0, (i % 3) as f32 * 25.0 - 25.0),
            );
            if tagged && i % 2 == 1 {
                app.world_mut().entity_mut(e).insert(Tag(i as u64));
            }
            if i % 2 == 0 {
                mine.push(e);
            }
        }
        push(
            &mut app,
            Order::Gather {
                units: mine,
                node,
                node_pos,
            },
        );
        let mut hashes = Vec::new();
        let mut ledger = Vec::new();
        for _ in 0..600 {
            step(&mut app);
            hashes.push(state_hash(&mut app));
            ledger.push((
                app.world().resource::<Stockpiles>().alloy(Faction::A),
                node_amount(&app, node),
            ));
        }
        (hashes, ledger)
    }

    let a = run(false);
    let b = run(false);
    assert_eq!(a.0, b.0, "identical runs diverged");
    assert_eq!(a.1, b.1, "the Alloy ledger diverged between identical runs");
    let c = run(true);
    assert_eq!(
        a.1, c.1,
        "an extra component changed the economy outcome (archetype order leaked in)"
    );
    assert_eq!(
        a.0, c.0,
        "adding an unrelated component changed the per-tick state hashes"
    );
}
