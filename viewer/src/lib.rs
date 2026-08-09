//! WebAssembly bindings that expose `reda`'s redstone simulator to a browser
//! viewer.
//!
//! This crate does not reimplement any redstone behaviour. Every method here
//! is a thin wrapper around `reda::compile::compile` and
//! `reda::redstone::simulator::Simulator` -- if the page shows a signal, it is
//! reading the same simulator state the project's 193 tests run against.
//!
//! **No filesystem, and no subprocess.** Circuits come only from
//! `reda::circuits`: the hand-written generators, which are pure Rust, plus
//! the Verilog-derived ones, which reach this crate as `reda::circuits::
//! verilog`'s *baked* netlists rather than by running Yosys (see
//! [`CIRCUITS`] below, and `VerilogCircuit::baked_netlist` for why a baked
//! copy exists and what keeps it honest). Nothing here calls `reda::formats`
//! (the `.litematic` reader/writer needs `std::fs`, which compiles for
//! `wasm32-unknown-unknown` and then fails at runtime), and nothing here
//! spawns a process.
//!
//! # Testing a `JsValue`-returning method natively
//!
//! `JsValue` (and anything built from it, like the `serde-wasm-bindgen`
//! output of [`Session::pinout`] and [`Session::legend`]) only works inside an
//! actual `wasm32` binary running under a JS host: on a native target,
//! constructing one aborts the process (`wasm-bindgen`'s own glue calls an
//! unimplemented extern). Every other method here returns a plain type
//! (`bool`, `u32`, `String`, `Vec<u8>`, `Vec<i32>`, `Vec<String>`, or a
//! `Result<_, JsValue>` whose `Ok` arm never touches `JsValue`), which is why
//! `tests/and4_truth_table.rs` can drive this API under plain `cargo test`
//! without a browser: it never calls `pinout()` or `legend()`, only
//! `list_circuits`, `Session::new`, `set_lever`, `run_until_stable`, `size`,
//! and `slice`.

use std::collections::{BTreeMap, HashMap};

use reda::circuits::{and4, full_adder, seven_segment, verilog};
use reda::compile::primitive_graph::{self, PrimitiveGraph, Provenance};
use reda::compile::topology::{Library, Primitive as TopoPrimitive, TemplateNode};
use reda::compile::{compile, Netlist};
use reda::redstone::simulator::Simulator;
use reda::redstone::world::block::{BlockKind, BlockState, Face, Facing};
use reda::redstone::world::storage::World;

use serde::Serialize;
use wasm_bindgen::prelude::*;

/// Upper bound on how many game ticks `run_until_stable` will spend before
/// reporting divergence. `tests/reference_circuits.rs` and
/// `tests/seven_segment.rs` both use 2000 for circuits of this same size
/// range; this is a generous multiple of that so a legitimately slow-to-settle
/// circuit does not get misreported as diverged.
const MAX_GAME_TICKS: u64 = 10_000;

// ---------------------------------------------------------------------
// Circuit registry
// ---------------------------------------------------------------------

/// One circuit generator, adapted to a uniform shape: a [`Netlist`] plus its
/// outputs as `(display_name, internal_signal_name)` pairs in the order they
/// should be reported (matters for `full_adder`'s `sum`/`cout` and
/// `seven_segment`'s `a`..`g`).
///
/// Every generator's `netlist.inputs` are already the names a caller should
/// use with `set_lever` -- `and4`'s `a..d`, `full_adder`'s `a, b, cin`,
/// `segment_a`/`seven_segment`'s `d3..d0` -- so inputs need no adapting.
type CircuitBuilder = fn() -> (Netlist, Vec<(String, String)>);

fn and4_adapter() -> (Netlist, Vec<(String, String)>) {
    let (netlist, output) = and4::build_and4_netlist();
    (netlist, vec![(and4::OUTPUT_NAME.to_string(), output)])
}

fn full_adder_adapter() -> (Netlist, Vec<(String, String)>) {
    let (netlist, output_signal) = full_adder::build_full_adder_netlist();
    let outputs = full_adder::OUTPUT_NAMES
        .iter()
        .map(|&name| (name.to_string(), output_signal[name].clone()))
        .collect();
    (netlist, outputs)
}

fn segment_a_adapter() -> (Netlist, Vec<(String, String)>) {
    // Segment index 0 is "a" in `seven_segment::SEGMENT_NAMES`, matching
    // `tests/reference_circuits.rs`'s `the_compiled_segment_a_matches_its_truth_table`.
    let (netlist, output) = seven_segment::build_single_segment_netlist(0);
    (netlist, vec![(seven_segment::SEGMENT_NAMES[0].to_string(), output)])
}

fn seven_segment_adapter() -> (Netlist, Vec<(String, String)>) {
    let (netlist, segment_signal) = seven_segment::build_seven_segment_netlist();
    let outputs = seven_segment::SEGMENT_NAMES
        .iter()
        .map(|&name| (name.to_string(), segment_signal[name].clone()))
        .collect();
    (netlist, outputs)
}

/// The hand-written circuits `Session::new` accepts, in the order
/// `list_circuits` reports them (`reda::circuits`' own size ladder).
const CIRCUITS: &[(&str, CircuitBuilder)] = &[
    ("and4", and4_adapter),
    ("full_adder", full_adder_adapter),
    ("segment_a", segment_a_adapter),
    ("seven_segment", seven_segment_adapter),
];

