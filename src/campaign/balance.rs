//! The campaign balance sim (C4 AC12): balance-as-backtesting one layer up.
//! Many AI-only campaigns are played to their end, and two things are read:
//! does **each nation win sometimes**, and does a campaign's **length** land
//! on the C0 target (`rules.length_target`, 15-25 rounds)?
//!
//! The readings (F-089, provisional):
//! - a campaign's length is the round it ended on; an undecided one (the
//!   round cap came first) counts at the cap, so it pulls the reading long,
//!   not out of it;
//! - "lands on the target" is the **median** length inside the target;
//! - "wins sometimes" is at least one won campaign per nation.
//!
//! Deterministic: a campaign is a pure function of its seed, and a batch
//! reports its campaigns in seed order however many threads played them.

use crate::sim::content::Content;

use super::battle::Resolver;
use super::content::CampaignContent;
use super::{Campaign, CampaignOutcome};

/// How one AI-only campaign ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CampaignResult {
    pub seed: u64,
    /// The winner (`None`: undecided at the round cap).
    pub winner: Option<usize>,
    /// The round it ended on.
    pub rounds: u32,
}

/// Play one AI-only campaign on `seed` to its end.
pub fn play_ai_campaign(
    content: &Content,
    campaign: &CampaignContent,
    seed: u64,
    resolver: &mut dyn Resolver,
) -> Result<CampaignResult, String> {
    let mut c = Campaign::new(content.clone(), campaign.clone(), seed, None).map_err(|e| e.to_string())?;
    while !c.is_over() {
        c.play_round(&[], resolver).map_err(|e| format!("seed {seed}: {e}"))?;
    }
    let winner = match c.outcome() {
        Some(CampaignOutcome::Won { nation }) => Some(nation),
        _ => None,
    };
    Ok(CampaignResult { seed, winner, rounds: c.round() })
}

/// A batch of AI-only campaigns, in seed order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BalanceReport {
    /// The nations, by id, in content order.
    pub nations: Vec<String>,
    pub results: Vec<CampaignResult>,
    /// The C0 length target and the round cap the batch was played under.
    pub min_rounds: u32,
    pub max_rounds: u32,
    pub round_cap: u32,
}

/// One campaign's result, or why it could not be played.
type Played = Result<CampaignResult, String>;

/// Play one campaign per seed. `threads` (at least 1) campaigns at a time,
/// each with its own resolver from `make`; the report is in seed order.
pub fn balance_batch<R, F>(
    content: &Content,
    campaign: &CampaignContent,
    seeds: &[u64],
    threads: usize,
    make: F,
) -> Result<BalanceReport, String>
where
    R: Resolver,
    F: Fn() -> R + Sync,
{
    let threads = threads.max(1);
    let mut slots: Vec<Option<Played>> = vec![None; seeds.len()];
    std::thread::scope(|s| {
        // Campaign `i` goes to worker `i % threads`: a fixed split.
        let mut parts: Vec<Vec<(usize, &mut Option<Played>)>> =
            (0..threads).map(|_| Vec::new()).collect();
        for (i, slot) in slots.iter_mut().enumerate() {
            parts[i % threads].push((i, slot));
        }
        for part in parts {
            let make = &make;
            s.spawn(move || {
                let mut r = make();
                for (i, slot) in part {
                    *slot = Some(play_ai_campaign(content, campaign, seeds[i], &mut r));
                }
            });
        }
    });
    let results = slots.into_iter().map(|r| r.expect("every campaign was played")).collect::<Result<_, _>>()?;
    let lt = &campaign.rules.length_target;
    Ok(BalanceReport {
        nations: content.nations.iter().map(|n| n.id.clone()).collect(),
        results,
        min_rounds: lt.min_rounds,
        max_rounds: lt.max_rounds,
        round_cap: campaign.rules.round_cap,
    })
}

