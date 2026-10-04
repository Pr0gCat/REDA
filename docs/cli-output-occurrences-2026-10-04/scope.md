# CLI output occurrences: clean consumer qualification

Clean HEAD 4d21915 plus only the seven-file source occurrence metadata patch and the exact two-bin patch passes 11 build_circuit tests and 3 mc_dump tests. This excludes dirty user arithmetic and the disjoint-support candidate.

mc_dump emits source-ordered OUTPUTPORT records for raw and certified rewrite results, using the owned occurrence positions and actual rewrite signal bindings. build_circuit stdout and JSON consume the same per-occurrence binding helper. Repeated drivers use public labels as JSON keys to prevent duplicate keys; unique unpinned outputs retain their old internal keys for compatibility. Missing or surplus labels fail explicitly. Certified aliases keep their distinct real lamp positions.

Signal-keyed pin placements cannot represent occurrence-specific pins. Ambiguous aliased output pins and input/output key collisions are explicitly rejected before placements can silently overwrite one another; this limitation remains open and is not claimed as successful alias pin support. Raw duplicate-output canonical ownership is being separately repaired and is not qualified by these tests. Actual new CLI export/reload and parser acceptance remains pending. No whole gap is complete.
