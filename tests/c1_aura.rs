//! C1 AC3 — **the General's aura**: friendly units within a radius get a
//! bonus. Each General type carries its own aura in RON (a radius plus stat
//! modifiers: offense, defense, armor, speed, attack interval, HP regen); the
//! synthesists scale theirs by the distinct domains nearby
//! (`PerDistinctDomain` with a max). Six kinds, one per nation. An aura never
//! touches the nemesis bonus. Different kinds stack; the same kind does not —
//! a unit takes the strongest (DESIGN_BRIEF "Campaign battles", F-052).
//!
//! Every expected number is derived from the loaded content (F-044).

use std::path::PathBuf;

use bevy::prelude::*;

use onus::headless::{self, MatchSettings};
use onus::sim::battle::{AuraField, BattleSetup, General};
use onus::sim::combat::{
    attack_ticks_with, damage_per_hit, damage_per_hit_to_building, damage_per_hit_to_building_with,
    damage_per_hit_with, move_speed, move_speed_with, AttackCooldown, Health, StatBonus,
};
use onus::sim::content::{AuraScaling, AuraStat, Content};
use onus::sim::economy::UnitDefIdx;
use onus::sim::spatial::Faction;
use onus::sim::{AiCommanders, MatchState, MoveTarget, Position};

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data")
}

fn content() -> Content {
    Content::load_from_dir(&data_dir()).expect("content loads")
}

/// Where the test's army stands: mid-map, far from both bases and from B's
/// General, so nothing but what a test places is inside any aura.
const HERE: Vec2 = Vec2::new(0.0, 0.0);

/// A quiet battle (commanders stood down) with A's Generals `a` and one B
/// General; A's Generals are moved to `HERE` (each test then places them).
fn battle(a: &[&str]) -> App {
    let mut setup = BattleSetup::default().with_general(Faction::B, "general_bulwark");
    for id in a {
        setup = setup.with_general(Faction::A, id);
    }
    let mut app = headless::ai_vs_ai(content(), &MatchSettings::default().with_battle(setup))
        .expect("battle builds");
    app.insert_resource(AiCommanders::default());
    for g in generals_of(&mut app, Faction::A) {
        app.world_mut().get_mut::<Position>(g).unwrap().0 = HERE;
    }
    app
}

fn generals_of(app: &mut App, side: Faction) -> Vec<Entity> {
    let mut q = app
        .world_mut()
        .query_filtered::<(Entity, &Faction), With<General>>();
    let mut v: Vec<Entity> = q
        .iter(app.world())
        .filter(|(_, f)| **f == side)
        .map(|(e, _)| e)
        .collect();
    v.sort_by_key(|e| e.to_bits());
    v
}

fn spawn_unit(app: &mut App, id: &str, faction: Faction, pos: Vec2) -> Entity {
    let (idx, kind, hp) = {
        let c = app.world().resource::<Content>();
        let idx = c.unit_index(id).unwrap();
        (idx, c.units[idx].mvp_kind, Health::from_def(c, idx))
    };
    app.world_mut()
        .spawn((Position(pos), UnitDefIdx(idx), kind, faction, hp))
        .id()
}

fn bonus(app: &App, e: Entity) -> StatBonus {
    app.world().resource::<AuraField>().get(e)
}

fn radius(c: &Content, general: &str) -> f32 {
    c.generals
        .iter()
        .find(|g| g.id == general)
        .unwrap()
        .aura
        .radius
}

/// The aura's `add` for `stat`, from the content.
fn add(c: &Content, general: &str, stat: AuraStat) -> u32 {
    let g = c.generals.iter().find(|g| g.id == general).unwrap();
    g.aura
        .modifiers
        .iter()
        .find(|m| m.stat == stat)
        .unwrap()
        .add
        .unwrap()
}

// ---- the data ----------------------------------------------------------------

#[test]
fn six_aura_kinds_one_per_nation_each_a_list_of_modifiers_in_ron() {
    let c = content();
    let stats = |id: &str| -> Vec<AuraStat> {
        let g = c.generals.iter().find(|g| g.id == id).unwrap();
        g.aura.modifiers.iter().map(|m| m.stat).collect()
    };
    assert_eq!(
        stats("general_synthesis"),
        [AuraStat::Offense, AuraStat::Armor]
    );
    assert_eq!(
        stats("general_bulwark"),
        [AuraStat::Defense, AuraStat::Armor]
    );
    assert_eq!(stats("general_frenzy"), [AuraStat::AttackTicks]);
    assert_eq!(stats("general_focus"), [AuraStat::Offense]);
    assert_eq!(stats("general_logistics"), [AuraStat::Speed]);
    assert_eq!(stats("general_endurance"), [AuraStat::Regen]);
    for g in &c.generals {
        let expect_synthesis = g.id == "general_synthesis";
        assert_eq!(
            matches!(g.aura.scaling, AuraScaling::PerDistinctDomain { .. }),
            expect_synthesis,
            "only the synthesists scale by domains"
        );
        assert!(g.aura.radius > 0.0);
    }
    let frenzy = c
        .generals
        .iter()
        .find(|g| g.id == "general_frenzy")
        .unwrap();
    assert!(
        frenzy.aura.modifiers[0].mul_milli() < 1000,
        "Frenzy attacks faster"
    );
}

