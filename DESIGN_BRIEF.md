# <Game Onus> — Design Brief

**One-line pitch:** <"X meets Y" — e.g., "AoE-style economy with Total War positioning, in 5-minute matches">

> Status: DRAFT — pillars, roster, stats, counters, economy, win condition, nations, campaign layer, and hook drafted; core loop / fun-hypothesis still open.
> Source of truth for M4 content: every stat here is mirrored in `assets/data/*.ron`.

**Two layers:** the **battle layer** (real-time 1v1 skirmish — the MVP) and the **campaign layer** (strategic meta of nations, rounds, diplomacy, dominance — post-MVP). Build the battle layer to *fun* before any campaign layer exists.

---

## Pillars (working draft — reword freely)

Load-bearing principles. When a decision is a coin-flip, the pillars break the tie.

- **P1 — Every strategy has a counter.** The closed pentagon + coupled economy: the game rewards *reading and adapting*, not executing one memorized build fastest. Tiebreaker: anything strong with no clear answer gets nerfed or cut.
- **P2 — Won by composition and position, not APM.** From the 4-stat model (esp. the Defense/Armor split) and the Total War touchstone. Tiebreaker: reward the smart read over fast clicking.
- **P3 — Legible at a glance.** A counter game only works if you can instantly read what you face and what beats it. Readability is a *rule*, not polish. Tiebreaker: anything that muddies "what am I looking at / what beats it" loses.

> P2 (depth) and P3 (legibility) are in deliberate tension; when they clash, the MVP favors legibility.

## Hook / fantasy

**You are the heir of a disgraced house — the last *synthesists* in an age that damns the mixing of the three arts as heresy.**

Generations ago a unified craft that married **Machine**, **Flesh**, and **Energy** fell; its heirs fractured into pure houses, each canonizing one art as holy truth and the mix as profane. Your family quietly kept the old synthesis alive — until **the Rupture**: a great ley-well torn open, a region unmade, thousands lost. Your house was blamed, its heresy named as the cause, and cast down — name disgraced, works burned, holdings carved up among the very houses that condemned it. But the record is ash, and no one truly knows: did your ancestors' forbidden synthesis tear the world, or did the pure houses seize a convenient catastrophe to be rid of a heresy they already feared? You are what's left — rising through **guile** because no army was left to inherit, rebuilding the forbidden synthesis that is either your vindication or the seed of the next Rupture.

**Drive:** *vengeance* against the houses that broke you · *glory* to reclaim the name they stripped · *legacy* to prove the synthesis was never the sin.

**Why the fiction is the mechanics:** combining domains — exactly what the counter pentagon and coupled economy reward — is your family's heresy and birthright. Every mixed army re-enacts the transgression that damned your house; the pure houses stay single-domain out of *doctrine*, and your edge is having no pride left to lose. The RTS opening (a lone HQ, a few workers, building up from nothing) is the disgraced heir's rise, literally.

**Moral ambiguity (seeded for sequels):** the truth of the Rupture is never confirmed. You rebuild the very art that may have caused a catastrophe — vindication or hubris left open. A sequel can resolve it, or make your own synthesis court a second Rupture.

> Naming: keep **Machine / Flesh / Energy** as the legible category labels (pillar P3). Proper names for the three sacred arts, the fallen unified craft, and the five houses are a *flavor layer* on top (see Open questions).

## Core loop

- **30-second loop:** <gather → build → produce → position → fight → repeat>
- **Match arc:** opening <...> · mid <...> · end <...>
- **Target match length:** 5–8 min

## Interesting decisions

1. **Worker allocation** across gather-types, shifting as army needs shift.
2. **Expand vs build vs contest** — Alloy rewards expanding, Biomass rewards building, Aether rewards fighting.
3. **Diversify vs specialize** — committing to one unit exposes you to its predator (see pentagon).
4. <add your own>

## Stats (4-D, 1–10 design scale)

- **Speed** — movement / mobility.
- **Offense** — damage per hit.
- **Defense** — hit points (total durability pool).
- **Armor** — flat mitigation subtracted from *each* incoming hit.

