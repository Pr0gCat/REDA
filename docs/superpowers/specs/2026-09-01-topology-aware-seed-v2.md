# Topology-Aware Seed v2

Status: approved for implementation on 2026-09-01 by the user's "設定目標開做".

This spec replaces only the initial placement and route-scheduling policy of
the timing-directed fragment synthesiser. It does not change the public
synthesis API, the physical router, certification, fragment optimisation, or
the pinned-IO contract.

## 1. Problem

The current independent seed is deterministic and independently certified,
but its geometry is not topology-aware in practice:

- canonical instances use `logical_gate.0` multiplied by fixed X/Z pitches;
- primitives inside an instance are laid out by primitive ordinal at a fixed
  pitch;
- all instances are placed before any route is attempted;
- net order uses only the earliest target's topological rank and source ID.

A fresh budget-zero acceptance run at commit `25836d1` reproduced the checked
report exactly:

| Case | Result |
| --- | --- |
| `and4` | certified, 120 ticks and 1477 non-air blocks; baseline 18 and 472 |
| `verilog:and4` | certified, 238 ticks and 2121 blocks; baseline 22 and 480 |
| `full_adder` | route 0 refused as `PhysicalInvariant` |
| `segment_a` | route 4, sink 1 exceeded 262144 queue entries |
| `seven_segment` | route 4, sink 1 exceeded 262144 queue entries |
| `pinned:verilog:seven_segment` | route 0, sink 3 exceeded 262144 queue entries |

The replacement must change the source geometry of these failures. Increasing
router caps is not a fix and must not be used to satisfy this spec.

A route-request diagnostic adds one narrower shared pattern without claiming a
single root cause: every failure occurs on a non-first fanout sink. The current
tree order connects a near sink before a much farther sink. `full_adder` then
fails while materialising the far branch; the other three requests exhaust the
route-wide queue counter while searching later branches. This makes farthest
critical sink first a measured hypothesis, not a general router rewrite.

## 2. Goals

The first hard gate is correctness at optimisation budget zero:

1. All six acceptance cases independently seed, route, emit, physically
   verify, simulate, and certify.
2. No case ends in `PhysicalInvariant`, `QueueEntries`, or another hidden
   fallback.
3. The 11 pins of `pinned:verilog:seven_segment` retain byte-identical caller
   coordinates and `toward` values.
4. Repeated construction with the same input and config is byte-identical.

The second hard gate is seed quality:

- `and4` is at most 36 observed settle game ticks and 944 non-air blocks.

The full replacement gate from the parent timing-directed synthesis spec
remains authoritative after these two milestones. Meeting this spec does not
by itself authorize switching front doors or deleting the legacy generator.

## 3. Non-goals

- Do not modify the physical A* router or raise `RouterLimits`.
- Do not add a wall-clock budget, randomness, or hash-map iteration to seed
  construction.
- Do not route through the legacy planner or use it as an oracle.
- Do not redesign fragment transactions or realised timing analysis.
- Do not integrate the disconnected `macro_cells::CellLibrary`.
- Do not move pinned caller cells, handovers, or net cells.

## 4. Chosen architecture

Seed v2 has three private, independently testable stages:

```text
InstanceGraph + selected topology + fixed pins
    -> SeedPlacementAnalysis
    -> SeedPlacementPlan
    -> materialise + legalise
    -> RouteSchedule
    -> existing router / verifier / certifier
```

`placement.rs` owns pure graph analysis and preferred poses. `seed.rs` remains
the transaction coordinator and owns materialisation, collision legalization,
routing, emission, and certification. `routing.rs` remains ignorant of the
placement policy.

The private interface is:

```rust
pub(crate) struct SeedPlacementRequest<'a> {
    pub graph: &'a InstanceGraph,
    pub pins: &'a BTreeMap<PhysicalEndpointId, PortPin>,
}

pub(crate) struct PreferredInstancePose {
    pub preferred_origin: Anchor,
    pub facing: CellFacing,
}

pub(crate) struct SeedPlacementPlan {
    pub instances: BTreeMap<InstanceId, PreferredInstancePose>,
    pub automatic_inputs: BTreeMap<PortId, Anchor>,
    pub automatic_outputs: BTreeMap<PortId, Anchor>,
}
```

The plan contains preferences, not committed coordinates. The existing bounded
legalizer may move a primitive within its deterministic Manhattan shell, and
fragment `InstancePlacementOverride` values remain relative to the planned
instance origin.

## 5. Structural analysis

`analyse_instance_dag` reads the selected `InstanceGraph`, including duplicate
instances and implementation-expanded topology. It rejects cycles instead of
appending cyclic IDs as the current private order helper does.

For every instance it computes:

- predecessors and successors in typed-ID order;
- forward longest-path level;
- reverse longest-path level to a declared output;
- fanout from concrete sink assignments;
- cell-only head and tail delay from selected topology primitives;
- structural slack, using zero estimated wire delay before placement.

The analysis is immutable and contains no anchors. After preferred anchors are
known, Manhattan port-to-port distance is a secondary estimate; it never
replaces realised timing, which remains the post-route authority.

## 6. Placement frame and fixed boundaries

`PortPin.toward` remains the current public authority and means signal travel
direction. A role-dependent outside direction may be derived internally:

```text
input outside  = toward.opposite()
output outside = toward
```

Pinned `at`, `handover(role)`, and `net_cell(role)` are materialised first and
never enter a placement candidate set.

The placement frame has a horizontal `forward` axis and its perpendicular
`lateral` axis:

- with pinned inputs and outputs, `forward` is the dominant cardinal direction
  from the median input net cell toward the median output net cell;
- with only pinned inputs, use the stable majority `toward`, breaking ties by
  NESW order;
- with only pinned outputs, use the opposite of the stable majority `toward`;
- without pins, use East.

All internal coordinates are expressed in this frame and transformed to world
anchors only at the plan boundary. For the checked pinned seven-segment fixture,
this points the internal circuit from the input line at z=120 toward the glyph
around z=24..56 instead of placing everything east of x=112.

## 7. Track-aware layered placement

The primary coordinate is the forward level, not gate ID. Column spacing is
derived from the widest selected physical variant in that level plus a fixed
routing channel; it is not a single circuit-wide 40-block pitch.

Each logical net has a lifetime interval from its source level to its furthest
sink level. A stable greedy interval colouring assigns reusable lateral tracks:

1. longer critical intervals first;
2. higher fanout first;
3. lower structural slack first;
4. typed source identity last.

Intervals that do not overlap may reuse a track. Pinned boundary net cells
provide fixed lateral attractors but never move; conflicts are resolved by a
short internal dogleg, not by changing pin geometry.

Within a level, instances are ordered by the weighted median track of their
incident nets. Critical edges have the highest weight, then fanout, then typed
identity. Stable left-to-right and right-to-left barycentric sweeps reduce
crossings; a final deterministic lane legalizer removes macro-envelope overlap.

Primitive positions inside an instance use `ValidatedTopology` connectivity and
`EmbeddingHint`, not `PrimitiveId.node` as distance. Facing is selected by
enumerating NESW and scoring the actual `PhysicalVariant` port coordinates:

```text
score = weighted port-to-track Manhattan distance
      + topology-edge distance
      + embedding-hint penalty
      + layer-order penalty
```

The lexicographic tie-break is score, facing index, instance ID, primitive ID.
The existing physical variant footprints and keep-outs remain the legality
authority.

## 8. Route schedule

Placement and route ordering remain separate types. `RouteSchedule` contains
the exact source and ordered sink identities that `route_all` will enumerate.

Routes are ordered by:

1. pinned boundary escape obligations;
2. lower structural slack;
3. higher fanout;
4. longer level span;
5. typed source identity.

Within a tree, sinks are ordered by lower structural slack, then decreasing
forward distance from the source, then the existing `PendingTarget` key. The
first branch therefore establishes the longest critical trunk and nearer sinks
can attach to it. Route IDs are assigned only after this canonical order is
complete.

All source and sink terminals are reserved before the first route. Routing still
uses the existing `PhysicalRouter`, exact terminal contracts, repeater facing,
signal-strength rules, route-tree reservation, and full post-route verification.

## 9. Bounded repair

Seed construction may make deterministic layout attempts inside the existing
seed backtrack cap. Every attempt starts from a fresh candidate and unchanged
router limits; partial physical state is never reused.

A failed route produces a typed `SeedRoutingFailure` containing the scheduled
route, source, failing sink, router category, optional cap limit/work used, and
placement-plan fingerprint. The next attempt adds exactly one monotonic repair
constraint:

- the first failure of any fanout tree promotes the named sink to the first
  branch and gives the source an exclusive guarded track;
- if that canonical promotion already exists, the next failure separates the
  failed source and sink owners by one additional lateral pitch;
- a single-sink failure skips promotion and directly separates its owners.

Repairs are keyed by typed identities and sorted canonically. Repeating an
already-present repair is terminal `SeedExhausted`, not an infinite retry. The
error records attempts used and the final typed refusal. No repair changes a
pin, router cap, certification cap, or optimisation budget.

## 10. Fingerprints and determinism

The synthesis case fingerprint must include a placement-policy revision. The
candidate and emitted-world fingerprints remain unchanged in meaning. Pure
analysis and plan values use only ordered collections and explicit stable
tie-breaks.

Tests compare complete plans and schedules, not source text. Expectations use
literal typed IDs and hand-derived coordinates.

## 11. Tests and acceptance sequence

TDD proceeds in this order:

1. A reversed-declaration producer/consumer fixture fails under gate-ID
   placement and passes only when dependency levels determine forward order.
2. Pure DAG analysis tests cover levels, fanout, slack, cycles, duplicate
   instances, and deterministic interval-track reuse.
3. A fake placer proves `SparseSeedBuilder` consumes the injected plan, including
   automatic boundary homes and facing.
4. Physical-port tests prove facing scores use actual torch and repeater port
   geometry and topology connection ordinals.
5. Pinned tests require all 11 literal `(Anchor, toward)` pairs, caller air,
   handover geometry, and inward-only internal placement.
6. Recording-router tests pin canonical route and sink order plus typed failure
   diagnostics and bounded repair.
7. Run focused seed, pin, handover, routing, fragment, and architecture suites.
8. Run the six-case release acceptance harness at budget zero. First require six
   certified cases; then tune only placement constants or scoring weights until
   `and4 <= 36 ticks` and `<= 944 blocks`.
9. Run the full replacement harness. Do not generate shipping configuration or
   switch front doors unless the parent spec's complete replacement gate passes.

## 12. Rejected alternatives

Reusing the old spring placer would make the new seed depend on the generator it
is intended to replace and does not model selected fragment topology or
duplicates cleanly.

An exact SAT/ILP placement model is unnecessary before the deterministic channel
floorplan is measured and would make budget-zero construction difficult to
bound.

Adding more fragment moves cannot repair cases that have no certified seed.
Raising router caps only spends more work on the same bad geometry and is
explicitly forbidden.
