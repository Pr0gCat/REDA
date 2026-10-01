# Reject sideways refresh while preserving forward climbs

Independent production reference f7bcf88. Ordinary legacy branch realization
forced a repeater into the cell before any stair whenever that cell was absent
from the bend set. The bend set omitted the source boundary by default. Thus
an immediate sideways climb could insert an east-facing delivery repeater
before a south/up continuation and still report that the branch carries signal.
The isolated physical fragment never powers its output lamp when input is true.
A separate circuit can hide this through another wire branch; a successful final
truth table does not validate that particular claimed refresh.

The first candidate switched ordinary routing to LatestLegalCell everywhere.
It removed every strict-flat-invalid stair refresh, and all frontend controls
passed, but XOR mean settling rose from 6.375 to 6.5 ticks. Driver/lamp glitches
became 2/1 versus checkpoint 1/0. It was rejected and withdrawn; its complete
patch and XOR traces are retained in the companion experiment artifacts.

That experiment revealed an important distinction. The old XOR repeater at
(23,1,36) drives forward into the solid floor beneath wire (23,2,35), so it
provides real delayed power despite the strict flat-path predicate returning
false. Removing it changes a downstream landing from tick 4 to tick 2. All
recorded paths are identical between that flat-only candidate and the control;
this is their only removed relay, with no added or changed relays. The strict
predicate is appropriate for its flat contract, not proof that every ascending
legacy step is physically ineffective. The diagnostic label now explicitly says
strict_flat_path_legal; prior frozen artifacts remain unchanged.

The accepted rule keeps the established reserve policy and timing choices.
It includes the source boundary in refresh eligibility: a first cell must have
flat entry and a forward flat/up-one continuation. A forced pre-stair refresh
must have flat entry, ascend exactly one cell, and continue in the same
horizontal direction. Forward climbs keep their existing solid-floor delivery;
sideways and descending transitions cannot force an ineffective repeater.
The rule uses coordinates and direction, never netlist/module/case names.
Gate/block counts and occupied volume are not acceptance criteria.

## Independent physical regression

The new test constructs two branch paths via the actual branch realizer, lays
its recorded blocks and floors, and drives an independent input lever/output
lamp through every one-input setup/target pair, retaining simulator history.
Both the forward ascent and sideways ascent have four target plus four setup
phases (16 complete phases total). Each branch must both claim carry and
physically deliver the independent Boolean input at its lamp.

Replaying the same new test with production routing.rs from f7bcf88 fails at
sideways path [(2,1,1),(2,2,2),(2,2,3)], false->true: actual lamp false, expected
true. The forward case passes first. The accepted implementation passes the
whole test. This is a real output-truth improvement rather than a size saving
or a test that just checks the emitted component kind. The replay's original
source and all existing test assertions were retained; only this additive test
was inserted in the control checkout during the temporary replay.

## Qualification

- Both frontend/generator suites preserve every fixed quality cap across 1312
  target transitions, with repeated compilation determinism and physical truth.
- All 68 routing unit tests pass, including the new independent physical case.
- Observed-output isolation passes its complete history variants.
- Native/fresh-Yosys physical DFF and all four constant-driver tests pass.
- All five inverter-bypass qualification tests retain independent frozen caps.
- git diff --check passes. The first rejected patch remains archived.

The companion JSON retains fixed-suite reports, control/flat-only XOR histories,
and the actual failed original physical-test log. No independent cap was
regenerated, old test relaxed, simulator rule changed, history reset, or input
hold extended. This does not claim universal timing improvement, full repository
success, or Minecraft certification. General backfeed and arrival balancing
still need work; automatic torch-row selection is disabled and the optimization
goal remains active.

Reproduce after source tools/env.sh, with RUST_MIN_STACK=33554432:

    cargo test --release --lib compile::routing::tests
    cargo test --release --test frontend_quality_report --test generator_quality_matrix
    cargo test --release --test observed_output_isolation --test dff_physical --test constant_drivers --test inverter_bypass_quality
