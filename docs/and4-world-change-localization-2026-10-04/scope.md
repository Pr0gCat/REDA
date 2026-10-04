# Separate historical and4 world changes

No shared production source, historical expectation, physics or limits was changed. All code changes are isolated diagnostic sources under /tmp. These are localization results, not accepted repairs.

Frozen exact grown guard results after actual compilation:

- 57ed241 parent: pass, bbox(68,6,61) and original world_digest7885360476135848183;751filtered.
- 57ed241: pass, same original world;757filtered.
- cc43241 parent9dd064f: pass, same original world;945filtered.
- cc43241: bbox61 passes;digest15815489870140392465 fails;1204filtered.
- 32df849 parent26370a0: bbox61 passes;digest16361578916203226807 fails;1212filtered.
- 32df849: bbox(68,7,62) fails;1214filtered.
- current b01ad8e: bbox(68,6,62) fails;1265filtered.

cc43241 is the first physical world change between the exact passing adjacent source and failing adjacent source. Its routing search explicitly changed historical toward-goal height discount to charging every vertical edge. The first world diff has51 changed coordinate states,236 versus235 nonAir cells, unchanged primitive public lever/lamp positions and unchanged world extent. e5dedef later restores the negotiated caller's original discounted prices; this historical cause alone cannot explain the remaining current failure.

32df849 separately expands placement cells for every multi-input Torch in any graph without a DffPosedge. This is broader than the actual selected_gate/terminal contract predicate used by routing. The measured current-versus-original primary input c changes from(34,1,58) to(34,1,59);the specific 32df849 world coordinates have not been separately dumped. The current versus original world has174 changed coordinate states,236 versus242 nonAir cells. The current maximum nonAir extent is(65,3,59);actual stone(34,0,59) and lever(34,1,59) occupy the boundary, so trimming the world cannot recover the original physical contract.

Counterexamples reject simple fixes:

- Restoring the entire pre-cc candidate_world_size logic still yieldsbbox62. The extra extent is actual anchor/route geometry, not merely extra footprint padding. No digest/containment pass is claimed.
- Disabling only typed DirectedDustRunway selection leavesbbox62 and changes footprint arithmetic to[137,1644,1444,278,78,84],still not historical. Placement expansion remains independently active.
- Disabling only blanket weak placement geometry while preserving all current typed routing requirements restoresbbox61, but digest10083724303505815603 still fails. Modern weak=false also activates de1e7ec ordinary straight approach occupancy, so this is not a complete frozen-placement replay and is not an adoptable fix.

A coherent general repair must derive placement reservations from the actual caller-owned terminal contracts (ordinary straight approaches, selected weak runways, sequential geometry) rather than a global combinational-vs-DFF predicate. Compatibility/growth policy and qualified generated routing must be explicitly distinguished without removing accepted isolation. It must pass exact historical contract where promised and independent physical/continuous-wave controls elsewhere. No such repair is qualified here. Other footprint/derived-realised guards are therefore still open; they cannot all be attributed to sizing.

Historical builds use clean git archives of Cargo.toml,Cargo.lock,src,docs/derived,tests/fixtures and the original test unchanged. Commands use explicit --manifest-path, CARGO_INCREMENTAL=0,CARGO_BUILD_JOBS=1,RUST_MIN_STACK=536870912,ulimit stack524288;source files were touched after archive to force actual rebuild when reusing a target. An initial cache-only pre-lid invocation was discarded and overwritten by the actual compile log. An initial pre-unified compile omitted original include_str fixture inputs;exact original commit files were restored before the recorded passing run. Raw logs show real compile and counts. Historical executables were not preserved before reused target links;these source commits and raw logs make the experiment reproducible but do not claim archived historical executable hashes.
