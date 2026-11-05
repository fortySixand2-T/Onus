//! L2 integration tests for **B3 AC (harden `batch::production_totals`)**.
//!
//! `production_totals` used to take its column header from `records.first()`
//! and read every later record through it. A batch whose first record carried
//! an **unlabelled** production block (`ProductionCounts::default()`, what a
//! synthetic record carries) therefore reported *nothing* for the whole batch:
//! every later row's production was silently dropped under an empty header.
//! The same rule dropped any unit a later record named and the first did not.
//!
//! The fix (F-037): the schema is the **union of every record's unit ids**, in
//! order of first appearance (record order, then header order within a record),
//! built by a linear scan — never a map. An unlabelled block contributes no
//! column and no count, which is lossless: `ProductionCounts`'s fields are
//! private, and its only unlabelled value is the empty `Default`, so an
//! unlabelled block cannot carry a count to lose.

use onus::batch::{self, MatchRecord, ProductionCounts};
use onus::headless::{self, MatchSettings};
use onus::sim::content::Content;
use onus::sim::spatial::Faction;

fn content() -> Content {
    headless::content().expect("assets/data/*.ron parse into sim structs")
}

/// A short real match, so the production block is a genuine labelled snapshot.
fn played(c: &Content) -> MatchRecord {
    let r = batch::run_match(
        c,
        &MatchSettings::default()
            .with_strategies("rush", "mass_ripper")
            .with_tick_cap(1_500),
    )
    .expect("shipped names");
    assert!(
        r.produced.total(Faction::A) + r.produced.total(Faction::B) > 0,
        "the fixture match must build something, or the test proves nothing"
    );
    r
}

fn total_of(totals: &[(String, u32)]) -> u32 {
    totals.iter().map(|(_, n)| n).sum()
}

fn ids_of(totals: &[(String, u32)]) -> Vec<&str> {
    totals.iter().map(|(id, _)| id.as_str()).collect()
}

/// **The pinned bug.** An unlabelled record *first* used to blank the whole
/// batch: the header came from it (empty), so the played record's production
/// was read through zero columns and vanished. Order must not decide whether
/// production is counted.
#[test]
fn an_unlabelled_first_record_no_longer_drops_the_batch() {
    let c = content();
    let real = played(&c);
    let unlabelled = MatchRecord {
        produced: ProductionCounts::default(),
        ..real.clone()
    };
    let want = batch::production_totals(std::slice::from_ref(&real));
    let expected_ids: Vec<&str> = c.units.iter().map(|u| u.id.as_str()).collect();
    assert_eq!(ids_of(&want), expected_ids);

    let first = batch::production_totals(&[unlabelled.clone(), real.clone()]);
    assert_eq!(
        first, want,
        "an unlabelled record first dropped every later row's production"
    );
    let last = batch::production_totals(&[real.clone(), unlabelled.clone()]);
    assert_eq!(last, want, "an unlabelled record last changed the totals");
    assert_eq!(
        total_of(&first),
        real.produced.total(Faction::A) + real.produced.total(Faction::B)
    );
}

/// A batch of nothing but unlabelled records has no schema: no columns, not a
/// row of zeros under invented names.
#[test]
fn only_unlabelled_records_total_nothing() {
    let c = content();
    let unlabelled = MatchRecord {
        produced: ProductionCounts::default(),
        ..played(&c)
    };
    assert_eq!(batch::production_totals(&[unlabelled.clone(), unlabelled]), Vec::new());
}

/// The schema is the **union** of the headers. A later record naming a unit
/// the first does not is counted under that name, appended in first-appearance
/// order, instead of being silently dropped.
#[test]
fn the_schema_is_the_union_of_every_records_unit_ids() {
    let shipped = content();
    let mut extended = shipped.clone();
    let mut phantom = extended.units[0].clone();
    phantom.id = "phantom".to_string();
    extended.units.push(phantom);

    let a = played(&shipped);
    let b = played(&extended);
    assert_eq!(b.produced.unit_ids().last().map(String::as_str), Some("phantom"));

    let totals = batch::production_totals(&[a.clone(), b.clone()]);
    let mut want: Vec<&str> = shipped.units.iter().map(|u| u.id.as_str()).collect();
    want.push("phantom");
    assert_eq!(ids_of(&totals), want, "first-appearance order, union of headers");
    for (id, n) in &totals {
        let by_hand: u32 = [&a, &b]
            .iter()
            .map(|r| r.produced.get(Faction::A, id) + r.produced.get(Faction::B, id))
            .sum();
        assert_eq!(*n, by_hand, "{id}: totals are the records summed by name");
    }
    assert_eq!(
        total_of(&totals),
        [&a, &b]
            .iter()
            .map(|r| r.produced.total(Faction::A) + r.produced.total(Faction::B))
            .sum::<u32>(),
        "no record's production was dropped"
    );

    // The other way round, the extended header leads and fixes the order.
    let swapped = batch::production_totals(&[b, a]);
    let ext_ids: Vec<&str> = extended.units.iter().map(|u| u.id.as_str()).collect();
    assert_eq!(ids_of(&swapped), ext_ids);
    assert_eq!(total_of(&swapped), total_of(&totals));
}
