//! C6 AC4 — **screens and round flow** (compiled into the `c6_live` test
//! binary: each test binary costs 1.8 GB on the build box).
//!
//! The campaign client, driven headlessly under `MinimalPlugins` with its
//! logic systems only (the view, sprites, gizmos and text, is the shipped
//! app's): the screens are Bevy states, the map's keys stage `Decision`s,
//! Enter plays the round on them, a refused round shows why and changes
//! nothing, and the round's report shows on the map. City positions come
//! from `assets/data/map_layout.ron`, validated against the campaign map.
//!
//! For the user on the Mac (F-099): that the map reads (positions, holder
//! colours, labels), that the panel text fits, and the feel of the keys.

use std::path::PathBuf;

use bevy::prelude::*;

use onus::campaign::story::Story;
use onus::campaign::{BattleOutcome, BattlePlan, Campaign, CampaignContent, Decision, Side};
use onus::campaign_client::{
    self as cc, campaign_args, map_text, CampaignSession, MapLayout, Screen, CAMPAIGN_SEED,
};
use onus::sim::content::Content;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data")
}
fn content() -> Content {
    Content::load_from_dir(&data_dir()).expect("content loads")
}
fn campaign_content() -> CampaignContent {
    CampaignContent::shipped_with_diplomacy(&content()).unwrap()
}

/// Holds every battle: a scripted, instant resolver for the AI's battles.
pub fn hold(p: &BattlePlan) -> BattleOutcome {
    BattleOutcome::unchanged(p, Some(Side::Defend))
}

/// The attacker takes every battle.
pub fn storm(p: &BattlePlan) -> BattleOutcome {
    BattleOutcome::unchanged(p, Some(Side::Attack))
}

/// A live session for House on `seed`, AI battles on `hold`.
pub fn session(seed: u64) -> CampaignSession {
    session_on(seed, Box::new(hold))
}

/// A live session for House on `seed`, AI battles on `resolver`.
pub fn session_on(seed: u64, resolver: Box<dyn onus::campaign::Resolver + Send + Sync>) -> CampaignSession {
    let c = Campaign::new(content(), campaign_content(), seed, Some("house")).unwrap().with_live_battles();
    let story = Story::shipped(c.content(), c.campaign()).unwrap();
    let layout = MapLayout::shipped(c.campaign()).unwrap();
    CampaignSession::new(c, story, layout, resolver)
}

/// The client's logic on a headless app, the sim chain on `Update` (one
/// tick per update, as every headless test), started on `s`.
pub fn app(s: CampaignSession) -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .insert_resource(Time::<Fixed>::from_hz(60.0))
        .insert_resource(s.campaign.content().clone())
        .init_resource::<ButtonInput<KeyCode>>();
    cc::add_campaign_client(&mut app, Update);
    cc::start(&mut app, s);
    app.update();
    app
}

pub fn press(app: &mut App, key: KeyCode) {
    app.world_mut().resource_mut::<ButtonInput<KeyCode>>().press(key);
    app.update();
    let mut keys = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
    keys.release(key);
    keys.clear();
    // The state change the key asked for lands on the next frame.
    app.update();
}

pub fn screen(app: &App) -> Screen {
    app.world().resource::<State<Screen>>().get().clone()
}
pub fn sess(app: &App) -> &CampaignSession {
    app.world().resource::<CampaignSession>()
}

#[test]
fn the_shipped_layout_places_every_city_and_a_bad_one_is_refused() {
    let cc = campaign_content();
    let l = MapLayout::shipped(&cc).unwrap();
    assert_eq!(l.positions().len(), cc.cities.len());
    let text = std::fs::read_to_string(data_dir().join("map_layout.ron")).unwrap();
    let refuse = |t: &str, says: &str| {
        let e = MapLayout::from_ron(t, &cc).expect_err(says);
        assert!(e.contains(says), "`{e}` should say `{says}`");
    };
    let first = "(id: \"cinderhold\", x: 0.0, y: 0.0)";
    assert!(text.contains(first), "the layout names Cinderhold at the centre");
    refuse(&text.replacen(first, "", 1), "cinderhold");
    refuse(&text.replacen(first, "(id: \"atlantis\", x: 0.0, y: 0.0)", 1), "atlantis");
    refuse(&text.replacen(first, &format!("{first}, {first}"), 1), "twice");
    refuse(&text.replacen(first, "(id: \"cinderhold\", x: 0.0, y: 150.0)", 1), "same place");
    refuse(&text.replacen(first, "(id: \"cinderhold\", x: inf, y: 0.0)", 1), "finite");
    refuse("(cities: [", "parse");
}

#[test]
fn campaign_is_chosen_on_the_command_line_and_the_skirmish_is_untouched() {
    let args = |a: &[&str]| campaign_args(a.iter().map(|s| s.to_string()));
    assert_eq!(args(&["onus"]), None, "no argument: the skirmish");
    assert_eq!(args(&["onus", "campaign"]), Some(CAMPAIGN_SEED));
    assert_eq!(args(&["onus", "campaign", "42"]), Some(42));
    let main = std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/main.rs")).unwrap();
    assert!(main.contains("onus::build_app().run()"), "the skirmish is still build_app");
    assert!(main.contains("campaign_args"), "the campaign is chosen by argument");
    // The shipped session: House Vael, the shipped map with diplomacy, live
    // battles, the Rupture first.
    let s = cc::shipped_session(CAMPAIGN_SEED).unwrap();
    let c = &s.campaign;
    assert_eq!(c.player(), c.nation_index("house"));
    assert!(c.campaign().diplomacy().is_some());
    assert!(c.live_battles());
    assert_eq!(c.seed(), CAMPAIGN_SEED);
    assert_eq!(c.round(), 0);
}

