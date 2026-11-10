//! C5 AC8 — diplomacy balance: is diplomacy a choice or a dominant strategy?
//! Per seed the sim plays fourteen AI-only campaigns: every nation diplomatic,
//! then each nation abstaining alone; every nation abstaining, then each
//! nation diplomatic alone. Each nation's placement (1 + how many nations
//! outlasted it) is compared with and without its own diplomacy, in both
//! contexts. Diplomacy is **dominant** for a nation when engaging is never
//! worse and sometimes better, and **a choice** when it is sometimes better
//! and sometimes worse. The reading passes when diplomacy is dominant for no
//! nation and is a choice for at least one (critic C5 r1, provisional F-097).
//! Alongside placement the summary gives a finer measure, the place with ties
//! broken by the round a nation fell (later is better); it is reported, not
//! gated.
//!
//! The measurement is `#[ignore]` (it plays every battle of 14 campaigns a
//! seed):
//!   ONUS_C5_SEEDS=20 ONUS_C5_THREADS=2 cargo test --release --test c5_balance \
//!     the_diplomacy_balance_reading -- --ignored --nocapture
//! The fast tests check the machinery on a scripted war.

use std::path::PathBuf;

use onus::campaign::balance::{
    diplomacy_batch, play_diplomacy_campaign, DiploCampaign, DiplomacyReport, Profile, Role,
};
use onus::campaign::diplomacy::DiplomacyContent;
use onus::campaign::{BattleOutcome, BattlePlan, Campaign, CampaignContent, HeadlessResolver, Side};
use onus::sim::content::Content;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data")
}
fn content() -> Content {
    Content::load_from_dir(&data_dir()).expect("content loads")
}
fn plain() -> CampaignContent {
    CampaignContent::shipped(&content()).unwrap()
}
fn diplomacy() -> DiplomacyContent {
    DiplomacyContent::shipped(&content()).unwrap()
}
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
fn each_seed_plays_fourteen_profiles_in_order_on_any_thread_count() {
    let (c, cc, d) = (content(), plain(), diplomacy());
    let seeds = [1, 2];
    let one = diplomacy_batch(&c, &cc, &d, &seeds, 1, by_numbers).unwrap();
    let three = diplomacy_batch(&c, &cc, &d, &seeds, 3, by_numbers).unwrap();
    assert_eq!(one, three, "the thread count does not change the reading");
    let n = c.nations.len();
    let profiles = Profile::all(n);
    assert_eq!(profiles.len(), 2 + 2 * n);
    assert_eq!(profiles[0], Profile::AllEngaged);
    assert_eq!(profiles[1 + n], Profile::AllAbstaining);
    assert_eq!(one.campaigns.len(), seeds.len() * profiles.len());
    for (i, camp) in one.campaigns.iter().enumerate() {
        assert_eq!((camp.seed, camp.profile), (seeds[i / profiles.len()], profiles[i % profiles.len()]));
        let mut r = by_numbers();
        assert_eq!(*camp, play_diplomacy_campaign(&c, &cc, &d, camp.seed, camp.profile, &mut r).unwrap());
    }
}

#[test]
fn a_profile_is_the_diplomacy_lever_it_names() {
    let d = diplomacy();
    let n = 6;
    // Who takes diplomatic action.
    let engaged = |p: Profile| (0..n).map(|k| !p.apply(d.clone()).takes_no_initiative(k)).collect::<Vec<_>>();
    assert_eq!(engaged(Profile::AllEngaged), vec![true; n]);
    assert_eq!(engaged(Profile::AllAbstaining), vec![false; n]);
    assert_eq!(engaged(Profile::AllBut(2)), vec![true, true, false, true, true, true]);
    assert_eq!(engaged(Profile::Only(2)), vec![false, false, true, false, false, false]);
    // Who answers offers: under `Only` the others are passive, not deaf;
    // the nation left out of `AllBut` abstains outright.
    let answers = |p: Profile| (0..n).map(|k| !p.apply(d.clone()).abstains(k)).collect::<Vec<_>>();
    assert_eq!(answers(Profile::Only(2)), vec![true; n]);
    assert_eq!(answers(Profile::AllBut(2)), vec![true, true, false, true, true, true]);
    assert_eq!(answers(Profile::AllAbstaining), vec![false; n]);
    assert_eq!(Profile::AllBut(2).role(2), Some(Role::Without));
    assert_eq!(Profile::Only(2).role(2), Some(Role::With));
}

#[test]
fn every_nation_abstaining_plays_as_c4() {
    let (c, cc, d) = (content(), plain(), diplomacy());
    let camp = play_diplomacy_campaign(&c, &cc, &d, 3, Profile::AllAbstaining, &mut by_numbers()).unwrap();
    let mut c4 = Campaign::new(c.clone(), cc.clone(), 3, None).unwrap();
    let mut fallen = vec![None; c.nations.len()];
    let mut r = by_numbers();
    while !c4.is_over() {
        c4.play_round(&[], &mut r).unwrap();
        for (k, f) in fallen.iter_mut().enumerate() {
            if f.is_none() && !c4.nations()[k].alive {
                *f = Some(c4.round());
            }
        }
    }
    assert_eq!(camp.fallen, fallen);
    assert_eq!(camp.rounds, c4.round());
}

