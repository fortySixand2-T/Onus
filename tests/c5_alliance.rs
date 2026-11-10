//! C5 AC3 — alliance, with C0's cap of one ally. Courting raises a pair's
//! standing one step (no higher than Tied); an alliance needs Tied. The other
//! nation answers by its disposition, and an offer is paid only when it is
//! accepted. Allies never fight; an AI ally asked to marches on the shared
//! enemy's capital for `war_rounds` rounds; an alliance ends when either
//! nation falls; and the player wins when every rival still standing is his
//! ally. Here every AI is passive (it answers, but offers nothing), so the
//! player's actions are the only diplomacy.

use std::path::PathBuf;

use onus::campaign::diplomacy::{Act, DiplomacyContent, Standing};
use onus::campaign::{
    BattleOutcome, BattlePlan, Campaign, CampaignContent, CampaignOutcome, Decision, RoundError, Side,
};
use onus::sim::content::Content;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data")
}
fn content() -> Content {
    Content::load_from_dir(&data_dir()).expect("content loads")
}
fn diplomacy_text() -> String {
    std::fs::read_to_string(data_dir().join("diplomacy.ron")).expect("diplomacy.ron")
}
fn swap(t: &str, from: &str, to: &str) -> String {
    assert!(t.contains(from), "the file has `{from}`");
    t.replacen(from, to, 1)
}
/// `text` as diplomacy, every AI passive.
fn passive(text: &str) -> DiplomacyContent {
    let c = content();
    let mut d = DiplomacyContent::from_ron(text, &c).unwrap_or_else(|e| panic!("{e}"));
    for n in 0..c.nations.len() {
        d = d.with_passive(n);
    }
    d
}
/// The shipped diplomacy at no cost, every AI passive.
fn free() -> DiplomacyContent {
    let t = swap(&diplomacy_text(), "court_cost: 100", "court_cost: 0");
    let t = swap(&t, "ally_cost: 150", "ally_cost: 0");
    passive(&swap(&t, "incite_cost: 200", "incite_cost: 0"))
}
fn shipped_map() -> CampaignContent {
    CampaignContent::shipped(&content()).unwrap()
}
fn game(cc: CampaignContent, d: DiplomacyContent, seed: u64) -> Campaign {
    Campaign::new(content(), cc.with_diplomacy(d), seed, Some("house")).unwrap()
}
fn by_numbers() -> impl FnMut(&BattlePlan) -> BattleOutcome {
    |p: &BattlePlan| {
        let a = p.attack.units.len();
        let d = p.defend.units.len() + p.buildings.len();
        BattleOutcome::unchanged(p, Some(if a > d { Side::Attack } else { Side::Defend }))
    }
}
fn court(id: &str) -> Decision {
    Decision::Court { nation: id.into() }
}
fn ally(id: &str) -> Decision {
    Decision::Ally { nation: id.into() }
}
fn ask(id: &str) -> Decision {
    Decision::Ask { target: id.into() }
}
fn refused(c: &mut Campaign, ds: &[Decision], why: &str) {
    let before = c.state_hash();
    match c.play_round(ds, &mut by_numbers()) {
        Err(RoundError::BadDecision { why: w, .. }) => assert!(w.contains(why), "expected `{why}` in: {w}"),
        other => panic!("{ds:?} must be refused ({why}): {other:?}"),
    }
    assert_eq!(c.state_hash(), before, "a refused round changes nothing");
}
fn idx(c: &Campaign, id: &str) -> usize {
    c.nation_index(id).unwrap()
}

