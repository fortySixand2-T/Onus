//! C6 critic probes (adversarial review of the campaign client, isolated:
//! diff + spec only). Self-contained, so it compiles both as its own test
//! binary and as a module of another (`#[path] mod critic_c6;`), which is
//! how it was run on the disk-starved build box.
//!
//! Probes:
//! - AC3: a changed played outcome must diverge (or be refused) at its round.
//! - AC6: a live battle, with both sides on the headless AI, reads the same
//!   outcome as the headless resolver (differential oracle for `populate`,
//!   `read_outcome`, the tick cap and the field clearing).
//! - AC6: a *defending* player (factions swapped) reads his own side back:
//!   at a short cap nothing changes; retreating loses the city.
//! - AC8: the legacy beat shows on the Story screen before the Over screen.
//! - F-099 S18: a round refused after a pause changes nothing and pauses at
//!   the same battle again.

use std::path::PathBuf;

use bevy::prelude::*;

use onus::campaign::story::Story;
use onus::campaign::{
    battle_settings, resolve_battle, BattleOutcome, BattlePlan, Campaign, CampaignContent, CampaignOutcome,
    Decision, PlayedBattle, RoundStep, Side,
};
use onus::campaign_client::{self as cc, story_text, CampaignSession, LiveBattle, MapLayout, Screen};
use onus::headless;
use onus::sim::content::Content;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data")
}
fn content() -> Content {
    Content::load_from_dir(&data_dir()).expect("content loads")
}
fn ccontent(cap: Option<u32>, round_cap: Option<u32>) -> CampaignContent {
    let mut c = CampaignContent::shipped_with_diplomacy(&content()).unwrap();
    if let Some(cap) = cap {
        c.rules.battle.tick_cap = cap;
    }
    if let Some(r) = round_cap {
        c.rules.round_cap = r;
    }
    c
}
fn live_campaign(seed: u64, cap: Option<u32>, round_cap: Option<u32>) -> Campaign {
    Campaign::new(content(), ccontent(cap, round_cap), seed, Some("house"))
        .unwrap()
        .with_live_battles()
}

fn hold(p: &BattlePlan) -> BattleOutcome {
    BattleOutcome::unchanged(p, Some(Side::Defend))
}

fn house(c: &Campaign) -> usize {
    c.nation_index("house").unwrap()
}
fn house_army(c: &Campaign) -> u32 {
    let h = house(c);
    c.armies().iter().find(|a| a.nation == h).unwrap().id
}

/// House's army walks toward `to` a road at a time: this round's decision.
fn toward(c: &Campaign, army: u32, to: usize) -> Vec<Decision> {
    let dist = c.distances(to);
    let Some(a) = c.army(army) else { return vec![] };
    let Some(here) = a.at() else { return vec![] };
    if here == to || !a.general.present() {
        return vec![];
    }
    let next = c
        .campaign()
        .roads_of(here)
        .map(|(_, road)| road.other(here))
        .min_by_key(|&n| (dist[n], n))
        .unwrap();
    vec![Decision::March {
        army,
        to: c.campaign().cities[next].id.clone(),
    }]
}

/// Play a live round to its end, every pause answered with `answer`.
fn finish_round(c: &mut Campaign, ds: &[Decision], answer: &dyn Fn(&BattlePlan) -> BattleOutcome) {
    let mut played: Vec<PlayedBattle> = Vec::new();
    loop {
        match c.step_round(ds, &played, &mut hold).unwrap() {
            RoundStep::Played(_) => return,
            RoundStep::Battle(plan) => played.push(PlayedBattle {
                round: plan.round,
                city: c.campaign().cities[plan.city].id.clone(),
                outcome: answer(&plan),
            }),
        }
    }
}

/// Advance a live campaign until a round whose **first** pause satisfies
/// `want`; returns the campaign before that round, the round's decisions and
/// the plan. `ds` picks each round's decisions.
fn advance_to_pause(
    mut c: Campaign,
    ds: &dyn Fn(&Campaign) -> Vec<Decision>,
    want: &dyn Fn(&Campaign, &BattlePlan) -> bool,
    rounds: u32,
) -> Option<(Campaign, Vec<Decision>, BattlePlan)> {
    for _ in 0..rounds {
        if c.is_over() {
            return None;
        }
        let d = ds(&c);
        if let RoundStep::Battle(plan) = c.clone().step_round(&d, &[], &mut hold).unwrap() {
            if want(&c, &plan) {
                return Some((c, d, plan));
            }
        }
        finish_round(&mut c, &d, &hold);
    }
    None
}

