//! C1 critic probes (rts-critic), landed as red tests ahead of their fixes.

use bevy::prelude::*;

use onus::headless::{self, MatchSettings};
use onus::sim::battle::{self, AuraField, BattleSetup, BattleState, General, LossCause, SideGround};
use onus::sim::combat::{Health, StatBonus};
use onus::sim::content::Content;
use onus::sim::economy::UnitDefIdx;
use onus::sim::spatial::Faction;
use onus::sim::{AiCommanders, BattleAiAction, BattleAiJournal, CommandQueue, MatchState, Order, Position, StateHashLog};

fn content() -> Content {
    headless::content().expect("content loads")
}

fn order(app: &mut App, o: Order) {
    app.world_mut().resource_mut::<CommandQueue>().0.push_back(o);
}

fn spawn_unit(app: &mut App, id: &str, faction: Faction, pos: Vec2) -> Entity {
    let (idx, kind, hp) = {
        let c = app.world().resource::<Content>();
        let idx = c.unit_index(id).unwrap();
        (idx, c.units[idx].mvp_kind, Health::from_def(c, idx))
    };
    app.world_mut().spawn((Position(pos), UnitDefIdx(idx), kind, faction, hp)).id()
}

fn generals_of(app: &mut App, side: Faction) -> Vec<Entity> {
    let mut q = app.world_mut().query_filtered::<(Entity, &Faction), With<General>>();
    let mut v: Vec<Entity> = q.iter(app.world()).filter(|(_, f)| **f == side).map(|(e, _)| e).collect();
    v.sort_by_key(|e| e.to_bits());
    v
}

fn run(app: &mut App, cap: u32) -> Option<u32> {
    for t in 0..cap {
        if app.world().resource::<MatchState>().outcome().is_some() {
            return Some(t);
        }
        headless::step(app);
    }
    None
}

/// Probe: a retreat on a non-axis-aligned front must finish. `install` is the
/// public entry C4 builds battles through and takes any `SideGround`.
#[test]
fn critic_retreat_finishes_on_a_diagonal_front() {
    let mut stalled = Vec::new();
    for deg in [10.0f32, 30.0, 45.0, 60.0, 73.0, 135.0, 200.0, 333.0] {
        let mut app = headless::ai_vs_ai(content(), &MatchSettings::default()).unwrap();
        app.insert_resource(AiCommanders::default());
        let setup = BattleSetup::default()
            .with_general(Faction::A, "general_logistics")
            .with_general(Faction::B, "general_bulwark");
        setup.check(app.world().resource::<Content>()).unwrap();
        let r = deg.to_radians();
        let front = Vec2::new(r.cos(), r.sin());
        let ground = [
            SideGround { base: Vec2::new(-750.0, 0.0), front },
            SideGround { base: Vec2::new(750.0, 0.0), front: -front },
        ];
        battle::install(app.world_mut(), &setup, ground);
        // a soldier of A's, somewhere on the field
        spawn_unit(&mut app, "ravager", Faction::A, Vec2::new(-300.0, 37.3));
        headless::step(&mut app);
        order(&mut app, Order::Retreat { faction: Faction::A }.issued_by(Faction::A));
        let done = run(&mut app, 60 * 120);
        if done.is_none() {
            stalled.push(deg);
            continue;
        }
        let res = app.world().resource::<BattleState>().result().cloned().unwrap();
        assert_eq!(res.loss(Faction::A), Some(LossCause::Retreated), "{deg} deg");
    }
    assert!(stalled.is_empty(), "retreat never finished at fronts {stalled:?} deg");
}

/// Probe: a retreat under pursuit still ends the battle (escape or fall), it
/// never stalls because combat holds retreating units in a fight.
#[test]
fn critic_retreat_under_pursuit_ends() {
    let setup = BattleSetup::default()
        .with_general(Faction::A, "general_logistics")
        .with_general(Faction::B, "general_bulwark");
    let mut app =
        headless::ai_vs_ai(content(), &MatchSettings::default().with_battle(setup)).unwrap();
    app.insert_resource(AiCommanders::default());
    let mut mine = Vec::new();
    for k in 0..6 {
        mine.push(spawn_unit(&mut app, "bulwark", Faction::A, Vec2::new(-100.0, k as f32 * 20.0)));
    }
    let mut theirs = Vec::new();
    for k in 0..6 {
        theirs.push(spawn_unit(&mut app, "ripper", Faction::B, Vec2::new(-40.0, k as f32 * 20.0)));
    }
    headless::step(&mut app);
    order(&mut app, Order::Retreat { faction: Faction::A }.issued_by(Faction::A));
    for _ in 0..20 {
        order(
            &mut app,
            Order::AttackMove { units: theirs.clone(), dest: Vec2::new(-1000.0, 0.0) }.issued_by(Faction::B),
        );
        headless::tick(&mut app, 30);
    }
    let done = run(&mut app, 60 * 120);
    assert!(done.is_some(), "a retreat under pursuit never ended");
    let r = app.world().resource::<BattleState>().result().cloned().unwrap();
    assert!(matches!(r.loss(Faction::A), Some(LossCause::Retreated) | Some(LossCause::GeneralsFell) | Some(LossCause::VictoryBuildingLost)));
}

