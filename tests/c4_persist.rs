//! C4 AC8 — **armies persist**: survivors from each battle outcome return to
//! their army, and the dead are removed. Each held city adds a reinforcement
//! levy every round.
//!
//! The levy (CAMPAIGN_PLAN C4, F-089): a city's `levy.alloy` goes to its
//! holder's treasury; its `levy.units` join the holder's lowest-id army
//! standing in the city with room under `army_cap`, or else the city's
//! garrison (capped at `army_cap` too). An army standing in a city it holds
//! takes in the garrison first. A garrison defends its city.

use std::path::PathBuf;

use onus::campaign::{BattleOutcome, BattlePlan, Campaign, CampaignContent, Decision, Side};
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
fn campaign(text: &str, player: Option<&str>) -> Campaign {
    // House's start army all Rippers: it crosses to Forge Gate in one round.
    let shipped = "units: [(\"sentinel\", 2), (\"ripper\", 2), (\"arclight\", 2)]";
    assert!(text.contains(shipped));
    let text = text.replacen(shipped, "units: [(\"ripper\", 6)]", 1);
    let cc = CampaignContent::from_ron(&text, &content()).expect("loads");
    Campaign::new(content(), cc, 6, player).unwrap()
}
fn house(c: &Campaign) -> usize {
    c.nation_index("house").unwrap()
}
fn house_army(c: &Campaign) -> u32 {
    c.armies().iter().find(|a| a.nation == house(c)).unwrap().id
}
/// Nobody's assault succeeds, and nobody is hurt.
fn hold(p: &BattlePlan) -> BattleOutcome {
    BattleOutcome::unchanged(p, Some(Side::Defend))
}

#[test]
fn survivors_return_to_their_army_with_their_hp_and_the_dead_are_gone() {
    let mut c = campaign(&shipped_text(), Some("house"));
    let h = house(&c);
    let army = house_army(&c);
    let before = c.army(army).unwrap().units.clone();
    assert_eq!(before.len(), 6);
    // House's assault fails: its first unit dies, its second is at 1 HP,
    // its third at half; every other unit untouched. The defender loses
    // every second unit.
    let mut seen = None;
    let mut r = |p: &BattlePlan| {
        let mut o = BattleOutcome::unchanged(p, Some(Side::Defend));
        if p.attack.nation == h {
            o.attack.units[0] = 0;
            o.attack.units[1] = 1;
            o.attack.units[2] = p.attack.units[2].hp / 2;
            for (i, hp) in o.defend.units.iter_mut().enumerate() {
                if i % 2 == 1 {
                    *hp = 0;
                }
            }
            seen = Some(p.clone());
        }
        o
    };
    c.play_round(&[Decision::March { army, to: "forge_gate".into() }], &mut r).unwrap();
    let p = seen.expect("House's assault on Forge Gate (round 1)");
    let after = &c.army(army).unwrap().units;
    // The levy of Cinderhold joined after the battle (the army is home).
    let levy = c.campaign().city_levy[c.city_index("cinderhold").unwrap()].len();
    assert_eq!(after.len(), before.len() - 1 + levy, "one dead, the levy in");
    assert_eq!(after[0].def, before[1].def);
    assert_eq!(after[0].hp, 1);
    assert_eq!(after[1].hp, before[2].hp / 2);
    for (a, b) in after[2..before.len() - 1].iter().zip(&before[3..]) {
        assert_eq!(a, b, "the untouched carry over as they were");
    }
    // The defender's dead are gone from their army or garrison.
    let defenders: u32 = p.defend.units.len() as u32;
    let fg = c.city_index("forge_gate").unwrap();
    let alive_there: usize = c
        .armies()
        .iter()
        .filter(|a| a.nation == p.defend.nation && a.at() == Some(fg))
        .map(|a| a.units.len())
        .sum::<usize>()
        + c.cities()[fg].garrison.len();
    let lost = defenders / 2;
    let levy_fg = c.campaign().city_levy[fg].len() as u32;
    assert_eq!(alive_there as u32, defenders - lost + levy_fg);
}

#[test]
fn each_held_city_pays_its_alloy_levy_every_round() {
    let mut c = campaign(&shipped_text(), None);
    for n in 0..c.nations().len() {
        assert_eq!(c.nations()[n].treasury, 0);
    }
    let mut r = hold;
    for round in 1..=3u32 {
        let paid: Vec<u32> = (0..c.nations().len())
            .map(|n| {
                c.cities_of(n)
                    .iter()
                    .map(|&k| c.campaign().cities[k].levy.alloy)
                    .sum()
            })
            .collect();
        let before: Vec<u32> = c.nations().iter().map(|n| n.treasury).collect();
        c.play_round(&[], &mut r).unwrap();
        for n in 0..c.nations().len() {
            assert_eq!(
                c.nations()[n].treasury,
                before[n] + paid[n],
                "round {round}: {}'s levy",
                c.content().nations[n].id
            );
        }
    }
}