impl BalanceReport {
    /// Campaigns won, per nation (content order).
    pub fn wins(&self) -> Vec<u32> {
        let mut w = vec![0; self.nations.len()];
        for r in &self.results {
            if let Some(n) = r.winner {
                w[n] += 1;
            }
        }
        w
    }

    /// Campaigns the round cap ended.
    pub fn undecided(&self) -> u32 {
        self.results.iter().filter(|r| r.winner.is_none()).count() as u32
    }

    /// The median length, in rounds (the lower middle of an even count).
    pub fn median_rounds(&self) -> Option<u32> {
        let mut l: Vec<u32> = self.results.iter().map(|r| r.rounds).collect();
        l.sort_unstable();
        l.get(l.len().saturating_sub(1) / 2).copied()
    }

    /// Campaigns whose length is inside the target.
    pub fn in_target(&self) -> u32 {
        self.results.iter().filter(|r| (self.min_rounds..=self.max_rounds).contains(&r.rounds)).count() as u32
    }

    /// AC12's two readings: `Ok` when every nation won at least once and the
    /// median length is inside the target; otherwise every miss, by name.
    pub fn verdict(&self) -> Result<(), String> {
        let mut miss = Vec::new();
        for (n, w) in self.nations.iter().zip(self.wins()) {
            if w == 0 {
                miss.push(format!("{n} never won"));
            }
        }
        match self.median_rounds() {
            Some(m) if (self.min_rounds..=self.max_rounds).contains(&m) => {}
            Some(m) => miss.push(format!(
                "median length {m} rounds is outside {}..={}",
                self.min_rounds, self.max_rounds
            )),
            None => miss.push("no campaign was played".to_string()),
        }
        if miss.is_empty() { Ok(()) } else { Err(miss.join("; ")) }
    }

    /// The reading, one line per nation, then the lengths.
    pub fn summary(&self) -> String {
        let n = self.results.len() as u32;
        let mut s = format!("{n} AI-only campaigns\n");
        for (id, w) in self.nations.iter().zip(self.wins()) {
            s += &format!("  {id:<12} won {w:>3} ({:.1}%)\n", 100.0 * w as f64 / n.max(1) as f64);
        }
        s += &format!("  undecided    {:>3} (round cap {})\n", self.undecided(), self.round_cap);
        let mut l: Vec<u32> = self.results.iter().map(|r| r.rounds).collect();
        l.sort_unstable();
        s += &format!(
            "  length: median {:?}, min {:?}, max {:?}; {} of {n} inside {}..={}\n",
            self.median_rounds(),
            l.first(),
            l.last(),
            self.in_target(),
            self.min_rounds,
            self.max_rounds
        );
        s
    }
}

// ---- C5 AC8: diplomacy balance ------------------------------------------------

use super::diplomacy::DiplomacyContent;

/// Which nations play diplomacy in one campaign of the diplomacy batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    /// Every nation diplomatic.
    AllEngaged,
    /// Every nation diplomatic but this one, which abstains.
    AllBut(usize),
    /// Every nation abstaining (C4's campaign, AC2).
    AllAbstaining,
    /// This nation alone takes diplomatic action; every other nation is
    /// passive (answers its offers by disposition, offers nothing). A
    /// nation alone in diplomacy needs someone to answer it.
    Only(usize),
}

/// A nation's part in a profile that one of its comparisons turns on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// It alone is diplomatic.
    With,
    /// It alone abstains.
    Without,
}

impl Profile {
    /// The batch's profiles for `n` nations, in play order: all engaged,
    /// each abstaining alone, all abstaining, each engaged alone.
    pub fn all(n: usize) -> Vec<Profile> {
        let mut v = vec![Profile::AllEngaged];
        v.extend((0..n).map(Profile::AllBut));
        v.push(Profile::AllAbstaining);
        v.extend((0..n).map(Profile::Only));
        v
    }

