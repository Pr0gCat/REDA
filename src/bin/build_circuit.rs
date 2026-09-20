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

use std::path::Path;

use reda::circuits::{and4, full_adder, seven_segment, verilog};
use reda::compile::lowering::{lower, lower_optimised};
use reda::compile::planner::{Anchor, PortPin, PortPlacements, PortRole};
use reda::compile::{compile, CompileError, CompiledCircuit, Netlist};
use reda::formats::litematic;
use reda::redstone::simulator::position::Position;
use reda::redstone::world::block::{BlockKind, Facing};
use reda::redstone::world::storage::World;

/// A `--pins` file, parsed but not yet resolved against a circuit: the port
/// names exactly as the caller wrote them -- inputs by their declared names,
/// outputs by **display label** (`a`..`g`, `y`), because internal `gN` names
/// are this compiler's business, not a caller's.
#[derive(Debug)]
struct PinsFile {
    inputs: Vec<(String, PortPin)>,
    outputs: Vec<(String, PortPin)>,
}

/// Parse the `--pins` JSON:
///
/// ```json
/// {"inputs":  {"d0": {"at": [x,y,z], "toward": "north"}},
///  "outputs": {"a":  {"at": [x,y,z], "toward": "north"}}}
/// ```
///
/// Hand-rolled like the pinout writer it mirrors -- this crate's binaries
/// keep JSON as plain text on both sides (see the writer below, and
/// `mc_dump`'s format note). Every refusal names what was wrong and where,
/// because a pins file is written by hand and read exactly once.
fn parse_pins_file(text: &str) -> Result<PinsFile, String> {
    let mut cursor = Cursor {
        bytes: text.as_bytes(),
        pos: 0,
    };
    let mut pins = PinsFile {
        inputs: Vec::new(),
        outputs: Vec::new(),
    };
    let (mut saw_inputs, mut saw_outputs) = (false, false);

    cursor.expect(b'{')?;
    if cursor.peek() != Some(b'}') {
        loop {
            let section = cursor.string()?;
            cursor.expect(b':')?;
            match section.as_str() {
                "inputs" => {
                    if saw_inputs {
                        return Err("section `inputs` contains duplicate key `inputs`".to_string());
                    }
                    saw_inputs = true;
                    parse_port_map(&mut cursor, "inputs", &mut pins.inputs)?;
                }
                "outputs" => {
                    if saw_outputs {
                        return Err(
                            "section `outputs` contains duplicate key `outputs`".to_string()
                        );
                    }
                    saw_outputs = true;
                    parse_port_map(&mut cursor, "outputs", &mut pins.outputs)?;
                }
                other => {
                    return Err(format!(
                        "unknown key \"{other}\": a pins file has \"inputs\" and \"outputs\""
                    ))
                }
            }
            if !cursor.take(b',') {
                break;
            }
        }
    }
    cursor.expect(b'}')?;
    cursor.skip_ws();
    if cursor.pos != cursor.bytes.len() {
        return Err(format!(
            "trailing content after the closing brace: {}",
            String::from_utf8_lossy(&cursor.bytes[cursor.pos..])
        ));
    }
    Ok(pins)
}

/// One `{"name": {pin}, ...}` section of the pins file.
fn parse_port_map(
    cursor: &mut Cursor,
    section: &str,
    ports: &mut Vec<(String, PortPin)>,
) -> Result<(), String> {
    cursor.expect(b'{')?;
    if cursor.peek() != Some(b'}') {
        loop {
            let name = cursor.string()?;
            cursor.expect(b':')?;
            let pin = parse_pin(cursor)
                .map_err(|why| format!("section \"{section}\", port \"{name}\": {why}"))?;
            if ports.iter().any(|(existing, _)| existing == &name) {
                return Err(format!(
                    "section \"{section}\", port \"{name}\" is pinned twice"
                ));
            }
            ports.push((name, pin));
            if !cursor.take(b',') {
                break;
            }
        }
    }
    cursor.expect(b'}')
}

