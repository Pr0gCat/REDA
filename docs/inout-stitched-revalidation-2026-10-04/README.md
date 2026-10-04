# Recovered shared-inout physical stitching qualification

The recovered private prototype now compiles and passes its complete independent source-bound settled-state test. Two compiled internal tri-state writers and a genuinely independent external lever drive one diode-isolated shared dust channel. The external presence/value controls only describe the caller protocol; they do not inject an evaluator-produced value into the channel. Every one of 37 implementation gates, each injection pad, every channel cell, status outputs and valid public readback are checked at all 8192 endpoints of the 4096 ordered transitions. Expected 0/1/X/Z classifications match the retained independent Icarus oracle by exact source and CSV SHA256. The test terminates successfully after 391.25 seconds.

The recovered generic Drive lowering retains raw data when a conditional arm is absent, preserving conditional width/sign through typed XOR-zero and compiling presence separately. The previously failing external-caller placement now succeeds without changing routing limits or simulator physics. This result validates the recovered final prototype; the older pre-recovery successes are not used as its proof.

The three semantic controls also pass: source hierarchy against all8192 Icarus endpoints; vector aliases/concatenation/multiple readers; and explicit unsupported arithmetic/read-feedback boundaries. Refusal controls establish limits and are not counted as supported behavior.

All jobs run serially through `tools/run_memory_bounded.sh`, with aggregate MemoryMax=4GiB, MemorySwapMax=0 and Cargo jobs=1. Source hashes are checked again after completion. The archive retains all168 frozen source/fixture/Cargo files, rather than relying on temporary directories. Logs and exit codes are included.

Replay: extract `source.tar.gz` into an isolated workspace directory, set `CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 RUST_MIN_STACK=2147483648`, set shell stack limit to2097152 KiB, then run through the repository memory wrapper:

```
cargo test --offline --lib compile::resolved_circuit::tests::real_compiled_drivers_and_readers_use_same_shared_channel -- --exact --nocapture
cargo test --offline --lib frontend::resolved::tests -- --nocapture
```

This remains a private prototype. Public native compilation, typed inout export/viewer contracts, transient waveform quality and actual Minecraft conformance are not qualified. The unrefreshed shared channel supports at most four total drivers. No eight-gap acceptance row is closed by this evidence.
