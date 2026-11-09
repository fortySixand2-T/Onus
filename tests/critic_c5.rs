//! Critic C5 — adversarial probes against the diplomacy milestone (critic
//! round 1, moved in unchanged in substance).

use std::path::PathBuf;

use onus::campaign::diplomacy::{Act, DiplomacyContent, Standing};
use onus::campaign::{BattleOutcome, BattlePlan, Campaign, CampaignContent, Decision, Side};
use onus::sim::content::Content;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data")
}
fn content() -> Content {
    Content::load_from_dir(&data_dir()).expect("content loads")
}
fn diplomatic() -> CampaignContent {
    CampaignContent::shipped_with_diplomacy(&content()).unwrap()
}
fn hold(p: &BattlePlan) -> BattleOutcome {
    BattleOutcome::unchanged(p, Some(Side::Defend))
}
fn by_numbers() -> impl FnMut(&BattlePlan) -> BattleOutcome {
    |p: &BattlePlan| {
        let a = p.attack.units.len();
        let d = p.defend.units.len() + p.buildings.len();
        BattleOutcome::unchanged(p, Some(if a > d { Side::Attack } else { Side::Defend }))
    }
}
fn court(id: &str) -> Decision {
    Decision::Court { nation: id.into() }
}

/// House courts and allies Aetherkin, then betrays it: Forgeborn (tolerance
/// 0) and the Reach (tolerance 0) now refuse every offer from House.
fn betrayer() -> Campaign {
    let mut c = Campaign::new(content(), diplomatic(), 3, Some("house")).unwrap();
    let h = c.nation_index("house").unwrap();
    let script: Vec<Vec<Decision>> = vec![
        vec![],
        vec![],
        vec![court("aetherkin")],
        vec![],
        vec![],
        vec![Decision::Ally { nation: "aetherkin".into() }],
        vec![Decision::Betray { nation: "aetherkin".into() }],
    ];
    for ds in &script {
        c.play_round(ds, &mut hold).unwrap_or_else(|e| panic!("round {}: {e}", c.round() + 1));
    }
    assert_eq!(c.betrayals(h), 1, "setup: House betrayed once");
    while c.nations()[h].treasury < 100 {
        c.play_round(&[], &mut hold).unwrap();
    }
    c
}

/// AC7: "A changed diplomatic decision diverges at its round." Two refused
/// offers to different nations leave identical hashes, so a save whose
/// decision was changed from one to the other loads without complaint, and
/// the loaded campaign's record of diplomacy differs from the one played.
#[test]
fn critic_a_changed_refused_offer_diverges_at_its_round() {
    let base = betrayer();
    let mut a = base.clone();
    let mut b = base.clone();
    let ra = a.play_round(&[court("forgeborn")], &mut hold).unwrap();
    let rb = b.play_round(&[court("ashen_reach")], &mut hold).unwrap();
    let h = a.nation_index("house").unwrap();
    let mine = |r: &onus::campaign::RoundReport| r.diplomacy.iter().find(|x| x.actor == h).copied().unwrap();
    assert!(!mine(&ra).accepted && !mine(&rb).accepted, "both offers refused");
    assert_ne!(mine(&ra).act, mine(&rb).act, "two different decisions");
    // Tamper the save: replace round N's Court forgeborn with Court ashen_reach.
    let mut log = a.log();
    let k = log.rounds.len() - 1;
    assert_eq!(log.rounds[k], vec![court("forgeborn")]);
    log.rounds[k] = vec![court("ashen_reach")];
    let resumed = Campaign::resume(content(), diplomatic(), &log, &mut hold);
    match resumed {
        Err(e) => assert!(e.contains(&format!("round {} diverged", k + 1)), "{e}"),
        Ok(r) => panic!(
            "a changed diplomatic decision (round {}: Court forgeborn -> Court ashen_reach) loaded \
             without diverging; the loaded record says {:?} where the played one says {:?}",
            k + 1,
            r.diplomacy_log().last(),
            a.diplomacy_log().last()
        ),
    }
}

