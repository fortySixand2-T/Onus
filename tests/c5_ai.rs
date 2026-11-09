//! C5 AC6 — the diplomatic AI, played out. Each AI nation takes at most one
//! diplomatic action a round, by fixed rules from its disposition and the
//! map; its willingness is C0's notice rule; it never makes an offer to the
//! player. (The fixed order of its rules is pinned by the unit tests in
//! `src/campaign/diplomacy.rs`; `ai::decide_diplomacy` reads the campaign and
//! cannot step its rng.)

use std::path::PathBuf;

use onus::campaign::diplomacy::{Act, DiplomacyRecord};
use onus::campaign::{BattleOutcome, BattlePlan, Campaign, CampaignContent, Side};
use onus::sim::content::Content;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data")
}
fn content() -> Content {
    Content::load_from_dir(&data_dir()).expect("content loads")
}
fn by_numbers() -> impl FnMut(&BattlePlan) -> BattleOutcome {
    |p: &BattlePlan| {
        let a = p.attack.units.len();
        let d = p.defend.units.len() + p.buildings.len();
        BattleOutcome::unchanged(p, Some(if a > d { Side::Attack } else { Side::Defend }))
    }
}
fn game(seed: u64, player: Option<&str>) -> Campaign {
    let c = content();
    let cc = CampaignContent::shipped_with_diplomacy(&c).unwrap();
    Campaign::new(c, cc, seed, player).unwrap()
}
/// Play to the end (or 60 rounds), checking every round as it goes.
fn play(seed: u64, player: Option<&str>, mut each: impl FnMut(&Campaign, &[DiplomacyRecord])) -> Campaign {
    let mut c = game(seed, player);
    let mut r = by_numbers();
    for _ in 0..60 {
        if c.is_over() {
            break;
        }
        let rep = c.play_round(&[], &mut r).expect("the AI's decisions are always legal");
        each(&c, &rep.diplomacy);
    }
    c
}

#[test]
fn the_ai_plays_diplomacy_one_action_a_round_and_the_same_way_every_time() {
    let mut total = 0;
    for seed in 1..=4 {
        let c = play(seed, None, |c, recs| {
            for (k, r) in recs.iter().enumerate() {
                assert_eq!(r.round, c.round());
                assert!(recs[k + 1..].iter().all(|o| o.actor != r.actor), "two actions by {} in a round", r.actor);
            }
        });
        total += c.diplomacy_log().len();
        assert_eq!(play(seed, None, |_, _| {}).diplomacy_log(), c.diplomacy_log(), "seed {seed}: deterministic");
    }
    assert!(total > 0, "the AI plays some diplomacy");
}

#[test]
fn the_ai_never_makes_an_offer_to_the_player() {
    for seed in 1..=4 {
        let c = play(seed, Some("house"), |_, _| {});
        let h = c.nation_index("house").unwrap();
        for r in c.diplomacy_log() {
            assert_ne!(r.actor, h, "the player made no decision");
            assert!(
                !matches!(r.act, Act::Court { with } | Act::Ally { with } if with == h),
                "seed {seed}: an offer to the player: {r:?}"
            );
        }
    }
}

#[test]
fn willingness_is_bias_plus_standing_less_notice() {
    let c = content();
    let d = onus::campaign::diplomacy::DiplomacyContent::shipped(&c).unwrap();
    play(2, None, |camp, _| {
        for n in camp.alive() {
            for m in camp.alive() {
                if n == m {
                    continue;
                }
                let above = camp.cities_of(m).len().saturating_sub(camp.cities_of(n).len()) as i32;
                let s = camp.standing(n, m).unwrap().rank();
                let want = d.ally_bias(n) + s - d.rules.notice_per_city * above;
                assert_eq!(camp.willingness(n, m), Some(want));
            }
        }
    });
}
