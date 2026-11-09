//! C5 AC4 — betrayal. Betraying an ally makes the pair Hostile, and every
//! other living nation's standing towards the betrayer drops
//! `betrayal_standing_drop` steps (never below Hostile). Betrayals are
//! counted: a nation refuses every offer from one that has betrayed more
//! often than its `betrayal_tolerance`. The betrayer's armies standing in the
//! old ally's cities assault them that same round. Every AI is passive.

use std::path::PathBuf;

use onus::campaign::diplomacy::{Act, DiplomacyContent, Standing};
use onus::campaign::{BattleOutcome, BattlePlan, Campaign, CampaignContent, Decision, RoundError, Side};
use onus::sim::content::Content;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data")
}
fn content() -> Content {
    Content::load_from_dir(&data_dir()).expect("content loads")
}
fn swap(t: &str, from: &str, to: &str) -> String {
    assert!(t.contains(from), "the file has `{from}`");
    t.replacen(from, to, 1)
}
/// The shipped diplomacy at no cost (with `edit` applied), every AI passive.
fn free_with(edit: impl Fn(String) -> String) -> DiplomacyContent {
    let t = std::fs::read_to_string(data_dir().join("diplomacy.ron")).unwrap();
    let t = swap(&t, "court_cost: 100", "court_cost: 0");
    let t = swap(&t, "ally_cost: 150", "ally_cost: 0");
    let t = edit(swap(&t, "incite_cost: 200", "incite_cost: 0"));
    let c = content();
    let mut d = DiplomacyContent::from_ron(&t, &c).unwrap_or_else(|e| panic!("{e}"));
    for n in 0..c.nations.len() {
        d = d.with_passive(n);
    }
    d
}
fn game(d: DiplomacyContent, seed: u64) -> Campaign {
    let cc = CampaignContent::shipped(&content()).unwrap().with_diplomacy(d);
    Campaign::new(content(), cc, seed, Some("house")).unwrap()
}
fn hold(p: &BattlePlan) -> BattleOutcome {
    BattleOutcome::unchanged(p, Some(Side::Defend))
}
fn court(id: &str) -> Decision {
    Decision::Court { nation: id.into() }
}
fn ally(id: &str) -> Decision {
    Decision::Ally { nation: id.into() }
}
fn betray(id: &str) -> Decision {
    Decision::Betray { nation: id.into() }
}
fn idx(c: &Campaign, id: &str) -> usize {
    c.nation_index(id).unwrap()
}
fn round(c: &mut Campaign, ds: &[Decision]) -> onus::campaign::RoundReport {
    c.play_round(ds, &mut hold).unwrap()
}
fn refused(c: &mut Campaign, ds: &[Decision], why: &str) {
    let before = c.state_hash();
    match c.play_round(ds, &mut hold) {
        Err(RoundError::BadDecision { why: w, .. }) => assert!(w.contains(why), "expected `{why}` in: {w}"),
        other => panic!("{ds:?} must be refused ({why}): {other:?}"),
    }
    assert_eq!(c.state_hash(), before);
}

#[test]
fn a_betrayal_makes_the_pair_hostile_and_drops_everyones_standing_towards_the_betrayer() {
    let mut c = game(free_with(|t| t), 3);
    let (h, aeth, forge, verdant, toll) =
        (idx(&c, "house"), idx(&c, "aetherkin"), idx(&c, "forgeborn"), idx(&c, "verdant"), idx(&c, "tollmarch"));
    refused(&mut c, &[betray("aetherkin")], "not this nation's ally");
    round(&mut c, &[court("tollmarch")]);
    round(&mut c, &[court("aetherkin")]);
    round(&mut c, &[ally("aetherkin")]);
    assert!(c.allied(h, aeth));
    let before: Vec<Option<Standing>> = (0..6).map(|m| c.standing(m, h)).collect();
    assert_eq!(c.betrayals(h), 0);
    let r = round(&mut c, &[betray("aetherkin")]);
    assert_eq!((r.diplomacy[0].act, r.diplomacy[0].accepted), (Act::Betray { ally: aeth }, true));
    assert_eq!(c.standing(h, aeth), Some(Standing::Hostile), "the pair goes Hostile");
    assert_eq!(c.ally_of(h), None);
    assert_eq!(c.betrayals(h), 1, "counted");
    assert_eq!(before[toll], Some(Standing::Tied));
    assert_eq!(c.standing(toll, h), Some(Standing::Neutral), "one step down");
    assert_eq!(c.standing(verdant, h), Some(Standing::Hostile), "one step down");
    assert_eq!(c.standing(forge, h), Some(Standing::Hostile), "never below Hostile");
    // Only standings towards the betrayer move.
    assert_eq!(c.standing(verdant, toll), Some(Standing::Neutral));
}

