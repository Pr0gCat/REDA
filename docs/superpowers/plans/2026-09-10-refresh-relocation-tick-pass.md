# Refresh Relocation Tick Pass Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to execute this plan task-by-task, and superpowers:test-driven-development for Tasks 2-4.

**Goal:** Add one deterministic hierarchical Refresh Relocation pass that can lower certified `observed_settle` without worsening blocks, volume, or static routed delay.

**Architecture:** Keep the existing finite `HierarchicalProposalStream`. Add a per-evaluation acceptance policy in `search.rs`; Passes 1-4 remain lexicographic and Pass 5 alone uses joint-quality acceptance. Implement the route mutation beside `prune_route`, replay it inside `union_candidate` before child stamping, and freeze descriptors only when Pass 5 is first reached. The existing whole-world certifier remains authoritative.

**Tech Stack:** Rust standard library, existing REDA hierarchy/route/timing types, existing certification path, serial Cargo on PowerShell.

---

## Fixed constraints

- Base source anchors: `37e92660fe8247d2fb74511883a70ef3501d8310`.
- No density algorithm, Pass 6, new framework, dependency, public API, config knob, or trace field.
- Passes 1-4 keep descriptor order, fingerprints, policy, and trace bytes.
- Budget zero and Passes 1-4 do not probe `relocate_refresh`.
- Every candidate still passes `certify_planned`; local strength checks only reject work early.
- Run one Cargo command at a time. Format only touched Rust files with `rustfmt`, never repository-wide `cargo fmt`.
- Maintenance (`atomic_publish` wasm gap, stale checked-in flat fixture, existing
  Clippy/fmt debt) is outside this plan. Do not fix or claim those follow-ups in
  a pass commit; this plan's flat control uses its own fresh temporary baseline.

## Source map

- `src/compile/fragment_synth/search.rs:102-108,207-246,482-514` — evaluation policy and incumbent replacement.
- `src/compile/fragment_synth/route_opt.rs:25-163,165-350` — existing prune mutation, strength proof, slack ordering, focused fixtures.
- `src/compile/fragment_synth/union.rs:408-525,1954-1981` — full parent-route map, prune replay point, reusable `seam_tree` fixture.
- `src/compile/fragment_synth/hierarchy_api.rs:102-270,290-483,593-605,809-1043,2547-2820` — front door, candidate state, finite stream, stage tests, real-circuit tests.
- `src/compile/fragment_synth/seed.rs:4805-4899,5131-5170` — existing four-circuit hierarchical report and worker-count matrix; extend rather than duplicate them.
- `tests/build_circuit_pins.rs:495-565` — existing single-module pinned control; add a true child-module fixture beside it.
- `src/circuits/hierarchical_builder.rs:161-196,256-327,357-460,536-662` — four acceptance circuits.
- `docs/superpowers/reports/2026-09-10-refresh-relocation-retention.md` — fresh baseline and post-feature evidence.

## Task 1: Land the comparable characterization harness first

**Files:**

- Modify: `src/compile/fragment_synth/seed.rs:4805-4899`.
- Modify: `tests/build_circuit_pins.rs` near `:510`.
- Create: `docs/superpowers/reports/2026-09-10-refresh-relocation-retention.md`.

- [ ] Extend the existing ignored `every_hierarchical_circuit_certifies_through_module_floorplan` harness and its `run_hierarchical_cases` helper; do not add a second four-circuit runner. Keep its `hierarchical_circuit_cases()` order and `REDA_EXTRA_CIRCUITS` filtering. For budget zero and `REDA_RETENTION_BUDGET` (default `u64::MAX`), print one stable record containing all four `QualityKey` fields, `evaluations_used`, `stop_reason`, elapsed milliseconds, case fingerprint, candidate fingerprint, and every existing trace entry.

Change the helper signature to:

```rust
fn run_hierarchical_cases(
    cases: Vec<(String, HierarchicalNetlist)>,
    points: &[(&str, SynthesisBudget)],
)
```

