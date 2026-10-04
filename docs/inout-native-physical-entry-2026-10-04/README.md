# Native source-to-physical entry integration prototype

`frontend::compile_systemverilog_physical` now compiles native source directly to a compiled world with typed source-owned pinout. Ordinary Boolean sources reuse the existing native frontend and default physical compiler. A structurally identified top-level Inout declaration selects the qualified shared-channel semantic/materialization path. Selection uses parsed port direction, not fixture names. Original source inout identity, external ownership/value controls, checked X/Z observation and bundle loading remain available to an external library consumer.

The netlist-only `compile_systemverilog` entry retains its existing contract and does not silently erase shared-bus ownership. The source-to-physical entry returns `CompiledNativeCircuit` with physical world and typed pinout. It is implemented in the isolated recovery tree; the production CLI/viewer still use their original paths and have not adopted this entry.

Two actual external-crate integration tests pass: ordinary inversion retains source a/y ports and no shared port; the original hierarchical two-writer source compiles to one actual shared Inout terminal, exports native.litematic/native.pinout.json and successfully reloads them with exact source/world validation. Tests run serially through the4GiB/zero-swap wrapper, terminate0, and require no Yosys invocation. The game and whole-requirement absence-of-Yosys acceptance are not inferred from these tests.

Replay begins from `../inout-stitched-revalidation-2026-10-04/source.tar.gz`; apply `complete-owned.patch` and run `cargo test --offline --test native_physical_inout -- --nocapture --test-threads=1` with the same bounded compiler environment. The patch contains all changes since that archive, including pinout, checked consumer, export/load and this entry. Changed source hashes and actual exported bundle are retained.

Production adoption, native/auto CLI and viewer consumers, broad representative inout variants, intentional caller mismatch tests, full transient quality, game conformance and complete project regression checks remain open. All eight acceptance rows remain open.
