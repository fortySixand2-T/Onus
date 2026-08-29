# <Game Title> — Design Brief

**One-line pitch:** <"X meets Y" — e.g., "AoE-style economy with Total War positioning, in 5-minute matches">

> Status: DRAFT — pillars, roster, stats, counters, economy, win condition, nations, and campaign layer drafted; hook / core loop / fun-hypothesis still open.
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

<One paragraph: the player fantasy and its emotional core.>

Theme: one faction wages war through three technological **domains** — **Machine**, **Flesh**, **Energy** — each its own barracks. (Rename freely.)

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
| **Bulwark**  | Foundry (Machine)     | walking fortress        | 2 | 4 | 9 | 9 | 110 Alloy   | Ravager  |
| **Sentinel** | Foundry (Machine)     | agile war-frame         | 7 | 6 | 5 | 5 | 70 Alloy    | Ripper   |
| **Ripper**   | Gene-Vats (Flesh)     | fragile swarm           | 9 | 6 | 3 | 1 | 40 Biomass  | Arclight |
| **Ravager**  | Gene-Vats (Flesh)     | regenerating bio-titan  | 4 | 7 | 8 | 4 | 90 Biomass  | Sentinel |
| **Arclight** | Aether Spire (Energy) | armor-melting channeler | 5 | 9 | 2 | 2 | 80 Aether   | Bulwark  |

Economy unit (from HQ, not a barracks): **Worker** — gathers and builds, cheap, no combat role.

## Counter pentagon

Each unit deals a bonus against exactly one prey and is prey to exactly one predator — a closed loop, so every counter has its own counter.

**Nemesis bonus:** **+30% damage, *ignoring armor***, vs the one prey.

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
