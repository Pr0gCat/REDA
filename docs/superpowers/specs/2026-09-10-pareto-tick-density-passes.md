# Pareto Tick/Density Passes

Date: 2026-09-10. Base: HEAD `1cf6085`.

## Goals

- Add a per-evaluation acceptance policy so a new stage can only take a
  candidate that is non-worsening across all four `QualityKey` fields and
  strictly improves ticks, non-air blocks, or occupied volume. Static routed
  delay alone cannot retain a tick/density candidate.
- Add Pass 5, Refresh Relocation: the relocation half of Parent Route Repack
  that was specified in
  `docs/superpowers/specs/2026-09-06-hierarchical-optimization-passes.md` and
  deliberately left unimplemented.
- Keep a conditional, measurement-gated slot for Pass 6, Critical Route
  Shortcut.
- Keep every existing stage, descriptor order, refusal, trace field and
  budget-zero result byte-identical.

## Non-goals

- No change to `QualityKey`'s fields, derived `Ord`, or its use as the global
  incumbent order (`src/compile/fragment_synth/certification.rs:91`).
- No new optimizer framework, pass registry, router, dependency, configuration
  knob, or serialized trace field.
- No mutation of compiled child block interiors, pins, pin contracts, metrics,
  timing graphs, observations, or logical assignments by any proposal.
- No Cell Topology Search (see Deferred).
- No repository-wide formatting or lint cleanup inside pass commits.

## Current pipeline (as of `1cf6085`)

- `run_budgeted_proposals` (`src/compile/fragment_synth/search.rs:207`) loops:
  check budget, snapshot the incumbent fingerprint, pull one
  `ProposalEvaluation` from the stream, count one evaluation, then accept iff
  `candidate.quality() < best.quality()` (`search.rs:235`) — a single
  lexicographic comparison on the derived `Ord` of `QualityKey`.
- Each iteration pushes one `ProposalTrace` (`search.rs:46`) carrying
  `proposal_index`, parent/fragment/choice fingerprints, terminal, cap work,
  `certified_quality` and `accepted`.
- `HierarchicalProposalStream`
  (`src/compile/fragment_synth/hierarchy_api.rs:845`) is one finite stream with
  four stages in a fixed index order: Port Align Z over `edges`, Block Pull X
  over `pull_x_edges` (frozen from the incumbent when alignment is exhausted),
  Input Seam Absorption over `seams` (fixed at construction), then Parent Route
  Repack over `prunes` (frozen from the incumbent's timing graph when the seam
  stage is exhausted).
- Parent Route Repack today is pruning only: `prune_route`
  (`src/compile/fragment_synth/route_opt.rs:39`) greedily turns
  strength-redundant, non-terminal, route-owned repeaters into dust,
  downstream-first, each attempt proven by `branches_carry_through` and
  reverted exactly on failure. Descriptor order is minimum timing slack then
  route id (`route_opt.rs:132`).
- Prunes are applied inside `union_candidate`
  (`src/compile/fragment_synth/union.rs:413`) on the renumbered parent clone
  before any child is stamped (`union.rs:515`), so every touched cell is
  parent-owned by construction; a route that is gone or has nothing left to
  prune is refused as stale. `normalise_routes_and_connections`
  (`union.rs:1078`) then calls `refresh_exact_route_delays`
  (`src/compile/fragment_synth/seed.rs:2784`).
- Accepted proposals go through the unchanged flat whole-world certification
  `certify_planned` (`seed.rs:375`): realise + emit + physical verify, realised
  timing derivation and static analysis, combinational equivalence,
  compatibility views, optional exhaustive truth, transition-manifest sweep,
  then metrics including the `QualityKey` (`certification.rs:280`–`:380`).

## Acceptance policy

One private, per-evaluation policy in `search.rs`, and nothing else:

- A crate-private `enum Acceptance { Lexicographic, JointQuality }` and one
  private helper `fn accepts(policy, candidate, incumbent) -> bool`.
- `ProposalEvaluation` (`search.rs:102`) gains one crate-private field carrying
  that policy. Every existing construction site sets `Acceptance::Lexicographic`,
  so existing stages keep the exact `candidate.quality() < best.quality()` rule.
- `JointQuality` accepts iff, comparing candidate to incumbent,
  `observed_settle`, `non_air_blocks`, `occupied_volume` and
  `static_routed_delay` are each no worse, and at least one of
  `observed_settle`, `non_air_blocks`, or `occupied_volume` is strictly better.
- `ProposalTrace` is unchanged: no new field, no new serialized value, no change
  to terminals. A policy-rejected certified candidate keeps its
  `certified_quality`, `accepted = false`, and the existing `NoImprovement`
  terminal. Refused, capped and failed proposals are unaffected.

