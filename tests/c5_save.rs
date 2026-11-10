//! C5 AC7 — diplomacy is saved and hashed. Standings, betrayal counts and
//! steered wars are in the campaign state hash (the unit tests in
//! `src/campaign/diplomacy.rs` move each alone). The player's diplomatic
//! decisions are in the save; a diplomatic campaign saves, loads and replays
//! to the same hashes; a changed diplomatic decision diverges at its round;
//! and a save is refused by a content that differs in its diplomacy.

use std::path::PathBuf;

use onus::campaign::diplomacy::DiplomacyContent;
use onus::campaign::{BattleOutcome, BattlePlan, Campaign, CampaignContent, CampaignLog, Decision, Side};
use onus::sim::content::Content;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data")
}
fn content() -> Content {
    Content::load_from_dir(&data_dir()).expect("content loads")
}
fn diplomatic() -> CampaignContent {
    CampaignContent::shipped_with_diplomacy(&content()).unwrap()
}
/// Every defender holds: nobody falls, so the script plays to its end.
fn by_numbers(p: &BattlePlan) -> BattleOutcome {
    BattleOutcome::unchanged(p, Some(Side::Defend))
}
fn court(id: &str) -> Decision {
    Decision::Court { nation: id.into() }
}
/// House courts, allies, asks and betrays; the AI plays its own diplomacy.
fn script() -> Vec<Vec<Decision>> {
    vec![
        vec![],
        vec![],
        vec![court("aetherkin")],
        vec![],
        vec![],
        vec![Decision::Ally { nation: "aetherkin".into() }],
        vec![Decision::Ask { target: "forgeborn".into() }],
        vec![court("tollmarch")],
        vec![Decision::Betray { nation: "aetherkin".into() }],
        vec![],
    ]
}
fn played(rounds: &[Vec<Decision>]) -> Campaign {
    let mut c = Campaign::new(content(), diplomatic(), 3, Some("house")).unwrap();
    for ds in rounds {
        c.play_round(ds, &mut by_numbers).unwrap_or_else(|e| panic!("round {}: {e}", c.round() + 1));
    }
    c
}

#[test]
fn a_diplomatic_campaign_saves_loads_and_replays_to_the_same_hashes() {
    let c = played(&script());
    let h = c.nation_index("house").unwrap();
    assert!(c.diplomacy_log().iter().filter(|r| r.actor == h).all(|r| r.accepted), "the script plays");
    assert_eq!(c.betrayals(h), 1);
    let log = c.log();
    assert_eq!(log.rounds, script(), "the player's diplomatic decisions are the save");
    let text = log.to_ron().unwrap();
    let back = CampaignLog::from_ron(&text).unwrap();
    assert_eq!(back, log, "through RON and back");
    let r = Campaign::resume(content(), diplomatic(), &back, &mut by_numbers).unwrap();
    assert_eq!(r.round_hashes(), c.round_hashes());
    assert_eq!(r.diplomacy_log(), c.diplomacy_log());
    assert_eq!(r.state_hash(), c.state_hash());
    // And it plays on identically.
    let (mut a, mut b) = (c, r);
    for _ in 0..6 {
        if a.is_over() {
            break;
        }
        a.play_round(&[], &mut by_numbers).unwrap();
        b.play_round(&[], &mut by_numbers).unwrap();
        assert_eq!(a.state_hash(), b.state_hash());
    }
}

#[test]
fn a_changed_diplomatic_decision_diverges_at_its_round() {
    let c = played(&script());
    // Court Tollmarch in round 8 became a courting of the Reach: both are
    // accepted at the same price, so only the standings differ.
    let mut log = c.log();
    let other = played(&{
        let mut s = script();
        s[7] = vec![court("ashen_reach")];
        s
    });
    assert_eq!(
        other.nations()[other.nation_index("house").unwrap()].treasury,
        c.nations()[c.nation_index("house").unwrap()].treasury,
        "the same price"
    );
    log.rounds[7] = vec![court("ashen_reach")];
    let e = Campaign::resume(content(), diplomatic(), &log, &mut by_numbers).unwrap_err();
    assert!(e.contains("round 8 diverged"), "{e}");
}

#[test]
fn a_save_is_refused_by_a_content_that_differs_in_its_diplomacy() {
    let plain = CampaignContent::shipped(&content()).unwrap();
    let with_save = played(&script()).log();
    let e = Campaign::resume(content(), plain.clone(), &with_save, &mut by_numbers).unwrap_err();
    assert!(e.contains("different campaign content"), "{e}");
    let mut c = Campaign::new(content(), plain, 3, Some("house")).unwrap();
    c.play_round(&[], &mut by_numbers).unwrap();
    let without_save = c.log();
    let e = Campaign::resume(content(), diplomatic(), &without_save, &mut by_numbers).unwrap_err();
    assert!(e.contains("different campaign content"), "{e}");
    // Another diplomacy number is another content.
    let t = std::fs::read_to_string(data_dir().join("diplomacy.ron")).unwrap().replacen("war_rounds: 4", "war_rounds: 5", 1);
    let other = CampaignContent::shipped(&content())
        .unwrap()
        .with_diplomacy(DiplomacyContent::from_ron(&t, &content()).unwrap());
    let e = Campaign::resume(content(), other, &with_save, &mut by_numbers).unwrap_err();
    assert!(e.contains("different campaign content"), "{e}");
}
