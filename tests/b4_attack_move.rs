//! B4 turtle, step 1: **`Order::AttackMove`** (BALANCE_PLAN "Lever chosen …
//! attack-move", F-047, F-048).
//!
//! F-047 measured why the turtle wins: attackers on a plain `MoveTo` walk
//! through the defender's fire without fighting back, and their nearest-target
//! hits go to workers and buildings. The lever is a second order, used by AI
//! waves when the strategy's RON says so:
//!
//! - a unit under `AttackMove { dest }` marches to `dest`, and on the way
//!   engages enemy **armed** units (offense > 0) inside `attack_move_range`
//!   (shipped equal to `engage_range`; the step-2 tests below vary it), chasing
//!   to the `pursue_range` leash exactly as an idle unit does;
//! - armed units come before workers and buildings; the nearest unarmed target
//!   is the fallback only when no armed unit is in range (so a wave reaching
//!   the HQ still razes it);
//! - when the fight ends (target dead or past the leash) it resumes the march
//!   to the **same** `dest`;
//! - a plain `MoveTo` is unchanged: a pure move, no chase.
//!
//! The switch is content: `StrategyDef::wave_order`, `move` or `attack_move`,
//! required in `strategies.ron`. Step 1 ships `move` everywhere, so shipped
//! behaviour is unchanged.
//!
//! Every distance below is read from the loaded content (F-044): no shipped
//! balance value is pinned here.

use std::path::PathBuf;

use bevy::prelude::*;

use onus::batch::seed_at;
use onus::headless::{self, MatchSettings};
use onus::sim::combat::{Casualties, Engaging, Health, Target};
use onus::sim::content::{Content, WaveOrder};
use onus::sim::economy::{Building, ProductionQueue, Stockpiles, UnitDefIdx};
use onus::sim::replay::{CommandLog, LoggedOrder, ReplaySource, StateHashLog};
use onus::sim::spatial::Faction;
use onus::sim::{AiCommanders, AttackMoveDest, CommandQueue, MoveTarget, Order, Position, RateReport};

// ---- harness ----------------------------------------------------------------

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data")
}

fn content() -> Content {
    Content::load_from_dir(&data_dir()).expect("assets/data/*.ron parse into sim structs")
}

/// The shipped sim chain on `Update`, one `step()` == one 60 Hz tick (as in
/// `m4b_combat`).
fn sim_app() -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .insert_resource(Time::<Fixed>::from_hz(60.0))
        .insert_resource(content())
        .init_resource::<CommandQueue>()
        .init_resource::<RateReport>()
        .init_resource::<Casualties>()
        .insert_resource(Stockpiles::default());
    onus::add_sim_systems(&mut app, Update);
    app
}

fn step(app: &mut App) {
    let dt = app.world().resource::<Time<Fixed>>().timestep();
    app.world_mut().resource_mut::<Time<Fixed>>().advance_by(dt);
    app.update();
}

fn spawn_unit(app: &mut App, id: &str, faction: Faction, pos: Vec2) -> Entity {
    let (idx, kind, hp) = {
        let c = app.world().resource::<Content>();
        let idx = c.unit_index(id).unwrap_or_else(|| panic!("unit `{id}`"));
        (idx, c.units[idx].mvp_kind, Health::from_def(c, idx))
    };
    app.world_mut()
        .spawn((Position(pos), UnitDefIdx(idx), kind, faction, hp))
        .id()
}

fn spawn_building(app: &mut App, id: &str, faction: Faction, pos: Vec2) -> Entity {
    let (def, hp) = {
        let c = app.world().resource::<Content>();
        let def = c.building_index(id).unwrap_or_else(|| panic!("building `{id}`"));
        (def, Health::from_building_def(c, def))
    };
    app.world_mut()
        .spawn((Position(pos), Building { def }, faction, ProductionQueue::default(), hp))
        .id()
}

fn order(app: &mut App, o: Order) {
    app.world_mut().resource_mut::<CommandQueue>().0.push_back(o);
}

fn pos(app: &App, e: Entity) -> Option<Vec2> {
    app.world().get::<Position>(e).map(|p| p.0)
}

