//! A headless lockstep peer, for proving determinism **across processes**.
//!
//! In-process tests can share a bug the way they share an address space: one
//! allocator, one static, one `Content` parsed once. Two processes share
//! nothing but the socket and the files on disk, which is the only way to show
//! that what crosses the wire is enough — that no `Entity`, no pointer and no
//! accident of allocation is doing quiet work.
//!
//! Usage (both sides print one machine-readable line and exit):
//!
//! ```text
//! netpeer host   <ticks> <seed> [addr]   # prints `PORT <n>` first, then RESULT
//! netpeer client <ticks> <seed> <addr>
//! ```
//!
//! `RESULT tick=<n> hash=<hex> agreed=<n> failure=<none|...>`

use std::io::Write;
use std::net::TcpListener;

use bevy::prelude::*;

use onus::net::{NetConfig, NetLink};
use onus::sim::combat::{Casualties, Health};
use onus::sim::content::Content;
use onus::sim::economy::{Building, ProductionQueue, Stockpiles, UnitDefIdx};
use onus::sim::replay::{CommandLog, StateHashLog};
use onus::sim::spatial::Faction;
use onus::sim::{AiCommanders, CommandQueue, MatchState, Position, RateReport, ResourceNode};

/// Frames to keep servicing the socket after this peer has finished playing, so
/// the partner — which is a turn behind by design — is not cut off mid-tick.
const LINGER_FRAMES: u32 = 240;

fn config() -> NetConfig {
    NetConfig {
        turn_delay_ticks: 4,
        hash_interval_ticks: 10,
        stall_timeout_secs: 30.0,
    }
}

/// The same starting world on both sides, spawned in the same order — a
/// lockstep match assumes it, and the handshake's content check is what makes
/// assuming it safe.
///
/// **Each peer runs its own side's scripted commander, seeded from the match
/// seed.** That is what makes this a test of a *seeded* match rather than of a
/// constant: `AiCommanders` is the only seeded thing in the sim, and its
/// decisions (where a barracks goes, where a wave aims) come out of a stream
/// derived from `seed` and the faction slot. Only the local side's commander
/// runs here — its orders cross the wire like a player's, so both peers apply
/// them on the same tick and neither computes the other's.
fn build(link: NetLink, seed: u64, me: Faction) -> App {
    let content = Content::load_default().expect("assets/data/*.ron");
    let alloy = content.economy.starting_alloy;
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .insert_resource(Time::<Fixed>::from_hz(60.0))
        .insert_resource(content)
        .init_resource::<CommandQueue>()
        .init_resource::<RateReport>()
        .init_resource::<Casualties>()
        .insert_resource(Stockpiles::starting(alloy))
        .insert_resource(CommandLog::new(seed));
    onus::add_sim_systems(&mut app, Update);
    onus::net::add_net_link(&mut app, Update, link);
    for (faction, base) in [
        (Faction::A, Vec2::new(-750.0, 0.0)),
        (Faction::B, Vec2::new(750.0, 0.0)),
    ] {
        let (def, hp) = {
            let c = app.world().resource::<Content>();
            let def = c.building_index("hq").expect("hq");
            (def, Health::from_building_def(c, def))
        };
        app.world_mut().spawn((
            Position(base),
            Building { def },
            faction,
            ProductionQueue::default(),
            hp,
        ));
        app.world_mut().spawn((
            Position(base + Vec2::new(0.0, 250.0)),
            ResourceNode { amount: 100_000 },
        ));
        // **The starting layout is seeded**, through the sim's own generator, so
        // the match depends on its seed from tick 0 rather than only from the
        // commander's first random decision (which is its barracks placement, at
        // `mvp_ai.barracks_at_tick`). A short run must be able to tell two seeds
        // apart, or "two runs of one seed agree" is a claim about a constant.
        //
        // Both peers compute **both** sides' layouts from the same numbers, so
        // this is match setup, not local randomness: a pure function of
        // `(seed, faction)`, evaluated identically in both processes.
        let slot = match faction {
            Faction::A => 0u64,
            Faction::B => 1,
        };
        let spots = onus::sim::random_layout(
            3,
            seed ^ (slot.wrapping_mul(0x9E37_79B9_7F4A_7C15)),
            base - Vec2::splat(60.0),
            base + Vec2::splat(60.0),
        );
        for spot in spots {
            let (idx, kind, hp) = {
                let c = app.world().resource::<Content>();
                let idx = c.unit_index("worker").expect("worker");
                (idx, c.units[idx].mvp_kind, Health::from_def(c, idx))
            };
            app.world_mut().spawn((
                Position(spot.pos),
                UnitDefIdx(idx),
                kind,
                faction,
                hp,
            ));
        }
    }
    // The seeded commander — this peer's side only.
    app.insert_resource(AiCommanders::new(seed, &[me]));
    app
}

