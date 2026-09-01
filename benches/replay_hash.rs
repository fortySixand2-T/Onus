//! L4 bench for M5: what a tick costs with and without per-tick state hashing.
//!
//! The hash is opt-in (insert a `StateHashLog` and the sim records one per
//! tick); this is the measurement that justifies it being opt-in rather than
//! always on. Run with `cargo bench --bench replay_hash`.

use std::path::PathBuf;

use bevy::prelude::*;
use criterion::{black_box, criterion_group, criterion_main, Criterion};

use onus::sim::combat::{Casualties, Health};
use onus::sim::content::Content;
use onus::sim::economy::{Building, ProductionQueue, Stockpiles, UnitDefIdx};
use onus::sim::spatial::Faction;
use onus::sim::{
    AiCommanders, CommandLog, CommandQueue, Position, RateReport, ResourceNode, StateHashLog,
};

fn content() -> Content {
    Content::load_from_dir(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data"))
        .expect("assets/data")
}

fn step(app: &mut App) {
    let dt = app.world().resource::<Time<Fixed>>().timestep();
    app.world_mut().resource_mut::<Time<Fixed>>().advance_by(dt);
    app.update();
}

/// The M4c AI-vs-AI fixture: the sim's standard match shape.
fn ai_vs_ai(seed: u64, hashing: bool) -> App {
    let c = content();
    let alloy = c.economy.starting_alloy;
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .insert_resource(Time::<Fixed>::from_hz(60.0))
        .insert_resource(c)
        .init_resource::<CommandQueue>()
        .init_resource::<RateReport>()
        .init_resource::<Casualties>()
        .insert_resource(Stockpiles::starting(alloy));
    onus::add_sim_systems(&mut app, Update);
    for (faction, base) in [
        (Faction::A, Vec2::new(-750.0, 0.0)),
        (Faction::B, Vec2::new(750.0, 0.0)),
    ] {
        let def = app
            .world()
            .resource::<Content>()
            .building_index("hq")
            .unwrap();
        app.world_mut().spawn((
            Position(base),
            Building { def },
            faction,
            ProductionQueue::default(),
        ));
        app.world_mut().spawn((
            Position(base + Vec2::new(0.0, 250.0)),
            ResourceNode { amount: 100_000 },
        ));
        for i in 0..3 {
            let (idx, kind, hp) = {
                let c = app.world().resource::<Content>();
                let idx = c.unit_index("worker").unwrap();
                (idx, c.units[idx].mvp_kind, Health::from_def(c, idx))
            };
            app.world_mut().spawn((
                Position(base + Vec2::new(0.0, 20.0 * i as f32)),
                UnitDefIdx(idx),
                kind,
                faction,
                hp,
            ));
        }
    }
    app.insert_resource(AiCommanders::new(seed, &[Faction::A, Faction::B]));
    app.insert_resource(CommandLog::new(seed));
    if hashing {
        app.insert_resource(StateHashLog::default());
    }
    app
}

fn bench_ticks(c: &mut Criterion) {
    const TICKS: u32 = 600;
    let mut group = c.benchmark_group("m5_tick");
    group.sample_size(10);
    for hashing in [false, true] {
        let name = if hashing { "with_hash" } else { "without_hash" };
        group.bench_function(name, |b| {
            b.iter(|| {
                let mut app = ai_vs_ai(black_box(4), hashing);
                for _ in 0..TICKS {
                    step(&mut app);
                }
                app.world().resource::<Stockpiles>().alloy(Faction::A)
            });
        });
    }
    group.finish();
}

fn bench_one_hash(c: &mut Criterion) {
    let mut app = ai_vs_ai(4, false);
    for _ in 0..1_200 {
        step(&mut app);
    }
    let entities = {
        let mut q = app.world_mut().query_filtered::<Entity, With<Position>>();
        q.iter(app.world()).count()
    };
    c.bench_function(&format!("state_hash_{entities}_things"), |b| {
        b.iter(|| onus::sim::state_hash(app.world_mut()));
    });
}

criterion_group!(benches, bench_ticks, bench_one_hash);
criterion_main!(benches);