fn mutated(name: &str, edit: impl Fn(&str) -> String) -> Result<Content, String> {
    let out = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/c1_aura")
        .join(name);
    std::fs::create_dir_all(&out).unwrap();
    for file in ["resources.ron", "strategies.ron"] {
        std::fs::copy(data_dir().join(file), out.join(file)).unwrap();
    }
    let text = std::fs::read_to_string(data_dir().join("units.ron")).unwrap();
    let edited = edit(&text);
    assert_ne!(edited, text, "`{name}`: the edit changed nothing");
    std::fs::write(out.join("units.ron"), edited).unwrap();
    Content::load_from_dir(&out).map_err(|e| e.to_string())
}

#[test]
fn an_aura_that_names_the_nemesis_or_is_malformed_is_a_load_error() {
    let focus = "modifiers: [(stat: Offense, add: 2)]";
    for (name, to) in [
        ("nemesis_stat", "modifiers: [(stat: Nemesis, add: 2)]"),
        (
            "nemesis_field",
            "modifiers: [(stat: Offense, add: 2, nemesis: 2.0)]",
        ),
        ("both", "modifiers: [(stat: Offense, add: 2, mul: 0.5)]"),
        ("neither", "modifiers: [(stat: Offense)]"),
        ("mul_on_add_stat", "modifiers: [(stat: Offense, mul: 0.5)]"),
        ("zero_add", "modifiers: [(stat: Offense, add: 0)]"),
        (
            "twice",
            "modifiers: [(stat: Offense, add: 1), (stat: Offense, add: 1)]",
        ),
        ("empty", "modifiers: []"),
        ("slower", "modifiers: [(stat: AttackTicks, mul: 1.5)]"),
        ("zero_mul", "modifiers: [(stat: AttackTicks, mul: 0.0)]"),
    ] {
        let r = mutated(name, |t| t.replacen(focus, to, 1));
        assert!(r.is_err(), "`{name}` must be refused: {to}");
    }
    let r = mutated("zero_max", |t| {
        t.replacen("PerDistinctDomain(max: 3)", "PerDistinctDomain(max: 0)", 1)
    });
    assert!(r.is_err(), "a domain cap of 0 is refused");
    let r = mutated("big_max", |t| {
        t.replacen("PerDistinctDomain(max: 3)", "PerDistinctDomain(max: 9)", 1)
    });
    assert!(
        r.is_err(),
        "a domain cap above the number of domains is refused"
    );
    let r = mutated("no_radius", |t| {
        t.replacen("radius: 240.0", "radius: 0.0", 1)
    });
    assert!(r.is_err(), "a zero radius is refused");
}

// ---- the arithmetic ------------------------------------------------------------

#[test]
fn no_bonus_is_exactly_the_plain_rule_for_every_pair() {
    let c = content();
    let n = c.units.len() + c.generals.len();
    for a in 0..n {
        for d in 0..n {
            assert_eq!(
                damage_per_hit_with(&c, a, d, StatBonus::NONE, StatBonus::NONE),
                damage_per_hit(&c, a, d)
            );
        }
        for b in 0..c.buildings.len() {
            assert_eq!(
                damage_per_hit_to_building_with(&c, a, b, StatBonus::NONE),
                damage_per_hit_to_building(&c, a, b)
            );
        }
        assert_eq!(move_speed_with(&c, a, StatBonus::NONE), move_speed(&c, a));
    }
    assert_eq!(attack_ticks_with(90, StatBonus::NONE), 90);
}

