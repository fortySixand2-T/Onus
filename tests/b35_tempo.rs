//! L2 integration tests for **B3.5's tuning checkbox** — the *arc* of a match.
//!
//! B3's pentagon was first measured on a batch whose decided-match median was
//! ~1:16 against DESIGN_BRIEF's 5-8 minute target: a reading about openings,
//! not about a game. The tuning that followed is entirely in
//! `assets/data/*.ron` (F-029). This file pins what that tuning bought, so it
//! cannot silently regress:
//!
//!   - **length** — the decided-match median of a small, named batch falls
//!     inside the 5-8 minute band, and most of the batch is decided at all (a
//!     run that times out is not a long match, it is an unfinished one, F-020);
//!   - **density** — those minutes are spent *fighting*. A six-minute match
//!     between two units a side is not what the arc is for, so the same batch
//!     has to field a stated minimum of combat units per match. Length without
//!     density would be the failure mode of the abandoned first attempt
//!     (F-026: slower training bought minutes by shrinking the army).
//!
//! The batch is three strategies that span the roster's tempo — `rush` (the
//! all-in), `synth_triad` (the widest tech opening, four lines across all
//! three domains) and `turtle` (the long game) — on one seed, in both spawn
//! orientations: 3 x 3 x 2 = 18 matches. These rather than the five `mass_*`
//! probes on purpose: the probes are `b3_pentagon`'s instrument and are
//! already played there, and their heavy-armour pairs run past the runner's
//! cap under this tuning (F-029), which would make a length pin read a
//! timeout. `mvp` is not here either: it is the one script B3.5 left untuned
//! (B1 pins it number for number), so it is not a statement about the arc.
//!
//! Bounds, not golden numbers: this is a *property* of the content (the arc is
//! in the band, the fights are real), and every number asserted here is stated
//! with the measurement it came from. A tuning that moves a match from 5:08 to
//! 5:40 is fine; one that drops it to 1:16 or empties the field is not.

use onus::batch::{self, BatchSettings, MatchRecord};
use onus::headless::{self, SIM_HZ};
use onus::sim::content::Content;
use onus::sim::spatial::Faction;

/// The band DESIGN_BRIEF asks a match to land in, in ticks.
const BAND_LOW: u32 = 5 * 60 * SIM_HZ;
const BAND_HIGH: u32 = 8 * 60 * SIM_HZ;

/// The batch: the roster's three tempo poles, one seed, both orientations.
const POLES: [&str; 3] = ["rush", "synth_triad", "turtle"];
const MATCHES: usize = POLES.len() * POLES.len() * 2;

fn shipped() -> Content {
    headless::content().expect("assets/data/*.ron parse into sim structs")
}

/// Play the pinned batch. One seed (`BatchSettings::default()`'s seed base),
/// so the run is the same 18 matches every time.
fn poles_batch(content: &Content) -> Vec<MatchRecord> {
    let settings = BatchSettings::default()
        .with_only(POLES.iter().map(|s| s.to_string()).collect())
        .with_seeds(1);
    let records = batch::run_batch(content, &settings, &mut |_| {}).expect("shipped names");
    assert_eq!(records.len(), MATCHES, "3 strategies x 3 x 2 orientations");
    records
}

/// Combat units (`offense > 0`, the filter `ai::think` itself uses) built by
/// both sides in one match — the workers are the economy, not the fight.
fn combat_built(content: &Content, r: &MatchRecord) -> u32 {
    content
        .units
        .iter()
        .filter(|u| u.offense > 0)
        .map(|u| {
            r.produced.get(Faction::A, &u.id) + r.produced.get(Faction::B, &u.id)
        })
        .sum()
}

fn mmss(ticks: u32) -> String {
    let s = ticks / SIM_HZ;
    format!("{}:{:02}", s / 60, s % 60)
}

/// The decided-match median of the pole batch is inside the 5-8 minute band,
/// and the batch is decided: a capped match is undecided, never a long one.
///
/// Measured on the tuning this ships with (F-029): 18 of 18 decided, median
/// 5:42. The assertion is the band, not 5:42.
#[test]
fn the_decided_match_median_is_in_the_five_to_eight_minute_band() {
    let content = shipped();
    let records = poles_batch(&content);

    let mut decided: Vec<u32> = records
        .iter()
        .filter(|r| r.result.is_decided())
        .map(|r| r.ticks)
        .collect();
    decided.sort_unstable();
    assert!(
        decided.len() >= 15,
        "only {} of {MATCHES} matches were decided — the rest hit the cap, and an \
         unfinished match is not a long one",
        decided.len()
    );

    let median = decided[decided.len() / 2];
    assert!(
        (BAND_LOW..=BAND_HIGH).contains(&median),
        "decided-match median {} ({median} ticks) is outside the 5-8 minute band \
         [{BAND_LOW}, {BAND_HIGH}]; the whole batch was {:?}",
        mmss(median),
        decided.iter().map(|t| mmss(*t)).collect::<Vec<_>>()
    );
}

/// Those minutes are fights. Every match that *takes* minutes fields a real
/// army, and the batch as a whole averages one.
///
/// The per-match floor is asserted on matches of at least `LONG` (three
/// minutes) only, because a short match here is a legitimate reading and not a
/// thin one: the `rush` mirror is two all-ins meeting at the door and is
/// decided at 0:32 with four bodies. What must never happen is the F-026
/// failure — a *long* clock bought by shrinking the army.
///
/// Measured on the tuning this ships with (F-029): this batch averages 29
/// combat units per match (the whole 400-match roster batch reads 33), and the
/// thinnest match over three minutes fields 34. The floors below sit under
/// those readings with room for a later balance pass to move.
#[test]
fn the_matches_are_dense_enough_to_be_fights() {
    /// A match this long or longer has to have an army in it.
    const LONG: u32 = 3 * 60 * SIM_HZ;
    const MIN_PER_LONG_MATCH: u32 = 15;
    const MIN_MEAN: u32 = 20;

    let content = shipped();
    let records = poles_batch(&content);

    let per_match: Vec<u32> = records.iter().map(|r| combat_built(&content, r)).collect();
    println!(
        "combat units per match: {:?}",
        records
            .iter()
            .zip(&per_match)
            .map(|(r, n)| format!(
                "{} vs {} [{}] {} {n}",
                r.strategies[0],
                r.strategies[1],
                r.orientation.name(),
                mmss(r.ticks)
            ))
            .collect::<Vec<_>>()
    );
    for (r, built) in records.iter().zip(&per_match) {
        if r.ticks < LONG {
            continue;
        }
        assert!(
            *built >= MIN_PER_LONG_MATCH,
            "{} vs {} [{}] ran {} and fielded {built} combat units — a match that \
             long with an army that small is a clock, not a fight",
            r.strategies[0],
            r.strategies[1],
            r.orientation.name(),
            mmss(r.ticks),
        );
    }
    let mean = per_match.iter().sum::<u32>() / per_match.len() as u32;
    assert!(
        mean >= MIN_MEAN,
        "the batch averaged {mean} combat units per match, under the {MIN_MEAN} the \
         tuning was measured to hold: {per_match:?}"
    );
}