// ---- a headless client ------------------------------------------------------

fn session(c: Campaign, resolver: Box<dyn onus::campaign::Resolver + Send + Sync>) -> CampaignSession {
    let story = Story::shipped(c.content(), c.campaign()).unwrap();
    let layout = MapLayout::shipped(c.campaign()).unwrap();
    CampaignSession::new(c, story, layout, resolver)
}

fn app(s: CampaignSession) -> App {
    app_on(s, false)
}

/// The client's logic headless: the sim on `Update` (`fixed == false`, as
/// the C6 tests run it, one tick per `headless::step`), or on `FixedUpdate`
/// as the shipped app runs it, one fixed step per `app.update()`.
fn app_on(s: CampaignSession, fixed: bool) -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .insert_resource(Time::<Fixed>::from_hz(60.0))
        .insert_resource(s.campaign.content().clone())
        .init_resource::<ButtonInput<KeyCode>>();
    if fixed {
        app.insert_resource(bevy::time::TimeUpdateStrategy::FixedTimesteps(1));
        cc::add_campaign_client(&mut app, FixedUpdate);
    } else {
        cc::add_campaign_client(&mut app, Update);
    }
    cc::start(&mut app, s);
    app.update();
    app
}

fn press(app: &mut App, key: KeyCode) {
    app.world_mut().resource_mut::<ButtonInput<KeyCode>>().press(key);
    app.update();
    let mut keys = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
    keys.release(key);
    keys.clear();
    app.update();
}

fn screen(app: &App) -> Screen {
    app.world().resource::<State<Screen>>().get().clone()
}
fn sess(app: &App) -> &CampaignSession {
    app.world().resource::<CampaignSession>()
}
fn sess_mut(app: &mut App) -> Mut<'_, CampaignSession> {
    app.world_mut().resource_mut::<CampaignSession>()
}

fn skip_story(app: &mut App) {
    let mut n = 0;
    while screen(app) == Screen::Story {
        press(app, KeyCode::Enter);
        n += 1;
        assert!(n < 50);
    }
}

/// Step the battle until the client leaves the Battle screen or the round
/// is paused at a different battle.
fn fight_out(app: &mut App, max: u32) -> u32 {
    fight_out_on(app, max, false)
}
fn fight_out_on(app: &mut App, max: u32, fixed: bool) -> u32 {
    let mut n = 0;
    let start = sess(app).battle.clone();
    while screen(app) == Screen::Battle && sess(app).battle == start {
        assert!(n < max, "the battle is still on after {max} ticks");
        if fixed {
            app.update();
        } else {
            headless::step(app);
        }
        n += 1;
    }
    n
}

/// Real sim ticks played in the current match.
fn sim_tick(app: &App) -> u32 {
    app.world().resource::<onus::sim::MatchState>().tick()
}

/// The outcome the client played for `plan`, wherever it now is.
fn played_for(app: &App, c: &Campaign, plan: &BattlePlan) -> BattleOutcome {
    let id = &c.campaign().cities[plan.city].id;
    let s = sess(app);
    s.played
        .iter()
        .chain(s.campaign.played().iter().flatten())
        .find(|b| b.round == plan.round && &b.city == id)
        .map(|b| b.outcome.clone())
        .expect("the battle was played")
}

/// A client paused at `plan` (fielded, no tick yet).
fn client_at(c: &Campaign, ds: &[Decision], plan: &BattlePlan) -> App {
    client_at_on(c, ds, plan, false)
}
fn client_at_on(c: &Campaign, ds: &[Decision], plan: &BattlePlan, fixed: bool) -> App {
    let mut s = session(c.clone(), Box::new(hold));
    s.staged = ds.to_vec();
    let mut app = app_on(s, fixed);
    skip_story(&mut app);
    assert_eq!(screen(&app), Screen::Map);
    press(&mut app, KeyCode::Enter);
    assert_eq!(screen(&app), Screen::Battle);
    assert_eq!(sess(&app).battle.as_ref(), Some(plan));
    assert!(app.world().get_resource::<LiveBattle>().is_some(), "fielded");
    app
}

