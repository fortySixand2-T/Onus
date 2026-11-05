//! Critic C2 AC5: the client's no-nation pick is today's match, tick for tick.

use std::path::PathBuf;

use bevy::prelude::*;
use onus::client::{NATION_PREV_KEY, OPPONENT_PREV_KEY, START_KEY, NationPick};
use onus::sim::content::Content;
use onus::sim::economy::Stockpiles;
use onus::sim::{CommandQueue, RateReport, StateHashLog, TickGate};

fn content() -> Content {
    Content::load_from_dir(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data"))
        .expect("assets/data/*.ron")
}

fn base(pick: bool) -> App {
    let c = content();
    let alloy = c.economy.starting_alloy;
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .insert_resource(Time::<Fixed>::from_hz(60.0))
        .insert_resource(c)
        .init_resource::<CommandQueue>()
        .init_resource::<RateReport>()
        .init_resource::<ButtonInput<KeyCode>>()
        .insert_resource(Stockpiles::starting(alloy))
        .add_systems(Startup, onus::setup::setup);
    onus::add_sim_systems(&mut app, Update);
    if pick {
        onus::add_nation_pick(&mut app);
    }
    app.insert_resource(StateHashLog::default());
    frame(&mut app);
    if !pick {
        // The pre-C2 client: no pick, the sim runs from the first frame.
        app.world_mut().resource_mut::<TickGate>().open();
    }
    app
}

/// One frame on a manually advanced fixed clock, exactly as `headless::step`.
fn frame(app: &mut App) {
    onus::headless::step(app);
}

fn press(app: &mut App, key: KeyCode) {
    app.world_mut().resource_mut::<ButtonInput<KeyCode>>().press(key);
    frame(app);
    let mut keys = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
    keys.release(key);
    keys.clear();
}

#[test]
fn a_no_nation_pick_plays_the_pre_c2_client_match_hash_for_hash() {
    const TICKS: usize = 900;
    let mut reference = base(false);
    while reference.world().resource::<StateHashLog>().0.len() < TICKS {
        frame(&mut reference);
    }
    let mut client = base(true);
    press(&mut client, NATION_PREV_KEY);
    press(&mut client, OPPONENT_PREV_KEY);
    press(&mut client, OPPONENT_PREV_KEY);
    let p = client.world().resource::<NationPick>().clone();
    assert_eq!((p.player, p.opponent), (None, None));
    assert!(client.world().resource::<StateHashLog>().0.is_empty());
    press(&mut client, START_KEY);
    while client.world().resource::<StateHashLog>().0.len() < TICKS {
        frame(&mut client);
    }
    let a = &reference.world().resource::<StateHashLog>().0[..TICKS];
    let b = &client.world().resource::<StateHashLog>().0[..TICKS];
    let first = a.iter().zip(b).position(|(x, y)| x != y);
    assert_eq!(first, None, "the no-nation client diverges from the pre-C2 client");
}

/// Control: the harness above is deterministic (two reference runs agree).
#[test]
fn control_two_reference_clients_agree() {
    const TICKS: usize = 900;
    let run = || {
        let mut r = base(false);
        while r.world().resource::<StateHashLog>().0.len() < TICKS {
            frame(&mut r);
        }
        r.world().resource::<StateHashLog>().0[..TICKS].to_vec()
    };
    assert_eq!(run(), run());
}
