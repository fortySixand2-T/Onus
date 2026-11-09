//! C4 AC3 — **siege damage persists**: a city's building HP carries between
//! rounds. Unattacked cities repair at a RON rate, destroyed buildings need
//! rebuilding, and a conqueror inherits the damage.
//!
//! The readings (F-089): a city is attacked in a round when a battle (or an
//! uncontested capture) happens there. At the end of every other round each
//! standing building of the city gains `repair_per_round x` its holder's
//! repair multiplier, up to the holder's pool. A destroyed building (HP 0)
//! does not repair and does not take the field; at the end of a round its
//! city is not attacked, its holder rebuilds it if the treasury holds the
//! building's `alloy_cost`, paying it, and it stands again with one round's
//! repair. Captured buildings keep their HP (clamped to the new holder's
//! pool).

use std::path::PathBuf;

use onus::campaign::{BattleOutcome, BattlePlan, Campaign, CampaignContent, Decision, HeadlessResolver, Resolver, Side};
use onus::sim::content::Content;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data")
}
fn content() -> Content {
    Content::load_from_dir(&data_dir()).expect("content loads")
}
fn campaign() -> Campaign {
    let t = std::fs::read_to_string(data_dir().join("campaign.ron")).unwrap();
    let shipped = "units: [(\"sentinel\", 2), (\"ripper\", 2), (\"arclight\", 2)]";
    assert!(t.contains(shipped));
    let t = t.replacen(shipped, "units: [(\"ripper\", 6)]", 1);
    let cc = CampaignContent::from_ron(&t, &content()).expect("loads");
    Campaign::new(content(), cc, 6, Some("house")).unwrap()
}
fn ids(c: &Campaign) -> (usize, u32, usize) {
    let h = c.nation_index("house").unwrap();
    let army = c.armies().iter().find(|a| a.nation == h).unwrap().id;
    (h, army, c.city_index("forge_gate").unwrap())
}
/// House's assault on Forge Gate ends as `win` says, its buildings left at
/// `hp(slot, full)`; everything else is a scripted hold.
fn assault(h: usize, win: bool, hp: impl Fn(usize, u32) -> u32) -> impl FnMut(&BattlePlan) -> BattleOutcome {
    move |p: &BattlePlan| {
        if p.attack.nation != h {
            return BattleOutcome::unchanged(p, Some(Side::Defend));
        }
        let mut o = BattleOutcome::unchanged(p, Some(if win { Side::Attack } else { Side::Defend }));
        for (b, out) in p.buildings.iter().zip(o.buildings.iter_mut()) {
            *out = hp(b.slot, b.hp);
        }
        o
    }
}
fn hold(p: &BattlePlan) -> BattleOutcome {
    BattleOutcome::unchanged(p, Some(Side::Defend))
}
fn repair(c: &Campaign, owner: usize) -> u32 {
    let cc = c.campaign();
    (cc.rules.repair_per_round as u64 * cc.repair_milli[owner] as u64 / 1000) as u32
}

#[test]
fn damage_carries_and_an_unattacked_city_repairs_at_its_rate() {
    let mut c = campaign();
    let (h, army, fg) = ids(&c);
    let owner = c.cities()[fg].owner;
    let max0 = c.building_max_hp(owner, c.cities()[fg].buildings[0].def);
    let mut r = assault(h, false, |slot, full| if slot == 0 { full / 4 } else { full });
    c.play_round(&[Decision::March { army, to: "forge_gate".into() }], &mut r).unwrap();
    assert_eq!(c.cities()[fg].buildings[0].hp, max0 / 4, "attacked this round: no repair");
    let step = repair(&c, owner);
    assert!(step > 0 && max0 / 4 + 2 * step < max0, "the fixture repairs in steps");
    let mut r = hold;
    c.play_round(&[], &mut r).unwrap();
    assert_eq!(c.cities()[fg].buildings[0].hp, max0 / 4 + step, "one round's repair");
    c.play_round(&[], &mut r).unwrap();
    assert_eq!(c.cities()[fg].buildings[0].hp, max0 / 4 + 2 * step);
    for _ in 0..20 {
        c.play_round(&[], &mut r).unwrap();
    }
    assert_eq!(c.cities()[fg].buildings[0].hp, max0, "repaired to its pool, no further");
}

