//! C4 AC11 — a campaign **saves as its seed plus a log of the player's
//! decisions**, on M5's replay pattern: a versioned RON file stamped with its
//! content, loaded by replaying every round and checking each round's state
//! hash. Another content or format version is refused.

use std::path::PathBuf;

use onus::campaign::{
    BattleOutcome, BattlePlan, Campaign, CampaignContent, CampaignLog, Decision, HeadlessResolver, Side,
    CAMPAIGN_LOG_VERSION,
};
use onus::sim::content::Content;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data")
}
fn content() -> Content {
    Content::load_from_dir(&data_dir()).expect("content loads")
}
fn shipped() -> CampaignContent {
    CampaignContent::load_from_dir(&data_dir(), &content()).expect("campaign loads")
}
fn hold(p: &BattlePlan) -> BattleOutcome {
    BattleOutcome::unchanged(p, Some(Side::Defend))
}
/// Player House plays six rounds on the headless resolver: a march, holds,
/// a raise once it can pay.
fn played() -> Campaign {
    let mut c = Campaign::new(content(), shipped(), 21, Some("house")).unwrap();
    let h = c.nation_index("house").unwrap();
    let army = c.armies().iter().find(|a| a.nation == h).unwrap().id;
    let mut r = HeadlessResolver;
    c.play_round(&[Decision::March { army, to: "forge_gate".into() }], &mut r).unwrap();
    let mut raised = false;
    for _ in 0..5 {
        if c.is_over() {
            break;
        }
        let ds = if !raised && c.nations()[h].treasury >= c.campaign().rules.raise_general_cost {
            raised = true;
            vec![Decision::Raise { city: "cinderhold".into() }]
        } else {
            vec![]
        };
        c.play_round(&ds, &mut r).unwrap();
    }
    assert!(raised, "the fixture saves a raise too");
    c
}
fn tmp(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("onus_c4_save_{}_{name}.ron", std::process::id()))
}

#[test]
fn a_save_is_the_seed_and_the_players_decisions() {
    let c = played();
    let log = c.log();
    assert_eq!(log.version, CAMPAIGN_LOG_VERSION);
    assert_eq!(log.seed, c.seed());
    assert_eq!(log.player.as_deref(), Some("house"));
    assert_eq!(log.rounds.len() as u32, c.round(), "one entry per round played");
    assert_eq!(log.hashes, c.round_hashes());
    assert!(matches!(log.rounds[0].as_slice(), [Decision::March { .. }]));
    let n: usize = log.rounds.iter().map(|r| r.len()).sum();
    assert_eq!(n, 2, "the player's decisions only: the AI's are recomputed");
    // An AI-only campaign logs its rounds and no decisions.
    let mut ai = Campaign::new(content(), shipped(), 21, None).unwrap();
    let mut r = hold;
    for _ in 0..3 {
        ai.play_round(&[], &mut r).unwrap();
    }
    let l = ai.log();
    assert_eq!((l.player.clone(), l.rounds.len()), (None, 3));
    assert!(l.rounds.iter().all(|r| r.is_empty()));
}

#[test]
fn a_saved_campaign_loads_round_for_round_and_plays_on_identically() {
    let c = played();
    let path = tmp("roundtrip");
    c.log().save(&path).unwrap();
    let log = CampaignLog::load(&path).unwrap();
    std::fs::remove_file(&path).ok();
    assert_eq!(log, c.log(), "what is written is what is read");
    let mut r = HeadlessResolver;
    let mut back = Campaign::resume(content(), shipped(), &log, &mut r).unwrap();
    assert_eq!(back.state_hash(), c.state_hash());
    assert_eq!(back.round_hashes(), c.round_hashes());
    assert_eq!((back.cities(), back.armies(), back.nations()), (c.cities(), c.armies(), c.nations()));
    let mut c = c;
    if !c.is_over() {
        c.play_round(&[], &mut r).unwrap();
        back.play_round(&[], &mut r).unwrap();
        assert_eq!(back.state_hash(), c.state_hash(), "the loaded campaign plays on as the original");
    }
}

#[test]
fn a_save_from_another_content_or_version_is_refused() {
    let c = played();
    let log = c.log();
    let mut r = HeadlessResolver;
    // Another battle content.
    let mut other = content();
    other.buildings[0].alloy_cost += 1;
    let e = Campaign::resume(other, shipped(), &log, &mut r).unwrap_err();
    assert!(e.contains("different battle content"), "{e}");
    // Another campaign content.
    let t = std::fs::read_to_string(data_dir().join("campaign.ron")).unwrap();
    assert!(t.contains("repair_per_round: 1000,"));
    let t = t.replacen("repair_per_round: 1000,", "repair_per_round: 999,", 1);
    let cc = CampaignContent::from_ron(&t, &content()).unwrap();
    let e = Campaign::resume(content(), cc, &log, &mut r).unwrap_err();
    assert!(e.contains("different campaign content"), "{e}");
    // Another format version, on read and on write.
    let text = log.to_ron().unwrap();
    let key = format!("version: {CAMPAIGN_LOG_VERSION},");
    assert!(text.contains(&key));
    let bumped = text.replacen(&key, &format!("version: {},", CAMPAIGN_LOG_VERSION + 1), 1);
    let e = CampaignLog::from_ron(&bumped).unwrap_err();
    assert!(e.contains("format version"), "{e}");
    let mut w = log.clone();
    w.version += 1;
    assert!(w.to_ron().unwrap_err().contains("format version"));
    assert!(Campaign::resume(content(), shipped(), &w, &mut r).is_err());
    // A save whose rounds and hashes disagree.
    let mut short = log.clone();
    short.hashes.pop();
    assert!(short.to_ron().is_err());
    assert!(Campaign::resume(content(), shipped(), &short, &mut r).is_err());
}

#[test]
fn a_tampered_save_stops_at_the_first_divergent_round() {
    let c = played();
    let mut log = c.log();
    let mut r = HeadlessResolver;
    // Drop the round-1 march: round 1 plays differently.
    log.rounds[0].clear();
    let e = Campaign::resume(content(), shipped(), &log, &mut r).unwrap_err();
    assert!(e.contains("round 1 diverged"), "{e}");
    // A wrong hash at round 3 (the rounds before it still agree).
    let mut log = c.log();
    log.hashes[2] ^= 1;
    let e = Campaign::resume(content(), shipped(), &log, &mut r).unwrap_err();
    assert!(e.contains("round 3 diverged"), "{e}");
}