#[test]
fn the_drop_is_read_from_the_ron() {
    let mut c = game(free_with(|t| swap(&t, "betrayal_standing_drop: 1", "betrayal_standing_drop: 2")), 3);
    let (h, toll) = (idx(&c, "house"), idx(&c, "tollmarch"));
    round(&mut c, &[court("tollmarch")]);
    round(&mut c, &[court("aetherkin")]);
    round(&mut c, &[ally("aetherkin")]);
    round(&mut c, &[betray("aetherkin")]);
    assert_eq!(c.standing(toll, h), Some(Standing::Hostile), "Tied, two steps down");
}

#[test]
fn a_nation_refuses_every_offer_from_one_that_betrayed_more_than_it_forgives() {
    let mut c = game(free_with(|t| t), 3);
    let (h, verdant, ash) = (idx(&c, "house"), idx(&c, "verdant"), idx(&c, "ashen_reach"));
    round(&mut c, &[court("aetherkin")]);
    round(&mut c, &[ally("aetherkin")]);
    round(&mut c, &[betray("aetherkin")]);
    // Both are Hostile to House now; both would still be willing (Verdant
    // 2 + 0, the Reach 1 + 0, against a court_threshold of 1).
    assert_eq!(c.standing(ash, h), Some(Standing::Hostile));
    assert!(c.willingness(ash, h).unwrap() >= 1 && c.willingness(verdant, h).unwrap() >= 1);
    // The Reach forgives no betrayal; Verdant forgives one.
    let r = round(&mut c, &[court("ashen_reach")]);
    assert!(!r.diplomacy[0].accepted, "the Reach (tolerance 0) refuses a betrayer");
    let r = round(&mut c, &[court("verdant")]);
    assert!(r.diplomacy[0].accepted, "Verdant (tolerance 1) forgives one betrayal");
    // A second betrayal is one more than Verdant forgives.
    round(&mut c, &[court("verdant")]);
    round(&mut c, &[ally("verdant")]);
    round(&mut c, &[betray("verdant")]);
    assert_eq!(c.betrayals(h), 2);
    let r = round(&mut c, &[court("tollmarch")]);
    assert!(r.diplomacy[0].accepted, "Tollmarch (tolerance 2) still deals");
    let r = round(&mut c, &[court("verdant")]);
    assert!(!r.diplomacy[0].accepted, "Verdant refuses a second betrayal");
}

#[test]
fn the_betrayers_armies_in_the_old_allys_cities_assault_them_that_round() {
    let mut c = game(free_with(|t| t), 3);
    let (h, aeth) = (idx(&c, "house"), idx(&c, "aetherkin"));
    let starfall = c.city_index("starfall").unwrap();
    round(&mut c, &[court("aetherkin")]);
    round(&mut c, &[ally("aetherkin")]);
    let army = c.armies().iter().find(|a| a.nation == h).unwrap().id;
    round(&mut c, &[Decision::March { army, to: "starfall".into() }]);
    while c.army(army).unwrap().at() != Some(starfall) {
        let r = round(&mut c, &[]);
        assert!(!r.battles.iter().any(|b| b.attacker == h), "a guest does not fight");
    }
    let r = round(&mut c, &[]);
    assert!(r.battles.iter().all(|b| b.city != starfall), "still a guest");
    let r = round(&mut c, &[betray("aetherkin")]);
    let b = r.battles.iter().find(|b| b.city == starfall).expect("the betrayer assaults Starfall at once");
    assert_eq!((b.attacker, b.defender), (h, aeth));
}