#[test]
fn the_repair_rate_is_the_holders_perk() {
    let c = campaign();
    let cc = c.campaign();
    let forge = c.nation_index("forgeborn").unwrap();
    let house = c.nation_index("house").unwrap();
    assert!(cc.repair_milli[forge] > 1000, "Forgeborn repairs faster (campaign.ron)");
    assert_eq!(cc.repair_milli[house], 1000);
    assert!(repair(&c, forge) > repair(&c, house));
    // House's capture of a Forgeborn city repairs at House's rate.
    let mut c = campaign();
    let (h, army, fg) = ids(&c);
    let mut r = assault(h, true, |_, full| full / 4);
    c.play_round(&[Decision::March { army, to: "forge_gate".into() }], &mut r).unwrap();
    assert_eq!(c.cities()[fg].owner, h);
    let before = c.cities()[fg].buildings[0].hp;
    let mut r = hold;
    c.play_round(&[], &mut r).unwrap();
    assert_eq!(c.cities()[fg].buildings[0].hp, before + repair(&c, h));
}

#[test]
fn the_conqueror_inherits_the_damage() {
    let mut c = campaign();
    let (h, army, fg) = ids(&c);
    let mut r = assault(h, true, |slot, full| full / (2 + slot as u32));
    c.play_round(&[Decision::March { army, to: "forge_gate".into() }], &mut r).unwrap();
    assert_eq!(c.cities()[fg].owner, h);
    let fresh = campaign();
    for (slot, b) in c.cities()[fg].buildings.iter().enumerate() {
        let was = fresh.cities()[fg].buildings[slot].hp;
        let want = (was / (2 + slot as u32)).min(c.building_max_hp(h, b.def));
        assert_eq!(b.hp, want, "slot {slot}: as the battle left it");
    }
}

#[test]
fn a_destroyed_building_stays_down_until_rebuilt_and_rebuilding_costs_alloy() {
    let mut c = campaign();
    let (h, army, fg) = ids(&c);
    // Destroy every building, and lose: Forgeborn keeps a ruined city.
    let mut r = assault(h, false, |_, _| 0);
    c.play_round(&[Decision::March { army, to: "forge_gate".into() }], &mut r).unwrap();
    let owner = c.cities()[fg].owner;
    assert_ne!(owner, h);
    assert!(c.cities()[fg].buildings.iter().all(|b| b.hp == 0), "nothing stands");
    let costs: Vec<u32> = c.cities()[fg]
        .buildings
        .iter()
        .map(|b| c.content().buildings[b.def].alloy_cost)
        .collect();
    assert!(costs.iter().all(|&k| k > 0));
    // A ruin does not repair: the round it fell, and while unpaid for.
    // Round by round the holder pays for each ruin it can afford, in slot
    // order, after its levies; a rebuilt building stands at one repair step.
    let mut r = hold;
    let mut rebuilt_any = false;
    for _ in 0..40 {
        let before: Vec<u32> = c.cities()[fg].buildings.iter().map(|b| b.hp).collect();
        let t0 = c.nations()[owner].treasury;
        let armies: Vec<u32> = c.armies().iter().map(|a| a.id).collect();
        let rep = c.play_round(&[], &mut r).unwrap();
        // The AI may also raise a General (C4 AC5): that is the one other spend.
        let raised = c.armies().iter().filter(|a| a.nation == owner && !armies.contains(&a.id)).count() as u32;
        let raise = raised * c.campaign().rules.raise_general_cost;
        assert!(rep.battles.iter().all(|b| b.city != fg), "the fixture leaves Forge Gate alone");
        assert_eq!(c.cities()[fg].owner, owner);
        let levy: u32 = c.cities_of(owner).iter().map(|&k| c.campaign().cities[k].levy.alloy).sum();
        let after: Vec<u32> = c.cities()[fg].buildings.iter().map(|b| b.hp).collect();
        let rebuilt: Vec<usize> = (0..after.len()).filter(|&k| before[k] == 0 && after[k] > 0).collect();
        let paid: u32 = rebuilt.iter().map(|&k| costs[k]).sum();
        assert_eq!(c.nations()[owner].treasury, t0 + levy - paid - raise, "rebuilding is paid for");
        for &k in &rebuilt {
            let max = c.building_max_hp(owner, c.cities()[fg].buildings[k].def);
            assert_eq!(after[k], repair(&c, owner).min(max), "rebuilt at one repair step");
        }
        for k in 0..after.len() {
            if after[k] == 0 {
                assert!(c.nations()[owner].treasury < costs[k], "slot {k}: a ruin is rebuilt once its holder can pay");
            }
        }
        rebuilt_any |= !rebuilt.is_empty();
        if after.iter().all(|&hp| hp > 0) {
            break;
        }
    }
    assert!(rebuilt_any, "the city was rebuilt");
    assert!(c.cities()[fg].buildings.iter().all(|b| b.hp > 0), "all of it");
}

