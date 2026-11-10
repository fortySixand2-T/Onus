//! C6 AC2 — **live battles in the campaign core**: a campaign played with
//! live battles pauses each round at every battle the player's nation
//! fights, handing back its plan and changing nothing; given the played
//! outcome it resumes. AI-vs-AI battles are still auto-resolved. A played
//! outcome that does not fit, or that names a battle the round does not
//! fight, refuses the round. Fed its outcomes, a live round hashes exactly
//! as the same round played with a resolver that returns them; without
//! live battles nothing changes (C4's golden is checked by c4_autoresolve).
//!
//! C6 AC3 — **saves carry played battles** (the tests at the end): a live
//! campaign's save holds each played battle's outcome beside the round's
//! decisions and resumes to the same hashes; a changed outcome diverges at
//! its round, a missing one refuses its round; a save without live battles
//! is written exactly as before. (One test binary for both: each costs
//! 1.8 GB on the build box.)
//!
//! The readings (F-099 S5–S7, S10): a battle is live when the player's nation
//! attacks or holds and it is fought (an undefended city is no battle); a
//! played outcome is keyed by (round, city id); a pause is the round
//! refused-and-replayed, so a client caches AI battles with `Memo`.
//!
//! The client's tests (C6 AC4 on) are modules of this binary, for the same
//! reason: files under `tests/c6/` are not test targets of their own.

#[path = "c6/client.rs"]
mod client;
#[path = "c6/diplomacy.rs"]
mod diplomacy;

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use onus::campaign::{
    BattleOutcome, BattlePlan, Campaign, CampaignContent, CampaignLog, Decision, Memo, PlayedBattle, Resolver,
    RoundError, RoundStep, Side,
};
use onus::sim::content::Content;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data")
}
fn content() -> Content {
    Content::load_from_dir(&data_dir()).expect("content loads")
}
fn new_campaign(seed: u64, live: bool) -> Campaign {
    let cc = CampaignContent::shipped_with_diplomacy(&content()).unwrap();
    let c = Campaign::new(content(), cc, seed, Some("house")).unwrap();
    if live {
        c.with_live_battles()
    } else {
        c
    }
}

/// A scripted resolver that depends only on the plan: the seed's low bit
/// names the winner, the loser's units lose half their HP, the winner's a
/// quarter, buildings lose a third when the attacker wins.
fn script(p: &BattlePlan) -> BattleOutcome {
    let w = if p.seed & 1 == 0 { Side::Attack } else { Side::Defend };
    let mut o = BattleOutcome::unchanged(p, Some(w));
    let (win, lose) = match w {
        Side::Attack => (&mut o.attack, &mut o.defend),
        Side::Defend => (&mut o.defend, &mut o.attack),
    };
    for hp in &mut win.units {
        *hp -= *hp / 4;
    }
    for hp in &mut lose.units {
        *hp /= 2;
    }
    if w == Side::Attack {
        for hp in &mut o.buildings {
            *hp -= *hp / 3;
        }
    }
    o.ticks = 1000 + (p.seed % 1000) as u32;
    o
}

/// House's army walks toward `to` a road at a time: this round's decision.
fn toward(c: &Campaign, army: u32, to: &str) -> Vec<Decision> {
    let target = c.city_index(to).unwrap();
    let dist = c.distances(target);
    let Some(a) = c.army(army) else { return vec![] };
    let Some(here) = a.at() else { return vec![] };
    if here == target || !a.general.present() {
        return vec![];
    }
    let next = c
        .campaign()
        .roads_of(here)
        .map(|(_, road)| road.other(here))
        .min_by_key(|&n| (dist[n], n))
        .unwrap();
    vec![Decision::March { army, to: c.campaign().cities[next].id.clone() }]
}
fn house_army(c: &Campaign) -> u32 {
    let h = c.nation_index("house").unwrap();
    c.armies().iter().find(|a| a.nation == h).unwrap().id
}
fn players(c: &Campaign, p: &BattlePlan) -> bool {
    let h = c.player().unwrap();
    p.attack.nation == h || p.defend.nation == h
}