Use one output function so baseline and post-feature formatting cannot diverge:

```rust
fn print_retention_record(name: &str, budget: u64, result: &SynthesisResult, elapsed_ms: u128) {
    let q = result.metrics.quality;
    println!(
        "RETENTION name={name} budget={budget} settle={} blocks={} volume={} static={} evals={} stop={:?} wall_ms={elapsed_ms} case={} candidate={}",
        q.observed_settle,
        q.non_air_blocks,
        q.occupied_volume,
        q.static_routed_delay.0,
        result.evaluations_used,
        result.stop_reason,
        result.case_fingerprint.as_str(),
        result.candidate_fingerprint.as_str(),
    );
    for entry in &result.trace {
        println!("TRACE name={name} entry={entry:?}");
    }
}
```

- [ ] Add a two-level pinned and4 fixture from
  `build_and4_netlist() -> (netlist, output_signal)`. The child owns
  `netlist.gates`, declares inputs `a,b,c,d`, and declares `output_signal` as its
  output. The gate-free top declares inputs `a,b,c,d`, output `y`, and has one
  `ModuleInstance` binding the four child inputs to the same-named parent signals
  plus `output_signal -> y`, all with `PortBinding::Signal`. Pin
  input `a` at `(21,1,62)` facing North (handover `(21,1,61)`, net
  `(21,1,60)`) and output `y` at `(53,1,10)` facing North (handover
  `(53,1,11)`, net `(53,1,12)`). Compile at budgets `0` and `u64::MAX`; assert
  exact requested position maps, air at each caller-owned pin, exact South-facing
  handover repeater state, route conductor at each net cell, equal case
  fingerprints, and candidate equality iff no trace entry was accepted.

- [ ] Run the characterization checks. These are expected **GREEN before the feature**; they are not fake RED tests.

```powershell
cargo test --lib every_hierarchical_circuit_certifies_through_module_floorplan -- --ignored --nocapture
cargo test --test build_circuit_pins hierarchy_with_a_child_preserves_requested_pins_through_exhaustion -- --exact --nocapture
```

- [ ] Commit only the harness and pinned fixture, so the baseline has an exact,
  clean pre-feature revision.

```powershell
git add -- src/compile/fragment_synth/seed.rs tests/build_circuit_pins.rs
git diff --cached --check
git commit -m "test: add hierarchical tick-pass characterization"
```

- [ ] From that clean commit, capture the pre-feature transcript and provenance
  before touching Tasks 2-4.

```powershell
$retentionBaseline = Join-Path $env:TEMP 'reda-refresh-relocation-pre-feature.txt'
$env:REDA_EXTRA_CIRCUITS = 'ripple_adder8,alu4_full,multiplier4,alu8'
$env:REDA_RETENTION_BUDGET = [string][uint64]::MaxValue
cargo test --release --lib every_hierarchical_circuit_certifies_through_module_floorplan -- --ignored --nocapture 2>&1 | Tee-Object -LiteralPath $retentionBaseline
if ($LASTEXITCODE -ne 0) { throw 'hierarchical baseline failed' }
git rev-parse HEAD
(Get-FileHash -LiteralPath $retentionBaseline -Algorithm SHA256).Hash
Remove-Item Env:REDA_RETENTION_BUDGET,Env:REDA_EXTRA_CIRCUITS -ErrorAction SilentlyContinue
```

- [ ] Still on the clean pre-feature commit, capture the flat baseline once and
  record its hash and acceptance verdict. Keep this exact baseline file for Task
  5; do not regenerate it after the feature.

