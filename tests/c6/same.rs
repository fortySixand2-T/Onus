//! C6 AC7 — **same campaign, either way** (a module of the `c6_live`
//! binary). A campaign driven through the client (keys to decisions, the
//! player's battles fielded live and fought in the sim) hashes round for
//! round exactly as the same decisions and battle outcomes fed headlessly,
//! and its save resumes to the same hashes.

use bevy::prelude::*;

use onus::campaign::story::Story;
use onus::campaign::{Campaign, CampaignContent, CampaignLog, Decision, PlayedBattle, RoundStep};
use onus::campaign_client::{CampaignSession, MapLayout, Screen};
use onus::headless;
use onus::sim::content::Content;

use super::client::{app, press, screen, sess, hold};

/// A short battle cap, so a test campaign's battles are fought in the sim
/// without running for minutes.
const CAP: u32 = 600;

fn content() -> Content {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data");
    Content::load_from_dir(&dir).unwrap()
}

fn campaign_content(content: &Content) -> CampaignContent {
    let mut cc = CampaignContent::shipped_with_diplomacy(content).unwrap();
    cc.rules.battle.tick_cap = CAP;
    cc
}

/// The AI's battles: the defender holds every one, so the player lives long enough to fight several.
fn session(seed: u64) -> CampaignSession {
    let content = content();
    let cc = campaign_content(&content);
    let c = Campaign::new(content, cc, seed, Some("house")).unwrap().with_live_battles();
    let story = Story::shipped(c.content(), c.campaign()).unwrap();
    let layout = MapLayout::shipped(c.campaign()).unwrap();
    CampaignSession::new(c, story, layout, Box::new(hold))
}

/// Fight the battle on the field: retreating at once (`retreat`), or left
/// to run until the battle layer or the cap ends it.
fn fight(app: &mut App, retreat: bool) {
    if retreat {
        press(app, KeyCode::KeyT);
    }
    let mut n = 0;
    let battles = sess(app).campaign.played().len();
    let pending = sess(app).played.len();
    while screen(app) == Screen::Battle && sess(app).played.len() == pending && sess(app).campaign.played().len() == battles {
        assert!(n <= CAP + 10, "the battle is still on after {n} ticks");
        headless::step(app);
        n += 1;
    }
}

/// Stage this round's orders by keys: every fifth round, a march of the
/// player's first army to its first neighbour; on rounds divisible by
/// three, a General raised at his first city.
fn stage(app: &mut App, round: u32) {
    if round.is_multiple_of(5) {
        press(app, KeyCode::Tab);
        press(app, KeyCode::ArrowRight);
        press(app, KeyCode::KeyM);
    }
    if round.is_multiple_of(3) {
        press(app, KeyCode::ArrowDown);
        press(app, KeyCode::KeyR);
    }
}

#[test]
fn a_client_driven_campaign_hashes_as_the_same_decisions_and_outcomes_fed_headlessly() {
    let mut app = app(session(3));
    let mut twin = sess(&app).campaign.clone();
    let mut rounds = 0;
    // The first battle is fought, then every other one retreated.
    let mut retreats = true;
    for round in 1..=16u32 {
        while screen(&app) == Screen::Story {
            press(&mut app, KeyCode::Enter);
        }
        if screen(&app) == Screen::Over {
            break;
        }
        assert_eq!(screen(&app), Screen::Map);
        stage(&mut app, round);
        let mut ds: Vec<Decision> = sess(&app).decisions();
        if twin.clone().step_round(&ds, &[], &mut hold).is_err() {
            // A refused round (a General the treasury cannot pay): clear
            // the orders, as a player would.
            press(&mut app, KeyCode::Backspace);
            ds = sess(&app).decisions();
        }
        let before = sess(&app).campaign.round_hashes().len();
        press(&mut app, KeyCode::Enter);
        while screen(&app) == Screen::Battle {
            retreats = !retreats;
            fight(&mut app, retreats);
        }
        let c = &sess(&app).campaign;
        assert_eq!(c.round_hashes().len(), before + 1, "round {round} was played");
        let played: Vec<PlayedBattle> = c.played().last().cloned().unwrap_or_default();
        match twin.step_round(&ds, &played, &mut hold).unwrap() {
            RoundStep::Played(_) => {}
            RoundStep::Battle(p) => panic!("round {round}: the twin is missing the battle at {}", p.city),
        }
        assert_eq!(twin.state_hash(), c.state_hash(), "round {round}: the client and the headless feed agree");
        assert_eq!(twin.round_hashes(), c.round_hashes());
        rounds += 1;
    }
    // Battles of every ending were fought in the sim: decided by the battle
    // layer (a retreat among them) and run to the cap.
    let fought: Vec<(u32, bool)> = sess(&app)
        .campaign
        .played()
        .iter()
        .flatten()
        .map(|b| (b.outcome.ticks, b.outcome.winner.is_some()))
        .collect();
    assert!(rounds >= 6, "{rounds} rounds played");
    assert!(fought.len() >= 3, "the player fought {} battles live", fought.len());
    assert!(fought.iter().all(|&(t, _)| t > 0), "every battle was fought in the sim: {fought:?}");
    assert!(fought.iter().any(|&(t, d)| d && t < CAP), "a decided battle: {fought:?}");
    assert!(fought.iter().any(|&(t, d)| !d && t == CAP), "a battle at the cap: {fought:?}");
    // The save resumes to the same hashes.
    let c = sess(&app).campaign.clone();
    let text = c.log().to_ron().unwrap();
    let back = CampaignLog::from_ron(&text).unwrap();
    let content = content();
    let cc = campaign_content(&content);
    let r = Campaign::resume(content, cc, &back, &mut hold).unwrap();
    assert_eq!(r.round_hashes(), c.round_hashes());
    assert_eq!(r.state_hash(), c.state_hash());
    assert_eq!(r.played(), c.played());
}
