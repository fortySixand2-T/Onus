//! C2 AC5 — **the client: pick a nation, and see the opponent's at a glance**
//! (pillar P3: colour plus a name label).
//!
//! Driven headlessly under `MinimalPlugins` through the shipped driver: the
//! shipped `setup`, the shipped nation-pick systems (registered by the one
//! [`onus::add_nation_pick`] the game itself uses) and the shipped sim chain.
//! Every assertion is on what the player is shown (label text and colour, a
//! sprite's colour, the panel) or on what the sim was set up with — never on
//! the client's own bookkeeping alone.
//!
//! What this cannot judge, and a human must look at on the Mac: that the
//! labels sit where they read well, that the colours tell the sides apart on
//! screen, and the unit rings (a `Gizmos` system, which needs the render
//! plugins).

use std::path::PathBuf;

use bevy::prelude::*;

use onus::client::{
    cycle_nation, nation_color, nation_name, NationLabel, NationPick, NationPicker, OptionsPanel,
    PickPhase, Selected, BUILDING_COLOR, NATION_NEXT_KEY, NATION_PREV_KEY, OPPONENT_NEXT_KEY,
    START_KEY,
};
use onus::sim::ai::AiCommanders;
use onus::sim::combat::Health;
use onus::sim::content::Content;
use onus::sim::economy::{Building, ProductionQueue, Stockpiles};
use onus::sim::nation::{Defs, NationRules};
use onus::sim::spatial::Faction;
use onus::sim::{CommandQueue, MatchState, Position, RateReport, TickGate};