/// Build the netlist for one of `list_circuits()`'s names, from whichever of
/// the two catalogs it belongs to.
///
/// # Two catalogs, one list
///
/// `reda::circuits::verilog` deliberately keeps the Verilog-derived circuits
/// out of the binaries' `available_circuits()`, because synthesizing one is
/// fallible (Yosys may be absent) and slow (a Python + WASM-Yosys run) in a
/// way no hand-written circuit is. Neither reason survives the trip into
/// this crate: nothing here synthesizes anything. A wasm build cannot run
/// Yosys at all, so it loads the netlist Yosys already produced -- embedded,
/// checked in, and re-derived from source by a test on every machine that
/// *can* run Yosys (see `VerilogCircuit::baked_netlist`). Parsing that is as
/// fast and as infallible as calling `build_and4_netlist()`.
///
/// What does survive is the naming: `verilog:seven_segment` and
/// `seven_segment` compute the same function out of entirely different
/// gates, so the `verilog:` prefix is carried through verbatim rather than
/// flattened -- a viewer showing one of the two must be unambiguous about
/// which, for exactly the reason a conformance report must be.
///
/// Resolving straight out of `verilog::CIRCUITS` rather than restating its
/// entries here means a circuit added to that catalog shows up in this
/// viewer with no change to this crate at all.
fn build_named_circuit(name: &str) -> Option<(Netlist, Vec<(String, String)>)> {
    if let Some(&(_, build)) = CIRCUITS.iter().find(|&&(n, _)| n == name) {
        return Some(build());
    }
    verilog::find(name).map(|circuit| circuit.baked_netlist())
}

/// Every circuit name `Session::new` accepts: the hand-written size ladder
/// first, then the Verilog-derived circuits under their own `verilog:`
/// prefixed names.
#[wasm_bindgen]
pub fn list_circuits() -> Vec<String> {
    CIRCUITS
        .iter()
        .map(|&(name, _)| name.to_string())
        .chain(verilog::CIRCUITS.iter().map(|circuit| circuit.name.to_string()))
        .collect()
}

// ---------------------------------------------------------------------
// Block kind legend
// ---------------------------------------------------------------------

/// Every `BlockKind` variant, in declaration order. `slice`'s block-kind byte
/// is this variant's index in this list (equivalently, `kind as u8`) -- see
/// `block_kind_id`. Keeping this list in the same order as the `BlockKind`
/// enum itself means adding a new variant there needs no renumbering here.
const ALL_BLOCK_KINDS: [BlockKind; 20] = [
    BlockKind::Air,
    BlockKind::Solid,
    BlockKind::Glass,
    BlockKind::Slab,
    BlockKind::RedstoneWire,
    BlockKind::Repeater,
    BlockKind::Comparator,
    BlockKind::Torch,
    BlockKind::WallTorch,
    BlockKind::Lever,
    BlockKind::RedstoneBlock,
    BlockKind::Lamp,
    BlockKind::Piston,
    BlockKind::Button,
    BlockKind::PressurePlate,
    BlockKind::WeightedPressurePlate,
    BlockKind::Observer,
    BlockKind::Target,
    BlockKind::DaylightDetector,
    BlockKind::Other,
];

/// `slice`'s block-kind byte for a given `BlockKind`.
///
/// `BlockKind` is a plain fieldless enum with no explicit discriminants, so
/// its variants already number 0..20 in declaration order; casting `as u8`
/// just reads that off. This is the "block kind id" `legend()` documents, and
/// it is stable across calls within one build because it depends only on the
/// enum's declaration order in `reda`, not on which blocks happen to appear in
/// a particular circuit.
fn block_kind_id(kind: BlockKind) -> u8 {
    kind as u8
}

fn block_kind_display_name(kind: BlockKind) -> &'static str {
    match kind {
        BlockKind::Air => "Air",
        BlockKind::Solid => "Solid Block",
        BlockKind::Glass => "Glass",
        BlockKind::Slab => "Slab",
        BlockKind::RedstoneWire => "Redstone Dust",
        BlockKind::Repeater => "Repeater",
        BlockKind::Comparator => "Comparator",
        BlockKind::Torch => "Redstone Torch",
        BlockKind::WallTorch => "Redstone Wall Torch",
        BlockKind::Lever => "Lever",
        BlockKind::RedstoneBlock => "Redstone Block",
        BlockKind::Lamp => "Redstone Lamp",
        BlockKind::Piston => "Piston",
        BlockKind::Button => "Button",
        BlockKind::PressurePlate => "Pressure Plate",
        BlockKind::WeightedPressurePlate => "Weighted Pressure Plate",
        BlockKind::Observer => "Observer",
        BlockKind::Target => "Target",
        BlockKind::DaylightDetector => "Daylight Detector",
        BlockKind::Other => "Other",
    }
}

/// A placeholder display colour for each block kind, as a CSS hex string.
/// These are a reasonable default palette, not a design requirement -- the
/// page consuming `legend()` is free to recolour by `id` however it likes;
/// what matters is that the id-to-block-kind mapping never needs the page to
/// know redstone rules.
fn block_kind_colour(kind: BlockKind) -> &'static str {
    match kind {
        BlockKind::Air => "#00000000",
        BlockKind::Solid => "#8a8a8a",
        BlockKind::Glass => "#cfe8ff",
        BlockKind::Slab => "#b0b0b0",
        BlockKind::RedstoneWire => "#ff3b3b",
        BlockKind::Repeater => "#c9a227",
        BlockKind::Comparator => "#d9a441",
        BlockKind::Torch => "#ff8c00",
        BlockKind::WallTorch => "#ff8c00",
        BlockKind::Lever => "#7a5230",
        BlockKind::RedstoneBlock => "#b30000",
        BlockKind::Lamp => "#ffd54a",
        BlockKind::Piston => "#8d6e46",
        BlockKind::Button => "#a67c52",
        BlockKind::PressurePlate => "#a67c52",
        BlockKind::WeightedPressurePlate => "#a67c52",
        BlockKind::Observer => "#4c4c4c",
        BlockKind::Target => "#c14953",
        BlockKind::DaylightDetector => "#cbb994",
        BlockKind::Other => "#444444",
    }
}