```powershell
$paretoBaseline = Join-Path $env:TEMP 'reda-refresh-relocation-flat-control-baseline.json'
$paretoBeforeDir = Join-Path $env:TEMP ("reda-refresh-relocation-before-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $paretoBeforeDir | Out-Null
cargo run --release --bin fragment_baseline -- --output $paretoBaseline --replace
if ($LASTEXITCODE -ne 0) { throw 'flat baseline capture failed' }
cargo run --release --bin fragment_acceptance -- `
    --baseline $paretoBaseline `
    --output (Join-Path $paretoBeforeDir 'acceptance.json') `
    --shipping-source (Join-Path $paretoBeforeDir 'shipping_config.rs') `
    --shuffle-seed 0x5245444120260831
if ($LASTEXITCODE -ne 0) { throw 'pre-feature flat control failed' }
(Get-FileHash -LiteralPath $paretoBaseline -Algorithm SHA256).Hash
(Get-FileHash -LiteralPath (Join-Path $paretoBeforeDir 'acceptance.json') -Algorithm SHA256).Hash
```

- [ ] Record command, the committed harness revision, both baseline hashes,
  the flat-control verdict, and per-case records in the report. Commit only that
  report.

```powershell
git add -- docs/superpowers/reports/2026-09-10-refresh-relocation-retention.md
git diff --cached --check
git commit -m "docs: record hierarchical tick-pass baseline"
```

## Task 2: Add the per-evaluation acceptance policy

**Files:**

- Modify: `src/compile/fragment_synth/search.rs:102-108,207-246,482-514`.
- Modify: `src/compile/fragment_synth/fragment.rs:169-211`.
- Modify: `src/compile/fragment_synth/hierarchy_api.rs:930-1043` only to initialize existing stages explicitly.

- [ ] Write failing unit tests in `search.rs`:

  - `JointQuality` rejects lower settle with higher static delay.
  - It rejects lower settle with more blocks or volume.
  - It rejects static-delay-only and all-equal candidates.
  - It accepts lower settle only when blocks, volume, and static delay are all
    no worse.
  - `Lexicographic` preserves the current lower-settle/higher-static acceptance.
  - A policy-rejected certified proposal retains `certified_quality`, records `NoImprovement`, and does not replace the incumbent.

- [ ] Run RED:

```powershell
cargo test --lib compile::fragment_synth::search::tests::joint_quality -- --nocapture
```

Expected: compilation fails because `Acceptance` and the evaluation field do not exist.

- [ ] Add the minimum policy; do not implement `Default`.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Acceptance {
    Lexicographic,
    JointQuality,
}

