//! C5 AC2 — the golden licence. Without diplomacy a campaign plays exactly as
//! C4: C4's campaign golden is unchanged, and a diplomatic decision is
//! refused. With diplomacy attached but every nation abstaining, a campaign
//! fights the same battles, with the same results, as C4.

use std::path::PathBuf;

use onus::campaign::diplomacy::DiplomacyContent;
use onus::campaign::{
    BattleOutcome, BattlePlan, Campaign, CampaignContent, Decision, HeadlessResolver, Resolver, RoundError, Side,
};
use onus::sim::content::Content;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data")
}
fn content() -> Content {
    Content::load_from_dir(&data_dir()).expect("content loads")
}
fn plain() -> CampaignContent {
    CampaignContent::shipped(&content()).expect("campaign loads")
}
/// The shipped diplomacy, every nation abstaining.
fn abstaining() -> CampaignContent {
    let c = content();
    let mut d = DiplomacyContent::shipped(&c).unwrap();
    for n in 0..c.nations.len() {
        d = d.with_abstaining(n);
    }
    plain().with_diplomacy(d)
}
fn by_numbers() -> impl FnMut(&BattlePlan) -> BattleOutcome {
    |p: &BattlePlan| {
        let a = p.attack.units.len();
        let d = p.defend.units.len() + p.buildings.len();
        BattleOutcome::unchanged(p, Some(if a > d { Side::Attack } else { Side::Defend }))
    }
}

/// C4's own pin, `c4_autoresolve::campaign_golden`, read again here.
#[test]
fn c4s_campaign_golden_is_unchanged() {
    let mut c = Campaign::new(content(), plain(), 1, None).unwrap();
    for _ in 0..6 {
        c.play_round(&[], &mut HeadlessResolver).unwrap();
    }
    assert_eq!((c.round(), c.state_hash()), (6, 0x1600_ef99_7dbf_8728), "C4's golden moved");
}

#[test]
fn without_diplomacy_every_diplomatic_decision_is_refused() {
    let ds = [
        Decision::Court { nation: "aetherkin".into() },
        Decision::Ally { nation: "aetherkin".into() },
        Decision::Ask { target: "forgeborn".into() },
        Decision::Betray { nation: "aetherkin".into() },
        Decision::Incite { a: "forgeborn".into(), b: "verdant".into() },
    ];
    for d in ds {
        let mut c = Campaign::new(content(), plain(), 3, Some("house")).unwrap();
        let before = c.state_hash();
        match c.play_round(std::slice::from_ref(&d), &mut by_numbers()) {
            Err(RoundError::BadDecision { why, .. }) => assert!(why.contains("no diplomacy"), "{d:?}: {why}"),
            other => panic!("{d:?} must be refused without diplomacy: {other:?}"),
        }
        assert_eq!(c.state_hash(), before, "a refused round changes nothing");
        assert_eq!(c.round(), 0);
    }
}

fn same_play(seed: u64, rounds: u32, player: Option<&str>, mut make: impl FnMut() -> Box<dyn Resolver>) {
    let mut a = Campaign::new(content(), plain(), seed, player).unwrap();
    let mut b = Campaign::new(content(), abstaining(), seed, player).unwrap();
    let (mut ra, mut rb) = (make(), make());
    for r in 0..rounds {
        if a.is_over() {
            break;
        }
        let x = a.play_round(&[], ra.as_mut()).unwrap();
        let y = b.play_round(&[], rb.as_mut()).unwrap();
        assert_eq!(x.order, y.order, "seed {seed} round {}: turn order", r + 1);
        assert_eq!(x.battles, y.battles, "seed {seed} round {}: battles", r + 1);
        assert_eq!(a.cities(), b.cities(), "seed {seed} round {}: cities", r + 1);
        assert_eq!(a.armies(), b.armies(), "seed {seed} round {}: armies", r + 1);
        assert_eq!(a.nations(), b.nations(), "seed {seed} round {}: nations", r + 1);
        assert_eq!(a.outcome(), b.outcome(), "seed {seed} round {}: outcome", r + 1);
    }
    assert_eq!(a.history(), b.history());
    assert_ne!(a.state_hash(), b.state_hash(), "the content differs, so the hash does");
}

#[test]
fn with_every_nation_abstaining_a_campaign_plays_as_c4_on_a_scripted_war() {
    for seed in 1..=4 {
        same_play(seed, 40, None, || Box::new(by_numbers()));
        same_play(seed, 40, Some("house"), || Box::new(by_numbers()));
    }
}

#[test]
fn with_every_nation_abstaining_a_campaign_plays_as_c4_on_the_battle_sim() {
    same_play(1, 6, None, || Box::new(HeadlessResolver));
}
