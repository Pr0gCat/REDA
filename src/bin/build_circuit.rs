//! Compiles one of the project's reference circuits and writes it to a
//! `.litematic` file that can be pasted straight into Minecraft via
//! Litematica.
//!
//! Run with `cargo run --bin build_circuit -- <name>` from the repo root, or
//! with no argument to list the available circuits along with their gate
//! counts and bounding boxes. The output path is relative to the current
//! working directory, so running it from anywhere else writes `output/`
//! there instead.
//!
//! The full seven-segment decoder is 232x5x298 -- slow to render in
//! browser-based litematic viewers, which have to build a mesh over that
//! whole grid. The smaller circuits listed here are a size ladder meant to
//! open quickly in the same kind of viewer.

use std::path::Path;

use reda::circuits::{and4, full_adder, seven_segment};
use reda::compile::{compile, Netlist};
use reda::formats::litematic;
use reda::redstone::world::block::BlockKind;
use reda::redstone::world::storage::World;

/// One selectable reference circuit: a name for the CLI, and a way to build
/// its netlist fresh each time it is needed.
struct CircuitInfo {
    name: &'static str,
    build: fn() -> Netlist,
}

fn available_circuits() -> Vec<CircuitInfo> {
    vec![
        CircuitInfo { name: "and4", build: || and4::build_and4_netlist().0 },
        CircuitInfo { name: "full_adder", build: || full_adder::build_full_adder_netlist().0 },
        CircuitInfo { name: "segment_a", build: || seven_segment::build_single_segment_netlist(0).0 },
        CircuitInfo { name: "seven_segment", build: || seven_segment::build_seven_segment_netlist().0 },
    ]
}

fn count_non_air(world: &World) -> usize {
    let (size_x, size_y, size_z) = world.size();
    let mut count = 0usize;
    for x in 0..size_x {
        for y in 0..size_y {
            for z in 0..size_z {
                if world.get(x, y, z).kind != BlockKind::Air {
                    count += 1;
                }
            }
        }
    }
    count
}

fn list_circuits(circuits: &[CircuitInfo]) {
    println!("Usage: build_circuit <name>");
    println!();
    println!("Available circuits:");
    for info in circuits {
        let name = info.name;
        let netlist = (info.build)();
        let gate_count = netlist.gates.len();
        let compiled =
            compile(&netlist).unwrap_or_else(|err| panic!("circuit '{name}' failed to compile: {err:?}"));
        let (size_x, size_y, size_z) = compiled.world.size();
        println!("  {name:<14} {gate_count:>4} gates   {size_x}x{size_y}x{size_z}");
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let circuits = available_circuits();

    let Some(requested_name) = args.get(1) else {
        list_circuits(&circuits);
        return;
    };

    let Some(info) = circuits.iter().find(|c| c.name == requested_name.as_str()) else {
        eprintln!("unknown circuit '{requested_name}'");
        eprintln!();
        list_circuits(&circuits);
        std::process::exit(1);
    };
    let name = info.name;

    let netlist = (info.build)();
    let gate_count = netlist.gates.len();

    let compiled = compile(&netlist).expect("reference circuits are acyclic and fully driven");
    let (size_x, size_y, size_z) = compiled.world.size();
    let non_air_blocks = count_non_air(&compiled.world);

    let output_dir = Path::new("output");
    std::fs::create_dir_all(output_dir).expect("failed to create the output directory");
    let output_path = output_dir.join(format!("{name}.litematic"));

    litematic::save(&output_path, &compiled.world, name).expect("failed to write the litematic file");

    println!("circuit: {name}");
    println!("bounding box: {size_x} x {size_y} x {size_z}");
    println!("non-air blocks: {non_air_blocks}");
    println!("gate count: {gate_count}");
    println!("wrote {}", output_path.display());
}
