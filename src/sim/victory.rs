//! The win condition and the end of the match (M4c). **Render-free** — ECS +
//! [`Content`] only.
//!
//! The rule is content, not code: a building whose definition says
//! `victory: true` (the HQ) is the thing whose loss loses the match, and
//! `Content::validate` insists there is exactly one such building.
//!
//! ## The end is sim state
//! [`MatchState`] lives in the sim, so "who won, and on which tick" is part of
//! the simulation's own truth — the driver reads it to draw a banner, and a
//! headless AI-vs-AI run reads the same thing. It is:
//!
//! - **exactly once**: the outcome is written only while it is `None`, and the
//!   whole rest of the sim chain is gated on `run_if(match_running)`, so once a
//!   winner is recorded nothing further runs that could change it;
//! - **order-independent**: the check counts standing victory buildings per
//!   faction into a fixed-size array and decides from the two counts, so no
//!   query, archetype or hash order can reach the result. Both sides losing
//!   their HQ on the same tick is a draw, not a race;
//! - **tick-counted**: `MatchState::tick` is the sim's own tick number, never a
//!   clock reading.
//!
//! A match only becomes decidable once both sides actually have an HQ
//! ([`MatchState::engaged`]). Without that rule, every fixture that spawns one
//! side's base would "win" on its first tick and freeze the sim.
//!
//! **That latch is read from the state at the *start* of a tick**
//! ([`match_watch`], the first system in the chain), never from what is left
//! standing at the end of one. Whether a loss is decidable is a property of the
//! configuration the match started from, not of which tick the loss happens to
//! land on: an HQ destroyed on the very first tick has to end the match exactly
//! like one destroyed on the thousandth. Latching after the fact made the first
//! tick a hole you could hide a whole match in.

use bevy::ecs::prelude::*;

use crate::sim::content::Content;
use crate::sim::economy::Building;
use crate::sim::spatial::Faction;

fn faction_slot(f: Faction) -> usize {
    match f {
        Faction::A => 0,
        Faction::B => 1,
    }
}

/// How the match ended: the winner (`None` = draw — both HQs fell on the same
/// tick) and the tick it was decided on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MatchOutcome {
    pub winner: Option<Faction>,
    pub tick: u32,
}

/// The match: how many ticks it has run, whether it is contested yet, and its
/// outcome once decided. Sim-owned (the driver reads it; it never writes it).
#[derive(Resource, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MatchState {
    tick: u32,
    engaged: bool,
    outcome: Option<MatchOutcome>,
}

impl MatchState {
    /// Sim ticks elapsed. Stops advancing once the match is over — the recorded
    /// outcome is the last thing that happens.
    pub fn tick(&self) -> u32 {
        self.tick
    }

    /// Have both sides been seen with an HQ at the same time, at the start of
    /// some tick? Until then there is nothing to win.
    pub fn engaged(&self) -> bool {
        self.engaged
    }

    pub fn outcome(&self) -> Option<MatchOutcome> {
        self.outcome
    }

    pub fn is_over(&self) -> bool {
        self.outcome.is_some()
    }
}

/// Run condition for the sim chain: everything except this module's own check
/// stops the moment the match is decided, so nothing keeps playing after the
/// result is recorded.
pub fn match_running(state: Res<MatchState>) -> bool {
    !state.is_over()
}

/// Observe whether the match is contested, **before** the tick is played. Runs
/// first in the chain, so the configuration a tick starts from is what decides
/// whether that tick's losses count — see the module docs.
pub fn match_watch(
    content: Res<Content>,
    mut state: ResMut<MatchState>,
    buildings: Query<(&Building, &Faction)>,
) {
    if state.engaged || state.is_over() {
        return;
    }
    let standing = count_victory_buildings(&content, &buildings);
    state.engaged = standing[0] > 0 && standing[1] > 0;
}

/// Standing victory buildings per faction. A fixed-size array summed over the
/// query, so the count — and every decision taken from it — is independent of
/// iteration order.
fn count_victory_buildings(
    content: &Content,
    buildings: &Query<(&Building, &Faction)>,
) -> [u32; 2] {
    let mut standing = [0u32; 2];
    for (b, faction) in buildings {
        if content.buildings.get(b.def).is_some_and(|d| d.victory) {
            standing[faction_slot(*faction)] += 1;
        }
    }
    standing
}

/// Decide the match: whoever still has a victory building when the other side
/// has none has won. Runs **last** in the chain, on the state the tick actually
/// ended in.
pub fn match_end(
    content: Res<Content>,
    mut state: ResMut<MatchState>,
    buildings: Query<(&Building, &Faction)>,
) {
    // Exactly once: after this, the outcome is frozen and the chain is off.
    if state.is_over() {
        return;
    }

    // Order-independent by construction: two counters, summed.
    let standing = count_victory_buildings(&content, &buildings);

    let tick = state.tick;
    state.tick = state.tick.saturating_add(1);

    // Contested only once both sides have been on the board — observed by
    // `match_watch` at the *start* of this tick, so a first-tick loss decides
    // exactly like any other.
    if !state.engaged {
        return;
    }

    let outcome = match (standing[0], standing[1]) {
        (0, 0) => Some(MatchOutcome {
            winner: None,
            tick,
        }),
        (0, _) => Some(MatchOutcome {
            winner: Some(Faction::B),
            tick,
        }),
        (_, 0) => Some(MatchOutcome {
            winner: Some(Faction::A),
            tick,
        }),
        _ => None,
    };
    if outcome.is_some() {
        state.outcome = outcome;
    }
}
