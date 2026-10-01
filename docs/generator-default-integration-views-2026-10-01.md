# Source and certified implementation views

Reference fd99a80; physical generator control9779496.
Default integration needs consumers to use the actual rewritten implementation,
not fabricated source gate positions or an expansion using default techniques.

CompiledCircuit::from_certified_rewrite now retains the sealed rewrite and
constructs source-named public ports, actual implementation gate positions,
canonical physical facings and typed observations from its certified manifest.
physical_rewrite() retains every output occurrence's distinct terminal; the
legacy output_positions map keeps its existing one-coordinate-per-name shape
and explicitly documents which alias occurrence it represents.
implementation_netlist(source) exposes the matching physical graph.

Timing observes actual implementation nets with a deterministic prefix that
cannot collide with source names, plus original public output driver names.
When a primary input is also an output, its output label observes the actual
public driver and its implementation input has a separate label. Critical
paths use the implementation graph and proved output bindings. Arbitrary
proved input/output permutations are preserved; declaration order is never
assumed to be the binding order. Graph-preserving callers retain their existing
labels, graph, layout kind and observations. Unavailable legacy route analysis
continues to return no fabricated per-edge model.

PrimitiveGraph::from_certified_rewrite projects the selected manifest without
choosing library entries or merge isolation again. It preserves comparators,
reference torches, repeaters, actual signal edges, bare junction contributors
and source-named boundary nodes, including permuted and repeated ports.
Viewer uses this projection when a rewrite is present and shows actual gate
kinds. Its source diagram remains logical; a boundary equivalence proof does
not assign generated primitive ownership to removed source gates, so their
physical containment/terminal mappings stay empty and boundary_rewritten
explicitly marks this relationship. Existing baked and graph-preserving
viewer paths keep their established lowering provenance.

Validation:5 physical-rewrite integration tests pass, including collision
and retained-history measurements, primary output aliases, permuted input and
output bindings, selected comparator and merge edges, and16 representative
qualified techniques/constants. All24 viewer tests pass, all13 timing tests
pass, all14 fragment API tests (including existing fingerprint snapshots)
pass, and both DFF Native/fresh-Yosys and observed output isolation regression
tests pass. No circuit generation, physical verifier or simulator changes.

Default automatic selection is still disabled. A fixed-data prospective
policy based on twelve target metrics (choose player only when it dominates
correct shipping; otherwise retain shipping) would choose3656 player and440
shipping cases and violate existing caps for139 nonconstant tables. Failures
include source tables21,35,49 in glitch and wrong-pulse metrics. This is evidence
against enabling that policy; all prior caps remain intact. Setup metrics
would add further requirements, not justify these target regressions. The
remaining circuit generation must resolve these tradeoffs before full4096
qualification of default selection and its fallback.
