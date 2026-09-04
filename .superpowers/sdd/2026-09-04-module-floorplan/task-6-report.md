# Task 6 report: candidate translation and identity renumbering

## Files

- Created `src/compile/fragment_synth/relocate.rs`: `Offset`, `IdMap`, `RelocateError`,
  `translate`, `translate_route`, `anchors_of`, `renumber`, plus the private shared
  walkers `for_each_anchor`/`for_each_route_anchor` and per-collection `renumber_*`
  helpers. Three tests, matching the brief's `relocate::tests` exactly.
- Modified `src/compile/fragment_synth/mod.rs`: added `pub(crate) mod relocate;`
  (alphabetically between `realise` and `route_schedule`).
- Modified `src/compile/fragment_synth/candidate.rs`: factored `fingerprint()`'s payload
  construction into a private `fn fingerprint_payload(&self) -> CandidateFingerprintPayload<'_>`,
  used by both `fingerprint()` (unchanged output — see Verification) and a new
  `#[cfg(test)] pub(crate) fn fingerprint_payload_for_test(&self) -> impl Serialize + '_`.
- Modified `src/compile/fragment_synth/seed.rs`: added `pub(crate) fn certified_full_adder()`
  to the existing `pub(crate) mod tests` — compiles `circuits::full_adder::build_full_adder_netlist().0`
  through `lower_optimised`, then the existing `build()` helper (same services as `api.rs`).

## What was built

Exactly the interfaces the brief specifies:

```rust
pub(crate) struct Offset { pub dx: i32, pub dy: i32, pub dz: i32 }
pub(crate) fn translate(candidate: &mut ExpandedPhysicalCandidate, offset: Offset);
pub(crate) fn translate_route(tree: &mut RealisedRouteTree, offset: Offset);
pub(crate) struct IdMap { pub instances: BTreeMap<InstanceId, InstanceId>, pub route_offset: u32 }
pub(crate) fn renumber(candidate: &mut ExpandedPhysicalCandidate, map: &IdMap) -> Result<(), RelocateError>;
pub(crate) fn anchors_of(candidate: &ExpandedPhysicalCandidate) -> Vec<Anchor>;
pub(crate) enum RelocateError { UnmappedInstance(InstanceId) }
```

**One shared walker.** `for_each_anchor(candidate: &mut ExpandedPhysicalCandidate, f: &mut dyn
FnMut(&mut Anchor))` is the single place that knows every anchor-carrying field: placements
(`anchor`, `delayed.at`, `blocks[].at`), boundaries (`delayed.at`, `blocks[].at`), routes
(via `for_each_route_anchor`: `cells[].at`, `floors[].at`, `branches[].{root, path[],
terminal.at}`), junctions (`at`, `cells[].at`), observations (`site.at`), pins (rebuilt —
see below), `pin_contracts` (`.at`, in place). `translate` calls `for_each_anchor` with a
shifting closure; `anchors_of` clones the candidate, walks the clone with a
collecting closure, and returns the collected `Vec<Anchor>` (discarding the clone) — so it
literally cannot visit a different field set than `translate` does. `translate_route` and
the route branch of `for_each_anchor` both go through `for_each_route_anchor`, the same
pattern one level down, for the same reason.

`PortPlacements` (the `pins` field) hides its map behind a private field with no
`iter_mut`, so a pin's `at` can't be borrowed in place. `for_each_anchor` rebuilds it:
reads each `(name, pin)` from the old map, runs `f` on a local copy of `at`, and reinserts
through `PortPlacements::pin(name, at, toward)` into a fresh `PortPlacements::default()`.
`pin_contracts` (`BTreeMap<PhysicalEndpointId, PortPin>`) needed no such rebuild — `PortPin`'s
fields are `pub`, so `.values_mut()` reaches `at` directly, and the keys (always
`PrimaryInput`/`DeclaredOutput`, enforced elsewhere) never change under `renumber` either.