The Defense/Armor split gives two kinds of tanky: high-Armor shrugs off many small hits but folds to a few big ones; high-Defense/low-Armor is the reverse — countered differently.

## MVP roster (5 combat units, 2 / 2 / 1 across three barracks)

Numbers are placeholders for the balance sim to settle.

| Unit | Barracks (domain) | Role | Spd | Off | Def | Arm | Cost | Bonus vs |
|------|-------------------|------|:--:|:--:|:--:|:--:|------|----------|
| **Bulwark**  | Foundry (Machine)     | walking fortress        | 2 | 4 | 9 | 9 | 95 Alloy    | Ravager  |
| **Sentinel** | Foundry (Machine)     | agile war-frame         | 7 | 6 | 5 | 5 | 90 Alloy    | Ripper   |
| **Ripper**   | Gene-Vats (Flesh)     | fragile swarm           | 9 | 6 | 3 | 1 | 40 Biomass  | Arclight |
| **Ravager**  | Gene-Vats (Flesh)     | regenerating bio-titan  | 4 | 7 | 8 | 3 | 95 Biomass  | Sentinel |
| **Arclight** | Aether Spire (Energy) | armor-melting channeler | 5 | 9 | 4 | 2 | 65 Aether   | Bulwark  |

Timings live in `assets/data/units.ron` (`mvp_train_ticks`, `mvp_attack_ticks`, at 60 ticks/s). B4's tuning pass (F-046) set the ones it moved: Bulwark trains in 1300 ticks (was 1500) and Ravager in 1100 (was 1200); Ripper hits every 50 ticks (was 40). The costs above are B4's too (Bulwark 110 -> 95, Ravager 90 -> 80).