// ---- AC3: a changed outcome ---------------------------------------------------

fn defended_fixture() -> Campaign {
    // House walks on Ironspire; every battle of his is answered "the
    // defender held", nobody hurt, at tick 777.
    let mut c = live_campaign(11, None, None);
    let army = house_army(&c);
    let target = c.city_index("ironspire").unwrap();
    let answer = |p: &BattlePlan| {
        let mut o = BattleOutcome::unchanged(p, Some(Side::Defend));
        o.ticks = 777;
        o
    };
    for _ in 0..12 {
        if c.is_over() {
            break;
        }
        let ds = toward(&c, army, target);
        finish_round(&mut c, &ds, &answer);
    }
    assert!(c.played().iter().any(|r| !r.is_empty()), "the fixture played a battle");
    c
}

#[test]
fn critic_ac3_a_played_outcome_with_changed_ticks_diverges_or_is_refused() {
    let c = defended_fixture();
    let log = c.log();
    let k = log.played.iter().position(|b| !b.is_empty()).unwrap();
    let mut changed = log.clone();
    changed.played[k][0].outcome.ticks = 1;
    match Campaign::resume(content(), ccontent(None, None), &changed, &mut hold) {
        Err(e) => assert!(e.contains(&format!("round {}", k + 1)), "{e}"),
        Ok(r) => panic!(
            "a changed played outcome (ticks 777 -> 1, round {}) resumed silently; the resumed \
             campaign's battle history differs from the saved one's: {}",
            k + 1,
            r.history() != c.history()
        ),
    }
}

#[test]
fn critic_ac3_a_played_outcome_with_winner_defend_changed_to_undecided_diverges_or_is_refused() {
    let c = defended_fixture();
    let log = c.log();
    let k = log.played.iter().position(|b| !b.is_empty()).unwrap();
    let mut changed = log.clone();
    assert_eq!(changed.played[k][0].outcome.winner, Some(Side::Defend));
    changed.played[k][0].outcome.winner = None;
    match Campaign::resume(content(), ccontent(None, None), &changed, &mut hold) {
        Err(e) => assert!(e.contains(&format!("round {}", k + 1)), "{e}"),
        Ok(r) => panic!(
            "a changed played outcome (winner Defend -> undecided, round {}) resumed silently; \
             history differs: {}",
            k + 1,
            r.history() != c.history()
        ),
    }
}

// ---- AC6: the live battle against the headless resolver ----------------------

fn first_attack(cap: u32) -> (Campaign, Vec<Decision>, BattlePlan) {
    let start = live_campaign(3, Some(cap), None);
    let army = house_army(&start);
    let target = start.city_index("ironspire").unwrap();
    let h = house(&start);
    advance_to_pause(start, &|c| toward(c, army, target), &|_, p| p.attack.nation == h, 40)
        .expect("an attack of House's")
}

/// The live battle, both sides on the AI the headless resolver gives them,
/// run in the shipped arrangement (sim on `FixedUpdate`): the outcome must be
/// the headless resolver's on the same plan.
#[test]
fn critic_ac6_a_live_attack_with_both_sides_on_the_ai_reads_as_the_headless_resolver() {
    const CAP: u32 = 1500;
    let (c, ds, plan) = first_attack(CAP);
    let mut app = client_at_on(&c, &ds, &plan, true);
    assert_eq!(app.world().resource::<LiveBattle>().ticks, 0);
    assert_eq!(sim_tick(&app), 0);
    let settings = battle_settings(c.content(), c.campaign(), &plan);
    let both = headless::commanders(c.content(), &settings).unwrap();
    app.world_mut().insert_resource(both);
    fight_out_on(&mut app, CAP + 20, true);
    let got = played_for(&app, &c, &plan);
    let want = resolve_battle(c.content(), c.campaign(), &plan);
    assert_eq!(got, want, "the live read differs from the headless resolver on the same plan and AI");
}

/// In the arrangement the C6 tests run the client in (sim on `Update`), the
/// battle's tick count must be the sim's: a tick counted that the sim never
/// played ends a capped battle one tick early and stamps it with the wrong
/// tick count.
#[test]
fn critic_ac6_the_tests_arrangement_counts_only_ticks_the_sim_played() {
    let (c, ds, plan) = first_attack(40);
    let app = client_at(&c, &ds, &plan);
    let live = app.world().resource::<LiveBattle>().ticks;
    assert_eq!(live, sim_tick(&app), "LiveBattle.ticks {live} vs sim ticks {}", sim_tick(&app));
}

