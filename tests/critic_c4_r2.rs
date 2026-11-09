//! C4 critic round 2 probes (adversarial; not part of the implementation).

use std::path::PathBuf;

use onus::campaign::{battle_settings, BattleOutcome, BattlePlan, Campaign, CampaignContent, Decision, Side};
use onus::sim::content::Content;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data")
}

/// The shipped content, with the Reach given a unit-HP perk (Defense +1 on the sentinel; was: on
/// every combat unit) next to its BuildingHp one: legal data (TraitStat::Defense
/// is "a unit's HP pool"), so the campaign must keep it the Reach's own.
fn hardy_reach() -> Content {
    let dir = std::env::temp_dir().join(format!("onus_c4_critic_r2_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for e in std::fs::read_dir(data_dir()).unwrap() {
        let e = e.unwrap();
        if e.file_type().unwrap().is_file() {
            std::fs::copy(e.path(), dir.join(e.file_name())).unwrap();
        }
    }
    let n = std::fs::read_to_string(data_dir().join("nations.ron")).unwrap();
    let perk = "(applies_to: Buildings, stat: BuildingHp, mul: 1.30),";
    let i = n.find("id: \"ashen_reach\"").unwrap();
    let (head, tail) = n.split_at(i);
    let tail = tail.replace(perk, &format!("{perk}\n                (applies_to: Unit(\"sentinel\"), stat: Defense, add: 1),"));
    std::fs::write(dir.join("nations.ron"), format!("{head}{tail}")).unwrap();
    let c = Content::load_from_dir(&dir).unwrap_or_else(|e| panic!("{e}"));
    let _ = std::fs::remove_dir_all(&dir);
    c
}

fn with_map(content: &Content, cities: &str, roads: &str, armies: &str) -> CampaignContent {
    let t = std::fs::read_to_string(data_dir().join("campaign.ron")).unwrap();
    let head = &t[..t.find("    cities: [").unwrap()];
    let tail = &t[t.find("    ranks: [").unwrap()..];
    let text = format!("{head}    cities: [{cities}],\n    roads: [{roads}],\n    start_armies: [{armies}],\n{tail}");
    CampaignContent::from_ron(&text, content).unwrap_or_else(|e| panic!("{e}"))
}

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

/// AC4 ("no trait transfers") / AC5 ("perks apply only to the nation's own
/// armies and cities"), round 2: the city's *garrison* is per-holder state
/// too. A levy raised by the Reach in X sits there at the Reach's unit pool;
/// when X passes to House through absorption, those units are House's, and
/// must be within House's own pool - else the Reach's perk rides along, and
/// the next battle at X is one the content refuses (HeadlessResolver panics).
#[test]
fn an_absorbed_garrison_does_not_keep_the_fallen_nations_unit_pool() {
    let content = hardy_reach();
    let cities = format!(
        r#"
    (id: "c_x", name: "X", owner: "ashen_reach", buildings: ["hq"], levy: (alloy: 0, units: ["sentinel", "sentinel"])),
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
    let cc = with_map(&content, &cities, &roads, armies);
    let mut c = Campaign::new(content.clone(), cc.clone(), 3, Some("house")).unwrap();
    let h = c.nation_index("house").unwrap();
    let a = c.nation_index("ashen_reach").unwrap();
    let f = c.nation_index("forgeborn").unwrap();
    let army = c.armies().iter().find(|x| x.nation == h).unwrap().id;
    let x = c.city_index("c_x").unwrap();
    let sentinel = c.cities()[x].garrison.first().map(|u| u.def);
    // Round 1: nobody takes anything; the Reach's levy gathers in X.
    let mut hold = |p: &BattlePlan| BattleOutcome::unchanged(p, Some(Side::Defend));
    c.play_round(&[], &mut hold).unwrap();
    let g = c.cities()[x].garrison.clone();
    assert!(!g.is_empty(), "the Reach's levy garrisons X");
    let def = sentinel.unwrap_or(g[0].def);
    assert!(
        c.unit_max_hp(a, def) > c.unit_max_hp(h, def),
        "probe premise: the Reach's sentinel pool ({}) is above House's ({})",
        c.unit_max_hp(a, def),
        c.unit_max_hp(h, def)
    );
    // Round 2: House takes the Reach's capital; X (attacked and held, or not) passes to House.
    let mut r = |p: &BattlePlan| {
        let w = if p.attack.nation == h { Side::Attack } else { Side::Defend };
        BattleOutcome::unchanged(p, Some(w))
    };
    c.play_round(&[Decision::March { army, to: "ash_cap".into() }], &mut r).unwrap();
    assert_eq!(c.cities()[x].owner, h, "X passed to House with the Reach's capital");
    let over: Vec<String> = c.cities()[x]
        .garrison
        .iter()
        .filter(|u| u.hp > c.unit_max_hp(h, u.def))
        .map(|u| format!("unit {} at {} HP over House's pool {}", u.def, u.hp, c.unit_max_hp(h, u.def)))
        .collect();
    // ...and the next assault on X must be a battle the content accepts.
    let mut refused = None;
    for _ in 0..3 {
        let mut r = |p: &BattlePlan| {
            if p.attack.nation == f && p.city == x && refused.is_none() {
                if let Err(e) = onus::headless::battle(content.clone(), &battle_settings(&content, &cc, p)) {
                    refused = Some(e.to_string());
                }
            }
            BattleOutcome::unchanged(p, Some(Side::Defend))
        };
        c.play_round(&[], &mut r).unwrap();
    }
    assert_eq!(
        (over, refused),
        (Vec::<String>::new(), None),
        "(garrison over its new holder's pool, battle refusal: HeadlessResolver panics on a refused battle)"
    );
}