#[test]
fn each_stat_bonus_moves_its_own_number_by_the_documented_rule() {
    let c = content();
    let cb = &c.combat;
    let ravager = c.unit_index("ravager").unwrap();
    let ripper = c.unit_index("ripper").unwrap();
    let (r, p) = (&c.units[ravager], &c.units[ripper]);
    assert_ne!(r.nemesis.as_deref(), Some("ripper"));
    let plain = damage_per_hit(&c, ravager, ripper);
    // Offense: two more design points of base damage.
    let off = StatBonus {
        offense: 2,
        ..StatBonus::NONE
    };
    assert_eq!(
        damage_per_hit_with(&c, ravager, ripper, off, StatBonus::NONE),
        (r.offense + 2) * cb.damage_per_offense - p.armor * cb.mitigation_per_armor
    );
    // Armor: two more design points of mitigation.
    let arm = StatBonus {
        armor: 2,
        ..StatBonus::NONE
    };
    assert_eq!(
        damage_per_hit_with(&c, ravager, ripper, StatBonus::NONE, arm),
        plain - 2 * cb.mitigation_per_armor
    );
    // Defense: what gets through × defense / (defense + bonus), floored.
    let def = StatBonus {
        defense: 2,
        ..StatBonus::NONE
    };
    assert_eq!(
        damage_per_hit_with(&c, ravager, ripper, StatBonus::NONE, def),
        plain * p.defense / (p.defense + 2)
    );
    // Speed: two more design points.
    let spd = StatBonus {
        speed: 2,
        ..StatBonus::NONE
    };
    assert_eq!(
        move_speed_with(&c, ravager, spd),
        (r.speed + 2) as f32 * cb.speed_per_point
    );
    // Attack interval: per-mille, floored, never below one tick.
    let fast = StatBonus {
        attack_milli: 800,
        ..StatBonus::NONE
    };
    assert_eq!(
        attack_ticks_with(r.mvp_attack_ticks, fast),
        r.mvp_attack_ticks * 800 / 1000
    );
    let instant = StatBonus {
        attack_milli: 1,
        ..StatBonus::NONE
    };
    assert_eq!(attack_ticks_with(5, instant), 1);
}

#[test]
fn an_aura_never_touches_the_nemesis_bonus() {
    let c = content();
    let nb = &c.nemesis_bonus;
    let cb = &c.combat;
    // Every nemesis pair in the roster, under every kind of bonus on both sides.
    let all = StatBonus {
        offense: 3,
        defense: 3,
        armor: 3,
        speed: 3,
        regen: 3,
        attack_milli: 500,
    };
    let mut pairs = 0;
    for (a, au) in c.units.iter().enumerate() {
        let Some(prey) = au.nemesis.as_deref() else {
            continue;
        };
        let d = c.unit_index(prey).unwrap();
        pairs += 1;
        // The multiplier is the content's, applied to the (buffed) base; with
        // ignore_armor, no armor — aura armor included — mitigates a nemesis hit.
        let base = (au.offense + 3) * cb.damage_per_offense;
        let boosted = (base as u64 * nb.mult_milli() as u64 / 1000) as u32;
        let through = if nb.ignore_armor {
            boosted
        } else {
            boosted - (c.units[d].armor + 3) * cb.mitigation_per_armor
        };
        let dd = c.units[d].defense;
        assert_eq!(
            damage_per_hit_with(&c, a, d, all, all),
            through * dd / (dd + 3),
            "{} vs its nemesis {}",
            au.id,
            prey
        );
        // And the nemesis ratio over the non-nemesis rule is unchanged by an
        // offense bonus: the bonus enters the base, never the multiplier.
        let off = StatBonus {
            offense: 3,
            ..StatBonus::NONE
        };
        assert_eq!(
            damage_per_hit_with(&c, a, d, off, StatBonus::NONE),
            boosted
                - if nb.ignore_armor {
                    0
                } else {
                    c.units[d].armor * cb.mitigation_per_armor
                }
        );
    }
    assert_eq!(pairs, 5, "the pentagon: five nemesis pairs");
}

// ---- the field -----------------------------------------------------------------

#[test]
fn friendly_units_inside_the_radius_get_the_aura_and_no_one_else_does() {
    let c = content();
    let id = "general_focus";
    let mut app = battle(&[id]);
    let r = radius(&c, id);
    let g = generals_of(&mut app, Faction::A)[0];
    let inside = spawn_unit(
        &mut app,
        "ravager",
        Faction::A,
        HERE + Vec2::new(r - 1.0, 0.0),
    );
    let outside = spawn_unit(
        &mut app,
        "ravager",
        Faction::A,
        HERE + Vec2::new(r + 1.0, 0.0),
    );
    // An enemy well inside the radius: the side filter, not distance, is what keeps him out.
    let enemy = spawn_unit(
        &mut app,
        "ravager",
        Faction::B,
        HERE + Vec2::new(0.0, r * 0.5),
    );
    headless::step(&mut app);
    let want = StatBonus {
        offense: add(&c, id, AuraStat::Offense),
        ..StatBonus::NONE
    };
    assert_eq!(bonus(&app, inside), want);
    assert_eq!(bonus(&app, outside), StatBonus::NONE);
    assert_eq!(
        bonus(&app, enemy),
        StatBonus::NONE,
        "an aura is for friends"
    );
    assert_eq!(
        bonus(&app, g),
        StatBonus::NONE,
        "a General's aura is for his army"
    );
    // It follows the General: move him off, and the unit loses it next tick.
    app.world_mut().get_mut::<Position>(g).unwrap().0 = HERE + Vec2::new(0.0, -2.0 * r);
    headless::step(&mut app);
    assert_eq!(bonus(&app, inside), StatBonus::NONE);
}

