//! Critic C5 — adversarial probes against the diplomacy milestone (critic
//! round 1, moved in unchanged in substance).

use std::path::PathBuf;

use onus::campaign::{BattleOutcome, BattlePlan, Campaign, CampaignContent, Decision, Side};
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
fn hold(p: &BattlePlan) -> BattleOutcome {
    BattleOutcome::unchanged(p, Some(Side::Defend))
}
fn court(id: &str) -> Decision {
    Decision::Court { nation: id.into() }
}

/// House courts and allies Aetherkin, then betrays it: Forgeborn (tolerance
/// 0) and the Reach (tolerance 0) now refuse every offer from House.
fn betrayer() -> Campaign {
    let mut c = Campaign::new(content(), diplomatic(), 3, Some("house")).unwrap();
    let h = c.nation_index("house").unwrap();
    let script: Vec<Vec<Decision>> = vec![
        vec![],
        vec![],
        vec![court("aetherkin")],
        vec![],
        vec![],
        vec![Decision::Ally { nation: "aetherkin".into() }],
        vec![Decision::Betray { nation: "aetherkin".into() }],
    ];
    for ds in &script {
        c.play_round(ds, &mut hold).unwrap_or_else(|e| panic!("round {}: {e}", c.round() + 1));
    }
    assert_eq!(c.betrayals(h), 1, "setup: House betrayed once");
    while c.nations()[h].treasury < 100 {
        c.play_round(&[], &mut hold).unwrap();
    }
    c
}

/// AC7: "A changed diplomatic decision diverges at its round." Two refused
/// offers to different nations leave identical hashes, so a save whose
/// decision was changed from one to the other loads without complaint, and
/// the loaded campaign's record of diplomacy differs from the one played.
#[test]
fn critic_a_changed_refused_offer_diverges_at_its_round() {
    let base = betrayer();
    let mut a = base.clone();
    let mut b = base.clone();
    let ra = a.play_round(&[court("forgeborn")], &mut hold).unwrap();
    let rb = b.play_round(&[court("ashen_reach")], &mut hold).unwrap();
    let h = a.nation_index("house").unwrap();
    let mine = |r: &onus::campaign::RoundReport| r.diplomacy.iter().find(|x| x.actor == h).copied().unwrap();
    assert!(!mine(&ra).accepted && !mine(&rb).accepted, "both offers refused");
    assert_ne!(mine(&ra).act, mine(&rb).act, "two different decisions");
    // Tamper the save: replace round N's Court forgeborn with Court ashen_reach.
    let mut log = a.log();
    let k = log.rounds.len() - 1;
    assert_eq!(log.rounds[k], vec![court("forgeborn")]);
    log.rounds[k] = vec![court("ashen_reach")];
    let resumed = Campaign::resume(content(), diplomatic(), &log, &mut hold);
    match resumed {
        Err(e) => assert!(e.contains(&format!("round {} diverged", k + 1)), "{e}"),
        Ok(r) => panic!(
            "a changed diplomatic decision (round {}: Court forgeborn -> Court ashen_reach) loaded \
             without diverging; the loaded record says {:?} where the played one says {:?}",
            k + 1,
            r.diplomacy_log().last(),
            a.diplomacy_log().last()
        ),
    }
}

