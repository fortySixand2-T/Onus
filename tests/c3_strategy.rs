//! C3 (F-084) — **a strategy's three-resource overrides.** A strategy may carry
//! a `three_resource` block (`worker_target`, `barracks`) that a
//! three-resource match plays in place of its own values. An Alloy-only match
//! never reads it, so Alloy-only play is unchanged; an override is held to the
//! same load rules as the strategy itself.

use std::path::PathBuf;

use onus::headless::{self, MatchSettings};
use onus::sim::content::Content;
use onus::sim::spatial::Faction;
use onus::sim::three::EconomyMode;
use onus::sim::{AiAction, AiJournal};

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data")
}

/// The shipped content with every three-resource override taken out of
/// `strategies.ron`, then `block` (if any) given to `mass_sentinel`.
fn load_with(name: &str, block: Option<&str>) -> Result<Content, String> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/c3_strategy").join(name);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    for entry in std::fs::read_dir(data_dir()).expect("data dir") {
        let p = entry.expect("entry").path();
        if p.extension().is_some_and(|e| e == "ron") {
            std::fs::copy(&p, dir.join(p.file_name().unwrap())).expect("copy");
        }
    }
    let shipped = std::fs::read_to_string(data_dir().join("strategies.ron")).expect("read");
    let mut s: String = shipped
        .lines()
        .filter(|l| !l.trim_start().starts_with("three_resource:"))
        .map(|l| format!("{l}\n"))
        .collect();
    if let Some(b) = block {
        let at = s.find("id: \"mass_sentinel\"").expect("mass_sentinel");
        let anchor = "wave_order: attack_move,";
        let end = at + s[at..].find(anchor).expect("its wave order") + anchor.len();
        s.insert_str(end, &format!("\n            three_resource: ({b}),"));
    }
    std::fs::write(dir.join("strategies.ron"), s).expect("write");
    Content::load_from_dir(&dir).map_err(|e| e.to_string())
}

fn journal(c: Content, economy: EconomyMode) -> Vec<(u32, Faction, AiAction)> {
    let s = MatchSettings::default()
        .with_economy(economy)
        .with_strategies("mass_sentinel", "mass_sentinel");
    let mut app = headless::ai_vs_ai(c, &s).expect("strategies exist");
    headless::tick(&mut app, 6_000);
    app.world().resource::<AiJournal>().0.clone()
}

fn workers_ordered(j: &[(u32, Faction, AiAction)]) -> usize {
    j.iter()
        .filter(|(_, f, a)| *f == Faction::A && matches!(a, AiAction::TrainWorker { .. }))
        .count()
}

const MORE_WORKERS: &str = "worker_target: 9";

#[test]
fn a_strategy_without_overrides_plays_its_own_values_in_both_economies() {
    let c = load_with("none", None).unwrap();
    let s = &c.strategies[c.strategy_index("mass_sentinel").unwrap()];
    assert!(s.three_resource.is_none());
    for three in [false, true] {
        assert_eq!(s.worker_target_in(three), s.worker_target);
        assert_eq!(s.barracks_in(three).len(), s.barracks.len());
    }
}

#[test]
fn an_override_is_read_only_in_a_three_resource_match() {
    let plain = load_with("plain", None).unwrap();
    let over = load_with("over", Some(MORE_WORKERS)).unwrap();
    let s = &over.strategies[over.strategy_index("mass_sentinel").unwrap()];
    assert_eq!(s.worker_target_in(false), s.worker_target);
    assert_eq!(s.worker_target_in(true), 9);
    // Alloy only: the override changes nothing, action for action.
    assert_eq!(
        journal(plain.clone(), EconomyMode::Alloy),
        journal(over.clone(), EconomyMode::Alloy)
    );
    // Three resources: it trains the extra workers.
    let a = workers_ordered(&journal(plain, EconomyMode::ThreeResource));
    let b = workers_ordered(&journal(over, EconomyMode::ThreeResource));
    assert!(b > a, "the override's worker target is played ({a} -> {b})");
}

#[test]
fn an_override_changes_the_fingerprint_and_its_absence_does_not() {
    let a = load_with("fp_a", None).unwrap().fingerprint();
    let b = load_with("fp_b", None).unwrap().fingerprint();
    let c = load_with("fp_c", Some(MORE_WORKERS)).unwrap().fingerprint();
    assert_eq!(a, b);
    assert_ne!(a, c, "a log recorded under an override must not replay without it");
}

#[test]
fn an_override_is_held_to_the_strategy_load_rules() {
    for (name, block, needle) in [
        ("zero_workers", "worker_target: 0", "worker_target must be positive"),
        ("no_barracks", "barracks: []", "opens no barracks"),
        (
            "bad_building",
            "barracks: [(building: \"nowhere\", at_tick: 300, offset: 130.0)]",
            "is not a building",
        ),
        (
            "cannot_train",
            "barracks: [(building: \"gene_vats\", at_tick: 300, offset: 130.0)]",
            "none of its barracks",
        ),
    ] {
        let e = load_with(name, Some(block)).expect_err(name);
        assert!(e.contains(needle), "{name}: {e}");
    }
}