    /// `d` with this profile's levers: abstaining nations, and (under
    /// `Only`) passive ones.
    pub fn apply(self, mut d: DiplomacyContent) -> DiplomacyContent {
        for k in 0..d.nations() {
            d = match self {
                Profile::AllEngaged => d,
                Profile::AllBut(j) if k == j => d.with_abstaining(k),
                Profile::AllBut(_) => d,
                Profile::AllAbstaining => d.with_abstaining(k),
                Profile::Only(j) if k == j => d,
                Profile::Only(_) => d.with_passive(k),
            };
        }
        d
    }

    /// Nation `k`'s role, when the profile singles it out.
    pub fn role(self, k: usize) -> Option<Role> {
        match self {
            Profile::AllBut(j) if j == k => Some(Role::Without),
            Profile::Only(j) if j == k => Some(Role::With),
            _ => None,
        }
    }
}

/// How one campaign of the diplomacy batch went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiploCampaign {
    pub seed: u64,
    pub profile: Profile,
    /// The round each nation fell in (`None`: it stood at the end).
    pub fallen: Vec<Option<u32>>,
    /// The round it ended on.
    pub rounds: u32,
    /// Diplomatic actions taken (offers refused included).
    pub actions: usize,
}

impl DiploCampaign {
    /// Nation `k`'s placement: 1 + the nations that outlasted it (fell
    /// later, or stood at the end while it fell). Nations that fell in one
    /// round, or both stood, share a place.
    pub fn placement(&self, k: usize) -> u32 {
        let last = |f: Option<u32>| f.unwrap_or(u32::MAX);
        1 + self.fallen.iter().filter(|&&f| last(f) > last(self.fallen[k])).count() as u32
    }

    /// The finer measure (critic C5 r1): nation `k`'s placement, a shared
    /// place broken by the round it fell (later is better); lower is better.
    /// A nation that fell keeps its place; this only orders within it.
    pub fn finer(&self, k: usize) -> (u32, u32) {
        (self.placement(k), self.fallen[k].map_or(0, |r| u32::MAX - r))
    }
}

/// Play one AI-only campaign of the diplomacy batch on `seed` to its end:
/// `campaign` (without diplomacy) takes `diplomacy` under `profile`.
pub fn play_diplomacy_campaign(
    content: &Content,
    campaign: &CampaignContent,
    diplomacy: &DiplomacyContent,
    seed: u64,
    profile: Profile,
    resolver: &mut dyn Resolver,
) -> Result<DiploCampaign, String> {
    let cc = campaign.clone().with_diplomacy(profile.apply(diplomacy.clone()));
    let mut c = Campaign::new(content.clone(), cc, seed, None).map_err(|e| e.to_string())?;
    let mut fallen = vec![None; content.nations.len()];
    while !c.is_over() {
        c.play_round(&[], resolver).map_err(|e| format!("seed {seed} {profile:?}: {e}"))?;
        for (k, f) in fallen.iter_mut().enumerate() {
            if f.is_none() && !c.nations()[k].alive {
                *f = Some(c.round());
            }
        }
    }
    Ok(DiploCampaign {
        seed,
        profile,
        fallen,
        rounds: c.round(),
        actions: c.diplomacy_log().len(),
    })
}

/// One campaign of the batch, played or refused.
type DiploPlayed = Result<DiploCampaign, String>;
/// Whether a profile belongs to a summary group.
type IsGroup = fn(Profile) -> bool;

/// The diplomacy batch: every profile of every seed, seed-major, profiles in
/// [`Profile::all`] order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiplomacyReport {
    pub nations: Vec<String>,
    pub campaigns: Vec<DiploCampaign>,
}