#[derive(Serialize)]
struct LegendEntry {
    id: u8,
    name: String,
    colour: String,
}

// ---------------------------------------------------------------------
// Signal strength
// ---------------------------------------------------------------------

/// The signal strength `slice` reports for one cell.
///
/// Redstone dust (`BlockKind::RedstoneWire`) carries its own analog strength
/// in `BlockState::power` (0-15, maintained by
/// `propagate::recompute_dust_strengths`), so that is reported as-is.
///
/// Everything else is boolean in this viewer: 15 if powered, 0 if not. A
/// block counts as powered if it is lit (`BlockState::lit` -- torches,
/// repeaters, lamps, levers all use this) or if it carries a nonzero analog
/// power of its own (`BlockState::power` -- comparators; `lit` already tracks
/// `power > 0` for them too, see `Simulator::apply_comparator_tick`, so this
/// is really the same check twice for every block kind `compile()` ever
/// emits). This intentionally does not consult `reda`'s block-taxonomy
/// module: every block kind the reference circuits place (`Solid`,
/// `RedstoneWire`, `WallTorch`, `Lever`, `Repeater`, `Lamp`) reports correctly
/// off `power`/`lit` alone, and inventing a parallel notion of "powered" for
/// kinds this viewer never renders (e.g. a constant-source redstone block)
/// would be speculative.
fn signal_strength(state: &BlockState) -> u8 {
    if state.kind == BlockKind::RedstoneWire {
        state.power
    } else if state.power > 0 || state.lit {
        15
    } else {
        0
    }
}

// ---------------------------------------------------------------------
// 3D geometry and per-tick strengths
// ---------------------------------------------------------------------

/// How many bytes [`Session::geometry`] spends on each non-air cell. See that
/// method's doc comment for the field layout.
const GEOMETRY_BYTES_PER_CELL: usize = 10;

/// `geometry()`'s byte for a cell's `facing`. `Facing` has no explicit
/// discriminants and (unlike `BlockKind`) is not exhaustively listed anywhere
/// else in this crate, so this mapping is spelled out by hand rather than
/// cast -- North/South/East/West/Up/Down as 0..6, and 255 as the sentinel for
/// "no facing" (`Option::None`), a value no real `Facing` variant can ever
/// produce. A repeater, comparator or wall torch without a `facing` byte at
/// all would be indistinguishable from one that defaulted to some direction
/// by accident -- 255 makes "no facing" a visible, checkable value instead of
/// a silent default. See `tests/geometry_ordering.rs` for the check.
fn facing_id(facing: Option<Facing>) -> u8 {
    match facing {
        Some(Facing::North) => 0,
        Some(Facing::South) => 1,
        Some(Facing::East) => 2,
        Some(Facing::West) => 3,
        Some(Facing::Up) => 4,
        Some(Facing::Down) => 5,
        None => 255,
    }
}

/// `geometry()`'s byte for a cell's `face` (`Lever`/`Button`'s attachment
/// surface -- see `Face`'s doc comment). Same sentinel convention as
/// [`facing_id`]: 255 means `Option::None`, a value no real `Face` variant can
/// produce.
fn face_id(face: Option<Face>) -> u8 {
    match face {
        Some(Face::Floor) => 0,
        Some(Face::Wall) => 1,
        Some(Face::Ceiling) => 2,
        None => 255,
    }
}

/// Every non-air cell's coordinate, in the fixed order [`Session::geometry`]
/// and [`Session::strengths`] both walk: ascending flat index over `World`'s
/// own internal layout -- `y` outermost, then `z`, then `x` innermost (see
/// the `YZX` layout `World`'s own module doc comment describes and
/// `World::decode` implements). Walking coordinates directly (rather than,
/// say, `World::positions_of` per kind) is what keeps this a single global
/// order across every block kind at once, instead of one grouped by kind.
///
/// This is the one place that order is spelled out in code; both
/// `Session::geometry` and `Session::strengths` call this so they can never
/// disagree with each other about it.
fn non_air_coords(world: &World) -> impl Iterator<Item = (i32, i32, i32)> + '_ {
    let (size_x, size_y, size_z) = world.size();
    (0..size_y).flat_map(move |y| {
        (0..size_z).flat_map(move |z| {
            (0..size_x).filter_map(move |x| {
                if world.get(x, y, z).kind == BlockKind::Air {
                    None
                } else {
                    Some((x, y, z))
                }
            })
        })
    })
}

// ---------------------------------------------------------------------
// Axis
// ---------------------------------------------------------------------

/// Which axis `slice` holds fixed.
#[wasm_bindgen]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    X = 0,
    Y = 1,
    Z = 2,
}

// ---------------------------------------------------------------------
// Pinout
// ---------------------------------------------------------------------

#[derive(Serialize)]
struct Pin {
    name: String,
    x: i32,
    y: i32,
    z: i32,
}

#[derive(Serialize)]
struct Pinout {
    inputs: Vec<Pin>,
    outputs: Vec<Pin>,
}