B4 step 2b (F-049) retuned the roster around attack-move waves (every strategy's `wave_order` is `attack_move`): Ripper trains in 1040 ticks (was 720) and hits every 70 (was 50); Sentinel costs 90 (was 70); Ravager costs 95 (was 80) with Armor 3 (was 4); Arclight costs 65 (was 80) with Defense 4 (was 2). The table above carries these. A building's HP pool doubled (`building_hp_per_defense` 420 -> 840), and the nemesis bonus is now double damage (was +30%).

Economy unit (from HQ, not a barracks): **Worker** — gathers and builds, cheap, no combat role.

## Counter pentagon

Each unit deals a bonus against exactly one prey and is prey to exactly one predator — a closed loop, so every counter has its own counter.

**Nemesis bonus:** **double damage (+100%), *ignoring armor***, vs the one prey (+30% until F-049).

Cycle: **Sentinel → Ripper → Arclight → Bulwark → Ravager → Sentinel**

- **Sentinel → Ripper** — rapid-fire guns shred the fragile swarm.
- **Ripper → Arclight** — the swarm overruns the squishy channeler.
- **Arclight → Bulwark** — armor-melting energy pierces the tank's plating (why the bonus ignores armor).
- **Bulwark → Ravager** — the fortress out-tanks the bio-titan in a slugfest.
- **Ravager → Sentinel** — heavy organic mass crushes the lighter mech.

Base stats set *soft* matchups; the nemesis bonus is the *hard override* on top. Extends cleanly: a 2nd unit per barracks later just adds spokes.

## Nations (light asymmetry)

The 1v1 is *near-symmetric, not a mirror*: both sides draw from the same roster and pentagon, but each plays a **nation** with a light trait package — a **domain lean** (a tech edge in one of Machine/Flesh/Energy) plus a small **people speciality** (an economic or unit perk). Kept deliberately light for MVP so it flavors play without exploding balance.

Illustrative examples: *Forgeborn* — Machine lean, Foundry units −10% Alloy · *Verdant* — Flesh lean, Biomass grows faster · *Aetherkin* — Energy lean, cheaper Aether channeling.

Balance methodology preserved: the balance sim runs **mirror matchups** (same nation both sides) to isolate unit/pentagon balance, and **cross-nation matchups** to test faction balance. Mirror is a measurement lens, not the play experience. These nations double as the conquest-map neighbor archetypes (below).

## Economy (single-tier domain mapping)

One resource per domain, each gathered a **different way** so it isn't three re-skins of the same fetch loop.

| Resource | Domain / Building | Powers | Acquisition | Pattern |
|----------|-------------------|--------|-------------|---------|
| **Alloy**   | Machine / Foundry     | Bulwark, Sentinel | **Mined** — finite deposits, workers harvest & return    | *expand*  |
| **Biomass** | Flesh / Gene-Vats     | Ripper, Ravager   | **Grown** — passive generation, storage-capped; build Vats | *invest* |
| **Aether**  | Energy / Aether Spire | Arclight          | **Channeled** — scarce, contested ley-wells you must hold  | *contest* |

**Why it's coupled to combat:** each resource's difficulty matches its units' pentagon role. Alloy funds the staple Machine backbone via map control. Biomass is passive and floor-setting, so the cheap Ripper swarm is *always* affordable. Aether is scarce and gates the harshest override (Arclight melting Bulwark), so the hardest counter is the hardest to mass. Net: the economy enforces the same "stay diversified" pressure the pentagon does.

**Buildings:** HQ (spawns Workers, Alloy drop-off) · Foundry (Machine barracks) · Gene-Vats (Flesh barracks) · Aether Spire (Energy barracks).

**MVP staging:** the first playable uses **Alloy only**. The three-resource domain system is the *first* economic expansion after the core loop proves fun.

## Win condition & match parameters

**Battle layer / MVP (1v1):**
- **Win by:** destroying the enemy **HQ** — clean, forces aggression, guarantees matches end (which the balance sim needs).
- **Players:** 1v1 vs scripted AI first; two **nations** with light asymmetry (see above). PvP arrives with lockstep (M6).
- **Map:** small, **seeded-procedural** — deterministic (same seed → same map, required for replay / lockstep / balance sim), seeded-*fair* (not a strict mirror; asymmetry comes from nations, not terrain).
- **Start:** one HQ + a few Workers + a nearby Alloy deposit (Alloy-only MVP).
- **Length target:** 5–8 min.

## Campaign layer (post-MVP vision)

The full game is a **campaign to dominate a map of nations** — you plus **5 neighbors of differing sizes and domain-flavored advantages** (a Machine-heavy neighbor opens with a Foundry/Alloy edge, etc.; neighbors *are* Nations, above). Each **round** is a battle-layer skirmish.

**Win = domination:** every neighbor subjugated or neutralized by the campaign's end. Three levers to reduce a rival (deliberately non-redundant):

1. **Conquer** — win the battle, destroy their HQ, take the nation.
2. **Ally** — form an alliance instead of fighting. Hard cap: **≤ 1 ally at any time** — a scalpel, not a win button; invites betrayal and switching.
3. **Destabilize** — use diplomacy to instigate conflict *between* other nations (proxy war), weakening a rival without committing your own army. *(Unlock: "2 diplomacy" — see Open questions.)*

Design note: the 1-ally cap + destabilize is the intended flavor — you can't befriend your way to victory, but you can turn the map against itself.

Gated behind: the battle loop proving fun and the pentagon balancing first. None of this is built until then.

**Plan:** `CAMPAIGN_PLAN.md` orders this work. C0 designs the nations and campaign rules on paper now, and building waits for the B5 Go.

## Campaign design (C0 draft, 2026-11-02)

> **Decided by the user (2026-11-02):**
> - Armies persist the way they do in Age of Empires: **units lost are lost, and survivors
>   carry over** to the next battle.
> - Nations hold **cities**, and battles are fought over cities.
> - A battle is won by **killing the enemy General** or **making the enemy retreat**.
>
> Everything else in this section is a **recommendation awaiting review**, marked *(rec)*.

### Map: nations made of cities

- A nation is a set of **cities** joined by roads, one of which is its **capital**. A nation's
  **size** is its number of cities.
- **The map is fixed and hand-authored** *(rec)*, because the story places the houses. It has
  16 cities in total. The AI's choices are still seeded and deterministic.
- **Your house starts with one city.** It is the heir's last holding, at the edge of the
  Rupture scar, and borders all three great houses.

| Nation | Cities | Capital |
|---|---|---|
| Your house | 1 | Cinderhold *(placeholder name)* |
| Forgeborn | 4 | — |
| Verdant | 4 | — |
| Aetherkin | 3 | — |
| Tollmarch | 2 | — |
| The Ashen Reach | 2 | — |

```
              [Forgeborn x4]
                    |
   [Tollmarch x2]--( F )--[Aetherkin x3]
                    |  \      |
              (Cinderhold)-- RUPTURE SCAR (impassable)
                    |  /      |
   [Ashen Reach x2]--( V )----+
                    |
               [Verdant x4]
```

The layout is illustrative only; the exact adjacency is decided in the C4 data.

### Armies and carry-over (decided)

- Each nation has one or more **armies** on the map. An army is a roster: its General, its
  units, and their current HP *(rec: HP carries too; units do not heal fully between
  battles)*.
- After a battle, **survivors return to the army roster, and the dead are gone**.
- **Reinforcement** *(rec)*: each city a nation holds adds a levy of Alloy, or of units
  trained at its barracks, every round. Size therefore matters, and lost armies can be
  rebuilt slowly. Without this, armies only ever shrink.
- **Healing** *(rec)*: an army that skips a round in one of its own cities recovers some HP.

### Campaign battles (decided: General and retreat)

- **A battle is a siege or field fight over one city.**
  - The **defender** holds the city: its HQ, any barracks the city has, workers, and the
    garrison.
  - The **attacker** arrives with its army roster, its General, and a few workers.
  - Both sides can still gather and train during the battle *(rec)*. This keeps the RTS core
    that B1–B4 balanced.
- **The General** is a unit, one per army, and is always on the field.
  - **If your General dies, you lose the battle.** The rest of your army survives and
    withdraws, so it still carries over.
  - The General fights, but is fragile enough that protecting him is a decision (stats to come
    from the sim).
  - **He buffs nearby units** *(decided by the user, 2026-11-02)*. Friendly units within his
    aura radius get a bonus. The radius, the stats it touches and its size are RON data, tuned
    by the sim. Keeping him close to the fight helps the army, and that is exactly what puts
    him at risk.
  - **Each nation's General gives a different kind of buff** *(decided by the user,
    2026-11-02)*. The aura matches the nation's doctrine, so its army fights differently near
    its General, and the opponent can read that (P3: the aura is visible on the field).
    Every aura is a list of stat modifiers in RON, so the kinds are data, not code. The one
    exception is a special scaling rule for the synthesists. Sizes are tuned by the sim.

    | Nation | Aura | Effect on nearby friendly units | How it plays |
    |---|---|---|---|
    | Your house | **Synthesis** | a bonus that grows with the number of *different domains* among the nearby units | rewards a mixed army; a pure army gets almost nothing |
    | Forgeborn | **Bulwark** | + defense and armor | hold the line; hard to break near the General |
    | Verdant | **Frenzy** | faster attacks (shorter attack interval) | short, violent trades; the swarm hits harder near him |
    | Aetherkin | **Focus** | + offense | few units, each hitting harder |
    | Tollmarch | **Logistics** | + move speed | arrives first, picks fights, gets out; strong for retreats |
    | The Ashen Reach | **Endurance** | slow HP regeneration | outlasts the enemy; survivors carry more HP between battles |

  - **Absorbing an aura** *(decided by the user, 2026-11-02)*. When you take a nation's
    capital, you gain **a new General carrying that nation's aura**, and he leads a new army
    of your own. Taking each capital therefore adds a General and an army, so you can field
    up to six. A Forgeborn General leads your Bulwark army; a Verdant one leads your Frenzy
    army.
    - *(rec)* **One General per army, and one army per side in a battle.** Auras never stack,
      so the choice is which General to send, not how many.
    - *(rec)* The new General's army starts small, raised from the conquered cities' levy.
      An absorbed General who dies is replaced after N rounds, like the AI's Generals.
    - The campaign sim must measure the snowball risk: each capital adds both cities and an
      army.
    *(rec)* An aura changes stats only. It never touches the nemesis bonus, so the pentagon's
    hard counters stay intact. C2's balance gate checks every aura against the pentagon and
    the 35–65% strength band.
  - *(rec)* A nation whose General dies gets a weaker replacement General after N rounds.
    **Your own General is the heir**: if he dies, the battle is lost and the heir is wounded
    for N rounds. The campaign is not lost.
