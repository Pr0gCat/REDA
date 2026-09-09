# Refresh Relocation Tick Pass, with a Density Non-Regression Guard

Date: 2026-09-10. Base: HEAD `37e9266`
(`37e92660fe8247d2fb74511883a70ef3501d8310`). Every source anchor below is as
of that commit.

This document keeps its original filename. Its scope is narrower than the
filename suggests and the honest scope is the one below: **one new tick-only
hierarchical pass, plus an acceptance policy that guarantees the pass cannot
retain a density regression.** Nothing here improves density, and no gate in
this document may be read as a density win.

## Goals

- Add Pass 5, Refresh Relocation: the relocation half of Parent Route Repack
  that was specified in
  `docs/superpowers/specs/2026-09-06-hierarchical-optimization-passes.md` and
  deliberately left unimplemented. It targets `observed_settle` by removing one
  repeater from a parent route without moving any cell.
- Add a per-evaluation acceptance policy so that a Pass 5 candidate is retained
  only when `observed_settle` strictly improves and the other three `QualityKey`
  fields do not worsen. This is the **density
  non-regression guard**: it makes "Pass 5 never trades density for ticks" a
  proven property of the acceptance rule instead of an expectation about the
  mutation. Static routed delay alone cannot retain a candidate.
- Keep every existing stage, descriptor order, refusal, trace field and
  budget-zero result byte-identical.

## Non-goals

- **No density improvement.** Pass 5 changes no coordinate and no occupied
  cell, so `non_air_blocks` and `occupied_volume` are invariant under it. This
  document neither claims nor implements a density optimization. Density
  optimization is a separate future spec, written only after this pass lands or
  is reverted; it is not an amendment to this one.
- No guard over Passes 1-4. They keep `Acceptance::Lexicographic` and therefore
  keep exactly today's behaviour, including today's freedom to accept a tick win
  that costs blocks or volume. The guard is scoped to Pass 5 acceptances and
  nothing else.
- No change to `QualityKey`'s fields, derived `Ord`, or its use as the global
  incumbent order (`src/compile/fragment_synth/certification.rs:91`).
- No new optimizer framework, pass registry, router, dependency, production
  configuration knob, or serialized trace field.
- No mutation of compiled child block interiors, pins, pin contracts, metrics,
  timing graphs, observations, or logical assignments by any proposal.
- No Pass 6 and no Cell Topology Search (see Deferred).
- No repository-wide formatting or lint cleanup inside pass commits.
- No maintenance-lane work in this cycle's implementation plan (see Maintenance
  lane: it is a prerequisite and a follow-up, never a pass commit).

## Current pipeline (as of `37e9266`)

- `run_budgeted_proposals` (`src/compile/fragment_synth/search.rs:207`) loops:
  check budget, snapshot the incumbent fingerprint, pull one
  `ProposalEvaluation` from the stream, count one evaluation, then accept iff
  `candidate.quality() < best.quality()` (`search.rs:234`) -- a single
  lexicographic comparison on the derived `Ord` of `QualityKey`.
- Each iteration pushes one `ProposalTrace` (`search.rs:45`) carrying
  `proposal_index`, parent/fragment/choice fingerprints, terminal, cap work,
  `certified_quality` and `accepted`.
- `HierarchicalProposalStream` (`src/compile/fragment_synth/hierarchy_api.rs:845`)
  is one finite stream with four stages in a fixed index order: Port Align Z
  over `edges`, Block Pull X over `pull_x_edges` (frozen from the incumbent when
  alignment is exhausted), Input Seam Absorption over `seams` (fixed at
  construction), then Parent Route Repack over `prunes` (frozen from the
  incumbent's timing graph when the seam stage is exhausted,
  `hierarchy_api.rs:918`).
- Parent Route Repack today is pruning only: `prune_route`
  (`src/compile/fragment_synth/route_opt.rs:39`) greedily turns
  strength-redundant, non-terminal, route-owned repeaters into dust,
  downstream-first, each attempt proven by `branches_carry_through`
  (`route_opt.rs:82`) and reverted exactly on failure. Descriptor order is
  minimum timing slack then route id (`prune_descriptors`, `route_opt.rs:132`).
