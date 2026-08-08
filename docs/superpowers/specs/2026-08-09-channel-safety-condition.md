# The channel router's real safety condition, derived from `dust_reach`

## Why this exists

`wip/wired-or-genlib` (`bb37141`) made an OR compile to a bare wire join
instead of a NOR pair, and the Verilog circuits' truth tables broke. Reduced
to a Yosys-independent two-level merge-of-merges, the failure showed spurious
power at a junction whose real inputs were all dark, on fresh state — neither
decay nor a settling artefact. That agent's conclusion:

> The channel/track-sharing algorithm's safety argument for letting nets
> share physical proximity implicitly assumes every socket ends in a
> mandatory, isolating repeater — which is exactly what a bare merge socket
> doesn't have.

`src/compile/mod.rs` keeps five spacing constants — `COLUMN_CLEARANCE`,
`TRACK_SPACING`, `ENTRY_OFFSET`, `TRACK_SHARE_GAP`, `SLOT_PITCH` — and the
router's correctness rests on them. None of them was derived; they were
chosen generously enough that nothing collided, and every circuit passing
since is evidence they are sufficient, not evidence they are right.
Meanwhile `3791c52` built machinery that can actually answer the question —
`redstone::simulator::connectivity::dust_reach`, which says exactly which
cells would join a given cell's net — but the channel router never consults
it; it consults the constants.

This document derives the real condition from `dust_reach`, scores each
constant against it, quantifies the missing-repeater question, and writes
the safety condition down as a checkable rule. **No router behaviour is
changed here** — every placement/routing decision is byte-identical to
`b03a354` (confirmed below); the only additions are one spec (this file) and
one test file, `tests/spacing_derivation.rs`, that confirms every numeric
claim against the real `Simulator`.

## The derivation

`dust_connections`/`dust_reach` (`src/redstone/simulator/connectivity.rs`)
have exactly three cases, and all three are bounded the same way:

```rust
pub fn dust_reach(world: &World, from: Position, direction: Facing) -> Connections {
    let mut reach = Connections::none();
    let neighbour = from.offset(direction);          // same layer: unconditional
    reach.push(neighbour);
    let neighbour_steps = supports_dust_step(world, neighbour);
    if neighbour_steps && !is_conductive(world, from.up()) {
        reach.push(neighbour.up());                  // climb
    }
    if !neighbour_steps {
        reach.push(neighbour.down());                // descend
    }
    reach
}
```

Every target `dust_reach`/`dust_connections` can ever produce is
`from.offset(direction)`, or that same cell shifted up or down by exactly
one — i.e. **horizontal Chebyshev distance exactly 1 along a single cardinal
axis, and vertical distance 0 or 1**. There is no case for distance 2, no
case for a horizontal *and* vertical step at once (no true diagonal), and no
case that depends on anything other than the immediate neighbour's own
support/conductivity. This is exhaustive — the function has no other
branches — so the following is not a heuristic, it is a direct reading of
the code:

> **Safety condition.** Two conductor cells belonging to different nets may
> never be placed such that one is exactly one cardinal step from the other
> in a single horizontal axis (the other axis equal) while their Y differs
> by 0 or 1 — *unless* the cell being approached is a block whose
> `BlockKind` is not, and will never become, `RedstoneWire` (a repeater is
> the only such block this router ever places), in which case every axis
> **except its own facing** carries no restriction at all.

Two consequences fall out immediately:

1. **A gap of 2 in the shared horizontal axis is both necessary and
   sufficient** to rule out every case at once, regardless of Y, regardless
   of what supports/conducts nearby: at distance ≥ 2 no branch above can
   ever fire, and at distance 1 the same-layer branch always fires
   unconditionally. This is not a new fact — `COLUMN_CLEARANCE`'s own doc
   comment already says "one empty block between two columns is already
   enough — but only exactly enough" — but it is now shown to be exactly
   what the code proves, not merely what has held up in practice.
2. **A repeater is a real firewall, not a probabilistic one.** `is_dust`
   checks `BlockKind::RedstoneWire` specifically (`connectivity.rs:24`), so
   a repeater can never be the *target* of the same-layer rule. And
   `verify_connectivity` only ever starts its BFS from actual
   `RedstoneWire` cells (`world.positions_of(BlockKind::RedstoneWire)`,
   `mod.rs:3380`) and only ever walks through `dust_connections`' targets —
   so a repeater is never the *source* of a join either, in either
   direction. Its non-facing sides are structurally invisible to the join
   mechanism, permanently, not merely "unlikely to matter".