Property that keeps the global order intact: "all four no worse and at least one
of the first three strictly better" implies the first differing field decreased,
hence `candidate.quality() < incumbent.quality()` under the existing derived
`Ord`.
`JointQuality` is therefore a strict subset of today's acceptances — it can only
refuse candidates the lexicographic rule would have taken, never accept one it
would have rejected. Incumbent quality still moves monotonically down the
existing order, so the "quality never worsens" retention gate holds by
construction rather than by measurement.

Pass 5 uses `JointQuality`. Passes 1–4 stay `Lexicographic`; a future Pass 6
amendment must opt in explicitly if it survives its own design review.

For Pass 5 specifically, coordinates and occupied cells do not change, so
`non_air_blocks` and `occupied_volume` are invariant. Its acceptance rule
therefore intentionally reduces to: `observed_settle` must strictly decrease
and `static_routed_delay` must not increase. A static-delay-only relocation is
recorded as `NoImprovement`, not installed as the incumbent.

## Stage order

The finite stream order becomes:

1. Port Align Z (unchanged)
2. Block Pull X (unchanged)
3. Input Seam Absorption (unchanged)
4. Parent Route Repack — pruning (unchanged)
5. Refresh Relocation (new)
6. Critical Route Shortcut (conditional, not in the first milestone)

Pass 5 descriptors are frozen once, from the then-current incumbent, the first
time the stage is reached — after every Pass 4 descriptor has been offered.
Pass 4 and Pass 5 may therefore freeze against different incumbents and timing
graphs. Stage index arithmetic extends the existing
`edges.len() + pull_x + seams.len() + prunes.len()` chain, forcing the prune
list to freeze before its length is used. The current `?` that ends the stream
when `prune()` returns `None` must become an explicit fall-through into Pass 5.
A stage never re-freezes, and a descriptor that has become stale is refused,
never retargeted.

## Pass 5: Refresh Relocation

Implementation stays in the existing `route_opt.rs`; no relocation module or
second pass framework is added. Scope is existing internal cells of one parent
route, on the renumbered parent clone inside `union_candidate`, before any child
is stamped. At that point every `tree.cells` entry is parent-route-owned by
construction. Compiled child interiors stay opaque. Path coordinates, cell
coordinates and floors do not change.

Pass 5 must not reuse `prunable_parent_routes`: that filter deliberately drops
routes for which `prune_route` is a no-op, including the routes relocation is
meant to rescue. `union_and_certify` instead derives a separate
`refreshable_parent_routes` map from the planned parent after replaying the
candidate's accepted prune choices in their exact list order. It probes the
same `relocate_refresh` operation on a throwaway clone and retains every route
the probe changes. The candidate keeps this map separately from the existing
prune map, both keyed from original parent route id to the final timed tree id.

Descriptors reuse `ParentRouteChoice { route }` rather than introducing a
struct with the same shape. They are one per refreshable parent route, ordered
by minimum final route-arc slack then original parent route id. Their fingerprint
schemas remain distinct from prune schemas. Choices are cumulative on the
candidate in a separate `refreshes` list.

Operation, per route, deterministic and greedy in downstream-to-upstream order
(greatest maximum path index over the branches through the cell, then anchor):

1. Select a downstream, non-terminal repeater `D` that direct pruning cannot
   remove, an upstream repeater `U`, and the repeater `P` immediately preceding
   `U` on every affected branch. A first repeater has no proven incoming
   strength and is never `U`.
2. Require identical branch membership for `U` and `D`. On those branches the
   path segment from `P` through `U` to `D` must be identical and straight;
   this excludes a relocation across a divergence or merge.
3. Select the latest internal dust cell `N` strictly between `U` and `D` for
   which every affected branch has the same predecessor and successor steps at
   `N`. No branch outside the affected set may contain `N`. Facing is derived
   from that single successor direction.
4. On a throwaway tree, convert `U` and `D` to dust and place at `N` the exact
   prior state of `U`, changing only its facing. Prove both sides: the signal
   from `P` still reaches `N`, and the refreshed signal from `N` still reaches
   every affected terminal. Dust consumes one strength, a correctly oriented
   repeater restores full strength, anything else is not a conductor, and zero
   strength fails.
5. Each relocation must reduce the route's repeater count by exactly one; a step
   that does not is undone.
6. The whole route is one proposal. It is emitted only if the route's repeater
   count strictly falls. Each tentative step reverts to the exact prior state on
   failure, and later steps are proven against the state left by retained
   earlier ones.

This local proof covers route topology, repeater direction and signal strength;
it does not claim to prove cross-route electrical coupling. The unchanged
physical verifier, equivalence proof and transition simulation remain the only
authority for coupling and behaviour, and may refuse a locally plausible tree.

