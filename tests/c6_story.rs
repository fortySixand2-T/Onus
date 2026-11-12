//! C6 AC1 — **story content**: `assets/data/story.ron` holds every story
//! beat (an id, a trigger, a title and its text), validated at load against
//! the battle and campaign content, and a render-free rule
//! (`campaign::story`) reads the beats off campaign state: each fires once,
//! the first time its trigger holds, in file order. The story never changes
//! campaign state or its hash.
//!
//! The readings (F-099 S11–S13): vengeance is a capture of the player's that
//! absorbs the nation (read from the battle history); glory is any General
//! of the player's first holding the rank; legacy is the player's win; a loss
//! beat fires when he loses. The start beat fires before round 1.

use std::path::PathBuf;

use onus::campaign::story::{Story, StoryProgress, Trigger};
use onus::campaign::{BattleOutcome, BattlePlan, Campaign, CampaignContent, CampaignOutcome, Decision, Resolver, Side};
use onus::sim::content::Content;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data")
}
fn content() -> Content {
    Content::load_from_dir(&data_dir()).expect("content loads")
}
fn campaign_content() -> CampaignContent {
    CampaignContent::load_from_dir(&data_dir(), &content()).expect("campaign loads")
}
fn story() -> Story {
    Story::shipped(&content(), &campaign_content()).unwrap_or_else(|e| panic!("{e}"))
}
fn story_text() -> String {
    std::fs::read_to_string(data_dir().join("story.ron")).expect("story.ron")
}

/// House's assaults always succeed; everyone else's fail.
fn favour(n: usize) -> impl FnMut(&BattlePlan) -> BattleOutcome {
    move |p: &BattlePlan| {
        let w = if p.attack.nation == n { Side::Attack } else { Side::Defend };
        BattleOutcome::unchanged(p, Some(w))
    }
}

fn house_army(c: &Campaign) -> u32 {
    let h = c.nation_index("house").unwrap();
    c.armies().iter().find(|a| a.nation == h).unwrap().id
}

