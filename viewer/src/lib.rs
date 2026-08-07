//! WebAssembly bindings that expose `reda`'s redstone simulator to a browser
//! viewer.
//!
//! This crate does not reimplement any redstone behaviour. Every method here
//! is a thin wrapper around `reda::compile::compile` and
//! `reda::redstone::simulator::Simulator` -- if the page shows a signal, it is
//! reading the same simulator state the project's 193 tests run against.
//!
//! **No filesystem.** Circuits come only from `reda::circuits`, which are
//! pure Rust netlist generators. Nothing here calls `reda::formats` (the
//! `.litematic` reader/writer needs `std::fs`, which compiles for
//! `wasm32-unknown-unknown` and then fails at runtime).
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

use std::collections::BTreeMap;

use reda::circuits::{and4, full_adder, seven_segment};
use reda::compile::{compile, Netlist};
use reda::redstone::simulator::Simulator;
use reda::redstone::world::block::{BlockKind, BlockState};
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

/// Every circuit `Session::new` accepts, in the order `list_circuits` reports
/// them.
const CIRCUITS: &[(&str, CircuitBuilder)] = &[
    ("and4", and4_adapter),
    ("full_adder", full_adder_adapter),
    ("segment_a", segment_a_adapter),
    ("seven_segment", seven_segment_adapter),
];

fn find_builder(name: &str) -> Option<CircuitBuilder> {
    CIRCUITS.iter().find(|&&(n, _)| n == name).map(|&(_, build)| build)
}

/// Every circuit name `Session::new` accepts.
#[wasm_bindgen]
pub fn list_circuits() -> Vec<String> {
    CIRCUITS.iter().map(|&(name, _)| name.to_string()).collect()
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
const GEOMETRY_BYTES_PER_CELL: usize = 7;

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
}

impl Session {
    /// Build (or rebuild, for `reset`) a session from scratch: look up the
    /// circuit's generator, compile it, and settle the simulator once so a
    /// caller sees a self-consistent world before ever calling `step`.
    fn build(circuit_name: &str) -> Result<Session, String> {
        let build_netlist = find_builder(circuit_name).ok_or_else(|| {
            format!(
                "unknown circuit `{circuit_name}` -- see list_circuits() for the valid names"
            )
        })?;
        let (netlist, outputs) = build_netlist();
        let compiled =
            compile(&netlist).map_err(|error| format!("compile() failed: {error:?}"))?;

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
    /// [`GEOMETRY_BYTES_PER_CELL`] (7) bytes each:
    ///
    /// `[x_lo, x_hi, y_lo, y_hi, z_lo, z_hi, kind]`
    ///
    /// `x`/`y`/`z` are little-endian `u16` world coordinates -- `u16`, not
    /// `u8`, because `seven_segment`'s Z axis reaches 298, past a single
    /// byte's range -- and `kind` is the same "block kind id" `slice()` and
    /// `legend()` already use (`block_kind_id`, i.e. `BlockKind as u8`).
    ///
    /// A caller builds this **once per circuit**: the set of non-air cells
    /// and their coordinates/kinds is fixed the moment `compile()` lays the
    /// world out, and never changes as the simulation runs (only *strengths*
    /// change -- see [`Session::strengths`]).
    ///
    /// Entry `i` here and byte `i` of `strengths()` describe the very same
    /// cell -- this pairing, and [`non_air_coords`]'s order, is exactly what
    /// `tests/geometry_ordering.rs` pins down. If the two ever drifted out of
    /// order relative to each other, a 3D view would still render a
    /// plausible-looking circuit, just with every colour attached to the
    /// wrong block, and nothing on screen would say so.
    pub fn geometry(&self) -> Vec<u8> {
        let world = self.simulator.world();
        let mut bytes = Vec::new();
        for (x, y, z) in non_air_coords(world) {
            let kind = world.get(x, y, z).kind;
            let mut cell = [0u8; GEOMETRY_BYTES_PER_CELL];
            cell[0..2].copy_from_slice(&(x as u16).to_le_bytes());
            cell[2..4].copy_from_slice(&(y as u16).to_le_bytes());
            cell[4..6].copy_from_slice(&(z as u16).to_le_bytes());
            cell[6] = block_kind_id(kind);
            bytes.extend_from_slice(&cell);
        }
        bytes
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
