//! The campaign client (C6): a thin Bevy driver over the render-free
//! campaign (`campaign`). This module is its **logic**: the screens (Bevy
//! states), the session (the campaign, the story's progress, what the
//! player has staged), the keys, and the text each screen shows. The view
//! ([`crate::campaign_view`]) only draws what this module holds, so the
//! whole client runs headless under `MinimalPlugins` in the tests.
//!
//! The client never changes the campaign directly: the keys stage the same
//! `Decision`s the campaign already consumes, and ending the turn plays the
//! round on them (`Campaign::step_round`). A refused round changes nothing
//! and shows why. So a save is still the seed plus the decision log.
//!
//! `cargo run -- campaign [seed]` opens it ([`campaign_args`]); `cargo run`
//! is the skirmish, untouched.

use std::path::{Path, PathBuf};

use bevy::ecs::schedule::ScheduleLabel;
use bevy::prelude::*;
use bevy::state::app::StatesPlugin;
use serde::Deserialize;

use crate::campaign::diplomacy::{Act, DiplomacyRecord};
use crate::campaign::story::{Story, StoryProgress};
use crate::campaign::{
    BattleOutcome, BattlePlan, Campaign, CampaignContent, CampaignOutcome, Decision, HeadlessResolver, Location, Memo,
    PlayedBattle, Resolver, RoundReport, RoundStep, Side,
};
use crate::client::nation_name;
use crate::sim::content::Content;

/// The seed `cargo run -- campaign` plays when none is given.
pub const CAMPAIGN_SEED: u64 = 20_261_110;

/// The map layout's file name in the data directory.
pub const LAYOUT_FILE: &str = "map_layout.ron";

/// The campaign client's screens.
#[derive(States, Debug, Clone, PartialEq, Eq, Hash, Default)]
pub enum Screen {
    /// A story beat (the Rupture opens the campaign).
    #[default]
    Story,
    /// The campaign map: cities, roads, armies; orders are staged here.
    Map,
    /// The round's diplomatic action.
    Diplomacy,
    /// A battle of the player's, played live.
    Battle,
    /// The campaign is over.
    Over,
}

/// `cargo run -- campaign [seed]`: `Some(seed)` opens the campaign client
/// (on [`CAMPAIGN_SEED`] unless a numeric seed follows), `None` the
/// skirmish. `args` are the process arguments, the program name first.
pub fn campaign_args(args: impl IntoIterator<Item = String>) -> Option<u64> {
    let mut args = args.into_iter().skip(1);
    if args.next().as_deref() != Some("campaign") {
        return None;
    }
    Some(args.next().and_then(|s| s.parse().ok()).unwrap_or(CAMPAIGN_SEED))
}

// ---- the map layout -------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlaceDef {
    id: String,
    x: f32,
    y: f32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LayoutFile {
    cities: Vec<PlaceDef>,
}

/// Where each campaign city is drawn (`assets/data/map_layout.ron`),
/// validated against the campaign map. Presentation only.
#[derive(Debug, Clone, PartialEq)]
pub struct MapLayout {
    positions: Vec<Vec2>,
}

