//! C4 AC9 — a round launches a battle with `MatchSettings` built from
//! campaign state: the nations, their traits and size, and the city being
//! fought over.
//!
//! The mapping (F-089): the attacker plays Faction A and the holder Faction
//! B, on a map side and a battle seed drawn from the campaign's rng. Each
//! side plays its nation (its traits) and its nation's strategy. Its starting
//! Alloy is `starting_alloy + alloy_per_city x` the cities it holds (its
//! size). Its roster is its armies' units at their HP, then its battle-only
//! workers; the holder also fields the city's standing buildings at their HP.

use std::path::PathBuf;

use onus::campaign::{battle_settings, BattleOutcome, BattlePlan, Campaign, CampaignContent, Decision, Side};
use onus::headless::{self, Orientation};
use onus::sim::battle::{BattleState, General};
use onus::sim::combat::Health;
use onus::sim::content::Content;
use onus::sim::economy::{Building, Stockpiles, UnitDefIdx};
use onus::sim::spatial::Faction;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data")
}
fn content() -> Content {
    Content::load_from_dir(&data_dir()).expect("content loads")
}
fn campaign(seed: u64) -> Campaign {
    let t = std::fs::read_to_string(data_dir().join("campaign.ron")).unwrap();
    let shipped = "units: [(\"sentinel\", 2), (\"ripper\", 2), (\"arclight\", 2)]";
    assert!(t.contains(shipped));
    let t = t.replacen(shipped, "units: [(\"ripper\", 6)]", 1);
    let cc = CampaignContent::from_ron(&t, &content()).expect("loads");
    Campaign::new(content(), cc, seed, Some("house")).unwrap()
}
/// House's assault on Forge Gate (round 1), its units hurt: the plan.
fn house_assault(seed: u64) -> (Campaign, BattlePlan) {
    let mut c = campaign(seed);
    let h = c.nation_index("house").unwrap();
    let army = c.armies().iter().find(|a| a.nation == h).unwrap().id;
    let mut seen = None;
    let mut r = |p: &BattlePlan| {
        if p.attack.nation == h {
            seen = Some(p.clone());
        }
        BattleOutcome::unchanged(p, Some(Side::Defend))
    };
    c.play_round(&[Decision::March { army, to: "forge_gate".into() }], &mut r).unwrap();
    (c, seen.expect("House's assault on Forge Gate"))
}

#[test]
fn the_settings_carry_the_nations_their_strategies_and_the_rules() {
    let (c, p) = house_assault(3);
    let s = battle_settings(c.content(), c.campaign(), &p);
    let n = &c.content().nations;
    let (a, d) = (&n[p.attack.nation], &n[p.defend.nation]);
    assert_eq!(s.nations, [Some(a.id.clone()), Some(d.id.clone())], "attacker A, holder B");
    assert_eq!(s.strategies, [Some(a.strategy.clone()), Some(d.strategy.clone())]);
    assert_eq!(s.economy, c.campaign().economy);
    assert_eq!(s.tick_cap, c.campaign().rules.battle.tick_cap);
    assert_eq!(s.seed, p.seed);
    assert_eq!(
        s.orientation,
        if p.flipped { Orientation::Swapped } else { Orientation::Normal }
    );
    // Size: each side's Alloy by the cities its nation holds.
    let b = &c.campaign().rules.battle;
    let want = |cities: u32| b.starting_alloy + b.alloy_per_city * cities;
    assert_eq!(p.attack.cities, c.cities_of(p.attack.nation).len() as u32);
    assert_eq!(p.defend.cities, c.cities_of(p.defend.nation).len() as u32);
    assert_eq!(s.starting_alloy, Some([want(p.attack.cities), want(p.defend.cities)]));
}

