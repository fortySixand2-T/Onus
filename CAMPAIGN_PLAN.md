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
- [ ] **The General's aura:** friendly units within a radius get a bonus. Each General type
  carries its own aura, defined in RON as a radius plus a list of stat modifiers (offense,
  defense, armor, speed, attack interval, HP regeneration). The synthesists add a
  domain-count scaling rule. Six aura kinds, one per nation (see the brief). An aura never
  touches the nemesis bonus.
- [ ] **Retreat:** an `Order`. The side's units head for its entry edge, and the battle ends
  as a retreat once the withdrawal finishes. The outcome records which units got off the
  field.
- [ ] **Battle setup from a roster.** A side can start with a given army (its General, its
  units and their HP) and given buildings, instead of the skirmish start. The attacker and
  defender setups are what C4 builds from campaign state.
- [ ] **The battle outcome reports survivors:** each surviving unit's type and HP per side,
  every building's HP (siege damage persists), and whether each General survived.
  C4 writes this back to the campaign.
- [ ] **AI:** protects its General, and retreats on a data-driven threshold.
- [ ] **Golden licence:** skirmish mode reproduces today's pinned results exactly.
- [ ] **Balance:** the B3 gate runs on campaign-style battles (General plus retreat), and B5
  is re-read with a human playing them.

## C2 — Nations in the 1v1, light traits on Alloy (after B5 Go)

- [ ] `nations.ron` holds the C0 roster. The sim applies traits as data-driven modifiers
  (costs, stats, times, carry), never as code per nation. Selection works through
  `MatchSettings`, the same way strategies do.
- [ ] **Golden licence:** a "no nation" setting reproduces today's pinned results exactly.
- [ ] **Batch runner:** add a nation axis. Mirror matchups keep isolating unit balance, and
  cross-nation matchups test faction balance.
- [ ] **Gate:** a nation criterion. Each nation's cross-nation strength is 35–65%, and the
  pentagon still holds under every nation pair. Tuning happens in RON only.
- [ ] **Client:** pick a nation, and see the opponent's nation at a glance (P3).

## C3 — The three-resource economy (Biomass grown, Aether channelled)

This is the brief's "first economic expansion after fun":
- Biomass is passive and capped by storage.
- Aether comes from contested ley-wells you must hold.
- Swap the Alloy-only stand-in traits for the real domain leans.
- Re-run the balance and nation gates.

## C4 — Campaign core (headless, render-free)

- [ ] `src/campaign/` is a deterministic, seeded, render-free campaign state machine that
  owns its own state hash. It holds the map, the nations and their cities, ownership, rounds,
  and conquest.
- [ ] **Veteran Generals:** a surviving General gains experience and rank, and each rank
  strengthens his aura (RON per rank, with a cap). A replacement General starts at rank 0.
- [ ] **Siege damage persists:** a city's building HP carries between rounds. Unattacked cities
  repair at a RON rate, destroyed buildings need rebuilding, and a conqueror inherits the
  damage.
- [ ] **Absorbed Generals:** taking a capital gives the conqueror a new General with that
  nation's aura, leading a new army, up to 3 absorbed Generals. No trait transfers.
- [ ] **Raised Generals:** a nation can raise extra Generals of its own house at a city, for a
  RON levy cost. Nation perks apply only to the nation's own armies and cities.
- [ ] **Army movement:** each round an army gets march points equal to its slowest unit's
  Speed (plus the Logistics bonus). Roads have RON lengths and terrain multipliers, and
  partial progress carries over.
- [ ] **Converging armies:** every army that reaches a city in the round its battle starts
  fights in it, so a battle can have several Generals per side. Auras of different kinds
  stack; auras of the same kind do not.
- [ ] **Armies persist:** survivors from each battle outcome return to their army, and the dead
  are removed. Each held city adds a reinforcement levy every round.
- [ ] A round launches a battle with `MatchSettings` built from campaign state: the nations,
  their traits and size, and the city being fought over.
- [ ] Battles between two AI nations are auto-resolved by the headless battle sim.
- [ ] A campaign saves as seed plus a log of player decisions, reusing M5's replay pattern.
- [ ] **Campaign balance sim:** play many AI-only campaigns. Each nation should win
  sometimes, and length should land on the C0 target. This is balance-as-backtesting, one
  layer up.

## C5 — Diplomacy

- Alliance with the ≤1 cap.
- Betrayal and its cost.
- Destabilize, i.e. proxy war, as C0 defines it.
- Diplomatic AI driven by each nation's disposition.
- The campaign sim measures whether diplomacy is a choice or a dominant strategy.

## C6 — Campaign client

- The map screen, round flow, diplomacy screen, and the move into and out of battles.
- Story beats from the hook: the Rupture, and the heir's vengeance, glory or legacy.

## C7 — Campaign fun gate (human; like B5)

- Play full campaigns.
- Are the choices about who to fight, ally or turn against each other real?
- Does each nation feel different?
- Go or No-Go on the campaign as shipped.
