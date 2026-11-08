//! C4 AC7 — **converging armies**: every army that reaches a city in the
//! round its battle starts fights in it, so a battle can have several
//! Generals per side. Auras of different kinds stack; auras of the same kind
//! do not.

use std::path::PathBuf;

use bevy::prelude::*;

use onus::campaign::{BattleOutcome, BattlePlan, Campaign, CampaignContent, Decision, Location, Side};
use onus::headless::{self, MatchSettings};
use onus::sim::battle::{AuraField, BattleSetup, General};
use onus::sim::combat::Health;
use onus::sim::content::Content;
use onus::sim::economy::UnitDefIdx;
use onus::sim::spatial::Faction;
use onus::sim::{AiCommanders, Position};

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data")
}
fn content() -> Content {
    Content::load_from_dir(&data_dir()).expect("content loads")
}
/// The shipped campaign with House fielding a second army at Cinderhold
/// (`second`'s units, led by `general`).
fn two_house_armies(first: &str, second: &str, general: &str) -> Campaign {
    let t = std::fs::read_to_string(data_dir().join("campaign.ron")).unwrap();
    let shipped = "(nation: \"house\",       general: \"general_synthesis\", at: \"cinderhold\",\n         units: [(\"sentinel\", 2), (\"ripper\", 2), (\"arclight\", 2)]),";
    assert!(t.contains(shipped));
    let t = t.replacen(
        shipped,
        &format!(
            "(nation: \"house\", general: \"general_synthesis\", at: \"cinderhold\", units: [{first}]),\n\
             (nation: \"house\", general: \"{general}\", at: \"cinderhold\", units: [{second}]),"
        ),
        1,
    );
    let cc = CampaignContent::from_ron(&t, &content()).expect("loads");
    Campaign::new(content(), cc, 4, Some("house")).unwrap()
}
fn house_armies(c: &Campaign) -> Vec<u32> {
    let h = c.nation_index("house").unwrap();
    c.armies().iter().filter(|a| a.nation == h).map(|a| a.id).collect()
}
/// Records every plan, and lets the attacker win or lose as told.
fn recorder(win: bool, log: &mut Vec<BattlePlan>) -> impl FnMut(&BattlePlan) -> BattleOutcome + '_ {
    move |p: &BattlePlan| {
        log.push(p.clone());
        BattleOutcome::unchanged(p, Some(if win { Side::Attack } else { Side::Defend }))
    }
}

#[test]
fn armies_arriving_together_fight_one_battle_with_every_general() {
    let mut c = two_house_armies("(\"ripper\", 5)", "(\"ripper\", 3)", "general_focus");
    let [a, b] = house_armies(&c)[..] else { panic!("two House armies") };
    let house = c.nation_index("house").unwrap();
    let mut plans = Vec::new();
    {
        let mut r = recorder(true, &mut plans);
        c.play_round(
            &[
                Decision::March { army: a, to: "forge_gate".into() },
                Decision::March { army: b, to: "forge_gate".into() },
            ],
            &mut r,
        )
        .unwrap();
    }
    let p = plans
        .iter()
        .find(|p| p.attack.nation == house)
        .expect("House's assault on round 1 (a Ripper army crosses in one round)");
    assert_eq!(p.attack.armies, vec![a, b], "both armies fight");
    assert_eq!(p.attack.generals.len(), 2, "two Generals on the attacking side");
    let kinds: Vec<usize> = p.attack.generals.iter().map(|g| g.kind).collect();
    let cn = c.content();
    assert_eq!(
        kinds,
        vec![
            cn.general_index("general_synthesis").unwrap(),
            cn.general_index("general_focus").unwrap()
        ]
    );
    assert_eq!(p.attack.units.len(), 8, "every unit of both armies");
    assert_eq!(p.attack.units.iter().filter(|u| u.army == a).count(), 5);
    assert_eq!(p.attack.units.iter().filter(|u| u.army == b).count(), 3);
    // The city fell to both, and both stand in it.
    let fg = c.city_index("forge_gate").unwrap();
    for id in [a, b] {
        assert_eq!(c.army(id).unwrap().location, Location::At(fg));
    }
}