Confirmed against the real `Simulator`, not just read off the code, in
`tests/spacing_derivation.rs`:

- `two_bare_dust_probes_touching_directly_cross_talk` — two independent
  lever-driven dust cells one cell apart (coordinate distance 1) cross-talk:
  the second lever's own probe reads power that can only have come from the
  first lever's net.
- `two_bare_dust_probes_one_cell_apart_do_not_cross_talk` — the same rig at
  coordinate distance 2 reads exactly zero. This is the whole boundary
  `COLUMN_CLEARANCE`/`TRACK_SPACING`/`TRACK_SHARE_GAP` all reduce to.
- `a_repeater_terminated_socket_does_not_cross_talk_even_when_a_foreign_net_touches_its_side`
  — the *same* touching geometry as the first test, except the cell a
  foreign, fully-powered net touches is a repeater terminating a route
  instead of bare dust: zero leak, on either side of driving the real net
  through it afterwards to confirm the rig itself works.

## The missing-repeater question, quantified

> What changes when a socket has no terminating repeater?

**The required clearance from a foreign net drops from coordinate distance 2
to coordinate distance 1, on every side but the repeater's own facing axis —
one full cell, a 50% reduction in required gap, and it goes to zero on the
sides that matter for channel sharing.** This is not "a repeater helps" in
some vague sense; it is the entire reason the constants have gotten away
with being uniform for years despite covering both cases: most sockets in
this project's reference circuits *do* end in a mandatory repeater, so the
side clearance they actually needed was 0, and `COLUMN_CLEARANCE = 2`
(already the correct minimum for bare dust — see below) was never actually
tested at its own boundary by them. A bare merge socket is the first
geometry in this project that needs the full, tight minimum on every
approach, because it has no repeater anywhere to absorb the difference.

The current `HEAD` (`b03a354`) already gets this right for the one case that
matters today: `merge_branch_is_bare` (`mod.rs:2625`) isolates a shared
branch with a real repeater, and `reserve_columns`/`row_body_zones` never
special-case merge vs. non-merge gates — every approach column, merge or
not, is already held to the same `COLUMN_CLEARANCE = 2`, which (see below)
is the actual derived minimum, not a generous guess that happens to cover
bare dust too. The `wip` branch's bug was in a version of this code that
predates that fix, not a live bug in `HEAD`.

## Verdict on each constant

