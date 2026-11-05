//! C1 AC9 — **balance**: the B3 kill-criteria gate run on campaign-style
//! battles (a General per side, the battle AI's guard and retreat on). The
//! gate is the same `KillGate` over the same `MatchRecord`s; only the matches
//! differ. The full reading is a measurement (`--ignored`, recorded in
//! FINDINGS F-060); the suite keeps a small batch that proves battles feed
//! the gate faithfully. B5's human re-read is not something a test can do.

use onus::batch::{MatchRecord, MatchResult, ProductionCounts};
use onus::gate::{GateSpec, KillGate};
use onus::headless::{self, MatchSettings, Orientation};
use onus::sim::battle::{BattleSetup, BattleState, LossCause};
use onus::sim::content::Content;
use onus::sim::economy::Produced;
use onus::sim::spatial::Faction;
use onus::sim::MatchState;

fn content() -> Content {
    headless::content().expect("content loads")
}

/// One campaign-style match: both sides field `general` (a mirror of
/// Generals, so the strategies are what differs) on the skirmish start.
/// Returns the gate's record and the battle's loss causes.
fn battle_match(
    c: &Content,
    a: &str,
    b: &str,
    seed: u64,
    o: Orientation,
    general: &str,
) -> (MatchRecord, [Option<LossCause>; 2]) {
    let setup = BattleSetup::default()
        .with_general(Faction::A, general)
        .with_general(Faction::B, general);
    let settings = MatchSettings::default()
        .with_seed(seed)
        .with_strategies(a, b)
        .with_orientation(o)
        .with_battle(setup);
    let mut app = headless::ai_vs_ai(c.clone(), &settings).expect("names resolve");
    let mut ticks = 0;
    let mut result = MatchResult::Timeout;
    while ticks < settings.tick_cap {
        headless::step(&mut app);
        ticks += 1;
        if let Some(o) = app.world().resource::<MatchState>().outcome() {
            result = match o.winner {
                Some(f) => MatchResult::Decided(f),
                None => MatchResult::MutualLoss,
            };
            break;
        }
    }
    let lost = app
        .world()
        .resource::<BattleState>()
        .result()
        .map(|r| r.lost)
        .unwrap_or([None, None]);
    let produced = ProductionCounts::of(c, app.world().resource::<Produced>());
    (
        MatchRecord::new(c, &settings, result, ticks, produced),
        lost,
    )
}

fn batch(
    c: &Content,
    probes: &[&str],
    seeds: &[u64],
    general_for: impl Fn(u64) -> String,
) -> (Vec<MatchRecord>, Vec<[Option<LossCause>; 2]>) {
    let (mut recs, mut causes) = (Vec::new(), Vec::new());
    for &seed in seeds {
        let g = general_for(seed);
        for a in probes {
            for b in probes {
                for o in Orientation::ALL {
                    let (r, l) = battle_match(c, a, b, seed, o, &g);
                    recs.push(r);
                    causes.push(l);
                }
            }
        }
    }
    (recs, causes)
}

#[test]
fn campaign_battles_feed_the_b3_gate_and_every_decision_has_a_battle_cause() {
    let c = content();
    let probes = ["mass_ripper", "mass_sentinel"];
    let (recs, causes) = batch(&c, &probes, &[0], |_| "general_focus".to_string());
    assert_eq!(
        recs.len(),
        probes.len() * probes.len() * Orientation::ALL.len()
    );
    for (r, l) in recs.iter().zip(&causes) {
        match r.result {
            MatchResult::Decided(w) => {
                let loser = if w == Faction::A { 1 } else { 0 };
                assert!(l[loser].is_some(), "{r:?}: the loser has a cause");
                assert!(l[1 - loser].is_none(), "{r:?}: the winner lost nothing");
            }
            MatchResult::MutualLoss => assert!(l[0].is_some() && l[1].is_some()),
            MatchResult::Timeout => assert_eq!(*l, [None, None]),
        }
    }
    // The gate reads a battle batch exactly as it reads a skirmish one.
    let g = KillGate::of(&c, &recs, &GateSpec::default());
    assert_eq!(g.strength.rows.len(), probes.len());
    assert_eq!(
        g,
        KillGate::of(&c, &recs, &GateSpec::default()),
        "a pure function"
    );
}

/// The measurement: the B3 pentagon batch (the five mass probes, every
/// ordered pair, both orientations) on six seeds, seed k fielding the k-th
/// General kind on both sides — every aura kind is played. Prints the gate's
/// reading and the battle causes; F-060 records them.
#[test]
#[ignore = "measurement: run with --ignored --nocapture (release) and record in FINDINGS"]
fn b3_gate_on_campaign_battles() {
    let c = content();
    let kinds: Vec<String> = c.generals.iter().map(|g| g.id.clone()).collect();
    assert_eq!(kinds.len(), 6);
    let probes = [
        "mass_sentinel",
        "mass_ripper",
        "mass_arclight",
        "mass_bulwark",
        "mass_ravager",
    ];
    let seeds: Vec<u64> = (0..6).collect();
    let (recs, causes) = batch(&c, &probes, &seeds, |s| kinds[s as usize % 6].clone());
    let g = KillGate::of(&c, &recs, &GateSpec::default());
    println!("C1 B3 on campaign battles: {} matches", recs.len());
    println!("status: {:?}", g.status);
    for r in &g.strength.rows {
        println!(
            "K1 {:<14} strength {:?} interval {:?} {:?}",
            r.strategy, r.strength.value, r.strength.interval, r.strength.status
        );
    }
    println!(
        "K1 failing {:?} dominant {:?} losing {:?} -> {:?}",
        g.strength.failing, g.strength.dominant, g.strength.losing, g.strength.status
    );
    println!(
        "K2 slot A {:?} (n {}) {:?}; left base {:?} {:?} -> {:?}",
        g.seat.slot_a.value,
        g.seat.slot_a.n,
        g.seat.slot_a.status,
        g.seat.left_spawn.value,
        g.seat.left_spawn.status,
        g.seat.status
    );
    let t = &g.termination;
    println!(
        "K3 median {:?} ticks; timeout {:?} {:?}; band share {:?} {:?}; below {:?}; beyond {:?} -> {:?}",
        t.decided_median,
        t.timeout_rate.value,
        t.timeout_rate.status,
        t.band_share.value,
        t.band_share.status,
        t.below.value,
        t.beyond.value,
        t.status
    );
    let mut tally = [0u32; 4]; // generals fell, HQ lost, retreated, timeout
    for (r, l) in recs.iter().zip(&causes) {
        if r.result == MatchResult::Timeout {
            tally[3] += 1;
        }
        for cause in l.iter().flatten() {
            tally[match cause {
                LossCause::GeneralsFell => 0,
                LossCause::VictoryBuildingLost => 1,
                LossCause::Retreated => 2,
            }] += 1;
        }
    }
    println!(
        "causes: generals fell {}, HQ lost {}, retreated {}, timeouts {}",
        tally[0], tally[1], tally[2], tally[3]
    );
    for (k, kind) in kinds.iter().enumerate() {
        let n: Vec<u32> = recs
            .iter()
            .filter(|r| r.seed as usize % 6 == k && r.result != MatchResult::Timeout)
            .map(|r| r.ticks)
            .collect();
        let mean = n.iter().map(|t| *t as f64).sum::<f64>() / n.len().max(1) as f64;
        println!(
            "{kind:<18} decided {} mean length {:.0} ticks",
            n.len(),
            mean
        );
    }
}
