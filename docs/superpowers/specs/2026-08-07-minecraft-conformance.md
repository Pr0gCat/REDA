# Conformance: check our circuits against the real game

## The problem in one sentence

A test that runs our compiler against our simulator cannot detect a rule both
of them get wrong.

Two such rules have already shipped. Levers were emitted without a `face`
property, so every one of them would have popped off on paste. Repeater
`facing` was implemented as the exact inverse of Minecraft's definition, so
every repeater we ever emitted points backwards and every circuit we ever
produced is dead in game. Both times the suite was fully green, because the
compiler and the simulator shared the assumption.

Every end-to-end test in this project has that shape. They catch layout and
routing bugs — which is most bugs — and they are structurally blind to this
class.

## What closes it

A vanilla Minecraft server, driven over RCON, running the actual redstone
implementation. Not a fixture, not a screenshot, not a person flying around:
an automated test that places a compiled circuit into a real world, flips its
levers, reads its lamps, and compares against the truth table.

Ground truth, repeatable, and cheap enough to run on every compiler change.

## Shape

1. `/forceload add` the circuit's footprint, so nothing sits in an unloaded
   chunk where redstone does not tick.
2. Place every non-air cell. We already have the palette and the block states;
   they become `/setblock` commands. `/fill` for runs of the same state where
   it pays.
3. Flip a lever with `/setblock`, wait for the circuit to settle, read each
   lamp with `/data get block`.
4. Compare against the same truth table the in-repo tests use.

Waiting is real time on 1.20.1 — there is no `/tick step` before 1.20.3. Our
circuits settle in about six seconds, so a sixteen-vector sweep is on the order
of two minutes. That is fine for something that runs deliberately rather than
on every `cargo test`.

## The requirement that gives this teeth

**It must be shown to fail on a build we know is wrong.**

`a1d50a4` has the inverted repeater convention. The harness must be run against
it and must fail, and the failure must be recognisably about repeaters — not a
generic "output wrong". Only then does passing on the fixed build mean
anything.

This is the same discriminating check used for the bit-packing vectors and the
sparse-propagation benchmark: a test that has never been observed to fail has
not been shown to test anything.

## Boundaries

- The EULA is the user's to accept. Set everything else up and leave
  `eula=true` unwritten.
- This does not replace the in-repo simulator tests. They are faster and they
  localise failures; this one says whether the whole thing is true.
- Start with `and4`. Seven gates, four levers, one lamp, 611 blocks — small
  enough to place quickly and to diagnose by hand when it disagrees. The
  decoder follows once `and4` is honest.
