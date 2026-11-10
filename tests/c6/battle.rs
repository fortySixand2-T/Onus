//! C6 AC6 — **into and out of battles** (a module of the `c6_live` binary).
//! When the round pauses at a battle of the player's, the client enters the
//! Battle screen: the match `battle_settings` builds from the plan, the
//! player's side on the faction he commands (`PLAYER_FACTION`), the
//! opposing nation's AI on its own strategy. He commands through `Order`s
//! in the sim schedule; T issues the C1 retreat. The battle ends when the
//! battle layer decides it or the campaign's tick cap passes; the outcome
//! is read off the field as the headless resolver reads it, the field is
//! cleared, and the round resumes with it.
//!
//! For the user on the Mac: the battle's framing (the bases are 1500 apart),
//! and the feel of commanding and retreating.

use bevy::prelude::*;

use onus::campaign::story::Story;
use onus::campaign::{BattlePlan, Campaign, CampaignContent, PlayedBattle, RoundStep, Side};
use onus::campaign_client::{live_settings, CampaignSession, LiveBattle, MapLayout, Screen};
use onus::client::PLAYER_FACTION;
use onus::headless;
use onus::sim::battle::{BattleState, General};
use onus::sim::content::Content;
use onus::sim::economy::{Building, UnitDefIdx};
use onus::sim::nation::NationRules;
use onus::sim::spatial::Faction;
use onus::sim::Position;
use onus::sim::{AiCommanders, TickGate};

use super::client::{app, hold, press, screen, sess};

const OTHER: Faction = Faction::B;

/// A live session for House on `seed`, the campaign's battle tick cap
/// `cap` (`None`: the shipped one).
fn session_capped(seed: u64, cap: Option<u32>) -> CampaignSession {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data");
    let content = Content::load_from_dir(&dir).unwrap();
    let mut cc = CampaignContent::shipped_with_diplomacy(&content).unwrap();
    if let Some(cap) = cap {
        cc.rules.battle.tick_cap = cap;
    }
    let c = Campaign::new(content, cc, seed, Some("house")).unwrap().with_live_battles();
    let story = Story::shipped(c.content(), c.campaign()).unwrap();
    let layout = MapLayout::shipped(c.campaign()).unwrap();
    CampaignSession::new(c, story, layout, Box::new(hold))
}

/// Holding every round until the first battle of the player's.
fn to_battle(seed: u64, cap: Option<u32>) -> App {
    let mut app = app(session_capped(seed, cap));
    press(&mut app, KeyCode::Enter);
    for _ in 0..20 {
        if screen(&app) == Screen::Battle {
            return app;
        }
        assert_eq!(screen(&app), Screen::Map);
        press(&mut app, KeyCode::Enter);
    }
    panic!("no battle of the player's in 20 rounds");
}

fn plan(app: &App) -> BattlePlan {
    sess(app).battle.clone().expect("paused at a battle")
}

/// The player's side of the plan, and the opponent's.
fn sides(app: &App, plan: &BattlePlan) -> (Side, Side) {
    if Some(plan.attack.nation) == sess(app).campaign.player() {
        (Side::Attack, Side::Defend)
    } else {
        (Side::Defend, Side::Attack)
    }
}

fn count<F: bevy::ecs::query::QueryFilter>(app: &mut App, f: Faction) -> usize {
    let world = app.world_mut();
    let mut q = world.query_filtered::<&Faction, F>();
    q.iter(world).filter(|x| **x == f).count()
}

/// Step the battle until the client leaves the Battle screen.
fn fight_out(app: &mut App, max: u32) -> u32 {
    let mut n = 0;
    while screen(app) == Screen::Battle {
        assert!(n < max, "the battle is still on after {max} ticks");
        headless::step(app);
        n += 1;
    }
    n
}

fn last_played(app: &App) -> PlayedBattle {
    sess(app).campaign.played().last().and_then(|r| r.last()).cloned().expect("a played battle")
}