- Prunes are applied inside `union_candidate`
  (`src/compile/fragment_synth/union.rs:413`) on the renumbered parent clone
  before any child is stamped (`union.rs:525`), so every touched cell is
  parent-owned by construction; a route that is gone or has nothing left to
  prune is refused as stale. `normalise_routes_and_connections`
  (`union.rs:1078`) then calls `refresh_exact_route_delays`.
- `union_and_certify` (`hierarchy_api.rs:443`) computes
  `prunable_parent_routes` (`hierarchy_api.rs:489`) on **every** compile and
  narrows `union_candidate`'s complete parent-route map to the prunable subset
  (`hierarchy_api.rs:473`).
- Accepted proposals go through the unchanged flat whole-world certification
  `certify_planned` (`src/compile/fragment_synth/seed.rs:375`), which calls
  `SparseSeedBuilder::finish_attempt` (`seed.rs:776`) with `pins: None`:
  `validate_shape` + `validate_physical_ownership`, then realise, emit,
  physical verify, realised timing derivation and static analysis,
  combinational equivalence, compatibility views, optional exhaustive truth,
  transition-manifest sweep, then metrics including the `QualityKey`.

## Acceptance policy

One private, per-evaluation policy in `search.rs`, and nothing else:

- A crate-private `enum Acceptance { Lexicographic, JointQuality }` and one
  private helper
  `fn accepts(policy, candidate: QualityKey, incumbent: QualityKey) -> bool`.
- `ProposalEvaluation` (`search.rs:102`) gains one crate-private field carrying
  that policy. Every existing construction site sets `Acceptance::Lexicographic`,
  so existing stages keep the exact `candidate.quality() < best.quality()` rule.
- `JointQuality` accepts iff, comparing candidate to incumbent,
  `observed_settle` is strictly lower and `non_air_blocks`, `occupied_volume`
  and `static_routed_delay` are each no worse.
- `ProposalTrace` is unchanged: no new field, no new serialized value, no change
  to terminals. A policy-rejected certified candidate keeps its
  `certified_quality`, `accepted = false`, and the existing `NoImprovement`
  terminal. Refused, capped and failed proposals are unaffected.

### Stage-selection wiring

The policy is chosen where the stage is chosen, not where the evaluation is
built. `HierarchicalProposalStream::next` (`hierarchy_api.rs:930`) already picks
one stage per index and yields a tuple of everything that stage decided; that
tuple explicitly carries the policy as its last element:

- Passes 1-4 (Port Align Z, Block Pull X, Input Seam Absorption, Parent Route
  Repack) each yield `Acceptance::Lexicographic`.
- Pass 5 (Refresh Relocation) yields `Acceptance::JointQuality`.
- The three shared `ProposalEvaluation` constructions below the selection (stale
  refusal, successful compile, failed compile) all set `acceptance` from that one
  tuple element. No construction site names a policy literally, so a future stage
  cannot silently inherit the wrong one.

### Why the guard cannot disturb the global order

"Settle strictly lower and every later field no worse" implies the first field
decreased, hence
`candidate.quality() < incumbent.quality()` under the existing derived `Ord`.
`JointQuality` is therefore a strict subset of today's acceptances -- it can only
refuse candidates the lexicographic rule would have taken, never accept one it
would have rejected. Incumbent quality still moves monotonically down the
existing order, so the "quality never worsens" retention gate holds by
construction rather than by measurement.

For Pass 5, coordinates and occupied cells do not change, so `non_air_blocks`
and `occupied_volume` are invariant. A lower-settle candidate that raises static
routed delay, and a static-delay-only improvement, are both recorded as
`NoImprovement` and are not installed as the incumbent.

## Stage order

The finite stream order becomes:

1. Port Align Z (unchanged)
2. Block Pull X (unchanged)
3. Input Seam Absorption (unchanged)
4. Parent Route Repack -- pruning (unchanged)
5. Refresh Relocation (new, last)