/// One `{"at": [x,y,z], "toward": "<facing>"}` object, keys in any order,
/// both required, nothing else admitted.
///
/// `toward` is the direction the signal travels through the pinned cell, and
/// nothing here interprets it: which neighbour that resolves to depends on
/// whether the port is an input or an output, and the parser does not know.
/// **The parser validates syntax and nothing else** -- every semantic refusal
/// belongs to `PortPlacements`, so the editor and the in-game adapter that
/// never touch this file inherit the same rules.
fn parse_pin(cursor: &mut Cursor) -> Result<PortPin, String> {
    cursor.expect(b'{')?;
    let (mut at, mut toward) = (None, None);
    loop {
        let key = cursor.string()?;
        cursor.expect(b':')?;
        match key.as_str() {
            "at" => {
                if at.is_some() {
                    return Err("duplicate key \"at\"".to_string());
                }
                let triple = (|| {
                    cursor.expect(b'[')?;
                    let x = cursor.integer()?;
                    cursor.expect(b',')?;
                    let y = cursor.integer()?;
                    cursor.expect(b',')?;
                    let z = cursor.integer()?;
                    cursor.expect(b']')?;
                    Ok(Anchor { x, y, z })
                })()
                .map_err(|why: String| {
                    format!("\"at\" must be [x, y, z] -- exactly three integers ({why})")
                })?;
                at = Some(triple);
            }
            "toward" => {
                if toward.is_some() {
                    return Err("duplicate key \"toward\"".to_string());
                }
                let name = cursor.string()?;
                toward = Some(match name.as_str() {
                    "north" => Facing::North,
                    "south" => Facing::South,
                    "east" => Facing::East,
                    "west" => Facing::West,
                    other => {
                        return Err(format!(
                            "\"toward\" must be north, south, east, or west; got \"{other}\""
                        ))
                    }
                });
            }
            other => {
                return Err(format!(
                    "unknown key \"{other}\": a pin has \"at\" and \"toward\""
                ))
            }
        }
        if !cursor.take(b',') {
            break;
        }
    }
    cursor.expect(b'}')?;
    match (at, toward) {
        (Some(at), Some(toward)) => Ok(PortPin { at, toward }),
        (None, _) => Err("missing \"at\": the caller's cell as [x, y, z]".to_string()),
        (_, None) => Err(
            "missing \"toward\": the direction the signal travels through that cell".to_string(),
        ),
    }
}

/// The pins parser's read head. Just enough JSON for the format above: no
/// escapes (port names are identifiers), no floats, no nesting beyond what
/// the two shapes state -- anything else is a defect worth naming, not a
/// generality worth supporting.
struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Cursor<'_> {
    fn skip_ws(&mut self) {
        while matches!(self.bytes.get(self.pos), Some(b' ' | b'\t' | b'\r' | b'\n')) {
            self.pos += 1;
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.skip_ws();
        self.bytes.get(self.pos).copied()
    }

    /// Consume `wanted` if it is next; report whether it was.
    fn take(&mut self, wanted: u8) -> bool {
        if self.peek() == Some(wanted) {
            self.pos += 1;
            return true;
        }
        false
    }

    fn expect(&mut self, wanted: u8) -> Result<(), String> {
        self.skip_ws();
        match self.bytes.get(self.pos) {
            Some(&byte) if byte == wanted => {
                self.pos += 1;
                Ok(())
            }
            Some(&byte) => Err(format!(
                "expected '{}' at byte {}, found '{}'",
                wanted as char, self.pos, byte as char
            )),
            None => Err(format!(
                "expected '{}' at byte {}, found end of file",
                wanted as char, self.pos
            )),
        }
    }

    fn string(&mut self) -> Result<String, String> {
        self.expect(b'"')?;
        let start = self.pos;
        while let Some(&byte) = self.bytes.get(self.pos) {
            if byte == b'"' {
                let text = std::str::from_utf8(&self.bytes[start..self.pos])
                    .map_err(|_| format!("the string at byte {start} is not UTF-8"))?;
                self.pos += 1;
                return Ok(text.to_string());
            }
            if byte == b'\\' {
                return Err(format!(
                    "the string at byte {start} uses an escape; port and facing names never need one"
                ));
            }
            self.pos += 1;
        }
        Err(format!("the string at byte {start} never closes"))
    }

    fn integer(&mut self) -> Result<i32, String> {
        self.skip_ws();
        let start = self.pos;
        if self.bytes.get(self.pos) == Some(&b'-') {
            self.pos += 1;
        }
        while matches!(self.bytes.get(self.pos), Some(b'0'..=b'9')) {
            self.pos += 1;
        }
        std::str::from_utf8(&self.bytes[start..self.pos])
            .expect("digits and a sign are UTF-8")
            .parse::<i32>()
            .map_err(|_| format!("expected an integer at byte {start}"))
    }
}