// ---------------------------------------------------------------------
// Primitive topology
//
// A JSON view of `compile::primitive_graph::PrimitiveGraph` -- the flat,
// position-free graph of redstone primitives `compile::primitive_graph::expand`
// builds from a `Netlist`. See
// `docs/superpowers/specs/2026-08-08-primitive-level-flow.md`, "The primitive
// level" and "Topology carries no positions": this is connectivity only, no
// coordinates -- a page consuming it has to invent its own layout (a
// force-directed one, in this viewer's case), which is exactly why this is a
// separate view from `slice()`/`geometry()` rather than another overlay on
// them.
//
// Every edge is signal flow now -- there is no more `EdgeKind` to mirror.
// "Rigid" (must become physical adjacency) is a realisation-time idea, not a
// topology one (spec: "There are no rigid edges here"), so a NOR gate is one
// `Torch` node with its inputs landing directly on it; nothing here
// distinguishes edges by kind any more.
//
// These types mirror `Primitive`, `Provenance` and `TemplateNode` as plain,
// `Serialize`-able shadows rather than adding `#[derive(Serialize)]` to
// `src/compile/topology.rs` / `primitive_graph.rs` themselves -- exposing
// this data should not mean changing those modules (see this crate's own
// module doc comment: "This crate does not reimplement any redstone
// behaviour").
// ---------------------------------------------------------------------

fn primitive_name(primitive: TopoPrimitive) -> &'static str {
    match primitive {
        TopoPrimitive::Torch => "Torch",
        TopoPrimitive::Repeater => "Repeater",
        TopoPrimitive::Comparator => "Comparator",
        TopoPrimitive::Lever => "Lever",
        TopoPrimitive::Lamp => "Lamp",
    }
}

/// Deliberately exhaustive, with no wildcard arm: a new `TemplateNode` is a
/// new thing the topology view has to be able to name, and the compiler
/// failing here is how we find that out.
///
/// It has done so twice — `cd186ad` added `SecondTorch`, `a0b485c` added
/// `IsolatingRepeater` — and both times this match was the only thing that
/// noticed, because the root crate's own tests and clippy run pass without
/// ever building this crate. That is the real gap: a task that changes
/// `topology` and checks only the root crate reports clean while leaving the
/// viewer unbuildable.
fn template_role_name(role: TemplateNode) -> String {
    match role {
        TemplateNode::Torch => "Torch".to_string(),
        TemplateNode::SecondTorch => "Torch 2".to_string(),
        TemplateNode::IsolatingRepeater(branch) => format!("Isolating repeater {branch}"),
    }
}

#[derive(Serialize)]
#[serde(tag = "kind")]
enum TopologyProvenance {
    Gate { gate: usize, role: String },
    PrimaryInput { name: String },
    PrimaryOutput { name: String },
}

fn topology_provenance(provenance: &Provenance) -> TopologyProvenance {
    match provenance {
        Provenance::Gate { gate, role } => {
            TopologyProvenance::Gate { gate: *gate, role: template_role_name(*role) }
        }
        Provenance::PrimaryInput { name } => TopologyProvenance::PrimaryInput { name: name.clone() },
        Provenance::PrimaryOutput { name } => TopologyProvenance::PrimaryOutput { name: name.clone() },
    }
}

#[derive(Serialize)]
struct TopologyNode {
    id: usize,
    primitive: &'static str,
    provenance: TopologyProvenance,
}

#[derive(Serialize)]
struct TopologyEdge {
    from: usize,
    to: usize,
}

#[derive(Serialize)]
struct TopologyGate {
    index: usize,
    /// This gate's own output signal name -- `Netlist::Gate::output` --
    /// carried through separately from `PrimitiveGraph`, which only ever
    /// remembers a gate by its index (see `Provenance::Gate`'s doc comment).
    output: String,
    /// `"nor"` or `"merge"` -- `Gate::is_merge`, and the one field that lets
    /// this view stop calling everything a NOR.
    ///
    /// Every hand-written circuit in [`CIRCUITS`] is pure NOR, so until the
    /// Verilog-derived circuits reached this crate there was nothing else
    /// for a gate to be and nothing lost by assuming it. A synthesised
    /// netlist is not pure NOR: Yosys `OR2`/`OR3` map onto wire merges, 11
    /// of `verilog:seven_segment`'s 31 gates. A merge is a fundamentally
    /// different object -- no torch, no support, no gate body at all, just
    /// the point where several producers' dust are allowed to touch -- and
    /// a view that draws it as one more NOR is not simplifying, it is
    /// wrong.
    kind: &'static str,
    arity: usize,
    /// Topological layer: 0 for a gate driven only by primary inputs, one
    /// more than the greatest layer among any gate that drives it otherwise.
    /// A display hint for the topology view's layout only -- `compile()`
    /// never consults this, and it carries no positions of its own, just an
    /// ordering. See [`compute_gate_layers`].
    layer: i32,
    /// Every node id instantiated for this gate -- `PrimitiveGraph::gate_nodes[index]`
    /// verbatim -- so a page can group and hull them without recomputing the
    /// grouping from `nodes`' own provenance.
    ///
    /// **Empty for a merge whose every branch is bare**: such a merge
    /// instantiates no primitive whatsoever. A page that draws gates by
    /// hulling this field draws nothing at all for one -- which is what
    /// [`TopologyGate::outputs`] is for. (Every merge in
    /// `verilog:seven_segment` happens to have at least one isolated branch,
    /// and so at least one repeater here; the all-bare case is nonetheless
    /// one the compiler really produces, and
    /// `primitive_graph::tests::an_all_bare_merge_owns_no_nodes_but_still_
    /// names_the_ones_its_dust_joins` is where it is pinned down.)
    nodes: Vec<usize>,
    /// The nodes a consumer of this gate actually reads its value from --
    /// `PrimitiveGraph::output_nodes[index]` verbatim.
    ///
    /// For a NOR this is the same single torch already in `nodes`. For a
    /// merge it is the set of nodes whose dust physically joins to form the
    /// merge's net: each bare branch's own producer (recursively, through
    /// chained bare merges), each isolated branch's own repeater. That set
    /// is the only honest answer to "where is this gate" for a gate that is
    /// a junction rather than a body, and it is what a view needs in order
    /// to draw the junction at all.
    outputs: Vec<usize>,
}

