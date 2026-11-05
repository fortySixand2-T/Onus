//! B4 turtle, step 0: **measure, no behaviour change** (BALANCE_PLAN "Plan (user
//! go-ahead 2026-10-09)", F-047).
//!
//! Why does `turtle` win ~84% under g1? This file plays turtle against each of
//! the other strategies and *watches* the defender's army at the moments that
//! matter. It is read-only: it builds the standard headless match
//! ([`onus::headless::ai_vs_ai`]), steps it, and after every tick reads the
//! world (unit positions, the AI journal). It writes nothing into the sim, so
//! every match it plays is the same match the balance batch plays.
//!
//! ## What it records, per match (turtle = T, the other side = defender D)
//! - **T's first wave**: the tick of T's first `AiAction::Attack` and its size
//!   (the journal's `force`). Its members are T's armed units whose
//!   `MoveTarget` is the order's `dest` right after that tick, or, for a wave
//!   sent as an attack-move (B4 step 1, `wave_order: attack_move`), whose
//!   `AttackMoveDest` is (combat may already have turned the `MoveTarget` into
//!   a chase on the commit tick). Under `wave_order: move` no unit carries an
//!   `AttackMoveDest`, so the reading is the step-0 one.
//! - **Arrival**: the first tick any member of that wave is within
//!   `combat.engage_range` (read from content) of D's HQ.
//! - At commit and at arrival, D's **armed** units (`offense > 0`):
//!   - *home*: alive and within [`home_radius`] of D's own HQ;
//!   - *field*: alive and outside it;
//!   - *dead*: died so far.
//! - **The trade**: armed units lost by each side from T's first commit until
//!   D's HQ falls, or the first wave is spent (every member dead), whichever is
//!   first; and how many of D's losses died away from home.
//! - **D's earlier waves**: the distinct armed units D sent (Attack orders)
//!   before T's first commit, how many of them were dead by that commit, and
//!   how many died near T's HQ (within `home_radius` of it).
//! - Winner and end tick.
//!
//! ## The home radius
//! `max over every shipped strategy of every barracks opening's offset +
//! combat.engage_range`. The farthest a scripted barracks stands from its HQ,
//! plus the reach in which an idle unit there picks a fight. A unit inside it
//! is either at its production line or close enough to defend the HQ on its
//! own; outside it, it is on the road. Both terms are content. On g1 it is
//! 225 + 220 = 445 world units, against bases 1 500 apart.
//!
//! ## Assertions
//! Structural facts only, never balance numbers: the accounting identity
//! (home + field + dead = every armed unit D has fielded), that a wave's
//! members are the journal's force, that arrival follows commit, that the
//! trade window is inside the match. The table is printed, not asserted.
//!
//! The full table (2 seeds x 9 opponents x 2 seats, ~36 full matches) is
//! `#[ignore]`d — it runs past a minute in release. One full match is kept
//! un-ignored as the fast structural check.

use std::collections::BTreeMap;

use onus::batch::seed_at;
use onus::headless::{self, MatchSettings, Orientation};
use onus::sim::combat::{damage_per_hit, damage_per_hit_to_building};
use onus::sim::content::Content;
use onus::sim::economy::{Building, UnitDefIdx};
use onus::sim::spatial::Faction;
use onus::sim::{
    AiAction, AiJournal, AttackCooldown, AttackMoveDest, Engaging, Health, MatchState, MoveTarget,
    Position, Target,
};

use bevy::math::Vec2;
use bevy::prelude::Entity;

const TURTLE: &str = "turtle";

/// The home radius: the farthest scripted barracks offset of any shipped
/// strategy, plus `engage_range`. See the module docs.
fn home_radius(c: &Content) -> f32 {
    let far = c
        .strategies
        .iter()
        .flat_map(|s| s.barracks.iter().map(|o| o.offset))
        .fold(0.0f32, f32::max);
    far + c.combat.engage_range
}

fn armed(c: &Content, def: usize) -> bool {
    c.units.get(def).is_some_and(|u| u.offense > 0)
}

/// One unit, as the observer has seen it. Keyed by entity bits in a BTreeMap,
/// so nothing here depends on a hash order.
#[derive(Clone, Copy, Debug)]
struct Track {
    faction: Faction,
    armed: bool,
    /// The unit's full HP pool, from content.
    max_hp: u32,
    /// Index into `Content::units`.
    def: usize,
    /// The `Target` it held last tick (a unit whose target died on the tick
    /// it was hit holds none; the previous one names it).
    last_target: Option<u64>,
    pos: Vec2,
    /// Tick it was last seen alive; `died` is set the first tick it is gone.
    died: Option<u32>,
}

/// D's armed units at one moment.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Census {
    home: u32,
    field: u32,
    dead: u32,
}

