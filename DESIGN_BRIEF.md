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
| House Vael (you) | 1 | Cinderhold |
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
- **Every army is led by one General.** More armies means more Generals, and several armies
  can converge on one battle to **overwhelm** a defender *(decided by the user,
  2026-11-02)*. A nation's Generals are:
  - its own first General (for you, the heir);
  - up to **3 absorbed Generals** from conquered capitals (decided);
  - **additional Generals of its own house**, raised at a city for a levy cost *(decided by the
    user, 2026-11-02; the cost is RON)*. These carry the house's own aura.

### Army movement on the map (decided: moving takes turns, and mobility depends on type)

- **An army marches only as fast as its slowest unit.** Each round an army gets **march
  points** equal to the lowest Speed stat in its roster. These are the same Speed values the
  battle uses: Bulwark 2, Ravager 4, Arclight 5, Worker 5, Sentinel 7, Ripper 9.
- **Every road has a length** in march points (RON). An army moves along roads, spending its
  points, and keeps any partial progress into the next round. Some examples:
  - a road of length 9 takes a Ripper army one round;
  - it takes a Bulwark army five rounds;
  - it takes a mixed army as long as its Bulwarks need.
- **This is the trade-off:** heavy armies hit hard but arrive late, and the enemy sees them
  coming. A fast raiding army can strike before the defender can reinforce. Splitting an army
  by speed is a real decision.
- *(rec)* Tollmarch's **Logistics** aura also adds march points on the map, not only speed in
  battle. Rough terrain or a road through the Rupture scar's edge can carry a length
  multiplier.
- **Converging:** a battle includes every army that is at the city, or adjacent to it with
  enough march points to arrive, in the round it starts. Armies arriving later join the
  **next round's assault**. Siege damage persists, so a siege can continue across rounds,
  with fresh armies arriving.

### Campaign battles (decided: General and retreat)

- **A battle is a siege or field fight over one city.**
  - The **defender** holds the city: its HQ, any barracks the city has, workers, and the
    garrison.
  - The **attacker** arrives with its army roster, its General, and a few workers.
  - Both sides can still gather and train during the battle *(rec)*. This keeps the RTS core
    that B1–B4 balanced.
- **The General** is a unit, one per army, and is always on the field.
  - **If your General dies, you lose the battle.** *(rec)* With several armies in one battle, a
    side loses when **all** its Generals are dead. Each dead General's own army withdraws,
    and the others fight on. The rest of your army survives and
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

  - **Veteran Generals** *(decided by the user, 2026-11-02)*. A General who survives a
    battle gains experience, and his rank rises.
    - *(rec)* Each rank strengthens his aura by one step: a larger radius or a larger bonus,
      as RON data per rank, with a cap on the top rank.
    - *(rec)* A replacement General starts at rank 0. Losing a veteran is therefore a real
      loss, which raises the stakes of protecting him and makes **retreating to save him** a
      real choice.
    - *(rec)* Winning earns more experience than retreating, and retreating earns more than
      dying.
  - **Absorbing an aura** *(decided by the user, 2026-11-02)*. When you take a nation's
    capital, you gain **a new General carrying that nation's aura**, and he leads a new army
    of your own. A Forgeborn General leads your Bulwark army, and a Verdant one leads your
    Frenzy army.
    - **At most 3 absorbed Generals** *(decided by the user, 2026-11-02)*.
      *(rec)* After 3, taking a further capital offers a swap: you may replace one absorbed
      General with the new one.
    - **This is the only thing conquest grants beyond the cities themselves.** There is no
      abstract "absorbed trait" (the earlier draft's "gain its art" is dropped by the user's
      decision). An aura reaches the field only through a General who is there.
    - **Several armies, and so several Generals, can fight in one battle**, so a side can
      overwhelm the other.
      *(rec)* Auras of different kinds stack. Auras of the same kind do not: a unit takes the
      strongest. For example, a unit standing near both a Bulwark and a Frenzy General gets
      both buffs.
    - *(rec)* The new General's army starts small, raised from the conquered cities' levy.
      An absorbed General who dies is replaced after N rounds at rank 0. The replacement
      still counts toward the 3.
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

