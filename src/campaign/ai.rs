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
//!
//! A nation with fewer than `ai.max_armies` armies that can pay
//! `raise_general_cost` raises one General at its capital (C4 AC5), before
//! it marches.

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
    let armies = c.armies().iter().filter(|a| a.nation == n).count();
    let capital = c.campaign().capital[n];
    if armies < c.campaign().ai.max_armies as usize
        && c.nations()[n].treasury >= c.campaign().rules.raise_general_cost
        && c.cities()[capital].owner == n
    {
        out.push(Decision::Raise {
            city: c.campaign().cities[capital].id.clone(),
        });
    }
    let min = c.campaign().ai.attack_min_units as usize;
    let ready: Vec<(u32, usize)> = c
        .armies()
        .iter()
        .filter(|a| a.nation == n && a.units.len() >= min && a.general.present())
        .filter_map(|a| a.at().map(|at| (a.id, at)))
        .filter(|&(_, at)| {
            let owner = c.cities()[at].owner;
            owner == n || c.allied(n, owner)
        })
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
    let hostile = |c: &Campaign, cap: usize| {
        let owner = c.cities()[cap].owner;
        owner != n && !c.allied(n, owner) && dist[cap] != u32::MAX
    };
    // C5: a steered war aims at its target's capital, while it can be reached.
    let war = c
        .war_target(n)
        .map(|w| c.campaign().capital[w.target])
        .filter(|&cap| hostile(c, cap));
    let target = match war {
        Some(cap) => cap,
        None => c
            .campaign()
            .capital
            .iter()
            .enumerate()
            .filter(|&(m, _)| m != n && c.nations()[m].alive && !c.allied(n, m))
            .map(|(_, &cap)| cap)
            .filter(|&cap| hostile(c, cap))
            .min_by_key(|&cap| (dist[cap], cap))?,
    };
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

/// Nation `n`'s diplomatic action this round, if any (C5 AC6). At most one;
/// by fixed rules in a fixed order, from its disposition and the map:
///
/// 1. **betray** its ally, once that ally is its last living rival;
/// 2. **ally** with the Tied nation it is most willing towards, when its
///    willingness reaches `ally_threshold` and the other would accept;
/// 3. **ask** its AI ally, when the ally is not already at war, to attack
///    the nation whose capital is nearest its own (by road; ties to the lower
///    index) among those allied with neither;
/// 4. **incite** the pair of nations Tied with it holding the most cities
///    (ties to the lowest pair), neither already steered, not allies;
/// 5. **court** the nation it is most willing towards (ties to the lower
///    index), when its willingness reaches `court_threshold` and the other
///    would accept.
///
/// It reads the campaign and nothing else: it cannot draw from the rng. It
/// never makes an offer the other would refuse, and so never one to the
/// player, who answers nothing in C5. It acts only when it can pay.
pub fn decide_diplomacy(c: &Campaign, n: usize) -> Option<Decision> {
    use super::diplomacy::{Offer, Standing};
    let dc = c.campaign().diplomacy()?;
    if dc.takes_no_initiative(n) || !c.nations()[n].alive || Some(n) == c.player() {
        return None;
    }
    let r = &dc.rules;
    let treasury = c.nations()[n].treasury;
    let id = |m: usize| c.content().nations[m].id.clone();
    let rivals: Vec<usize> = c.alive().into_iter().filter(|&m| m != n).collect();
    let ally = c.ally_of(n);
    let willing = |m: usize| c.willingness(n, m).unwrap_or(i32::MIN);
    // The most willing of `ms` (ties to the lower index).
    let best = |ms: Vec<usize>| ms.into_iter().max_by_key(|&m| (willing(m), std::cmp::Reverse(m)));

    // 1. Betray.
    if let Some(a) = ally {
        if rivals == [a] {
            return Some(Decision::Betray { nation: id(a) });
        }
    }
    // 2. Ally.
    if ally.is_none() && treasury >= r.ally_cost {
        let ms = rivals
            .iter()
            .copied()
            .filter(|&m| c.standing(n, m) == Some(Standing::Tied))
            .filter(|&m| willing(m) >= r.ally_threshold && c.accepts(m, n, Offer::Ally))
            .collect();
        if let Some(m) = best(ms) {
            return Some(Decision::Ally { nation: id(m) });
        }
    }
    // 3. Ask.
    if let Some(a) = ally.filter(|&a| Some(a) != c.player() && c.war_target(a).is_none()) {
        let dist = c.distances(c.campaign().capital[n]);
        let target = rivals
            .iter()
            .copied()
            .filter(|&t| t != a && !c.allied(t, a) && !c.allied(t, n))
            .map(|t| (dist[c.campaign().capital[t]], t))
            .filter(|&(d, _)| d != u32::MAX)
            .min();
        if let Some((_, t)) = target {
            return Some(Decision::Ask { target: id(t) });
        }
    }
    // 4. Incite.
    if c.tied_with(n) >= r.destabilize_requires_tied && treasury >= r.incite_cost {
        let tied: Vec<usize> = rivals
            .iter()
            .copied()
            .filter(|&m| c.standing(n, m) == Some(Standing::Tied) && c.war_target(m).is_none())
            .collect();
        let mut pick: Option<(usize, usize, usize)> = None;
        for (i, &x) in tied.iter().enumerate() {
            for &y in &tied[i + 1..] {
                if c.allied(x, y) {
                    continue;
                }
                let size = c.cities_of(x).len() + c.cities_of(y).len();
                if pick.is_none_or(|(s, _, _)| size > s) {
                    pick = Some((size, x, y));
                }
            }
        }
        if let Some((_, x, y)) = pick {
            return Some(Decision::Incite { a: id(x), b: id(y) });
        }
    }
    // 5. Court.
    if treasury >= r.court_cost {
        let ms = rivals
            .iter()
            .copied()
            .filter(|&m| c.standing(n, m).is_some_and(|s| s < Standing::Tied))
            .filter(|&m| willing(m) >= r.court_threshold && c.accepts(m, n, Offer::Court))
            .collect();
        if let Some(m) = best(ms) {
            return Some(Decision::Court { nation: id(m) });
        }
    }
    None
}