fn hp(app: &App, e: Entity) -> Option<u32> {
    app.world().get::<Health>(e).map(|h| h.current)
}

fn alive(app: &App, e: Entity) -> bool {
    app.world().get_entity(e).is_ok()
}

fn engaging(app: &App, e: Entity) -> bool {
    app.world().get::<Engaging>(e).is_some()
}

fn target(app: &App, e: Entity) -> Option<Entity> {
    app.world().get::<Target>(e).map(|t| t.0)
}

fn attack_dest(app: &App, e: Entity) -> Option<Vec2> {
    app.world().get::<AttackMoveDest>(e).map(|d| d.0)
}

/// Hold `sentry` where it stands for one tick: a commander `MoveTo` to its own
/// position. A unit on a commander order does not chase (that is the very rule
/// under test for `MoveTo`), so a held sentry neither walks into the attacker
/// nor runs away; it still hits anything inside its own reach.
fn hold(app: &mut App, sentry: Entity, side: Faction) {
    if let Some(p) = pos(app, sentry) {
        order(
            app,
            Order::MoveTo {
                units: vec![sentry],
                dest: p,
            }
            .issued_by(side),
        );
    }
}

/// The attacker, the sentry and the lateral offset for the walk-past fixture:
/// a sentry beside the attacker's line of march, **inside** `engage_range` and
/// **outside** both units' own attack reach. So a unit that only fights what is
/// already in reach (a `MoveTo` unit) walks past it, and only engagement can
/// make the two fight.
fn walk_past_offset(c: &Content, attacker: &str, sentry: &str) -> f32 {
    let reach = c.unit(attacker).unwrap().mvp_attack_range.max(c.unit(sentry).unwrap().mvp_attack_range);
    let engage = c.combat.engage_range;
    assert!(reach < engage, "the fixture needs attack reach < engage_range");
    (reach + engage) / 2.0
}

const ATTACKER: &str = "sentinel";
const SENTRY: &str = "ripper";

// ---- 1. engages an armed enemy that a MoveTo unit walks past ----------------

#[test]
fn an_attack_moving_unit_engages_an_armed_enemy_a_move_to_unit_walks_past() {
    let c = content();
    let off = walk_past_offset(&c, ATTACKER, SENTRY);
    let start = Vec2::ZERO;
    let dest = Vec2::new(2.0 * c.combat.pursue_range + 400.0, 0.0);
    let sentry_at = Vec2::new(dest.x / 2.0, off);

    // ---- control: a plain MoveTo walks straight past --------------------
    let mut app = sim_app();
    let mover = spawn_unit(&mut app, ATTACKER, Faction::A, start);
    let sentry = spawn_unit(&mut app, SENTRY, Faction::B, sentry_at);
    let sentry_max = hp(&app, sentry).unwrap();
    order(
        &mut app,
        Order::MoveTo {
            units: vec![mover],
            dest,
        }
        .issued_by(Faction::A),
    );
    let mut arrived = false;
    for _ in 0..20_000 {
        hold(&mut app, sentry, Faction::B);
        step(&mut app);
        let p = pos(&app, mover).expect("the mover is never in reach of the sentry");
        assert_eq!(p.y, 0.0, "a MoveTo unit left its line of march");
        assert!(!engaging(&app, mover), "a MoveTo unit engaged");
        assert_eq!(attack_dest(&app, mover), None, "a MoveTo carries no attack-move");
        if p == dest {
            arrived = true;
            break;
        }
    }
    assert!(arrived, "the MoveTo unit never reached its destination");
    assert_eq!(hp(&app, sentry), Some(sentry_max), "the MoveTo unit hit the sentry");

    // ---- attack-move: the same march engages the sentry ----------------
    let mut app = sim_app();
    let mover = spawn_unit(&mut app, ATTACKER, Faction::A, start);
    let sentry = spawn_unit(&mut app, SENTRY, Faction::B, sentry_at);
    order(
        &mut app,
        Order::AttackMove {
            units: vec![mover],
            dest,
        }
        .issued_by(Faction::A),
    );
    let mut left_line = false;
    let mut chased = false;
    let mut hurt = false;
    for _ in 0..20_000 {
        hold(&mut app, sentry, Faction::B);
        step(&mut app);
        let Some(p) = pos(&app, mover) else { break };
        left_line |= p.y != 0.0;
        chased |= engaging(&app, mover);
        hurt |= hp(&app, sentry).is_none_or(|h| h < sentry_max);
        if hurt && left_line && chased {
            break;
        }
    }
    assert!(chased, "the attack-mover never engaged the armed sentry in engage_range");
    assert!(left_line, "the attack-mover never left its line to close on the sentry");
    assert!(hurt, "the attack-mover never hit the sentry");
}

