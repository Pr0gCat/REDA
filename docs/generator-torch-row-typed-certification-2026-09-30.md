# Typed physical certification of the torch matrix

Status: the fixed nonconstant [1,6,3,5] ROM now passes the existing structural
and full ExpandedStrict physical certification pipeline. This is a test-only
integration step, not production adoption or certification of all data tables.

The earlier normalized NOR/OR graph had honest observation sites but no typed
route ownership. The adapter now records actual primitives, distributed OR
junctions, shared conductor trees, branch landings, repeater delays, boundary
ports and pin contracts. Inputs move one cell outward, leaving explicit dust
between each input and its inverter support. Owned emission and durable
certified emission reproduce every cell of this adapted prototype exactly.
Rebuilding the candidate must preserve its fingerprint.

A distributed dust OR cannot be represented honestly as a single-cell junction.
ExpandedStrict now cuts all explicitly declared junction wire cells when
checking cross-route connectivity, and independently requires the declared
region to be connected through its wires and listed contributor landings.
Reverse contributor discovery may continue past an interior landing only into
cells owned by that same junction. It never follows the incoming route past
that landing. The Connectivity rule revision increases from 1 to 2; legacy
verification retains its existing revision.

The negative fixtures reject a missing contributor, a forged disconnected
junction wire and a reversed route repeater. Existing verifier tests retain
coverage for foreign-route shorts, unlisted primitive contributors, powered
support false positives, repeater direction and strict coupling.

## Independent quality acceptance

The certified world's persistent-history 16 measured transitions, each with a
setup transition, retain the earlier frozen prototype's nine quality caps:

| Metric | Certified candidate |
| --- | ---: |
| Worst settling | 14 ticks |
| Total settling | 162 ticks |
| Latest driver change | 10 ticks |
| Driver glitches / single-input glitches | 0 / 0 |
| Lamp glitches / single-input glitches | 0 / 0 |
| Worst / total driver pulse duration | 0 / 0 ticks |

Actual driver and lamp truth use the independent literal data words.
The recorded route count checks coverage only and does not rank quality.
Detailed traces and fingerprint are in the accompanying JSON.

## Scope and remaining integration

Only this fixed five-row fixture is qualified by full typed certification here.
The earlier 2744-table logical mapping and 4096-table physical simulation
results do not imply exhaustive typed certification. Constant outputs,
longer-route ownership, frontend rewrite provenance and public compile-path
candidate selection remain separate work. The default compiler is unchanged.
No Minecraft runtime claim is made.

Validation:

- `cargo test --release --lib compile::verification::tests`: 15 tests.
- `cargo test --release --lib compile::fragment_synth::api::tests`: 14 tests;
  the three explicit case-identity snapshots are deliberately revised for
  Connectivity v2. Their old values remain available in git history.
- `cargo test --release --test torch_row_decoder typed_matrix -- --nocapture`:
  two tests covering positive certification, deterministic reconstruction,
  independent quality caps and three rejected metadata/physical mutations.
- `cargo test --release --test frontend_quality_report --test generator_quality_matrix`:
  both quality suites, 1312 measured transitions across native and Yosys
  frontends, retaining all existing truth, timing, pulse and determinism checks.
- `git diff --check`.

The existing packing.rs warnings remain. These targeted checks do not assert
that the entire repository's test suite is green.
