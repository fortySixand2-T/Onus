//! C4 AC10 — battles between two AI nations are **auto-resolved by the
//! headless battle sim**: [`HeadlessResolver`] plays each campaign battle as
//! a headless match built by `battle_settings`, and the campaign reads the
//! result back from the battle's own report.

use std::path::PathBuf;

use onus::campaign::{
    resolve_battle, BattleOutcome, BattlePlan, Campaign, CampaignContent, Decision, HeadlessResolver, Side,
};
use onus::campaign::battle_settings;
use onus::headless;
use onus::sim::battle::BattleState;
use onus::sim::content::Content;
use onus::sim::spatial::Faction;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data")
}
fn content() -> Content {
    Content::load_from_dir(&data_dir()).expect("content loads")
}
fn shipped() -> CampaignContent {
    CampaignContent::load_from_dir(&data_dir(), &content()).expect("campaign loads")
}
/// House's (all-Ripper) assault on Forge Gate on round 1: the plan.
fn house_assault() -> (Campaign, BattlePlan) {
    let t = std::fs::read_to_string(data_dir().join("campaign.ron")).unwrap();
    let shipped = "units: [(\"sentinel\", 2), (\"ripper\", 2), (\"arclight\", 2)]";
    assert!(t.contains(shipped));
    let t = t.replacen(shipped, "units: [(\"ripper\", 6)]", 1);
    let cc = CampaignContent::from_ron(&t, &content()).expect("loads");
    let mut c = Campaign::new(content(), cc, 3, Some("house")).unwrap();
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
fn a_campaign_battle_is_played_out_by_the_headless_sim() {
    let (c, p) = house_assault();
    let o = resolve_battle(c.content(), c.campaign(), &p);
    let cap = c.campaign().rules.battle.tick_cap;
    assert!(o.ticks > 0 && o.ticks <= cap, "{} ticks", o.ticks);
    assert_eq!(o.attack.units.len(), p.attack.units.len());
    assert_eq!(o.defend.units.len(), p.defend.units.len());
    assert_eq!(o.attack.generals.len(), p.attack.generals.len());
    assert_eq!(o.defend.generals.len(), p.defend.generals.len());
    assert_eq!(o.buildings.len(), p.buildings.len());
    for (side, ps, os) in [(p.attack.nation, &p.attack, &o.attack), (p.defend.nation, &p.defend, &o.defend)] {
        for (u, &hp) in ps.units.iter().zip(&os.units) {
            assert!(hp <= c.unit_max_hp(side, u.def), "no unit leaves above its pool");
        }
    }
    // Something happened: a real battle, not the plan handed back.
    assert_ne!(o, BattleOutcome::unchanged(&p, o.winner), "the battle changed something");
    if o.ticks < cap {
        assert!(o.winner.is_some(), "a battle decided before the cap has a winner");
    } else {
        assert_eq!(o.winner, None, "the cap is undecided");
    }
    // The outcome is the battle's own report, read back by plan unit. The
    // attacker brings no workers and no buildings, so it can train nothing:
    // its survivors in the report are exactly its plan units still alive.
    assert_eq!(c.campaign().rules.battle.attacker_workers, 0);
    let s = battle_settings(c.content(), c.campaign(), &p);
    let mut app = headless::battle(content(), &s).unwrap();
    while app.world().resource::<BattleState>().result().is_none() {
        headless::step(&mut app);
    }
    let rep = app.world().resource::<BattleState>().result().unwrap().report(Faction::A).clone();
    let mut want: Vec<(usize, u32)> = rep.units.iter().filter(|u| !u.general).map(|u| (u.unit, u.hp)).collect();
    let mut got: Vec<(usize, u32)> = p
        .attack
        .units
        .iter()
        .zip(&o.attack.units)
        .filter(|(_, &hp)| hp > 0)
        .map(|(u, &hp)| (u.def, hp))
        .collect();
    want.sort_unstable();
    got.sort_unstable();
    assert_eq!(got, want, "each survivor read back onto its own plan unit");
    let fates: Vec<bool> = rep.generals.iter().map(|g| g.survived).collect();
    assert_eq!(o.attack.generals, fates);
}

#[test]
fn resolving_is_deterministic() {
    let (c, p) = house_assault();
    let a = resolve_battle(c.content(), c.campaign(), &p);
    let b = resolve_battle(c.content(), c.campaign(), &p);
    assert_eq!(a, b, "one plan, one outcome");
    // Another battle seed is another battle.
    let mut q = p.clone();
    q.seed ^= 0x9e37_79b9_7f4a_7c15;
    q.flipped = !q.flipped;
    let d = resolve_battle(c.content(), c.campaign(), &q);
    assert!(d.ticks > 0);
}

#[test]
fn an_ai_only_campaign_fights_its_battles_in_the_sim() {
    let mut c = Campaign::new(content(), shipped(), 5, None).unwrap();
    let mut r = HeadlessResolver;
    let worker = c.content().unit_index("worker").unwrap();
    let mut fought = 0;
    for _ in 0..8 {
        if c.is_over() {
            break;
        }
        let rep = c.play_round(&[], &mut r).unwrap();
        fought += rep.battles.iter().filter(|b| b.ticks > 0).count();
        for a in c.armies() {
            assert!(a.units.iter().all(|u| u.def != worker), "battle-only workers never join an army");
            for u in &a.units {
                assert!(u.hp > 0 && u.hp <= c.unit_max_hp(a.nation, u.def));
            }
        }
    }
    assert!(fought > 0, "eight rounds of AI campaign fought at least one battle in the sim");
}

/// The campaign's own golden (C4): an AI-only campaign on the headless
/// resolver, pinned by its per-round state hashes. Any change to the campaign
/// rules, the AI, the plan-to-match mapping or the battle sim moves it.
#[test]
fn campaign_golden() {
    let run = || {
        let mut c = Campaign::new(content(), shipped(), 1, None).unwrap();
        let mut r = HeadlessResolver;
        for _ in 0..GOLDEN_ROUNDS {
            if c.is_over() {
                break;
            }
            c.play_round(&[], &mut r).unwrap();
        }
        (c.round(), c.state_hash(), c.round_hashes().to_vec())
    };
    let (round, hash, hashes) = run();
    eprintln!("campaign golden: round {round} hash {hash:#018x}");
    assert_eq!(run(), (round, hash, hashes), "two runs, one campaign");
    assert_eq!((round, hash), GOLDEN, "the pinned campaign moved");
}

const GOLDEN_ROUNDS: u32 = 6;
const GOLDEN: (u32, u64) = (6, 0x1600_ef99_7dbf_8728);
