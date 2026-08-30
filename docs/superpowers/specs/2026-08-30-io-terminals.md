# IO terminals: the pinned cell is the caller's

**Status: IMPLEMENTED (2026-08-31).**
Motivating failure: a seven-segment display is *defined* by where its outputs
sit -- seven signals arranged as a digit glyph -- and REDA could not be told
where any IO went. The decoder compiled and verified, but its outputs landed
wherever the placer put their driving gates, so the one circuit whose meaning
is geometric could not be built as the thing it names.

**The working model is a PCB router's.** The caller finishes the schematic
(Verilog), then hands the compiler a pin-position file the way an FPGA build
takes a pin-constraint file: signal name to board location. Placement and
routing are then the tool's problem. The caller says *where*, never *how*.

## The contract (settled first, everything else follows)

A **pin** is `port name -> (Anchor, toward: Facing)`: an absolute cell plus
**the horizontal direction the signal travels through it**. Both halves are
requirements. `toward` accepts only `North`, `East`, `South`, or `West`;
`Up` and `Down` are refused as `PinRefusal::VerticalToward`. A caller who says
*where* without saying *which way* has not finished specifying the port -- the
same way a PCB pad without an entry side leaves the router guessing at the one
thing the board already decided.

**The pinned cell belongs to the caller. REDA never puts a block in it.**
That cell is where the caller's own build lives -- a lamp, a lever, a piston,
another circuit's wire, anything, decided after compilation and outside it.

`toward` fixes the single cell REDA may use, because the signal's direction
of travel says which side of the caller's cell the circuit is on:

- **Output**: the signal leaves the circuit heading `toward`, so REDA drives
  the caller's cell from the cell the signal comes *from* -- the neighbour
  opposite `toward`.
- **Input**: the signal enters heading `toward`, so REDA reads the caller's
  cell from the cell the signal goes *to* -- the neighbour in the `toward`
  direction.

Either way REDA occupies exactly one neighbour, named by the pin and not
chosen by the compiler. **The other five neighbours carry nothing of
REDA's**, so nothing but this port's own signal can reach the caller's cell,
and the caller may build into them freely.

The whole promise, per pinned port:

- **Output**: when the signal is logically high, the pinned cell is powered;
  when low, it is not. Powered in the game's own sense -- whatever the caller
  puts there sees redstone power. No particular strength is promised.
- **Input**: the caller powers the pinned cell by any means the game accepts
  as a signal (a source, dust, a strongly powered block), and the circuit
  reads it as high; unpowered is low.
- **How the handover is built is REDA's business** -- dust, a repeater,
  whatever makes the promise hold -- but *where* it is built is the pin's.

What REDA deliberately does not know: what hardware the caller attaches.
Levers and lamps stop being part of a port's definition and become what they
always physically were -- test fixtures. **Unpinned ports keep today's
behaviour exactly** (lever bodies for inputs, lamp under the producing gate's
pin for outputs); every existing circuit and test is untouched by default.

The cost of the pin naming the cell rather than the compiler choosing it is
stated plainly: the caller's cell, handover cell, and first ordinary net cell
are fixed. The route may reach that net cell from any lawful direction or
shape, but it may not move the handover elsewhere. If negotiation cannot reach
it, routing ends in `PinRefusal::UnreachableHandover`, a **refusal by name
against that pin**, not a silent reroute -- the caller over-constrained the
board and is told which pin did it.

The physics behind "powered" is not this document's to assert from memory:
every claim above is a requirement to be checked against the simulator's own
power model in `src/redstone` and pinned by tests -- in particular which
constructions actually deliver power into a neighbouring cell, and which
states of the caller's cell REDA's reader can sense. Where the model and this
prose disagree, the model is right and the prose is the bug.

## The mechanism: terminals are pinned bodies

The position mechanism, `planner::PortPlacements`, threads pins through
`planner::starting_layout`, the relaxation solve (struck from the matrix), the
separation projection (neighbours pay), snap (returned exactly), and
`planner::try_move` (`PlannerError::PortIsPinned`). Its vocabulary is
`(Anchor, toward: Facing)`, and pinning a port declares a terminal rather than
pinning the body that happens to realise it (the lever, or the producing
gate). The old pin-the-gate-by-output-signal behaviour had no shipping caller
and its tests now assert terminal semantics.

