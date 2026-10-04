# Native legacy combinational processes

Supports complete explicit `always @(a or b)` and `@(a,b)` lists, ordered blocking assignments, atomic concatenated targets, and one complete existing case statement. Wildcard and always_comb single-statement historical paths retain their existing lowering. Normalization occurs after hierarchy aliases are known and before both RTL emission and bit blasting.

Blocking statements substitute current per-bit symbolic values. All concatenated RHS slices are evaluated before any LHS update. A read before the local target has been written is rejected as storage. Sensitivity coverage checks actual external reads through canonical flattened aliases; direct wire reads are not expanded to their upstream driver inputs. The final local write becomes the driver; existing multiple-driver checks remain.

Projected expressions retain source bit offsets via RtlExpr.origin_lsb, default zero for existing paths. Wide temporary reads have explicit four-bit provenance coverage.

The formerly unsupported complete `always @(a or b) y = a & b` source is preserved verbatim as a native-positive case. The unsupported five-case fallback matrix now uses incomplete `always @(a)` for that slot.

Nine tests cover literal truth, atomic swaps, alias membership/order, storage/incomplete/mixed-edge rejection, complete case lowering, bit provenance, real native/auto CLI with missing Python, ordered hierarchical physical checks, and full legacy seven-segment generation. Physical checks retain all 512 from/to phases, all implemented gates, public lamps, and independent literal truth; full seven-segment also compares the exact original wildcard control world. Implemented oracle inputs are bound through certified implementation observations because public d0 names can differ from implementation input0 names.

Retained failed evidence: tests.log is the original concatenated-target parser failure; final-tests.log contains eight passing cases and a MissingInput failure from the test helper source/implementation naming mismatch. The helper correction reads actual implemented lever states while preserving independent source truth. No simulation rules, caps, golden values, or ignores changed.

Limits remain: mixed/nested combinational control flow, incomplete lists/latches, mixed/async edges, general event simulation and four-state semantics are unsupported. This closes one bounded native frontend hole; it does not complete the broad frontend gap or prove Minecraft/QoR acceptance. Top-level inout remains explicitly unsupported.

Pure patch is bound to committed HEAD in source-bindings.json and excludes pre-existing user arithmetic changes. Root performs independent clean qualification and commit.

## Independent clean qualification

The root independently applies the precise patch to committed 92bf87e, excluding dirty user arithmetic and formatting. All 70 integration tests pass: nine new process tests plus 61 existing compiler, hierarchy, parameter, inout, DFF and reset tests. All three existing viewer controls pass in native debug and Node/WASM release, preserving the four exact netlist/source-map fixtures, the historical channel-world fingerprint and the separate 512-phase default seven-segment control. Python is unavailable throughout. Immutable source hashes, commands and raw compressed logs are retained in `clean-qualification.json`. This is qualified legacy process support, not complete native HDL or whole-project convergence.