/// Everything one match yields.
#[derive(Clone, Debug)]
struct MatchDiag {
    opponent: String,
    turtle_seat: Faction,
    seed_k: u32,
    winner: Option<Faction>,
    end_tick: u32,
    decided: bool,
    /// T's first wave, if it ever sent one.
    wave: Option<WaveDiag>,
    /// T's armed losses before its first commit (or the end, if it never commits).
    turtle_lost_before_commit: u32,
    /// D's Attack orders before T's first commit.
    d_attacks_before: u32,
    /// Distinct armed units D sent at T before T's first commit.
    d_sent_before: u32,
    /// Of those, dead by T's first commit.
    d_sent_dead_before: u32,
    /// Of those, died within `home_radius` of T's HQ (before T's commit).
    d_sent_died_near_turtle: u32,
    /// D's first wave at T's base, if D attacked before T's first commit.
    d_wave: Option<DWaveDiag>,
}

/// D's attacks on T before T's first commit, seen from T's base.
#[derive(Clone, Copy, Debug)]
struct DWaveDiag {
    /// D's first Attack: tick and size.
    commit: u32,
    size: u32,
    /// First tick any unit D sent is within `engage_range` of T's HQ: the
    /// tick, D's sent units alive then, and T's armed units alive then (all of
    /// them are home: T sends nothing before its commit).
    arrival: Option<(u32, u32, u32)>,
    /// From D's first commit to T's first commit: T's armed losses, and the
    /// HP T's buildings lost (hits D's sent units put into buildings rather
    /// than into T's army).
    turtle_lost: u32,
    turtle_building_hp_lost: u32,
    /// The HP pool of one of T's armed units, summed over T's armed units
    /// alive at D's arrival: the army D's wave walked into, in HP.
    turtle_army_hp_at_arrival: u32,
    /// The same for D's sent units alive at that arrival.
    d_army_hp_at_arrival: u32,
    /// The fight, from D's first commit to T's first commit:
    fight: Fight,
}

/// Hits and contact during D's attack on T's base (D's first commit to T's
/// first commit). A hit is a tick on which the unit's `AttackCooldown` was
/// reset to its `mvp_attack_ticks`; its damage is `damage_per_hit*` for the
/// target it held.
#[derive(Clone, Copy, Debug, Default)]
struct Fight {
    /// Unit-ticks of D's sent units with a T armed unit within engage_range.
    contact: u32,
    /// Of those, ticks on a commander order (`MoveTarget` without `Engaging`).
    contact_on_order: u32,
    /// D's sent units' hits on T's armed units, workers, buildings; damage.
    d_hits_army: u32,
    d_hits_worker: u32,
    d_hits_bld: u32,
    d_dmg_army: u32,
    /// T's armed units' hits on D's units, and damage.
    t_hits: u32,
    t_dmg: u32,
}

#[derive(Clone, Debug)]
struct WaveDiag {
    commit: u32,
    size: u32,
    members: u32,
    at_commit: Census,
    arrival: Option<(u32, Census, u32)>, // (tick, D census, wave members alive)
    /// End of the trade window (D's HQ fell, or the wave was spent, or the
    /// match ended).
    window_end: u32,
    turtle_lost: u32,
    d_lost: u32,
    d_lost_away: u32,
    /// Of `turtle_lost`, how many were first-wave members.
    wave_lost: u32,
}

fn census(tracks: &BTreeMap<u64, Track>, d: Faction, d_hq: Vec2, r: f32) -> Census {
    let mut c = Census::default();
    for t in tracks.values().filter(|t| t.faction == d && t.armed) {
        match t.died {
            Some(_) => c.dead += 1,
            None if t.pos.distance(d_hq) <= r => c.home += 1,
            None => c.field += 1,
        }
    }
    c
}

