# SDD ledger — plan: docs/superpowers/plans/2026-08-31-timing-directed-fragment-synthesis.md

Workspace: C:/Users/LTY/Desktop/REDA/.claude/worktrees/codex-timing-fragment-v2
Branch: codex/timing-directed-fragment-synthesis-v2
Starting HEAD: 80fd95c
Spec: docs/superpowers/specs/2026-08-31-timing-directed-fragment-synthesis.md

## Pre-flight self-consistency scan

| Task | Produces / consumes | Self-consistency finding |
| --- | --- | --- |
| 0 | Fingerprint, Ratio, PhysicalMetrics | `PhysicalMetrics` names planner-owned `Anchor`; plan must make the coordinate type durable before legacy deletion. |
| 1 | TransitionManifest, baseline evaluator, revision fields | Consumes library/simulator/verifier revision providers before the plan creates them; expected RED text also mentions a fixture despite using an in-memory test. |
| 2 | Stable IDs and pure topology expansion | Must also produce `ObservationId`; Task 3 consumes it before Task 6. Commit command omits modified `primitive_graph.rs`. |
| 3 | InstanceGraph, expanded candidate, legacy adapter | Uses typed observations and route trees; valid after Task 2 identity correction. |
| 4 | Lossless realiser/verifier adapter | Existing `Source`/`Net` are lossy; task needs a durable physical-verification view and must not create a second emitter. |
| 5 | Repeater direction preservation | Existing StrengthAware search is private and gate-index/string based; task must extract a typed router seam for Task 8. One cargo command has two test filters. |
| 6 | Typed observer and realised timing graph | Valid after ObservationId moves earlier; same-position aliases require one position to fan out to multiple identities. |
| 7 | Complete certification | Greater-than-eight-input proof is unspecified; current equivalence is structural and SAT is test-only. |
| 8 | Independent sparse seed | Global constraint incorrectly says independent seed exists after Task 7. Existing source-text guard is bypassable; typed router seam must be a compile-time dependency boundary. |
| 9 | Budget state machine | Proposal-internal routing/placement/proof caps are not defined or fingerprinted. |
| 10 | Single-instance transactions | Depends on fixed proposal limits and route-tree closure; otherwise prefix determinism is unprovable. |
| 11 | Duplication | One cargo command has two test filters; merge duplication remains explicitly excluded. |
| 12 | Replacement gate | Must record fixed shuffle seed/order and generate a production shipping config only on pass. |
| 13 | Front-door switch and deletion | Production must not parse `tests/fixtures`; shared router/verifier must live outside deletable policy code; `compile` has no provenance variable. |

## Shared file/interface scan

| Tasks | Shared file or interface | Finding / handoff rule |
| --- | --- | --- |
| 0 → 1,2,7,9,12 | Fingerprint and physical metrics | Task 0 owns canonical encoding and all revision-provider primitives needed by baseline capture. |
| 1 → 7,12 | TransitionManifest and evaluator | One implementation is reused; later tasks extend certification without rebuilding manifest order. |
| 2 → 3,4,6,8,11 | Typed identities and topology | ObservationId moves here; all later maps use these IDs without string projections. |
| 2 ↔ legacy primitive graph | merge isolation query | One pure query feeds both; legacy graph result remains unchanged. |
| 3 → 4,5,6,8,10,11 | ExpandedPhysicalCandidate and route ownership | Candidate owns route trees once and references per-sink bindings. |
| 4 → 6,7,8,10,12 | CertifiedWorld | No timing, seed result, or candidate promotion can bypass structural certification. |
| 5 → 6,8,10,13 | typed physical router | Extract to durable shared module; old planner and new synthesis use adapters; deletion retains it. |
| 6 → 7,10,12 | RealisedTimingGraph and typed observations | Complete rebuild after accepted candidate; rejected transactions cannot mutate it. |
| 7 → 8,9,10,11,12 | CertifiedCandidate and equivalence certificate | Compositional proof is production code and covers all input vectors by induction. |
| 8 → 9 | certified sparse seed | Budget zero returns this seed; no old generation policy is called. |
| 9 → 10,11,12 | ProposalTrace and limits | Every internal cap is fixed, config-owned, fingerprinted, and independent of requested budget. |
| 12 → 13 | acceptance report and shipping constant | Generate checked production constant plus parity test; never parse test fixture in production. |
| 13 ↔ planner deletion | shared physics extraction | Delete only policy/fallback; typed router, emitter, verifier rules, simulator, and topology remain. |

