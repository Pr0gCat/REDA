# Certified source boundary for physical rewrites

The original CompiledCircuit API explicitly requires its caller's netlist to
match every physical gate observation. A ROM rewrite changes the internal
graph, so returning that type against the original netlist would fabricate
positions for eliminated source gates and invalidate timing/provenance tools.

realise_physical_rewrite provides a separate read-only result holding both
source and implemented netlists, the exact boundary rewrite certificate, the
implemented graph's equivalence certificate, the fully certified emitted world
and implemented observations. It returns source input, output and output-driver
coordinates in source declaration order. Repeated output aliases consume each
implemented declaration occurrence instead of collapsing through a name map.
Source intermediates never receive invented physical coordinates.

Construction runs the existing complete rewrite proof, instance/topology proof,
full structural/ExpandedStrict physical verification and compatibility boundary
validation on the same borrowed candidate/netlist/library. Failure returns no
sealed result. Fields are private; no deserialization constructor exists. Each
proof separately uses the caller's work limit; this is not a timing budget or
physical performance guarantee. No new certificate format or verifier revision
is introduced.

The complete 4096-table prototype harness now uses this production API for its
actual certified worlds and observation data. The native-source proof set stays
f50a6fbfb11fbc021c8be2590d255a058c88d0bd6da2964a0eefb13178a791d3.
All original per-table nine-metric limits and independently frozen previous
nonconstant limits remain enforced: 65536 measured target transitions per arm,
611056 control / 608752 candidate aggregate settling, 120 improved tables and
16 worst ticks. This interface introduces no additional speed improvement.

Focused checks reverse both input and output declaration order and verify exact
positions, refuse omitted bindings, zero proof work, malformed candidate
contributors and source data wrong only at address 3. Fresh Yosys two-case
integration uses the same sealed API. Internal observations belong exclusively
to implemented(), which is the netlist timing analysis must receive.

Production compile() has not enabled matrix selection yet. The matrix builder
still resides in the qualified integration fixture. Next work is to promote
that builder and bounded function recognition into generation, connect the
explicit rewrite result to callers, and compare actual shipping layouts against
independent fixed controls before accepting selection. This interface does not
claim adoption, full Minecraft certification or completion of the ongoing goal.

Validation on the final implementation: full-family replay passes in 100.33s;
focused ordered-boundary/refusal and fresh-Yosys tests pass. Existing four
rewrite-proof tests and four constant-driver physical tests pass. Production
quality suites preserve all caps over 1312 measured transitions. git diff
--check passes. Existing unrelated packing warnings remain; this is not a
claim of a globally green repository suite.
