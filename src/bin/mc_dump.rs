//! Dumps a compiled reference circuit as a plain-text description the
//! conformance harness (`conformance/circuit_conformance.py`) can turn into
//! `/setblock` commands against a real Minecraft server.
//!
//! Deliberately NOT JSON: this crate does not depend on `serde_json`, and
//! adding it just for this one debug-adjacent tool is not worth a new
//! dependency. The format is one line per record, whitespace-separated,
//! easy to parse with `str.split()` on the Python side.
//!
//! Run with `cargo run --bin mc_dump -- <circuit-name>` from the repo root.
//!
//! `<circuit-name>` is either a hand-written reference circuit out of
//! `reda::circuits` (`and4`, `seven_segment`, ...) or one of the Verilog
//! sources this project ships, named with a `verilog:` prefix
//! (`verilog:and4`, `verilog:seven_segment`) and synthesized through
//! `reda::frontend`. There is also a third form, `mc_dump verilog <file.v>
//! <top-module>`, for a Verilog file this project does not ship.
//!
//! The prefix is not cosmetic. `verilog:seven_segment` and `seven_segment`
//! compute the same function but are entirely different circuits -- different
//! gates, different geometry -- so a conformance report has to be able to say
//! unambiguously which of the two it ran. See `reda::circuits::verilog` for
//! why they are a separate catalog rather than more entries in the list
//! below.
//!
//! Supported `verilog:` sources use the native frontend by default and need
//! no Python or Yosys. `--frontend yosys`, or an unsupported construct that
//! selects the optional auto fallback, needs Python with `yowasp-yosys`.
//! Missing optional tools produce the frontend's diagnostic rather than a
//! panic (see `FrontendError`'s `Display` implementation).
//!
//! Output line kinds:
//!
//!   SIZE   size_x size_y size_z
//!   BLOCK  x y z kind facing face lit delay name
//!   BLOCKPROP x y z property value
//!   INPUT  name x y z
//!   OUTPUT name x y z
//!   OUTLABEL label internal_name
//!   OUTPUTPORT ordinal source_name implemented_signal x y z [label]
//!   GATEOUT name x y z [component_kind]
//!   GATE   output_name input1,input2,... kind
//!   DFFCELL output_name role x y z component_kind
//!
//! `INPUT` and `OUTPUT` retain source boundary names. For a certified rewrite,
//! `OUTPUTPORT` preserves each source-ordered physical lamp and its actual signal,
//! including aliases. `OUTLABEL` maps source names to cosmetic public labels.
//!
//! `GATE` and `GATEOUT` describe actual implemented gates, with collision-free
//! internal labels. Implementation inputs use their proved source input names.
//! Rewritten gates are emitted in checked topological order; legacy records
//! retain their original order and spelling. The optional component kind lets
//! consumers read comparator, repeater, torch or dust without inferring geometry
//! from a logical gate kind. `BLOCKPROP` retains comparator mode.
//!
//! The conformance parser also accepts `dff_p`, observed at its slave repeater.
//! `DFFCELL` records expose the five rotated Design H sites; stateful truth
//! requires the separate sequential trajectory driver.
//! The combinational evaluator accepts `nor`, `merge`, `buf` and `andnot`; a `merge` gate is a
//! **wired OR**, not a NOR, so a consumer that assumes every gate inverts
//! computes the wrong expected value for that gate *and for everything
//! downstream of it*. Every hand-written reference circuit is pure NOR and
//! so emits nothing but `nor`; a `verilog:` circuit generally has both. A
//! merge also has no torch and no gate body at all -- just a dust junction
//! where its inputs' routes are allowed to touch -- so its `GATEOUT`
//! position holds `minecraft:redstone_wire`, and its logical value is "that
//! dust has non-zero power", not "a torch is lit". Both facts are the reason
//! this field exists rather than being inferrable.
//!
//! Ordinary gate-level Verilog is lowered before placement. A selected physical
//! rewrite can introduce its own certified Buf or comparator AndNot stages;
//! the emitted records describe those stages, never removed source cells.
//!
//! `input1,input2,...` is `-` when a gate has no inputs at all, so that
//! `kind` is always the same whitespace-separated field.
//!
//! `BLOCK`'s `facing`/`face` are the Minecraft-lowercase string or the
//! literal `-` when the block has no such property. `lit` is `true`/`false`
//! (used both for `lit` on torches/lamps and `powered` on repeaters/levers --
//! the Python side knows which property name each block kind actually
//! wants). `delay` is `0` for anything that is not a repeater. `name` is the
//! bare Minecraft block id (`minecraft:stone`), with no blockstate
//! properties attached -- the Python side builds the full `name[props]`
//! string itself, because *which* properties are legal to send and what
//! their resting/pre-trigger value should be is a placement-order decision
//! that belongs to the harness (see its module docstring), not to this dump.

