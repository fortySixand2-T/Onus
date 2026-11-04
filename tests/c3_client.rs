//! C3 AC6 — **the client: pick the economy, see all three banks, see the
//! ley-wells and who holds them.** Kept thin: the pick only hands its choice
//! to the sim's own setup (`three::install`, `nation::install`,
//! `three::spawn_wells`, the functions a headless match uses), and what is
//! shown is read off sim state.
//!
//! Driven headlessly under `MinimalPlugins` through the shipped driver, as
//! `c2_client` is. What this cannot judge (a human on the Mac): that the
//! wells and their hold rings read well on screen, that the holder colours
//! tell the sides apart, and that the panel's three-bank line fits.

use std::path::PathBuf;

use bevy::prelude::*;

use onus::client::{
    well_color, NationPick, NationPicker, OptionsPanel, PickPhase, Selected, ECONOMY_KEY,
    START_KEY, WELL_COLOR, WELL_CONTESTED_COLOR,
};
use onus::sim::combat::Health;
use onus::sim::content::Content;
use onus::sim::economy::{Building, ProductionQueue, Stockpiles, UnitDefIdx};
use onus::sim::nation::NationRules;
use onus::sim::spatial::Faction;
use onus::sim::three::{EconomyMode, LeyWell, ThreeEconomy, WellState};
use onus::sim::{CommandQueue, MatchState, Position, RateReport};

