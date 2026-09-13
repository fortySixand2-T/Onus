//! L4 bench for M5: what a tick costs with and without per-tick state hashing.
//!
//! The hash is opt-in (insert a `StateHashLog` and the sim records one per
//! tick); this is the measurement that justifies it being opt-in rather than
//! always on. Run with `cargo bench --bench replay_hash`.
//!
//! The match itself is `onus::headless` — the *shared* headless-match
//! constructor (B2). What is benched is unchanged; it is no longer a private
//! copy of the fixture, so the bench measures the same match the balance runner
//! plays.

use criterion::{black_box, criterion_group, criterion_main, Criterion};

use onus::headless::{self, MatchSettings};
use onus::sim::economy::Stockpiles;
use onus::sim::spatial::Faction;

/// The M4c AI-vs-AI fixture: the sim's standard match shape.
fn ai_vs_ai(seed: u64, hashing: bool) -> bevy::prelude::App {
    let content = headless::content().expect("assets/data");
    headless::ai_vs_ai(
        content,
        &MatchSettings::default()
            .with_seed(seed)
            .with_hashing(hashing),
    )
    .expect("the default strategy is always known")
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
                headless::tick(&mut app, TICKS);
                app.world().resource::<Stockpiles>().alloy(Faction::A)
            });
        });
    }
    group.finish();
}

fn bench_one_hash(c: &mut Criterion) {
    let mut app = ai_vs_ai(4, false);
    headless::tick(&mut app, 1_200);
    let entities = {
        let mut q = app
            .world_mut()
            .query_filtered::<bevy::prelude::Entity, bevy::prelude::With<onus::sim::Position>>();
        q.iter(app.world()).count()
    };
    c.bench_function(&format!("state_hash_{entities}_things"), |b| {
        b.iter(|| onus::sim::state_hash(app.world_mut()));
    });
}

criterion_group!(benches, bench_ticks, bench_one_hash);
criterion_main!(benches);
