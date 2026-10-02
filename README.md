# REDA - Redstone EDA

A compiler from logic to working Minecraft redstone, with a simulator honest
enough to prove the result before anyone pastes it into a world.

Verilog goes in; a `.litematic` comes out; the project's own simulator runs the
compiled circuit's full truth table, and a conformance harness re-runs it inside
a real Minecraft Java 26.2 server. The premise is that redstone's cost model is not CMOS's --
fan-in barely affects delay, an OR is a wire join and costs nothing, and delay is
overwhelmingly a placement problem -- so the interesting work is a technology
library and a cost model that say what redstone actually costs.

## The pipeline

```
Verilog
  |  src/frontend        native first (auto); unsupported features fall back
  |                      to yosys + abc via yowasp-yosys.
gate-level Netlist       $_AND_ $_NAND_ $_XOR_ $_MUX_ ... carried in Gate::kind
  |  src/compile/lowering
  |                      applies src/compile/topology's own recipes.
  |                      An explicit pass: compile() refuses to do it for you.
realisable Netlist       NOR(1..=3) and wire merges, nothing else
  |  src/compile         floorplan, route, emit, then four invariants
World                    -> .litematic, -> the simulator, -> a real server
```

`src/redstone` is the simulator and the block model it needs; `src/compile` is
the placer and router; `src/formats` writes the schematic. The four invariants
(spacing, connectivity, torch merge, signal strength) run unconditionally inside
`compile()` and are constraints, not cost terms: a layout that violates one is
illegal, not expensive.

The default compiler also tries deterministic, certified route-refresh reduction
on combinational boundaries with one to six inputs. It accepts only complete
retained-history truth and nonregressing setup/target timing, glitch and pulse
metrics. See [measured seven-segment and ROM improvements](docs/generator-qualified-refresh-reduction-2026-10-02.md).
Legacy compatibility routing now preserves caller-owned refresh reserves;
[validation and rejected experiments](docs/generator-legacy-refresh-policy-2026-10-02.md)
record unchanged default waves and the remaining root failures.

## The circuits

Four hand-written reference circuits, built gate by gate, plus the same two
functions written in Verilog and synthesised. `verilog:and4` and `and4` compute
the same thing and are entirely different circuits, which is the point.

Measured at the hybrid switchover with `cargo test --release --test
reference_circuits -- --nocapture` and `cargo test --release --test
verilog_frontend -- --nocapture` (settle is worst-case game ticks over a full
input sweep; the bounding box is the occupied extent, not the world the
compiler allocates around it). **`path` is which of `compile`'s two placers
produced the row** -- see "Two placers, one entry point" below:

| circuit | gates | blocks | settle | bounding box | path |
|---|---|---|---|---|---|
| and4 | 7 | 232 | 14 | 45x4x25 | relaxation |
| full_adder | 22 | 1065 | 46 | 35x5x107 | relaxation |
| segment_a | 46 | 6416 | 68 | 137x6x182 | emitter |
| seven_segment | 84 | 16244 | 98 | 219x6x251 | emitter |
| verilog:and4 | 9 | 290 | 14 | 50x4x29 | relaxation |
| verilog:seven_segment | 47 | 10088 | 80 | 151x6x236 | emitter |

The hand-written four are pinned by
`the_hand_written_circuits_keep_their_measured_size`: no lowering touches pure
NOR, so a change there means something moved that should not have. Which path
each takes is pinned separately, by
`every_reference_circuit_records_which_path_produced_it`.

The table above records the historical hybrid switchover. The 2026-09-30
channel compaction measurements, including native frontend circuits, are in
[the generator optimization report](docs/generator-optimization-2026-09-30.md).
The subsequent shared-track compaction is measured in
[the track-sharing report](docs/generator-track-sharing-2026-09-30.md).
Feed-through target refinement and its measured tradeoff are in
[the feed-through report](docs/generator-feedthrough-2026-09-30.md).
The subsequent branch strength budgeting refinement is measured in
[the tap reserve report](docs/generator-tap-reserve-2026-09-30.md).
Centered row ordering on that baseline is measured in
[the centered ordering report](docs/generator-centered-ordering-2026-09-30.md).

### Two placers, one entry point

`compile` tries relaxation placement with A* routing first, and falls back to
the row/channel/track emitter on any failure -- placement, routing or
verification. Channel emission tries a verified compact layout before its
historical layout; `compile_legacy` retains the historical baseline. The same
netlist always takes the same path; nothing here reads a
clock or a random number. `CompiledCircuit::planner_kind` records which one ran,
so a circuit that quietly stops taking the better placer is visible rather than
invisible.