There is no sixth stage. Stage index arithmetic extends the existing
`edges.len() + pull_x + seams.len() + prunes.len()` chain by that same shape,
forcing the prune list to freeze before its length is used. The current `?` that
ends the stream when `prune()` returns `None` becomes an explicit fall-through
into Pass 5. A stage never re-freezes, and a descriptor that has become stale is
refused, never retargeted.

## Pass 5: Refresh Relocation

Implementation stays in the existing `route_opt.rs`; no relocation module and no
second pass framework is added. Scope is existing internal cells of one parent
route, on the renumbered parent clone inside `union_candidate`, before any child
is stamped. At that point every `tree.cells` entry is parent-route-owned by
construction. Compiled child interiors stay opaque. Path coordinates, cell
coordinates and floors do not change. A successful step changes only three
existing states: `U` and `D` become dust and `N` receives `U`'s repeater state
with the proven facing. No cell is inserted, removed, or moved.

### Descriptor freezing (no per-compile probing)

`union_and_certify` must **not** compute or probe refreshability. It keeps its
existing per-compile work exactly as it stands, plus one clone:

- `prunable_parent_routes` and the `retain` that narrows the prune map stay where
  and as they are, so the Pass 1-4 prune map, its ordering and its fingerprints
  are byte-for-byte what they are today.
- The candidate additionally keeps `union_candidate`'s already-produced complete
  parent-route map (pre-union parent `RouteId` to final timed tree `RouteId`),
  captured by cloning it before the existing `retain` narrows it. That is the
  minimum needed: Pass 5 rescues exactly the routes `prunable_parent_routes`
  drops, so the prunable-only map cannot name them, and the slack lookup needs
  the final timed tree id.

Refresh descriptors are frozen **lazily, once**, the first time Pass 5 is
reached -- after every Pass 4 descriptor has been offered. The freeze, and only
the freeze:

1. reads the then-current incumbent's planned parent trees
   (`planned.candidate.routes`, the pre-union trees the union mutates);
2. replays that incumbent's accepted prune choices, in their exact list order, on
   a throwaway clone of each named tree, so the probe sees the same tree the
   union will;
3. probes `relocate_refresh` on that clone and retains every route the probe
   changes;
4. orders the retained routes by minimum analysed slack of the final tree that
   carries them, then by original parent route id -- the same total order
   `prune_descriptors` uses, through the same shared helper.

Consequences that are part of the contract:

- Budget zero runs no probe at all, and neither does any Pass 1-4 evaluation.
- Pass 4 and Pass 5 may freeze against different incumbents and timing graphs.
- A route whose probe fails is never offered, so no evaluation is spent on it.
- The freeze happens at most once per stream.

Descriptors reuse `ParentRouteChoice { route }` rather than introducing a struct
with the same shape. Their fingerprint schemas
(`hierarchical-parent-refresh-fragment-v1`,
`hierarchical-parent-refresh-choice-v1`) are distinct from the prune schemas.
Choices are cumulative on the candidate in a separate `refreshes` list.

### Operation

Per route, deterministic and greedy in downstream-to-upstream order. Candidate
`(U, D)` pairs are ordered by greatest maximum path index of `D` over the
branches through it, then `D`'s anchor, then greatest maximum path index of `U`,
then `U`'s anchor; `Anchor`'s derived `Ord` is the only tiebreak and no
comparison depends on map iteration order, worker count or wall clock.

1. Select a downstream, non-terminal repeater `D` that direct pruning cannot
   remove -- proven by turning `D` to dust on a throwaway clone and requiring the
   existing `branches_carry_through` to refuse it. Select an upstream repeater
   `U`, and the repeater `P` immediately preceding `U` on every affected branch.
   A first repeater has no proven incoming strength and is never `U`.
2. Require identical branch membership for `U` and `D`: every branch through one
   passes through the other. On those branches the path segment from `P` through
   `U` to `D` must be identical and straight -- every consecutive step the same
   unit horizontal step. This excludes a relocation across a divergence, a merge,
   a bend or a vertical step.