// ---- AC6: a defending player ---------------------------------------------------

fn defending(cap: Option<u32>) -> (Campaign, Vec<Decision>, BattlePlan) {
    for seed in 1..40u64 {
        let start = live_campaign(seed, cap, Some(400));
        let h = house(&start);
        if let Some(found) = advance_to_pause(start, &|_| vec![], &|_, p| p.defend.nation == h, 60) {
            return found;
        }
    }
    panic!("no seed in which House defends");
}

#[test]
fn critic_ac6_a_defending_player_at_a_short_cap_walks_off_as_he_came_on() {
    let (c, ds, plan) = defending(Some(5));
    let mut app = client_at(&c, &ds, &plan);
    fight_out(&mut app, 30);
    let o = played_for(&app, &c, &plan);
    assert_eq!(o.winner, None, "at the cap the defender holds");
    assert_eq!(o.ticks, 5);
    assert_eq!(o.attack.units, plan.attack.units.iter().map(|u| u.hp).collect::<Vec<_>>());
    assert_eq!(o.defend.units, plan.defend.units.iter().map(|u| u.hp).collect::<Vec<_>>());
    assert_eq!(o.buildings, plan.buildings.iter().map(|b| b.hp).collect::<Vec<_>>());
    assert!(o.attack.generals.iter().chain(&o.defend.generals).all(|g| *g));
}

#[test]
fn critic_ac6_a_defending_player_who_retreats_loses_the_city() {
    let (c, ds, plan) = defending(None);
    let mut app = client_at(&c, &ds, &plan);
    press(&mut app, KeyCode::KeyT);
    let cap = c.campaign().rules.battle.tick_cap;
    fight_out(&mut app, cap + 20);
    let o = played_for(&app, &c, &plan);
    assert_eq!(o.winner, Some(Side::Attack), "the defender retreated: the attacker wins");
    assert!(o.defend.units.iter().any(|&hp| hp > 0) || plan.defend.units.is_empty(), "the retreat walked units off");
    // The round, once played, gave the city to the attacker.
    if sess(&app).campaign.round() == c.round() + 1 {
        assert_eq!(sess(&app).campaign.cities()[plan.city].owner, plan.attack.nation);
    }
}

// ---- F-099 S18: refused after a pause ------------------------------------------

#[test]
fn critic_s18_a_round_refused_after_a_pause_changes_nothing_and_pauses_again() {
    let start = live_campaign(3, None, None);
    let army = house_army(&start);
    let target = start.city_index("ironspire").unwrap();
    let (c, ds, plan) = advance_to_pause(start, &|c| toward(c, army, target), &|_, _| true, 40).expect("a battle");
    let mut s = session(c.clone(), Box::new(hold));
    s.staged = ds.clone();
    let mut app = app(s);
    skip_story(&mut app);
    // Enter: the round stops at the battle (before it is fielded, answer it
    // with an outcome that does not fit: one unit too many).
    app.world_mut().resource_mut::<ButtonInput<KeyCode>>().press(KeyCode::Enter);
    app.update();
    {
        let mut keys = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
        keys.release(KeyCode::Enter);
        keys.clear();
    }
    assert_eq!(sess(&app).battle.as_ref(), Some(&plan));
    let mut bad = BattleOutcome::unchanged(&plan, None);
    bad.attack.units.push(1);
    let to = sess_mut(&mut app).battle_played(bad);
    assert_eq!(to, Screen::Map, "a refused round goes back to the map");
    app.world_mut().resource_mut::<NextState<Screen>>().set(to);
    app.update();
    let s = sess(&app);
    assert_eq!(s.campaign.state_hash(), c.state_hash(), "the refused round changed nothing");
    assert_eq!(s.campaign.round(), c.round());
    assert!(s.played.is_empty() && s.battle.is_none());
    assert!(s.message.as_deref().is_some_and(|m| m.contains("refused")), "{:?}", s.message);
    assert_eq!(s.staged, ds, "the staged orders stay");
    assert!(app.world().get_resource::<LiveBattle>().is_none());
    // Ending the turn again pauses at the same battle.
    press(&mut app, KeyCode::Enter);
    assert_eq!(screen(&app), Screen::Battle);
    assert_eq!(sess(&app).battle.as_ref(), Some(&plan));
}

