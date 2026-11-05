//! C2 AC1 — **nations as data**: `assets/data/nations.ron` holds C0's six
//! nations with their General types, a nation's traits are data-driven
//! modifiers on the content its side plays with (costs, unit stats, training
//! time, worker gather time, building HP), and a match names one nation per side
//! through `MatchSettings`, the way it names strategies.

use std::path::PathBuf;

use bevy::prelude::*;

use onus::headless::{self, MatchSettings, MatchSetupError};
use onus::sim::battle::AuraField;
use onus::sim::combat::StatBonus;
use onus::sim::content::Content;
use onus::sim::economy::{Building, Produced};
use onus::sim::nation::{self, Defs, NationRules, UnknownNation};
use onus::sim::{
    AiCommanders, Carrying, CommandLog, CommandQueue, Faction, Health, MatchLog, Order, Position,
    ReplaySource, StateHashLog, Stockpiles, UnitDefIdx, UnitKind,
};

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data")
}

fn shipped() -> Content {
    Content::load_from_dir(&data_dir()).expect("content loads")
}

/// `round(v * mul)` half up, from the RON's own float — the rule the brief
/// states, written independently of the sim's helper.
fn traited(v: u32, mul: f32) -> u32 {
    let milli = (mul * 1000.0).round() as u64;
    ((v as u64 * milli + 500) / 1000) as u32
}

fn nation_mul(c: &Content, nation: &str, k: usize) -> f32 {
    let n = c.nations.iter().find(|n| n.id == nation).unwrap();
    n.traits[k].mul.expect("a mul trait")
}

fn nation_add(c: &Content, nation: &str, k: usize) -> i32 {
    let n = c.nations.iter().find(|n| n.id == nation).unwrap();
    n.traits[k].add.expect("an add trait")
}

/// A headless match with the given nations and **no commanders**: the test
/// drives it by orders alone.
fn quiet(a: Option<&str>, b: Option<&str>) -> App {
    let s = MatchSettings::default().with_nations(a, b);
    let mut app = headless::battle(shipped(), &s).expect("builds");
    app.insert_resource(AiCommanders::new(0, &[]));
    app
}

fn building_of(app: &mut App, f: Faction, def: usize) -> Option<Entity> {
    let mut q = app.world_mut().query::<(Entity, &Building, &Faction)>();
    let mut v: Vec<Entity> = q
        .iter(app.world())
        .filter(|(_, b, g)| b.def == def && **g == f)
        .map(|(e, ..)| e)
        .collect();
    v.sort_by_key(|e| e.to_bits());
    v.first().copied()
}

fn push(app: &mut App, o: Order, by: Faction) {
    app.world_mut()
        .resource_mut::<CommandQueue>()
        .0
        .push_back(o.issued_by(by));
}

const BASE: [Vec2; 2] = [Vec2::new(-750.0, 0.0), Vec2::new(750.0, 0.0)];

/// Place `building` for both sides near their bases, step once, and return
/// the two buildings.
fn place_both(app: &mut App, building: &str) -> [Entity; 2] {
    let def = shipped().building_index(building).unwrap();
    for (i, f) in [Faction::A, Faction::B].into_iter().enumerate() {
        push(
            app,
            Order::Place {
                faction: f,
                building: def,
                pos: BASE[i] + Vec2::new(0.0, -150.0),
            },
            f,
        );
    }
    headless::step(app);
    [
        building_of(app, Faction::A, def).expect("A placed it"),
        building_of(app, Faction::B, def).expect("B placed it"),
    ]
}

// ---- the roster --------------------------------------------------------------

#[test]
fn the_shipped_roster_is_c0s_six_nations_with_their_generals() {
    let c = shipped();
    let got: Vec<(&str, &str)> = c
        .nations
        .iter()
        .map(|n| (n.id.as_str(), n.general.as_str()))
        .collect();
    assert_eq!(
        got,
        [
            ("house", "general_synthesis"),
            ("forgeborn", "general_bulwark"),
            ("verdant", "general_frenzy"),
            ("aetherkin", "general_focus"),
            ("tollmarch", "general_logistics"),
            ("ashen_reach", "general_endurance"),
        ]
    );
    let names: Vec<&str> = c.nations.iter().map(|n| n.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "House Vael",
            "Forgeborn",
            "Verdant",
            "Aetherkin",
            "Tollmarch",
            "Ashen Reach"
        ]
    );
    for n in &c.nations {
        assert!(
            c.general_index(&n.general).is_some(),
            "{}: general resolves",
            n.id
        );
        assert!(
            c.strategy(&n.strategy).is_some(),
            "{}: strategy resolves",
            n.id
        );
    }
}

#[test]
fn content_without_a_nations_file_loads_with_no_nations() {
    let out = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/c2_nations/none");
    std::fs::create_dir_all(&out).unwrap();
    for f in ["units.ron", "resources.ron", "strategies.ron"] {
        std::fs::copy(data_dir().join(f), out.join(f)).unwrap();
    }
    let _ = std::fs::remove_file(out.join("nations.ron"));
    let c = Content::load_from_dir(&out).expect("loads");
    assert!(c.nations.is_empty());
}

// ---- selection through MatchSettings ----------------------------------------

#[test]
fn the_default_match_names_no_nation_and_installs_nothing() {
    assert_eq!(MatchSettings::default().nations, [None, None]);
    let mut app = headless::ai_vs_ai(shipped(), &MatchSettings::default()).unwrap();
    headless::tick(&mut app, 30);
    assert!(!app.world().contains_resource::<NationRules>());
}