#[test]
fn a_ruin_does_not_take_the_field() {
    let mut c = campaign();
    let (h, army, fg) = ids(&c);
    let mut r = assault(h, false, |slot, full| if slot == 0 { 0 } else { full });
    c.play_round(&[Decision::March { army, to: "forge_gate".into() }], &mut r).unwrap();
    assert_eq!(c.cities()[fg].buildings[0].hp, 0);
    // House assaults again; the field is exactly what stood when it came.
    let cinderhold = c.city_index("cinderhold").unwrap();
    for _ in 0..6 {
        let standing: Vec<usize> = (0..c.cities()[fg].buildings.len())
            .filter(|&k| c.cities()[fg].buildings[k].hp > 0)
            .collect();
        let mut seen = None;
        let mut r = |p: &BattlePlan| {
            if p.attack.nation == h && p.city == fg {
                seen = Some(p.clone());
            }
            BattleOutcome::unchanged(p, Some(Side::Defend))
        };
        let ds = if c.army(army).unwrap().at() == Some(cinderhold) {
            vec![Decision::March { army, to: "forge_gate".into() }]
        } else {
            vec![]
        };
        c.play_round(&ds, &mut r).unwrap();
        if let Some(p) = seen {
            let fielded: Vec<usize> = p.buildings.iter().map(|b| b.slot).collect();
            assert_eq!(fielded, standing, "only standing buildings take the field");
            assert!(!fielded.contains(&0), "the ruin was still down: the fixture checks something");
            return;
        }
    }
    panic!("no second assault");
}

/// The invariant behind "perks stay their own" (critic C4 rounds 1 and 2):
/// after every round, every building and every garrison unit is within its
/// city's holder's own pool, and every army unit within its army's nation's,
/// whichever way the city changed hands.
fn within_pools(c: &Campaign, what: &str) {
    let r = c.round();
    for (k, city) in c.cities().iter().enumerate() {
        for b in &city.buildings {
            let max = c.building_max_hp(city.owner, b.def);
            assert!(b.hp <= max, "{what}, round {r}: city {k} holds a building at {} HP over its holder's pool {max}", b.hp);
        }
        for u in &city.garrison {
            let max = c.unit_max_hp(city.owner, u.def);
            assert!(u.hp <= max, "{what}, round {r}: city {k}'s garrison holds a unit at {} HP over its holder's pool {max}", u.hp);
        }
    }
    for a in c.armies() {
        for u in &a.units {
            let max = c.unit_max_hp(a.nation, u.def);
            assert!(u.hp <= max, "{what}, round {r}: army {} holds a unit at {} HP over its nation's pool {max}", a.id, u.hp);
        }
    }
}

#[test]
fn every_building_and_unit_stays_within_its_holders_pool_every_round() {
    let content = content();
    let cc = CampaignContent::load_from_dir(&data_dir(), &content).unwrap();
    let by_numbers = |p: &BattlePlan| {
        let w = if p.attack.units.len() > p.defend.units.len() { Side::Attack } else { Side::Defend };
        BattleOutcome::unchanged(p, Some(w))
    };
    for seed in 1..=3u64 {
        for real in [false, true] {
            let mut c = Campaign::new(content.clone(), cc.clone(), seed, None).unwrap();
            let mut scripted = by_numbers;
            let mut headless = HeadlessResolver;
            let r: &mut dyn Resolver = if real { &mut headless } else { &mut scripted };
            while !c.is_over() {
                c.play_round(&[], r).unwrap();
                within_pools(&c, &format!("seed {seed}, headless {real}"));
            }
        }
    }
}