- **Retreat** is an order you can give at any time.
  - Your units march for the map edge you came from, and every unit that reaches it carries
    over.
  - The city goes to the other side: a retreating attacker fails, and a retreating defender
    loses the city.
  - Retreat is how you save an army. A lost battle need not cost you the whole army.
  - The AI decides when to retreat with a data-driven threshold *(rec)*: the share of its
    army lost, or the enemy outnumbering it by a set ratio.
- **Destroying the city's HQ** also wins the battle and takes the city *(rec)*. The skirmish
  mode keeps today's rule that destroying the HQ wins.

### Rounds *(rec)*

1. **Order.** Each round, every nation acts once, in a seeded order. Its choices:
   - move an army along a road;
   - attack an adjacent city;
   - hold and heal;
   - a diplomatic action.
2. **Your battles are played.** Battles between two AI nations are **auto-resolved by the real
   headless battle sim**, which is deterministic and already built. The survivors it reports
   carry over exactly as yours do.
3. **Taking a capital** absorbs the nation: its other cities go to the conqueror. You also gain
   **its art**, meaning its speciality trait. You start with no art and end as the synthesis.
   Traits stack, so C0 caps how many.

### Diplomacy *(rec, refines the brief)*

- **Standing.** Each pair of nations has a standing: Hostile, Neutral, Tied or Allied.
- **Ally.**
  - At most one ally at a time.
  - Allies do not attack each other.
  - An ally can be asked to attack a shared enemy.