/// Play one live round to the end: each pause is answered with `script`.
/// Returns the plans it paused at.
fn live_round(c: &mut Campaign, ds: &[Decision], r: &mut dyn Resolver) -> Vec<BattlePlan> {
    let mut played: Vec<PlayedBattle> = Vec::new();
    let mut paused = Vec::new();
    loop {
        let before = (c.state_hash(), c.round(), c.history().len(), c.round_hashes().to_vec());
        match c.step_round(ds, &played, r).unwrap_or_else(|e| panic!("round {}: {e}", c.round() + 1)) {
            RoundStep::Played(_) => return paused,
            RoundStep::Battle(plan) => {
                let after = (c.state_hash(), c.round(), c.history().len(), c.round_hashes().to_vec());
                assert_eq!(before, after, "a pause leaves the campaign unchanged");
                assert!(players(c, &plan), "only the player's battles pause");
                assert!(!plan.undefended(), "an undefended city is no battle");
                assert_eq!(plan.round, c.round() + 1);
                assert!(
                    !played.iter().any(|b| b.city == c.campaign().cities[plan.city].id),
                    "a battle pauses once"
                );
                played.push(PlayedBattle {
                    round: plan.round,
                    city: c.campaign().cities[plan.city].id.clone(),
                    outcome: script(&plan),
                });
                paused.push(plan);
            }
        }
    }
}

#[test]
fn a_live_campaign_pauses_at_the_players_battles_and_hashes_as_a_resolver_would() {
    let mut live = new_campaign(11, true);
    let mut auto = new_campaign(11, false);
    assert!(live.live_battles() && !auto.live_battles());
    assert_eq!(live.state_hash(), auto.state_hash(), "the live flag is not campaign state");
    let army = house_army(&live);
    // In the live campaign the resolver only ever sees AI-vs-AI battles.
    let ai_battles = Rc::new(RefCell::new(0u32));
    let seen = ai_battles.clone();
    let h = live.player().unwrap();
    let mut ai_only = move |p: &BattlePlan| {
        assert!(p.attack.nation != h && p.defend.nation != h, "a player battle went to the resolver");
        *seen.borrow_mut() += 1;
        script(p)
    };
    let mut paused = 0;
    let mut player_battles = 0;
    for round in 0..30 {
        if live.is_over() {
            break;
        }
        let ds = toward(&live, army, if round < 15 { "ironspire" } else { "forge_gate" });
        paused += live_round(&mut live, &ds, &mut ai_only).len();
        let mut s = script;
        let report = auto.play_round(&ds, &mut s).unwrap();
        player_battles += report.battles.iter().filter(|b| (b.attacker == h || b.defender == h) && b.ticks > 0).count();
        assert_eq!(live.round_hashes(), auto.round_hashes(), "round {}", live.round());
        assert_eq!(live.state_hash(), auto.state_hash());
    }
    eprintln!("paused {paused}, player battles {player_battles}, ai battles {}", ai_battles.borrow());
    assert!(paused >= 2, "the walk fought battles of the player's");
    assert_eq!(paused, player_battles, "every fought player battle paused, once");
    assert!(*ai_battles.borrow() >= 1, "AI-vs-AI battles were auto-resolved");
    assert_eq!(live.played().len(), live.round() as usize, "the played battles, round by round");
    assert_eq!(live.played().iter().map(Vec::len).sum::<usize>(), paused);
}

/// The first round of seed `seed` in which the player's battle pauses:
/// the campaign just before it, the decisions, and the plan.
fn first_pause(seed: u64) -> (Campaign, Vec<Decision>, BattlePlan) {
    let mut c = new_campaign(seed, true);
    let army = house_army(&c);
    let mut r = script;
    for _ in 0..30 {
        let ds = toward(&c, army, "ironspire");
        let before = c.clone();
        if let RoundStep::Battle(plan) = c.step_round(&ds, &[], &mut r).unwrap() {
            assert_eq!(c.state_hash(), before.state_hash());
            return (c, ds, plan);
        }
    }
    panic!("no player battle in 30 rounds");
}

fn refused(c: &mut Campaign, ds: &[Decision], played: &[PlayedBattle], why: &str) {
    let before = c.state_hash();
    let rounds = c.round();
    let mut r = script;
    match c.step_round(ds, played, &mut r) {
        Err(RoundError::BadBattle { city, why: w }) => {
            assert!(w.contains(why), "expected `{why}` in `{w}` ({city})")
        }
        other => panic!("expected a refused battle (`{why}`), got {other:?}"),
    }
    assert_eq!((c.state_hash(), c.round()), (before, rounds), "a refused round changes nothing");
}