impl MapLayout {
    /// Load `map_layout.ron` from a data directory, validated.
    pub fn load_from_dir(dir: &Path, campaign: &CampaignContent) -> Result<Self, String> {
        let path = dir.join(LAYOUT_FILE);
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("map layout: read {}: {e}", path.display()))?;
        Self::from_ron(&text, campaign)
    }

    /// The shipped layout, from the crate's own `assets/data`.
    pub fn shipped(campaign: &CampaignContent) -> Result<Self, String> {
        Self::load_from_dir(&data_dir(), campaign)
    }

    /// Parse and validate layout RON: every city of the campaign map exactly
    /// once, no unknown city, finite coordinates, no two in the same place.
    pub fn from_ron(text: &str, campaign: &CampaignContent) -> Result<Self, String> {
        let f: LayoutFile = ron::from_str(text).map_err(|e| format!("map layout: parse: {e}"))?;
        let mut pos: Vec<Option<Vec2>> = vec![None; campaign.cities.len()];
        for p in f.cities {
            let Some(i) = campaign.city_index(&p.id) else {
                return Err(format!("map layout: unknown city `{}`", p.id));
            };
            if pos[i].is_some() {
                return Err(format!("map layout: city `{}` is placed twice", p.id));
            }
            if !p.x.is_finite() || !p.y.is_finite() {
                return Err(format!("map layout: city `{}` is not at a finite position", p.id));
            }
            let v = Vec2::new(p.x, p.y);
            if let Some(j) = pos.iter().position(|q| *q == Some(v)) {
                return Err(format!(
                    "map layout: `{}` and `{}` are in the same place",
                    campaign.cities[j].id, p.id
                ));
            }
            pos[i] = Some(v);
        }
        let mut positions = Vec::with_capacity(pos.len());
        for (i, p) in pos.into_iter().enumerate() {
            match p {
                Some(v) => positions.push(v),
                None => return Err(format!("map layout: city `{}` has no position", campaign.cities[i].id)),
            }
        }
        Ok(MapLayout { positions })
    }

    /// Per campaign city (by index), where it is drawn.
    pub fn positions(&self) -> &[Vec2] {
        &self.positions
    }
}

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/data")
}

// ---- the session ----------------------------------------------------------

/// The resolver of the AI's battles, boxed (the shipped one plays the
/// headless battle sim; a test scripts one).
pub struct DynResolver(pub Box<dyn Resolver + Send + Sync>);

impl Resolver for DynResolver {
    fn resolve(
        &mut self,
        content: &Content,
        campaign: &CampaignContent,
        plan: &BattlePlan,
    ) -> BattleOutcome {
        self.0.resolve(content, campaign, plan)
    }
}

/// What the map's keys point at.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MapCursor {
    /// The selected army of the player's (standing at a city).
    pub army: Option<u32>,
    /// The selected neighbour of that army's city: an index into its roads,
    /// in `campaign.ron` order.
    pub target: Option<usize>,
    /// The selected city of the player's (where `R` raises a General).
    pub city: Option<usize>,
    /// The diplomacy screen's selected rival (a nation index).
    pub rival: Option<usize>,
    /// The first rival of an incitement, waiting for the second.
    pub incite: Option<usize>,
}

/// The campaign being played, and the client's own state around it (what
/// is staged, which story beats are waiting): never campaign state.
#[derive(Resource)]
pub struct CampaignSession {
    pub campaign: Campaign,
    pub story: Story,
    pub progress: StoryProgress,
    pub layout: MapLayout,
    /// Story beats waiting to be shown, in order (indices into the story).
    pub beats: Vec<usize>,
    /// The player's staged army and General decisions for this round.
    pub staged: Vec<Decision>,
    /// The round's one diplomatic action, staged on the diplomacy screen.
    pub diplomatic: Option<Decision>,
    /// The player's battles this round, played so far.
    pub played: Vec<PlayedBattle>,
    /// The battle of the player's the round is paused at.
    pub battle: Option<BattlePlan>,
    /// Why the last round was refused.
    pub message: Option<String>,
    /// The last round played.
    pub report: Option<RoundReport>,
    pub cursor: MapCursor,
    resolver: Memo<DynResolver>,
}

impl CampaignSession {
    /// A session on `campaign`, its AI battles going to `resolver`. The
    /// beats that hold before round 1 (the Rupture) are waiting.
    pub fn new(
        campaign: Campaign,
        story: Story,
        layout: MapLayout,
        resolver: Box<dyn Resolver + Send + Sync>,
    ) -> Self {
        let mut progress = StoryProgress::new(&story);
        let beats = progress.advance(&story, &campaign);
        CampaignSession {
            campaign,
            story,
            progress,
            layout,
            beats,
            staged: Vec::new(),
            diplomatic: None,
            played: Vec::new(),
            battle: None,
            message: None,
            report: None,
            cursor: MapCursor::default(),
            resolver: Memo::new(DynResolver(resolver)),
        }
    }

    /// The decisions ending the turn plays the round on: the diplomatic
    /// action first (as the AI's, so what it spends is gone before a
    /// General is raised), then the staged orders in the order given.
    pub fn decisions(&self) -> Vec<Decision> {
        self.diplomatic.iter().chain(&self.staged).cloned().collect()
    }

