//! C4 AC6 — **army movement**: each round an army gets march points equal to
//! its slowest unit's Speed (plus the Logistics bonus). Roads have RON
//! lengths and terrain multipliers, and partial progress carries over.
//!
//! Every expected number is read from the loaded content.

use std::path::PathBuf;

use onus::campaign::{BattleOutcome, BattlePlan, Campaign, CampaignContent, Decision, Location, Side};
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
/// The shipped campaign with House's start army replaced by `units`.
fn with_house_army(units: &str) -> Campaign {
    let shipped = "units: [(\"sentinel\", 2), (\"ripper\", 2), (\"arclight\", 2)]";
    let t = shipped_text();
    assert!(t.contains(shipped));
    let t = t.replacen(shipped, &format!("units: [{units}]"), 1);
    let cc = CampaignContent::from_ron(&t, &content()).expect("loads");
    Campaign::new(content(), cc, 9, Some("house")).unwrap()
}
fn house_army(c: &Campaign) -> u32 {
    let h = c.nation_index("house").unwrap();
    c.armies().iter().find(|a| a.nation == h).unwrap().id
}
/// House's assaults succeed; everyone else's fail.
fn favour_house(c: &Campaign) -> impl FnMut(&BattlePlan) -> BattleOutcome {
    let h = c.nation_index("house").unwrap();
    move |p: &BattlePlan| {
        let w = if p.attack.nation == h { Side::Attack } else { Side::Defend };
        BattleOutcome::unchanged(p, Some(w))
    }
}
fn speed(id: &str) -> u32 {
    content().unit(id).unwrap().speed
}
fn general_speed(id: &str) -> u32 {
    let c = content();
    c.unit_def(c.general_index(id).unwrap()).unwrap().speed
}
/// The road cost from `a` to `b`, from the RON: ceil(length x terrain mult).
fn cost(c: &Campaign, a: &str, b: &str) -> u32 {
    let (a, b) = (c.city_index(a).unwrap(), c.city_index(b).unwrap());
    c.campaign().roads[c.campaign().road_between(a, b).unwrap()].cost
}
/// March House's army to `to`; the rounds it takes to arrive.
fn rounds_to(c: &mut Campaign, to: &str) -> u32 {
    let army = house_army(c);
    let target = c.city_index(to).unwrap();
    let mut r = favour_house(c);
    c.play_round(&[Decision::March { army, to: to.into() }], &mut r).unwrap();
    let mut n = 1;
    while c.army(army).unwrap().location != Location::At(target) {
        c.play_round(&[], &mut r).unwrap();
        n += 1;
        assert!(n < 50, "never arrived");
    }
    n
}

#[test]
fn a_road_costs_its_length_times_its_terrain_multiplier_rounded_up() {
    let c = with_house_army("(\"ripper\", 5)");
    let cc = c.campaign();
    for (def, road) in cc.roads_def.iter().zip(&cc.roads) {
        let mult = cc.terrain.iter().find(|t| t.id == def.terrain).unwrap().length_mult;
        let want = (def.length as f64 * mult as f64 - 1e-9).ceil() as u32;
        assert_eq!(road.cost, want, "{}-{} ({})", def.from, def.to, def.terrain);
    }
    // The shipped map uses every terrain, and the scar's edge costs more.
    assert!(cost(&c, "cinderhold", "starfall") > cost(&c, "cinderhold", "forge_gate"));
}

#[test]
fn march_points_are_the_slowest_units_speed() {
    for (units, slowest) in [
        ("(\"ripper\", 5)", speed("ripper")),
        ("(\"bulwark\", 5)", speed("bulwark")),
        ("(\"ripper\", 4), (\"bulwark\", 1)", speed("bulwark")),
        ("(\"sentinel\", 3), (\"arclight\", 2)", speed("arclight")),
    ] {
        let c = with_house_army(units);
        assert_eq!(c.march_points(house_army(&c)), slowest, "{units}");
    }
    // An army of no units marches at its General's pace.
    let c = with_house_army("");
    assert_eq!(c.march_points(house_army(&c)), general_speed("general_synthesis"));
}