#[test]
fn an_army_arriving_later_joins_the_next_assault_not_the_first() {
    let mut c = two_house_armies("(\"ripper\", 5)", "(\"bulwark\", 3)", "general_bulwark");
    let [fast, slow] = house_armies(&c)[..] else { panic!("two House armies") };
    let house = c.nation_index("house").unwrap();
    let mut plans = Vec::new();
    {
        // Every House assault fails, so the city is still there to assault.
        let mut r = recorder(false, &mut plans);
        c.play_round(
            &[
                Decision::March { army: fast, to: "forge_gate".into() },
                Decision::March { army: slow, to: "forge_gate".into() },
            ],
            &mut r,
        )
        .unwrap();
        for _ in 0..10 {
            c.play_round(&[], &mut r).unwrap();
        }
    }
    let ours: Vec<&BattlePlan> = plans.iter().filter(|p| p.attack.nation == house).collect();
    assert!(ours.len() >= 2, "{} House assaults", ours.len());
    assert_eq!(ours[0].attack.armies, vec![fast], "the fast army fights alone, first");
    assert_eq!(ours[0].round, 1);
    assert_eq!(ours[1].attack.armies, vec![slow], "the slow one fights when it arrives");
    assert!(ours[1].round > 1);
}

#[test]
fn every_defending_army_in_the_city_defends_it() {
    // Two Generals with no units: the weakest city on the map draws the AI.
    let mut c = two_house_armies("", "", "general_focus");
    let house = c.nation_index("house").unwrap();
    let ours = house_armies(&c);
    let mut plans = Vec::new();
    {
        // Every assault succeeds: the AI rolls over the map and reaches us.
        let mut r = recorder(true, &mut plans);
        while !c.is_over() {
            c.play_round(&[], &mut r).unwrap();
        }
    }
    let cinder = c.city_index("cinderhold").unwrap();
    // The assault on Cinderhold met both House armies (they never left).
    let on_us: Vec<&BattlePlan> = plans.iter().filter(|p| p.city == cinder).collect();
    assert!(!on_us.is_empty(), "Cinderhold borders four nations: someone came");
    for p in on_us {
        assert_eq!(p.defend.nation, house);
        assert_eq!(p.defend.armies, ours);
        assert_eq!(p.defend.generals.len(), 2);
    }
}

// ---- the battle layer: auras of several Generals ---------------------------

const HERE: Vec2 = Vec2::new(0.0, 0.0);

fn bonus_with(a: &[&str]) -> onus::sim::combat::StatBonus {
    let mut setup = BattleSetup::default().with_general(Faction::B, "general_bulwark");
    for id in a {
        setup = setup.with_general(Faction::A, id);
    }
    let mut app = headless::ai_vs_ai(content(), &MatchSettings::default().with_battle(setup))
        .expect("battle builds");
    app.insert_resource(AiCommanders::default());
    let mut q = app.world_mut().query_filtered::<(Entity, &Faction), With<General>>();
    let gs: Vec<Entity> = q
        .iter(app.world())
        .filter(|(_, f)| **f == Faction::A)
        .map(|(e, _)| e)
        .collect();
    for g in gs {
        app.world_mut().get_mut::<Position>(g).unwrap().0 = HERE;
    }
    let (idx, kind, hp) = {
        let c = app.world().resource::<Content>();
        let idx = c.unit_index("sentinel").unwrap();
        (idx, c.units[idx].mvp_kind, Health::from_def(c, idx))
    };
    let u = app
        .world_mut()
        .spawn((Position(HERE + Vec2::new(10.0, 0.0)), UnitDefIdx(idx), kind, Faction::A, hp))
        .id();
    headless::step(&mut app);
    app.world().resource::<AuraField>().get(u)
}

#[test]
fn different_kinds_stack_and_the_same_kind_does_not() {
    let focus = bonus_with(&["general_focus"]);
    let bulwark = bonus_with(&["general_bulwark"]);
    let both = bonus_with(&["general_focus", "general_bulwark"]);
    assert!(focus.offense > 0 && bulwark.defense > 0);
    assert_eq!(both.offense, focus.offense + bulwark.offense, "kinds stack");
    assert_eq!(both.defense, focus.defense + bulwark.defense, "kinds stack");
    let twice = bonus_with(&["general_focus", "general_focus"]);
    assert_eq!(twice, focus, "two of one kind give one kind's bonus");
}