## Pre-flight rulings

- Ruling: Repair the implementation plan before Task 0 — the reviewed dependency graph is not executable as written — cost if wrong: one extra documentation commit and no production-code churn.
- Ruling: Task 0 will provide deterministic library, simulator, and verifier revision identifiers before baseline capture — the baseline spec requires all three — cost if wrong: revision constants may need later schema migration.
- Ruling: Define ObservationId with the stable identity family in Task 2; Task 6 implements observation behavior — identity must exist before candidate storage — cost if wrong: one public type moves earlier than originally planned.
- Ruling: Task 5 extracts a typed physical router seam into a durable shared module while preserving full BlockState direction; Task 8 consumes it — reusing private gate-index routing is impossible without this seam — cost if wrong: more refactoring before seed work.
- Ruling: Use a compositional instance-aware equivalence proof for >8 inputs, not the test-only SAT solver — validated primitive semantics plus complete typed sink assignments permit an all-input proof by DAG induction — cost if wrong: a SAT miter task must replace it before promotion.
- Ruling: SearchConfig owns fixed router, placement, backtracking, proof, and transaction caps, all included in SynthesisCaseFingerprint — budget prefix guarantees otherwise do not hold — cost if wrong: tuning requires explicit config changes and baseline refreshes.
- Ruling: Task 12 generates a production shipping constant/config and a parity test from the acceptance report; Task 13 reads only that constant — production must not depend on tests/fixtures — cost if wrong: one generated source artifact must be reviewed.
- Ruling: External high is strength > 0 throughout fixtures and certification — pinned IO contract requires this — cost if wrong: callers expecting strength 15 would need a separate contract.
- Ruling: The tool policy does not permit a model override without an explicit user-selected model, so subagents inherit the current model despite the SDD template requesting an explicit model — higher agent cost is accepted to follow tool policy — cost if wrong: less model-cost optimisation.

## Progress

Preflight plan repair: first implementation complete at d80a642; independent review requested fixes.

Review round 1 findings:

- Task 0 examples compared `Option<Anchor>` fields with bare `Anchor` values.
- Generated shipping search/certification configs were not proven to be consumed by the production front door.
- Task 13 still used globs and broad staging instead of an auditable exact file/deletion list.

The original implementer has been resumed for a scoped fix commit; fresh re-review is required afterward.

Review round 2 result:

- The three round-1 findings are fixed in 5709b7e.
- One remaining Important finding: Task 13's artifact regeneration wrote unlisted `output/...` temporary files and did not prove byte-stable binary regeneration.
- The implementer is resumed for an exact two-run staging manifest, extra-output rejection, and per-artifact byte/hash comparison.

Review round 3 result:

- Two-run artifact-set and hash checks are present, but cleanup could traverse a pre-existing junction outside the worktree.
- Destination after-copy hashes were asserted but not persisted as evidence.
- Scoped ruling: remove cleanup entirely; refuse a pre-existing staging root, verify Git top-level and all existing ancestors are non-reparse paths, then preserve deterministic hash evidence in an ignored non-staged file.

Review round 4 result:

- Staging-path safety and two-run artifact evidence passed review.
- Task 8 still referenced Task 9's future `compile_fragment_synth` API in its call-spy test.
- Evidence JSON readback did not verify the full ordered field/mapping/value contract.
- Final scoped fix loop: expose a Task-8-owned seed-with-services seam and fully validate the four evidence records on readback.

Review round 5 result:

- The Task 8 seam and Task 13 evidence contract passed.
- One Rust visibility mismatch remained: an external integration test was asked to construct crate-private sealed seed services.
- Fifth and final fix loop: keep service injection/call-spy checks in Task 8 unit tests; create the public-API-only architecture integration test in Task 9.

Breaker adjudication after fix round 5/5:

- Ruling: Task 8's restriction applies to spy-backed and test-only `SeedServices` injection only; Task 9 production code may construct the crate-private production services and call the seed seam. This preserves the sealed public boundary while allowing the production wrapper to exist. Cost if wrong: the Task 9 wrapper may need a small internal constructor extraction.
- Ruling: Task 9 Steps 1 and 3 contain only crate unit tests and their RED run. Create and first run `tests/fragment_synth_architecture.rs` only after Step 5 has implemented the public `compile_fragment_synth` API, then include it in the Task 9 PASS run and commit. Cost if wrong: the public architecture test is post-implementation rather than the initial RED test; the search state-machine unit tests remain the TDD driver.

Preflight plan repair: complete with two breaker rulings (commits 80fd95c..c0ca315; all other review findings clean).

Task 0 review round 0:

- Metrics, `Anchor`, cell-library traversal, dependency scope, and API containment passed.
- Important: simulator and physical-verifier revisions duplicate semantic truth in `compile::revisions` instead of being supplied by authority-owning modules.
- Important: fingerprint tests lack a known SHA-256 vector; revision tests do not independently cover every required semantic category.
- Ruling: Task 0's file allow-list expands only to the exact simulator/verifier authority modules needed to make behavior and revision consume one typed source of truth. Detached duplicate literals are forbidden. Cost if wrong: a few additional authority modules change in this commit and need focused regressions.
- Task 0: fix round 1/5 in progress.

Task 0: fix round 1/5 (3 addressed, 0 open; commit a038d00).
Task 0: minor (deferred): revision-seam-only simulator semantic types are public and may be reducible to `pub(crate)` if later tasks need no external API.
Task 0: complete (commits c0ca315..a038d00, review clean).

Task 1 review round 0:

- Critical: capture provenance does not enforce repository root/clean state and can record the wrong current commit.
- Critical: `BenchmarkCase` lacks required `transition_count`, including for uncertified new coverage.
- Critical: evaluator validates only requested outputs and can ignore extra/duplicate output identities.
- Important: output write has a TOCTOU overwrite race and is not atomic.
- Important: fresh capture can silently downgrade previously certified cases to new coverage.
- Important: netlist/pin/world hash tests and sparse-manifest order tests are not independently complete.
- Minor (deferred): segment_a plus seven_segment dominate measured block-transition work; later quality aggregation must remain per-case rather than one unnormalised score.
- Ruling: preserve the two-phase baseline boundary during fixes: first commit corrected evaluator/schema/tests, capture from that clean commit, then second commit regenerated fixture/integration evidence. No baseline value may be hand-edited. Cost if wrong: two additional fix commits and another expensive capture.
- Ruling: `baseline_commit` records the clean repository HEAD at each capture; the checked immutable fixture must name the corrected evaluator commit, while later fresh-process repeatability captures may name their then-current clean HEAD. Cost if wrong: provenance semantics may require a separate evaluator-source fingerprint in a later schema.
- Task 1: fix round 1/5 in progress.

Task 1: fix round 1/5 (7 addressed, 1 open — canonical hash tests do not independently pin multi-element ordering/YZX bytes; commits afe577d..2413908).
Task 1: fix round 2/5 in progress.

Task 1: fix round 2/5 (netlist/pin/literal world bytes addressed; 1 open — chosen world cells do not distinguish YZX from YXZ; commit 939c75c).
Task 1: fix round 3/5 in progress.

Task 1: fix round 3/5 (1 addressed, 0 open — canonical world bytes now distinguish YZX from YXZ; commit f2c2c9e).
Task 1: complete (commits a038d00..f2c2c9e, review clean).

Task 2 implementation dispatch:

- The first fresh implementer remained running without creating a RED test, a patch, or a blocker report; the worktree stayed clean at `f2c2c9e`.
- Ruling: terminate that no-output dispatch and replace it with a fresh implementer whose first bounded checkpoint is the focused topology RED test. Cost if wrong: agent context is discarded, but no repository work existed to preserve.
- Task 2: replacement implementation in progress.

Task 2 implementation result:

- The controller took over after two fresh implementers each remained `running` without a patch or checkpoint response; one parallel read-only agent separately reported model capacity exhaustion.
- Task 2 implementation committed as `9764399`; focused fragment tests, legacy primitive-graph tests, revision tests, and `primitive_graph_equivalence` pass (the Yosys case required the established outside-sandbox rerun).
- Two fresh independent reviewer dispatches also remained `running` without returning even a bounded checkpoint and were terminated. CodeRabbit CLI is not installed. Ruling: keep Task 2 review open and do not mark the task complete until an independent reviewer becomes available; controller self-review and passing tests are evidence, not a substitute for the required fresh review. Cost if wrong: Task 3 starts later, but no unreviewed interface is consumed.

Task 2 review infrastructure follow-up:

- A third fresh reviewer also returned no checkpoint within the bounded window and was terminated.
- Ruling revised for forward progress: retain the independent review as explicit debt, but allow Task 3 to start against commit `9764399`; do not merge or switch the production front door until the debt is discharged. Cost if wrong: a Task 2 interface correction may require rebasing uncommitted Task 3 work.

Task 3 graph phase:

- Exact graph-invariant tests first failed because the API did not exist, then passed 4/4 after implementing one-to-one typed instances and assignment validation.
- Candidate physical state, legacy adapter, candidate fingerprint, and Task 3 commit remain in progress.

Task 3 representation phase:

- Review of the actual route metadata rejected the planned universal-trunk plus disjoint-branch ownership model: legacy and planner routes can contain partially shared subpaths and currently retain only one flat physical cell arena plus ordered terminals.
- Ruling: one `RealisedRouteTree` owns conductor/floor states once; ordered branches retain typed sink and terminal metadata without owning those cells again. Whole-tree transaction closure remains unchanged. Cost if wrong: Task 5 may need to add optional ordered per-sink path references, but physical ownership and byte-exact emission stay canonical.
- Added typed boundary placements so unpinned levers/lamps and future pinned terminal bodies have explicit candidate state and fingerprint identity.
- Review-driven graph fixes reject altered merge contributors, duplicate input names, duplicate instance roles, and outer/expanded identity mismatches.
- Adapter RED then GREEN: NOT, bare merge, mixed merge, fanout, and and4 re-emit byte-identical worlds and identical compatibility positions/facings.
- Focused results: instance graph 6/6, candidate 4/4, full `compile_end_to_end` 14/14; fresh independent Task 3 review in progress.

Task 3 review round 0:

- Three critical gaps: incomplete candidates could emit, same-state multi-owner cells were accepted, and mixed-merge isolation repeaters had no primitive placement.
- Important gaps: routes lacked ordered per-sink paths, fingerprints omitted map identity and canonical set ordering, delayed boundary ownership was placement-wide, 64-input merge expansion overflowed, compatibility views copied legacy maps, and the direct two-node BUF representation test was absent.
- Fix round implemented all representation and adapter findings. `InstanceGraph` authority re-instantiation and full corruption certification remain explicitly in Task 4; pinned halo certification remains in the planned pin-aware certification task.
- Post-fix results: candidate 7/7, topology 7/7, exact legacy adapter 1/1, full `compile_end_to_end` 14/14, and full library 634 passed with 63 ignored and 0 failed. Two fresh post-fix reviewers are checking candidate invariants and legacy/path migration independently.

Task 3 review round 1:

- Two post-fix reviewers found no critical issue but reported declared-output closure, forged connection endpoints, exact delayed-owner IDs, graph ordering, stateful revalidation, observation identity, mixed-merge repeater orientation, pin geometry binding, and the 65-input boundary.
- The fix adds explicit direct declared-output routes, exact topology-derived connection sources/landings, name-to-typed pin contracts with caller-cell/handover/facing checks, canonical graph validation, and named 65-input rejection.
- Controller timing audit added a RED assertion that each branch charges only route-owned repeaters. It caught the mixed-merge isolation terminal being charged once as a route and once as a topology primitive; the adapter now transfers both ownership and delay charge.
- Current focused results: instance graph 7/7, candidate 8/8, topology 8/8, `compile_end_to_end` 14/14, delay reconciliation 1/1, and clippy has no new warnings. Third-round legacy/path review is clean; candidate-invariant review and final 700-test library run are in progress.

