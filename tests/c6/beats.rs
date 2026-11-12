//! C6 AC8 — **story beats on screen** (a module of the `c6_live` binary).
//! The Rupture opens the campaign. After each round, the beats it triggered
//! show on the Story screen, in order, before the map; the legacy beat (or
//! the loss beat) shows before the Over screen.
//!
//! For the user on the Mac: the story's tone, and its pacing between rounds.

use bevy::prelude::*;

use onus::campaign::story::StoryProgress;
use onus::campaign::{BattleOutcome, CampaignOutcome, Side};
use onus::campaign_client::{over_text, story_text, CampaignSession, Screen};

use super::client::{app, press, screen, sess, session_on, storm};

/// The round's battles of the player's, decided for (`wins`) or against
/// him: the scripted stand-in for the battle screen.
fn decide(app: &mut App, wins: bool) {
    while screen(app) == Screen::Battle {
        let world = app.world_mut();
        let mut s = world.resource_mut::<CampaignSession>();
        let plan = s.battle.clone().expect("the round is paused at a battle");
        let mine = if s.campaign.player() == Some(plan.attack.nation) { Side::Attack } else { Side::Defend };
        let theirs = if mine == Side::Attack { Side::Defend } else { Side::Attack };
        let to = s.battle_played(BattleOutcome::unchanged(&plan, Some(if wins { mine } else { theirs })));
        world.resource_mut::<NextState<Screen>>().set(to);
        app.update();
    }
}

/// Read the beats on the Story screen, Enter past each; returns them.
fn read_beats(app: &mut App) -> Vec<usize> {
    let mut shown = Vec::new();
    while screen(app) == Screen::Story {
        let s = sess(app);
        let k = *s.beats.first().expect("the Story screen shows a waiting beat");
        let b = &s.story.beats()[k];
        let t = story_text(s);
        assert!(t.contains(&b.title) && t.contains(&b.text), "the beat shows: {t}");
        shown.push(k);
        press(app, KeyCode::Enter);
    }
    shown
}

/// Play rounds, the player marching his first army each round and winning
/// (`wins`) or losing every battle of his; each round's beats are checked
/// against the story rule run on the same campaign. Returns every beat shown
/// after the Rupture, and the app on the screen it ended on.
fn play(seed: u64, wins: bool, rounds: u32) -> (Vec<usize>, App) {
    let mut app = app(session_on(seed, Box::new(storm)));
    let mut twin = StoryProgress::new(&sess(&app).story);
    let opening = twin.advance(&sess(&app).story, &sess(&app).campaign);
    assert_eq!(read_beats(&mut app), opening, "the Rupture opens the campaign");
    let mut all = Vec::new();
    for _ in 0..rounds {
        if screen(&app) == Screen::Over {
            break;
        }
        assert_eq!(screen(&app), Screen::Map);
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::ArrowRight);
        press(&mut app, KeyCode::KeyM);
        let before = sess(&app).campaign.round();
        press(&mut app, KeyCode::Enter);
        decide(&mut app, wins);
        assert_eq!(sess(&app).campaign.round(), before + 1, "the round was played");
        let want = twin.advance(&sess(&app).story, &sess(&app).campaign);
        let shown = read_beats(&mut app);
        assert_eq!(shown, want, "round {}: the beats it triggered, in order", before + 1);
        let after = if sess(&app).campaign.is_over() { Screen::Over } else { Screen::Map };
        assert_eq!(screen(&app), after, "then the map (or the end)");
        all.extend(shown);
    }
    (all, app)
}

fn id(app: &App, k: usize) -> String {
    sess(app).story.beats()[k].id.clone()
}

#[test]
fn a_rounds_beats_show_in_order_before_the_map() {
    let (all, app) = play(3, true, 30);
    let ids: Vec<String> = all.iter().map(|&k| id(&app, k)).collect();
    assert!(!ids.is_empty(), "a winning player's rounds trigger beats");
    assert!(ids.iter().any(|i| i.starts_with("glory") || i.starts_with("vengeance")), "{ids:?}");
}

#[test]
fn the_loss_beat_shows_before_the_over_screen() {
    let (all, app) = play(3, false, 30);
    assert_eq!(screen(&app), Screen::Over);
    assert_eq!(sess(&app).campaign.outcome(), Some(CampaignOutcome::PlayerLost));
    let last = *all.last().expect("the loss beat");
    assert_eq!(id(&app, last), "fallen", "the loss beat is the last shown");
    assert!(over_text(sess(&app)).contains("lost"));
}
