//! C5 AC5 — destabilize. A nation Tied with `destabilize_requires_tied`
//! nations may incite two of them (Tied with it, not allied with each other)
//! to war, for `incite_cost`. The two go Hostile, and each AI of the two
//! marches on the other's capital for `war_rounds` rounds. The inciter's own
//! armies are never committed. Every AI is passive.

use std::path::PathBuf;

use onus::campaign::diplomacy::{Act, DiplomacyContent, Standing};
use onus::campaign::{
    BattleOutcome, BattlePlan, Campaign, CampaignContent, Decision, Location, RoundError, Side,
};
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
/// The shipped diplomacy, House starting Tied with Aetherkin and Verdant,
/// with `edit` applied; every AI passive.
fn diplomacy(edit: impl Fn(String) -> String) -> DiplomacyContent {
    let t = std::fs::read_to_string(data_dir().join("diplomacy.ron")).unwrap();
    let t = swap(
        &t,
        "(nation: \"house\", towards: [],",
        "(nation: \"house\", towards: [(\"aetherkin\", Tied), (\"verdant\", Tied)],",
    );
    let t = edit(t);
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
fn incite(a: &str, b: &str) -> Decision {
    Decision::Incite { a: a.into(), b: b.into() }
}
fn idx(c: &Campaign, id: &str) -> usize {
    c.nation_index(id).unwrap()
}
fn refused(c: &mut Campaign, ds: &[Decision], why: &str) {
    let before = c.state_hash();
    match c.play_round(ds, &mut hold) {
        Err(RoundError::BadDecision { why: w, .. }) => assert!(w.contains(why), "expected `{why}` in: {w}"),
        other => panic!("{ds:?} must be refused ({why}): {other:?}"),
    }
    assert_eq!(c.state_hash(), before);
}
fn heading(c: &Campaign, n: usize) -> String {
    let a = c.armies().iter().find(|a| a.nation == n).unwrap();
    match a.location {
        Location::Marching { to, .. } => c.campaign().cities[to].id.clone(),
        Location::At(at) => format!("at {}", c.campaign().cities[at].id),
    }
}

#[test]
fn incited_nations_go_hostile_and_march_on_each_others_capitals() {
    let d = diplomacy(|t| swap(&t, "incite_cost: 200", "incite_cost: 0"));
    // A seed on which House acts before both: their armies have not left yet.
    let seed = (1..=64)
        .find(|&s| {
            let mut c = game(d.clone(), s);
            let r = c.play_round(&[], &mut hold).unwrap();
            let pos = |id| r.order.iter().position(|&m| m == c.nation_index(id).unwrap()).unwrap();
            pos("house") < pos("aetherkin") && pos("house") < pos("verdant")
        })
        .expect("a seed where House acts first");
    let mut c = game(d.clone(), seed);
    let mut twin = game(d, seed);
    let (h, aeth, verdant) = (idx(&c, "house"), idx(&c, "aetherkin"), idx(&c, "verdant"));
    let mine = c.armies().iter().find(|a| a.nation == h).unwrap().clone();
    let r = c.play_round(&[incite("aetherkin", "verdant")], &mut hold).unwrap();
    twin.play_round(&[], &mut hold).unwrap();
    assert_eq!((r.diplomacy[0].act, r.diplomacy[0].accepted), (Act::Incite { a: aeth, b: verdant }, true));
    assert_eq!(c.standing(aeth, verdant), Some(Standing::Hostile));
    assert_eq!(c.standing(h, aeth), Some(Standing::Tied), "the inciter's standing is untouched");
    let (wa, wv) = (c.war_target(aeth).unwrap(), c.war_target(verdant).unwrap());
    assert_eq!((wa.target, wa.until, wv.target, wv.until), (verdant, 4, aeth, 4));
    assert_eq!(heading(&c, aeth), "prism_vale", "Aetherkin towards Greenmere");
    assert_eq!(heading(&twin, aeth), "starfall", "unincited, towards Cinderhold");
    assert_eq!(heading(&c, verdant), "thornwick", "Verdant towards Leyhold");
    assert_eq!(heading(&twin, verdant), "rootfall", "unincited, towards Cinderhold");
    // The inciter commits nothing.
    let after = c.armies().iter().find(|a| a.id == mine.id).unwrap();
    assert_eq!(after.location, mine.location);
    assert_eq!(c.armies().iter().filter(|a| a.nation == h).count(), 1);
}

#[test]
fn inciting_costs_incite_cost() {
    let mut c = game(diplomacy(|t| t), 3);
    let h = idx(&c, "house");
    refused(&mut c, &[incite("aetherkin", "verdant")], "costs 200");
    while c.nations()[h].treasury < 200 {
        c.play_round(&[], &mut hold).unwrap();
    }
    let mut twin = c.clone();
    c.play_round(&[incite("aetherkin", "verdant")], &mut hold).unwrap();
    twin.play_round(&[], &mut hold).unwrap();
    assert_eq!(c.nations()[h].treasury + 200, twin.nations()[h].treasury);
}

#[test]
fn inciting_needs_two_tied_nations_and_two_different_ones() {
    let free = |t: String| swap(&t, "incite_cost: 200", "incite_cost: 0");
    let mut c = game(diplomacy(free), 3);
    refused(&mut c, &[incite("aetherkin", "forgeborn")], "Tied");
    refused(&mut c, &[incite("aetherkin", "aetherkin")], "two different");
    refused(&mut c, &[incite("aetherkin", "house")], "this nation");
    // With only one Tied nation there is nothing to play off.
    let one = |t: String| {
        let t = swap(&t, "incite_cost: 200", "incite_cost: 0");
        swap(&t, "(\"verdant\", Tied)", "(\"verdant\", Neutral)")
    };
    let mut c = game(diplomacy(one), 3);
    refused(&mut c, &[incite("aetherkin", "verdant")], "Tied with 2");
    // The rule's number is the RON's: three Tied nations needed.
    let three = |t: String| {
        let t = swap(&t, "incite_cost: 200", "incite_cost: 0");
        swap(&t, "destabilize_requires_tied: 2", "destabilize_requires_tied: 3")
    };
    let mut c = game(diplomacy(three), 3);
    refused(&mut c, &[incite("aetherkin", "verdant")], "Tied with 3");
}