// ---- 2. resumes and reaches dest after the fight ----------------------------

#[test]
fn an_attack_moving_unit_resumes_its_march_and_reaches_dest_after_the_fight() {
    let c = content();
    let off = walk_past_offset(&c, ATTACKER, SENTRY);
    let dest = Vec2::new(2.0 * c.combat.pursue_range + 400.0, 0.0);
    let sentry_at = Vec2::new(dest.x / 2.0, off);

    let mut app = sim_app();
    // A group, so the fight is decided by numbers and not by a stat line.
    let group: Vec<Entity> = (0..4)
        .map(|i| spawn_unit(&mut app, ATTACKER, Faction::A, Vec2::new(0.0, -10.0 * i as f32)))
        .collect();
    let sentry = spawn_unit(&mut app, SENTRY, Faction::B, sentry_at);
    order(
        &mut app,
        Order::AttackMove {
            units: group.clone(),
            dest,
        }
        .issued_by(Faction::A),
    );
    let mut max_y = 0.0f32;
    let mut sentry_died_at = None;
    let mut done_at = None;
    for t in 1..=40_000u32 {
        hold(&mut app, sentry, Faction::B);
        step(&mut app);
        for &u in &group {
            if let Some(p) = pos(&app, u) {
                max_y = max_y.max(p.y);
            }
        }
        if sentry_died_at.is_none() && !alive(&app, sentry) {
            sentry_died_at = Some(t);
        }
        let living: Vec<Entity> = group.iter().copied().filter(|u| alive(&app, *u)).collect();
        if sentry_died_at.is_some() && !living.is_empty() && living.iter().all(|u| pos(&app, *u) == Some(dest)) {
            done_at = Some(t);
            break;
        }
    }
    assert!(sentry_died_at.is_some(), "the group never killed the sentry");
    assert!(max_y > 0.0, "the group never left its line: it did not fight, it walked");
    let done = done_at.expect("after the fight the survivors never reached dest");
    assert!(done > sentry_died_at.unwrap());
    // Arrival completes the order: nothing left to march to, no stale chase.
    step(&mut app);
    for &u in group.iter().filter(|u| alive(&app, **u)) {
        assert_eq!(attack_dest(&app, u), None, "an arrived attack-mover still holds its order");
        assert!(!engaging(&app, u));
        assert_eq!(app.world().get::<MoveTarget>(u).map(|m| m.0), None);
    }
}

// ---- 3. armed units before workers and buildings ----------------------------

/// The priority fixture: from the attacker, a worker is nearest (inside its
/// attack reach), a building next, and an armed unit farthest, but still inside
/// `engage_range`.
fn priority_fixture(app: &mut App) -> (Entity, Entity, Entity, Entity) {
    let (reach, engage) = {
        let c = app.world().resource::<Content>();
        (c.unit(ATTACKER).unwrap().mvp_attack_range, c.combat.engage_range)
    };
    let me = spawn_unit(app, ATTACKER, Faction::A, Vec2::ZERO);
    let worker = spawn_unit(app, "worker", Faction::B, Vec2::new(reach / 2.0, 0.0));
    let bld = spawn_building(app, "foundry", Faction::B, Vec2::new(0.0, -reach * 0.75));
    let armed = spawn_unit(app, SENTRY, Faction::B, Vec2::new(0.0, (reach + engage) / 2.0));
    (me, worker, bld, armed)
}

