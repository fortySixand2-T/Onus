//! C4 AC2 — **veteran Generals**: a surviving General gains experience and
//! rank, and each rank strengthens his aura (RON per rank, with a cap). A
//! replacement General starts at rank 0.
//!
//! The readings (F-089): a General earns `xp.win` from a battle his side
//! wins and `xp.survive` from one it loses; a timeout counts as the holder's
//! win; an uncontested city earns nothing. His rank is the highest whose `xp`
//! he has reached; the last rank is the cap. A fallen General's army waits
//! `general_replacement_rounds` full rounds without one (it cannot march,
//! and fields no General) and then gets a new one at rank 0.

use std::path::PathBuf;

use bevy::prelude::*;

use onus::campaign::{battle_settings, BattleOutcome, BattlePlan, Campaign, CampaignContent, Decision, Side};
use onus::headless::{self, MatchSettings};
use onus::sim::battle::{AuraField, AuraRank, BattleSetup, General};
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
/// The shipped campaign, House's start army all Rippers (one round to
/// Forge Gate).
fn campaign() -> Campaign {
    let t = std::fs::read_to_string(data_dir().join("campaign.ron")).unwrap();
    let shipped = "units: [(\"sentinel\", 2), (\"ripper\", 2), (\"arclight\", 2)]";
    assert!(t.contains(shipped));
    let t = t.replacen(shipped, "units: [(\"ripper\", 6)]", 1);
    let cc = CampaignContent::from_ron(&t, &content()).expect("loads");
    Campaign::new(content(), cc, 6, Some("house")).unwrap()
}
fn house(c: &Campaign) -> usize {
    c.nation_index("house").unwrap()
}
fn house_army(c: &Campaign) -> u32 {
    c.armies().iter().find(|a| a.nation == house(c)).unwrap().id
}
/// House's assaults end as `house_wins` says, its General surviving unless
/// `general_dies`; nobody else is hurt. Every House plan is logged.
fn script(h: usize, house_wins: bool, general_dies: bool, log: &mut Vec<BattlePlan>) -> impl FnMut(&BattlePlan) -> BattleOutcome + '_ {
    move |p: &BattlePlan| {
        if p.attack.nation != h {
            return BattleOutcome::unchanged(p, Some(Side::Defend));
        }
        log.push(p.clone());
        let mut o = BattleOutcome::unchanged(p, Some(if house_wins { Side::Attack } else { Side::Defend }));
        if general_dies {
            o.attack.generals = vec![false; p.attack.generals.len()];
        }
        o
    }
}

#[test]
fn a_surviving_general_earns_experience_win_or_lose() {
    let cc = campaign().campaign().clone();
    for (wins, earned) in [(true, cc.rules.xp.win), (false, cc.rules.xp.survive)] {
        let mut c = campaign();
        let (h, army) = (house(&c), house_army(&c));
        assert_eq!(c.army(army).unwrap().general.xp, 0, "a start General is green");
        let mut log = Vec::new();
        {
            let mut r = script(h, wins, false, &mut log);
            c.play_round(&[Decision::March { army, to: "forge_gate".into() }], &mut r).unwrap();
        }
        assert_eq!(log.len(), 1);
        assert_eq!(c.army(army).unwrap().general.xp, earned, "won: {wins}");
    }
    assert!(cc.rules.xp.win > cc.rules.xp.survive && cc.rules.xp.survive > 0);
}

#[test]
fn rank_follows_experience_up_to_the_cap() {
    let c = campaign();
    let cc = c.campaign();
    assert!(cc.ranks.len() >= 2);
    assert_eq!(cc.ranks[0].xp, 0, "rank 0 needs nothing");
    for (k, r) in cc.ranks.iter().enumerate() {
        assert_eq!(cc.rank_of(r.xp), k);
        if r.xp > 0 {
            assert_eq!(cc.rank_of(r.xp - 1), k - 1);
        }
    }
    assert_eq!(cc.rank_of(u32::MAX), cc.ranks.len() - 1, "the last rank is the cap");
    for w in cc.ranks.windows(2) {
        assert!(w[1].xp > w[0].xp, "rank thresholds rise");
        assert!(w[1].radius_mult >= w[0].radius_mult && w[1].extra_factor >= w[0].extra_factor);
        assert!(
            w[1].radius_mult > w[0].radius_mult || w[1].extra_factor > w[0].extra_factor,
            "each rank strengthens the aura"
        );
    }
}

