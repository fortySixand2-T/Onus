//! L2 integration tests for **B4: multi-barracks as a real capability** — the
//! commander's *opening reservation* (F-041, from F-035).
//!
//! F-035 measured that a scripted opening past the first rarely goes up: the
//! tech step places an opening only when the stockpile covers its cost, and the
//! army step spends the stockpile on anything cheaper first, so under B3.5's
//! economy the stockpile never climbs back to 150-200. The rule added in
//! `sim::ai` (stated there and in F-041):
//!
//!   - an opening is **due** once `tick >= at_tick` and it is not standing;
//!   - the first due opening the tech step cannot pay for (RON order) has its
//!     Alloy reserved, held back from the army step, so **while an opening is
//!     due, the army waits**;
//!   - the tech walk is unchanged: a later, cheaper opening that is due and
//!     affordable still goes up first (B3.5 AC0b's skip rule, RNG included);
//!   - workers are not held, and an opening whose `at_tick` is still ahead
//!     reserves nothing.
//!
//! What is encoded here:
//!   - a three-line script really builds three lines **in a real match** (red
//!     before the reservation: one of three went up);
//!   - every multi-opening strategy in the shipped set, in its own mirror,
//!     places a script-order **prefix** of its openings, and every opening it
//!     left unplaced came due too late to bank: `at_tick + grace > end tick`,
//!     with `grace` the fewest ticks its Alloy can be banked from empty at the
//!     side's best-case income, derived from the loaded RON (F-046, the bound);
//!     at least one strategy must still place more than one opening, so the
//!     bound cannot make the claim vacuous;
//!   - the army never trains while an opening is due;
//!   - the hold does not reorder the tech walk: a cheaper opening listed later
//!     still goes up past a reserved one, and both stand;
//!   - an opening that is not due yet changes nothing, tick for tick.
//!
//! Everything runs headless (`MinimalPlugins`) through the shipped sim chain.

use bevy::prelude::*;

use onus::batch::seed_at;
use onus::headless::{self, MatchSettings, DEFAULT_TICK_CAP};
use onus::sim::content::{BarracksOpening, Content};
use onus::sim::replay::state_hash;
use onus::sim::spatial::Faction;
use onus::sim::{AiAction, AiJournal, MatchState};

// ---- harness ----------------------------------------------------------------

fn content() -> Content {
    headless::content().expect("assets/data/*.ron parse into sim structs")
}

fn opening(building: &str, at_tick: u32, offset: f32) -> BarracksOpening {
    BarracksOpening {
        building: building.to_string(),
        at_tick,
        offset,
    }
}

/// `content` with strategy `id`'s opening list replaced by `openings`.
fn with_openings(mut content: Content, id: &str, openings: Vec<BarracksOpening>) -> Content {
    let i = content
        .strategy_index(id)
        .unwrap_or_else(|| panic!("the shipped set names `{id}`"));
    content.strategies[i].barracks = openings;
    content
}

/// F-030's mass-probe script: one building opened three times, at 300 / 600 /
/// 900 ticks and 130 / 165 / 200 units from home.
fn three_lines(building: &str) -> Vec<BarracksOpening> {
    vec![
        opening(building, 300, 130.0),
        opening(building, 600, 165.0),
        opening(building, 900, 200.0),
    ]
}

fn match_on(content: Content, a: &str, b: &str, seed: u64) -> App {
    let settings = MatchSettings::default()
        .with_seed(seed)
        .with_strategies(a, b);
    headless::ai_vs_ai(content, &settings).expect("both sides name shipped strategies")
}

/// The ticks at which `f` placed each barracks, in placement order.
fn placements(app: &App, f: Faction) -> Vec<(u32, usize)> {
    app.world()
        .resource::<AiJournal>()
        .for_faction(f)
        .into_iter()
        .filter_map(|(t, a)| match a {
            AiAction::PlaceBarracks { building, .. } => Some((t, building)),
            _ => None,
        })
        .collect()
}

fn over(app: &App) -> bool {
    app.world().resource::<MatchState>().is_over()
}

/// Steps `app` until `done` holds, the match is decided, or the match cap.
/// Returns the tick it stopped on.
fn play_until(app: &mut App, done: impl Fn(&App) -> bool) -> u32 {
    let mut t = 0;
    while t < DEFAULT_TICK_CAP && !over(app) && !done(app) {
        headless::step(app);
        t += 1;
    }
    t
}

// ---- the capability -----------------------------------------------------------