/// Play one match, observing every tick. Read-only on the world.
fn diagnose(content: &Content, opponent: &str, turtle_seat: Faction, seed_k: u32) -> MatchDiag {
    let seed = seed_at(0, seed_k);
    let (a, b) = match turtle_seat {
        Faction::A => (TURTLE, opponent),
        Faction::B => (opponent, TURTLE),
    };
    let settings = MatchSettings::default()
        .with_seed(seed)
        .with_strategies(a, b)
        .with_orientation(Orientation::Normal);
    let mut app = headless::ai_vs_ai(content.clone(), &settings).expect("shipped strategies");
    let me = turtle_seat;
    let d = match me {
        Faction::A => Faction::B,
        Faction::B => Faction::A,
    };
    let r = home_radius(content);
    let engage = content.combat.engage_range;

    // HQs do not move: read once.
    let mut hq_q = app
        .world_mut()
        .query::<(Entity, &Position, &Building, &Faction)>();
    let mut hq_of = [Vec2::ZERO; 2];
    let slot = |f: Faction| match f {
        Faction::A => 0,
        Faction::B => 1,
    };
    for (_, p, bld, f) in hq_q.iter(app.world()) {
        if content.buildings[bld.def].victory {
            hq_of[slot(*f)] = p.0;
        }
    }
    let (t_hq, d_hq) = (hq_of[slot(me)], hq_of[slot(d)]);

    #[allow(clippy::type_complexity)]
    let mut unit_q = app.world_mut().query::<(
        Entity,
        &Position,
        &UnitDefIdx,
        &Faction,
        Option<&MoveTarget>,
        Option<&AttackCooldown>,
        Option<&Target>,
        Option<&Engaging>,
        Option<&AttackMoveDest>,
    )>();
    let mut fight = Fight::default();
    // Building defs by entity bits, every faction (for classifying hits).
    let mut bld_def: BTreeMap<u64, usize> = BTreeMap::new();
    let mut bld_q = app
        .world_mut()
        .query::<(Entity, &Building, &Faction, Option<&Health>)>();
    // T's buildings' HP, by entity bits: last seen value.
    let mut t_bld_hp: BTreeMap<u64, u32> = BTreeMap::new();
    let mut t_bld_hp_lost = 0u32;
    let mut d_first: Option<(u32, u32)> = None;
    let mut d_arrival: Option<(u32, u32, u32)> = None;
    let mut d_bld_lost_at_first = 0u32;
    let mut t_army_hp_at_d_arrival = 0u32;
    let mut d_army_hp_at_d_arrival = 0u32;
    let mut t_bld_hp_lost_at_commit: Option<u32> = None;

    let mut tracks: BTreeMap<u64, Track> = BTreeMap::new();
    let mut journal_seen = 0usize;
    let mut tick = 0u32;
    let mut first_wave: Option<(u32, u32, Vec<u64>, Census)> = None;
    let mut arrival: Option<(u32, Census, u32)> = None;
    let mut d_sent: Vec<u64> = Vec::new(); // sorted, distinct
    let mut d_attacks_before = 0u32;
    let mut outcome = None;

    while tick < settings.tick_cap {
        headless::step(&mut app);
        tick += 1;

        // ---- units: positions, births, deaths ------------------------------
        let mut seen: Vec<(u64, Vec2, Option<Vec2>)> = Vec::new();
        // (attacker bits, hit target bits, on an order without Engaging)
        let mut hits: Vec<(u64, Option<u64>)> = Vec::new();
        let mut on_order: Vec<u64> = Vec::new();
        for (e, p, def, f, mt, cd, tgt, eng, am) in unit_q.iter(app.world()) {
            let k = e.to_bits();
            // The order's destination: the attack-move's if there is one, else the move's.
            seen.push((k, p.0, am.map(|a| a.0).or(mt.map(|m| m.0))));
            if mt.is_some() && eng.is_none() {
                on_order.push(k);
            }
            let ticks = content.units[def.0].mvp_attack_ticks;
            if ticks > 0 && cd.is_some_and(|c| c.0 == ticks) {
                let prev = tracks.get(&k).and_then(|t| t.last_target);
                hits.push((k, tgt.map(|t| t.0.to_bits()).or(prev)));
            }
            if let Some(t) = tracks.get_mut(&k) {
                t.last_target = tgt.map(|t| t.0.to_bits());
            }
            tracks
                .entry(k)
                .and_modify(|t| t.pos = p.0)
                .or_insert(Track {
                    faction: *f,
                    armed: armed(content, def.0),
                    max_hp: Health::from_def(content, def.0).max,
                    def: def.0,
                    last_target: None,
                    pos: p.0,
                    died: None,
                });
        }
        seen.sort_unstable_by_key(|s| s.0);
        for (k, t) in tracks.iter_mut() {
            if t.died.is_none() && seen.binary_search_by_key(k, |s| s.0).is_err() {
                t.died = Some(tick);
            }
        }

        // ---- the fight at T's base (D attacking, T not yet committed) ------
        if d_first.is_some() && first_wave.is_none() {
            on_order.sort_unstable();
            let t_armed: Vec<Vec2> = tracks
                .values()
                .filter(|t| t.faction == me && t.armed && t.died.is_none())
                .map(|t| t.pos)
                .collect();
            for k in &d_sent {
                let t = tracks[k];
                if t.died.is_some() {
                    continue;
                }
                if t_armed.iter().any(|q| q.distance(t.pos) <= engage) {
                    fight.contact += 1;
                    if on_order.binary_search(k).is_ok() {
                        fight.contact_on_order += 1;
                    }
                }
            }
            for (a, victim) in &hits {
                let at = tracks[a];
                let Some(v) = victim else {
                    continue;
                };
                if at.faction == d && d_sent.binary_search(a).is_ok() {
                    if let Some(vt) = tracks.get(v) {
                        if vt.armed {
                            fight.d_hits_army += 1;
                            fight.d_dmg_army += damage_per_hit(content, at.def, vt.def);
                        } else {
                            fight.d_hits_worker += 1;
                        }
                    } else if bld_def.contains_key(v) {
                        fight.d_hits_bld += 1;
                    }
                } else if at.faction == me && at.armed {
                    if let Some(vt) = tracks.get(v) {
                        fight.t_hits += 1;
                        fight.t_dmg += damage_per_hit(content, at.def, vt.def);
                    } else if let Some(b) = bld_def.get(v) {
                        fight.t_hits += 1;
                        fight.t_dmg += damage_per_hit_to_building(content, at.def, *b);
                    }
                }
            }
        }

        // ---- T's buildings' HP ---------------------------------------------
        {
            for (e, b, _, _) in bld_q.iter(app.world()) {
                bld_def.entry(e.to_bits()).or_insert(b.def);
            }
            let mut now: Vec<(u64, u32)> = bld_q
                .iter(app.world())
                .filter(|(_, _, f, _)| **f == me)
                .map(|(e, b, _, h)| {
                    let full = Health::from_building_def(content, b.def).max;
                    (e.to_bits(), h.map_or(full, |h| h.current))
                })
                .collect();
            now.sort_unstable_by_key(|x| x.0);
            for (k, hp) in t_bld_hp.iter_mut() {
                match now.binary_search_by_key(k, |x| x.0) {
                    Ok(i) => {
                        t_bld_hp_lost += hp.saturating_sub(now[i].1);
                        *hp = now[i].1;
                    }
                    Err(_) => {
                        t_bld_hp_lost += *hp;
                        *hp = 0;
                    }
                }
            }
            for (k, hp) in now {
                t_bld_hp.entry(k).or_insert(hp);
            }
        }

        // ---- new AI actions this tick --------------------------------------
        let new: Vec<(Faction, AiAction)> = {
            let j = app.world().resource::<AiJournal>();
            let v = j.0[journal_seen..]
                .iter()
                .map(|(_, f, a)| (*f, *a))
                .collect();
            journal_seen = j.0.len();
            v
        };
        for (f, act) in new {
            let AiAction::Attack { dest, force } = act else {
                continue;
            };
            let members: Vec<u64> = seen
                .iter()
                .filter(|(k, _, mt)| {
                    *mt == Some(dest) && tracks.get(k).is_some_and(|t| t.faction == f && t.armed)
                })
                .map(|(k, _, _)| *k)
                .collect();
            if f == me && first_wave.is_none() {
                let c = census(&tracks, d, d_hq, r);
                first_wave = Some((tick, force, members, c));
                t_bld_hp_lost_at_commit = Some(t_bld_hp_lost);
            } else if f == d && first_wave.is_none() {
                d_attacks_before += 1;
                if d_first.is_none() {
                    d_first = Some((tick, force));
                    d_bld_lost_at_first = t_bld_hp_lost;
                }
                for k in members {
                    if let Err(i) = d_sent.binary_search(&k) {
                        d_sent.insert(i, k);
                    }
                }
            }
        }

        // ---- arrival of D's units at T's base (before T commits) -----------
        if first_wave.is_none() && d_arrival.is_none() && !d_sent.is_empty() {
            let alive = |k: &u64| tracks.get(k).is_some_and(|t| t.died.is_none());
            if d_sent
                .iter()
                .any(|k| alive(k) && tracks[k].pos.distance(t_hq) <= engage)
            {
                let d_alive = d_sent.iter().filter(|k| alive(k)).count() as u32;
                d_army_hp_at_d_arrival = d_sent
                    .iter()
                    .filter(|k| alive(k))
                    .map(|k| tracks[k].max_hp)
                    .sum();
                let t_armed: Vec<&Track> = tracks
                    .values()
                    .filter(|t| t.faction == me && t.armed && t.died.is_none())
                    .collect();
                t_army_hp_at_d_arrival = t_armed.iter().map(|t| t.max_hp).sum();
                d_arrival = Some((tick, d_alive, t_armed.len() as u32));
            }
        }

        // ---- arrival of the first wave -------------------------------------
        if let (Some((_, _, members, _)), None) = (&first_wave, &arrival) {
            let near = members.iter().any(|k| {
                tracks
                    .get(k)
                    .is_some_and(|t| t.died.is_none() && t.pos.distance(d_hq) <= engage)
            });
            if near {
                let alive = members
                    .iter()
                    .filter(|k| tracks.get(k).is_some_and(|t| t.died.is_none()))
                    .count() as u32;
                arrival = Some((tick, census(&tracks, d, d_hq, r), alive));
            }
        }

        if let Some(o) = app.world().resource::<MatchState>().outcome() {
            outcome = Some(o);
            break;
        }
    }

    let end_tick = tick;
    let (winner, decided) = match outcome {
        Some(o) => (o.winner, true),
        None => (None, false),
    };

    let commit_tick = first_wave.as_ref().map(|w| w.0).unwrap_or(end_tick);
    let turtle_lost_before_commit = tracks
        .values()
        .filter(|t| t.faction == me && t.armed && t.died.is_some_and(|x| x <= commit_tick))
        .count() as u32;
    let d_sent_dead_before = d_sent
        .iter()
        .filter(|k| tracks[*k].died.is_some_and(|x| x <= commit_tick))
        .count() as u32;
    let d_sent_died_near_turtle = d_sent
        .iter()
        .filter(|k| {
            let t = &tracks[*k];
            t.died.is_some_and(|x| x <= commit_tick) && t.pos.distance(t_hq) <= r
        })
        .count() as u32;

    let t_bld_hp_lost_at_commit = t_bld_hp_lost_at_commit.unwrap_or(t_bld_hp_lost);
    let d_wave = d_first.map(|(commit, size)| DWaveDiag {
        commit,
        size,
        arrival: d_arrival,
        turtle_lost: tracks
            .values()
            .filter(|t| {
                t.faction == me && t.armed && t.died.is_some_and(|x| x > commit && x <= commit_tick)
            })
            .count() as u32,
        turtle_building_hp_lost: t_bld_hp_lost_at_commit.saturating_sub(d_bld_lost_at_first),
        turtle_army_hp_at_arrival: t_army_hp_at_d_arrival,
        d_army_hp_at_arrival: d_army_hp_at_d_arrival,
        fight,
    });

    let wave = first_wave.map(|(commit, size, members, at_commit)| {
        // The wave is spent at the tick its last member died.
        let spent = members
            .iter()
            .map(|k| tracks[k].died)
            .try_fold(0u32, |acc, d| d.map(|x| acc.max(x)));
        let window_end = spent.map_or(end_tick, |s| s.min(end_tick));
        let in_window = |t: &Track| t.died.is_some_and(|x| x > commit && x <= window_end);
        let turtle_lost = tracks
            .values()
            .filter(|t| t.faction == me && t.armed && in_window(t))
            .count() as u32;
        let d_dead: Vec<&Track> = tracks
            .values()
            .filter(|t| t.faction == d && t.armed && in_window(t))
            .collect();
        let wave_lost = members.iter().filter(|k| in_window(&tracks[*k])).count() as u32;
        WaveDiag {
            commit,
            size,
            members: members.len() as u32,
            at_commit,
            arrival,
            window_end,
            turtle_lost,
            d_lost: d_dead.len() as u32,
            d_lost_away: d_dead.iter().filter(|t| t.pos.distance(d_hq) > r).count() as u32,
            wave_lost,
        }
    });

    // ---- structural facts ---------------------------------------------------
    let fielded = tracks
        .values()
        .filter(|t| t.faction == d && t.armed)
        .count() as u32;
    let now = census(&tracks, d, d_hq, r);
    assert_eq!(
        now.home + now.field + now.dead,
        fielded,
        "census does not account for every armed unit D fielded"
    );
    if let Some(w) = &wave {
        assert!(w.size > 0, "a wave of nobody");
        assert_eq!(
            w.members, w.size,
            "{opponent} {turtle_seat:?} k{seed_k}: the wave's members (MoveTarget == dest) \
             are not the journal's force"
        );
        assert!(w.commit <= w.window_end && w.window_end <= end_tick);
        assert!(w.wave_lost <= w.members && w.wave_lost <= w.turtle_lost);
        assert!(w.d_lost_away <= w.d_lost);
        if let Some((at, c, alive)) = w.arrival {
            assert!(at >= w.commit, "arrival before commit");
            assert!(alive >= 1 && alive <= w.members);
            assert!(c.dead >= w.at_commit.dead, "the dead came back");
        }
    }
    if let Some(dw) = &d_wave {
        assert!(dw.size > 0 && dw.commit <= commit_tick);
        assert!(dw.fight.contact_on_order <= dw.fight.contact);
        if let Some((at, alive, _)) = dw.arrival {
            assert!(at >= dw.commit && alive >= 1 && alive <= d_sent.len() as u32);
        }
    }
    assert!(
        d_sent_died_near_turtle <= d_sent_dead_before && d_sent_dead_before <= d_sent.len() as u32
    );

    MatchDiag {
        opponent: opponent.to_string(),
        turtle_seat,
        seed_k,
        winner,
        end_tick,
        decided,
        wave,
        turtle_lost_before_commit,
        d_attacks_before,
        d_sent_before: d_sent.len() as u32,
        d_sent_dead_before,
        d_sent_died_near_turtle,
        d_wave,
    }
}