- **Betrayal.** Attacking your ally is possible, but every nation's standing towards you drops
  one step.
- **Destabilize.** This answers "2 diplomacy": you need **Tied standing with two nations**, and
  you can then incite them to war with each other. Both armies bleed in auto-resolved battles.
- **Notice.** Neighbours' hostility toward you grows with your size. At the start you are too
  small to be a target, which is the guile fantasy. Once you grow, the map turns on you.

### Win, lose and length *(rec)*

- **Win:** domination. Every neighbour is conquered, allied, or reduced to no cities.
- **Lose:** you lose your capital, or you have no army and no city able to raise one.
- **Length:** 15–25 rounds, about 2–3 hours, since you play roughly one battle a round.

### The nations (drafts; traits are intents, numbers come from the balance sim)

Each nation's battle personality maps to a strategy script. Each nation also gets an
Alloy-only stand-in trait, used until Biomass and Aether exist.

**Your house: the synthesists**
- **Art:** none. They practise the forbidden mix.
- **Cities:** 1.
- **Lean:** none. **Speciality:** **Synthesis**, a small bonus for units fighting beside units
  of another domain. It rewards the mixed army the pentagon already wants.
- **Alloy-only stand-in:** the same.
- **Disposition:** everyone distrusts you, and the Forgeborn hate you.
- **General's aura:** Synthesis: a bonus that grows with the number of domains mixed nearby.
- **Look:** ash grey and ember.

**Forgeborn: the zealots of Machine**
- **Story:** led the condemnation of your house.
- **Cities:** 4.
- **Lean:** Foundry units are cheaper. **Speciality:** sturdier buildings and walls.
- **Battle personality:** Bulwark and Sentinel, slow and defensive, they hold.
- **Alloy-only stand-in:** Foundry units cost x% less Alloy.
- **Disposition:** Hostile to you. They never ally with you until you are big enough to fear.
- **General's aura:** Bulwark: + defense and armor.
- **Look:** iron and brass.