#[test]
fn a_courting_is_answered_by_disposition_and_paid_only_on_acceptance() {
    let d = passive(&diplomacy_text());
    let mut c = game(shipped_map(), d, 3);
    let (h, aeth, forge) = (idx(&c, "house"), idx(&c, "aetherkin"), idx(&c, "forgeborn"));
    // An empty treasury cannot pay for a courting.
    refused(&mut c, &[court("aetherkin")], "costs 100");
    while c.nations()[h].treasury < 100 {
        c.play_round(&[], &mut by_numbers()).unwrap();
    }
    assert_eq!(c.standing(h, aeth), Some(Standing::Neutral));
    assert_eq!(c.willingness(aeth, h), Some(2), "Aetherkin: ally_bias 1 + Neutral 1, House is smaller");
    assert_eq!(c.willingness(forge, h), Some(-2), "Forgeborn: ally_bias -2 + Hostile 0");

    // Aetherkin, the most reasonable house, accepts and is paid.
    let mut yes = c.clone();
    let mut not = c.clone();
    let r = yes.play_round(&[court("aetherkin")], &mut by_numbers()).unwrap();
    not.play_round(&[], &mut by_numbers()).unwrap();
    assert_eq!(yes.standing(h, aeth), Some(Standing::Tied), "one step up");
    assert_eq!(yes.standing(aeth, h), Some(Standing::Tied), "standing is per pair");
    assert_eq!(yes.nations()[h].treasury + 100, not.nations()[h].treasury, "court_cost is paid");
    assert_eq!(r.diplomacy.len(), 1);
    assert_eq!((r.diplomacy[0].actor, r.diplomacy[0].act, r.diplomacy[0].accepted), (h, Act::Court { with: aeth }, true));

    // The Forgeborn refuse, and nothing is paid; the refusal is recorded.
    let mut no = c.clone();
    let r = no.play_round(&[court("forgeborn")], &mut by_numbers()).unwrap();
    assert_eq!(no.standing(h, forge), Some(Standing::Hostile));
    assert_eq!(no.nations()[h].treasury, not.nations()[h].treasury, "a refused offer costs nothing");
    assert!(!r.diplomacy[0].accepted);
    assert_eq!(no.diplomacy_log(), &r.diplomacy[..]);

    // Courting reaches Tied at most.
    while yes.nations()[h].treasury < 100 {
        yes.play_round(&[], &mut by_numbers()).unwrap();
    }
    refused(&mut yes, &[court("aetherkin")], "Tied at most");
    refused(&mut yes, &[court("house")], "this nation");
    refused(&mut yes, &[court("nobody")], "unknown nation");
}

#[test]
fn an_alliance_needs_tied_standing_one_action_a_round_and_the_cap_of_one() {
    let mut c = game(shipped_map(), free(), 3);
    let (h, aeth, verdant) = (idx(&c, "house"), idx(&c, "aetherkin"), idx(&c, "verdant"));
    refused(&mut c, &[ally("aetherkin")], "needs Tied");
    refused(&mut c, &[court("aetherkin"), court("verdant")], "diplomatic action");
    c.play_round(&[court("aetherkin")], &mut by_numbers()).unwrap();
    c.play_round(&[ally("aetherkin")], &mut by_numbers()).unwrap();
    assert_eq!(c.standing(h, aeth), Some(Standing::Allied));
    assert!(c.allied(h, aeth) && c.allied(aeth, h));
    assert_eq!((c.ally_of(h), c.ally_of(aeth)), (Some(aeth), Some(h)));
    c.play_round(&[court("verdant")], &mut by_numbers()).unwrap();
    assert_eq!(c.standing(h, verdant), Some(Standing::Tied));
    refused(&mut c, &[ally("verdant")], "cap of 1");
    // An AI at the cap refuses an alliance (here it is not Tied with Verdant
    // either; the cap alone is shown by
    // `a_tied_and_willing_nation_already_at_the_cap_refuses_an_alliance`).
    assert!(!c.accepts(aeth, verdant, onus::campaign::diplomacy::Offer::Ally));
    // The player never answers an offer in C5.
    assert!(!c.accepts(h, verdant, onus::campaign::diplomacy::Offer::Court));
}