    /// The screen to show when nothing else is pending.
    fn resting(&self) -> Screen {
        if !self.beats.is_empty() {
            Screen::Story
        } else if self.campaign.is_over() {
            Screen::Over
        } else {
            Screen::Map
        }
    }

    /// End the turn: step the round on the staged decisions and the battles
    /// played so far. Returns the screen to go to: the map (or the story,
    /// or the end) once the round is played or refused; the battle screen
    /// when the round stops at a battle of the player's.
    pub fn end_turn(&mut self) -> Screen {
        let ds = self.decisions();
        match self.campaign.step_round(&ds, &self.played, &mut self.resolver) {
            Ok(RoundStep::Played(report)) => {
                self.staged.clear();
                self.diplomatic = None;
                self.played.clear();
                self.resolver.clear();
                self.battle = None;
                self.message = None;
                self.report = Some(report);
                self.cursor = MapCursor::default();
                self.resting()
            }
            Ok(RoundStep::Battle(plan)) => {
                self.battle = Some(plan);
                Screen::Battle
            }
            Err(e) => {
                // A refused round changed nothing; what was played of it
                // is played again.
                self.played.clear();
                self.resolver.clear();
                self.battle = None;
                self.message = Some(format!("Round refused: {e}"));
                Screen::Map
            }
        }
    }

    /// The battle the round is paused at was played: record its outcome and
    /// step the round again (the next screen, as [`end_turn`](Self::end_turn)).
    pub fn battle_played(&mut self, outcome: BattleOutcome) -> Screen {
        let Some(plan) = self.battle.take() else {
            return self.resting();
        };
        self.played.push(PlayedBattle {
            round: plan.round,
            city: self.campaign.campaign().cities[plan.city].id.clone(),
            outcome,
        });
        self.end_turn()
    }

    /// The player's armies standing at a city, ascending id.
    fn orderable_armies(&self) -> Vec<u32> {
        let p = self.campaign.player();
        self.campaign
            .armies()
            .iter()
            .filter(|a| Some(a.nation) == p && a.at().is_some())
            .map(|a| a.id)
            .collect()
    }

    /// The cities a selected army can march to, in road order.
    fn neighbours(&self) -> Vec<usize> {
        let Some(at) = self.cursor.army.and_then(|id| self.campaign.army(id)).and_then(|a| a.at()) else {
            return Vec::new();
        };
        self.campaign.campaign().roads_of(at).map(|(_, r)| r.other(at)).collect()
    }

    /// The city the cursor's target points at.
    pub fn target_city(&self) -> Option<usize> {
        self.cursor.target.and_then(|k| self.neighbours().get(k).copied())
    }

    /// The living nations other than the player's, ascending index.
    fn rivals(&self) -> Vec<usize> {
        let c = &self.campaign;
        (0..c.nations().len())
            .filter(|&n| Some(n) != c.player() && c.nations()[n].alive)
            .collect()
    }

    fn player_cities(&self) -> Vec<usize> {
        self.campaign.player().map_or_else(Vec::new, |p| self.campaign.cities_of(p))
    }
}

fn cycle(len: usize, cur: Option<usize>, forward: bool) -> Option<usize> {
    if len == 0 {
        return None;
    }
    Some(match (cur, forward) {
        (None, true) => 0,
        (None, false) => len - 1,
        (Some(i), true) => (i + 1) % len,
        (Some(i), false) => (i + len - 1) % len,
    })
}

/// The shipped session: House Vael, the heir, on the shipped map with
/// diplomacy, his battles played live, the AI's on the headless sim.
pub fn shipped_session(seed: u64) -> Result<CampaignSession, String> {
    let content = Content::load_default().map_err(|e| e.to_string())?;
    let campaign = CampaignContent::shipped_with_diplomacy(&content).map_err(|e| e.to_string())?;
    let c = Campaign::new(content, campaign, seed, Some("house"))
        .map_err(|e| e.to_string())?
        .with_live_battles();
    let story = Story::shipped(c.content(), c.campaign()).map_err(|e| e.to_string())?;
    let layout = MapLayout::shipped(c.campaign())?;
    Ok(CampaignSession::new(c, story, layout, Box::new(HeadlessResolver)))
}