3. Enumerate the topology-legal candidate cells `N` -- the cells strictly between
   `U` and `D` on that identical segment that are route-owned dust, that have the
   same predecessor and successor step on every affected branch, and that no
   branch outside the affected set contains -- **from downstream to upstream**
   (descending path index, then anchor). If the enumeration is empty, refuse this
   `(U, D)` pair: an empty window is never silently widened past `U` or `D`.
4. Take the **first** enumerated `N` that passes **both** post-mutation strength
   proofs, on a throwaway tree in which `U` and `D` are dust and `N` carries the
   exact prior state of `U` with only its facing changed to the successor
   direction:
   - **incoming**: on every affected branch, the signal from `P` still reaches
     `N`;
   - **outgoing**: on every affected branch, the refreshed signal from `N` still
     reaches that branch's recorded terminal.

   Dust consumes one strength, a correctly oriented repeater restores full
   strength, anything else is not a conductor, and zero strength fails -- the same
   walk `branches_carry_through` already performs. **If the latest candidate
   fails, the enumeration continues to earlier candidates**; the pair is refused
   only when every candidate fails. Selecting only the latest legal cell would
   refuse relocations that are provably safe one cell upstream, which is the
   defect this rule exists to prevent.
5. Each relocation must reduce the route's repeater count by exactly one; a step
   that does not is undone.
6. The whole route is one proposal. It is emitted only if the route's repeater
   count strictly falls. Each tentative step reverts to the exact prior state on
   failure, and later steps are proven against the state left by retained earlier
   ones.

This local proof covers route topology, repeater direction and signal strength;
it does not claim to prove cross-route electrical coupling. The unchanged
physical verifier, equivalence proof and transition simulation remain the only
authority for coupling and behaviour, and may refuse a locally plausible tree.

### The linear regression fixture

One route, one branch, all on `y = 0, z = 0`, in the existing route-fixture
convention (`union::tests::seam_tree`'s cell/branch/terminal shape,
`MAX_SIGNAL_STRENGTH = 15`):

- `P`: repeater at `x = 0`
- dust `x = 1..=8`
- `U`: repeater at `x = 9`
- dust `x = 10..=17`
- `D`: repeater at `x = 18`
- dust `x = 19..=26`
- terminal repeater at `x = 27`

Pruning cannot remove either interior repeater: with `U` as dust the walk from
`P` dies at `x = 15`, and with `D` as dust the walk from `U` dies at `x = 24`.
Relocation is the only pass that can shorten this route.

The walk restarts at 15 immediately after a repeater and each dust costs one, so
a repeater at `N` needs `N - P <= 15` (incoming) and `terminal - N <= 15`
(outgoing) -- legal `N` is exactly `12..=15`. Enumerating downstream to upstream:

| `N` | incoming `P -> N` | outgoing `N -> 27` | verdict |
| --- | --- | --- | --- |
| 17 | dies at `x = 15` | would pass | **fails**, continue earlier |
| 16 | dies at `x = 15` | would pass | fails, continue earlier |
| 15 | dust 1..=14 at strength 14..=1 | dust 16..=26 at 14..=4 | **passes, selected** |
| 13 | dust 1..=12 at strength 14..=3 | dust 14..=26 at 14..=2 | **passes**, never reached: the scan stopped at 15 |
| 11 | passes | dies at `x = 26` | fails |

The fixture must assert all of: the latest candidate `N = 17` fails the incoming
proof, the scan continues earlier rather than refusing the route, `N = 13` does
pass both proofs on its own, and the selected relocation is the first success at
`N = 15` -- repeaters at `{0, 9, 18, 27}` become `{0, 15, 27}`, exactly one fewer.

Note for reviewers: an earlier draft of this spec claimed `N = 13` would be
*selected*. It is legal, but it is not selected, because `N = 15` is legal too and
is enumerated first. The strength arithmetic above, not the draft, is
authoritative.