#[test]
fn a_veterans_rank_goes_into_the_battle() {
    let mut c = campaign();
    let (h, army) = (house(&c), house_army(&c));
    let top = c.campaign().ranks.last().unwrap().xp;
    let win = c.campaign().rules.xp.win;
    let mut log = Vec::new();
    {
        // Win until he reaches the top rank, marching on city after city.
        let mut r = script(h, true, false, &mut log);
        let mut rounds = 0;
        while c.army(army).unwrap().general.xp < top {
            // Next stop: the neighbour nearest a hostile city (a hostile
            // neighbour is nearest of all).
            let next = c.army(army).unwrap().at().and_then(|at| {
                c.campaign()
                    .roads_of(at)
                    .map(|(_, rd)| rd.other(at))
                    .min_by_key(|&o| {
                        let d = c.distances(o);
                        (0..c.cities().len()).filter(|&k| c.cities()[k].owner != h).map(|k| d[k]).min()
                    })
            });
            let ds = match next {
                Some(o) => vec![Decision::March { army, to: c.campaign().cities[o].id.clone() }],
                None => vec![],
            };
            c.play_round(&ds, &mut r).unwrap();
            rounds += 1;
            assert!(rounds < 30 && !c.is_over(), "never reached the top rank");
        }
    }
    let xp = c.army(army).unwrap().general.xp;
    assert!(xp >= top && log.len() as u32 >= top.div_ceil(win));
    let p = log.last().unwrap();
    // The last logged plan was fought before the final win: its rank is the
    // rank of the xp he had then.
    let before = xp - win;
    assert_eq!(p.attack.generals[0].rank, c.campaign().rank_of(before));
    // And the battle gets the rank's numbers.
    let s = battle_settings(c.content(), c.campaign(), p);
    let side = s.battle.as_ref().unwrap().side(Faction::A);
    let rank = &c.campaign().ranks[p.attack.generals[0].rank];
    let want = if rank.radius_milli() == 1000 && rank.extra_factor == 0 {
        None
    } else {
        Some(AuraRank {
            radius_milli: rank.radius_milli(),
            extra_factor: rank.extra_factor,
        })
    };
    assert_eq!(side.ranks.first().copied().flatten(), want);
}

#[test]
fn a_fallen_general_is_replaced_at_rank_zero_after_the_wait() {
    let mut c = campaign();
    let (h, army) = (house(&c), house_army(&c));
    let wait = c.campaign().rules.general_replacement_rounds;
    assert!(wait > 0);
    {
        let mut log = Vec::new();
        let mut r = script(h, true, false, &mut log);
        c.play_round(&[Decision::March { army, to: "forge_gate".into() }], &mut r).unwrap();
    }
    assert!(c.army(army).unwrap().general.xp > 0);
    let fg = c.city_index("forge_gate").unwrap();
    let next = c
        .campaign()
        .roads_of(fg)
        .map(|(_, rd)| rd.other(fg))
        .find(|&o| c.cities()[o].owner != h)
        .unwrap();
    let next = c.campaign().cities[next].id.clone();
    let mut log = Vec::new();
    let mut r = script(h, false, true, &mut log);
    c.play_round(&[Decision::March { army, to: next.clone() }], &mut r).unwrap();
    let mut rounds_waited = 0;
    loop {
        // Arrived or not, once he falls the army is leaderless.
        if !c.army(army).unwrap().general.present() {
            break;
        }
        c.play_round(&[], &mut r).unwrap();
        assert!(c.round() < 10, "the General never fell");
    }
    let g = c.army(army).unwrap().general;
    assert_eq!(g.xp, 0, "a fallen General's experience is gone");
    // While it waits, the army cannot march and fields no General.
    while !c.army(army).unwrap().general.present() {
        let at = c.army(army).unwrap().at().expect("home after the failed assault");
        let to = c.campaign().roads_of(at).map(|(_, rd)| rd.other(at)).next().unwrap();
        let to = c.campaign().cities[to].id.clone();
        assert!(c.play_round(&[Decision::March { army, to }], &mut r).is_err(), "a leaderless army holds");
        c.play_round(&[], &mut r).unwrap();
        rounds_waited += 1;
        assert!(rounds_waited <= wait, "waited too long");
    }
    assert_eq!(rounds_waited, wait, "it waits the RON rounds");
    let g = c.army(army).unwrap().general;
    assert_eq!((g.xp, c.campaign().rank_of(g.xp)), (0, 0), "the replacement starts at rank 0");
}

