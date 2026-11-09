//! C4 AC5 — **raised Generals**: a nation raises an extra General of its own
//! house at one of its cities, for `raise_general_cost` treasury Alloy.
//! Nation perks act only on the nation's own armies and cities.
//!
//! The readings (F-089): `Decision::Raise { city }` at a city the nation
//! holds pays the cost and stands up a new, empty army there, led by the
//! nation's own house General at rank 0; it fills from the levy. The AI
//! raises one at its capital when it can pay and has fewer than
//! `ai.max_armies` armies.

use std::path::PathBuf;

use onus::campaign::{
    battle_settings, BattleOutcome, BattlePlan, Campaign, CampaignContent, Decision, Location, RoundError, Side,
};
use onus::sim::content::Content;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data")
}
fn content() -> Content {
    Content::load_from_dir(&data_dir()).expect("content loads")
}
fn shipped() -> CampaignContent {
    CampaignContent::load_from_dir(&data_dir(), &content()).expect("campaign loads")
}
fn hold(p: &BattlePlan) -> BattleOutcome {
    BattleOutcome::unchanged(p, Some(Side::Defend))
}
/// House, saved up to the raise cost.
fn saved_up() -> Campaign {
    let mut c = Campaign::new(content(), shipped(), 4, Some("house")).unwrap();
    let h = c.nation_index("house").unwrap();
    let cost = c.campaign().rules.raise_general_cost;
    assert!(cost > 0);
    let mut r = hold;
    while c.nations()[h].treasury < cost {
        c.play_round(&[], &mut r).unwrap();
    }
    c
}

#[test]
fn a_nation_raises_a_general_of_its_own_house_for_alloy() {
    let mut c = saved_up();
    let h = c.nation_index("house").unwrap();
    let cost = c.campaign().rules.raise_general_cost;
    let home = c.city_index("cinderhold").unwrap();
    let before: Vec<u32> = c.armies().iter().map(|a| a.id).collect();
    let t0 = c.nations()[h].treasury;
    let levy: u32 = c.cities_of(h).iter().map(|&k| c.campaign().cities[k].levy.alloy).sum();
    let mut r = hold;
    let rep = c.play_round(&[Decision::Raise { city: "cinderhold".into() }], &mut r).unwrap();
    assert_eq!(c.nations()[h].treasury, t0 - cost + levy, "the raise is paid from the treasury");
    let new: Vec<_> = c.armies().iter().filter(|a| !before.contains(&a.id)).collect();
    let mine: Vec<_> = new.iter().filter(|a| a.nation == h).collect();
    assert_eq!(mine.len(), 1);
    let a = mine[0];
    assert_eq!(a.general.kind, c.campaign().nation_general[h], "his own house's General");
    // Rank 0; he may have held Cinderhold in this very round's battle.
    let fought = rep.battles.iter().any(|b| b.city == home);
    let xp = if fought { c.campaign().rules.xp.win } else { 0 };
    assert_eq!((a.general.xp, a.general.vacant_until), (xp, None));
    assert_eq!(a.location, Location::At(home));
    for u in &a.units {
        assert_eq!(u.hp, c.unit_max_hp(h, u.def));
    }
}

#[test]
fn a_raise_is_refused_without_the_alloy_or_the_city() {
    let fresh = Campaign::new(content(), shipped(), 4, Some("house")).unwrap();
    let h = fresh.nation_index("house").unwrap();
    assert!(fresh.nations()[h].treasury < fresh.campaign().rules.raise_general_cost);
    let mut c = fresh.clone();
    let mut r = hold;
    let e = c.play_round(&[Decision::Raise { city: "cinderhold".into() }], &mut r).unwrap_err();
    assert!(matches!(e, RoundError::BadDecision { .. }), "{e}");
    assert_eq!(c.state_hash(), fresh.state_hash(), "a refused round changes nothing");
    let mut c = saved_up();
    let snap = c.state_hash();
    for city in ["ironspire", "nowhere"] {
        let e = c.play_round(&[Decision::Raise { city: city.into() }], &mut r).unwrap_err();
        assert!(matches!(e, RoundError::BadDecision { .. }), "{city}: {e}");
        assert_eq!(c.state_hash(), snap);
    }
}