#[test]
fn a_match_names_one_nation_per_side() {
    let s = MatchSettings::default().with_nations(Some("forgeborn"), Some("verdant"));
    assert_eq!(
        s.nations,
        [Some("forgeborn".to_string()), Some("verdant".to_string())]
    );
    let app = headless::battle(shipped(), &s).unwrap();
    let c = shipped();
    let r = app.world().resource::<NationRules>();
    let idx = |id: &str| c.nations.iter().position(|n| n.id == id);
    assert_eq!(r.nation_of(Faction::A), idx("forgeborn"));
    assert_eq!(r.nation_of(Faction::B), idx("verdant"));

    let one = MatchSettings::default().with_nations(None, Some("tollmarch"));
    let app = headless::battle(shipped(), &one).unwrap();
    let r = app.world().resource::<NationRules>();
    assert_eq!(r.nation_of(Faction::A), None);
    assert_eq!(r.nation_of(Faction::B), idx("tollmarch"));
}

#[test]
fn an_unknown_nation_is_refused_by_name_and_side() {
    let s = MatchSettings::default().with_nations(None, Some("atlantis"));
    let Err(err) = headless::battle(shipped(), &s) else {
        panic!("an unknown nation must be refused");
    };
    assert_eq!(
        err,
        MatchSetupError::Nation(UnknownNation {
            id: "atlantis".to_string(),
            faction: Faction::B,
        })
    );
}

// ---- the traits, through the sim --------------------------------------------

#[test]
fn forgeborn_pays_less_for_foundry_units_and_only_forgeborn() {
    let c = shipped();
    let mul = nation_mul(&c, "forgeborn", 0);
    let mut app = quiet(Some("forgeborn"), None);
    let [fa, fb] = place_both(&mut app, "foundry");
    let before = {
        let s = app.world().resource::<Stockpiles>();
        [s.alloy(Faction::A), s.alloy(Faction::B)]
    };
    let bulwark = c.unit_index("bulwark").unwrap();
    for (b, f) in [(fa, Faction::A), (fb, Faction::B)] {
        push(
            &mut app,
            Order::Train {
                building: b,
                unit: bulwark,
            },
            f,
        );
    }
    headless::step(&mut app);
    let s = app.world().resource::<Stockpiles>();
    let base = c.units[bulwark].mvp_alloy_cost;
    assert_eq!(
        before[0] - s.alloy(Faction::A),
        traited(base, mul),
        "Forgeborn price"
    );
    assert_eq!(
        before[1] - s.alloy(Faction::B),
        base,
        "no nation pays the RON price"
    );
    assert!(traited(base, mul) < base);

    // Through the lookup every read site uses: a Gene-Vats unit is untouched.
    let rules = app.world().resource::<NationRules>();
    let defs = Defs::new(&c, Some(rules));
    let ripper = c.unit_index("ripper").unwrap();
    assert_eq!(
        defs.roster_unit(Faction::A, ripper).unwrap().mvp_alloy_cost,
        c.units[ripper].mvp_alloy_cost
    );
    let sentinel = c.unit_index("sentinel").unwrap();
    assert_eq!(
        defs.roster_unit(Faction::A, sentinel)
            .unwrap()
            .mvp_alloy_cost,
        traited(c.units[sentinel].mvp_alloy_cost, mul)
    );
}

#[test]
fn verdant_trains_gene_vats_units_faster() {
    let c = shipped();
    let mul = nation_mul(&c, "verdant", 0);
    let ripper = c.unit_index("ripper").unwrap();
    let base = c.units[ripper].mvp_train_ticks;
    let fast = traited(base, mul);
    assert!(fast < base);

    let mut app = quiet(Some("verdant"), None);
    let [ga, gb] = place_both(&mut app, "gene_vats");
    for (b, f) in [(ga, Faction::A), (gb, Faction::B)] {
        push(
            &mut app,
            Order::Train {
                building: b,
                unit: ripper,
            },
            f,
        );
    }
    let made = |app: &App| {
        let p = app.world().resource::<Produced>();
        [p.count(Faction::A, ripper), p.count(Faction::B, ripper)]
    };
    // The first tick applies the order; the queue then counts down.
    let mut first = [None, None];
    for t in 1..=base + 5 {
        headless::step(&mut app);
        let m = made(&app);
        for k in 0..2 {
            if m[k] > 0 && first[k].is_none() {
                first[k] = Some(t);
            }
        }
    }
    let (a, b) = (first[0].expect("A trained"), first[1].expect("B trained"));
    assert_eq!(
        b - a,
        base - fast,
        "Verdant's Ripper arrives {} ticks early",
        base - fast
    );
}

#[test]
fn tollmarch_workers_gather_faster() {
    let c = shipped();
    let mul = nation_mul(&c, "tollmarch", 0);
    let worker = c.unit_index("worker").unwrap();
    let base = c.units[worker].mvp_gather_ticks;
    let fast = traited(base, mul);
    assert!(fast < base, "a gather-time multiplier below 1");
    let carry = c.units[worker].mvp_carry_capacity;
    // Both sides play the same script; only the nation differs.
    let s = MatchSettings::default().with_nations(Some("tollmarch"), None);
    let mut app = headless::battle(shipped(), &s).unwrap();
    let mut longest = [0u32; 2];
    let mut most = [0u32; 2];
    for _ in 0..900 {
        headless::step(&mut app);
        let mut q = app.world_mut().query::<(
            &Faction,
            &Carrying,
            Option<&onus::sim::economy::GatherPhase>,
        )>();
        for (f, load, phase) in q.iter(app.world()) {
            let k = if *f == Faction::A { 0 } else { 1 };
            most[k] = most[k].max(load.0);
            if let Some(onus::sim::economy::GatherPhase::Harvesting { ticks_left }) = phase {
                longest[k] = longest[k].max(*ticks_left);
            }
        }
    }
    assert_eq!(
        longest,
        [fast, base],
        "a load takes Tollmarch's gather time, its side only"
    );
    assert_eq!(most, [carry, carry], "the load itself is unchanged");
}