/// The cap binds the nation answering, too (critic C5 r1): Verdant, Tied
/// with Aetherkin and willing, would ally with Aetherkin; once allied with
/// House it refuses, and Aetherkin's AI, which offers only what would be
/// accepted, does not offer.
#[test]
fn a_tied_and_willing_nation_already_at_the_cap_refuses_an_alliance() {
    use onus::campaign::diplomacy::Offer;
    let t = swap(&diplomacy_text(), "court_cost: 100", "court_cost: 0");
    let t = swap(&t, "ally_cost: 150", "ally_cost: 0");
    let t = swap(&t, "(nation: \"verdant\", towards: [],", "(nation: \"verdant\", towards: [(\"aetherkin\", Tied)],");
    let mut c = game(shipped_map(), passive(&t), 3);
    let (h, aeth, verdant) = (idx(&c, "house"), idx(&c, "aetherkin"), idx(&c, "verdant"));
    let rules_threshold = 3;
    assert_eq!(c.standing(verdant, aeth), Some(Standing::Tied));
    assert!(c.willingness(verdant, aeth).unwrap() >= rules_threshold, "setup: willing");
    assert!(c.accepts(verdant, aeth, Offer::Ally), "control: under the cap it would accept");
    c.play_round(&[court("verdant")], &mut by_numbers()).unwrap();
    c.play_round(&[ally("verdant")], &mut by_numbers()).unwrap();
    assert_eq!(c.ally_of(verdant), Some(h), "setup: House allied with Verdant");
    assert_eq!(c.standing(verdant, aeth), Some(Standing::Tied), "still Tied");
    assert!(c.willingness(verdant, aeth).unwrap() >= rules_threshold, "still willing");
    assert!(!c.accepts(verdant, aeth, Offer::Ally), "at the cap, Verdant refuses");
    // Aetherkin's AI acting (only House passive): it never offers Verdant,
    // at the cap, an alliance, and Verdant keeps one ally.
    let full = DiplomacyContent::from_ron(&t, &content()).unwrap().with_passive(verdant);
    let mut v = Campaign::new(content(), shipped_map().with_diplomacy(full), 3, Some("house")).unwrap();
    v.play_round(&[court("verdant")], &mut by_numbers()).unwrap();
    v.play_round(&[ally("verdant")], &mut by_numbers()).unwrap();
    assert_eq!(v.ally_of(verdant), Some(h), "setup: House allied with Verdant");
    for _ in 0..4 {
        let rep = v.play_round(&[], &mut by_numbers()).unwrap();
        assert!(
            !rep.diplomacy.iter().any(|r| r.act == Act::Ally { with: verdant }),
            "an alliance was offered to a nation at the cap: {:?}",
            rep.diplomacy
        );
        assert_eq!(v.allies_of(verdant), vec![h], "Verdant keeps one ally");
    }
}

#[test]
fn the_ally_cost_is_paid_only_on_acceptance() {
    let t = swap(&diplomacy_text(), "court_cost: 100", "court_cost: 0");
    // Forgeborn come to Tied with House, but stay unwilling (-2 + 2 < 3).
    let t = swap(&t, "towards: [(\"house\", Hostile)]", "towards: [(\"house\", Tied)]");
    let t = swap(&t, "(nation: \"house\", towards: [],", "(nation: \"house\", towards: [(\"aetherkin\", Tied)],");
    let mut c = game(shipped_map(), passive(&t), 3);
    let (h, aeth, forge) = (idx(&c, "house"), idx(&c, "aetherkin"), idx(&c, "forgeborn"));
    while c.nations()[h].treasury < 150 {
        c.play_round(&[], &mut by_numbers()).unwrap();
    }
    let mut not = c.clone();
    not.play_round(&[], &mut by_numbers()).unwrap();
    let mut no = c.clone();
    no.play_round(&[ally("forgeborn")], &mut by_numbers()).unwrap();
    assert_eq!(no.standing(h, forge), Some(Standing::Tied), "refused");
    assert_eq!(no.nations()[h].treasury, not.nations()[h].treasury);
    c.play_round(&[ally("aetherkin")], &mut by_numbers()).unwrap();
    assert_eq!(c.standing(h, aeth), Some(Standing::Allied), "accepted");
    assert_eq!(c.nations()[h].treasury + 150, not.nations()[h].treasury, "ally_cost is paid");
}

/// House allied with Aetherkin from round 2 (court, then ally), at no cost.
fn allied_with_aetherkin(seed: u64) -> Campaign {
    let mut c = game(shipped_map(), free(), seed);
    c.play_round(&[court("aetherkin")], &mut by_numbers()).unwrap();
    c.play_round(&[ally("aetherkin")], &mut by_numbers()).unwrap();
    c
}

