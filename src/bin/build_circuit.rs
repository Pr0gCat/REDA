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
//! The full seven-segment decoder is 232x7x257 -- slow to render in
//! browser-based litematic viewers, which have to build a mesh over that
//! whole grid. The smaller circuits listed here are a size ladder meant to
//! open quickly in the same kind of viewer.
//!
//! `<name>` also accepts the Verilog sources this project ships, under their
//! `verilog:`-prefixed names (`verilog:and4`, `verilog:seven_segment`), and
//! there is a `build_circuit verilog <file.v> <top-module>` form for one it
//! does not -- the same three selection forms `mc_dump` has, for the same
//! reason: the synthesised circuits are the ones this project quotes progress
//! from, and being able to look at them in a viewer or paste them into a
//! world should not be the one thing only the hand-written ones can do. See
//! `reda::circuits::verilog` for why they are a separate catalog.
//!
//! They are deliberately *not* in the no-argument listing's size table,
//! though -- that table compiles every circuit it prints, and doing that here
//! would turn `build_circuit` with no arguments into a multi-second Yosys run
//! that fails outright on a machine with no `python`/`yowasp-yosys`. They are
//! listed by name only.

use std::io::Write;
use std::path::Path;

use reda::circuits::{and4, full_adder, seven_segment, verilog};
use reda::compile::lowering::{lower, lower_optimised};
use reda::compile::{compile, CompiledCircuit, Netlist};
use reda::formats::litematic;
use reda::redstone::world::block::BlockKind;
use reda::redstone::world::storage::World;

struct CircuitInfo {
    name: &'static str,
    build: fn() -> Netlist,
    /// `(label, internal signal name)` pairs, in the order they should be
    /// printed.
    output_labels: fn() -> Vec<(String, String)>,
}

fn available_circuits() -> Vec<CircuitInfo> {
    vec![
        CircuitInfo {
            name: "and4",
            build: || and4::build_and4_netlist().0,
            output_labels: || vec![(and4::OUTPUT_NAME.to_string(), and4::build_and4_netlist().1)],
        },
        CircuitInfo {
            name: "full_adder",
            build: || full_adder::build_full_adder_netlist().0,
            output_labels: || {
                let (_, signal_of) = full_adder::build_full_adder_netlist();
                full_adder::OUTPUT_NAMES
                    .iter()
                    .map(|&label| (label.to_string(), signal_of[label].clone()))
                    .collect()
            },
        },
        CircuitInfo {
            name: "segment_a",
            build: || seven_segment::build_single_segment_netlist(0).0,
            // Segment index 0 is "a" in `seven_segment::SEGMENT_NAMES`.
            output_labels: || {
                vec![(
                    seven_segment::SEGMENT_NAMES[0].to_string(),
                    seven_segment::build_single_segment_netlist(0).1,
                )]
            },
        },
        CircuitInfo {
            name: "seven_segment",
            build: || seven_segment::build_seven_segment_netlist().0,
            output_labels: || {
                let (_, signal_of) = seven_segment::build_seven_segment_netlist();
                seven_segment::SEGMENT_NAMES
                    .iter()
                    .map(|&label| (label.to_string(), signal_of[label].clone()))
                    .collect()
            },
        },
    ]
}

/// A circuit selected off the command line: everything `main` needs, with no
/// trace of which selection form produced it. The `name` doubles as the
/// output file's stem, so it must stay filesystem-safe -- which is why the
/// ad-hoc file form below names its output after the top module rather than
/// after the path it came from.
struct SelectedCircuit {
    name: String,
    netlist: Netlist,
    output_labels: Vec<(String, String)>,
    lowering_path: LoweringPath,
}

/// The lowering contract selected by the source circuit.  Hand-written
/// reference netlists use the compatibility path; a synthesized netlist is
/// where the whole-netlist polarity optimizer belongs.  Keep this beside the
/// selected netlist so the litematic binary cannot silently diverge from
/// `mc_dump`.
#[derive(Clone, Copy)]
enum LoweringPath {
    Ordinary,
    Optimised,
}