fn fmt_census(c: Census) -> String {
    format!("{:>2}/{:>2}/{:>2}", c.home, c.field, c.dead)
}

fn print_rows(rows: &[MatchDiag]) {
    println!(
        "{:<18} {:>4} {:>2} {:>6} {:>6} | {:>6} {:>4} {:>8} | {:>6} {:>4} {:>8} | {:>5} {:>5} {:>4} | {:>3} {:>4} {:>4} {:>4} {:>4}",
        "opponent", "T@", "k", "winner", "end",
        "commit", "wave", "D h/f/d",
        "arrive", "wAl", "D h/f/d",
        "T-", "D-", "away",
        "Datk", "sent", "dead", "nrT", "T-pre"
    );
    for m in rows {
        let w = m.wave.as_ref();
        let winner = match (m.decided, m.winner) {
            (false, _) => "cap".to_string(),
            (true, None) => "draw".to_string(),
            (true, Some(f)) if f == m.turtle_seat => "turtle".to_string(),
            (true, Some(_)) => "opp".to_string(),
        };
        let (commit, size, atc) = w.map_or(("-".into(), "-".into(), "-".into()), |w| {
            (
                w.commit.to_string(),
                w.size.to_string(),
                fmt_census(w.at_commit),
            )
        });
        let (arr, alive, ata) = match w.and_then(|w| w.arrival) {
            Some((t, c, a)) => (t.to_string(), a.to_string(), fmt_census(c)),
            None => ("-".into(), "-".into(), "-".into()),
        };
        let (tl, dl, away) = w.map_or(("-".into(), "-".into(), "-".into()), |w| {
            (
                w.turtle_lost.to_string(),
                w.d_lost.to_string(),
                w.d_lost_away.to_string(),
            )
        });
        println!(
            "{:<18} {:>4} {:>2} {:>6} {:>6} | {:>6} {:>4} {:>8} | {:>6} {:>4} {:>8} | {:>5} {:>5} {:>4} | {:>3} {:>4} {:>4} {:>4} {:>4}",
            m.opponent, format!("{:?}", m.turtle_seat), m.seed_k, winner, m.end_tick,
            commit, size, atc, arr, alive, ata, tl, dl, away,
            m.d_attacks_before, m.d_sent_before, m.d_sent_dead_before, m.d_sent_died_near_turtle,
            m.turtle_lost_before_commit
        );
    }
}