// ---- the app ----------------------------------------------------------------

/// Register the client's logic on `app`: the screens and their keys. **The
/// one definition**, shared by the shipped app and the headless tests.
/// `_sim` is the schedule the battle sim runs on (`FixedUpdate` shipped,
/// `Update` in the tests).
pub fn add_campaign_client(app: &mut App, _sim: impl ScheduleLabel + Clone) {
    if !app.is_plugin_added::<StatesPlugin>() {
        app.add_plugins(StatesPlugin);
    }
    app.init_state::<Screen>();
    let session = resource_exists::<CampaignSession>;
    app.add_systems(
        Update,
        (
            story_keys.run_if(in_state(Screen::Story)).run_if(session),
            map_keys.run_if(in_state(Screen::Map)).run_if(session),
            diplomacy_keys.run_if(in_state(Screen::Diplomacy)).run_if(session),
        ),
    );
}

/// Start `session` on `app`: its waiting beats, else the map (or the end).
pub fn start(app: &mut App, session: CampaignSession) {
    let screen = session.resting();
    app.insert_resource(session);
    app.world_mut().resource_mut::<NextState<Screen>>().set(screen);
}

/// The shipped campaign client on `seed`: a window, the client's logic and
/// its view.
pub fn build_campaign_app(seed: u64) -> App {
    let session = shipped_session(seed).unwrap_or_else(|e| panic!("the campaign does not load: {e}"));
    let mut app = App::new();
    app.add_plugins(DefaultPlugins)
        .insert_resource(Time::<Fixed>::from_hz(60.0))
        .insert_resource(session.campaign.content().clone());
    add_campaign_client(&mut app, FixedUpdate);
    crate::campaign_view::add_campaign_view(&mut app);
    start(&mut app, session);
    app
}

fn go(next: &mut NextState<Screen>, now: &Screen, to: Screen) {
    if *now != to {
        next.set(to);
    }
}

/// Story: Enter (or Space) shows the next beat, then moves on.
fn story_keys(
    keys: Res<ButtonInput<KeyCode>>,
    mut s: ResMut<CampaignSession>,
    now: Res<State<Screen>>,
    mut next: ResMut<NextState<Screen>>,
) {
    if keys.just_pressed(KeyCode::Enter) || keys.just_pressed(KeyCode::Space) {
        if !s.beats.is_empty() {
            s.beats.remove(0);
        }
        let to = s.resting();
        go(&mut next, now.get(), to);
    }
}

/// Map: Tab picks an army, Left/Right a neighbour for it, M stages the
/// march; Up/Down pick a city, R stages a General raised there; Backspace
/// clears the staged orders; D opens diplomacy; Enter ends the turn.
fn map_keys(
    keys: Res<ButtonInput<KeyCode>>,
    mut s: ResMut<CampaignSession>,
    now: Res<State<Screen>>,
    mut next: ResMut<NextState<Screen>>,
) {
    let s = &mut *s;
    if keys.just_pressed(KeyCode::Tab) {
        let armies = s.orderable_armies();
        let cur = s.cursor.army.and_then(|id| armies.iter().position(|&a| a == id));
        s.cursor.army = cycle(armies.len(), cur, true).map(|k| armies[k]);
        s.cursor.target = None;
    }
    for (key, forward) in [(KeyCode::ArrowRight, true), (KeyCode::ArrowLeft, false)] {
        if keys.just_pressed(key) {
            s.cursor.target = cycle(s.neighbours().len(), s.cursor.target, forward);
        }
    }
    for (key, forward) in [(KeyCode::ArrowDown, true), (KeyCode::ArrowUp, false)] {
        if keys.just_pressed(key) {
            let cities = s.player_cities();
            let cur = s.cursor.city.and_then(|c| cities.iter().position(|&x| x == c));
            s.cursor.city = cycle(cities.len(), cur, forward).map(|k| cities[k]);
        }
    }
    if keys.just_pressed(KeyCode::KeyM) {
        if let (Some(army), Some(to)) = (s.cursor.army, s.target_city()) {
            let to = s.campaign.campaign().cities[to].id.clone();
            s.staged.retain(|d| !matches!(d, Decision::March { army: a, .. } if *a == army));
            s.staged.push(Decision::March { army, to });
        }
    }
    if keys.just_pressed(KeyCode::KeyR) {
        if let Some(c) = s.cursor.city {
            let city = s.campaign.campaign().cities[c].id.clone();
            s.staged.push(Decision::Raise { city });
        }
    }
    if keys.just_pressed(KeyCode::Backspace) {
        s.staged.clear();
        s.message = None;
    }
    if keys.just_pressed(KeyCode::KeyD) && s.campaign.has_diplomacy() {
        go(&mut next, now.get(), Screen::Diplomacy);
        return;
    }
    if keys.just_pressed(KeyCode::Enter) {
        let to = s.end_turn();
        go(&mut next, now.get(), to);
    }
}

