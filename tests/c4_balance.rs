//! C4 AC12 — the **campaign balance sim**: many AI-only campaigns, played to
//! their end on the headless battle sim. Each nation should win sometimes,
//! and a campaign's length should land on the C0 target
//! (`rules.length_target`, 15-25 rounds).
//!
//! The measurement is `#[ignore]` (it plays every battle of every campaign):
//!   ONUS_C4_CAMPAIGNS=60 ONUS_C4_THREADS=2 cargo test --release --test c4_balance \
//!     the_campaign_balance_reading -- --ignored --nocapture
//! The fast tests check the batch's machinery and one real campaign.

use std::path::PathBuf;

use onus::campaign::balance::{balance_batch, play_ai_campaign, BalanceReport, CampaignResult};
use onus::campaign::{BattleOutcome, BattlePlan, CampaignContent, HeadlessResolver, Side};
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
/// A cheap scripted war: the side with more on the field wins, nothing dies.
fn by_numbers() -> impl FnMut(&BattlePlan) -> BattleOutcome {
    |p: &BattlePlan| {
        let a = p.attack.units.len();
        let d = p.defend.units.len() + p.buildings.len();
        BattleOutcome::unchanged(p, Some(if a > d { Side::Attack } else { Side::Defend }))
    }
}
fn env_or(key: &str, default: usize) -> usize {
    std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

#[test]
fn a_batch_is_in_seed_order_and_the_same_on_any_thread_count() {
    let (c, cc) = (content(), shipped());
    let seeds: Vec<u64> = (1..=6).collect();
    let one = balance_batch(&c, &cc, &seeds, 1, by_numbers).unwrap();
    let three = balance_batch(&c, &cc, &seeds, 3, by_numbers).unwrap();
    assert_eq!(one, three, "the thread count does not change the reading");
    assert_eq!(one.results.iter().map(|r| r.seed).collect::<Vec<_>>(), seeds);
    for (r, &s) in one.results.iter().zip(&seeds) {
        let mut res = by_numbers();
        assert_eq!(*r, play_ai_campaign(&c, &cc, s, &mut res).unwrap(), "seed {s} alone");
        assert!(r.rounds >= 1 && r.rounds <= cc.rules.round_cap);
    }
    let wins: u32 = one.wins().iter().sum();
    assert_eq!(wins + one.undecided(), seeds.len() as u32);
    assert_eq!((one.min_rounds, one.max_rounds), (15, 25), "the C0 target, from the RON");
    assert_eq!(one.nations.len(), c.nations.len());
}

#[test]
fn the_verdict_names_every_miss() {
    let r = |seed, winner, rounds| CampaignResult { seed, winner, rounds };
    let rep = |results| BalanceReport {
        nations: vec!["a".into(), "b".into()],
        results,
        min_rounds: 15,
        max_rounds: 25,
        round_cap: 40,
    };
    assert_eq!(rep(vec![r(1, Some(0), 15), r(2, Some(1), 25), r(3, None, 40)]).verdict(), Ok(()));
    let e = rep(vec![r(1, Some(0), 10), r(2, Some(0), 12), r(3, None, 40)]).verdict().unwrap_err();
    assert!(e.contains("b never won") && !e.contains("a never won"), "{e}");
    assert!(e.contains("median length 12"), "{e}");
    // An undecided campaign counts at the cap: it pulls the median long.
    let e = rep(vec![r(1, Some(0), 20), r(2, Some(1), 40), r(3, None, 40)]).verdict().unwrap_err();
    assert!(e.contains("median length 40"), "{e}");
    assert!(rep(vec![]).verdict().is_err());
}

#[test]
fn a_real_ai_campaign_is_played_to_its_end_on_the_battle_sim() {
    let (c, cc) = (content(), shipped());
    let a = play_ai_campaign(&c, &cc, 7, &mut HeadlessResolver).unwrap();
    assert!(a.rounds <= cc.rules.round_cap);
    assert_eq!(a, play_ai_campaign(&c, &cc, 7, &mut HeadlessResolver).unwrap(), "deterministic");
    eprintln!("seed 7: {a:?}");
}

#[test]
#[ignore = "the AC12 measurement: plays every battle of many campaigns"]
fn the_campaign_balance_reading() {
    let (c, cc) = (content(), shipped());
    let n = env_or("ONUS_C4_CAMPAIGNS", 60) as u64;
    let threads = env_or("ONUS_C4_THREADS", 2);
    let seeds: Vec<u64> = (1..=n).collect();
    let rep = balance_batch(&c, &cc, &seeds, threads, || HeadlessResolver).unwrap();
    eprintln!("{}", rep.summary());
    for r in &rep.results {
        eprintln!("  seed {:>3}: {:?} in {} rounds", r.seed, r.winner.map(|w| &rep.nations[w]), r.rounds);
    }
    if let Err(e) = rep.verdict() {
        panic!("AC12 not met: {e}");
    }
}