/// Property sweep over AI-only diplomatic campaigns (and with a player
/// holding): cap of one ally, allies never fight, standings symmetric, only
/// living nations allied, the AI's betrayal only against its last rival, and
/// every AI offer accepted (it never makes one it knows is refused).
#[test]
fn critic_diplomatic_invariants_hold_every_round() {
    let mut seen_battles = 0;
    let mut seen_alliances = 0;
    let mut seen_betrayals = 0;
    for seed in 1..=12u64 {
        for player in [None, Some("house"), Some("tollmarch")] {
            let mut c = Campaign::new(content(), diplomatic(), seed, player).unwrap();
            let mut r = by_numbers();
            let n = c.nations().len();
            for _ in 0..60 {
                if c.is_over() {
                    break;
                }
                let alive_before = c.alive();
                let rep = c.play_round(&[], &mut r).unwrap();
                for b in &rep.battles {
                    seen_battles += 1;
                    assert_ne!(
                        c.standing(b.attacker, b.defender),
                        Some(Standing::Allied),
                        "seed {seed} {player:?} round {}: allies fought at city {}",
                        rep.round,
                        b.city
                    );
                }
                for x in 0..n {
                    assert!(c.allies_of(x).len() <= 1, "seed {seed}: {x} over the cap");
                    for y in 0..n {
                        if x != y {
                            assert_eq!(c.standing(x, y), c.standing(y, x), "symmetric");
                            if c.standing(x, y) == Some(Standing::Allied) {
                                assert!(c.nations()[x].alive && c.nations()[y].alive, "a dead ally");
                            }
                        }
                    }
                    if c.ally_of(x).is_some() {
                        seen_alliances += 1;
                    }
                }
                for rec in &rep.diplomacy {
                    assert!(Some(rec.actor) != c.player(), "the player held");
                    assert!(rec.accepted, "seed {seed}: the AI made an offer it knew would be refused: {rec:?}");
                    if let Act::Betray { ally } = rec.act {
                        seen_betrayals += 1;
                        let mut want = vec![rec.actor, ally];
                        want.sort();
                        assert_eq!(alive_before, want, "seed {seed}: betrayal before the ally was the last rival");
                    }
                }
            }
        }
    }
    eprintln!("battles {seen_battles}, alliance-rounds {seen_alliances}, betrayals {seen_betrayals}");
    assert!(seen_battles > 0 && seen_alliances > 0);
}

/// AC5: "Both armies bleed in auto-resolved battles." With House Tied to
/// Aetherkin and Verdant and incite free, an incited pair of AIs meets in
/// battle within the war.
#[test]
fn critic_an_incited_pair_fights_each_other() {
    let t = std::fs::read_to_string(data_dir().join("diplomacy.ron")).unwrap();
    let t = t.replacen(
        "(nation: \"house\", towards: [],",
        "(nation: \"house\", towards: [(\"aetherkin\", Tied), (\"verdant\", Tied)],",
        1,
    );
    let t = t.replacen("incite_cost: 200", "incite_cost: 0", 1);
    let c0 = content();
    let mut d = DiplomacyContent::from_ron(&t, &c0).unwrap();
    for k in 0..c0.nations.len() {
        d = d.with_passive(k);
    }
    let mut hits = 0;
    for seed in 1..=8u64 {
        let cc = CampaignContent::shipped(&c0).unwrap().with_diplomacy(d.clone());
        let mut c = Campaign::new(content(), cc, seed, Some("house")).unwrap();
        let (aeth, ver) = (c.nation_index("aetherkin").unwrap(), c.nation_index("verdant").unwrap());
        let mut r = by_numbers();
        c.play_round(&[Decision::Incite { a: "aetherkin".into(), b: "verdant".into() }], &mut r).unwrap();
        let mut met = false;
        for _ in 0..12 {
            if c.is_over() {
                break;
            }
            let rep = c.play_round(&[], &mut r).unwrap();
            met |= rep.battles.iter().any(|b| {
                (b.attacker == aeth && b.defender == ver) || (b.attacker == ver && b.defender == aeth)
            });
        }
        eprintln!("seed {seed}: incited pair met in battle: {met}");
        hits += met as u32;
    }
    assert!(hits > 0, "an incited pair never fought each other on 8 seeds");
}

/// AC5 "the inciter's army is never committed", for an AI inciter: inciting
/// never steers the inciter's own armies.
#[test]
fn critic_an_ai_inciter_is_never_steered_by_its_incite() {
    let mut incites = 0;
    for seed in 1..=64u64 {
      for player in [None, Some("house"), Some("tollmarch"), Some("verdant")] {
        let mut c = Campaign::new(content(), diplomatic(), seed, player).unwrap();
        let mut r = by_numbers();
        for _ in 0..60 {
            if c.is_over() {
                break;
            }
            let before: Vec<_> = (0..c.nations().len()).map(|k| c.war_target(k)).collect();
            let rep = c.play_round(&[], &mut r).unwrap();
            for rec in &rep.diplomacy {
                if let Act::Incite { .. } = rec.act {
                    // Steered by someone else this round (incited or asked)?
                    let steered_by_other = rep.diplomacy.iter().any(|o| {
                        o.accepted
                            && o.actor != rec.actor
                            && match o.act {
                                Act::Incite { a, b } => a == rec.actor || b == rec.actor,
                                Act::Ask { ally, .. } => ally == rec.actor,
                                _ => false,
                            }
                    });
                    if steered_by_other {
                        continue;
                    }
                    incites += 1;
                    let now = c.war_target(rec.actor);
                    if !(now.is_none() || now == before[rec.actor]) {
                        eprintln!("player {player:?} records: {:#?}", rep.diplomacy);
                    }
                    assert!(
                        now.is_none() || now == before[rec.actor],
                        "seed {seed} round {}: the inciter {} is steered after its incite: {now:?}",
                        rep.round,
                        rec.actor
                    );
                }
            }
        }
      }
    }
    eprintln!("AI incites seen: {incites}");
    assert!(incites > 0, "no AI incited on 64 seeds x 4 player settings");
}
