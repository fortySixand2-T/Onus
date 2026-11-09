//! C4 critic probes (adversarial; not part of the implementation).

use std::path::PathBuf;

use onus::campaign::{
    battle_settings, BattleOutcome, BattlePlan, Campaign, CampaignContent, CampaignLog, Decision,
    HeadlessResolver, Side,
};
use onus::sim::content::Content;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data")
}
fn content() -> Content {
    Content::load_from_dir(&data_dir()).expect("content loads")
}
fn shipped_text() -> String {
    std::fs::read_to_string(data_dir().join("campaign.ron")).unwrap()
}
fn shipped() -> CampaignContent {
    CampaignContent::from_ron(&shipped_text(), &content()).unwrap()
}

/// The shipped file with its map (cities, roads, start armies) replaced.
fn with_map(cities: &str, roads: &str, armies: &str) -> CampaignContent {
    let t = shipped_text();
    let head = &t[..t.find("    cities: [").unwrap()];
    let tail = &t[t.find("    ranks: [").unwrap()..];
    let text = format!(
        "{head}    cities: [{cities}],\n    roads: [{roads}],\n    start_armies: [{armies}],\n{tail}"
    );
    CampaignContent::from_ron(&text, &content()).unwrap_or_else(|e| panic!("{e}"))
}

/// The four capitals nobody in a probe uses, each far off on a long road.
const OTHERS: &str = r#"
    (id: "verd_cap", name: "V", owner: "verdant", capital: true, buildings: ["hq"], levy: (alloy: 0, units: [])),
    (id: "aeth_cap", name: "A", owner: "aetherkin", capital: true, buildings: ["hq"], levy: (alloy: 0, units: [])),
    (id: "toll_cap", name: "T", owner: "tollmarch", capital: true, buildings: ["hq"], levy: (alloy: 0, units: [])),
"#;
const OTHER_ROADS: &str = r#"
    (from: "home", to: "verd_cap", length: 90, terrain: "plain"),
    (from: "home", to: "aeth_cap", length: 90, terrain: "plain"),
    (from: "home", to: "toll_cap", length: 90, terrain: "plain"),
"#;

/// AC4 ("no trait transfers") and AC5 ("perks apply only to the nation's own
/// ... cities"): a city that passes to a conqueror through absorption must
/// sit inside the **conqueror's** building pool. Ashen Reach's sturdier
/// buildings (BuildingHp x1.30) are its perk; they must not ride along into
/// House Vael's hands. Here the Reach's outlying city is attacked (and held)
/// in the round its capital falls, so no repair round clamps it.
#[test]
fn an_absorbed_city_does_not_keep_the_fallen_nations_building_pool() {
    let cities = format!(
        r#"
    (id: "c_x", name: "X", owner: "ashen_reach", buildings: ["hq"], levy: (alloy: 0, units: [])),
    (id: "home", name: "H", owner: "house", capital: true, buildings: ["hq"], levy: (alloy: 0, units: [])),
    (id: "ash_cap", name: "AC", owner: "ashen_reach", capital: true, buildings: ["hq"], levy: (alloy: 0, units: [])),
    (id: "forge_cap", name: "F", owner: "forgeborn", capital: true, buildings: ["hq"], levy: (alloy: 0, units: [])),
    {OTHERS}"#
    );
    let roads = format!(
        r#"
    (from: "home", to: "ash_cap", length: 1, terrain: "plain"),
    (from: "forge_cap", to: "c_x", length: 1, terrain: "plain"),
    (from: "c_x", to: "home", length: 50, terrain: "plain"),
    {OTHER_ROADS}"#
    );
    let armies = r#"
    (nation: "house", general: "general_synthesis", at: "home", units: [("sentinel", 5)]),
    (nation: "forgeborn", general: "general_bulwark", at: "forge_cap", units: [("sentinel", 5)]),
    "#;
    let cc = with_map(&cities, &roads, armies);
    let content = content();
    let mut c = Campaign::new(content.clone(), cc.clone(), 3, Some("house")).unwrap();
    let h = c.nation_index("house").unwrap();
    let f = c.nation_index("forgeborn").unwrap();
    let army = c.armies().iter().find(|a| a.nation == h).unwrap().id;
    let x = c.city_index("c_x").unwrap();
    let hq = c.cities()[x].buildings[0].def;
    // House takes the Reach's capital; Forgeborn's assault on X fails.
    let mut r = |p: &BattlePlan| {
        let w = if p.attack.nation == h { Side::Attack } else { Side::Defend };
        BattleOutcome::unchanged(p, Some(w))
    };
    c.play_round(&[Decision::March { army, to: "ash_cap".into() }], &mut r).unwrap();
    assert_eq!(c.cities()[x].owner, h, "X passed to House with the Reach's capital");
    let hp = c.cities()[x].buildings[0].hp;
    let max = c.building_max_hp(h, hq);
    let over = (hp > max).then(|| {
        format!(
            "House holds X's HQ at {hp} HP, above House's own pool {max}: the Reach's \
             BuildingHp perk transferred through conquest"
        )
    });
    // ...and the next assault on X must be a battle the content accepts.
    let mut refused = None;
    let mut r = |p: &BattlePlan| {
        if p.attack.nation == f && p.city == x {
            if let Err(e) = onus::headless::battle(content.clone(), &battle_settings(&content, &cc, p)) {
                refused = Some(e.to_string());
            }
        }
        BattleOutcome::unchanged(p, Some(Side::Defend))
    };
    c.play_round(&[], &mut r).unwrap();
    assert_eq!(
        (over, refused),
        (None, None),
        "(pool breach, battle refusal: HeadlessResolver panics on a refused battle)"
    );
}