fn step(app: &mut App) {
    let dt = app.world().resource::<Time<Fixed>>().timestep();
    app.world_mut().resource_mut::<Time<Fixed>>().advance_by(dt);
    app.update();
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let role = args.get(1).map(String::as_str).unwrap_or("host");
    let ticks: u32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(300);
    let seed: u64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(7);

    let (link, me) = match role {
        "host" => {
            let addr = args.get(4).cloned().unwrap_or("127.0.0.1:0".to_string());
            let listener = TcpListener::bind(&addr).expect("bind");
            // The port is ephemeral and announced, never fixed: two runs on one
            // machine must not fight over an address.
            println!("PORT {}", listener.local_addr().expect("addr").port());
            std::io::stdout().flush().ok();
            let (stream, _) = listener.accept().expect("accept");
            (
                NetLink::from_stream(stream, Faction::A, config()).expect("link"),
                Faction::A,
            )
        }
        _ => {
            let addr = args.get(4).cloned().expect("client needs an address");
            (
                NetLink::connect(&addr, Faction::B, config()).expect("connect"),
                Faction::B,
            )
        }
    };

    let mut app = build(link, seed, me);
    let mut frames = 0u32;
    // A frame budget so a broken peer exits instead of hanging a test.
    while app.world().resource::<MatchState>().tick() < ticks && frames < ticks * 20 {
        step(&mut app);
        frames += 1;
    }
    // **The result is what this peer achieved by `ticks`**, snapshotted before
    // anything that happens on the way out.
    let link = app.world().resource::<NetLink>();
    let failure = match link.failure() {
        None => "none".to_string(),
        Some(f) => format!("{f:?}").replace(' ', "_"),
    };
    let agreed = link.hashes_agreed();
    let tick = app.world().resource::<MatchState>().tick();
    let hash = app
        .world()
        .resource::<StateHashLog>()
        .0
        .get(ticks.saturating_sub(1) as usize)
        .copied()
        .unwrap_or(0);

    // **Then linger.** A lockstep peer that exits the instant it is done leaves
    // its partner with a socket that resets mid-tick — the partner is a few
    // ticks behind by construction (that is what a turn delay *is*), and it is
    // still writing turns nobody is reading. So keep servicing the link for a
    // bounded while, long enough for the other side to finish with the turns
    // this one already sent. Nothing observed here can change the result above.
    for _ in 0..LINGER_FRAMES {
        step(&mut app);
    }

    // Debug aid, off unless asked: dump this peer's per-tick hashes and command
    // log so two processes' accounts can be diffed directly.
    if let Ok(dir) = std::env::var("ONUS_NET_DUMP") {
        let side = if me == Faction::A { "host" } else { "client" };
        let hashes: Vec<String> = app
            .world()
            .resource::<StateHashLog>()
            .0
            .iter()
            .enumerate()
            .map(|(i, h)| format!("{i} {h:016x}"))
            .collect();
        let _ = std::fs::write(format!("{dir}/{side}.hashes"), hashes.join("\n"));
        let cmds: Vec<String> = app
            .world()
            .resource::<CommandLog>()
            .commands()
            .iter()
            .map(|c| format!("{:?}", c))
            .collect();
        let _ = std::fs::write(format!("{dir}/{side}.commands"), cmds.join("\n"));
    }

    println!("RESULT tick={tick} hash={hash:016x} agreed={agreed} failure={failure}");
}