/// D's waves at T's base, per match.
fn print_d_rows(rows: &[MatchDiag]) {
    println!(
        "\n{:<18} {:>4} {:>2} | {:>6} {:>4} | {:>6} {:>4} {:>4} {:>6} {:>6} | {:>4} {:>4} {:>4} {:>6} | {:>5} {:>5} | {:>4} {:>4} {:>4} {:>6} | {:>4} {:>6}",
        "D wave at T base", "T@", "k", "Dcmt", "size", "arrive", "Dal", "Tal", "D HP", "T HP",
        "sent", "D-", "T-", "T bldHP-", "cont", "order", "Dh-a", "Dh-w", "Dh-b", "Ddmg", "Th", "Tdmg"
    );
    for m in rows {
        let Some(dw) = m.d_wave else {
            continue;
        };
        let (arr, dal, tal) = match dw.arrival {
            Some((t, a, b)) => (t.to_string(), a.to_string(), b.to_string()),
            None => ("-".into(), "-".into(), "-".into()),
        };
        let fx = dw.fight;
        println!(
            "{:<18} {:>4} {:>2} | {:>6} {:>4} | {:>6} {:>4} {:>4} {:>6} {:>6} | {:>4} {:>4} {:>4} {:>6} | {:>5} {:>5} | {:>4} {:>4} {:>4} {:>6} | {:>4} {:>6}",
            m.opponent, format!("{:?}", m.turtle_seat), m.seed_k, dw.commit, dw.size,
            arr, dal, tal, dw.d_army_hp_at_arrival, dw.turtle_army_hp_at_arrival,
            m.d_sent_before, m.d_sent_dead_before, dw.turtle_lost, dw.turtle_building_hp_lost,
            fx.contact, fx.contact_on_order, fx.d_hits_army, fx.d_hits_worker, fx.d_hits_bld,
            fx.d_dmg_army, fx.t_hits, fx.t_dmg
        );
    }
}