fn content() -> Content {
    Content::load_from_dir(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data"))
        .expect("assets/data/*.ron")
}

fn app_with(c: Content) -> App {
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
    onus::add_nation_pick(&mut app);
    app.add_systems(Update, (onus::ui::update_options_panel, onus::ui::show_wells));
    app.update();
    app
}

fn app() -> App {
    app_with(content())
}

fn press(app: &mut App, key: KeyCode) {
    app.world_mut().resource_mut::<ButtonInput<KeyCode>>().press(key);
    app.update();
    let mut keys = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
    keys.release(key);
    keys.clear();
}

fn frames(app: &mut App, n: u32) {
    for _ in 0..n {
        app.update();
    }
}

fn picker_text(app: &mut App) -> String {
    let mut q = app.world_mut().query_filtered::<&Text, With<NationPicker>>();
    q.single(app.world()).expect("one picker").0.clone()
}

fn panel(app: &mut App) -> String {
    let mut q = app.world_mut().query_filtered::<&Text, With<OptionsPanel>>();
    q.single(app.world()).expect("one panel").0.clone()
}

/// Every well, by entity: `(entity, position, well, sprite colour)`.
fn wells(app: &mut App) -> Vec<(Entity, Vec2, LeyWell, Option<Color>)> {
    let mut q = app
        .world_mut()
        .query::<(Entity, &Position, &LeyWell, Option<&Sprite>)>();
    let mut v: Vec<_> = q
        .iter(app.world())
        .map(|(e, p, w, s)| (e, p.0, *w, s.map(|s| s.color)))
        .collect();
    v.sort_by_key(|(e, ..)| e.to_bits());
    v
}

#[test]
fn the_pick_defaults_to_alloy_only_and_the_economy_key_toggles_it() {
    let mut app = app();
    assert_eq!(app.world().resource::<NationPick>().economy, EconomyMode::Alloy);
    let text = picker_text(&mut app);
    assert!(text.contains(EconomyMode::Alloy.label()), "{text}");
    press(&mut app, ECONOMY_KEY);
    assert_eq!(
        app.world().resource::<NationPick>().economy,
        EconomyMode::ThreeResource
    );
    frames(&mut app, 1);
    let text = picker_text(&mut app);
    assert!(text.contains(EconomyMode::ThreeResource.label()), "{text}");
    press(&mut app, ECONOMY_KEY);
    assert_eq!(app.world().resource::<NationPick>().economy, EconomyMode::Alloy);
}

#[test]
fn content_without_the_block_offers_only_alloy() {
    let mut c = content();
    c.three_resource = None;
    let mut app = app_with(c);
    press(&mut app, ECONOMY_KEY);
    assert_eq!(app.world().resource::<NationPick>().economy, EconomyMode::Alloy);
    assert!(!picker_text(&mut app).contains(EconomyMode::ThreeResource.label()));
}

#[test]
fn an_alloy_only_start_is_todays_match() {
    let mut app = app();
    press(&mut app, START_KEY);
    frames(&mut app, 3);
    assert!(app.world().resource::<MatchState>().tick() > 0);
    assert!(!app.world().contains_resource::<ThreeEconomy>());
    assert!(wells(&mut app).is_empty(), "no wells in an Alloy-only match");
    // The panel's bank line is today's.
    let hq = {
        let mut q = app.world_mut().query::<(Entity, &Faction, &Building)>();
        q.iter(app.world()).find(|(_, f, _)| **f == Faction::A).unwrap().0
    };
    app.world_mut().entity_mut(hq).insert(Selected);
    frames(&mut app, 1);
    let text = panel(&mut app);
    assert!(text.starts_with("Alloy: "), "{text}");
    assert!(!text.contains("Biomass") && !text.contains("Aether"), "{text}");
}

#[test]
fn a_three_resource_start_installs_the_economy_the_leans_and_the_wells() {
    let c = content();
    let t = c.three_resource.clone().unwrap();
    let mut app = app();
    press(&mut app, ECONOMY_KEY);
    press(&mut app, START_KEY);
    assert_eq!(app.world().resource::<NationPick>().phase, PickPhase::Playing);
    assert!(app.world().contains_resource::<ThreeEconomy>());
    let s = app.world().resource::<Stockpiles>();
    for f in [Faction::A, Faction::B] {
        assert_eq!(s.biomass(f), t.starting_biomass);
        assert_eq!(s.aether(f), t.starting_aether);
    }
    // The nations were installed for the three-resource economy.
    let rules = app.world().resource::<NationRules>().clone();
    let names = [Faction::A, Faction::B].map(|f| {
        rules
            .nation_of(f)
            .map(|i| c.nations[i].id.clone())
    });
    let want = NationRules::resolve_in(&c, &names, EconomyMode::ThreeResource)
        .unwrap()
        .unwrap();
    for f in [Faction::A, Faction::B] {
        assert_eq!(
            rules.side(f).unwrap().building_pools,
            want.side(f).unwrap().building_pools
        );
    }
    // The wells, framed by the two HQs, and the match plays.
    let w = wells(&mut app);
    assert_eq!(w.len(), t.wells.sites.len());
    let hqs: Vec<Vec2> = {
        let mut q = app.world_mut().query::<(&Position, &Building, &Faction)>();
        [Faction::A, Faction::B]
            .iter()
            .map(|f| q.iter(app.world()).find(|(_, b, g)| **g == *f && c.buildings[b.def].victory).unwrap().0 .0)
            .collect()
    };
    let mut sites: Vec<usize> = w.iter().map(|(_, _, well, _)| well.site()).collect();
    sites.sort_unstable();
    assert_eq!(sites, (0..t.wells.sites.len()).collect::<Vec<_>>(), "one well per site");
    for (_, p, well, _) in &w {
        assert_eq!(*p, t.wells.sites[well.site()].at(hqs[0], hqs[1]));
    }
    frames(&mut app, 3);
    assert!(app.world().resource::<MatchState>().tick() > 0);
}

#[test]
fn the_panel_shows_all_three_banks_and_prices_each_unit_in_its_own() {
    let c = content();
    let mut app = app();
    press(&mut app, ECONOMY_KEY);
    press(&mut app, START_KEY);
    let vats = c.building_index("gene_vats").unwrap();
    app.world_mut().spawn((
        Position(Vec2::new(-300.0, -200.0)),
        Building { def: vats },
        Faction::A,
        ProductionQueue::default(),
        Health::from_building_def(&c, vats),
        Selected,
    ));
    frames(&mut app, 1);
    let text = panel(&mut app);
    let s = app.world().resource::<Stockpiles>();
    let banks = format!(
        "Alloy: {}   Biomass: {}   Aether: {}",
        s.alloy(Faction::A),
        s.biomass(Faction::A),
        s.aether(Faction::A)
    );
    assert!(text.starts_with(&banks), "{text}\nwant: {banks}");
    let ripper = c.unit_index("ripper").unwrap();
    let price = format!("({} Biomass)", c.units[ripper].cost.amount);
    assert!(text.contains(&price), "{text}\nwant: {price}");
}

#[test]
fn each_well_is_drawn_in_its_holders_colour() {
    let c = content();
    let mut app = app();
    press(&mut app, ECONOMY_KEY);
    press(&mut app, START_KEY);
    frames(&mut app, 2);
    let rules = app.world().resource::<NationRules>().clone();
    // Every well has a sprite, coloured for its state.
    for (_, _, well, colour) in wells(&mut app) {
        assert_eq!(colour, Some(well_color(&c, Some(&rules), well.state())));
    }
    // The palette: empty, each side, contested — four distinct colours.
    let palette = [
        well_color(&c, Some(&rules), WellState::Empty),
        well_color(&c, Some(&rules), WellState::Held(Faction::A)),
        well_color(&c, Some(&rules), WellState::Held(Faction::B)),
        well_color(&c, Some(&rules), WellState::Contested),
    ];
    assert_eq!(palette[0], WELL_COLOR);
    assert_eq!(palette[3], WELL_CONTESTED_COLOR);
    for i in 0..4 {
        for j in i + 1..4 {
            assert_ne!(palette[i], palette[j], "{i} vs {j}");
        }
    }
    // No nations: the sides still differ.
    assert_ne!(
        well_color(&c, None, WellState::Held(Faction::A)),
        well_color(&c, None, WellState::Held(Faction::B))
    );
    // A unit of A walks onto the furthest-from-everyone well: it turns A's.
    let (e, at, ..) = wells(&mut app)
        .into_iter()
        .find(|(_, _, w, _)| w.state() == WellState::Empty)
        .expect("an empty well");
    let worker = c.unit_index("worker").unwrap();
    let spawn = |app: &mut App, f: Faction| {
        app.world_mut().spawn((
            Position(at),
            UnitDefIdx(worker),
            c.units[worker].mvp_kind,
            f,
            Health::from_def(&c, worker),
        ));
    };
    spawn(&mut app, Faction::A);
    frames(&mut app, 2);
    let colour = |app: &mut App| wells(app).into_iter().find(|w| w.0 == e).unwrap().3;
    assert_eq!(colour(&mut app), Some(palette[1]));
    spawn(&mut app, Faction::B);
    frames(&mut app, 2);
    assert_eq!(colour(&mut app), Some(palette[3]), "both sides: contested");
}