/// Turn the command line into a circuit, or into a message explaining why it
/// could not be one. Only the `verilog` forms can fail -- see
/// `reda::circuits::verilog`, and `mc_dump`'s identical `select`.
fn select(
    args: &[String],
    circuits: &[CircuitInfo],
    mode: reda::frontend::FrontendMode,
) -> Result<SelectedCircuit, String> {
    if args.first().map(String::as_str) == Some("verilog") {
        let (Some(path), Some(top_module)) = (args.get(1), args.get(2)) else {
            return Err("usage: build_circuit verilog <file.v> <top-module>".to_string());
        };
        let (netlist, output_labels) = std::fs::read_to_string(Path::new(path))
            .map_err(|err| err.to_string())
            .and_then(|source| verilog::synthesize_with_frontend(&source, path, top_module, mode))
            .map_err(|err| format!("could not synthesize {path} ({top_module}): {err}"))?;
        return Ok(SelectedCircuit {
            name: top_module.clone(),
            netlist,
            output_labels,
            lowering_path: LoweringPath::Optimised,
        });
    }

    let requested = args
        .first()
        .expect("select is only called with at least one argument");

    if let Some(circuit) = verilog::find(requested) {
        let (netlist, output_labels) = verilog::synthesize_with_frontend(
            circuit.source,
            circuit.source_path,
            circuit.top_module,
            mode,
        )
        .map_err(|err| format!("could not synthesize '{}': {err}", circuit.name))?;
        // `verilog:seven_segment` would be a colon in a filename -- illegal
        // on Windows, and an alternate-data-stream separator at that. The
        // written file is `verilog_seven_segment.litematic`.
        return Ok(SelectedCircuit {
            name: circuit.name.replace(':', "_"),
            netlist,
            output_labels,
            lowering_path: LoweringPath::Optimised,
        });
    }

    if let Some(info) = circuits.iter().find(|c| c.name == requested.as_str()) {
        return Ok(SelectedCircuit {
            name: info.name.to_string(),
            netlist: (info.build)(),
            output_labels: (info.output_labels)(),
            lowering_path: LoweringPath::Ordinary,
        });
    }

    Err(format!("unknown circuit '{requested}'"))
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

/// Each public output label with its signal and lamp, in label order.
fn output_pinout<'a>(
    compiled: &CompiledCircuit,
    labels: &'a [(String, String)],
) -> Vec<(&'a str, &'a str, (i32, i32, i32))> {
    labels
        .iter()
        .map(|(label, signal)| (label.as_str(), signal.as_str(), compiled.output_positions[signal]))
        .collect()
}

/// Print every input lever's and output lamp's coordinate, schematic-local:
/// the (x, y, z) the `.litematic` itself uses, from the corner it is pasted at.
fn print_pinout(compiled: &CompiledCircuit, output_labels: &[(String, String)]) {
    println!();
    println!(
        "pinout (schematic-local coordinates: x,y,z from the corner the .litematic is pasted at)"
    );
    println!("  inputs (drive with a lever or any source at):");
    for (name, &(x, y, z)) in &compiled.input_positions {
        println!("    {name:<12} ({x}, {y}, {z})");
    }
    println!("  outputs (read with a lamp or any reader at):");
    for (label, _, (x, y, z)) in output_pinout(compiled, output_labels) {
        println!("    {label:<12} ({x}, {y}, {z})");
    }
}

