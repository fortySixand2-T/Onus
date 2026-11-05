//! C2 AC3 — **the batch runner has a nation axis**. Mirror matchups (a
//! nation against itself) keep isolating unit balance; cross-nation matchups
//! (the same strategy on both sides, different nations) test faction balance;
//! personality matchups play each nation's own script. Same discipline as the
//! strategy batch: content order, filters never reorder, both orientations,
//! deterministic, and every row is the match it says it is.

use onus::batch::{
    self, BatchSettings, Fixture, MatchRecord, NationAxis, NationBatchError, NationRecord,
};
use onus::headless::{self, MatchSettings, Orientation};
use onus::metrics::WinMatrix;
use onus::sim::content::Content;

fn content() -> Content {
    headless::content().expect("content loads")
}

fn names(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

fn pair(a: &str, b: &str) -> [String; 2] {
    [a.to_string(), b.to_string()]
}

fn nation_ids(c: &Content) -> Vec<String> {
    c.nations.iter().map(|n| n.id.clone()).collect()
}

#[test]
fn the_nation_roster_is_content_order_and_a_filter_never_reorders() {
    let c = content();
    assert_eq!(batch::nation_roster(&c, None).unwrap(), nation_ids(&c));
    let a = batch::nation_roster(&c, Some(&names(&["tollmarch", "house"]))).unwrap();
    let b = batch::nation_roster(&c, Some(&names(&["house", "tollmarch"]))).unwrap();
    assert_eq!(a, names(&["house", "tollmarch"]));
    assert_eq!(a, b);
    assert_eq!(
        batch::nation_roster(&c, Some(&names(&["house", "atlantis"]))),
        Err(NationBatchError::Nation("atlantis".into()))
    );
}

#[test]
fn mirror_fixtures_hold_one_nation_and_every_strategy_pair() {
    let c = content();
    let nations = names(&["house", "verdant"]);
    let strategies = names(&["mvp", "rush", "turtle"]);
    let f = batch::nation_fixtures(&c, NationAxis::Mirror, &nations, &strategies);
    assert_eq!(f.len(), 2 * 3 * 3);
    assert!(f.iter().all(|x| x.sides[0].nation == x.sides[1].nation));
    // Nation-major, then the strategy matrix row-major.
    assert_eq!(f[0], Fixture::new(("house", "mvp"), ("house", "mvp")));
    assert_eq!(f[1], Fixture::new(("house", "mvp"), ("house", "rush")));
    assert_eq!(f[9], Fixture::new(("verdant", "mvp"), ("verdant", "mvp")));
}

#[test]
fn cross_fixtures_hold_one_strategy_and_two_different_nations() {
    let c = content();
    let nations = nation_ids(&c);
    let strategies = names(&["mass_ripper", "turtle"]);
    let f = batch::nation_fixtures(&c, NationAxis::Cross, &nations, &strategies);
    let n = nations.len();
    assert_eq!(f.len(), n * (n - 1) * 2);
    assert!(f.iter().all(|x| x.sides[0].strategy == x.sides[1].strategy));
    assert!(f.iter().all(|x| x.sides[0].nation != x.sides[1].nation));
    // Every ordered pair of distinct nations appears once per strategy.
    for a in &nations {
        for b in nations.iter().filter(|b| *b != a) {
            for s in &strategies {
                let want = Fixture::new((a, s), (b, s));
                assert_eq!(
                    f.iter().filter(|x| **x == want).count(),
                    1,
                    "{a} vs {b} on {s}"
                );
            }
        }
    }
    assert_eq!(
        f[0],
        Fixture::new(("house", "mass_ripper"), ("forgeborn", "mass_ripper"))
    );
}

#[test]
fn personality_fixtures_play_each_nations_own_script() {
    let c = content();
    let nations = nation_ids(&c);
    let f = batch::nation_fixtures(&c, NationAxis::Personality, &nations, &[]);
    let n = nations.len();
    assert_eq!(f.len(), n * (n - 1));
    for x in &f {
        for side in &x.sides {
            let def = c.nations.iter().find(|d| d.id == side.nation).unwrap();
            assert_eq!(
                side.strategy, def.strategy,
                "{} plays its personality",
                side.nation
            );
        }
        assert_ne!(x.sides[0].nation, x.sides[1].nation);
    }
}

#[test]
fn the_axis_names_round_trip() {
    for a in NationAxis::ALL {
        assert_eq!(NationAxis::parse(a.name()), Some(a));
    }
    assert_eq!(NationAxis::parse("both"), None);
}

#[test]
fn an_unknown_name_is_refused_before_any_match_is_played() {
    let c = content();
    let mut played = 0;
    let bad_nation = [Fixture::new(("house", "mvp"), ("atlantis", "mvp"))];
    let err = batch::run_fixtures(&c, &bad_nation, &BatchSettings::default(), &mut |_| {
        played += 1
    });
    assert_eq!(err, Err(NationBatchError::Nation("atlantis".into())));
    let bad_strategy = [Fixture::new(("house", "mvp"), ("house", "blitz"))];
    let err = batch::run_fixtures(&c, &bad_strategy, &BatchSettings::default(), &mut |_| {
        played += 1
    });
    assert!(matches!(err, Err(NationBatchError::Strategy(_))));
    assert!(batch::run_nation_batch(
        &c,
        &BatchSettings::default().with_only(names(&["blitz"])),
        None,
        NationAxis::Cross,
        &mut |_| played += 1,
    )
    .is_err());
    assert_eq!(played, 0);
}

/// A small cross batch: the record count and order, that each row is the
/// match it names (re-played directly through `headless`), and that a re-run
/// is identical.
#[test]
fn a_cross_batch_plays_every_fixture_in_both_orientations_faithfully() {
    let c = content();
    let settings = BatchSettings::default()
        .with_seeds(2)
        .with_tick_cap(600)
        .with_only(names(&["rush"]));
    let nations = names(&["forgeborn", "tollmarch"]);
    let mut seen = 0;
    let records = batch::run_nation_batch(
        &c,
        &settings,
        Some(&nations),
        NationAxis::Cross,
        &mut |_| seen += 1,
    )
    .unwrap();
    // 2 ordered pairs x 1 strategy x 2 seeds x 2 orientations.
    assert_eq!(records.len(), 8);
    assert_eq!(seen, 8);
    let first = &records[0];
    assert_eq!(first.nations, pair("forgeborn", "tollmarch"));
    assert_eq!(first.record.orientation, Orientation::ALL[0]);
    assert_eq!(records[1].record.orientation, Orientation::ALL[1]);
    assert_eq!(records[2].nations, pair("tollmarch", "forgeborn"));
    assert_eq!(records[4].record.seed, batch::seed_at(0, 1));
    for r in &records {
        assert!(!r.is_mirror());
        assert_eq!(r.record.strategies, pair("rush", "rush"));
        let direct = batch::run_match(
            &c,
            &MatchSettings::default()
                .with_seed(r.record.seed)
                .with_strategies("rush", "rush")
                .with_nations(Some(&r.nations[0]), Some(&r.nations[1]))
                .with_tick_cap(600)
                .with_orientation(r.record.orientation),
        )
        .unwrap();
        assert_eq!(r.record, direct, "the row is the match it names");
    }
    let again = batch::run_nation_batch(
        &c,
        &settings,
        Some(&nations),
        NationAxis::Cross,
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(records, again, "a nation batch is deterministic");
}

/// The traits are really in play on the cross axis: Verdant's gene vats
/// train faster, so on the same rush script its side has trained more by the
/// cap than the traitless House; in the House mirror the slots train alike.
#[test]
fn cross_matches_differ_from_their_mirror_by_the_traits() {
    use onus::sim::spatial::Faction::{A, B};
    let c = content();
    let settings = BatchSettings::default()
        .with_tick_cap(6_000)
        .with_only(names(&["rush"]));
    let nations = names(&["house", "verdant"]);
    let cross = batch::run_nation_batch(
        &c,
        &settings,
        Some(&nations),
        NationAxis::Cross,
        &mut |_| {},
    )
    .unwrap();
    let mirror = batch::run_nation_batch(
        &c,
        &settings,
        Some(&nations),
        NationAxis::Mirror,
        &mut |_| {},
    )
    .unwrap();
    assert!(mirror.iter().all(NationRecord::is_mirror));
    let built = |rec: &MatchRecord, f| rec.produced.get(f, "ripper");
    // Normal orientation, House in slot A and Verdant in slot B.
    let r = &cross[0];
    assert_eq!(r.nations, pair("house", "verdant"));
    let m = &mirror[0];
    assert_eq!(m.nations, pair("house", "house"));
    eprintln!(
        "cross: house {} verdant {}; house mirror: {} {}",
        built(&r.record, A),
        built(&r.record, B),
        built(&m.record, A),
        built(&m.record, B)
    );
    assert!(built(&r.record, B) > built(&r.record, A));
    assert!(
        built(&r.record, B) > built(&m.record, B),
        "Verdant out-trains House in slot B"
    );
}

/// The traitless House mirror is the plain strategy batch, record for record:
/// the nation axis adds nations, never noise.
#[test]
fn the_house_mirror_is_the_plain_batch() {
    let c = content();
    let only = names(&["mvp", "rush"]);
    let settings = BatchSettings::default()
        .with_tick_cap(3_000)
        .with_only(only);
    let plain = batch::run_batch(&c, &settings, &mut |_| {}).unwrap();
    let house = batch::run_nation_batch(
        &c,
        &settings,
        Some(&names(&["house"])),
        NationAxis::Mirror,
        &mut |_| {},
    )
    .unwrap();
    let house: Vec<MatchRecord> = house.into_iter().map(|r| r.record).collect();
    assert_eq!(house, plain);
}

/// Relabelled by nation, a cross batch reads as a nation win matrix through
/// the same arithmetic as the strategy matrix.
#[test]
fn a_nation_matrix_is_read_off_the_relabelled_records() {
    let c = content();
    let settings = BatchSettings::default()
        .with_tick_cap(300)
        .with_only(names(&["rush"]));
    let nations = names(&["verdant", "aetherkin", "house"]);
    let records = batch::run_nation_batch(
        &c,
        &settings,
        Some(&nations),
        NationAxis::Cross,
        &mut |_| {},
    )
    .unwrap();
    let relabelled: Vec<MatchRecord> = records.iter().map(NationRecord::by_nation).collect();
    let m = WinMatrix::of(&relabelled);
    // First appearance: house (content order first) leads.
    assert_eq!(
        m.ids(),
        names(&["house", "verdant", "aetherkin"]).as_slice()
    );
    for (r, l) in records.iter().zip(&relabelled) {
        assert_eq!(l.strategies, r.nations);
        assert_eq!(l.result, r.record.result);
        assert_eq!(l.seed, r.record.seed);
    }
}

// ---- the `balance` binary ----------------------------------------------------

fn balance(args: &[&str]) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_balance"))
        .args(args)
        .output()
        .expect("binary runs")
}

#[test]
fn the_binary_plays_the_nation_axis_and_prints_the_nation_matrix() {
    let out = balance(&[
        "--nations",
        "cross",
        "--only-nations",
        "verdant,house",
        "--only",
        "rush",
        "--tick-cap",
        "60",
    ]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    // 2 ordered pairs x 1 strategy x 1 seed x 2 orientations.
    assert!(stderr.contains("= 4 matches"), "{stderr}");
    assert!(stdout.contains("axis cross, 4 matches"), "{stdout}");
    assert!(stdout.contains("nation matrix"), "{stdout}");
    assert!(stdout.contains("[0] house"), "content order: {stdout}");
    assert!(stdout.contains("[1] verdant"), "{stdout}");
}

#[test]
fn the_binary_refuses_a_bad_nation_request() {
    for args in [
        &["--nations", "sideways"][..],
        &[
            "--nations",
            "cross",
            "--only-nations",
            "atlantis",
            "--tick-cap",
            "1",
        ],
        &["--only-nations", "house"],
        &["--nations", "mirror", "--report", "x.ron"],
    ] {
        let out = balance(args);
        assert!(!out.status.success(), "{args:?} was accepted");
        assert!(
            !String::from_utf8_lossy(&out.stderr).is_empty(),
            "{args:?}: silent refusal"
        );
    }
}
