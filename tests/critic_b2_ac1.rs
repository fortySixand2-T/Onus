//! Critic probes for **B2 AC1** — the lifted headless-match constructor.
//!
//! Every probe here is one that *should* pass per the spec. The differential
//! oracle is the pre-B2 bench fixture itself, reconstructed verbatim from
//! `git show 6e690fe:benches/replay_hash.rs`, so "lifted unchanged" is checked
//! against the actual prior code rather than against constants the lift itself
//! produced.

use std::path::PathBuf;

use bevy::prelude::*;

use onus::headless::{self, MatchSettings};
use onus::sim::combat::{Casualties, Health};
use onus::sim::content::Content;
use onus::sim::economy::{Building, ProductionQueue, Stockpiles, UnitDefIdx};
use onus::sim::spatial::Faction;
use onus::sim::{
    AiCommanders, AiJournal, CommandLog, CommandQueue, Position, RateReport, ResourceNode,
    StateHashLog,
};

// ---------------------------------------------------------------------------
// The pre-B2 fixture, copied verbatim out of the parent commit's bench.
// ---------------------------------------------------------------------------

fn legacy_content() -> Content {
    Content::load_from_dir(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data"))
        .expect("assets/data")
}

fn legacy_step(app: &mut App) {
    let dt = app.world().resource::<Time<Fixed>>().timestep();
    app.world_mut().resource_mut::<Time<Fixed>>().advance_by(dt);
    app.update();
}

fn legacy_ai_vs_ai(seed: u64, hashing: bool) -> App {
    let c = legacy_content();
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

fn content() -> Content {
    headless::content().expect("assets/data")
}

fn run(mut app: App, ticks: u32) -> (Vec<u64>, Vec<(u32, Faction, onus::sim::AiAction)>) {
    headless::tick(&mut app, ticks);
    let hashes = app.world().resource::<StateHashLog>().0.clone();
    let journal = app.world().resource::<AiJournal>().0.clone();
    (hashes, journal)
}

// ---- P1: differential oracle against the real prior fixture ----------------

#[test]
fn p1_default_settings_match_the_pre_b2_fixture_over_a_long_horizon() {
    const TICKS: u32 = 1_500;
    let mut old = legacy_ai_vs_ai(4, true);
    for _ in 0..TICKS {
        legacy_step(&mut old);
    }
    let old_hashes = old.world().resource::<StateHashLog>().0.clone();
    let old_journal = old.world().resource::<AiJournal>().0.clone();

    let new = headless::ai_vs_ai(
        content(),
        &MatchSettings::default().with_seed(4).with_hashing(true),
    )
    .expect("default strategy is known");
    let (new_hashes, new_journal) = run(new, TICKS);

    assert_eq!(old_hashes.len(), TICKS as usize);
    assert_eq!(new_hashes.len(), TICKS as usize);
    let first_diff = old_hashes
        .iter()
        .zip(&new_hashes)
        .position(|(a, b)| a != b);
    assert_eq!(
        first_diff, None,
        "lifted fixture diverges from the pre-B2 bench fixture at tick {:?}",
        first_diff.map(|i| i + 1)
    );
    assert_eq!(old_journal, new_journal, "AI journals must be identical");
}

#[test]
fn p1b_the_pin_is_seed_sensitive_not_just_shape_sensitive() {
    // A different seed must move the pinned quantity, otherwise the pin proves
    // nothing about the run.
    let a = headless::ai_vs_ai(
        content(),
        &MatchSettings::default().with_seed(4).with_hashing(true),
    )
    .unwrap();
    let b = headless::ai_vs_ai(
        content(),
        &MatchSettings::default().with_seed(5).with_hashing(true),
    )
    .unwrap();
    let (ha, _) = run(a, 600);
    let (hb, _) = run(b, 600);
    assert_ne!(ha, hb);
}

// ---- P2: the four `Option` shapes ------------------------------------------

fn shape(a: Option<&str>, b: Option<&str>, seed: u64) -> MatchSettings {
    let mut s = MatchSettings::default().with_seed(seed).with_hashing(true);
    s.strategies = [a.map(str::to_string), b.map(str::to_string)];
    s
}

#[test]
fn p2_all_four_option_shapes_of_the_default_play_the_same_match() {
    let c = content();
    let d = c.default_strategy.clone();
    let base = run(
        headless::ai_vs_ai(content(), &shape(None, None, 21)).unwrap(),
        600,
    );
    for (a, b) in [
        (Some(d.as_str()), Some(d.as_str())),
        (Some(d.as_str()), None),
        (None, Some(d.as_str())),
    ] {
        let got = run(
            headless::ai_vs_ai(content(), &shape(a, b, 21)).unwrap(),
            600,
        );
        assert_eq!(
            got.0, base.0,
            "spelling the default as ({a:?}, {b:?}) changes the per-tick hashes"
        );
        assert_eq!(
            got.1, base.1,
            "spelling the default as ({a:?}, {b:?}) changes the AI journal"
        );
    }
}

#[test]
fn p2b_a_half_named_matchup_equals_the_fully_named_one() {
    let c = content();
    let d = c.default_strategy.clone();
    let half = run(
        headless::ai_vs_ai(content(), &shape(Some("rush"), None, 33)).unwrap(),
        900,
    );
    let full = run(
        headless::ai_vs_ai(content(), &shape(Some("rush"), Some(&d), 33)).unwrap(),
        900,
    );
    assert_eq!(half.0, full.0, "an unnamed side must play the default");
    assert_eq!(half.1, full.1);

    let half_b = run(
        headless::ai_vs_ai(content(), &shape(None, Some("turtle"), 33)).unwrap(),
        900,
    );
    let full_b = run(
        headless::ai_vs_ai(content(), &shape(Some(&d), Some("turtle"), 33)).unwrap(),
        900,
    );
    assert_eq!(half_b.0, full_b.0);
    assert_eq!(half_b.1, full_b.1);
}

#[test]
fn p2c_named_sides_land_on_the_right_faction() {
    let c = content();
    // `AiCommanders` stores in faction-slot order (A, B).
    let app = headless::ai_vs_ai(
        content(),
        &MatchSettings::default().with_strategies("rush", "turtle"),
    )
    .unwrap();
    let ids: Vec<&str> = app
        .world()
        .resource::<AiCommanders>()
        .commanders()
        .iter()
        .map(|cm| cm.strategy(&c).id.as_str())
        .collect();
    assert_eq!(ids, vec!["rush", "turtle"]);

    // And the pairing is not symmetric: swapping the names must change play,
    // which is what proves the names reach *different* sides.
    let fwd = run(
        headless::ai_vs_ai(content(), &shape(Some("rush"), Some("turtle"), 42)).unwrap(),
        900,
    );
    let rev = run(
        headless::ai_vs_ai(content(), &shape(Some("turtle"), Some("rush"), 42)).unwrap(),
        900,
    );
    assert_ne!(fwd.1, rev.1, "which side plays which strategy must matter");
}

// ---- P3: fallibility -------------------------------------------------------

#[test]
fn p3_an_unknown_name_on_either_side_is_refused() {
    for (a, b, who, id) in [
        ("nope", "turtle", Faction::A, "nope"),
        ("rush", "nope", Faction::B, "nope"),
    ] {
        let err = headless::ai_vs_ai(content(), &MatchSettings::default().with_strategies(a, b))
            .expect_err("unknown strategy must be refused");
        assert_eq!(err.id, id);
        assert_eq!(err.faction, Some(who));
    }
}

#[test]
fn p3b_a_half_named_matchup_with_a_bad_name_is_refused() {
    for s in [
        shape(Some("bogus"), None, 0),
        shape(None, Some("bogus"), 0),
    ] {
        let err = headless::ai_vs_ai(content(), &s).expect_err("a typo must not build a match");
        assert_eq!(err.id, "bogus");
    }
}

#[test]
fn p3c_the_empty_name_is_a_name_not_a_default() {
    let err = headless::ai_vs_ai(content(), &shape(Some(""), None, 0))
        .expect_err("the empty string names no strategy");
    assert_eq!(err.id, "");
}

// ---- P4: determinism -------------------------------------------------------

#[test]
fn p4_repeat_runs_are_bit_identical() {
    let s = shape(Some("mass_bulwark"), Some("mass_ravager"), 77);
    let a = run(headless::ai_vs_ai(content(), &s).unwrap(), 900);
    let b = run(headless::ai_vs_ai(content(), &s).unwrap(), 900);
    assert_eq!(a.0, b.0);
    assert_eq!(a.1, b.1);
    assert!(!a.1.is_empty());
}

/// Raw `Entity` bits are not comparable across worlds (inserting a
/// `StateHashLog` shifts lazy component registration and therefore the next
/// free entity index — true of the pre-B2 fixture too, see `p9b`). Compare the
/// journal by what it *says*, with entity bits blanked.
fn journal_shape(j: &[(u32, Faction, onus::sim::AiAction)]) -> Vec<String> {
    j.iter()
        .map(|(t, f, a)| {
            let text = format!("{a:?}");
            let blanked: String = text
                .split(char::is_whitespace)
                .map(|w| {
                    if w.trim_end_matches(&[',', ')'][..])
                        .split('v')
                        .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
                        && w.contains('v')
                    {
                        "<entity>".to_string()
                    } else {
                        w.to_string()
                    }
                })
                .collect::<Vec<_>>()
                .join(" ");
            format!("{t} {f:?} {blanked}")
        })
        .collect()
}

#[test]
fn p4b_hashing_opt_in_does_not_change_the_match() {
    let s = MatchSettings::default()
        .with_seed(5)
        .with_strategies("rush", "mass_sentinel");
    let mut plain = headless::ai_vs_ai(content(), &s).unwrap();
    assert!(!plain.world().contains_resource::<StateHashLog>());
    headless::tick(&mut plain, 900);
    let hashed = headless::ai_vs_ai(content(), &s.clone().with_hashing(true)).unwrap();
    let (hashes, journal) = run(hashed, 900);
    assert_eq!(
        journal_shape(&plain.world().resource::<AiJournal>().0),
        journal_shape(&journal),
        "asking for hashes must not change what the commanders decide"
    );
    assert_eq!(hashes.len(), 900);
}

// ---- P5: stepping semantics ------------------------------------------------

#[test]
fn p5_step_advances_exactly_one_tick_and_tick_n_advances_n() {
    let mut app = headless::ai_vs_ai(
        content(),
        &MatchSettings::default().with_seed(3).with_hashing(true),
    )
    .unwrap();
    assert_eq!(app.world().resource::<StateHashLog>().0.len(), 0);
    headless::tick(&mut app, 0);
    assert_eq!(
        app.world().resource::<StateHashLog>().0.len(),
        0,
        "tick(app, 0) must not advance the match"
    );
    for n in 1..=4u32 {
        headless::step(&mut app);
        assert_eq!(app.world().resource::<StateHashLog>().0.len(), n as usize);
        assert!(app
            .world()
            .resource::<AiCommanders>()
            .commanders()
            .iter()
            .all(|c| c.tick() == n));
    }
    headless::tick(&mut app, 17);
    assert_eq!(app.world().resource::<StateHashLog>().0.len(), 21);
    assert!(app
        .world()
        .resource::<AiCommanders>()
        .commanders()
        .iter()
        .all(|c| c.tick() == 21));
}

#[test]
fn p5b_stepping_matches_a_hand_rolled_fixed_clock_advance() {
    // 120 hand-driven ticks and 120 helper ticks must land on the same state.
    let mut by_hand = headless::ai_vs_ai(
        content(),
        &MatchSettings::default().with_seed(8).with_hashing(true),
    )
    .unwrap();
    for _ in 0..120 {
        let dt = by_hand.world().resource::<Time<Fixed>>().timestep();
        by_hand
            .world_mut()
            .resource_mut::<Time<Fixed>>()
            .advance_by(dt);
        by_hand.update();
    }
    let helper = headless::ai_vs_ai(
        content(),
        &MatchSettings::default().with_seed(8).with_hashing(true),
    )
    .unwrap();
    let (hashes, _) = run(helper, 120);
    assert_eq!(by_hand.world().resource::<StateHashLog>().0, hashes);
}

// ---- P6: content() is cwd-independent --------------------------------------

#[test]
fn p6_content_is_cwd_independent_and_agrees_with_load_default() {
    let from_repo_root = Content::load_from_dir(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data"),
    )
    .expect("assets/data");
    let prev = std::env::current_dir().expect("cwd");
    std::env::set_current_dir("/").expect("chdir /");
    let got = headless::content();
    std::env::set_current_dir(&prev).expect("restore cwd");
    let got = got.expect("headless::content() must not depend on the working directory");
    assert_eq!(got.fingerprint().hash(), from_repo_root.fingerprint().hash());
    // ...and it is the same content the driver loads from the repo root.
    let driver = Content::load_default().expect("load_default from the repo root");
    assert_eq!(got.fingerprint().hash(), driver.fingerprint().hash());
}

// ---- P7: the constructor does not half-build on refusal --------------------

#[test]
fn p7_a_refused_match_leaves_the_caller_with_nothing_and_no_panic() {
    let r = std::panic::catch_unwind(|| {
        headless::ai_vs_ai(
            Content::load_from_dir(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data"))
                .unwrap(),
            &MatchSettings::default().with_strategies("nope", "nope"),
        )
        .is_err()
    });
    assert_eq!(r.ok(), Some(true), "a typo must return Err, never panic");
}

// ---- P8: spawn geometry is one knob AC3 can flip ---------------------------

#[test]
fn p8_the_two_bases_are_mirror_images_so_an_orientation_swap_is_well_defined() {
    let mut app = headless::ai_vs_ai(content(), &MatchSettings::default()).unwrap();
    let mut hqs: Vec<(Faction, Vec2)> = app
        .world_mut()
        .query_filtered::<(&Faction, &Position), With<Building>>()
        .iter(app.world())
        .map(|(f, p)| (*f, p.0))
        .collect();
    hqs.sort_by_key(|(f, _)| onus::headless::SIDES.iter().position(|s| s == f).unwrap());
    assert_eq!(hqs.len(), 2);
    assert_eq!(hqs[0].1, -hqs[1].1, "the bases must be mirror images");
    assert_eq!(onus::headless::SIDES, [Faction::A, Faction::B]);
}

// ---- P9: is the hashing divergence sim state, or only raw entity ids? ------

#[test]
fn p9_hashing_does_not_change_the_hashed_state_of_the_match() {
    // The honest test of "opt-in hashing cannot change the match": run both,
    // then hash the *plain* world by hand at the end and compare with the
    // hashed run's last logged hash. `state_hash` keys on `SimId`, not raw
    // entity bits, so this is the state the sim actually reads.
    let s = MatchSettings::default()
        .with_seed(5)
        .with_strategies("rush", "mass_sentinel");
    let mut plain = headless::ai_vs_ai(content(), &s).unwrap();
    headless::tick(&mut plain, 900);
    let hashed = headless::ai_vs_ai(content(), &s.clone().with_hashing(true)).unwrap();
    let (hashes, _) = run(hashed, 900);
    assert_eq!(
        onus::sim::state_hash(plain.world_mut()),
        *hashes.last().unwrap(),
        "asking for hashes changed the simulated state"
    );
}

#[test]
fn p9b_the_lift_agrees_with_the_legacy_fixture_without_hashing_too() {
    // The equivalence pin only ever runs with hashing on. Check the unhashed
    // path as well, by hashing both worlds by hand at the end.
    let mut old = legacy_ai_vs_ai(9, false);
    for _ in 0..900 {
        legacy_step(&mut old);
    }
    let mut new = headless::ai_vs_ai(content(), &MatchSettings::default().with_seed(9)).unwrap();
    headless::tick(&mut new, 900);
    assert_eq!(
        onus::sim::state_hash(old.world_mut()),
        onus::sim::state_hash(new.world_mut())
    );
    assert_eq!(
        journal_shape(&old.world().resource::<AiJournal>().0),
        journal_shape(&new.world().resource::<AiJournal>().0)
    );
}

#[test]
fn p9c_the_plain_hashed_entity_id_offset_is_pre_existing_not_the_lifts() {
    // Documented: the legacy fixture shows the identical raw-entity-id shift
    // between hashing on and off, so it is a property of the sim/engine and
    // not something this lift introduced.
    let mut plain = legacy_ai_vs_ai(5, false);
    let mut hashed = legacy_ai_vs_ai(5, true);
    for _ in 0..900 {
        legacy_step(&mut plain);
        legacy_step(&mut hashed);
    }
    assert_eq!(
        journal_shape(&plain.world().resource::<AiJournal>().0),
        journal_shape(&hashed.world().resource::<AiJournal>().0),
        "the legacy fixture agrees modulo raw entity bits"
    );
    assert_eq!(
        onus::sim::state_hash(plain.world_mut()),
        onus::sim::state_hash(hashed.world_mut())
    );
}

// ---- P10: is the pin's fold actually sensitive to an intermediate tick? ----

#[test]
fn p10_the_pinned_fold_detects_a_single_perturbed_intermediate_tick() {
    // Replica of the fold `tests/b2_headless.rs` pins with. A fold that only
    // samples would let an unpinned tick drift unseen, so check it moves for a
    // change at *every* position, including ones the six pinned ticks miss.
    fn fold(hashes: &[u64]) -> u64 {
        hashes.iter().fold(0xcbf2_9ce4_8422_2325u64, |a, h| {
            (a ^ h).wrapping_mul(0x1000_0000_01b3)
        })
    }
    let app = headless::ai_vs_ai(
        content(),
        &MatchSettings::default().with_seed(4).with_hashing(true),
    )
    .unwrap();
    let (hashes, _) = run(app, 600);
    assert_eq!(hashes.len(), 600);
    let base = fold(&hashes);
    let pinned = [1usize, 10, 60, 120, 300, 600];
    for i in 0..600 {
        let mut perturbed = hashes.clone();
        perturbed[i] ^= 1;
        assert_ne!(
            fold(&perturbed),
            base,
            "a one-bit change at tick {} is invisible to the fold (pinned: {})",
            i + 1,
            pinned.contains(&(i + 1))
        );
    }
    // ...and a swap of two adjacent ticks is caught too.
    let mut swapped = hashes.clone();
    swapped.swap(200, 201);
    assert_ne!(fold(&swapped), base, "the fold must be order-sensitive");
}
