# IO terminals: a port is a coordinate someone else owns

**Status: DESIGN, approved in discussion 2026-08-30.** Motivating failure: a
seven-segment display is *defined* by where its outputs sit -- seven signals
arranged as a digit glyph -- and REDA today cannot be told where any IO goes.
The decoder compiles and verifies, but its outputs land wherever the placer
put their driving gates, so the one circuit whose meaning is geometric cannot
be built as the thing it names.

## The contract (settled first, everything else follows)

A **pin** is `port name -> (Anchor, outside: Facing)`: an absolute cell plus
the horizontal direction that faces the caller's world. A **terminal** is what
REDA builds at a pinned cell: one redstone dust on its own floor block.
Nothing else -- no lever, no lamp, no repeater in the interface.

The whole promise, per pinned port:

- **Output**: when the signal is logically high, the dust at the pinned cell
  carries strength > 0; when low, 0. No particular strength is promised.
- **Input**: the caller powers the pinned dust by any means, at any strength
  >= 1, and the circuit reads it as high; unpowered is low. REDA never
  assumes what drives it.
- **The outside cell** -- the pinned cell's neighbour in the `outside`
  direction -- ships empty. No block, no wiring, no other net's anything.
  That is where the caller attaches, and internal routing approaches the
  terminal from the other sides only.

What REDA deliberately does not know: what hardware sits beyond the boundary.
Levers and lamps stop being part of a port's definition and become what they
always physically were -- test fixtures. **Unpinned ports keep today's
behaviour exactly** (lever bodies for inputs, lamp under the producing gate's
pin for outputs); every existing circuit and test is untouched by default.

## The mechanism: terminals are pinned bodies

The one existing position mechanism, `PortPlacements`
(src/compile/planner.rs:2891), already threads pins through starting layout,
the relaxation solve (struck from the matrix), the separation projection
(neighbours pay), snap (returned exactly), and `try_move` (`PortIsPinned`).
Its vocabulary changes from a bare `Anchor` to `(Anchor, outside: Facing)`,
and its *meaning* changes: pinning a port no longer pins the body that
happens to realise it (the lever, or the producing gate) -- it declares a
terminal. The old pin-the-gate-by-output-signal behaviour has no shipping
caller and its tests are re-pointed at terminal semantics.

**Input terminal.** The port's body stops being a lever. Footprint: floor
block, dust at the pinned cell, and a normalizing repeater one cell inward
on one of the three non-outside sides (which side is a realisation variant,
swept like a gate facing). The repeater exists because the contract admits
external strength 1, and dust at strength 1 propagates nowhere: whatever
arrives is reshaped to 15 by construction, so the net's source strength is
geometry again -- the same move that made merge strength pure geometry in
the second campaign. The route source is the repeater's output pin.

**Output terminal.** A new 1-cell body (dust + floor) added as an extra sink
on the declared output's net. The router treats it exactly like a gate
socket: an approach discipline, a strength requirement (>= 1 on arrival --
the strength-aware arm reasons about this natively; a distance-only plan
that starves it is caught by strength verification and the portfolio's
second arm retries). The producing gate stays free; the net simply has one
more place it must reach. No lamp is placed for a pinned output.

Because terminals are bodies with springs to their net partners, the placer
pulls the circuit *toward* the pinned coordinates during relaxation instead
of the router discovering the distance after placement froze. Pinned anchors
suppress the whole-layout drift translation (already true today), and world
sizing grows to cover every pin plus margin.

**Refused by name, before planning:** a pin outside any growable world
bound; two terminals of different nets in dust-connecting adjacency (the
caller left no gap -- a glyph needs one empty cell between segments); a
terminal whose outside cell collides with another terminal or its claimed
cells; a pin for a port name the netlist does not declare. Output pins are
given by display label (`a`..`g`) and resolved through the existing output
labels channel, because asking the caller for internal `gN` names would be
absurd.

## Verification follows the contract

- `verify_signal_strength`'s declared-output pass walks to the terminal cell
  instead of a lamp cell for pinned outputs: the net must deliver strength
  >= 1 at the pinned dust. Same check, new address.
- The lamp invariant (`LampNotAtFixedOffsetFromTorch`) and the lever check
  in `equivalence` apply only to unpinned ports, which still have lamps and
  levers.
- A new invariant: every pinned port's cell holds its terminal dust, its
  outside cell is air, and the outside cell is adjacent to no other net's
  cells.
- The truth-table battery drives a pinned input by placing a temporary
  source in the reserved outside cell (it is guaranteed empty) and reads a
  pinned output as strength > 0 at its terminal cell. Strength-1
  independence is pinned by a unit test on the input terminal footprint
  (drive the dust at 1, observe 15 past the repeater), not by running whole
  batteries at every strength.

## Plumbing

`compile_planned` already takes `PortPlacements`; `compile_grown` gets the
parameter it always plumbed internally and hard-coded to default
(src/compile/mod.rs:7188 -- the growth loop underneath threads placements
end to end already). `compile()` and the legacy emitter are not touched.
`build_circuit` learns `--pins <file.json>`:

    {"inputs":  {"d0": {"at": [x,y,z], "outside": "south"}, ...},
     "outputs": {"a":  {"at": [x,y,z], "outside": "south"}, ...}}

The pinout sidecar records, for a pinned port, the terminal cell plus its
`outside` facing (unpinned ports keep the bare coordinate they have today).
The facing is not decoration: it is how a consumer knows where the caller's
side is.

## The viewer plays the caller

For a pinned circuit the viewer is no longer just a reader -- it is the
demo's user, and it attaches things on the outside cells the way any caller
would:

- **Pinned inputs**: no lever exists in the artifact, so the existing lever
  UI drives a viewer-installed source placed in the reserved outside cell at
  load time. Same panel, same `set_lever` name, different block underneath.
- **Pinned outputs**: badges already read strength > 0 at the recorded cell
  and need nothing. For the display demo the viewer additionally hangs a
  lamp on each output's outside cell, so the digit glyph is literally lit
  lamps, not seven numbers in a side panel.

Both attachments live on cells the contract guarantees empty, which is the
point: the viewer exercises the interface exactly as an external caller
would, and touches nothing inside the boundary.

## Acceptance

The circuit that named the problem: the decoder (verilog:seven_segment, 47
gates) with its seven outputs pinned as a digit glyph (one-cell gaps between
segments -- the contract does not care which plane the glyph lies in; the
demo lays it flat on the ground), four inputs pinned in a row, all outsides
facing away from the circuit, through `compile_grown`. Passes when:
truth table 16/16 through the real Simulator, every terminal delivers per
contract, every outside cell ships empty, and the four invariants hold --
and the baked demo joins the viewer dropdown, where flipping the levers
shows the digit on viewer-attached lamps.

## Deliberately out

- Relative patterns, region or face constraints, ordering constraints.
  Absolute cells are the whole vocabulary until they are measurably not
  enough.
- The legacy emitter. Its IO geometry is the template's business.
- Multi-cell pads, analog strength contracts, or promising any strength
  above 1 at an output terminal.
- Pinning a gate's position or facing. Ports only.
