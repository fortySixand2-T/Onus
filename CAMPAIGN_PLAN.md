# CAMPAIGN_PLAN.md — Nations and the campaign layer

The plan after the battle layer. `DESIGN_BRIEF.md` describes the vision: a campaign to dominate
a map of six nations, where each round is a 1v1 battle. This file orders the work.
**Designing the nations comes first and needs no code.**

Same rules as BUILD_PLAN and BALANCE_PLAN:
- one AC per commit, test-first;
- `rts-implementer` builds, and `rts-critic` reviews on diff + spec only;
- decisions go to FINDINGS (F-series);
- content is data: nations and campaign rules live in RON, never in Rust;
- the brief changes first, then the data.

## The gate

`BALANCE_PLAN.md` B5 says: build nations and the campaign **only if** the core is both
balanced and fun. B5 needs a human to play, and that hasn't happened yet. So:

- **C0 (design on paper) starts now.** It builds nothing.
- **Overridden by the user (2026-11-02): "start c1 now, don't wait for b5".** C1 starts
  before the human play test. B5 still runs when the user can play, and its findings can
  revise C1 and later milestones.
- **Overridden again by the user (2026-11-03): "start c2".** C2 also starts before the human
  play test, on the same terms as C1 (FINDINGS F-062).
- **Overridden a third time by the user (2026-11-04): "start c3".** C3 also starts before the
  human play test, on the same terms as C1 and C2 (FINDINGS F-075).
- **Overridden a fourth time by the user (2026-11-07): "merge after ci, then start c4".** C4
  also starts before the human play test, on the same terms as C1 to C3 (FINDINGS F-088).
- **Overridden a fifth time by the user (2026-11-08): "merge after ci, then start c5".** C5
  also starts before the human play test, on the same terms as C1 to C4 (FINDINGS F-094).
- **Overridden a sixth time by the user (2026-11-10): "merge after ci, then start c6".** C6
  also starts before the human play test, on the same terms as C1 to C5 (FINDINGS F-098).
- ~~**C1 onward waits for B5 Go.**~~ A B5 No-Go sends us back to the battle layer, and C0 is
  revised against what the play test found.

## Starting point (2026-11-02)

- **Battle layer:** M0–M6 and B1–B4 are done. A 1v1 match against a scripted AI, on Alloy
  only. Both sides are identical mirrors, with no nations.
- **Buildings:** HQ, Foundry, Gene-Vats, Aether Spire. **Units:** Worker plus five combat
  units in a counter pentagon across three domains (Machine, Flesh, Energy).
- **Not built:**
  - nations (the brief puts two light-trait nations in MVP scope, but they were never built);
  - Biomass and Aether;
  - any campaign code.
- **Dependency to note:** the brief's example traits ("Biomass grows faster", "cheaper Aether")
  need the three-resource economy. On Alloy only, a trait can touch only Alloy costs, unit
  stats, train and build times, and worker economics.

---

## C0 — Nation and campaign design (paper) ✅ done

**Status (2026-11-02): ✅ done.** The design is in `DESIGN_BRIEF.md`: "Campaign design"
plus "Campaign data shape" (F-052, updates 1–7). The user's decisions:
- armies persist;
- battles are fought over cities;
- victory by the General's death or by retreat;
- per-nation General auras;
- absorbed Generals, at most 3;
- veteran Generals;
- siege damage persists;
- several armies converge on one battle;
- march points from the slowest unit's Speed;
- extra house Generals can be raised;
- perks apply only to a nation's own armies.

The names (House Vael, Cinderhold) were delegated to me. Items marked *(rec)* stand as the
design unless the user changes them. C1 started 2026-11-02 by the user's decision, ahead of B5.

The output goes into `DESIGN_BRIEF.md`, the one source of truth. Each item is
reviewed by the user, not by the critic, since this is design, not code.