### Refusal cases

Each refused before certification and covered by a named assertion in the
smallest grouped route fixtures:

- the descriptor names a route the union no longer has;
- the route has no `(P, U, N, D)` relocation that reduces its repeater count;
- `U` is the route's first repeater;
- branch membership differs at `U`, `N`, or `D`, or any of them is a branch
  terminal;
- affected branches do not share the same predecessor refresh `P`, or the
  `P -> U -> D` segment is not identical and straight across them;
- the candidate window strictly between `U` and `D` is empty;
- `D` is a repeater direct pruning could have removed;
- the run contains a missing cell or anything other than the required
  parent-route-owned dust/repeaters;
- every candidate `N` fails the incoming `P -> N` or outgoing `N -> terminal`
  proof;
- branches disagree on the facing at `N`, or the step at `N` is not a unit
  horizontal step;
- a branch path does not end at its recorded terminal, or a cell on it is missing
  or not a conductor.

### Rebuild order

`union_candidate` applies every accepted prune in list order, then every accepted
refresh relocation in list order, then stamps child blocks. A relocation never
re-runs pruning, even if it makes another deletion look possible. Replaying an
accepted incumbent therefore starts from the same planned tree and repeats the
same successful operations in the same order. After mutation the existing
`normalise_routes_and_connections` / `refresh_exact_route_delays` path recomputes
delays; nothing else is edited.

## Deferred, in one place

- **Pass 6, Critical Route Shortcut.** Not specified, not scheduled, not
  measured, and deliberately carrying no milestone in this document. If Passes
  1-5 plateau and a recorded trace ever shows a shortcut removing a critical-path
  repeater, that evidence starts a separate spec of its own.
- **Density optimization.** A separate future spec, as stated in Non-goals.
- **Cell Topology Search.** Still deferred. The flat optimizer already searches
  implementation, facing and placement, and the current cell library has few
  genuine implementation alternatives. Do not add module-rebuild caches or mutate
  child gate topology until a real ALU/CU case demonstrates a candidate with
  multiplicative benefit, and then only under its own spec.

## Budget and time semantics

- Budget zero preserves the existing candidate, pinned IO and the flat path bit
  for bit. No new stage runs, and no refresh probe runs, at budget zero.
- Every offered descriptor costs exactly one evaluation, including refusals and
  stale descriptors, as today.
- `SynthesisBudget::Time` stops only between complete proposals; the check stays
  where it is at the top of the loop.
- Traces remain deterministic prefixes: a run with budget `n` produces the first
  `n` entries of a run with budget `m > n` for the same case.
- Stage freezing happens at most once per stream and only on first reach.

## Certification and pinned IO invariants

- Every accepted candidate is a full flat whole-world certification through the
  unchanged `certify_planned` path. The strength walk is a rejection filter, never
  a substitute.
- No proposal writes metrics, timing graphs, observations, pins, pin contracts, or
  logical assignments.
- Pass 5 edits only internal parent-route cells; it does not move coordinates,
  floors, branch terminals, pinned IO placements, or the pinned handover contract.
- What re-verifies pins, stated exactly: `certify_planned` builds its `SeedInput`
  with `pins: None` (`seed.rs:378`), so `finish_attempt`'s `validate_shape`
  (`candidate.rs:630`) runs `validate_pin_contracts` (`candidate.rs:1052`) -- the
  candidate's **internal** pin-contract consistency: `pins`, `pin_name_bindings`
  and `pin_contracts` agree with each other, no `toward` is vertical, and each
  endpoint has its expected observation and delayed owner. It does **not**
  re-compare the candidate's pins against the caller's requested
  `PortPlacements`; that comparison is `validate_pin_contracts_against`
  (`candidate.rs:1018`), which this path does not call. Requested-placement
  equality is therefore proven by dedicated tests that hold the requested pins and
  check the emitted cells -- `tests/build_circuit_pins.rs:510`
  (`compile_hierarchical_preserves_the_checked_seven_segment_pin_contract`) and
  the new hierarchy-with-a-child pinned fixture below -- not by `certify_planned`.
