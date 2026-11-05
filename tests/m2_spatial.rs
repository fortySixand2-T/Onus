//! L2 integration tests for M2 (scale + spatial index).
//!
//! These drive the render-free sim data through a headless `MinimalPlugins`
//! app where an ECS surface is needed, and exercise the pure spatial index
//! directly where it isn't. They encode the M2 acceptance criteria and the
//! critic probes (differential grid==brute, edge cases, speedup holds).

use bevy::prelude::*;

use onus::sim::spatial::*;
use onus::sim::Position;

// ---- AC1: spawn 1000+ units across two factions ----------------------------

#[test]
fn spawn_1000_units_two_factions() {
    // A deterministic layout is the spawn substrate: 1000+ points, each tagged
    // with one of the two mirror-nation sides.
    let layout = random_layout(1000, 0xC0FFEE, Vec2::new(-500.0, -500.0), Vec2::new(500.0, 500.0));
    assert_eq!(layout.len(), 1000, "layout produces exactly the requested count");

    // Spawn them into a headless ECS world as real sim entities.
    let mut app = App::new();
    app.add_plugins(MinimalPlugins);
    for u in &layout {
        app.world_mut().spawn((Position(u.pos), u.faction));
    }

    let mut q = app.world_mut().query::<(&Position, &Faction)>();
    let all: Vec<Faction> = q.iter(app.world()).map(|(_, f)| *f).collect();
    assert_eq!(all.len(), 1000, "1000 units live in the sim world");

    let a = all.iter().filter(|f| **f == Faction::A).count();
    let b = all.iter().filter(|f| **f == Faction::B).count();
    assert!(a > 0 && b > 0, "both factions are present (A={a}, B={b})");
    assert_eq!(a + b, 1000, "every unit belongs to exactly one of the two sides");
}

#[test]
fn random_layout_is_deterministic() {
    let bounds = (Vec2::new(-100.0, -100.0), Vec2::new(100.0, 100.0));
    let a = random_layout(500, 42, bounds.0, bounds.1);
    let b = random_layout(500, 42, bounds.0, bounds.1);
    let c = random_layout(500, 43, bounds.0, bounds.1);

    let same = a.iter().zip(&b).all(|(x, y)| x.pos == y.pos && x.faction == y.faction);
    assert!(same, "same seed reproduces the identical layout");
    let differ = a.iter().zip(&c).any(|(x, y)| x.pos != y.pos || x.faction != y.faction);
    assert!(differ, "a different seed yields a different layout");
}