A terminal is a pinned body whose anchor is the caller's cell and whose
immediate handover occupies **one** neighbour, the one `toward` names. Its
footprint claims the caller's cell and emits nothing there -- that cell ships
exactly as it was. There is no facing sweep and no handover variant to choose:
a terminal has a single lawful interface realisation, which is what makes a
pin a specification rather than a hint. The remaining five neighbours are
claimed only to the extent of keeping signal-carrying cells out of them; REDA
builds in none of them.

**Input terminal.** The port's body stops being a lever. REDA's reader sits
in the `toward` neighbour and normalizes: whatever the caller's cell offers,
the net's source is full strength by construction, so source strength is
geometry again -- the same move that made merge strength pure geometry in
the second campaign. The route source is that reader's output pin. Which
construction reads which states of the caller's cell is a question for
`src/redstone`, answered with tests, not assumed here.

**Output terminal.** A new body added as an extra sink on the declared
output's net, driving the caller's cell from the neighbour opposite `toward`.
The router must reach the fixed ordinary net cell behind that handover. As
`PortPin::net_cell` specifies, this net cell may be approached from any lawful
side, in any lawful shape, at any strength the delivery repeater can read; the
handover's cell and facing remain fixed. The strength-aware arm reasons about
the arrival natively; a distance-only plan that starves it is caught by
verification and the portfolio's second arm retries. The producing gate stays
free; the net simply has one more place it must reach, at one exact address.
No lamp is placed for a pinned output -- the lamp, if the caller wants one, is
theirs to put in their own cell.

Because terminals are bodies with springs to their net partners, the placer
pulls the circuit *toward* the pinned coordinates during relaxation instead
of the router discovering the distance after placement froze. Pinned anchors
suppress the whole-layout drift translation (already true today), and world
sizing grows to cover every pin plus margin.

**Refused by name at the pin-set door, before planning:** a vertical
`toward`; a pin outside any growable world bound; two pinned cells of
different nets in signal-carrying adjacency (the caller left no gap -- a glyph
needs a gap between segments); a pinned cell colliding with another port's
cell, handover, or first net cell; and a pin for a port name the netlist does
not declare. `planner::validate_port_placements` can decide each of these from
the pin set and netlist alone.

`PinRefusal::UnreachableHandover` is deliberately different: it is a named
**routing-time** refusal raised by `planner::lay_net` only after search cannot
reach the fixed terminal and the negotiation loop has had its rip-up and
re-ordering opportunities. It is not a pre-planning refusal. Output pins are
given by display label (`a`..`g`) and resolved through the existing output
labels channel, because asking the caller for internal `gN` names would be
absurd.

## Verification follows the contract

The contract is about a cell REDA does not own and may be empty when the
world ships, so verification stops measuring REDA's own dust and starts
**probing the caller's cell**: place a test receiver there, run the vectors,
require it powered exactly on the high ones. The probe is a fixture, not an
interface assumption -- it is how the harness stands in for a caller.

- `compile::verify_signal_strength`'s declared-output pass walks, for a pinned
  output, to the handover cell and states what it must deliver into the
  caller's cell. Same check, new address, and the address is REDA's own last
  cell.
- The lamp invariant
  (`equivalence::EquivalenceError::LampNotAtFixedOffsetFromTorch`) and the
  lever check in `compile::equivalence` apply only to unpinned ports, which
  still have lamps and levers.
- `planner::verify_terminal_contract` requires, per pinned port, that the
  caller's cell ships empty; the one neighbour the pin names carries this
  port's own handover; and the other five neighbours carry no net at all.
- The truth-table battery drives a pinned input by placing a fixture source
  in the caller's cell and reads a pinned output by probing the caller's
  cell. Both fixtures are installed into cells the contract guarantees
  empty, and removed with the fixture teardown -- the shipped world contains
  neither.

## Where pins come from

A pin set has several sources and will grow more: a file today, a definition
made in an editor, and -- planned -- coordinates picked in the game itself
through a mod. They are adapters, not variants. **`PortPlacements` is the one
representation the compiler knows**, and every source's only job is to
produce one.

Two consequences that are easy to get wrong and expensive to fix later:

- **Validation belongs to the pin set, not to any parser.** A check written
  in the JSON reader protects the file and abandons the editor and the mod.
  Every structural refusal is stated against `PortPlacements` and runs before
  planning, whoever assembled it; `UnreachableHandover` remains structured
  against the same pin but can only be known during routing.