fn content() -> Content {
    Content::load_from_dir(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data"))
        .expect("assets/data/*.ron")
}

/// The shipped driver minus the window: the shipped `setup`, the nation pick
/// and the sim chain (in `Update`, one tick per frame, as every L2 test runs
/// it), plus the panel.
fn app() -> App {
    let c = content();
    let alloy = c.economy.starting_alloy;
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .insert_resource(Time::<Fixed>::from_hz(60.0))
        .insert_resource(c)
        .init_resource::<CommandQueue>()
        .init_resource::<RateReport>()
        .init_resource::<ButtonInput<KeyCode>>()
        .insert_resource(Stockpiles::starting(alloy))
        .add_systems(Startup, onus::setup::setup);
    onus::add_sim_systems(&mut app, Update);
    onus::add_nation_pick(&mut app);
    app.add_systems(Update, onus::ui::update_options_panel);
    app.update(); // Startup, and the first (gated) frame.
    app
}

/// One frame with `key` pressed on it.
fn press(app: &mut App, key: KeyCode) {
    app.world_mut().resource_mut::<ButtonInput<KeyCode>>().press(key);
    app.update();
    let mut keys = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
    keys.release(key);
    keys.clear();
}

fn frames(app: &mut App, n: u32) {
    for _ in 0..n {
        app.update();
    }
}

fn pick(app: &App) -> NationPick {
    app.world().resource::<NationPick>().clone()
}

fn tick(app: &App) -> u32 {
    app.world().resource::<MatchState>().tick()
}

/// The label of `faction`: its text and its colour.
fn label(app: &mut App, faction: Faction) -> (String, Color) {
    let mut q = app.world_mut().query::<(&NationLabel, &Text, &TextColor)>();
    let found: Vec<(String, Color)> = q
        .iter(app.world())
        .filter(|(l, _, _)| l.0 == faction)
        .map(|(_, t, c)| (t.0.clone(), c.0))
        .collect();
    assert_eq!(found.len(), 1, "one label for {faction:?}");
    found[0].clone()
}

fn picker_text(app: &mut App) -> String {
    let mut q = app.world_mut().query_filtered::<&Text, With<NationPicker>>();
    q.single(app.world()).expect("one picker").0.clone()
}

fn hq_color(app: &mut App, faction: Faction) -> Color {
    let mut q = app.world_mut().query::<(&Faction, &Building, &Sprite)>();
    q.iter(app.world())
        .find(|(f, _, _)| **f == faction)
        .map(|(_, _, s)| s.color)
        .expect("an HQ")
}

fn index(c: &Content, id: &str) -> usize {
    c.nations.iter().position(|n| n.id == id).expect("a shipped nation")
}

/// Press `key` until the picked nation of `side` is `want`.
fn choose(app: &mut App, opponent: bool, want: Option<usize>) {
    let key = if opponent { OPPONENT_NEXT_KEY } else { NATION_NEXT_KEY };
    for _ in 0..=app.world().resource::<Content>().nations.len() + 1 {
        let p = pick(app);
        if (if opponent { p.opponent } else { p.player }) == want {
            return;
        }
        press(app, key);
    }
    panic!("never reached {want:?}");
}

#[test]
fn the_cycle_runs_through_every_nation_and_no_nation_and_wraps() {
    let n = 6;
    let mut at = None;
    let mut seen = vec![at];
    for _ in 0..n {
        at = cycle_nation(at, n, true);
        seen.push(at);
    }
    assert_eq!(seen, vec![None, Some(0), Some(1), Some(2), Some(3), Some(4), Some(5)]);
    assert_eq!(cycle_nation(Some(5), n, true), None, "wraps forward");
    assert_eq!(cycle_nation(None, n, false), Some(5), "wraps back");
    assert_eq!(cycle_nation(Some(0), n, false), None);
    assert_eq!(cycle_nation(None, 0, true), None, "no nations: only no nation");
}

#[test]
fn the_match_waits_on_the_pick_and_the_sim_does_not_tick() {
    let mut app = app();
    let c = content();
    let p = pick(&app);
    assert_eq!(p.phase, PickPhase::Picking);
    assert_eq!((p.player, p.opponent), (Some(0), Some(1)), "a nation each by default");
    assert!(!app.world().resource::<TickGate>().is_open());
    frames(&mut app, 30);
    assert_eq!(tick(&app), 0, "nothing is played while the player picks");
    assert!(!app.world().contains_resource::<NationRules>());
    let text = picker_text(&mut app);
    assert!(text.contains(&c.nations[0].name), "{text}");
    assert!(text.contains(&c.nations[1].name), "{text}");
    // Proof the counter would move: start, and it does.
    press(&mut app, START_KEY);
    frames(&mut app, 5);
    assert!(tick(&app) > 0);
    assert!(picker_text(&mut app).is_empty(), "the picker is gone once playing");
}

#[test]
fn the_picked_nations_are_the_matchs_nations_and_the_opponent_plays_its_personality() {
    let mut app = app();
    let c = content();
    let (me, them) = (index(&c, "ashen_reach"), index(&c, "verdant"));
    choose(&mut app, false, Some(me));
    choose(&mut app, true, Some(them));
    press(&mut app, NATION_PREV_KEY);
    press(&mut app, NATION_NEXT_KEY);
    assert_eq!(pick(&app).player, Some(me), "prev then next is a no-op");
    assert!(!app.world().contains_resource::<NationRules>(), "nothing applied before start");

    press(&mut app, START_KEY);
    let rules = app.world().resource::<NationRules>().clone();
    assert_eq!(rules.nation_of(Faction::A), Some(me));
    assert_eq!(rules.nation_of(Faction::B), Some(them));
    assert!(app.world().resource::<TickGate>().is_open());
    // The AI plays the opponent nation's own script.
    let ai = app.world().resource::<AiCommanders>();
    let b = ai.commanders().iter().find(|x| x.faction == Faction::B).unwrap();
    assert_eq!(b.strategy(&c).id, c.nations[them].strategy);
    // The start was given its side's tables: Ashen Reach's HQ has the traited pool.
    let want = Defs::new(&c, Some(&rules)).building_health(Faction::A, c.building_index("hq").unwrap());
    assert_ne!(want, Health::from_building_def(&c, c.building_index("hq").unwrap()));
    let mut q = app.world_mut().query::<(&Faction, &Building, &Health)>();
    let hq = q.iter(app.world()).find(|(f, _, _)| **f == Faction::A).unwrap().2;
    assert_eq!(hq.max, want.max);
}

#[test]
fn once_playing_the_keys_change_nothing() {
    let mut app = app();
    press(&mut app, START_KEY);
    let before = (pick(&app), app.world().resource::<NationRules>().clone());
    for key in [NATION_NEXT_KEY, OPPONENT_NEXT_KEY, NATION_PREV_KEY, START_KEY] {
        press(&mut app, key);
    }
    let after = (pick(&app), app.world().resource::<NationRules>().clone());
    assert_eq!((before.0.player, before.0.opponent), (after.0.player, after.0.opponent));
    assert_eq!(after.0.phase, PickPhase::Playing);
    for f in [Faction::A, Faction::B] {
        assert_eq!(before.1.nation_of(f), after.1.nation_of(f));
    }
}

#[test]
fn both_sides_are_labelled_by_name_in_their_nations_colour() {
    let mut app = app();
    let c = content();
    let (me, them) = (index(&c, "forgeborn"), index(&c, "aetherkin"));
    choose(&mut app, false, Some(me));
    choose(&mut app, true, Some(them));
    // While picking the labels already show the choice.
    let (text, colour) = label(&mut app, Faction::B);
    assert!(text.contains(&c.nations[them].name), "{text}");
    assert_eq!(colour, nation_color(&c, Some(them)));
    press(&mut app, START_KEY);
    frames(&mut app, 2);
    let (mine, my_colour) = label(&mut app, Faction::A);
    let (theirs, their_colour) = label(&mut app, Faction::B);
    assert!(mine.contains(nation_name(&c, Some(me))), "{mine}");
    assert!(theirs.contains(nation_name(&c, Some(them))), "{theirs}");
    assert!(theirs.to_lowercase().contains("opponent"), "{theirs}");
    assert_eq!(my_colour, nation_color(&c, Some(me)));
    assert_eq!(their_colour, nation_color(&c, Some(them)));
    assert_ne!(my_colour, their_colour);
    // And the bases wear it.
    assert_eq!(hq_color(&mut app, Faction::B), their_colour);
    assert_eq!(hq_color(&mut app, Faction::A), my_colour);
}

#[test]
fn no_nation_on_both_sides_is_todays_match() {
    let mut app = app();
    choose(&mut app, false, None);
    choose(&mut app, true, None);
    let (text, _) = label(&mut app, Faction::B);
    assert!(text.contains(nation_name(&content(), None)), "{text}");
    press(&mut app, START_KEY);
    frames(&mut app, 3);
    assert!(tick(&app) > 0);
    assert!(!app.world().contains_resource::<NationRules>(), "the golden path: no rules");
    let ai = app.world().resource::<AiCommanders>();
    let b = ai.commanders().iter().find(|x| x.faction == Faction::B).unwrap();
    assert_eq!(b.strategy_index(), None, "the default AI, as before C2");
    assert_eq!(hq_color(&mut app, Faction::B), BUILDING_COLOR);
}

/// The train line prices what the player's nation actually pays.
#[test]
fn the_panel_prices_training_at_the_players_nation_cost() {
    let mut app = app();
    let c = content();
    let forge = index(&c, "forgeborn");
    choose(&mut app, false, Some(forge));
    press(&mut app, START_KEY);
    let foundry = c.building_index("foundry").unwrap();
    app.world_mut().spawn((
        Position(Vec2::new(-300.0, -200.0)),
        Building { def: foundry },
        Faction::A,
        ProductionQueue::default(),
        Health::from_building_def(&c, foundry),
        Selected,
    ));
    for e in app
        .world_mut()
        .query_filtered::<Entity, (With<Selected>, Without<Building>)>()
        .iter(app.world())
        .collect::<Vec<_>>()
    {
        app.world_mut().entity_mut(e).remove::<Selected>();
    }
    app.update();
    let rules = app.world().resource::<NationRules>().clone();
    let bulwark = c.unit_index("bulwark").unwrap();
    let cost = Defs::new(&c, Some(&rules))
        .roster_unit(Faction::A, bulwark)
        .unwrap()
        .mvp_alloy_cost;
    assert_ne!(cost, c.units[bulwark].mvp_alloy_cost, "Forgeborn's foundry discount");
    let mut q = app.world_mut().query_filtered::<&Text, With<OptionsPanel>>();
    let panel = q.single(app.world()).unwrap().0.clone();
    assert!(panel.contains(&format!("Bulwark ({cost})")), "{panel}");
}
