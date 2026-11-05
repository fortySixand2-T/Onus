//! C3 AC5: the scripted AI in the three-resource economy. It builds its
//! script's openings, pays each unit in that unit's own resource, keeps a
//! keeper on its home ley-well and a guard detachment on the middle one (and
//! back home when an enemy reaches its home well). Detachment sizes are RON;
//! Alloy-only traces are untouched. Every number is read from the shipped RON.

use bevy::prelude::*;
use std::path::PathBuf;

use onus::headless::{self, MatchSettings};
use onus::sim::content::Content;
use onus::sim::three::{Bank, EconomyMode, LeyWell, WellState};
use onus::sim::{AiAction, AiJournal, Building, Faction, Health, Position, Stockpiles, UnitDefIdx};

fn shipped() -> Content {
    Content::load_from_dir(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data"))
        .expect("content loads")
}

fn match_of(content: Content, economy: EconomyMode, a: &str, b: &str) -> App {
    let s = MatchSettings::default()
        .with_economy(economy)
        .with_strategies(a, b);
    headless::ai_vs_ai(content, &s).expect("strategies exist")
}

fn journal(app: &App, f: Faction) -> Vec<(u32, AiAction)> {
    app.world().resource::<AiJournal>().for_faction(f)
}

/// Every well: `(entity, position, well)`, by entity.
fn wells(app: &mut App) -> Vec<(Entity, Vec2, LeyWell)> {
    let mut q = app.world_mut().query::<(Entity, &Position, &LeyWell)>();
    let mut v: Vec<_> = q.iter(app.world()).map(|(e, p, w)| (e, p.0, *w)).collect();
    v.sort_by_key(|(e, ..)| e.to_bits());
    v
}

fn hq(app: &mut App, f: Faction) -> Vec2 {
    let c = app.world().resource::<Content>().clone();
    let mut q = app.world_mut().query::<(&Position, &Building, &Faction)>();
    q.iter(app.world())
        .find(|(_, b, g)| **g == f && c.buildings[b.def].victory)
        .map(|(p, ..)| p.0)
        .expect("an HQ")
}

/// `f`'s home well (the one nearest its HQ) and the middle one (the site
/// half-way between the HQs).
fn home_and_middle(app: &mut App, f: Faction) -> ((Entity, Vec2), (Entity, Vec2)) {
    let mine = hq(app, f);
    let theirs = hq(
        app,
        if f == Faction::A {
            Faction::B
        } else {
            Faction::A
        },
    );
    let w = wells(app);
    let home = w
        .iter()
        .min_by(|a, b| a.1.distance(mine).total_cmp(&b.1.distance(mine)))
        .unwrap();
    let mid = (mine + theirs) / 2.0;
    let middle = w
        .iter()
        .min_by(|a, b| a.1.distance(mid).total_cmp(&b.1.distance(mid)))
        .unwrap();
    ((home.0, home.1), (middle.0, middle.1))
}

fn drain(app: &mut App, f: Faction, bank: Bank) {
    let mut s = app.world_mut().resource_mut::<Stockpiles>();
    let have = s.amount(f, bank);
    assert!(s.try_spend_in(f, bank, have));
}

#[test]
fn the_ai_opens_its_gene_vats_and_trains_rippers_only_on_biomass() {
    let c = shipped();
    let vats = c.building_index("gene_vats").unwrap();
    let ripper = c.unit_index("ripper").unwrap();
    let mut app = match_of(c.clone(), EconomyMode::ThreeResource, "mass_ripper", "mvp");
    // Starved of Biomass, it still opens its Vats (Alloy) but never orders a
    // Ripper it could not pay for: its budget is the unit's own bank.
    for _ in 0..1500 {
        drain(&mut app, Faction::A, Bank::Biomass);
        headless::tick(&mut app, 1);
    }
    let j = journal(&app, Faction::A);
    assert!(
        j.iter().any(
            |(_, a)| matches!(a, AiAction::PlaceBarracks { building, .. } if *building == vats)
        ),
        "the script's Gene-Vats opening goes up"
    );
    assert!(
        !j.iter()
            .any(|(_, a)| matches!(a, AiAction::TrainArmy { .. })),
        "no Ripper ordered without the Biomass for it: {j:?}"
    );
    // Alloy is not what a Ripper costs: the side has Alloy to spare.
    assert!(
        app.world().resource::<Stockpiles>().alloy(Faction::A) >= c.units[ripper].mvp_alloy_cost
    );
    // Given the Biomass, it trains and pays in Biomass.
    let price = c.units[ripper].cost.amount;
    let _ = app
        .world_mut()
        .resource_mut::<Stockpiles>()
        .add_in(Faction::A, Bank::Biomass, price);
    let alloy = app.world().resource::<Stockpiles>().alloy(Faction::A);
    let mut trained = false;
    for _ in 0..120 {
        headless::tick(&mut app, 1);
        if journal(&app, Faction::A)
            .iter()
            .any(|(_, a)| *a == AiAction::TrainArmy { unit: ripper })
        {
            trained = true;
            break;
        }
    }
    assert!(trained, "a Ripper is ordered once the Biomass is there");
    let s = app.world().resource::<Stockpiles>();
    assert!(
        s.biomass(Faction::A) < price,
        "the Ripper was paid in Biomass"
    );
    assert!(
        s.alloy(Faction::A) + 10 >= alloy,
        "and not in Alloy (at most a worker's price apart)"
    );
}

#[test]
fn the_ai_keeps_its_ron_keepers_on_its_home_well_and_channels_aether() {
    let c = shipped();
    let t = c.three_resource.clone().unwrap();
    let mut app = match_of(c.clone(), EconomyMode::ThreeResource, "mvp", "mvp");
    headless::tick(&mut app, 2400);
    for f in [Faction::A, Faction::B] {
        let ((home, home_pos), _) = home_and_middle(&mut app, f);
        let keepers: Vec<Entity> = {
            let mut v: Vec<Entity> = journal(&app, f)
                .iter()
                .filter_map(|(_, a)| match a {
                    AiAction::Keep { unit, well } => {
                        assert_eq!(*well, home, "a keeper goes to its own home well");
                        Some(*unit)
                    }
                    _ => None,
                })
                .collect();
            v.sort_by_key(|e| e.to_bits());
            v.dedup();
            v
        };
        assert_eq!(
            keepers.len() as u32,
            t.ai.keepers,
            "{f:?}: the RON's keepers"
        );
        for k in &keepers {
            let p = app.world().get::<Position>(*k).expect("the keeper lives").0;
            assert!(
                p.distance(home_pos) <= t.wells.hold_radius,
                "{f:?}: keeper on its well"
            );
        }
        let w = app.world().get::<LeyWell>(home).unwrap();
        assert_eq!(w.state(), WellState::Held(f), "{f:?} holds its home well");
        assert!(
            app.world().resource::<Stockpiles>().aether(f) > 0,
            "{f:?} channels Aether"
        );
    }
}

#[test]
fn the_ai_sends_its_ron_guards_to_contest_the_middle_well() {
    let c = shipped();
    let t = c.three_resource.clone().unwrap();
    let mut app = match_of(c.clone(), EconomyMode::ThreeResource, "mvp", "mvp");
    let ((_, _), (middle, middle_pos)) = home_and_middle(&mut app, Faction::A);
    // Watch the opening (sampled every 30 ticks): the detachment is ordered
    // to the middle, and one of A's fighters stands on it.
    let (mut ordered, mut there) = (false, false);
    for _ in 0..200 {
        headless::tick(&mut app, 30);
        for (_, a) in journal(&app, Faction::A) {
            if let AiAction::Guard { well, force } = a {
                assert!(force <= t.ai.guards, "never more than the RON's detachment");
                ordered |= well == middle;
            }
        }
        let mut q = app
            .world_mut()
            .query::<(&Position, &UnitDefIdx, &Faction)>();
        there |= q.iter(app.world()).any(|(p, d, f)| {
            *f == Faction::A
                && c.units[d.0].offense > 0
                && p.0.distance(middle_pos) <= t.wells.hold_radius
        });
        if ordered && there {
            break;
        }
    }
    assert!(ordered, "A's guard detachment is sent to the middle well");
    assert!(there, "a guard of A stands on the middle well");
}

#[test]
fn the_guards_go_home_when_an_enemy_reaches_the_home_well() {
    let c = shipped();
    let mut app = match_of(c.clone(), EconomyMode::ThreeResource, "mvp", "mvp");
    let ((home, home_pos), (middle, _)) = home_and_middle(&mut app, Faction::A);
    // Wait for the detachment to head for the middle.
    let mut out = false;
    for _ in 0..240 {
        headless::tick(&mut app, 30);
        if journal(&app, Faction::A)
            .iter()
            .any(|(_, a)| matches!(a, AiAction::Guard { well, .. } if *well == middle))
        {
            out = true;
            break;
        }
    }
    assert!(out, "the guards went out first");
    let before = journal(&app, Faction::A).len();
    // An enemy (a worker: no fight to muddy it) walks onto A's home well.
    let w = c.unit_index("worker").unwrap();
    app.world_mut().spawn((
        Position(home_pos),
        UnitDefIdx(w),
        c.units[w].mvp_kind,
        Faction::B,
        Health::from_def(&c, w),
    ));
    headless::tick(&mut app, 120);
    let later = journal(&app, Faction::A);
    assert!(
        later[before..]
            .iter()
            .any(|(_, a)| matches!(a, AiAction::Guard { well, .. } if *well == home)),
        "the guards are called back home: {:?}",
        &later[before..]
    );
}

#[test]
fn the_detachment_sizes_are_ron() {
    let mut c = shipped();
    {
        let ai = &mut c.three_resource.as_mut().unwrap().ai;
        ai.keepers = 0;
        ai.guards = 0;
    }
    let mut app = match_of(c, EconomyMode::ThreeResource, "mvp", "mvp");
    headless::tick(&mut app, 3600);
    let j = app.world().resource::<AiJournal>();
    assert!(
        !j.0.iter()
            .any(|(_, _, a)| matches!(a, AiAction::Keep { .. } | AiAction::Guard { .. })),
        "no keepers and no guards when the RON asks for none"
    );
}

#[test]
fn an_alloy_only_ai_neither_keeps_nor_guards() {
    let mut app = match_of(shipped(), EconomyMode::Alloy, "mvp", "mvp");
    headless::tick(&mut app, 3600);
    assert!(wells(&mut app).is_empty());
    let j = app.world().resource::<AiJournal>();
    assert!(!j.0.is_empty());
    assert!(!j
        .0
        .iter()
        .any(|(_, _, a)| matches!(a, AiAction::Keep { .. } | AiAction::Guard { .. })));
}

/// F-083: a fallen guard is replaced once per attack wave, not body by body
/// out of the army. Refilling at once starved the wave in a mirror: each new
/// Sentinel walked alone to the middle well and traded 1-for-1 with the
/// enemy's replacement, so neither army ever reached `attack_at_army` and the
/// match ran to the cap with no wave sent.
#[test]
fn a_mirror_still_sends_its_waves_while_guards_fall_at_the_middle_well() {
    let c = shipped();
    let mut app = match_of(
        c,
        EconomyMode::ThreeResource,
        "mass_sentinel",
        "mass_sentinel",
    );
    headless::tick(&mut app, 20_000);
    for f in [Faction::A, Faction::B] {
        let j = journal(&app, f);
        assert!(
            j.iter().any(|(_, a)| matches!(a, AiAction::Guard { .. })),
            "{f:?} still guards the middle well"
        );
        assert!(
            j.iter().any(|(_, a)| matches!(a, AiAction::Attack { .. })),
            "{f:?} sends an attack wave"
        );
    }
}