### Siege damage persists *(decided by the user, 2026-11-02)*

- A city's buildings keep their HP between rounds. If the battle ends without the city
  falling, by retreat or by a General's death, the damage stays. A strong city can therefore
  be **worn down over several assaults**.
- *(rec)* **Repair.** A city that is not attacked in a round repairs a set amount of building
  HP. The repair rate is RON data, and the Forgeborn repair faster as part of their sturdier
  buildings.
- *(rec)* **Destroyed buildings stay destroyed** until rebuilt. Rebuilding is paid from the
  city's levy over rounds, or by workers during a battle.
- *(rec)* **A conqueror inherits the damage:** a city taken half-ruined stays half-ruined
  until it is repaired.

### Rounds *(rec)*

1. **Order.** Each round, every nation acts in a seeded order. **Each of its armies** may:
   - march along roads, spending its march points (see Army movement);
   - attack the city it has reached;
   - hold, which heals and lets the army join another army;
   - split into two armies (a General is needed for each) or merge with another.

   The nation also gets one diplomatic action per round.
2. **Your battles are played.** Battles between two AI nations are **auto-resolved by the real
   headless battle sim**, which is deterministic and already built. The survivors it reports
   carry over exactly as yours do.
3. **Taking a capital** absorbs the nation. Its other cities go to the conqueror, and you
   gain a General with its aura, up to 3 (see Absorbing an aura). No trait transfers: a
   nation's speciality stays with that nation's cities and Generals.

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

**Perks apply only to the nation's own armies** *(decided by the user, 2026-11-02)*. A nation's
lean and speciality affect only armies that nation fields, and the cities it holds. They never
transfer through conquest. An absorbed General brings his **aura** to your army, but not his
old nation's perks: a Forgeborn General leading your army does not make your Foundry units
cheaper.

**House Vael: the synthesists (you)**
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

### Names (chosen 2026-11-02)

The user delegated the naming.
- **House Vael** is the player's house. *Vael* is close to "veil": the truth of the Rupture
  is veiled, and the record is ash.
- **Cinderhold** is its capital and last holding. It is the one city the pure houses left
  standing after burning the rest, at the edge of the Rupture scar.
- The heir's General is **the Heir of Vael**.

C0 has no open questions left. Items marked *(rec)* stand as the design until the user
changes them, or until a sim or play test shows they fail.

## Campaign data shape (C0 draft, 2026-11-02)

These are drafts of the content files C1–C4 will load. **Nothing loads them yet.** They live
here, not in `assets/data/`, until the milestone that reads them, so the shipped content and
its fingerprint stay unchanged. Every number is a placeholder for the sims to tune. The types
follow the existing RON style (`units.ron`, `strategies.ron`).

### `units.ron` additions (C1): Generals and auras

```ron
// One General type per aura kind. A General is a unit: the 4-stat model, a barracks of none,
// and an aura. If it dies, its side loses the battle (C1).
generals: [
    (
        id: "general_synthesis", name: "Heir of Vael",
        speed: 5, offense: 5, defense: 6, armor: 4,
        aura: (
            radius: 220.0,                          // world units
            modifiers: [ (stat: Offense, add: 1), (stat: Armor, add: 1) ],
            scaling: PerDistinctDomain(max: 3),     // Synthesis only: x1 per domain present nearby
        ),
    ),
    (id: "general_bulwark",   name: "Forge-Marshal", speed: 3, offense: 4, defense: 8, armor: 6,
     aura: (radius: 200.0, modifiers: [ (stat: Defense, add: 2), (stat: Armor, add: 2) ], scaling: Flat)),
    (id: "general_frenzy",    name: "Brood-Mother",  speed: 7, offense: 6, defense: 5, armor: 2,
     aura: (radius: 200.0, modifiers: [ (stat: AttackTicks, mul: 0.8) ], scaling: Flat)),
    (id: "general_focus",     name: "Ley-Warden",    speed: 5, offense: 7, defense: 4, armor: 2,
     aura: (radius: 240.0, modifiers: [ (stat: Offense, add: 2) ], scaling: Flat)),
    (id: "general_logistics", name: "Quartermaster", speed: 8, offense: 3, defense: 5, armor: 3,
     aura: (radius: 260.0, modifiers: [ (stat: Speed, add: 2) ], scaling: Flat)),
    (id: "general_endurance", name: "Ash-Keeper",    speed: 4, offense: 4, defense: 7, armor: 4,
     aura: (radius: 200.0, modifiers: [ (stat: RegenPerSecond, add: 1.0) ], scaling: Flat)),
],
// Ranks scale the whole aura (C4). A replacement General starts at rank 0.
general_ranks: [
    (xp: 0,  aura_mult: 1.00),
    (xp: 3,  aura_mult: 1.25),
    (xp: 8,  aura_mult: 1.50),   // top rank: the cap
],
// The AI's battle behaviour around its General (C1).
mvp_general_ai: (guard_radius: 150.0, retreat_at_army_lost: 0.6),
```