/// A city with no standing victory building (its HQ is a ruin, or it never
/// had one) defended only by its garrison: the attacker kills every defender
/// and every building, and must take it. The battle layer's loss rules
/// (Generals fell / victory building lost / retreated) give such a defender
/// no way to lose, so every assault runs to the tick cap and "the defender
/// holds" - forever.
#[test]
#[ignore = "plays a real battle to the tick cap"]
fn annihilating_a_garrison_without_an_hq_takes_the_city() {
    let cities = format!(
        r#"
    (id: "home", name: "H", owner: "house", capital: true, buildings: ["hq"], levy: (alloy: 0, units: [])),
    (id: "ruin", name: "R", owner: "ashen_reach", buildings: ["foundry"], levy: (alloy: 0, units: ["sentinel"])),
    (id: "ash_cap", name: "AC", owner: "ashen_reach", capital: true, buildings: ["hq"], levy: (alloy: 0, units: [])),
    (id: "forge_cap", name: "F", owner: "forgeborn", capital: true, buildings: ["hq"], levy: (alloy: 0, units: [])),
    {OTHERS}"#
    );
    let roads = format!(
        r#"
    (from: "home", to: "ruin", length: 1, terrain: "plain"),
    (from: "ruin", to: "ash_cap", length: 90, terrain: "plain"),
    (from: "home", to: "forge_cap", length: 90, terrain: "plain"),
    {OTHER_ROADS}"#
    );
    let armies = r#"
    (nation: "house", general: "general_synthesis", at: "home", units: [("ravager", 8), ("sentinel", 8)]),
    "#;
    let cc = with_map(&cities, &roads, armies);
    let mut c = Campaign::new(content(), cc, 3, Some("house")).unwrap();
    let h = c.nation_index("house").unwrap();
    let army = c.armies().iter().find(|a| a.nation == h).unwrap().id;
    let ruin = c.city_index("ruin").unwrap();
    let mut r = HeadlessResolver;
    c.play_round(&[], &mut r).unwrap(); // the levy garrisons the ruin
    assert_eq!(c.cities()[ruin].garrison.len(), 1);
    let rep = c.play_round(&[Decision::March { army, to: "ruin".into() }], &mut r).unwrap();
    let b = rep.battles.iter().find(|b| b.city == ruin).expect("a battle at the ruin");
    eprintln!("{b:?}; army now {:?}", c.army(army).map(|a| a.units.len()));
    assert!(
        b.captured,
        "16 units and a General against one Sentinel and a Foundry: the city must fall \
         ({b:?}); the defender had no way to lose"
    );
}