#[test]
fn allies_never_fight() {
    let mut c = allied_with_aetherkin(3);
    let h = idx(&c, "house");
    let starfall = c.city_index("starfall").unwrap();
    let army = c.armies().iter().find(|a| a.nation == h && a.at().is_some()).unwrap().id;
    // The twin: no alliance (Aetherkin courted, never allied).
    let mut twin = game(shipped_map(), free(), 3);
    twin.play_round(&[court("aetherkin")], &mut by_numbers()).unwrap();
    twin.play_round(&[], &mut by_numbers()).unwrap();
    let twin_army = twin.armies().iter().find(|a| a.nation == h && a.at().is_some()).unwrap().id;
    let march = |id| vec![Decision::March { army: id, to: "starfall".into() }];
    // Every defender holds, so nobody takes Cinderhold while House is away.
    let mut hold = |p: &BattlePlan| BattleOutcome::unchanged(p, Some(Side::Defend));
    c.play_round(&march(army), &mut hold).unwrap();
    twin.play_round(&march(twin_army), &mut hold).unwrap();
    let mut stood = false;
    let mut twin_fought = false;
    for _ in 0..8 {
        let r = c.play_round(&[], &mut hold).unwrap();
        stood |= c.army(army).is_some_and(|a| a.at() == Some(starfall));
        assert!(
            !r.battles.iter().any(|b| b.city == starfall && b.attacker == h),
            "House assaulted its ally's city: {:?}",
            r.battles
        );
        let r = twin.play_round(&[], &mut hold).unwrap();
        twin_fought |= r.battles.iter().any(|b| b.city == starfall && b.attacker == h);
        if c.is_over() || twin.is_over() {
            break;
        }
    }
    assert!(stood, "House's army stood at Starfall as a guest");
    assert!(twin_fought, "without the alliance it assaults Starfall");
}

#[test]
fn an_ally_asked_to_marches_on_the_shared_enemys_capital_for_war_rounds() {
    // Two actions a round, House starting Tied with Aetherkin: ally and ask in
    // round 1, on a seed where House acts before Aetherkin.
    let t = swap(&diplomacy_text(), "actions_per_round: 1", "actions_per_round: 2");
    let t = swap(&t, "(nation: \"house\", towards: [],", "(nation: \"house\", towards: [(\"aetherkin\", Tied)],");
    let t = swap(&swap(&t, "court_cost: 100", "court_cost: 0"), "ally_cost: 150", "ally_cost: 0");
    let d = passive(&t);
    let seed = (1..=64)
        .find(|&s| {
            let mut c = game(shipped_map(), d.clone(), s);
            let r = c.play_round(&[], &mut by_numbers()).unwrap();
            let pos = |n| r.order.iter().position(|&m| m == n).unwrap();
            pos(c.nation_index("house").unwrap()) < pos(c.nation_index("aetherkin").unwrap())
        })
        .expect("a seed where House acts first");
    let mut c = game(shipped_map(), d.clone(), seed);
    let mut twin = game(shipped_map(), d, seed);
    let (h, aeth, verdant) = (idx(&c, "house"), idx(&c, "aetherkin"), idx(&c, "verdant"));
    refused(&mut c, &[ask("verdant")], "no ally");
    assert_eq!(c.ally_of(h), None);
    c.play_round(&[ally("aetherkin"), ask("verdant")], &mut by_numbers()).unwrap();
    twin.play_round(&[ally("aetherkin")], &mut by_numbers()).unwrap();
    let w = c.war_target(aeth).expect("Aetherkin is steered");
    assert_eq!((w.target, w.until), (verdant, 4), "four rounds, this one counted");
    assert_eq!(twin.war_target(aeth), None);
    let to = |c: &Campaign| {
        let a = c.armies().iter().find(|a| a.nation == aeth).unwrap();
        match a.location {
            onus::campaign::Location::Marching { to, .. } => c.campaign().cities[to].id.clone(),
            onus::campaign::Location::At(at) => format!("at {}", c.campaign().cities[at].id),
        }
    };
    assert_eq!(to(&c), "prism_vale", "towards Greenmere, Verdant's capital");
    assert_eq!(to(&twin), "starfall", "unasked, towards Ironspire (House is its ally)");
    for r in 2..=4 {
        c.play_round(&[], &mut by_numbers()).unwrap();
        assert_eq!(c.war_target(aeth).is_some(), r < 4, "round {r}");
    }
    assert_eq!(c.war_target(aeth), None, "the war has run its four rounds");
    refused(&mut c, &[ask("aetherkin")], "ally");
    refused(&mut c, &[ask("house")], "this nation");
}