/// Play every profile of every seed. `threads` (at least 1) campaigns at a
/// time, each worker with its own resolver from `make`; the report is in
/// seed-then-profile order.
pub fn diplomacy_batch<R, F>(
    content: &Content,
    campaign: &CampaignContent,
    diplomacy: &DiplomacyContent,
    seeds: &[u64],
    threads: usize,
    make: F,
) -> Result<DiplomacyReport, String>
where
    R: Resolver,
    F: Fn() -> R + Sync,
{
    let profiles = Profile::all(content.nations.len());
    let jobs: Vec<(u64, Profile)> = seeds.iter().flat_map(|&s| profiles.iter().map(move |&p| (s, p))).collect();
    let threads = threads.max(1);
    let mut slots: Vec<Option<DiploPlayed>> = vec![None; jobs.len()];
    std::thread::scope(|s| {
        let mut parts: Vec<Vec<(usize, &mut Option<DiploPlayed>)>> = (0..threads).map(|_| Vec::new()).collect();
        for (i, slot) in slots.iter_mut().enumerate() {
            parts[i % threads].push((i, slot));
        }
        for part in parts {
            let (make, jobs) = (&make, &jobs);
            s.spawn(move || {
                let mut r = make();
                for (i, slot) in part {
                    let (seed, p) = jobs[i];
                    *slot = Some(play_diplomacy_campaign(content, campaign, diplomacy, seed, p, &mut r));
                }
            });
        }
    });
    let campaigns = slots.into_iter().map(|r| r.expect("every campaign was played")).collect::<Result<_, _>>()?;
    Ok(DiplomacyReport {
        nations: content.nations.iter().map(|n| n.id.clone()).collect(),
        campaigns,
    })
}

/// One nation's comparisons: how often engaging placed it better, the same,
/// or worse than abstaining.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Comparison {
    pub better: u32,
    pub same: u32,
    pub worse: u32,
}

impl Comparison {
    fn add<T: Ord>(&mut self, with: T, without: T) {
        match with.cmp(&without) {
            std::cmp::Ordering::Less => self.better += 1,
            std::cmp::Ordering::Equal => self.same += 1,
            std::cmp::Ordering::Greater => self.worse += 1,
        }
    }

    fn plus(self, o: Comparison) -> Comparison {
        Comparison { better: self.better + o.better, same: self.same + o.same, worse: self.worse + o.worse }
    }

    /// What diplomacy is for the nation, by AC8's reading.
    pub fn class(&self) -> &'static str {
        match (self.better > 0, self.worse > 0) {
            (true, false) => "dominant",
            (true, true) => "a choice",
            (false, true) => "only a cost",
            (false, false) => "no effect",
        }
    }
}

impl DiplomacyReport {
    fn find(&self, seed: u64, p: Profile) -> &DiploCampaign {
        self.campaigns
            .iter()
            .find(|c| c.seed == seed && c.profile == p)
            .expect("every profile of every seed is played")
    }

    /// The seeds, in order.
    pub fn seeds(&self) -> Vec<u64> {
        let mut s: Vec<u64> = self.campaigns.iter().map(|c| c.seed).collect();
        s.dedup();
        s
    }

    /// Nation `k`'s comparisons: among diplomats (all engaged against it
    /// alone abstaining) and among abstainers (it alone engaged against
    /// all abstaining), per seed.
    pub fn comparisons(&self, k: usize) -> [Comparison; 2] {
        let mut out = [Comparison::default(); 2];
        for seed in self.seeds() {
            let p = |pr| self.find(seed, pr).placement(k);
            out[0].add(p(Profile::AllEngaged), p(Profile::AllBut(k)));
            out[1].add(p(Profile::Only(k)), p(Profile::AllAbstaining));
        }
        out
    }

    /// Nation `k`'s comparisons by the finer measure
    /// ([`DiploCampaign::finer`]), in the same two contexts.
    pub fn finer_comparisons(&self, k: usize) -> [Comparison; 2] {
        let mut out = [Comparison::default(); 2];
        for seed in self.seeds() {
            let p = |pr| self.find(seed, pr).finer(k);
            out[0].add(p(Profile::AllEngaged), p(Profile::AllBut(k)));
            out[1].add(p(Profile::Only(k)), p(Profile::AllAbstaining));
        }
        out
    }

    /// Both contexts together.
    pub fn comparison(&self, k: usize) -> Comparison {
        let [a, b] = self.comparisons(k);
        a.plus(b)
    }

