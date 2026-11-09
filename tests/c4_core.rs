//! C4 AC1 — `src/campaign/` is a deterministic, seeded, render-free campaign
//! state machine that owns its own state hash. It holds the map, the nations
//! and their cities, ownership, rounds and conquest.
//!
//! Battles here go to scripted resolvers (closures over the plan), so these
//! tests pin the state machine alone; the headless battle sim is AC10's.

use std::path::PathBuf;

use onus::campaign::{
    BattleOutcome, BattlePlan, Campaign, CampaignContent, CampaignOutcome, Decision, Location,
    RoundError, Side,
};
use onus::sim::content::Content;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data")
}
fn content() -> Content {
    Content::load_from_dir(&data_dir()).expect("content loads")
}
fn shipped_text() -> String {
    std::fs::read_to_string(data_dir().join("campaign.ron")).expect("campaign.ron")
}
fn shipped() -> CampaignContent {
    CampaignContent::shipped(&content()).expect("the shipped campaign loads")
}
fn new(seed: u64, player: Option<&str>) -> Campaign {
    Campaign::new(content(), shipped(), seed, player).expect("campaign starts")
}
fn city(c: &Campaign, id: &str) -> usize {
    c.city_index(id).unwrap_or_else(|| panic!("city {id}"))
}
fn nation(c: &Campaign, id: &str) -> usize {
    c.nation_index(id).unwrap_or_else(|| panic!("nation {id}"))
}
/// A resolver where `n`'s assaults always succeed and everyone else's fail.
fn favour(n: usize) -> impl FnMut(&BattlePlan) -> BattleOutcome {
    move |p: &BattlePlan| {
        let w = if p.attack.nation == n { Side::Attack } else { Side::Defend };
        BattleOutcome::unchanged(p, Some(w))
    }
}
fn invalid(text: &str) -> String {
    match CampaignContent::from_ron(text, &content()) {
        Ok(_) => panic!("must be refused"),
        Err(e) => e.to_string(),
    }
}

#[test]
fn the_shipped_map_holds_sixteen_cities_and_every_nation_has_one_capital() {
    let c = new(1, None);
    assert_eq!(c.cities().len(), 16, "C0: 16 cities");
    let sizes: Vec<(String, usize)> = c
        .content()
        .nations
        .iter()
        .enumerate()
        .map(|(n, d)| (d.id.clone(), c.cities_of(n).len()))
        .collect();
    let want = [
        ("house", 1),
        ("forgeborn", 4),
        ("verdant", 4),
        ("aetherkin", 3),
        ("tollmarch", 2),
        ("ashen_reach", 2),
    ];
    for (id, n) in want {
        let got = sizes.iter().find(|(i, _)| i == id).map(|s| s.1);
        assert_eq!(got, Some(n), "{id}'s size (the brief's table)");
    }
    for n in 0..c.content().nations.len() {
        let cap = c.campaign().capital[n];
        assert_eq!(c.cities()[cap].owner, n, "each nation holds its capital");
        assert!(c.campaign().cities[cap].capital);
    }
    assert_eq!(c.campaign().cities[city(&c, "cinderhold")].owner, "house");
    assert_eq!(c.alive().len(), 6);
    assert_eq!(c.armies().len(), 6, "one start army per nation");
    assert_eq!(c.round(), 0);
    assert!(!c.is_over());
}

