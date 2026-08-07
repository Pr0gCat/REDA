//! Dumps a compiled reference circuit as a plain-text description the
//! conformance harness (`conformance/and4_conformance.py`) can turn into
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
//!   GATEOUT name x y z
//!   GATE   output_name input1,input2,...
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

use reda::circuits::and4;
use reda::compile::{compile, Netlist};
use reda::redstone::world::block::{BlockKind, Face, Facing};
use reda::redstone::world::storage::World;

/// A named reference circuit's builder, paired with the name used to select
/// it on the command line -- see `available_circuits` below.
type NamedCircuitBuilder = (&'static str, fn() -> Netlist);

fn available_circuits() -> Vec<NamedCircuitBuilder> {
    vec![("and4", || and4::build_and4_netlist().0)]
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
        eprintln!("available: {}", circuits.iter().map(|c| c.0).collect::<Vec<_>>().join(", "));
        std::process::exit(1);
    };

    let Some((name, build)) = circuits.iter().find(|c| c.0 == requested.as_str()) else {
        eprintln!("unknown circuit '{requested}'");
        std::process::exit(1);
    };

    let netlist = build();
    let compiled = compile(&netlist).expect("reference circuits are acyclic and fully driven");

    dump_world(&compiled.world);

    for (input_name, (x, y, z)) in &compiled.input_positions {
        println!("INPUT {input_name} {x} {y} {z}");
    }
    for (output_name, (x, y, z)) in &compiled.output_positions {
        println!("OUTPUT {output_name} {x} {y} {z}");
    }
    for (gate_name, (x, y, z)) in &compiled.gate_output_positions {
        println!("GATEOUT {gate_name} {x} {y} {z}");
    }
    for gate in &netlist.gates {
        println!("GATE {} {}", gate.output, gate.inputs.join(","));
    }

    eprintln!("dumped circuit '{name}': {} inputs, {} outputs, {} gate outputs", netlist.inputs.len(), netlist.outputs.len(), compiled.gate_output_positions.len());
}
