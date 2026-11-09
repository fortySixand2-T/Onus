//! Critic C5 AC8 — who acts, and does it ever matter? Instrumented.

use std::path::PathBuf;

use onus::campaign::balance::Profile;
use onus::campaign::diplomacy::{Act, DiplomacyContent};
use onus::campaign::{Campaign, CampaignContent, HeadlessResolver};
use onus::sim::content::Content;

fn content() -> Content {
    Content::load_from_dir(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data")).unwrap()
}

#[test]
#[ignore]
fn critic_who_acts_in_each_profile() {
    let c0 = content();
    let d = DiplomacyContent::shipped(&c0).unwrap();
    let n = c0.nations.len();
    let seeds: u64 = std::env::var("CRIT_SEEDS").ok().and_then(|v| v.parse().ok()).unwrap_or(2);
    for seed in 1..=seeds {
        for p in Profile::all(n) {
            let cc = CampaignContent::shipped(&c0).unwrap().with_diplomacy(p.apply(d.clone()));
            let mut c = Campaign::new(c0.clone(), cc, seed, None).unwrap();
            let mut fallen = vec![0u32; n];
            let mut r = HeadlessResolver;
            while !c.is_over() {
                c.play_round(&[], &mut r).unwrap();
                for (k, f) in fallen.iter_mut().enumerate() {
                    if *f == 0 && !c.nations()[k].alive {
                        *f = c.round();
                    }
                }
            }
            let mut per = vec![[0u32; 5]; n];
            for rec in c.diplomacy_log() {
                let i = match rec.act {
                    Act::Court { .. } => 0,
                    Act::Ally { .. } => 1,
                    Act::Ask { .. } => 2,
                    Act::Betray { .. } => 3,
                    Act::Incite { .. } => 4,
                };
                per[rec.actor][i] += 1;
            }
            eprintln!(
                "seed {seed} {p:?}: fallen {fallen:?} rounds {} outcome {:?} acts[court,ally,ask,betray,incite] {:?}",
                c.round(),
                c.outcome(),
                per
            );
        }
    }
}
