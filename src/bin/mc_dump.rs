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
//!   INPUT  name x y z
//!   OUTPUT name x y z
//!   OUTLABEL label internal_name
//!   GATEOUT name x y z
//!   GATE   output_name input1,input2,... kind
//!
//! `OUTPUT`'s `name` is always the netlist's own internal signal name (a
//! `GATE` output name, so it is guaranteed to also key into every `GATE`
//! line and the boolean evaluation the Python side does over them).
//! `OUTLABEL` is a purely cosmetic side channel mapping that same internal
//! name to the human label the circuit is documented with ("y", "sum",
//! "a".."g") -- see `build_circuit.rs`'s identical `output_labels` field for
//! why the two names differ at all. A dump consumer that ignores `OUTLABEL`
//! entirely still gets a fully correct, just less legible, circuit.
//!
//! `GATE` lines are the netlist's own gates, in `Netlist::gates` order
//! (already a valid topological order -- `NetlistBuilder` only ever appends
//! a gate after every one of its inputs already exists). Together with
//! `INPUT`, that is enough for the Python side to hand-evaluate the pure
//! boolean function for any input vector and know what every gate's
//! *should-be* value is -- not just the final output -- which is what lets
//! a failing run localise itself to the first disagreeing gate instead of
//! only reporting "the lamp was wrong".
//!
//! `kind` is `nor` or `merge`, and it is not decoration: a `merge` gate
//! (`Gate::is_merge`) is a **wired OR**, not a NOR, so a consumer that
//! assumes every gate inverts computes the wrong expected value for that
//! gate *and for everything downstream of it*. Every hand-written reference
//! circuit is pure NOR and so emits nothing but `nor`; the Verilog frontend
//! maps Yosys's `OR2`/`OR3` cells onto merges, so a `verilog:` circuit
//! generally has both. A merge also has no torch and no gate body at all --
//! just a dust junction where its inputs' routes are allowed to touch -- so
//! its `GATEOUT` position holds `minecraft:redstone_wire`, and its logical
//! value is "that dust has non-zero power", not "a torch is lit". Both facts
//! are the reason this field exists rather than being inferrable.
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
use reda::compile::{compile, Netlist};
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
}

fn usage(circuits: &[NamedCircuit]) -> String {
    let hand_written: Vec<&str> = circuits.iter().map(|c| c.name).collect();
    let synthesized: Vec<&str> = verilog::CIRCUITS.iter().map(|c| c.name).collect();
    format!(
        "usage: mc_dump <circuit-name>\n       mc_dump verilog <file.v> <top-module>\n\n\
         hand-written circuits: {}\n\
         verilog circuits:      {}\n\n\
         The `verilog:` ones are synthesized through yosys and need `python` with \
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
fn select(args: &[String], circuits: &[NamedCircuit]) -> Result<SelectedCircuit, String> {
    // `mc_dump verilog <file.v> <top-module>`: a Verilog source this project
    // does not ship. Spelled as separate arguments rather than folded into
    // the `verilog:` name syntax because a Windows path (`C:\...`) already
    // contains a colon.
    if args.first().map(String::as_str) == Some("verilog") {
        let (Some(path), Some(top_module)) = (args.get(1), args.get(2)) else {
            return Err("usage: mc_dump verilog <file.v> <top-module>".to_string());
        };
        let (netlist, output_labels) = verilog::synthesize_file(Path::new(path), top_module)
            .map_err(|err| format!("could not synthesize {path} ({top_module}): {err}"))?;
        return Ok(SelectedCircuit { name: format!("verilog {path}:{top_module}"), netlist, output_labels });
    }

    let requested = args.first().expect("select is only called with at least one argument");

    if let Some(circuit) = verilog::find(requested) {
        let (netlist, output_labels) = circuit
            .synthesize()
            .map_err(|err| format!("could not synthesize '{}': {err}", circuit.name))?;
        return Ok(SelectedCircuit { name: circuit.name.to_string(), netlist, output_labels });
    }

    if let Some(circuit) = circuits.iter().find(|c| c.name == requested.as_str()) {
        return Ok(SelectedCircuit {
            name: circuit.name.to_string(),
            netlist: (circuit.build)(),
            output_labels: (circuit.output_labels)(),
        });
    }

    Err(format!("unknown circuit '{requested}'\n\n{}", usage(circuits)))
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

fn dump_world(world: &World) {
    let (size_x, size_y, size_z) = world.size();
    println!("SIZE {size_x} {size_y} {size_z}");
    for y in 0..size_y {
        for z in 0..size_z {
            for x in 0..size_x {
                let state = world.get(x, y, z);
                if state.kind == BlockKind::Air {
                    continue;
                }
                println!(
                    "BLOCK {x} {y} {z} {} {} {} {} {} {}",
                    kind_str(state.kind),
                    facing_str(state.facing),
                    face_str(state.face),
                    state.lit,
                    state.delay,
                    state.name,
                );
            }
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let circuits = available_circuits();

    if args.is_empty() {
        eprintln!("{}", usage(&circuits));
        std::process::exit(1);
    }

    let SelectedCircuit { name, netlist, output_labels } = match select(&args, &circuits) {
        Ok(selected) => selected,
        Err(message) => {
            eprintln!("{message}");
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

    dump_world(&compiled.world);

    for (input_name, (x, y, z)) in &compiled.input_positions {
        println!("INPUT {input_name} {x} {y} {z}");
    }
    for (output_name, (x, y, z)) in &compiled.output_positions {
        println!("OUTPUT {output_name} {x} {y} {z}");
    }
    for (label, internal_name) in &output_labels {
        println!("OUTLABEL {label} {internal_name}");
    }
    for (gate_name, (x, y, z)) in &compiled.gate_output_positions {
        println!("GATEOUT {gate_name} {x} {y} {z}");
    }
    for gate in &netlist.gates {
        let inputs = if gate.inputs.is_empty() { "-".to_string() } else { gate.inputs.join(",") };
        let kind = if gate.is_merge { "merge" } else { "nor" };
        println!("GATE {} {inputs} {kind}", gate.output);
    }

    let merge_count = netlist.gates.iter().filter(|gate| gate.is_merge).count();
    eprintln!(
        "dumped circuit '{name}': {} inputs, {} outputs, {} gate outputs ({} of them wired-OR merges)",
        netlist.inputs.len(),
        netlist.outputs.len(),
        compiled.gate_output_positions.len(),
        merge_count,
    );
}