#[test]
fn aetherkin_arclights_reach_further() {
    let c = shipped();
    let add = nation_add(&c, "aetherkin", 0) as f32;
    let arclight = c.unit_index("arclight").unwrap();
    let reach = c.units[arclight].mvp_attack_range;
    let gap = reach + add / 2.0; // beyond the RON reach, inside the traited one

    let duel = |a: Option<&str>| {
        let mut app = quiet(a, None);
        let mut spawn = |f: Faction, x: f32| {
            let hp =
                Defs::new(&c, app.world().get_resource::<NationRules>()).unit_health(f, arclight);
            app.world_mut()
                .spawn((
                    Position(Vec2::new(x, 2_000.0)),
                    UnitDefIdx(arclight),
                    UnitKind::Soldier,
                    f,
                    hp,
                ))
                .id()
        };
        let ea = spawn(Faction::A, 0.0);
        let eb = spawn(Faction::B, gap);
        headless::step(&mut app);
        let hp = |e: Entity| {
            let h = app.world().get::<Health>(e).unwrap();
            (h.current, h.max)
        };
        (hp(ea), hp(eb))
    };
    let ((a_now, a_max), (b_now, b_max)) = duel(Some("aetherkin"));
    assert_eq!(a_now, a_max, "B's Arclight is out of reach");
    assert!(b_now < b_max, "Aetherkin's Arclight hits at {gap}");
    let ((a_now, a_max), (b_now, b_max)) = duel(None);
    assert_eq!(
        (a_now, b_now),
        (a_max, b_max),
        "with no nation neither reaches"
    );
}

#[test]
fn ashen_reach_buildings_have_more_hp() {
    let c = shipped();
    let mul = nation_mul(&c, "ashen_reach", 0);
    let mut app = quiet(Some("ashen_reach"), None);
    let [fa, fb] = place_both(&mut app, "foundry");
    let pool = |e: Entity| app.world().get::<Health>(e).unwrap().max;
    let foundry = c.building("foundry").unwrap();
    let base = foundry.mvp_defense * c.combat.building_hp_per_defense;
    assert_eq!(pool(fb), base);
    assert_eq!(pool(fa), traited(base, mul));
    assert!(pool(fa) > base);
}

#[test]
fn a_starting_unit_takes_its_sides_traited_pool() {
    // A Defense trait (here, written for the test) must reach the units on the
    // field at tick 0, not only those trained later.
    let dir = with_nations(
        "start_pool",
        r#"(nations: [(id: "stout", name: "Stout", domain: None, colour: (0.5,0.5,0.5),
            accent: (0.5,0.5,0.5), general: "general_bulwark", strategy: "mvp",
            traits: [(applies_to: Workers, stat: Defense, add: 2)])])"#,
    );
    let c = Content::load_from_dir(&dir).expect("loads");
    let worker = c.unit_index("worker").unwrap();
    let s = MatchSettings::default().with_nations(Some("stout"), None);
    let mut app = headless::battle(c.clone(), &s).unwrap();
    let mut q = app.world_mut().query::<(&Faction, &UnitDefIdx, &Health)>();
    let mut pools = [0u32; 2];
    for (f, d, h) in q.iter(app.world()) {
        if d.0 == worker {
            pools[if *f == Faction::A { 0 } else { 1 }] = h.max;
            assert_eq!(h.current, h.max);
        }
    }
    let base = c.units[worker].defense * c.combat.hp_per_defense;
    assert_eq!(pools, [base + 2 * c.combat.hp_per_defense, base]);
}

// ---- traits are data: any target, any stat, refused when unplayable ---------

/// A copy of the shipped data with `nations.ron` replaced by `text`.
fn with_nations(name: &str, text: &str) -> PathBuf {
    let out = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/c2_nations")
        .join(name);
    std::fs::create_dir_all(&out).unwrap();
    for f in ["units.ron", "resources.ron", "strategies.ron"] {
        std::fs::copy(data_dir().join(f), out.join(f)).unwrap();
    }
    std::fs::write(out.join("nations.ron"), text).unwrap();
    out
}

fn one_trait(name: &str, t: &str) -> Result<Content, String> {
    let text = format!(
        r#"(nations: [(id: "x", name: "X", domain: None, colour: (0.1,0.2,0.3),
            accent: (0.4,0.5,0.6), general: "general_focus", strategy: "mvp",
            traits: [{t}])])"#
    );
    Content::load_from_dir(&with_nations(name, &text)).map_err(|e| e.to_string())
}

fn x_defs(c: &Content) -> NationRules {
    NationRules::resolve(c, &[Some("x".to_string()), None])
        .unwrap()
        .unwrap()
}