What that bought, against the emitter's own numbers:

| circuit | blocks | settle |
|---|---|---|
| and4 | 472 -> **232** | 18 -> **14** |
| full_adder | 1,784 -> **1,065** | 42 -> **46** |
| verilog:and4 | 480 -> **290** | 22 -> **14** |
| the other three | unchanged -- they fall back | unchanged |

**`full_adder` settles four game ticks slower** at 40% fewer blocks. Recorded
rather than smoothed over: the relaxation is placed for wirelength, and nothing
on this path weights a spring by criticality yet.

Two other things moved earlier and neither was recorded at the time. Every
settle time fell, hand-written circuits included, when the router learnt to
terminate a legal gate input with directed dust instead of a repeater -- `and4`
24 to 18, `seven_segment` 112 to 98. And the Verilog decoder went from 56 gates
and 12,348 blocks to 47 and 10,088 when lowering began choosing gate polarities
globally.

The two Verilog rows are the only ones a tool chooses the structure of, so they
are the only ones that move. They are still short of the 31 gates, 7,888 blocks
and 82 ticks that `docs/superpowers/specs/2026-08-09-polarity-assignment.md`
set as the target -- that spec's own reckoning of how far it got is at its
end.

## Running it

With Rust (including clippy, rustfmt and the `wasm32-unknown-unknown` target),
wasm-pack and Node.js installed, create the Python environment once:

```sh
uv venv .venv
uv pip install --python .venv/bin/python -r requirements.txt
```

In each Bash session, run `source tools/env.sh` from the repo root. This selects
the Python environment and keeps Cargo build output and YoWASP's cache in this
checkout's `target/`. It also places scratch files there so YoWASP's private
`/tmp` mount does not hide the Verilog frontend's inputs and outputs on Linux.

```sh
./check.sh                                        # everything that must be true
cargo run --release --bin build_circuit           # list circuits
cargo run --release --bin build_circuit -- seven_segment      # -> output/*.litematic
cargo run --release --bin mc_dump -- verilog:and4             # text dump for the harness
```

The `verilog:` circuits need `python` with `yowasp-yosys` (`pip install -r
requirements.txt`); everything else needs only cargo. `viewer/` is a separate
crate: a browser page that runs this simulator compiled to wasm and draws the
world, in 3D, and as a graph of primitives -- see `viewer/README.md`.
`conformance/` drives a real Minecraft server over RCON; see
`docs/minecraft-server.md`.

## The written record

`docs/superpowers/specs/` is one dated document per decision, each with the
measurements that drove it. They are arguments made at a date, not a manual, and
they are not rewritten when the code moves past them -- a stale fact inside one
gets marked, the reasoning stays as it was. Start with
`2026-08-05-redstone-eda-design.md` for the founding thesis and
`2026-08-09-polarity-assignment.md` for what is open now.

### CLI 前端選擇

原生前端亦支援 `reg`、`always @*`／`always @(*)`、現有正緣／enable `always` 形式與串接連續賦值目標。標準七段 `.v` 可無 Yosys 生成；支援範圍及完整驗證見[原生 legacy HDL](docs/native-legacy-hdl-2026-10-02.md)。

`build_circuit` 與 `mc_dump` 的 HDL 輸入預設使用 `--frontend auto`：先嘗試原生編譯器，只有明確分類為不支援的功能才回退 Yosys。語法、語意、內部驗證與後續實體編譯錯誤會直接失敗。`--frontend native` 禁止回退；`--frontend yosys` 強制使用 Yosys。實際前端與回退原因輸出到 stderr。

```bash
source tools/env.sh
cargo run --release --bin mc_dump -- verilog tests/fixtures/and4.v and4 --frontend auto
cargo run --release --bin build_circuit -- verilog tests/fixtures/seven_segment.sv seven_segment --frontend native
```

舊的 `synthesize_verilog` API 與烘焙流程仍固定使用 Yosys，供獨立基準比較。此 CLI 模式不代表已完成正式生產切換的所有驗收。

原生前端新增整數 `parameter`／`localparam` 預設值、相依常數運算與寬度／索引解析；驗證及限制見[整數參數支援](docs/native-integer-parameters-2026-10-02.md)。

原生前端支援巢狀模組實例、具名／位置連接與整數參數覆寫；完整七段與既有 DFF 的實體驗證見[模組實例支援](docs/native-module-instances-2026-10-02.md)。

Reference partition checks validate both original worlds and selected certified graphs.
[Current root convergence evidence](docs/world-partition-selected-graph-2026-10-02.md)
records 17 remaining failures; the full project is not yet green.