#[test]
fn levy_units_join_an_army_in_the_city_up_to_the_cap() {
    let mut c = campaign(&shipped_text(), Some("house"));
    let army = house_army(&c);
    let cap = c.campaign().rules.army_cap as usize;
    let cinder = c.city_index("cinderhold").unwrap();
    let levy = c.campaign().city_levy[cinder].clone();
    assert!(!levy.is_empty());
    let start = c.army(army).unwrap().units.len();
    let mut r = hold;
    c.play_round(&[], &mut r).unwrap();
    let a = c.army(army).unwrap();
    assert_eq!(a.units.len(), start + levy.len(), "the levy joined the army at home");
    let last = a.units.last().unwrap();
    assert_eq!(last.def, levy[levy.len() - 1]);
    assert_eq!(last.hp, c.unit_max_hp(house(&c), last.def), "at full HP");
    assert!(c.cities()[cinder].garrison.is_empty(), "nothing left over");
    for _ in 0..40 {
        if c.is_over() {
            break;
        }
        c.play_round(&[], &mut r).unwrap();
        assert!(c.army(army).unwrap().units.len() <= cap, "never above army_cap");
    }
    assert_eq!(c.army(army).unwrap().units.len(), cap, "filled to the cap");
    assert!(c.cities()[cinder].garrison.len() <= cap, "the garrison is capped too");
}

#[test]
fn a_city_without_an_army_gathers_a_garrison_that_defends_it() {
    let mut c = campaign(&shipped_text(), Some("house"));
    let fg = c.city_index("forge_gate").unwrap();
    let forge = c.nation_index("forgeborn").unwrap();
    let mut r = hold;
    c.play_round(&[], &mut r).unwrap();
    if c.armies().iter().any(|a| a.nation == forge && a.at() == Some(fg)) {
        panic!("the fixture needs Forge Gate without an army on round 1");
    }
    let levy = &c.campaign().city_levy[fg];
    assert_eq!(c.cities()[fg].garrison.len(), levy.len());
    assert_eq!(c.cities()[fg].garrison[0].def, levy[0]);
    // House marches on it: the garrison is on the defending side.
    let army = house_army(&c);
    let seen = std::cell::RefCell::new(None);
    let mut r = |p: &BattlePlan| {
        if p.city == fg {
            *seen.borrow_mut() = Some(p.clone());
        }
        BattleOutcome::unchanged(p, Some(Side::Defend))
    };
    // (Cinderhold's levy joined it: it walks at a Sentinel's pace now.)
    c.play_round(&[Decision::March { army, to: "forge_gate".into() }], &mut r).unwrap();
    for _ in 0..5 {
        if seen.borrow().is_some() {
            break;
        }
        c.play_round(&[], &mut r).unwrap();
    }
    let p = seen.into_inner().expect("the assault on Forge Gate");
    let garrison: Vec<_> = p.defend.units.iter().filter(|u| u.army.is_none()).collect();
    assert!(!garrison.is_empty(), "the garrison defends");
}

#[test]
fn a_garrison_falls_with_its_city() {
    let mut c = campaign(&shipped_text(), Some("house"));
    let fg = c.city_index("forge_gate").unwrap();
    let mut r = hold;
    c.play_round(&[], &mut r).unwrap();
    assert!(!c.cities()[fg].garrison.is_empty());
    let army = house_army(&c);
    let h = house(&c);
    let before = c.army(army).unwrap().units.len();
    let mut r = |p: &BattlePlan| {
        let w = if p.attack.nation == h { Side::Attack } else { Side::Defend };
        BattleOutcome::unchanged(p, Some(w))
    };
    c.play_round(&[Decision::March { army, to: "forge_gate".into() }], &mut r).unwrap();
    for _ in 0..5 {
        if c.cities()[fg].owner == h {
            break;
        }
        c.play_round(&[], &mut r).unwrap();
    }
    assert_eq!(c.cities()[fg].owner, h);
    // Whatever stands there now is House's own levy, taken in by its army.
    assert!(c.cities()[fg].garrison.iter().all(|u| c.unit_max_hp(h, u.def) == u.hp));
    let a = c.army(army).unwrap();
    let levy = c.campaign().city_levy[fg].len();
    assert_eq!(
        a.units.len(),
        before + levy,
        "the old garrison did not join the conqueror; the city's first levy did"
    );
}

#[test]
fn every_unit_stays_within_its_own_nations_pool() {
    // Everyone's assaults succeed, so cities change hands and nations are
    // absorbed: no unit ever carries more HP than its holder's pool.
    for seed in [1, 2, 3, 11] {
        let cc = CampaignContent::from_ron(&shipped_text(), &content()).expect("loads");
        let mut c = Campaign::new(content(), cc, seed, None).unwrap();
        let mut r = |p: &BattlePlan| BattleOutcome::unchanged(p, Some(Side::Attack));
        while !c.is_over() {
            c.play_round(&[], &mut r).unwrap();
            for a in c.armies() {
                for u in &a.units {
                    assert!(u.hp <= c.unit_max_hp(a.nation, u.def), "round {}: army {}", c.round(), a.id);
                }
            }
            for (k, city) in c.cities().iter().enumerate() {
                for u in &city.garrison {
                    assert!(u.hp <= c.unit_max_hp(city.owner, u.def), "round {}: city {k}", c.round());
                }
            }
        }
    }
}
