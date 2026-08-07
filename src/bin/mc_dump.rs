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
//! Output line kinds:
//!
//!   SIZE   size_x size_y size_z
//!   BLOCK  x y z kind facing face lit delay name
//!   INPUT  name x y z
//!   OUTPUT name x y z
//!   OUTLABEL label internal_name
//!   GATEOUT name x y z
//!   GATE   output_name input1,input2,...
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
//! `GATE` lines are the netlist's own NOR gates, in `Netlist::gates` order
//! (already a valid topological order -- `NetlistBuilder` only ever appends
//! a gate after every one of its inputs already exists). Together with
//! `INPUT`, that is enough for the Python side to hand-evaluate the pure
//! boolean NOR function for any input vector and know what every gate's
//! *should-be* value is -- not just the final output -- which is what lets
//! a failing run localise itself to the first disagreeing gate instead of
//! only reporting "the lamp was wrong".
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

use reda::circuits::{and4, full_adder, seven_segment};
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
    output_labels: fn() -> Vec<(&'static str, String)>,
}

fn available_circuits() -> Vec<NamedCircuit> {
    vec![
        NamedCircuit {
            name: "and4",
            build: || and4::build_and4_netlist().0,
            output_labels: || vec![(and4::OUTPUT_NAME, and4::build_and4_netlist().1)],
        },
        NamedCircuit {
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
        NamedCircuit {
            name: "segment_a",
            build: || seven_segment::build_single_segment_netlist(0).0,
            output_labels: || {
                vec![(seven_segment::SEGMENT_NAMES[0], seven_segment::build_single_segment_netlist(0).1)]
            },
        },
        NamedCircuit {
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
    let args: Vec<String> = std::env::args().collect();
    let circuits = available_circuits();

    let Some(requested) = args.get(1) else {
        eprintln!("usage: mc_dump <circuit-name>");
        eprintln!("available: {}", circuits.iter().map(|c| c.name).collect::<Vec<_>>().join(", "));
        std::process::exit(1);
    };

    let Some(circuit) = circuits.iter().find(|c| c.name == requested.as_str()) else {
        eprintln!("unknown circuit '{requested}'");
        std::process::exit(1);
    };
    let name = circuit.name;

    let netlist = (circuit.build)();
    let compiled = compile(&netlist).expect("reference circuits are acyclic and fully driven");

    dump_world(&compiled.world);

    for (input_name, (x, y, z)) in &compiled.input_positions {
        println!("INPUT {input_name} {x} {y} {z}");
    }
    for (output_name, (x, y, z)) in &compiled.output_positions {
        println!("OUTPUT {output_name} {x} {y} {z}");
    }
    for (label, internal_name) in (circuit.output_labels)() {
        println!("OUTLABEL {label} {internal_name}");
    }
    for (gate_name, (x, y, z)) in &compiled.gate_output_positions {
        println!("GATEOUT {gate_name} {x} {y} {z}");
    }
    for gate in &netlist.gates {
        println!("GATE {} {}", gate.output, gate.inputs.join(","));
    }

    eprintln!("dumped circuit '{name}': {} inputs, {} outputs, {} gate outputs", netlist.inputs.len(), netlist.outputs.len(), compiled.gate_output_positions.len());
}
