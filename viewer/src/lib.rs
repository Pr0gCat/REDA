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

use std::collections::{BTreeMap, HashMap, HashSet};

use reda::circuits::{and4, full_adder, seven_segment, verilog};
use reda::compile::primitive_graph::{self, PrimitiveGraph, Provenance};
use reda::compile::topology::{Library, Primitive as TopoPrimitive, TemplateNode};
use reda::compile::lowering::{lower_optimised_with_provenance, lower_with_provenance};
use reda::compile::planner::PortPlacements;
use reda::compile::{compile, compile_planned, Netlist};
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
/// Names carrying this prefix are built by `compile_planned` -- the planner
/// choosing its own anchors -- rather than by `compile`.
const PLANNED_PREFIX: &str = "planned:";

/// Names carrying this prefix are PRE-BAKED `compile_grown` circuits: the
/// generation takes minutes natively and would take tens of minutes in wasm,
/// so the page fetches the `.litematic` the `build_circuit --grown` bin
/// wrote, plus its pinout sidecar, and hands both to `Session::from_baked`.
const GROWN_PREFIX: &str = "grown:";

/// A pre-baked circuit's world and port coordinates: what `Session::from_baked`
/// hands `build_inner` in place of a `compile()` run.
type BakedParts = (
    World,
    BTreeMap<String, (i32, i32, i32)>,
    BTreeMap<String, (i32, i32, i32)>,
);

fn build_named_circuit(name: &str) -> Option<(Netlist, Vec<(String, String)>)> {
    let name = name.strip_prefix(PLANNED_PREFIX).unwrap_or(name);
    let name = name.strip_prefix(GROWN_PREFIX).unwrap_or(name);
    if let Some(&(_, build)) = CIRCUITS.iter().find(|&&(n, _)| n == name) {
        return Some(build());
    }
    verilog::find(name).map(|circuit| circuit.baked_netlist())
}

/// Every circuit name `Session::new` accepts: the hand-written size ladder
/// first, then the Verilog-derived circuits under their own `verilog:`
/// prefixed names.
///
/// # The `planned:` entries are gone, and `compile` is why
///
/// They were added when `compile` was always the row/channel/track emitter,
/// so `planned:x` was the only way to see what the planner did on its own.
/// `compile` is now a hybrid -- it tries relaxation placement and falls back
/// -- which turned every one of those four entries into either a duplicate or
/// a trap:
///
/// - `planned:and4` and `planned:full_adder` became **byte-identical to the
///   plain names**, because those circuits now take the planner path through
///   `compile`. Measured in the viewer: same input coordinates, same clip box.
/// - `planned:segment_a` and `planned:seven_segment` **hang the page**. The
///   planner places them and cannot route them, and `compile_planned` spends
///   its whole rip-up budget finding that out -- tens of seconds of frozen UI
///   on a synchronous wasm call, ending in an error message. `compile` reaches
///   the same conclusion in eight rounds and falls back, so the plain names
///   show a circuit instead of a stall.
///
/// So the plain names now show the best layout that exists for each circuit,
/// and there is nothing a `planned:` name could add. If a future change makes
/// the two paths differ again on a circuit that routes, this is the function
/// to put them back in -- `PLANNED_PREFIX` and its branch in `Session::new`
/// are deliberately left in place for that.
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
    /// `"nor"` or `"merge"` -- the two things redstone builds, and the one
    /// field that lets this view stop calling everything a NOR.
    ///
    /// Every hand-written circuit in [`CIRCUITS`] is pure NOR, so until the
    /// Verilog-derived circuits reached this crate there was nothing else
    /// for a gate to be and nothing lost by assuming it. A merge is a
    /// fundamentally different object -- no torch, no support, no gate body
    /// at all, just the point where several producers' dust are allowed to
    /// touch -- and a view that draws it as one more NOR is not
    /// simplifying, it is wrong.
    kind: &'static str,
    /// The **gate-level** cell this torch or merge came from, as
    /// `"<kind> <output>"` -- e.g. `"mux g20"`, `"nand g12"` -- or `None` if
    /// this gate was already realisable in the netlist the session loaded
    /// (every gate of every hand-written circuit).
    ///
    /// This is the one place the two levels meet in this crate. The netlist
    /// the viewer loads for a `verilog:` circuit is at the gate level, and
    /// `compile::lowering::lower` rewrites it into torches and merges before
    /// anything is placed -- so the graph drawn here is, correctly, the
    /// realisation. Without this field that is *all* it could ever be, and
    /// the fact that five of those torches are one multiplexer would be
    /// invisible in the view whose entire job is showing what a circuit
    /// really is.
    source: Option<String>,
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

/// The primitive level of [`Session::topology`]: what the circuit lowers
/// into, and the only level anything is ever placed at.
#[derive(Serialize)]
struct PrimitiveLevel {
    nodes: Vec<TopologyNode>,
    edges: Vec<TopologyEdge>,
    gates: Vec<TopologyGate>,
}