fn accepts(policy: Acceptance, candidate: QualityKey, incumbent: QualityKey) -> bool {
    match policy {
        Acceptance::Lexicographic => candidate < incumbent,
        Acceptance::JointQuality => {
            candidate.observed_settle < incumbent.observed_settle
                && candidate.non_air_blocks <= incumbent.non_air_blocks
                && candidate.occupied_volume <= incumbent.occupied_volume
                && candidate.static_routed_delay <= incumbent.static_routed_delay
        }
    }
}
```

Add `pub acceptance: Acceptance` to `ProposalEvaluation` (matching its sibling
fields so sibling modules can construct it), set every current constructor in
`search.rs`, `fragment.rs`, and `hierarchy_api.rs` explicitly to `Lexicographic`,
and replace only `search.rs:235-238` with
`accepts(evaluation.acceptance, candidate.quality(), best.quality())`.

- [ ] Run GREEN and the unchanged flat-search tests:

```powershell
cargo test --lib compile::fragment_synth::search -- --nocapture
cargo test --lib compile::fragment_synth::fragment -- --nocapture
```

- [ ] Commit.

```powershell
git add -- src/compile/fragment_synth/search.rs src/compile/fragment_synth/fragment.rs src/compile/fragment_synth/hierarchy_api.rs
git diff --cached --check
git commit -m "refactor: make proposal acceptance explicit"
```

## Task 3: Implement and prove `relocate_refresh`

**Files:**

- Modify: `src/compile/fragment_synth/route_opt.rs:25-163,165-350`.

- [ ] Build `linear_relocation_route()` in the existing test module from `union::tests::seam_tree(0, 26)`: retain the `z=0` branch/cells, then set `x=9` and `x=18` with `crate::compile::repeater(Facing::East)`. The existing helper stores the correct opposite-facing block state; do not assign `BlockState::facing` directly.

- [ ] Write RED tests asserting:

  - direct pruning removes neither `U=9` nor `D=18`;
  - on manually mutated clones, existing `branches_carry_through` rejects
    `N=17` and `N=16`, but accepts `N=13`;
  - downstream-first selection stops at `N=15`;
  - repeater anchors change from `{0,9,18,27}` to `{0,15,27}`;
  - one grouped refusal table covers unequal branch membership, bend/vertical segment, empty window, direct-prunable `D`, missing/non-conductor cell, and no strength-feasible `N`, with the tree byte-equal to its input after refusal.

- [ ] Run RED:

```powershell
cargo test --lib compile::fragment_synth::route_opt::tests::refresh_relocation -- --nocapture
```

Expected: unresolved `relocate_refresh`; do not add a production trial helper.

- [ ] Implement only this interface:

```rust
pub(crate) fn relocate_refresh(tree: &mut RealisedRouteTree) -> bool;
```

Reuse the existing cell-kind strength walk. Enumerate `(U,D)` and `N` with sorted vectors: downstream path index descending, then `Anchor`; never depend on map iteration. For each trial, clone once, set `U` and `D` to dust, copy `U`'s repeater state to `N` with successor-facing direction, prove incoming `P -> N` and outgoing `N -> every terminal`, then replace the original only if the repeater count fell by exactly one. Later retained steps operate on the updated tree.

- [ ] Run GREEN plus prune regressions:

```powershell
cargo test --lib compile::fragment_synth::route_opt -- --nocapture
```

- [ ] Commit.

```powershell
git add -- src/compile/fragment_synth/route_opt.rs
git diff --cached --check
git commit -m "feat: relocate redundant parent-route refreshes"
```

## Task 4: Integrate Pass 5 lazily and wire its policy

**Files:**

- Modify: `src/compile/fragment_synth/union.rs:408-525`.
- Modify: `src/compile/fragment_synth/hierarchy_api.rs:174-270,290-483,593-605,809-1043,2547-2695`.

- [ ] Write RED tests before plumbing:

  - `union_candidate` replays all prunes, then all refreshes, before `union.rs:525` child stamping; stale/no-op refresh is refused.
  - a stream-level test freezes Pull-X, prune, then refresh vectors once and proves exact stage ranges from their lengths;
  - every Passes 1-4 evaluation carries `Lexicographic`; the first Pass 5 evaluation carries `JointQuality`;
  - budget zero and a budget ending at the last Pass 4 index leave `refreshes: None`, proving no relocation probe;
  - freeze uses the post-Pass-4 incumbent and replays its accepted prunes before probing.

- [ ] Run RED:

```powershell
cargo test --lib compile::fragment_synth::hierarchy_api::tests::refresh_stage -- --nocapture
cargo test --lib compile::fragment_synth::union::tests::refresh_relocation -- --nocapture
```

Expected: missing refresh fields/stage and wrong policy on the new stage.

- [ ] Extend the existing `UnionInput<'a>` with exactly one field,
  `pub refreshes: &'a [ParentRouteChoice]`; keep every current field unchanged.

Immediately after the existing prune loop, replay `refreshes` by route id with `relocate_refresh`; refuse a missing route or no-op. Do not touch stamping, normalization, or certification.

- [ ] Preserve both route maps without new probing in `union_and_certify`:

```rust
let (union, mut parent_routes) = union_candidate(input)?;
let all_parent_routes = parent_routes.clone();
parent_routes.retain(|route, _| prunable.contains(route));
```

Add `all_parent_routes: BTreeMap<RouteId, RouteId>` and cumulative `refreshes: Vec<ParentRouteChoice>` to `HierarchicalCandidate`. Thread `refreshes` through `HierarchicalCompiler`, `compile_module_with_blocks`, `compile_proposal`, and `union_and_certify`. Do not change how `parent_routes` is produced or ordered.