- **Refusals are structured, not prose.** An editor has to point at the pin
  that is wrong and a mod has to highlight a block; both need the offending
  port name and cell as data. A refusal that only renders as a sentence is a
  refusal only a CLI can use.

## Plumbing

`compile::compile_planned` and `compile::compile_grown` both take
`planner::PortPlacements`; `compile_grown` passes them into
`planner::plan_from_netlist_with_growth` in both portfolio arms. `compile()`
and the legacy emitter remain unchanged. `build_circuit` accepts
`--pins <file.json>` -- the first adapter, the pin-constraint file of the PCB
analogy:

    {"inputs":  {"d0": {"at": [x,y,z], "toward": "north"}, ...},
     "outputs": {"a":  {"at": [x,y,z], "toward": "north"}, ...}}

`toward` is the horizontal direction the signal travels through that cell;
vertical values are refused. The reader must not have to guess the sign: a
round-trip test states, for one input and one output, exactly which neighbour
each `toward` resolves to.

The pinout sidecar reports back, for a pinned port, the caller's cell, its
`toward`, and the resolved handover cell. The handover is derivable rather
than chosen, but the caller building against it should not have to redo the
derivation -- and a reported cell that disagrees with the shipped world is a
bug the sidecar makes visible.

## The viewer plays the caller

For a pinned circuit the viewer is no longer just a reader -- it is the
demo's user, and it builds in the caller's own cells the way any caller
would:

- **Pinned inputs**: no lever exists in the artifact, so the existing lever
  UI drives a viewer-installed source placed in the caller's cell at load
  time. Same panel, same `set_lever` name, different block underneath.
- **Pinned outputs**: the viewer hangs a lamp in each output's caller cell,
  so the digit glyph is literally seven lit lamps at the pinned coordinates
  -- not seven numbers in a side panel. Badges read those lamps.

Every attachment lands in a cell the contract guarantees empty, which is the
point: the viewer exercises the interface exactly as an external caller
would, and touches nothing inside the boundary.

## Acceptance

The shipped circuit is the decoder (`verilog:seven_segment`, 47 gates) with
its seven outputs pinned as a digit glyph (a gap between segments -- the
contract does not care which plane the glyph lies in; the demo lays it flat
on the ground), four inputs pinned in a row, and every `toward` carrying the
signal between the circuit and the caller's world through `compile_grown`.
The shipping input mapping is explicit and MSB-to-LSB: `d3=(76,1,120)`,
`d2=(88,1,120)`, `d1=(100,1,120)`, and `d0=(112,1,120)`, all with
`toward=North`. `planner::tests::pinned_glyph_decoder`,
`viewer/baked/verilog_seven_segment.grown.pins.json`, and
`viewer/tests/verilog_circuits.rs` hold those coordinates and names.

The native acceptance passes the truth table 16/16 through the real
`Simulator` with fixtures in the callers' cells; every pinned cell ships
empty; each handover sits exactly where its pin says; and the four physical
invariants hold. The baked demo is in the viewer dropdown, where input
toggles drive caller-installed sources and show the digit on lamps the viewer
put in the pinned cells themselves. Live browser acceptance on 2026-08-31
confirmed the initial zero, digit two, 3D caller-source toggle, geometry
preservation, and reset-to-zero contract.

## Deliberately out

- Relative patterns, region or face constraints, ordering constraints.
  Absolute cells are the whole vocabulary until they are measurably not
  enough.
- The legacy emitter. Its IO geometry is the template's business.
- Multi-cell pads, analog strength contracts, or promising any particular
  strength at the caller's cell.
- Pinning a gate's position or facing. Ports only.
- The editor and the in-game adapters themselves. This campaign ships the
  representation they will target and the file adapter over it; building
  them is their own work.
- A board outline. The PCB analogy has one and REDA does not: worlds grow
  only toward positive coordinates and pinned anchors suppress the layout's
  drift translation, so today the pin coordinates *implicitly* decide where
  the gate mass may spread, and a badly placed glyph fails as
  `outside the world` or `Deadlocked` rather than as a stated refusal. The
  glyph's own coordinates had to be found by sweeping candidate translations.
  Naming the region the caller grants the compiler is the obvious next
  campaign; this one ships without it.
