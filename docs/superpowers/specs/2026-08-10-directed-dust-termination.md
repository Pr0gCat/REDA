# Directed dust termination

## Goal

Let the router end a gate input with redstone dust when a straight, isolated
dust run weakly powers that gate's support block in vanilla Minecraft. Keep a
repeater wherever that geometry is not legal or is worse under the configured
cost function.

## Fact already established

`2026-08-09-dust-directionality.md` originally measured this rule on 1.20.1;
the three shape probes were re-run on the target 26.2 server on 2026-08-11
with the same result. The simulator now implements it:

- dust weakly powers the block below it;
- horizontally, a dust cell powers a block only when its opposite side is
  connected and neither perpendicular side is connected;
- a one-sided endpoint is filled into a straight run by vanilla, so it can
  point into its forward block;
- corners, T junctions and crossings do not horizontally power a block.

Weak power is sufficient to turn off a torch attached to the powered support,
so it implements a NOR input correctly. It cannot re-drive dust.

## Second measured mechanism: weak block into a diode rear port

The same weakly powered conductor is nevertheless a valid **rear input** to a
repeater and to a comparator. This is not the normal rule that a strongly
powered block re-drives nearby dust; it is a special rule of a diode reading
the block immediately behind it.

This was re-measured on a live 26.2 server on 2026-08-10, using the minimal
shape below. The source redstone block is placed last, so every observed state
is a real transition rather than a forced blockstate.

```
redstone block -> dust -> [B] -> repeater/comparator -> lamp
                              ^ weakly powered by dust
```

For both diode kinds: dust reached power 15, a torch on top of `B` turned off
(proving weak block power), the diode's `powered` state became true, and its
output lamp lit. The server was stopped cleanly after each probe.

This adds a missing simulator rule: `signal_from` currently permits a diode to
read its rear block only when `block_signal_at` reports `Strong`. It must
instead distinguish the **rear input of a repeater/comparator**, which accepts
any non-zero powered conductive block, from ordinary block-to-dust propagation
and comparator side inputs, which remain strong-only.

## Current gap

`lay_bent_path` always overwrites the final route cell with a repeater. That
was correct before dust directionality was modelled, but is now unnecessarily
conservative for a straight route entering a NOR support. The compiler's
simulation and `net_reach` invariant can already recognise a directed-dust
arrival; the planner never selects one.

## Candidate approaches

### A. Delete all terminal repeaters

Rejected. A bent, branched or crowded dust endpoint does not point into its
support; removing the repeater would make a gate receive no signal or would
remove diode isolation.

### B. Local directed-dust terminal (chosen)

Teach each routed gate-input endpoint to choose one of two explicit modes:

- `RepeaterIntoSupport`: existing directionally isolated, strength-refreshing
  terminal.
- `DirectedDustIntoSupport`: final cell is dust and powers the adjacent gate
  support weakly.

The dust mode is legal only if the final path segment is straight toward the
support, the final dust cell's simulated shape has no perpendicular
connection, it is not a bare merge branch, and the actual arriving strength
is non-zero. Its additional keep-out must be checked against the live
reservation before placement, because dust needs two cells' lateral clearance
where a repeater did not.

This is deliberately a local router optimisation. It has no placement
reordering or gate-topology change, so its benefit and safety can be measured
without mixing in a larger 3D-planning experiment.

The diode rule above is not a third way to power a NOR support: a torch's
support can already read weak power directly. It is a distinct **repeater /
comparator input topology** that the planner must preserve for future typed
ports. It can keep a dust run straight while inserting a diode boundary one
block later, and therefore matters to routing even when it does not remove a
repeater.

### C. Placement-aware directed-dust optimisation

The eventual architecture: include terminal mode in the placement/routing
cost function so gate input faces, columns and routes are chosen to make more
inputs eligible for dust. This is the right long-term direction, but it needs
the local option in B first; otherwise the cost function would price an
ability the emitter cannot build.

## Design for B

1. Represent terminal choice in routing geometry, not as an implicit final
   overwrite. Both reservation construction and world emission consume the
   same choice, so they cannot judge different shapes.
2. Generate the existing repeater route as the fallback. For each ordinary
   gate socket, evaluate a dust candidate against the final path geometry and
   current live `Reservation`.
3. A candidate must end one horizontal step from its conductive support with
   travel direction pointing at that support. It must not be a bend, must have
   no expected perpendicular dust/component attachment, and must pass the
   existing signal-strength calculation with its extra unrefreshed final hop.
4. Claim the dust cell and its required keep-out before approving the next
   candidate. This follows the live-reservation discipline introduced for
   widened bypasses; candidates must see earlier approved candidates.
5. Emit dust for approved candidates and preserve repeaters otherwise. A
   route whose strength planner needs the final cell as a refresh repeater
   keeps that repeater even if its final geometry would otherwise be a legal
   dust arrival.
   `verify_connectivity`, `verify_torch_merge` and `verify_signal_strength`
   remain unconditional. They are constraints, not cost terms.
6. Correct the simulator before changing the router: split `signal_from` into
   ordinary propagation (strong blocks only) and diode-rear reading (weak or
   strong block). Route repeater and comparator rear inputs through the latter;
   keep comparator side inputs strong-only.

## Tests and acceptance

- A hand-built positive route: straight dust directly into a gate support
  compiles, simulates the NOR truth table, and contains dust rather than a
  repeater at the socket.
- A one-bend route retains a repeater, because dust would not point into the
  support.
- A candidate with a lateral attachment retains a repeater.
- A candidate whose final unrefreshed dust hop would be dead retains a
  repeater.
- A conflicting pair is resolved by the live reservation; it must never
  introduce a connectivity violation.
- Repeater and comparator each accept a weakly powered rear block in the
  simulator, matching the live-server probe; a weak block still cannot
  re-drive ordinary adjacent dust or a comparator side input.
- All existing reference circuits and Verilog circuits retain truth-table
  correctness. At least `verilog:and4` removes the observed redundant
  terminal repeater. Re-measure blocks, terminal-repeaters, and settle ticks
  instead of promising a count in advance.
- Run `./check.sh`. RCON probes on 26.2 certify the component-level rules;
  whole-circuit diode behaviour must be driven by a real 26.2 client action,
  because `/setblock` does not schedule a repeater after its rear input
  changes (documented in `docs/minecraft-server.md`). The browser 3D view
  must expose the changed output.

## Out of scope

- Moving gates or changing the floorplan to create more dust-eligible ends.
- Changing gate topology or logical lowering.
- Treating dust's rendered history-dependent dot/cross state as an input;
  its block-power behaviour is identical for this optimisation.
- Replacing repeaters that serve a merge branch, a strength refresh, or a
  deliberate diode/isolation boundary.
