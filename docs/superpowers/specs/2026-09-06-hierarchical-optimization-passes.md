# Hierarchical Optimization Passes

## Goal

Extend the hierarchical generator with a small ordered set of optimization
passes that can reduce observed settle latency, non-air block count, or
occupied volume on real circuits. A pass is retained only when a non-toy
acceptance circuit produces a strictly better fully certified candidate.

The first target is programmatic hierarchical `ripple_adder8`. Its current
best exhausted alignment result is 576 observed game ticks, 71,147 non-air
blocks, 676 static routed ticks, and seven alignment evaluations followed by
seven zero-yield boundary-sharing evaluations.

## Constraints

- Budget zero must preserve the existing candidate, pinned IO, and flat path.
- Every proposal must end in the unchanged flat whole-world certification.
- Distinct modules remain compile-once/stamp-many artifacts.
- Proposal traces remain deterministic prefixes for evaluation budgets.
- Time budgets stop only between complete proposals.
- `QualityKey` ordering remains unchanged: observed settle, blocks, volume,
  then static routed delay.
- No new optimizer framework, pass registry, routing engine, dependency, or
  configuration knob is introduced.
- Cargo commands run serially in this Windows worktree.

## Pipeline

The pipeline is one `HierarchicalProposalStream`, not one search runner per
pass. Each stage freezes its stable descriptors when the prior stage is
exhausted. A descriptor that later becomes stale is deterministically refused
rather than silently retargeted.

### Pass 1: Port Align Z

Retain the existing block-edge proposal unchanged. For each block-to-block
edge, move the sink block in Z until the selected source output and sink input
ports align. Edges remain ordered by structural slack, source block, sink
block, and sink input.

This stage is already measured and is the known-good prefix of the optimizer.

### Pass 2: Block Pull X

For each block edge, move the sink block one cell toward the source in X while
preserving accepted Z alignment and all other block offsets. The direction is
the sign of the realised source-port X minus realised sink-port X. A zero
delta produces no descriptor.

This is deliberately one cell and one proposal per edge. Collision,
reservation, routing, or certification failure refuses the proposal. Larger
search radii and a new placement configuration are out of scope.

### Pass 3: Input Seam Absorption

Keep the parent route's terminal repeater at the child input boundary. In the
stamped child candidate, replace one first internal non-terminal repeater with
dust when all branches using that input remain strength-valid.

Candidates are identified in block-local coordinates by sink block, input
port, and repeater anchor. The selected anchor must still be a route-owned
repeater in the input route when applied; otherwise the proposal is refused.
Terminal repeaters, primitive-owned cells, declared IO boundaries, and pinned
handover cells are never candidates.

The local strength proof walks every affected branch from the retained parent
boundary repeater with strength 15. Dust consumes one strength and retained
repeaters restore 15. Missing cells, an invalid conductor, zero strength, or a
terminal mismatch refuses the proposal before expensive certification.

This pass replaces the failed parent-boundary-removal experiment. The
`AutomaticAtLeast`, `shared_inputs`, `required_root_strength`, and
single-consumer plumbing from that experiment must be deleted.

### Pass 4: Parent Route Repack

Operate only on cells that belonged to the parent route before union, keeping
compiled child interiors opaque.

For each low-slack route, first greedily replace strength-redundant internal
route repeaters with dust in deterministic downstream-to-upstream order. The
entire route cleanup is one proposal and every tentative deletion must pass
the same branch strength walk.

If direct deletion cannot reduce the route, try the smallest relocation that
can remove a downstream repeater: move its common upstream refresh to the
latest legal straight parent-owned dust cell, convert the old position to
dust, and remove the downstream refresh. A relocation is emitted only when
all affected branches share the same predecessor refresh and the net
repeater count falls.

Path coordinates and floors do not change in this stage. Repeater facing is
derived from the existing straight path steps. Branch terminals and boundary
owners are excluded.

### Deferred: Critical Route Shortcut

A straight or one-corner branch-private shortcut is a possible later stage,
but it is not part of the first implementation. Dust length alone has no
timing cost, and shortcut ownership/coupling logic is materially larger. Add
it only if the first four stages measurably plateau and a trace proves that a
shortcut would remove a critical-path repeater.

### Deferred: Cell Topology Search

The flat optimizer already searches implementation, facing, and placement.
The current cell library has few genuine implementation alternatives, and
`ripple_adder8` has no parent-owned glue gates. Do not add module-rebuild
caches or mutate child gate topology until a real ALU/CU case demonstrates a
candidate with multiplicative benefit.

## Candidate Safety and Metadata

Route mutations clone the incumbent flat `ExpandedPhysicalCandidate`. They
reuse `refresh_exact_route_delays` after changing route cells, then call the
existing `certify_planned` path. No proposal may directly edit metrics,
timing graphs, observations, pins, pin contracts, or logical assignments.

The cheap strength walk is a rejection filter, not a replacement for full
certification. Successful candidates still pass shape and physical ownership,
world emission and physical verification, realised timing derivation,
equivalence, and transition simulation.

## Budget Order

The finite stream order is:

1. all Port Align Z descriptors;
2. all Block Pull X descriptors;
3. all Input Seam Absorption descriptors;
4. all Parent Route Repack descriptors.

Within route stages, timing-graph slack is primary. Ties use route ID, sink
ID, and anchor coordinates. The incumbent changes only when a certified
candidate has a lower `QualityKey`.

## Retention Gates

Each new pass must satisfy all of these before it remains in production:

1. a focused red/green test proves its physical mutation and refusal cases;
2. budget traces remain deterministic prefixes and quality never worsens;
3. budget zero and pinned IO are unchanged;
4. at least one of `ripple_adder8`, `alu4_full`, `multiplier4`, or `alu8`
   accepts a candidate with lower observed settle, or equal settle with fewer
   blocks/volume;
5. the accepted candidate passes unchanged certification.

A pass that only improves a synthetic fixture is removed. Measurements report
observed settle, non-air blocks, occupied volume, static routed delay,
evaluations, and wall time separately.
