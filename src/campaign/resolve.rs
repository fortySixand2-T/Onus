//! The shipped resolver (C4 AC10): a campaign battle played out by the
//! headless battle sim, exactly as a skirmish is — [`headless::battle`] on
//! the settings [`battle_settings`] builds, stepped until the battle layer
//! decides it or the campaign's tick cap runs out.
//!
//! What comes back is read from the battle's own report, by sim id: each
//! plan unit, General and building is found on the field when the battle
//! starts, and its fate is looked up at the end. A unit the battle trained
//! and the battle-only workers are not plan units, so they do not come back.

use bevy::ecs::entity::Entity;
use bevy::ecs::query::{Has, With};

use crate::headless;
use crate::sim::battle::{BattleState, General};
use crate::sim::combat::Health;
use crate::sim::content::Content;
use crate::sim::economy::{Building, UnitDefIdx};
use crate::sim::replay::{self, SimId};
use crate::sim::spatial::Faction;

use super::battle::{battle_settings, BattleOutcome, BattlePlan, PlanSide, Resolver, Side, SideOutcome};
use super::content::CampaignContent;

/// Plays every battle it is handed in the headless battle sim.
#[derive(Debug, Clone, Copy, Default)]
pub struct HeadlessResolver;

impl Resolver for HeadlessResolver {
    fn resolve(&mut self, content: &Content, campaign: &CampaignContent, plan: &BattlePlan) -> BattleOutcome {
        resolve_battle(content, campaign, plan)
    }
}

/// The sim ids of one side's plan things, in plan order.
struct Fielded {
    generals: Vec<SimId>,
    units: Vec<SimId>,
    buildings: Vec<SimId>,
}

/// Play `plan` in the headless battle sim (C4 AC10). Deterministic: the
/// battle's seed and map side are the plan's.
pub fn resolve_battle(content: &Content, campaign: &CampaignContent, plan: &BattlePlan) -> BattleOutcome {
    let settings = battle_settings(content, campaign, plan);
    let mut app = headless::battle(content.clone(), &settings)
        .unwrap_or_else(|e| panic!("a campaign battle the content refuses: {e}"));
    // The ids the sim's first tick would assign, assigned now (identify is
    // idempotent and assigns in spawn order) so the plan can be found again.
    replay::identify(app.world_mut());
    let fielded = [Faction::A, Faction::B].map(|f| {
        let world = app.world_mut();
        // In spawn order, which is ascending entity index. (Not sim-id
        // order: Bevy 0.19's entity bits run downward as indices rise, and
        // `identify` numbers by bits.)
        let mut q = world.query_filtered::<(Entity, &Faction, &SimId, Has<General>), With<UnitDefIdx>>();
        let mut units: Vec<(u32, SimId, bool)> = q
            .iter(world)
            .filter(|(_, side, ..)| **side == f)
            .map(|(e, _, id, g)| (e.index_u32(), *id, g))
            .collect();
        units.sort_unstable_by_key(|u| u.0);
        let mut q = world.query_filtered::<(Entity, &Faction, &SimId), With<Building>>();
        let mut buildings: Vec<(u32, SimId)> = q
            .iter(world)
            .filter(|(_, side, _)| **side == f)
            .map(|(e, _, id)| (e.index_u32(), *id))
            .collect();
        buildings.sort_unstable_by_key(|b| b.0);
        Fielded {
            generals: units.iter().filter(|u| u.2).map(|u| u.1).collect(),
            // The side's roster units in roster order, then its workers.
            units: units.iter().filter(|u| !u.2).map(|u| u.1).collect(),
            buildings: buildings.into_iter().map(|b| b.1).collect(),
        }
    });
    let [fa, fd] = &fielded;
    assert_eq!(fa.generals.len(), plan.attack.generals.len(), "every attacking General on the field");
    assert_eq!(fd.generals.len(), plan.defend.generals.len(), "every defending General on the field");
    assert!(fa.units.len() >= plan.attack.units.len() && fd.units.len() >= plan.defend.units.len());
    assert_eq!(fd.buildings.len(), plan.buildings.len(), "every city building on the field");

    let mut ticks = 0;
    while ticks < settings.tick_cap
        && app.world().resource::<BattleState>().result().is_none()
    {
        headless::step(&mut app);
        ticks += 1;
    }
    let state = app.world().resource::<BattleState>();
    let Some(result) = state.result() else {
        // The tick cap: undecided, and the defender holds. Everyone walks
        // off with the HP he has now.
        return at_the_cap(&mut app, plan, &fielded, ticks);
    };
    let winner = match (result.loss(Faction::A), result.loss(Faction::B)) {
        (None, Some(_)) => Some(Side::Attack),
        (Some(_), None) => Some(Side::Defend),
        _ => None,
    };
    let side = |f: Faction, n_units: usize, ids: &Fielded| {
        let r = result.report(f);
        let hp_of = |id: SimId| r.units.iter().find(|u| u.sim_id == id).map_or(0, |u| u.hp);
        SideOutcome {
            generals: r.generals.iter().map(|g| g.survived).collect(),
            units: ids.units[..n_units].iter().map(|&id| hp_of(id)).collect(),
        }
    };
    let attack = side(Faction::A, plan.attack.units.len(), fa);
    let defend = side(Faction::B, plan.defend.units.len(), fd);
    let rb = result.report(Faction::B);
    let buildings = fd
        .buildings
        .iter()
        .map(|&id| rb.buildings.iter().find(|b| b.sim_id == id).map_or(0, |b| b.hp))
        .collect();
    BattleOutcome {
        winner,
        attack,
        defend,
        buildings,
        ticks,
    }
}

/// An undecided battle: every plan thing's HP read off the field (or, for
/// one that escaped in a retreat, the HP it left with); gone is dead.
fn at_the_cap(app: &mut bevy::app::App, plan: &BattlePlan, fielded: &[Fielded; 2], ticks: u32) -> BattleOutcome {
    let escaped: Vec<(SimId, u32)> = app
        .world()
        .resource::<BattleState>()
        .escaped()
        .iter()
        .map(|e| (e.sim_id, e.hp))
        .collect();
    let world = app.world_mut();
    let mut q = world.query::<(&SimId, Option<&Health>)>();
    // A campaign roster gives every unit and building its HP, so each
    // carries `Health`; a General is spawned with it.
    let live: Vec<(SimId, Option<u32>)> = q.iter(world).map(|(id, h)| (*id, h.map(|h| h.current))).collect();
    let hp = |id: SimId, planned: u32| {
        live.iter()
            .find(|(i, _)| *i == id)
            .map(|&(_, h)| h.unwrap_or(planned))
            .or_else(|| escaped.iter().find(|(i, _)| *i == id).map(|&(_, h)| h))
            .unwrap_or(0)
    };
    let side = |s: &PlanSide, f: &Fielded| SideOutcome {
        generals: f.generals.iter().map(|&id| hp(id, 1) > 0).collect(),
        units: s.units.iter().zip(&f.units).map(|(u, &id)| hp(id, u.hp)).collect(),
    };
    BattleOutcome {
        winner: None,
        attack: side(&plan.attack, &fielded[0]),
        defend: side(&plan.defend, &fielded[1]),
        buildings: plan
            .buildings
            .iter()
            .zip(&fielded[1].buildings)
            .map(|(b, &id)| hp(id, b.hp))
            .collect(),
        ticks,
    }
}
