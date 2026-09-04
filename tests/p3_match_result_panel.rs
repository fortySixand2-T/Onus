//! L2 tests for the match-result panel — **what the player is shown**, not what
//! the sim knows.
//!
//! The defect these exist for survived M4c and all of M5 because every test in
//! this repo asks `MatchState` directly: the sim recorded the outcome correctly,
//! the driver never rendered it, and from the player's chair a decided match was
//! a game that had stopped responding. So nothing here reads `MatchState` to
//! decide whether it passed — every assertion is on the **text of the panel**
//! the shipped system writes, driven headlessly under `MinimalPlugins` through
//! the shipped system itself (`onus::ui::update_options_panel`).
//!
//! Every "the panel says X" assertion is paired with proof it would otherwise
//! have said something else: the same fixture, one step earlier, shows the
//! selection line — so a panel that never changed could not pass.

use std::path::PathBuf;

use bevy::prelude::*;

use onus::client::{
    match_clock, match_result_text, OptionsPanel, Selected, PLAYER_FACTION, RESULT_DRAW,
    RESULT_INPUT_OFF, RESULT_LOSS, RESULT_WIN,
};
use onus::sim::combat::{Casualties, Health};
use onus::sim::content::Content;
use onus::sim::economy::{Building, ProductionQueue, Stockpiles, UnitDefIdx};
use onus::sim::spatial::Faction;
use onus::sim::{
    CommandQueue, MatchOutcome, MatchState, Position, RateReport, UnitKind,
};

// ---- harness ----------------------------------------------------------------

fn content() -> Content {
    Content::load_from_dir(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data"))
        .expect("assets/data/*.ron")
}

/// A headless app running the shipped sim chain **and the shipped panel
/// system**, with a panel entity shaped like `setup.rs` spawns.
fn panel_app(hz: f64) -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .insert_resource(Time::<Fixed>::from_hz(hz))
        .insert_resource(content())
        .init_resource::<CommandQueue>()
        .init_resource::<RateReport>()
        .init_resource::<Casualties>()
        .insert_resource(Stockpiles::starting(0));
    onus::add_sim_systems(&mut app, Update);
    // The one shipped writer of the panel, after the **whole** sim chain so the
    // frame's text describes the tick that just ran — `match_end` is the system
    // that records the outcome, and it runs last. (In the shipped app the sim is
    // in `FixedUpdate`, which finishes before `Update` begins, so this ordering
    // is a property of the test composition rather than of the game.)
    app.add_systems(
        Update,
        onus::ui::update_options_panel.after(onus::sim::victory::match_end),
    );
    app.world_mut().spawn((Text::new(""), OptionsPanel));
    app
}

fn step(app: &mut App) {
    let dt = app.world().resource::<Time<Fixed>>().timestep();
    app.world_mut().resource_mut::<Time<Fixed>>().advance_by(dt);
    app.update();
}

fn tick(app: &mut App, n: u32) {
    for _ in 0..n {
        step(app);
    }
}

/// What the player sees.
fn panel(app: &mut App) -> String {
    let mut q = app.world_mut().query_filtered::<&Text, With<OptionsPanel>>();
    q.iter(app.world())
        .next()
        .map(|t| t.0.clone())
        .expect("the panel entity exists")
}

fn spawn_building(app: &mut App, faction: Faction, pos: Vec2) -> Entity {
    let (def, hp) = {
        let c = app.world().resource::<Content>();
        let def = c.building_index("hq").expect("hq");
        (def, Health::from_building_def(c, def))
    };
    app.world_mut()
        .spawn((
            Position(pos),
            Building { def },
            faction,
            ProductionQueue::default(),
            hp,
        ))
        .id()
}