#[derive(Serialize)]
struct Topology {
    nodes: Vec<TopologyNode>,
    edges: Vec<TopologyEdge>,
    gates: Vec<TopologyGate>,
}

/// Topological layer of each gate in `netlist.gates`' own indexing: 0 for a
/// gate driven only by primary inputs, one more than the greatest layer among
/// its own gate-driven inputs otherwise. Purely a display hint for
/// [`Session::topology`]'s layout -- `compile()` never consults this, unlike
/// `Netlist::topological_order` (an ordering consistent with this layering,
/// but not the same information: two gates can share a layer and still need
/// relative ordering against unrelated gates).
fn compute_gate_layers(netlist: &Netlist) -> Vec<i32> {
    let mut producer_of: HashMap<&str, usize> = HashMap::with_capacity(netlist.gates.len());
    for (g, gate) in netlist.gates.iter().enumerate() {
        producer_of.insert(gate.output.as_str(), g);
    }
    let order = netlist
        .topological_order()
        .expect("compile() already validated this netlist has no combinational cycle");

    let mut layer = vec![0i32; netlist.gates.len()];
    for g in order {
        let mut max_layer = 0i32;
        for input in &netlist.gates[g].inputs {
            if let Some(&producer) = producer_of.get(input.as_str()) {
                max_layer = max_layer.max(layer[producer] + 1);
            }
        }
        layer[g] = max_layer;
    }
    layer
}

// ---------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------

/// One running instance of a circuit: the simulator plus enough bookkeeping
/// to answer `pinout()` and to rebuild the circuit from scratch on `reset()`.
#[wasm_bindgen]
pub struct Session {
    circuit_name: String,
    simulator: Simulator,
    /// Signal name -> lever coordinate. Comes straight from
    /// `CompiledCircuit::input_positions`; every generator's input names
    /// double as the names `set_lever` takes.
    input_positions: BTreeMap<String, (i32, i32, i32)>,
    /// `(display_name, lamp coordinate)`, in each circuit's declared output
    /// order (not alphabetical -- `full_adder`'s `sum` before `cout`,
    /// `seven_segment`'s `a` before `b`, etc).
    output_positions: Vec<(String, (i32, i32, i32))>,
    /// The primitive-topology expansion of this circuit's own netlist --
    /// connectivity only, no positions (see this module's "Primitive
    /// topology" section and `compile::primitive_graph`'s own doc comment).
    /// Built once alongside `simulator` and never touched afterwards: unlike
    /// the world, nothing about a circuit's topology changes as the
    /// simulation runs.
    primitive_graph: PrimitiveGraph,
    /// Everything about each gate that `PrimitiveGraph` does not itself
    /// remember, indexed exactly as `primitive_graph`'s own
    /// `Provenance::Gate::gate`, `gate_nodes` and `output_nodes` are.
    gate_meta: Vec<GateMeta>,
}

/// The per-gate facts `Session::topology` reports that live in the `Netlist`
/// rather than in the expanded graph -- see [`TopologyGate`], whose fields
/// these become.
struct GateMeta {
    output: String,
    arity: usize,
    is_merge: bool,
    layer: i32,
}

impl Session {
    /// Build (or rebuild, for `reset`) a session from scratch: look up the
    /// circuit's generator, compile it, and settle the simulator once so a
    /// caller sees a self-consistent world before ever calling `step`.
    fn build(circuit_name: &str) -> Result<Session, String> {
        let (netlist, outputs) = build_named_circuit(circuit_name).ok_or_else(|| {
            format!(
                "unknown circuit `{circuit_name}` -- see list_circuits() for the valid names"
            )
        })?;
        let compiled =
            compile(&netlist).map_err(|error| format!("compile() failed: {error:?}"))?;

        // Every circuit `build_named_circuit` can return went through
        // `NetlistBuilder` -- the hand-written ones directly, the Verilog
        // ones via the genlib frontend -- which enforces fan-in 1..=3 before
        // a `Gate` ever exists, the same range `Library::default_library`
        // covers. `compile()` above just succeeded, which means every input
        // and output name it checks (`CompileError::UndrivenSignal`) already
        // resolves, and that it found no combinational cycle either -- the
        // third thing `expand` re-checks now that a merge forces it to walk
        // gates in topological order. So `expand` has no way to fail here
        // that `compile` would not already have caught; see
        // `topology::Library`'s doc comment and `primitive_graph::expand`'s
        // `ExpandError` for what it does check.
        let primitive_graph = primitive_graph::expand(&netlist, &Library::default_library())
            .unwrap_or_else(|error| {
                panic!("expand() failed on a netlist compile() already accepted: {error}")
            });
        let gate_meta: Vec<GateMeta> = {
            let layers = compute_gate_layers(&netlist);
            netlist
                .gates
                .iter()
                .zip(layers)
                .map(|(gate, layer)| GateMeta {
                    output: gate.output.clone(),
                    arity: gate.inputs.len(),
                    is_merge: gate.is_merge,
                    layer,
                })
                .collect()
        };

        let output_positions = outputs
            .into_iter()
            .map(|(display_name, signal_name)| {
                let position = *compiled.output_positions.get(&signal_name).unwrap_or_else(|| {
                    panic!(
                        "compile() must place every output this generator declared; \
                         missing `{signal_name}`"
                    )
                });
                (display_name, position)
            })
            .collect();

        let mut simulator = Simulator::new(compiled.world);
        // `Simulator::new` already settles dust strengths, but every lever
        // starts off (`lever(false)` in `src/compile/mod.rs`) and torches are
        // laid out already self-consistent with that -- so nothing is
        // actually mismatched yet. Running to stability here just keeps the
        // guarantee explicit rather than incidental: a freshly built session
        // is fully settled before a caller ever reads it.
        simulator
            .run_until_stable(MAX_GAME_TICKS)
            .map_err(|error| format!("initial settle failed: {error:?}"))?;

        Ok(Session {
            circuit_name: circuit_name.to_string(),
            simulator,
            input_positions: compiled.input_positions,
            output_positions,
            primitive_graph,
            gate_meta,
        })
    }
}