// ---- AC8: the legacy beat before the Over screen --------------------------------

#[test]
fn critic_ac8_the_legacy_beat_shows_before_the_over_screen() {
    let c = live_campaign(5, None, Some(400));
    let h = house(&c);
    let army = house_army(&c);
    // The AI's battles: House wins his own (never resolved here), the
    // defender holds the rest.
    let favour = move |p: &BattlePlan| {
        let w = if p.attack.nation == h { Side::Attack } else { Side::Defend };
        BattleOutcome::unchanged(p, Some(w))
    };
    let mut app = app(session(c, Box::new(favour)));
    let mut shown: Vec<String> = Vec::new();
    let read = |app: &mut App, shown: &mut Vec<String>| {
        while screen(app) == Screen::Story {
            let s = sess(app);
            let k = *s.beats.first().unwrap();
            let b = &s.story.beats()[k];
            assert!(story_text(s).contains(&b.title));
            shown.push(b.id.clone());
            press(app, KeyCode::Enter);
        }
    };
    read(&mut app, &mut shown);
    for _ in 0..300 {
        if screen(&app) == Screen::Over {
            break;
        }
        assert_eq!(screen(&app), Screen::Map);
        let c = &sess(&app).campaign;
        // March on the nearest living rival capital.
        let dist = c.army(army).and_then(|a| a.at()).map(|at| c.distances(at));
        let target = (0..c.nations().len())
            .filter(|&n| n != h && c.nations()[n].alive)
            .map(|n| c.campaign().capital[n])
            .min_by_key(|&cap| (dist.as_ref().map_or(0, |d| d[cap]), cap));
        let ds = target.map_or_else(Vec::new, |t| toward(c, army, t));
        sess_mut(&mut app).staged = ds;
        let before = sess(&app).campaign.round();
        press(&mut app, KeyCode::Enter);
        let mut n = 0;
        while screen(&app) == Screen::Battle {
            n += 1;
            assert!(n < 20);
            let to = {
                let mut s = sess_mut(&mut app);
                let plan = s.battle.clone().unwrap();
                let w = if plan.attack.nation == h { Side::Attack } else { Side::Defend };
                s.battle_played(BattleOutcome::unchanged(&plan, Some(w)))
            };
            app.world_mut().resource_mut::<NextState<Screen>>().set(to);
            app.update();
        }
        assert_eq!(sess(&app).campaign.round(), before + 1, "{:?}", sess(&app).message);
        read(&mut app, &mut shown);
    }
    assert_eq!(sess(&app).campaign.outcome(), Some(CampaignOutcome::Won { nation: h }), "House won");
    assert_eq!(screen(&app), Screen::Over);
    assert_eq!(shown.last().map(String::as_str), Some("legacy"), "legacy is the last beat shown: {shown:?}");
    assert!(!shown.contains(&"fallen".to_string()));
}

// ---- round 2: the history hash, the sim's tick count -------------------------

/// A live fixture whose played outcomes vary in every field: the winner
/// cycles (attack, defend, undecided), units walk off hurt, the second
/// General (if any) falls, buildings are damaged, ticks differ per round.
fn varied_fixture() -> Campaign {
    let mut c = live_campaign(11, None, None);
    let army = house_army(&c);
    let target = c.city_index("ironspire").unwrap();
    let answer = |p: &BattlePlan| {
        let w = match p.round % 3 {
            0 => Some(Side::Attack),
            1 => Some(Side::Defend),
            _ => None,
        };
        let mut o = BattleOutcome::unchanged(p, w);
        for hp in o.attack.units.iter_mut().chain(o.defend.units.iter_mut()) {
            *hp = (*hp).div_ceil(2);
        }
        for hp in o.buildings.iter_mut() {
            *hp = (*hp).div_ceil(2);
        }
        if o.attack.generals.len() > 1 {
            o.attack.generals[1] = false;
        }
        o.ticks = 300 + p.round;
        o
    };
    for _ in 0..14 {
        if c.is_over() {
            break;
        }
        let ds = toward(&c, army, target);
        finish_round(&mut c, &ds, &answer);
        assert_eq!(c.history_hashes().len(), c.round_hashes().len(), "a history hash per round, pauses or not");
    }
    assert!(c.played().iter().flatten().count() >= 1, "the fixture played a battle");
    c
}