use std::path::Path;

use reda::circuits::{and4, full_adder, seven_segment, verilog};
use reda::compile::lowering::{lower, lower_optimised};
use reda::compile::{compile, CompiledCircuit, Netlist};
use reda::redstone::world::block::{BlockKind, Face, Facing};
use reda::redstone::world::storage::World;

/// A named reference circuit: a builder plus a way to translate its outputs'
/// internal (auto-generated) signal names into human-meaningful labels
/// ("y", "sum"/"cout", "a".."g") -- mirrors `build_circuit.rs`'s
/// `CircuitInfo`, which solves the exact same naming problem for the
/// `.litematic` exporter. `Netlist::outputs` only ever carries the internal
/// names `NetlistBuilder` generated, so without this a 7-output circuit's
/// dump would report outputs named `g53`..`g61` instead of `a`..`g`.
struct NamedCircuit {
    name: &'static str,
    build: fn() -> Netlist,
    /// `(label, internal signal name)` pairs, in the order they should be
    /// printed.
    output_labels: fn() -> Vec<(String, String)>,
}

fn available_circuits() -> Vec<NamedCircuit> {
    vec![
        NamedCircuit {
            name: "and4",
            build: || and4::build_and4_netlist().0,
            output_labels: || vec![(and4::OUTPUT_NAME.to_string(), and4::build_and4_netlist().1)],
        },
        NamedCircuit {
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
        NamedCircuit {
            name: "segment_a",
            build: || seven_segment::build_single_segment_netlist(0).0,
            output_labels: || {
                vec![(
                    seven_segment::SEGMENT_NAMES[0].to_string(),
                    seven_segment::build_single_segment_netlist(0).1,
                )]
            },
        },
        NamedCircuit {
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

/// A circuit selected off the command line, with its netlist already built
/// (or synthesized) and its outputs already labelled -- everything `main`
/// needs after selection, with no trace of which of the three selection
/// forms produced it.
struct SelectedCircuit {
    name: String,
    netlist: Netlist,
    output_labels: Vec<(String, String)>,
    lowering_path: LoweringPath,
}

/// The lowering contract chosen by the circuit's source: hand-written
/// reference netlists retain the long-standing compatibility path, while a
/// synthesized netlist can use the whole-netlist polarity assignment.
#[derive(Clone, Copy)]
enum LoweringPath {
    Ordinary,
    Optimised,
}

fn usage(circuits: &[NamedCircuit]) -> String {
    let hand_written: Vec<&str> = circuits.iter().map(|c| c.name).collect();
    let synthesized: Vec<&str> = verilog::CIRCUITS.iter().map(|c| c.name).collect();
    format!(
        "usage: mc_dump <circuit-name>\n       mc_dump verilog <file.v> <top-module>\n\n\
         hand-written circuits: {}\n\
         verilog circuits:      {}\n\n\
         HDL uses --frontend auto|native|yosys (default auto); Yosys fallback needs `python` with \
         `yowasp-yosys` installed.",
        hand_written.join(", "),
        synthesized.join(", "),
    )
}

/// Turn the command line into a circuit, or into a message explaining why it
/// could not be one.
///
/// Both `verilog` forms can fail for reasons that are nobody's bug -- no
/// `python`, no `yowasp-yosys`, a Verilog file that does not parse -- so this
/// returns the frontend's own error text rather than unwrapping it. That is
/// the whole reason selection is a fallible function here and a plain lookup
/// in `build_circuit`'s older shape.
fn select(
    args: &[String],
    circuits: &[NamedCircuit],
    mode: reda::frontend::FrontendMode,
) -> Result<SelectedCircuit, String> {
    // `mc_dump verilog <file.v> <top-module>`: a Verilog source this project
    // does not ship. Spelled as separate arguments rather than folded into
    // the `verilog:` name syntax because a Windows path (`C:\...`) already
    // contains a colon.
    if args.first().map(String::as_str) == Some("verilog") {
        let (Some(path), Some(top_module)) = (args.get(1), args.get(2)) else {
            return Err("usage: mc_dump verilog <file.v> <top-module>".to_string());
        };
        let (netlist, output_labels) = std::fs::read_to_string(Path::new(path))
            .map_err(|err| err.to_string())
            .and_then(|source| verilog::synthesize_with_frontend(&source, path, top_module, mode))
            .map_err(|err| format!("could not synthesize {path} ({top_module}): {err}"))?;
        return Ok(SelectedCircuit {
            name: format!("verilog {path}:{top_module}"),
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
        return Ok(SelectedCircuit {
            name: circuit.name.to_string(),
            netlist,
            output_labels,
            lowering_path: LoweringPath::Optimised,
        });
    }

    if let Some(circuit) = circuits.iter().find(|c| c.name == requested.as_str()) {
        return Ok(SelectedCircuit {
            name: circuit.name.to_string(),
            netlist: (circuit.build)(),
            output_labels: (circuit.output_labels)(),
            lowering_path: LoweringPath::Ordinary,
        });
    }

    Err(format!(
        "unknown circuit '{requested}'\n\n{}",
        usage(circuits)
    ))
}

fn facing_str(f: Option<Facing>) -> &'static str {
    match f {
        Some(Facing::North) => "north",
        Some(Facing::South) => "south",
        Some(Facing::East) => "east",
        Some(Facing::West) => "west",
        Some(Facing::Up) => "up",
        Some(Facing::Down) => "down",
        None => "-",
    }
}

fn face_str(f: Option<Face>) -> &'static str {
    match f {
        Some(Face::Floor) => "floor",
        Some(Face::Wall) => "wall",
        Some(Face::Ceiling) => "ceiling",
        None => "-",
    }
}

fn kind_str(k: BlockKind) -> &'static str {
    match k {
        BlockKind::Air => "Air",
        BlockKind::Solid => "Solid",
        BlockKind::Glass => "Glass",
        BlockKind::Slab => "Slab",
        BlockKind::RedstoneWire => "RedstoneWire",
        BlockKind::Repeater => "Repeater",
        BlockKind::Comparator => "Comparator",
        BlockKind::Torch => "Torch",
        BlockKind::WallTorch => "WallTorch",
        BlockKind::Lever => "Lever",
        BlockKind::RedstoneBlock => "RedstoneBlock",
        BlockKind::Lamp => "Lamp",
        BlockKind::Piston => "Piston",
        BlockKind::Button => "Button",
        BlockKind::PressurePlate => "PressurePlate",
        BlockKind::WeightedPressurePlate => "WeightedPressurePlate",
        BlockKind::Observer => "Observer",
        BlockKind::Target => "Target",
        BlockKind::DaylightDetector => "DaylightDetector",
        BlockKind::Other => "Other",
    }
}

fn write_world(world: &World, output: &mut impl std::io::Write) -> std::io::Result<()> {
    let (size_x, size_y, size_z) = world.size();
    writeln!(output, "SIZE {size_x} {size_y} {size_z}")?;
    for y in 0..size_y {
        for z in 0..size_z {
            for x in 0..size_x {
                let state = world.get(x, y, z);
                if state.kind == BlockKind::Air {
                    continue;
                }
                writeln!(
                    output,
                    "BLOCK {x} {y} {z} {} {} {} {} {} {}",
                    kind_str(state.kind),
                    facing_str(state.facing),
                    face_str(state.face),
                    state.lit,
                    state.delay,
                    state.name,
                )?;
                for (key, value) in &state.extra_properties {
                    writeln!(output, "BLOCKPROP {x} {y} {z} {key} {value}")?;
                }
            }
        }
    }
    Ok(())
}

fn circuit_records(
    source: &Netlist,
    compiled: &CompiledCircuit,
    labels: &[(String, String)],
) -> Vec<String> {
    let mut records = Vec::new();
    for (name, (x, y, z)) in &compiled.input_positions {
        records.push(format!("INPUT {name} {x} {y} {z}"));
    }
    for (name, (x, y, z)) in &compiled.output_positions {
        records.push(format!("OUTPUT {name} {x} {y} {z}"));
    }
    for (label, name) in labels {
        records.push(format!("OUTLABEL {label} {name}"));
    }
    let mut remaining_labels: Vec<_> = labels.iter().collect();
    for (ordinal, name) in source.outputs.iter().enumerate() {
        let (x, y, z) = compiled.output_positions[name];
        let label = remaining_labels
            .iter()
            .position(|(_, internal)| internal == name)
            .map(|index| remaining_labels.remove(index).0.as_str())
            .unwrap_or(name);
        records.push(format!("OUTPUTPORT {ordinal} {name} {name} {x} {y} {z} {label}"));
    }
    for (name, &(x, y, z)) in &compiled.gate_output_positions {
        let sequential = source
            .gates
            .iter()
            .any(|gate| gate.output == *name && gate.kind.is_sequential());
        let component = if sequential {
            format!(" {}", kind_str(compiled.bench.get(x, y, z).kind))
        } else {
            String::new()
        };
        records.push(format!("GATEOUT {name} {x} {y} {z}{component}"));
    }
    for gate in &source.gates {
        let inputs = if gate.inputs.is_empty() {
            "-".into()
        } else {
            gate.inputs.join(",")
        };
        records.push(format!("GATE {} {inputs} {}", gate.output, gate.kind.wire_name()));
        if gate.kind.is_sequential() {
            let &(x, y, z) = &compiled.gate_output_positions[&gate.output];
            for (role, (dx, dy, dz)) in reda::compile::construct::DFF_SITES {
                let (cx, cy, cz) = (x + dx, y + dy, z + dz);
                records.push(format!(
                    "DFFCELL {} {role} {cx} {cy} {cz} {}",
                    gate.output,
                    kind_str(compiled.bench.get(cx, cy, cz).kind)
                ));
            }
        }
    }
    records
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mode = reda::frontend::FrontendMode::take_argument(&mut args).unwrap_or_else(|why| {
        eprintln!("{why}");
        std::process::exit(1);
    });
    let circuits = available_circuits();

    if args.is_empty() {
        eprintln!("{}", usage(&circuits));
        std::process::exit(1);
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
            std::process::exit(1);
        }
    };

    // Lower before compiling -- `compile` requires it, and every `GATE` line
    // below has to name a gate that was really placed (see this module's own
    // doc comment).
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

    // Not an `expect`: a hand-written reference circuit is acyclic and fully
    // driven by construction, but a synthesized one is only as well-formed as
    // whatever Verilog it came from, and a compiler bug reaching this point
    // is exactly the kind of thing this binary exists to help investigate.
    // Printing the error beats a panic message wrapped around it.
    let compiled = match compile(&netlist) {
        Ok(compiled) => compiled,
        Err(err) => {
            eprintln!("circuit '{name}' failed to compile: {err}");
            std::process::exit(1);
        }
    };

    write_world(&compiled.bench, &mut std::io::stdout().lock()).expect("dump world");

    for record in circuit_records(&netlist, &compiled, &output_labels) {
        println!("{record}");
    }

    let merge_count = netlist
        .gates
        .iter()
        .filter(|gate| gate.is_merge())
        .count();
    eprintln!(
        "dumped circuit '{name}': {} inputs, {} outputs, {} gate outputs ({} of them wired-OR merges)",
        netlist.inputs.len(),
        netlist.outputs.len(),
        compiled.gate_output_positions.len(),
        merge_count,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dff_dump_exposes_actual_cells_and_outputs() {
        let (netlist, labels) = verilog::synthesize_with_frontend(
            include_str!("../../tests/fixtures/mixed_clock_reset_feedback.sv"),
            "mixed_clock_reset_feedback.sv",
            "mixed_clock_reset_feedback",
            reda::frontend::FrontendMode::Native,
        )
        .unwrap();
        let lowered = lower_optimised(&netlist).unwrap();
        let physical = compile(&lowered).unwrap();
        let records = circuit_records(&lowered, &physical, &labels);
        let cells: Vec<_> = records
            .iter()
            .filter(|record| record.starts_with("DFFCELL "))
            .collect();
        assert_eq!(cells.len(), 10);
        for record in &cells {
            let fields: Vec<_> = record.split_whitespace().collect();
            let (x, y, z) = (
                fields[3].parse::<i32>().unwrap(),
                fields[4].parse::<i32>().unwrap(),
                fields[5].parse::<i32>().unwrap(),
            );
            assert_eq!(fields[6], kind_str(physical.bench.get(x, y, z).kind));
            let component = if fields[2] == "InvC" { "WallTorch" } else { "Repeater" };
            assert_eq!(fields[6], component, "{record}");
        }
        for gate in lowered
            .gates
            .iter()
            .filter(|gate| gate.kind.is_sequential())
        {
            assert!(records
                .iter()
                .any(|record| record.starts_with(&format!("GATEOUT {} ", gate.output))));
            assert_eq!(
                cells
                    .iter()
                    .filter(|record| record.split_whitespace().nth(1) == Some(gate.output.as_str()))
                    .count(),
                5
            );
        }
        if let Ok(directory) = std::env::var("REDA_DUMP_FIXTURE_DIR") {
            let mut bytes = Vec::new();
            write_world(&physical.bench, &mut bytes).unwrap();
            for record in records {
                bytes.extend_from_slice(record.as_bytes());
                bytes.push(b'\n');
            }
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(Path::new(&directory).join("mixed-clock.txt"), bytes).unwrap();
        }
    }
}