Rebuild order is fixed: `union_candidate` applies every accepted prune in list
order, then every accepted refresh relocation in list order, then stamps child
blocks. A relocation never re-runs pruning, even if it makes another deletion
look possible. Replaying an accepted incumbent therefore starts from the same
planned tree and repeats the same successful operations in the same order.

Refusal cases, each refused before certification and covered by a named
assertion in the smallest grouped route fixtures:

- the descriptor names a route the union no longer has;
- the route has no `(P, U, N, D)` relocation that reduces its repeater count;
- `U` is the route's first repeater, or affected branches do not share the same
  predecessor refresh and identical straight segment;
- branch membership differs at `U`, `N`, or `D`, or any of them is a branch
  terminal;
- the run contains a missing cell or anything other than the required
  parent-route-owned dust/repeaters;
- the incoming `P -> N` or outgoing `N -> terminal` strength proof fails;
- branches disagree on the facing at `N`;
- a branch path does not end at its recorded terminal, or a cell on it is
  missing or not a conductor.

After mutation the existing `normalise_routes_and_connections` /
`refresh_exact_route_delays` path recomputes delays; nothing else is edited.

## Pass 6: Critical Route Shortcut (conditional)

Not part of the first milestone. It may be specified into implementation only
when both hold, measured fresh on this branch:

- Passes 1–5 have plateaued on the acceptance corpus — a full budgeted run
  produces no further accepted candidate — and
- a recorded trace shows a shortcut would remove a repeater on the critical
  path. Dust length alone carries no timing cost, so a shortcut that only
  shortens dust is not a candidate.

If both hold, write and approve a Milestone 2 amendment defining the mutation,
ownership, coupling and strength proofs before implementation. This document
does not pre-commit that unmeasured design.

## Deferred: Cell Topology Search

Still deferred. The flat optimizer already searches implementation, facing and
placement, and the current cell library has few genuine implementation
alternatives. Do not add module-rebuild caches or mutate child gate topology
until a real ALU/CU case demonstrates a candidate with multiplicative benefit,
and then only under a separate amendment to this spec — not as an extension of
Pass 5 or Pass 6.

## Budget and time semantics

- Budget zero preserves the existing candidate, pinned IO and the flat path bit
  for bit. No new stage runs at budget zero.
- Every offered descriptor costs exactly one evaluation, including refusals and
  stale descriptors, as today.
- `SynthesisBudget::Time` stops only between complete proposals; the check stays
  where it is at the top of the loop.
- Traces remain deterministic prefixes: a run with budget `n` produces the first
  `n` entries of a run with budget `m > n` for the same case.
- Stage freezing happens at most once per stream and only on first reach.

## Certification and pinned IO invariants

- Every accepted candidate is a full flat whole-world certification through the
  unchanged `certify_planned` path. The strength walk is a rejection filter,
  never a substitute.
- No proposal writes metrics, timing graphs, observations, pins, pin contracts,
  or logical assignments.
- Pass 5 edits only internal parent-route cells; it does not move coordinates,
  floors, branch terminals, pinned IO placements, or the pinned handover
  contract. The unchanged certification path re-verifies those invariants.
- Pass 6 is not implemented by this milestone and cannot claim those invariants
  until its separate amendment proves them.
- Distinct modules remain compile-once/stamp-many; no pass rebuilds a child.

## TDD requirements

Every behaviour lands red first, at the smallest level that can fail:

- Acceptance policy: `JointQuality` refuses lower-settle/more-blocks,
  static-delay-only and all-equal candidates; accepts an otherwise-equal
  candidate improved on settle, blocks, or volume in turn; and `Lexicographic`
  reproduces today's result on the same inputs.
- Trace shape: every existing production and test construction site sets
  `Lexicographic` explicitly and an existing scripted stream produces
  byte-identical `ProposalTrace` output. A certified static-delay-only
  `JointQuality` result keeps `certified_quality`, uses `NoImprovement`, and has
  `accepted = false`.
- Relocation mutation: a fixture route where relocation removes exactly one
  repeater, asserted on cell kinds and repeater count.
- Grouped route fixtures with a named assertion for every refusal case listed
  above; do not create one test function per row when one fixture proves several.
- Stream tests: stage index boundaries, prune-exhaustion fall-through,
  freeze-once against the post-Pass-4 incumbent, stale-descriptor refusal, and
  that the Passes 1–4 descriptor order is unchanged.
- Rebuild-order test: accepted prunes replay before cumulative refreshes, and a
  prior accepted choice cannot become a no-op because of a later operation.
- Certified acceptance tests on real circuits stay `#[ignore]`d with a stated
  cost, matching the existing hierarchical acceptance tests.

## Baseline and acceptance corpus