#[test]
fn the_ai_raises_at_its_capital_when_it_can_pay_and_has_room() {
    let mut c = Campaign::new(content(), shipped(), 9, None).unwrap();
    let cost = c.campaign().rules.raise_general_cost;
    let max = c.campaign().ai.max_armies as usize;
    let mut r = hold;
    let mut raised = 0;
    for _ in 0..20 {
        if c.is_over() {
            break;
        }
        let before = c.clone();
        c.play_round(&[], &mut r).unwrap();
        for a in c.armies().iter().filter(|a| before.army(a.id).is_none()) {
            let n = a.nation;
            let had = before.armies().iter().filter(|b| b.nation == n).count();
            assert!(had < max, "{n} raised with {had} armies");
            assert!(before.nations()[n].treasury >= cost, "{n} raised what it could not pay for");
            assert_eq!(a.general.kind, c.campaign().nation_general[n]);
            assert_eq!(a.location, Location::At(c.campaign().capital[n]), "raised at its capital");
            raised += 1;
        }
    }
    assert!(raised > 0, "twenty rounds of AI campaign raised a General");
}

#[test]
fn perks_act_only_on_the_nations_own_armies_and_cities() {
    // A captured Forgeborn city holds House's pool, not the Forgeborn's
    // sturdier one (BuildingHp is a three-resource trait).
    let t = std::fs::read_to_string(data_dir().join("campaign.ron")).unwrap();
    assert!(t.contains("economy: Alloy,"));
    let t = t.replacen("economy: Alloy,", "economy: ThreeResource,", 1);
    let cc = CampaignContent::from_ron(&t, &content()).expect("loads");
    let mut c = Campaign::new(content(), cc, 4, Some("house")).unwrap();
    let (h, forge) = (c.nation_index("house").unwrap(), c.nation_index("forgeborn").unwrap());
    let fg = c.city_index("forge_gate").unwrap();
    let hq = c.cities()[fg].buildings[0].def;
    assert!(c.building_max_hp(forge, hq) > c.building_max_hp(h, hq), "BuildingHp is a Forgeborn perk");
    let army = c.armies().iter().find(|a| a.nation == h).unwrap().id;
    let mut r = |p: &BattlePlan| {
        let w = if p.attack.nation == h { Side::Attack } else { Side::Defend };
        BattleOutcome::unchanged(p, Some(w))
    };
    c.play_round(&[Decision::March { army, to: "forge_gate".into() }], &mut r).unwrap();
    for _ in 0..10 {
        if c.cities()[fg].owner == h {
            break;
        }
        c.play_round(&[], &mut r).unwrap();
    }
    assert_eq!(c.cities()[fg].owner, h);
    assert_eq!(c.cities()[fg].buildings[0].hp, c.building_max_hp(h, hq), "House's pool");
    // A raised army is its nation's: it fights as that nation.
    let mut c = saved_up();
    let before: Vec<u32> = c.armies().iter().map(|a| a.id).collect();
    let mut r = hold;
    c.play_round(&[Decision::Raise { city: "cinderhold".into() }], &mut r).unwrap();
    let raised = c.armies().iter().find(|a| !before.contains(&a.id) && a.nation == h).unwrap().id;
    let mut seen = None;
    let mut r = |p: &BattlePlan| {
        if p.attack.armies.contains(&raised) {
            seen.get_or_insert_with(|| p.clone());
        }
        BattleOutcome::unchanged(p, Some(Side::Defend))
    };
    c.play_round(&[Decision::March { army: raised, to: "rootfall".into() }], &mut r).unwrap();
    for _ in 0..10 {
        if c.army(raised).unwrap().at().is_some() {
            break;
        }
        c.play_round(&[], &mut r).unwrap();
    }
    let p = seen.expect("the raised army assaulted Rootfall");
    let s = battle_settings(c.content(), c.campaign(), &p);
    assert_eq!(s.nations[0].as_deref(), Some("house"));
}