| constant | value | derived minimum | verdict |
|---|---|---|---|
| `COLUMN_CLEARANCE` | 2 | 2 | **Exactly right.** Matches the derivation exactly; already argued in its own doc comment, now shown to follow from `dust_reach`'s exhaustive case list, not merely observed to work. |
| `TRACK_SPACING` | 5 | 2 | **Over-generous by 3.** Two tracks in a reverted channel (`layout_z`, `track_count[channel] > BAND_CAP`) are flat, same-`Y` (`band_y(0)`), parallel dust runs — the identical geometry `COLUMN_CLEARANCE` covers, just along Z instead of X. The doc comment already admits this: "the original justification... no longer applies to a dust staircase... left at its old, empirically-safe value rather than retuned." Confirmed this is not merely theoretical: instrumenting `layout_z` (temporarily, not committed) while running `tests/reference_circuits.rs` shows real channels with `track_count` up to 10 (the Verilog `seven_segment`/`segment_a` circuits), so a channel exists in this project's own reference set whose reverted depth is `TRACK_SPACING * 10 = 50` Z-cells where the derived minimum would cost `2 * 10 = 20` — 30 cells of pure waste in that one channel alone. |
| `TRACK_SHARE_GAP` | 4 | 2 | **Over-generous by 2.** Two nets sharing one track are colinear, same-`Y`-same-`Z` dust spans with a gap in `X` — the same rule again, applied along the axis the track itself runs. `lay_track` never places anything in the gap cells for either net (each `lay_track` call only writes its own `[lo, hi]` span), and `plan_track_run`'s tap-avoidance rule keeps a repeater off the boundary cell (`hi`/`lo` are always taps), so there is no repeater-forward-injection risk to justify the extra 2 either. |
| `ENTRY_OFFSET` | 4 | not fully proven | **Insufficient in some case — confirmed.** See below: this is not the constant that failed, but the mechanism built on top of it (`resolve_bypass_and_geometry`'s widened pass) has a real, reproducible hole that the constant's own doc comment already half-admits ("this router's spacing proof only actually covers a column's clearance from feed-through candidates and from other members of the same net — it never checks that one gate's own output column and an unrelated gate's socket-approach column... land `COLUMN_CLEARANCE` apart"). |
| `SLOT_PITCH` | 14 | derived from the other two | **Exactly right, conditionally.** Its own doc comment derives it correctly from `ENTRY_OFFSET` and `COLUMN_CLEARANCE` (`14` leaves a 5-wide inter-row gap, exactly `2 * COLUMN_CLEARANCE + 1`, room for one clear feed-through column). It inherits `ENTRY_OFFSET`'s own unproven status rather than adding a new one. |

`BYPASS_MAX_DISTANCE = 2 * COLUMN_CLEARANCE - 1 = 3` is the one constant in
this file that was already derived, explicitly, from `COLUMN_CLEARANCE` — it
gets no separate row in the table because there is nothing left to check;
its own doc comment is the proof.

## A confirmed bug: `resolve_bypass_and_geometry`'s widened pass

Random NOR-only netlist stress testing (feed-forward, no merges, no
adversarial construction — plain `Gate`s built the same way every reference
circuit already is) finds a real, reproducible `ConnectivityViolation` on
valid, non-cyclic, fully-driven input. Minimal reproduction:

```rust
let gates = vec![
    Gate { name: "g0".into(), inputs: vec!["in0", "in3", "in2"], output: "g0".into(), is_merge: false },
    Gate { name: "g1".into(), inputs: vec!["in1", "in3", "in4"], output: "g1".into(), is_merge: false },
];
// inputs: in0, in1, in2, in3, in4; outputs: g0, g1
```

`compile()` returns
`ConnectivityViolation { cell: (28, 1, 19), found_net: "in1", expected_cell: (28, 1, 17), expected_net: "in2" }`
— two **primary input** nets' own dust physically joined. No merge, no
fan-out, nothing exotic: this is the plainest netlist shape the project has.

**Root cause.** `in1` and `in2` are both levers in row 0, both far enough
from their sink's approach column to fail `compute_bypass`'s proven-safe
check (`BYPASS_MAX_DISTANCE = 3`) but within `resolve_bypass_and_geometry`'s
widened query range (`BYPASS_QUERY_MAX_DISTANCE = 12`) — both at exactly
distance 12, the boundary. Both are therefore evaluated in the widened loop
(`mod.rs:3188`), and both are approved, because each candidate's horizontal
jog — the new east-west run from the lever's pin to its `exit_x`, laid at
the row's own Z — is checked only against `probe_reservation`, a **snapshot
of the baseline (proven-safe-only) geometry taken once before the loop
starts** (`mod.rs:3181`). That snapshot has no entry for either candidate's
own jog, because in the baseline neither of them jogs at all — they go down
a ramp into a track instead. So neither candidate's check can see the
*other* candidate's prospective jog, and both new horizontal runs — which
happen to overlap in X because both pins sit in the same row at the same Z
— get written for real.

The function's own doc comment argues this can't happen:

> Every column this router ever places is, by construction, at least
> `COLUMN_CLEARANCE` from every other column in the same channel... So [the
> baseline reservation] is exactly as informative as any "final"
> reservation would be, and every candidate can be checked against the one
> baseline pass and promoted all at once — no candidate's promotion can
> invalidate another's answer.

This proof is real but incomplete: it covers the **vertical columns** a net
occupies (fixed X regardless of bypass status, so the baseline already knows
about them) but says nothing about the **horizontal jog** a widened bypass
candidate introduces at its own row's Z — a cell range that does not exist
in the baseline at all, for *any* candidate, so two candidates that both
introduce one at the same Z can defeat each other's check without either
one being individually wrong.

This is a distinct bug from the five constants above — it is a staleness
gap in a *check*, not a wrong number — but it is the same disease: an
untested assumption standing in for a derivation, in the same "channel
safety argument" family the task asked about. It was found, confirmed via
`compile()`'s own `ConnectivityViolation` (which walks the exact same
`dust_connections` the real `Simulator` uses to settle a circuit — the
identical mechanism, not a proxy for it), and is reported here rather than
fixed, per this task's scope. It does not affect any of this project's five
reference circuits (none of their nets land at this exact distance-12
coincidence), which is why 314 tests have stayed green through it.