fn mean(xs: impl Iterator<Item = f32>) -> String {
    let v: Vec<f32> = xs.collect();
    if v.is_empty() {
        "-".into()
    } else {
        format!("{:.1}", v.iter().sum::<f32>() / v.len() as f32)
    }
}

/// Per opponent, means over the matches in which T sent its first wave (and,
/// for the arrival columns, in which it arrived).
fn print_aggregate(rows: &[MatchDiag], opponents: &[String]) {
    println!(
        "\n{:<18} {:>5} {:>5} | {:>5} {:>17} | {:>5} {:>17} | {:>5} {:>5} {:>5} | {:>5} {:>5} {:>5}",
        "opponent (mean)", "T win", "n", "wave", "D@commit h/f/d", "wAl", "D@arrival h/f/d",
        "T-", "D-", "away", "sent", "dead", "nrT"
    );
    for opp in opponents {
        let ms: Vec<&MatchDiag> = rows.iter().filter(|m| &m.opponent == opp).collect();
        let wins = ms
            .iter()
            .filter(|m| m.decided && m.winner == Some(m.turtle_seat))
            .count();
        let ws: Vec<&WaveDiag> = ms.iter().filter_map(|m| m.wave.as_ref()).collect();
        let arr: Vec<(Census, u32)> = ws
            .iter()
            .filter_map(|w| w.arrival.map(|a| (a.1, a.2)))
            .collect();
        let f = |x: u32| x as f32;
        println!(
            "{:<18} {:>5} {:>5} | {:>5} {:>5}/{:>5}/{:>5} | {:>5} {:>5}/{:>5}/{:>5} | {:>5} {:>5} {:>5} | {:>5} {:>5} {:>5}",
            opp,
            format!("{}/{}", wins, ms.len()),
            format!("{}w{}a", ws.len(), arr.len()),
            mean(ws.iter().map(|w| f(w.size))),
            mean(ws.iter().map(|w| f(w.at_commit.home))),
            mean(ws.iter().map(|w| f(w.at_commit.field))),
            mean(ws.iter().map(|w| f(w.at_commit.dead))),
            mean(arr.iter().map(|a| f(a.1))),
            mean(arr.iter().map(|a| f(a.0.home))),
            mean(arr.iter().map(|a| f(a.0.field))),
            mean(arr.iter().map(|a| f(a.0.dead))),
            mean(ws.iter().map(|w| f(w.turtle_lost))),
            mean(ws.iter().map(|w| f(w.d_lost))),
            mean(ws.iter().map(|w| f(w.d_lost_away))),
            mean(ms.iter().map(|m| f(m.d_sent_before))),
            mean(ms.iter().map(|m| f(m.d_sent_dead_before))),
            mean(ms.iter().map(|m| f(m.d_sent_died_near_turtle))),
        );
    }
    println!(
        "\n{:<18} {:>5} | {:>5} {:>5} {:>7} {:>7} | {:>5} {:>5} {:>5} {:>8} | {:>6} {:>6} {:>6} {:>6} {:>6} {:>7} {:>7}",
        "D waves (mean)", "n", "Dal", "Tal", "D HP", "T HP", "sent", "D-", "T-", "T bldHP-",
        "order%", "Dh-a", "Dh-w", "Dh-b", "Th", "Ddmg", "Tdmg"
    );
    for opp in opponents {
        let ds: Vec<(DWaveDiag, &MatchDiag)> = rows
            .iter()
            .filter(|m| &m.opponent == opp)
            .filter_map(|m| m.d_wave.map(|d| (d, m)))
            .collect();
        let arr: Vec<&(DWaveDiag, &MatchDiag)> =
            ds.iter().filter(|(d, _)| d.arrival.is_some()).collect();
        let f = |x: u32| x as f32;
        let cont: u32 = ds.iter().map(|(d, _)| d.fight.contact).sum();
        let ord: u32 = ds.iter().map(|(d, _)| d.fight.contact_on_order).sum();
        let order_pct = if cont == 0 {
            "-".to_string()
        } else {
            format!("{:.0}", 100.0 * ord as f32 / cont as f32)
        };
        println!(
            "{:<18} {:>5} | {:>5} {:>5} {:>7} {:>7} | {:>5} {:>5} {:>5} {:>8} | {:>6} {:>6} {:>6} {:>6} {:>6} {:>7} {:>7}",
            opp,
            format!("{}w{}a", ds.len(), arr.len()),
            mean(arr.iter().map(|(d, _)| f(d.arrival.unwrap().1))),
            mean(arr.iter().map(|(d, _)| f(d.arrival.unwrap().2))),
            mean(arr.iter().map(|(d, _)| f(d.d_army_hp_at_arrival))),
            mean(arr.iter().map(|(d, _)| f(d.turtle_army_hp_at_arrival))),
            mean(ds.iter().map(|(_, m)| f(m.d_sent_before))),
            mean(ds.iter().map(|(_, m)| f(m.d_sent_dead_before))),
            mean(ds.iter().map(|(d, _)| f(d.turtle_lost))),
            mean(ds.iter().map(|(d, _)| f(d.turtle_building_hp_lost))),
            order_pct,
            mean(ds.iter().map(|(d, _)| f(d.fight.d_hits_army))),
            mean(ds.iter().map(|(d, _)| f(d.fight.d_hits_worker))),
            mean(ds.iter().map(|(d, _)| f(d.fight.d_hits_bld))),
            mean(ds.iter().map(|(d, _)| f(d.fight.t_hits))),
            mean(ds.iter().map(|(d, _)| f(d.fight.d_dmg_army))),
            mean(ds.iter().map(|(d, _)| f(d.fight.t_dmg))),
        );
    }
}