/// Turn parsed pins into the planner's `PortPlacements`: inputs pin under
/// their own names; each output label resolves to its internal signal through
/// `output_labels`, and a label that resolves nowhere is refused by name.
/// Undeclared *input* names are left for the planner's own door
/// (`InvalidPortPin` names them), which also owns every geometric refusal.
fn resolve_pins(
    pins: &PinsFile,
    output_labels: &[(String, String)],
) -> Result<PortPlacements, String> {
    let mut placements = PortPlacements::default();
    for (name, pin) in &pins.inputs {
        placements.pin(name.clone(), pin.at, pin.toward);
    }
    for (label, pin) in &pins.outputs {
        let signal = output_labels
            .iter()
            .find(|(candidate, _)| candidate == label)
            .map(|(_, signal)| signal.clone())
            .ok_or_else(|| {
                format!(
                    "no declared output is labelled \"{label}\" (this circuit's labels: {})",
                    output_labels
                        .iter()
                        .map(|(label, _)| label.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })?;
        placements.pin(signal, pin.at, pin.toward);
    }
    Ok(placements)
}

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
fn select(args: &[String], circuits: &[CircuitInfo]) -> Result<SelectedCircuit, String> {
    if args.first().map(String::as_str) == Some("verilog") {
        let (Some(path), Some(top_module)) = (args.get(1), args.get(2)) else {
            return Err("usage: build_circuit verilog <file.v> <top-module>".to_string());
        };
        let (netlist, output_labels) = verilog::synthesize_file(Path::new(path), top_module)
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
        let (netlist, output_labels) = circuit
            .synthesize()
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
///
/// A pinned port's cell is the **caller's**: it ships empty, and the line
/// says which way its signal travels and where REDA's handover ended up. The
/// handover is derivable rather than chosen, but a caller building against it
/// should not have to redo the derivation -- and a reported cell that
/// disagrees with the shipped world is a bug this line makes visible. An
/// unpinned run prints exactly what it always did.
fn print_pinout(
    compiled: &CompiledCircuit,
    output_labels: &[(String, String)],
    placements: &PortPlacements,
) {
    let annotate = |port: &str, at: (i32, i32, i32), pin: Option<PortPin>, role: PortRole| match pin
    {
        Some(pin) => {
            let handover = handover_in_the_world(&compiled.world, port, at, pin, role)
                .unwrap_or_else(|why| {
                    panic!("the pinout would describe a world that did not ship: {why}")
                });
            format!(
                "   yours (ships empty), signal {}, handover ({}, {}, {})",
                facing_name(pin.toward),
                handover.x,
                handover.y,
                handover.z
            )
        }
        None => String::new(),
    };
    println!();
    println!(
        "pinout (schematic-local coordinates: x,y,z from the corner the .litematic is pasted at)"
    );
    println!("  inputs (lever, unless pinned):");
    for (name, &(x, y, z)) in &compiled.input_positions {
        println!(
            "    {name:<12} ({x}, {y}, {z}){}",
            annotate(name, (x, y, z), placements.get(name), PortRole::Input)
        );
    }
    println!("  outputs (lamp, unless pinned):");
    for (label, signal) in output_labels {
        let (x, y, z) = compiled.output_positions[signal];
        println!(
            "    {label:<12} ({x}, {y}, {z}){}",
            annotate(label, (x, y, z), placements.get(signal), PortRole::Output)
        );
    }
}

/// REDA's own handover cell for a pinned port, **read out of the shipped
/// world** and required to be the cell the pin derives.
///
/// The handover is derivable rather than chosen, so a report could simply
/// print `pin.handover(role)` and be right whenever the compiler is. That is
/// exactly the failure worth catching: it would print the same number when the
/// compiler is wrong, and ship a correct-looking file describing a world that
/// does not exist. So the cell that gets reported is the one the world
/// actually holds, found through `compile`'s own shared predicates rather than
/// by a search invented here, and a disagreement is named instead of hidden.
///
/// `recorded` is the port's position as the compiled circuit recorded it,
/// which for a pinned port must be the caller's own cell -- checked, because a
/// recorded position that drifted off the pin would make every coordinate on
/// the line describe a different port than its name says.
///
/// Not a second copy of `verify_terminal_contract`, which asserts the same
/// geometry *inside* the compiler and from the plan's own arithmetic. This
/// arrives at it from the other end -- the shipped blocks, through the
/// predicates that only know how to recognise a terminal -- so the report is
/// not taking the compiler's word for the compiler's work.
fn handover_in_the_world(
    world: &World,
    port: &str,
    recorded: (i32, i32, i32),
    pin: PortPin,
    role: PortRole,
) -> Result<Anchor, String> {
    let derived = pin.handover(role);
    let cell = |(x, y, z): (i32, i32, i32)| format!("({x}, {y}, {z})");
    if recorded != (pin.at.x, pin.at.y, pin.at.z) {
        return Err(format!(
            "port `{port}` is pinned at {} but the compiled circuit records it at {} -- \
             the shipped world is not the one the pins asked for",
            cell((pin.at.x, pin.at.y, pin.at.z)),
            cell(recorded)
        ));
    }
    let at = Position::new(recorded.0, recorded.1, recorded.2);
    let found = match role {
        PortRole::Input => reda::compile::input_terminal_reader(world, at),
        PortRole::Output => reda::compile::output_terminal_handover(world, at),
    };
    match found {
        Some(cell_in_world)
            if (cell_in_world.x, cell_in_world.y, cell_in_world.z)
                == (derived.x, derived.y, derived.z) =>
        {
            Ok(derived)
        }
        Some(cell_in_world) => Err(format!(
            "port `{port}`: its `toward` puts the handover at {}, but the shipped world holds it \
             at {}",
            cell((derived.x, derived.y, derived.z)),
            cell((cell_in_world.x, cell_in_world.y, cell_in_world.z))
        )),
        None => Err(format!(
            "port `{port}`: its `toward` puts the handover at {}, but the shipped world has no \
             handover beside the caller's cell {} at all",
            cell((derived.x, derived.y, derived.z)),
            cell(recorded)
        )),
    }
}

/// The lowercase name the pins file and the pinout sidecar share for a
/// `toward` facing.
fn facing_name(facing: Facing) -> &'static str {
    match facing {
        Facing::North => "north",
        Facing::South => "south",
        Facing::East => "east",
        Facing::West => "west",
        // The planner's door refused any vertical `toward` long before a
        // compiled circuit existed to print.
        Facing::Up | Facing::Down => unreachable!("a pin's `toward` is horizontal"),
    }
}

fn list_circuits(circuits: &[CircuitInfo]) {
    println!("Usage: build_circuit <name> [--grown [--pins <file.json>]]");
    println!("       build_circuit verilog <file.v> <top-module>");
    println!();
    println!("--grown compiles through the generation front door (minutes, not");
    println!("milliseconds); --pins declares IO terminals for it -- inputs by name,");
    println!("outputs by display label:");
    println!("  {{\"inputs\":  {{\"d0\": {{\"at\": [x,y,z], \"toward\": \"north\"}}}},");
    println!("   \"outputs\": {{\"a\":  {{\"at\": [x,y,z], \"toward\": \"north\"}}}}}}");
    println!();
    println!("Available circuits:");
    for info in circuits {
        let name = info.name;
        let netlist = (info.build)();
        let gate_count = netlist.gates.len();
        let compiled = compile(&netlist)
            .unwrap_or_else(|err| panic!("circuit '{name}' failed to compile: {err:?}"));
        let (size_x, size_y, size_z) = compiled.world.size();
        println!("  {name:<14} {gate_count:>4} gates   {size_x}x{size_y}x{size_z}");
    }
    // No gate count or bounding box here: printing those means synthesizing,
    // and synthesizing means a Yosys run per entry -- see this file's module
    // doc comment for why a listing must not do that.
    println!();
    println!("Verilog circuits (synthesized on demand; need `python` with `yowasp-yosys`):");
    for circuit in verilog::CIRCUITS {
        println!("  {:<22} module {}", circuit.name, circuit.top_module);
    }
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    // `--grown` compiles through the failure-directed generation front door
    // (`compile_grown`) instead of the fast trial-then-fallback `compile`.
    // Minutes, not milliseconds -- see compile_grown's own doc for the
    // measured costs -- and the file is named `<name>.grown.litematic` so
    // the two producers' outputs can sit side by side.
    let grown = args.iter().any(|arg| arg == "--grown");
    args.retain(|arg| arg != "--grown");
    // `--pins <file.json>` declares terminals for the ports the caller owns
    // (the IO-terminals contract, docs/superpowers/specs/2026-08-30):
    // inputs by name, outputs by display label. Grown-only, because pins
    // compile through the generation front door -- `compile` has no
    // placements parameter and the legacy emitter never will.
    let pins_path = match args.iter().position(|arg| arg == "--pins") {
        Some(index) => {
            args.remove(index);
            if index >= args.len() {
                eprintln!("--pins needs a file: --pins <file.json>");
                std::process::exit(1);
            }
            Some(args.remove(index))
        }
        None => None,
    };
    if pins_path.is_some() && !grown {
        eprintln!(
            "--pins requires --grown: pinned ports compile through the generation front door"
        );
        std::process::exit(1);
    }
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
    } = match select(&args, &circuits) {
        Ok(selected) => selected,
        Err(message) => {
            eprintln!("{message}");
            eprintln!();
            list_circuits(&circuits);
            std::process::exit(1);
        }
    };
    // The pins file resolves against the *selected* circuit -- output labels
    // are its vocabulary -- and every defect exits here by name, before the
    // minutes-long compile a --grown run is about to pay for.
    let placements = match &pins_path {
        Some(path) => {
            let text = match std::fs::read_to_string(path) {
                Ok(text) => text,
                Err(err) => {
                    eprintln!("could not read the pins file {path}: {err}");
                    std::process::exit(1);
                }
            };
            match parse_pins_file(&text).and_then(|pins| resolve_pins(&pins, &output_labels)) {
                Ok(placements) => placements,
                Err(why) => {
                    eprintln!("pins file {path}: {why}");
                    std::process::exit(1);
                }
            }
        }
        None => PortPlacements::default(),
    };
    // Lower first: `compile` takes only NOR gates and wire merges, and a
    // synthesized netlist arrives at the gate level. This is the identity on
    // every hand-written circuit, so `gate_count` below still reports what
    // really gets placed either way.
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

    // Not an `expect`: a synthesized netlist is only as well-formed as the
    // Verilog it came from, so this has to be able to fail readably rather
    // than panicking. See `mc_dump`'s identical reasoning.
    let compiled = match if grown {
        reda::compile::compile_grown(&netlist, &placements)
    } else {
        compile(&netlist)
    } {
        Ok(compiled) => compiled,
        // A refused pin is the *caller's* input, not a compiler failure, and
        // it arrives as data precisely so each adapter can present it its own
        // way: an editor points at the pin, a mod highlights the block, and
        // this one -- the file adapter -- renders the sentence the refusal was
        // written to be. Everything else stays the Debug dump, because
        // everything else is this compiler's own business and there is nothing
        // for the caller to fix.
        Err(err @ CompileError::InvalidPortPin { .. }) => {
            eprintln!("pins: {err}");
            std::process::exit(1);
        }
        Err(err) => {
            eprintln!("circuit '{name}' failed to compile: {err:?}");
            std::process::exit(1);
        }
    };
    let (size_x, size_y, size_z) = compiled.world.size();
    let non_air_blocks = count_non_air(&compiled.world);

    let output_dir = Path::new("output");
    std::fs::create_dir_all(output_dir).expect("failed to create the output directory");
    let stem = if grown {
        format!("{name}.grown")
    } else {
        name.clone()
    };
    let output_path = output_dir.join(format!("{stem}.litematic"));

    litematic::save(&output_path, &compiled.world, &name)
        .expect("failed to write the litematic file");

    // A plain-text block dump beside the litematic, one non-air block per
    // line -- `x y z kind facing lit power` -- for viewers that are not
    // Minecraft: the mc_dump format's spirit, without the conformance
    // harness's framing.
    {
        use std::io::Write;
        let dump_path = output_dir.join(format!("{stem}.blocks.txt"));
        let mut dump = std::fs::File::create(&dump_path).expect("failed to create the dump");
        for x in 0..size_x {
            for y in 0..size_y {
                for z in 0..size_z {
                    let state = compiled.world.get(x, y, z);
                    if state.kind == reda::redstone::world::block::BlockKind::Air {
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
    // does not carry names. Hand-rolled -- this crate deliberately has no
    // serde_json (see mc_dump) and the structure is two flat maps.
    //
    // An unpinned port stays the bare `[x,y,z]` it has always been. A pinned
    // one records the caller's cell, its `toward`, and the resolved handover
    // cell: `toward` is not decoration -- it is how a consumer knows which way
    // the signal runs -- and the handover is reported rather than left to be
    // re-derived. Reported from the *world* at that: `handover_in_the_world`
    // reads it back out of what shipped and refuses to write a number the
    // blocks do not agree with. A pinned output is keyed by its display label,
    // the name the caller pinned it under.
    {
        use std::io::Write;
        let json_path = output_dir.join(format!("{stem}.pinout.json"));
        let mut json = std::fs::File::create(&json_path).expect("failed to create the pinout json");
        let entry = |key: &str,
                     (x, y, z): (i32, i32, i32),
                     pin: Option<PortPin>,
                     role: PortRole| match pin {
            Some(pin) => {
                let handover = handover_in_the_world(&compiled.world, key, (x, y, z), pin, role)
                    .unwrap_or_else(|why| {
                        panic!("the pinout would describe a world that did not ship: {why}")
                    });
                format!(
                    "\"{key}\":{{\"at\":[{x},{y},{z}],\"toward\":\"{}\",\"handover\":[{},{},{}]}}",
                    facing_name(pin.toward),
                    handover.x,
                    handover.y,
                    handover.z
                )
            }
            None => format!("\"{key}\":[{x},{y},{z}]"),
        };
        let inputs = compiled
            .input_positions
            .iter()
            .map(|(name, &position)| entry(name, position, placements.get(name), PortRole::Input))
            .collect::<Vec<_>>()
            .join(",");
        let label_of: std::collections::BTreeMap<&str, &str> = output_labels
            .iter()
            .map(|(label, signal)| (signal.as_str(), label.as_str()))
            .collect();
        let outputs = compiled
            .output_positions
            .iter()
            .map(|(signal, &position)| {
                let pin = placements.get(signal);
                let key = match pin {
                    Some(_) => label_of
                        .get(signal.as_str())
                        .copied()
                        .unwrap_or(signal.as_str()),
                    None => signal.as_str(),
                };
                entry(key, position, pin, PortRole::Output)
            })
            .collect::<Vec<_>>()
            .join(",");
        write!(
            json,
            "{{\"inputs\":{{{inputs}}},\"outputs\":{{{outputs}}}}}"
        )
        .expect("failed to write the pinout json");
        println!("wrote {}", json_path.display());
    }

    println!("circuit: {name}");
    println!("bounding box: {size_x} x {size_y} x {size_z}");
    println!("non-air blocks: {non_air_blocks}");
    println!("gate count: {gate_count}");
    println!("wrote {}", output_path.display());

    print_pinout(&compiled, &output_labels, &placements);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The spec's own example shape parses: both sections, any of the four
    /// horizontal facings, negative-free coordinates as written.
    #[test]
    fn a_well_formed_pins_file_parses_into_both_sections() {
        let pins = parse_pins_file(
            r#"{"inputs": {"d0": {"at": [3, 1, 10], "toward": "south"},
                           "d1": {"at": [5, 1, 10], "toward": "south"}},
                "outputs": {"a": {"at": [4, 1, 2], "toward": "north"}}}"#,
        )
        .expect("the spec's example shape is lawful");

        assert_eq!(pins.inputs.len(), 2);
        assert_eq!(pins.outputs.len(), 1);
        let (name, pin) = &pins.inputs[0];
        assert_eq!(name, "d0");
        assert_eq!(pin.at, Anchor { x: 3, y: 1, z: 10 });
        assert_eq!(pin.toward, Facing::South);
        let (label, pin) = &pins.outputs[0];
        assert_eq!(label, "a");
        assert_eq!(pin.toward, Facing::North);
    }

    /// A section may be absent (pin only outputs, only inputs, or nothing);
    /// keys may come in either order inside a pin.
    #[test]
    fn missing_sections_and_reordered_pin_keys_are_lawful() {
        let pins = parse_pins_file(r#"{"outputs": {"y": {"toward": "west", "at": [7, 2, 0]}}}"#)
            .expect("a file that only pins outputs is lawful");
        assert!(pins.inputs.is_empty());
        assert_eq!(pins.outputs[0].1.at, Anchor { x: 7, y: 2, z: 0 });
        assert_eq!(pins.outputs[0].1.toward, Facing::West);

        assert!(parse_pins_file("{}")
            .expect("an empty object pins nothing")
            .inputs
            .is_empty());
    }

    /// Every malformation is refused with a message that names it -- the
    /// file is hand-written, so "bad json" alone would send the author
    /// hunting through the whole thing.
    #[test]
    fn pins_file_defects_are_named() {
        for (broken, must_mention) in [
            // Not an object at all.
            ("[]", "'{'"),
            // A section this format does not have.
            (r#"{"outpts": {}}"#, "outpts"),
            // A facing that is not one of the four horizontal names.
            (
                r#"{"inputs": {"a": {"at": [1,1,1], "toward": "up"}}}"#,
                "up",
            ),
            (
                r#"{"inputs": {"a": {"at": [1,1,1], "toward": "North"}}}"#,
                "North",
            ),
            // A pin missing one of its two required keys.
            (r#"{"inputs": {"a": {"at": [1,1,1]}}}"#, "toward"),
            (r#"{"inputs": {"a": {"toward": "south"}}}"#, "\"at\""),
            // A key a pin does not have.
            (
                r#"{"inputs": {"a": {"at": [1,1,1], "toward": "south", "colour": "red"}}}"#,
                "colour",
            ),
            // A coordinate that is not a three-integer array.
            (
                r#"{"inputs": {"a": {"at": [1,1], "toward": "south"}}}"#,
                "integer",
            ),
            // The same port pinned twice in one section.
            (
                r#"{"inputs": {"a": {"at": [1,1,1], "toward": "south"},
                               "a": {"at": [4,1,1], "toward": "south"}}}"#,
                "twice",
            ),
            // Content after the closing brace.
            (r#"{} trailing"#, "trailing"),
        ] {
            let error = parse_pins_file(broken).expect_err(&format!("must refuse: {broken}"));
            assert!(
                error.contains(must_mention),
                "the refusal of `{broken}` must mention `{must_mention}`, got: {error}"
            );
        }
    }

    #[test]
    fn duplicate_sections_and_pin_keys_are_refused_with_full_context() {
        for (broken, section, port, key) in [
            (r#"{"inputs": {}, "inputs": {}}"#, "inputs", None, "inputs"),
            (
                r#"{"outputs": {}, "outputs": {}}"#,
                "outputs",
                None,
                "outputs",
            ),
            (
                r#"{"inputs": {"a": {"at": [1,1,1], "at": [2,2,2], "toward": "south"}}}"#,
                "inputs",
                Some("a"),
                "at",
            ),
            (
                r#"{"outputs": {"y": {"at": [1,1,1], "toward": "south", "toward": "north"}}}"#,
                "outputs",
                Some("y"),
                "toward",
            ),
        ] {
            let error = parse_pins_file(broken).expect_err("duplicate JSON keys must be refused");
            assert!(
                error.contains(section),
                "the refusal must name section `{section}`: {error}"
            );
            if let Some(port) = port {
                assert!(
                    error.contains(port),
                    "the refusal must name port `{port}`: {error}"
                );
            }
            assert!(
                error.contains(key),
                "the refusal must name duplicate key `{key}`: {error}"
            );
        }
    }

    /// The pin's port name travels in every defect message, so the author
    /// knows *which* of seven pinned segments was malformed.
    #[test]
    fn a_defective_pin_is_named_by_its_port() {
        let error = parse_pins_file(r#"{"outputs": {"g": {"at": [1,1,1], "toward": "down"}}}"#)
            .expect_err("a vertical toward is refused");
        assert!(
            error.contains('g'),
            "the port name travels with the defect: {error}"
        );
    }

    /// Output pins arrive as display labels and leave as internal signals;
    /// inputs pass through under their own names.
    #[test]
    fn resolve_pins_translates_output_labels_to_internal_signals() {
        let pins = parse_pins_file(
            r#"{"inputs": {"a": {"at": [3, 1, 20], "toward": "south"}},
                "outputs": {"y": {"at": [5, 1, 2], "toward": "north"}}}"#,
        )
        .expect("parses");
        let labels = vec![("y".to_string(), "g6".to_string())];

        let placements = resolve_pins(&pins, &labels).expect("`y` resolves through the labels");
        assert_eq!(
            placements.get("a"),
            Some(PortPin {
                at: Anchor { x: 3, y: 1, z: 20 },
                toward: Facing::South
            })
        );
        assert_eq!(
            placements.get("g6"),
            Some(PortPin {
                at: Anchor { x: 5, y: 1, z: 2 },
                toward: Facing::North
            }),
            "the placement is keyed by the internal signal the label names"
        );
        assert_eq!(
            placements.get("y"),
            None,
            "the display label itself pins nothing"
        );
    }

    /// A world holding exactly one repeater, so the two predicates behind
    /// `handover_in_the_world` have something real to find. `facing` is stored
    /// the way the compiler stores it, which is the field that tells a
    /// *reading* repeater from a *delivering* one.
    fn world_with_a_repeater(at: Anchor, facing: Facing) -> World {
        let mut world = World::new(16, 3, 16);
        let mut repeater = reda::redstone::world::block::BlockState::air();
        repeater.kind = BlockKind::Repeater;
        repeater.facing = Some(facing);
        repeater.delay = 1;
        world.set(at.x, at.y, at.z, repeater);
        world
    }

    /// The sidecar prints the handover the **world** holds, not the one the
    /// pin derives -- and says so when they disagree.
    ///
    /// Both roles resolve to a repeater beside the caller's cell, and with
    /// `toward: north` both happen to store `South`: an input's reader points
    /// its input side back at the caller's cell, an output's deliverer points
    /// its output side into it. That is the one field that keeps the two
    /// predicates from ever answering for each other.
    #[test]
    fn a_reported_handover_is_read_out_of_the_world_and_a_disagreement_is_named() {
        let at = Anchor { x: 4, y: 1, z: 6 };
        let pin = PortPin {
            at,
            toward: Facing::North,
        };

        // An input reads the caller's cell from the north neighbour.
        let reader = Anchor { x: 4, y: 1, z: 5 };
        let world = world_with_a_repeater(reader, Facing::South);
        assert_eq!(
            handover_in_the_world(&world, "d0", (at.x, at.y, at.z), pin, PortRole::Input),
            Ok(reader)
        );
        // The same world says nothing about an output pinned there: a
        // delivering repeater would store the opposite facing.
        assert!(
            handover_in_the_world(&world, "d0", (at.x, at.y, at.z), pin, PortRole::Output).is_err()
        );

        // An output drives the caller's cell from the south neighbour.
        let deliverer = Anchor { x: 4, y: 1, z: 7 };
        let world = world_with_a_repeater(deliverer, Facing::South);
        assert_eq!(
            handover_in_the_world(&world, "y", (at.x, at.y, at.z), pin, PortRole::Output),
            Ok(deliverer)
        );

        // A world with nothing beside the caller's cell is the bug this check
        // exists to make visible, and it names the port and both cells.
        let empty = World::new(16, 3, 16);
        let error = handover_in_the_world(&empty, "y", (at.x, at.y, at.z), pin, PortRole::Output)
            .expect_err("an empty world holds no handover");
        assert!(error.contains('y'), "the refusal names the port: {error}");
        assert!(
            error.contains("(4, 1, 7)"),
            "and the cell the pin named: {error}"
        );

        // So is a recorded position that has drifted off the pin: the sidecar
        // would otherwise print a cell nobody owns.
        let world = world_with_a_repeater(deliverer, Facing::South);
        let error = handover_in_the_world(&world, "y", (9, 1, 9), pin, PortRole::Output)
            .expect_err("a recorded cell that is not the pinned one");
        assert!(
            error.contains("(9, 1, 9)"),
            "the refusal names the drift: {error}"
        );
    }

    /// A label the circuit does not declare is refused by name, with the
    /// labels that would have worked.
    #[test]
    fn an_unresolvable_output_label_is_refused_by_name() {
        let pins = parse_pins_file(r#"{"outputs": {"q": {"at": [5, 1, 2], "toward": "north"}}}"#)
            .expect("parses");
        let labels = vec![("y".to_string(), "g6".to_string())];

        let error = resolve_pins(&pins, &labels).expect_err("`q` labels nothing");
        assert!(error.contains('q'), "the refusal names the label: {error}");
        assert!(
            error.contains('y'),
            "and offers the labels that exist: {error}"
        );
    }
}