/// A unit of the player's, **selected**, so the panel has something else it
/// could be saying. Without this every "the panel shows the result" assertion
/// could pass against a panel that simply never says anything.
fn spawn_selected_worker(app: &mut App) -> Entity {
    let (idx, kind, hp) = {
        let c = app.world().resource::<Content>();
        let idx = c.unit_index("worker").expect("worker");
        (idx, c.units[idx].mvp_kind, Health::from_def(c, idx))
    };
    app.world_mut()
        .spawn((
            Position(Vec2::new(-300.0, 0.0)),
            UnitDefIdx(idx),
            kind,
            PLAYER_FACTION,
            hp,
            Selected,
        ))
        .id()
}

/// A contested match with a selected worker on the board: two HQs, both alive,
/// the panel showing the ordinary selection line. `destroy` ends it.
struct Fixture {
    app: App,
    mine: Entity,
    theirs: Entity,
}

fn contested(hz: f64) -> Fixture {
    let mut app = panel_app(hz);
    let mine = spawn_building(&mut app, PLAYER_FACTION, Vec2::new(-400.0, 0.0));
    let theirs = spawn_building(&mut app, Faction::B, Vec2::new(400.0, 0.0));
    spawn_selected_worker(&mut app);
    // Two ticks so `match_watch` sees both HQs and the match becomes decidable.
    tick(&mut app, 2);
    Fixture { app, mine, theirs }
}

/// The state every result test starts from — and the proof that a panel which
/// never changed could not pass any of them.
fn assert_shows_selection(app: &mut App) {
    let text = panel(app);
    assert!(
        text.contains("Worker") && text.contains("right-click"),
        "the fixture does not start on the selection line, so a result \
         assertion would prove nothing about a change: {text:?}"
    );
    for phrase in [RESULT_WIN, RESULT_LOSS, RESULT_DRAW, RESULT_INPUT_OFF] {
        assert!(!text.contains(phrase), "a running match already claims a result: {text:?}");
    }
}

// ---- the three outcomes, as rendered ----------------------------------------

#[test]
fn a_running_match_shows_no_result_at_all() {
    let mut f = contested(60.0);
    assert_shows_selection(&mut f.app);
    // ...and goes on not claiming one.
    tick(&mut f.app, 120);
    assert_shows_selection(&mut f.app);
    assert!(
        f.app.world().resource::<MatchState>().outcome().is_none(),
        "the fixture decided on its own"
    );
}

#[test]
fn destroying_the_enemy_hq_tells_the_player_they_won() {
    let mut f = contested(60.0);
    assert_shows_selection(&mut f.app);

    f.app.world_mut().despawn(f.theirs);
    step(&mut f.app);

    let text = panel(&mut f.app);
    assert!(text.contains(RESULT_WIN), "the win is not on the panel: {text:?}");
    assert!(!text.contains(RESULT_LOSS) && !text.contains(RESULT_DRAW));
    assert!(
        text.contains(RESULT_INPUT_OFF),
        "the panel does not say why orders stopped working: {text:?}"
    );
    // The result replaces the selection line rather than sitting under it.
    assert!(
        !text.contains("right-click"),
        "a decided match still offers orders: {text:?}"
    );
}

#[test]
fn losing_your_hq_tells_the_player_they_lost() {
    let mut f = contested(60.0);
    assert_shows_selection(&mut f.app);

    f.app.world_mut().despawn(f.mine);
    step(&mut f.app);

    let text = panel(&mut f.app);
    assert!(text.contains(RESULT_LOSS), "the loss is not on the panel: {text:?}");
    assert!(!text.contains(RESULT_WIN) && !text.contains(RESULT_DRAW));
    assert!(text.contains(RESULT_INPUT_OFF));
    assert!(!text.contains("right-click"));
}

/// The draw is reachable — both HQs falling on one tick is a draw, not a race
/// (F-010) — and must be reported as neither a win nor a loss.
#[test]
fn losing_both_hqs_on_one_tick_is_reported_as_a_draw() {
    let mut f = contested(60.0);
    assert_shows_selection(&mut f.app);

    f.app.world_mut().despawn(f.mine);
    f.app.world_mut().despawn(f.theirs);
    step(&mut f.app);

    let text = panel(&mut f.app);
    assert!(text.contains(RESULT_DRAW), "the draw is not on the panel: {text:?}");
    assert!(
        !text.contains(RESULT_WIN) && !text.contains(RESULT_LOSS),
        "a draw was reported as a win or a loss: {text:?}"
    );
    assert!(text.contains(RESULT_INPUT_OFF));
}