// ---------------------------------------------------------------------
// Gate level
//
// The netlist the session *loaded*, before `lowering::lower` touched it --
// `$_AND_`, `$_NAND_`, `$_MUX_` and the rest, in `GateKind`'s own vocabulary.
//
// This exists because "what is this circuit" and "what did it lower to" are
// different questions, and only the second one had an answer here. The
// primitive graph below is the realisation: correct, necessary for the
// planner, and already what the 2D and 3D views show in more detail. It is
// the wrong default for a view whose entire job is naming what a circuit is.
// `verilog:and4` is three ANDs. Reporting it as nine NORs is not a summary of
// that, it is a different fact about a later stage.
//
// The two levels are related by `lowering::lower_with_provenance`, whose map
// this section inverts ([`CellMeta::lowered`]): every gate-level cell knows
// which realisable gates its own expansion produced, and through them which
// primitive nodes. Nothing is dropped -- the primitive level is still served,
// in full, alongside this.
// ---------------------------------------------------------------------

/// One input pin of a gate-level cell: the signal name it reads, and which
/// other cell drives that signal.
///
/// The driver is resolved here rather than in the page, because "which cell
/// produces `g14`" is a fact about the netlist and a consumer re-deriving it
/// from names is a second, drift-prone copy of it. `cell` is `None` for a
/// primary input, which is the only other thing a signal name can be in a
/// netlist `compile()` accepted (`CompileError::UndrivenSignal` is exactly
/// the case where it is neither).
#[derive(Serialize)]
struct CellInput {
    name: String,
    cell: Option<usize>,
}

/// One gate-level cell -- one entry of the netlist the session loaded.
#[derive(Serialize)]
struct TopologyCell {
    /// Index into the *source* netlist's gates, and this cell's own identity
    /// everywhere in this structure (see [`CellInput::cell`]).
    index: usize,
    /// The signal this cell drives.
    output: String,
    /// [`GateKind::wire_name`] -- `"and"`, `"nand"`, `"mux"`, `"andnot"`,
    /// `"merge"`, `"nor"`, ... The one spelling shared by this view, the
    /// baked netlist files and `mc_dump`, so three readers cannot disagree
    /// about what a gate is.
    kind: &'static str,
    /// This cell's inputs, in the kind's own pin order -- which matters:
    /// `andnot` and `mux` are not symmetric in their pins, so a view that
    /// reorders them is drawing a different circuit.
    inputs: Vec<CellInput>,
    /// Whether redstone builds this kind directly (`GateKind::is_realisable`
    /// -- a NOR or a wire merge). False for every gate-level kind, which is
    /// the same question as "did this cell have to be lowered".
    realisable: bool,
    /// Topological layer among cells, 0 for a cell driven only by primary
    /// inputs. Display hint only, same as [`TopologyGate::layer`] -- but
    /// computed over the *source* netlist, so it is this level's own
    /// layering rather than a projection of the lowered one.
    layer: i32,
    /// Which gates of the lowered netlist this cell's expansion produced --
    /// indices into [`PrimitiveLevel::gates`]. The inverse of
    /// `lower_with_provenance`'s map.
    ///
    /// One entry for a cell that was already realisable. Several for a
    /// gate-level one -- and occasionally fewer than its expansion has
    /// steps, since a shared inverter is credited to whichever cell first
    /// built it (see `lower_with_provenance`'s doc comment). That sharing is
    /// why this is a list per cell rather than a count: the counts do not add
    /// up to the expansion sizes, and pretending otherwise would be a
    /// confidently wrong number.
    lowered: Vec<usize>,
    /// Every primitive node those lowered gates instantiate, in ascending id
    /// order. Empty is possible in principle (a cell lowering to nothing but
    /// all-bare merges); in practice every cell of every circuit here owns at
    /// least one.
    nodes: Vec<usize>,
}

/// One primitive edge that leaves a cell from somewhere other than that
/// cell's own output -- a **shared inverter**, drawn from the torch that
/// really exists to the gate that really reads it.
///
/// `lowering::lower` routes every intermediate one-input NOR through
/// `NetlistBuilder::not`, whose cache makes one torch serve every expansion
/// that wants the same `!x`; the torch is credited to whichever cell built it
/// first, and every later reader gets no gate of its own for it (see
/// `lower_with_provenance`'s doc comment, and
/// `lowering::tests::inverters_are_shared_across_gates_and_with_existing_
/// nor1_gates`).
///
/// A picture that nests primitives inside their cell therefore cannot be
/// drawn from [`TopologyCell::nodes`] alone. `verilog:seven_segment`'s
/// `andnot g3` owns exactly one torch and reads a second one -- `!d1` -- from
/// inside `nand g1`'s box. Drawing that torch inside `g1` and saying nothing
/// makes `g3` look like a one-torch cell that reads `d1` directly, which is
/// not what gets built. So the crossing is a first-class thing the view is
/// handed, rather than something a page has to notice on its own.
///
/// # What counts as one
///
/// Exactly: an edge whose two endpoint nodes belong to different cells, and
/// whose source is **not** one of the nodes a consumer of the owning cell
/// reads it from (`PrimitiveGraph::output_nodes` of the lowered gate carrying
/// that cell's declared output). Every other cell-crossing edge is already
/// explained by a gate-level edge the picture draws anyway -- cell A drives
/// signal `A.output`, cell B reads it -- possibly through an all-bare merge
/// that owns no node of its own, which is why the rule is stated on the
/// terminal node set rather than on "is the source cell adjacent".
#[derive(Serialize, Debug)]
struct SharedEdge {
    /// Node id inside `owner`, interior to it: a torch that is not what a
    /// reader of `owner`'s own output signal would read.
    from: usize,
    /// Node id inside `reader`.
    to: usize,
    /// Index of the cell credited with building `from`.
    owner: usize,
    /// Index of the cell whose expansion reads it.
    reader: usize,
}

