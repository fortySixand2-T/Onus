//! C3 AC7: **the golden licence**. The default (Alloy-only) economy
//! reproduces today's pinned results exactly. The pins are the existing golden
//! tests (c1_golden, c2_golden, critic_b35_ac0, b1_matchup's journal pins,
//! m5's replays), which C3 leaves untouched. This file adds what they cannot
//! say: the three-resource block is inert in an Alloy-only match, and a match
//! log records its economy, which a replay applies or refuses.

use bevy::prelude::*;
use std::path::PathBuf;

use onus::headless::{self, MatchSettings};
use onus::sim::content::Content;
use onus::sim::nation::NationRules;
use onus::sim::three::{EconomyMode, LeyWell, ThreeEconomy};
use onus::sim::{
    AiCommanders, AiJournal, CommandLog, Faction, MatchLog, MatchState, ReplaySource,
    StateHashLog,
};

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data")
}

fn shipped() -> Content {
    let c = Content::load_from_dir(&data_dir()).expect("content loads");
    assert!(c.three_resource.is_some(), "the shipped content has the block");
    c
}

/// The shipped content as it stood before C3: no `three_resource` block, no
/// three-resource trait lists.
fn before_c3() -> Content {
    let mut c = shipped();
    c.three_resource = None;
    for n in &mut c.nations {
        n.three_resource_traits.clear();
    }
    c
}

type Trace = (Vec<u64>, usize, Option<u32>);

fn play(c: Content, s: &MatchSettings, ticks: u32) -> Trace {
    let mut app = headless::ai_vs_ai(c, &s.clone().with_hashing(true)).expect("builds");
    headless::tick(&mut app, ticks);
    assert!(!app.world().contains_resource::<ThreeEconomy>());
    let hashes = app.world().resource::<StateHashLog>().0.clone();
    let journal = app.world().resource::<AiJournal>().0.len();
    let end = app.world().resource::<MatchState>().outcome().map(|o| o.tick);
    (hashes, journal, end)
}

#[test]
fn alloy_only_is_the_default_economy() {
    assert_eq!(MatchSettings::default().economy, EconomyMode::Alloy);
    assert_eq!(MatchLog::default().economy, EconomyMode::Alloy);
}

#[test]
fn an_alloy_only_match_plays_identically_with_and_without_the_three_resource_content() {
    for (seed, a, b) in [(0u64, None, None), (4, Some("rush"), Some("turtle"))] {
        let mut s = MatchSettings::default().with_seed(seed);
        if let (Some(a), Some(b)) = (a, b) {
            s = s.with_strategies(a, b);
        }
        let with = play(shipped(), &s, 1_800);
        assert_eq!(with.0.len(), 1_800);
        assert_eq!(with, play(before_c3(), &s, 1_800), "seed {seed}");
        // Nations too: the Alloy-only list is the one played.
        let n = s.clone().with_nations(Some("tollmarch"), Some("verdant"));
        assert_eq!(play(shipped(), &n, 1_200), play(before_c3(), &n, 1_200), "seed {seed}");
    }
}

/// Record a match, AI against AI, and return its log as it comes back from
/// disk, plus the recording's hashes.
fn record(s: &MatchSettings, ticks: u32) -> (MatchLog, StateHashLog) {
    let mut rec = headless::ai_vs_ai(shipped(), &s.clone().with_hashing(true)).unwrap();
    headless::tick(&mut rec, ticks);
    let hashes = rec.world().resource::<StateHashLog>().clone();
    let log = rec.world().resource::<CommandLog>().log().clone();
    (MatchLog::from_ron(&log.to_ron().unwrap()).unwrap(), hashes)
}

/// Replay `log` into a world built from `setup` with the log's seed: the
/// replay has to bring whatever the setup did not.
fn replay(content: Content, log: MatchLog, setup: MatchSettings, ticks: u32) -> App {
    let rs = setup.with_seed(log.seed).with_hashing(true);
    let mut rep = headless::ai_vs_ai(content, &rs).unwrap();
    rep.insert_resource(AiCommanders::default());
    rep.insert_resource(StateHashLog::default());
    rep.insert_resource(CommandLog::new(log.seed));
    rep.insert_resource(ReplaySource::new(log));
    headless::tick(&mut rep, ticks);
    rep
}

fn wells(app: &mut App) -> usize {
    let mut q = app.world_mut().query::<&LeyWell>();
    q.iter(app.world()).count()
}

#[test]
fn an_alloy_only_log_is_written_as_before() {
    let (log, _) = record(&MatchSettings::default().with_seed(3), 300);
    assert_eq!(log.economy, EconomyMode::Alloy);
    let text = log.to_ron().unwrap();
    assert!(!text.contains("economy"), "an Alloy-only log is byte-for-byte pre-C3");
    // ...so a pre-C3 log, which has no such field, reads as Alloy-only.
    assert_eq!(MatchLog::from_ron(&text).unwrap().economy, EconomyMode::Alloy);
}