/// Probe: Synthesis counts the domains of friendly units only.
#[test]
fn critic_synthesis_ignores_enemy_domains() {
    let mut app = headless::ai_vs_ai(
        content(),
        &MatchSettings::default().with_battle(
            BattleSetup::default()
                .with_general(Faction::A, "general_synthesis")
                .with_general(Faction::B, "general_bulwark"),
        ),
    )
    .unwrap();
    app.insert_resource(AiCommanders::default());
    let g = generals_of(&mut app, Faction::A)[0];
    app.world_mut().get_mut::<Position>(g).unwrap().0 = Vec2::ZERO;
    let mine = spawn_unit(&mut app, "bulwark", Faction::A, Vec2::new(10.0, 0.0));
    spawn_unit(&mut app, "ripper", Faction::B, Vec2::new(0.0, 15.0));
    spawn_unit(&mut app, "arclight", Faction::B, Vec2::new(0.0, -15.0));
    headless::step(&mut app);
    let c = content();
    let syn = c.generals.iter().find(|g| g.id == "general_synthesis").unwrap();
    let off = syn.aura.modifiers.iter().find(|m| format!("{:?}", m.stat) == "Offense").unwrap().add.unwrap();
    let b: StatBonus = app.world().resource::<AuraField>().get(mine);
    assert_eq!(b.offense, off, "one friendly domain => x1, enemies don't count");
}

/// Probe: one battle, played twice, hashes identically every tick (with the
/// AIs playing, auras, guard and retreat in play).
#[test]
fn critic_battle_is_deterministic_per_tick() {
    let s = MatchSettings::default()
        .with_seed(7)
        .with_hashing(true)
        .with_battle(
            BattleSetup::default()
                .with_general(Faction::A, "general_synthesis")
                .with_general(Faction::A, "general_frenzy")
                .with_general(Faction::B, "general_endurance")
                .with_general(Faction::B, "general_focus"),
        );
    let play = || {
        let mut app = headless::ai_vs_ai(content(), &s).unwrap();
        let end = run(&mut app, 60 * 60 * 15);
        let h = app.world().resource::<StateHashLog>().0.clone();
        let r = app.world().resource::<BattleState>().result().cloned();
        let j = app.world().resource::<BattleAiJournal>().0.clone();
        (h, format!("{r:?}"), format!("{j:?}"), end)
    };
    let a = play();
    let b = play();
    eprintln!("battle end {:?} result {} journal {}", a.3, a.1, a.2);
    assert_eq!(a.0, b.0);
    assert_eq!(a.1, b.1);
}

/// Probe (exploratory): in a default skirmish-start battle, when does the AI
/// retreat and how many fighters had it fielded?
#[test]
fn critic_ai_retreat_exploratory() {
    for seed in [0u64, 1, 2, 3] {
        let s = MatchSettings::default().with_seed(seed).with_battle(
            BattleSetup::default()
                .with_general(Faction::A, "general_synthesis")
                .with_general(Faction::B, "general_bulwark"),
        );
        let mut app = headless::ai_vs_ai(content(), &s).unwrap();
        let end = run(&mut app, 60 * 60 * 15);
        let r = app.world().resource::<BattleState>().result().cloned();
        let j: Vec<_> = app.world().resource::<BattleAiJournal>().0.iter().filter(|(_, _, a)| matches!(a, BattleAiAction::Retreat)).cloned().collect();
        eprintln!("seed {seed}: end {end:?} winner {:?} lost {:?} retreats {:?}", r.as_ref().map(|r| r.winner), r.as_ref().map(|r| r.lost), j);
    }
}


/// Probe: even on an axis-aligned front, a base at a non-round coordinate must
/// not leave a retreating unit a float ulp short of its edge.
#[test]
fn critic_retreat_finishes_on_an_axis_front_with_odd_base() {
    for bx in [-733.37f32, -701.913, -777.77, -640.123, -812.0625] {
        let mut app = headless::ai_vs_ai(content(), &MatchSettings::default()).unwrap();
        app.insert_resource(AiCommanders::default());
        let setup = BattleSetup::default()
            .with_general(Faction::A, "general_logistics")
            .with_general(Faction::B, "general_bulwark");
        let ground = [
            SideGround { base: Vec2::new(bx, 13.7), front: Vec2::X },
            SideGround { base: Vec2::new(-bx, 13.7), front: -Vec2::X },
        ];
        battle::install(app.world_mut(), &setup, ground);
        spawn_unit(&mut app, "ravager", Faction::A, Vec2::new(-300.3, 37.3));
        headless::step(&mut app);
        order(&mut app, Order::Retreat { faction: Faction::A }.issued_by(Faction::A));
        let done = run(&mut app, 60 * 120);
        assert!(done.is_some(), "base x {bx}: the retreat never finished");
    }
}