#[wasm_bindgen]
impl Session {
    /// Build a session for one of `list_circuits()`'s names. Throws if the
    /// name is not recognised or the netlist fails to compile.
    #[wasm_bindgen(constructor)]
    pub fn new(circuit_name: &str) -> Result<Session, JsValue> {
        Session::build(circuit_name).map_err(|error| JsValue::from_str(&error))
    }

    /// World size as `[x, y, z]`.
    pub fn size(&self) -> Vec<i32> {
        let (x, y, z) = self.simulator.world().size();
        vec![x, y, z]
    }

    /// `{ inputs: [{name, x, y, z}], outputs: [{name, x, y, z}] }`.
    ///
    /// Only meaningful when called from JS -- see the module doc comment on
    /// why a native `cargo test` cannot call this.
    pub fn pinout(&self) -> JsValue {
        let inputs = self
            .input_positions
            .iter()
            .map(|(name, &(x, y, z))| Pin { name: name.clone(), x, y, z })
            .collect();
        let outputs = self
            .output_positions
            .iter()
            .map(|&(ref name, (x, y, z))| Pin { name: name.clone(), x, y, z })
            .collect();
        serde_wasm_bindgen::to_value(&Pinout { inputs, outputs })
            .expect("Pinout serializes without error -- it is plain strings and integers")
    }

    /// `[{id, name, colour}]` for every block kind `reda` knows about, so a
    /// page can colour `slice`'s block-kind byte without knowing any redstone
    /// rules. Same native-test caveat as `pinout`.
    pub fn legend(&self) -> JsValue {
        let entries: Vec<LegendEntry> = ALL_BLOCK_KINDS
            .iter()
            .map(|&kind| LegendEntry {
                id: block_kind_id(kind),
                name: block_kind_display_name(kind).to_string(),
                colour: block_kind_colour(kind).to_string(),
            })
            .collect();
        serde_wasm_bindgen::to_value(&entries)
            .expect("LegendEntry serializes without error -- it is plain strings and integers")
    }

    /// Set one input lever on or off. Does not advance the simulator --
    /// follow with `step()` or `run_until_stable()` to see the effect. Throws
    /// if `name` is not one of this circuit's inputs.
    pub fn set_lever(&mut self, name: &str, on: bool) -> Result<(), JsValue> {
        let &(x, y, z) = self.input_positions.get(name).ok_or_else(|| {
            JsValue::from_str(&format!(
                "no input named `{name}` on circuit `{}`",
                self.circuit_name
            ))
        })?;
        let mut state = self.simulator.world().get(x, y, z).clone();
        state.lit = on;
        self.simulator.world_mut().set(x, y, z, state);
        Ok(())
    }

    /// Advance exactly one game tick. Returns whether anything changed.
    pub fn step(&mut self) -> bool {
        self.simulator.step() > 0
    }

    /// Run until nothing is left to schedule, or `MAX_GAME_TICKS` is
    /// exhausted. Returns the number of game ticks it took. Throws
    /// (`SimulationError::Diverged` or `UnsupportedComponent`, formatted with
    /// `{:?}`) rather than silently returning a stale reading.
    pub fn run_until_stable(&mut self) -> Result<u32, JsValue> {
        self.simulator
            .run_until_stable(MAX_GAME_TICKS)
            .map(|ticks| ticks as u32)
            .map_err(|error| JsValue::from_str(&format!("{error:?}")))
    }

    /// How many game ticks this session has run so far.
    pub fn tick_count(&self) -> u32 {
        self.simulator.current_tick() as u32
    }

    /// Rebuild the circuit from scratch, discarding every input change and
    /// resetting the tick count to 0.
    pub fn reset(&mut self) -> Result<(), JsValue> {
        *self = Session::build(&self.circuit_name).map_err(|error| JsValue::from_str(&error))?;
        Ok(())
    }