/// The gate level of [`Session::topology`]: the circuit as it was written.
#[derive(Serialize)]
struct GateLevel {
    cells: Vec<TopologyCell>,
    /// Primary input names, in the netlist's own order.
    inputs: Vec<String>,
    /// Primary outputs: the signal name, and the cell driving it.
    outputs: Vec<CellInput>,
    /// Every shared inverter, as the edge it really is -- see [`SharedEdge`].
    /// Empty for a circuit whose cells each built their own (`verilog:and4`),
    /// and for every hand-written circuit, where lowering is the identity and
    /// there are no expansions to share between.
    shared_edges: Vec<SharedEdge>,
}

#[derive(Serialize)]
struct Topology {
    gate_level: GateLevel,
    primitive_level: PrimitiveLevel,
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
    /// The **gate-level** netlist this session loaded, before
    /// `lowering::lower` rewrote it -- kept whole rather than shadowed,
    /// because it is the answer to "what is this circuit" and every field
    /// [`Session::topology`] reports about a cell is read straight off it.
    /// For a hand-written circuit this is already realisable and lowering is
    /// the identity, so the two levels coincide; that is the truth for those
    /// circuits, not something to dress up.
    source_netlist: Netlist,
    /// Per source-gate facts, indexed as `source_netlist.gates` is.
    cell_meta: Vec<CellMeta>,
}

/// The per-cell facts [`Session::topology`]'s gate level reports that are not
/// in `source_netlist` itself -- see [`TopologyCell`], whose fields these
/// become.
struct CellMeta {
    /// See [`TopologyCell::layer`].
    layer: i32,
    /// See [`TopologyCell::lowered`] -- `lower_with_provenance`'s map,
    /// inverted.
    lowered: Vec<usize>,
    /// The lowered gates that carry this source cell's physical output rail.
    terminals: Vec<usize>,
}

/// The per-gate facts `Session::topology` reports that live in the `Netlist`
/// rather than in the expanded graph -- see [`TopologyGate`], whose fields
/// these become.
struct GateMeta {
    output: String,
    arity: usize,
    is_merge: bool,
    /// See [`TopologyGate::source`]. `None` when the source gate was already
    /// realisable, since "this NOR came from that NOR" says nothing.
    source: Option<String>,
    layer: i32,
}

fn source_terminals_by_declared_output(
    source_netlist: &Netlist,
    lowered_netlist: &Netlist,
    provenance: &[usize],
) -> Vec<Vec<usize>> {
    let mut terminals = vec![Vec::new(); source_netlist.gates.len()];
    for (lowered, (&source, gate)) in provenance.iter().zip(&lowered_netlist.gates).enumerate() {
        if gate.output == source_netlist.gates[source].output {
            terminals[source].push(lowered);
        }
    }
    assert!(terminals.iter().all(|terminals| !terminals.is_empty()));
    terminals
}

impl Session {
    /// Build (or rebuild, for `reset`) a session from scratch: look up the
    /// circuit's generator, compile it, and settle the simulator once so a
    /// caller sees a self-consistent world before ever calling `step`.
    fn build(circuit_name: &str) -> Result<Session, String> {
        Session::build_inner(circuit_name, None)
    }