/// Diplomacy: Up/Down pick a rival; C courts it, A allies with it, K asks
/// the player's ally to attack it, B betrays it, I (on two rivals in turn)
/// incites them against each other. One action a round: a new one replaces
/// the staged one. Backspace clears it; Esc (or D) goes back to the map.
fn diplomacy_keys(
    keys: Res<ButtonInput<KeyCode>>,
    mut s: ResMut<CampaignSession>,
    now: Res<State<Screen>>,
    mut next: ResMut<NextState<Screen>>,
) {
    let s = &mut *s;
    for (key, forward) in [(KeyCode::ArrowDown, true), (KeyCode::ArrowUp, false)] {
        if keys.just_pressed(key) {
            let rivals = s.rivals();
            let cur = s.cursor.rival.and_then(|n| rivals.iter().position(|&x| x == n));
            s.cursor.rival = cycle(rivals.len(), cur, forward).map(|k| rivals[k]);
        }
    }
    if let Some(r) = s.cursor.rival {
        let id = s.campaign.content().nations[r].id.clone();
        let staged = if keys.just_pressed(KeyCode::KeyC) {
            Some(Decision::Court { nation: id })
        } else if keys.just_pressed(KeyCode::KeyA) {
            Some(Decision::Ally { nation: id })
        } else if keys.just_pressed(KeyCode::KeyK) {
            Some(Decision::Ask { target: id })
        } else if keys.just_pressed(KeyCode::KeyB) {
            Some(Decision::Betray { nation: id })
        } else if keys.just_pressed(KeyCode::KeyI) {
            match s.cursor.incite.take() {
                Some(a) if a != r => Some(Decision::Incite {
                    a: s.campaign.content().nations[a].id.clone(),
                    b: id,
                }),
                Some(_) => None,
                None => {
                    s.cursor.incite = Some(r);
                    None
                }
            }
        } else {
            None
        };
        if staged.is_some() {
            if !matches!(staged, Some(Decision::Incite { .. })) {
                s.cursor.incite = None;
            }
            s.diplomatic = staged;
        }
    }
    if keys.just_pressed(KeyCode::Backspace) {
        s.diplomatic = None;
        s.cursor.incite = None;
    }
    if keys.just_pressed(KeyCode::Escape) || keys.just_pressed(KeyCode::KeyD) {
        go(&mut next, now.get(), Screen::Map);
    }
}

// ---- what the screens say -------------------------------------------------

fn city_name(c: &Campaign, city: usize) -> &str {
    &c.campaign().cities[city].name
}

fn decision_text(c: &Campaign, d: &Decision) -> String {
    let city = |id: &str| c.city_index(id).map_or(id.to_string(), |i| city_name(c, i).to_string());
    match d {
        Decision::March { army, to } => format!("March army {army} to {}", city(to)),
        Decision::Raise { city: at } => format!("Raise a General at {}", city(at)),
        Decision::Court { nation } => format!("Court {}", nation_by_id(c, nation)),
        Decision::Ally { nation } => format!("Ally with {}", nation_by_id(c, nation)),
        Decision::Ask { target } => format!("Ask your ally to attack {}", nation_by_id(c, target)),
        Decision::Betray { nation } => format!("Betray {}", nation_by_id(c, nation)),
        Decision::Incite { a, b } => format!("Incite {} and {} to war", nation_by_id(c, a), nation_by_id(c, b)),
    }
}

