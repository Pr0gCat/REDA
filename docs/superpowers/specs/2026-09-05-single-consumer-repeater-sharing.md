# Single-consumer repeater sharing

## Evidence

The block-alignment proposal stream of `docs/superpowers/specs/2026-09-05-timing-aware-module-floorplan.md` has been run to exhaustion on programmatic hierarchical `ripple_adder8`.  Best certified result: **576 observed settle ticks / 71,147 non-air blocks**.  Target: **474 / 100,615**.  Size passes with room to spare; latency misses by **102 ticks**.  That spec names this design as the next experiment.

## What changes, in plain terms

Every parent wire into a block input ends today in a repeater standing on the child's lever cell: `place_blocks` registers each input `TargetGeometry` with `TerminalRequirement::Exact(RouteTerminalKind::OutputTerminalRepeater)`.  The wire that arrives there was already driven by the *source* block's own output repeater.  Two diodes in series where the second one only repeats what the first already guaranteed.

Keep the source block's output repeater.  Where the wire provably has one consumer and arrives straight and strong enough, end the parent's route in directed dust instead of a repeater.  One repeater delay -- at least two ticks -- disappears per shared edge.

## Eligibility

An edge is a sharing candidate only when all three hold:

1. It is already an entry of `hierarchy_api::explicit_block_edges` -- a legal explicit `BlockEdge`, both endpoints resolving through `InstanceGraph::block`, driver a single-terminal `InstanceDriver::Primitive` whose `logical_owner` is the source block.
2. The source `PhysicalEndpointId::PrimitiveOutput` has **exactly one** sink.  The scan is over the *parent* `InstanceGraph::assignments`, using the same `endpoint_for_driver` identity `route_schedule::schedule_routes` uses to resolve a driver to its sinks -- not a count of `hierarchy_api::explicit_block_edges` entries, which would miss a parent gate input or declared-output sink fed by the same driver and wrongly pass a multi-consumer edge as single-consumer.  One consumer means removing this sink repeater cannot change some other consumer's isolation.
3. The child's required strength at its root dust is computable from data the child's own compile already certified -- no re-walk of world cells.  Add `required_root_strength(&CompiledBlock, PortId) -> Option<u8>` in `blocks.rs`, next to `CompiledBlock` (no new struct needed).  Find the `candidate.routes` entry whose source is `PhysicalEndpointId::PrimaryInput(port)` and index that `RealisedRouteTree`'s typed `cells` by anchor.  For each branch, `branch.path[0]` is the root dust.  If the first actual repeater is at zero-based path index `r`, the root must carry at least `max(1, r)`; if there is no repeater, it must carry at least `branch.path.len()`.  Take the maximum over all branches.  A missing route/cell, a non-dust root, an empty path, or a result above `MAX_SIGNAL_STRENGTH` is `None` -- ineligible.  This uses interior refreshes as the cutoff; terminal kind alone is insufficient because a terminal repeater at the end of a long dust run does not reduce the strength needed to reach it.

This filter only decides which proposals are worth an evaluation.  The router still decides per route: `routing::select_terminal_kind` keeps its collinearity check (`terminal_style`, incoming direction == outgoing), its isolation check (`terminal_is_isolated_typed`), its predecessor strength and its `budget_needs_repeater` refresh check, and falls back to `RouteTerminalKind::RepeaterIntoSupport` when any of them fails.

## Minimal API