/// The shipped content with Ashen Reach's Sentinels made sturdier
/// (Defense +1, a unit-HP perk; its BuildingHp x1.30 is already shipped).
fn hardy_reach() -> Content {
    let dir = std::env::temp_dir().join(format!("onus_c4_siege_hardy_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for e in std::fs::read_dir(data_dir()).unwrap() {
        let e = e.unwrap();
        if e.file_type().unwrap().is_file() {
            std::fs::copy(e.path(), dir.join(e.file_name())).unwrap();
        }
    }
    let n = std::fs::read_to_string(data_dir().join("nations.ron")).unwrap();
    let perk = "(applies_to: Buildings, stat: BuildingHp, mul: 1.30),";
    let (head, tail) = n.split_at(n.find("id: \"ashen_reach\"").unwrap());
    assert!(tail.contains(perk));
    let tail = tail.replace(perk, &format!("{perk} (applies_to: Unit(\"sentinel\"), stat: Defense, add: 1),"));
    std::fs::write(dir.join("nations.ron"), format!("{head}{tail}")).unwrap();
    let c = Content::load_from_dir(&dir).unwrap_or_else(|e| panic!("{e}"));
    let _ = std::fs::remove_dir_all(&dir);
    c
}

/// The invariant, where it bites: the Reach's outlying city X is garrisoned
/// (its levy) and assaulted, and held, in the very round House takes the
/// Reach's capital, so X passes to House by absorption with the Reach's
/// sturdier HQ and Sentinels in it, and no repair round re-bases them.
#[test]
fn an_attacked_garrisoned_city_absorbed_by_a_conqueror_takes_his_pools() {
    let content = hardy_reach();
    let t = std::fs::read_to_string(data_dir().join("campaign.ron")).unwrap();
    let head = &t[..t.find("    cities: [").unwrap()];
    let tail = &t[t.find("    ranks: [").unwrap()..];
    let cap = |id: &str, n: &str| {
        format!("(id: \"{id}\", name: \"{id}\", owner: \"{n}\", capital: true, buildings: [\"hq\"], levy: (alloy: 0, units: [])),")
    };
    let cities = format!(
        "(id: \"c_x\", name: \"X\", owner: \"ashen_reach\", buildings: [\"hq\"], levy: (alloy: 0, units: [\"sentinel\", \"sentinel\"])),\n{}{}{}{}{}{}",
        cap("home", "house"),
        cap("ash_cap", "ashen_reach"),
        cap("forge_cap", "forgeborn"),
        cap("verd_cap", "verdant"),
        cap("aeth_cap", "aetherkin"),
        cap("toll_cap", "tollmarch"),
    );
    let roads = r#"
        (from: "home", to: "ash_cap", length: 1, terrain: "plain"),
        (from: "forge_cap", to: "c_x", length: 1, terrain: "plain"),
        (from: "c_x", to: "home", length: 50, terrain: "plain"),
        (from: "home", to: "verd_cap", length: 90, terrain: "plain"),
        (from: "home", to: "aeth_cap", length: 90, terrain: "plain"),
        (from: "home", to: "toll_cap", length: 90, terrain: "plain"),"#;
    let armies = r#"
        (nation: "house", general: "general_synthesis", at: "home", units: [("sentinel", 5)]),
        (nation: "forgeborn", general: "general_bulwark", at: "forge_cap", units: [("sentinel", 5)]),"#;
    let text = format!("{head}    cities: [{cities}],\n    roads: [{roads}],\n    start_armies: [{armies}],\n{tail}");
    let cc = CampaignContent::from_ron(&text, &content).unwrap_or_else(|e| panic!("{e}"));
    let mut c = Campaign::new(content, cc, 3, Some("house")).unwrap();
    let (h, a, f) = (c.nation_index("house").unwrap(), c.nation_index("ashen_reach").unwrap(), c.nation_index("forgeborn").unwrap());
    let x = c.city_index("c_x").unwrap();
    let army = c.armies().iter().find(|x| x.nation == h).unwrap().id;
    // House takes what it attacks; everyone else's assaults are held.
    let mut r = |p: &BattlePlan| {
        let w = if p.attack.nation == h { Side::Attack } else { Side::Defend };
        BattleOutcome::unchanged(p, Some(w))
    };
    c.play_round(&[], &mut r).unwrap();
    within_pools(&c, "round 1");
    let g = c.cities()[x].garrison.clone();
    assert!(!g.is_empty(), "premise: the Reach's levy garrisons X");
    assert!(g.iter().all(|u| u.hp == c.unit_max_hp(a, u.def) && u.hp > c.unit_max_hp(h, u.def)), "premise: Reach-sturdy Sentinels");
    let hq = c.cities()[x].buildings[0];
    assert!(hq.hp > c.building_max_hp(h, hq.def), "premise: a Reach-sturdy HQ");
    let rep = c.play_round(&[Decision::March { army, to: "ash_cap".into() }], &mut r).unwrap();
    assert!(rep.battles.iter().any(|b| b.city == x && b.attacker == f && !b.captured), "premise: X was assaulted and held this round");
    assert_eq!(c.cities()[x].owner, h, "premise: X passed to House with the Reach's capital");
    assert!(!c.cities()[x].garrison.is_empty(), "premise: the garrison passed too");
    within_pools(&c, "round 2, X absorbed");
    for u in &c.cities()[x].garrison {
        assert_eq!(u.hp, c.unit_max_hp(h, u.def), "an unhurt Sentinel stands at House's full pool");
    }
    // And the next assault on X is a battle the content accepts.
    let mut real = HeadlessResolver;
    for _ in 0..3 {
        if c.is_over() {
            break;
        }
        c.play_round(&[], &mut real).unwrap();
        within_pools(&c, "after X was absorbed");
    }
}
