//! The campaign AI (C4): what a nation nobody plays decides each round.
//! Deterministic: it reads the campaign state and draws its tie-breaks from
//! the campaign RNG, in army-id order.
//!
//! Each army standing at a city it holds, with at least
//! `ai.attack_min_units` units, takes the first road toward the nearest
//! hostile **capital** (by road cost; equal routes broken by the RNG). A
//! hostile city on the way is assaulted, since the next hop is a march on it.
//! Aiming at capitals ends wars: taking one absorbs a nation, where trading
//! outlying cities can go on forever (F-089).
//!
//! An army standing at a hostile city (waiting its turn to assault) holds.

use super::{Campaign, Decision};

/// The units holding `city` for its owner: every owner army standing there
/// (its units and its General), and the city's garrison.
pub fn defence(c: &Campaign, city: usize) -> u32 {
    let owner = c.cities()[city].owner;
    c.armies()
        .iter()
        .filter(|a| a.nation == owner && a.at() == Some(city))
        .map(|a| a.units.len() as u32 + 1)
        .sum::<u32>()
        + c.cities()[city].garrison.len() as u32
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
        if let Some(to) = toward_capital(c, n, at) {
            out.push(Decision::March {
                army: id,
                to: c.campaign().cities[to].id.clone(),
            });
        }
    }
    out
}

/// The first city on a cheapest road route from `at` to the nearest hostile
/// capital (ties to the lower city index), if one can be reached. Of several
/// first hops on equally cheap routes, the RNG picks one.
fn toward_capital(c: &mut Campaign, n: usize, at: usize) -> Option<usize> {
    let dist = c.distances(at);
    let target = c
        .campaign()
        .capital
        .iter()
        .enumerate()
        .filter(|&(m, _)| m != n && c.nations()[m].alive)
        .map(|(_, &cap)| cap)
        .filter(|&cap| c.cities()[cap].owner != n && dist[cap] != u32::MAX)
        .min_by_key(|&cap| (dist[cap], cap))?;
    // Walk back from the target: the neighbours of `at` on a cheapest route.
    let back = c.distances(target);
    let mut hops: Vec<usize> = c
        .campaign()
        .roads_of(at)
        .map(|(_, r)| (r.other(at), r.cost))
        .filter(|&(o, cost)| cost.saturating_add(back[o]) == dist[target])
        .map(|(o, _)| o)
        .collect();
    hops.sort_unstable();
    hops.dedup();
    if hops.len() <= 1 {
        return hops.first().copied();
    }
    let k = (c.rng().next_u64() % hops.len() as u64) as usize;
    Some(hops[k])
}
