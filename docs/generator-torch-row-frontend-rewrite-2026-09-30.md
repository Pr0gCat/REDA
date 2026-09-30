# Proof from frontend logic to the actual torch matrix

Status: the complete nonconstant two-address / three-output family now has a
verified chain from native frontend logic to the normalized matrix graph and
from that graph to the actual typed physical candidate. This is an integration
prerequisite, not production adoption, constant-output support or a new speed
improvement. Public compile() continues to use its existing generator.

## Problem and general solution

The earlier physical certificate proved the matrix against its own normalized
NOR/OR/BUF graph. The existing prove_combinational_equivalence API deliberately
requires an instance graph for the same logical netlist; it cannot establish
that a different frontend graph was safely replaced. Retaining frontend gate
identities on unrelated physical nodes would falsify observation metadata.

prove_netlist_rewrite now checks exact boundary equivalence between two
arbitrary valid combinational netlists. Its caller supplies rewritten input
and output names in the source's declared port order. These bindings must
cover the entire rewritten boundaries, including the multiplicity of output
aliases. Renamed ports, reordered declarations and different gate structures
are supported; omitted, duplicated or unknown bindings are rejected.

The checker reuses Netlist::combinational_order, GateKind::evaluate and the
existing netlist fingerprint encoding. It validates gate arity, unique drivers,
resolved signals and acyclicity before evaluation, and rejects stateful gates.
It computes the complete vector count and required work before enumeration;
shift/multiplication overflow and an insufficient work cap fail closed. Even
an empty boundary costs at least one step per vector. No sampled proof or
partial certificate is returned. A mismatch identifies the source input vector
and output index with both Boolean values.

The certificate records both exact netlist hashes, ordered boundary bindings,
all vectors checked, work used and a versioned proof fingerprint. Fields are
private and it has no deserialization constructor. This is a functional
certificate only; it does not stand in for physical certification or timing.
Proof work counts are resource limits, never circuit quality rankings.

## Experiment and authoritative proof chain

For every one of the 2744 nonconstant ROM tables, generate a SystemVerilog
module using nested address conditionals and literal data values, run the real
native frontend and lower_optimised, and materialize its declared y0/y1/y2
boundary through CompileArtifact::output_map. Output aliases are retained;
source gates are not relabelled as matrix gates.

The complete four-vector rewrite proof runs against the actual normalized
matrix netlist. The existing prove_combinational_equivalence then proves that
normalized netlist against the selected actual instance graph and explicit
prototype library. The rewrite certificate's rewritten hash must equal that
second certificate's lowered hash, and its candidate fingerprint must equal
the actual candidate fingerprint. Those same candidates pass the existing
full structural/ExpandedStrict certification and exact durable emission.

Thus all 2744 source rewrites are proved, with 10976 exhaustive Boolean input
vectors across the family. The physical test retains 43904 measured plus
43904 setup transitions per timing arm, plus a separate internal truth replay,
all topology proofs and deterministic reconstruction. All nine per-table caps
against the committed independent prototype control remain unchanged.
The settling totals remain 420944 control / 420816 candidate; this round does
not introduce an additional timing improvement.

Two focused cases additionally use fresh Yosys synthesis, covering both a
five-row matrix and a six-row matrix with explicit restoration buffers. The
same linked hash/candidate proof chain passes. A changed source whose only
error is at address 3 is rejected with that exact counterexample, proving the
checker did not stop after the earlier matching input vectors. No claim of
exhaustive Yosys-family proof is made.

## Validation and remaining work

The four focused rewrite tests cover deterministic certificates, renamed ports,
changed topology, exact budget acceptance, one-step-short refusal, invalid
functions and bindings, output aliases, incorrect input permutations, cycles,
undriven signals, multiple drivers, sequential logic, invalid arity, overflowing
input width and a zero-budget empty circuit. The exhaustive matrix test and
the fresh-Yosys/wrong-source integration test pass.

Existing equivalence regression: nine tests. Fragment API regression: 14 tests.
Both production frontend quality suites pass all their existing checks across
1312 measured transitions. git diff --check passes. An initial release unit-test
compile crashed inside rustc; an identical retry with RUST_MIN_STACK=16777216
compiled successfully and all focused suites passed. No test was skipped or
weakened. Existing unrelated packing.rs warnings remain; this is not a claim
that the entire repository suite is green.

The 1352 tables with at least one exported constant output are still outside
this qualification. The public compiler also needs explicit rewrite attribution
and boundary integration: an original intermediate gate without a physical
counterpart must never be assigned a fictitious observation position. Those
requirements remain open before enabling matrix selection in production.