/// Walk army `army` to `to`, a road at a time, advancing the story after
/// every round; returns `(round, beat id)` for every beat that fired.
fn walk(
    c: &mut Campaign,
    r: &mut dyn Resolver,
    s: &Story,
    p: &mut StoryProgress,
    army: u32,
    to: &str,
) -> Vec<(u32, String)> {
    let target = c.city_index(to).unwrap();
    let dist = c.distances(target);
    let mut fired = Vec::new();
    for _ in 0..80 {
        if c.is_over() {
            return fired;
        }
        let Some(a) = c.army(army) else { return fired };
        if a.at() == Some(target) {
            return fired;
        }
        let ds = match a.at() {
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
        for k in p.advance(s, c) {
            fired.push((c.round(), s.beats()[k].id.clone()));
        }
    }
    panic!("army {army} never reached {to}");
}

#[test]
fn the_shipped_story_has_the_hooks_beats() {
    let s = story();
    let count = |f: &dyn Fn(&Trigger) -> bool| s.beats().iter().filter(|b| f(&b.trigger)).count();
    assert_eq!(count(&|t| *t == Trigger::Start), 1, "one Rupture beat opens the campaign");
    assert_eq!(count(&|t| *t == Trigger::Won), 1, "one legacy beat");
    assert!(count(&|t| matches!(t, Trigger::CapitalTaken(_) | Trigger::AnyCapitalTaken)) >= 1, "vengeance");
    assert!(count(&|t| matches!(t, Trigger::RankReached(_))) >= 1, "glory");
    assert_eq!(count(&|t| *t == Trigger::Lost), 1, "a loss beat");
    for b in s.beats() {
        assert!(!b.title.trim().is_empty() && !b.text.trim().is_empty(), "{}", b.id);
    }
    let start = s.beats().iter().find(|b| b.trigger == Trigger::Start).unwrap();
    assert!(start.text.contains("Rupture"), "the start beat tells of the Rupture");
}

/// Story RON from `(id, trigger)` pairs, each with a title and a text.
fn mk(beats: &[(&str, &str)]) -> String {
    let mut t = String::from("(beats: [\n");
    for (id, trigger) in beats {
        t += &format!("(id: \"{id}\", trigger: {trigger}, title: \"T\", text: \"x\"),\n");
    }
    t + "])"
}

#[test]
fn a_bad_story_is_refused_at_load() {
    let (c, cc) = (content(), campaign_content());
    let ok = story_text();
    assert!(Story::from_ron(&ok, &c, &cc).is_ok());
    let minimal = [("a", "Start"), ("b", "Won"), ("c", "AnyCapitalTaken"), ("d", "RankReached(1)")];
    assert!(Story::from_ron(&mk(&minimal), &c, &cc).is_ok(), "the minimal story loads");
    let refuse = |text: &str, says: &str| {
        let e = Story::from_ron(text, &c, &cc).expect_err(says).to_string();
        assert!(e.contains(says), "`{e}` should say `{says}`");
    };
    let with = |k: usize, beat: (&'static str, &'static str)| {
        let mut b = minimal.to_vec();
        b[k] = beat;
        mk(&b)
    };
    let plus = |beat: (&'static str, &'static str)| {
        let mut b = minimal.to_vec();
        b.push(beat);
        mk(&b)
    };
    refuse(&plus(("a", "Lost")), "repeated");
    refuse(&plus(("e", "CapitalTaken(\"atlantis\")")), "unknown nation");
    refuse(&plus(("e", "RankReached(0)")), "rank 0");
    refuse(&plus(("e", "RankReached(3)")), "does not exist");
    refuse(&plus(("e", "Start")), "share the trigger");
    refuse(&plus(("e", "AnyCapitalTaken")), "share the trigger");
    refuse(&with(0, ("a", "Lost")), "Start");
    refuse(&with(1, ("b", "Lost")), "Won");
    refuse(&with(2, ("c", "Lost")), "vengeance");
    refuse(&with(3, ("d", "Lost")), "glory");
    refuse(&mk(&minimal).replacen("title: \"T\"", "title: \" \"", 1), "empty title");
    refuse(&mk(&minimal).replacen("text: \"x\"", "text: \"\"", 1), "empty text");
    refuse(&mk(&minimal).replacen("id: \"a\"", "id: \"\"", 1), "empty id");
    refuse("(beats: [", "<story>");
    refuse(&plus(("e", "Nightfall")), "<story>");
}

#[test]
fn the_start_beat_fires_once_before_round_one() {
    let s = story();
    let c = Campaign::new(content(), campaign_content(), 4, Some("house")).unwrap();
    let mut p = StoryProgress::new(&s);
    let first = p.advance(&s, &c);
    assert_eq!(first.len(), 1);
    assert_eq!(s.beats()[first[0]].trigger, Trigger::Start);
    assert!(p.advance(&s, &c).is_empty(), "a beat fires once");
    assert!(p.fired()[first[0]]);
}

#[test]
fn vengeance_fires_when_the_players_capture_absorbs_the_nation_and_glory_at_a_rank() {
    let s = story();
    let mut c = Campaign::new(content(), campaign_content(), 5, Some("house")).unwrap();
    let h = c.nation_index("house").unwrap();
    let forge = c.nation_index("forgeborn").unwrap();
    let mut p = StoryProgress::new(&s);
    p.advance(&s, &c);
    let army = house_army(&c);
    let mut r = favour(h);
    let fired = walk(&mut c, &mut r, &s, &mut p, army, "ironspire");
    eprintln!("{fired:?}");
    assert!(!c.nations()[forge].alive, "Ironspire taken");
    let absorbed_round = c
        .history()
        .iter()
        .find(|b| b.absorbed && b.attacker == h && b.defender == forge)
        .expect("House absorbed the Forgeborn")
        .round;
    let at = |id: &str| fired.iter().find(|(_, b)| b == id).map(|(r, _)| *r);
    assert_eq!(at("vengeance_forgeborn"), Some(absorbed_round), "vengeance in the round of the capture");
    assert_eq!(at("vengeance_any"), None, "the named beat takes the Forgeborn, not the general one");
    // Glory: rank 1 (xp 3) after his first won battle, in or after it.
    let glory = at("glory_1").expect("glory fired");
    let first_win = c
        .history()
        .iter()
        .filter(|b| b.attacker == h && b.winner == Some(Side::Attack))
        .map(|b| b.round)
        .min()
        .expect("House won a battle");
    assert!(glory >= first_win, "glory follows a won battle");
    let rank_now = c
        .armies()
        .iter()
        .filter(|a| a.nation == h)
        .map(|a| c.campaign().rank_of(a.general.xp))
        .max()
        .unwrap();
    assert!(rank_now >= 1);
    // Each beat once.
    let mut ids: Vec<&String> = fired.iter().map(|(_, b)| b).collect();
    ids.sort();
    let n = ids.len();
    ids.dedup();
    assert_eq!(ids.len(), n, "no beat fired twice");
}

#[test]
fn a_capital_taken_by_someone_else_is_no_vengeance_and_a_fall_fires_the_loss_beat() {
    let s = story();
    let mut c = Campaign::new(content(), campaign_content(), 7, Some("house")).unwrap();
    let mut p = StoryProgress::new(&s);
    p.advance(&s, &c);
    // Every assault succeeds; House holds and never marches.
    let mut r = |plan: &BattlePlan| BattleOutcome::unchanged(plan, Some(Side::Attack));
    let mut fired = Vec::new();
    while !c.is_over() {
        c.play_round(&[], &mut r).unwrap();
        for k in p.advance(&s, &c) {
            fired.push(s.beats()[k].id.clone());
        }
    }
    eprintln!("outcome {:?}, fired {fired:?}", c.outcome());
    let h = c.nation_index("house").unwrap();
    assert!(c.history().iter().any(|b| b.absorbed && b.attacker != h), "someone else took a capital");
    assert!(!fired.iter().any(|b| b.starts_with("vengeance")), "not the heir's vengeance");
    assert_eq!(c.outcome(), Some(CampaignOutcome::PlayerLost));
    assert_eq!(fired.last().map(String::as_str), Some("fallen"), "the loss beat");
    assert!(!fired.iter().any(|b| b == "legacy"));
}

#[test]
fn the_legacy_beat_fires_when_the_heir_wins() {
    let t = std::fs::read_to_string(data_dir().join("campaign.ron")).unwrap();
    let cap = "round_cap: 40,";
    assert!(t.contains(cap));
    let cc = CampaignContent::from_ron(&t.replacen(cap, "round_cap: 400,", 1), &content()).unwrap();
    let s = Story::shipped(&content(), &cc).unwrap();
    let mut c = Campaign::new(content(), cc, 5, Some("house")).unwrap();
    let h = c.nation_index("house").unwrap();
    let mut p = StoryProgress::new(&s);
    p.advance(&s, &c);
    let army = house_army(&c);
    let mut r = favour(h);
    let mut fired = Vec::new();
    for capital in ["ironspire", "tollgate", "ashgrave", "greenmere", "leyhold"] {
        fired.extend(walk(&mut c, &mut r, &s, &mut p, army, capital));
        if c.is_over() {
            break;
        }
    }
    eprintln!("outcome {:?} round {}, fired {fired:?}", c.outcome(), c.round());
    assert_eq!(c.outcome(), Some(CampaignOutcome::Won { nation: h }));
    let ids: Vec<&str> = fired.iter().map(|(_, b)| b.as_str()).collect();
    assert_eq!(ids.last(), Some(&"legacy"), "legacy closes the story");
    assert!(ids.contains(&"vengeance_verdant") && ids.contains(&"vengeance_any"), "{ids:?}");
    assert!(!ids.contains(&"fallen"));
}

#[test]
fn an_ai_only_campaign_fires_only_the_start_and_the_story_never_touches_the_hash() {
    let s = story();
    let mut c = Campaign::new(content(), campaign_content(), 2, None).unwrap();
    let mut p = StoryProgress::new(&s);
    let mut r = |plan: &BattlePlan| BattleOutcome::unchanged(plan, Some(Side::Attack));
    let mut fired = Vec::new();
    for _ in 0..12 {
        let before = c.state_hash();
        for k in p.advance(&s, &c) {
            fired.push(s.beats()[k].id.clone());
        }
        assert_eq!(c.state_hash(), before, "reading the story changes nothing");
        if c.is_over() {
            break;
        }
        c.play_round(&[], &mut r).unwrap();
    }
    assert_eq!(fired, vec!["rupture".to_string()]);
}
