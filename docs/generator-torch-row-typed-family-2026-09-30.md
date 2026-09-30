# Strict certification across the nonconstant torch-matrix family

Status: all 2744 nonconstant two-address / three-output ROM tables pass typed
structural and ExpandedStrict physical certification, exact durable emission,
deterministic reconstruction, internal truth and independent per-table timing
acceptance. This remains a prototype adapter; public compile() does not select
these matrices. Constant outputs and frontend rewrite provenance remain open.

## Observed problems and general fixes

The five-row fixture certified previously was insufficient evidence for the
whole family. The first exhaustive attempt failed on unused literal rails:
there was no actual sink whose route could honestly own their conductors.
The adapter now retains each rail only through its last real decoder tap.
True rails still feed their global inverter. An unused inverse rail and its
connector have no conductor load. This is derived from physical taps and does
not recognize a table name or manually selected data words.

The six-row table [3,6,6,1] then exposed an output-restoration repeater hidden
inside a nominal OR collector. That collector is not one continuous passive
OR region. The normalized graph now explicitly records an early OR (when
needed), a BUF implemented by the actual restoration repeater, and a late OR
(when needed). A late-only output trims its unused prefix rather than inventing
a constant input to a buffer. The early OR observation is placed away from
possible contributor landings. All actual buffers have primitive delay owners;
branch counts exclude a delay already owned by the target primitive.

The prototype uses an explicit Library with the default NOR1 and bare OR
entries plus a one-repeater BUF entry. Every selected topology is symbolically
proved. The shipping default Library's two-torch BUF implementation is not
changed. This explicit library is supplied to the existing certifiers; no
unchecked emission or verifier omission is introduced.

Two generic verifier issues were exposed by those honest records:

- Listed junction injection terminals can be adjacent. Connectivity now cuts
  only the edge between two listed landings of the same junction. It does not
  cut the entire landing cell or edges to upstream/unlisted route conductors.
  Reverse closure can walk between listed landings, still stopping at their
  incoming routes. The region connectedness check remains in force.
- An incoming route terminating on a primitive-owned repeater must not seed
  that primitive's output as its own signal. Emission still preserves the
  incoming terminal identity. Coupling uses its validated primitive owner
  when classifying that terminal, so the buffer output remains a separately
  checked source network.

ExpandedStrict Connectivity and Coupling revisions both increase to 3. The
three explicit API case-identity snapshots change deliberately. Historical
quality references are retained; legacy verifier revisions are unchanged.

## Independent experiment

Each data table compares the certified candidate against the independently
retained, committed aligned-refresh prototype. All nine metrics must be no
worse per table; there is no aggregate allowance for an individual regression.
The original literal words independently define both driver and lamp truth.
Every actual internal NOR, OR and BUF observation is also checked against the
normalized logical graph after every setup and target transition.

Each timing arm runs 43904 measured transitions plus 43904 setup transitions
in persistent simulator history. The internal-truth replay runs those phases
again. Candidate reconstruction must preserve its fingerprint, and durable
certified emission must match every cell of the adapted prototype.

| Measurement | Control | Certified candidate |
| --- | ---: | ---: |
| Sum of measured settling ticks | 420944 | 420816 |
| Tables with lower total settling | - | 16 |
| Worst settling | 16 ticks | 16 ticks |
| Driver and lamp glitches | 0 | 0 |

The 128-tick aggregate reduction comes from trimming unused rail activity;
it is not a claim that driver latency or the worst critical path improved.
No table regresses in settling, latest driver change, glitches, single-input
glitches or pulse duration. Block counts and volume are not acceptance metrics.
The control here is the prior prototype, not 2744 production compilations or
2744 Yosys compilations; no family-wide superiority over those generators is
claimed from this experiment.

## Regression and boundaries

- The exhaustive typed test checks all 2744 tables with full strict certification,
  physical truth, all internal truth, topology proofs, exact emission,
  deterministic candidates and all nine independent per-table quality caps.
- Three focused typed tests retain the five-row fixture and reject missing
  contributors, disconnected forged regions, reversed repeaters, missing
  primitive delay ownership and a foreign wire touching a buffer output.
- All 15 verifier unit tests pass. The existing junction test now additionally
  accepts adjacent listed terminals, rejects a missing listed terminal, and
  rejects an unlisted foreign route attached to a listed terminal.
- All 14 fragment API tests pass, including revised explicit fingerprints.
- Both production frontend quality suites pass: 1312 measured transitions,
  preserving their existing driver/lamp truth, timing, pulse and determinism caps.
- git diff --check passes.

The Rust 1.98.1 release unit-test compiler initially crashed in LLVM. Retrying
with RUST_MIN_STACK=16777216, as its diagnostic suggested, succeeded; no test
was disabled or weakened. The existing packing.rs warnings remain. These
focused checks do not claim the entire repository suite is green.

The remaining 1352 tables include exported constant outputs and are explicitly
outside this qualification. No larger address width, Minecraft runtime,
constant-driver integration or production frontend-to-matrix rewrite is
certified by these results. Those remain required before shipping selection
through the public compiler.