#[test]
fn any_stat_on_any_target_is_a_line_of_data() {
    let c = one_trait(
        "combat_offense",
        "(applies_to: CombatUnits, stat: Offense, add: 1)",
    )
    .expect("loads");
    let r = x_defs(&c);
    let d = Defs::new(&c, Some(&r));
    for (i, u) in c.units.iter().enumerate() {
        let want = if u.offense > 0 { u.offense + 1 } else { 0 };
        assert_eq!(d.unit(Faction::A, i).unwrap().offense, want, "{}", u.id);
        assert_eq!(
            d.unit(Faction::B, i).unwrap().offense,
            u.offense,
            "B untouched"
        );
    }
    // Generals take no trait: a General's definition is the content's.
    let g = c.general_index("general_focus").unwrap();
    assert_eq!(
        d.unit(Faction::A, g).unwrap().offense,
        c.unit_def(g).unwrap().offense
    );

    let c = one_trait(
        "building_armor",
        "(applies_to: Building(\"hq\"), stat: Armor, add: 1)",
    )
    .expect("loads");
    let r = x_defs(&c);
    let d = Defs::new(&c, Some(&r));
    let hq = c.building_index("hq").unwrap();
    assert_eq!(
        d.building(Faction::A, hq).unwrap().mvp_armor,
        c.buildings[hq].mvp_armor + 1
    );

    let c = one_trait(
        "attack_ticks",
        "(applies_to: Unit(\"sentinel\"), stat: AttackTicks, mul: 0.5)",
    )
    .expect("loads");
    let r = x_defs(&c);
    let d = Defs::new(&c, Some(&r));
    let s = c.unit_index("sentinel").unwrap();
    assert_eq!(
        d.unit(Faction::A, s).unwrap().mvp_attack_ticks,
        traited(c.units[s].mvp_attack_ticks, 0.5)
    );
}

#[test]
fn several_traits_compose_in_ron_order() {
    let c = one_trait(
        "compose",
        "(applies_to: Unit(\"bulwark\"), stat: AlloyCost, mul: 0.5),
         (applies_to: CombatUnits, stat: AlloyCost, mul: 0.5)",
    )
    .expect("loads");
    let r = x_defs(&c);
    let d = Defs::new(&c, Some(&r));
    let b = c.unit_index("bulwark").unwrap();
    let base = c.units[b].mvp_alloy_cost;
    assert_eq!(
        d.roster_unit(Faction::A, b).unwrap().mvp_alloy_cost,
        traited(traited(base, 0.5), 0.5)
    );
}

#[test]
fn an_unplayable_trait_is_refused_at_load() {
    let refused = [
        (
            "wrong_form",
            "(applies_to: CombatUnits, stat: AlloyCost, add: 5)",
        ),
        (
            "both_forms",
            "(applies_to: CombatUnits, stat: AlloyCost, add: 5, mul: 0.9)",
        ),
        (
            "zero_mul",
            "(applies_to: CombatUnits, stat: TrainTicks, mul: 0.0)",
        ),
        (
            "over_max",
            "(applies_to: CombatUnits, stat: Offense, add: 10)",
        ),
        (
            "neg_stat",
            "(applies_to: Unit(\"ripper\"), stat: Armor, add: -5)",
        ),
        (
            "unknown_unit",
            "(applies_to: Unit(\"dragon\"), stat: Speed, add: 1)",
        ),
        (
            "unknown_building",
            "(applies_to: Barracks(\"castle\"), stat: AlloyCost, mul: 0.9)",
        ),
        (
            "hp_on_units",
            "(applies_to: CombatUnits, stat: BuildingHp, mul: 1.1)",
        ),
        (
            "speed_on_buildings",
            "(applies_to: Buildings, stat: Speed, add: 1)",
        ),
        (
            "carry_on_soldiers",
            "(applies_to: CombatUnits, stat: CarryCapacity, add: 1)",
        ),
        (
            "reach_past_engage",
            "(applies_to: Unit(\"arclight\"), stat: AttackRange, add: 500)",
        ),
        (
            "arm_a_worker",
            "(applies_to: Workers, stat: Offense, add: 1)",
        ),
        (
            "unknown_field",
            "(applies_to: Workers, stat: Speed, add: 1, note: \"hi\")",
        ),
    ];
    for (name, t) in refused {
        assert!(one_trait(name, t).is_err(), "{name}: `{t}` must be refused");
    }
    // The references a nation makes must resolve, too.
    for (name, text) in [
        (
            "bad_general",
            r#"(nations: [(id: "x", name: "X", domain: None, colour: (0.1,0.2,0.3),
                accent: (0.4,0.5,0.6), general: "general_nobody", strategy: "mvp", traits: [])])"#,
        ),
        (
            "bad_strategy",
            r#"(nations: [(id: "x", name: "X", domain: None, colour: (0.1,0.2,0.3),
                accent: (0.4,0.5,0.6), general: "general_focus", strategy: "nope", traits: [])])"#,
        ),
        (
            "bad_domain",
            r#"(nations: [(id: "x", name: "X", domain: "water", colour: (0.1,0.2,0.3),
                accent: (0.4,0.5,0.6), general: "general_focus", strategy: "mvp", traits: [])])"#,
        ),
        (
            "bad_colour",
            r#"(nations: [(id: "x", name: "X", domain: None, colour: (1.5,0.2,0.3),
                accent: (0.4,0.5,0.6), general: "general_focus", strategy: "mvp", traits: [])])"#,
        ),
        (
            "duplicate",
            r#"(nations: [(id: "x", name: "X", domain: None, colour: (0.1,0.2,0.3),
                accent: (0.4,0.5,0.6), general: "general_focus", strategy: "mvp", traits: []),
                (id: "x", name: "Y", domain: None, colour: (0.1,0.2,0.3),
                accent: (0.4,0.5,0.6), general: "general_focus", strategy: "mvp", traits: [])])"#,
        ),
    ] {
        assert!(
            Content::load_from_dir(&with_nations(name, text)).is_err(),
            "{name} must be refused"
        );
    }
}

// ---- determinism and the hash ------------------------------------------------