    fn build_inner(
        circuit_name: &str,
        baked: Option<BakedParts>,
    ) -> Result<Session, String> {
        let base_name = circuit_name.strip_prefix(GROWN_PREFIX).unwrap_or(circuit_name);
        let (source_netlist, outputs) = build_named_circuit(circuit_name).ok_or_else(|| {
            format!(
                "unknown circuit `{circuit_name}` -- see list_circuits() for the valid names"
            )
        })?;

        // Lower once, here, and use the result for everything below.
        //
        // A `verilog:` circuit's baked netlist is at the **gate level**
        // (`$_AND_`, `$_NAND_`, `$_MUX_`); the hand-written ones are already
        // NOR gates and merges, on which this is the identity. Doing it once
        // matters more than it looks: `compiled`, `primitive_graph` and
        // `gate_meta` below are all correlated by gate index and signal
        // name, so they have to come from one and the same netlist. That is
        // exactly why `compile` refuses to lower on its own -- see its own
        // doc comment.
        let (netlist, provenance, source_terminals) = if verilog::find(base_name).is_some() {
            lower_optimised_with_provenance(&source_netlist).map(|lowered| {
                (lowered.netlist, lowered.provenance, lowered.source_terminals)
            })
        } else {
            lower_with_provenance(&source_netlist).map(|(netlist, provenance)| {
                let source_terminals = source_terminals_by_declared_output(&source_netlist, &netlist, &provenance);
                (netlist, provenance, source_terminals)
            })
        }
        .map_err(|error| format!("lowering failed: {error}"))?;
        // A `planned:` name asks for the layout the planner produced on its
        // own, rather than the row/channel/track one it merely realises and
        // checks. They are different circuits computing the same function,
        // which is the whole point of being able to look at both.
        let (world, input_positions, output_positions_by_signal) = match baked {
            // A pre-baked world skips compilation entirely: the litematic IS
            // the circuit `compile_grown` produced, and the sidecar carries
            // the lever and lamp coordinates the schematic cannot.
            Some(parts) => parts,
            None => {
        let compiled = if circuit_name.starts_with(PLANNED_PREFIX) {
            // Nothing is pinned here: the viewer's job is to show what the
            // planner does when it is left to decide.
            //
            // `{error}`, not `{error:?}`: `CompileError` writes a sentence
            // saying what failed and why -- "the geometry is structurally
            // connected but the real, decayed signal dies out before it
            // arrives" -- and the derived Debug throws all of that away for a
            // struct dump. This page is where a person reads these.
            compile_planned(&netlist, &PortPlacements::default())
                .map_err(|error| format!("the planner could not build this circuit: {error}"))?
        } else {
            compile(&netlist).map_err(|error| format!("this circuit does not compile: {error}"))?
        };
                (compiled.world, compiled.input_positions, compiled.output_positions)
            }
        };

        // Every gate in `netlist` is a NOR or a merge of fan-in 1..=3 --
        // `lower` produces nothing else, and `compile()` above rejects
        // anything that is not -- which is the same range
        // `Library::default_library` covers. `compile()` succeeding also
        // means every input
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
                .enumerate()
                .zip(layers)
                .map(|((index, gate), layer)| {
                    let origin = &source_netlist.gates[provenance[index]];
                    GateMeta {
                        output: gate.output.clone(),
                        arity: gate.inputs.len(),
                        is_merge: gate.is_merge(),
                        source: (!origin.kind.is_realisable())
                            .then(|| format!("{} {}", origin.kind.wire_name(), origin.output)),
                        layer,
                    }
                })
                .collect()
        };

        // The gate level, indexed as `source_netlist.gates` is.
        //
        // `compute_gate_layers` on the *source* netlist is sound for the same
        // reason it is on the lowered one: lowering rewrites each gate into
        // an expansion but never introduces a dependency between two source
        // gates that was not already there, so the lowered netlist being
        // acyclic (which `compile()` above checked) makes this one acyclic
        // too.
        let cell_meta: Vec<CellMeta> = {
            let layers = compute_gate_layers(&source_netlist);
            let mut lowered: Vec<Vec<usize>> = vec![Vec::new(); source_netlist.gates.len()];
            for (lowered_index, &source) in provenance.iter().enumerate() {
                lowered[source].push(lowered_index);
            }
            layers
                .into_iter()
                .zip(lowered)
                .zip(source_terminals)
                .map(|((layer, lowered), terminals)| CellMeta { layer, lowered, terminals })
                .collect()
        };

        let output_positions = outputs
            .into_iter()
            .map(|(display_name, signal_name)| {
                // Signal name first -- what `CompiledCircuit` keys by --
                // then the display name, which is what a hand-written or
                // human-facing pinout sidecar naturally uses. The coordinate
                // is the substance either way.
                let position = *output_positions_by_signal
                    .get(&signal_name)
                    .or_else(|| output_positions_by_signal.get(&display_name))
                    .unwrap_or_else(|| {
                    panic!(
                        "compile() must place every output this generator declared; \
                         missing `{signal_name}`"
                    )
                });
                (display_name, position)
            })
            .collect();

        let mut simulator = Simulator::new(world);
        // `Simulator::new` already settles dust strengths, but every lever
        // starts off (`lever(false)` in `src/compile/mod.rs`) and torches are
        // laid out already self-consistent with that -- so nothing is
        // actually mismatched yet. Running to stability here just keeps the
        // guarantee explicit rather than incidental: a freshly built session
        // is fully settled before a caller ever reads it.
        simulator
            .run_until_stable(MAX_GAME_TICKS)
            .map_err(|error| format!("the circuit never settled: {error:?}"))?;