#[test]
fn the_round_pauses_into_the_plans_battle_with_the_player_on_his_faction() {
    let mut app = to_battle(3, None);
    let p = plan(&app);
    let (me, them) = sides(&app, &p);
    let side = |s: Side| if s == Side::Attack { &p.attack } else { &p.defend };
    let (mine, theirs) = (side(me), side(them));
    let content = sess(&app).campaign.content().clone();
    assert!(app.world().get_resource::<LiveBattle>().is_some());
    assert!(app.world().resource::<TickGate>().is_open(), "the battle ticks");
    assert!(app.world().get_resource::<BattleState>().is_some());
    // The player's nation on his faction, the opponent's on the other.
    let rules = app.world().resource::<NationRules>();
    assert_eq!(rules.nation_of(PLAYER_FACTION), Some(mine.nation));
    assert_eq!(rules.nation_of(OTHER), Some(theirs.nation));
    // Only the opponent has an AI, on his nation's strategy.
    let ai = app.world().resource::<AiCommanders>().commanders();
    assert_eq!(ai.len(), 1);
    assert_eq!(ai[0].faction, OTHER);
    assert_eq!(ai[0].strategy(&content).id, content.nations[theirs.nation].strategy);
    // Each side's plan on the field.
    assert_eq!(count::<With<General>>(&mut app, PLAYER_FACTION), mine.generals.len());
    assert_eq!(count::<With<General>>(&mut app, OTHER), theirs.generals.len());
    let units = |app: &mut App, f| count::<(With<UnitDefIdx>, Without<General>)>(app, f);
    let workers = sess(&app).campaign.campaign().rules.battle.clone();
    let (wm, wt) = if me == Side::Attack {
        (workers.attacker_workers, workers.defender_workers)
    } else {
        (workers.defender_workers, workers.attacker_workers)
    };
    assert_eq!(units(&mut app, PLAYER_FACTION), mine.units.len() + wm as usize);
    assert_eq!(units(&mut app, OTHER), theirs.units.len() + wt as usize);
    let holder = if me == Side::Defend { PLAYER_FACTION } else { OTHER };
    assert_eq!(count::<With<Building>>(&mut app, holder), p.buildings.len());
    // The plan's map side: the attacker stands left unless it drew the
    // reflected map, whichever faction the player commands.
    let attacker = if me == Side::Attack { PLAYER_FACTION } else { OTHER };
    let world = app.world_mut();
    let mut q = world.query_filtered::<(&Position, &Faction), With<General>>();
    for (pos, f) in q.iter(world) {
        if *f == attacker {
            assert_eq!(pos.0.x > 0.0, p.flipped, "the attacker's map side is the plan's");
        }
    }
}

#[test]
fn the_battle_ends_at_the_cap_and_the_round_resumes_with_the_field_as_it_stands() {
    let mut app = to_battle(3, Some(20));
    let p = plan(&app);
    let before = sess(&app).campaign.clone();
    let ticks = fight_out(&mut app, 25);
    assert!(ticks >= 20, "the battle runs to the cap: {ticks}");
    let played = last_played(&app);
    assert_eq!(played.city, before.campaign().cities[p.city].id);
    let o = &played.outcome;
    assert_eq!(o.ticks, 20);
    assert_eq!(o.winner, None, "at the cap the defender holds");
    // Twenty ticks is no contact: everyone walks off as he came on, read
    // off the field unit by unit in plan order.
    assert_eq!(o.attack.units, p.attack.units.iter().map(|u| u.hp).collect::<Vec<_>>());
    assert_eq!(o.defend.units, p.defend.units.iter().map(|u| u.hp).collect::<Vec<_>>());
    assert_eq!(o.buildings, p.buildings.iter().map(|b| b.hp).collect::<Vec<_>>());
    assert!(o.attack.generals.iter().chain(&o.defend.generals).all(|g| *g));
    // The field is cleared and the sim stopped.
    let world = app.world_mut();
    assert_eq!(world.query::<&Position>().iter(world).count(), 0, "the battle world is cleared");
    assert!(world.get_resource::<LiveBattle>().is_none());
    assert!(world.get_resource::<BattleState>().is_none());
    assert!(!world.resource::<TickGate>().is_open(), "no sim ticks outside a battle");
    // The round resumed with that outcome.
    let mut twin = before;
    // No orders were staged: the round's decisions are none.
    let ds: Vec<onus::campaign::Decision> = Vec::new();
    match twin.step_round(&ds, std::slice::from_ref(&played), &mut hold).unwrap() {
        RoundStep::Played(_) => assert_eq!(twin.state_hash(), sess(&app).campaign.state_hash()),
        RoundStep::Battle(_) => assert_eq!(screen(&app), Screen::Battle, "the round's next battle"),
    }
}

