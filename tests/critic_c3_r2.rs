//! Critic C3 round 2: the F4 fix must not refuse valid content.

use std::path::PathBuf;

use onus::headless::{self, MatchSettings};
use onus::sim::content::Content;

/// `resources.ron`'s `three_resource` block is optional: content without it
/// "can only play Alloy-only matches". The shipped nations (verdant's
/// `Buildings` Growth among them) must still load and play Alloy-only.
#[test]
fn critic_shipped_nations_load_without_the_optional_three_resource_block() {
    let data = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data");
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/critic_c3_r2/noblock");
    std::fs::create_dir_all(&dir).unwrap();
    for e in std::fs::read_dir(&data).unwrap() {
        let p = e.unwrap().path();
        if p.extension().is_some_and(|x| x == "ron") {
            std::fs::copy(&p, dir.join(p.file_name().unwrap())).unwrap();
        }
    }
    let r = std::fs::read_to_string(data.join("resources.ron")).unwrap();
    let at = r.find("    three_resource: (").expect("the block");
    let cut = format!("{})\n", &r[..at]);
    std::fs::write(dir.join("resources.ron"), cut).unwrap();
    let c = Content::load_from_dir(&dir).expect("Alloy-only content with the shipped nations loads");
    assert!(c.three_resource.is_none());
    let s = MatchSettings::default()
        .with_seed(1)
        .with_strategies("mass_ripper", "mass_bulwark")
        .with_nations(Some("verdant"), Some("tollmarch"));
    let mut app = headless::ai_vs_ai(c, &s).unwrap();
    headless::tick(&mut app, 600);
}
