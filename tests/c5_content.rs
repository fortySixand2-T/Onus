//! C5 AC1 — `diplomacy.ron` holds every diplomacy number, validated at load
//! against the battle content. Each pair of nations starts at the standing
//! their dispositions give, and the file's fingerprint joins the campaign
//! content's. Diplomacy is opt-in: a campaign takes it on only when asked.

use std::path::PathBuf;

use onus::campaign::diplomacy::{DiplomacyContent, Standing};
use onus::campaign::CampaignContent;
use onus::sim::content::Content;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data")
}
fn content() -> Content {
    Content::load_from_dir(&data_dir()).expect("content loads")
}
fn text() -> String {
    std::fs::read_to_string(data_dir().join("diplomacy.ron")).expect("diplomacy.ron")
}
fn shipped() -> DiplomacyContent {
    DiplomacyContent::shipped(&content()).expect("the shipped diplomacy loads")
}
fn invalid(t: &str) -> String {
    match DiplomacyContent::from_ron(t, &content()) {
        Ok(_) => panic!("must be refused:\n{t}"),
        Err(e) => e.to_string(),
    }
}
fn swap(t: &str, from: &str, to: &str) -> String {
    assert!(t.contains(from), "the shipped file has `{from}`");
    t.replacen(from, to, 1)
}
fn n(c: &Content, id: &str) -> usize {
    c.nations.iter().position(|d| d.id == id).unwrap()
}

#[test]
fn the_shipped_file_gives_every_nation_its_disposition_and_the_c0_rules() {
    let c = content();
    let d = shipped();
    let r = &d.rules;
    assert_eq!(r.ally_cap, 1, "C0: at most one ally");
    assert_eq!(r.betrayal_standing_drop, 1, "C0: one step");
    assert_eq!(r.destabilize_requires_tied, 2, "C0: Tied with two nations");
    assert_eq!(r.actions_per_round, 1, "C0: one diplomatic action per round");
    assert_eq!(d.dispositions.len(), c.nations.len(), "one per nation, in nation order");
    for (k, (disp, nat)) in d.dispositions.iter().zip(&c.nations).enumerate() {
        assert_eq!(disp.nation, nat.id, "disposition {k} is in nation order");
    }
    // The brief: the Forgeborn hate you; Verdant and Tollmarch can be bought.
    let (house, forge, verdant, toll) = (n(&c, "house"), n(&c, "forgeborn"), n(&c, "verdant"), n(&c, "tollmarch"));
    assert_eq!(d.start_standing(forge, house), Standing::Hostile);
    assert_eq!(d.start_standing(house, forge), Standing::Hostile, "standing is per pair");
    assert_eq!(d.start_standing(verdant, toll), d.default_standing);
    assert!(d.ally_bias(forge) < d.ally_bias(house), "Forgeborn never ally readily");
    assert!(d.ally_bias(verdant) > d.ally_bias(house), "Verdant can be bought");
    assert!(d.ally_bias(toll) > d.ally_bias(house), "Tollmarch sells alliances");
    assert!(Standing::Hostile < Standing::Neutral && Standing::Neutral < Standing::Tied && Standing::Tied < Standing::Allied);
}

#[test]
fn every_number_is_read_from_the_ron() {
    let c = content();
    let t = text();
    let d = DiplomacyContent::from_ron(&swap(&t, "court_cost: 100", "court_cost: 77"), &c).unwrap();
    assert_eq!(d.rules.court_cost, 77);
    let d = DiplomacyContent::from_ron(&swap(&t, "war_rounds: 4", "war_rounds: 9"), &c).unwrap();
    assert_eq!(d.rules.war_rounds, 9);
    let d = DiplomacyContent::from_ron(&swap(&t, "notice_per_city: 1", "notice_per_city: 3"), &c).unwrap();
    assert_eq!(d.rules.notice_per_city, 3);
    let d = DiplomacyContent::from_ron(&swap(&t, "default_standing: Neutral", "default_standing: Hostile"), &c).unwrap();
    assert_eq!(d.start_standing(n(&c, "verdant"), n(&c, "tollmarch")), Standing::Hostile);
}