#[test]
fn the_same_kind_does_not_stack_a_unit_takes_one() {
    let c = content();
    let id = "general_focus";
    let mut app = battle(&[id, id]);
    let u = spawn_unit(&mut app, "ravager", Faction::A, HERE + Vec2::new(10.0, 0.0));
    headless::step(&mut app);
    assert_eq!(bonus(&app, u).offense, add(&c, id, AuraStat::Offense));
}

#[test]
fn different_kinds_stack() {
    let c = content();
    let mut app = battle(&["general_focus", "general_bulwark", "general_frenzy"]);
    let u = spawn_unit(&mut app, "ravager", Faction::A, HERE + Vec2::new(10.0, 0.0));
    headless::step(&mut app);
    let frenzy = c
        .generals
        .iter()
        .find(|g| g.id == "general_frenzy")
        .unwrap();
    assert_eq!(
        bonus(&app, u),
        StatBonus {
            offense: add(&c, "general_focus", AuraStat::Offense),
            defense: add(&c, "general_bulwark", AuraStat::Defense),
            armor: add(&c, "general_bulwark", AuraStat::Armor),
            attack_milli: frenzy.aura.modifiers[0].mul_milli(),
            ..StatBonus::NONE
        }
    );
}

#[test]
fn synthesis_scales_with_distinct_domains_nearby_up_to_its_max() {
    let c = content();
    let id = "general_synthesis";
    let AuraScaling::PerDistinctDomain { max } =
        c.generals.iter().find(|g| g.id == id).unwrap().aura.scaling
    else {
        panic!("synthesis scales by domain")
    };
    let (o, a) = (add(&c, id, AuraStat::Offense), add(&c, id, AuraStat::Armor));
    let mut app = battle(&[id]);
    // One domain (flesh): ×1 — a pure army gets the base bonus.
    let rav = spawn_unit(&mut app, "ravager", Faction::A, HERE + Vec2::new(10.0, 0.0));
    spawn_unit(&mut app, "ripper", Faction::A, HERE + Vec2::new(20.0, 0.0));
    headless::step(&mut app);
    assert_eq!((bonus(&app, rav).offense, bonus(&app, rav).armor), (o, a));
    // Two domains: ×2.
    spawn_unit(&mut app, "bulwark", Faction::A, HERE + Vec2::new(0.0, 10.0));
    headless::step(&mut app);
    assert_eq!(
        (bonus(&app, rav).offense, bonus(&app, rav).armor),
        (2 * o, 2 * a)
    );
    // Three: ×min(3, max). Workers (no domain) and enemies add nothing.
    spawn_unit(
        &mut app,
        "arclight",
        Faction::A,
        HERE + Vec2::new(0.0, 20.0),
    );
    spawn_unit(&mut app, "worker", Faction::A, HERE + Vec2::new(0.0, 30.0));
    headless::step(&mut app);
    let f = 3.min(max);
    assert_eq!(
        (bonus(&app, rav).offense, bonus(&app, rav).armor),
        (f * o, f * a)
    );
}

#[test]
fn synthesis_counts_no_domain_from_an_empty_or_unpowered_radius() {
    let mut app = battle(&["general_synthesis"]);
    let w = spawn_unit(&mut app, "worker", Faction::A, HERE + Vec2::new(10.0, 0.0));
    headless::step(&mut app);
    assert_eq!(bonus(&app, w), StatBonus::NONE, "no domain nearby: ×0");
}