- Distinct modules remain compile-once/stamp-many; no pass rebuilds a child.

## TDD requirements

Every behaviour lands red first, at the smallest level that can fail:

- Acceptance policy: `JointQuality` refuses a lower-settle candidate that raises
  static delay, blocks, or volume; refuses static-delay-only and all-equal
  candidates; and accepts only lower settle with the other three fields no
  worse. `Lexicographic` reproduces today's result on the same inputs.
- Trace shape: every existing production and test construction site sets
  `Lexicographic` explicitly and an existing scripted stream produces
  byte-identical `ProposalTrace` output. A certified `JointQuality` result the
  policy refuses keeps `certified_quality`, uses `NoImprovement`, and has
  `accepted = false`.
- Stage-selection wiring: a stream-level test walks the stream to Pass 5 and
  observes `Acceptance::JointQuality` on the evaluation Pass 5 yields, and
  `Acceptance::Lexicographic` on the evaluations Passes 1-4 yield.
- Relocation mutation: the linear regression fixture above, asserted on cell
  kinds, the selected `N`, the per-candidate proof verdicts and the repeater
  count.
- Grouped route fixtures with a named assertion for every refusal case listed
  above; do not create one test function per row when one fixture proves several.
- Stream tests: stage index boundaries, prune-exhaustion fall-through,
  freeze-once against the post-Pass-4 incumbent, that the freeze replays accepted
  prunes before probing, stale-descriptor refusal, and that the Passes 1-4
  descriptor order is unchanged.
- No-probe tests: budget zero and Pass 1-4 evaluations run zero relocation
  probes, asserted by the absence of any frozen refresh descriptor list and any
  refresh-schema fingerprint in the trace.
- Rebuild-order test: accepted prunes replay before cumulative refreshes, and a
  prior accepted choice cannot become a no-op because of a later operation.
- Worker determinism: the real circuit retained by the gate runs through Pass 5
  at worker budgets 1, 2, and 4 with identical quality, fingerprints, stop
  reason, evaluation count, and complete trace. The existing one-evaluation
  matrix is only a broad control because it cannot reach Pass 5.
- Certified acceptance tests on real circuits stay `#[ignore]`d with a stated
  cost, matching the existing hierarchical acceptance tests.

## Measurement

### Hierarchical retention harness (test-only, ignored)

Extend the existing ignored four-circuit hierarchy harness
`run_hierarchical_cases` / `every_hierarchical_circuit_certifies_through_module_floorplan`
(`src/compile/fragment_synth/seed.rs:4805-4899`); do not add a competing runner.
It already covers `ripple_adder(8)`, `alu4_full()`, `multiplier4()` and `alu8()`
through `compile_hierarchical` and already owns `REDA_EXTRA_CIRCUITS` filtering.
For each circuit it runs at budget zero and at exhaustion -- or at exactly the
budget needed to reach Pass 5 from a test-only environment variable, so a
multi-hour exhaustion is not the only option. That variable is harness-only: no
production path reads it.

Each run prints, per circuit and per budget: all four `QualityKey` fields
(`observed_settle`, `non_air_blocks`, `occupied_volume`, `static_routed_delay`)
separately, `evaluations_used`, `stop_reason`, wall time, and the candidate
fingerprint.

The corpus harness prints every existing trace entry, but it does not guess a
stage from an opaque fingerprint. Exact Pass 5 attribution comes from a focused
ignored real-circuit test beside it: that test constructs the existing private
hierarchical stream, runs it through `run_budgeted_proposals`, then records the
exact stage ranges from the frozen descriptor vector lengths and checks the
resulting trace suffix. A separate stream-level unit test directly observes
`Acceptance::JointQuality` on the Pass 5 `ProposalEvaluation`; `accepted` itself
is checked only on `ProposalTrace`, where the controller writes it. This needs no
RouteId bound, fingerprint inversion, public API, or `ProposalTrace`
serialization change.