#[test]
fn the_rosters_are_the_armies_the_workers_and_the_city() {
    let (c, p) = house_assault(3);
    let s = battle_settings(c.content(), c.campaign(), &p);
    let cn = c.content();
    let b = &c.campaign().rules.battle;
    let setup = s.battle.as_ref().expect("a campaign battle");
    for (side, ps, workers) in [
        (Faction::A, &p.attack, b.attacker_workers),
        (Faction::B, &p.defend, b.defender_workers),
    ] {
        let ss = setup.side(side);
        let generals: Vec<String> = ps.generals.iter().map(|g| cn.general_at(g.kind).unwrap().id.clone()).collect();
        assert_eq!(ss.generals, generals, "{side:?}'s Generals, in army order");
        let roster = ss.roster.as_ref().expect("a roster, never the skirmish start");
        assert_eq!(roster.units.len(), ps.units.len() + workers as usize);
        for (r, u) in roster.units.iter().zip(&ps.units) {
            assert_eq!(r.id, cn.units[u.def].id);
            assert_eq!(r.hp, Some(u.hp));
        }
        assert!(roster.units[ps.units.len()..].iter().all(|r| r.id == "worker" && r.hp.is_none()));
    }
    let attack = setup.side(Faction::A).roster.as_ref().unwrap();
    assert!(attack.buildings.is_empty(), "the attacker brings no buildings");
    let defend = setup.side(Faction::B).roster.as_ref().unwrap();
    assert_eq!(defend.buildings.len(), p.buildings.len());
    assert!(!p.buildings.is_empty());
    for (r, pb) in defend.buildings.iter().zip(&p.buildings) {
        assert_eq!(r.id, cn.buildings[pb.def].id);
        assert_eq!(r.hp, Some(pb.hp));
    }
}

#[test]
fn the_settings_build_the_battle_the_campaign_describes() {
    let (c, p) = house_assault(3);
    let s = battle_settings(c.content(), c.campaign(), &p);
    let mut app = headless::battle(content(), &s).expect("the campaign's battle builds");
    assert!(app.world().get_resource::<BattleState>().is_some());
    let b = &c.campaign().rules.battle;
    let mut q = app.world_mut().query::<(&Faction, &UnitDefIdx, &Health, Option<&General>)>();
    let mut roster = [0usize; 2];
    let mut generals = [0usize; 2];
    for (f, _, _, g) in q.iter(app.world()) {
        let i = (*f == Faction::B) as usize;
        if g.is_some() {
            generals[i] += 1;
        } else {
            roster[i] += 1;
        }
    }
    assert_eq!(generals, [p.attack.generals.len(), p.defend.generals.len()]);
    assert_eq!(
        roster,
        [
            p.attack.units.len() + b.attacker_workers as usize,
            p.defend.units.len() + b.defender_workers as usize
        ]
    );
    let mut q = app.world_mut().query::<(&Faction, &Building)>();
    let buildings = q.iter(app.world()).filter(|(f, _)| **f == Faction::B).count();
    assert_eq!(buildings, p.buildings.len());
    let st = app.world().resource::<Stockpiles>();
    let [sa, sb] = s.starting_alloy.unwrap();
    assert_eq!((st.alloy(Faction::A), st.alloy(Faction::B)), (sa, sb));
}

#[test]
fn battle_seeds_come_from_the_campaign_seed() {
    let (_, p1) = house_assault(3);
    let (_, p2) = house_assault(3);
    assert_eq!(p1, p2, "one campaign seed, one battle");
    let seeds: Vec<(u64, bool)> = (0..8).map(house_assault).map(|(_, p)| (p.seed, p.flipped)).collect();
    let mut distinct: Vec<u64> = seeds.iter().map(|s| s.0).collect();
    distinct.sort();
    distinct.dedup();
    assert_eq!(distinct.len(), 8, "every campaign seed gives its battle its own seed");
    assert!(seeds.iter().any(|s| s.1) && seeds.iter().any(|s| !s.1), "both map sides occur");
}

#[test]
fn a_skirmish_without_starting_alloy_is_unchanged() {
    let s = headless::MatchSettings::default();
    assert_eq!(s.starting_alloy, None);
    let app = headless::ai_vs_ai(content(), &s).unwrap();
    let want = content().economy.starting_alloy;
    let st = app.world().resource::<Stockpiles>();
    assert_eq!((st.alloy(Faction::A), st.alloy(Faction::B)), (want, want));
}