#[test]
fn the_logistics_general_adds_his_bonus() {
    let c = with_house_army("(\"ripper\", 5)");
    let toll = c.nation_index("tollmarch").unwrap();
    let a = c.armies().iter().find(|a| a.nation == toll).unwrap();
    assert_eq!(a.general.kind, c.campaign().logistics_general);
    let slowest = a
        .units
        .iter()
        .map(|u| c.content().units[u.def].speed)
        .min()
        .unwrap();
    assert_eq!(c.march_points(a.id), slowest + c.campaign().rules.march.logistics_bonus);
    assert!(c.campaign().rules.march.logistics_bonus > 0);
    // Nobody else gets it.
    for a in c.armies().iter().filter(|a| a.nation != toll) {
        let slowest = a.units.iter().map(|u| c.content().units[u.def].speed).min().unwrap();
        assert_eq!(c.march_points(a.id), slowest);
    }
}

#[test]
fn a_slow_army_takes_rounds_and_keeps_its_partial_progress() {
    let road = {
        let c = with_house_army("(\"bulwark\", 5)");
        cost(&c, "cinderhold", "forge_gate")
    };
    let pts = speed("bulwark");
    let want = road.div_ceil(pts);
    assert!(want > 1, "the fixture needs a road a Bulwark cannot walk in one round");
    let mut c = with_house_army("(\"bulwark\", 5)");
    assert_eq!(rounds_to(&mut c, "forge_gate"), want, "{road} points at {pts} a round");

    // Halfway, the progress is on the army and carries round to round.
    let mut c = with_house_army("(\"bulwark\", 5)");
    let army = house_army(&c);
    let mut r = favour_house(&c);
    c.play_round(&[Decision::March { army, to: "forge_gate".into() }], &mut r).unwrap();
    match c.army(army).unwrap().location {
        Location::Marching { progress, .. } => assert_eq!(progress, pts),
        l => panic!("still on the road after one round, not {l:?}"),
    }
    c.play_round(&[], &mut r).unwrap();
    match c.army(army).unwrap().location {
        Location::Marching { progress, .. } => assert_eq!(progress, 2 * pts),
        l => panic!("still on the road after two rounds, not {l:?}"),
    }
}

#[test]
fn a_fast_army_crosses_in_one_round_and_the_mixed_army_waits_for_its_slowest() {
    let mut fast = with_house_army("(\"ripper\", 5)");
    assert!(cost(&fast, "cinderhold", "forge_gate") <= speed("ripper"));
    assert_eq!(rounds_to(&mut fast, "forge_gate"), 1);
    let mut mixed = with_house_army("(\"ripper\", 4), (\"bulwark\", 1)");
    let mut slow = with_house_army("(\"bulwark\", 5)");
    assert_eq!(rounds_to(&mut mixed, "forge_gate"), rounds_to(&mut slow, "forge_gate"));
}

#[test]
fn terrain_slows_a_march() {
    let mut c = with_house_army("(\"ripper\", 5)");
    let scar = cost(&c, "cinderhold", "starfall");
    assert!(scar > speed("ripper"), "the scarred road is longer than a Ripper round");
    assert_eq!(rounds_to(&mut c, "starfall"), scar.div_ceil(speed("ripper")));
}

#[test]
fn a_marching_army_cannot_be_given_another_road() {
    let mut c = with_house_army("(\"bulwark\", 5)");
    let army = house_army(&c);
    let mut r = favour_house(&c);
    c.play_round(&[Decision::March { army, to: "forge_gate".into() }], &mut r).unwrap();
    assert!(c
        .play_round(&[Decision::March { army, to: "rootfall".into() }], &mut r)
        .is_err());
}