### Hierarchy-with-a-child pinned fixture

A small pinned hierarchical fixture reuses
`build_and4_netlist() -> (netlist, output_signal)`: the child owns the returned
gates and declares inputs `a,b,c,d` plus `output_signal`; a gate-free top declares
inputs `a,b,c,d`, output `y`, and binds each child input to the same-named parent
signal plus `output_signal -> y`, all with `PortBinding::Signal`. Input
`a` is pinned at `(21,1,62)` facing North (handover `(21,1,61)`, net cell
`(21,1,60)`), and output `y` at `(53,1,10)` facing North (handover `(53,1,11)`,
net cell `(53,1,12)`). It is compiled at budget zero and `u64::MAX` (stream
exhaustion, hence Pass 5 is reached even when it has no descriptor). It asserts
exact pin
coordinates and contract semantics on both runs, through what the public
`SynthesisResult` actually exposes -- `compiled.world` and the position maps, not
the candidate, which the result does not carry: the caller-owned pin cell is
exactly air, the handover cell holds exactly the expected handover repeater state,
the first net cell is a route conductor, and each pinned port's entry in
`compiled.input_positions` / `compiled.output_positions` is the caller's own pin
cell and is identical across the two budgets. Fingerprint semantics are asserted
precisely: the case fingerprint is identical across budgets, and the candidate
fingerprint is unchanged from budget zero if and only if no trace entry was
accepted.

### Baseline protocol

The baseline must be comparable to the post-feature run, which means the harness
itself cannot be part of the feature diff:

1. **Land only the harness first**, as its own commit: the retention harness and
   pinned fixture, with no policy, no stage and no
   `route_opt.rs` change.
2. At that pre-feature commit, run the harness on all four circuits and the pinned
   fixture and record the full transcript, the recorded commit id and the
   transcript file's hash in the report. Capture the flat baseline and run
   `fragment_acceptance` here too; preserve that exact baseline file/hash for the
   post-feature comparison. At this commit Pass 5 does not exist; that fact and
   the final trace index are part of the baseline.
3. Only then implement the acceptance policy and Pass 5, and re-run the same
   harness.

This preserves a comparable baseline because both sides run the same harness code,
the same circuits, the same budgets and the same printing, and the only difference
between the transcripts is the feature. Capturing the baseline after the feature
would measure the harness and the feature together; capturing it with an ad-hoc
script would leave nothing to re-run.

`fragment_baseline` is **not** the hierarchical baseline: it captures the legacy
flat benchmark corpus, which contains no hierarchical case and therefore cannot
show a Pass 5 effect. It is captured once only to feed `fragment_acceptance`,
which stays an **unchanged flat control**: it must produce the same verdict before
and after the feature, proving the flat path did not move. The capture command, on
this Windows worktree, uses a task-specific temporary path and the binary's
existing `--replace` flag rather than deleting anything by hand:

```powershell
$paretoBaseline = Join-Path $env:TEMP 'reda-refresh-relocation-baseline.json'
cargo run --release --bin fragment_baseline -- --output $paretoBaseline --replace
if ($LASTEXITCODE -ne 0) { throw 'flat control baseline capture failed' }
(Get-FileHash -LiteralPath $paretoBaseline -Algorithm SHA256).Hash
git rev-parse HEAD
```

Record the printed hash and the full commit id together. Historical numbers are
never reused as gates.

### Historical evidence (motivation only, not authoritative)

The 2026-09-07 retention report recorded certified wins for Pull-X, Seam
Absorption and pruning, and a feasibility screen that found removable repeaters
still present on `multiplier4` parent routes. That is why relocation is worth one
more attempt. Those figures were captured under a budget and revision provenance
this spec does not restate; no gate in this document may be evaluated against
them.

## Retention gates and revert-on-miss

Pass 5 stays in production only if all of these hold against the fresh baseline:

1. focused red/green tests cover its mutation and every refusal case;
2. traces remain deterministic prefixes and incumbent quality never worsens;
3. budget zero, the pinned fixture's contract, and the `fragment_acceptance` flat
   control are unchanged;