#[test]
fn a_three_resource_log_records_its_economy_and_replays_it_faithfully() {
    const TICKS: u32 = 2_400;
    let s = MatchSettings::default()
        .with_seed(9)
        .with_economy(EconomyMode::ThreeResource);
    let (log, recorded) = record(&s, TICKS);
    assert_eq!(log.economy, EconomyMode::ThreeResource, "the sim stamps the economy");
    assert!(log.to_ron().unwrap().contains("economy"));
    assert!(log.commands.len() > 10, "the AI played: {}", log.commands.len());
    // Into a world set up Alloy-only: the replay installs the economy itself.
    let mut rep = replay(shipped(), log.clone(), MatchSettings::default(), TICKS);
    {
        let src = rep.world().resource::<ReplaySource>();
        assert_eq!(src.rejection(), None, "a faithful log is not refused");
        assert!(src.finished());
    }
    assert!(rep.world().contains_resource::<ThreeEconomy>());
    let t = shipped().three_resource.unwrap();
    assert_eq!(wells(&mut rep), t.wells.sites.len());
    let replayed = rep.world().resource::<StateHashLog>().clone();
    assert_eq!(recorded.first_divergence(&replayed), None);
    // The replay's own log names the same economy: a replay of a replay holds.
    assert_eq!(
        rep.world().resource::<CommandLog>().log().economy,
        EconomyMode::ThreeResource
    );
    // Into a world already set up three-resource: nothing to install.
    let rep = replay(shipped(), log, s.clone(), TICKS);
    assert_eq!(rep.world().resource::<ReplaySource>().rejection(), None);
    let replayed = rep.world().resource::<StateHashLog>().clone();
    assert_eq!(recorded.first_divergence(&replayed), None);
}

#[test]
fn a_three_resource_nation_log_replays_its_leans_into_an_alloy_set_up_world() {
    const TICKS: u32 = 1_800;
    let s = MatchSettings::default()
        .with_seed(5)
        .with_economy(EconomyMode::ThreeResource)
        .with_nations(Some("ashen_reach"), Some("forgeborn"));
    let (log, recorded) = record(&s, TICKS);
    // The setup named the nations Alloy-only (their stand-ins installed): the
    // replay re-installs them under the log's economy (their leans).
    let setup = MatchSettings::default().with_nations(Some("ashen_reach"), Some("forgeborn"));
    let rep = replay(shipped(), log, setup, TICKS);
    assert_eq!(rep.world().resource::<ReplaySource>().rejection(), None);
    let rules = rep.world().resource::<NationRules>();
    let c = shipped();
    let hq = c.building_index("hq").unwrap();
    let ashen = c.nations.iter().find(|n| n.id == "ashen_reach").unwrap();
    assert!(!ashen.three_resource_traits.is_empty());
    assert!(
        rules.side(Faction::B).unwrap().building_pools[hq]
            > c.buildings[hq].mvp_defense * c.combat.building_hp_per_defense,
        "Forgeborn plays its three-resource lean (sturdier buildings)"
    );
    let replayed = rep.world().resource::<StateHashLog>().clone();
    assert_eq!(recorded.first_divergence(&replayed), None);
}

#[test]
fn a_three_resource_log_is_refused_by_content_without_the_block() {
    let s = MatchSettings::default()
        .with_seed(2)
        .with_economy(EconomyMode::ThreeResource);
    let (log, _) = record(&s, 120);
    let text = log.to_ron().unwrap();
    let mut lacking = shipped();
    lacking.three_resource = None;
    let why = MatchLog::from_ron_for(&text, &lacking).unwrap_err();
    assert!(why.contains("three-resource"), "{why}");
}

#[test]
fn a_replay_into_a_world_of_another_economy_is_refused() {
    // Recorded Alloy-only, replayed into a three-resource world.
    let (log, _) = record(&MatchSettings::default().with_seed(1), 300);
    let three = MatchSettings::default().with_economy(EconomyMode::ThreeResource);
    let rep = replay(shipped(), log, three, 60);
    let src = rep.world().resource::<ReplaySource>();
    let why = src.rejection().expect("refused");
    assert!(why.contains("economy"), "{why}");
    assert_eq!(src.cursor(), 0, "nothing of a refused log is fed");
}

#[test]
fn a_three_resource_log_replayed_into_a_match_under_way_is_refused() {
    let s = MatchSettings::default()
        .with_seed(6)
        .with_economy(EconomyMode::ThreeResource);
    let (log, _) = record(&s, 300);
    let mut rep = headless::ai_vs_ai(shipped(), &MatchSettings::default().with_seed(6)).unwrap();
    headless::tick(&mut rep, 10);
    rep.insert_resource(AiCommanders::default());
    rep.insert_resource(ReplaySource::new(log));
    headless::tick(&mut rep, 5);
    let src = rep.world().resource::<ReplaySource>();
    let why = src.rejection().expect("refused");
    assert!(why.contains("economy"), "{why}");
    assert!(!rep.world().contains_resource::<ThreeEconomy>(), "nothing installed");
}
