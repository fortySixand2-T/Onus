use bevy::prelude::*;
use onus::headless::{self, MatchSettings};
use onus::sim::combat::{Casualties, Health};
use onus::sim::economy::{Building, Stockpiles};
use onus::sim::spatial::Faction;
use onus::sim::three::EconomyMode;
use onus::sim::{AttackMoveDest, Position, UnitKind};
#[test]
#[ignore]
fn stall() {
    let c = headless::content().unwrap();
    let seed: u64 = std::env::var("SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(onus::batch::seed_at(0, 0));
    let mut s = MatchSettings::default()
        .with_seed(seed)
        .with_strategies("mass_sentinel", "mass_sentinel");
    if std::env::var("THREE").is_ok() {
        s = s.with_economy(EconomyMode::ThreeResource);
    }
    let mut app = headless::ai_vs_ai(c.clone(), &s).unwrap();
    if std::env::var("JOURNAL").is_ok() {
        headless::tick(&mut app, 20000);
        for (t, f, a) in app.world().resource::<onus::sim::AiJournal>().0.iter() {
            if *f == Faction::A && !matches!(a, onus::sim::AiAction::Gather { .. }) {
                println!("J {t} {a:?}");
            }
        }
        return;
    }
    for t in 0..54_000u32 {
        headless::step(&mut app);
        if t % 1800 == 0 {
            let w = app.world_mut();
            let cas = *w.resource::<Casualties>();
            let st = *w.resource::<Stockpiles>();
            let mut line = format!("t{:>5} ", t);
            for f in [Faction::A, Faction::B] {
                let mut q = w.query::<(&Faction, &UnitKind, &Position, Option<&AttackMoveDest>)>();
                let (mut n, mut wk, mut sx, mut am) = (0, 0, 0.0, 0);
                for (ff, k, p, a) in q.iter(w) {
                    if *ff != f {
                        continue;
                    }
                    if *k == UnitKind::Worker {
                        wk += 1;
                        continue;
                    }
                    n += 1;
                    sx += p.0.x;
                    if a.is_some() {
                        am += 1;
                    }
                }
                let mut qb = w.query::<(&Faction, &Building, &Health)>();
                let blds: Vec<String> = qb
                    .iter(w)
                    .filter(|(ff, _, _)| **ff == f)
                    .map(|(_, b, h)| format!("{}:{}", c.buildings[b.def].id, h.current))
                    .collect();
                line += &format!(
                    "| {:?} sol {:>2} (am {:>2}) x̄ {:>6.0} wk {} alloy {:>4} lost {:>3} {:?} ",
                    f,
                    n,
                    am,
                    if n > 0 { sx / n as f32 } else { 0.0 },
                    wk,
                    st.alloy(f),
                    cas.lost(f),
                    blds
                );
            }
            println!("{line}");
        }
    }
}