#[test]
fn an_attack_moving_unit_prefers_an_armed_unit_over_a_nearer_worker_or_building() {
    let mut app = sim_app();
    let (me, worker, bld, armed) = priority_fixture(&mut app);
    let (w_max, b_max) = (hp(&app, worker).unwrap(), hp(&app, bld).unwrap());
    order(
        &mut app,
        Order::AttackMove {
            units: vec![me],
            dest: Vec2::new(5_000.0, 0.0),
        }
        .issued_by(Faction::A),
    );
    step(&mut app);
    assert_eq!(target(&app, me), Some(armed), "attack-move picked a non-combatant first");
    // Fight until the armed unit is gone (or it is): the worker and building
    // take nothing from the attack-mover while an armed enemy is in range.
    for _ in 0..5_000 {
        if !alive(&app, armed) || !alive(&app, me) {
            break;
        }
        hold(&mut app, armed, Faction::B);
        step(&mut app);
        if alive(&app, armed) && alive(&app, me) {
            assert_eq!(target(&app, me), Some(armed));
        }
    }
    assert_eq!(hp(&app, worker), Some(w_max), "the worker was hit while an armed unit stood");
    assert_eq!(hp(&app, bld), Some(b_max), "the building was hit while an armed unit stood");
}

/// The control: an **idle** unit keeps the pure nearest-target rule (Step 1
/// changes only attack-move behaviour).
#[test]
fn an_idle_unit_keeps_the_pure_nearest_target_rule() {
    let mut app = sim_app();
    let (me, worker, _, _) = priority_fixture(&mut app);
    step(&mut app);
    assert_eq!(target(&app, me), Some(worker), "idle targeting changed");
}

// ---- 4. still kills the HQ when nothing armed is left -----------------------

#[test]
fn an_attack_moving_wave_still_razes_the_hq_when_nothing_armed_is_left() {
    let mut app = sim_app();
    let hq_at = Vec2::new(600.0, 0.0);
    let hq = spawn_building(&mut app, "hq", Faction::B, hq_at);
    let miner = spawn_unit(&mut app, "worker", Faction::B, hq_at + Vec2::new(0.0, 40.0));
    let wave: Vec<Entity> = (0..4)
        .map(|i| spawn_unit(&mut app, ATTACKER, Faction::A, Vec2::new(0.0, 10.0 * i as f32)))
        .collect();
    order(
        &mut app,
        Order::AttackMove {
            units: wave,
            dest: hq_at,
        }
        .issued_by(Faction::A),
    );
    for _ in 0..100_000 {
        if !alive(&app, hq) {
            break;
        }
        step(&mut app);
    }
    assert!(!alive(&app, hq), "an attack-move wave with nothing armed against it left the HQ standing");
    let _ = miner;
}

// ---- 5. MoveTo unchanged, and an explicit order ends an attack-move ---------

#[test]
fn a_move_to_or_gather_order_ends_an_attack_move() {
    let mut app = sim_app();
    let u = spawn_unit(&mut app, ATTACKER, Faction::A, Vec2::ZERO);
    let dest = Vec2::new(3_000.0, 0.0);
    order(&mut app, Order::AttackMove { units: vec![u], dest }.issued_by(Faction::A));
    step(&mut app);
    assert_eq!(attack_dest(&app, u), Some(dest));
    assert_eq!(app.world().get::<MoveTarget>(u).map(|m| m.0), Some(dest));
    let elsewhere = Vec2::new(0.0, 900.0);
    order(
        &mut app,
        Order::MoveTo {
            units: vec![u],
            dest: elsewhere,
        }
        .issued_by(Faction::A),
    );
    step(&mut app);
    assert_eq!(attack_dest(&app, u), None, "a MoveTo did not replace the attack-move");
    assert_eq!(app.world().get::<MoveTarget>(u).map(|m| m.0), Some(elsewhere));
}

