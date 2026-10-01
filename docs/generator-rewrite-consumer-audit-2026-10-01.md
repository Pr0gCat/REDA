# Certified rewrite consumer audit

Reference: 79c0b28. Default automatic selection remains disabled.

The legacy expansion-equivalence and world-partition APIs assumed every
physical primitive came from the supplied source netlist's default library.
For a certified physical rewrite they now require the actual manifest's flat
projection. They reject a changed source, graph, public world or observation
view before using the sealed certificate. Complete BlockState equality includes
comparator mode and retained extra properties; a world hash that omits those
properties is insufficient. The sealed constructor derives expected metadata
from proved boundary bindings, preserving permutations and output aliases.

Partitioning accounts for actual primitive and boundary components from the
manifest. Remaining dust, route repeaters and solid supports are classified
only after exact sealed-world equality; no foreign blocks can hide as fill.
Legacy geometry checks remain in the original path. The strict physical
adapter, electrical verifier and simulator are unchanged.

Six physical rewrite consumer tests pass, including adversarial source/edge/
primitive/mode/position/observation mutations. Selected comparator, repeater,
NOR, merge, constant, alias and permuted-port families pass both public APIs;
primitive counts match the manifest and partition covers every non-air block.
Nine legacy equivalence tests and five legacy partition tests also pass.

The independent frozen 4096-function diagnostic now evaluates the prospective
policy's retained setup history using tools/probe_default_setup_tradeoff.py.
Of 36 shipping choices, 33 dominate the player candidate on target metrics.
Three choices have a lower total settling time but worse worst settling,
driver last-change and lamp last-change metrics: [6,4,0,1], [6,0,2,1] and
[6,0,4,1]. The same three regress on those setup metrics relative to the
player candidate. None has setup glitches, but absence of glitches does not
qualify the delay tradeoff. See generator-default-setup-audit-2026-10-01.json.
Function encodings in this diagnostic identify evidence, not generator rules.

Next required work: remove or resolve that structural quality tradeoff;
make mc_dump and its conformance consumer describe actual gates, selected
components and explicit source boundary bindings; integrate qualified
selection into compile() without changing unsupported/cyclic/DFF contracts;
measure the actual default world across the entire frozen family and repeat.
No default-selection completion is claimed by this consumer commit.