- [x] **The nation sheet**, one per nation (the player's house plus 5 neighbours). Each covers:
  - **Identity:** name, the art it holds holy (or none), its doctrine, and its part in the
    Rupture and the heir's fall.
  - **Size:** what "size" means mechanically (see the map decision below).
  - **Trait package:** a domain lean plus a people speciality. Each is described by intent
    ("Machine units cost less"), with the number left for the balance sim. Each nation also
    gets an Alloy-only stand-in trait, so C2 can ship before C3.
  - **Battle personality:** which strategy script the nation's AI plays and how it adapts.
    This reuses B1's strategies-as-data, so each nation is a `strategies.ron` entry plus
    traits.
  - **Diplomatic disposition:** who it hates, who it would ally with, and how far it can be
    trusted.
  - **General's aura:** the kind of buff its General gives, matched to its doctrine.
  - **Look:** colour and silhouette cues. Legibility is pillar P3: you must see which nation
    you're facing.
- [x] **Trait magnitude bounds.** This settles the brief's open question: how strong can a
  domain lean get before it distorts the pentagon? The proposed rule: no trait may move a
  pentagon link out of "holds", or a nation's cross-nation strength outside 35–65%. The
  balance sim checks this from C2 on.
- [x] **Campaign map structure:**
  - Is a nation a single node, or a set of **cities or provinces** with a capital?
  - What does size control?
  - Which nations border which?
  - Is the map fixed or seeded?
- [x] **Round structure:**
  - Who fights whom each round.
  - Turn order.
  - How battles between two AI neighbours are decided. The proposal is to auto-resolve them
    with the real headless battle sim, which is deterministic and already built.
- [x] **What carries between rounds** (decided 2026-11-02: surviving units and their HP carry over; the dead are lost). The options were nothing, an economy bonus, surviving or veteran units,
  or captured cities' traits.
- [x] **Diplomacy rules:**
  - The ≤1-ally cap.
  - What betrayal costs.
  - What "2 diplomacy" means: the brief's open question, leaning towards "ties with two
    states".
  - How destabilize works.
- [x] **Win, lose and length:**
  - Domination is the win condition, and the player's loss condition needs defining.
  - The target campaign length in rounds and in hours.
- [x] **Data shape** (drafted 2026-11-02, `DESIGN_BRIEF.md` "Campaign data shape"). Sketch `assets/data/nations.ron` and `campaign.ron` from the decisions
  above. These are the fields only; no numbers.
- [x] Record the decisions in an F-entry, and update this plan's C1–C7 ACs to match.

### Draft roster, for the user to react to (not decided)

This draft follows the brief's fiction:
- The pure houses each canonise one art.
- The heir's house is the disgraced synthesists.
- Five neighbours across three domains means some domains repeat or some neighbours are
  outsiders.
- Sizes differ.

| Nation | Art | Size | Lean (intent) | Speciality (intent) | Alloy-only stand-in | Role in the story |
|---|---|---|---|---|---|---|
| **Your house** (synthesists) | none: the mix | smallest | none | a bonus for a **mixed** army, which re-enacts the heresy | the mixed-army bonus works on Alloy | the disgraced heir, rising by guile |
| *Forgeborn* | Machine | large | Foundry units cheaper | sturdier buildings | Foundry units −x% Alloy | led the condemnation |
| *Verdant* | Flesh | large | Biomass grows faster | Gene-Vats train faster | Gene-Vats train −x% time | took the largest share of your holdings |
| *Aetherkin* | Energy | medium | cheaper Aether | Arclight range | Arclight +x range | guards the ley-wells, and fears another Rupture |
| *(minor) Machine city-state* | Machine | small | Alloy mines richer | workers carry more | +x carry | merchants: allies for sale |
| *(minor) Rupture remnant* | Energy or none | small | — | scarred and defensive | building HP +x% | survivors who may know the truth about the Rupture |

---

## C1 — Campaign battle rules in the 1v1 (started 2026-11-02, ahead of B5 by user decision)

The user's decisions change the battle itself, so they come first. Every item is data-driven.
A **skirmish** mode keeps today's rules exactly.

- [x] **The General:** a unit type in `units.ron`, one per side. When a side's General dies,
  that side loses the battle.
- [x] **Several Generals per side:** a battle can field more than one General per side, and
  that side loses when **all** its Generals are dead (or it retreats).
- [x] **The General's aura:** friendly units within a radius get a bonus. Each General type
  carries its own aura, defined in RON as a radius plus a list of stat modifiers (offense,
  defense, armor, speed, attack interval, HP regeneration). The synthesists add a
  domain-count scaling rule. Six aura kinds, one per nation (see the brief). An aura never
  touches the nemesis bonus.
- [x] **Retreat:** an `Order`. The side's units head for its entry edge, and the battle ends
  as a retreat once the withdrawal finishes. The outcome records which units got off the
  field.
- [x] **Battle setup from a roster.** A side can start with a given army (its General, its
  units and their HP) and given buildings, instead of the skirmish start. The attacker and
  defender setups are what C4 builds from campaign state.
- [x] **The battle outcome reports survivors:** each surviving unit's type and HP per side,
  every building's HP (siege damage persists), and whether each General survived.
  C4 writes this back to the campaign.
- [x] **AI:** protects its General, and retreats on a data-driven threshold.
- [x] **Golden licence:** skirmish mode reproduces today's pinned results exactly.
- [ ] **Balance:** the B3 gate runs on campaign-style battles (General plus retreat), and B5
  is re-read with a human playing them.
  *(2026-11-02: the B3 half is done and recorded in FINDINGS F-060 (gate Fail on K1 and K3; K2 undetermined after F-061).
  The B5 human re-read is still open, so this box stays unticked.)*

## C2 — Nations in the 1v1, light traits on Alloy (started 2026-11-03, ahead of B5 by user decision)

- [x] `nations.ron` holds the C0 roster. The sim applies traits as data-driven modifiers
  (costs, stats, times, carry), never as code per nation. Selection works through
  `MatchSettings`, the same way strategies do.
- [x] **Golden licence:** a "no nation" setting reproduces today's pinned results exactly.
- [x] **Batch runner:** add a nation axis. Mirror matchups keep isolating unit balance, and
  cross-nation matchups test faction balance.
- [ ] **Gate:** a nation criterion. Each nation's cross-nation strength is 35–65%, and the
  pentagon still holds under every nation pair. Tuning happens in RON only.
  *(2026-11-03: the criterion runs and reads **Fail** (FINDINGS F-066). Tollmarch's
  minimum carry trait (+1 on a base of 2) is 94.7% cross strength and breaks the pentagon
  wherever it is prey. That cannot be tuned by magnitude, so this box stays unticked
  pending the user's decision.)*
  *(Later, 2026-11-03: Tollmarch's trait is now faster gathering (×0.98) and House has
  Synthesis. Cross reads **undetermined** (F-074). Five nations read Pass. House reads
  40.3% [33.2, 47.6]: the mono-domain cross probes never let its mixed-army trait act. The
  pentagon was not run, and its baseline sen>rip link can't read Pass (F-066). Still
  unticked, pending the user.)*
- [x] **Client:** pick a nation, and see the opponent's nation at a glance (P3).

## C3 — The three-resource economy (Biomass grown, Aether channelled) (started 2026-11-04, ahead of B5 by user decision)

This is the brief's "first economic expansion after fun":
- Biomass is passive and capped by storage.
- Aether comes from contested ley-wells you must hold.
- Swap the Alloy-only stand-in traits for the real domain leans.
- Re-run the balance and nation gates.

The three-resource economy is **opt-in**, chosen through `MatchSettings` (and the client's pick)
the way nations and battles are. The default stays Alloy-only and plays exactly as today. Every
number lives in RON (`resources.ron`'s `three_resource` block, `nations.ron`).

- [x] **Biomass:** passive, storage-capped, grown by building Gene-Vats. Each side's HQ grows a
  small floor and each Gene-Vats adds growth and storage. Growth lands every `interval_ticks`
  and stops at the side's storage cap: the sum of its standing buildings' `storage`. Rates,
  storage and the starting Biomass are RON.
- [x] **Aether:** channelled from ley-wells placed on the map from RON (positions along and
  across the line between the two HQs, so every map and both orientations get the same layout).
  A side **holds** a well while at least one of its units or buildings is within `hold_radius`
  and no enemy unit or building is. A held well banks `per_interval` Aether to its holder every
  `interval_ticks` ticks of holding. Holding progress pauses while the well is empty or
  contested, and restarts when the holder changes. Radius, rate and sites are RON.
- [x] **Unit prices:** in three-resource mode units are paid in their brief resource (the
  `cost` in `units.ron`): Machine units and Workers in Alloy, Ripper and Ravager in Biomass,
  Arclight in Aether. Buildings stay priced in Alloy. An order the side cannot pay in that
  resource is refused and charges nothing.
- [x] **Real leans:** in three-resource mode each nation plays its brief traits (lean plus
  speciality) from `three_resource_traits` in `nations.ron`, instead of the Alloy-only
  stand-in. Forgeborn: Foundry units cheaper, sturdier buildings. Verdant: Biomass grows
  faster, Gene-Vats train faster. Aetherkin: Arclights cost less Aether, longer Arclight range.
  Tollmarch: richer Alloy mines (more Alloy banked per load), faster gathering. Ashen Reach:
  more building HP. House Vael: Synthesis. The Alloy-only stand-ins keep working in Alloy-only
  mode, unchanged. A trait that cannot act in the mode its list is played in is a load error.
- [x] **AI:** in three-resource mode the AI runs the three-resource economy. It builds its
  Gene-Vats (its script's openings), keeps a worker on its home well, sends a guard detachment
  to contest and hold the middle well (and back to its home well when an enemy takes it), and
  pays each unit in its own resource. The detachment sizes are RON. Alloy-only AI traces are
  unchanged.
- [x] **Client:** the pick screen chooses the economy. The panel shows all three resources,
  and the ley-wells are drawn with their holder. Kept thin; anything that needs a human look on
  the Mac is listed in FINDINGS.
- [x] **Golden licence:** the default (Alloy-only) setting reproduces today's pinned results
  exactly: no re-pin. A match log records its economy mode, and a replay applies it or refuses.
- [x] **Gates:** the B3 balance gate (K1/K2/K3) and the C2 nation cross gate are re-run on the
  three-resource economy, and the numbers are recorded in FINDINGS. Economy numbers and trait
  sizes may be tuned in RON (each step recorded); base unit combat stats are not retuned.

## C4 — Campaign core (headless, render-free) (started 2026-11-08, ahead of B5 by user decision)

- [x] `src/campaign/` is a deterministic, seeded, render-free campaign state machine that
  owns its own state hash. It holds the map, the nations and their cities, ownership, rounds,
  and conquest.
- [x] **Veteran Generals:** a surviving General gains experience and rank, and each rank
  strengthens his aura (RON per rank, with a cap). A replacement General starts at rank 0.
- [x] **Siege damage persists:** a city's building HP carries between rounds. Unattacked cities
  repair at a RON rate, destroyed buildings need rebuilding, and a conqueror inherits the
  damage.
- [x] **Absorbed Generals:** taking a capital gives the conqueror a new General with that
  nation's aura, leading a new army, up to 3 absorbed Generals. No trait transfers.
- [x] **Raised Generals:** a nation can raise extra Generals of its own house at a city, for a
  RON levy cost. Nation perks apply only to the nation's own armies and cities.
- [x] **Army movement:** each round an army gets march points equal to its slowest unit's
  Speed (plus the Logistics bonus). Roads have RON lengths and terrain multipliers, and
  partial progress carries over.
- [x] **Converging armies:** every army that reaches a city in the round its battle starts
  fights in it, so a battle can have several Generals per side. Auras of different kinds
  stack; auras of the same kind do not.
- [x] **Armies persist:** survivors from each battle outcome return to their army, and the dead
  are removed. Each held city adds a reinforcement levy every round.
- [x] A round launches a battle with `MatchSettings` built from campaign state: the nations,
  their traits and size, and the city being fought over.
- [x] Battles between two AI nations are auto-resolved by the headless battle sim.
- [x] A campaign saves as seed plus a log of player decisions, reusing M5's replay pattern.
- [ ] **Campaign balance sim:** play many AI-only campaigns. Each nation should win
  sometimes, and length should land on the C0 target. This is balance-as-backtesting, one
  layer up.

## C5 — Diplomacy (started 2026-11-08, ahead of B5 by user decision)

The five items: alliance with the ≤1 cap; betrayal and its cost; destabilize, i.e. proxy war, as
C0 defines it; diplomatic AI driven by each nation's disposition; and the campaign sim measuring
whether diplomacy is a choice or a dominant strategy. The rules are C0's "Diplomacy" (the brief's
"Diplomacy" section; "2 diplomacy" is **Tied standing with two nations**, as C0 resolved it).

Diplomacy is **opt-in through campaign content**. Its numbers live in a new
`assets/data/diplomacy.ron`, which a campaign's content takes on only when asked
(`CampaignContent::with_diplomacy`). A campaign without it plays exactly as C4 did. Every number is
RON. Readings the spec does not settle are in FINDINGS (F-095), marked provisional. No client or UI:
that is C6.

- [x] **Content:** `diplomacy.ron` holds every diplomacy number:
  - each nation's disposition, `(towards, ally_bias, betrayal_tolerance)`;
  - the default standing;
  - `ally_cap`, `betrayal_standing_drop` and `destabilize_requires_tied`;
  - the diplomatic actions a nation may take per round, and each action's treasury cost;
  - the AI's acceptance thresholds, `notice_per_city`, and how many rounds an incited or requested
    war lasts.

  It is validated at load against the battle content:
  - exactly one disposition per nation;
  - no unknown, self or repeated `towards` entry;
  - no pair starts Allied;
  - the ally cap is at most 1;
  - a betrayal drops at least one step;
  - destabilize needs at least two Tied nations.

  Each pair of nations starts at the standing their dispositions give: the default standing when
  neither names the other, and the more hostile one when both do. The file's fingerprint joins the
  campaign content's.
- [x] **Golden licence:** without diplomacy, a campaign plays exactly as C4. C4's campaign golden
  (0x1600ef997dbf8728) and every battle golden are unchanged, and a diplomatic decision is refused.
  With diplomacy attached but every nation abstaining, a campaign fights the same battles, with the
  same results, as C4.
- [x] **Alliance with the ≤1 cap:**
  - Each pair of nations has a standing: Hostile, Neutral, Tied or Allied.
  - A nation can **court** another, which raises their standing one step (no higher than Tied), for
    `court_cost`.
  - A nation can **ally** with a nation it is Tied with, for `ally_cost`, while both are under the
    cap of one ally.
  - An AI nation answers an offer by its disposition, and the cost is paid only when it accepts.
  - Allies never fight: no battle starts at a city where the only foreign army belongs to the
    holder's ally.
  - A nation can **ask** its ally to attack a shared enemy. An AI ally then marches on that enemy's
    capital for the RON number of rounds.
  - A nation takes at most the RON number of diplomatic actions per round (one).
  - An alliance ends when either nation falls.
  - The player wins by domination when every rival still standing is his ally.
- [x] **Betrayal and its cost:** a nation can betray its ally.
  - The pair goes Hostile.
  - Every other living nation's standing towards the betrayer drops `betrayal_standing_drop` steps,
    never below Hostile.
  - The betrayal is counted. A nation refuses every offer from a nation whose betrayals exceed its
    `betrayal_tolerance`.
  - The betrayer's armies standing in the old ally's cities assault them that round.
- [x] **Destabilize (proxy war):** a nation with Tied standing with `destabilize_requires_tied`
  nations (two) can incite two of them, not allied with each other, to war, for `incite_cost`. The
  two go Hostile, and each of them that is AI marches on the other's capital for the RON number of
  rounds. Both armies bleed in auto-resolved battles, and the inciter's army is never committed.
- [x] **Diplomatic AI:** each AI nation takes at most one diplomatic action per round, chosen from
  its disposition and the map.
  - Its **willingness** towards a nation is its `ally_bias`, plus their standing, less
    `notice_per_city` for every city that nation holds above its own. This is C0's notice rule: a
    small house is no one's target, and a grown one finds few friends.
  - By fixed rules and in a fixed order it betrays its ally once that ally is its last rival, allies,
    asks its ally to attack, incites, or courts.
  - It draws nothing from the rng.
  - It never makes an offer to the player.
- [x] **Saved and hashed:**
  - Standings, betrayal counts and war targets are in the campaign state hash.
  - The player's diplomatic decisions are in the save.
  - A diplomatic campaign saves, loads, and replays to the same hashes.
  - A changed diplomatic decision diverges at its round.
  - A save played with diplomacy is refused by content without it, and the reverse.
- [ ] **Diplomacy balance:** the campaign sim measures whether diplomacy is a choice or a dominant
  strategy.
  - Per seed it plays these AI-only campaigns:
    - every nation diplomatic, then each nation abstaining alone;
    - every nation abstaining, then each nation diplomatic alone.
  - It compares each nation's placement (how many nations outlasted it) with and without its own
    diplomacy.
  - Diplomacy is **dominant** for a nation when engaging is never worse and sometimes better.
    It is a **choice** for a nation when engaging is better in some campaigns and worse in others.
  - The reading passes when diplomacy is dominant for no nation and is a choice for at least one
    (provisional, F-097 D31). The tie-break by the round a nation fell (D32) is reported, not gated.

## C6 — Campaign client (started 2026-11-10, ahead of B5 by user decision)

The two items: the map screen, round flow, diplomacy screen, and the move into and out of battles;
and story beats from the hook (the Rupture, and the heir's vengeance, glory or legacy).

The campaign (`src/campaign/`) and the battle sim (`src/sim/`) stay render-free; the client is a thin
Bevy driver over them. The player's choices on the screens become the same `Decision`s the campaign
already consumes, so a save is still the seed plus the log. Battle input is still `Order`s in the sim
schedule. Readings the spec leaves open are in FINDINGS (F-099), marked provisional. What only a human
can judge (layout, readability, feel) is listed there **for the user on the Mac**, and no AC is ticked
on a look alone.

- [ ] **Story content:** `assets/data/story.ron` holds every story beat: an id, a trigger, a title
  and its text. The triggers are:
  - the campaign starts (the Rupture);
  - the player takes a nation's capital (vengeance; a named nation, or any);
  - a General of the player's reaches a rank (glory);
  - the player wins (legacy);
  - the player loses.

  It is validated at load against the battle and campaign content: unique ids, known nations, a rank
  above 0 that the campaign has, no empty title or text, exactly one start beat and one win beat,
  and at least one vengeance beat and one glory beat. A render-free rule (`campaign::story`) reads
  the beats off campaign state: each fires once, the first time its trigger holds, in file order.
  The story never changes campaign state or its hash.
- [ ] **Live battles in the campaign core:** a campaign can be played with live battles.
  - A round then pauses at each battle the player's nation fights (an undefended city is no
    battle). It hands back that battle's plan and leaves the campaign unchanged.
  - Given the battle's played outcome, the round resumes. AI-vs-AI battles in the round are still
    auto-resolved.
  - A played outcome that does not fit its battle, or names a battle the round does not fight,
    refuses the round.
  - A live round fed its outcomes hashes exactly as the same round played with a resolver that
    returns them.
  - Without live battles a campaign plays exactly as before: C4's golden (0x1600ef997dbf8728) and
    every battle golden are unchanged.
- [ ] **Saves carry played battles:** a live campaign's save holds each played battle's outcome
  beside the round's decisions, and it resumes to the same hashes. A changed outcome diverges at its
  round, and a missing one refuses its round. A save without live battles is written exactly as
  before.
- [ ] **Screens and round flow:**
  - `cargo run -- campaign` opens the campaign client: the player is House Vael, the heir, on the
    shipped map with diplomacy. `cargo run` plays the 1v1 skirmish exactly as before.
  - The screens are Bevy states: Story, Map, Diplomacy, Battle and Over.
  - The map screen shows the cities (holder, capital, buildings' HP), the roads and the armies. Keys
    select one of the player's armies, a neighbouring city for it to march on, and a city to raise a
    General at, and stage each as a `Decision`.
  - Ending the turn plays the round on the staged decisions. A refused round shows why and changes
    nothing. The round's report (the battles fought and the cities taken) shows on the map.
  - City positions on the screen are RON (`assets/data/map_layout.ron`), validated against the
    campaign map.
- [ ] **Diplomacy screen:** it shows each rival's standing with the player, the player's ally and
  each nation's betrayals. Keys stage the round's one diplomatic action (court, ally, ask, betray or
  incite) as a `Decision`.
  - The round plays that action **before** the player's staged orders, as the AI's is played
    before its own, so what it spends is gone before a General is raised (a provisional reading,
    F-099 S17).
- [ ] **Into and out of battles:** when the round pauses at a battle of the player's, the client
  enters the Battle screen.
  - The battle is the match `battle_settings` builds from the plan, with the player's side on the
    faction he commands. The opposing nation's AI plays its own strategy.
  - The player commands his side through `Order`s in the sim schedule, and a retreat key issues the
    C1 retreat.
  - The battle ends when the battle layer decides it or the campaign's tick cap passes. Its outcome
    (survivors and their HP, buildings, each General's fate, a retreat) is read off the field the
    way the headless resolver reads it. The battle world is cleared, and the round resumes with the
    outcome.
- [ ] **Same campaign, either way:** a campaign driven through the client (keys to decisions, live
  battles) hashes round for round exactly as the same decisions and battle outcomes fed headlessly,
  and its save resumes to the same hashes.
- [ ] **Story beats on screen:** the Rupture beat opens the campaign. After each round, the beats it
  triggered show on the Story screen, in order, before the map. The legacy beat (or the loss beat)
  shows before the Over screen.

## C7 — Campaign fun gate (human; like B5)

- Play full campaigns.
- Are the choices about who to fight, ally or turn against each other real?
- Does each nation feel different?
- Go or No-Go on the campaign as shipped.