fn list_circuits(circuits: &[CircuitInfo]) {
    println!("HDL frontend: --frontend auto|native|yosys (default auto)");
    println!("Usage: build_circuit <name>");
    println!("       build_circuit verilog <file.v> <top-module>");
    println!();
    println!("Available circuits:");
    for info in circuits {
        let name = info.name;
        let netlist = (info.build)();
        let gate_count = netlist.gates.len();
        let compiled = compile(&netlist)
            .unwrap_or_else(|err| panic!("circuit '{name}' failed to compile: {err}"));
        let (size_x, size_y, size_z) = compiled.world.size();
        println!("  {name:<14} {gate_count:>4} gates   {size_x}x{size_y}x{size_z}");
    }
    // No gate count or bounding box here: printing those means synthesizing,
    // and synthesizing means a Yosys run per entry -- see this file's module
    // doc comment for why a listing must not do that.
    println!();
    println!("Verilog circuits (--frontend auto|native|yosys; auto prefers native):");
    for circuit in verilog::CIRCUITS {
        println!("  {:<22} module {}", circuit.name, circuit.top_module);
    }
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mode = reda::frontend::FrontendMode::take_argument(&mut args).unwrap_or_else(|why| {
        eprintln!("{why}");
        std::process::exit(1);
    });
    let circuits = available_circuits();
    if args.is_empty() {
        list_circuits(&circuits);
        return;
    }
    let SelectedCircuit {
        name,
        netlist,
        output_labels,
        lowering_path,
    } = match select(&args, &circuits, mode) {
        Ok(selected) => selected,
        Err(message) => {
            eprintln!("{message}");
            eprintln!();
            list_circuits(&circuits);
            std::process::exit(1);
        }
    };
    // Lower first: `compile` takes only realisable gates, and a synthesized
    // netlist arrives at the gate level. This is the identity on every
    // hand-written circuit, so `gate_count` below reports what is placed.
    let netlist = match match lowering_path {
        LoweringPath::Ordinary => lower(&netlist),
        LoweringPath::Optimised => lower_optimised(&netlist),
    } {
        Ok(lowered) => lowered,
        Err(err) => {
            eprintln!("circuit '{name}' could not be lowered into redstone: {err}");
            std::process::exit(1);
        }
    };
    let gate_count = netlist.gates.len();
    let compiled = compile(&netlist).unwrap_or_else(|err| {
        eprintln!("circuit '{name}' failed to compile: {err}");
        std::process::exit(1);
    });
    let (size_x, size_y, size_z) = compiled.world.size();
    let non_air_blocks = count_non_air(&compiled.world);

    let output_dir = Path::new("output");
    std::fs::create_dir_all(output_dir).expect("failed to create the output directory");
    let output_path = output_dir.join(format!("{name}.litematic"));
    litematic::save(&output_path, &compiled.world, &name)
        .expect("failed to write the litematic file");

    // A plain-text block dump beside the litematic, one non-air block per
    // line -- `x y z kind facing lit power` -- for viewers that are not
    // Minecraft.
    {
        let dump_path = output_dir.join(format!("{name}.blocks.txt"));
        let mut dump = std::fs::File::create(&dump_path).expect("failed to create the dump");
        for x in 0..size_x {
            for y in 0..size_y {
                for z in 0..size_z {
                    let state = compiled.world.get(x, y, z);
                    if state.kind == BlockKind::Air {
                        continue;
                    }
                    writeln!(
                        dump,
                        "{x} {y} {z} {:?} {} {} {}",
                        state.kind,
                        state
                            .facing
                            .map(|facing| format!("{facing:?}"))
                            .unwrap_or_else(|| "-".to_string()),
                        u8::from(state.lit),
                        state.power
                    )
                    .expect("failed to write the dump");
                }
            }
        }
        println!("wrote {}", dump_path.display());
    }

    // The pinout as JSON beside the litematic: the viewer's baked-circuit
    // loader needs the lever and lamp coordinates, and the schematic file
    // does not carry names.
    {
        let json_path = output_dir.join(format!("{name}.pinout.json"));
        let entry = |key: &str, (x, y, z): (i32, i32, i32)| format!("\"{key}\":[{x},{y},{z}]");
        let inputs = compiled
            .input_positions
            .iter()
            .map(|(name, &position)| entry(name, position))
            .collect::<Vec<_>>()
            .join(",");
        let pinout = output_pinout(&compiled, &output_labels);
        let outputs = pinout
            .iter()
            .map(|&(label, _, position)| entry(label, position))
            .collect::<Vec<_>>()
            .join(",");
        let labels = pinout
            .iter()
            .map(|&(label, _, _)| format!("\"{label}\":\"{label}\""))
            .collect::<Vec<_>>()
            .join(",");
        std::fs::write(
            &json_path,
            format!("{{\"inputs\":{{{inputs}}},\"outputs\":{{{outputs}}},\"output_labels\":{{{labels}}}}}"),
        )
        .expect("failed to write the pinout json");
        println!("wrote {}", json_path.display());
    }

    println!("circuit: {name}");
    println!("bounding box: {size_x} x {size_y} x {size_z}");
    println!("non-air blocks: {non_air_blocks}");
    println!("gate count: {gate_count}");
    println!("wrote {}", output_path.display());

    print_pinout(&compiled, &output_labels);
}
