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
//! Synthesizing a `verilog:` circuit needs `python` with `yowasp-yosys`
//! installed. If either is missing this exits non-zero with the frontend's
//! own message (see `FrontendError`'s `Display` impl) rather than panicking:
//! a missing external toolchain is a normal thing for this binary to
//! encounter, not a bug in it.
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
//! The conformance reader accepts `nor`, `merge`, `buf` and `andnot`; a `merge` gate is a
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
    compiled
        .validate_rewrite_view(source)
        .expect("dump must match its sealed rewrite");
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
    let implementation = compiled.implementation_netlist(source);
    let signal = |name: &str| {
        if let Some(rewrite) = compiled.physical_rewrite() {
            if let Some(ordinal) = rewrite
                .rewrite_certificate()
                .rewritten_inputs_in_source_order()
                .iter()
                .position(|binding| binding == name)
            {
                return source.inputs[ordinal].clone();
            }
        }
        compiled.implementation_signal_label(name)
    };
    if let Some(rewrite) = compiled.physical_rewrite() {
        let mut remaining_labels: Vec<_> = labels.iter().collect();
        for (ordinal, ((name, binding), &(x, y, z))) in source
            .outputs
            .iter()
            .zip(
                rewrite
                    .rewrite_certificate()
                    .rewritten_outputs_in_source_order(),
            )
            .zip(rewrite.source_output_positions())
            .enumerate()
        {
            let label = remaining_labels
                .iter()
                .position(|(_, internal)| internal == name)
                .map(|index| remaining_labels.remove(index).0.as_str())
                .unwrap_or(name);
            records.push(format!(
                "OUTPUTPORT {ordinal} {name} {} {x} {y} {z} {label}",
                signal(binding)
            ));
        }
    }
    for (name, &(x, y, z)) in &compiled.gate_output_positions {
        let component = if compiled.physical_rewrite().is_some() {
            format!(" {}", kind_str(compiled.world.get(x, y, z).kind))
        } else {
            String::new()
        };
        records.push(format!("GATEOUT {} {x} {y} {z}{component}", signal(name)));
    }
    let order = if compiled.physical_rewrite().is_some() {
        implementation
            .combinational_order()
            .expect("sealed rewrite is acyclic")
    } else {
        (0..implementation.gates.len()).collect()
    };
    for index in order {
        let gate = &implementation.gates[index];
        let inputs = if gate.inputs.is_empty() {
            "-".into()
        } else {
            gate.inputs
                .iter()
                .map(|name| signal(name))
                .collect::<Vec<_>>()
                .join(",")
        };
        records.push(format!(
            "GATE {} {inputs} {}",
            signal(&gate.output),
            gate.kind.wire_name()
        ));
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
            eprintln!("circuit '{name}' failed to compile: {err:?}");
            std::process::exit(1);
        }
    };

    write_world(&compiled.world, &mut std::io::stdout().lock()).expect("dump world");

    for record in circuit_records(&netlist, &compiled, &output_labels) {
        println!("{record}");
    }

    let merge_count = compiled
        .implementation_netlist(&netlist)
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
    use reda::compile::fragment_synth::torch_row::compile_player_torch_row;
    use reda::compile::Gate;

    #[test]
    fn rewritten_dumps_keep_actual_gates_components_and_ordered_alias_ports() {
        let cases = [
            Netlist {
                inputs: vec!["left".into(), "right".into()],
                outputs: vec![
                    "implemented:literal_a".into(),
                    "implemented::literal_b".into(),
                    "sum".into(),
                ],
                gates: vec![
                    Gate::nor("removed_internal", &["left", "right"]),
                    Gate::nor("implemented:literal_a", &["left"]),
                    Gate::nor("implemented::literal_b", &["right"]),
                    Gate::merge("sum", &["implemented:literal_a", "implemented::literal_b"]),
                ],
            },
            Netlist {
                inputs: vec!["left".into(), "right".into()],
                outputs: vec!["left".into(), "left".into(), "right".into()],
                gates: vec![],
            },
            Netlist {
                inputs: vec!["left".into(), "right".into()],
                outputs: vec!["negative".into(), "negative".into(), "negative".into()],
                gates: vec![Gate::nor("negative", &["left"])],
            },
            Netlist {
                inputs: vec!["left".into(), "right".into()],
                outputs: vec![
                    "product".into(),
                    "negative_left".into(),
                    "negative_right".into(),
                ],
                gates: vec![
                    Gate::nor("negative_left", &["left"]),
                    Gate::nor("negative_right", &["right"]),
                    Gate {
                        name: "product".into(),
                        output: "product".into(),
                        inputs: vec!["left".into(), "right".into()],
                        kind: reda::compile::topology::GateKind::AndNot,
                    },
                ],
            },
            Netlist {
                inputs: vec!["left".into(), "right".into()],
                outputs: vec!["high".into(), "low".into(), "high".into()],
                gates: vec![Gate::nor("high", &[]), Gate::nor("low", &["high"])],
            },
        ];
        for (index, source) in cases.iter().enumerate() {
            let compiled = CompiledCircuit::from_certified_rewrite(
                compile_player_torch_row(source, 1_000_000).unwrap(),
            );
            let labels: Vec<_> = source
                .outputs
                .iter()
                .enumerate()
                .map(|(ordinal, name)| (format!("public{ordinal}"), name.clone()))
                .collect();
            let records = circuit_records(source, &compiled, &labels);
            let gates: Vec<_> = records.iter().filter(|s| s.starts_with("GATE ")).collect();
            assert_eq!(
                gates.len(),
                compiled.implementation_netlist(source).gates.len()
            );
            assert!(gates.iter().all(|s| !s.contains("removed_internal")));
            assert_eq!(
                records
                    .iter()
                    .filter(|s| s.starts_with("OUTPUTPORT "))
                    .count(),
                source.outputs.len()
            );
            let ports: Vec<_> = records
                .iter()
                .filter(|s| s.starts_with("OUTPUTPORT "))
                .map(|s| s.split_whitespace().skip(4).take(3).collect::<Vec<_>>())
                .collect();
            assert_eq!(
                ports
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>()
                    .len(),
                source.outputs.len()
            );
            let mut bytes = Vec::new();
            write_world(&compiled.world, &mut bytes).unwrap();
            for record in records {
                bytes.extend_from_slice(record.as_bytes());
                bytes.push(b'\n');
            }
            if let Ok(directory) = std::env::var("REDA_DUMP_FIXTURE_DIR") {
                std::fs::create_dir_all(&directory).unwrap();
                std::fs::write(
                    Path::new(&directory).join(format!("rewrite-{index}.txt")),
                    bytes,
                )
                .unwrap();
            }
        }
    }
    #[test]
    fn dump_uses_proved_permuted_bindings() {
        use reda::compile::fragment_synth::rewrite::realise_physical_rewrite;
        use reda::compile::topology::{
            GateKind, Library, LibraryEntry, Primitive, Template, TemplateNode,
        };
        use std::collections::BTreeMap;
        let initial_source = Netlist {
            inputs: vec!["a".into(), "b".into()],
            outputs: vec!["sum".into(), "a".into(), "b".into()],
            gates: vec![Gate::merge("sum", &["a", "b"])],
        };
        let initial = compile_player_torch_row(&initial_source, 1_000_000).unwrap();
        let source = Netlist {
            inputs: vec!["rhs".into(), "lhs".into()],
            outputs: vec!["right".into(), "sum".into(), "left".into()],
            gates: vec![
                Gate {
                    name: "right".into(),
                    output: "right".into(),
                    kind: GateKind::Buf,
                    inputs: vec!["rhs".into()],
                },
                Gate {
                    name: "left".into(),
                    output: "left".into(),
                    kind: GateKind::Buf,
                    inputs: vec!["lhs".into()],
                },
                Gate::merge("sum", &["right", "left"]),
            ],
        };
        let library = Library::new(BTreeMap::from([(
            GateKind::Buf,
            vec![LibraryEntry {
                name: "matrix-repeater-buffer",
                template: Template {
                    nodes: vec![(TemplateNode::Torch, Primitive::Repeater)],
                    internal_edges: vec![],
                    inputs: vec![TemplateNode::Torch],
                    output: Some(TemplateNode::Torch),
                    embedding_hints: vec![],
                },
            }],
        )]));
        let rewrite = realise_physical_rewrite(
            &source,
            initial.implemented(),
            initial.implemented_candidate(),
            &library,
            &["b".into(), "a".into()],
            &[
                "positive_b".into(),
                "shared_sum".into(),
                "positive_a".into(),
            ],
            1_000_000,
        )
        .unwrap();
        let compiled = CompiledCircuit::from_certified_rewrite(rewrite);
        assert_eq!(
            compiled.input_positions["rhs"],
            initial.source_input_positions()[1]
        );
        let records = circuit_records(&source, &compiled, &[]);
        assert!(records
            .iter()
            .any(|record| record.starts_with("GATE implemented:positive_b rhs buf")));
        assert!(records
            .iter()
            .any(|record| record.starts_with("GATE implemented:positive_a lhs buf")));
        let mut bytes = Vec::new();
        write_world(&compiled.world, &mut bytes).unwrap();
        for record in records {
            bytes.extend_from_slice(record.as_bytes());
            bytes.push(b'\n');
        }
        if let Ok(directory) = std::env::var("REDA_DUMP_FIXTURE_DIR") {
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(Path::new(&directory).join("permuted.txt"), bytes).unwrap();
        }
    }
}
