//! C1 AC8 — **the golden licence**: skirmish mode reproduces today's pinned
//! results exactly. The pins themselves are the existing golden tests (b1,
//! b2, b35, critic_b2_ac4's state hashes, m5's replays, critic_p2's
//! allocation order), which C1 leaves untouched and must pass as they are.
//! This file adds what those cannot say: that the campaign layer is
//! **inert** in a skirmish. The shipped content and the same content with
//! the whole C1 section cut out play the same skirmish, tick for tick and
//! hash for hash, and decide the same things.

use std::path::PathBuf;

use bevy::prelude::*;

use onus::headless::{self, MatchSettings};
use onus::sim::battle::{AuraField, BattleState, General};
use onus::sim::content::Content;
use onus::sim::{AiJournal, MatchState, StateHashLog};

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data")
}

fn shipped() -> Content {
    Content::load_from_dir(&data_dir()).expect("content loads")
}

/// The shipped data with everything from the C1 marker to the end of the
/// units file removed — the content as it stood before the campaign layer.
fn without_campaign_layer(name: &str) -> Content {
    let out = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/c1_golden")
        .join(name);
    std::fs::create_dir_all(&out).unwrap();
    for file in ["resources.ron", "strategies.ron"] {
        std::fs::copy(data_dir().join(file), out.join(file)).unwrap();
    }
    let text = std::fs::read_to_string(data_dir().join("units.ron")).unwrap();
    let at = text
        .find("    // ---- C1: the campaign battle layer")
        .expect("the C1 marker");
    let stripped = format!("{})\n", &text[..at]);
    std::fs::write(out.join("units.ron"), stripped).unwrap();
    let c = Content::load_from_dir(&out).expect("pre-C1 content loads");
    assert!(c.generals.is_empty() && c.battle.is_none() && c.general_ai.is_none());
    c
}

fn play(c: Content, settings: &MatchSettings, ticks: u32) -> (Vec<u64>, usize, Option<u32>) {
    let mut app = headless::ai_vs_ai(c, &settings.clone().with_hashing(true)).expect("builds");
    headless::tick(&mut app, ticks);
    let hashes = app.world().resource::<StateHashLog>().0.clone();
    let journal = app.world().resource::<AiJournal>().0.len();
    let end = app
        .world()
        .resource::<MatchState>()
        .outcome()
        .map(|o| o.tick);
    (hashes, journal, end)
}

#[test]
fn the_default_match_is_a_skirmish_with_no_campaign_state() {
    assert!(MatchSettings::default().battle.is_none());
    let mut app = headless::ai_vs_ai(shipped(), &MatchSettings::default()).unwrap();
    headless::tick(&mut app, 60);
    assert!(!app.world().contains_resource::<BattleState>());
    assert!(!app.world().contains_resource::<AuraField>());
    let mut q = app.world_mut().query_filtered::<Entity, With<General>>();
    assert_eq!(q.iter(app.world()).count(), 0);
}

#[test]
fn a_skirmish_plays_identically_with_and_without_the_campaign_layer() {
    for seed in [0u64, 4, 11] {
        let s = MatchSettings::default().with_seed(seed);
        let with = play(shipped(), &s, 1_800);
        let without = play(without_campaign_layer("seeds"), &s, 1_800);
        assert_eq!(with.0.len(), 1_800, "seed {seed}: one hash per tick");
        assert_eq!(
            with, without,
            "seed {seed}: the campaign layer reached a skirmish"
        );
    }
}

#[test]
fn named_strategies_play_identically_with_and_without_the_campaign_layer() {
    for (a, b) in [("rush", "turtle"), ("mass_ripper", "mass_bulwark")] {
        let s = MatchSettings::default().with_strategies(a, b);
        let with = play(shipped(), &s, 3_600);
        let without = play(without_campaign_layer("named"), &s, 3_600);
        assert_eq!(
            with, without,
            "{a} vs {b}: the campaign layer reached a skirmish"
        );
    }
}