/// Everything about a campaign but its input (the played outcomes).
fn observable(c: &Campaign) -> String {
    format!(
        "{:?}|{:?}|{:?}|{:?}|{:?}|{}|{:?}",
        c.nations(),
        c.cities(),
        c.armies(),
        c.history(),
        c.outcome(),
        c.round(),
        c.round_hashes()
    )
}

/// AC3: every single-field change to a played outcome is refused, or it
/// changes nothing a player could ever see (now, or over the next rounds).
/// A change that resumes and moves the campaign is a silent divergence.
#[test]
fn critic_r2_every_single_field_tamper_of_a_played_outcome_is_refused_or_changes_nothing() {
    let c = varied_fixture();
    let log = c.log();
    let mut tampers: Vec<(String, onus::campaign::CampaignLog)> = Vec::new();
    for (r, round) in log.played.iter().enumerate() {
        for (b, pb) in round.iter().enumerate() {
            let mut push = |what: String, f: &dyn Fn(&mut BattleOutcome)| {
                let mut l = log.clone();
                f(&mut l.played[r][b].outcome);
                if l.played[r][b].outcome != pb.outcome {
                    tampers.push((format!("round {} battle {b}: {what}", r + 1), l));
                }
            };
            for w in [None, Some(Side::Attack), Some(Side::Defend)] {
                push(format!("winner {w:?}"), &|o| o.winner = w);
            }
            push("ticks+1".into(), &|o| o.ticks += 1);
            push("ticks-1".into(), &|o| o.ticks = o.ticks.saturating_sub(1));
            for i in 0..pb.outcome.attack.units.len() {
                push(format!("attack unit {i} hp-1"), &|o| o.attack.units[i] = o.attack.units[i].saturating_sub(1));
                push(format!("attack unit {i} dead"), &|o| o.attack.units[i] = 0);
            }
            for i in 0..pb.outcome.defend.units.len() {
                push(format!("defend unit {i} hp-1"), &|o| o.defend.units[i] = o.defend.units[i].saturating_sub(1));
                push(format!("defend unit {i} dead"), &|o| o.defend.units[i] = 0);
            }
            for i in 0..pb.outcome.attack.generals.len() {
                push(format!("attack general {i} flipped"), &|o| o.attack.generals[i] = !o.attack.generals[i]);
            }
            for i in 0..pb.outcome.defend.generals.len() {
                push(format!("defend general {i} flipped"), &|o| o.defend.generals[i] = !o.defend.generals[i]);
            }
            for i in 0..pb.outcome.buildings.len() {
                push(format!("building {i} hp-1"), &|o| o.buildings[i] = o.buildings[i].saturating_sub(1));
            }
        }
    }
    assert!(!tampers.is_empty());
    let want = observable(&c);
    let (mut refused, mut inert) = (0, Vec::new());
    let mut silent = Vec::new();
    for (what, l) in &tampers {
        // Through the save text, as a player's edited file would come in.
        let text = l.to_ron().unwrap();
        let back = onus::campaign::CampaignLog::from_ron(&text);
        let resumed = back.and_then(|b| Campaign::resume(content(), ccontent(None, None), &b, &mut hold));
        match resumed {
            Err(_) => refused += 1,
            Ok(mut r) => {
                let mut o = c.clone();
                let same_now = observable(&r) == want;
                // Play on: three more rounds, the same orders and answers.
                for _ in 0..3 {
                    if o.is_over() || r.is_over() {
                        break;
                    }
                    finish_round(&mut o, &[], &hold);
                    finish_round(&mut r, &[], &hold);
                }
                if same_now && observable(&r) == observable(&o) {
                    inert.push(what.clone());
                } else {
                    silent.push(what.clone());
                }
            }
        }
    }
    eprintln!("tampers {}, refused {refused}, inert {}: {inert:?}", tampers.len(), inert.len());
    assert!(silent.is_empty(), "changed outcomes that resumed silently and moved the campaign: {silent:?}");
}

