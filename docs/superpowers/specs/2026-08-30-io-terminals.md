# IO terminals: the pinned cell is the caller's

**Status: DESIGN, revised 2026-08-30 after the first implementation pass.**
Motivating failure: a seven-segment display is *defined* by where its outputs
sit -- seven signals arranged as a digit glyph -- and REDA today cannot be
told where any IO goes. The decoder compiles and verifies, but its outputs
land wherever the placer put their driving gates, so the one circuit whose
meaning is geometric cannot be built as the thing it names.

**The working model is a PCB router's.** The caller finishes the schematic
(Verilog), then hands the compiler a pin-position file the way an FPGA build
takes a pin-constraint file: signal name to board location. Placement and
routing are then the tool's problem. The caller says *where*, never *how*.

## The contract (settled first, everything else follows)

A **pin** is `port name -> (Anchor, toward: Facing)`: an absolute cell plus
**the direction the signal travels through it**. Both halves are
requirements. A caller who says *where* without saying *which way* has not
finished specifying the port -- the same way a PCB pad without an entry side
leaves the router guessing at the one thing the board already decided.

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
chosen by the compiler. **The other three neighbours carry nothing of
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
stated plainly: the router gets exactly one approach to each terminal instead
of three. A pin whose one lawful cell cannot be reached is a **refusal by
name against that pin**, not a silent reroute -- the caller over-constrained
the board and is told which pin did it.

The physics behind "powered" is not this document's to assert from memory:
every claim above is a requirement to be checked against the simulator's own
power model in `src/redstone` and pinned by tests -- in particular which
constructions actually deliver power into a neighbouring cell, and which
states of the caller's cell REDA's reader can sense. Where the model and this
prose disagree, the model is right and the prose is the bug.

## The mechanism: terminals are pinned bodies

The one existing position mechanism, `PortPlacements`
(src/compile/planner.rs:3166), already threads pins through starting layout,
the relaxation solve (struck from the matrix), the separation projection
(neighbours pay), snap (returned exactly), and `try_move` (`PortIsPinned`).
Its vocabulary changes from a bare `Anchor` to `(Anchor, toward: Facing)`,
and its *meaning* changes: pinning a port no longer pins the body that
happens to realise it (the lever, or the producing gate) -- it declares a
terminal. The old pin-the-gate-by-output-signal behaviour has no shipping
caller and its tests are re-pointed at terminal semantics.

A terminal is a pinned body whose anchor is the caller's cell and whose whole
realisation lives in **one** neighbour, the one `toward` names. Its footprint
claims the caller's cell and emits nothing there -- that cell ships exactly
as it was. There is no facing sweep and no variant to choose: a terminal has
a single lawful realisation, which is what makes a pin a specification rather
than a hint. The remaining three neighbours are claimed only to the extent of
keeping foreign nets out of them; REDA builds in none of them.

**Input terminal.** The port's body stops being a lever. REDA's reader sits
in the `toward` neighbour and normalizes: whatever the caller's cell offers,
the net's source is full strength by construction, so source strength is
geometry again -- the same move that made merge strength pure geometry in
the second campaign. The route source is that reader's output pin. Which
construction reads which states of the caller's cell is a question for
`src/redstone`, answered with tests, not assumed here.

**Output terminal.** A new body added as an extra sink on the declared
output's net, driving the caller's cell from the neighbour opposite `toward`.
The router treats that cell exactly like a gate socket, with one difference
that matters: a socket may be approached from several sides and this may not.
The strength-aware arm reasons about the arrival natively; a distance-only
plan that starves it is caught by verification and the portfolio's second arm
retries. The producing gate stays free; the net simply has one more place it
must reach, at one exact address. No lamp is placed for a pinned output --
the lamp, if the caller wants one, is theirs to put in their own cell.

Because terminals are bodies with springs to their net partners, the placer
pulls the circuit *toward* the pinned coordinates during relaxation instead
of the router discovering the distance after placement froze. Pinned anchors
suppress the whole-layout drift translation (already true today), and world
sizing grows to cover every pin plus margin.

**Refused by name, before planning:** a pin outside any growable world
bound; two pinned cells of different nets in signal-carrying adjacency (the
caller left no gap -- a glyph needs a gap between segments); a pinned cell
colliding with another port's cell or handover; one port's handover cell
sitting where another port needs its own; a pin whose handover cell cannot be
reached; a pin for a port name the netlist does not declare. Every one of
these is the caller over-constraining the board, so every one names the pin
that did it. Output pins are given by display label (`a`..`g`) and resolved
through the existing output labels channel, because asking the caller for
internal `gN` names would be absurd.

## Verification follows the contract

The contract is about a cell REDA does not own and may be empty when the
world ships, so verification stops measuring REDA's own dust and starts
**probing the caller's cell**: place a test receiver there, run the vectors,
require it powered exactly on the high ones. The probe is a fixture, not an
interface assumption -- it is how the harness stands in for a caller.

- `verify_signal_strength`'s declared-output pass walks, for a pinned output,
  to the handover cell and states what it must deliver into the caller's
  cell. Same check, new address, and the address is REDA's own last cell.
- The lamp invariant (`LampNotAtFixedOffsetFromTorch`) and the lever check
  in `equivalence` apply only to unpinned ports, which still have lamps and
  levers.
- A new invariant, per pinned port: the caller's cell ships empty; the one
  neighbour the pin names carries this port's own handover; no other
  neighbour carries any net at all.
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
  Every named refusal in this document is stated against `PortPlacements`
  and runs before planning, whoever assembled it.
- **Refusals are structured, not prose.** An editor has to point at the pin
  that is wrong and a mod has to highlight a block; both need the offending
  port name and cell as data. A refusal that only renders as a sentence is a
  refusal only a CLI can use.

## Plumbing

`compile_planned` already takes `PortPlacements`; `compile_grown` gets the
parameter it always plumbed internally and hard-coded to default
(src/compile/mod.rs:7188 -- the growth loop underneath threads placements
end to end already). `compile()` and the legacy emitter are not touched.
`build_circuit` learns `--pins <file.json>` -- the first adapter, the
pin-constraint file of the PCB analogy:

    {"inputs":  {"d0": {"at": [x,y,z], "toward": "north"}, ...},
     "outputs": {"a":  {"at": [x,y,z], "toward": "north"}, ...}}

`toward` is the direction the signal travels through that cell, and the
reader must not have to guess the sign: a round-trip test states, for one
input and one output, exactly which neighbour each `toward` resolves to.

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

The circuit that named the problem: the decoder (verilog:seven_segment, 47
gates) with its seven outputs pinned as a digit glyph (a gap between
segments -- the contract does not care which plane the glyph lies in; the
demo lays it flat on the ground), four inputs pinned in a row, every `toward`
carrying the signal out of the circuit and into the caller's world, through
`compile_grown`. Passes when: truth table 16/16 through the real Simulator
with fixtures in the callers' cells, every pinned cell ships empty, each
handover sits exactly where its pin says, and the four
invariants hold -- and the baked demo joins the viewer dropdown, where
flipping the levers shows the digit on lamps the viewer put in the pinned
cells themselves.

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