**`renumber`.** `IdMap` maps `InstanceId -> InstanceId` (must be total over every instance
the candidate uses, or `RelocateError::UnmappedInstance`) and adds a flat `route_offset` to
every `RouteId` (routes have no cross-candidate meaning, so a parent only needs a number
that doesn't collide with its own — no lookup table needed). Internal helper methods on
`IdMap` (`instance`, `route`, `primitive`, `connection`, `endpoint`, `observation`,
`routed_sink`, `delayed_owner`, `route_target`, `contributor`) each remap one identity type
by recursing into the `InstanceId`/`RouteId`/`ConnectionId`/`PrimitiveId` it contains, so
every call site composes them instead of hand-rolling the match arms repeatedly. `renumber`
walks, in order: the `InstanceGraph` (`Instance.id`, `expanded.instance`, every
`PrimitiveId`/`ConnectionId` inside `expanded.topology.{primitives, connections, output}`,
and `assignments`' `sink`/`driver` instance ids, `terminals`, `contributors`), then rebuilds
`placements`, `connections`, `routes`, `junctions`, `observations` — every `BTreeMap` keyed
by a renumbered identity is fully rebuilt via `std::mem::take` + reinsert, never mutated in
place, so a key can never end up disagreeing with its value. `boundaries`, `pins`,
`pin_contracts`, and `pin_name_bindings` are untouched: their keys/values are always
`PortId`-based (`PrimaryInput`/`DeclaredOutput`), which `renumber` never remaps, matching
the brief's explicit statement that `PortId` is not renumbered here.

**One field beyond the brief's explicit list:** `ObservationSite.logical_owner: Option<InstanceId>`
(inside `VerifiedObservation.site`, `identity.rs`). The brief's renumber list names
`ObservationId::{PrimitiveOutput, InstanceOutput, JunctionOutput}` (the map *key*) but not
this field on the *value*. It carries an `InstanceId` used by `verify.rs`'s structural
comparisons (e.g. `observation.site.logical_owner == Some(primitive.instance)`). Leaving it
un-renumbered would leave it pointing at the pre-renumbering instance — silently wrong, not
merely unused — so `renumber_observations` remaps it through the same `map.instance()`
alongside `site.id`. This is the only place I extended past the brief's literal field list;
everything else matches it exactly.

## A genuine finding, not worked around: `ValidatedTopology.fingerprint` goes stale under `renumber`

`ExpandedInstance.topology.fingerprint` (`topology.rs::finish_topology`) is computed from
`canonical_fingerprint` of a JSON payload containing `primitives`/`connections`/`output` —
i.e. it is a hash *of* the concrete `PrimitiveId`/`ConnectionId` values, which embed
`InstanceId`. After `renumber` rewrites those ids, the cached `fingerprint` field still
holds the *old* value; nothing recomputes it, because `finish_topology` and its
`FingerprintPayload` are private to `topology.rs`, and this task's file scope (per the
brief's own "Files" list and Step 5 `git add`) is `relocate.rs`/`mod.rs`
(+ `candidate.rs`/`seed.rs` as needed for the payload/test-fixture refactor) — not
`topology.rs`. `validate_shape()` never reads `topology.fingerprint`, so this does not
surface in any test here, including the renumbering test's own `validate_shape()` call.
It **would** surface in `verify.rs`'s structural comparison
(`actual.topology.fingerprint != expected.topology.fingerprint`, `verify.rs:274`), which
runs when re-verifying a candidate against a freshly re-`instantiate()`d topology for the
same instance id — exactly what a full re-certification of a renumbered/stamped block
would do. I did not add a `topology.rs` recompute path to paper over this, since it's
outside this task's assigned files and outside what its tests check; flagging it here for
whichever of Task 7-9 re-verifies a stamped block, since that task will need either to
recompute `topology.fingerprint` after renumbering (mirroring `finish_topology`'s payload)
or to skip/relax that specific structural check for a stamped instance.

## Test helper: `certified_full_adder`

Added next to the existing `build`/`build_with_pins` helpers in `seed.rs`'s
`pub(crate) mod tests` (already `pub(crate)` since Task 4, per the module's own comment).
Full adder was chosen (as the brief specifies) over the smaller `not_netlist()` fixture
already in this module because it has multiple instances, an internal route, and a
declared-output route — small enough to be fast, large enough that a walker silently
skipping a field would actually be caught. `build_full_adder_netlist()` uses
`NetlistBuilder::and_reduce`/`or_reduce`, which already emit `GateKind::Nor`/`Or` directly
(confirmed by reading `netlist_builder.rs`), so `lower_optimised` is a no-op pass-through
here, but is kept in per the brief's literal instruction and to fail loudly (via `.expect`)
if that assumption ever stops holding.

## Strict RED/GREEN

`relocate.rs` was first written with only the `#[cfg(test)] mod tests` block (the brief's
three tests verbatim, adapted to call `candidate.fingerprint_payload_for_test()` as a
method rather than a free function, matching how it's actually implemented on
`ExpandedPhysicalCandidate`) and no implementation. `cargo test --lib
compile::fragment_synth::relocate` failed to compile — 12 errors, all `E0422`/`E0425` for
`Offset`, `IdMap`, `InstanceId` (unimported), `translate`, `anchors_of`, `renumber` not
existing — confirmed RED. The full implementation (walkers, `IdMap`, `renumber_*` helpers)
was then added and the same command passed clean on the first run afterward — confirmed
GREEN. No implementation code was written before the RED run.

## The extra property the brief asked for beyond its own three tests

Already covered by the brief's own first test body, which I implemented verbatim:
- `moved.validate_shape().expect(...)` after translating by `(40, 0, 40)` — passes. A
  translated candidate is a well-formed candidate; `validate_shape` did **not** reject it.
- Round-trip: translate by `(7, 0, -3)` then by `(-7, 0, 3)`, `after.fingerprint() ==
  before.fingerprint()` — passes, confirming the anchor walk is exhaustive (a missed field
  would leave a residual offset invisible to the count-only first half of the same test,
  but not to this fingerprint round-trip).
- The renumbering test's own `after.validate_shape().expect(...)` also passes.

## Verification

- `cargo test --lib compile::fragment_synth::relocate` — 3 passed, 0 failed (all new).
- `cargo test --lib compile::fragment_synth::candidate` — 10 passed, 0 failed, all
  pre-existing, including `candidate_fingerprint_binds_typed_ownership_even_when_world_bytes_match`
  and `fingerprint_canonicalises_block_arenas_and_includes_map_keys` — confirms
  `fingerprint()`'s output is unchanged by the `fingerprint_payload` refactor.
- `cargo check --lib --tests` — compiles clean. New dead-code warnings: 13, all
  `pub(crate)` functions in `relocate.rs` (`translate`, `anchors_of`, `renumber`, and the
  `renumber_*` helpers) that are only called from `#[cfg(test)]` right now — expected for an
  interface this task builds and Task 7/8 consumes; `translate_route` specifically is
  marked `#[allow(dead_code)]` with a comment (it isn't even called from the test-cfg build,
  since `translate` reaches routes through the shared walker instead). Pre-existing baseline
  (confirmed via `git stash`) was 10 warnings; nothing outside `relocate.rs`/`candidate.rs`
  changed.
- No other test suites were run per the task instructions (only the two named `--lib`
  filters; no full `cargo test --lib`).

## Global constraints check

- `SynthesisInput`, `compile_fragment_synth`, `PhysicalEndpointId`, legacy front doors: untouched.
- `ExpandedPhysicalCandidate`'s fields and `fingerprint()`'s output: unchanged (verified above).
- Determinism: `BTreeMap` used throughout `relocate.rs` for every rebuilt map; no
  `HashMap`/`HashSet` introduced.
- Rust 2021, no new dependencies (`thiserror`, already a project dependency).

## Concerns

1. **`ValidatedTopology.fingerprint` staleness after `renumber`** — see the dedicated
   section above. Not a defect in this task's own deliverable (nothing in its scope checks
   it), but real, and worth a deliberate decision in whichever task next re-verifies a
   stamped/renumbered block.
2. One field renumbered beyond the brief's literal list (`ObservationSite.logical_owner`)
   — documented above with the reasoning; flagging in case the brief's omission was
   deliberate for a reason I'm not seeing rather than an oversight.

## Fix round 1

Two review findings on the original diff, both latent (only triggerable by callers not yet
written) and both fixed here, each with a RED-then-GREEN test. Two small cleanups done
alongside per the review's instruction.

### Finding 1: `renumber` never rewrote `BoundaryPlacement.delayed.owner`

`for_each_anchor`'s boundary loop shifts `.delayed.at` (a coordinate, translate's job) but
nothing in `renumber` ever touched `.delayed.owner` (an identity, `DelayedOwner::Primitive`
or `DelayedOwner::Route`, either of which embeds an id `renumber` rewrites everywhere else).
`renumber_placements` already does exactly this for a *placement's* `delayed.owner`
(`delayed.owner = map.delayed_owner(delayed.owner)?;`) — the boundary case was a plain
asymmetry, not a deliberate omission.

**Fix:** added `renumber_boundaries` (`relocate.rs`, next to `renumber_placements`), called
from `renumber` right after `renumber_placements`. It walks `candidate.boundaries.values_mut()`
and remaps `delayed.owner` through the same `map.delayed_owner(...)` placements already use.
`boundaries`' key (`PhysicalEndpointId`, always `PrimaryInput`/`DeclaredOutput` here) is a
`PortId` and is never renumbered, so — unlike every other `renumber_*` helper — this one
never rebuilds the map, only mutates values in place.

**Test:** `renumber_remaps_boundary_delayed_owner` (`relocate.rs::tests`). Takes a real
`full_adder_candidate()`, picks one existing boundary, and directly sets its `.delayed` to
`DelayedComponent { owner: DelayedOwner::Primitive(primitive_id), .. }` (a real primitive id
from the candidate's own placements) in one clone, and to
`DelayedComponent { owner: DelayedOwner::Route(route_id), .. }` (a real route id from the
candidate's own routes) in a second clone. Renumbers both with the same `IdMap` the existing
`renumbering_shifts_instances_and_routes_consistently` test uses (`id.0 + 100` instances,
`route_offset: 50`), then asserts the boundary's `delayed.owner` moved to the expected
remapped id in each case. Constructed the boundary state directly rather than trying to
compile a candidate that naturally produces a primitive/route-owned boundary, per the task's
own suggestion — this is a unit test of `renumber`, not of the seed.

### Finding 2: `renumber_instance_graph` never re-sorted `assignments`

`InstanceGraph::assignments` must stay sorted by `PhysicalSink`
(`InstanceGraph::validate`'s `NonCanonicalAssignmentOrder` check), and for two
`InstanceInput` sinks that order is decided by the instance id embedded in the sink —
exactly the field `renumber_instance_graph` rewrites in place without ever re-sorting
afterward. `IdMap`'s contract never requires the mapping to be monotonic (nothing checks or
documents that), so a non-monotonic map silently leaves `assignments` in the *old* order,
which no longer matches the *new* ids' order. The original diff's own test used `id.0 + 100`
for every instance, which is monotonic and therefore never exercises this.

**Fix:** added `candidate.instances.assignments.sort_by_key(|assignment| assignment.sink);`
at the end of `renumber_instance_graph`, right after the assignment-rewriting loop —
mirroring the `assignments.sort_by_key(|assignment| assignment.sink)` at the end of
`InstanceGraph::with_variants` (`instance_graph.rs`).

**Did `instances` need the same treatment? Conclusion: no**, and I verified this myself
rather than taking the review's framing on faith. `instances`' sort key is
`(logical_gate, role, id)`, checked by `validate`'s `NonCanonicalInstanceOrder` guard
(`instances.windows(2).any(|pair| (g0,r0,id0) >= (g1,r1,id1))`). `validate` separately
requires every `(logical_gate, role)` pair to be unique across `instances`
(`roles.insert((instance.logical_gate, instance.role))`, erroring
`DuplicateInstanceRole` on a repeat) — so for any two distinct instances in a valid graph,
`logical_gate` or `role` already differs. `renumber` never touches either field, only `id`.
That means the 3-tuple comparison between any two entries always resolves on its first or
second component and never falls through to comparing `id` — so however non-monotonically
`map` permutes ids, the relative order of the `(logical_gate, role, id)` tuples, and
therefore the vector's sortedness, is unaffected. Left `instances` unsorted and added a
comment in `renumber_instance_graph` recording this reasoning in place, so a future reader
doesn't have to re-derive it.

**Test:** `renumbering_resorts_assignments_after_a_non_monotonic_remap` (`relocate.rs::tests`).
Builds an `IdMap` that reverses the full adder candidate's instance ids
(`ids.iter().zip(ids.iter().rev())`) — deliberately non-monotonic, unlike every existing
test's `id.0 + 100` map — renumbers with it, then calls
`after.instances.validate(&lowered)` where `lowered` is the same lowered full-adder netlist
`seed::tests::certified_full_adder()` compiles the candidate from
(`crate::circuits::full_adder::build_full_adder_netlist()` through `lower_optimised`), per
the review's instruction that `validate` must be given the netlist the graph was built from.
Asserts `validate` succeeds.

### Small fix: `IdMap::route` now checked

`RouteId(id.0 + self.route_offset)` was unchecked `u32` addition; every comparable
identity-arithmetic site in the codebase (e.g. the duplicate-id derivation in
`InstanceGraph::with_variants`, `instance_graph.rs:270-277`) uses `checked_add(...).ok_or(...)`.
Changed `IdMap::route` to
`id.0.checked_add(self.route_offset).map(RouteId).ok_or(RelocateError::RouteIdOverflow(id))`,
added the new `RelocateError::RouteIdOverflow(RouteId)` variant, and threaded the now-fallible
`route`/`routed_sink` through every call site (`delayed_owner`, `renumber_connections`,
`renumber_routes`) with `?`. No behavioural change for any id that doesn't actually overflow
`u32`, which is every case any existing or new test exercises.

### Small fix: doc comment on `renumber` recording the fingerprint-staleness precondition

Added a doc comment on `renumber` stating plainly: the renumbered candidate's `instances`
field carries a STALE `ValidatedTopology.fingerprint`, because that fingerprint is a cached
hash over a payload containing `PrimitiveId`/`ConnectionId` values
(`topology.rs`'s `finish_topology`/`FingerprintPayload`, both private to that module) and
`renumber` has no way to recompute it from outside `topology.rs` — it can only rewrite the
ids the hash was already taken over. Named `verify.rs:274` as the structural comparison that
reads this field, and stated that a caller needing a verifiable graph after renumbering must
rebuild it with `InstanceGraph::one_to_one_with_implementations` rather than reuse the one
`renumber` produced. This documents the finding the original report already raised in prose;
per this round's brief, the report's own (separately-corrected) claim about what the
fingerprint round-trip test would or wouldn't catch was left untouched.

### Deferred by ruling

The report's claim that the fingerprint round-trip test in
`translate_moves_every_anchor_by_the_offset_and_nothing_else` would catch a field `translate`
skips is inaccurate (a field neither `translate` nor `anchors_of` visits returns to its
original value trivially under a round-trip, so a skipped field would not be caught by that
test). Left the report's prose as-is per this round's explicit instruction not to touch it;
the correction is recorded elsewhere.

### Commands run

- `cargo build --lib` — clean after the checked-`route` change, confirming the `?`
  threading compiles.
- `cargo test --lib compile::fragment_synth::relocate` — RED check: with
  `renumber_boundaries` uncalled and the `assignments.sort_by_key` removed, 3 passed, 2
  failed (`renumber_remaps_boundary_delayed_owner`: wrong `Primitive` id in the boundary's
  `delayed.owner`; `renumbering_resorts_assignments_after_a_non_monotonic_remap`:
  `NonCanonicalAssignmentOrder` from `InstanceGraph::validate`). Restored the fix from a
  backup copy of the file and confirmed it was byte-identical to what's committed.
- `cargo test --lib compile::fragment_synth::relocate` — GREEN: 5 passed, 0 failed (the
  original 3 plus the 2 new ones).
- `cargo test --lib compile::fragment_synth::instance_graph` — 10 passed, 0 failed, all
  pre-existing, unaffected by the `assignments` re-sort (their own maps stay monotonic by
  construction).
- `cargo check --lib --tests` — compiles clean, 24 warnings (was 23 with the original
  diff's own baseline check; +1 for the new `renumber_boundaries` function, currently only
  reachable from `#[cfg(test)]`, same as every other `renumber_*` helper).

### Global constraints check (this round)

- `SynthesisInput`, `compile_fragment_synth`, `ExpandedPhysicalCandidate`'s existing fields,
  `PhysicalEndpointId`, legacy front doors: untouched.
- `fingerprint()`'s output: unaffected — `renumber` and `renumber_boundaries` don't touch
  anchors, and no fingerprint test was modified.
- Determinism: `BTreeMap` only; no `HashMap`/`HashSet` introduced. `renumber_boundaries`
  mutates the existing `BTreeMap`'s values in place rather than rebuilding it, since its key
  never changes.
- Rust 2021, no new dependencies.