/// Critic C6 r1 #2: a tick cap of 0 (the content refuses it on load, but
/// the rules are open to a caller) ends the battle at once, unplayed,
/// rather than running the sim without end.
#[test]
fn a_tick_cap_of_0_ends_the_battle_at_once_with_no_tick_played() {
    let mut app = app(session_capped(3, Some(0)));
    press(&mut app, KeyCode::Enter);
    // Holding round after round until a battle of the player's is played:
    // each one ends in the frame it is fielded, so the client never rests
    // on the Battle screen.
    for _ in 0..40 {
        if sess(&app).campaign.played().iter().any(|r| !r.is_empty()) {
            break;
        }
        assert_ne!(screen(&app), Screen::Battle, "a battle on a cap of 0 is still on");
        press(&mut app, KeyCode::Enter);
    }
    let played = last_played(&app);
    assert_eq!(played.outcome.ticks, 0, "no tick was played");
    assert_eq!(played.outcome.winner, None, "unplayed, the defender holds");
    assert_eq!(app.world().resource::<onus::sim::MatchState>().tick(), 0, "the sim never ticked");
    assert!(!app.world().resource::<TickGate>().is_open());
}

#[test]
fn the_retreat_key_issues_the_c1_retreat() {
    let mut app = to_battle(3, None);
    let p = plan(&app);
    let (me, them) = sides(&app, &p);
    press(&mut app, KeyCode::KeyT);
    let cap = sess(&app).campaign.campaign().rules.battle.tick_cap;
    fight_out(&mut app, cap + 10);
    let o = last_played(&app).outcome;
    assert_eq!(o.winner, Some(them), "a side that retreats loses the battle");
    let mine = if me == Side::Attack { &o.attack } else { &o.defend };
    assert!(mine.units.iter().any(|&hp| hp > 0), "the retreat walked units off the field");
}

/// Whichever side the player is on, the settings put him on his faction and
/// the attacker on the plan's map side: checked for both sides of one plan
/// (the client fields only the player's).
#[test]
fn both_sides_of_a_plan_field_the_player_on_his_faction_and_the_attacker_on_the_plans_side() {
    let app = to_battle(3, None);
    let p = plan(&app);
    let c = sess(&app).campaign.clone();
    for (me, flipped) in [(Side::Attack, false), (Side::Attack, true), (Side::Defend, false), (Side::Defend, true)] {
        let mut p = p.clone();
        p.flipped = flipped;
        let (settings, factions, strategy) = live_settings(c.content(), c.campaign(), &p, me);
        let (mine, theirs) = if me == Side::Attack { (&p.attack, &p.defend) } else { (&p.defend, &p.attack) };
        assert_eq!(factions[if me == Side::Attack { 0 } else { 1 }], PLAYER_FACTION);
        assert_eq!(strategy, c.content().nations[theirs.nation].strategy);
        let mut app = headless::battle(c.content().clone(), &settings).unwrap();
        let rules = app.world().resource::<NationRules>();
        assert_eq!(rules.nation_of(PLAYER_FACTION), Some(mine.nation));
        assert_eq!(rules.nation_of(OTHER), Some(theirs.nation));
        assert_eq!(count::<With<General>>(&mut app, PLAYER_FACTION), mine.generals.len());
        let holder = if me == Side::Defend { PLAYER_FACTION } else { OTHER };
        assert_eq!(count::<With<Building>>(&mut app, holder), p.buildings.len());
        let attacker = factions[0];
        let world = app.world_mut();
        let mut q = world.query_filtered::<(&Position, &Faction), With<General>>();
        for (pos, f) in q.iter(world) {
            assert_eq!((pos.0.x > 0.0) == flipped, *f == attacker, "{me:?} flipped {flipped}: {f:?} at {}", pos.0.x);
        }
    }
}