Task 3 review round 2:

- Candidate review demonstrated forged route timing metadata while world bytes stayed fixed. `validate_route_timing` now resolves every path coordinate through the exclusive physical-owner ledger, binds each root to its typed source observation, confirms the terminal state, and re-derives the exact Route-owned repeater charge.
- The same review found an `i32::MIN` subtraction panic. Source-gap and path-continuity arithmetic now use `i32::abs_diff` widened to `u64`; the extreme-coordinate regression returns a named error and the reviewer re-reported clean.
- Immutable topology re-instantiation, junction contributor certification, and observation-to-owner certification remain the exact pre-existing Task 4 corruption-matrix scope. They are recorded, not silently waived or partially duplicated in Task 3.
- Both final Task 3 review tracks are clean. Final current-source library regression passed: 637 passed, 63 ignored, 0 failed (700 total).

Task 3: complete (commit b2341ee, review clean).

Task 4 checkpoint:

- Durable typed emission, immutable topology/primitive re-instantiation, exact terminal ownership, strict route continuity/coupling and lossless legacy adaptation committed as `b1ddbe5`.
- Current-source full library regression before commit: 670 passed, 63 ignored, 0 failed. Focused candidate/verify/realise/verification/emission plus compile-end-to-end and OR-merge suites passed after formatting.
- Final junction review found one load-bearing P1: listed contributor metadata does not prove physical reach to the unique junction observation, and same-source unlisted routes can join the allowed route group.
- Task 4 fix round 1/5 opened against `b1ddbe5`; production checkpoint remains committed, but Task 4 is not complete until positive junction closure is tested, fixed and re-reviewed.
- Task 4 fix round 1/5: commit `be1f409`; reviewer found 0 Critical, 3 Important, 2 Minor. Open: same-route-ID spur alias, powered-support false reach, foreign-instance primitive bypass. Round 2 resumed the implementer with all three findings plus fail-open lookup and directional regressions.
- Task 4 minor (deferred to round 2): contributor lookup failures must fail closed rather than rely silently on structural-verifier call order.
- Task 4 minor (deferred to round 2): add direct route-alias, support-only, foreign-primitive, and repeater-output-face mutations.
- Task 4 fix round 2/5: commit `fa7cd89`; all 5 prior findings addressed. Scoped review found one new Important false positive: a one-cell outbound landing route sourced by the junction is classified as an incoming contributor before its source exemption. Round 3 resumed the implementer with this single finding.
- Task 4 fix round 3/5: commit `3a7ddaf`; outbound junction-source landing routes are exempted before incoming classification. Scoped review: I4 addressed, 0 new findings, approved.
- Task 4 polish: commit `f1bf7f3` removes the only new clippy warning; minimal scoped review approved. The unrelated pre-existing simulator `clone_on_copy` warning remains.
- Task 4 final verification: `cargo test --lib` 681 passed, 63 ignored, 0 failed (744 total, 535.22s); focused integrations remained green; `git diff --check` clean.
- Task 4: complete (commits `b1ddbe5..f1bf7f3`, review clean).

Task 5 preflight rulings:

- Ruling: Task 5 is an authority extraction, not permission to change the shipping route layout. `LegacyPlannerRouterAdapter` must preserve current anchors, exact states, terminal metadata and refusal category; extracting StrengthAware code does not switch the production front door before Task 13. Cost if wrong: the durable router may retain an internal legacy-parity scoring path longer than the final fragment implementation needs.
- Ruling: `route_step_is_legal(previous, at, next, state)` is the shared local cell-direction authority, especially for repeater rear/front traversal and exact state. World-dependent dust stair/support connectivity remains an additional emitted-world check; the local signature cannot truthfully replace it. Cost if wrong: Task 5 may need a second world-aware wrapper, but there will still be only one local repeater-axis rule.
- Initial Task 5 commit `ddef1ac` is `DONE_WITH_CONCERNS`, not complete. It establishes typed API/full-state route types, a bounded fragment router, moves branch realisation and StrengthAware expansion, and preserves focused parity, but `LegacyPlannerRouterAdapter` still invokes `planner::lay_net`; DistanceOnly/try_move/reservation/ring closure remain a second physics authority and legacy limits are sentinel-only.
- Ruling: although Task 5's file header omits `src/compile/verification.rs`, Step 5 explicitly requires the shared legality function in structural verification and Task 4 made that module the durable verifier authority. Task 5 may modify it with focused regression coverage. Cost if wrong: one extra file enters the Task 5 diff, but the required authority remains outside deletable planner policy.