/// A `MoveTo` unit with an armed enemy inside `engage_range` (but outside its
/// own reach) never chases, never stops, and lands exactly on `dest`; the same
/// fixture as test 1, seen from the order that must not change.
#[test]
fn move_to_stays_a_pure_move_beside_an_enemy_in_engage_range() {
    let c = content();
    let off = walk_past_offset(&c, ATTACKER, SENTRY);
    let mut app = sim_app();
    let u = spawn_unit(&mut app, ATTACKER, Faction::A, Vec2::ZERO);
    // The sentry stands beside the *start*, so it is in engage range on tick 1.
    let sentry = spawn_unit(&mut app, SENTRY, Faction::B, Vec2::new(0.0, off));
    let dest = Vec2::new(1_000.0, 0.0);
    order(&mut app, Order::MoveTo { units: vec![u], dest }.issued_by(Faction::A));
    let speed = onus::sim::combat::move_speed(&c, c.unit_index(ATTACKER).unwrap()) / 60.0;
    let mut last_x = 0.0f32;
    for _ in 0..5_000 {
        hold(&mut app, sentry, Faction::B);
        step(&mut app);
        let p = pos(&app, u).unwrap();
        assert_eq!(p.y, 0.0);
        assert!(!engaging(&app, u));
        if p == dest {
            break;
        }
        // It never stops to fight: full speed every tick.
        assert!((p.x - last_x - speed).abs() < 1e-3, "a MoveTo unit slowed or stopped");
        last_x = p.x;
    }
    assert_eq!(pos(&app, u), Some(dest));
}

// ---- 6. the switch is content; an attack-move match is deterministic --------

#[test]
fn every_shipped_strategy_ships_wave_order_move() {
    let c = content();
    for s in &c.strategies {
        assert_eq!(s.wave_order, WaveOrder::Move, "strategy `{}`: step 1 ships neutral", s.id);
    }
    assert_eq!(c.ai.wave_order, WaveOrder::Move);
}

/// A scratch content dir: the shipped files, with `strategies.ron` rewritten.
fn scratch_content(name: &str, edit: impl Fn(&str) -> String) -> PathBuf {
    let out = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target").join(name);
    std::fs::create_dir_all(&out).expect("scratch dir");
    for file in ["units.ron", "resources.ron"] {
        std::fs::copy(data_dir().join(file), out.join(file)).expect("copy");
    }
    let text = std::fs::read_to_string(data_dir().join("strategies.ron")).expect("strategies");
    std::fs::write(out.join("strategies.ron"), edit(&text)).expect("write");
    out
}

#[test]
fn a_missing_wave_order_is_a_load_error_not_a_default() {
    let dir = scratch_content("b4_am_missing_wave_order", |t| {
        let s = t.replacen("            wave_order: move,\n", "", 1);
        assert_ne!(s, t, "the shipped set has no wave_order to strip");
        s
    });
    let err = Content::load_from_dir(&dir).expect_err("a strategy with no wave_order must not load");
    assert!(format!("{err:?}").contains("wave_order"), "the error does not name the field: {err:?}");
}

#[test]
fn attack_move_parses_from_the_ron_and_moves_the_fingerprint() {
    let dir = scratch_content("b4_am_all_attack_move", |t| {
        t.replace("wave_order: move,", "wave_order: attack_move,")
    });
    let am = Content::load_from_dir(&dir).expect("attack_move is a valid wave order");
    assert!(am.strategies.iter().all(|s| s.wave_order == WaveOrder::AttackMove));
    assert_ne!(
        am.fingerprint().hash(),
        content().fingerprint().hash(),
        "the wave order is content the fingerprint must see"
    );
}

/// The shipped content with every strategy's wave order set to `w`.
fn content_with_wave_order(w: WaveOrder) -> Content {
    let mut c = content();
    for s in &mut c.strategies {
        s.wave_order = w;
    }
    c.ai.wave_order = w;
    c
}

const AM_TICKS: u32 = 6_000;

/// `rush` against `mvp`: both attack inside the window, so the waves meet.
fn am_settings() -> MatchSettings {
    MatchSettings::default()
        .with_seed(seed_at(0, 3))
        .with_strategies("rush", "mvp")
        .with_hashing(true)
}

fn run(content: Content, ticks: u32) -> App {
    let mut app = headless::ai_vs_ai(content, &am_settings()).expect("shipped strategies");
    headless::tick(&mut app, ticks);
    app
}

fn hashes(app: &App) -> Vec<u64> {
    app.world().resource::<StateHashLog>().0.clone()
}