#[test]
fn an_outcome_that_does_not_fit_its_battle_or_names_no_battle_is_refused() {
    let (mut c, ds, plan) = first_pause(11);
    let city = c.campaign().cities[plan.city].id.clone();
    let good = PlayedBattle { round: plan.round, city: city.clone(), outcome: script(&plan) };
    let bent = |f: &dyn Fn(&mut BattleOutcome)| {
        let mut b = good.clone();
        f(&mut b.outcome);
        vec![b]
    };
    refused(&mut c, &ds, &bent(&|o| o.attack.units.push(1)), "units");
    refused(&mut c, &ds, &bent(&|o| { o.defend.units.pop(); }), "units");
    refused(&mut c, &ds, &bent(&|o| o.attack.generals.push(true)), "Generals");
    refused(&mut c, &ds, &bent(&|o| o.buildings.push(1)), "buildings");
    let max = c.unit_max_hp(plan.attack.nation, plan.attack.units[0].def);
    refused(&mut c, &ds, &bent(&|o| o.attack.units[0] = max + 1), "HP");
    if let Some(b) = plan.buildings.first() {
        let bmax = c.building_max_hp(plan.defend.nation, b.def);
        refused(&mut c, &ds, &bent(&|o| o.buildings[0] = bmax + 1), "HP");
    }
    let cap = c.campaign().rules.battle.tick_cap;
    refused(&mut c, &ds, &bent(&|o| o.ticks = cap + 1), "tick cap");
    // Naming a battle the round does not fight: another city, another round,
    // an unknown city, or the same battle twice.
    let elsewhere = c.campaign().cities.iter().map(|x| x.id.clone()).find(|id| *id != city).unwrap();
    let other = |round: u32, city: &str| PlayedBattle { round, city: city.to_string(), outcome: script(&plan) };
    refused(&mut c, &ds, &[good.clone(), other(plan.round, &elsewhere)], "does not fight");
    refused(&mut c, &ds, &[other(plan.round + 1, &city)], "round");
    refused(&mut c, &ds, &[other(plan.round, "atlantis")], "unknown city");
    refused(&mut c, &ds, &[good.clone(), good.clone()], "twice");
    // The good one plays.
    let mut r = script;
    let mut played = vec![good];
    loop {
        match c.step_round(&ds, &played, &mut r).unwrap() {
            RoundStep::Played(_) => break,
            RoundStep::Battle(p) => played.push(PlayedBattle {
                round: p.round,
                city: c.campaign().cities[p.city].id.clone(),
                outcome: script(&p),
            }),
        }
    }
    assert_eq!(c.round(), plan.round);
}

#[test]
fn play_round_on_a_live_campaign_refuses_a_battle_to_play_and_a_campaign_without_live_battles_takes_none() {
    let (mut c, ds, plan) = first_pause(11);
    let before = c.state_hash();
    let mut r = script;
    match c.play_round(&ds, &mut r) {
        Err(RoundError::BattleToPlay { city }) => assert_eq!(city, c.campaign().cities[plan.city].id),
        other => panic!("{other:?}"),
    }
    assert_eq!(c.state_hash(), before);
    // Without live battles: step_round is play_round, and refuses outcomes.
    let mut auto = new_campaign(11, false);
    let played = [PlayedBattle {
        round: 1,
        city: "ironspire".into(),
        outcome: BattleOutcome::unchanged(&plan, None),
    }];
    match auto.step_round(&[], &played, &mut r) {
        Err(RoundError::BadBattle { why, .. }) => assert!(why.contains("no live battles"), "{why}"),
        other => panic!("{other:?}"),
    }
    let mut twin = new_campaign(11, false);
    let RoundStep::Played(a) = auto.step_round(&[], &[], &mut r).unwrap() else { panic!("no pause") };
    let b = twin.play_round(&[], &mut r).unwrap();
    assert_eq!(a, b);
    assert_eq!(auto.state_hash(), twin.state_hash());
    assert!(auto.played().is_empty(), "nothing played without live battles");
}

#[test]
fn memo_resolves_each_plan_once() {
    let calls = Rc::new(RefCell::new(0u32));
    let seen = calls.clone();
    let inner = move |p: &BattlePlan| {
        *seen.borrow_mut() += 1;
        script(p)
    };
    let mut m = Memo::new(inner);
    let (c, _, plan) = first_pause(11);
    let a = m.resolve(c.content(), c.campaign(), &plan);
    let b = m.resolve(c.content(), c.campaign(), &plan);
    assert_eq!(a, b);
    assert_eq!(*calls.borrow(), 1, "the second call came from the memo");
    let mut other = plan.clone();
    other.seed ^= 1;
    m.resolve(c.content(), c.campaign(), &other);
    assert_eq!(*calls.borrow(), 2);
}

// ---- AC3: saves --------------------------------------------------------------

