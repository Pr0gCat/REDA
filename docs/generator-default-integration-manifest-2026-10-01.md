# Preserve the actual certified implementation manifest

Reference `9779496`. Default compiler integration cannot reconstruct a
rewritten circuit by expanding its source graph, or by expanding the
implemented Netlist with the default library. For example, a NOR1 may have
been certified as a subtract comparator with a constant reference rather
than the default wall-torch technique. Removed source gates also have no
physical placement to invent.

CertifiedPhysicalRewrite now retains the exact ExpandedPhysicalCandidate
passed through source equivalence, structural and physical certification.
The readonly implemented_candidate() accessor exposes the actual instance
techniques, expanded primitive topology, placements, routes, observations
and output boundary ownership. The private stored value stays tied to the
same sealed source/implemented graphs, certificates and emitted world.
Mutating an external clone cannot mutate the certified value.

No generation, simulator, verifier, timing metric or physical geometry
changes. A targeted test checks actual comparator techniques, all placed
block states against the certified world, compatibility observations and
clone isolation for NOR, polarity-sum and composed-product implementations.
All77 Native/fresh-Yosys source equivalence cases pass again.

This supplies the manifest required for downstream integration; automatic
selection remains disabled. Next work must preserve this graph/technique
contract in CompiledCircuit, timing and viewer, then qualify deterministic
nonregressing default selection and its fallback across the full4096 family.