/// **The gating test.** A `mass_bulwark` scripted with F-030's three Foundries
/// (the most expensive body, so the hardest case) plays a real
/// head-to-head against the shipped `mass_sentinel` and must stand all three
/// before the match ends. Red before the reservation: the first Foundry went up
/// and the stockpile never reached 150 again (F-035).
#[test]
fn a_three_line_script_builds_all_three_lines_in_a_real_match() {
    let c = content();
    let foundry = c.building_index("foundry").unwrap();
    let c = with_openings(c, "mass_bulwark", three_lines("foundry"));
    let mut app = match_on(c, "mass_bulwark", "mass_sentinel", seed_at(0, 0));
    let stopped = play_until(&mut app, |app| placements(app, Faction::A).len() >= 3);
    let placed = placements(&app, Faction::A);
    assert_eq!(
        placed.iter().map(|(_, b)| *b).collect::<Vec<_>>(),
        vec![foundry; 3],
        "a three-Foundry `mass_bulwark` placed {placed:?} by tick {stopped} (match over: {}) — \
         the army step spent the Alloy its due openings needed",
        over(&app)
    );
}

/// The fewest ticks a side running `strategy` can take to bank `building`'s
/// Alloy from an empty stockpile — the opening's **grace**.
///
/// Derivation, every value from the loaded content (`assets/data/*.ron`):
///   - the side keeps at most `worker_target` gatherers (the strategy's RON);
///   - one gatherer delivers at most `mvp_carry_capacity` Alloy per
///     `mvp_gather_ticks` (the gathering unit's RON): a trip is at least the
///     harvest, plus a walk to the deposit and back that only makes it longer;
///   - so the side's income is at most `worker_target * carry / gather_ticks`
///     Alloy per tick, and banking `alloy_cost` (the building's RON) from zero
///     takes at least `ceil(alloy_cost * gather_ticks / (worker_target * carry))`
///     ticks.
///
/// It is a **lower bound** on the real banking time — walking, the army and
/// worker spending before the opening comes due, and lost workers all slow it
/// — so it excuses the fewest openings an income model can: the claim it bounds
/// stays as strict as an honest bound allows. Walking time is left out because
/// the deposit's distance is fixture geometry (`headless`), not content.
fn grace(c: &Content, strategy: &str, building: &str) -> u32 {
    let gatherers: Vec<_> = c.units.iter().filter(|u| u.gathers).collect();
    assert_eq!(gatherers.len(), 1, "the content has one gathering unit");
    let (carry, gather_ticks) = (
        gatherers[0].mvp_carry_capacity,
        gatherers[0].mvp_gather_ticks,
    );
    let workers = c.strategy(strategy).unwrap().worker_target;
    let cost = c.building(building).unwrap().alloy_cost;
    assert!(
        carry > 0 && gather_ticks > 0 && workers > 0,
        "a zero income term: carry {carry}, gather_ticks {gather_ticks}, workers {workers}"
    );
    (cost * gather_ticks).div_ceil(workers * carry)
}

/// Every strategy the shipped set scripts with more than one opening, in its
/// own mirror (seed `seed_at(0, 1)`, both sides checked), places its openings
/// **in script order** and leaves unplaced only an opening that came due too
/// close to the match end to bank: what was placed is a prefix of the script,
/// and every unplaced opening has `at_tick + grace > end tick` ([`grace`] —
/// derived from the RON, not chosen). Equivalently, every opening due at least
/// `grace` ticks before the end is placed. Iterated from the content, so a
/// script added later is covered too.
///
/// The bound (F-046): g1 scripts `mass_sentinel`'s third line at 15 000 and its
/// mirror ends at 15 519, too soon to bank a Foundry at any income the RON
/// allows; the claim is about the reservation, not about the match outlasting
/// every script. Non-vacuity: at least one strategy must still place more than
/// one opening on both sides, so the bound cannot excuse the whole claim away.
#[test]
fn every_multi_opening_strategy_places_its_whole_script_in_a_head_to_head() {
    let c = content();
    let multi: Vec<String> = c
        .strategies
        .iter()
        .filter(|s| s.barracks.len() > 1)
        .map(|s| s.id.clone())
        .collect();
    assert!(!multi.is_empty(), "the shipped set has no multi-opening strategy");
    let mut placed_more_than_one = Vec::new();
    for id in &multi {
        let s = c.strategy(id).unwrap();
        let want: Vec<usize> = s
            .barracks
            .iter()
            .map(|o| c.building_index(&o.building).unwrap())
            .collect();
        let mut app = match_on(c.clone(), id, id, seed_at(0, 1));
        let n = want.len();
        let stopped = play_until(&mut app, |app| {
            placements(app, Faction::A).len() >= n && placements(app, Faction::B).len() >= n
        });
        let mut fewest = n;
        for f in [Faction::A, Faction::B] {
            let got: Vec<usize> = placements(&app, f).iter().map(|(_, b)| *b).collect();
            fewest = fewest.min(got.len());
            assert!(
                got.len() <= n && got[..] == want[..got.len()],
                "`{id}` ({f:?}) placed {got:?}, not a script-order prefix of {want:?}, \
                 by tick {stopped} (match over: {})",
                over(&app)
            );
            for (k, o) in s.barracks.iter().enumerate().skip(got.len()) {
                let g = grace(&c, id, &o.building);
                assert!(
                    o.at_tick + g > stopped,
                    "`{id}` ({f:?}) left opening {k} (`{}`, at_tick {}) unplaced, but with \
                     grace {g} it was due by tick {} and the match ran to tick {stopped} \
                     (match over: {}); placed {got:?} of {want:?}",
                    o.building,
                    o.at_tick,
                    o.at_tick + g,
                    over(&app)
                );
            }
        }
        if fewest > 1 {
            placed_more_than_one.push(id.clone());
        }
    }
    assert!(
        !placed_more_than_one.is_empty(),
        "no multi-opening strategy placed more than one opening on both sides of its mirror: \
         the grace bound has made the claim vacuous"
    );
}