#[test]
fn the_ai_issues_the_wave_order_its_strategy_names() {
    for (w, want_am) in [(WaveOrder::Move, false), (WaveOrder::AttackMove, true)] {
        let app = run(content_with_wave_order(w), AM_TICKS);
        let log = app.world().resource::<CommandLog>();
        let (mut moves, mut ams) = (0, 0);
        for cmd in log.commands() {
            match cmd.order {
                LoggedOrder::MoveTo { .. } => moves += 1,
                LoggedOrder::AttackMove { .. } => ams += 1,
                _ => {}
            }
        }
        if want_am {
            assert!(ams > 0, "attack_move content issued no AttackMove");
            assert_eq!(moves, 0, "attack_move content still sent a wave on MoveTo");
        } else {
            assert!(moves > 0, "move content issued no MoveTo wave");
            assert_eq!(ams, 0, "move content issued an AttackMove");
        }
    }
}

#[test]
fn an_attack_move_match_is_deterministic_and_replays_tick_for_tick() {
    let am = content_with_wave_order(WaveOrder::AttackMove);
    let a = run(am.clone(), AM_TICKS);
    let b = run(am.clone(), AM_TICKS);
    let (ha, hb) = (hashes(&a), hashes(&b));
    assert!(!ha.is_empty());
    assert_eq!(ha, hb, "two runs of one attack-move match diverged");

    // It is not the move match under another name.
    let mv = run(content_with_wave_order(WaveOrder::Move), AM_TICKS);
    assert_ne!(hashes(&mv), ha, "the wave order changed nothing in this match");

    // Replayed from its own log with the AI stood down: identical per tick.
    let log = a.world().resource::<CommandLog>().log().clone();
    let mut rep = headless::ai_vs_ai(am, &am_settings()).expect("shipped strategies");
    rep.world_mut().remove_resource::<AiCommanders>();
    rep.insert_resource(AiCommanders::default());
    rep.insert_resource(StateHashLog::default());
    rep.insert_resource(CommandLog::new(log.seed));
    rep.insert_resource(ReplaySource::new(log));
    headless::tick(&mut rep, AM_TICKS);
    assert_eq!(hashes(&rep), ha, "the attack-move log did not replay tick for tick");
}

// ---- step 2: the mechanism's own number, `mvp_combat.attack_move_range` -----
//
// F-048: with every wave on attack-move the turtle drops from 84% to 49%, but
// the early committers turn dominant. The mechanism needs a number of its
// own to tune (BALANCE_PLAN step 2, at most 3 adjustments), separate from the
// idle `engage_range` it borrowed. `attack_move_range` is the radius in which
// an attack-mover acquires a target. It is used by attack-movers only, so it
// cannot move a match in which no strategy ships `attack_move`.

fn sim_app_with(c: Content) -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .insert_resource(Time::<Fixed>::from_hz(60.0))
        .insert_resource(c)
        .init_resource::<CommandQueue>()
        .init_resource::<RateReport>()
        .init_resource::<Casualties>()
        .insert_resource(Stockpiles::default());
    onus::add_sim_systems(&mut app, Update);
    app
}

/// The shipped data with `units.ron` edited by `edit`.
fn scratch_units(name: &str, edit: impl Fn(&str) -> String) -> PathBuf {
    let out = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target").join(name);
    std::fs::create_dir_all(&out).expect("scratch dir");
    for file in ["strategies.ron", "resources.ron"] {
        std::fs::copy(data_dir().join(file), out.join(file)).expect("copy");
    }
    let text = std::fs::read_to_string(data_dir().join("units.ron")).expect("units");
    std::fs::write(out.join("units.ron"), edit(&text)).expect("write");
    out
}