/// The three outcomes are three *different* things on screen. (A win and a loss
/// that rendered identically would pass every test above that only looks for
/// its own phrase.)
#[test]
fn the_three_outcomes_read_differently() {
    let render = |kill_mine: bool, kill_theirs: bool| {
        let mut f = contested(60.0);
        if kill_mine {
            f.app.world_mut().despawn(f.mine);
        }
        if kill_theirs {
            f.app.world_mut().despawn(f.theirs);
        }
        step(&mut f.app);
        panel(&mut f.app)
    };
    let win = render(false, true);
    let loss = render(true, false);
    let draw = render(true, true);
    assert_ne!(win, loss);
    assert_ne!(win, draw);
    assert_ne!(loss, draw);
}

// ---- "when", from ticks and the sim's own timestep --------------------------

/// The panel says *when* the match was decided, and the figure is the sim's
/// tick count converted by its **fixed timestep** — not a wall clock. Proven by
/// running the same match at half the tick rate: the same tick number, twice
/// the elapsed match time.
#[test]
fn the_result_says_when_and_derives_it_from_the_sims_own_timestep() {
    let decide_at = |hz: f64| {
        let mut f = contested(hz);
        tick(&mut f.app, 118); // 120 ticks in total before the kill
        f.app.world_mut().despawn(f.theirs);
        step(&mut f.app);
        (
            panel(&mut f.app),
            f.app
                .world()
                .resource::<MatchState>()
                .outcome()
                .expect("decided")
                .tick,
        )
    };
    let (fast, fast_tick) = decide_at(60.0);
    let (slow, slow_tick) = decide_at(30.0);
    assert_eq!(fast_tick, slow_tick, "the fixture decided on different ticks");
    assert!(
        fast.contains(&format!("tick {fast_tick}")),
        "the panel does not name the deciding tick: {fast:?}"
    );
    // 120 ticks is two seconds at 60 Hz and four at 30 Hz — the *same* tick
    // count reading as different match time is the whole point: the figure comes
    // from the sim's timestep, and nothing else could produce both.
    assert!(fast.contains("0:02"), "60 Hz: 120 ticks is 0:02, got {fast:?}");
    assert!(slow.contains("0:04"), "30 Hz: 120 ticks is 0:04, got {slow:?}");
}

/// The pure formatter, on its own: the clock, and the three headlines.
#[test]
fn the_result_text_formats_every_outcome_and_clock() {
    assert_eq!(match_clock(0, 1.0 / 60.0), "0:00");
    assert_eq!(match_clock(59, 1.0 / 60.0), "0:00");
    assert_eq!(match_clock(60, 1.0 / 60.0), "0:01");
    assert_eq!(match_clock(4_625, 1.0 / 60.0), "1:17");
    assert_eq!(match_clock(28_800, 1.0 / 60.0), "8:00");
    // A timestep that is not exact in binary: truncating `120 * (1/30)` reports
    // 3 seconds for a four-second match, so the product is rounded to the
    // millisecond before it is truncated to seconds.
    assert_eq!(match_clock(120, 1.0 / 30.0), "0:04");
    assert_eq!(match_clock(119, 1.0 / 30.0), "0:03");

    let at = |winner| MatchOutcome { winner, tick: 600 };
    let dt = 1.0 / 60.0;
    assert!(match_result_text(at(Some(PLAYER_FACTION)), dt).starts_with(RESULT_WIN));
    assert!(match_result_text(at(Some(Faction::B)), dt).starts_with(RESULT_LOSS));
    assert!(match_result_text(at(None), dt).starts_with(RESULT_DRAW));
    assert!(match_result_text(at(None), dt).contains("0:10"));
}

