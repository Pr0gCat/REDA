# ROM64 ordinary-entry qualification

Status: complete for the stated ROM64 ordinary-entry scope. Simulation and actual Minecraft each pass all 1,088 fixed phases; independent raw replay and persisted-air/unforced-chunk cleanup audits pass. Eight selected related regression tests pass, with zero failures and zero ignored tests.

The ordinary native physical API and `build_circuit --frontend native|auto verilog ...` generate the certified 64-word, 16-bit torch matrix directly. Six-input combinational boundaries with one to sixteen outputs retain the source graph, implementation graph, physical observations and rewrite certificate. Structural generation and physical verification remain mandatory; the ordinary entry no longer repeats a full retained-history quality experiment against raw routing. Four/five-input selection remains unchanged. This does not establish exhaustive timing dominance over raw routing.

The unchanged original ROM64 consumer test passed in 13.04 seconds. It checks all 64 literal words, 1,024 public values, 141,312 actual implementation-gate values, native/auto CLI execution without Yosys or Python, and exported/reloaded world and pinout identity. The original canonical world fingerprint remains `f3af4444f9776335f3c51a768a8a7ae3d5e387e66974807feb5009919b459b5a`; no golden was repinned.

The fixed version-two transition manifest contains 544 pairs and 1,088 phases in one continuous history. Coverage includes every address, every directed single-bit flip, every directed complement, increasing adjacent addresses, and the sixteen historic diagnostic pairs among 0, 21, 42 and 63. The physically farthest literal row is address 63, the farthest collector contribution is address 0, and the longest row route reaches output bit 15. Their activation and deactivation are covered by the fixed full-address, complement and single-bit cases. No all-pairs timing claim follows from this sample.

Simulation independently replays 510,486 events and checks 2,402,304 actual gate values and 17,408 public values. The slowest measured settlement is 56 game ticks. Initial construction has driver transients; subsequent sampled settled-to-settled phases have zero driver glitches. Lamp values are asserted separately by the executable; the event replay does not claim lamp waveform coverage.

The independent geometry audit checks all 857 repeater directions, delays and restoring intervals, all 887 decoder/programming torches, every literal rail and address row, sixteen separated collectors, and the solid support under all 11,783 dust cells. Direction, delay, missing-refresh and missing-programming-torch corruption controls are rejected.

The game uses the existing native atomic-capture implementation with sixty sites per storage page to keep every RCON reply small. All implementation gates, public lamps and physical relay powered/locked states are observed. Frozen direct-reader parity checks every site before qualification. Existing build/vector floors, three agreeing polls, the 13.3-second ceiling and region-readiness guard remain unchanged. Game observations establish steady truth and sampled quiescence; they do not prove per-tick or intra-tick glitch freedom. After the first game phases at 20 ticks/second, native `tick rate 1000` accelerates game ticks. This is game-rule conformance at an accelerated clock, not a real-time latency benchmark. Cleanup restores 20 ticks/second. The initial read-only check correctly rejects unloaded coordinates. A recorded reload of the already-cleared region then confirms persisted air at every original site without placing or clearing any blocks; the same chunks are released again. The unchanged independent auditor accepts the actual air readings and final unforced-chunk replies. The clock resumes normally and the server stops. Both the unloaded-read rejection and the successful reload observations are retained.

Every heavy job is serialized under the existing 4 GiB memory limit, zero swap and one Cargo worker. Minecraft is off during compilation and regression tests. The source snapshot includes pre-existing workspace edits; its complete source hashes bind the actual tested context. Only the ordinary-entry selector, six-input matrix generator admission, ROM64 fixture/tests and this focused evidence are eligible for this commit. Broader synthesis-search admission edits remain unstaged and are not qualified by this document. The eight broader project-gap rows are outside this ROM64 goal.

A separate three-phase timing diagnostic measures generation plus physical checks at 3,428 ms, simulator initialization at 25 ms, startup simulation at 104 ms, and the two settled complement transitions at 48/49 ms; checking all actual values takes 2 ms per phase. These are observations under the retained optimized/assertion-enabled build, not general performance guarantees.

## Retained proof

`qualification-artifacts.tar.gz` preserves the root-relative source snapshot, source/binary bindings, transition manifest, actual observations, game commands and replies, export/reload artifacts, independent auditors and terminal results. `manifest.json` binds the archive and the summarized results. Credentials and server properties are not included. The older consumer-result manifest bound version one of the transition manifest; its original bytes are retained as `transition-manifest-v1.json`, while final simulation/game qualification uses version two.

To replay offline, extract the archive at the repository root, then run:

```sh
python3 target/qualification/rom64-current-game/audit.py
python3 target/qualification/rom64-current-game/audit-structure.py
python3 target/qualification/rom64-current-game/audit-representative.py
python3 target/qualification/rom64-current-game/audit-batch-raw.py
python3 target/qualification/rom64-current-game/audit-cleanup.py --offline \
  --dump-sha256 82d57afb78995008f4d5c333ab558a4f6b56bdff0c3fdce25c7d60baa2f1bdb9
```

The frozen source and original source-bound binaries were independently hashed before and after their runs. Binary hashes identify the executed artifacts; offline observation replay does not require those executables. Fresh builds need the pinned Cargo dependencies and original optimization/assertion settings retained in the runner scripts.
