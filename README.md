# REDA - Redstone EDA

A compiler from logic to working Minecraft redstone, with a simulator honest
enough to prove the result before anyone pastes it into a world.

Verilog goes in; a `.litematic` comes out; the project's own simulator runs the
compiled circuit's full truth table, and a conformance harness re-runs it inside
a real 1.20.1 server. The premise is that redstone's cost model is not CMOS's --
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

Measured at `f70ef0e` with `cargo test --release --test reference_circuits --
--nocapture` and `cargo test --release --test verilog_frontend -- --nocapture`
(settle is worst-case game ticks over a full input sweep):

| circuit | gates | blocks | settle | bounding box |
|---|---|---|---|---|
| and4 | 7 | 472 | 24 | 66x5x53 |
| full_adder | 22 | 1784 | 62 | 68x7x131 |
| segment_a | 46 | 6416 | 82 | 148x7x188 |
| seven_segment | 84 | 16244 | 112 | 232x7x257 |
| verilog:and4 | 9 | 480 | 28 | 64x5x53 |
| verilog:seven_segment | 56 | 12348 | 88 | 162x7x259 |

The two Verilog rows are the only ones a tool chooses the structure of, so they
are the only ones that move. They are currently worse than they were before ABC
stopped technology-mapping, for a reason with a spec of its own:
`docs/superpowers/specs/2026-08-09-polarity-assignment.md`.

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