#[test]
fn of_one_kind_the_strongest_reaches_a_unit() {
    let c = content();
    let id = "general_synthesis";
    let o = add(&c, id, AuraStat::Offense);
    let r = radius(&c, id);
    let mut app = battle(&[id, id]);
    let gs = generals_of(&mut app, Faction::A);
    // General 0 at HERE sees one domain; General 1 sits 1.5 r away and sees
    // three. The unit between them is inside both auras.
    let far = HERE + Vec2::new(1.5 * r, 0.0);
    app.world_mut().get_mut::<Position>(gs[1]).unwrap().0 = far;
    let shared = spawn_unit(
        &mut app,
        "ravager",
        Faction::A,
        HERE + Vec2::new(0.75 * r, 0.0),
    );
    spawn_unit(&mut app, "bulwark", Faction::A, far + Vec2::new(10.0, 0.0));
    spawn_unit(
        &mut app,
        "arclight",
        Faction::A,
        far + Vec2::new(-10.0, 10.0),
    );
    let near_only = spawn_unit(&mut app, "ripper", Faction::A, HERE + Vec2::new(-10.0, 0.0));
    headless::step(&mut app);
    assert_eq!(
        bonus(&app, near_only).offense,
        o,
        "General 0 sees flesh only"
    );
    assert_eq!(
        bonus(&app, shared).offense,
        3 * o,
        "the stronger of the two"
    );
}

// ---- the effects in the running sim -----------------------------------------------

#[test]
fn frenzy_shortens_the_attack_interval_in_combat() {
    let c = content();
    let mut app = battle(&["general_frenzy"]);
    let rav = c.unit_index("ravager").unwrap();
    let u = spawn_unit(&mut app, "ravager", Faction::A, HERE + Vec2::new(10.0, 0.0));
    // A target in reach that cannot hit back (a worker), so it stands there.
    spawn_unit(&mut app, "worker", Faction::B, HERE + Vec2::new(30.0, 0.0));
    headless::step(&mut app);
    let milli = c
        .generals
        .iter()
        .find(|g| g.id == "general_frenzy")
        .unwrap()
        .aura
        .modifiers[0]
        .mul_milli();
    let want = (c.units[rav].mvp_attack_ticks * milli / 1000).max(1);
    assert_eq!(app.world().get::<AttackCooldown>(u).unwrap().0, want);
    assert!(want < c.units[rav].mvp_attack_ticks);
}

#[test]
fn logistics_speeds_movement() {
    let c = content();
    let id = "general_logistics";
    let mut app = battle(&[id]);
    let rav = c.unit_index("ravager").unwrap();
    let start = HERE + Vec2::new(0.0, 10.0);
    let u = spawn_unit(&mut app, "ravager", Faction::A, start);
    app.world_mut()
        .entity_mut(u)
        .insert(MoveTarget(start + Vec2::new(0.0, 100.0)));
    headless::step(&mut app);
    let moved = app.world().get::<Position>(u).unwrap().0.distance(start);
    let speed =
        (c.units[rav].speed + add(&c, id, AuraStat::Speed)) as f32 * c.combat.speed_per_point;
    assert!(
        (moved - speed / 60.0).abs() < 1e-3,
        "moved {moved}, want {}",
        speed / 60.0
    );
}

#[test]
fn endurance_heals_on_the_regen_interval_up_to_the_pool() {
    let c = content();
    let id = "general_endurance";
    let interval = c.battle.as_ref().unwrap().regen_interval_ticks;
    let heal = add(&c, id, AuraStat::Regen);
    let mut app = battle(&[id]);
    let u = spawn_unit(&mut app, "ravager", Faction::A, HERE + Vec2::new(10.0, 0.0));
    let away = spawn_unit(
        &mut app,
        "ravager",
        Faction::A,
        HERE + Vec2::new(0.0, 900.0),
    );
    let max = app.world().get::<Health>(u).unwrap().max;
    // Play the first tick (a multiple of the interval), then wound both.
    headless::step(&mut app);
    for e in [u, away] {
        app.world_mut().get_mut::<Health>(e).unwrap().current = max - 2 * heal - 1;
    }
    // One full interval of ticks: exactly one heal lands.
    headless::tick(&mut app, interval);
    assert_eq!(
        app.world().get::<Health>(u).unwrap().current,
        max - heal - 1
    );
    assert_eq!(
        app.world().get::<Health>(away).unwrap().current,
        max - 2 * heal - 1
    );
    // Never above the pool.
    headless::tick(&mut app, 3 * interval);
    assert_eq!(app.world().get::<Health>(u).unwrap().current, max);
    assert!(app.world().resource::<MatchState>().outcome().is_none());
}

#[test]
fn a_skirmish_carries_no_aura_field() {
    let mut app = headless::ai_vs_ai(content(), &MatchSettings::default()).unwrap();
    headless::tick(&mut app, 3);
    assert!(app.world().get_resource::<AuraField>().is_none());
}
