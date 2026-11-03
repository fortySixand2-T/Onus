//! C1 AC7 — **the battle AI**: it protects its General (an enemy fighter
//! within `guard_radius` of him pulls its army onto him) and retreats on a
//! data-driven threshold (`retreat_at_army_lost`), both read from
//! `units.ron` `mvp_general_ai` (CAMPAIGN_PLAN C1, DESIGN_BRIEF "Campaign
//! battles"). A battle side with no city still fights.

use std::path::PathBuf;

use bevy::prelude::*;

use onus::headless::{self, MatchSettings};
use onus::sim::battle::{BattleSetup, BattleState, General, Roster};
use onus::sim::combat::Health;
use onus::sim::content::Content;
use onus::sim::economy::{Building, UnitDefIdx};
use onus::sim::spatial::Faction;
use onus::sim::{
    AiAction, AiCommanders, AiJournal, AttackMoveDest, BattleAiAction, BattleAiJournal, Position,
};

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data")
}

fn content() -> Content {
    Content::load_from_dir(&data_dir()).expect("content loads")
}

fn think() -> u32 {
    content().ai.think_interval_ticks
}

fn mutated(name: &str, edit: impl Fn(&str) -> String) -> Result<Content, String> {
    let out = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/c1_ai")
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

/// A battle where only A's commander plays (B stands still), so every
/// decision in the journal is A's response to what the test placed.
fn a_plays(setup: BattleSetup) -> App {
    let settings = MatchSettings::default().with_battle(setup);
    let mut app = headless::ai_vs_ai(content(), &settings).expect("battle builds");
    app.insert_resource(AiCommanders::new(settings.seed, &[Faction::A]));
    app
}

fn generals_of(app: &mut App, side: Faction) -> Vec<(Entity, Vec2)> {
    let mut q = app
        .world_mut()
        .query_filtered::<(Entity, &Faction, &Position), With<General>>();
    let mut v: Vec<_> = q
        .iter(app.world())
        .filter(|(_, f, _)| **f == side)
        .map(|(e, _, p)| (e, p.0))
        .collect();
    v.sort_by_key(|(e, _)| e.to_bits());
    v
}

fn hq_of(app: &mut App, side: Faction) -> Vec2 {
    let c = content();
    let mut q = app.world_mut().query::<(&Building, &Faction, &Position)>();
    q.iter(app.world())
        .find(|(b, f, _)| **f == side && c.buildings[b.def].victory)
        .map(|(.., p)| p.0)
        .unwrap()
}

fn fighters_of(app: &mut App, side: Faction) -> Vec<Entity> {
    let c = content();
    let mut q = app
        .world_mut()
        .query_filtered::<(Entity, &Faction, &UnitDefIdx), Without<General>>();
    let mut v: Vec<_> = q
        .iter(app.world())
        .filter(|(_, f, d)| **f == side && c.units.get(d.0).is_some_and(|u| u.offense > 0))
        .map(|(e, ..)| e)
        .collect();
    v.sort_by_key(|e| e.to_bits());
    v
}

fn spawn_unit(app: &mut App, id: &str, faction: Faction, pos: Vec2) -> Entity {
    let c = content();
    let idx = c.unit_index(id).unwrap();
    app.world_mut()
        .spawn((
            Position(pos),
            UnitDefIdx(idx),
            c.units[idx].mvp_kind,
            faction,
            Health::from_def(&c, idx),
        ))
        .id()
}

fn journal(app: &App) -> Vec<(u32, AiAction)> {
    app.world().resource::<AiJournal>().for_faction(Faction::A)
}

/// A's battle decisions (guard, retreat): their own journal, so the skirmish
/// action trace is untouched.
fn battle_journal(app: &App) -> Vec<(u32, BattleAiAction)> {
    app.world().resource::<BattleAiJournal>().for_faction(Faction::A)
}

fn ravagers(n: usize) -> Roster {
    (0..n).fold(Roster::default().building("hq"), |r, _| r.unit("ravager"))
}

fn guarded(n: usize) -> BattleSetup {
    BattleSetup::default()
        .with_general(Faction::A, "general_focus")
        .with_roster(Faction::A, ravagers(n))
        .with_general(Faction::B, "general_bulwark")
}

#[test]
fn the_battle_ai_reads_its_numbers_from_ron_and_refuses_bad_ones() {
    let c = content();
    let ai = c.general_ai.as_ref().expect("the shipped content has a battle AI");
    let text = std::fs::read_to_string(data_dir().join("units.ron")).unwrap();
    assert!(text.contains(&format!("guard_radius: {:.1}", ai.guard_radius)));
    assert!(text.contains(&format!("retreat_at_army_lost: {}", ai.retreat_at_army_lost)));
    assert_eq!(ai.retreat_milli(), (ai.retreat_at_army_lost * 1000.0).round() as u32);
    let cases: [(&str, &str, &str); 3] = [
        ("guard0", "guard_radius: 150.0", "guard_radius: 0.0"),
        ("lost0", "retreat_at_army_lost: 0.6", "retreat_at_army_lost: 0.0"),
        ("lost_over", "retreat_at_army_lost: 0.6", "retreat_at_army_lost: 1.5"),
    ];
    for (name, from, to) in cases {
        let err = mutated(name, |t| t.replacen(from, to, 1)).expect_err(name);
        assert!(err.contains("mvp_general_ai"), "{name}: {err}");
    }
    let err = mutated("no_block", |t| {
        let at = t.find("    mvp_general_ai: (").unwrap();
        let end = at + t[at..].find("    ),\n").unwrap() + "    ),\n".len();
        format!("{}{}", &t[..at], &t[end..])
    })
    .expect_err("generals with no battle AI");
    assert!(err.contains("mvp_general_ai"), "{err}");
}

#[test]
fn an_enemy_fighter_near_the_general_pulls_the_army_onto_him() {
    let c = content();
    let radius = c.general_ai.as_ref().unwrap().guard_radius;
    let mut app = a_plays(guarded(4));
    let (g, gpos) = generals_of(&mut app, Faction::A)[0];
    let front = (gpos - hq_of(&mut app, Faction::A)).normalize();
    spawn_unit(&mut app, "ravager", Faction::B, gpos + front * (radius - 10.0));
    headless::step(&mut app);
    let bj = battle_journal(&app);
    assert_eq!(bj, [(0, BattleAiAction::Guard { general: g, force: 4 })]);
    let j = journal(&app);
    assert!(!j.iter().any(|(_, a)| matches!(a, AiAction::Attack { .. })), "a guard, not a wave");
    for e in fighters_of(&mut app, Faction::A) {
        assert_eq!(app.world().get::<AttackMoveDest>(e).map(|d| d.0), Some(gpos));
    }
}

#[test]
fn a_threat_outside_the_radius_or_one_that_cannot_fight_does_not() {
    let c = content();
    let radius = c.general_ai.as_ref().unwrap().guard_radius;
    for (id, d) in [("ravager", radius + 10.0), ("worker", radius - 10.0)] {
        let mut app = a_plays(guarded(4));
        let (_, gpos) = generals_of(&mut app, Faction::A)[0];
        let front = (gpos - hq_of(&mut app, Faction::A)).normalize();
        spawn_unit(&mut app, id, Faction::B, gpos + front * d);
        headless::step(&mut app);
        assert!(battle_journal(&app).is_empty(), "{id}: {:?}", battle_journal(&app));
        let j = journal(&app);
        assert!(j.iter().any(|(_, a)| matches!(a, AiAction::Attack { force: 4, .. })), "{id}");
    }
}

/// Field `n` fighters, let A decide once, take `lose` of them off, and run
/// one more decision: did A retreat?
fn retreats_after_losing(n: usize, lose: usize) -> (bool, Vec<(u32, BattleAiAction)>) {
    let think = content().ai.think_interval_ticks;
    let mut app = a_plays(guarded(n));
    headless::step(&mut app);
    for e in fighters_of(&mut app, Faction::A).into_iter().take(lose) {
        app.world_mut().despawn(e);
    }
    headless::tick(&mut app, think);
    let j = battle_journal(&app);
    (app.world().resource::<BattleState>().retreating(Faction::A), j)
}

#[test]
fn it_retreats_once_it_has_lost_the_ron_share_of_its_army_and_not_before() {
    let milli = content().general_ai.unwrap().retreat_milli() as usize;
    let n = 10;
    let at = (n * milli).div_ceil(1000); // the fewest losses that reach the threshold
    let (gone, j) = retreats_after_losing(n, at);
    assert!(gone, "{at} of {n} lost: {j:?}");
    assert_eq!(j, [(think(), BattleAiAction::Retreat)], "one retreat, on the next decision");
    let (stays, j) = retreats_after_losing(n, at - 1);
    assert!(!stays, "{} of {n} lost: {j:?}", at - 1);
    assert!(j.is_empty(), "{j:?}");
}

#[test]
fn once_retreating_it_issues_nothing_more() {
    let think = content().ai.think_interval_ticks;
    let mut app = a_plays(guarded(5));
    headless::step(&mut app);
    for e in fighters_of(&mut app, Faction::A) {
        app.world_mut().despawn(e);
    }
    headless::tick(&mut app, think);
    assert_eq!(battle_journal(&app), [(think, BattleAiAction::Retreat)]);
    let n = journal(&app).len();
    headless::tick(&mut app, 3 * think);
    assert_eq!(journal(&app).len(), n, "nothing after the retreat");
    assert_eq!(battle_journal(&app).len(), 1);
}

#[test]
fn a_side_with_no_city_still_attacks_with_all_it_has() {
    let c = content();
    let wave = c.ai.attack_at_army as usize;
    assert!(wave >= 2, "the test fields fewer than a full wave");
    let n = wave - 1;
    let setup = BattleSetup::default()
        .with_general(Faction::A, "general_focus")
        .with_roster(Faction::A, (0..n).fold(Roster::default(), |r, _| r.unit("ravager")))
        .with_general(Faction::B, "general_bulwark");
    let mut app = a_plays(setup);
    let target = hq_of(&mut app, Faction::B);
    headless::step(&mut app);
    let j = journal(&app);
    let spread = c.ai.attack_spread;
    let attack = j
        .iter()
        .find_map(|(_, a)| match a {
            AiAction::Attack { dest, force } => Some((*dest, *force)),
            _ => None,
        })
        .expect("attacks without a city");
    assert_eq!(attack.1, n as u32);
    assert!((attack.0 - target).abs().max_element() <= spread, "at the enemy HQ");
}

#[test]
fn in_a_field_battle_the_wave_goes_for_the_enemy_general() {
    let c = content();
    let setup = BattleSetup::default()
        .with_general(Faction::A, "general_focus")
        .with_roster(Faction::A, Roster::default().unit("ravager").unit("ravager"))
        .with_general(Faction::B, "general_bulwark")
        .with_roster(Faction::B, Roster::default().unit("ravager"));
    let mut app = a_plays(setup);
    let (_, gb) = generals_of(&mut app, Faction::B)[0];
    headless::step(&mut app);
    let dest = journal(&app)
        .iter()
        .find_map(|(_, a)| match a {
            AiAction::Attack { dest, .. } => Some(*dest),
            _ => None,
        })
        .expect("attacks the General");
    assert!((dest - gb).abs().max_element() <= c.ai.attack_spread);
}