#[test]
fn a_bad_campaign_file_is_refused_with_a_clear_reason() {
    let t = shipped_text();
    let cases = [
        (
            t.replacen("owner: \"verdant\", capital: true", "owner: \"nobody\", capital: true", 1),
            "unknown nation `nobody`",
        ),
        (
            t.replacen("(id: \"forge_gate\", name: \"Forge Gate\", owner: \"forgeborn\",",
                "(id: \"forge_gate\", name: \"Forge Gate\", owner: \"forgeborn\", capital: true,", 1),
            "two capitals",
        ),
        (t.replacen("buildings: [\"hq\"], levy: (alloy: 40, units: [\"bulwark\"])",
            "buildings: [\"castle\"], levy: (alloy: 40, units: [\"bulwark\"])", 1), "unknown building `castle`"),
        (t.replacen("to: \"ironspire\",", "to: \"atlantis\",", 1), "unknown city"),
        (t.replacen("length: 7, terrain: \"plain\"", "length: 0, terrain: \"plain\"", 1), "length 0"),
        (t.replacen("terrain: \"rough\"", "terrain: \"swamp\"", 1), "unknown terrain `swamp`"),
        (t.replacen("at: \"ironspire\"", "at: \"greenmere\"", 1), "does not hold"),
        (t.replacen("(\"sentinel\", 4)", "(\"worker\", 4)", 1), "not a combat unit"),
        (t.replacen("general: \"general_bulwark\",   at:", "general: \"general_nobody\",   at:", 1),
            "unknown general"),
        (t.replacen("(xp: 0, radius_mult: 1.0,  extra_factor: 0)", "(xp: 0, radius_mult: 1.2,  extra_factor: 0)", 1),
            "rank 0 must be"),
        (t.replacen("round_cap: 40", "round_cap: 0", 1), "round_cap is 0"),
        (t.replacen("economy: Alloy,", "economy: Alloy, surprise: 1,", 1), "cannot parse"),
    ];
    for (text, why) in cases {
        assert_ne!(text, t, "the edit for `{why}` applied");
        let e = invalid(&text);
        assert!(e.contains(why), "expected `{why}` in: {e}");
    }
    // An unreachable city: cut both roads into the Ashen Reach's capital and
    // its neighbour's last link... simplest, an island city.
    let island = t.replacen(
        "    ],\n    roads: [",
        "        (id: \"isle\", name: \"Isle\", owner: \"house\",\n         buildings: [\"hq\"], levy: (alloy: 1, units: [\"sentinel\"])),\n    ],\n    roads: [",
        1,
    );
    assert_ne!(island, t);
    assert!(invalid(&island).contains("not reachable"), "{}", invalid(&island));
}

#[test]
fn a_campaign_refuses_an_unknown_player_nation() {
    assert!(Campaign::new(content(), shipped(), 1, Some("atlantis")).is_err());
}

/// Same seed, same content, same decisions: the same hash after every round.
/// Another seed: another campaign.
#[test]
fn the_campaign_is_deterministic_and_owns_its_state_hash() {
    let run = |seed: u64| {
        let mut c = new(seed, None);
        let first = c.state_hash();
        let mut r = favour(c.nation_index("verdant").unwrap());
        for _ in 0..12 {
            if c.is_over() {
                break;
            }
            c.play_round(&[], &mut r).expect("round plays");
        }
        (first, c.round_hashes().to_vec(), c.state_hash())
    };
    let a = run(7);
    let b = run(7);
    assert_eq!(a, b, "same seed, same campaign, hash for hash");
    assert!(!a.1.is_empty());
    assert_eq!(a.1.last().copied(), Some(a.2), "the last round hash is the state hash");
    let c = run(8);
    assert_ne!(a.0, c.0, "the seed is part of the state");
    assert_ne!(a.1, c.1, "another seed plays another campaign");
}

#[test]
fn the_state_hash_moves_with_the_state() {
    let c0 = new(3, Some("house"));
    let mut c1 = c0.clone();
    assert_eq!(c0.state_hash(), c1.state_hash());
    let army = c1.armies()[0].id;
    // Holding a round still moves the round counter and the RNG.
    c1.play_round(&[], &mut favour(99)).unwrap();
    assert_ne!(c0.state_hash(), c1.state_hash());
    let mut c2 = c0.clone();
    c2.play_round(&[Decision::March { army, to: "forge_gate".into() }], &mut favour(99))
        .unwrap();
    assert_ne!(c1.state_hash(), c2.state_hash(), "a march shows in the hash");
}