#[test]
fn the_state_hash_sees_a_nation_even_one_with_no_traits() {
    // House Vael's traits are `[]` in a skirmish; the hash must still tell its
    // match from a no-nation one, or a log could replay without its nations.
    let hash_at = |a: Option<&str>| {
        let s = MatchSettings::default()
            .with_nations(a, None)
            .with_hashing(true);
        let mut app = headless::battle(shipped(), &s).unwrap();
        headless::tick(&mut app, 3);
        app.world().resource::<onus::sim::StateHashLog>().0.clone()
    };
    assert_ne!(hash_at(Some("house")), hash_at(None));
    assert_eq!(
        hash_at(Some("house")),
        hash_at(Some("house")),
        "deterministic"
    );
}

#[test]
fn a_nation_match_is_deterministic() {
    let run = || {
        let s = MatchSettings::default()
            .with_seed(7)
            .with_nations(Some("tollmarch"), Some("verdant"))
            .with_hashing(true);
        let mut app = headless::battle(shipped(), &s).unwrap();
        headless::tick(&mut app, 2_400);
        app.world().resource::<onus::sim::StateHashLog>().0.clone()
    };
    assert_eq!(run(), run());
}

#[test]
fn install_is_a_no_op_with_no_nations() {
    let mut app = quiet(None, None);
    nation::install(app.world_mut(), &[None, None]).unwrap();
    assert!(!app.world().contains_resource::<NationRules>());
}

// ---- the log carries the nations (critic finding 2) --------------------------

/// Record `a` vs `b` for `ticks`, AI against AI, and return the log as it
/// comes back from disk, plus the recording's hashes.
fn record(a: Option<&str>, b: Option<&str>, ticks: u32) -> (MatchLog, StateHashLog) {
    let s = MatchSettings::default()
        .with_seed(9)
        .with_nations(a, b)
        .with_hashing(true);
    let mut rec = headless::ai_vs_ai(shipped(), &s).unwrap();
    headless::tick(&mut rec, ticks);
    let hashes = rec.world().resource::<StateHashLog>().clone();
    let log = rec.world().resource::<CommandLog>().log().clone();
    (MatchLog::from_ron(&log.to_ron().unwrap()).unwrap(), hashes)
}

/// Replay `log` into a world built from **nothing but its seed** — the replay
/// has to bring the nations itself.
fn replay(log: MatchLog, setup_nations: [Option<&str>; 2], ticks: u32) -> App {
    let rs = MatchSettings::default()
        .with_seed(log.seed)
        .with_nations(setup_nations[0], setup_nations[1])
        .with_hashing(true);
    let mut rep = headless::ai_vs_ai(shipped(), &rs).unwrap();
    rep.insert_resource(AiCommanders::default());
    rep.insert_resource(StateHashLog::default());
    rep.insert_resource(CommandLog::new(log.seed));
    rep.insert_resource(ReplaySource::new(log));
    headless::tick(&mut rep, ticks);
    rep
}

#[test]
fn a_nation_match_log_names_its_nations_and_replays_them_faithfully() {
    const TICKS: u32 = 1_200;
    let (log, recorded) = record(Some("forgeborn"), Some("tollmarch"), TICKS);
    assert_eq!(
        log.nations,
        [Some("forgeborn".to_string()), Some("tollmarch".to_string())],
        "the sim stamps each side's nation into the log"
    );
    assert!(
        log.commands.len() > 10,
        "the AI played: {}",
        log.commands.len()
    );
    let rep = replay(log.clone(), [None, None], TICKS);
    let src = rep.world().resource::<ReplaySource>();
    assert_eq!(src.rejection(), None, "a faithful log is not refused");
    assert!(src.finished());
    let rules = rep.world().resource::<NationRules>();
    let c = rep.world().resource::<Content>();
    assert_eq!(
        c.nations[rules.nation_of(Faction::A).unwrap()].id,
        "forgeborn"
    );
    assert_eq!(
        c.nations[rules.nation_of(Faction::B).unwrap()].id,
        "tollmarch"
    );
    let replayed = rep.world().resource::<StateHashLog>().clone();
    assert_eq!(recorded.first_divergence(&replayed), None);
    // The replay's own log names the same nations: a replay of a replay holds.
    assert_eq!(
        rep.world().resource::<CommandLog>().log().nations,
        log.nations
    );

    // One side only: the other slot stays none, and it still replays.
    let (log, recorded) = record(None, Some("verdant"), 600);
    assert_eq!(log.nations, [None, Some("verdant".to_string())]);
    let rep = replay(log, [None, None], 600);
    assert_eq!(rep.world().resource::<ReplaySource>().rejection(), None);
    let replayed = rep.world().resource::<StateHashLog>().clone();
    assert_eq!(recorded.first_divergence(&replayed), None);
}

#[test]
fn a_no_nation_log_is_written_as_before_and_a_pre_c2_log_reads_as_no_nations() {
    let (log, _) = record(None, None, 300);
    assert_eq!(log.nations, [None, None]);
    let text = log.to_ron().unwrap();
    assert!(
        !text.contains("nations"),
        "a no-nation log is byte-for-byte pre-C2"
    );
    // ...so a pre-C2 log, which has no such field, reads as no nations.
    assert_eq!(MatchLog::from_ron(&text).unwrap().nations, [None, None]);
}