/// Ten live rounds of seed 11, House walking on Ironspire.
fn live_played() -> Campaign {
    let mut c = new_campaign(11, true);
    let army = house_army(&c);
    let mut r = script;
    for _ in 0..10 {
        if c.is_over() {
            break;
        }
        let ds = toward(&c, army, "ironspire");
        live_round(&mut c, &ds, &mut r);
    }
    assert!(c.played().iter().map(Vec::len).sum::<usize>() >= 1, "the fixture played a battle");
    c
}
fn cc() -> CampaignContent {
    CampaignContent::shipped_with_diplomacy(&content()).unwrap()
}

#[test]
fn a_live_save_holds_its_played_battles_and_resumes_to_the_same_hashes() {
    let c = live_played();
    let log = c.log();
    assert!(log.live);
    assert_eq!(log.played, c.played());
    assert_eq!(log.played.len(), log.rounds.len());
    let text = log.to_ron().unwrap();
    assert!(text.contains("played") && text.contains("live"), "{text}");
    let back = CampaignLog::from_ron(&text).unwrap();
    assert_eq!(back, log, "the save round-trips");
    // The resolver on resume only ever sees AI battles: the player's come
    // from the save.
    let h = c.player().unwrap();
    let mut ai_only = move |p: &BattlePlan| {
        assert!(p.attack.nation != h && p.defend.nation != h, "a played battle went to the resolver");
        script(p)
    };
    let r = Campaign::resume(content(), cc(), &back, &mut ai_only).unwrap();
    assert_eq!(r.round_hashes(), c.round_hashes());
    assert_eq!(r.state_hash(), c.state_hash());
    assert!(r.live_battles(), "a resumed live campaign plays on live");
    assert_eq!(r.played(), c.played());
}

#[test]
fn a_changed_outcome_diverges_at_its_round_and_a_missing_one_refuses_it() {
    let c = live_played();
    let log = c.log();
    let k = log.played.iter().position(|b| !b.is_empty()).expect("a round with a played battle");
    let mut r = script;
    // A changed outcome (the other side wins).
    let mut changed = log.clone();
    let o = &mut changed.played[k][0].outcome;
    o.winner = match o.winner {
        Some(Side::Attack) => Some(Side::Defend),
        _ => Some(Side::Attack),
    };
    let e = Campaign::resume(content(), cc(), &changed, &mut r).unwrap_err();
    assert!(e.contains(&format!("round {} diverged", k + 1)), "{e}");
    // A missing one.
    let mut missing = log.clone();
    missing.played[k].remove(0);
    let e = Campaign::resume(content(), cc(), &missing, &mut r).unwrap_err();
    assert!(e.contains(&format!("round {} refused", k + 1)) && e.contains("no played outcome"), "{e}");
    // A played battle in a save without live battles, and a live save whose
    // played rounds do not match its decision rounds, are refused at load.
    let mut not_live = log.clone();
    not_live.live = false;
    assert!(not_live.to_ron().is_err());
    let mut short = log.clone();
    short.played.pop();
    assert!(short.to_ron().unwrap_err().contains("played"));
    assert!(Campaign::resume(content(), cc(), &short, &mut r).is_err());
}

/// The C4 save, field for field: what a save without live battles was
/// written as before C6.
#[derive(serde::Serialize)]
struct C4Log {
    version: u32,
    seed: u64,
    player: Option<String>,
    content: onus::sim::content::ContentFingerprint,
    campaign: u64,
    rounds: Vec<Vec<Decision>>,
    hashes: Vec<u64>,
}

#[test]
fn a_save_without_live_battles_is_written_exactly_as_before() {
    let mut c = new_campaign(11, false);
    let army = house_army(&c);
    let mut r = script;
    for _ in 0..6 {
        if c.is_over() {
            break;
        }
        let ds = toward(&c, army, "ironspire");
        c.play_round(&ds, &mut r).unwrap();
    }
    let log = c.log();
    assert!(!log.live && log.played.is_empty());
    let c4 = C4Log {
        version: log.version,
        seed: log.seed,
        player: log.player.clone(),
        content: log.content.clone(),
        campaign: log.campaign,
        rounds: log.rounds.clone(),
        hashes: log.hashes.clone(),
    };
    let before = ron::ser::to_string_pretty(&c4, ron::ser::PrettyConfig::default()).unwrap();
    assert_eq!(log.to_ron().unwrap(), before, "byte for byte the C4 save");
    // And a C4 save loads as one without live battles.
    let back = CampaignLog::from_ron(&before).unwrap();
    assert_eq!(back, log);
    let again = Campaign::resume(content(), cc(), &back, &mut r).unwrap();
    assert_eq!(again.round_hashes(), c.round_hashes());
    assert!(!again.live_battles());
}