/// Probe: both sides retreat on the same tick (the shipped battle AI does this
/// in 3 of 5 sampled default battles). Neither held the field, so neither can
/// be reported as the winner with no loss — yet the slower withdrawer is.
#[test]
fn critic_a_mutual_retreat_does_not_crown_the_slower_withdrawer() {
    let setup = BattleSetup::default()
        .with_general(Faction::A, "general_bulwark")
        .with_general(Faction::B, "general_logistics");
    let mut app =
        headless::ai_vs_ai(content(), &MatchSettings::default().with_battle(setup)).unwrap();
    app.insert_resource(AiCommanders::default());
    spawn_unit(&mut app, "bulwark", Faction::A, Vec2::new(-100.0, 0.0));
    spawn_unit(&mut app, "ripper", Faction::B, Vec2::new(100.0, 0.0));
    headless::step(&mut app);
    order(&mut app, Order::Retreat { faction: Faction::A }.issued_by(Faction::A));
    order(&mut app, Order::Retreat { faction: Faction::B }.issued_by(Faction::B));
    assert!(run(&mut app, 60 * 120).is_some());
    let st = app.world().resource::<BattleState>();
    let r = st.result().cloned().unwrap();
    assert!(st.retreating(Faction::A) && st.retreating(Faction::B));
    assert!(
        r.winner.is_none() || r.lost.iter().all(|l| l.is_some()),
        "both sides withdrew, yet {:?} won with losses {:?}",
        r.winner,
        r.lost
    );
}

/// Probe: an enemy standing INSIDE a General's radius gets nothing. (The
/// shipped c1_aura test only checks an enemy outside the radius, so removing
/// the side filter in `battle::auras` passes it.)
#[test]
fn critic_an_enemy_inside_the_radius_gets_no_aura() {
    let mut app = headless::ai_vs_ai(
        content(),
        &MatchSettings::default().with_battle(
            BattleSetup::default()
                .with_general(Faction::A, "general_focus")
                .with_general(Faction::B, "general_bulwark"),
        ),
    )
    .unwrap();
    app.insert_resource(AiCommanders::default());
    let g = generals_of(&mut app, Faction::A)[0];
    app.world_mut().get_mut::<Position>(g).unwrap().0 = Vec2::ZERO;
    let enemy = spawn_unit(&mut app, "ravager", Faction::B, Vec2::new(5.0, 0.0));
    headless::step(&mut app);
    assert_eq!(app.world().resource::<AuraField>().get(enemy), StatBonus::NONE);
}

/// Probe: the Synthesis cap binds. Shipped data has max 3 = the number of
/// domains, so no shipped-content test can see the `.min(max)`; drop the
/// RON max to 2 and three domains must give x2.
#[test]
fn critic_synthesis_cap_binds_when_below_the_domain_count() {
    let src = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data");
    let out = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/critic_c1/cap2");
    std::fs::create_dir_all(&out).unwrap();
    for f in ["resources.ron", "strategies.ron"] {
        std::fs::copy(src.join(f), out.join(f)).unwrap();
    }
    let text = std::fs::read_to_string(src.join("units.ron")).unwrap();
    assert!(text.contains("PerDistinctDomain(max: 3)"));
    std::fs::write(out.join("units.ron"), text.replace("PerDistinctDomain(max: 3)", "PerDistinctDomain(max: 2)")).unwrap();
    let c = Content::load_from_dir(&out).expect("max 2 loads");
    let syn = c.generals.iter().find(|g| g.id == "general_synthesis").unwrap();
    let off = syn.aura.modifiers.iter().find(|m| format!("{:?}", m.stat) == "Offense").unwrap().add.unwrap();
    let mut app = headless::ai_vs_ai(
        c,
        &MatchSettings::default().with_battle(
            BattleSetup::default()
                .with_general(Faction::A, "general_synthesis")
                .with_general(Faction::B, "general_bulwark"),
        ),
    )
    .unwrap();
    app.insert_resource(AiCommanders::default());
    let g = generals_of(&mut app, Faction::A)[0];
    app.world_mut().get_mut::<Position>(g).unwrap().0 = Vec2::ZERO;
    let u = spawn_unit(&mut app, "ravager", Faction::A, Vec2::new(10.0, 0.0));
    spawn_unit(&mut app, "bulwark", Faction::A, Vec2::new(0.0, 10.0));
    spawn_unit(&mut app, "arclight", Faction::A, Vec2::new(0.0, -10.0));
    headless::step(&mut app);
    assert_eq!(app.world().resource::<AuraField>().get(u).offense, 2 * off);
}
