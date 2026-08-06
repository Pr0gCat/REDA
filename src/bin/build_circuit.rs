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
use reda::compile::{compile, CompiledCircuit, Netlist};
use reda::formats::litematic;
use reda::redstone::world::block::BlockKind;
use reda::redstone::world::storage::World;

/// One selectable reference circuit: a name for the CLI, a way to build its
/// netlist fresh each time it is needed, and a way to translate its outputs'
/// internal (auto-generated) signal names into the human-meaningful labels
/// used elsewhere in this project ("sum", "cout", the segment letters) --
/// `Netlist::outputs` only carries the internal names, since that is what
/// `NetlistBuilder` hands back.
struct CircuitInfo {
    name: &'static str,
    build: fn() -> Netlist,
    /// `(label, internal signal name)` pairs, in the order they should be
    /// printed.
    output_labels: fn() -> Vec<(&'static str, String)>,
}

fn available_circuits() -> Vec<CircuitInfo> {
    vec![
        CircuitInfo {
            name: "and4",
            build: || and4::build_and4_netlist().0,
            // and4 has one, unnamed output -- "y" is just a conventional
            // single-output label, not a name the circuit itself carries.
            output_labels: || vec![("y", and4::build_and4_netlist().1)],
        },
        CircuitInfo {
            name: "full_adder",
            build: || full_adder::build_full_adder_netlist().0,
            output_labels: || {
                let (_, signal_of) = full_adder::build_full_adder_netlist();
                full_adder::OUTPUT_NAMES
                    .iter()
                    .map(|&label| (label, signal_of[label].clone()))
                    .collect()
            },
        },
        CircuitInfo {
            name: "segment_a",
            build: || seven_segment::build_single_segment_netlist(0).0,
            // Segment index 0 is "a" in `seven_segment::SEGMENT_NAMES`.
            output_labels: || vec![("a", seven_segment::build_single_segment_netlist(0).1)],
        },
        CircuitInfo {
            name: "seven_segment",
            build: || seven_segment::build_seven_segment_netlist().0,
            output_labels: || {
                let (_, signal_of) = seven_segment::build_seven_segment_netlist();
                seven_segment::SEGMENT_NAMES
                    .iter()
                    .map(|&label| (label, signal_of[label].clone()))
                    .collect()
            },
        },
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

/// Print every input lever's and output lamp's coordinate, so a player who
/// just pasted the schematic can tell which lever is which signal without
/// having to read the source.
///
/// Coordinates are schematic-local: the same (x, y, z) the `.litematic` file
/// itself uses, with the origin at the corner of the pasted structure --
/// whichever corner the player's paste tool anchors on. They are not
/// absolute world coordinates; the player has to add their own paste
/// position to get those.
///
/// `output_labels` translates each output's internal signal name (the only
/// name `compiled.output_positions` knows about) to the human label this
/// circuit is documented with, and fixes the print order -- `sum` before
/// `cout`, `a` before `g`, rather than whatever order the internal names
/// happen to sort into.
fn print_pinout(compiled: &CompiledCircuit, output_labels: &[(&str, String)]) {
    println!();
    println!("pinout (schematic-local coordinates: x,y,z from the corner the .litematic is pasted at)");
    println!("  inputs (lever):");
    for (name, (x, y, z)) in &compiled.input_positions {
        println!("    {name:<12} ({x}, {y}, {z})");
    }
    println!("  outputs (lamp):");
    for (label, signal) in output_labels {
        let (x, y, z) = compiled.output_positions[signal];
        println!("    {label:<12} ({x}, {y}, {z})");
    }
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

    print_pinout(&compiled, &(info.output_labels)());
}
