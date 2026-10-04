# Rejected gap4 streaming/static candidate

Independent replay rejects this candidate. Six original feedback cases complete
256 phases each in both arms: Native 3×256 and Yosys 3×256 per arm.
Both source-bound literal state oracles, physical events and hidden storage
cells pass. Same-graph quality comparison finds 1025 regressions:
145 settlement delays, 88 transition counts, 728 last-arrival delays and
64 wrong-target pulse widths. The candidate must not be adopted on this evidence.

```sh
python3 replay.py
```

Expected exit status is **1**, with `qualified:false`. Retained compressed traces
and the exact main checker reproduce every regression. Original full control
logs remain at the paths in the result's raw bindings; their hashes are retained
without duplicating large logs. Source manifests were independently checked
against both staged trees. The corrected baseline's dedicated test executable
hash and routing-source binding were verified, with compilation after the source
timestamp. The earlier shared-target-cache run is excluded and its retained
artifact hashes are recorded; its apparent terminal success is not a baseline.

This compares the combined optional streaming/static-potential arm against the
required router. It does not isolate the static heuristic's causal contribution,
establish broader gap4 completion, or claim quality from block counts. No main
source adoption or production change is part of this package.