/// Replace the value on the `attack_move_range:` line of `units.ron`.
fn with_attack_move_range(text: &str, value: &str) -> String {
    let mut found = false;
    let s = text
        .lines()
        .map(|l| {
            let t = l.trim_start();
            if !found && t.starts_with("attack_move_range:") {
                found = true;
                let indent = &l[..l.len() - t.len()];
                format!("{indent}attack_move_range: {value},")
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(found, "units.ron has no attack_move_range line");
    s
}

/// The longest attack reach of any unit: an attack-mover must at least be able
/// to fight what it can already hit.
fn longest_reach(c: &Content) -> f32 {
    c.units.iter().map(|u| u.mvp_attack_range).fold(0.0, f32::max)
}

#[test]
fn attack_move_range_is_a_required_combat_number() {
    let dir = scratch_units("b4_am_missing_range", |t| {
        let s: String = t
            .lines()
            .filter(|l| !l.trim_start().starts_with("attack_move_range:"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_ne!(s.len(), t.len(), "units.ron has no attack_move_range to strip");
        s
    });
    let err = Content::load_from_dir(&dir).expect_err("no attack_move_range must not load");
    assert!(
        format!("{err:?}").contains("attack_move_range"),
        "the error does not name the field: {err:?}"
    );
}

#[test]
fn attack_move_range_out_of_bounds_is_refused() {
    let c = content();
    let below = longest_reach(&c) - 1.0;
    let above = c.combat.pursue_range + 1.0;
    for (tag, v) in [
        ("below_reach", format!("{below:.1}")),
        ("above_leash", format!("{above:.1}")),
        ("nan", "NaN".to_string()),
    ] {
        let dir = scratch_units(&format!("b4_am_range_{tag}"), |t| with_attack_move_range(t, &v));
        let err = Content::load_from_dir(&dir)
            .expect_err(&format!("attack_move_range {v} ({tag}) must not load"));
        assert!(format!("{err:?}").contains("attack_move_range"), "{tag}: {err:?}");
    }
    // Both bounds are inclusive.
    for (tag, v) in [
        ("at_reach", format!("{:.1}", longest_reach(&c))),
        ("at_leash", format!("{:.1}", c.combat.pursue_range)),
    ] {
        let dir = scratch_units(&format!("b4_am_range_{tag}"), |t| with_attack_move_range(t, &v));
        let got = Content::load_from_dir(&dir).unwrap_or_else(|e| panic!("{tag}: {e:?}"));
        assert_ne!(got.fingerprint().hash(), 0);
    }
}

#[test]
fn attack_move_range_moves_the_fingerprint() {
    let c = content();
    let v = format!("{:.1}", longest_reach(&c));
    let dir = scratch_units("b4_am_range_fp", |t| with_attack_move_range(t, &v));
    let other = Content::load_from_dir(&dir).expect("a valid range loads");
    assert_ne!(other.combat.attack_move_range, c.combat.attack_move_range);
    assert_ne!(other.fingerprint().hash(), c.fingerprint().hash());
}

/// An attack-mover acquires inside `attack_move_range`, not `engage_range`:
/// with the range at the longest reach, a sentry beside the march (outside
/// that range, inside `engage_range`) is walked past; with the range opened to
/// `engage_range` the same march engages it.
#[test]
fn an_attack_mover_acquires_only_inside_attack_move_range() {
    let shipped = content();
    let lo = longest_reach(&shipped);
    let engage = shipped.combat.engage_range;
    assert!(lo < engage, "the fixture needs longest reach < engage_range");
    let off = (lo + engage) / 2.0;
    let dest = Vec2::new(2.0 * shipped.combat.pursue_range + 400.0, 0.0);
    let sentry_at = Vec2::new(dest.x / 2.0, off);

    let march = |range: f32| -> (bool, bool) {
        let mut c = content();
        c.combat.attack_move_range = range;
        let mut app = sim_app_with(c);
        let mover = spawn_unit(&mut app, ATTACKER, Faction::A, Vec2::ZERO);
        let sentry = spawn_unit(&mut app, SENTRY, Faction::B, sentry_at);
        order(
            &mut app,
            Order::AttackMove {
                units: vec![mover],
                dest,
            }
            .issued_by(Faction::A),
        );
        let (mut chased, mut arrived) = (false, false);
        for _ in 0..20_000 {
            hold(&mut app, sentry, Faction::B);
            step(&mut app);
            let Some(p) = pos(&app, mover) else { break };
            chased |= engaging(&app, mover);
            if p == dest {
                arrived = true;
                break;
            }
        }
        (chased, arrived)
    };

    let (chased, arrived) = march(lo);
    assert!(!chased, "an attack-mover engaged a sentry outside attack_move_range ({lo})");
    assert!(arrived, "the attack-mover never reached dest");
    let (chased, _) = march(engage);
    assert!(chased, "an attack-mover with the range at engage_range did not engage");
}
