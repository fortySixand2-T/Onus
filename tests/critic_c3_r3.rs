//! Critic C3 round 3: the block-gated Growth check opens no hole.

use std::path::PathBuf;

use onus::headless::{self, MatchSettings};
use onus::sim::content::Content;
use onus::sim::nation::{SideRules, TraitDef, TraitStat, TraitTarget};
use onus::sim::three::{EconomyMode, ThreeEconomy};
use onus::sim::{AiCommanders, CommandLog, MatchLog, ReplaySource, StateHashLog};

fn data() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data")
}
fn shipped() -> Content {
    Content::load_from_dir(&data()).unwrap()
}
/// The shipped content with `resources.ron`'s optional block cut out, loaded from disk.
fn blockless() -> Content {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(format!("target/critic_c3_r3/noblock_{n}"));
    std::fs::create_dir_all(&dir).unwrap();
    for e in std::fs::read_dir(data()).unwrap() {
        let p = e.unwrap().path();
        if p.extension().is_some_and(|x| x == "ron") {
            std::fs::copy(&p, dir.join(p.file_name().unwrap())).unwrap();
        }
    }
    let r = std::fs::read_to_string(data().join("resources.ron")).unwrap();
    let at = r.find("    three_resource: (").unwrap();
    std::fs::write(dir.join("resources.ron"), format!("{})\n", &r[..at])).unwrap();
    let c = Content::load_from_dir(&dir).unwrap();
    assert!(c.three_resource.is_none());
    c
}
fn growth(on: TraitTarget) -> TraitDef {
    TraitDef { applies_to: on, stat: TraitStat::Growth, add: None, mul: Some(1.5) }
}
fn verdant(c: &Content) -> usize {
    c.nations.iter().position(|n| n.id == "verdant").unwrap()
}

#[test]
fn critic_with_the_block_every_invalid_growth_is_refused_and_a_valid_one_is_not() {
    let c = shipped();
    let k = verdant(&c);
    for (on, ok) in [
        (TraitTarget::Building("foundry".into()), false),
        (TraitTarget::Building("hq".into()), true),
        (TraitTarget::Building("gene_vats".into()), true),
        (TraitTarget::Building("nowhere".into()), false),
    ] {
        let mut x = c.clone();
        x.nations[k].three_resource_traits.push(growth(on.clone()));
        let r = SideRules::build_in(&x, k, EconomyMode::ThreeResource);
        assert_eq!(r.is_ok(), ok, "{on:?}: {r:?}", r = r.as_ref().err());
    }
    // `Buildings` with no source at all acts on nothing.
    let mut x = c.clone();
    x.three_resource.as_mut().unwrap().growth.sources.clear();
    assert!(SideRules::build_in(&x, k, EconomyMode::ThreeResource).is_err());
}

#[test]
fn critic_growth_in_the_alloy_list_is_refused_with_or_without_the_block() {
    for c in [shipped(), blockless()] {
        let k = verdant(&c);
        let mut x = c.clone();
        x.nations[k].traits.push(growth(TraitTarget::Buildings));
        assert!(SideRules::build_in(&x, k, EconomyMode::Alloy).is_err(), "Growth cannot act Alloy-only");
        let mut y = c.clone();
        y.nations[k].three_resource_traits.push(growth(TraitTarget::Building("nowhere".into())));
        assert!(SideRules::build_in(&y, k, EconomyMode::ThreeResource).is_err(), "unknown building");
    }
}

#[test]
fn critic_blockless_content_never_starts_a_three_resource_match() {
    let c = blockless();
    let s = MatchSettings::default()
        .with_economy(EconomyMode::ThreeResource)
        .with_nations(Some("verdant"), Some("tollmarch"));
    // `battle` refuses with an error; `ai_vs_ai` refuses by panicking (its
    // documented contract for every non-strategy refusal).
    assert!(matches!(
        headless::battle(c.clone(), &s),
        Err(headless::MatchSetupError::Economy(_))
    ));
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| headless::ai_vs_ai(c, &s)));
    assert!(r.is_err(), "ai_vs_ai must not start it");
}

#[test]
fn critic_a_three_resource_log_never_replays_into_blockless_content() {
    const TICKS: u32 = 600;
    let s = MatchSettings::default()
        .with_seed(13)
        .with_economy(EconomyMode::ThreeResource)
        .with_nations(Some("verdant"), Some("tollmarch"))
        .with_hashing(true);
    let mut rec = headless::ai_vs_ai(shipped(), &s).unwrap();
    headless::tick(&mut rec, TICKS);
    let log = rec.world().resource::<CommandLog>().log().clone();
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/critic_c3_r3");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("three.ron");
    log.save(&path).unwrap();
    // From disk: refused at load.
    assert!(MatchLog::load_for(&path, &blockless()).is_err(), "load_for");
    // In memory, past the file check: the backstop refuses, no economy installed.
    let rs = MatchSettings::default()
        .with_seed(log.seed)
        .with_nations(Some("verdant"), Some("tollmarch"))
        .with_hashing(true);
    let mut rep = headless::ai_vs_ai(blockless(), &rs).unwrap();
    rep.insert_resource(AiCommanders::default());
    rep.insert_resource(StateHashLog::default());
    rep.insert_resource(CommandLog::new(log.seed));
    rep.insert_resource(ReplaySource::new(log));
    headless::tick(&mut rep, TICKS);
    assert!(rep.world().resource::<ReplaySource>().rejection().is_some(), "in-memory replay refused");
    assert!(!rep.world().contains_resource::<ThreeEconomy>(), "no three-resource economy installed");
}