#[test]
fn a_log_naming_a_nation_this_content_lacks_is_refused() {
    let (mut log, _) = record(Some("forgeborn"), Some("tollmarch"), 300);
    log.nations[1] = Some("atlantis".to_string());
    let text = log.to_ron().unwrap();
    // The front door refuses it, naming the nation...
    let why = MatchLog::from_ron_for(&text, &shipped()).unwrap_err();
    assert!(why.contains("atlantis"), "{why}");
    // ...and so does the backstop, for a log that never went through it.
    let rep = replay(MatchLog::from_ron(&text).unwrap(), [None, None], 60);
    let src = rep.world().resource::<ReplaySource>();
    let why = src.rejection().expect("refused");
    assert!(why.contains("atlantis"), "{why}");
    assert_eq!(src.cursor(), 0, "nothing of a refused log is fed");
    assert!(
        !rep.world().contains_resource::<NationRules>(),
        "nothing installed"
    );
}

#[test]
fn a_log_whose_nations_disagree_with_the_world_is_refused() {
    // The world already plays nations the log did not record — either way round.
    let (log, _) = record(None, None, 300);
    let rep = replay(log, [Some("forgeborn"), None], 60);
    let why = rep
        .world()
        .resource::<ReplaySource>()
        .rejection()
        .map(str::to_string);
    assert!(
        why.as_deref().is_some_and(|w| w.contains("nation")),
        "{why:?}"
    );

    let (log, _) = record(Some("forgeborn"), Some("tollmarch"), 300);
    let rep = replay(log.clone(), [Some("verdant"), Some("tollmarch")], 60);
    assert!(rep.world().resource::<ReplaySource>().rejection().is_some());
    // The same nations already installed by setup are the same match: accepted.
    let rep = replay(log, [Some("forgeborn"), Some("tollmarch")], 60);
    assert_eq!(rep.world().resource::<ReplaySource>().rejection(), None);
}

// ---- every trait read site is guarded (critic finding 3) ---------------------

/// The defender's side of a hit on a **building** reads the building's side's
/// table: an Armor trait on side A's buildings mitigates a no-nation
/// attacker's hit. (The unit defender is guarded by critic_c2's
/// `offense_and_armor_traits_reach_combat_on_both_ends_of_a_hit`.)
#[test]
fn a_building_armor_trait_mitigates_hits_on_its_side_only() {
    let c = one_trait("bld_armor", "(applies_to: Buildings, stat: Armor, add: 1)").expect("loads");
    let s = MatchSettings::default().with_nations(Some("x"), None);
    let mut app = headless::battle(c.clone(), &s).expect("builds");
    app.insert_resource(AiCommanders::new(0, &[]));
    let foundry = c.building_index("foundry").unwrap();
    // Far from both bases, so nothing but the one attacker is near it.
    let spots = [Vec2::new(-750.0, 650.0), Vec2::new(750.0, 650.0)];
    for (f, at) in [Faction::A, Faction::B].into_iter().zip(spots) {
        push(
            &mut app,
            Order::Place {
                faction: f,
                building: foundry,
                pos: at,
            },
            f,
        );
    }
    headless::step(&mut app);
    let targets = [
        building_of(&mut app, Faction::A, foundry).expect("A placed it"),
        building_of(&mut app, Faction::B, foundry).expect("B placed it"),
    ];
    // One sentinel of the *other* side beside each foundry.
    let sentinel = c.unit_index("sentinel").unwrap();
    let attackers = [Faction::B, Faction::A];
    for (i, f) in attackers.into_iter().enumerate() {
        let at = spots[i] + Vec2::new(0.0, 60.0);
        let hp = Health::from_def(&c, sentinel);
        app.world_mut().spawn((
            Position(at),
            UnitDefIdx(sentinel),
            c.units[sentinel].mvp_kind,
            f,
            hp,
        ));
    }
    let hp = |app: &App, e: Entity| app.world().get::<Health>(e).unwrap().current;
    let full = [hp(&app, targets[0]), hp(&app, targets[1])];
    let mut took = [0u32; 2];
    for _ in 0..600 {
        headless::step(&mut app);
        for k in 0..2 {
            if took[k] == 0 {
                took[k] = full[k] - hp(&app, targets[k]);
            }
        }
        if took.iter().all(|t| *t > 0) {
            break;
        }
    }
    let plain = onus::sim::combat::damage_to_building_between(
        &c,
        &c.units[sentinel],
        &c.buildings[foundry],
        onus::sim::combat::StatBonus::NONE,
    );
    let rules = app.world().resource::<NationRules>().clone();
    let armored = Defs::new(&c, Some(&rules))
        .building(Faction::A, foundry)
        .unwrap();
    let mitigated = onus::sim::combat::damage_to_building_between(
        &c,
        &c.units[sentinel],
        armored,
        onus::sim::combat::StatBonus::NONE,
    );
    assert!(
        mitigated < plain && mitigated > 0,
        "the trait changes the hit"
    );
    assert_eq!(
        took,
        [mitigated, plain],
        "A's foundry takes the traited hit, B's the plain one"
    );
}

/// What one side's commander did in the first `ticks` of a match where side A
/// plays nation `x`.
fn ai_actions(c: &Content, ticks: u32) -> [Vec<onus::sim::ai::AiAction>; 2] {
    let s = MatchSettings::default()
        .with_seed(3)
        .with_nations(Some("x"), None);
    let mut app = headless::ai_vs_ai(c.clone(), &s).unwrap();
    headless::tick(&mut app, ticks);
    let j = app.world().resource::<onus::sim::AiJournal>();
    [Faction::A, Faction::B].map(|f| j.for_faction(f).into_iter().map(|(_, a)| a).collect())
}

