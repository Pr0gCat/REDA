# Minecraft fidelity: pin every rule to its source

## Why

Two bugs of the same shape have already shipped, and a full green test suite
saw neither.

**Levers.** We never emitted the `face` property. Minecraft's default is
`wall`, our layout puts levers on top of a block, so every lever in every
`.litematic` we produced would have popped off as a dropped item on the first
block update. No test could see it: our simulator's lever is lit-or-unlit with
no concept of attachment.

**Repeaters.** Our `facing` convention is the exact inverse of Minecraft's. The
wiki defines it as "the direction from the output side to the input side"; we
implemented input at `facing.opposite()`. Every repeater we have ever emitted
points backwards in game, so a pasted circuit is dead. All 216 tests passed,
because the compiler and the simulator share the same wrong assumption and
agree with each other perfectly.

That second one is the whole problem in one sentence:

> **A test that runs our compiler against our simulator cannot detect a rule
> both of them get wrong.**

Every end-to-end truth-table test in this project has that shape. They are
valuable — they catch layout and routing bugs, which is most bugs — but they
are structurally blind to a shared misreading of Minecraft. And "does the
circuit work when pasted into the game" is the entire point of the project.

## What this task is

An audit. Every place the codebase encodes a rule about how Minecraft behaves
gets three things:

1. **A citation.** The authoritative statement, quoted, in a comment beside the
   code, with a link. Not "redstone works like this" — the actual wording.
2. **A direct test.** An assertion of the rule itself, not of a circuit that
   happens to depend on it. It must fail if the rule is inverted or dropped.
3. **A verdict.** Match, or divergence — and divergences get fixed or recorded
   with a reason.

## What counts as a rule

Anything where the code's behaviour is determined by how Minecraft works
rather than by our own choice. Three layers, in descending order of blast
radius:

**The emit path** — what we write into a `.litematic`. A divergence here means
the pasted circuit is broken, silently, with no local symptom at all. This is
where both known bugs lived. We currently place `Solid`, `RedstoneWire`,
`Repeater`, `WallTorch`, `Lever` and `Lamp`; every property each of those needs
in order to be the block we intend must be emitted and correct, including
properties we never read back ourselves.

**The simulate path** — connection rules, power rules, strength decay, diode
delays, what a torch powers and does not power, what supports what. A
divergence here means our proof is of the wrong theorem.

**The load path** — parsing a `.litematic` somebody else produced. A divergence
here corrupts anything we import, including any future macro cell.

## Known suspects

Not exhaustive; the audit's job is to find what this list misses.

- Comparator `mode` (compare / subtract) and repeater `locked` survive only as
  strings in `extra_properties`; nothing reads them.
- Block entities are dropped entirely on load. A comparator's output signal
  lives there, and `power_emitted_by` currently hardcodes 15 for a powered
  comparator.
- `flags_of` dispatches on `state.kind` for some blocks and `state.name` for
  others; a block whose `kind` is `Other` gets zero flags regardless of name.
- `FULL_BLOCK_SUFFIXES` misses `minecraft:bricks` and `minecraft:terracotta`
  (no underscore prefix). `"minecraft:carpet"` is not a real block ID.
- The `lit` -> `powered` fallback is wrong for 1.21 copper bulbs (noted in code;
  1.20 has no such block).
- Quasi-connectivity, torch burnout and locational behaviour are not modelled
  at all. We place none of the affected blocks today, so these are latent
  rather than wrong — but that should be a recorded decision, not an accident.
- Nothing in the repo has ever loaded a `.litematic` that Litematica produced.

## The one thing that would have caught both

A `.litematic` exported from a real game, committed as a fixture, loaded and
compared against what we generate for the same structure.

It cannot be created without the game, so it is not in this task's scope. But
say in the report what such a fixture would need to contain to be worth the
most — which blocks, in which orientations — so that capturing one later is a
five-minute job rather than a design exercise.

## Boundaries

- Fix divergences on the emit and simulate paths.
- Record, don't fix, anything requiring block-entity support; that is queued
  separately and is large.
- Do not change layout, routing, or timing. Circuit sizes and settle times
  must not move except where a genuine rule fix forces it — and if one does,
  that is the headline of the report, not a footnote.
