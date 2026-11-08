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

use onus::campaign::{BattleOutcome, BattlePlan, Campaign, CampaignContent, Decision, Side};
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
        let rep = c.play_round(&[], &mut r).unwrap();
        assert!(rep.battles.iter().all(|b| b.city != fg), "the fixture leaves Forge Gate alone");
        assert_eq!(c.cities()[fg].owner, owner);
        let levy: u32 = c.cities_of(owner).iter().map(|&k| c.campaign().cities[k].levy.alloy).sum();
        let after: Vec<u32> = c.cities()[fg].buildings.iter().map(|b| b.hp).collect();
        let rebuilt: Vec<usize> = (0..after.len()).filter(|&k| before[k] == 0 && after[k] > 0).collect();
        let paid: u32 = rebuilt.iter().map(|&k| costs[k]).sum();
        assert_eq!(c.nations()[owner].treasury, t0 + levy - paid, "rebuilding is paid for, and only that");
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