/// AC11: a save loads back to the very same campaign, and both play on
/// identically, including the log itself.
#[test]
fn a_resumed_campaign_is_the_same_campaign_and_its_save_is_the_same_save() {
    let mut c = Campaign::new(content(), shipped(), 21, Some("house")).unwrap();
    let h = c.nation_index("house").unwrap();
    let army = c.armies().iter().find(|a| a.nation == h).unwrap().id;
    let mut r = |p: &BattlePlan| BattleOutcome::unchanged(p, Some(if p.round.is_multiple_of(2) { Side::Attack } else { Side::Defend }));
    c.play_round(&[Decision::March { army, to: "rootfall".into() }], &mut r).unwrap();
    for _ in 0..5 {
        c.play_round(&[], &mut r).unwrap();
    }
    let log = c.log();
    let text = log.to_ron().unwrap();
    let back = CampaignLog::from_ron(&text).unwrap();
    assert_eq!(back, log);
    let mut d = Campaign::resume(content(), shipped(), &back, &mut r).unwrap();
    assert_eq!(d.state_hash(), c.state_hash());
    assert_eq!(d.history(), c.history());
    assert_eq!(d.armies(), c.armies());
    assert_eq!(d.cities(), c.cities());
    assert_eq!(d.nations(), c.nations());
    assert_eq!(d.log(), c.log());
    for _ in 0..4 {
        if c.is_over() {
            break;
        }
        let a = c.play_round(&[], &mut r).unwrap();
        let b = d.play_round(&[], &mut r).unwrap();
        assert_eq!(a, b);
        assert_eq!(c.state_hash(), d.state_hash());
    }
}

/// AC11: tampering with anything that is not a hash (the player, the seed,
/// a decision) is refused on load; so is a forged last hash.
#[test]
fn every_kind_of_tamper_is_refused() {
    let mut c = Campaign::new(content(), shipped(), 21, Some("house")).unwrap();
    let h = c.nation_index("house").unwrap();
    let army = c.armies().iter().find(|a| a.nation == h).unwrap().id;
    let mut r = |p: &BattlePlan| BattleOutcome::unchanged(p, Some(Side::Defend));
    c.play_round(&[], &mut r).unwrap();
    c.play_round(&[Decision::March { army, to: "rootfall".into() }], &mut r).unwrap();
    c.play_round(&[], &mut r).unwrap();
    let log = c.log();
    let mut tampers: Vec<(&str, CampaignLog)> = Vec::new();
    let mut t = log.clone();
    t.player = Some("forgeborn".into());
    tampers.push(("player", t));
    let mut t = log.clone();
    t.player = None;
    tampers.push(("no player", t));
    let mut t = log.clone();
    t.seed ^= 1;
    tampers.push(("seed", t));
    let mut t = log.clone();
    t.rounds[1] = vec![Decision::March { army, to: "forge_gate".into() }];
    tampers.push(("decision", t));
    let mut t = log.clone();
    t.rounds.swap(0, 1);
    tampers.push(("decision order", t));
    let mut t = log.clone();
    *t.hashes.last_mut().unwrap() ^= 1;
    tampers.push(("last hash", t));
    let mut t = log.clone();
    t.campaign ^= 1;
    tampers.push(("campaign fingerprint", t));
    for (what, t) in tampers {
        assert!(Campaign::resume(content(), shipped(), &t, &mut r).is_err(), "{what} tamper accepted");
    }
}

/// Seeded: the seed must reach the outcome-relevant draws. Different seeds
/// must give a different turn order somewhere in a few rounds.
#[test]
fn the_seed_reaches_the_turn_order() {
    let mut orders = Vec::new();
    for seed in 0..8u64 {
        let mut c = Campaign::new(content(), shipped(), seed, None).unwrap();
        let mut r = |p: &BattlePlan| BattleOutcome::unchanged(p, Some(Side::Defend));
        let rep = c.play_round(&[], &mut r).unwrap();
        orders.push(rep.order);
    }
    orders.sort();
    orders.dedup();
    assert!(orders.len() > 1, "eight seeds, one turn order");
}

/// The balance reading, as a list: per seed the winner, the length and the
/// battle-by-battle record (city, attacker, winner, captured). Prints only.
#[test]
#[ignore = "the seed-variety probe: real campaigns"]
fn seed_variety_probe() {
    let (c0, cc) = (content(), shipped());
    let mut seen: Vec<String> = Vec::new();
    for seed in 1..=10u64 {
        let mut c = Campaign::new(c0.clone(), cc.clone(), seed, None).unwrap();
        while !c.is_over() {
            c.play_round(&[], &mut HeadlessResolver).unwrap();
        }
        let trace: Vec<String> = c
            .history()
            .iter()
            .map(|b| format!("r{} c{} a{} d{} {:?} {}", b.round, b.city, b.attacker, b.defender, b.winner, b.captured))
            .collect();
        let s = format!("{:?} {} | {}", c.outcome(), c.round(), trace.join(", "));
        eprintln!("seed {seed}: {s}");
        seen.push(s);
    }
    seen.sort();
    seen.dedup();
    eprintln!("distinct campaigns (battle-for-battle): {}", seen.len());
}