// ---- what this could break --------------------------------------------------

/// The panel's other jobs are untouched while the match runs: Alloy, a selected
/// resource node, and a selected building's production list all still render.
#[test]
fn a_running_match_still_shows_everything_the_panel_showed_before() {
    let mut f = contested(60.0);
    assert_shows_selection(&mut f.app);

    // A selected building still shows its Alloy and what it can train...
    let hq = f.mine;
    f.app.world_mut().entity_mut(hq).insert(Selected);
    step(&mut f.app);
    let text = panel(&mut f.app);
    assert!(
        text.contains("Alloy: 0") && text.contains("in production") && text.contains("Worker"),
        "the building panel is gone: {text:?}"
    );

    // ...and once the match ends, that gives way to the result.
    f.app.world_mut().despawn(f.theirs);
    step(&mut f.app);
    let text = panel(&mut f.app);
    assert!(text.contains(RESULT_WIN) && !text.contains("in production"), "{text:?}");
}

/// The panel is presentation: showing a result must not touch sim state. The
/// same match, played with the panel system and without it, ends in the same
/// world.
#[test]
fn rendering_the_result_does_not_touch_the_sim() {
    let play = |with_panel: bool| {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(Time::<Fixed>::from_hz(60.0))
            .insert_resource(content())
            .init_resource::<CommandQueue>()
            .init_resource::<RateReport>()
            .init_resource::<Casualties>()
            .insert_resource(Stockpiles::starting(0));
        onus::add_sim_systems(&mut app, Update);
        if with_panel {
            app.add_systems(
                Update,
                onus::ui::update_options_panel.after(onus::sim::victory::match_end),
            );
            app.world_mut().spawn((Text::new(""), OptionsPanel));
        }
        let mine = spawn_building(&mut app, PLAYER_FACTION, Vec2::new(-400.0, 0.0));
        let theirs = spawn_building(&mut app, Faction::B, Vec2::new(400.0, 0.0));
        spawn_selected_worker(&mut app);
        let _ = mine;
        tick(&mut app, 3);
        app.world_mut().despawn(theirs);
        tick(&mut app, 30);
        (
            app.world().resource::<MatchState>().outcome(),
            onus::sim::state_hash(app.world_mut()),
        )
    };
    let (with, hash_with) = play(true);
    let (without, hash_without) = play(false);
    assert!(with.is_some(), "the fixture never decided");
    assert_eq!(with, without, "the panel changed the outcome");
    assert_eq!(hash_with, hash_without, "the panel changed the world");
}

/// The sim stays free of presentation: no wording, no panel, no player-side
/// notion of "you" anywhere under `src/sim/`.
#[test]
fn the_sim_knows_nothing_about_the_panel() {
    let sim = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sim");
    let mut files = Vec::new();
    let mut stack = vec![sim];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).expect("read src/sim") {
            let p = e.expect("entry").path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                files.push(p);
            }
        }
    }
    files.sort();
    assert!(!files.is_empty());
    let mut offenders = Vec::new();
    for f in &files {
        let text = std::fs::read_to_string(f).expect("read");
        for (i, line) in text.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            for needle in [
                "OptionsPanel",
                "PLAYER_FACTION",
                "RESULT_WIN",
                "RESULT_LOSS",
                "RESULT_DRAW",
                "match_result_text",
                "VICTORY",
                "DEFEAT",
            ] {
                if code.contains(needle) {
                    offenders.push(format!("{}:{}: {}", f.display(), i + 1, code.trim()));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "presentation leaked into the sim: {offenders:#?}"
    );
}

/// `UnitKind` is used by the fixture; keep the import honest.
#[test]
fn the_fixture_selects_a_real_unit() {
    let mut f = contested(60.0);
    let mut q = f.app.world_mut().query_filtered::<&UnitKind, With<Selected>>();
    assert_eq!(q.iter(f.app.world()).count(), 1);
}
