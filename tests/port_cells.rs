//! A circuit's inputs and outputs are ports, not levers and lamps. Left
//! alone they go wherever suits the circuit; a caller can put each one on
//! an edge, or at an exact cell -- at any height -- in its own coordinates,
//! as an in-game tool marking where they go would.

use reda::compile::Side;
use reda::frontend::{compile_systemverilog_physical, CompileOptions, SourceInput};
use reda::redstone::{simulator::Simulator, world::block::BlockKind};

const FULL_ADDER: &str = "module full_adder(input a, input b, input cin, output s, output cout);
  assign s = a ^ b ^ cin;
  assign cout = (a & b) | (cin & (a ^ b));
endmodule";

fn build(options: &CompileOptions) -> reda::frontend::CompiledNativeCircuit {
    compile_systemverilog_physical(
        &[SourceInput {
            name: "full_adder.sv",
            text: FULL_ADDER,
        }],
        options,
    )
    .expect("compiles")
}

/// Every row of the truth table, driven by bench levers and read off bench
/// lamps.
fn check_truth_table(circuit: &reda::frontend::CompiledNativeCircuit) {
    let mut sim = Simulator::new(circuit.physical.bench.clone());
    sim.run_until_stable(4000).unwrap();
    for row in 0..8u32 {
        let value = |bit: u32| row & (1 << bit) != 0;
        for (bit, name) in ["a", "b", "cin"].into_iter().enumerate() {
            let [x, y, z] = circuit.pinout.inputs[name];
            let mut lever = sim.world().get(x, y, z).clone();
            assert_eq!(lever.kind, BlockKind::Lever, "the bench drives {name}");
            lever.lit = value(bit as u32);
            sim.world_mut().set(x, y, z, lever);
        }
        sim.run_until_stable(4000).unwrap();
        let ones = (0..3).filter(|&bit| value(bit)).count();
        for (name, expected) in [("s", ones % 2 == 1), ("cout", ones >= 2)] {
            let [x, y, z] = circuit.pinout.outputs[name];
            assert_eq!(sim.world().get(x, y, z).lit, expected, "{name} on row {row:03b}");
        }
    }
}

fn kinds(world: &reda::redstone::world::storage::World) -> Vec<BlockKind> {
    let (sx, sy, sz) = world.size();
    let mut kinds = Vec::new();
    for x in 0..sx {
        for y in 0..sy {
            for z in 0..sz {
                kinds.push(world.get(x, y, z).kind);
            }
        }
    }
    kinds
}

#[test]
fn the_circuit_has_ports_not_levers_or_lamps() {
    let circuit = build(&CompileOptions::new("full_adder"));
    let kinds = kinds(&circuit.physical.world);
    assert!(!kinds.contains(&BlockKind::Lever));
    assert!(!kinds.contains(&BlockKind::Lamp));
    check_truth_table(&circuit);
}

#[test]
fn ports_go_on_the_edges_named() {
    let options = CompileOptions::new("full_adder")
        .with_side("a", Side::West)
        .with_side("b", Side::West)
        .with_side("cin", Side::West)
        .with_side("s", Side::East)
        .with_side("cout", Side::East);
    let circuit = build(&options);
    let west = circuit.pinout.inputs.values().map(|at| at[0]).max().unwrap();
    let east = circuit.pinout.outputs.values().map(|at| at[0]).min().unwrap();
    assert!(west < east, "inputs west of outputs");
    let (size_x, _, _) = circuit.physical.world.size();
    assert!(circuit.pinout.inputs.values().all(|at| at[0] <= 4), "inputs at the west edge");
    assert!(circuit.pinout.outputs.values().all(|at| at[0] >= size_x - 5), "outputs at the east edge");
    check_truth_table(&circuit);
}

#[test]
fn ports_go_exactly_at_the_cells_given_at_any_height() {
    let cells = [
        ("a", (100, 64, 200)),
        ("b", (100, 64, 196)),
        ("cin", (100, 70, 190)),
        ("s", (160, 64, 200)),
        ("cout", (160, 67, 192)),
    ];
    let options = cells
        .iter()
        .fold(CompileOptions::new("full_adder"), |options, &(port, cell)| options.with_cell(port, cell));
    let circuit = build(&options);
    let (ox, oy, oz) = circuit.physical.origin;
    for (port, (x, y, z)) in cells {
        let [lx, ly, lz] = circuit
            .pinout
            .inputs
            .get(port)
            .or_else(|| circuit.pinout.outputs.get(port))
            .copied()
            .unwrap();
        assert_eq!((lx + ox, ly + oy, lz + oz), (x, y, z), "{port}");
    }
    let kinds = kinds(&circuit.physical.world);
    assert!(!kinds.contains(&BlockKind::Lever));
    assert!(!kinds.contains(&BlockKind::Lamp));
    check_truth_table(&circuit);
}

#[test]
fn a_cell_names_one_bit() {
    let error = compile_systemverilog_physical(
        &[SourceInput {
            name: "wide.sv",
            text: "module wide(input [1:0] a, output y); assign y = a[0] & a[1]; endmodule",
        }],
        &CompileOptions::new("wide").with_cell("a", (0, 64, 0)),
    )
    .expect_err("a whole vector port cannot sit at one cell");
    assert!(error.contains("a[0]"), "{error}");
}