- [ ] Freeze descriptors only at first Pass 5 reach. Reuse `ParentRouteChoice` and `prune_descriptors`: clone each planned parent tree, replay accepted prunes naming that route, keep the route only when `relocate_refresh` changes the clone, then call `prune_descriptors(timing, &filtered_full_map)` for slack/id order.

The stream field and boundary helper are:

```rust
refreshes: Option<Vec<ParentRouteChoice>>,

fn refresh(
    &mut self,
    index: usize,
    freeze: impl FnOnce() -> Vec<ParentRouteChoice>,
) -> Option<ParentRouteChoice>;
```

`refresh()` subtracts `edges.len() + pull_x_edges.len() + seams.len() + prunes.len()`. The prune vector must already be frozen before its length is used.

- [ ] Extend the stage-selection tuple from three elements to
  `(fragment_fingerprint, choice_fingerprint, proposal, acceptance)` without
  rewriting the existing stage-arm bodies.

Passes 1-4 return `Acceptance::Lexicographic`; Pass 5 returns `Acceptance::JointQuality`. The three shared `ProposalEvaluation` constructors use only the tuple variable. Pass 5 schemas are exactly `hierarchical-parent-refresh-fragment-v1` and `hierarchical-parent-refresh-choice-v1`; do not modify prune schemas.

- [ ] Run GREEN and prove current stages did not move:

```powershell
cargo test --lib compile::fragment_synth::union -- --nocapture
cargo test --lib compile::fragment_synth::hierarchy_api -- --nocapture
cargo test --lib compile::fragment_synth::search -- --nocapture
```

- [ ] Commit.

```powershell
git add -- src/compile/fragment_synth/union.rs src/compile/fragment_synth/hierarchy_api.rs
git diff --cached --check
git commit -m "feat: add lazy hierarchical refresh relocation"
```

## Task 5: Prove real benefit, pins, determinism, and retention

**Files:**

- Modify: `src/compile/fragment_synth/hierarchy_api.rs` test module near `:2702`.
- Modify: `docs/superpowers/reports/2026-09-10-refresh-relocation-retention.md`.

- [ ] Add one ignored real-circuit test that constructs the same private stream
  used by `compile_hierarchical`, runs it to exhaustion through
  `run_budgeted_proposals`, then reads exact stage boundaries from `edges`,
  frozen `pull_x_edges`, `seams`, `prunes`, and `refreshes`. Assert every
  accepted trace entry at or after the Pass 5 start has unchanged blocks/volume,
  lower settle, non-increased static delay, and full certification. At least one
  of the four acceptance circuits must have such an accepted entry; otherwise
  the retention gate fails.

- [ ] Run the focused real-circuit test, then commit that test so later
  provenance-sensitive baseline/control tools see a clean repository.

```powershell
cargo test --release --lib compile::fragment_synth::hierarchy_api::tests::refresh_relocation_improves_an_acceptance_circuit -- --ignored --exact --nocapture
git add -- src/compile/fragment_synth/hierarchy_api.rs
git diff --cached --check
git commit -m "test: attribute refresh relocation on real circuits"
```

- [ ] Rerun the unchanged Task 1 harness with the same filters/budgets and
  capture a post-feature transcript from that clean commit.

```powershell
$retentionAfter = Join-Path $env:TEMP 'reda-refresh-relocation-post-feature.txt'
$env:REDA_EXTRA_CIRCUITS = 'ripple_adder8,alu4_full,multiplier4,alu8'
$env:REDA_RETENTION_BUDGET = [string][uint64]::MaxValue
cargo test --release --lib every_hierarchical_circuit_certifies_through_module_floorplan -- --ignored --nocapture 2>&1 | Tee-Object -LiteralPath $retentionAfter
if ($LASTEXITCODE -ne 0) { throw 'hierarchical retention run failed' }
(Get-FileHash -LiteralPath $retentionAfter -Algorithm SHA256).Hash
Remove-Item Env:REDA_RETENTION_BUDGET,Env:REDA_EXTRA_CIRCUITS -ErrorAction SilentlyContinue
```