/// The shipped content, or, opt-in for B4 step 2, the directory named by
/// `ONUS_DIAG_DATA` (e.g. a scratch copy with `wave_order: attack_move`). The
/// variable is read only here, by this test file; the sim never sees it.
fn content() -> Content {
    match std::env::var_os("ONUS_DIAG_DATA") {
        Some(dir) => Content::load_from_dir(std::path::Path::new(&dir))
            .expect("ONUS_DIAG_DATA content loads"),
        None => headless::content().expect("shipped content loads"),
    }
}

/// The fast structural check (un-ignored): one full turtle match, both the
/// identities in [`diagnose`] and that the measurement has something to read.
#[test]
fn the_turtle_diagnostic_accounts_for_every_armed_unit_in_one_match() {
    let c = content();
    assert!(c.strategy_index(TURTLE).is_some(), "no `turtle` strategy");
    let r = home_radius(&c);
    assert!(
        r > c.combat.engage_range && r < 750.0,
        "home radius {r} must exceed engage_range and stay short of the map midpoint"
    );
    let m = diagnose(&c, "mass_bulwark", Faction::A, 0);
    print_rows(std::slice::from_ref(&m));
    print_d_rows(std::slice::from_ref(&m));
    assert!(m.decided, "the probe match did not end");
    let w = m
        .wave
        .as_ref()
        .expect("the turtle sent no wave against mass_bulwark");
    assert!(
        w.arrival.is_some() || m.winner != Some(m.turtle_seat),
        "the turtle won without its first wave ever reaching the enemy HQ"
    );
    assert!(
        m.d_attacks_before > 0,
        "the opponent never attacked before the turtle"
    );
}