#[test]
fn the_client_opens_on_the_story_then_the_map_shows_cities_roads_and_armies() {
    let mut app = app(session(3));
    assert_eq!(screen(&app), Screen::Story, "the campaign opens on the story");
    press(&mut app, KeyCode::Enter);
    assert_eq!(screen(&app), Screen::Map);
    let t = map_text(sess(&app));
    for want in ["Cinderhold", "Ironspire", "House Vael", "Forgeborn", "capital", "HQ", "Round 0"] {
        assert!(t.contains(want), "map text lacks `{want}`:\n{t}");
    }
    assert!(t.contains("Cinderhold - Forge Gate"), "roads are listed:\n{t}");
    assert!(t.contains("Heir of Vael"), "armies are listed with their General:\n{t}");
}

#[test]
fn keys_stage_a_march_and_enter_plays_the_round_on_it() {
    let mut app = app(session(3));
    press(&mut app, KeyCode::Enter);
    let h = sess(&app).campaign.nation_index("house").unwrap();
    let army = sess(&app).campaign.armies().iter().find(|a| a.nation == h).unwrap().id;
    press(&mut app, KeyCode::Tab);
    assert_eq!(sess(&app).cursor.army, Some(army), "Tab selects the player's army");
    // Right cycles the neighbours of Cinderhold, in road order: Forge Gate
    // is the first.
    press(&mut app, KeyCode::ArrowRight);
    press(&mut app, KeyCode::KeyM);
    assert_eq!(sess(&app).staged, vec![Decision::March { army, to: "forge_gate".into() }]);
    // M again on another neighbour replaces the march: one order per army.
    press(&mut app, KeyCode::ArrowRight);
    press(&mut app, KeyCode::KeyM);
    assert_eq!(sess(&app).staged, vec![Decision::March { army, to: "rootfall".into() }]);
    assert!(map_text(sess(&app)).contains("March army"), "the staged order shows");
    // The same decisions, fed headlessly, are what the client plays.
    let mut twin = sess(&app).campaign.clone();
    let mut r = hold;
    twin.play_round(&sess(&app).staged.clone(), &mut r).unwrap();
    press(&mut app, KeyCode::Enter);
    assert_eq!(screen(&app), Screen::Map);
    let c = &sess(&app).campaign;
    assert_eq!(c.round(), 1);
    assert_eq!(c.state_hash(), twin.state_hash());
    assert!(c.army(army).unwrap().at().is_none(), "the army marches");
    assert!(sess(&app).staged.is_empty(), "the staged orders were played");
    let t = map_text(sess(&app));
    assert!(t.contains("Round 1 report"), "the round's report shows:\n{t}");
}

#[test]
fn a_refused_round_shows_why_and_changes_nothing() {
    let mut app = app(session(3));
    press(&mut app, KeyCode::Enter);
    // Up/Down pick a city of the player's; R stages a raise there. The
    // treasury is empty in round 1: the round is refused.
    press(&mut app, KeyCode::ArrowDown);
    press(&mut app, KeyCode::KeyR);
    assert_eq!(sess(&app).staged, vec![Decision::Raise { city: "cinderhold".into() }]);
    let before = sess(&app).campaign.state_hash();
    press(&mut app, KeyCode::Enter);
    assert_eq!(screen(&app), Screen::Map);
    assert_eq!(sess(&app).campaign.round(), 0);
    assert_eq!(sess(&app).campaign.state_hash(), before);
    let msg = sess(&app).message.clone().expect("a message");
    assert!(msg.contains("treasury"), "{msg}");
    assert!(map_text(sess(&app)).contains(&msg));
    // Backspace clears the staged orders; the round then plays.
    press(&mut app, KeyCode::Backspace);
    assert!(sess(&app).staged.is_empty());
    press(&mut app, KeyCode::Enter);
    assert_eq!(sess(&app).campaign.round(), 1);
    assert!(sess(&app).message.is_none(), "a played round clears the message");
}

#[test]
fn the_battle_report_names_the_battles_and_the_cities_taken() {
    // The attacker always wins: the AI wars, and the report says so.
    let mut app = app(session_on(3, Box::new(storm)));
    press(&mut app, KeyCode::Enter);
    let mut seen = false;
    for _ in 0..12 {
        if screen(&app) != Screen::Map {
            break;
        }
        press(&mut app, KeyCode::Enter);
        let s = sess(&app);
        if let Some(rep) = &s.report {
            if let Some(b) = rep.battles.iter().find(|b| b.captured) {
                let city = &s.campaign.campaign().cities[b.city].name;
                let t = map_text(s);
                assert!(t.contains(&format!("{city} taken by")), "{t}");
                seen = true;
                break;
            }
        }
    }
    assert!(seen, "a city changed hands in twelve rounds (stopped on {:?})", screen(&app));
}