Baseline verification at starting HEAD 80fd95c:

- `cargo test` library result: 578 passed, 0 failed, 63 ignored.
- Early integration suites through `delay_model_reconciliation`: all non-ignored tests passed.
- `tests/mc_dump.rs` failed only because the sandbox denied Yosys access to its temporary preopen directory; `cargo test --test mc_dump` passed 1/1 outside that sandbox.
- Remaining integration binaries from `or_merge` through `verilog_litematic`: all non-ignored tests passed outside the sandbox.
- Baseline conclusion: clean; the single initial failure was an execution-environment restriction, not repository behavior.

Task 5 final closure:

- Legacy `lay_net` and `try_move` now issue typed route requests; production DistanceOnly/StrengthAware search, exact branch realisation, work limits, terminal policy, ring closure and local path legality live in `compile::routing`.
- Frozen legacy references remain test-only. Complete rip-up-loop parity now includes the all-pinned full adder and caught/fixed an ownership-only socket preclaim that had been misread as exact dust.
- Strict routing preserves exact full repeater state, rejects illegal rear/front traversal while alternatives still exist, forbids terminal transit, and preserves an earlier branch's state on non-prefix re-entry.
- Final verification: library 695 passed / 0 failed / 63 ignored; delay-model reconciliation 15 passed / 0 failed / 1 ignored; compile_end_to_end 14/14; legacy adapter 4/4; routing 8/8; `git diff --check` clean. Clippy has only the pre-existing simulator `clone_on_copy` warning.
- External review availability: two earlier Claude review rounds were acted on; final Claude/Codex subagent quotas and CodeRabbit CLI were unavailable. The controller completed the final authority/parity review with the full evidence above.
- Task 5: complete. Task 6 may consume the typed physical router.

Task 6 final closure:

- Added stable typed timing nodes/arcs, exact route and primitive delay ownership, deterministic max-plus analysis, typed simulator observation, tied-worst transition summaries, and production observation maps.
- Reconciliation covers AND4 and full-adder physical candidates, including the full-adder `g21` predecessor selecting `g19`'s three-repeater route over `g20`'s zero-repeater route.
- Final verification: library 708 passed / 0 failed / 63 ignored; delay reconciliation 17 passed / 0 failed / 1 ignored; compile end-to-end 14/14; timing graph 7/7; timing module 13/13; observer 5/5; timing integration 3/3; `cargo check --all-targets` passed; `git diff --check` clean.
- External reviewer capacity remained unavailable. Controller review found and fixed output-boundary conflation, repeated route-coordinate charging, and unstable test lookup.
- Task 6: complete. Task 7 may seal timing and structural artifacts behind candidate certification.

Task 7 final closure:

- Added immutable transition manifests, fixed fingerprinted search/certification caps, compositional all-input equivalence, fresh-simulator transition sweeps, complete metrics, and the sealed `CertifiedCandidate` promotion boundary.
- Controller review caught and fixed wrong-topology semantic certification, post-hoc event limiting, split source/destination event budgets, fingerprint-as-improvement ambiguity, and a symbolic identity panic.
- Final verification: library 724 passed / 0 failed / 63 ignored; manifest 4/4; config 3/3; certification 6/6; equivalence 9/9; compile end-to-end 14/14; reference circuits 10/10; seven-segment 3/3; simulator 19/19; all targets check passed; diff check clean.
- Clippy has only the two pre-existing unrelated warnings. External reviewer capacity remained unavailable, so the final pass was a controller review.
- Task 7: complete. Task 8 may build the independent topology-aware sparse seed and must not call legacy generation.
