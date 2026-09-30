# Honest logical bindings for the torch matrix

Status: logical-to-physical observation mapping evidence; not a certified
ExpandedPhysicalCandidate, not production adoption, and no new speed claim.

Problem: the prior matrix proved external truth and timing, but replacing an
existing frontend circuit with that world while retaining the original gate
identities would falsify per-net metadata. Physical verification requires the
actual gate/connection graph, not aliases claiming unrelated internal nodes.

The new test-only binding builder derives the same ordered implicant rows from
data, then explicitly represents the actual circuit with NOR and OR:

- The two physical address inverters are independent NOR nodes.
- Each literal mismatch torch is a distinct NOR node at its real position.
- A two-literal row has an explicit OR junction before its selection repeater;
  a one-literal row uses its actual mismatch signal directly.
- Each programmed data torch is its own NOR node, fed by the row-control net.
  Equal Boolean functions are retained as distinct physical gate identities.
- Each multi-term output has an OR collector; a single-term output retains its
  real data-torch signal as the declared output, without inventing a buffer.

Every logical gate has a real observed torch or dust site. The set of bound NOR
positions must equal the entire world's torch set, preventing hidden or fictitious
logic torches. The normalized netlist is reconstructed deterministically.
Existing InstanceGraph::one_to_one and validate accept it. Every selected gate
topology is independently checked with prove_topology_semantics.

## Physical evidence

For the fixed [1,6,3,5] ROM, the complete mapping contains 18 logical gates,
14 actual torches and four junctions. These counts check coverage, not quality.
Every internal signal and declared output is checked after both setup and target
phases of all 16 transitions in persistent simulator history. Public output
truth remains compared to independent literal data words.

The same checks pass for all 2744 nonconstant two-address/three-output data
tables (14 possible nonconstant Boolean functions per output, cubed). That is
43904 measured plus 43904 setup transitions. All instance graphs, internal
settled truth, torch coverage, topology semantics and binding reconstruction
checks pass. No case-name recognition is involved.

The remaining 1352 tables have at least one constant output. The frontend still
explicitly rejects exported constant outputs in src/frontend/netlist.rs, and
this mapper does not conceal that missing representation with fake NOR/OR
nodes. The earlier complete 4096-table physical truth/timing evidence is intact;
this logical mapping qualification is explicitly narrower and does not certify
constant-driver support.

## Validation and integration boundary

`cargo test --release --test torch_row_decoder matrix_logic -- --nocapture`
and `cargo test --release --test torch_row_decoder all_nonconstant -- --nocapture`
both pass. The exhaustive binding replay took 22.98 seconds. `git diff --check`
passes. The existing packing.rs unused_mut warning remains. Only the test
fixture and evidence files changed; prior timing caps were not edited.

This does not run certify_expanded_structure or realise_and_verify_expanded.
The current mapping establishes normalized logic and true observation sites;
it does not yet record typed routes, branch terminals, junction contributors,
block ownership or repeater delay ownership. These must be constructed from
actual geometry before structural and physical certification. The source
frontend netlist also needs an explicit proven rewrite/provenance relationship
to this normalized graph before shipping it through the existing public API.

Next concrete work: bind actual shared route trees and delayed components to
the normalized graph, run the existing structural/physical verifier pipeline,
and add constant-driver representation for the full ROM family. Keep the
independent original-generator timing reference and full data-family checks.