/// Player House takes a border city, then a capital: the capital's nation is
/// out, its cities are the conqueror's, its armies are gone.
#[test]
fn conquest_takes_cities_and_a_capital_ends_its_nation() {
    let mut c = new(5, Some("house"));
    let house = nation(&c, "house");
    let forge = nation(&c, "forgeborn");
    let army = c.armies().iter().find(|a| a.nation == house).unwrap().id;
    let mut r = favour(house);
    let walk = |c: &mut Campaign, r: &mut dyn onus::campaign::Resolver, to: &str| {
        let target = city(c, to);
        c.play_round(&[Decision::March { army, to: to.into() }], r).unwrap();
        for _ in 0..20 {
            if c.army(army).unwrap().location == Location::At(target) {
                return;
            }
            c.play_round(&[], r).unwrap();
        }
        panic!("army never reached {to}");
    };
    walk(&mut c, &mut r, "forge_gate");
    assert_eq!(c.cities()[city(&c, "forge_gate")].owner, house, "forge_gate taken");
    assert!(c.nations()[forge].alive, "a border city is not the nation");
    let taken = c.history().iter().find(|b| b.city == city(&c, "forge_gate")).unwrap();
    assert!(taken.captured && !taken.absorbed);
    assert_eq!((taken.attacker, taken.defender), (house, forge));

    walk(&mut c, &mut r, "ironspire");
    assert!(!c.nations()[forge].alive, "taking the capital ends the Forgeborn");
    assert!(c.cities_of(forge).is_empty());
    for id in ["ironspire", "brasswork", "anvil_reach", "forge_gate"] {
        assert_eq!(c.cities()[city(&c, id)].owner, house, "{id} passes to the conqueror");
    }
    assert!(c.armies().iter().all(|a| a.nation != forge), "its armies are disbanded");
    let fell = c.history().iter().find(|b| b.city == city(&c, "ironspire")).unwrap();
    assert!(fell.captured && fell.absorbed);
}

#[test]
fn a_failed_assault_falls_back_to_the_city_it_came_from() {
    let mut c = new(5, Some("house"));
    let house = nation(&c, "house");
    let army = c.armies().iter().find(|a| a.nation == house).unwrap().id;
    let home = city(&c, "cinderhold");
    let mut r = |p: &BattlePlan| BattleOutcome::unchanged(p, Some(Side::Defend));
    c.play_round(&[Decision::March { army, to: "forge_gate".into() }], &mut r).unwrap();
    for _ in 0..20 {
        if c.history().iter().any(|b| b.attacker == house) {
            break;
        }
        c.play_round(&[], &mut r).unwrap();
    }
    let b = c.history().iter().find(|b| b.attacker == house).expect("the assault was fought");
    assert_eq!(b.winner, Some(Side::Defend));
    assert!(!b.captured);
    assert_eq!(c.cities()[city(&c, "forge_gate")].owner, nation(&c, "forgeborn"));
    assert_eq!(c.army(army).unwrap().location, Location::At(home), "back home");
}

#[test]
fn an_undecided_battle_means_the_defender_holds() {
    let mut c = new(5, Some("house"));
    let house = nation(&c, "house");
    let army = c.armies().iter().find(|a| a.nation == house).unwrap().id;
    let mut r = |p: &BattlePlan| BattleOutcome::unchanged(p, None);
    c.play_round(&[Decision::March { army, to: "forge_gate".into() }], &mut r).unwrap();
    for _ in 0..20 {
        if c.history().iter().any(|b| b.attacker == house) {
            break;
        }
        c.play_round(&[], &mut r).unwrap();
    }
    let b = c.history().iter().find(|b| b.attacker == house).unwrap();
    assert_eq!(b.winner, None);
    assert_eq!(c.cities()[city(&c, "forge_gate")].owner, nation(&c, "forgeborn"));
}