fn nation_by_id<'a>(c: &'a Campaign, id: &'a str) -> &'a str {
    c.nation_index(id).map_or(id, |n| nation_name(c.content(), Some(n)))
}

/// One diplomatic record, as the player reads it.
fn record_text(c: &Campaign, r: &DiplomacyRecord) -> String {
    let name = |n: usize| nation_name(c.content(), Some(n)).to_string();
    let who = if Some(r.actor) == c.player() { "You".to_string() } else { name(r.actor) };
    let what = match r.act {
        Act::Court { with } => format!("courted {}", name(with)),
        Act::Ally { with } => format!("allied with {}", name(with)),
        Act::Ask { ally, target } => format!("asked {} to attack {}", name(ally), name(target)),
        Act::Betray { ally } => format!("betrayed {}", name(ally)),
        Act::Incite { a, b } => format!("incited {} and {} to war", name(a), name(b)),
    };
    let answer = if r.accepted { "" } else { " (refused)" };
    format!("{who} {what}{answer}")
}

/// The diplomacy screen's text: each rival's standing with the player and
/// its betrayals, the player's ally and betrayals, the staged action, and
/// the last round's diplomacy.
pub fn diplomacy_text(s: &CampaignSession) -> String {
    use std::fmt::Write;
    let c = &s.campaign;
    let mut t = format!("Diplomacy  -  round {}\n", c.round());
    let Some(p) = c.player() else {
        return t;
    };
    let name = |n: usize| nation_name(c.content(), Some(n));
    let _ = writeln!(t, "You ({}): betrayals {}, treasury {} Alloy", name(p), c.betrayals(p), c.nations()[p].treasury);
    let _ = writeln!(t, "Your ally: {}", c.ally_of(p).map_or("none", name));
    t.push('\n');
    for n in 0..c.nations().len() {
        if n == p {
            continue;
        }
        let mark = if s.cursor.rival == Some(n) { "> " } else { "  " };
        let st = c.standing(p, n).map_or("-".to_string(), |x| x.to_string());
        let _ = write!(t, "{mark}{}: {st}, betrayals {}", name(n), c.betrayals(n));
        if !c.nations()[n].alive {
            t.push_str(", fallen");
        }
        if let Some(a) = c.ally_of(n) {
            let _ = write!(t, ", allied with {}", name(a));
        }
        if s.cursor.incite == Some(n) {
            t.push_str("  [incite: pick the second]");
        }
        t.push('\n');
    }
    if let Some(d) = &s.diplomatic {
        let _ = writeln!(t, "\nAction: {}", decision_text(c, d));
    }
    if let Some(rep) = &s.report {
        if !rep.diplomacy.is_empty() {
            let _ = writeln!(t, "\nRound {} diplomacy", rep.round);
            for r in &rep.diplomacy {
                let _ = writeln!(t, "  {}", record_text(c, r));
            }
        }
    }
    t.push_str("\nUp/Down rival  C court  A ally  K ask  B betray  I incite (two)  Backspace clear  Esc map\n");
    t
}