    /// One 2D slice of the world, two bytes per cell: `[block_kind_id,
    /// signal_strength]`.
    ///
    /// `axis` names the axis held fixed at `index`; the other two axes sweep
    /// the slice, **always in ascending axis order (X before Y before Z) and
    /// always row-major with the later axis fastest**:
    ///
    /// - `Axis::X`: outer loop `y` in `0..size_y`, inner loop `z` in
    ///   `0..size_z`. Cell `(y, z)` is at byte offset `2 * (y * size_z + z)`.
    /// - `Axis::Y`: outer loop `x` in `0..size_x`, inner loop `z` in
    ///   `0..size_z`. Cell `(x, z)` is at byte offset `2 * (x * size_z + z)`.
    /// - `Axis::Z`: outer loop `x` in `0..size_x`, inner loop `y` in
    ///   `0..size_y`. Cell `(x, y)` is at byte offset `2 * (x * size_y + y)`.
    ///
    /// Throws if `index` is out of range for `axis`.
    pub fn slice(&self, axis: Axis, index: i32) -> Result<Vec<u8>, JsValue> {
        let world = self.simulator.world();
        let (size_x, size_y, size_z) = world.size();

        let in_range = match axis {
            Axis::X => index >= 0 && index < size_x,
            Axis::Y => index >= 0 && index < size_y,
            Axis::Z => index >= 0 && index < size_z,
        };
        if !in_range {
            return Err(JsValue::from_str(&format!(
                "index {index} out of range for axis {axis:?} (world size is {size_x}x{size_y}x{size_z})"
            )));
        }

        let (outer_len, inner_len) = match axis {
            Axis::X => (size_y, size_z),
            Axis::Y => (size_x, size_z),
            Axis::Z => (size_x, size_y),
        };

        let mut bytes = Vec::with_capacity((outer_len as usize) * (inner_len as usize) * 2);
        for outer in 0..outer_len {
            for inner in 0..inner_len {
                let (x, y, z) = match axis {
                    Axis::X => (index, outer, inner),
                    Axis::Y => (outer, index, inner),
                    Axis::Z => (outer, inner, index),
                };
                let state = world.get(x, y, z);
                bytes.push(block_kind_id(state.kind));
                bytes.push(signal_strength(state));
            }
        }
        Ok(bytes)
    }

    /// Static per-circuit geometry for a 3D view: one entry per non-air cell,
    /// visited in [`non_air_coords`]'s order, packed as
    /// [`GEOMETRY_BYTES_PER_CELL`] (10) bytes each:
    ///
    /// `[x_lo, x_hi, y_lo, y_hi, z_lo, z_hi, kind, facing, face, delay]`
    ///
    /// `x`/`y`/`z` are little-endian `u16` world coordinates -- `u16`, not
    /// `u8`, because `seven_segment`'s Z axis reaches 298, past a single
    /// byte's range -- and `kind` is the same "block kind id" `slice()` and
    /// `legend()` already use (`block_kind_id`, i.e. `BlockKind as u8`).
    ///
    /// `facing` ([`facing_id`]) and `face` ([`face_id`]) carry `BlockState`'s
    /// own `Option<Facing>`/`Option<Face>` verbatim, each as 0..6 (or 0..3 for
    /// `face`) for the real variants and **255 for `None`** -- a repeater,
    /// comparator or wall torch's direction (and a lever's attachment
    /// surface) is load-bearing for reading the circuit correctly (see the
    /// module-level plan doc's "Block shapes and orientation" section), so
    /// this is a real field with a real "absent" value, not something a 3D
    /// view can silently default and still look plausible.
    ///
    /// `delay` carries `BlockState::delay` verbatim: 1..=4 for a repeater,
    /// 0 for every other block kind. Unlike `facing`/`face`, this needs no
    /// 255 sentinel -- `delay`'s own "not applicable" value (0) already never
    /// collides with a real repeater delay (which starts at 1), so the raw
    /// byte is unambiguous as-is. See the module-level plan doc's "Repeater
    /// delay" section: a view renders this as the gap between a repeater's
    /// two nubs, the same way Minecraft itself shows it.
    ///
    /// A caller builds this **once per circuit**: the set of non-air cells
    /// and their coordinates/kinds/facing/face/delay is fixed the moment
    /// `compile()` lays the world out, and never changes as the simulation
    /// runs (only *strengths* change -- see [`Session::strengths`]).
    ///
    /// Entry `i` here and byte `i` of `strengths()` describe the very same
    /// cell -- this pairing, and [`non_air_coords`]'s order, is exactly what
    /// `tests/geometry_ordering.rs` pins down (facing, face and delay
    /// included). If the two ever drifted out of order relative to each
    /// other, a 3D view would still render a plausible-looking circuit, just
    /// with every colour (and orientation) attached to the wrong block, and
    /// nothing on screen would say so.
    pub fn geometry(&self) -> Vec<u8> {
        let world = self.simulator.world();
        let mut bytes = Vec::new();
        for (x, y, z) in non_air_coords(world) {
            let state = world.get(x, y, z);
            let mut cell = [0u8; GEOMETRY_BYTES_PER_CELL];
            cell[0..2].copy_from_slice(&(x as u16).to_le_bytes());
            cell[2..4].copy_from_slice(&(y as u16).to_le_bytes());
            cell[4..6].copy_from_slice(&(z as u16).to_le_bytes());
            cell[6] = block_kind_id(state.kind);
            cell[7] = facing_id(state.facing);
            cell[8] = face_id(state.face);
            cell[9] = state.delay;
            bytes.extend_from_slice(&cell);
        }
        bytes
    }

    /// `{ nodes: [{id, primitive, provenance}], edges: [{from, to}],
    /// gates: [{index, output, kind, arity, layer, nodes, outputs}] }` --
    /// the whole primitive-topology graph this circuit's netlist expands
    /// into, per this module's "Primitive topology" section. No positions:
    /// the graph itself carries none (see `compile::primitive_graph`'s doc
    /// comment), so a page consuming this has to lay it out itself. Same
    /// native-test caveat as `pinout`/`legend`.
    ///
    /// A gate's `kind` and `outputs` are what let a page draw a wire merge
    /// as the junction it is rather than as one more NOR -- see those two
    /// fields on [`TopologyGate`]. `nodes` alone cannot: a merge with only
    /// bare branches owns no node at all.
    pub fn topology(&self) -> JsValue {
        let nodes = self
            .primitive_graph
            .nodes
            .iter()
            .enumerate()
            .map(|(id, node)| TopologyNode {
                id,
                primitive: primitive_name(node.primitive),
                provenance: topology_provenance(&node.provenance),
            })
            .collect();
        let edges =
            self.primitive_graph.edges.iter().map(|edge| TopologyEdge { from: edge.from, to: edge.to }).collect();
        let gates = self
            .gate_meta
            .iter()
            .zip(self.primitive_graph.gate_nodes.iter())
            .zip(self.primitive_graph.output_nodes.iter())
            .enumerate()
            .map(|(index, ((meta, nodes), outputs))| TopologyGate {
                index,
                output: meta.output.clone(),
                kind: if meta.is_merge { "merge" } else { "nor" },
                arity: meta.arity,
                layer: meta.layer,
                nodes: nodes.clone(),
                outputs: outputs.clone(),
            })
            .collect();
        serde_wasm_bindgen::to_value(&Topology { nodes, edges, gates })
            .expect("Topology serializes without error -- it is plain strings and integers")
    }