    /// Both contexts together, by the finer measure.
    pub fn finer_comparison(&self, k: usize) -> Comparison {
        let [a, b] = self.finer_comparisons(k);
        a.plus(b)
    }

    /// AC8 (as read after critic C5 r1, provisional): `Ok` when diplomacy is
    /// dominant for no nation and is a choice (sometimes better, sometimes
    /// worse) for at least one; otherwise every miss, by name. Gated on
    /// placement, as AC8 states it.
    pub fn verdict(&self) -> Result<(), String> {
        let mut miss = Vec::new();
        let all: Vec<Comparison> = (0..self.nations.len()).map(|k| self.comparison(k)).collect();
        if all.iter().all(|c| c.better == 0 && c.worse == 0) {
            miss.push("diplomacy changes no outcome".to_string());
        } else if !all.iter().any(|c| c.class() == "a choice") {
            miss.push("no nation has a choice: diplomacy is never better for a nation in one campaign and worse in another".to_string());
        }
        for (id, c) in self.nations.iter().zip(&all) {
            if c.class() == "dominant" {
                miss.push(format!(
                    "{id}: diplomacy is dominant (better in {}, never worse, of {})",
                    c.better,
                    c.better + c.same
                ));
            }
        }
        if miss.is_empty() { Ok(()) } else { Err(miss.join("; ")) }
    }

    /// The reading: per nation, its comparisons in each context and what
    /// diplomacy is for it; then how varied the seeds were.
    pub fn summary(&self) -> String {
        let seeds = self.seeds();
        let mut s = format!(
            "{} seeds x {} profiles = {} AI-only campaigns\n",
            seeds.len(),
            Profile::all(self.nations.len()).len(),
            self.campaigns.len()
        );
        s += "  by placement (gated) | finer: placement, then the round it fell (reported)\n";
        s += "  nation        among diplomats (b/s/w)  among abstainers (b/s/w)  diplomacy is  | finer diplomats  finer abstainers  finer is\n";
        for (k, id) in self.nations.iter().enumerate() {
            let [a, b] = self.comparisons(k);
            let [fa, fb] = self.finer_comparisons(k);
            s += &format!(
                "  {id:<12}  {:>3}/{:>3}/{:>3}              {:>3}/{:>3}/{:>3}               {:<12}  | {:>3}/{:>3}/{:>3}      {:>3}/{:>3}/{:>3}       {}\n",
                a.better,
                a.same,
                a.worse,
                b.better,
                b.same,
                b.worse,
                self.comparison(k).class(),
                fa.better,
                fa.same,
                fa.worse,
                fb.better,
                fb.same,
                fb.worse,
                self.finer_comparison(k).class()
            );
        }
        let groups: [(&str, IsGroup); 4] = [
            ("all engaged", |p| p == Profile::AllEngaged),
            ("one abstaining", |p| matches!(p, Profile::AllBut(_))),
            ("all abstaining", |p| p == Profile::AllAbstaining),
            ("one engaged", |p| matches!(p, Profile::Only(_))),
        ];
        for (label, is) in groups {
            let cs: Vec<&DiploCampaign> = self.campaigns.iter().filter(|c| is(c.profile)).collect();
            let mut outcomes: Vec<&Vec<Option<u32>>> = cs.iter().map(|c| &c.fallen).collect();
            outcomes.sort();
            outcomes.dedup();
            let actions: usize = cs.iter().map(|c| c.actions).sum();
            let mut wins = vec![0u32; self.nations.len()];
            for c in &cs {
                for (k, f) in c.fallen.iter().enumerate() {
                    if f.is_none() && c.fallen.iter().filter(|g| g.is_none()).count() == 1 {
                        wins[k] += 1;
                    }
                }
            }
            s += &format!(
                "  {label}: {} distinct outcome(s) in {} campaigns; {actions} diplomatic actions; wins {:?}\n",
                outcomes.len(),
                cs.len(),
                self.nations.iter().zip(&wins).filter(|(_, &w)| w > 0).collect::<Vec<_>>()
            );
        }
        s
    }
}