/// The map screen's text: the round, the cities (holder, capital,
/// buildings' HP, garrison), the roads, the armies, what is staged, why a
/// round was refused, and the last round's report.
pub fn map_text(s: &CampaignSession) -> String {
    use std::fmt::Write;
    let c = &s.campaign;
    let content = c.content();
    let name = |n: usize| nation_name(content, Some(n));
    let mut t = String::new();
    let player = c.player();
    let _ = write!(t, "Round {}", c.round());
    if let Some(p) = player {
        let _ = write!(t, "  -  {} (you), treasury {} Alloy", name(p), c.nations()[p].treasury);
    }
    t.push('\n');
    if let Some(m) = &s.message {
        let _ = writeln!(t, "{m}");
    }
    t.push_str("\nCities\n");
    for (i, city) in c.cities().iter().enumerate() {
        let def = &c.campaign().cities[i];
        let _ = write!(t, "  {}: {}", def.name, name(city.owner));
        if c.campaign().capital[city.owner] == i {
            t.push_str(", capital");
        }
        for b in &city.buildings {
            let _ = write!(
                t,
                ", {} {}/{}",
                content.buildings[b.def].name,
                b.hp,
                c.building_max_hp(city.owner, b.def)
            );
        }
        if !city.garrison.is_empty() {
            let _ = write!(t, ", garrison {}", city.garrison.len());
        }
        t.push('\n');
    }
    t.push_str("\nRoads\n  ");
    let roads: Vec<String> = c
        .campaign()
        .roads
        .iter()
        .map(|r| format!("{} - {}", city_name(c, r.a), city_name(c, r.b)))
        .collect();
    t.push_str(&roads.join(", "));
    t.push_str("\n\nArmies\n");
    for a in c.armies() {
        let general = if a.general.present() {
            let g = content.general_at(a.general.kind).map_or("General", |g| g.name.as_str());
            format!("{g} (rank {})", c.campaign().rank_of(a.general.xp))
        } else {
            "no General".to_string()
        };
        let place = match a.location {
            Location::At(at) => format!("at {}", city_name(c, at)),
            Location::Marching { from, to, .. } => {
                format!("marching {} -> {}", city_name(c, from), city_name(c, to))
            }
        };
        let mark = if s.cursor.army == Some(a.id) { "> " } else { "  " };
        let _ = writeln!(
            t,
            "{mark}army {} {}: {general}, {} units, {place}",
            a.id,
            name(a.nation),
            a.units.len()
        );
    }
    t.push_str("\nOrders\n");
    if let Some(d) = &s.diplomatic {
        let _ = writeln!(t, "  {}", decision_text(c, d));
    }
    if let Some(id) = s.cursor.army {
        let to = s.target_city().map_or("(Left/Right)", |k| city_name(c, k));
        let _ = writeln!(t, "  army {id} -> {to}");
    }
    if let Some(k) = s.cursor.city {
        let _ = writeln!(t, "  raise at {}", city_name(c, k));
    }
    for d in &s.staged {
        let _ = writeln!(t, "  {}", decision_text(c, d));
    }
    if let Some(rep) = &s.report {
        let _ = writeln!(t, "\nRound {} report", rep.round);
        if rep.battles.is_empty() {
            t.push_str("  no battles\n");
        }
        for b in &rep.battles {
            let (city, att, def) = (city_name(c, b.city), name(b.attacker), name(b.defender));
            if b.captured {
                let _ = write!(t, "  {city} taken by {att} from {def}");
                if b.absorbed {
                    let _ = write!(t, ": {def} is no more");
                }
                t.push('\n');
            } else {
                let held = match b.winner {
                    Some(Side::Attack) => "the attack won",
                    _ => "the defence held",
                };
                let _ = writeln!(t, "  {city}: {att} attacked {def}; {held}");
            }
        }
    }
    t.push_str(
        "\nTab army  Left/Right target  M march  Up/Down city  R raise  Backspace clear  D diplomacy  Enter end turn\n",
    );
    t
}

/// The story screen's text: the waiting beat.
pub fn story_text(s: &CampaignSession) -> String {
    match s.beats.first().map(|&k| &s.story.beats()[k]) {
        Some(b) => format!("{}\n\n{}\n\nEnter to continue", b.title, b.text),
        None => String::new(),
    }
}

/// The end screen's text.
pub fn over_text(s: &CampaignSession) -> String {
    let c = &s.campaign;
    let what = match c.outcome() {
        Some(CampaignOutcome::Won { nation }) if Some(nation) == c.player() => "You won the campaign.".to_string(),
        Some(CampaignOutcome::Won { nation }) => {
            format!("{} won the campaign.", nation_name(c.content(), Some(nation)))
        }
        Some(CampaignOutcome::PlayerLost) => "Your capital fell. The campaign is lost.".to_string(),
        Some(CampaignOutcome::Undecided) => "The campaign ended undecided.".to_string(),
        None => "The campaign goes on.".to_string(),
    };
    format!("Campaign over after round {}\n\n{what}", c.round())
}