#[test]
fn an_alliance_ends_when_either_nation_falls() {
    let mut c = game(shipped_map(), free(), 3);
    let (h, ash) = (idx(&c, "house"), idx(&c, "ashen_reach"));
    c.play_round(&[court("ashen_reach")], &mut by_numbers()).unwrap();
    let r = c.play_round(&[ally("ashen_reach")], &mut by_numbers()).unwrap();
    assert_eq!((r.diplomacy[0].act, r.diplomacy[0].accepted), (Act::Ally { with: ash }, true));
    // (On this seed the Reach falls to Tollmarch in the alliance's own round.)
    assert_eq!(c.allied(h, ash), c.nations()[ash].alive);
    // Only the Reach can lose a battle.
    let mut r = |p: &BattlePlan| {
        let w = if p.defend.nation == ash { Side::Attack } else { Side::Defend };
        BattleOutcome::unchanged(p, Some(w))
    };
    for _ in 0..60 {
        if !c.nations()[ash].alive || c.is_over() {
            break;
        }
        c.play_round(&[], &mut r).unwrap();
        assert_eq!(c.allied(h, ash), c.nations()[ash].alive);
    }
    assert!(!c.nations()[ash].alive, "the Reach fell");
    assert_eq!(c.ally_of(h), None);
    assert_eq!(c.standing(h, ash), Some(Standing::Tied), "the alliance is over");
    assert!(c.outcome().is_none());
}

/// A star: House at the hub, a short road to every rival but Aetherkin.
fn star() -> CampaignContent {
    let t = std::fs::read_to_string(data_dir().join("campaign.ron")).unwrap();
    let head = &t[..t.find("    cities: [").unwrap()];
    let tail = &t[t.find("    ranks: [").unwrap()..];
    let cap = |id: &str, owner: &str| {
        format!("(id: \"{id}\", name: \"{id}\", owner: \"{owner}\", capital: true, buildings: [\"hq\"], levy: (alloy: 0, units: [])),")
    };
    let cities = [
        cap("home", "house"),
        cap("forge_cap", "forgeborn"),
        cap("verd_cap", "verdant"),
        cap("aeth_cap", "aetherkin"),
        cap("toll_cap", "tollmarch"),
        cap("ash_cap", "ashen_reach"),
    ]
    .join("\n");
    let road = |to: &str, len: u32| format!("(from: \"home\", to: \"{to}\", length: {len}, terrain: \"plain\"),");
    let roads = [road("forge_cap", 1), road("verd_cap", 1), road("aeth_cap", 90), road("toll_cap", 1), road("ash_cap", 1)].join("\n");
    let army = "(nation: \"house\", general: \"general_synthesis\", at: \"home\", units: [(\"ravager\", 8)]),";
    let armies = [army; 4].join("\n");
    let text = format!("{head}    cities: [{cities}],\n    roads: [{roads}],\n    start_armies: [{armies}],\n{tail}");
    CampaignContent::from_ron(&text, &content()).unwrap_or_else(|e| panic!("{e}"))
}

#[test]
fn the_player_wins_when_every_rival_still_standing_is_his_ally() {
    let play = |allied: bool| {
        let mut c = game(star(), free(), 5);
        c.play_round(&[court("aetherkin")], &mut by_numbers()).unwrap();
        let second = if allied { vec![ally("aetherkin")] } else { vec![] };
        c.play_round(&second, &mut by_numbers()).unwrap();
        let h = idx(&c, "house");
        let ids: Vec<u32> = c.armies().iter().filter(|a| a.nation == h).map(|a| a.id).collect();
        let ds: Vec<Decision> = ids
            .iter()
            .zip(["forge_cap", "verd_cap", "toll_cap", "ash_cap"])
            .map(|(&army, to)| Decision::March { army, to: to.into() })
            .collect();
        let mut win = |p: &BattlePlan| {
            let w = if p.attack.nation == h { Side::Attack } else { Side::Defend };
            BattleOutcome::unchanged(p, Some(w))
        };
        c.play_round(&ds, &mut win).unwrap();
        assert_eq!(c.alive(), vec![h, idx(&c, "aetherkin")], "four rivals fell");
        c
    };
    let c = play(true);
    assert_eq!(c.outcome(), Some(CampaignOutcome::Won { nation: idx(&c, "house") }));
    let c = play(false);
    assert_eq!(c.outcome(), None, "Aetherkin, Tied but no ally, still stands against him");
}