/// The commander prices what it orders from **its own side's** table: a
/// worker and an army it cannot afford under its traits are never ordered, by
/// it, while the other side orders both.
#[test]
fn the_ai_prices_workers_and_army_at_its_sides_traited_cost() {
    use onus::sim::ai::AiAction;
    let c = one_trait(
        "ai_units_dear",
        "(applies_to: Workers, stat: AlloyCost, mul: 100.0),
         (applies_to: CombatUnits, stat: AlloyCost, mul: 100.0)",
    )
    .expect("loads");
    let [a, b] = ai_actions(&c, 4_000);
    let worker = |x: &AiAction| matches!(x, AiAction::TrainWorker { .. });
    let army = |x: &AiAction| matches!(x, AiAction::TrainArmy { .. });
    let barracks = |x: &AiAction| matches!(x, AiAction::PlaceBarracks { .. });
    assert!(
        b.iter().any(worker) && b.iter().any(army),
        "the window is long enough: {b:?}"
    );
    assert!(
        a.iter().any(barracks),
        "A still builds, so it could train: {a:?}"
    );
    assert!(
        !a.iter().any(worker),
        "A priced a worker at the base cost: {a:?}"
    );
    assert!(
        !a.iter().any(army),
        "A priced its army at the base cost: {a:?}"
    );
}

#[test]
fn the_ai_prices_its_barracks_at_its_sides_traited_cost() {
    use onus::sim::ai::AiAction;
    let c = one_trait(
        "ai_bld_dear",
        "(applies_to: Buildings, stat: AlloyCost, mul: 100.0)",
    )
    .expect("loads");
    let [a, b] = ai_actions(&c, 4_000);
    let barracks = |x: &AiAction| matches!(x, AiAction::PlaceBarracks { .. });
    assert!(b.iter().any(barracks), "the window is long enough: {b:?}");
    assert!(
        !a.iter().any(barracks),
        "A priced a barracks at the base cost: {a:?}"
    );
}

// ---- BuildingHp rounds the way every other `mul` does (critic finding 4) -----

#[test]
fn a_building_hp_trait_rounds_half_up_like_every_mul() {
    // 1.004 puts every shipped pool on a fraction: 8400 -> 8433.6 (up),
    // 5040 -> 5060.16 (down), 4200 -> 4216.8 (up).
    let c = one_trait(
        "bhp_round",
        "(applies_to: Buildings, stat: BuildingHp, mul: 1.004)",
    )
    .expect("loads");
    let r = x_defs(&c);
    let d = Defs::new(&c, Some(&r));
    let per = c.combat.building_hp_per_defense;
    let mut rounded_up = 0;
    for (i, b) in c.buildings.iter().enumerate() {
        let base = b.mvp_defense * per;
        let got = d.building_health(Faction::A, i).max;
        assert_eq!(got, traited(base, 1.004), "{}: round half up", b.id);
        if (base as f64 * 1.004).fract() >= 0.5 {
            assert_eq!(got, (base as f64 * 1.004).floor() as u32 + 1, "{}", b.id);
            rounded_up += 1;
        }
        assert_eq!(d.building_health(Faction::B, i).max, base, "B untouched");
    }
    assert!(rounded_up > 0, "the fixture has a pool that rounds up");

    // Two of them compose in RON order, each rounding once, as AlloyCost does.
    let c = one_trait(
        "bhp_compose",
        "(applies_to: Building(\"hq\"), stat: BuildingHp, mul: 1.004),
         (applies_to: Buildings, stat: BuildingHp, mul: 1.004)",
    )
    .expect("loads");
    let r = x_defs(&c);
    let d = Defs::new(&c, Some(&r));
    let hq = c.building_index("hq").unwrap();
    let base = c.buildings[hq].mvp_defense * c.combat.building_hp_per_defense;
    assert_eq!(
        d.building_health(Faction::A, hq).max,
        traited(traited(base, 1.004), 1.004)
    );
}

// ---- the mixed-army trait: House Vael's Synthesis (C2, user decision) --------

/// Content whose one nation `x` has no stat traits and the given mixed-army
/// trait (`synthesis: (...)` RON, or "" for none).
fn with_synthesis(name: &str, syn: &str) -> Result<Content, String> {
    let line = if syn.is_empty() {
        String::new()
    } else {
        format!("synthesis: {syn},")
    };
    let text = format!(
        r#"(nations: [(id: "x", name: "X", domain: None, colour: (0.1,0.2,0.3),
            accent: (0.4,0.5,0.6), general: "general_focus", strategy: "mvp",
            traits: [], {line})])"#
    );
    Content::load_from_dir(&with_nations(name, &text)).map_err(|e| e.to_string())
}

const SYN: &str =
    "(radius: 120.0, modifiers: [(stat: Offense, add: 1), (stat: Armor, add: 1)], max: 2)";

/// A skirmish where A plays `x`, with no commanders; `units` are spawned on
/// an open stretch of the map and the field is read after one tick.
fn synthesis_field(c: &Content, units: &[(Faction, &str, Vec2)]) -> Vec<StatBonus> {
    let s = MatchSettings::default().with_nations(Some("x"), None);
    let mut app = headless::battle(c.clone(), &s).expect("builds");
    app.insert_resource(AiCommanders::new(0, &[]));
    let es: Vec<Entity> = units
        .iter()
        .map(|&(f, id, at)| {
            let u = c.unit_index(id).unwrap();
            let hp = Health::from_def(c, u);
            app.world_mut()
                .spawn((
                    Position(Vec2::new(0.0, 650.0) + at),
                    UnitDefIdx(u),
                    c.units[u].mvp_kind,
                    f,
                    hp,
                ))
                .id()
        })
        .collect();
    headless::step(&mut app);
    let field = app
        .world()
        .get_resource::<AuraField>()
        .expect("a synthesis nation has the field");
    es.iter().map(|e| field.get(*e)).collect()
}

fn bonus(n: u32) -> StatBonus {
    StatBonus {
        offense: n,
        armor: n,
        ..StatBonus::NONE
    }
}