/// The rule itself: no `TrainArmy` is ever issued on a decision at which an
/// opening is due and not yet placed. Read off the journal of the three-line
/// fixture, with the opening ticks from the script.
#[test]
fn the_army_trains_nothing_while_an_opening_is_due() {
    let c = content();
    let c = with_openings(c, "mass_bulwark", three_lines("foundry"));
    let due: Vec<u32> = c
        .strategy("mass_bulwark")
        .unwrap()
        .barracks
        .iter()
        .map(|o| o.at_tick)
        .collect();
    let mut app = match_on(c, "mass_bulwark", "mass_sentinel", seed_at(0, 0));
    headless::tick(&mut app, 20_000);
    let journal = app.world().resource::<AiJournal>().for_faction(Faction::A);
    let placed: Vec<u32> = placements(&app, Faction::A).iter().map(|(t, _)| *t).collect();
    let mut trained = 0;
    for (t, a) in &journal {
        if !matches!(a, AiAction::TrainArmy { .. }) {
            continue;
        }
        trained += 1;
        for (k, at) in due.iter().enumerate() {
            if at <= t {
                assert!(
                    placed.get(k).is_some_and(|p| p <= t),
                    "trained at tick {t} while opening {k} (due at {at}) was unplaced \
                     (placements at {placed:?})"
                );
            }
        }
    }
    assert!(trained >= 5, "the fixture trained only {trained} units: vacuous");
}

/// An opening whose `at_tick` has not come reserves nothing: a script with a
/// second opening due only far in the future plays **tick for tick** as the
/// same script without it, until that tick. This is the reservation's
/// confinement in one fixture: it bites only when an opening is due.
#[test]
fn an_opening_not_yet_due_changes_nothing() {
    const HORIZON: u32 = 9_000;
    let one = with_openings(content(), "mass_bulwark", vec![opening("foundry", 300, 130.0)]);
    let later = with_openings(
        content(),
        "mass_bulwark",
        vec![opening("foundry", 300, 130.0), opening("foundry", 50_000, 165.0)],
    );
    let seed = seed_at(0, 2);
    let mut a = match_on(one, "mass_bulwark", "mass_ripper", seed);
    let mut b = match_on(later, "mass_bulwark", "mass_ripper", seed);
    for t in 0..HORIZON {
        headless::step(&mut a);
        headless::step(&mut b);
        assert_eq!(
            state_hash(a.world_mut()),
            state_hash(b.world_mut()),
            "a not-yet-due opening changed the match at tick {t}"
        );
    }
    assert_eq!(
        a.world().resource::<AiJournal>().0,
        b.world().resource::<AiJournal>().0,
        "a not-yet-due opening changed a decision"
    );
    let trained = a
        .world()
        .resource::<AiJournal>()
        .for_faction(Faction::A)
        .iter()
        .filter(|(_, x)| matches!(x, AiAction::TrainArmy { .. }))
        .count();
    assert!(trained >= 5, "the fixture trained only {trained} units: vacuous");
}

/// The reserve is held back from the **army only**, not from the tech walk: a
/// cheaper opening listed after a due, unaffordable one still goes up first
/// once the stockpile covers it — the skip rule B3.5 AC0b pinned — and the
/// reserved one follows, because the army is still waiting for it. Fixture: a
/// Sentinel army behind a Spire (200) due at 600 and a Gene-Vats (150) due at
/// 650; all three openings stand, the Gene-Vats before the Spire.
#[test]
fn a_cheaper_later_opening_still_goes_up_and_the_reserved_one_follows() {
    let c = content();
    let (foundry, spire, vats) = (
        c.building_index("foundry").unwrap(),
        c.building_index("aether_spire").unwrap(),
        c.building_index("gene_vats").unwrap(),
    );
    let c = with_openings(
        c,
        "mass_sentinel",
        vec![
            opening("foundry", 300, 130.0),
            opening("aether_spire", 600, 165.0),
            opening("gene_vats", 650, 200.0),
        ],
    );
    let mut app = match_on(c, "mass_sentinel", "mass_bulwark", seed_at(0, 3));
    let stopped = play_until(&mut app, |app| placements(app, Faction::A).len() >= 3);
    let got: Vec<usize> = placements(&app, Faction::A).iter().map(|(_, b)| *b).collect();
    assert_eq!(
        got,
        vec![foundry, vats, spire],
        "placed {got:?} by tick {stopped} (match over: {})",
        over(&app)
    );
}