#[test]
fn a_refused_round_changes_nothing() {
    let mut c = new(5, Some("house"));
    let before = c.state_hash();
    let bad = [
        Decision::March { army: 999, to: "forge_gate".into() },
        Decision::March { army: c.armies()[1].id, to: "forge_gate".into() }, // not House's
        Decision::March { army: c.armies()[0].id, to: "ironspire".into() },  // no road
        Decision::March { army: c.armies()[0].id, to: "atlantis".into() },
    ];
    assert_eq!(c.armies()[0].nation, nation(&c, "house"));
    for d in bad {
        let e = c.play_round(std::slice::from_ref(&d), &mut favour(0)).unwrap_err();
        assert!(matches!(e, RoundError::BadDecision { .. }), "{e}");
        assert_eq!(c.state_hash(), before, "{d:?}: refused, untouched");
        assert_eq!(c.round(), 0);
    }
    let mut ai = new(5, None);
    let e = ai
        .play_round(&[Decision::March { army: 1, to: "forge_gate".into() }], &mut favour(0))
        .unwrap_err();
    assert_eq!(e, RoundError::NoPlayer);
}

/// Every assault succeeds: the AI conquers until one nation is left, and the
/// campaign is over, won by domination; it then refuses another round.
#[test]
fn domination_ends_the_campaign() {
    let mut c = new(11, None);
    let mut r = |p: &BattlePlan| BattleOutcome::unchanged(p, Some(Side::Attack));
    while !c.is_over() {
        c.play_round(&[], &mut r).unwrap();
    }
    match c.outcome() {
        Some(CampaignOutcome::Won { nation }) => {
            assert_eq!(c.alive(), vec![nation]);
            assert_eq!(c.cities_of(nation).len(), 16, "the winner holds the map");
        }
        o => panic!("expected domination, got {o:?} after {} rounds", c.round()),
    }
    assert_eq!(c.play_round(&[], &mut r).unwrap_err(), RoundError::Over);
}

#[test]
fn the_player_loses_with_his_capital() {
    let mut c = new(2, Some("house"));
    let house = nation(&c, "house");
    // House never marches; everyone's assaults succeed.
    let mut r = |p: &BattlePlan| BattleOutcome::unchanged(p, Some(Side::Attack));
    while !c.is_over() {
        c.play_round(&[], &mut r).unwrap();
    }
    if c.cities()[c.campaign().capital[house]].owner != house {
        assert_eq!(c.outcome(), Some(CampaignOutcome::PlayerLost));
        assert!(!c.nations()[house].alive);
    } else {
        panic!("Cinderhold borders four nations and was never taken: {:?}", c.outcome());
    }
}

#[test]
fn nothing_holds_on_a_bare_round_cap() {
    let t = shipped_text()
        .replacen("round_cap: 40", "round_cap: 2", 1)
        .replacen("(min_rounds: 15, max_rounds: 25)", "(min_rounds: 1, max_rounds: 2)", 1);
    let cc = CampaignContent::from_ron(&t, &content()).unwrap();
    let mut c = Campaign::new(content(), cc, 1, None).unwrap();
    let mut r = |p: &BattlePlan| BattleOutcome::unchanged(p, None);
    c.play_round(&[], &mut r).unwrap();
    assert!(!c.is_over());
    c.play_round(&[], &mut r).unwrap();
    assert_eq!(c.outcome(), Some(CampaignOutcome::Undecided));
}

/// `src/campaign/` is render-free: no render or window type, no wall-clock,
/// no hash map, nothing from the Bevy driver.
#[test]
fn the_campaign_module_is_render_free() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/campaign");
    let banned = [
        "bevy::render", "bevy::sprite", "bevy::window", "bevy::ui", "bevy::text", "bevy::winit",
        "bevy::input", "Sprite", "Camera", "Mesh", "Color", "Instant", "SystemTime", "HashMap",
        "HashSet", "crate::client", "crate::input", "crate::ui", "crate::setup", "thread_rng",
        "DefaultPlugins",
    ];
    let mut files = 0;
    for e in std::fs::read_dir(&dir).expect("src/campaign") {
        let p = e.unwrap().path();
        if p.extension().is_some_and(|x| x == "rs") {
            files += 1;
            let text = std::fs::read_to_string(&p).unwrap();
            for b in banned {
                assert!(!text.contains(b), "{} uses `{b}`", p.display());
            }
        }
    }
    assert!(files >= 3, "the campaign's sources were scanned");
}