## Writing the rule down

> **The dust-adjacency safety condition.** For any two conductor cells
> belonging to different nets (or different `MergeGroups` groups), it must
> never be the case that they differ by exactly 1 in one horizontal axis and
> 0 in the other, with a Y-difference of 0 or 1. Equivalently: **maintain a
> horizontal gap of at least 2 between any two different-net conductor
> lines, at every Y** — except across a **repeater's own non-facing sides**,
> where the gap may be 0, because a repeater's `BlockKind` can never satisfy
> `is_dust` and `verify_connectivity`'s BFS never starts from or walks
> through one.

This is the fact both the `wired-or-genlib` bug and `BAND_HEIGHT = 2`
(`layout_z`'s "skip-band edge" argument, already using exactly this
distance-1-cannot-bridge property to justify letting two Y-bands share one Z
line) turned out to depend on, and it has never been written down as a
single rule before now — it existed as two independent, unconnected proofs
in two doc comments (`COLUMN_CLEARANCE`'s and `layout_z`'s), each re-deriving
the same fact about `dust_reach` without citing the other.

**Is it checkable mechanically? Yes, and partially already is.** Three
things exist at three different strengths today:

1. **Derivable, not checked, for the fixed geometry** (`reserve_columns`'s
   direct `used_columns.insert(...)` calls, `row_body_zones`, `SLOT_PITCH`'s
   arithmetic): the router trusts that `SLOT_PITCH`/`ENTRY_OFFSET`'s
   formulas keep every fixed column ≥ 2 apart, but never asks the
   `Reservation` to confirm it before writing. This is exactly the gap
   `ENTRY_OFFSET`'s own doc comment names.
2. **Checked against a stale snapshot** (`resolve_bypass_and_geometry`'s
   widened pass): a real `Reservation` query exists, but it does not see
   sibling candidates decided in the same pass — the bug above.
3. **Checked for real, unconditionally, after the fact**
   (`verify_connectivity`): this is the actual mechanical version of the
   rule already in the codebase — it re-derives the *effect* of the
   condition (every dust network partitions correctly) by walking
   `dust_connections` over the finished world. It is sound and complete for
   *detecting* a violation; it is simply not consulted early enough to
   *prevent* one from ever being computed for two candidates that then get
   written seemingly-independently.

**To become a fifth invariant**, the condition would need to move from (3)'s
"catch it after every cell is written" to something checked **while
placing/reserving**, the same way `reserve_feedthrough`'s `fits` closure
already checks a single feed-through candidate's X against every existing
column with exactly this ≥ `COLUMN_CLEARANCE` rule. Concretely: every place
that currently trusts arithmetic or a stale snapshot (`reserve_columns`'s
direct inserts, `resolve_bypass_and_geometry`'s widened loop) would instead
maintain one running `Reservation` — including horizontal jog cells, not
just columns — updated after each candidate is decided, and query it before
approving the next one. That is a real, scoped change (it touches
`resolve_bypass_and_geometry`'s loop body and `reserve_columns`'s two
`.insert()` call sites), not a reinterpretation of data already collected —
which is exactly why it is a *fix*, out of scope for this document, and not
merely a rewording of an existing check.

## What was and was not touched

- `tests/spacing_derivation.rs` (new): three tests confirming the derivation
  and the repeater quantification against the real `Simulator`.
- This file (new).
- No constant, no placement decision, no routing decision changed. Verified:
  `cargo test --release` — 317 passed (314 pre-existing + 3 new), 0 failed,
  0 ignored. `cargo clippy --all-targets -- -D warnings` — clean. Reference
  circuit settle ticks unchanged (`and4` 24, `full_adder` 62, `segment_a`
  82 game ticks, read directly from `cargo test --release --test
  reference_circuits -- --nocapture`), confirming the router's own output is
  byte-identical to before this investigation.
