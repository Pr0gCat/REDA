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
