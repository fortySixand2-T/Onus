//! C2 AC2 — **the golden licence**: the "no nation" default reproduces
//! today's pinned results exactly. The pins themselves are the existing golden
//! tests (b1, b2, b35, critic_b2_ac4's state hashes, m5's replays, critic_p2's
//! allocation order, c1_golden), which C2 leaves untouched and must pass as
//! they are — no pin is re-written. This file adds what those cannot say:
//! that the nation layer is **inert** when no side names a nation. The
//! shipped content and the same content with `nations.ron` removed (the
//! content as it stood before C2) play the same matches, skirmish and campaign
//! battle, tick for tick and hash for hash, and decide the same things.

use std::path::PathBuf;

use onus::headless::{self, MatchSettings};
use onus::sim::battle::BattleSetup;
use onus::sim::content::Content;
use onus::sim::nation::NationRules;
use onus::sim::{AiJournal, Faction, MatchState, StateHashLog};

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data")
}

fn shipped() -> Content {
    let c = Content::load_from_dir(&data_dir()).expect("content loads");
    assert!(!c.nations.is_empty(), "the shipped content has nations");
    c
}

/// The shipped data without `nations.ron`: the content before C2.
fn without_nations(name: &str) -> Content {
    let out = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/c2_golden")
        .join(name);
    std::fs::create_dir_all(&out).unwrap();
    for file in ["units.ron", "resources.ron", "strategies.ron"] {
        std::fs::copy(data_dir().join(file), out.join(file)).unwrap();
    }
    let _ = std::fs::remove_file(out.join("nations.ron"));
    let c = Content::load_from_dir(&out).expect("pre-C2 content loads");
    assert!(c.nations.is_empty());
    c
}

type Trace = (Vec<u64>, usize, Option<u32>, bool);

fn play(c: Content, settings: &MatchSettings, ticks: u32) -> Trace {
    let mut app = headless::ai_vs_ai(c, &settings.clone().with_hashing(true)).expect("builds");
    headless::tick(&mut app, ticks);
    let hashes = app.world().resource::<StateHashLog>().0.clone();
    let journal = app.world().resource::<AiJournal>().0.len();
    let end = app
        .world()
        .resource::<MatchState>()
        .outcome()
        .map(|o| o.tick);
    let rules = app.world().contains_resource::<NationRules>();
    (hashes, journal, end, rules)
}

#[test]
fn naming_no_nation_is_the_default() {
    let d = MatchSettings::default();
    assert_eq!(d.nations, [None, None]);
    let explicit = d.clone().with_nations(None, None);
    assert_eq!(explicit.nations, d.nations);
    for seed in [0u64, 5] {
        let s = MatchSettings::default().with_seed(seed);
        assert_eq!(
            play(shipped(), &s, 1_200),
            play(shipped(), &s.clone().with_nations(None, None), 1_200),
            "seed {seed}"
        );
    }
}

#[test]
fn a_skirmish_plays_identically_with_and_without_the_nation_layer() {
    for seed in [0u64, 4, 11] {
        let s = MatchSettings::default().with_seed(seed);
        let with = play(shipped(), &s, 1_800);
        let without = play(without_nations("seeds"), &s, 1_800);
        assert_eq!(with.0.len(), 1_800, "seed {seed}: one hash per tick");
        assert!(!with.3, "seed {seed}: no NationRules with no nation");
        assert_eq!(
            with, without,
            "seed {seed}: the nation layer reached a no-nation match"
        );
    }
}

#[test]
fn named_strategies_play_identically_with_and_without_the_nation_layer() {
    for (a, b) in [
        ("rush", "turtle"),
        ("mass_ripper", "mass_bulwark"),
        ("synth_triad", "mvp"),
    ] {
        let s = MatchSettings::default().with_strategies(a, b);
        let with = play(shipped(), &s, 3_600);
        let without = play(without_nations("named"), &s, 3_600);
        assert_eq!(
            with, without,
            "{a} vs {b}: the nation layer reached a no-nation match"
        );
    }
}

#[test]
fn a_campaign_battle_plays_identically_with_and_without_the_nation_layer() {
    let setup = BattleSetup::default()
        .with_general(Faction::A, "general_bulwark")
        .with_general(Faction::B, "general_frenzy");
    let s = MatchSettings::default()
        .with_seed(3)
        .with_strategies("mass_sentinel", "mass_ravager")
        .with_battle(setup);
    let with = play(shipped(), &s, 3_000);
    let without = play(without_nations("battle"), &s, 3_000);
    assert_eq!(with, without, "the nation layer reached a no-nation battle");
}

#[test]
fn a_decided_match_is_decided_identically() {
    // One full match to its end, so the outcome — not only the opening — is
    // shown unchanged.
    let s = MatchSettings::default()
        .with_seed(2)
        .with_strategies("rush", "mvp");
    let cap = headless::DEFAULT_TICK_CAP;
    let with = play(shipped(), &s, cap);
    let without = play(without_nations("decided"), &s, cap);
    assert!(
        with.2.is_some(),
        "the match is decided inside the default cap"
    );
    assert_eq!(with, without);
}
