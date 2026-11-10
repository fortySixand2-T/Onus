//! C6 AC5 — **the diplomacy screen** (a module of the `c6_live` binary). It
//! shows each rival's standing with the player, the player's ally and each
//! nation's betrayals; its keys stage the round's one diplomatic action
//! (court, ally, ask, betray or incite) as a `Decision`, played with the
//! round's other decisions when the turn ends.
//!
//! For the user on the Mac: that the screen reads, and the feel of the keys.

use bevy::prelude::*;

use onus::campaign::{Decision, PlayedBattle, RoundStep};
use onus::campaign_client::{diplomacy_text, Screen};

use super::client::{app, hold, play_battle, press, screen, sess, session};

fn id(app: &App, n: usize) -> String {
    sess(app).campaign.content().nations[n].id.clone()
}

/// Down until the cursor is on rival `n`.
fn select(app: &mut App, n: usize) {
    for _ in 0..12 {
        if sess(app).cursor.rival == Some(n) {
            return;
        }
        press(app, KeyCode::ArrowDown);
    }
    panic!("rival {n} could not be selected");
}

fn open(seed: u64) -> App {
    let mut app = app(session(seed));
    press(&mut app, KeyCode::Enter); // past the Rupture
    press(&mut app, KeyCode::KeyD);
    assert_eq!(screen(&app), Screen::Diplomacy, "D opens the diplomacy screen");
    app
}

#[test]
fn the_screen_shows_standings_the_ally_and_betrayals() {
    let mut app = open(3);
    let s = sess(&app);
    let c = &s.campaign;
    let p = c.player().unwrap();
    let t = diplomacy_text(s);
    for n in 0..c.nations().len() {
        let name = &c.content().nations[n].name;
        if n == p {
            assert!(t.contains(&format!("You ({name}): betrayals {}", c.betrayals(n))), "{t}");
            continue;
        }
        let st = c.standing(p, n).unwrap();
        let want = format!("{name}: {st}, betrayals {}", c.betrayals(n));
        assert!(t.contains(&want), "diplomacy text lacks `{want}`:\n{t}");
    }
    assert!(t.contains("Your ally: none"), "{t}");
    press(&mut app, KeyCode::Escape);
    assert_eq!(screen(&app), Screen::Map, "Esc goes back to the map");
}

#[test]
fn keys_stage_one_diplomatic_action() {
    let mut app = open(3);
    let p = sess(&app).campaign.player().unwrap();
    let rivals: Vec<usize> = (0..sess(&app).campaign.nations().len()).filter(|&n| n != p).collect();
    let (a, b) = (rivals[0], rivals[1]);
    select(&mut app, a);
    let staged = |app: &App| sess(app).diplomatic.clone();
    press(&mut app, KeyCode::KeyC);
    assert_eq!(staged(&app), Some(Decision::Court { nation: id(&app, a) }));
    // One action a round: another key replaces it.
    press(&mut app, KeyCode::KeyA);
    assert_eq!(staged(&app), Some(Decision::Ally { nation: id(&app, a) }));
    press(&mut app, KeyCode::KeyK);
    assert_eq!(staged(&app), Some(Decision::Ask { target: id(&app, a) }));
    press(&mut app, KeyCode::KeyB);
    assert_eq!(staged(&app), Some(Decision::Betray { nation: id(&app, a) }));
    // Incite takes two rivals: I on the first, I on the second.
    press(&mut app, KeyCode::KeyI);
    assert_eq!(sess(&app).cursor.incite, Some(a));
    select(&mut app, b);
    press(&mut app, KeyCode::KeyI);
    assert_eq!(staged(&app), Some(Decision::Incite { a: id(&app, a), b: id(&app, b) }));
    assert_eq!(sess(&app).cursor.incite, None);
    assert!(diplomacy_text(sess(&app)).contains("Action: Incite"), "the staged action shows");
    press(&mut app, KeyCode::Backspace);
    assert_eq!(staged(&app), None, "Backspace clears it");
    assert!(sess(&app).staged.is_empty(), "army orders are not the diplomacy screen's");
}

/// Court the most willing rival, then ally with it once Tied, round after
/// round, each round checked against the same decisions fed headlessly.
/// Returns the ally (`None`: none in 30 rounds, or the campaign ended).
fn court_to_ally(app: &mut App) -> Option<usize> {
    let p = sess(app).campaign.player().unwrap();
    for _ in 0..30 {
        let c = &sess(app).campaign;
        if c.is_over() {
            return None;
        }
        if let Some(a) = c.ally_of(p) {
            return Some(a);
        }
        let best = (0..c.nations().len())
            .filter(|&n| n != p && c.nations()[n].alive)
            .max_by_key(|&n| (c.willingness(n, p).unwrap(), std::cmp::Reverse(n)))
            .unwrap();
        let tied = c.standing(p, best) == Some(onus::campaign::diplomacy::Standing::Tied);
        select(app, best);
        press(app, if tied { KeyCode::KeyA } else { KeyCode::KeyC });
        let d = sess(app).diplomatic.clone();
        let ds = sess(app).decisions();
        assert_eq!(ds.first(), d.as_ref(), "the diplomatic action comes first");
        // An action the treasury cannot pay refuses the round: hold
        // instead, as a player would. The player's battles are held
        // (scripted: the battle screen is AC6's).
        let mut twin = sess(app).campaign.clone();
        if twin.clone().step_round(&ds, &[], &mut hold).is_err() {
            press(app, KeyCode::Backspace);
        }
        let ds = sess(app).decisions();
        let mut played = Vec::new();
        while let RoundStep::Battle(plan) = twin.step_round(&ds, &played, &mut hold).expect("holding is never refused") {
            played.push(PlayedBattle {
                round: plan.round,
                city: twin.campaign().cities[plan.city].id.clone(),
                outcome: hold(&plan),
            });
        }
        press(app, KeyCode::Escape);
        press(app, KeyCode::Enter);
        while screen(app) == Screen::Battle {
            play_battle(app, hold);
        }
        assert_eq!(sess(app).campaign.state_hash(), twin.state_hash(), "the client plays what was staged");
        assert_eq!(sess(app).diplomatic, None, "a played round clears the action");
        if screen(app) == Screen::Map {
            press(app, KeyCode::KeyD);
        }
    }
    None
}

#[test]
fn the_action_is_played_with_the_round_first_and_its_result_shows() {
    let mut app = open(3);
    let a = court_to_ally(&mut app).expect("an ally within 30 rounds of courting");
    let t = diplomacy_text(sess(&app));
    let name = &sess(&app).campaign.content().nations[a].name;
    assert!(t.contains(&format!("Your ally: {name}")), "{t}");
    assert!(t.contains(&format!("{name}: Allied")), "{t}");
    assert!(t.contains("You allied with"), "the round's record shows:\n{t}");
}
