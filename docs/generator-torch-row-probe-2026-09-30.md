# Side-torch row physical experiment

Status: geometry evidence only; not an accepted generator optimization.

Problem: generic gates and independently routed terms obscure opportunities to
share ROM word-selection rails and output columns. The proposed general
primitive uses an active-low upper dust rail on solid blocks at y=2, programmed
wall torches on their south sides at y=2, and shared output dust at y=1.
Rows and output columns have pitch 3. No case-name recognition is involved.

The single-row probe checks populated and empty output columns through four
input states. The multi-row probe programs three distinct three-bit words
(101, 110, 011), exercises all eight enable masks and all 64 transitions,
with another 64 setup transitions in persistent simulator history. This
includes multi-enabled states, beyond the intended one-hot ROM protocol.
Every settled output and the far end of each control rail is checked.

Fast persistent history: correct settled truth, no control-rail backflow,
worst settling 32 game ticks, 165 observed output changes, 12 column transitions
with more than one change. The zero-pulse hypothesis was falsified. The test
freezes these diagnostic results; passing it is NOT quality acceptance.

Diagnostic intervention: advance 61 game ticks before each applied mask,
longer than the simulator's 60-tick torch burnout window. Same geometry and
mask sequence: worst settling 2 game ticks, 141 observed changes, zero extra
pulses. This is consistent with burnout contributing to the stress failures.
It does not establish unrestricted speed, and holding is not a proposed
acceptance relaxation or a change to the simulator.

Remaining work: an actual address decoder, transition hazards with that
decoder, signal restoration for longer rows/columns, dynamic MUX data inputs,
production compilation integration, fixed existing quality benchmarks and
repeat-compilation determinism. No production generator code changed.

Verification: `cargo test --release --test torch_row -- --nocapture`, three
physical simulator tests passed. These do not certify Minecraft behavior.
The existing unrelated packing.rs unused_mut warning remains.

## Captured output

```text

running 3 tests
test programmed_torch_row_drives_separate_outputs_below ... ok
pulse: from=2, to=2, applied=2, x=9, changes=[Observation { tick: 94, position: Position { x: 9, y: 1, z: 11 }, label: "bit-9", value: false }, Observation { tick: 124, position: Position { x: 9, y: 1, z: 11 }, label: "bit-9", value: true }]
pulse: from=2, to=6, applied=2, x=6, changes=[Observation { tick: 136, position: Position { x: 6, y: 1, z: 11 }, label: "bit-6", value: false }, Observation { tick: 140, position: Position { x: 6, y: 1, z: 11 }, label: "bit-6", value: true }]
pulse: from=2, to=6, applied=2, x=9, changes=[Observation { tick: 136, position: Position { x: 9, y: 1, z: 11 }, label: "bit-9", value: false }, Observation { tick: 140, position: Position { x: 9, y: 1, z: 11 }, label: "bit-9", value: true }]
pulse: from=3, to=5, applied=3, x=3, changes=[Observation { tick: 164, position: Position { x: 3, y: 1, z: 11 }, label: "bit-3", value: false }, Observation { tick: 186, position: Position { x: 3, y: 1, z: 11 }, label: "bit-3", value: true }]
pulse: from=3, to=5, applied=3, x=6, changes=[Observation { tick: 164, position: Position { x: 6, y: 1, z: 11 }, label: "bit-6", value: false }, Observation { tick: 184, position: Position { x: 6, y: 1, z: 11 }, label: "bit-6", value: true }]
pulse: from=3, to=5, applied=5, x=6, changes=[Observation { tick: 188, position: Position { x: 6, y: 1, z: 11 }, label: "bit-6", value: false }, Observation { tick: 190, position: Position { x: 6, y: 1, z: 11 }, label: "bit-6", value: true }]
pulse: from=4, to=2, applied=4, x=3, changes=[Observation { tick: 212, position: Position { x: 3, y: 1, z: 11 }, label: "bit-3", value: false }, Observation { tick: 222, position: Position { x: 3, y: 1, z: 11 }, label: "bit-3", value: true }]
pulse: from=4, to=3, applied=4, x=6, changes=[Observation { tick: 226, position: Position { x: 6, y: 1, z: 11 }, label: "bit-6", value: false }, Observation { tick: 250, position: Position { x: 6, y: 1, z: 11 }, label: "bit-6", value: true }]
pulse: from=5, to=3, applied=5, x=6, changes=[Observation { tick: 278, position: Position { x: 6, y: 1, z: 11 }, label: "bit-6", value: false }, Observation { tick: 282, position: Position { x: 6, y: 1, z: 11 }, label: "bit-6", value: true }]
pulse: from=5, to=4, applied=5, x=6, changes=[Observation { tick: 286, position: Position { x: 6, y: 1, z: 11 }, label: "bit-6", value: false }, Observation { tick: 310, position: Position { x: 6, y: 1, z: 11 }, label: "bit-6", value: true }]
pulse: from=6, to=2, applied=6, x=9, changes=[Observation { tick: 330, position: Position { x: 9, y: 1, z: 11 }, label: "bit-9", value: false }, Observation { tick: 336, position: Position { x: 9, y: 1, z: 11 }, label: "bit-9", value: true }]
pulse: from=6, to=6, applied=6, x=9, changes=[Observation { tick: 352, position: Position { x: 9, y: 1, z: 11 }, label: "bit-9", value: false }, Observation { tick: 376, position: Position { x: 9, y: 1, z: 11 }, label: "bit-9", value: true }]
matrix hold=0: 64 transitions plus 64 setup transitions; worst settling=32 game ticks; output changes=165; pulsed column transitions=12
test multiple_programmed_rows_share_output_columns_without_feedback ... ok
matrix hold=61: 64 transitions plus 64 setup transitions; worst settling=2 game ticks; output changes=141; pulsed column transitions=0
test held_rows_do_not_emit_extra_output_pulses ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.03s

```