The rule from the brief: no aura modifies the nemesis multiplier, and content validation
rejects one that tries.

### `nations.ron` (C2)

```ron
(
    nations: [
        (
            id: "house", name: "House Vael",
            domain: None,                               // the synthesists hold no art
            colour: (0.45, 0.42, 0.40), accent: (0.95, 0.45, 0.15),
            general: "general_synthesis",
            strategy: "house",                          // a strategies.ron entry: the AI personality
            traits: [],                                 // its edge is the Synthesis aura
            disposition: (towards: [], ally_bias: 0, betrayal_tolerance: 1),
        ),
        (
            id: "forgeborn", name: "Forgeborn",
            domain: Some("machine"),
            colour: (0.35, 0.33, 0.30), accent: (0.80, 0.62, 0.25),
            general: "general_bulwark",
            strategy: "forgeborn",
            // The Alloy-only stand-in trait; it becomes the real lean in C3.
            traits: [
                (applies_to: Barracks("foundry"), stat: AlloyCost,   mul: 0.90),
                (applies_to: Buildings,           stat: RepairRate,  mul: 1.50),
            ],
            disposition: (towards: [ ("house", Hostile) ], ally_bias: -2, betrayal_tolerance: 0),
        ),
        // verdant, aetherkin, tollmarch, ashen_reach follow the same shape (see the nation sheets)
    ],
)
```

### `campaign.ron` (C4)

```ron
(
    cities: [
        (
            id: "cinderhold", name: "Cinderhold", owner: "house", capital: true,
            position: (0.0, 0.0),                       // map-screen layout only
            buildings: [ (building: "hq", hp: None), (building: "foundry", hp: None) ],  // None = full
            levy: (alloy: 60, units: []),               // per round
            local: None,                                // e.g. Some(RichAlloy) for a mining town
        ),
        // ... 15 more
    ],
    roads: [
        (from: "cinderhold", to: "forge_gate", length: 9,  terrain: Plain),
        (from: "cinderhold", to: "scar_edge",  length: 12, terrain: Scarred),   // terrain multiplies length
    ],
    terrain: [ (kind: Plain, length_mult: 1.0), (kind: Scarred, length_mult: 1.5) ],
    start_armies: [
        (nation: "house", general: "general_synthesis", at: "cinderhold",
         units: [ (unit: "sentinel", count: 3), (unit: "ripper", count: 4) ]),
        // ...
    ],
    rules: (
        march_points: SlowestUnitSpeed,     // per round; Logistics adds its Speed bonus
        max_absorbed_generals: 3,
        raise_general_cost: (alloy: 400),   // a house General (decided: allowed)
        ally_cap: 1,
        betrayal_standing_drop: 1,
        destabilize_requires_tied: 2,
        repair_per_round: 300,              // building HP; unattacked cities only
        heal_per_round: 0.20,               // share of max HP; armies holding in their own city
        xp: (win: 3, retreat: 1, death: 0),
        general_replacement_rounds: 3,
        notice_per_city: 1,                 // neighbours' hostility toward you
        round_cap: 25,
    ),
)
```

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