4. the focused real-circuit test directly observes at least one corpus circuit
   accept a `JointQuality` Pass 5 candidate with
   strictly lower `observed_settle` and unchanged `non_air_blocks` and
   `occupied_volume`. Pass 5 cannot change occupied cells, so no block or volume
   improvement is claimed; a static-delay-only result, and a lower-settle result
   that raises static routed delay, are reported and do not retain it;
5. that candidate passes unchanged full certification;
6. the win is not confined to a synthetic fixture.

Miss any gate and the revert is complete, in one commit: `relocate_refresh` and
its tests, the stage, descriptors, stream arithmetic, union wiring, retained full
parent-route map, Pass-5-only tests, and any acceptance-policy plumbing no
surviving stage uses. No dormant function, enum variant, descriptor type,
disabled knob, or commented-out stage is left behind. The characterization
harness may stay only if it still passes and reports no Pass 5 entry. A revert
must restore stream indices and trace output to the pre-pass bytes, proven by
rerunning the prefix-determinism and budget-zero tests.

## Portability

- No new dependency, no platform-conditional code, no threads, no filesystem or
  clock use inside a pass.
- Determinism must not depend on worker count, hash iteration order or wall-clock
  time; all descriptor and candidate ordering uses total orders over ids and
  anchors.
- The crate must continue to build for `wasm32` targets once the maintenance
  lane's `atomic_publish` gap is closed; no pass may add a host-only path. Until
  that gap is closed the wasm build is stated as unproven, not claimed.
- Native viewer tests and `wasm-pack build --target web` are non-blocking
  follow-up verification after the separate maintenance fix lands; they are not
  completion gates for this plan while the known wasm prerequisite is red.
- Cargo commands run serially in this Windows worktree; no pass work may start a
  second concurrent Cargo process.

## Maintenance lane

Pre-existing repository debt, reproduced at a fixed pre-branch baseline in the
2026-09-09 report and therefore not caused by this work. **It is not part of this
cycle's implementation plan.** It is sequenced as its own commits, never folded
into a pass commit, and appears in the plan only as a named prerequisite or
follow-up:

1. **wasm `atomic_publish`** (*prerequisite for the wasm portability claim only*)
   -- `src/compile/fragment_synth/benchmark.rs:1223` calls `atomic_publish`, which
   is defined only under `cfg(windows)` and `cfg(unix)`, so `wasm-pack test` and
   `wasm-pack build --target web` fail with E0425.
2. **Fixture regeneration** (*follow-up*) --
   `tests/fixtures/fragment_synth_baseline.json` carries a stale verifier-revision
   hash. Regenerate cleanly from a documented command on an otherwise clean tree,
   in its own commit. This plan's flat control uses a fresh temporary baseline,
   so it does not depend on or repair the checked-in fixture.
3. **Strict Clippy** (*follow-up*) -- `cargo clippy --all-targets -- -D warnings`
   is red on pre-existing library and test diagnostics. Fix in its own commit or
   commits, grouped by lint, touching no pass logic. Re-measure the diagnostic
   counts when the work starts rather than trusting the historical tally.
4. **Whole-repository `cargo fmt`** (*follow-up, deferred*) -- formatting drift is
   broad and a repo-wide reformat would bury every pass diff. Each pass commit
   keeps only its own touched files `rustfmt`-clean; the global pass is a separate,
   later, standalone commit.

## Milestone

**One milestone, this cycle: harness, acceptance policy, Pass 5.** Land the
retention harness and pinned fixture and capture the pre-feature baseline, then the
private `Acceptance` policy with its tests, then Refresh Relocation with its
mutation and refusal tests, then measure the corpus against that baseline and apply
the retention gates. This milestone targets ticks while requiring density to remain
unchanged; it does not claim a density win. Nothing else ships, and this spec closes
when the gates are decided either way.

Plan: `docs/superpowers/plans/2026-09-10-refresh-relocation-tick-pass.md`.
