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
//! all-in, which commits on its first body), `mvp` (the default opener, the
//! one script B3.5 left untuned because B1 pins it number for number) and
//! `mass_bulwark` (the slowest, most expensive army in the set) — on one seed,
//! in both spawn orientations: 3 x 3 x 2 = 18 matches. `mass_bulwark` is in
//! deliberately: it is the matchup class that used to be a 2-damage-a-hit
//! grind and was *undecidable* inside the old cap (F-029), so if that ever
//! returns this test reads it as a timeout or a runaway median rather than
//! letting it hide in a batch average.
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
const POLES: [&str; 3] = ["mass_bulwark", "mvp", "rush"];
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
/// Measured on the tuning this ships with (F-030): 18 of 18 decided, median
/// 6:22, 10 of the 18 inside the band. The assertion is the band, not 6:22 —
/// and "decided" is now a real statement, because the cap is a 15-minute
/// backstop rather than the top of the band it is measuring.
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
/// Measured on the tuning this ships with (F-030): this batch averages 22
/// combat units per match, and the thinnest match over three minutes fields
/// 15. A separate 18-match probe of named matchups reads 37 built and **30
/// casualties** per match — the number that matters most here, because the
/// tuning this replaced built 33 units a match and lost 9.9 of them: armies
/// that assemble and never trade. The floors below sit under these readings
/// with room for a later balance pass to move.
#[test]
fn the_matches_are_dense_enough_to_be_fights() {
    /// A match this long or longer has to have an army in it.
    const LONG: u32 = 3 * 60 * SIM_HZ;
    const MIN_PER_LONG_MATCH: u32 = 12;
    const MIN_MEAN: u32 = 18;

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