- [ ] Add and run an ignored Pass-5 worker-determinism test in
  `hierarchy_api.rs`. On the real circuit that won the retention gate, call
  `compile_hierarchical_with_threads` at `u64::MAX` with worker budgets 1, 2,
  and 4; compare candidate fingerprint, all four quality fields, stop reason,
  evaluations, and complete trace. The existing seed matrix stays as a broad
  one-evaluation control but is not Pass 5 evidence.

```powershell
cargo test --test build_circuit_pins -- --nocapture
cargo test --release --lib compile::fragment_synth::hierarchy_api::tests::refresh_relocation_is_worker_deterministic -- --ignored --exact --nocapture --test-threads=1
```

- [ ] Run the unchanged flat control against the exact pre-feature baseline from
  Task 1. Verify its SHA-256 still matches the report before running; never
  recapture it at the feature commit.

```powershell
$paretoBaseline = Join-Path $env:TEMP 'reda-refresh-relocation-flat-control-baseline.json'
if (-not (Test-Path -LiteralPath $paretoBaseline)) { throw 'pre-feature flat baseline is missing' }
(Get-FileHash -LiteralPath $paretoBaseline -Algorithm SHA256).Hash
$paretoAfterDir = Join-Path $env:TEMP ("reda-refresh-relocation-after-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $paretoAfterDir | Out-Null
cargo run --release --bin fragment_acceptance -- `
    --baseline $paretoBaseline `
    --output (Join-Path $paretoAfterDir 'acceptance.json') `
    --shipping-source (Join-Path $paretoAfterDir 'shipping_config.rs') `
    --shuffle-seed 0x5245444120260831
if ($LASTEXITCODE -ne 0) { throw 'post-feature flat control failed' }
(Get-FileHash -LiteralPath (Join-Path $paretoAfterDir 'acceptance.json') -Algorithm SHA256).Hash
```

- [ ] Update the report with both commit ids, transcript hashes, full four-field deltas, accepted Pass 5 indices, stop reasons, fingerprints, wall times, pin result, worker matrix, certification result, and flat-control result.

- [ ] Apply the retention decision. Keep Pass 5 only if at least one non-synthetic acceptance circuit has a fully certified lower-settle Pass 5 acceptance with blocks/volume unchanged and static delay non-increased. On a miss, remove Task 3's production mutation/tests, Task 4's stage/plumbing, every now-unused Task 2 policy change, and Pass-5-only tests in one revert commit; leave no dormant function, stage, enum variant, descriptor, or knob.

- [ ] Commit the report only after the decision is made.

```powershell
git add -- docs/superpowers/reports/2026-09-10-refresh-relocation-retention.md
git diff --cached --check
git commit -m "test: certify refresh relocation retention"
```

## Task 6: Final review and verification

- [ ] Run focused formatting checks on only touched Rust files, then serial verification:

```powershell
rustfmt --check --edition 2021 src/compile/fragment_synth/search.rs src/compile/fragment_synth/fragment.rs src/compile/fragment_synth/route_opt.rs src/compile/fragment_synth/union.rs src/compile/fragment_synth/hierarchy_api.rs src/compile/fragment_synth/seed.rs tests/build_circuit_pins.rs
cargo test --lib compile::fragment_synth::search
cargo test --lib compile::fragment_synth::route_opt
cargo test --lib compile::fragment_synth::union
cargo test --lib compile::fragment_synth::hierarchy_api
cargo test --test build_circuit_pins
git diff --check
```

- [ ] Request two independent reviews: one correctness/determinism review and one Ponytail review for unnecessary types, probes, clones, or public surface. Fix only concrete findings, rerun the smallest affected command, then rerun `git diff --check`.

- [ ] Confirm the final diff contains no density claim, Pass 6 implementation, maintenance fix, dependency, public API, trace schema change, or second pass framework. Confirm `git status --short` contains only intended files.