- Capture a fresh baseline at the pass branch point with
  `fragment_baseline --output %TEMP%\\reda-pareto-baseline.json`, after removing
  only that exact temporary output if it already exists, and record the full
  baseline commit id beside the captured file's hash.
  Historical numbers are not reused as gates.
- Corpus for retention: the hierarchical circuits `ripple_adder8`,
  `multiplier4`, `alu4_full`, `alu8`, plus the `fragment_acceptance` evaluator
  over budgets `[0, 1, 2, 4, 8]`.
- Report `observed_settle`, `non_air_blocks`, `occupied_volume`,
  `static_routed_delay`, evaluations used, stop reason and wall time separately,
  per circuit, with the command transcript.

### Historical evidence (motivation only, not authoritative)

The 2026-09-07 retention report recorded certified wins for Pull-X, Seam
Absorption and pruning, and a feasibility screen that found removable repeaters
still present on `multiplier4` parent routes. That is why relocation is worth
one more attempt. Those figures were captured under a budget and revision
provenance this spec does not restate; no gate in this document may be evaluated
against them.

## Retention gates and revert-on-miss

A new pass stays in production only if all of these hold against the fresh
baseline:

1. focused red/green tests cover its mutation and every refusal case;
2. traces remain deterministic prefixes and incumbent quality never worsens;
3. budget zero and pinned IO results are unchanged;
4. at least one corpus circuit accepts a `JointQuality` candidate with lower
   `observed_settle`; Pass 5 cannot change occupied cells, so block/volume
   improvements are not claimed for this milestone, and a static-delay-only
   result is reported but does not retain it;
5. that candidate passes unchanged full certification;
6. the win is not confined to a synthetic fixture.

Miss any gate and the revert is complete, in one commit: the stage, its
descriptors, its stream arithmetic, its tests, and any acceptance-policy
plumbing no surviving stage uses. No dormant enum variant, unused descriptor
type, disabled knob or commented-out stage is left behind. A revert must restore
stream indices and trace output to the pre-pass bytes, proven by rerunning the
prefix-determinism and budget-zero tests.

## Portability

- No new dependency, no platform-conditional code, no threads, no filesystem or
  clock use inside a pass.
- Determinism must not depend on worker count, hash iteration order or
  wall-clock time; all descriptor ordering uses total orders over ids and
  anchors.
- The crate must continue to build for `wasm32` targets once the maintenance
  lane's `atomic_publish` gap is closed; no pass may add a host-only path.
- Final verification includes the native viewer tests and
  `wasm-pack build --target web` after the maintenance fix lands.
- Cargo commands run serially in this Windows worktree.

## Maintenance lane

Pre-existing repository debt, reproduced at a fixed pre-branch baseline in the
2026-09-09 report and therefore not caused by this work. It is sequenced as its
own commits, never folded into a pass commit, and no pass gate is blocked on it
except where stated:

1. **wasm `atomic_publish`** — `src/compile/fragment_synth/benchmark.rs:1223`
   calls `atomic_publish`, which is defined only under `cfg(windows)` and
   `cfg(unix)`, so `wasm-pack test` and `wasm-pack build --target web` fail with
   E0425. Fix first, as its own commit, since the portability invariant above
   depends on it.
2. **Fixture regeneration** — `tests/fixtures/fragment_synth_baseline.json`
   carries a stale verifier-revision hash. Regenerate cleanly from a documented
   command on an otherwise clean tree, in its own commit, before the fresh
   baseline capture, so the corpus is not measured against a known red fixture
   check.
3. **Strict Clippy** — `cargo clippy --all-targets -- -D warnings` is red on
   pre-existing library and test diagnostics. Fix in its own commit or commits,
   grouped by lint, touching no pass logic. Re-measure the diagnostic counts
   when the work starts rather than trusting the historical tally.
4. **Whole-repository `cargo fmt`** — deferred. Formatting drift is broad and a
   repo-wide reformat would bury every pass diff. Each pass commit keeps only
   its own touched files `rustfmt`-clean; the global pass is a separate, later,
   standalone commit.

## Milestones

**Milestone 1 (this cycle): acceptance policy + Pass 5 only.** Land the private
`Acceptance` policy with its tests, then Refresh Relocation with its mutation
and refusal tests, then measure the corpus against the fresh baseline and apply
the retention gates. This milestone targets ticks while requiring density to
remain unchanged; it does not claim a density win. Nothing else ships in this
milestone.

**Milestone 2 (conditional): Pass 6.** Requires a fresh measured go/no-go after
Milestone 1: recorded plateau evidence plus a trace showing a shortcut would
remove a critical-path repeater. Without both, Pass 6 is not implemented and
this spec closes at Milestone 1.