    /// One byte per non-air cell, in **exactly** `geometry()`'s order (see
    /// [`non_air_coords`]): byte `i` here is
    /// [`signal_strength`] of the cell `geometry()`'s entry `i` describes.
    ///
    /// Call this **every tick**. Unlike `geometry()`, this is the only thing
    /// that changes as the simulation runs -- a step or a lever flip never
    /// adds or removes a block, only changes what's lit or how strong the
    /// dust is -- so a caller can keep `geometry()`'s buffer (and whatever
    /// per-instance transform it built from it) fixed for the session's
    /// whole lifetime and just re-upload this one array.
    pub fn strengths(&self) -> Vec<u8> {
        let world = self.simulator.world();
        non_air_coords(world).map(|(x, y, z)| signal_strength(world.get(x, y, z))).collect()
    }
}

// ---------------------------------------------------------------------
// Repeater delay: proof that geometry() carries it, independent of compile()
// ---------------------------------------------------------------------

/// `compile()` hardcodes `delay = 1` on every repeater it places, so no
/// circuit in [`CIRCUITS`] ever exercises anything but delay 1 -- the plan
/// document is explicit that this is not a reason to skip carrying `delay`
/// through `geometry()`. This module is the proof: it builds a `World` by
/// hand (never touching `compile()`), places four otherwise-identical
/// repeaters differing only in `delay` (1, 2, 3 and 4), and checks that
/// `geometry()` reports each one's real, distinct value rather than some
/// constant that would "look plausible" the way `facing`/`face` could if
/// silently defaulted (see [`Session::geometry`]'s doc comment).
///
/// This test is deliberately a unit test inside this module (not a
/// `tests/*.rs` integration test) because it constructs a [`Session`]
/// directly via its private fields, sidestepping [`Session::build`]'s
/// `find_builder`/`compile()` path entirely -- there is no other way to get
/// a repeater with `delay != 1` into a `Session` today, precisely because
/// `compile()` never emits one.
#[cfg(test)]
mod repeater_delay_geometry_tests {
    use super::*;
    use reda::redstone::world::storage::World;

    /// Build a session wrapping a hand-crafted `World` containing one
    /// repeater per delay value 1..=4, laid out along X so each has its own
    /// non-air cell, all sharing the same facing (the facing is irrelevant
    /// to this test; keeping it fixed isolates `delay` as the only thing
    /// that varies between the four cells).
    fn session_with_one_repeater_per_delay() -> Session {
        let mut world = World::new(8, 1, 1);
        for (i, delay) in (1u8..=4).enumerate() {
            let mut state = BlockState::air();
            state.kind = BlockKind::Repeater;
            state.facing = Some(Facing::North);
            state.delay = delay;
            state.name = "minecraft:repeater".to_string();
            world.set(i as i32 * 2, 0, 0, state);
        }
        let simulator = Simulator::new(world);
        // This fixture bypasses `Session::build` entirely (see this module's
        // doc comment), so `primitive_graph`/`gate_meta` need a value from
        // somewhere else -- an empty `Netlist` expands to an empty, but
        // still real, `PrimitiveGraph` rather than needing a second
        // "uninitialised" constructor on that type just for this one test.
        let empty_netlist = Netlist { inputs: Vec::new(), outputs: Vec::new(), gates: Vec::new() };
        let primitive_graph = primitive_graph::expand(&empty_netlist, &Library::default_library())
            .expect("an empty netlist has nothing for expand() to fail on");
        Session {
            circuit_name: "repeater_delay_test_fixture".to_string(),
            simulator,
            input_positions: BTreeMap::new(),
            output_positions: Vec::new(),
            primitive_graph,
            gate_meta: Vec::new(),
        }
    }

    #[test]
    fn geometry_reports_each_repeaters_real_delay_distinctly() {
        let session = session_with_one_repeater_per_delay();
        let bytes = session.geometry();

        assert_eq!(bytes.len() % GEOMETRY_BYTES_PER_CELL, 0);
        let cells: Vec<&[u8]> = bytes.chunks_exact(GEOMETRY_BYTES_PER_CELL).collect();
        assert_eq!(cells.len(), 4, "all four repeaters must be non-air and appear once each");

        // Ascending-X order (see non_air_coords) puts delay 1 first, matching
        // the order they were placed above.
        let delays: Vec<u8> = cells.iter().map(|cell| cell[9]).collect();
        assert_eq!(
            delays,
            vec![1, 2, 3, 4],
            "geometry() must report each repeater's own delay, not a shared/defaulted value"
        );

        // Every kind byte must still read as Repeater -- delay is an
        // additional field, not a replacement for kind.
        for cell in &cells {
            assert_eq!(cell[6], BlockKind::Repeater as u8);
        }
    }
}
