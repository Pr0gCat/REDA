# Original whole physical-cone projection baseline

This is an identity/ownership experiment, not a quality optimization or a production candidate. The existing original whole compiler's exact one-to-one physical manifest is projected into three disjoint input-support cones. No leaf is recompiled or rerouted. Cells, route branches, terminal isolation, repeater delays and directions, primitive/library choices, aliases and observations are preserved. Typed IDs and integer origins move together. Shared gate ownership, cross-cone routes/aliases, caller pins and incomplete manifests fail closed.

All 121 owned metadata records and 15 source gates are covered exactly once. Inverting every leaf's typed-ID/origin mapping reproduces every original record payload. The union reproduces all 1094 occupied cell states, and the original, reconstructed union and sealed root have fingerprint `4818f3a3af7c2c2f80c973a59e730e5d82697afb6c519b38483e5a9b01cee37f`. Root inputs, ordered public occurrences including v/w aliases, output maps, every gate observation and facing are identical. Exact worlds and full root/leaf manifests are serialized here.

Independent recomputation:

```sh
python3 docs/original-physical-cone-projection-2026-10-04/recompute.py docs/original-physical-cone-projection-2026-10-04
```

The independent result reports record payload roundtrip counts, source-gate coverage and physical-cell equality, rather than trusting the Rust accepted flag. Every source dependency listed by Cargo's `.d` file, including embedded Verilog fixtures, plus Cargo bindings and producer executable/library hashes was frozen before execution. The executable bytes remain in `/tmp/reda-original-physical-cone-projection-20261004/evidence/identity-v3/producer-executable`.

The whole physical world and all caller/observer positions are unchanged, establishing the correct original physical-cone control. Independently running a projected leaf with a new stimulus schedule is not claimed equivalent to the original whole retained timeline. The next experiment must make a small, owned local change and compare against the original whole baseline, including all implementation/public waveform and work metrics. Broad generic certified composition remains incomplete.