/// The step-0 table (F-047): turtle against every other shipped strategy, both
/// seats, the balance report's first two seeds.
#[test]
#[ignore = "36 full matches; run explicitly in release with --nocapture"]
fn turtle_step0_table() {
    let c = content();
    let opponents: Vec<String> = c
        .strategies
        .iter()
        .map(|s| s.id.clone())
        .filter(|id| id != TURTLE)
        .collect();
    assert_eq!(opponents.len(), c.strategies.len() - 1);
    let mut rows = Vec::new();
    for opp in &opponents {
        for seat in [Faction::A, Faction::B] {
            for k in 0..2 {
                rows.push(diagnose(&c, opp, seat, k));
            }
        }
    }
    println!(
        "home radius {} (max barracks offset + engage_range {}); D h/f/d = defender armed home/field/dead; \
         wAl = wave members alive at arrival; T-/D- = armed lost in the trade window; away = D losses outside home; \
         Datk/sent/dead/nrT = D's attacks, distinct units sent, dead, died near T's HQ before T's commit; \
         T-pre = turtle armed lost before its commit",
        home_radius(&c),
        c.combat.engage_range
    );
    print_rows(&rows);
    print_d_rows(&rows);
    print_aggregate(&rows, &opponents);
}

/// B4 step 2's "the turtle still commits last" reading, on whatever
/// [`content`] names (the shipped data, or `ONUS_DIAG_DATA`). The method is
/// `critic_b35_closure`'s: slot A's first `Attack` in each strategy's mirror,
/// on the closure's three seeds. Printed, not asserted; the shipped claim is
/// pinned in `critic_b35_closure`.
#[test]
#[ignore = "30 mirrors to first attack; run explicitly in release with --nocapture"]
fn commit_order_on_the_diag_content() {
    let c = content();
    for k in 0..3u32 {
        let seed = seed_at(0x0070_A71E, k);
        let mut commits: Vec<(u32, String)> = Vec::new();
        for s in &c.strategies {
            let settings = MatchSettings::default()
                .with_seed(seed)
                .with_strategies(&s.id, &s.id);
            let mut app = headless::ai_vs_ai(c.clone(), &settings).expect("known strategy");
            let mut at = None;
            for _ in 0..onus::headless::DEFAULT_TICK_CAP {
                headless::step(&mut app);
                let j = app.world().resource::<AiJournal>();
                at = j
                    .for_faction(Faction::A)
                    .into_iter()
                    .find_map(|(t, a)| match a {
                        AiAction::Attack { .. } => Some(t),
                        _ => None,
                    });
                if at.is_some() || app.world().resource::<MatchState>().is_over() {
                    break;
                }
            }
            commits.push((at.unwrap_or(u32::MAX), s.id.clone()));
        }
        commits.sort();
        println!("seed {seed:#x}: {commits:?}");
    }
}
