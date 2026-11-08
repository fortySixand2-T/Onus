//! The campaign AI (C4): what a nation nobody plays decides each round.
//! Deterministic: it reads the campaign state and draws its tie-breaks from
//! the campaign RNG, in army-id order.
//!
//! Each army standing at a city it holds, with at least
//! `ai.attack_min_units` units:
//! - marches on the weakest adjacent hostile city (fewest defending units,
//!   ties broken by the RNG);
//! - with no hostile neighbour, takes the first road toward the nearest
//!   hostile city.
//!
//! An army standing at a hostile city (waiting its turn to assault) holds.

use super::{Campaign, Decision};

/// The units holding `city` for its owner: every owner army standing there.
pub fn defence(c: &Campaign, city: usize) -> u32 {
    let owner = c.cities()[city].owner;
    c.armies()
        .iter()
        .filter(|a| a.nation == owner && a.at() == Some(city))
        .map(|a| a.units.len() as u32 + 1)
        .sum()
}

/// Nation `n`'s decisions for this round.
pub fn decide(c: &mut Campaign, n: usize) -> Vec<Decision> {
    let mut out = Vec::new();
    let min = c.campaign().ai.attack_min_units as usize;
    let ready: Vec<(u32, usize)> = c
        .armies()
        .iter()
        .filter(|a| a.nation == n && a.units.len() >= min)
        .filter_map(|a| a.at().map(|at| (a.id, at)))
        .filter(|&(_, at)| c.cities()[at].owner == n)
        .collect();
    for (id, at) in ready {
        let neighbours: Vec<usize> = c.campaign().roads_of(at).map(|(_, r)| r.other(at)).collect();
        let hostile: Vec<usize> = neighbours
            .iter()
            .copied()
            .filter(|&o| c.cities()[o].owner != n)
            .collect();
        let to = if hostile.is_empty() {
            toward_hostile(c, n, at)
        } else {
            let weakest = hostile.iter().map(|&o| defence(c, o)).min().expect("not empty");
            let mut tied: Vec<usize> = hostile
                .into_iter()
                .filter(|&o| defence(c, o) == weakest)
                .collect();
            tied.sort_unstable();
            let k = (c.rng().next_u64() % tied.len() as u64) as usize;
            Some(tied[k])
        };
        if let Some(to) = to {
            out.push(Decision::March {
                army: id,
                to: c.campaign().cities[to].id.clone(),
            });
        }
    }
    out
}

/// The first city on the cheapest road route from `at` to the nearest city
/// `n` does not hold (ties to the lower index), if there is one.
fn toward_hostile(c: &Campaign, n: usize, at: usize) -> Option<usize> {
    let dist = c.distances(at);
    let target = (0..c.cities().len())
        .filter(|&o| c.cities()[o].owner != n && dist[o] != u32::MAX)
        .min_by_key(|&o| (dist[o], o))?;
    // Walk back from the target: the neighbour of `at` on a cheapest route.
    let back = c.distances(target);
    c.campaign()
        .roads_of(at)
        .map(|(_, r)| (r.other(at), r.cost))
        .filter(|&(o, cost)| cost.saturating_add(back[o]) == dist[target])
        .map(|(o, _)| o)
        .min()
}