#[test]
fn a_leaderless_army_fields_no_general() {
    let mut c = campaign();
    let (h, army) = (house(&c), house_army(&c));
    let mut log = Vec::new();
    {
        let mut r = script(h, false, true, &mut log);
        c.play_round(&[Decision::March { army, to: "forge_gate".into() }], &mut r).unwrap();
    }
    assert!(!c.army(army).unwrap().general.present());
    // The AI marches on Cinderhold sooner or later: its defenders field no
    // General while House's waits.
    let mut seen = Vec::new();
    let mut r = |p: &BattlePlan| {
        if p.defend.nation == h {
            seen.push(p.clone());
        }
        BattleOutcome::unchanged(p, Some(Side::Defend))
    };
    for _ in 0..c.campaign().rules.general_replacement_rounds {
        c.play_round(&[], &mut r).unwrap();
    }
    for p in &seen {
        if c.round() > p.round && p.defend.armies.contains(&army) {
            assert!(p.defend.generals.iter().all(|g| g.army != army), "no General to field");
        }
    }
}

// ---- the battle layer: a ranked aura ---------------------------------------

/// The bonus a Sentinel `d` world units from a General `id` of `rank` gets.
fn bonus_at(id: &str, rank: Option<AuraRank>, d: f32) -> onus::sim::combat::StatBonus {
    let setup = BattleSetup::default()
        .with_general(Faction::B, "general_bulwark")
        .with_ranked_general(Faction::A, id, rank);
    let mut app = headless::ai_vs_ai(content(), &MatchSettings::default().with_battle(setup)).expect("battle builds");
    app.insert_resource(AiCommanders::default());
    let mut q = app.world_mut().query_filtered::<(Entity, &Faction), With<General>>();
    let g = q.iter(app.world()).find(|(_, f)| **f == Faction::A).map(|(e, _)| e).unwrap();
    app.world_mut().get_mut::<Position>(g).unwrap().0 = Vec2::ZERO;
    let (idx, kind, hp) = {
        let c = app.world().resource::<Content>();
        let idx = c.unit_index("sentinel").unwrap();
        (idx, c.units[idx].mvp_kind, Health::from_def(c, idx))
    };
    let u = app
        .world_mut()
        .spawn((Position(Vec2::new(d, 0.0)), UnitDefIdx(idx), kind, Faction::A, hp))
        .id();
    headless::step(&mut app);
    app.world().resource::<AuraField>().get(u)
}

#[test]
fn a_rank_widens_and_strengthens_the_aura() {
    let c = content();
    let focus = c.general_at(c.general_index("general_focus").unwrap()).unwrap();
    let r = focus.aura.radius;
    let plain = bonus_at("general_focus", None, r * 0.5);
    assert!(plain.offense > 0);
    let wide = Some(AuraRank { radius_milli: 1300, extra_factor: 0 });
    // Beyond the plain radius, inside the ranked one.
    assert_eq!(bonus_at("general_focus", None, r * 1.2).offense, 0);
    assert_eq!(bonus_at("general_focus", wide, r * 1.2), plain, "a wider aura, as strong");
    // An extra factor applies the modifiers once more.
    let strong = Some(AuraRank { radius_milli: 1000, extra_factor: 1 });
    assert_eq!(bonus_at("general_focus", strong, r * 0.5).offense, 2 * plain.offense);
    // A plain rank is no rank.
    let none = Some(AuraRank { radius_milli: 1000, extra_factor: 0 });
    assert_eq!(bonus_at("general_focus", none, r * 0.5), plain);
}