#[test]
fn synthesis_is_active_beside_other_domains_and_scales_per_domain() {
    let c = with_synthesis("syn_mixed", SYN).expect("loads");
    use Faction::A;
    // A sentinel (Machine) and a ripper (Flesh): one other domain each.
    let two = synthesis_field(
        &c,
        &[
            (A, "sentinel", Vec2::ZERO),
            (A, "ripper", Vec2::new(30.0, 0.0)),
        ],
    );
    assert_eq!(two, [bonus(1), bonus(1)]);
    // Add an arclight (Energy): two other domains each. A second machine unit
    // adds no domain to the first, but gives the others nothing new either.
    let three = synthesis_field(
        &c,
        &[
            (A, "sentinel", Vec2::ZERO),
            (A, "ripper", Vec2::new(30.0, 0.0)),
            (A, "arclight", Vec2::new(0.0, 30.0)),
            (A, "bulwark", Vec2::new(30.0, 30.0)),
        ],
    );
    assert_eq!(three, [bonus(2), bonus(2), bonus(2), bonus(2)]);
    // Out of the radius, a neighbour does not count.
    let far = synthesis_field(
        &c,
        &[
            (A, "sentinel", Vec2::ZERO),
            (A, "ripper", Vec2::new(200.0, 0.0)),
        ],
    );
    assert_eq!(far, [StatBonus::NONE, StatBonus::NONE]);
}

#[test]
fn synthesis_is_inactive_in_a_mono_domain_army() {
    let c = with_synthesis("syn_mono", SYN).expect("loads");
    use Faction::A;
    // Two Machine units and a worker (no domain): nothing to mix.
    let f = synthesis_field(
        &c,
        &[
            (A, "sentinel", Vec2::ZERO),
            (A, "bulwark", Vec2::new(30.0, 0.0)),
            (A, "worker", Vec2::new(0.0, 30.0)),
        ],
    );
    assert_eq!(f, [StatBonus::NONE; 3]);
}

#[test]
fn synthesis_counts_no_more_domains_than_its_max() {
    let c = with_synthesis(
        "syn_cap",
        "(radius: 120.0, modifiers: [(stat: Offense, add: 1), (stat: Armor, add: 1)], max: 1)",
    )
    .expect("loads");
    use Faction::A;
    let f = synthesis_field(
        &c,
        &[
            (A, "sentinel", Vec2::ZERO),
            (A, "ripper", Vec2::new(30.0, 0.0)),
            (A, "arclight", Vec2::new(0.0, 30.0)),
        ],
    );
    assert_eq!(f, [bonus(1); 3], "two other domains, capped at one");
}

#[test]
fn synthesis_counts_only_friends_and_only_its_own_side_gains() {
    let c = with_synthesis("syn_enemy", SYN).expect("loads");
    use Faction::{A, B};
    // A's sentinel stands among B's ripper and arclight: enemies, no mix.
    // B (no nation) is mixed but has no such trait.
    let f = synthesis_field(
        &c,
        &[
            (A, "sentinel", Vec2::ZERO),
            (B, "ripper", Vec2::new(30.0, 0.0)),
            (B, "arclight", Vec2::new(0.0, 30.0)),
        ],
    );
    assert_eq!(f, [StatBonus::NONE; 3]);
}

#[test]
fn only_a_synthesis_nation_installs_the_field_and_the_hash_sees_the_trait() {
    // A nation without the trait leaves the skirmish as it was: no field.
    let plain = with_synthesis("syn_none", "").expect("loads");
    let s = MatchSettings::default().with_nations(Some("x"), None);
    let app = headless::battle(plain.clone(), &s).unwrap();
    assert!(!app.world().contains_resource::<AuraField>());
    // The trait is content: the fingerprint sees it.
    let syn = with_synthesis("syn_some", SYN).unwrap();
    assert_ne!(plain.fingerprint().hash(), syn.fingerprint().hash());
}

#[test]
fn an_unplayable_synthesis_is_refused_at_load() {
    for (name, syn) in [
        (
            "syn_max0",
            "(radius: 120.0, modifiers: [(stat: Offense, add: 1)], max: 0)",
        ),
        (
            "syn_max3",
            "(radius: 120.0, modifiers: [(stat: Offense, add: 1)], max: 3)",
        ),
        (
            "syn_r0",
            "(radius: 0.0, modifiers: [(stat: Offense, add: 1)], max: 1)",
        ),
        ("syn_empty", "(radius: 120.0, modifiers: [], max: 1)"),
        (
            "syn_twice",
            "(radius: 120.0, modifiers: [(stat: Armor, add: 1), (stat: Armor, add: 1)], max: 1)",
        ),
        (
            "syn_ticks_add",
            "(radius: 120.0, modifiers: [(stat: AttackTicks, add: 1)], max: 1)",
        ),
        (
            "syn_nemesis",
            "(radius: 120.0, modifiers: [(stat: Nemesis, add: 1)], max: 1)",
        ),
    ] {
        assert!(with_synthesis(name, syn).is_err(), "{name} must be refused");
    }
}

#[test]
fn the_shipped_house_vael_has_the_synthesis_trait() {
    let c = shipped();
    let house = c.nations.iter().find(|n| n.id == "house").unwrap();
    let syn = house
        .synthesis
        .as_ref()
        .expect("House's edge is the mixed army");
    assert!(syn.max >= 1 && !syn.modifiers.is_empty());
    for n in c.nations.iter().filter(|n| n.id != "house") {
        assert!(n.synthesis.is_none(), "{} has no mixed-army trait", n.id);
    }
}