#[test]
fn of_two_listings_the_more_hostile_one_stands() {
    let c = content();
    let t = swap(
        &text(),
        "(nation: \"house\", towards: [],",
        "(nation: \"house\", towards: [(\"forgeborn\", Tied), (\"verdant\", Tied)],",
    );
    let d = DiplomacyContent::from_ron(&t, &c).unwrap();
    let (house, forge, verdant) = (n(&c, "house"), n(&c, "forgeborn"), n(&c, "verdant"));
    assert_eq!(d.start_standing(house, forge), Standing::Hostile, "Forgeborn's Hostile beats House's Tied");
    assert_eq!(d.start_standing(verdant, house), Standing::Tied, "one listing: it stands");
}

#[test]
fn a_bad_file_is_refused_with_a_reason() {
    let t = text();
    let cases = [
        (swap(&t, "(nation: \"ashen_reach\",", "(nation: \"nobody\","), "unknown nation"),
        (swap(&t, "(nation: \"ashen_reach\",", "(nation: \"tollmarch\","), "twice"),
        (swap(&t, "towards: [(\"house\", Hostile)]", "towards: [(\"atlantis\", Hostile)]"), "unknown nation"),
        (swap(&t, "towards: [(\"house\", Hostile)]", "towards: [(\"forgeborn\", Hostile)]"), "itself"),
        (
            swap(&t, "towards: [(\"house\", Hostile)]", "towards: [(\"house\", Hostile), (\"house\", Neutral)]"),
            "twice",
        ),
        (swap(&t, "towards: [(\"house\", Hostile)]", "towards: [(\"house\", Allied)]"), "Allied"),
        (swap(&t, "default_standing: Neutral", "default_standing: Allied"), "Allied"),
        (swap(&t, "ally_cap: 1", "ally_cap: 2"), "ally_cap"),
        (swap(&t, "betrayal_standing_drop: 1", "betrayal_standing_drop: 0"), "betrayal_standing_drop"),
        (swap(&t, "destabilize_requires_tied: 2", "destabilize_requires_tied: 1"), "destabilize_requires_tied"),
        (swap(&t, "destabilize_requires_tied: 2", "destabilize_requires_tied: 6"), "destabilize_requires_tied"),
        (swap(&t, "actions_per_round: 1", "actions_per_round: 0"), "actions_per_round"),
        (swap(&t, "war_rounds: 4", "war_rounds: 0"), "war_rounds"),
        (swap(&t, "court_cost: 100", "court_cost: 100, bribe: 3"), "bribe"),
    ];
    for (bad, why) in cases {
        let e = invalid(&bad);
        assert!(e.contains(why), "expected `{why}` in: {e}");
    }
    // A nation without a disposition.
    let start = t.find("(nation: \"ashen_reach\"").unwrap();
    let end = start + t[start..].find("),").unwrap() + 2;
    let e = invalid(&format!("{}{}", &t[..start], &t[end..]));
    assert!(e.contains("ashen_reach") && e.contains("no disposition"), "{e}");
}

#[test]
fn diplomacy_is_opt_in_and_its_fingerprint_joins_the_campaigns() {
    let c = content();
    let plain = CampaignContent::shipped(&c).unwrap();
    assert!(plain.diplomacy().is_none(), "the shipped campaign has no diplomacy unless asked");
    let fp = plain.fingerprint();
    let with = plain.clone().with_diplomacy(shipped());
    assert!(with.diplomacy().is_some());
    assert_ne!(with.fingerprint(), fp, "diplomacy is in the campaign's fingerprint");
    assert_eq!(
        CampaignContent::shipped_with_diplomacy(&c).unwrap().fingerprint(),
        with.fingerprint(),
        "one way to attach it"
    );
    let other = DiplomacyContent::from_ron(&swap(&text(), "incite_cost: 200", "incite_cost: 201"), &c).unwrap();
    assert_ne!(plain.clone().with_diplomacy(other).fingerprint(), with.fingerprint(), "every number counts");
    let abstaining = shipped().with_abstaining(n(&c, "verdant"));
    assert!(abstaining.abstains(n(&c, "verdant")) && !abstaining.abstains(n(&c, "house")));
    assert_ne!(plain.clone().with_diplomacy(abstaining).fingerprint(), with.fingerprint(), "so does abstention");
    assert_eq!(CampaignContent::shipped(&c).unwrap().fingerprint(), fp, "the plain fingerprint is stable");
}