/// Older saves: a non-live save carries no history hashes and resumes; a
/// live save without them (written before the hash existed) is refused
/// loudly, never resumed unchecked; history hashes on a non-live save, or
/// one hash too few or too many, are refused.
#[test]
fn critic_r2_history_hashes_in_saves_are_checked_both_ways() {
    // Non-live.
    let mut plain = Campaign::new(content(), ccontent(None, None), 11, Some("house")).unwrap();
    for _ in 0..4 {
        plain.play_round(&[], &mut hold).unwrap();
    }
    let pl = plain.log();
    assert!(pl.history.is_empty());
    let text = pl.to_ron().unwrap();
    assert!(!text.contains("history"), "a non-live save is written as before:\n{text}");
    let back = onus::campaign::CampaignLog::from_ron(&text).unwrap();
    let r = Campaign::resume(content(), ccontent(None, None), &back, &mut hold).unwrap();
    assert_eq!(r.round_hashes(), plain.round_hashes());
    let mut bad = pl.clone();
    bad.history = vec![0; bad.rounds.len()];
    assert!(Campaign::resume(content(), ccontent(None, None), &bad, &mut hold).is_err());
    // Live.
    let c = varied_fixture();
    let l = c.log();
    assert_eq!(l.history.len(), l.rounds.len());
    let mut none = l.clone();
    none.history.clear();
    assert!(Campaign::resume(content(), ccontent(None, None), &none, &mut hold).is_err(), "a live save without history hashes");
    let mut short = l.clone();
    short.history.pop();
    assert!(Campaign::resume(content(), ccontent(None, None), &short, &mut hold).is_err());
    let mut long = l.clone();
    long.history.push(1);
    assert!(Campaign::resume(content(), ccontent(None, None), &long, &mut hold).is_err());
    // A changed history hash alone diverges at its round.
    let k = l.played.iter().position(|b| !b.is_empty()).unwrap();
    let mut forged = l.clone();
    forged.history[k] ^= 1;
    let e = Campaign::resume(content(), ccontent(None, None), &forged, &mut hold).unwrap_err();
    assert!(e.contains(&format!("round {}", k + 1)), "{e}");
    let ok = Campaign::resume(content(), ccontent(None, None), &l, &mut hold).unwrap();
    assert_eq!(ok.history_hashes(), c.history_hashes());
    assert_eq!(ok.history(), c.history());
}

/// AC6: the same battle, the player idle, read the same in the arrangement
/// the tests use (sim on `Update`) and the one shipped (`FixedUpdate`), at a
/// short cap: the tick count is the sim's either way, and equals the cap.
#[test]
fn critic_r2_a_capped_battle_reads_the_same_on_update_and_on_fixed_update() {
    const CAP: u32 = 40;
    let (c, ds, plan) = first_attack(CAP);
    let mut outs = Vec::new();
    for fixed in [false, true] {
        let mut app = client_at_on(&c, &ds, &plan, fixed);
        assert_eq!(app.world().resource::<LiveBattle>().ticks, 0);
        fight_out_on(&mut app, CAP + 20, fixed);
        outs.push(played_for(&app, &c, &plan));
    }
    assert_eq!(outs[0], outs[1], "Update vs FixedUpdate");
    let o = &outs[0];
    assert!(o.ticks == CAP || (o.winner.is_some() && o.ticks < CAP), "{o:?}");
}

/// AC6 (critic r1 #2, the cap of 0), in the shipped arrangement (sim on
/// `FixedUpdate`): the battle ends at once with no tick played, and the
/// round is played on it, not refused for ticks past the cap.
#[test]
fn critic_r2_a_tick_cap_of_0_ends_the_battle_unplayed_on_fixed_update() {
    let (c, ds, plan) = first_attack(0);
    let mut s = session(c.clone(), Box::new(hold));
    s.staged = ds.clone();
    let mut app = app_on(s, true);
    skip_story(&mut app);
    press(&mut app, KeyCode::Enter);
    for _ in 0..20 {
        if sess(&app).campaign.round() > c.round() {
            break;
        }
        app.update();
    }
    assert_eq!(sim_tick(&app), 0, "the sim never ticked");
    assert_eq!(
        sess(&app).campaign.round(),
        c.round() + 1,
        "the round was played on the unplayed battle: {:?}",
        sess(&app).message
    );
    let o = played_for(&app, &c, &plan);
    assert_eq!((o.ticks, o.winner), (0, None));
}
