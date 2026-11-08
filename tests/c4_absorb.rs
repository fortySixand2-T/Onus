//! C4 AC4 — **absorbed Generals**: taking a capital gives the conqueror a
//! new General with the fallen nation's aura, leading a new army, up to
//! `max_absorbed_generals` of them. No trait transfers.
//!
//! The readings (F-089): the absorbed General is the fallen nation's own
//! house General type, at rank 0. He leads a new, empty army of the
//! conqueror standing at the captured capital; it fills from the levy like
//! any other. The army is the conqueror's, so every trait it plays is the
//! conqueror's. The cap counts absorptions over the whole campaign.

use std::path::PathBuf;

use onus::campaign::{battle_settings, BattleOutcome, BattlePlan, Campaign, CampaignContent, Decision, Location, Resolver, Side};
use onus::sim::content::Content;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data")
}
fn content() -> Content {
    Content::load_from_dir(&data_dir()).expect("content loads")
}
fn campaign_with(cap: u32) -> Campaign {
    let t = std::fs::read_to_string(data_dir().join("campaign.ron")).unwrap();
    let shipped = "max_absorbed_generals: 3,";
    assert!(t.contains(shipped));
    let t = t.replacen(shipped, &format!("max_absorbed_generals: {cap},"), 1);
    let cc = CampaignContent::from_ron(&t, &content()).expect("loads");
    Campaign::new(content(), cc, 5, Some("house")).unwrap()
}
/// House's assaults always succeed; everyone else's fail.
fn favour(n: usize) -> impl FnMut(&BattlePlan) -> BattleOutcome {
    move |p: &BattlePlan| {
        let w = if p.attack.nation == n { Side::Attack } else { Side::Defend };
        BattleOutcome::unchanged(p, Some(w))
    }
}
/// Walk House's army `army` to `to`, a road at a time (the next hop is the
/// neighbour nearest `to`), taking whatever is on the way.
fn walk(c: &mut Campaign, r: &mut dyn Resolver, army: u32, to: &str) {
    let target = c.city_index(to).unwrap();
    let dist = c.distances(target);
    for _ in 0..60 {
        let at = c.army(army).unwrap().at();
        if at == Some(target) {
            return;
        }
        let ds = match at {
            Some(here) => {
                let next = c
                    .campaign()
                    .roads_of(here)
                    .map(|(_, road)| road.other(here))
                    .min_by_key(|&n| (dist[n], n))
                    .unwrap();
                vec![Decision::March { army, to: c.campaign().cities[next].id.clone() }]
            }
            None => vec![],
        };
        c.play_round(&ds, r).unwrap();
    }
    panic!("army {army} never reached {to}");
}
fn house_army(c: &Campaign) -> u32 {
    let h = c.nation_index("house").unwrap();
    c.armies().iter().find(|a| a.nation == h).unwrap().id
}

#[test]
fn taking_a_capital_gives_the_conqueror_its_general_leading_a_new_army() {
    let mut c = campaign_with(3);
    let (h, forge) = (c.nation_index("house").unwrap(), c.nation_index("forgeborn").unwrap());
    let army = house_army(&c);
    let before: Vec<u32> = c.armies().iter().map(|a| a.id).collect();
    let mut r = favour(h);
    walk(&mut c, &mut r, army, "ironspire");
    assert!(!c.nations()[forge].alive);
    assert_eq!(c.nations()[h].absorbed, 1);
    let ironspire = c.city_index("ironspire").unwrap();
    let kind = c.campaign().nation_general[forge];
    let new: Vec<_> = c.armies().iter().filter(|a| !before.contains(&a.id)).collect();
    assert_eq!(new.len(), 1, "one new army: {new:?}");
    let a = new[0];
    assert_eq!(a.nation, h, "the conqueror's army");
    assert_eq!(a.general.kind, kind, "led by the Forgeborn's own General (his aura)");
    assert_ne!(kind, c.campaign().nation_general[h]);
    assert_eq!((a.general.xp, a.general.vacant_until), (0, None), "rank 0, in command");
    assert_eq!(a.location, Location::At(ironspire), "raised at the captured capital");
    for u in &a.units {
        assert_eq!(u.hp, c.unit_max_hp(h, u.def), "its units are the conqueror's");
    }
}

#[test]
fn no_trait_transfers_the_absorbed_army_fights_as_the_conqueror() {
    let mut c = campaign_with(3);
    let (h, forge) = (c.nation_index("house").unwrap(), c.nation_index("forgeborn").unwrap());
    let army = house_army(&c);
    let mut r = favour(h);
    walk(&mut c, &mut r, army, "ironspire");
    let kind = c.campaign().nation_general[forge];
    let absorbed = c.armies().iter().find(|a| a.general.kind == kind).unwrap().id;
    // March the absorbed army on Tollmarch and look at its battle.
    let mut seen = None;
    let mut r = |p: &BattlePlan| {
        if p.attack.armies.contains(&absorbed) {
            seen.get_or_insert_with(|| p.clone());
        }
        let w = if p.attack.nation == h { Side::Attack } else { Side::Defend };
        BattleOutcome::unchanged(p, Some(w))
    };
    walk(&mut c, &mut r, absorbed, "silvermarket");
    let p = seen.expect("the absorbed army fought");
    assert_eq!(p.attack.nation, h, "it fights for House");
    assert!(p.attack.generals.iter().any(|g| g.army == absorbed && g.kind == kind), "with his aura");
    let s = battle_settings(c.content(), c.campaign(), &p);
    assert_eq!(s.nations[0].as_deref(), Some("house"), "House's traits, never the Forgeborn's");
}

#[test]
fn absorbed_generals_stop_at_the_cap() {
    for cap in [0u32, 1, 3] {
        let mut c = campaign_with(cap);
        let h = c.nation_index("house").unwrap();
        let (forge, verdant) = (c.nation_index("forgeborn").unwrap(), c.nation_index("verdant").unwrap());
        let army = house_army(&c);
        let mut r = favour(h);
        walk(&mut c, &mut r, army, "ironspire");
        walk(&mut c, &mut r, army, "greenmere");
        assert!(!c.nations()[forge].alive && !c.nations()[verdant].alive, "two capitals taken");
        let gen = &c.campaign().nation_general;
        let held = |k: usize| c.armies().iter().filter(|a| a.nation == h && a.general.kind == gen[k]).count();
        let want = cap.min(2);
        assert_eq!(c.nations()[h].absorbed, want, "cap {cap}");
        assert_eq!(held(forge), (want >= 1) as usize, "cap {cap}: the Forgeborn General");
        assert_eq!(held(verdant), (want >= 2) as usize, "cap {cap}: the Verdant General");
    }
}