**Verdant: the Flesh-shapers**
- **Story:** took the largest share of your holdings.
- **Cities:** 4.
- **Lean:** Biomass grows faster. **Speciality:** Gene-Vats train faster.
- **Battle personality:** swarms of Ripper and Ravager. They attack early and often.
- **Alloy-only stand-in:** Gene-Vats train x% faster.
- **Disposition:** greedy, and can be bought into an alliance.
- **General's aura:** Frenzy: faster attacks.
- **Look:** green and bone.

**Aetherkin: the wardens of Energy**
- **Story:** guard the ley-wells and fear another Rupture.
- **Cities:** 3.
- **Lean:** Aether is cheaper. **Speciality:** longer Arclight range.
- **Battle personality:** few, deadly Arclights behind a screen.
- **Alloy-only stand-in:** Arclight gets +x range.
- **Disposition:** the most reasonable house. It can be Tied if you avoid the ley-wells.
- **General's aura:** Focus: + offense.
- **Look:** violet and white.

**Tollmarch: the merchant city-state (minor, Machine-leaning)**
- **Cities:** 2.
- **Lean:** richer Alloy mines. **Speciality:** workers carry more.
- **Battle personality:** turtle; economy first.
- **Alloy-only stand-in:** +x worker carry.
- **Disposition:** neutral to all. They sell alliances, and switch sides for a better offer.
- **General's aura:** Logistics: + move speed.
- **Look:** gold and blue.

**The Ashen Reach: the Rupture remnant (minor)**
- **Cities:** 2.
- **Lean:** none. **Speciality:** scarred and defensive, with more building HP.
- **Battle personality:** defensive.
- **Alloy-only stand-in:** +x% building HP.
- **Disposition:** may ally with you if you seek the truth of the Rupture. This is a story
  thread.
- **General's aura:** Endurance: slow HP regeneration.
- **Look:** charcoal and red.

### Still open (C0)

- The names of your house and its capital.
- **Whether a General carries his own traits** (veteran Generals).
- **Siege rules:** does the defender's city keep its damaged buildings between rounds?
- **Trait caps** for absorbed arts.
- **Data shape:** `nations.ron` and `campaign.ron`, with fields only.

## Fun hypothesis (what we're actually testing)

**Hypothesis:** <e.g., "The counter pentagon + positioning produces meaningful tactical decisions in matches under 8 minutes, without economy micro dominating.">

**How we test it:**
1. **Play it by hand** — does a single fight feel like a decision, not a stat check?
2. **Balance sim** (headless AI-vs-AI): mirror matchups to isolate unit/pentagon balance, cross-nation matchups for faction balance; measure win-rate symmetry (~50% mirrors), match-length distribution, unit/build dominance. Balance-as-backtesting.

**Kill criteria:** <e.g., "if any unit wins >65% regardless of counter, the pentagon is broken">

## Non-goals (MVP scope guard)

NOT in the first playable: the entire **campaign layer** (nations-map, rounds, diplomacy, dominance), fog of war, PvP / multiplayer (M6), three resources (start with Alloy), tech tree, >~200 units, 3D, audio. Bespoke per-nation rosters (MVP nations are light trait packages only). Add only after the loop is proven fun.

## Open questions

- **Diplomacy — "2 diplomacy":** does destabilize unlock when you hold ties with *two states* (play them against each other), or at a second *tier* of a diplomacy track? (leaning: the former.)
- Nation asymmetry magnitude — how strong can a domain lean get before it distorts the pentagon?
- Theme names final? Machine / Flesh / Energy vs another flavor.
- Map generation: deposit placement rules; how much terrain variation on a seeded-fair map.
- <parking lot>

---

**Content-as-data:** every stat above is mirrored in `assets/data/*.ron` (`units.ron`, `resources.ron`; nations + campaign get their own files when built). Change the brief first, then the data.