#[test]
fn placement_counts_the_nations_that_outlasted_it() {
    let camp = DiploCampaign {
        seed: 1,
        profile: Profile::AllEngaged,
        fallen: vec![None, Some(3), Some(5), Some(5), None],
        rounds: 9,
        actions: 0,
    };
    assert_eq!((0..5).map(|k| camp.placement(k)).collect::<Vec<_>>(), vec![1, 5, 3, 3, 1]);
}

/// A two-nation report of one seed: the fall rounds of each profile, in
/// `Profile::all` order (all engaged; all but n0; all but n1; all
/// abstaining; only n0; only n1).
fn report(fallen: [[Option<u32>; 2]; 6]) -> DiplomacyReport {
    DiplomacyReport {
        nations: vec!["n0".into(), "n1".into()],
        campaigns: Profile::all(2)
            .into_iter()
            .zip(fallen)
            .map(|(profile, f)| DiploCampaign { seed: 1, profile, fallen: f.to_vec(), rounds: 9, actions: 1 })
            .collect(),
    }
}

#[test]
fn the_verdict_names_a_dominant_nation_and_a_reading_without_effect() {
    const A: [Option<u32>; 2] = [None, Some(5)]; // n0 outlasts n1
    const B: [Option<u32>; 2] = [Some(5), None]; // n1 outlasts n0
    // n0 does better engaging, in both contexts; n1 is unmoved.
    let dominant = report([A, B, A, B, A, B]);
    let e = dominant.verdict().unwrap_err();
    assert!(e.contains("n0") && e.contains("dominant") && !e.contains("n1"), "{e}");
    let s = dominant.summary();
    assert!(s.contains("n0") && s.contains("dominant"), "{s}");
    // Nothing changes: diplomacy does nothing.
    let e = report([A; 6]).verdict().unwrap_err();
    assert!(e.contains("changes no outcome"), "{e}");
    // n0 does worse engaging; n1 is unmoved: an outcome changes and
    // diplomacy is dominant for nobody, but it is never worth taking up:
    // no nation has a choice (critic C5 r1).
    let e = report([B, A, B, A, B, A]).verdict().unwrap_err();
    assert!(e.contains("no nation has a choice") && !e.contains("dominant"), "{e}");
    // n0 does better among diplomats and worse among abstainers: a choice;
    // n1 is unmoved. The reading passes.
    let choice = report([A, B, A, A, B, A]);
    assert_eq!(choice.comparison(0).class(), "a choice");
    assert_eq!(choice.verdict(), Ok(()));
}

#[test]
fn the_finer_measure_breaks_a_shared_place_by_the_round_a_nation_fell() {
    // Three nations: n2 always wins. n0 and n1 fall in the same round (a
    // shared place) or n0 falls later (the same place by count only if it
    // still shares it).
    let c = |fallen: Vec<Option<u32>>| DiploCampaign { seed: 1, profile: Profile::AllEngaged, fallen, rounds: 9, actions: 0 };
    let early = c(vec![Some(3), Some(6), None]);
    let late = c(vec![Some(5), Some(6), None]);
    // By placement alone, n0 places 3rd in both.
    assert_eq!((early.placement(0), late.placement(0)), (3, 3));
    // By the finer measure, falling later is better.
    assert!(late.finer(0) < early.finer(0));
    assert_eq!(early.finer(2), late.finer(2), "a survivor is a survivor");
    // Over a report: a nation that falls later with diplomacy and never
    // sooner reads "dominant" by the finer measure, "no effect" by place.
    const N: [Option<u32>; 2] = [Some(5), None];
    const L: [Option<u32>; 2] = [Some(7), None];
    let r = report([L, N, N, N, L, N]);
    assert_eq!(r.comparison(0).class(), "no effect");
    assert_eq!(r.finer_comparison(0).class(), "dominant");
}

#[test]
#[ignore = "the AC8 measurement: plays every battle of 14 campaigns a seed"]
fn the_diplomacy_balance_reading() {
    let (c, cc, d) = (content(), plain(), diplomacy());
    let n = env_or("ONUS_C5_SEEDS", 20) as u64;
    let threads = env_or("ONUS_C5_THREADS", 2);
    let seeds: Vec<u64> = (1..=n).collect();
    let rep = diplomacy_batch(&c, &cc, &d, &seeds, threads, || HeadlessResolver).unwrap();
    eprintln!("{}", rep.summary());
    if let Err(e) = rep.verdict() {
        panic!("AC8 not met: {e}");
    }
}
