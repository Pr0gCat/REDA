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
  |  src/frontend        yosys + abc, via yowasp-yosys. ABC optimises logic
  |                      and stops; it does no technology mapping.
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

## The circuits

Four hand-written reference circuits, built gate by gate, plus the same two
functions written in Verilog and synthesised. `verilog:and4` and `and4` compute
the same thing and are entirely different circuits, which is the point.

Measured at `0c2c710` with `cargo test --release --test reference_circuits --
--nocapture` and `cargo test --release --test verilog_frontend -- --nocapture`
(settle is worst-case game ticks over a full input sweep; the bounding box is
the occupied extent, not the world the compiler allocates around it):

| circuit | gates | blocks | settle | bounding box |
|---|---|---|---|---|
| and4 | 7 | 472 | 18 | 49x4x47 |
| full_adder | 22 | 1784 | 42 | 53x6x125 |
| segment_a | 46 | 6416 | 68 | 137x6x182 |
| seven_segment | 84 | 16244 | 98 | 219x6x251 |
| verilog:and4 | 9 | 480 | 22 | 47x4x47 |
| verilog:seven_segment | 47 | 10088 | 80 | 151x6x236 |

The hand-written four are pinned by
`the_hand_written_circuits_keep_their_measured_size`: no lowering touches pure
NOR, so a change there means something moved that should not have.

Two things moved since the last table and neither was recorded at the time.
Every settle time fell, hand-written circuits included, when the router learnt
to terminate a legal gate input with directed dust instead of a repeater --
`and4` 24 to 18, `seven_segment` 112 to 98. And the Verilog decoder went from
56 gates and 12,348 blocks to 47 and 10,088 when lowering began choosing gate
polarities globally.

The two Verilog rows are the only ones a tool chooses the structure of, so they
are the only ones that move. They are still short of the 31 gates, 7,888 blocks
and 82 ticks that `docs/superpowers/specs/2026-08-09-polarity-assignment.md`
set as the target -- that spec's own reckoning of how far it got is at its
end.

## Running it

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