- `routing::TerminalRequirement::AutomaticAtLeast(u8)` -- one new variant, one new arm in `select_terminal_kind`: directed dust iff `!budget_needs_repeater` and isolation proven and incoming == outgoing and `predecessor_strength.saturating_sub(2) >= min`, else `RepeaterIntoSupport`.  Subtract 2, not 1: predecessor -> parent terminal dust -> child root dust is two dust hops, and `min` is the required carried strength *after* that root dust.  `Automatic` keeps its existing arm unchanged; no existing caller changes.
- `seed::place_blocks` takes one more parameter, `shared_inputs: &BTreeMap<(InstanceId, u16), u8>`.  A listed input gets `AutomaticAtLeast(n)`; every other input keeps `Exact(OutputTerminalRepeater)`.  Threaded beside the existing `block_placements` through `plan_parent_with_services` and `hierarchy_api::compile_module_with_blocks`.
- `HierarchicalCandidate` gains `shared_inputs`, accumulated exactly like `block_placements`: a proposal clones the incumbent's map and inserts one entry, and `run_budgeted_proposals` only keeps it on strict `QualityKey` improvement.
- `HierarchicalProposalStream::next`: `proposal_index < edges.len()` stays the alignment proposal, unchanged; beyond that it indexes a `shareable: Vec<BlockEdge>` computed once at stream construction.  This list cannot go stale: eligibility reads only the immutable parent assignments and already-compiled child routes; proposal-specific geometry, isolation and predecessor strength remain router checks.  The alignment prefix is untouched, so the already-pinned `Evaluations(n)` staircases stay byte-identical.
- Fingerprints are versioned: `HierarchicalChoiceFingerprint` carries `shared_inputs` and becomes `hierarchical-block-choice-v2`; a sharing fragment fingerprints as `hierarchical-shared-input-v1` over its `BlockEdge` plus required strength.  Choice fingerprints are trace values, not candidate identity, so only trace assertions need re-pinning.

## Certification, fallback, determinism

- Certification is unchanged.  `union::union_candidate` and `seed::certify_planned` run full physical verification, equivalence proof and simulation on every proposal; eligibility avoids wasted evaluations, it is not the proof.
- Fallback is silent and local: the router substitutes a repeater and the route succeeds.  A sharing proposal is at worst `ProposalTerminal::NoImprovement`.
- Determinism: `shareable` derives from the same sorted `explicit_block_edges`, every map is a `BTreeMap`, nothing consults a clock.
- `SynthesisBudget::Evaluations(0)` evaluates no proposal, so budget-zero hierarchical output is bit-identical to today.  Flat synthesis never calls `place_blocks` (`ParentBlocks::none()`) and is untouched.

## TDD

1. **Pure strength and shareability.**  Over hand-built typed routes, pin the exact boundary: first repeater at indices 1 and 4 requires root strengths 1 and 4; no repeater on a five-cell branch requires 5; fanout takes the max; missing cells and requirements above 15 return `None`.  On a hand-built `InstanceGraph`, prove the filter accepts one consumer and rejects a second consumer visible only in `assignments`, a `None` strength, and a non-block edge.  No compile, no router.
2. **Terminal decision.**  Test `AutomaticAtLeast` directly: `predecessor_strength - 2 == min` chooses dust, one less chooses a repeater, and each of `budget_needs_repeater`, failed isolation and a bend independently forces the repeater fallback.
3. **Real landing.**  Compile a two-block fixture with sharing enabled and assert the emitted world holds `BlockKind::RedstoneWire` at the shared lever cell, and `BlockKind::Repeater` at a control fixture whose geometry fails collinearity.  Asserting landed block kinds, not just ticks, proves the splice happened.
4. **Measurement.**  Reuse `seed.rs::tests::ripple_adder8_hierarchical_budget_target_oracle` unchanged.  It already requires one certified best to satisfy both gates and otherwise reports a bounded negative result.

## Ceiling: this does not reach 474

Programmatic `ripple_adder8` has seven `cout -> cin` edges on its critical path, two ticks each at the minimum repeater delay: **7 * 2 = 14 ticks** is a conservative upper bound on what sharing those seven edges could remove, not a prediction -- certification, fallback, and every other edge's own routing can all reduce the real number below it.  576 - 14 = **562**, still 88 above the target even under that best case; the real number is only known once `ripple_adder8_hierarchical_budget_target_oracle` measures a certified best, and that oracle measurement remains authoritative over this bound.  Repeater sharing cannot close the 102-tick gap and is not proposed as a way to.  It is the next bounded experiment -- it measures whether the block port contract can safely give up a diode at all -- and its outcome is a number to report, pass or fail, not a promise to hit 474.

## The next smallest direction after this (not in scope)

`circuits::hierarchical_builder::expose` realises every module output as `NOT(NOT(value))`: two `Nor(1)` gates whose only job is to carry a literal port name -- 32 extra lowered gates (200 vs 168) and two gate delays per module output, eight times over on this critical path.  The fix belongs in `compile::hierarchy`'s `flatten`, as a general rule: collapse a `Nor(1) -> Nor(1)` chain whose intermediate has exactly one consumer into an alias of the source signal.  Editing the benchmark's builder instead would move the number without fixing anything.  It needs its own goal and spec; this design does not implement it.