        Ok(Session {
            circuit_name: circuit_name.to_string(),
            simulator,
            input_positions,
            output_positions,
            primitive_graph,
            gate_meta,
            source_netlist,
            cell_meta,
        })
    }

    /// The gate-level half of [`Session::topology`], as a plain Rust value.
    ///
    /// Split out of `topology()` rather than inlined there so it can be
    /// tested under a native `cargo test`: `topology()` itself returns a
    /// `JsValue` and so aborts off `wasm32` (see this module's doc comment,
    /// "Testing a `JsValue`-returning method natively"). This is the half
    /// that claims a cell is a `mux` reading these three signals in this pin
    /// order, and a picture confidently wrong about that is worse than no
    /// picture at all -- so it is the half that needs a test that runs
    /// everywhere, not only in a browser. See
    /// `gate_level_tests::seven_segments_gate_level_is_its_baked_netlist`.
    fn gate_level(&self) -> GateLevel {
        // Which cell drives each signal. Every gate's output is distinct --
        // two gates driving one signal is a netlist `compile()` rejects --
        // so this map never loses an entry to a collision.
        let producer: HashMap<&str, usize> = self
            .source_netlist
            .gates
            .iter()
            .enumerate()
            .map(|(index, gate)| (gate.output.as_str(), index))
            .collect();
        let cell_input = |name: &String| CellInput {
            name: name.clone(),
            cell: producer.get(name.as_str()).copied(),
        };

        let cells: Vec<TopologyCell> = self
            .source_netlist
            .gates
            .iter()
            .zip(self.cell_meta.iter())
            .enumerate()
            .map(|(index, (gate, meta))| {
                let mut nodes: Vec<usize> = meta
                    .lowered
                    .iter()
                    .flat_map(|&g| self.primitive_graph.gate_nodes[g].iter().copied())
                    .collect();
                nodes.sort_unstable();
                nodes.dedup();
                TopologyCell {
                    index,
                    output: gate.output.clone(),
                    kind: gate.kind.wire_name(),
                    inputs: gate.inputs.iter().map(&cell_input).collect(),
                    realisable: gate.kind.is_realisable(),
                    layer: meta.layer,
                    lowered: meta.lowered.clone(),
                    nodes,
                }
            })
            .collect();

        GateLevel {
            shared_edges: self.shared_edges(&cells),
            cells,
            inputs: self.source_netlist.inputs.clone(),
            outputs: self.source_netlist.outputs.iter().map(&cell_input).collect(),
        }
    }

    /// Every [`SharedEdge`] in this circuit -- see that type's doc comment for
    /// what one is and why the nested picture cannot be drawn without them.
    ///
    /// Stated over primitive **nodes** rather than over lowered signal names,
    /// because nodes are what a nested picture draws: the answer is exactly
    /// the set of lines that leave one box from a point other than the point
    /// its neighbours read it at. The two statements differ in one harmless
    /// place -- a bare merge branch puts its producer's own torch into the
    /// merge's output net, so reading that torch and reading the merge are
    /// physically the same dust, and the node-level rule correctly declines to
    /// call it a separate crossing.
    fn shared_edges(&self, cells: &[TopologyCell]) -> Vec<SharedEdge> {
        let mut owner_of_node: Vec<Option<usize>> = vec![None; self.primitive_graph.nodes.len()];
        let mut terminals: Vec<HashSet<usize>> = Vec::with_capacity(cells.len());
        for cell in cells {
            for &node in &cell.nodes {
                owner_of_node[node] = Some(cell.index);
            }
            let cell_terminals = self.cell_meta[cell.index]
                .terminals
                .iter()
                .flat_map(|&gate| self.primitive_graph.output_nodes[gate].iter().copied())
                .collect();
            terminals.push(cell_terminals);
        }

        let mut shared = Vec::new();
        for edge in &self.primitive_graph.edges {
            // A lever or a lamp belongs to no cell; those edges are primary
            // I/O, already drawn as such.
            let (Some(owner), Some(reader)) = (owner_of_node[edge.from], owner_of_node[edge.to])
            else {
                continue;
            };
            if owner == reader || terminals[owner].contains(&edge.from) {
                continue;
            }
            shared.push(SharedEdge { from: edge.from, to: edge.to, owner, reader });
        }
        shared
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

    /// Build a session from a pre-baked circuit: the gzip `.litematic` bytes
    /// `build_circuit --grown` wrote, and its `.pinout.json` parsed to
    /// `{inputs: {name: [x,y,z]}, outputs: {name: [x,y,z]}}`. The name still
    /// has to resolve in the catalog (with its `grown:` prefix stripped),
    /// because the topology view and the output ORDER come from the netlist
    /// -- only the world and the port coordinates come from the bake.
    pub fn from_baked(
        circuit_name: &str,
        world_bytes: &[u8],
        pinout: JsValue,
    ) -> Result<Session, JsValue> {
        #[derive(serde::Deserialize)]
        struct PinoutFile {
            inputs: BTreeMap<String, (i32, i32, i32)>,
            outputs: BTreeMap<String, (i32, i32, i32)>,
        }
        let world = reda::formats::litematic::load_bytes(world_bytes)
            .map_err(|error| JsValue::from_str(&format!("bad litematic: {error}")))?;
        let pinout: PinoutFile = serde_wasm_bindgen::from_value(pinout)
            .map_err(|error| JsValue::from_str(&format!("bad pinout: {error}")))?;
        Session::build_inner(circuit_name, Some((world, pinout.inputs, pinout.outputs)))
            .map_err(|error| JsValue::from_str(&error))
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

    /// **Both** levels of this circuit:
    ///
    /// ```text
    /// { gate_level:      { cells: [{index, output, kind, inputs, realisable,
    ///                               layer, lowered, nodes}],
    ///                      inputs: [name], outputs: [{name, cell}],
    ///                      shared_edges: [{from, to, owner, reader}] },
    ///   primitive_level: { nodes: [{id, primitive, provenance}],
    ///                      edges: [{from, to}],
    ///                      gates: [{index, output, kind, source, arity,
    ///                               layer, nodes, outputs}] } }
    /// ```
    ///
    /// Neither level is derivable from the other, so this returns both rather
    /// than picking one:
    ///
    /// - **`gate_level`** is the netlist as loaded -- `$_AND_`, `$_MUX_`,
    ///   `$_ANDNOT_` -- which is what a circuit *is*. It cannot be recovered
    ///   from the primitive graph: `lowering::lower` is not injective, a
    ///   shared inverter belongs to whichever cell built it first, and
    ///   nothing downstream remembers that five torches were one multiplexer.
    /// - **`primitive_level`** is what the circuit lowers into, the flat
    ///   `compile::primitive_graph` expansion -- what actually gets placed,
    ///   and what a planner needs. It cannot be recovered from the gate level
    ///   either, since it is the output of the lowering this crate does not
    ///   reimplement.
    ///
    /// The primitive level exists for the planner's benefit; a viewer's
    /// needs are not the same, so it is served alongside rather than instead.
    /// `cells[i].lowered` and `cells[i].nodes` are the join between the two,
    /// and `gate_level.shared_edges` is the one place that join is *not* a
    /// clean containment -- the inverters one cell built and another reads.
    ///
    /// No positions at either level: neither graph carries any (see
    /// `compile::primitive_graph`'s doc comment), so a page consuming this
    /// has to lay it out itself. Same native-test caveat as `pinout`/
    /// `legend`.
    ///
    /// A primitive gate's `kind` and `outputs` are what let a page draw a
    /// wire merge as the junction it is rather than as one more NOR -- see
    /// those two fields on [`TopologyGate`]. `nodes` alone cannot: a merge
    /// with only bare branches owns no node at all.
    pub fn topology(&self) -> JsValue {
        let gate_level = self.gate_level();

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
                source: meta.source.clone(),
                arity: meta.arity,
                layer: meta.layer,
                nodes: nodes.clone(),
                outputs: outputs.clone(),
            })
            .collect();
        let primitive_level = PrimitiveLevel { nodes, edges, gates };
        serde_wasm_bindgen::to_value(&Topology { gate_level, primitive_level })
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
// The gate level says what the netlist says
// ---------------------------------------------------------------------

/// The topology tab has now twice shown a picture that was confidently wrong
/// -- first labelling every wire merge `NOR`, then reporting `verilog:and4`
/// as nine NORs when it is three ANDs. Both times the data behind the picture
/// went unchecked because [`Session::topology`] returns a `JsValue` and so
/// cannot be called under a native `cargo test` (this module's doc comment).
///
/// [`Session::gate_level`] exists to close that: it is the half of
/// `topology()` that names what each cell *is*, it is a plain Rust value, and
/// this module checks it against `src/circuits/baked/seven_segment.netlist`
/// and `and4.netlist` transcribed by hand. Expected values are written out
/// literally rather than re-derived from `Netlist`, so a change that rewrites
/// both the netlist and the code reading it still has to face a third,
/// independent statement of what the circuit is.
#[cfg(test)]
mod gate_level_tests {
    use super::*;

    fn gate_level_of(circuit: &str) -> GateLevel {
        Session::build(circuit).expect("circuit builds").gate_level()
    }

    fn histogram(level: &GateLevel) -> BTreeMap<&'static str, usize> {
        let mut counts = BTreeMap::new();
        for cell in &level.cells {
            *counts.entry(cell.kind).or_insert(0) += 1;
        }
        counts
    }

    /// `and4` is the smallest possible statement of the whole point: three
    /// `and` cells.  The viewer must keep reporting those three logical
    /// cells even though the polarity search chooses their physical rails.
    #[test]
    fn and4s_gate_level_is_three_ands_with_the_optimised_physical_realisation() {
        let level = gate_level_of("verilog:and4");

        assert_eq!(level.cells.len(), 3, "and4.netlist declares `# 3 gates`");
        assert_eq!(histogram(&level), BTreeMap::from([("and", 3)]));
        assert!(level.cells.iter().all(|cell| !cell.realisable), "an $_AND_ is not realisable");

        // `gate and g0 <- d c` / `g1 <- a b` / `g2 <- g0 g1` -- pin order
        // included, since it is what the picture draws.
        let names = |cell: &TopologyCell| -> Vec<String> {
            cell.inputs.iter().map(|input| input.name.clone()).collect()
        };
        assert_eq!(names(&level.cells[0]), ["d", "c"]);
        assert_eq!(names(&level.cells[1]), ["a", "b"]);
        assert_eq!(names(&level.cells[2]), ["g0", "g1"]);
        assert_eq!(
            level.cells[2].inputs.iter().map(|input| input.cell).collect::<Vec<_>>(),
            [Some(0), Some(1)],
            "g2 reads the two other cells; the earlier two read primary inputs only"
        );
        for cell in &level.cells[..2] {
            assert!(cell.inputs.iter().all(|input| input.cell.is_none()));
        }

        assert_eq!(level.inputs, ["a", "b", "c", "d"]);
        assert_eq!(level.outputs.len(), 1);
        assert_eq!(level.outputs[0].name, "g2");
        assert_eq!(level.outputs[0].cell, Some(2), "the sole output is driven by the last cell");

        // The lowering is not lost: the chosen rails produce seven physical
        // gates.  This deliberately is not the old nine-NOR local expansion;
        // it is the same whole-netlist optimisation the official mc_dump
        // path uses.
        let lowered: usize = level.cells.iter().map(|cell| cell.lowered.len()).sum();
        assert_eq!(lowered, 7, "global polarity assignment must reach the official and4 realisation");
    }

    /// The mix from `seven_segment.netlist`'s own `# cells:` line, and the
    /// single `mux` in full -- kind, output, and all three inputs in pin
    /// order with their drivers resolved.
    #[test]
    fn seven_segments_gate_level_is_its_baked_netlist() {
        let level = gate_level_of("verilog:seven_segment");

        assert_eq!(level.cells.len(), 31, "seven_segment.netlist declares `# 31 gates`");
        assert_eq!(
            histogram(&level),
            BTreeMap::from([
                ("and", 5),
                ("andnot", 6),
                ("merge", 6),
                ("mux", 1),
                ("nand", 9),
                ("nor", 3),
                ("ornot", 1),
            ]),
            "the `# cells:` line of seven_segment.netlist, kind for kind"
        );

        // `gate mux g20 <- g14 g1 d0` -- the one cell the tab's own
        // verification singles out, because a multiplexer is exactly the kind
        // of thing that vanishes into a pile of torches when lowered.
        let muxes: Vec<&TopologyCell> = level.cells.iter().filter(|cell| cell.kind == "mux").collect();
        assert_eq!(muxes.len(), 1, "there is exactly one mux, and it must be findable");
        let mux = muxes[0];
        assert_eq!(mux.index, 20);
        assert_eq!(mux.output, "g20");
        assert!(!mux.realisable);
        assert_eq!(
            mux.inputs.iter().map(|input| (input.name.as_str(), input.cell)).collect::<Vec<_>>(),
            [("g14", Some(14)), ("g1", Some(1)), ("d0", None)],
            "A, B, S in Yosys's pin order -- a mux is not symmetric, so drawing \
             these in any other order draws a different circuit"
        );
        // What its neighbours are, so "the picture draws the right inputs" is
        // checked one hop out as well as at the cell itself.
        assert_eq!(level.cells[14].kind, "merge");
        assert_eq!(level.cells[14].output, "g14");
        assert_eq!(level.cells[1].kind, "nand");
        assert_eq!(level.cells[1].output, "g1");

        assert_eq!(level.inputs, ["d0", "d1", "d2", "d3"]);
        assert_eq!(
            level.outputs.iter().map(|output| output.name.as_str()).collect::<Vec<_>>(),
            ["g18", "g21", "g25", "g17", "g27", "g28", "g30"],
            "the seven `output` lines, in the netlist's own order"
        );
        assert!(level.outputs.iter().all(|output| output.cell.is_some()));

        // Every realisable cell is one lowered gate (`lower` adopts it
        // verbatim); every gate-level one is more.
        for cell in &level.cells {
            assert!(!cell.lowered.is_empty(), "cell {} lowered into nothing", cell.output);
            if cell.realisable {
                assert_eq!(cell.lowered.len(), 1, "{} is adopted verbatim", cell.output);
            }
        }
    }

    /// The join between the two levels is a partition: every lowered gate
    /// belongs to exactly one cell, and no cell claims one twice. Without
    /// this, a badge reading "lowers into 5" could be counting the same torch
    /// as its neighbour.
    #[test]
    fn every_lowered_gate_belongs_to_exactly_one_cell() {
        for circuit in ["verilog:and4", "verilog:seven_segment", "seven_segment", "full_adder"] {
            let session = Session::build(circuit).expect("circuit builds");
            let total = session.gate_meta.len();
            let level = session.gate_level();

            let mut seen = vec![0usize; total];
            for cell in &level.cells {
                for &g in &cell.lowered {
                    seen[g] += 1;
                }
            }
            assert!(
                seen.iter().all(|&count| count == 1),
                "{circuit}: every one of the {total} lowered gates must be claimed by exactly \
                 one cell, got {seen:?}"
            );
        }
    }

    /// The nested picture draws each cell's primitives *inside* that cell's
    /// box, so "every primitive is inside exactly one box, and no box claims
    /// one that is somebody else's" is not a nicety -- it is the thing the
    /// drawing asserts. A node claimed twice would appear in two boxes at
    /// once; a node claimed by nobody would be drawn nowhere while its edges
    /// still reached it.
    ///
    /// The exceptions are the primary I/O nodes -- one `Lever` per input, one
    /// `Lamp` per output -- which belong to no cell by construction
    /// (`Provenance::PrimaryInput`/`PrimaryOutput`) and are drawn as their own
    /// terminals.
    #[test]
    fn every_primitive_node_is_inside_exactly_one_cell_or_is_a_port() {
        for circuit in ["verilog:and4", "verilog:seven_segment", "seven_segment", "full_adder"] {
            let session = Session::build(circuit).expect("circuit builds");
            let ports = session
                .primitive_graph
                .nodes
                .iter()
                .filter(|node| !matches!(node.provenance, Provenance::Gate { .. }))
                .count();
            let total = session.primitive_graph.nodes.len();
            let level = session.gate_level();

            let mut claims = vec![0usize; total];
            for cell in &level.cells {
                for &node in &cell.nodes {
                    claims[node] += 1;
                }
            }
            for (id, &count) in claims.iter().enumerate() {
                let is_port =
                    !matches!(session.primitive_graph.nodes[id].provenance, Provenance::Gate { .. });
                let expected = usize::from(!is_port);
                assert_eq!(count, expected, "{circuit}: node #{id} is claimed by {count} cells");
            }
            let owned: usize = level.cells.iter().map(|cell| cell.nodes.len()).sum();
            assert_eq!(owned + ports, total, "{circuit}: cells and ports must cover every node");
        }
    }

    /// The shared rails of `verilog:seven_segment` remain named rather than
    /// hidden -- see [`SharedEdge`].  Global polarity assignment is allowed
    /// to change *which* logical cell first materialises a rail, so this test
    /// must pin ownership and real edges, not an obsolete local-lowering
    /// accident such as "g1 always builds !d1".
    #[test]
    fn a_shared_inverter_is_reported_as_an_edge_out_of_the_cell_that_built_it() {
        let session = Session::build("verilog:seven_segment").expect("circuit builds");
        let level = session.gate_level();
        assert!(!level.shared_edges.is_empty(), "this circuit really does share inverters");

        // Every shared edge has to be a real edge of the primitive graph,
        // leaving a node its owner really owns for a node its reader really
        // owns -- otherwise the dashed line the picture draws points at
        // nothing.
        let edges: HashSet<(usize, usize)> =
            session.primitive_graph.edges.iter().map(|edge| (edge.from, edge.to)).collect();
        for shared in &level.shared_edges {
            assert!(edges.contains(&(shared.from, shared.to)), "{shared:?} is not a real edge");
            assert_ne!(shared.owner, shared.reader);
            assert!(level.cells[shared.owner].nodes.contains(&shared.from));
            assert!(level.cells[shared.reader].nodes.contains(&shared.to));
        }

        // The selected solution materialises this shared rail in g3 and
        // feeds it into g1, g6 and g12.  Pin the actual deterministic search
        // result so a future change cannot silently return the viewer to
        // ordinary lowering while leaving only a vague "some sharing exists"
        // assertion behind.
        let g3 = level.cells.iter().find(|cell| cell.output == "g3").expect("g3 exists");
        assert_eq!(g3.kind, "andnot");
        let mut readers: Vec<&str> = level
            .shared_edges
            .iter()
            .filter(|shared| shared.owner == g3.index)
            .map(|shared| level.cells[shared.reader].output.as_str())
            .collect();
        readers.sort_unstable();
        readers.dedup();
        assert_eq!(
            readers,
            ["g1", "g12", "g6"],
            "the selected rail belongs to g3 and is read by three other cells"
        );
        // One torch, not three: every one of those edges leaves the same node.
        let sources: HashSet<usize> = level
            .shared_edges
            .iter()
            .filter(|shared| shared.owner == g3.index)
            .map(|shared| shared.from)
            .collect();
        assert_eq!(sources.len(), 1, "there is one physical primitive, shared -- not one per reader");
    }

    /// `verilog:and4`'s three ANDs share no physical rail.  A bare merge has
    /// no primitive node, so a cell can own more lowered gates than nodes;
    /// the relevant invariant is that every cell owns its own non-empty part
    /// of the optimised realisation, without borrowing from another cell.
    #[test]
    fn and4_shares_nothing_so_each_cell_owns_its_whole_expansion() {
        let level = gate_level_of("verilog:and4");
        assert!(level.shared_edges.is_empty());
        for cell in &level.cells {
            assert!(
                cell.lowered.len() >= cell.nodes.len() && !cell.lowered.is_empty() && !cell.nodes.is_empty(),
                "`{}` must own its physical gates and primitives without borrowed rails",
                cell.output,
            );
        }
    }

    /// The one cell the tab's own verification singles out, drawn: `mux g20`
    /// contains five lowered gates and five primitives in the selected
    /// solution, and the picture must show those five and no others.
    #[test]
    fn the_muxs_hull_holds_exactly_the_five_primitives_its_optimised_lowering_instantiates() {
        let session = Session::build("verilog:seven_segment").expect("circuit builds");
        let level = session.gate_level();
        let mux = level.cells.iter().find(|cell| cell.kind == "mux").expect("one mux");

        assert_eq!(mux.lowered.len(), 5);
        let union: Vec<usize> = {
            let mut nodes: Vec<usize> = mux
                .lowered
                .iter()
                .flat_map(|&g| session.primitive_graph.gate_nodes[g].iter().copied())
                .collect();
            nodes.sort_unstable();
            nodes.dedup();
            nodes
        };
        assert_eq!(mux.nodes, union, "the box holds its gates' nodes, all of them and nothing else");
        assert_eq!(mux.nodes.len(), 5);
        assert!(
            mux.nodes.iter().all(|&id| matches!(
                session.primitive_graph.nodes[id].provenance,
                Provenance::Gate { gate, .. } if mux.lowered.contains(&gate)
            )),
            "and each of those nodes names one of this cell's own gates as its provenance"
        );

    }

    /// A hand-written circuit is genuinely all-NOR -- `NetlistBuilder` emits
    /// nothing else -- so its gate level and its primitive level are the same
    /// netlist, and the tab showing a NOR wall for it is the truth rather
    /// than a fallback.
    #[test]
    fn a_hand_written_circuits_gate_level_is_already_realisable() {
        let session = Session::build("seven_segment").expect("circuit builds");
        let lowered = session.gate_meta.len();
        let level = session.gate_level();

        assert_eq!(level.cells.len(), lowered, "lowering is the identity on a hand-written circuit");
        assert!(
            level.cells.iter().all(|cell| cell.realisable && cell.kind == "nor"),
            "every hand-written gate is a NOR"
        );
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
            source_netlist: empty_netlist,
            cell_meta: Vec::new(),
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
