//! Compile a netlist of realisable gates into a redstone world that works in
//! Minecraft.
//!
//! This is the end-to-end path: a netlist goes in, a `.litematic` comes out,
//! and the simulator checks it against the truth table.
//!
//! # Two realisable gates: NOR, and the wire merge
//!
//! Redstone's native gate is NOR -- several dust runs feed one block with a
//! torch on its side: power any input and the torch goes dark. NOR is
//! universal, so it alone would do. But an OR needs no gate at all: two dust
//! runs joining take the maximum of their strengths, which *is* the
//! operation, for no torch and no tick. So [`compile`] accepts exactly two
//! kinds, [`topology::GateKind::Nor`] and [`topology::GateKind::Or`] (a
//! declared wire merge), and nothing else.
//!
//! Everything richer -- `$_AND_`, `$_NAND_`, `$_XOR_`, `$_MUX_`, the whole
//! gate level a Verilog frontend produces -- is a [`topology::GateKind`] too,
//! and [`lowering::lower`] rewrites it into those two before it gets here.
//! `compile` will not run that pass for you; see its own doc comment for why
//! that is deliberate.
//!
//! # Placement and routing
//!
//! Gates are levelised and every gate of one level shares one row, so Z grows
//! with the netlist's depth rather than its gate count. Between two rows sits
//! a routing channel: east-west tracks on one Y layer, north-south columns on
//! another, so that nets can cross. Tracks are shared by left-edge assignment,
//! which makes a channel as deep as the netlist's local density instead of as
//! deep as its edge count. See the "Placement and routing" section further
//! down for the full picture.
//!
//! Every run of dust gets a repeater at least every 15 blocks, and a route
//! always ends in a repeater facing the next gate's support block -- redstone
//! dust does not charge a block sideways (only weakly, straight down), so
//! every wire has to be terminated by an active component.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

use crate::redstone::rules::taxonomy::{flags_of, BlockPower};
use crate::redstone::simulator::component::torch_support_position;
use crate::redstone::simulator::connectivity::{
    dust_connections, dust_powers_block_toward, dust_reach,
};
use crate::redstone::simulator::position::{Position, ALL_SIX, HORIZONTAL};
use crate::redstone::simulator::propagate::MAX_SIGNAL_STRENGTH;
use crate::redstone::world::block::{BlockKind, BlockState, Face, Facing};
use crate::redstone::world::storage::World;

use self::planner::{
    terminal_style, Anchor, NodeRealisation, PrimitiveNode, RouteSink, RouteTerminal,
    RouteTerminalKind, TerminalApproach, TerminalStyle,
};
use self::topology::Primitive;

/// The realised world's **complete** electrical graph, compared against the
/// graph the netlist intends. Measurement only, and `#[cfg(test)]` for the same
/// reason `satcnf` below is: it ships in nothing, so "this phase only measures"
/// is a property of the build rather than a promise in a comment. See the
/// module's own doc comment for what it is for.
#[cfg(test)]
pub mod coupling;
/// The derived energising range of every block this compiler writes, read out
/// of the derived artifacts. Measurement only, `#[cfg(test)]` for the same
/// reason `coupling` above is. See the module's own doc comment.
#[cfg(test)]
pub mod energising;
pub mod equivalence;
pub mod geometry;
pub mod lowering;
pub mod physical;
pub mod planner;
pub mod polarity;
pub mod primitive_graph;
pub mod relax;
pub mod routing_stats;
/// A CDCL SAT solver and a tagged CNF builder, used by `planner`'s windowed
/// model. Test-only, so it ships in nothing and takes no dependency.
#[cfg(test)]
pub mod satcnf;
/// The incremental settle against a full re-settle, cell by cell, over every
/// surface this project reads truth through. Measurement only plus one pin --
/// see the module doc and `redstone::simulator::differential`.
#[cfg(test)]
pub mod resettle_differential;
/// The static strength walk against the running `Simulator`, cell by cell.
/// Measurement only -- see the module doc.
#[cfg(test)]
pub mod strength_differential;
pub mod topology;
pub mod world_partition;

// ---------------------------------------------------------------------
// 網表
// ---------------------------------------------------------------------

/// One gate of a netlist: what it is, what it reads, and what it drives.
///
/// Plain data, and derives nothing but the plain-data traits: two netlists
/// being comparable is what lets a test say "this is the same netlist" (see
/// `circuits::verilog::baked`, whose whole job is round-tripping one), and
/// `Clone` is what lets a caller keep one while handing another away.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gate {
    pub name: String,
    pub inputs: Vec<String>,
    pub output: String,
    /// Which gate this is -- see [`topology::GateKind`].
    ///
    /// This field used to be `is_merge: bool`, and the netlist was NOR by
    /// construction: the Verilog frontend ran `abc -genlib
    /// redstone_nor.genlib`, so Yosys handed this crate a design already
    /// technology-mapped onto NOR and wire merges. That collapsed the gate
    /// level before this project's own topology library ever saw it --
    /// which is the one decision that library exists to make. The frontend
    /// now reads Yosys's *gate-level* netlist (`$_AND_`, `$_NAND_`,
    /// `$_XOR_`, `$_MUX_`, ...) and this field carries it.
    ///
    /// Two kinds are realisable in redstone directly, and they are the only
    /// two anything below this module ever sees:
    ///
    /// - [`topology::GateKind::Nor`] -- `place_nor_gate`, one torch, one
    ///   support.
    /// - [`topology::GateKind::Or`] -- a **declared wire merge**: no torch,
    ///   no support, no gate body at all, just the point downstream of where
    ///   this gate's own declared inputs' dust runs are allowed to
    ///   physically touch. See
    ///   `docs/superpowers/specs/2026-08-08-gate-types-and-wired-or.md`,
    ///   "An OR is a node, not a disappearing act", and `MergeGroups` below,
    ///   which both invariants consult to honour this. [`Gate::is_merge`] is
    ///   the predicate the placer and both invariants ask.
    ///
    /// Every other kind is gate level and has no realisation of its own;
    /// [`lowering::lower`] rewrites it into the two that do, and the caller
    /// has to run that pass before [`compile`] will place anything -- see
    /// [`compile`]'s own doc comment for why it will not do it implicitly,
    /// and [`CompileError::NotRealisable`] for what happens if it is skipped.
    pub kind: topology::GateKind,
}

impl Gate {
    /// A NOR gate driving `output` -- the ordinary case, and what every
    /// hand-written circuit in this project is made of.
    pub fn nor(output: impl Into<String>, inputs: &[&str]) -> Gate {
        let output = output.into();
        Gate {
            name: output.clone(),
            inputs: inputs.iter().map(|s| (*s).to_string()).collect(),
            kind: topology::GateKind::Nor(inputs.len()),
            output,
        }
    }

    /// A declared wire merge driving `output`.
    pub fn merge(output: impl Into<String>, inputs: &[&str]) -> Gate {
        let output = output.into();
        Gate {
            name: output.clone(),
            inputs: inputs.iter().map(|s| (*s).to_string()).collect(),
            kind: topology::GateKind::Or(inputs.len()),
            output,
        }
    }

    /// Whether this gate's output net is a wire merge of its inputs rather
    /// than a gate body of any kind. The one question the placer and both
    /// invariants ask about a gate's kind: a declared merge's branches
    /// joining is not the bug `verify_connectivity` otherwise exists to
    /// catch, and `verify_torch_merge` must not require a torch a merge was
    /// never going to have.
    pub fn is_merge(&self) -> bool {
        matches!(self.kind, topology::GateKind::Or(_))
    }
}

/// 一個邏輯閘網表。這是編譯器的輸入。
///
/// A netlist may be at either of two levels, and [`lowering::lower`] is the
/// pass between them:
///
/// - **Gate level** -- what the Verilog frontend produces, in Yosys's own
///   vocabulary (`$_AND_`, `$_NAND_`, `$_MUX_`, ...). Nothing here can be
///   placed; each gate has an expansion into the level below.
/// - **Realisable** -- NOR gates and wire merges, the two things redstone
///   builds. Every hand-written circuit under `circuits/` is already at
///   this level, so lowering is the identity on them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Netlist {
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    pub gates: Vec<Gate>,
}

impl Netlist {
    /// Return the deterministic order of the combinational part of this
    /// netlist.  A sequential gate's Q output is a source for this cycle;
    /// its D/C inputs are sampled only at its clock boundary, so edges *into*
    /// that gate do not participate in combinational cycle detection.
    ///
    /// Consequently a path such as `q -> NOR -> DFF(D=q') -> q` is legal,
    /// while a loop consisting only of NOR/merge gates still returns `None`.
    pub fn combinational_order(&self) -> Option<Vec<usize>> {
        let gate_count = self.gates.len();

        let mut producer_of: HashMap<&str, usize> = HashMap::new();
        for (index, gate) in self.gates.iter().enumerate() {
            producer_of.insert(gate.output.as_str(), index);
        }

        let mut in_degree = vec![0usize; gate_count];
        let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); gate_count];
        for (index, gate) in self.gates.iter().enumerate() {
            if gate.kind.is_sequential() {
                continue;
            }
            for input in &gate.inputs {
                if let Some(&producer_index) = producer_of.get(input.as_str()) {
                    dependents[producer_index].push(index);
                    in_degree[index] += 1;
                }
            }
        }

        let mut ready: VecDeque<usize> = (0..gate_count).filter(|&i| in_degree[i] == 0).collect();
        let mut order = Vec::with_capacity(gate_count);
        while let Some(index) = ready.pop_front() {
            order.push(index);
            for &dependent in &dependents[index] {
                in_degree[dependent] -= 1;
                if in_degree[dependent] == 0 {
                    ready.push_back(dependent);
                }
            }
        }

        (order.len() == gate_count).then_some(order)
    }

    /// 依相依關係排出計算順序。回傳 `None` 表示網表有迴路。
    ///
    /// 相依關係只看「這個閘的輸入是不是另一個閘的輸出」—— 外部輸入不算
    /// 相依，一開始就可用。用 Kahn 演算法，處理順序固定（依索引由小到大），
    /// 保證同一個網表每次排出來的順序都一樣。
    pub fn topological_order(&self) -> Option<Vec<usize>> {
        self.combinational_order()
    }

    /// 這個訊號名稱是不是某個外部輸入或某個閘的輸出。
    fn is_driven(&self, signal: &str) -> bool {
        self.inputs.iter().any(|name| name == signal)
            || self.gates.iter().any(|gate| gate.output == signal)
    }
}

// ---------------------------------------------------------------------
// The NOR primitive: a torch plus its support block, nothing else
// ---------------------------------------------------------------------
//
// There used to be a "cell" here: a fixed 3x3/5x3/5x5 template with its own
// interior wiring (one support block *per input direction*, each with its own
// dust on top, all merging into one wire network above a separate centre
// block). That interior has been dissolved -- see
// `docs/superpowers/specs/2026-08-08-3d-codesign.md`, "Dissolving it". A NOR
// gate is now exactly what that spec says it is: a torch plus its support
// block, where the support block is the single physical sink every input's
// own route terminates against directly. There is no merge dust and no
// per-input support block to merge it through, because a solid block is
// already powered by *any* neighbour that drives it -- feeding three
// different faces of the same block from three different repeaters *is* a
// 3-input NOR merge, with nothing routed "inside" it at all.
//
// What is left of the old cell is placement, not wiring: where the support
// block and its torch go, and which of the support's free faces each input's
// route is asked to terminate against. `place_nor_gate` below decides both;
// the router (`emit`'s Ramps/Columns/Tracks passes) plans every dust cell
// that gets a signal there, exactly as it already does between gates -- so
// "inside a gate" and "between gates" are no longer different routing
// regimes, only different distances.

/// Where a NOR gate's support block sits, and where its output torch and
/// input sockets are relative to it. `size` is the ground-plan bounding box
/// this occupies -- support block, output torch and its pin, and every input
/// socket this gate actually uses -- for callers that need a footprint
/// without touching a `World` (`resolve_bypass_and_geometry`'s candidate
/// paths, and `topology::nor_footprint_area`, whose own answer is checked
/// against a really-placed cell's `size` here).
pub struct NorCell {
    /// 這個 cell 佔的空間
    pub size: (i32, i32, i32),
    /// Each input's socket -- the cell immediately against the support block
    /// on that input's face, relative to the support block itself. Left
    /// empty by `place_nor_gate`: the caller (the router) is the one who
    /// decides whether a lever or a repeater ends up there, and terminates
    /// its own route with it directly against the support -- no interior
    /// hop between "socket" and "support" exists anymore, because they are
    /// the same face of the same block.
    pub input_offsets: Vec<(i32, i32, i32)>,
    /// 輸出的相對座標 —— 就是輸出火把本身，讀它的 `lit` 就是這個閘的輸出。
    pub output_offset: (i32, i32, i32),
}

pub(crate) fn stone() -> BlockState {
    let mut state = BlockState::air();
    state.kind = BlockKind::Solid;
    state.name = "minecraft:stone".to_string();
    state
}

pub(crate) fn dust() -> BlockState {
    let mut state = BlockState::air();
    state.kind = BlockKind::RedstoneWire;
    state.name = "minecraft:redstone_wire".to_string();
    state
}

fn wall_torch(facing: Facing) -> BlockState {
    let mut state = BlockState::air();
    state.kind = BlockKind::WallTorch;
    state.name = "minecraft:redstone_wall_torch".to_string();
    state.facing = Some(facing);
    state.lit = true;
    state
}

/// A redstone lamp: the block a person actually reads an output from.
///
/// `lit` starts `false` -- it is not self-consistent with the world around it
/// yet at construction time, unlike the other active components here, so the
/// simulator's first settle pass is what gives it a correct initial value
/// (see `Simulator::new`, which recomputes dust strengths and then schedules
/// any mismatched component before the caller ever reads anything).
pub(crate) fn lamp() -> BlockState {
    let mut state = BlockState::air();
    state.kind = BlockKind::Lamp;
    state.name = "minecraft:redstone_lamp".to_string();
    state.lit = false;
    state
}

/// `place_primary_input` always stands the lever on top of a floor block
/// (`ensure_floor` runs right after this), never against a wall, so `face`
/// must be `Floor` -- Minecraft's own default is `Wall`, which is exactly
/// the mismatch that made every previous lever pop off as a dropped item on
/// paste (see `minecraft.wiki/w/Lever`'s blockstate table). `facing` is
/// purely cosmetic for a floor lever (it only orients the little handle),
/// so it is fixed to `North` for determinism rather than left unset.
fn lever(on: bool) -> BlockState {
    let mut state = BlockState::air();
    state.kind = BlockKind::Lever;
    state.name = "minecraft:lever".to_string();
    state.lit = on;
    state.face = Some(Face::Floor);
    state.facing = Some(Facing::North);
    state
}

/// Place a repeater whose signal travels toward `direction` -- i.e. it reads
/// its input from `direction.opposite()` and drives its output toward
/// `direction`.
///
/// Minecraft's `facing` block state records the opposite: "the direction from
/// the output side to the input side" (see `minecraft.wiki/w/Redstone_Repeater`'s
/// blockstate table), so the stored `facing` is `direction.opposite()`, not
/// `direction` itself. Every call site below only ever cares about the
/// direction the signal is travelling, so that is what this helper takes --
/// the Minecraft-facing conversion happens once, here.
pub(crate) fn repeater(direction: Facing) -> BlockState {
    let mut state = BlockState::air();
    state.kind = BlockKind::Repeater;
    state.name = "minecraft:repeater".to_string();
    state.facing = Some(direction.opposite());
    state.delay = 1;
    state.lit = true;
    state
}

/// 把一個 n 輸入的 NOR 閘畫進世界：一個支撐塊，加上貼在它輸出面的輸出火把。
/// 就這樣 —— 沒有內部佈線。
///
/// `facing` is the way the finished cell points: its output torch hangs on
/// that face, and its inputs arrive on the other three. North is the facing
/// every gate this compiler has ever placed was built to, and it is still
/// what every caller passes.
///
/// The support block is placed; the torch is placed, attached to it; and
/// every input this gate actually uses gets a socket coordinate
/// (`input_offsets`) immediately against one of the support's three free
/// horizontal faces (`geometry::input_directions` -- west/east/south for a
/// cell facing north). The socket itself is left as air, exactly as before:
/// the caller (the router) decides whether a lever or a repeater ends up
/// there, and that repeater's own output *is* what powers the support --
/// there is no support-per-input block or merge dust standing between the
/// socket and the block it drives anymore, because a solid block is already
/// powered by any conductor that faces into it, regardless of which of its
/// faces that happens to be.
///
/// This is `verify_torch_merge`'s N-sources-one-sink invariant made
/// physical: `support` is the one sink; every input's own route is one
/// source, terminating directly against it.
pub fn place_nor_gate(
    world: &mut World,
    origin: (i32, i32, i32),
    input_count: usize,
    facing: geometry::CellFacing,
) -> NorCell {
    let inputs = geometry::input_directions(facing);
    assert!(
        input_count <= inputs.len(),
        "place_nor_gate takes at most {} inputs, got {input_count}",
        inputs.len()
    );

    let support = Position::new(origin.0, origin.1, origin.2);
    world.set(support.x, support.y, support.z, stone());

    // 插座留空 —— 由呼叫端（router）決定要接拉桿還是中繼器，並直接面朝
    // `support` 把它充能。
    let mut input_offsets = Vec::with_capacity(input_count);
    for &direction in inputs.iter().take(input_count) {
        let socket = support.offset(direction);
        input_offsets.push((
            socket.x - support.x,
            socket.y - support.y,
            socket.z - support.z,
        ));
    }

    let out = geometry::output_direction(facing);
    let output_torch_pos = support.offset(out);
    world.set(
        output_torch_pos.x,
        output_torch_pos.y,
        output_torch_pos.z,
        wall_torch(out),
    );

    // 邊界盒：涵蓋支撐塊、所有用到的輸入插座，以及輸出插座（火把再往外
    // 一格 -- `emit`稍後會在那裡放這個閘輸出淨路的第一格紅石粉）。
    let output_socket = output_torch_pos.offset(out);
    let mut min = (support.x, support.y, support.z);
    let mut max = (support.x, support.y, support.z);
    let mut extend = |p: Position| {
        min.0 = min.0.min(p.x);
        min.1 = min.1.min(p.y);
        min.2 = min.2.min(p.z);
        max.0 = max.0.max(p.x);
        max.1 = max.1.max(p.y);
        max.2 = max.2.max(p.z);
    };
    extend(output_socket);
    for &(dx, dy, dz) in &input_offsets {
        extend(Position::new(
            support.x + dx,
            support.y + dy,
            support.z + dz,
        ));
    }

    let size = (max.0 - min.0 + 1, max.1 - min.1 + 1, max.2 - min.2 + 1);

    NorCell {
        size,
        input_offsets,
        output_offset: (
            output_torch_pos.x - support.x,
            output_torch_pos.y - support.y,
            output_torch_pos.z - support.z,
        ),
    }
}

/// Place an `input_count`-input wire-merge OR into the world: no support
/// block, no torch -- just the point downstream of where its declared
/// inputs' own routes are allowed to touch. See
/// `docs/superpowers/specs/2026-08-08-gate-types-and-wired-or.md`, "An OR is
/// a node, not a disappearing act": a merge is still a node this router has
/// to place *somewhere* (it needs a row, an X, a Z, the same as any other
/// gate), it just has nothing functional to place there.
///
/// Deliberately built to the *exact same footprint* `place_nor_gate` uses
/// for the same `input_count` and `facing` -- the same `input_offsets` (the
/// three horizontal `geometry::input_directions` faces around `origin`) and an
/// `output_offset` chosen so `emit`'s generic, gate-kind-agnostic geometry
/// code (`torch_of`, `cell_geometry_by_input_count`, `source_pin_position`,
/// `resolve_bypass_and_geometry`) needs no merge-specific case at all --
/// only *what gets physically written* at `origin` differs. A NOR's
/// support is a solid block that a repeater must actively drive (dust
/// cannot charge a block sideways); a merge's own junction is plain dust,
/// which joins any adjacent dust directly, so nothing has to drive it at
/// all.
///
/// `output_offset` stays `(0, 0, 0)`: `origin` -- the electrical junction
/// itself -- is what `emit` records in `gate_output_positions` for this
/// gate, exactly as before. But `origin` is directly adjacent to every one
/// of this same gate's own occupied input sockets
/// (`geometry::input_directions`: west, east and south for a cell facing
/// north) -- so `emit` must **not** start this gate's own *outbound route*
/// (`gate_pin`) from `origin` directly the way an earlier version of this
/// function's own doc comment here used to say it should:
/// a downstream route leaving from `origin` and heading in any of those
/// three directions -- something every router pass does routinely, with no
/// reason to think of `origin` as special -- would walk straight back
/// through its own gate's input wiring, silently overwriting one branch's
/// carefully-placed isolating repeater with plain dust. Caught by the real
/// simulator disagreeing with the signal-strength invariant on the
/// Verilog-derived seven-segment decoder (the first netlist dense enough
/// with real merges to ever route one this way), not by any hand-built
/// test.
///
/// The fix lives in `emit`, not here: it now starts a merge's outbound
/// route one hop out from `origin`, along `geometry::output_direction`,
/// exactly the way it always started a NOR's own outbound route one hop out
/// from *its* torch -- this function already reserved that cell
/// (`output_socket`, below) in the bounding box it returns, it just never got
/// a chance to matter as an actual routing origin until now. One hop is
/// clearance enough: `output_socket` is not adjacent to any of `origin`'s own
/// occupied faces, only to `origin` itself and to whatever this gate
/// drives -- exactly the same safety a NOR's own two-stage
/// support-then-torch-then-pin clearance provides, just one stage shorter
/// because a merge has no torch cell to skip past in the first place.
pub fn place_merge_gate(
    world: &mut World,
    origin: (i32, i32, i32),
    input_count: usize,
    facing: geometry::CellFacing,
) -> NorCell {
    let inputs = geometry::input_directions(facing);
    assert!(
        input_count <= inputs.len(),
        "place_merge_gate takes at most {} inputs, got {input_count}",
        inputs.len()
    );

    let support = Position::new(origin.0, origin.1, origin.2);
    ensure_floor(world, support);
    world.set(support.x, support.y, support.z, dust());

    // 插座留空 -- 由呼叫端（router）決定要接哪一種收尾：私有分支收裸紅石粉，
    // 有外部扇出的分支收隔離用中繼器。
    let mut input_offsets = Vec::with_capacity(input_count);
    for &direction in inputs.iter().take(input_count) {
        let socket = support.offset(direction);
        input_offsets.push((
            socket.x - support.x,
            socket.y - support.y,
            socket.z - support.z,
        ));
    }

    // No output torch to place -- `output_socket` is where this gate's own
    // outbound net's first cell (`emit`'s `gate_pin`) ends up, one hop out
    // from the junction itself (see this function's own doc comment for why
    // that clearance matters even though nothing physical stands there).
    // `emit` writes the actual dust for it, exactly as it does for a NOR's
    // own pin -- this function only reserves the cell in `input_offsets`'/
    // the bounding box's terms, the same way it always has.
    let output_socket = support.offset(geometry::output_direction(facing));
    let mut min = (support.x, support.y, support.z);
    let mut max = (support.x, support.y, support.z);
    let mut extend = |p: Position| {
        min.0 = min.0.min(p.x);
        min.1 = min.1.min(p.y);
        min.2 = min.2.min(p.z);
        max.0 = max.0.max(p.x);
        max.1 = max.1.max(p.y);
        max.2 = max.2.max(p.z);
    };
    extend(output_socket);
    for &(dx, dy, dz) in &input_offsets {
        extend(Position::new(
            support.x + dx,
            support.y + dy,
            support.z + dz,
        ));
    }

    let size = (max.0 - min.0 + 1, max.1 - min.1 + 1, max.2 - min.2 + 1);

    // `output_offset` stays `(0, 0, 0)` -- `origin` itself is still what
    // `gate_output_positions` records as this gate's own observable
    // position (`compile_places_the_isolating_repeater_on_exactly_the_
    // shared_branch`, `tests/or_merge.rs`, reads a socket straight off it:
    // `junction.offset(Facing::West)`), and origin genuinely is the
    // electrical junction. The one-hop clearance this function's own doc
    // comment now explains is `emit`'s job to apply, to where its outbound
    // *route* starts (`gate_pin`) -- not to this recorded position, which
    // must stay put.
    NorCell {
        size,
        input_offsets,
        output_offset: (0, 0, 0),
    }
}

// ---------------------------------------------------------------------
// Placement and routing
// ---------------------------------------------------------------------
//
// The floorplan is the classic row/channel shape of a standard-cell
// place-and-route, turned north so that it matches the way a NOR cell emits:
//
//     row 0            the primary inputs' levers            (largest Z)
//     channel 0        routing
//     row 1            every gate whose level is 0
//     channel 1        routing
//     row 2            every gate whose level is 1
//     ...                                                    (smallest Z)
//
// Signal flow is northwards (-Z), because `place_nor_gate` puts the output
// torch on the cell's north face. Every gate of one level shares one row, so Z
// grows with the *depth* of the netlist, not with its gate count.
//
// Inside a channel the two Manhattan directions live on different Y layers,
// and that is what lets nets cross each other at all:
//
//   Y = band_y(t)    east-west "tracks", one band per parallel track a
//                    channel's density actually needs (see `BAND_CAP`).
//                    One track carries several nets when their X spans are
//                    disjoint (left-edge assignment), so a channel needs as
//                    many bands as its local *density*, not its edge count.
//                    `band_y(t) - 1` is that band's own stone floor, doubling
//                    as the shield that stops a track from reaching down
//                    into a column. Today's fixed `TRACK_Y` was `band_y(0)`,
//                    the only band a channel could ever need before this was
//                    generalised -- see `docs/superpowers/specs/2026-08-08-
//                    3d-codesign.md`, "Layers, not two planes".
//   Y = GATE_Y       north-south "columns", one per pin. A column passes
//                    underneath any number of tracks without touching them.
//
// A net that has to reach a level further away than the next one takes a
// feed-through: a column that runs straight through the intervening rows in a
// reserved X slot and rejoins a track in the later channel. A long net
// therefore costs one column per level it crosses, instead of one dedicated
// lane for its whole length.
//
// A net with exactly one sink whose source column and sink approach column
// are close enough skips this whole climb-cross-descend dance and instead
// connects straight across at `GATE_Y`, never touching `TRACK_Y` at all.
// "Close enough" is decided two ways. Inside `BYPASS_MAX_DISTANCE`, it is not
// tuned at all -- that constant derives it from the same clearance invariant
// that keeps every other column apart, so a bypass route in that range is
// provably clear of every other net's column before it is ever laid (see
// that constant's doc comment for the proof). Beyond it, out to
// `BYPASS_QUERY_MAX_DISTANCE`, `resolve_bypass_and_geometry` asks instead of
// proving: it checks a candidate route's actual cells against the
// `Reservation` every other net's routing already claims, and against every
// row it crosses, and only takes the ones that come back clear. See
// `compute_bypass` and `resolve_bypass_and_geometry` for the two passes, and
// `lay_bent_path` for how either kind of route is built without paying for a
// mandatory strength refresh it does not need.

/// Y of the gate bodies, their input sockets, the output pins, and every
/// north-south routing column.
const GATE_Y: i32 = 1;

/// Vertical distance from one track band to the next -- one isolation/floor
/// cell plus one track cell, exactly the gap the original two-plane design
/// had between `GATE_Y` and its single track plane. This is the "layer
/// pitch" of the generalised multi-layer stack described in
/// `docs/superpowers/specs/2026-08-08-3d-codesign.md`, "Layers, not two
/// planes": today's two planes are `band_y(0)`'s case of this, not a
/// separate design.
///
/// Two bands separated by this much in Y never risk `dust_reach`'s
/// same-layer (distance-1) adjacency rule joining them, and -- see
/// `layout_z`'s doc comment for the full argument -- a net climbing past a
/// *lower* band's own track always does so at least `BAND_HEIGHT` cells away
/// from that band's Z, for the same reason. 1 would already clear the first
/// property; 2 is kept because it is what makes the second one arithmetically
/// exact (`layout_z`'s derivation divides through by it), and because it
/// reproduces the original `GATE_Y`/`TRACK_Y` gap exactly when there is only
/// one band.
const BAND_HEIGHT: i32 = 2;

/// The most tracks a channel may spread across real Y bands before this
/// module gives up on layering it and falls back to the old single-band,
/// `TRACK_SPACING`-separated stacking for that *whole* channel (see
/// `effective_band` and `layout_z`'s two branches).
///
/// # Where the value comes from
///
/// Not from the strength budget, even though that is the failure this cap
/// was first built to avoid: `band_levels(BAND_CAP - 1)` only has to stay
/// under `RAMP_REST_INTERVAL` (13) to avoid a mandatory rest stop -- a real
/// repeater sitting mid-ramp that costs actual game ticks, unlike plain dust
/// -- and that alone would allow a `BAND_CAP` as high as 6. Measured all the
/// way up there, `seven_segment` (this project's own target circuit)
/// confirmed the failure mode is real: worst-case settle went from 114
/// ticks to 242 once a rest stop landed on the critical path, even though
/// `verify_connectivity`/`verify_torch_merge` both still passed and the
/// truth table still matched. A circuit that takes twice as long to settle
/// has not been improved.
///
/// But sweeping `BAND_CAP` from 1 to 6 and re-measuring settle on all five
/// reference circuits at every value (release build, `cargo test --test
/// reference_circuits`/`seven_segment`/`verilog_frontend -- --nocapture`)
/// found the *real* ceiling much lower, well before any rest stop is ever
/// possible: past 2, `segment_a` and the Yosys-synthesised `seven_segment`
/// both start settling *slower* than the two-plane baseline, even though
/// every ramp involved is still a single, rest-free climb. The mechanism is
/// distinct from the rest-stop one: entering band `b` still starts its
/// straight column run `band_ramp_length(b)` cells further from the row than
/// band 0 needed to, and that extra length can cross another `MAX_DUST_RUN`
/// boundary and force one more mandatory repeater onto a path that never
/// needed one before -- dust itself is free in ticks, but the repeater a
/// long enough run of it eventually requires is not. 2 is the largest value
/// at which this never happened on any of the five circuits; `and4` and
/// `full_adder` never ask for more than two tracks in one channel regardless,
/// so they are unaffected either way.
///
/// # Why the fallback is whole-channel, not per-track
///
/// Layering the first `BAND_CAP` tracks of a dense channel and Z-stacking
/// only the overflow -- rather than reverting the whole channel -- very
/// nearly shipped a second bug here. An overflow track's climb still has to
/// pass *through* every lower band's own height on its way to the shared top
/// band, at a Z offset from that band's own Z line of `-OVERFLOW_SPACING *
/// k + BAND_HEIGHT * m` for some overflow index `k` and level gap `m` --
/// which lands exactly `1` cell away (adjacent, not clear) for real `(k, m)`
/// pairs at `OVERFLOW_SPACING = TRACK_SPACING`. Proving it clear in general
/// needs `OVERFLOW_SPACING` far larger than `TRACK_SPACING` ever was, which
/// erases the saving layering was meant to add back. Reverting the whole
/// channel avoids the question entirely -- a reverted channel is byte-for-
/// byte the original two-plane geometry, already proven safe, so there is
/// nothing new to prove.
const BAND_CAP: usize = 2;

/// The band a net's slot actually climbs to, once its whole channel's
/// density has been taken into account -- `raw_band` (`Net::tracks[slot]`,
/// the left-edge track index `assign_tracks` gave it) unchanged for a
/// channel with `BAND_CAP` tracks or fewer, or a hard `0` for every track of
/// a denser channel (see `BAND_CAP`'s doc comment for why the fallback is
/// whole-channel, not per-track).
///
/// `raw_band` still decides which physical *track* (X-disjoint sharing
/// group, and for a reverted channel, which `TRACK_SPACING`-separated Z
/// line) a net belongs to -- `layout_z` and `Net::entry_column` keep
/// indexing `track_z[channel]` with it unchanged. This function only
/// answers the *separate* question of which Y a climb into that track
/// actually reaches, which is where the two schemes differ.
fn effective_band(track_count: &[usize], channel: usize, raw_band: usize) -> usize {
    if track_count[channel] > BAND_CAP {
        0
    } else {
        raw_band
    }
}

/// How many Y levels a climb or descent into/out of `band` (0-indexed, the
/// same index `Net::tracks` already stored under the old two-plane name
/// "track index") crosses. By the one-horizontal-cell-per-level rule a dust
/// staircase always obeys (`move_between_layers`'s doc comment), this is
/// also how many *diagonal* Z cells it costs -- but not the *whole* physical
/// distance any more; see `band_ramp_length` for why those two used to be
/// the same number and no longer are.
///
/// Every caller that reaches this with a raw `Net::tracks[slot]` value is
/// expected to have already run it through `effective_band` first -- this
/// function does not clamp on its own. It stays this simple deliberately:
/// `effective_band` is the one place that has to know about `BAND_CAP`, so
/// this is `BAND_CAP - 1`'s own `rest_stops_for(band_levels(..))` staying
/// comfortably at 0 for every band this router ever actually asks a real
/// climb to reach -- the mandatory rest-stop machinery below exists as a
/// safety net for a larger `BAND_CAP`, not because today's circuits trigger
/// it.
fn band_levels(band: usize) -> i32 {
    BAND_HEIGHT * (band as i32 + 1)
}

/// Y of the one east-west track a net assigned to `band` is routed on.
/// `band_y(0) - 1` is that band's own stone floor, exactly mirroring the old
/// `TRACK_Y - 1`.
///
/// Every band shares one Z line per channel (`layout_z` computes it once,
/// not per band) -- only Y tells two bands' tracks apart. That is the whole
/// generalisation this module makes: the old design's `TRACK_Y` was
/// `band_y(0)`, hard-wired as the only band that could ever exist.
fn band_y(band: usize) -> i32 {
    GATE_Y + band_levels(band)
}

/// The longest run of climbing steps a dust staircase may take between two
/// signal refreshes -- deliberately one short of `MAX_DUST_RUN` (see that
/// constant's own doc comment for the underlying 14-cell budget a fresh
/// strength-15 source affords), so that even the worst case this router ever
/// hands a climb (arriving at strength 1, the minimum `plan_straight_run`'s
/// own reserve mechanism ever allows) still has a cell of slack rather than
/// landing exactly on the wire.
///
/// This is *not* the same kind of number as `MAX_DUST_RUN`: a flat run picks
/// repeater positions from the actual incoming strength, because a flat
/// cell can always become a repeater in place. A climbing step cannot -- see
/// `band_ramp_length`'s doc comment -- so every rest stop here is placed at
/// a fixed position, unconditionally, regardless of what the real strength
/// turns out to be. That is what lets `band_ramp_length` stay a pure
/// function of the band index alone, which the placement/Z-layout stages
/// need (they run before a single signal strength is computed).
const RAMP_REST_INTERVAL: i32 = MAX_DUST_RUN - 1;

/// How many mandatory rest stops (see `RAMP_REST_INTERVAL`) a climb or
/// descent of `levels` Y-levels needs.
fn rest_stops_for(levels: i32) -> i32 {
    if levels <= 0 {
        0
    } else {
        (levels - 1) / RAMP_REST_INTERVAL
    }
}

/// The physical Z (or X) distance a climb or descent into/out of `band`
/// costs -- `band_levels(band)` diagonal steps, the same one-cell-per-level
/// a dust staircase always pays, plus two extra *flat* cells per mandatory
/// rest stop.
///
/// A rest stop cannot be folded into one of the climbing steps the way a
/// flat run folds a repeater into one of its own cells: a repeater only
/// reads the block directly behind it, at the *same* Y, and every cell of a
/// climb sits one level above (or below) the one before it, so there is
/// never a "behind" a repeater placed mid-climb could read from. A rest
/// stop is therefore two genuine extra cells, not a relabelling of ones
/// that would exist anyway: the repeater itself, and one plain dust cell
/// to receive its output before the diagonal step can resume. That second
/// cell is not optional even for a climb, where a repeater's output *could*
/// charge the very next riser directly (a solid block, which then recharges
/// the dust standing on it) -- because it is never optional for a descent,
/// where the repeater's own front cell is deliberately left open air for
/// the diagonal wire-to-wire rule instead of holding anything a repeater
/// could charge (see `move_between_layers`'s own doc comment). Both
/// directions pay the same two-cell cost so this stays one function instead
/// of two.
///
/// `band_ramp_length(0)` (zero rest stops needed for `BAND_HEIGHT` levels)
/// reproduces the old constant `RAMP_LENGTH` exactly; higher bands cost
/// `BAND_HEIGHT` more levels each, plus two cells per rest stop, one every
/// `RAMP_REST_INTERVAL` levels.
///
/// This is *why* two nets bound for different bands never collide even
/// though every band's track sits on the very same Z line (`layout_z`'s doc
/// comment carries the full derivation, in terms of `band_levels`, not this
/// function -- a rest stop changes how *far* a climb travels, never how
/// *high*, so it cannot bring one net's climb any closer to another band's Y
/// than the vertical argument already guarantees).
fn band_ramp_length(band: usize) -> i32 {
    let levels = band_levels(band);
    levels + 2 * rest_stops_for(levels)
}

/// The strength remaining after a climb or descent of `levels` Y-levels,
/// with the same mandatory, position-fixed, two-cell rest stops
/// `move_between_layers` actually places (see `RAMP_REST_INTERVAL` and
/// `band_ramp_length`'s doc comment) -- the pure twin `plan_strengths`
/// needs, since the real ramp is not written until later (Ramps, then
/// Columns, then Tracks -- see the comment above `compile`'s Ramps loop).
fn ramp_ending_strength(levels: i32, incoming_strength: u8) -> u8 {
    let mut strength = incoming_strength;
    for level in 0..levels {
        if level > 0 && level % RAMP_REST_INTERVAL == 0 {
            strength = MAX_SIGNAL_STRENGTH;
            strength -= 1; // the rest stop's own output dust cell
        }
        strength -= 1; // the diagonal climb/descend step itself
    }
    strength
}

/// How much strength whatever *precedes* a climb or descent into/out of
/// `band` must still reserve -- not the whole `band_ramp_length(band)` (that
/// can exceed what a single strength-15 source can ever survive, once a
/// band needs more than `RAMP_REST_INTERVAL` levels), only enough to reach
/// the *first* mandatory rest stop, since every stop after that starts fresh
/// at `MAX_SIGNAL_STRENGTH` regardless of what arrived at the previous one.
fn ramp_reserve(band: usize) -> i32 {
    band_levels(band).min(RAMP_REST_INTERVAL)
}

/// A small, fixed height for scratch worlds that only ever place gate bodies
/// at `GATE_Y` (`cell_geometry_by_input_count`'s probe) -- no ramp, no track,
/// so no dependency on how many bands the real circuit ends up needing.
const GATE_ONLY_SCRATCH_HEIGHT: i32 = GATE_Y + 4;

/// How tall the world must be to hold every band any channel actually uses,
/// plus one spare layer of air above the highest track so nothing is ever
/// written outside the world -- generalises the old fixed `WORLD_HEIGHT`
/// (`band_y(0) + 2 == 5`, the original constant, when nothing needs a second
/// band).
fn world_height(track_count: &[usize]) -> i32 {
    // A reverted channel (`BAND_CAP`'s doc comment) never climbs past
    // `band_y(0)` regardless of how many raw tracks it has, so this has to
    // run every channel's raw track count through `effective_band` -- the
    // *tallest* channel by raw count is not necessarily the one that climbs
    // highest.
    let highest_band = (0..track_count.len())
        .map(|channel| effective_band(track_count, channel, track_count[channel].max(1) - 1))
        .max()
        .unwrap_or(0);
    band_y(highest_band) + 2
}

/// X distance between two neighbouring gates of the same row.
///
/// A gate cell reaches out to `cx ± ENTRY_OFFSET` for spacing purposes, so 14
/// leaves a five-wide gap between two cells -- room for exactly one
/// feed-through column that is still at least `COLUMN_CLEARANCE` clear of
/// everything on either side. Unchanged by cells dissolving: a gate's actual
/// physical footprint shrank (`place_nor_gate` no longer builds anything past
/// one cell from its support block), but the spacing this constant provides
/// was never actually about that footprint -- see `ENTRY_OFFSET`'s own doc
/// comment for why its value stayed put too.
const SLOT_PITCH: i32 = 14;

/// Where a gate's west/east routing entry column sits -- and, since
/// `row_body_zones` pads a gate's own keep-out zone by the same amount, the
/// effective half-width of a gate's footprint for spacing purposes.
///
/// A west/east socket cannot be entered by a plain north-south column,
/// however close that column runs to it: the socket-to-support relationship
/// is along the *X* axis (`place_nor_gate`'s `input_offsets` puts it one cell
/// out), so the final hop into it has to be an east-west one -- a repeater
/// only powers what is directly in front of it -- which needs a real jog, not
/// merely a nearby column. (A south input has no such jog at all: its socket
/// sits along *Z*, the same direction the column already travels, so
/// `approach_column` puts its entry column dead on the socket -- see that
/// function's doc comment.)
///
/// This kept its pre-dissolution name, `GATE_HALF_WIDTH`, until it was
/// renamed here to describe what it now does rather than what it used to be
/// the width of. Its value, 4, is left exactly as it was for a reason worth
/// stating plainly: this router's spacing proof only actually covers a
/// column's clearance from *feed-through* candidates and from other members
/// of the *same net* (`reserve_columns`'s own doc comment, and
/// `reserve_feedthrough`'s clearance search) -- it never checks that one
/// gate's own output column and an unrelated gate's socket-approach column,
/// meeting in the same channel by coincidence rather than by producer/
/// consumer relationship, land `COLUMN_CLEARANCE` apart. In practice they
/// almost always do, because 4 leaves generous slack around `SLOT_PITCH`'s
/// row spacing -- but `full_adder` was enough to falsify a tighter value (3,
/// the tightest this jog reasoning by itself requires) with an actual
/// `ConnectivityViolation` during this change's own development, between
/// gate `g1`'s own output column and gate `g5`'s unrelated east-input
/// approach column. Shrinking this constant is therefore a placement/
/// spacing-proof change, not a socket-geometry one, and belongs with
/// `docs/superpowers/specs/2026-08-08-3d-codesign.md`'s "Placement as an
/// optimisation" -- so it stays at its old, empirically-safe value here.
const ENTRY_OFFSET: i32 = 4;

/// Z distance between two neighbouring tracks of the same channel, when that
/// channel is dense enough to revert to the pre-layering scheme (`BAND_CAP`'s
/// doc comment) -- every track shares `band_y(0)`, so this is the only thing
/// telling them apart, exactly as when it was the sole scheme this module
/// had. The original justification (a strongly-powered landing from the old
/// repeater-built ramp reaching the next track over) no longer applies to a
/// dust staircase, but this is reverted geometry, not new geometry, so it is
/// left at its old, empirically-safe value rather than retuned.
const TRACK_SPACING: i32 = 5;

// A dust staircase's diagonal rule (`redstone::simulator::connectivity::
// dust_connections`) connects one dust cell straight to the next diagonal
// one -- exactly one block of horizontal travel per Y level, unlike the old
// repeater-built ramp this replaced (two blocks per level: a repeater, then
// the support block it drove). `recompute_dust_strengths`'s BFS spends
// exactly one strength per hop it walks, same-level or diagonal, without
// distinguishing them (see its `HORIZONTAL` loop) -- so a climb of `n`
// levels spends exactly `n` signal strength, one block of horizontal
// footprint and one point of strength per level, always in lockstep. This
// used to be a single flat constant (`RAMP_LENGTH = TRACK_Y - GATE_Y`, back
// when there was only ever one track band); see `band_ramp_length`, which
// replaces it now that different bands cost different amounts to reach.

/// Minimum X gap between two nets that share one track.
const TRACK_SHARE_GAP: i32 = 4;

/// Minimum X gap between two routing columns. Redstone dust connects to its
/// four horizontal neighbours, so one empty block between two columns is
/// already enough -- but only exactly enough, which is why every other
/// clearance in this module is derived from it rather than written out.
const COLUMN_CLEARANCE: i32 = 2;

/// The largest X gap between a net's source column and its one sink's
/// approach column that a direct `GATE_Y` route -- no ramp, no track -- can
/// bridge without ever risking another net's column.
///
/// Every column this router places -- a net's source column, a gate's socket
/// approach column, or a feed-through hop -- sits at least
/// `COLUMN_CLEARANCE` away from every other one in the same channel (see
/// `reserve_columns`'s own doc comment, and the shifted row placement in
/// `build_floorplan` that makes it true even before a single feed-through is
/// chosen). A third column strictly between two points `d` apart would
/// itself have to be at least `COLUMN_CLEARANCE` away from *both* of them,
/// which needs `d >= 2 * COLUMN_CLEARANCE`. So whenever `d < 2 *
/// COLUMN_CLEARANCE`, no such column can exist anywhere in the world, at any
/// Z -- not merely unlikely to -- and a direct horizontal jog between the two
/// is provably clear of every other net's dust. That is what makes this a
/// derived threshold rather than a tuned one; see `compute_bypass`, the only
/// caller.
const BYPASS_MAX_DISTANCE: i32 = 2 * COLUMN_CLEARANCE - 1;

/// The largest X gap `resolve_bypass_and_geometry` will even *ask* about --
/// past `BYPASS_MAX_DISTANCE`, where the proof above no longer applies and
/// the answer has to come from an actual `Reservation` query instead of a
/// geometric guarantee.
///
/// This is a measured cutoff, not a derived one -- see
/// `resolve_bypass_and_geometry`'s doc comment for the numbers it was picked
/// from. Widening the *query* range costs nothing by itself (a query that
/// finds a collision just falls back to the ramp/track route it would have
/// taken anyway); what stops this from growing without bound is that a
/// candidate this far out is asking a horizontal jog to run past
/// `BYPASS_MAX_DISTANCE`'s own row-body margin (`ENTRY_OFFSET +
/// COLUMN_CLEARANCE` from the *next* gate in the same row, `SLOT_PITCH` away
/// from this one) -- so past a certain distance every remaining candidate is
/// rejected by `jog_crosses_another_row_zone` anyway, and asking further out
/// only spends compile time, not correctness.
const BYPASS_QUERY_MAX_DISTANCE: i32 = 12;

/// West edge of the floorplan. Everything is laid out eastwards from here, so
/// no coordinate can go negative on the X axis.
const ORIGIN_X: i32 = 8;

/// `lay_dust_run`'s `start` (or a track's `source_x`) is not always a fresh
/// signal source anymore -- it can also be a ramp's landing, which arrives
/// carrying less than the full 15. `MAX_SIGNAL_STRENGTH - incoming_strength`
/// is how much of that budget is already spent before this run's first cell;
/// counting from there (instead of from zero) is what keeps a long run whose
/// *input* is already weakened from silently running the wire dead before a
/// repeater would otherwise have been due -- see `plan_straight_run` and
/// `plan_track_run`, which both start their counters this way.
///
/// A cell one hop past an active source (lever, torch-adjacent dust, or a
/// repeater's output) is at strength 14; each further hop of flat dust costs
/// exactly 1 more (`recompute_dust_strengths`); the 14th hop is still alive
/// at strength 1, and the 15th would be 0 -- dead
/// (`signal_strength_falls_off_over_distance_in_a_real_circuit` verifies
/// exactly this boundary). So at most 14 dust cells may follow a fresh
/// source before a repeater is mandatory.
///
/// A run that empties into a ramp cannot spend the full 14, though: the ramp
/// itself is `RAMP_LENGTH` more hops (see that constant's comment) that has
/// to survive on whatever strength is left when the flat run hands off to
/// it, and a ramp cannot absorb a repeater partway up -- a repeater cannot
/// change Y. So such a run's last cell must still be carrying at least
/// `RAMP_LENGTH + 1` (enough to survive `RAMP_LENGTH` more hops and land on
/// a live 1, not a dead 0), which means it may only spend
/// `MAX_DUST_RUN - RAMP_LENGTH` cells per refresh cycle, not the full budget
/// -- `plan_straight_run` and `plan_track_run` take that shortfall as their
/// `reserve` parameter. Every dust run in this router either starts or ends
/// at a ramp (frequently both), so this reservation, not the bare constant,
/// is what most calls actually use.
pub(crate) const MAX_DUST_RUN: i32 = 14;

/// The `reserve` **one hop** of a **bare**-terminated branch's own final
/// approach (`plan_bent_path`, via `lay_bent_path_bare`/`bare_branch_
/// landing_strength`) must budget, on top of the ordinary `MAX_DUST_RUN`
/// cycle -- unlike every other termination this router lays, a bare
/// branch's own last cell (the socket) is not where the signal's job ends.
/// Redstone dust decays by exactly one per hop, with no floor above zero to
/// protect against, so each hop past the socket has to be paid for up
/// front, in whatever budget decided the *socket's* own strength:
///
/// - One hop, socket to the merge's own junction (`compute_net_source_
///   strengths` already treats this as ordinary dust decay when combining
///   branches: "`.saturating_sub(1)`").
/// - One more hop, junction to the merge's own outbound pin (`emit`'s own
///   one-hop clearance past the junction -- see `place_merge_gate`'s doc
///   comment for why that pin is not the junction cell itself), which is
///   where the *next* net downstream (another gate, or a declared output's
///   lamp) starts counting from, and that next net's own
///   `debug_assert!(incoming_strength > 0)` requires it to receive a
///   genuinely live value, not the last surviving unit before zero.
///
/// So one bare hop must reserve 2, not 0: the ordinary budget already
/// guarantees a live (non-zero) value *at the socket itself* (a plain
/// mandatory-repeater socket needs nothing more, since a repeater there
/// would refresh to full regardless of how decayed its input was) --
/// reserving 2 more hops' worth keeps that same guarantee two hops further
/// out, at the one cell this single hop's own value actually has to reach
/// to be useful.
///
/// This is a *per-hop* unit, not the whole story: a chain of merges (an OR
/// reduced as a tree, which Yosys builds routinely -- one merge's own
/// output feeding straight into another merge, bare both times) needs one
/// more of these for every additional bare hop the chain has, which
/// [`bare_reserve_for_merge`] is what actually multiplies out per branch.
/// Measured, not assumed: the Verilog-derived seven-segment decoder's own
/// chained merges were the first netlist dense enough to expose a bare
/// branch decaying to exactly the wrong side of a *fixed* one-hop margin --
/// see `docs/superpowers/specs/2026-08-08-gate-types-and-wired-or.md`'s
/// task history.
const BARE_TERMINATION_RESERVE: i32 = 2;

/// The real `reserve` a bare branch feeding into merge gate `into` must
/// budget for its own final approach -- `BARE_TERMINATION_RESERVE` for
/// reaching `into`'s own pin, plus, if `into`'s own output is *itself* a
/// bare branch into a further merge, that merge's own requirement too,
/// recursively.
///
/// A net's fan-out decides where the chain stops, the same fanout rule
/// that decides bare vs. isolated everywhere else in this module
/// (`merge_branch_is_bare`): if `into`'s own output net has more than one
/// sink, or one sink that is not itself a merge, whatever terminates there
/// resets the budget on its own (a mandatory repeater always refreshes to
/// full, and a lamp reads whatever arrives with no requirement past it) --
/// so the chain, and the extra reserve it costs, goes no further than
/// `into` itself.
fn bare_reserve_for_merge(netlist: &Netlist, nets: &[Net], into: usize) -> i32 {
    let Some(net) = nets
        .iter()
        .find(|n| matches!(n.source, Source::Gate(g) if g == into))
    else {
        // `into`'s own output feeds nothing further (a declared output
        // only) -- the chain stops at its pin, same base reserve.
        return BARE_TERMINATION_RESERVE;
    };
    let mut sinks = net.sinks.iter().flatten().copied();
    let Some((first_gate, _)) = sinks.next() else {
        return BARE_TERMINATION_RESERVE;
    };
    let single_sink = sinks.all(|(g, _)| g == first_gate);
    if single_sink && netlist.gates[first_gate].is_merge() {
        BARE_TERMINATION_RESERVE + bare_reserve_for_merge(netlist, nets, first_gate)
    } else {
        BARE_TERMINATION_RESERVE
    }
}

/// Where a **bypassed** net's own path must actually start from, instead of
/// its raw `pin` (its source's own `gate_pin`/`lever_pin` entry), whenever
/// (a) the net's source is a merge gate and (b) a bend is actually needed to
/// get off that column at all (`pin.x != exit_x`) -- one hop further out
/// from `pin`, along `geometry::output_direction`, than the ordinary pin.
/// Returns `pin` itself, unchanged, in every other case.
///
/// This has to be a *shifted starting position*, not an extra waypoint on
/// top of the existing bend -- `lay_bent_path`/`lay_bent_path_bare` both
/// document their `waypoints` list as never more than two entries (one
/// optional bend, then the destination), and their whole shared
/// bend-avoidance/strength-budget machinery (`bent_path_bends`,
/// `plan_bent_path`) is built on that shape. Tried as a third waypoint
/// first: it does not merely violate a documented assumption, it breaks
/// two real invariants at once -- `bent_path_bends` marks the new waypoint
/// itself as an extra forbidden repeater position (on top of the real bend),
/// which shifted an interior refresh repeater late enough to let a real
/// branch decay to zero before reaching it (confirmed: `and4`'s
/// `[0,0,1,1]`/`[0,1,1,1]`/`[1,0,1,1]` truth-table rows went from correct to
/// wrong when this was tried as a waypoint); and where `pin.x == exit_x` and
/// `socket.x == exit_x` collapse to a single waypoint whose X and Z both
/// differ from `pin` at once, `direction_from` cannot express a diagonal
/// step at all -- it silently decides on X alone, and `bent_path_cells`'s
/// `while pos != waypoint` loop then never terminates, since `pos` only
/// ever moves in X while `waypoint`'s own Z stays forever out of reach
/// (confirmed: reproduced the exact "memory allocation of 103079215104
/// bytes failed" this produces). Shifting the *start* instead keeps the
/// two-entries-total shape completely unchanged; the one extra hop this
/// costs is paid explicitly by the caller (write the one dust cell between
/// `pin` and this position, claim it under the same net, and decay
/// `net_source_strength` by one more before calling `lay_bent_path*`) rather
/// than folded into machinery that was never designed to know about it.
///
/// # Why the extra hop is needed at all
///
/// A merge's own bare-socket columns sit *on* its own row (`row_z[row_of[g]]`
/// for merge `g`), completely unprotected by a repeater along most of their
/// length (that is the entire point of a bare join -- see `GateKind::Or`'s
/// doc comment). `pin` itself is one hop off that row (`emit`'s own
/// `gate_pin` convention), so a bend placed at `pin`'s own row draws a new
/// horizontal dust run exactly one cell -- not two -- from the merge's row:
/// exactly the `dust_reach`-derived unsafe distance
/// (`docs/superpowers/specs/2026-08-09-channel-safety-condition.md`'s "gap
/// of at least 2"), regardless of which column the bend's own run happens to
/// cross.
///
/// This is not a hypothetical: it produces a real, physical loop, confirmed
/// with the real `Simulator`. `and4`'s Yosys-derived netlist chains two bare
/// merges (`g2 = g0 OR g1`, feeding bare into `g6 = g2 OR g5`); `g6`'s own
/// outbound net bends at exactly `g6`'s pin's row, one cell from `g6`'s own
/// row, and that bend's run happens to cross both of `g6`'s own bare-socket
/// columns end to end. One of those sockets needs an interior refresh
/// repeater (`plan_bent_path`, over the branch's real routed distance); its
/// output and input dust cells turn out to sit on either side of that same
/// one-cell-adjacent bend -- so the bend electrically joins the repeater's
/// own output back to its own input, a closed loop that self-sustains
/// whatever value it powers up with regardless of the real logic upstream.
/// Reduced (outside this codebase) to a two-level merge-of-merges with no
/// Yosys involved at all, the same loop reproduces on the same geometry,
/// confirming it is a property of this shape, not of ABC's particular
/// choices.
///
/// The fix costs one extra hop of straight travel in
/// `geometry::output_direction` before the bend is allowed to turn -- moving
/// the bend's own row to distance 2 from the merge's row, which
/// `dust_reach`'s exhaustive case list already proves is always safe,
/// independent of column. Only applied when the source is a merge (an
/// ordinary gate's own pin sits one hop from a row whose *sockets* are always
/// repeater-terminated, so the identical geometry is already safe there --
/// see this same spec's "repeater is a real firewall" point).
///
/// `facing` is the *source* gate's, not the sink's: the hop is measured off
/// the merge's own output face, so it is that gate's cell this turns. Three
/// of the four callers hand over a real one. `emit` passes its own binding,
/// which is what decided how the gate at `net.source` was actually built.
/// `bare_branch_landing_strength` passes that same binding, threaded down to
/// it through `compute_net_source_strengths`, so the strength it predicts is
/// measured along the geometry `emit` will really lay.
/// `routing_stats::scan_bypass` passes the finished circuit's own
/// `gate_facings` entry for that source gate, read by `routing_stats::
/// source_pin` off the `CompiledCircuit` it is scanning (for a lever-sourced
/// net that pair carries north, but a lever is never a merge, so no lever
/// ever reaches the shift below).
///
/// Only `resolve_bypass_and_geometry` still passes a literal north, and not
/// for want of threading: it is what *decides* which nets get a bypass, so it
/// runs before the real world is built. It does call `emit` first, into a
/// scratch probe world it drops, but what comes back is an `EmitResult` of
/// positions and anchors -- no facings -- and the probe's own facings are that
/// emitter's literal. So there is nothing to read either way. See its own
/// comment.
fn bypass_source_start(
    netlist: &Netlist,
    net: &Net,
    pin: Position,
    exit_x: i32,
    facing: geometry::CellFacing,
) -> Position {
    let source_is_merge = matches!(net.source, Source::Gate(g) if netlist.gates[g].is_merge());
    if source_is_merge && pin.x != exit_x {
        pin.offset(geometry::output_direction(facing))
    } else {
        pin
    }
}

/// 編譯過程的錯誤。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompileError {
    /// A gate is not something redstone builds: either it is still at the
    /// gate level (an `$_AND_`, a `$_MUX_`) and needs [`lowering::lower`]
    /// run over it first, or its declared arity disagrees with how many
    /// inputs it actually has. Reached before anything is placed -- see
    /// [`compile`]'s own doc comment for why this is an error rather than an
    /// implicit lowering.
    NotRealisable {
        gate: String,
        kind: topology::GateKind,
    },
    /// 網表裡有迴路
    CyclicNetlist,
    /// 訊號沒有驅動來源
    UndrivenSignal(String),
    /// Legacy route metadata assigns a physical cell to the wrong net (or to
    /// more than one net). This is the reservation/spacing invariant that
    /// guards the other physical checks' ownership input.
    SpacingViolation {
        cell: (i32, i32, i32),
        expected_net: String,
        found_net: Option<String>,
    },
    /// Candidate metadata cannot be realised against the physical legacy
    /// replay target. This distinguishes an identity/style mismatch from a
    /// legal circuit that merely fails a redstone invariant.
    CandidateMetadataViolation { item: String, reason: String },
    /// A report that reads the row/channel/track emitter's geometry was asked
    /// about a circuit that emitter did not lay out.
    ///
    /// `compile::routing_stats` recomputes `build_floorplan`, `build_nets` and
    /// `resolve_bypass_and_geometry` from the netlist and then reads the
    /// compiled world *along the coordinates that geometry implies*. On a
    /// relaxation-placed world those coordinates address a layout that is not
    /// there, and the answer is not merely wrong: `scan_dust_run` walks in a
    /// straight line from a start toward a stop that is no longer on the same
    /// line, so before this refusal existed it **did not terminate**. Measured
    /// 2026-08-16: four `routing_stats` tests hung indefinitely the moment
    /// `compile` started placing and4 by relaxation.
    ///
    /// A latent trap rather than a new one -- `compile_planned` has produced
    /// such worlds since Task 10, and `analyze` is `pub` -- but the hybrid is
    /// what put it on the front path.
    NotALegacyLayout { report: String },
    /// Two nets' routed dust physically joined into one electrical network --
    /// the connectivity invariant `compile` checks right before it would
    /// otherwise return a circuit (see `verify_connectivity`). This is the
    /// only failure mode this module cannot attribute to a specific input:
    /// it means the router itself produced a world whose actual
    /// connectivity does not match the netlist that asked for it.
    ConnectivityViolation {
        /// The dust cell where the merge was discovered while walking the
        /// network outward from `expected_cell`.
        cell: (i32, i32, i32),
        /// The net `cell` belongs to by the router's own bookkeeping --
        /// not the net whose network reached it.
        found_net: String,
        /// The cell that first established which net this whole dust
        /// network belongs to.
        expected_cell: (i32, i32, i32),
        /// The net `expected_cell` -- and therefore, if the invariant held,
        /// every dust cell reachable from it -- belongs to.
        expected_net: String,
    },
    /// A gate's geometry does not actually implement a NOR -- the invariant
    /// `verify_torch_merge` checks right after `verify_connectivity` (see
    /// that function's doc comment for exactly what each variant asserts
    /// and why `verify_connectivity` cannot catch it: that check proves a
    /// net reaches the cells it should, never whether a torch inverts).
    TorchMergeViolation {
        /// The gate whose geometry failed to check out.
        gate: String,
        reason: TorchMergeFailure,
    },
    /// A net's own routed geometry is structurally connected -- connectivity
    /// and torch-merge both pass -- but does not actually deliver a
    /// non-zero signal to one of its declared sinks once real strength
    /// decay and repeater refresh are accounted for (see
    /// `verify_signal_strength`'s doc comment). This is the failure mode
    /// neither of the first two invariants can see: a dust run one block
    /// too long, or a repeater whose output lands somewhere nothing
    /// continues from, both leave the world perfectly connected and every
    /// torch genuinely inverting -- and silently wrong anyway.
    SignalStrengthViolation {
        /// The net that fails to deliver.
        net: String,
        /// Which of the net's own declared sinks never saw a non-zero
        /// signal.
        sink: SignalSink,
    },
}

/// Which declared sink of a net `verify_signal_strength` found undelivered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignalSink {
    /// A gate input -- specifically the support block its route terminates
    /// against, the same cell `verify_torch_merge` already proved is
    /// *structurally* reached by this net.
    GateInput {
        gate: String,
        support: (i32, i32, i32),
    },
    /// A declared circuit output's lamp.
    OutputLamp {
        output: String,
        lamp: (i32, i32, i32),
    },
}

/// Which condition of the torch-merge invariant failed. See
/// `verify_torch_merge`'s doc comment for how each of these is derived from
/// the simulator's own rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TorchMergeFailure {
    /// The gate's declared output position holds nothing `component::
    /// torch_support_position` can resolve a support for -- either it is
    /// not a torch at all, or it is a wall torch with no recorded `facing`.
    NoSupport { torch: (i32, i32, i32) },
    /// The support block fails `taxonomy::flags_of(..).is_conductive()` --
    /// the same gate `propagate::block_signal_at` itself enforces before it
    /// will ever report a block as powered. No input could ever invert
    /// this torch, no matter how it is wired, because the support can
    /// never be observed as powered at all.
    SupportNotConductive {
        torch: (i32, i32, i32),
        support: (i32, i32, i32),
    },
    /// A declared input net never structurally reaches the support block --
    /// driving that input would never darken this torch.
    InputDoesNotReachSupport {
        torch: (i32, i32, i32),
        support: (i32, i32, i32),
        input: String,
    },
    /// A net that is *not* one of the gate's declared inputs also
    /// structurally reaches the support block, corrupting the merge --
    /// driving that unrelated net would darken a torch it has no business
    /// influencing.
    ForeignNetReachesSupport {
        torch: (i32, i32, i32),
        support: (i32, i32, i32),
        net: String,
    },
    /// The torch's own output leaks into some other net's conductor
    /// instead of only the net it is meant to drive. A torch does not
    /// power the block it is attached to (that asymmetry is what makes it
    /// invert), but it powers every *other* neighbour -- so a stray
    /// conductor sitting on any of those other faces gets fed by this
    /// gate's own output.
    OutputLeaksIntoForeignNet {
        torch: (i32, i32, i32),
        leaked_cell: (i32, i32, i32),
        net: String,
    },
}

impl std::fmt::Display for CompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompileError::NotRealisable { gate, kind } => write!(
                f,
                "gate `{gate}` is a {kind:?}, which is not something redstone builds -- run                  `compile::lowering::lower` on this netlist first"
            ),
            CompileError::CyclicNetlist => write!(f, "netlist has a cycle"),
            CompileError::UndrivenSignal(name) => write!(f, "signal `{name}` is never driven"),
            CompileError::SpacingViolation { cell, expected_net, found_net } => write!(
                f,
                "spacing violation: route metadata assigns {cell:?} to `{expected_net}`, but the reservation records {}",
                found_net.as_deref().unwrap_or("no net")
            ),
            CompileError::CandidateMetadataViolation { item, reason } => {
                write!(f, "candidate metadata violation for {item}: {reason}")
            }
            CompileError::ConnectivityViolation { cell, found_net, expected_cell, expected_net } => {
                write!(
                    f,
                    "connectivity violation: the dust at {cell:?} belongs to net `{found_net}`, \
                     but is electrically connected to the network of net `{expected_net}` \
                     (established at {expected_cell:?})"
                )
            }
            CompileError::TorchMergeViolation { gate, reason } => match reason {
                TorchMergeFailure::NoSupport { torch } => write!(
                    f,
                    "torch-merge violation: gate `{gate}`'s output torch at {torch:?} has no \
                     resolvable support block"
                ),
                TorchMergeFailure::SupportNotConductive { torch, support } => write!(
                    f,
                    "torch-merge violation: gate `{gate}`'s torch at {torch:?} is attached to \
                     {support:?}, which is not conductive and can never be observed as powered -- \
                     this torch can never invert"
                ),
                TorchMergeFailure::InputDoesNotReachSupport { torch, support, input } => write!(
                    f,
                    "torch-merge violation: gate `{gate}`'s input `{input}` never reaches the \
                     support block {support:?} of its torch at {torch:?} -- driving `{input}` \
                     would never affect this gate"
                ),
                TorchMergeFailure::ForeignNetReachesSupport { torch, support, net } => write!(
                    f,
                    "torch-merge violation: net `{net}`, which is not a declared input of gate \
                     `{gate}`, reaches the support block {support:?} of its torch at {torch:?} -- \
                     driving `{net}` would corrupt this gate's output"
                ),
                TorchMergeFailure::OutputLeaksIntoForeignNet { torch, leaked_cell, net } => write!(
                    f,
                    "torch-merge violation: gate `{gate}`'s torch at {torch:?} powers \
                     {leaked_cell:?}, which belongs to net `{net}` -- the torch's own output is \
                     leaking into a net it does not drive"
                ),
            },
            CompileError::SignalStrengthViolation { net, sink } => match sink {
                SignalSink::GateInput { gate, support } => write!(
                    f,
                    "signal-strength violation: net `{net}` never delivers a non-zero signal to \
                     gate `{gate}`'s support block {support:?} -- the geometry is structurally \
                     connected but the real, decayed signal dies out before it arrives"
                ),
                SignalSink::OutputLamp { output, lamp } => write!(
                    f,
                    "signal-strength violation: net `{net}` never delivers a non-zero signal to \
                     output `{output}`'s lamp at {lamp:?} -- the geometry is structurally \
                     connected but the real, decayed signal dies out before it arrives"
                ),
            },
            CompileError::NotALegacyLayout { report } => write!(
                f,
                "{report} reads the row/channel/track emitter's geometry, and this circuit was \
                 not laid out by it -- see `CompiledCircuit::planner_kind`"
            ),
        }
    }
}

impl std::error::Error for CompileError {}

/// 編譯完成的電路。
pub struct CompiledCircuit {
    pub world: World,
    /// 每個輸入訊號的拉桿座標
    pub input_positions: BTreeMap<String, (i32, i32, i32)>,
    /// Each output signal's reading point -- the coordinate of the redstone
    /// lamp that lights up when the signal is high, not the internal NOR
    /// gate's output torch. This is what a person standing in front of the
    /// pasted circuit actually looks at.
    pub output_positions: BTreeMap<String, (i32, i32, i32)>,
    /// Every gate's actual output position -- the wall torch that is this
    /// gate's real output -- keyed by the gate's output signal name.
    ///
    /// Unlike `output_positions`, which only covers the netlist's *declared*
    /// outputs and points one game tick further down the line at the
    /// readable lamp, this covers every gate including purely internal ones
    /// with no lamp at all. This is what dynamic timing analysis
    /// (`crate::timing`) watches to measure every net, not just the ones a
    /// person reads -- the lamp's own extra delay
    /// (`LAMP_TURN_ON_DELAY_GAME_TICKS` / `LAMP_TURN_OFF_DELAY_GAME_TICKS`)
    /// is a display convenience, not part of the logic.
    pub gate_output_positions: BTreeMap<String, (i32, i32, i32)>,
    /// Which way each gate's cell was built, by gate index.
    ///
    /// Recorded by whoever placed it, never read back off the world: a merge's
    /// junction is dust, and dust has no facing to read. A verifier that
    /// re-derives a gate's socket faces has to be told which faces those are.
    pub gate_facings: Vec<geometry::CellFacing>,
    /// Explicit ownership data recorded while the legacy emitter places the
    /// world.  This is intentionally not reconstructed from block kinds.
    ///
    /// `None` for a circuit the planner placed itself: there was no legacy
    /// emitter involved, and nothing downstream may pretend otherwise.
    legacy_emission: Option<LegacyEmission>,
    planner_kind: PlannerKind,
}

impl CompiledCircuit {
    pub(crate) fn legacy_emission(&self) -> Option<&LegacyEmission> {
        self.legacy_emission.as_ref()
    }

    /// Which stage built `world`.
    pub fn planner_kind(&self) -> PlannerKind {
        self.planner_kind
    }
}

/// The legacy emitter's complete, replayable physical realisation.
#[derive(Debug, Clone)]
pub(crate) struct LegacyEmission {
    netlist: Netlist,
    primitive_anchors: Vec<Anchor>,
    primitive_nodes: Vec<PrimitiveNode>,
    routes: Vec<LegacyRoute>,
}

impl LegacyEmission {
    pub(crate) fn netlist(&self) -> &Netlist {
        &self.netlist
    }

    pub(crate) fn primitive_anchors(&self) -> &[Anchor] {
        &self.primitive_anchors
    }

    pub(crate) fn primitive_nodes(&self) -> &[PrimitiveNode] {
        &self.primitive_nodes
    }

    pub(crate) fn routes(&self) -> &[LegacyRoute] {
        &self.routes
    }
}

/// Per-net metadata captured at each legacy routing write.
#[derive(Debug, Clone)]
pub(crate) struct LegacyRoute {
    owner: String,
    anchors: Vec<Anchor>,
    terminals: Vec<RouteTerminal>,
    /// The block written into each anchor, parallel to `anchors`.
    blocks: Vec<BlockState>,
    /// The block one cell below each anchor, parallel to `anchors`.
    floors: Vec<BlockState>,
}

impl LegacyRoute {
    pub(crate) fn owner(&self) -> &str {
        &self.owner
    }

    pub(crate) fn anchors(&self) -> &[Anchor] {
        &self.anchors
    }

    pub(crate) fn terminals(&self) -> &[RouteTerminal] {
        &self.terminals
    }

    pub(crate) fn blocks(&self) -> &[BlockState] {
        &self.blocks
    }

    pub(crate) fn floors(&self) -> &[BlockState] {
        &self.floors
    }
}

/// 把一格地板鋪在 `pos` 正下方，讓紅石粉／拉桿／中繼器能立在上面。
pub(crate) fn ensure_floor(world: &mut World, pos: Position) {
    let floor = pos.down();
    world.set(floor.x, floor.y, floor.z, stone());
}

/// 從 `start` 走到 `end`（兩者必須沿同一軸對齊）是哪個方向。
pub(crate) fn direction_from(start: Position, end: Position) -> Facing {
    if end.x != start.x {
        if end.x > start.x {
            Facing::East
        } else {
            Facing::West
        }
    } else if end.z != start.z {
        if end.z > start.z {
            Facing::South
        } else {
            Facing::North
        }
    } else {
        unreachable!("direction_from 只處理水平直線上、起訖點不同的兩點")
    }
}

/// Number of cells strictly between `start` and `stop_before` along
/// `direction` -- the same count `lay_dust_run` would place blocks into, if
/// it were called with these same three arguments. Pure position arithmetic,
/// so the "Strength planning" pass in `compile` can use it to size a run
/// before any block for it exists.
fn straight_run_length(start: Position, direction: Facing, stop_before: Position) -> i32 {
    let mut len = 0;
    let mut pos = start.offset(direction);
    while pos != stop_before {
        len += 1;
        pos = pos.offset(direction);
    }
    len
}

/// Decide where repeaters must land along a straight run of `len` cells, and
/// report the strength of the last one -- what continues past it, whether
/// that is another run or a ramp. Pure (no `World`), so it can be replayed
/// during strength planning to learn a value before any block for it exists;
/// `lay_dust_run` below calls it to decide what to write.
///
/// `incoming_strength` is the strength of the cell immediately before this
/// run (the last cell of whatever preceded it: an active component, or
/// another run's own last cell). `reserve` is how much strength must remain
/// unspent in the last cell for whatever immediately follows to survive --
/// see `MAX_DUST_RUN`'s comment for the derivation of `RAMP_LENGTH` as that
/// reserve for a run that empties into a ramp, and 0 for one that empties
/// into a mandatory terminating repeater instead (which cannot die no matter
/// what strength reaches it, as long as it is nonzero).
fn plan_straight_run(len: i32, incoming_strength: u8, reserve: i32) -> (Vec<bool>, u8) {
    debug_assert!(
        incoming_strength > 0,
        "a run cannot start from an already-dead signal"
    );
    let threshold = (MAX_DUST_RUN - reserve) as i64;
    let mut is_repeater = vec![false; len.max(0) as usize];
    let mut last_refresh: i64 = incoming_strength as i64 - (MAX_SIGNAL_STRENGTH as i64 + 1);
    let mut strength = incoming_strength;
    for (i, slot) in is_repeater.iter_mut().enumerate() {
        if (i as i64) - last_refresh > threshold {
            *slot = true;
            last_refresh = i as i64;
            strength = MAX_SIGNAL_STRENGTH;
        } else {
            strength -= 1;
        }
    }
    (is_repeater, strength)
}

/// Lay dust from `start` (exclusive, already lit at `incoming_strength`)
/// along `direction` to `stop_before` (exclusive), inserting a repeater
/// wherever `plan_straight_run` decides the budget is spent. Returns the
/// strength of the last cell placed (or `incoming_strength` unchanged if
/// `start` and `stop_before` turn out to be adjacent -- nothing to lay).
fn lay_dust_run(
    world: &mut World,
    start: Position,
    direction: Facing,
    stop_before: Position,
    incoming_strength: u8,
    reserve: i32,
    route: &mut Route,
) -> u8 {
    let len = straight_run_length(start, direction, stop_before);
    let (is_repeater, ending_strength) = plan_straight_run(len, incoming_strength, reserve);

    let mut pos = start.offset(direction);
    for &place_repeater in &is_repeater {
        ensure_floor(world, pos);
        route.claim(pos.down());
        if place_repeater {
            world.set(pos.x, pos.y, pos.z, repeater(direction));
            route.note_repeater();
        } else {
            world.set(pos.x, pos.y, pos.z, dust());
        }
        route.claim(pos);
        pos = pos.offset(direction);
    }
    ending_strength
}

/// Break a multi-segment axis-aligned path from `start` through every point
/// of `waypoints` in order into its individual cells, in write order. Each
/// consecutive pair -- `start`/`waypoints[0]`, then each pair after it --
/// must share exactly one axis, same as every other segment helper here.
/// Pure position arithmetic (mirrors `straight_run_length`'s role for a
/// single segment), so `routing_stats` can replay the exact same cell list
/// `lay_bent_path` below writes without touching a `World`.
fn bent_path_cells(start: Position, waypoints: &[Position]) -> Vec<Position> {
    let mut cells = Vec::new();
    let mut prev = start;
    for &waypoint in waypoints {
        let direction = direction_from(prev, waypoint);
        let mut pos = prev.offset(direction);
        while pos != waypoint {
            cells.push(pos);
            pos = pos.offset(direction);
        }
        cells.push(waypoint);
        prev = waypoint;
    }
    cells
}

/// What actively terminates an ordinary route at a NOR support.
///
/// This is fixed before the final record/enforce emissions.  A directed-dust
/// endpoint changes neither the path nor its occupied cell; it changes only
/// the component in that already-reserved final cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TerminalKind {
    RepeaterIntoSupport,
    DirectedDustIntoSupport,
}

impl From<TerminalKind> for RouteTerminalKind {
    fn from(kind: TerminalKind) -> Self {
        match kind {
            TerminalKind::RepeaterIntoSupport => Self::RepeaterIntoSupport,
            TerminalKind::DirectedDustIntoSupport => Self::DirectedDustIntoSupport,
        }
    }
}

/// One terminal decision for every `nets[net][channel][sink]` socket.
type TerminalKinds = Vec<Vec<Vec<TerminalKind>>>;

fn default_terminal_kinds(nets: &[Net]) -> TerminalKinds {
    nets.iter()
        .map(|net| {
            net.sinks
                .iter()
                .map(|sinks| vec![TerminalKind::RepeaterIntoSupport; sinks.len()])
                .collect()
        })
        .collect()
}

/// Lay a multi-segment axis-aligned dust path from `start` (exclusive,
/// already lit at `incoming_strength`) through every point of `waypoints` in
/// order. `terminal` was selected from the fully-recorded baseline geometry:
/// it either preserves the old repeater endpoint or leaves this final cell as
/// a straight dust run that powers the support itself.
///
/// This is the one path-laying primitive every socket termination in this
/// module uses, whatever kind of route it ends: `compute_bypass`'s direct
/// routes (which may bend zero or one time getting from the source pin onto
/// the sink's approach column), and `emit`'s ordinary post-track/ramp
/// Columns pass (which may bend zero or one time getting from that approach
/// column onto the socket itself -- zero for a south input, whose socket
/// already sits on the column's own line of travel, one for a west/east
/// input, whose socket sits to the side of it; see `approach_column`'s doc
/// comment). Both call sites hand this a `waypoints` list with at most two
/// entries for exactly that reason -- an optional bend, then the socket.
///
/// The one waypoint before the last, if there is one, stays plain dust,
/// because the path changes axis there: a repeater only reads what is
/// directly behind it, so one sitting where the path turns would not be
/// connected to the segment after the turn at all. This does *not* force a
/// mandatory refresh at that turn -- a corner costs exactly the same one hop
/// of strength a straight cell does
/// (`recompute_dust_strengths`'s BFS does not distinguish them), so forcing
/// one would spend a repeater a short route never needs, which is exactly
/// what made an earlier version of this function's own bypass-only
/// predecessor regress settle time instead of improving it, and exactly what
/// the old cell-based design's dedicated `lay_segment_to_corner` paid on
/// every west/east gate-entry edge before this function took over that job
/// too. Instead the whole path shares one strength budget end to end,
/// exactly as if it had no bends at all, with turns simply excluded from
/// ever hosting the occasional repeater that budget calls for -- mirrors
/// `plan_track_run`'s handling of taps it must route around.
///
/// This path always ends in its own mandatory repeater, so nothing after it
/// needs preserved strength -- `reserve` is 0.
/// Returns the terminal style actually built, which is not always the one
/// asked for: `DirectedDustIntoSupport` is a preference, and `plan_bent_path`
/// overrides it whenever the run cannot reach the last cell without a
/// refresh. `lay_bent_path_bare` has always reported its realised kind; this
/// one used to let its caller record the request instead, so a plan could
/// claim a dust terminal the world did not have.
fn lay_bent_path(
    world: &mut World,
    start: Position,
    waypoints: &[Position],
    incoming_strength: u8,
    terminal: TerminalKind,
    route: &mut Route,
) -> RouteTerminalKind {
    debug_assert!(
        !waypoints.is_empty(),
        "a bent path must have somewhere to end"
    );
    debug_assert!(
        incoming_strength > 0,
        "a run cannot start from an already-dead signal"
    );

    let cells = bent_path_cells(start, waypoints);
    let bend_indices = bent_path_bends(&cells, waypoints);
    let (mut is_repeater, _ending_strength) =
        plan_bent_path(cells.len(), &bend_indices, incoming_strength, 0);
    match terminal {
        TerminalKind::RepeaterIntoSupport => {
            // The final cell is a mandatory repeater regardless of the budget
            // -- `waypoints`'s last element is never a bend, so this can
            // never collide with `bend_indices`.
            is_repeater[cells.len() - 1] = true;
        }
        TerminalKind::DirectedDustIntoSupport => {
            // Dust is preferred only when the run can already reach this
            // last cell without a refresh.  `plan_bent_path` owns that
            // strength calculation; if it needs this cell as a repeater,
            // retaining the repeater is the only correct physical result.
            // In particular, a geometric dust candidate must never turn a
            // long, otherwise valid route into a dead line merely to save a
            // component at its socket.
        }
    }

    let mut prev = start;
    for (index, &pos) in cells.iter().enumerate() {
        let direction = direction_from(prev, pos);
        ensure_floor(world, pos);
        route.claim(pos.down());
        if is_repeater[index] {
            world.set(pos.x, pos.y, pos.z, repeater(direction));
            route.note_repeater();
        } else {
            world.set(pos.x, pos.y, pos.z, dust());
        }
        route.claim(pos);
        prev = pos;
    }

    if is_repeater[cells.len() - 1] {
        RouteTerminalKind::RepeaterIntoSupport
    } else {
        RouteTerminalKind::DirectedDustIntoSupport
    }
}

/// The bend positions of a bent path from `start` through `waypoints`,
/// found by looking each waypoint-before-the-last up in its own already-
/// computed `cells` list -- shared by `lay_bent_path`, `lay_bent_path_bare`,
/// and `bare_branch_landing_strength` (the merge-junction strength
/// pre-pass's planning-only query), so all three can never derive a
/// different bend set for what is geometrically the same path. A repeater
/// must never land on one of these: it only reads what is directly behind
/// it, and a path changes axis at a bend (see `lay_bent_path`'s own doc
/// comment).
fn bent_path_bends(cells: &[Position], waypoints: &[Position]) -> BTreeSet<usize> {
    waypoints[..waypoints.len() - 1]
        .iter()
        .map(|&waypoint| {
            cells.iter().position(|&cell| cell == waypoint).expect(
                "every waypoint before the last is pushed onto `cells` by `bent_path_cells`",
            )
        })
        .collect()
}

/// `bent_path_bends`, plus the path's own last cell -- what a **bare**
/// merge branch's ending needs on top of an ordinary bent path's bends,
/// and the reason it needs its own function rather than reusing
/// `bent_path_bends` directly (both `lay_bent_path_bare` and
/// `bare_branch_landing_strength` call this, not `bent_path_bends`, so the
/// two can never derive a different forbidden set for the same path).
///
/// A bare branch's final cell is not a gate's own support/torch -- it is
/// the socket, one hop of plain dust from the merge's junction (see
/// `lay_bent_path_bare`'s own doc comment), and that last hop is a bare
/// dust-to-dust join, not a driven connection: nothing propagates *through*
/// a repeater in a direction its own facing was never chosen for, and a
/// repeater placed there has its facing decided by `direction_from`'s
/// *arrival* direction (whatever direction this path's own last segment
/// happens to travel in), not by "towards the junction" -- those only agree
/// by construction for a *bend-avoiding* repeater somewhere in the middle of
/// a run, never for the one cell whose whole reason for existing is to
/// physically touch a *different* net's own dust one hop further on. A
/// repeater landing there is not merely redundant, it makes that hop
/// electrically one-directional (a repeater's back side carries nothing),
/// silently turning off the two nets' own real bare join -- so the
/// terminal cell has to be forbidden from ever hosting one, on top of the
/// existing bends, using the exact same "walk back to the nearest eligible
/// cell" mechanism `plan_bent_path` already runs for a bend. Confirmed by a
/// real failure this fixed: `and4`'s Yosys-derived netlist chains two
/// bare merges (`NOT NOT` folded into a bare-branch OR of ORs), and its own
/// last bent-path segment is short enough, at a low enough incoming
/// strength, that the ordinary per-hop budget lands its one needed refresh
/// repeater exactly on the last index -- see
/// `docs/superpowers/specs/2026-08-08-gate-types-and-wired-or.md`'s task
/// history for the truth-table mismatch this produced before the fix.
fn bare_ending_bend_indices(cells: &[Position], waypoints: &[Position]) -> BTreeSet<usize> {
    let mut bends = bent_path_bends(cells, waypoints);
    if !cells.is_empty() {
        bends.insert(cells.len() - 1);
    }
    bends
}

/// The `is_repeater` assignment for a `len`-cell bent path (`bend_indices`
/// never eligible for one -- same decision `plan_track_run` makes,
/// generalised from X-coordinate taps to index-based ones: when the budget
/// would force a repeater onto a bend, it goes on the last non-bend cell
/// before it instead), plus the real strength the path's own last cell ends
/// up carrying under that assignment: `MAX_SIGNAL_STRENGTH` if a repeater
/// naturally lands there, otherwise decayed by one hop per cell since
/// whatever last refreshed it (another repeater, or the path's own start) --
/// the same two facts `plan_straight_run` already encodes for a plain
/// straight run, generalised past bends (which never host a repeater, and
/// so never interrupt the decay either -- see `lay_bent_path`'s own doc
/// comment for why that is correct and not merely convenient).
///
/// `lay_bent_path` and `lay_bent_path_bare` both build their own real
/// `is_repeater` assignment from this (the former then forces its own
/// mandatory final repeater on top); the ending strength is what
/// `bare_branch_landing_strength` needs, to learn what a **bare** ending
/// actually delivers before any block for it exists.
pub(crate) fn plan_bent_path(
    len: usize,
    bend_indices: &BTreeSet<usize>,
    incoming_strength: u8,
    reserve: i32,
) -> (Vec<bool>, u8) {
    let threshold = (MAX_DUST_RUN - reserve) as i64;
    let mut is_repeater = vec![false; len];
    let mut last_refresh: i64 = incoming_strength as i64 - (MAX_SIGNAL_STRENGTH as i64 + 1);
    let mut i = 0usize;
    while i < len {
        if (i as i64) - last_refresh <= threshold {
            i += 1;
            continue;
        }
        let mut j = i;
        while j > 0 && (j as i64) > last_refresh + 1 && bend_indices.contains(&j) {
            j -= 1;
        }
        if bend_indices.contains(&j) {
            // Nowhere in this run can hold the refresh: every candidate cell
            // is a bend or a staircase step. The row/channel router never
            // produces such a run -- its bends are sparse by construction --
            // but the planner's own routes climb, and a climb is a solid line
            // of cells no repeater can stand on. Leave it unplaced and let
            // `verify_signal_strength` report what that costs, rather than
            // walking `j` off the bottom of the array on the way there.
            i += 1;
            continue;
        }
        is_repeater[j] = true;
        last_refresh = j as i64;
        i = j + 1;
    }
    let ending_strength = if len == 0 {
        incoming_strength
    } else if is_repeater[len - 1] {
        MAX_SIGNAL_STRENGTH
    } else {
        (MAX_SIGNAL_STRENGTH as i64 - ((len as i64 - 1) - last_refresh)) as u8
    };
    (is_repeater, ending_strength)
}

/// Same as [`lay_bent_path`], but the final cell is left as whatever the
/// ordinary strength budget decides -- plain dust, unless this particular
/// branch happens to be long enough to need an interior refresh anyway --
/// rather than a *mandatory* repeater. Returns the real strength that final
/// cell ends up carrying (see [`plan_bent_path`]) -- `compute_net_source_
/// strengths` needs the identical answer before any block exists, via
/// `bare_branch_landing_strength`, which is why both go through the same
/// `plan_bent_path` core rather than each risking their own copy of this
/// budget/bend-avoidance logic.
///
/// This is the termination a wire-merge OR's own **private** branches use
/// (see `merge_branch_is_bare`): the destination is another net's own dust,
/// not a gate's support block, and dust joins dust directly (the same
/// same-layer rule `dust_connections` already applies everywhere else), so
/// nothing has to actively drive the join at all -- see
/// `docs/superpowers/specs/2026-08-08-gate-types-and-wired-or.md`, "In
/// redstone an OR is free". A branch whose source fans out anywhere besides
/// the merge it feeds must **not** use this -- it needs `lay_bent_path`'s
/// mandatory repeater instead, so backflow through the merge can never run
/// back up the shared wire to that other consumer.
///
/// `reserve` is [`bare_reserve_for_merge`]'s answer for whichever merge
/// this branch actually feeds, not a fixed constant -- a chain of merges
/// needs more than one hop's worth (see that function's own doc comment).
fn lay_bent_path_bare(
    world: &mut World,
    start: Position,
    waypoints: &[Position],
    incoming_strength: u8,
    reserve: i32,
    route: &mut Route,
) -> (u8, RouteTerminalKind) {
    debug_assert!(
        !waypoints.is_empty(),
        "a bent path must have somewhere to end"
    );
    debug_assert!(
        incoming_strength > 0,
        "a run cannot start from an already-dead signal"
    );

    let cells = bent_path_cells(start, waypoints);
    let bend_indices = bare_ending_bend_indices(&cells, waypoints);
    let (is_repeater, ending_strength) =
        plan_bent_path(cells.len(), &bend_indices, incoming_strength, reserve);

    let mut prev = start;
    for (index, &pos) in cells.iter().enumerate() {
        let direction = direction_from(prev, pos);
        ensure_floor(world, pos);
        route.claim(pos.down());
        if is_repeater[index] {
            world.set(pos.x, pos.y, pos.z, repeater(direction));
            route.note_repeater();
        } else {
            world.set(pos.x, pos.y, pos.z, dust());
        }
        route.claim(pos);
        prev = pos;
    }
    (
        ending_strength,
        if is_repeater[cells.len() - 1] {
            RouteTerminalKind::BareMergeRepeater
        } else {
            RouteTerminalKind::BareMergeDust
        },
    )
}

/// The two horizontal directions perpendicular to `direction` -- the only
/// two `seal_cross_talk` may ever seal.
///
/// This is a fact about *this router's own layout* (a staircase only ever
/// writes its direction of travel and the reverse, so only the sides are
/// ever still open when a landing is placed), not a copy of any connection
/// rule -- the rule itself (which of those side cells actually matters) is
/// `dust_reach`'s job, called below.
fn side_directions(direction: Facing) -> [Facing; 2] {
    match direction {
        Facing::North | Facing::South => [Facing::East, Facing::West],
        Facing::East | Facing::West => [Facing::North, Facing::South],
        Facing::Up | Facing::Down => unreachable!("a ramp only ever travels horizontally"),
    }
}

/// Which net a conductor cell belongs to. Built once, before a single seal
/// is written for real, by replaying the whole circuit's placement against a
/// throwaway world (`Footprint::record`) -- so the real pass already knows,
/// for every net, exactly which cells *every other net* will end up using,
/// not just whatever happens to already be on the page.
type Reservation = HashMap<Position, usize>;

/// One electrical segment of a net's physical route.
///
/// `emit` writes a net's route in three passes -- ramps, then columns, then
/// tracks -- because that is the order the *world* has to be written in: the
/// seals a ramp lays have to be overwritable by the column and the track that
/// run through them. That order is neither the order the signal travels in
/// nor one branch at a time: the Tracks pass lays a single east-west run that
/// every branch of a fanout draws a different *prefix* of.
///
/// So "how many repeaters stand between this net's source and this sink" is
/// not a quantity any one pass can keep as a running total. It is a sum over
/// the segments that one sink's signal passes through, and each pass knows
/// only its own segments. Every repeater is therefore attributed to the
/// segment carrying it at the moment it is placed, and
/// [`resolve_terminal_repeaters`] adds up each sink's own path once all three
/// passes have run.
///
/// That is precisely what the counter this replaces got wrong. `Route` used
/// to carry a per-pass running total, and only the Columns pass ever asked
/// for it, so a terminal recorded the column and gate-entry repeaters and
/// none of the ramp or track ones. Measured over the six reference circuits,
/// that silently dropped 22% to 48% of every repeater the emitter lays, and
/// all of `full_adder`'s 32-against-42 delay gap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Leg {
    /// The `GATE_Y` column feeding slot `slot`'s descending ramp: slot 0's is
    /// the trunk from the source pin, and slot `i > 0`'s is the feed-through
    /// from slot `i - 1`'s landing.
    Column { slot: usize },
    /// The ramp from slot `slot`'s entry column down onto its track.
    RampDown { slot: usize },
    /// Slot `slot`'s track, from its entry column out to the tap at `tap_x`.
    Track { slot: usize, tap_x: i32 },
    /// The ramp from slot `slot`'s track at `tap_x` back up to `GATE_Y`.
    RampUp { slot: usize, tap_x: i32 },
    /// The final branch into one socket -- the bent path from a landing to
    /// the gate's input cell, or, for a bypass net, the entire route.
    Branch { gate: usize, input_index: usize },
}

/// The state one `emit` pass threads through every routing write.
///
/// A keep-out cell only ever needs stone if nothing else in the entire
/// circuit will ever legitimately occupy it -- and that question can only be
/// answered once every net's footprint is known, not one net at a time as
/// the router happens to visit them. So `emit` runs twice: once recording,
/// against a throwaway world, to build a complete `Reservation`; once
/// enforcing, against the real world, consulting that now-complete
/// reservation before it ever writes a seal.
struct Footprint {
    reservation: Reservation,
    recording: bool,
    /// Route cells in the exact ownership context in which `emit` wrote them.
    /// This deliberately records emitter decisions instead of later inferring
    /// ownership by inspecting the finished world's blocks.
    route_anchors: Vec<Vec<Anchor>>,
    /// Every sink's concrete terminal location and style, keyed by route.
    /// This is populated at the actual emit call site, so fanout branches do
    /// not rely on an incidental flattened route order.
    ///
    /// Each terminal's `repeaters` is left at zero here and filled in by
    /// [`resolve_terminal_repeaters`] once all three passes have run -- see
    /// [`Leg`] for why no earlier moment can know the number.
    route_terminals: Vec<Vec<RouteTerminal>>,
    /// Every repeater this pass laid, attributed to the [`Leg`] carrying it.
    repeaters: BTreeMap<(usize, Leg), u64>,
}

impl Footprint {
    fn record() -> Self {
        Footprint {
            reservation: Reservation::new(),
            recording: true,
            route_anchors: Vec::new(),
            route_terminals: Vec::new(),
            repeaters: BTreeMap::new(),
        }
    }

    fn enforce(reservation: Reservation) -> Self {
        Footprint {
            reservation,
            recording: false,
            route_anchors: Vec::new(),
            route_terminals: Vec::new(),
            repeaters: BTreeMap::new(),
        }
    }

    /// Record that `pos` is this net's conductor cell -- dust, a repeater,
    /// or a block that physically supports either. A no-op once the
    /// reservation is complete (`recording == false`): nothing should still
    /// be discovering new cells at that point, only consulting them.
    fn claim(&mut self, pos: Position, net: usize) {
        if self.recording {
            let previous = self.reservation.insert(pos, net);
            if previous.is_none() {
                if self.route_anchors.len() <= net {
                    self.route_anchors.resize_with(net + 1, Vec::new);
                }
                self.route_anchors[net].push(Anchor {
                    x: pos.x,
                    y: pos.y,
                    z: pos.z,
                });
            }
        }
    }

    /// Begin attributing repeaters to `leg`, discarding whatever an earlier
    /// lay of the same leg recorded.
    ///
    /// Re-laying one leg is not a mistake: two exits of the same slot can
    /// share a tap column, in which case the Ramps pass writes that ramp
    /// twice, over itself, with the same repeaters both times. The count has
    /// to replace rather than accumulate or the second lay would double it.
    fn begin_leg(&mut self, net: usize, leg: Leg) {
        self.repeaters.insert((net, leg), 0);
    }

    fn note_repeater(&mut self, net: usize, leg: Leg) {
        *self.repeaters.entry((net, leg)).or_default() += 1;
    }

    /// Record that `count` repeaters stand between slot `slot`'s track entry
    /// column and its tap at `tap_x`.
    ///
    /// The Tracks pass counts its own rather than reporting through
    /// [`Footprint::note_repeater`]: one `lay_track` call fills every tap of
    /// one slot, and each tap needs a different prefix of the same run.
    fn note_track_tap(&mut self, net: usize, slot: usize, tap_x: i32, count: u64) {
        self.repeaters.insert((net, Leg::Track { slot, tap_x }), count);
    }

    /// How many repeaters `net` laid on `leg`. Zero for a leg nothing laid --
    /// a track whose tap *is* its own entry column has no cells at all, and
    /// a ramp shorter than `RAMP_REST_INTERVAL` places no rest stop.
    fn leg_repeaters(&self, net: usize, leg: Leg) -> u64 {
        self.repeaters.get(&(net, leg)).copied().unwrap_or(0)
    }

    fn terminal(
        &mut self,
        net: usize,
        gate: &Gate,
        input_index: usize,
        pos: Position,
        kind: RouteTerminalKind,
    ) {
        if self.recording {
            if self.route_terminals.len() <= net {
                self.route_terminals.resize_with(net + 1, Vec::new);
            }
            self.route_terminals[net].push(RouteTerminal {
                sink: RouteSink {
                    gate: gate.output.clone(),
                    input_index,
                    anchor: Anchor {
                        x: pos.x,
                        y: pos.y,
                        z: pos.z,
                    },
                },
                kind,
                // Filled by `resolve_terminal_repeaters` after the Tracks
                // pass; this pass has not laid the track yet, let alone the
                // one this branch draws from.
                repeaters: 0,
            });
        }
    }

    /// Pair every recorded route cell with the block the recording pass put
    /// there.
    ///
    /// `emitted` must be the world that actually ships, not the recording
    /// pass's scratch copy: the recording pass writes seals the enforcing
    /// pass refuses, so replaying its blocks would put stone where the final
    /// circuit has air. The recorded *ownership* still comes from the
    /// recording pass, which is the only place it exists.
    fn legacy_routes(&self, netlist: &Netlist, nets: &[Net], emitted: &World) -> Vec<LegacyRoute> {
        nets.iter()
            .enumerate()
            .map(|(net, route)| {
                let anchors = self.route_anchors.get(net).cloned().unwrap_or_default();
                let blocks = anchors
                    .iter()
                    .map(|anchor| emitted.get(anchor.x, anchor.y, anchor.z).clone())
                    .collect();
                // The cell each anchor stands on. `ensure_floor` writes
                // unconditionally, so the emitter floors cells that end up
                // empty; which ones is not derivable from the finished
                // blocks, only observable here.
                let floors = anchors
                    .iter()
                    .map(|anchor| emitted.get(anchor.x, anchor.y - 1, anchor.z).clone())
                    .collect();
                LegacyRoute {
                    owner: net_source_name(netlist, route).to_string(),
                    anchors,
                    terminals: self.route_terminals.get(net).cloned().unwrap_or_default(),
                    blocks,
                    floors,
                }
            })
            .collect()
    }
}

/// Which net a routing write belongs to, and where to record the cells it
/// touches -- bundled into one value purely so the low-level writers below
/// take one parameter instead of two (`net` and `footprint` always travel
/// together; every one of them ends up wanting both).
struct Route<'a> {
    net: usize,
    footprint: &'a mut Footprint,
    /// Which electrical segment the repeaters written from here on belong
    /// to. `emit` names it immediately before each call that can place one;
    /// the low-level writers below never choose it, they only report through
    /// it. `None` until the first [`Route::begin`], and for the Tracks pass,
    /// which reports through [`Route::note_track_tap`] instead.
    leg: Option<Leg>,
}

impl Route<'_> {
    fn claim(&mut self, pos: Position) {
        self.footprint.claim(pos, self.net);
    }

    /// Attribute every repeater written from here on to `leg`.
    fn begin(&mut self, leg: Leg) {
        self.leg = Some(leg);
        self.footprint.begin_leg(self.net, leg);
    }

    fn note_repeater(&mut self) {
        let leg = self.leg.expect(
            "every repeater the emitter lays belongs to a named leg, so `Route::begin` \
             must precede the write that places one -- see `Leg`",
        );
        self.footprint.note_repeater(self.net, leg);
    }

    fn note_track_tap(&mut self, slot: usize, tap_x: i32, count: u64) {
        self.footprint.note_track_tap(self.net, slot, tap_x, count);
    }

    fn terminal(
        &mut self,
        gate: &Gate,
        input_index: usize,
        pos: Position,
        kind: RouteTerminalKind,
    ) {
        self.footprint
            .terminal(self.net, gate, input_index, pos, kind);
    }
}

/// Seal `pos`'s keep-out cells -- the neighbours that `dust_reach` says would
/// join its net if they held dust -- wherever the completed reservation shows
/// a *different* net will actually occupy one.
///
/// The other two cases are both left alone, and for the same underlying
/// reason: this router always writes unconditionally, so a seal placed here
/// now would simply be overwritten by whatever legitimately comes later --
/// there is no case where materialising stone changes the outcome, only ones
/// where it would either be redundant or futile:
///
/// - **Unclaimed** (`reservation` has no entry): nothing in the whole circuit
///   ever touches this cell, so it stays air either way and an unreachable
///   cell can never join anything. This is the case that made the old,
///   undirected `seal_cross_talk` expensive -- it sealed every such cell on
///   every ramp step, unconditionally, and every one of those blocks turns
///   out to have been unnecessary (see this module's own doc comment on
///   `compile`'s size, or the spacing-model spec's "Success" section).
/// - **This net's own cell**: whatever gets written here later is exactly
///   the connection this cell is supposed to carry.
///
/// Only the remaining case -- reserved for a *different* net -- is an actual
/// keep-out violation, and even then sealing it cannot prevent the
/// connection (the other net's write overwrites the seal regardless). It is
/// sealed anyway, as a physically visible marker of a threat that
/// `verify_connectivity` is what actually catches, loudly, once the world is
/// finished -- this is defence in depth, not the mechanism the invariant
/// depends on. In practice this case has never fired on any of this
/// project's reference circuits: their existing column/track clearances
/// already keep every net's footprint clear of every other's.
///
/// Only ever called while enforcing (`Footprint::recording == false`) --
/// during the recording pass the reservation is necessarily incomplete, so a
/// sealing decision made from it could not be trusted.
fn seal_cross_talk(world: &mut World, pos: Position, direction: Facing, route: &Route) {
    debug_assert!(
        !route.footprint.recording,
        "sealing must only happen once the reservation is complete"
    );
    for side in side_directions(direction) {
        for candidate in dust_reach(world, pos, side).iter() {
            if world.get(candidate.x, candidate.y, candidate.z).kind != BlockKind::Air {
                continue;
            }
            match route.footprint.reservation.get(&candidate) {
                // Nobody will ever occupy this cell -- leaving it air costs
                // nothing, because nothing can ever turn it into a live
                // connection either. Sealing it would only spend a block to
                // guard against a write that will never happen.
                None => {}
                // This net's own material: whatever comes later here is
                // exactly the connection this cell is supposed to carry.
                Some(&owner) if owner == route.net => {}
                // A different net will legitimately place its own conductor
                // here. Sealing cannot stop that connection -- this router
                // always writes unconditionally, so any later write simply
                // overwrites the seal -- but it is a certain, not merely
                // possible, cross-talk threat, so it is worth making
                // physically obvious rather than silently trusting the later
                // write to happen to land exactly here. `verify_connectivity`
                // is what actually catches this, loudly, once the world is
                // finished; this is defence in depth, not the mechanism the
                // invariant depends on.
                Some(_) => {
                    world.set(candidate.x, candidate.y, candidate.z, stone());
                }
            }
        }
    }
}

/// Move a signal from `entry` (already lit, at whatever strength the caller
/// arranged for) along `direction` to height `target_y`, one dust staircase
/// step per Y level, and return the position of the final landing dust.
///
/// Climbing and descending are *not* mirror images of each other: they are
/// governed by two different, deliberately asymmetric halves of
/// `redstone::simulator::connectivity::dust_connections` (see that module's
/// doc comment for why).
///
/// - **Climb** requires the horizontal neighbour to be a conductor (the
///   "step" the next dust sits on) and requires the current cell's own space
///   above to stay open. So ascending places a solid riser one block ahead
///   at the current height, then lands new dust directly on top of it, one
///   level higher; `current`'s own space above is simply never written, so
///   it stays open air as the rule requires.
/// - **Descend** requires the opposite: the horizontal neighbour must stay
///   open, and the wire it connects to sits one level *below* that
///   neighbour. So descending lands new dust one block ahead and one level
///   down, with its own floor for support; the cell directly above that
///   landing (at the current cell's own height) is never written, so it
///   stays open.
///
/// Either direction spends exactly one signal strength per level walked
/// (`band_ramp_length`'s comment derives why) -- the caller is responsible
/// for having reserved that strength in whatever run feeds this ramp
/// (`MAX_DUST_RUN`'s comment) for the first `RAMP_REST_INTERVAL` levels.
/// Past that, this places its own mandatory rest-stop repeaters (see
/// `RAMP_REST_INTERVAL`'s doc comment for why a climbing step cannot simply
/// become one itself) at fixed, band-index-determined positions, so a climb
/// or descent of any length this router ever asks for survives regardless
/// of the real strength it happens to arrive with.
fn move_between_layers(
    world: &mut World,
    entry: Position,
    direction: Facing,
    target_y: i32,
    route: &mut Route,
) -> Position {
    let levels = (target_y - entry.y).abs();
    let climbing = target_y >= entry.y;
    let mut current = entry;
    for level in 0..levels {
        if level > 0 && level % RAMP_REST_INTERVAL == 0 {
            // Mandatory rest stop: two extra flat cells, not a relabelling
            // of the next climbing/descending step -- see
            // `band_ramp_length`'s doc comment for why a repeater cannot
            // stand in for one of those directly. A repeater only ever
            // powers the block directly in front of it (same Y); a
            // descending step's very next cell is deliberately left open
            // air instead (the diagonal wire-to-wire rule needs it that
            // way), so a repeater there would have nothing to charge. The
            // repeater's own output has to land on an ordinary flat dust
            // cell first -- the diagonal step then proceeds from *that*
            // cell exactly as it would from any other cell mid-run.
            let rest = current.offset(direction);
            ensure_floor(world, rest);
            route.claim(rest.down());
            world.set(rest.x, rest.y, rest.z, repeater(direction));
            route.note_repeater();
            route.claim(rest);
            if !route.footprint.recording {
                seal_cross_talk(world, rest, direction, route);
            }
            let rest_output = rest.offset(direction);
            ensure_floor(world, rest_output);
            route.claim(rest_output.down());
            world.set(rest_output.x, rest_output.y, rest_output.z, dust());
            route.claim(rest_output);
            if !route.footprint.recording {
                seal_cross_talk(world, rest_output, direction, route);
            }
            current = rest_output;
        }
        if climbing {
            let riser = current.offset(direction);
            world.set(riser.x, riser.y, riser.z, stone());
            route.claim(riser);
            let landing = riser.up();
            world.set(landing.x, landing.y, landing.z, dust());
            route.claim(landing);
            if !route.footprint.recording {
                seal_cross_talk(world, landing, direction, route);
            }
            current = landing;
        } else {
            let stepped = current.offset(direction);
            let landing = Position::new(stepped.x, stepped.y - 1, stepped.z);
            ensure_floor(world, landing);
            route.claim(landing.down());
            world.set(landing.x, landing.y, landing.z, dust());
            route.claim(landing);
            if !route.footprint.recording {
                seal_cross_talk(world, landing, direction, route);
            }
            current = landing;
        }
    }
    current
}

/// Same decision as `plan_straight_run`, but for one direction of a track: a
/// run that must also never land a repeater on one of `taps`' cells -- a
/// repeater only reads what is directly behind it, so a wire that turns on
/// top of one is not connected at all. When the budget would force a
/// repeater onto a tap, it goes on the last non-tap cell before it instead;
/// the run after it is then shorter than the budget, never longer. Pure,
/// like `plan_straight_run`, so it can be replayed during strength planning
/// without a `World`.
fn plan_track_run(
    cells: &[i32],
    incoming_strength: u8,
    reserve: i32,
    taps: &BTreeSet<i32>,
) -> (Vec<bool>, Vec<u8>) {
    debug_assert!(
        incoming_strength > 0,
        "a run cannot start from an already-dead signal"
    );
    let threshold = (MAX_DUST_RUN - reserve) as i64;

    // Pick the repeater cells before writing anything: where one repeater
    // ends up decides how much budget the cells after it have.
    let mut is_repeater = vec![false; cells.len()];
    let mut last_refresh: i64 = incoming_strength as i64 - (MAX_SIGNAL_STRENGTH as i64 + 1);
    let mut i = 0usize;
    while i < cells.len() {
        if (i as i64) - last_refresh <= threshold {
            i += 1;
            continue;
        }
        let mut j = i;
        while (j as i64) > last_refresh + 1 && taps.contains(&cells[j]) {
            j -= 1;
        }
        debug_assert!(
            !taps.contains(&cells[j]),
            "taps must never be dense enough to leave no room for a repeater"
        );
        is_repeater[j] = true;
        last_refresh = j as i64;
        i = j + 1;
    }

    let mut strengths = vec![0u8; cells.len()];
    let mut strength = incoming_strength;
    for (k, &placed_repeater) in is_repeater.iter().enumerate() {
        if placed_repeater {
            strength = MAX_SIGNAL_STRENGTH;
        } else {
            strength -= 1;
        }
        strengths[k] = strength;
    }
    (is_repeater, strengths)
}

/// The strength a track would deliver at each of `taps`, if laid with
/// `incoming_strength` arriving at `source_x` -- without touching a `World`.
///
/// This exists for the "Strength planning" pass in `compile`: a descending
/// ramp needs to know what the track hands it before it can tell the column
/// after it what it is starting from, but the track itself is not actually
/// written until the later Tracks pass (world-building order needs Ramps,
/// then Columns, then Tracks -- see the comment where `compile` calls them).
/// `lay_track` runs this identical pure decision again when it actually
/// writes the track, so the two can never silently disagree.
fn track_exit_strengths(
    source_x: i32,
    min_x: i32,
    max_x: i32,
    taps: &BTreeSet<i32>,
    incoming_strength: u8,
    reserve: i32,
) -> BTreeMap<i32, u8> {
    let mut exit_strength = BTreeMap::new();
    for (end, step) in [(min_x, -1i32), (max_x, 1i32)] {
        let length = (end - source_x) * step;
        if length <= 0 {
            continue;
        }
        let cells: Vec<i32> = (1..=length).map(|k| source_x + k * step).collect();
        let (_is_repeater, strengths) = plan_track_run(&cells, incoming_strength, reserve, taps);
        for (k, &x) in cells.iter().enumerate() {
            if taps.contains(&x) {
                exit_strength.insert(x, strengths[k]);
            }
        }
    }
    exit_strength
}

/// Lay one east-west track on band `band`'s own Y (`band_y`): dust from
/// `source_x` out to `min_x` and to `max_x`, with a repeater inserted before
/// the signal can run out (see `plan_track_run`). Returns the strength
/// delivered at each of `taps`, exactly as `track_exit_strengths` already
/// predicted during planning.
///
/// `taps` are the X positions where a ramp joins or leaves the track; every
/// one of them reserves `ramp_reserve(band)` strength for the ramp it feeds
/// (see `MAX_DUST_RUN`'s comment) -- applied to the whole track rather than
/// just at each tap, which is simpler and only ever costs an occasional
/// early repeater on a run that was already close to the 14-cell limit,
/// never a correctness problem. Only the *reserve* -- the first leg up to
/// the ramp's own first mandatory rest stop -- matters here, not the whole
/// `band_ramp_length(band)`: once a rest stop refreshes the signal, nothing
/// this track did beforehand affects it any more.
///
/// `band`, `z` and `source_x` travel together as `origin` purely to keep
/// this under `clippy::too_many_arguments` -- every one of them describes
/// where this one track physically is, not an independent knob. `band` here
/// is already the *effective* band (`effective_band`'s result) -- the
/// caller resolves whether this channel is layered or reverted before this
/// function ever sees it, since `z` (already resolved from the raw track
/// index) is the only thing that still needs to know the difference.
fn lay_track(
    world: &mut World,
    slot: usize,
    origin: (usize, i32, i32),
    span: (i32, i32),
    taps: &BTreeSet<i32>,
    incoming_strength: u8,
    route: &mut Route,
) -> BTreeMap<i32, u8> {
    let (band, z, source_x) = origin;
    let y = band_y(band);
    let reserve = ramp_reserve(band);
    let (min_x, max_x) = span;
    let mut exit_strength = BTreeMap::new();
    for (end, step) in [(min_x, -1i32), (max_x, 1i32)] {
        let length = (end - source_x) * step;
        if length <= 0 {
            continue;
        }
        let direction = if step > 0 { Facing::East } else { Facing::West };
        let cells: Vec<i32> = (1..=length).map(|k| source_x + k * step).collect();
        let (is_repeater, strengths) = plan_track_run(&cells, incoming_strength, reserve, taps);

        // One track feeds every tap on it, and each one leaves by a
        // different prefix of this run -- so this reports a *cumulative*
        // count at each tap rather than incrementing one leg (see `Leg`).
        // The two directions are two independent runs out of the same entry
        // column, so the count restarts with each of them.
        let mut laid = 0u64;
        for (k, &x) in cells.iter().enumerate() {
            let pos = Position::new(x, y, z);
            ensure_floor(world, pos);
            route.claim(pos.down());
            if is_repeater[k] {
                world.set(pos.x, pos.y, pos.z, repeater(direction));
                laid += 1;
            } else {
                world.set(pos.x, pos.y, pos.z, dust());
            }
            route.claim(pos);
            if taps.contains(&x) {
                exit_strength.insert(x, strengths[k]);
                route.note_track_tap(slot, x, laid);
            }
        }
    }
    exit_strength
}

/// 把一個外部輸入的拉桿與它的起始紅石粉畫進世界，回傳這個訊號的來源
/// （拉桿本身的座標，以及它驅動的第一格紅石粉）。
///
/// A lever is an active component, so it drives the dust next to it directly
/// (`power_emitted_by` reports `drives_dust` for a lever in every direction) --
/// no support block in between, unlike a gate's input socket.
///
/// The pin goes on the side a cell of this `facing` sends its output out of:
/// that is the way the route has to leave anyway, and it keeps the route from
/// turning back into the lever and overwriting it with dust. For the north
/// every caller passes today that is the lever's north side, which is where
/// the routes go -- the levers live in row 0, south of every gate row, and
/// signal flow is northwards.
pub(crate) fn place_primary_input(
    world: &mut World,
    home: Position,
    facing: geometry::CellFacing,
) -> (Position, Position) {
    world.set(home.x, home.y, home.z, lever(false));
    ensure_floor(world, home);

    let pin = home.offset(geometry::output_direction(facing));
    ensure_floor(world, pin);
    world.set(pin.x, pin.y, pin.z, dust());

    (home, pin)
}

/// Where a socket's approach column has to run.
///
/// The final repeater of a route must face the gate's support block, and a
/// repeater only reads from directly behind it, so each socket can only be
/// entered from one side: the west socket from the west, the east socket from
/// the east, and the south socket from the south.
///
/// South is special: its socket sits one cell *south* of the support --
/// exactly the direction a routing column already travels along (signal flow
/// is northwards) -- so a plain north-running column can terminate directly
/// on it with the right repeater orientation, no jog required. West and east
/// sockets sit to the *side* of a column that only ever runs north-south, so
/// entering either one needs a genuine east-west leg first: `emit`'s Columns
/// pass brings the signal down at `centre_x ± ENTRY_OFFSET` and jogs the
/// final `ENTRY_OFFSET - 1` cells sideways onto the socket itself (see
/// `ENTRY_OFFSET`'s own doc comment for why that distance, not a fixed cell
/// width, is what decides how far out this lands).
///
/// The old cell-based design paid for this same jog with a mandatory,
/// unconditional refresh repeater right before the corner
/// (`lay_segment_to_corner`), on top of the socket's own mandatory repeater
/// -- two guaranteed repeaters on every west/east edge, the largest single
/// share of `docs/superpowers/specs/2026-08-08-3d-codesign.md`'s measured
/// "gate-entry" cost. That forced refresh was never required by the jog
/// itself, only by treating "entering a gate" as a different kind of routing
/// problem from "entering anywhere else". `emit`'s Columns pass now lays this
/// whole leg -- landing, optional jog, socket -- with `lay_bent_path`, the
/// same general bent-path primitive `compute_bypass`'s direct routes already
/// used, sharing one strength budget end to end and forcing only the one
/// repeater every route needs at its very end.
fn approach_column(centre_x: i32, input_index: usize) -> i32 {
    match input_index {
        0 => centre_x - ENTRY_OFFSET,
        1 => centre_x + ENTRY_OFFSET,
        _ => centre_x,
    }
}

/// Where a net's signal comes from.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Source {
    Lever(usize),
    Gate(usize),
}

/// How a net leaves one channel.
#[derive(Debug, Clone, Copy)]
enum Exit {
    /// Down into one input socket of a gate in the row north of this channel.
    Socket {
        x: i32,
        gate: usize,
        input_index: usize,
    },
    /// Straight on northwards, to rejoin a track in a later channel.
    Feedthrough { x: i32, next_slot: usize },
}

impl Exit {
    fn x(self) -> i32 {
        match self {
            Exit::Socket { x, .. } | Exit::Feedthrough { x, .. } => x,
        }
    }
}

/// One signal, and every channel it has to appear in.
///
/// `channels`, `tracks` and `sinks` are parallel: entry `i` describes the
/// net's presence in channel `channels[i]`. `hops[i]` is the feed-through
/// column that carries it from `channels[i]` to `channels[i + 1]`.
#[derive(Debug, Clone)]
pub(crate) struct Net {
    source: Source,
    source_column: i32,
    channels: Vec<usize>,
    tracks: Vec<usize>,
    sinks: Vec<Vec<(usize, usize)>>,
    hops: Vec<i32>,
}

impl Net {
    /// A net carrying only what the four invariants actually read: its source
    /// and its sinks.
    ///
    /// `source_column`, `channels`, `tracks` and `hops` are the row/channel
    /// router's own scratch, and no verifier touches them -- leaving them
    /// empty is what makes a candidate's verification independent of the
    /// legacy floorplan that invented them.
    pub(crate) fn for_verification(source: Source, sinks: Vec<(usize, usize)>) -> Self {
        Net {
            source,
            source_column: 0,
            channels: Vec::new(),
            tracks: Vec::new(),
            sinks: vec![sinks],
            hops: Vec::new(),
        }
    }

    fn entry_column(&self, slot: usize) -> i32 {
        if slot == 0 {
            self.source_column
        } else {
            self.hops[slot - 1]
        }
    }

    fn exits(&self, slot: usize, centre_x: &[i32]) -> Vec<Exit> {
        let mut exits: Vec<Exit> = self.sinks[slot]
            .iter()
            .map(|&(gate, input_index)| Exit::Socket {
                x: approach_column(centre_x[gate], input_index),
                gate,
                input_index,
            })
            .collect();
        if slot + 1 < self.channels.len() {
            exits.push(Exit::Feedthrough {
                x: self.hops[slot],
                next_slot: slot + 1,
            });
        }
        exits
    }

    /// The X range this net's track has to span inside channel `slot`.
    fn span(&self, slot: usize, centre_x: &[i32]) -> (i32, i32) {
        let mut lo = self.entry_column(slot);
        let mut hi = lo;
        for exit in self.exits(slot, centre_x) {
            lo = lo.min(exit.x());
            hi = hi.max(exit.x());
        }
        (lo, hi)
    }
}

/// The netlist after levelisation, row ordering and X placement.
struct Floorplan {
    /// Row of each gate. Row 0 holds the levers, so a gate of level `l` is in
    /// row `l + 1`.
    row_of: Vec<usize>,
    /// Gate indices per row, ordered west to east. `rows[0]` is always empty.
    rows: Vec<Vec<usize>>,
    /// Centre X of each gate.
    centre_x: Vec<i32>,
    /// X of each primary input's lever.
    lever_x: Vec<i32>,
}

/// ASAP level of every gate: one row deeper than its deepest predecessor.
/// `order` is topological, so one forward pass is enough.
///
/// # Why not ALAP, or something that uses the slack in between
///
/// ASAP puts a gate as early as its inputs allow with no regard for where
/// its *consumer* sits -- which is exactly what stretches a single-fanout
/// gate like `and4`'s `g5` (`¬d`) across the whole netlist depth even
/// though its only consumer is the very last gate (see this module's
/// top-of-file docs for the full trace). That looked like a real bug, so it
/// was measured, not assumed: two alternatives were implemented and run
/// through the exact same four reference circuits and the same routing
/// pipeline below (ASAP itself unchanged either way -- only which level a
/// gate gets handed to `build_floorplan` differs).
///
/// - **Full ALAP**: every gate sits one row above the *earliest* of its
///   consumers' own ALAP levels; a gate with no consumer is pinned to the
///   deepest level the netlist needs.
/// - **Consumer-pulled**: a gate with *exactly one* consumer is pulled all
///   the way to its ALAP level (right next to that consumer); every gate
///   with zero or more-than-one consumer stays at ASAP. The reasoning
///   going in: a single-fanout gate has nothing to lose by moving next to
///   its one consumer, while a multi-fanout gate would only shorten one
///   consumer's edge at the expense of lengthening the others (the sum of
///   a multi-fanout gate's edge lengths does not depend on where it sits;
///   only a single-fanout edge passes a level shift through unchanged).
///
/// Both looked plausible on paper. Neither won. Numbers (release build,
/// `cargo run --bin build_circuit` for box/blocks, `cargo test --test
/// reference_circuits`/`seven_segment -- --nocapture` for settle,
/// `cargo run --bin routing_cost_report` for bypass count):
///
/// ```text
/// non-air blocks (bounding box unchanged in Y/Z shape terms, only listing the number that matters):
///   circuit         ASAP    ALAP    consumer-pulled
///   and4             571     571     571   (same gate count, ¬d's cost just moves to whichever side is now the long edge)
///   full_adder      2246    2872    3388   (+27.9%, +50.9%)
///   segment_a       6716    8122    8110   (+21.0%, +20.7%)
///   seven_segment  16694   16968   16694   (+1.6%, +0.0%)
///
/// blocks/gate:
///   and4            81.6    81.6    81.6
///   full_adder     102.1   130.5   154.0
///   segment_a      146.0   176.6   176.3
///   seven_segment  198.7   202.0   198.7
///
/// worst-case settle (game ticks):
///   and4              30      30      30
///   full_adder        82      86      86
///   segment_a         94     100      98
///   seven_segment    124     116     124
///
/// bypass edges (direct GATE_Y route, no ramp/track) out of all routed edges:
///   and4             6/10    5/10    5/10
///   full_adder       6/32    6/32    6/32
///   segment_a       15/83   10/83   12/83
///   seven_segment  34/156  30/156  34/156
/// ```
///
/// ASAP wins or ties on every circuit's block count, blocks/gate, and
/// bypass count. Its only loss anywhere is `seven_segment`'s settle time,
/// where full ALAP is 8 ticks faster (116 vs. 124) -- but full ALAP is also
/// 1.6% larger there and meaningfully larger everywhere else, so that one
/// win does not generalise into a reason to switch.
///
/// The `g5`-shaped fix does not actually shrink anything: moving a
/// single-fanin/single-fanout gate within its slack window does not
/// eliminate its long edge, it relocates it -- `g5`'s incoming edge (from a
/// lever, fixed at row 0) and outgoing edge (to `g6`, fixed at the deepest
/// row) between them always span the netlist's full depth minus one hop,
/// no matter which row `g5` itself occupies; ASAP puts the long hop on the
/// output side, ALAP/consumer-pulled put it on the input side, and `and4`'s
/// identical block count under all three (571) confirms neither is
/// cheaper. Applied netlist-wide, the *only* effect visible in the larger
/// circuits is the downside the module's own docs warned about: pushing
/// gates later crowds the deepest rows (they must now fit more gates,
/// widening the row and the channel under it) while several early-to-middle
/// levels empty out without the channel machinery ever letting an empty
/// level cost zero (`assign_tracks`/`layout_z` always reserve at least one
/// track's depth per channel) -- pure overhead, no shorter critical path to
/// show for it.
///
/// Kept: ASAP, unmodified.
fn compute_asap_levels(
    netlist: &Netlist,
    order: &[usize],
    producer_of: &HashMap<&str, usize>,
) -> Vec<usize> {
    let mut level = vec![0usize; netlist.gates.len()];
    for &g in order {
        let mut deepest = 0usize;
        for input in &netlist.gates[g].inputs {
            if let Some(&p) = producer_of.get(input.as_str()) {
                deepest = deepest.max(level[p] + 1);
            }
        }
        level[g] = deepest;
    }
    level
}

/// Levelise the DAG, order each row by barycentre, and give every gate an X.
fn build_floorplan(
    netlist: &Netlist,
    order: &[usize],
    producer_of: &HashMap<&str, usize>,
) -> Floorplan {
    let gate_count = netlist.gates.len();

    let level = compute_asap_levels(netlist, order, producer_of);
    let level_count = level.iter().copied().max().map_or(0, |m| m + 1);
    let row_count = level_count + 1;

    let mut rows: Vec<Vec<usize>> = vec![Vec::new(); row_count];
    for &g in order {
        rows[level[g] + 1].push(g);
    }

    let mut consumers: Vec<Vec<usize>> = vec![Vec::new(); gate_count];
    for (g, gate) in netlist.gates.iter().enumerate() {
        for input in &gate.inputs {
            if let Some(&p) = producer_of.get(input.as_str()) {
                consumers[p].push(g);
            }
        }
    }

    let input_slot: HashMap<&str, usize> = netlist
        .inputs
        .iter()
        .enumerate()
        .map(|(i, name)| (name.as_str(), i))
        .collect();

    let row_len: Vec<usize> = rows.iter().map(Vec::len).collect();
    let widest = row_len
        .iter()
        .copied()
        .max()
        .unwrap_or(0)
        .max(netlist.inputs.len())
        .max(1);

    let mut slot = vec![0usize; gate_count];
    for row in &rows {
        for (i, &g) in row.iter().enumerate() {
            slot[g] = i;
        }
    }

    // Barycentre ordering. Sweeping down puts each gate near the average
    // position of what feeds it; sweeping back up puts it near the average
    // position of what it feeds. Both reduce crossings, and fewer crossings
    // mean narrower net spans -- which is exactly what lets the left-edge
    // track assignment below pack more nets onto one track.
    let spread = |position: usize, len: usize| -> f64 {
        (position as f64 + 0.5) * (widest as f64 / len.max(1) as f64)
    };
    let lever_spread = |name: &str| -> Option<f64> {
        input_slot
            .get(name)
            .map(|&i| spread(i, netlist.inputs.len()))
    };

    for _ in 0..3 {
        for r in 1..row_count {
            let mut keyed: Vec<(f64, usize, usize)> = rows[r]
                .iter()
                .enumerate()
                .map(|(i, &g)| {
                    let mut sum = 0.0;
                    let mut count = 0.0;
                    for input in &netlist.gates[g].inputs {
                        if let Some(&p) = producer_of.get(input.as_str()) {
                            sum += spread(slot[p], row_len[level[p] + 1]);
                            count += 1.0;
                        } else if let Some(s) = lever_spread(input) {
                            sum += s;
                            count += 1.0;
                        }
                    }
                    let key = if count > 0.0 {
                        sum / count
                    } else {
                        spread(i, row_len[r])
                    };
                    (key, i, g)
                })
                .collect();
            keyed.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
            rows[r] = keyed.iter().map(|entry| entry.2).collect();
            for (i, &g) in rows[r].iter().enumerate() {
                slot[g] = i;
            }
        }
        for r in (1..row_count).rev() {
            let mut keyed: Vec<(f64, usize, usize)> = rows[r]
                .iter()
                .enumerate()
                .map(|(i, &g)| {
                    let mut sum = 0.0;
                    let mut count = 0.0;
                    for &c in &consumers[g] {
                        sum += spread(slot[c], row_len[level[c] + 1]);
                        count += 1.0;
                    }
                    let key = if count > 0.0 {
                        sum / count
                    } else {
                        spread(i, row_len[r])
                    };
                    (key, i, g)
                })
                .collect();
            keyed.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
            rows[r] = keyed.iter().map(|entry| entry.2).collect();
            for (i, &g) in rows[r].iter().enumerate() {
                slot[g] = i;
            }
        }
    }

    // X placement. Rows alternate a `COLUMN_CLEARANCE` shift so that a row's
    // output columns (at its own `cx`) can never land on the next row's socket
    // approach columns (at `cx' - 4`, `cx'` and `cx' + 4`): with the shift,
    // every pair of columns meeting in one channel is at least
    // `COLUMN_CLEARANCE` apart, which is what keeps two unrelated signals from
    // running side by side.
    let mut centre_x = vec![0i32; gate_count];
    for (r, row) in rows.iter().enumerate() {
        let shift = if r % 2 == 0 { 0 } else { COLUMN_CLEARANCE };
        let left = ((widest - row.len()) / 2) as i32 * SLOT_PITCH;
        for (i, &g) in row.iter().enumerate() {
            centre_x[g] = ORIGIN_X + left + i as i32 * SLOT_PITCH + ENTRY_OFFSET + shift;
        }
    }
    let lever_left = ((widest - netlist.inputs.len()) / 2) as i32 * SLOT_PITCH;
    let lever_x: Vec<i32> = (0..netlist.inputs.len())
        .map(|i| ORIGIN_X + lever_left + i as i32 * SLOT_PITCH + ENTRY_OFFSET)
        .collect();

    let row_of: Vec<usize> = level.iter().map(|&l| l + 1).collect();

    Floorplan {
        row_of,
        rows,
        centre_x,
        lever_x,
    }
}

/// Collect the nets: one per driven signal that actually has a sink.
fn build_nets(
    netlist: &Netlist,
    order: &[usize],
    plan: &Floorplan,
    producer_of: &HashMap<&str, usize>,
) -> Vec<Net> {
    let input_slot: HashMap<&str, usize> = netlist
        .inputs
        .iter()
        .enumerate()
        .map(|(i, name)| (name.as_str(), i))
        .collect();

    // Signal name -> index into `nets`, built in a fixed order so that two
    // compiles of the same netlist produce the same world.
    let mut index_of: HashMap<String, usize> = HashMap::new();
    let mut nets: Vec<Net> = Vec::new();
    let mut sinks_of: Vec<Vec<(usize, usize)>> = Vec::new();

    for &g in order {
        for (input_index, input) in netlist.gates[g].inputs.iter().enumerate() {
            let net = match index_of.get(input.as_str()) {
                Some(&i) => i,
                None => {
                    let (source, column) = if let Some(&i) = input_slot.get(input.as_str()) {
                        (Source::Lever(i), plan.lever_x[i])
                    } else {
                        let driver = *producer_of.get(input.as_str()).expect(
                            "every input was checked to be driven before placement started",
                        );
                        (Source::Gate(driver), plan.centre_x[driver])
                    };
                    let i = nets.len();
                    nets.push(Net {
                        source,
                        source_column: column,
                        channels: Vec::new(),
                        tracks: Vec::new(),
                        sinks: Vec::new(),
                        hops: Vec::new(),
                    });
                    sinks_of.push(Vec::new());
                    index_of.insert(input.clone(), i);
                    i
                }
            };
            sinks_of[net].push((g, input_index));
        }
    }

    // Turn each net's sink list into the per-channel structure. Channel `c`
    // lies between row `c` and row `c + 1`, so a sink in row `t` is fed from
    // channel `t - 1`, and the net's own source row is always a channel too:
    // when the nearest sink is further away, that first channel is where the
    // feed-through starts.
    for (i, net) in nets.iter_mut().enumerate() {
        let source_row = match net.source {
            Source::Lever(_) => 0,
            Source::Gate(g) => plan.row_of[g],
        };
        let mut by_channel: BTreeMap<usize, Vec<(usize, usize)>> = BTreeMap::new();
        by_channel.entry(source_row).or_default();
        for &(gate, input_index) in &sinks_of[i] {
            by_channel
                .entry(plan.row_of[gate] - 1)
                .or_default()
                .push((gate, input_index));
        }
        for (channel, sinks) in by_channel {
            net.channels.push(channel);
            net.sinks.push(sinks);
        }
        net.tracks = vec![0; net.channels.len()];
    }

    nets.retain(|net| net.sinks.iter().any(|s| !s.is_empty()));
    nets
}

/// Which nets can skip the ramp/track machinery entirely and connect
/// straight across at `GATE_Y` -- see `BYPASS_MAX_DISTANCE` for why a small
/// X gap makes that provably safe.
///
/// Deliberately restricted to nets with exactly one channel (no
/// feed-through: a bypass never has to survive crossing into a second
/// channel, so it never has to reason about a hop column it does not fully
/// control) and exactly one sink (fan-out would need more than one jog
/// leaving the same trunk cell; two independently-planned corners could
/// disagree about a shared cell's repeater placement, which is exactly the
/// kind of bug this module's whole `Footprint`/`verify_connectivity` apparatus
/// exists to catch, not something worth risking for a case this router does
/// not need to handle yet). Every other net keeps routing exactly as before.
///
/// Pure function of the floorplan and each net's channel/sink structure --
/// like `reserve_columns` and `assign_tracks`, no world access, so it can run
/// before either of them and its answer feeds straight into `assign_tracks`
/// (a bypassed net needs no track at all).
///
/// This is the proof-only half of bypass eligibility. `resolve_bypass_and_
/// geometry` widens the *distance* this reaches beyond `BYPASS_MAX_DISTANCE`
/// by asking an actual `Reservation` instead of proving one, but keeps this
/// function's single-channel/single-sink restriction exactly as is for that
/// wider query too -- fan-out and feed-through bypasses are a different, and
/// harder, problem (see `resolve_bypass_and_geometry`'s own doc comment).
fn compute_bypass(nets: &[Net], plan: &Floorplan) -> Vec<bool> {
    nets.iter()
        .map(|net| {
            if net.channels.len() != 1 || net.sinks[0].len() != 1 {
                return false;
            }
            let (gate, input_index) = net.sinks[0][0];
            let exit_x = approach_column(plan.centre_x[gate], input_index);
            (exit_x - net.source_column).abs() <= BYPASS_MAX_DISTANCE
        })
        .collect()
}

/// Reserve one X column for a feed-through, clear of everything it would run
/// past on its way north.
///
/// A feed-through is a plain dust run at `GATE_Y` that passes straight through
/// whole gate rows, so it has to miss both the rows' bodies and every other
/// routing column in every channel it crosses. Candidates are searched
/// outwards from `target` so that a feed-through stays near the rest of its
/// net -- a column parked far away would stretch that net's track and push the
/// channel's track count up.
fn reserve_feedthrough(
    target: i32,
    channels: std::ops::RangeInclusive<usize>,
    rows: std::ops::RangeInclusive<usize>,
    used_columns: &[BTreeSet<i32>],
    row_blocked: &[Vec<(i32, i32)>],
    west_limit: i32,
) -> i32 {
    let channels: Vec<usize> = channels.collect();
    let rows: Vec<usize> = rows.collect();
    let fits = |x: i32| -> bool {
        if rows
            .iter()
            .any(|&r| row_blocked[r].iter().any(|&(lo, hi)| x >= lo && x <= hi))
        {
            return false;
        }
        !channels.iter().any(|&c| {
            used_columns[c]
                .iter()
                .any(|&used| (x - used).abs() < COLUMN_CLEARANCE)
        })
    };

    let centre = target - target.rem_euclid(2);
    for step in 0.. {
        let east = centre + 2 * step;
        if fits(east) {
            return east;
        }
        let west = centre - 2 * step;
        if west >= west_limit && fits(west) {
            return west;
        }
    }
    unreachable!("the search walks east without bound, so it always terminates")
}

/// Every row's keep-out X intervals: a lever's clearance zone (row 0) or a
/// gate's body padded by `COLUMN_CLEARANCE` on both sides (every other row).
/// Nothing may run a column, a feed-through, or (see
/// `resolve_bypass_and_geometry`'s widened bypass jog) a horizontal jog
/// through one of these without risking that row's own hardware.
///
/// Extracted out of `reserve_columns` so `resolve_bypass_and_geometry` can
/// run the identical check against a *candidate* jog before `reserve_columns`
/// itself has any reason to care about one -- a gate or lever body is never a
/// conductor, so it never shows up in a `Reservation` the way another net's
/// dust would, and this is the only place that keep-out is recorded at all.
fn row_body_zones(plan: &Floorplan, row_count: usize) -> Vec<Vec<(i32, i32)>> {
    let mut row_blocked: Vec<Vec<(i32, i32)>> = vec![Vec::new(); row_count];
    for &x in &plan.lever_x {
        row_blocked[0].push((x - COLUMN_CLEARANCE + 1, x + COLUMN_CLEARANCE - 1));
    }
    for (g, &cx) in plan.centre_x.iter().enumerate() {
        row_blocked[plan.row_of[g]].push((
            cx - ENTRY_OFFSET - COLUMN_CLEARANCE + 1,
            cx + ENTRY_OFFSET + COLUMN_CLEARANCE - 1,
        ));
    }
    row_blocked
}

/// Column reservation: fill in every net's `hops` (the feed-through columns
/// that connect consecutive channels it has to appear in). Pure function of
/// the floorplan and each net's channel/sink structure -- no world access,
/// so it is exactly as reusable by routing analysis as it is by `compile`.
///
/// Extracted verbatim from `compile`'s "Column reservation" section; see the
/// comment there (still in place) for why the forced columns come first and
/// feed-throughs are placed last, against everything else.
fn reserve_columns(plan: &Floorplan, nets: &mut [Net], row_count: usize, channel_count: usize) {
    let mut used_columns: Vec<BTreeSet<i32>> = vec![BTreeSet::new(); channel_count.max(1)];
    let row_blocked = row_body_zones(plan, row_count);

    for net in nets.iter() {
        used_columns[net.channels[0]].insert(net.source_column);
        for (slot, &channel) in net.channels.iter().enumerate() {
            for &(gate, input_index) in &net.sinks[slot] {
                used_columns[channel].insert(approach_column(plan.centre_x[gate], input_index));
            }
        }
    }

    for net in nets.iter_mut() {
        for slot in 0..net.channels.len().saturating_sub(1) {
            let from = net.channels[slot];
            let to = net.channels[slot + 1];
            let target = net.entry_column(slot);
            let column = reserve_feedthrough(
                target,
                from..=to,
                (from + 1)..=to,
                &used_columns,
                &row_blocked,
                ORIGIN_X - ENTRY_OFFSET,
            );
            for columns in used_columns[from..=to].iter_mut() {
                columns.insert(column);
            }
            net.hops.push(column);
        }
    }
}

/// Left-edge track assignment: fill in every net's per-slot `tracks` index,
/// and return how many tracks each channel ended up needing.
///
/// A net for which `bypass[n]` holds contributes no member at all -- it
/// connects directly at `GATE_Y` (see `compute_bypass`) and never touches a
/// track, so it must not inflate the channel's track count or claim a track
/// index nobody will ever lay.
///
/// Extracted verbatim from `compile`'s "Left-edge track assignment" section;
/// see the comment there for why one track can carry many nets.
fn assign_tracks(
    plan: &Floorplan,
    nets: &mut [Net],
    channel_count: usize,
    bypass: &[bool],
) -> Vec<usize> {
    let mut track_count = vec![0usize; channel_count];
    for (channel, count) in track_count.iter_mut().enumerate() {
        let mut members: Vec<(i32, i32, usize, usize)> = Vec::new();
        for (n, net) in nets.iter().enumerate() {
            if bypass[n] {
                continue;
            }
            for (slot, &c) in net.channels.iter().enumerate() {
                if c == channel {
                    let (lo, hi) = net.span(slot, &plan.centre_x);
                    members.push((lo, hi, n, slot));
                }
            }
        }
        members.sort_by_key(|&(lo, _, n, slot)| (lo, n, slot));

        let mut track_end: Vec<i32> = Vec::new();
        for (lo, hi, n, slot) in members {
            let track = match track_end
                .iter()
                .position(|&end| lo - end >= TRACK_SHARE_GAP)
            {
                Some(t) => t,
                None => {
                    track_end.push(i32::MIN / 2);
                    track_end.len() - 1
                }
            };
            track_end[track] = hi;
            nets[n].tracks[slot] = track;
        }
        *count = track_end.len();
    }
    track_count
}

/// Z layout: each row's Z and every channel's track Zs, laid out northwards
/// from row 0 and then shifted back into the non-negative range.
///
/// Extracted verbatim from `compile`'s "Z layout" section, generalised for
/// `docs/superpowers/specs/2026-08-08-3d-codesign.md`'s "Layers, not two
/// planes": a channel's `track_count[channel]` tracks used to be Z-separated
/// copies of the one `TRACK_Y` plane (`TRACK_SPACING` apart, purely to avoid
/// one track's descending ramp injecting its signal into its same-Y
/// neighbour -- see the old constant's own doc comment, no longer present).
/// They are now `band_y(0)`, `band_y(1)`, ... of the *same* channel, sharing
/// one Z line (`shared_z` below) and telling each other apart only by Y.
///
/// # Why every band can safely share one Z line
///
/// A net assigned to band `b` enters this channel at `GATE_Y`, `band_ramp_
/// length(b)` cells south of `shared_z`, and climbs one Y level per Z cell
/// (`move_between_layers`) until it lands on `shared_z` at height `band_y(b)`.
/// So at any height `h` between `GATE_Y` and `band_y(b)`, that climb sits at
/// `Z = shared_z + (band_y(b) - h)`.
///
/// Consider two nets, bound for bands `b` and `k` with `b > k`. At the
/// instant the *deeper* one (`b`) passes through the *shallower* one's own
/// band height (`h = band_y(k)`), its Z is `shared_z + band_y(b) - band_y(k)
/// = shared_z + BAND_HEIGHT * (b - k)` -- strictly `BAND_HEIGHT` cells (at
/// least 2) away from `shared_z`, which is exactly where band `k`'s own
/// track lives. It never reaches band `k`'s Z at band `k`'s height, so it
/// never runs alongside band `k`'s track, only past it, at a stone's throw
/// in Z that `dust_reach`'s same-layer (distance-1) rule cannot bridge. The
/// two climbs also never share an X (`reserve_columns` keeps every column in
/// one channel `COLUMN_CLEARANCE` apart, band or no band), so this holds
/// regardless of which two nets happen to be climbing at once.
///
/// This is the "skip-band edge" the spec's Order section calls out as having
/// no equivalent in the old two-plane design -- and the reason it is safe
/// here is structural (a strict Z gap, provable from the climb geometry
/// alone), not a hopeful placement choice; `verify_connectivity` and
/// `verify_torch_merge` still check every compile regardless.
fn layout_z(
    row_count: usize,
    channel_count: usize,
    track_count: &[usize],
) -> (Vec<i32>, Vec<Vec<i32>>) {
    let mut row_z = vec![0i32; row_count];
    let mut track_z: Vec<Vec<i32>> = vec![Vec::new(); channel_count];
    for channel in 0..channel_count {
        let channel_south = row_z[channel] - 3;

        if track_count[channel] > BAND_CAP {
            // Reverted channel (`BAND_CAP`'s doc comment): byte-for-byte the
            // original two-plane geometry, at `band_y(0)` only. Every track
            // gets its own Z, `TRACK_SPACING` apart, exactly as when this
            // was the only scheme this module had.
            track_z[channel] = (0..track_count[channel])
                .map(|t| channel_south - TRACK_SPACING * (t as i32 + 1))
                .collect();
            let depth = TRACK_SPACING * track_count[channel] as i32;
            row_z[channel + 1] = channel_south - depth - band_ramp_length(0) - 4;
            continue;
        }

        // The highest band this channel actually uses -- 0 if it uses none
        // at all (every net bypassed), matching the old code's `max(1)`
        // treatment of a channel with zero real tracks.
        let highest_band = track_count[channel].max(1) - 1;
        let deepest_ramp = band_ramp_length(highest_band);

        // Three blocks clear of the row's own south socket leaves the
        // deepest band's ramp somewhere to start climbing from; shallower
        // bands start their own (shorter) climb later, closer to
        // `shared_z`, using the same south margin the deepest one needed.
        let shared_z = channel_south - deepest_ramp;
        track_z[channel] = vec![shared_z; track_count[channel]];

        // Symmetric on the north side: the deepest band's descent needs the
        // same `deepest_ramp` cells past `shared_z` before the column can
        // reach the next row's south socket approach.
        row_z[channel + 1] = shared_z - deepest_ramp - 4;
    }

    let z_offset = 3 - row_z[row_count - 1] + 2;
    for z in &mut row_z {
        *z += z_offset;
    }
    for channel in &mut track_z {
        for z in channel {
            *z += z_offset;
        }
    }
    (row_z, track_z)
}

/// `(entry_strength[net][slot], exit_strength[net][slot][exit_x])` -- see
/// `plan_strengths`.
type StrengthPlan = (Vec<Vec<u8>>, Vec<Vec<BTreeMap<i32, u8>>>);

/// Work out, for every net and every slot it appears in, the signal strength
/// arriving at that slot's ramp entry (`entry_strength[net][slot]`) and the
/// strength the track hands each of that slot's exits before their
/// descending ramp (`exit_strength[net][slot]`, keyed by exit X).
///
/// This has to run before any of the Ramps/Columns/Tracks passes write a
/// single block. The Columns pass needs to know, for a feed-through, what
/// strength survived the previous slot's track and descending ramp before it
/// can lay the column that continues from there -- but that number depends
/// on the *next* slot's track layout, which is not written until the later
/// Tracks pass (world-building order has to stay Ramps, then Columns, then
/// Tracks -- see the comment above the Ramps loop in `compile` for why).
/// Computing every slot's numbers up front, net by net in slot order (each
/// slot's result depends only on the previous slot's, within the same net),
/// sidesteps that ordering conflict entirely: pure arithmetic, no `World`,
/// so it does not care what order the real blocks go down in.
///
/// `plan_straight_run` and `track_exit_strengths` are the exact same
/// decisions `lay_dust_run` and `lay_track` make when they actually write
/// blocks later, so the numbers this produces are guaranteed to match what
/// ends up in the world, not just a plausible estimate of it.
///
/// `net_source_strength[n]` is what `net` `n`'s own source pin actually
/// carries -- `MAX_SIGNAL_STRENGTH` for a lever or an ordinary NOR gate
/// (both always full strength), but honestly computed for a net sourced by
/// a merge gate (see `compute_net_source_strengths`, which every real
/// caller of this function runs first to build it). Before `Gate::is_merge`
/// existed this was `MAX_SIGNAL_STRENGTH` unconditionally, which is why
/// every net used to start from that literal constant here -- a merge's own
/// junction can carry less, so this now has to be told rather than assume.
#[allow(clippy::too_many_arguments)]
fn plan_strengths(
    nets: &[Net],
    plan: &Floorplan,
    track_z: &[Vec<i32>],
    track_count: &[usize],
    lever_pin: &[Position],
    gate_pin: &[Position],
    bypass: &[bool],
    net_source_strength: &[u8],
) -> StrengthPlan {
    let mut entry_strength: Vec<Vec<u8>> = Vec::with_capacity(nets.len());
    let mut exit_strength: Vec<Vec<BTreeMap<i32, u8>>> = Vec::with_capacity(nets.len());

    for (n, net) in nets.iter().enumerate() {
        let mut net_entry = vec![0u8; net.channels.len()];
        let mut net_exit: Vec<BTreeMap<i32, u8>> = vec![BTreeMap::new(); net.channels.len()];

        // A bypassed net never ramps or touches a track (`emit`'s Columns
        // pass routes it directly instead), and its one channel may not even
        // have a `track_z` entry to read (a channel every one of whose nets
        // bypasses gets zero tracks -- see `assign_tracks`). Leaving these at
        // their zero default is safe precisely because nothing downstream
        // ever reads a bypassed net's entry/exit strength.
        if bypass[n] {
            entry_strength.push(net_entry);
            exit_strength.push(net_exit);
            continue;
        }

        for slot in 0..net.channels.len() {
            let channel = net.channels[slot];
            let band = net.tracks[slot];
            let eff_band = effective_band(track_count, channel, band);
            let reserve = ramp_reserve(eff_band);
            let z = track_z[channel][band];
            let entry = Position::new(
                net.entry_column(slot),
                GATE_Y,
                z + band_ramp_length(eff_band),
            );

            let arriving = if slot == 0 {
                let pin = match net.source {
                    Source::Lever(i) => lever_pin[i],
                    Source::Gate(g) => gate_pin[g],
                };
                let len = straight_run_length(pin, Facing::North, entry.offset(Facing::North));
                plan_straight_run(len, net_source_strength[n], reserve).1
            } else {
                let prev_band = net.tracks[slot - 1];
                let prev_channel = net.channels[slot - 1];
                let eff_prev_band = effective_band(track_count, prev_channel, prev_band);
                let prev_z = track_z[prev_channel][prev_band];
                let feed_x = net.hops[slot - 1];
                let track_strength = net_exit[slot - 1][&feed_x];
                let landing_strength =
                    ramp_ending_strength(band_levels(eff_prev_band), track_strength);
                let landing =
                    Position::new(feed_x, GATE_Y, prev_z - band_ramp_length(eff_prev_band));
                let len = straight_run_length(landing, Facing::North, entry.offset(Facing::North));
                plan_straight_run(len, landing_strength, reserve).1
            };
            net_entry[slot] = arriving;

            let track_incoming = ramp_ending_strength(band_levels(eff_band), arriving);
            let source_x = net.entry_column(slot);
            let (lo, hi) = net.span(slot, &plan.centre_x);
            let mut taps: BTreeSet<i32> = BTreeSet::new();
            taps.insert(source_x);
            for exit in net.exits(slot, &plan.centre_x) {
                taps.insert(exit.x());
            }
            net_exit[slot] = track_exit_strengths(source_x, lo, hi, &taps, track_incoming, reserve);
        }

        entry_strength.push(net_entry);
        exit_strength.push(net_exit);
    }

    (entry_strength, exit_strength)
}

/// The real, decayed strength `net` -- known to start at `source_strength`
/// at its own source pin -- delivers to `target`'s socket `(gate,
/// input_index)`, found in `net`'s own `slot`-th channel, via a **bare**
/// termination (no mandatory final repeater -- see `merge_branch_is_bare`).
///
/// Replicates exactly the same geometry and strength math `emit`'s own
/// Ramps/Columns passes use to lay `net`'s real blocks -- the bypass branch
/// mirrors `emit`'s direct-connection Columns code, the tracked branch
/// re-runs `plan_strengths` against `net` alone (a net's own slot-by-slot
/// computation never reads any *other* net, so slicing to one is exact, not
/// an approximation) to get the same `exit_strength` a batch call would --
/// without writing anything, so `compute_net_source_strengths` can learn
/// this before a single real block for `net`, or the merge gate waiting on
/// the answer, exists.
///
/// Only ever called for a bare-terminated socket: every other socket in
/// this compiler lands via `lay_bent_path`'s own mandatory repeater, which
/// always delivers `MAX_SIGNAL_STRENGTH` regardless of anything computed
/// here (`compute_net_source_strengths` never calls this otherwise).
#[allow(clippy::too_many_arguments)]
fn bare_branch_landing_strength(
    netlist: &Netlist,
    nets: &[Net],
    net: &Net,
    slot: usize,
    source_strength: u8,
    target: (usize, usize),
    plan: &Floorplan,
    row_z: &[i32],
    track_z: &[Vec<i32>],
    track_count: &[usize],
    is_bypass: bool,
    gate_cell: &[NorCell],
    lever_pin: &[Position],
    gate_pin: &[Position],
    facing: geometry::CellFacing,
) -> u8 {
    let (gate, input_index) = target;
    let reserve = bare_reserve_for_merge(netlist, nets, gate);
    let row_z_gate = row_z[plan.row_of[gate]];
    let (dx, dy, dz) = gate_cell[gate].input_offsets[input_index];
    let socket = Position::new(plan.centre_x[gate] + dx, GATE_Y + dy, row_z_gate + dz);
    let exit_x = approach_column(plan.centre_x[gate], input_index);

    if is_bypass {
        // Mirrors `emit`'s own bypass Columns code exactly: at most one bend
        // onto the sink's approach column, then the socket.
        let pin = match net.source {
            Source::Lever(i) => lever_pin[i],
            Source::Gate(g) => gate_pin[g],
        };
        let start = bypass_source_start(netlist, net, pin, exit_x, facing);
        let strength_at_start = if start != pin {
            source_strength.saturating_sub(1)
        } else {
            source_strength
        };

        let mut waypoints: Vec<Position> = Vec::new();
        if start.x != exit_x {
            waypoints.push(Position::new(exit_x, GATE_Y, start.z));
        }
        if socket.x != exit_x {
            waypoints.push(Position::new(exit_x, GATE_Y, row_z_gate));
        }
        waypoints.push(socket);

        let cells = bent_path_cells(start, &waypoints);
        let bend_indices = bare_ending_bend_indices(&cells, &waypoints);
        return plan_bent_path(cells.len(), &bend_indices, strength_at_start, reserve).1;
    }

    // Tracked: replay this one net's own ramp/track chain (slicing `plan_
    // strengths` to just `net` is exact, per this function's own doc
    // comment) up to and including `slot`, then the same short final bend
    // to the socket `emit`'s ordinary Columns pass lays.
    let (_entry, exit) = plan_strengths(
        std::slice::from_ref(net),
        plan,
        track_z,
        track_count,
        lever_pin,
        gate_pin,
        &[false],
        &[source_strength],
    );
    let channel = net.channels[slot];
    let band = net.tracks[slot];
    let eff_band = effective_band(track_count, channel, band);
    let z = track_z[channel][band];
    let track_exit = exit[0][slot][&exit_x];
    let landing_strength = ramp_ending_strength(band_levels(eff_band), track_exit);
    let landing = Position::new(exit_x, GATE_Y, z - band_ramp_length(eff_band));

    let mut waypoints: Vec<Position> = Vec::new();
    if socket.x != landing.x {
        waypoints.push(Position::new(landing.x, GATE_Y, row_z_gate));
    }
    waypoints.push(socket);

    let cells = bent_path_cells(landing, &waypoints);
    let bend_indices = bare_ending_bend_indices(&cells, &waypoints);
    plan_bent_path(cells.len(), &bend_indices, landing_strength, reserve).1
}

/// The real strength each net's own source pin delivers -- what
/// `plan_strengths` and `emit`'s own Ramps/Columns passes need instead of
/// assuming `MAX_SIGNAL_STRENGTH` unconditionally, now that a net can be
/// sourced by a **merge** gate.
///
/// `MAX_SIGNAL_STRENGTH` is still exactly right for a lever or an ordinary
/// NOR gate -- both are driven by an always-full-strength active component
/// (a lever's own power, or a torch's fixed output, independent of how
/// decayed the signal reaching the torch's *support* was). A merge's own
/// junction is different: `place_merge_gate` puts no active component
/// there at all, just the point where its declared inputs' own dust is
/// allowed to touch, so its real strength is whatever its branches deliver.
/// An isolated branch (see `merge_branch_is_bare`) still guarantees
/// `MAX_SIGNAL_STRENGTH` on its own (it ends in a mandatory repeater), so a
/// merge's junction can only carry less than full strength when *every one*
/// of its branches is bare -- a fully private merge, whose own branches may
/// have already decayed by the time they reach the junction.
///
/// Processes gates in `netlist`'s own topological order, so a merge's own
/// junction strength -- which depends on whatever nets feed *its* own
/// sockets -- is always resolved after those nets' own source strengths are
/// already known, however many merges deep a chain goes (an OR of ORs is
/// still just gates in dependency order; nothing here is special-cased for
/// nesting).
#[allow(clippy::too_many_arguments)]
fn compute_net_source_strengths(
    netlist: &Netlist,
    nets: &[Net],
    plan: &Floorplan,
    row_z: &[i32],
    track_z: &[Vec<i32>],
    track_count: &[usize],
    bypass: &[bool],
    lever_pin: &[Position],
    gate_pin: &[Position],
    gate_cell: &[NorCell],
    facing: geometry::CellFacing,
) -> Vec<u8> {
    let mut net_source_strength = vec![MAX_SIGNAL_STRENGTH; nets.len()];

    // Gate -> the net it drives, for gates that drive anything at all (a
    // merge whose own output is a declared circuit output but feeds no
    // *other* gate has none, and needs none: its lamp reads `gate_pin`
    // directly, unaffected by this).
    let mut net_of_gate: HashMap<usize, usize> = HashMap::new();
    for (n, net) in nets.iter().enumerate() {
        if let Source::Gate(g) = net.source {
            net_of_gate.insert(g, n);
        }
    }

    // (gate, input_index) -> which net feeds that socket, and at which of
    // that net's own slots -- every gate input names exactly one net (by
    // construction in `build_nets`), so this is a total, collision-free map.
    let mut feeder: HashMap<(usize, usize), (usize, usize)> = HashMap::new();
    for (n, net) in nets.iter().enumerate() {
        for (slot, sinks) in net.sinks.iter().enumerate() {
            for &(g, idx) in sinks {
                feeder.insert((g, idx), (n, slot));
            }
        }
    }

    let order = netlist
        .topological_order()
        .expect("compile() already rejected a cyclic netlist before emit() runs");
    for &g in &order {
        let gate = &netlist.gates[g];
        if !gate.is_merge() {
            continue; // stays MAX_SIGNAL_STRENGTH -- a torch's output always is.
        }
        // The *minimum* deliverable value across this merge's own branches,
        // not the maximum. `delivered` is "what this one branch supplies
        // when it alone is the live one" -- which branch that actually is
        // depends on the input combination, and this value feeds downstream
        // interior-repeater *placement* decisions (`net_source_strength`)
        // that are made once at compile time, for every combination at
        // once. Placing repeaters as if the least-decayed branch (typically
        // the shortest bare route) were always the one carrying a real
        // signal is an availability bug, not a conservative approximation:
        // for whichever combination actually drives the *most*-decayed
        // branch instead, the real junction value is that branch's own,
        // smaller delivery -- exactly `min`, not `max` -- and a repeater
        // placed on the assumption of the larger value can leave that real,
        // smaller run decaying to zero before it ever reaches one.
        //
        // Confirmed with the real `Simulator`: `and4`'s Yosys-derived
        // netlist chains `g6 = g2 OR g5`, where `g2`'s own branch decays far
        // less than `g5`'s over their real routed distances. Combinations
        // that drive `g5` alone (`g2` dark) reached a real junction value
        // measurably lower than the `max`-based nominal this used to
        // compute, and `g6`'s own downstream interior repeater -- placed
        // against the `max`-optimistic nominal -- sat far enough out that
        // exactly those combinations decayed to zero one hop short of it,
        // corrupting `and4`'s and the decoder's own truth tables on the
        // rows that depend on `g5` alone.
        let mut junction = MAX_SIGNAL_STRENGTH;
        for idx in 0..gate.inputs.len() {
            let &(feeding_net, slot) = feeder
                .get(&(g, idx))
                .expect("every gate input names a real net, which therefore lists it as a sink");
            let net = &nets[feeding_net];
            let delivered = if merge_branch_is_bare(netlist, net, g) {
                // `bare_branch_landing_strength` gives the strength at the
                // *socket* -- one hop away from the junction itself (the
                // same `geometry::input_directions` offset every socket sits
                // at), and that last hop is ordinary dust-to-dust decay like
                // any other, so the junction's own share of it is one less.
                bare_branch_landing_strength(
                    netlist,
                    nets,
                    net,
                    slot,
                    net_source_strength[feeding_net],
                    (g, idx),
                    plan,
                    row_z,
                    track_z,
                    track_count,
                    bypass[feeding_net],
                    gate_cell,
                    lever_pin,
                    gate_pin,
                    facing,
                )
                .saturating_sub(1)
            } else {
                // An isolated branch ends in a repeater sitting *at* the
                // socket (`lay_bent_path`'s own mandatory termination), and
                // a repeater's output is always exactly `MAX_SIGNAL_STRENGTH`
                // one hop on -- unlike dust, that hop costs no decay, so the
                // junction receives the full value, not one less.
                MAX_SIGNAL_STRENGTH
            };
            junction = junction.min(delivered);
        }
        if let Some(&out_net) = net_of_gate.get(&g) {
            // `net_source_strength` means "the strength at this net's own
            // source *pin*" everywhere else (a lever or a torch delivers
            // its full output straight to the adjacent pin cell, no decay
            // on that first hop) -- but a merge's pin is one hop of *plain
            // dust* out from its junction (see `emit`'s own doc comment on
            // why it is not the junction cell itself), and dust always
            // decays by one per hop. So the pin's own strength is the
            // junction's, minus that one hop -- not the junction's value
            // directly, which is what `junction` itself is.
            net_source_strength[out_net] = junction.saturating_sub(1);
        }
    }

    net_source_strength
}

/// What `emit` produces besides the blocks it writes into `world` --
/// everything `CompiledCircuit` needs that is not the world itself.
struct EmitResult {
    input_positions: BTreeMap<String, (i32, i32, i32)>,
    output_positions: BTreeMap<String, (i32, i32, i32)>,
    gate_output_positions: BTreeMap<String, (i32, i32, i32)>,
    primitive_anchors: Vec<Anchor>,
}

/// Everything the placement/routing stages before `emit` computed about
/// where things go, bundled into one value purely so `emit` takes one
/// parameter instead of five (`clippy::too_many_arguments`) -- every field
/// here already travels together from `compile` down to both `emit` calls.
struct RoutingGeometry<'a> {
    plan: &'a Floorplan,
    row_z: &'a [i32],
    nets: &'a [Net],
    track_z: &'a [Vec<i32>],
    /// Tracks per channel -- how `effective_band` (and, before it,
    /// `layout_z`) decides whether a channel is layered or reverted to the
    /// pre-layering, single-band geometry (see `BAND_CAP`'s doc comment).
    track_count: &'a [usize],
    /// Per-net: whether `compute_bypass` found this net's one sink close
    /// enough to connect directly at `GATE_Y` instead of via ramp and track.
    bypass: &'a [bool],
    /// Per routed socket: whether its final cell is dust or a repeater.
    terminals: &'a TerminalKinds,
}

/// Whether a route terminating at `gate`'s `input_index`-th socket should
/// end in a bare dust cell (`lay_bent_path_bare`) rather than the usual
/// mandatory repeater (`lay_bent_path`) -- true exactly when `gate` is a
/// declared merge (`Gate::is_merge`) and `net` -- the whole net landing on
/// that socket, source and every one of its sinks -- drives nothing besides
/// this one merge.
///
/// This is the fanout rule from
/// `docs/superpowers/specs/2026-08-08-gate-types-and-wired-or.md`, "When
/// isolation is actually needed, and how to know", read directly off the
/// netlist: "isolate a branch when its source fans out to anything besides
/// this merge; otherwise a bare join is correct." `net.sinks` is exactly
/// that fan-out list -- every `(gate, input_index)` pair this net's source
/// feeds, across every channel it appears in -- so "fans out to anything
/// besides this merge" is simply "some sink names a gate other than this
/// one". A net that feeds *this same* merge on a second socket (unusual,
/// but not excluded) is still bare: both sinks name `gate` itself, so there
/// is nothing "besides this merge" to protect against -- backflow between
/// the two sockets only circulates this source's own signal back into a
/// branch that was already carrying it, which corrupts nothing.
///
/// A non-merge `gate` always returns `false` here (its own condition on
/// `gate.is_merge()` fails first), which is exactly what keeps every existing
/// NOR socket -- including one whose net happens to fan out to several
/// consumers -- routed exactly as before: this function only ever changes
/// behaviour for a gate that declares itself a merge, and nothing produces
/// one yet outside this task's own tests.
fn merge_branch_is_bare(netlist: &Netlist, net: &Net, gate: usize) -> bool {
    netlist.gates[gate].is_merge() && net.sinks.iter().flatten().all(|&(g, _)| g == gate)
}

/// Write the whole circuit into `world`: every gate, every primary input,
/// every net's ramps/columns/tracks, and every output lamp.
///
/// This is called twice by `compile` against two different worlds and two
/// different `Footprint` modes (see `Footprint`'s own doc comment) -- once
/// to record where everything ends up before a single seal is written for
/// real, once to actually build the circuit enforcing keep-out against that
/// now-complete picture. Both calls run the exact same code, so the two
/// worlds can never disagree about where anything is -- only about whether
/// the orphaned keep-out cells around a ramp landing got sealed.
fn emit(
    world: &mut World,
    netlist: &Netlist,
    geometry: &RoutingGeometry,
    footprint: &mut Footprint,
) -> EmitResult {
    let RoutingGeometry {
        plan,
        row_z,
        nets,
        track_z,
        track_count,
        bypass,
        terminals,
    } = *geometry;
    // The legacy emitter builds every gate north. This binding is where that
    // is *decided* -- every cell placed below, and every face read off one,
    // turns this rather than naming a compass direction of its own -- but it
    // is not the only place that says so, and anyone widening it must widen
    // the rest or the readers will check a turned world against north's faces.
    //
    // Everything that still hardcodes north, and why each one is still a
    // literal. "No facing to read" is a claim about a site's *callers*, so
    // every one below was checked by opening them; where a facing is in fact
    // reachable, the entry says so rather than pretending otherwise:
    //
    //   * `cell_geometry_by_input_count` -- builds its cell cache keyed on
    //     north alone. The key already admits all four; the builder is handed
    //     a `Netlist` and nothing else, so *which* facings to build cells for
    //     is a question only its three call sites can answer
    //     (`resolve_bypass_and_geometry`, and `routing_stats`'s `analyze` and
    //     `distinct_totals_by_part`), and the first of them has no answer.
    //   * `resolve_bypass_and_geometry` -- one binding covering
    //     `source_pin_position`, the `cell_of_count` key and
    //     `bypass_source_start`; source and sink facings collapsed into one,
    //     which only holds while they are equal. It *decides* which nets
    //     bypass, so it runs before the real world is built -- it emits a
    //     scratch probe world and drops it, and an `EmitResult` carries
    //     positions and anchors but no facings.
    //   * `resolve_directed_dust_terminals` and `merge_gate_body_owners` --
    //     local bindings, each needing a facing *per gate* rather than one
    //     (see their own comments for exactly which of their callers could
    //     supply one and which could not).
    //   * `legacy_primitive_nodes` -- the `gate_footprint` call that records
    //     what this emitter built, for the seed the planner then realises.
    //     Also per gate, and travelling beside `primitive_anchors`.
    //   * `routing_stats`: `source_pin`'s lever arm, and four `cell_of_count`
    //     lookups keyed on the *sink* gate. Not for want of facings -- that
    //     module holds the `CompiledCircuit`, and `scan_bypass` now reads its
    //     source gate's facing straight off `gate_facings`. The lever arm has
    //     nothing to read, because facings are recorded per gate and a lever
    //     is not one. The four lookups would have to widen the cache above
    //     first: it only ever has north's key built, so asking it for a turned
    //     sink's geometry indexes a key nobody inserted.
    //   * `compile` -- fills `CompiledCircuit::gate_facings` with
    //     `vec![NORTH; gates]`, and truthfully: it seeds the planner from this
    //     emitter (`seed_from_legacy_parts`), never calls `plan_from_netlist`,
    //     and `from_legacy` zero-fills `variant_indices`, so every gate it
    //     ships really is north until Task 13 switches it over.
    //
    // Three entries left this list in Task 10, which is the task that started
    // choosing facings. Kept named rather than deleted, because "why is this
    // one not here any more" is the question a reader of the paragraph above
    // will ask:
    //
    //   * `compile_planned` -- now reads `candidate.facing_of(g)` per gate,
    //     bound before the candidate is moved into `realise_and_verify`.
    //   * `planner::emit_primitives` -- now binds `candidate.facing_of(index)`
    //     once per node and builds the merge, the NOR, both output pins and
    //     the lever to it.
    //   * `planner::plan_from_netlist`'s `gate_footprint` call -- the
    //     facing is now `snap`'s answer for that node, which exists before the
    //     footprint is recorded because relaxation chose it first.
    //
    // `git grep -n "CellFacing::NORTH" -- src` regenerates most of that list
    // (its remaining hits are test code). Not all of it: the planner asserts
    // north by *omission* rather than by name, in the three
    // `variant_indices = vec![0; anchors.len()]` lines its constructors carry,
    // which no grep for the constant will turn up. Regenerate with both, and
    // keep the result in agreement with this list rather than trusting either
    // alone.
    let facing = geometry::CellFacing::NORTH;
    let mut gate_cell: Vec<NorCell> = Vec::with_capacity(netlist.gates.len());
    let mut primitive_anchors: Vec<Anchor> =
        Vec::with_capacity(netlist.gates.len() + netlist.inputs.len());
    for _ in 0..netlist.gates.len() {
        gate_cell.push(NorCell {
            size: (0, 0, 0),
            input_offsets: Vec::new(),
            output_offset: (0, 0, 0),
        });
    }
    for (g, gate) in netlist.gates.iter().enumerate() {
        let origin = (plan.centre_x[g], GATE_Y, row_z[plan.row_of[g]]);
        primitive_anchors.push(Anchor {
            x: origin.0,
            y: origin.1,
            z: origin.2,
        });
        gate_cell[g] = if gate.is_merge() {
            place_merge_gate(world, origin, gate.inputs.len(), facing)
        } else {
            place_nor_gate(world, origin, gate.inputs.len(), facing)
        };
    }

    let mut input_positions: BTreeMap<String, (i32, i32, i32)> = BTreeMap::new();
    let mut lever_pin: Vec<Position> = Vec::with_capacity(netlist.inputs.len());
    for (i, name) in netlist.inputs.iter().enumerate() {
        let home = Position::new(plan.lever_x[i], GATE_Y, row_z[0]);
        let (lever_pos, pin) = place_primary_input(world, home, facing);
        primitive_anchors.push(Anchor {
            x: lever_pos.x,
            y: lever_pos.y,
            z: lever_pos.z,
        });
        input_positions.insert(name.clone(), (lever_pos.x, lever_pos.y, lever_pos.z));
        lever_pin.push(pin);
    }

    let torch_of = |g: usize, cell: &NorCell| -> Position {
        Position::new(
            plan.centre_x[g] + cell.output_offset.0,
            GATE_Y + cell.output_offset.1,
            row_z[plan.row_of[g]] + cell.output_offset.2,
        )
    };

    let mut gate_pin: Vec<Position> = Vec::with_capacity(netlist.gates.len());
    let mut gate_output_positions: BTreeMap<String, (i32, i32, i32)> = BTreeMap::new();
    for (g, cell) in gate_cell.iter().enumerate() {
        let torch = torch_of(g, cell);
        gate_output_positions.insert(netlist.gates[g].output.clone(), (torch.x, torch.y, torch.z));
        // A merge's own outbound route starts one hop out from `torch`
        // (== `origin` for a merge, since `output_offset == (0, 0, 0)`),
        // exactly like a NOR's -- not *at* `torch` directly. `torch` itself
        // is directly adjacent to every one of this same gate's own
        // occupied input sockets (west/east/south), so a route starting
        // there could walk straight back through its own gate's input
        // wiring the moment it needed to head in any of those directions
        // (see `place_merge_gate`'s own doc comment for the failure this
        // caused, and how it was found). One more hop out is exactly as
        // clear of them as a NOR's own pin is of *its* input sockets.
        let p = torch.offset(geometry::output_direction(facing));
        ensure_floor(world, p);
        world.set(p.x, p.y, p.z, dust());
        gate_pin.push(p);
    }

    // What every net's own source pin actually carries -- `MAX_SIGNAL_
    // STRENGTH` for a lever or an ordinary NOR gate, honestly computed for
    // one sourced by a merge (see `compute_net_source_strengths`'s own doc
    // comment). Has to run before `plan_strengths`, which now consumes this
    // instead of assuming the constant unconditionally.
    let net_source_strength = compute_net_source_strengths(
        netlist,
        nets,
        plan,
        row_z,
        track_z,
        track_count,
        bypass,
        &lever_pin,
        &gate_pin,
        &gate_cell,
        facing,
    );

    // Strength planning: work out what every ramp's entry and every track's
    // exits will carry, before any of them are actually built. See
    // `plan_strengths` for why this has to happen up front rather than
    // inline in the passes below.
    let (entry_strength, exit_strength) = plan_strengths(
        nets,
        plan,
        track_z,
        track_count,
        &lever_pin,
        &gate_pin,
        bypass,
        &net_source_strength,
    );

    // Every net's own pin -- the first cell of its route -- belongs to that
    // net too, exactly like everything the passes below claim as they write
    // it. Claiming it here, once, up front covers every net regardless of
    // whether it turns out to have any ramps at all.
    for (n, net) in nets.iter().enumerate() {
        match net.source {
            Source::Lever(i) => footprint.claim(lever_pin[i], n),
            Source::Gate(g) => footprint.claim(gate_pin[g], n),
        }
    }

    // A merge's own junction, too -- `place_merge_gate` writes it as dust,
    // but the loop above only ever claims a gate's *pin*, so a merge's own
    // junction would otherwise be the one dust cell in the whole compiled
    // world with no `Reservation` entry at all (see `merge_gate_body_
    // owners`'s own doc comment for why that is a real gap, not a
    // formality): every keep-out decision downstream -- `seal_cross_talk`,
    // and every other net's own track/ramp placement deciding where it is
    // and is not allowed to run -- reads `Reservation`, so an unclaimed
    // junction cell is invisible to it, free for some *other* net's own
    // route to be planned straight through. Claimed under the same
    // representative net index `merge_gate_body_owners` already uses for
    // the connectivity and signal-strength invariants, so all three agree
    // on which declared group this cell belongs to.
    for (&position, &owner) in &merge_gate_body_owners(netlist, nets, &gate_output_positions) {
        footprint.claim(position, owner);
    }

    // Ramps first. `move_between_layers` seals the blocks around each landing,
    // and a seal only fills air -- so anything that has to run *through* a
    // sealed cell (the tracks, and the columns) has to be laid afterwards to
    // overwrite it.
    for (n, net) in nets.iter().enumerate() {
        if bypass[n] {
            // No ramp at all: `compute_bypass` only admits nets whose one
            // sink is close enough to connect directly at `GATE_Y` (see
            // `BYPASS_MAX_DISTANCE`). Handled entirely in the Columns pass
            // below.
            continue;
        }
        let mut route = Route {
            net: n,
            footprint: &mut *footprint,
            leg: None,
        };
        for slot in 0..net.channels.len() {
            let channel = net.channels[slot];
            let band = net.tracks[slot];
            let eff_band = effective_band(track_count, channel, band);
            let z = track_z[channel][band];
            let entry = Position::new(
                net.entry_column(slot),
                GATE_Y,
                z + band_ramp_length(eff_band),
            );
            route.begin(Leg::RampDown { slot });
            move_between_layers(world, entry, Facing::North, band_y(eff_band), &mut route);
            for exit in net.exits(slot, &plan.centre_x) {
                let top = Position::new(exit.x(), band_y(eff_band), z);
                route.begin(Leg::RampUp { slot, tap_x: exit.x() });
                move_between_layers(world, top, Facing::North, GATE_Y, &mut route);
            }
        }
    }

    // Columns at `GATE_Y`: from a source pin up to its ramp, and from a ramp's
    // landing on to whatever it feeds.
    for (n, net) in nets.iter().enumerate() {
        let mut route = Route {
            net: n,
            footprint: &mut *footprint,
            leg: None,
        };

        if bypass[n] {
            // A direct connection: no ramp, no track. `resolve_bypass_and_
            // geometry` guarantees this net has exactly one channel and
            // exactly one sink, and that the direct path is safe -- either
            // proven so by `compute_bypass` (the sink's approach column is
            // within `BYPASS_MAX_DISTANCE` of this net's own source column,
            // close enough that no other net's column can possibly sit
            // between them -- see that constant's derivation), or checked so
            // against the actual `Reservation` and every row it crosses
            // (`resolve_bypass_and_geometry`'s widened pass, for a source/sink
            // gap up to `BYPASS_QUERY_MAX_DISTANCE`). Either way, this exact
            // path -- the same waypoints, laid out the same way below -- is
            // what was checked, so a plain path from the source pin straight
            // to the socket is safe here too.
            //
            // The path bends at most twice: once to get from the pin's own
            // column onto the sink's approach column (skipped if they are
            // already the same column), and once more at the destination
            // row -- the same final jog every socket gets, whether its input
            // is west/east (approach column does not line up with the
            // socket) or south (it does, and this second bend never
            // happens). `lay_bent_path` handles all of that with one shared
            // strength budget end to end, rather than a mandatory refresh at
            // every bend -- see its own doc comment for why that used to
            // regress settle time instead of improving it.
            let (gate, input_index) = net.sinks[0][0];
            // A bypass net *is* one branch: no ramp, no track, nothing
            // shared with anybody, so its whole route is that one leg.
            route.begin(Leg::Branch { gate, input_index });
            let pin = match net.source {
                Source::Lever(i) => lever_pin[i],
                Source::Gate(g) => gate_pin[g],
            };
            let exit_x = approach_column(plan.centre_x[gate], input_index);
            let row_z_gate = row_z[plan.row_of[gate]];
            let (dx, dy, dz) = gate_cell[gate].input_offsets[input_index];
            let socket = Position::new(plan.centre_x[gate] + dx, GATE_Y + dy, row_z_gate + dz);

            // See `bypass_source_start`'s own doc comment: a merge-sourced
            // net that needs to jog off its own column gets one extra hop
            // of straight travel first, laid here (not folded into
            // `waypoints`) because the shared bent-path machinery is built
            // on a strict "at most one bend, then the destination" shape.
            let start = bypass_source_start(netlist, net, pin, exit_x, facing);
            let strength_at_start = if start != pin {
                ensure_floor(world, start);
                world.set(start.x, start.y, start.z, dust());
                route.claim(start.down());
                route.claim(start);
                net_source_strength[n].saturating_sub(1)
            } else {
                net_source_strength[n]
            };

            let mut waypoints: Vec<Position> = Vec::new();
            if start.x != exit_x {
                waypoints.push(Position::new(exit_x, GATE_Y, start.z));
            }
            if socket.x != exit_x {
                waypoints.push(Position::new(exit_x, GATE_Y, row_z_gate));
            }
            waypoints.push(socket);
            if merge_branch_is_bare(netlist, net, gate) {
                let reserve = bare_reserve_for_merge(netlist, nets, gate);
                let (_, terminal) = lay_bent_path_bare(
                    world,
                    start,
                    &waypoints,
                    strength_at_start,
                    reserve,
                    &mut route,
                );
                route.terminal(&netlist.gates[gate], input_index, socket, terminal);
            } else {
                let built = lay_bent_path(
                    world,
                    start,
                    &waypoints,
                    strength_at_start,
                    terminals[n][0][0],
                    &mut route,
                );
                route.terminal(&netlist.gates[gate], input_index, socket, built);
            }
            continue;
        }

        // `slot` indexes several independent containers here (`net.channels`,
        // `net.tracks`, `exit_strength[n]`, plus `net.entry_column`/`net.exits`
        // itself), so no single `.iter().enumerate()` covers all of them.
        #[allow(clippy::needless_range_loop)]
        for slot in 0..net.channels.len() {
            let channel = net.channels[slot];
            let band = net.tracks[slot];
            let eff_band = effective_band(track_count, channel, band);
            let z = track_z[channel][band];
            let entry = Position::new(
                net.entry_column(slot),
                GATE_Y,
                z + band_ramp_length(eff_band),
            );
            if slot == 0 {
                let pin = match net.source {
                    Source::Lever(i) => lever_pin[i],
                    Source::Gate(g) => gate_pin[g],
                };
                route.begin(Leg::Column { slot: 0 });
                lay_dust_run(
                    world,
                    pin,
                    Facing::North,
                    entry.offset(Facing::North),
                    net_source_strength[n],
                    ramp_reserve(eff_band),
                    &mut route,
                );
            }
            for exit in net.exits(slot, &plan.centre_x) {
                let landing = Position::new(exit.x(), GATE_Y, z - band_ramp_length(eff_band));
                let landing_strength =
                    ramp_ending_strength(band_levels(eff_band), exit_strength[n][slot][&exit.x()]);
                match exit {
                    Exit::Socket {
                        gate, input_index, ..
                    } => {
                        let (dx, dy, dz) = gate_cell[gate].input_offsets[input_index];
                        let row_z_gate = row_z[plan.row_of[gate]];
                        let socket =
                            Position::new(plan.centre_x[gate] + dx, GATE_Y + dy, row_z_gate + dz);
                        // South's socket sits dead on `landing`'s own column
                        // (see `approach_column`'s doc comment), so this
                        // waypoint list is just `[socket]` there -- no bend.
                        // West/east need one bend, onto the socket's own X,
                        // before `lay_bent_path` can reach it.
                        let mut waypoints: Vec<Position> = Vec::new();
                        if socket.x != landing.x {
                            waypoints.push(Position::new(landing.x, GATE_Y, row_z_gate));
                        }
                        waypoints.push(socket);
                        route.begin(Leg::Branch { gate, input_index });
                        if merge_branch_is_bare(netlist, net, gate) {
                            let reserve = bare_reserve_for_merge(netlist, nets, gate);
                            let (_, terminal) = lay_bent_path_bare(
                                world,
                                landing,
                                &waypoints,
                                landing_strength,
                                reserve,
                                &mut route,
                            );
                            route.terminal(&netlist.gates[gate], input_index, socket, terminal);
                        } else {
                            let sink = net.sinks[slot]
                                .iter()
                                .position(|&sink| sink == (gate, input_index))
                                .expect("every socket exit came from this channel's sinks");
                            let built = lay_bent_path(
                                world,
                                landing,
                                &waypoints,
                                landing_strength,
                                terminals[n][slot][sink],
                                &mut route,
                            );
                            route.terminal(&netlist.gates[gate], input_index, socket, built);
                        }
                    }
                    Exit::Feedthrough { x, next_slot } => {
                        let next_channel = net.channels[next_slot];
                        let next_band = net.tracks[next_slot];
                        let eff_next_band = effective_band(track_count, next_channel, next_band);
                        let next_z = track_z[next_channel][next_band];
                        let next_entry =
                            Position::new(x, GATE_Y, next_z + band_ramp_length(eff_next_band));
                        // The column that feeds the *next* slot's ramp, which
                        // is how `resolve_terminal_repeaters` names it.
                        route.begin(Leg::Column { slot: next_slot });
                        lay_dust_run(
                            world,
                            landing,
                            Facing::North,
                            next_entry.offset(Facing::North),
                            landing_strength,
                            ramp_reserve(eff_next_band),
                            &mut route,
                        );
                    }
                }
            }
        }
    }

    // Tracks last, so they overwrite the ramps' seal blocks where they have to
    // pass through them.
    for (n, net) in nets.iter().enumerate() {
        if bypass[n] {
            // Never touches a track -- see the Ramps pass above.
            continue;
        }
        let mut route = Route {
            net: n,
            footprint: &mut *footprint,
            leg: None,
        };
        // Same multi-container indexing as the Columns pass above -- see its
        // own `#[allow]` comment.
        #[allow(clippy::needless_range_loop)]
        for slot in 0..net.channels.len() {
            let channel = net.channels[slot];
            let band = net.tracks[slot];
            let eff_band = effective_band(track_count, channel, band);
            let z = track_z[channel][band];
            let source_x = net.entry_column(slot);
            let (lo, hi) = net.span(slot, &plan.centre_x);
            let mut taps: BTreeSet<i32> = BTreeSet::new();
            taps.insert(source_x);
            for exit in net.exits(slot, &plan.centre_x) {
                taps.insert(exit.x());
            }
            let track_incoming =
                ramp_ending_strength(band_levels(eff_band), entry_strength[n][slot]);
            lay_track(
                world,
                slot,
                (eff_band, z, source_x),
                (lo, hi),
                &taps,
                track_incoming,
                &mut route,
            );
        }
    }

    // Every netlist output gets a lamp so a person can read it by eye instead
    // of having to know which buried wall torch to stare at.
    //
    // The obvious placement -- directly above the output torch, the one
    // direction a wall torch's power is never withheld (`power_emitted_toward`:
    // `direction == Facing::Up => full`, and `full.block_power` there is
    // `Strong`) -- turns out to be wrong. Verified in the simulator: it makes
    // `compile()`'s own reference circuits oscillate and never settle. The
    // reason is indirect power: `recompute_dust_strengths` treats *any*
    // strongly-powered solid block as a conduit that recharges every redstone
    // wire adjacent to it, not just wires that are meant to be on that net
    // (this is also how a NOR cell's own merge dust weakly powers its centre
    // block -- the same mechanism, just at `Strong` instead of `Weak`
    // strength). A lamp sitting right above the torch is orthogonally
    // adjacent to that gate's own merge dust one row south of it at the same
    // Y, so a strongly-lit lamp would immediately re-inject the gate's own
    // output back into its input pool -- output high -> merge dust charged ->
    // centre block powered -> torch (and lamp) should go off -> merge dust
    // uncharged -> torch back on -> repeat forever.
    //
    // The safe fix is to only ever weakly power the lamp, since
    // `recompute_dust_strengths` only treats *strongly* powered blocks as
    // conduits (`if kind == BlockPower::Strong`) -- a weakly powered lamp can
    // never leak back into a neighbouring wire, whatever it happens to be
    // adjacent to. Redstone dust only ever weakly powers the block directly
    // *beneath* it (`power_emitted_toward`'s `RedstoneWire` arm), so the lamp
    // goes under the gate's own output pin dust (`gate_pin`), replacing the
    // plain floor block `ensure_floor` already put there to hold that dust up
    // -- a lamp satisfies the same "full top face" support requirement a
    // stone floor does, so nothing else about that dust changes. That
    // location is otherwise never touched by any other net (each gate owns
    // its output column exclusively), so the lamp is guaranteed to be powered
    // by nothing but its own gate's output.
    let mut output_positions: BTreeMap<String, (i32, i32, i32)> = BTreeMap::new();
    for output_name in &netlist.outputs {
        let g = netlist
            .gates
            .iter()
            .position(|gate| &gate.output == output_name)
            .expect("every output was checked to be driven by a gate above");
        let lamp_pos = gate_pin[g].down();
        world.set(lamp_pos.x, lamp_pos.y, lamp_pos.z, lamp());
        output_positions.insert(output_name.clone(), (lamp_pos.x, lamp_pos.y, lamp_pos.z));
    }

    // All three passes have run, so every leg of every net now has its
    // count and each sink's own path can finally be added up.
    resolve_terminal_repeaters(netlist, nets, &plan.centre_x, bypass, footprint);

    EmitResult {
        input_positions,
        output_positions,
        gate_output_positions,
        primitive_anchors,
    }
}

/// Fill in every recorded terminal's repeater count, now that all three of
/// `emit`'s passes have run.
///
/// A sink's count is the sum of the [`Leg`]s its signal passes through, in
/// the order it passes them: the column into slot 0, that slot's ramp down
/// onto its track, the track out to the tap this branch leaves by, the ramp
/// back up to `GATE_Y` -- then, for an intermediate slot, the feed-through
/// column that starts the next one, and at the last slot the branch into the
/// socket itself.
///
/// This walks `Net::exits`, the same enumeration all three passes walked, so
/// it cannot name a tap column the emitter did not actually use. It is the
/// only writer of `RouteTerminal::repeaters` on the legacy path, and it
/// checks that it found exactly one terminal for every sink it priced --
/// a sink whose branch was never recorded, or recorded twice, would
/// otherwise leave a zero behind that reads as a genuinely repeaterless
/// route.
fn resolve_terminal_repeaters(
    netlist: &Netlist,
    nets: &[Net],
    centre_x: &[i32],
    bypass: &[bool],
    footprint: &mut Footprint,
) {
    // `terminal` only records while recording, so there is nothing to fill
    // in on the enforcing pass -- and its ledger describes the same world.
    if !footprint.recording {
        return;
    }

    for (n, net) in nets.iter().enumerate() {
        let mut priced: BTreeMap<(String, usize), u64> = BTreeMap::new();
        let mut charge = |gate: usize, input_index: usize, total: u64| {
            let previous = priced.insert(
                (netlist.gates[gate].output.clone(), input_index),
                total,
            );
            assert!(
                previous.is_none(),
                "net {n} priced `{}.in[{input_index}]` twice",
                netlist.gates[gate].output
            );
        };

        if bypass[n] {
            let (gate, input_index) = net.sinks[0][0];
            charge(
                gate,
                input_index,
                footprint.leg_repeaters(n, Leg::Branch { gate, input_index }),
            );
        } else {
            // What the signal has already passed through by the time it
            // reaches this slot's entry column: zero at slot 0, and at every
            // later slot the running total carried across the feed-through
            // that led here.
            let mut arriving = 0u64;
            for slot in 0..net.channels.len() {
                let on_the_track = arriving
                    + footprint.leg_repeaters(n, Leg::Column { slot })
                    + footprint.leg_repeaters(n, Leg::RampDown { slot });
                let mut carried = None;
                for exit in net.exits(slot, centre_x) {
                    let tap_x = exit.x();
                    let at_landing = on_the_track
                        + footprint.leg_repeaters(n, Leg::Track { slot, tap_x })
                        + footprint.leg_repeaters(n, Leg::RampUp { slot, tap_x });
                    match exit {
                        Exit::Socket {
                            gate, input_index, ..
                        } => charge(
                            gate,
                            input_index,
                            at_landing
                                + footprint.leg_repeaters(n, Leg::Branch { gate, input_index }),
                        ),
                        // At most one per slot, by construction: `Net::exits`
                        // pushes `hops[slot]` and nothing else.
                        Exit::Feedthrough { .. } => carried = Some(at_landing),
                    }
                }
                arriving = carried.unwrap_or(0);
            }
        }

        let Some(terminals) = footprint.route_terminals.get_mut(n) else {
            assert!(
                priced.is_empty(),
                "net {n} has {} sink(s) but recorded no terminal at all",
                priced.len()
            );
            continue;
        };
        assert_eq!(
            terminals.len(),
            priced.len(),
            "net {n} recorded {} terminal(s) for {} sink(s)",
            terminals.len(),
            priced.len()
        );
        for terminal in terminals.iter_mut() {
            let key = (terminal.sink.gate.clone(), terminal.sink.input_index);
            terminal.repeaters = *priced.get(&key).unwrap_or_else(|| {
                panic!(
                    "net {n} recorded a terminal at `{}.in[{}]`, which is not one of its sinks",
                    terminal.sink.gate, terminal.sink.input_index
                )
            });
        }
    }
}

/// Whether an already-reserved terminal repeater can safely become dust.
///
/// This checks the physical conditions only. Its caller owns the netlist
/// condition that the sink is an ordinary NOR support; merge *sources* are
/// safe here because their group strength is computed before this predicate.
/// Keeping that policy separate makes the physical proof useful in small
/// adversarial worlds below.
fn directed_dust_terminal_is_legal(
    world: &mut World,
    reservation: &Reservation,
    net: usize,
    socket: Position,
    support: Position,
    predecessor_strength: u8,
) -> bool {
    let toward_support = direction_from(socket, support);
    let predecessor = socket.offset(toward_support.opposite());

    // The predecessor must be this route's own live dust.  This excludes a
    // terminal refresh repeater and proves the new dust retains a positive
    // strength after its one final decay hop.
    if reservation.get(&predecessor) != Some(&net)
        || world.get(predecessor.x, predecessor.y, predecessor.z).kind != BlockKind::RedstoneWire
    {
        return false;
    }

    // No foreign route may occupy the terminal or a horizontal neighbour.
    // Besides preventing an electrical merge, the lateral part preserves a
    // one-axis dust shape, which is what gives this terminal direction.
    let approach = TerminalApproach::new(
        Anchor {
            x: predecessor.x,
            y: predecessor.y,
            z: predecessor.z,
        },
        Anchor {
            x: socket.x,
            y: socket.y,
            z: socket.z,
        },
        Anchor {
            x: support.x,
            y: support.y,
            z: support.z,
        },
        predecessor_strength,
        cell_is_free_for(reservation, socket, net),
    );
    if terminal_style(&approach) != TerminalStyle::DirectedDustIntoSupport {
        return false;
    }

    // Substitute the exact dust state just long enough to ask the
    // simulator's measured directionality predicate.  This catches an own
    // perpendicular attachment that the reservation deliberately permits.
    let old = world.get(socket.x, socket.y, socket.z).clone();
    if old.kind != BlockKind::Repeater {
        return false;
    }
    world.set(socket.x, socket.y, socket.z, dust());
    let points_into_support = dust_powers_block_toward(world, socket, toward_support);
    world.set(socket.x, socket.y, socket.z, old);
    points_into_support
}

/// Whether the component at `socket` really drives its adjacent `support`.
///
/// A terminal can be either the traditional repeater or a straight dust cell
/// whose weak directional power enters the support.  The graph has the same
/// signal-flow edge in both cases, so readers that audit a compiled world
/// must ask this semantic question rather than treating a repeater's block
/// kind as the topology itself.
pub(crate) fn input_socket_feeds_support(
    world: &World,
    socket: Position,
    support: Position,
) -> bool {
    let toward_support = direction_from(socket, support);
    let socket_state = world.get(socket.x, socket.y, socket.z);
    (socket_state.kind == BlockKind::Repeater
        && socket_state.facing == Some(toward_support.opposite()))
        || (socket_state.kind == BlockKind::RedstoneWire
            && dust_powers_block_toward(world, socket, toward_support))
}

/// Promote only the terminal repeaters which can become real directed dust.
///
/// The input `world` and `reservation` are a completed *all-repeater*
/// baseline.  That removes the usual circularity: every terminal candidate
/// sees every other route and its lateral keep-out before any candidate is
/// promoted.  A promotion changes the kind at its already-claimed socket, not
/// the path or its footprint, so the following record and real emissions use
/// one fixed decision without needing a fixed-point iteration.
fn resolve_directed_dust_terminals(
    world: &mut World,
    reservation: &Reservation,
    netlist: &Netlist,
    nets: &[Net],
    input_positions: &BTreeMap<String, (i32, i32, i32)>,
    gate_output_positions: &BTreeMap<String, (i32, i32, i32)>,
) -> TerminalKinds {
    let mut terminals = default_terminal_kinds(nets);
    let groups = MergeGroups::build(netlist, nets);

    // A merge's junction and outbound pin are gate body rather than route
    // cells, so they have no Reservation entry.  Include them exactly as the
    // signal-strength invariant does: a direct terminal driven by a merge
    // must reason about the actual joined dust network, not pretend the
    // merge's named output is a fresh full-strength source.
    let mut group_cells: HashMap<usize, HashSet<Position>> = HashMap::new();
    for (&position, &owner) in reservation {
        group_cells
            .entry(groups.root(owner))
            .or_default()
            .insert(position);
    }
    for (&position, &owner) in &merge_gate_body_owners(netlist, nets, gate_output_positions) {
        group_cells
            .entry(groups.root(owner))
            .or_default()
            .insert(position);
    }

    // Every real source in a declared merge group feeds the same dust.  The
    // decay walk keeps the strongest arrival at each cell, the same physical
    // behaviour and the same proof used by `verify_signal_strength`.
    let mut group_sources: HashMap<usize, Vec<(Position, &BlockState)>> = HashMap::new();
    for (n, net) in nets.iter().enumerate() {
        if matches!(net.source, Source::Gate(g) if netlist.gates[g].is_merge()) {
            continue;
        }
        let (source, source_state) = match net.source {
            Source::Lever(input) => {
                let &(x, y, z) = input_positions
                    .get(&netlist.inputs[input])
                    .expect("emit records every primary input");
                (Position::new(x, y, z), world.get(x, y, z))
            }
            Source::Gate(gate) => {
                let &(x, y, z) = gate_output_positions
                    .get(&netlist.gates[gate].output)
                    .expect("emit records every gate output");
                (Position::new(x, y, z), world.get(x, y, z))
            }
        };
        group_sources
            .entry(groups.root(n))
            .or_default()
            .push((source, source_state));
    }
    let empty_sources: Vec<(Position, &BlockState)> = Vec::new();
    let group_strength: HashMap<usize, HashMap<Position, u8>> = group_cells
        .iter()
        .map(|(&root, cells)| {
            let sources = group_sources.get(&root).unwrap_or(&empty_sources);
            (root, net_signal_strength(world, cells, sources))
        })
        .collect();

    // Each socket below is a sink gate's own, so it is that gate's facing this
    // turns -- north for all of them, and a literal rather than a parameter on
    // purpose, for the same two reasons `merge_gate_body_owners` gives. The
    // loop below visits every sink of every net, so one `CellFacing` is the
    // wrong shape for it: what it will need is a facing per gate. And there is
    // none to be had here -- this runs against `emit`'s all-repeater baseline
    // world, built and consumed inside `compile` before any `CompiledCircuit`
    // (and so any `gate_facings`) exists, and its only caller is `compile`
    // itself at that same point, so a parameter would move the identical
    // literal one frame up and dress it as a lookup. When `emit` starts
    // choosing facings, the placer's own per-gate choice is what has to arrive
    // here.
    let facing = geometry::CellFacing::NORTH;

    for (n, net) in nets.iter().enumerate() {
        let strength = &group_strength[&groups.root(n)];

        for (slot, sinks) in net.sinks.iter().enumerate() {
            for (sink, &(gate, input_index)) in sinks.iter().enumerate() {
                if netlist.gates[gate].is_merge() {
                    continue;
                }

                let &(torch_x, torch_y, torch_z) = gate_output_positions
                    .get(&netlist.gates[gate].output)
                    .expect("emit records every gate output");
                let torch = Position::new(torch_x, torch_y, torch_z);
                let Some(support) =
                    torch_support_position(world.get(torch.x, torch.y, torch.z), torch)
                else {
                    continue;
                };
                let socket = support.offset(geometry::input_directions(facing)[input_index]);
                let predecessor = socket.offset(direction_from(socket, support).opposite());
                if directed_dust_terminal_is_legal(
                    world,
                    reservation,
                    n,
                    socket,
                    support,
                    strength.get(&predecessor).copied().unwrap_or(0),
                ) {
                    terminals[n][slot][sink] = TerminalKind::DirectedDustIntoSupport;
                }
            }
        }
    }

    terminals
}

/// The X/Z footprint the world needs to hold `plan`/`nets` laid out with
/// `row_z` -- shared by every world `compile` and `resolve_bypass_and_geometry`
/// allocate (the real one, and the throwaway probe the latter builds).
///
/// `size_x` never depends on `row_z` (every column's X, including a
/// feed-through hop, is fixed by `reserve_columns` before any bypass decision
/// exists at all), so the same value serves the baseline probe's world and
/// the final one; only `size_z` moves with `row_z`.
fn world_size(plan: &Floorplan, nets: &[Net], row_z: &[i32]) -> (i32, i32) {
    let size_x = plan
        .centre_x
        .iter()
        .chain(plan.lever_x.iter())
        .copied()
        .max()
        .unwrap_or(ORIGIN_X)
        .max(
            nets.iter()
                .flat_map(|net| net.hops.iter())
                .copied()
                .max()
                .unwrap_or(ORIGIN_X),
        )
        + ENTRY_OFFSET
        + 4;
    let size_z = row_z[0] + 4;
    (size_x, size_z)
}

/// One gate cell's socket geometry per distinct (input count, facing) pair --
/// every offset a `NorCell` carries is relative to the cell's own origin, but
/// as of `place_nor_gate` taking a `geometry::CellFacing`, "relative" still
/// means "turned along with the cell": a west-facing 2-input cell's sockets
/// sit at different offsets than a north-facing one's. Both the input count
/// and the facing have to be in the key, or a lookup for one facing silently
/// hands back another's geometry. Shared by `resolve_bypass_and_geometry`
/// (needs a candidate socket position before any real gate is placed) and
/// `routing_stats` (needs the same lookup to read results back out of an
/// already-compiled world).
fn cell_geometry_by_input_count(netlist: &Netlist) -> HashMap<(usize, geometry::CellFacing), NorCell> {
    let mut cells = HashMap::new();
    let mut scratch = World::new(20, GATE_ONLY_SCRATCH_HEIGHT, 20);
    // North is the only key this map is ever built for, because it is the only
    // facing `emit` ever builds. The key already has room for the other three;
    // what is missing is a source of per-gate facings to enumerate, which is
    // `emit`'s to provide. A lookup at a facing that was never inserted panics
    // on the index rather than quietly handing back north's geometry -- see
    // this function's doc comment on why the facing is in the key at all.
    for gate in &netlist.gates {
        cells
            .entry((gate.inputs.len(), geometry::CellFacing::NORTH))
            .or_insert_with(|| {
                place_nor_gate(
                    &mut scratch,
                    (8, GATE_Y, 8),
                    gate.inputs.len(),
                    geometry::CellFacing::NORTH,
                )
            });
    }
    cells
}

/// Where a net's own source signal enters the router -- the same position
/// `emit` computes when it actually places the lever/gate output pin
/// (`place_primary_input`, `torch_of`), recomputed purely from geometry and a
/// `NorCell` lookup so `resolve_bypass_and_geometry` can size up a
/// *candidate* bypass path before any real `World` exists to place it in.
///
/// `facing` is how the thing at `source` -- a lever or a gate -- was built,
/// and it is also what keys `cell_of_count`, so the caller must pass the same
/// value it used to build that map or the lookup finds another facing's
/// geometry (or nothing at all). Taking it as a parameter rather than naming
/// north here is the point: the caller and this function used to assert north
/// separately for the very same gate, with nothing tying the two assertions
/// together.
fn source_pin_position(
    netlist: &Netlist,
    plan: &Floorplan,
    row_z: &[i32],
    cell_of_count: &HashMap<(usize, geometry::CellFacing), NorCell>,
    source: Source,
    facing: geometry::CellFacing,
) -> Position {
    match source {
        Source::Lever(i) => Position::new(plan.lever_x[i], GATE_Y, row_z[0])
            .offset(geometry::output_direction(facing)),
        Source::Gate(g) => {
            let cell = &cell_of_count[&(netlist.gates[g].inputs.len(), facing)];
            let torch = Position::new(
                plan.centre_x[g] + cell.output_offset.0,
                GATE_Y + cell.output_offset.1,
                row_z[plan.row_of[g]] + cell.output_offset.2,
            );
            torch.offset(geometry::output_direction(facing))
        }
    }
}

/// Whether `pos` -- one cell of a *candidate* bypass path -- is safe to
/// write: neither `pos` itself nor any of its four same-layer neighbours may
/// already belong to a different net in `reservation`. Same-layer adjacency
/// is exactly what `dust_connections` joins unconditionally (see its own doc
/// comment: same-layer is unconditional there, unlike the climb/descend
/// rules), so it is the one hazard a flat `GATE_Y` bypass path -- which never
/// ramps, so never needs those climb/descend rules `dust_reach` also models
/// -- actually has to avoid.
fn cell_is_free_for(reservation: &Reservation, pos: Position, net: usize) -> bool {
    let owned_by_other = |p: Position| matches!(reservation.get(&p), Some(&owner) if owner != net);
    !owned_by_other(pos)
        && HORIZONTAL
            .iter()
            .all(|&direction| !owned_by_other(pos.offset(direction)))
}

/// Whether a horizontal jog from `lo` to `hi` (inclusive, one row's own Z --
/// see `resolve_bypass_and_geometry`) crosses any *other* gate's or lever's
/// body in that row. `self_zone` is the jog's own source's body, which the
/// jog necessarily starts inside of; a gate or lever body is never a
/// conductor, so `row_body_zones` is the only place this keep-out is
/// recorded at all -- a `Reservation` alone would miss it entirely.
fn jog_crosses_another_row_zone(
    zones: &[(i32, i32)],
    self_zone: (i32, i32),
    lo: i32,
    hi: i32,
) -> bool {
    zones
        .iter()
        .any(|&zone| zone != self_zone && zone.0 <= hi && lo <= zone.1)
}

/// Resolve which nets bypass the ramp/track machinery, and lay out the
/// geometry that follows from that decision.
///
/// Two passes:
///
/// 1. **Proven-safe pass.** `compute_bypass`'s geometric proof (see
///    `BYPASS_MAX_DISTANCE`) decides an initial, conservative bypass set;
///    `assign_tracks`/`layout_z` turn that into a real geometry, and one
///    throwaway `emit` (`Footprint::record`, against a scratch `World`) turns
///    that geometry into a complete `Reservation` -- exactly what `compile`
///    itself built before this function existed.
/// 2. **Widened pass.** Every net the proof does not already cover, but whose
///    source/sink columns are within `query_limit`, gets a candidate direct
///    path built against that same baseline geometry, then checked against
///    the `Reservation` from step 1 (`cell_is_free_for`, for every *other*
///    net's conductors) and against every row it jogs through
///    (`jog_crosses_another_row_zone`, for gate/lever bodies, which are never
///    conductors and so never appear in a `Reservation` at all). A clear
///    candidate is promoted.
///
/// # Why the baseline reservation alone is *not* enough, and what closes the
/// gap
///
/// Every column this router ever places is, by construction
/// (`reserve_columns`'s own doc comment), at least `COLUMN_CLEARANCE` from
/// every other column in the same channel. A net's own source and sink
/// columns sit at the same X whether or not that net ends up bypassing --
/// bypassing only changes whether it ever reaches `TRACK_Y`, never which X it
/// occupies -- so "does some *other* net have a column between mine" is a
/// fact about `reserve_columns`'s output alone, never about who else happens
/// to be promoted already. The baseline `Reservation` already contains every
/// such column, as either a conventionally-routed net's own entry/exit dust
/// or a feed-through's permanent hop (feed-throughs are never bypass-eligible
/// at all, so their columns are identical in every candidate reservation).
///
/// That argument covers the *columns* -- it says nothing about the
/// *horizontal jog* a widened candidate draws at its own row's Z when its pin
/// and `exit_x` disagree (`waypoints` below). That jog is a new cell range
/// that does not exist in the baseline at all, for any candidate, because the
/// baseline was built entirely from `bypass_proven` -- nothing in it ever
/// jogs. Two candidates in the same widened pass can introduce jogs that
/// overlap in X at the same Z without either one's check, against the
/// baseline alone, ever seeing the other's: this was a real, reproducible bug
/// (see `docs/superpowers/specs/2026-08-09-channel-safety-condition.md`), not
/// a hypothetical.
///
/// The fix is to stop treating the baseline as the final word: `probe_
/// reservation` is mutated in place as each candidate is promoted, folding in
/// every cell its `bent_path_cells` actually occupies (not just its fixed
/// columns) under its own net index. So the loop below checks each candidate
/// against "the baseline plus every sibling already promoted this pass", the
/// same live-reservation discipline `reserve_feedthrough` already uses for
/// feed-through columns -- and *that* is what makes "no candidate's promotion
/// can invalidate another's answer" true: not because jogs cannot collide,
/// but because a later candidate's check can now see an earlier one's jog and
/// will correctly refuse to overlap it.
///
/// # Fan-out and feed-through: still excluded, on purpose
///
/// This still only ever considers nets with exactly one channel and exactly
/// one sink -- the same restriction `compute_bypass` already had. The
/// `Reservation` query does not, by itself, make either of the excluded
/// shapes safe to add:
///
/// - **Fan-out** (one channel, several sinks) needs more than one jog leaving
///   the same trunk cell. Those jogs would have to share one strength budget
///   from a single branch point, the way `lay_track`'s taps already do for a
///   real track -- but a track's taps are all on one shared straight run,
///   while a bypass fan-out's branches point in different directions from
///   the trunk, which `lay_bent_path` (built for one single-source-to-single-
///   sink path) cannot express at all. That is new plumbing, not a
///   consequence of trusting the `Reservation` instead of a proof; the query
///   would still be answerable, but the *route* it would be answering for
///   does not exist yet.
/// - **Feed-through** (more than one channel) is harder for a sharper reason:
///   its whole reason to exist is reaching a row that is *not* the next one,
///   which means its direct path would have to cross an entire intervening
///   row -- gate bodies and all, at the row's own Z, not just skirt one row's
///   edge the way a single-channel jog does. `jog_crosses_another_row_zone`
///   generalises to "any number of rows" without difficulty, but a feed-
///   through's *own* channel-to-channel hop column (`net.hops`) already
///   exists specifically to solve this by going around the row instead of
///   through it -- so the reservation-query win here would only ever be
///   skipping a hop column's own two short ramp-free stretches, not the
///   track/ramp machinery a single-channel bypass skips. Far smaller payoff
///   for materially more surface area to get wrong.
///
/// Both are left to the existing feed-through/track machinery. If a later
/// measurement shows either payoff is worth the extra plumbing, this is
/// where it would plug in -- the `Reservation` this function already builds
/// does not need to change to support it.
///
/// # Where `BYPASS_QUERY_MAX_DISTANCE` came from
///
/// Measured on the four reference circuits, release build (`cargo run --bin
/// build_circuit` for box/blocks, `cargo run --bin routing_cost_report` for
/// settle and bypass counts), sweeping the query limit with
/// `BYPASS_MAX_DISTANCE` (3, i.e. the proof alone, no query at all) as the
/// "off" baseline:
///
/// ```text
/// bypass edges (direct GATE_Y route) out of all routed edges:
///   limit            and4    full_adder   segment_a   seven_segment
///   3  (off)         6/10     6/32        15/83        34/156
///   6                7/10    11/32        26/83        45/156
///   8                8/10    13/32        27/83        45/156
///   9-11             8/10    13/32        27/83        45/156
///   12               8/10    13/32        27/83        46/156
///   15-30            8/10    13/32        27/83        46/156  (unchanged past 12)
///
/// non-air blocks:
///   limit            and4    full_adder   segment_a   seven_segment
///   3  (off)          571     2246         6716        16694
///   6                 551     2116         6686        16694
///   8                 551     2066         6686        16694
///   9-11              551     2066         6686        16694
///   12                551     2066         6686        16654
///   15-30             551     2066         6686        16654  (unchanged past 12)
///
/// worst-case settle (game ticks):
///   limit            and4    full_adder   segment_a   seven_segment
///   3  (off)           30       82           94          124
///   6                  28       78           92          124
///   8                  28       76           92          124
///   9-11               28       76           92          124
///   12                 28       76           92          122
///   15-30              28       76           92          122  (unchanged past 12)
/// ```
///
/// Every circuit is flat from 12 all the way to 30 -- checked directly, not
/// extrapolated -- which matches the geometric ceiling this router actually
/// has: past a certain jog length a gate-sourced candidate runs into the
/// *next* gate in its own row (`jog_crosses_another_row_zone`), and once
/// every net that will ever clear that check has been found, asking about a
/// longer one only spends compile time, never finds another win. 12 is the
/// smallest limit at which all four circuits already show that flatness (11
/// still leaves `seven_segment` one edge and 40 blocks short of where 12-30
/// all land), so it is what `BYPASS_QUERY_MAX_DISTANCE` is set to -- nothing
/// past it was observed to help even once.
fn resolve_bypass_and_geometry(
    netlist: &Netlist,
    plan: &Floorplan,
    nets: &mut [Net],
    row_count: usize,
    channel_count: usize,
    query_limit: i32,
) -> (Vec<bool>, Vec<i32>, Vec<Vec<i32>>) {
    let bypass_proven = compute_bypass(nets, plan);
    let baseline_terminals = default_terminal_kinds(nets);
    let baseline_track_count = assign_tracks(plan, nets, channel_count, &bypass_proven);
    let (baseline_row_z, baseline_track_z) =
        layout_z(row_count, channel_count, &baseline_track_count);

    let (size_x, size_z) = world_size(plan, nets, &baseline_row_z);
    let mut scratch = World::new(
        size_x.max(8),
        world_height(&baseline_track_count),
        size_z.max(8),
    );
    let mut footprint = Footprint::record();
    {
        let geometry = RoutingGeometry {
            plan,
            row_z: &baseline_row_z,
            nets,
            track_z: &baseline_track_z,
            track_count: &baseline_track_count,
            bypass: &bypass_proven,
            terminals: &baseline_terminals,
        };
        emit(&mut scratch, netlist, &geometry, &mut footprint);
    }
    // Mutated as each candidate below is promoted, not just read: see "Why
    // the baseline reservation alone is not enough" above.
    let mut probe_reservation = footprint.reservation;
    drop(scratch);

    let cell_of_count = cell_geometry_by_input_count(netlist);
    let row_zones = row_body_zones(plan, row_count);
    let mut bypass_final = bypass_proven.clone();

    // North, and a literal, because this pass runs *before* the real world is
    // built: it decides which nets get a bypass, and `emit` -- the only thing
    // that knows how a gate was actually built -- is downstream of the answer.
    // The three uses below are what `cell_geometry_by_input_count` was keyed
    // for and what `emit` will replay, so they have to agree with `emit`'s own
    // binding, which is north. The other caller, `routing_stats::
    // recompute_geometry`, replays this pass from a `Netlist` and nothing else
    // -- on purpose, so what it reports is re-derived rather than remembered
    // -- so it has no facing to hand over either, even though the module above
    // it holds a finished `CompiledCircuit`.
    //
    // One binding stands for two different gates here: the net's *source* (at
    // `source_pin_position` and `bypass_source_start`) and its *sink* (the
    // `cell_of_count` key and the socket derived from it). That is sound only
    // while every gate is north. The day facings vary, this splits into two
    // lookups -- source facing and sink facing -- and `cell_geometry_by_input_
    // count` has to build a cell per facing actually used, not just north's.
    let facing = geometry::CellFacing::NORTH;

    for (n, net) in nets.iter().enumerate() {
        if bypass_proven[n] || net.channels.len() != 1 || net.sinks[0].len() != 1 {
            continue;
        }
        let (gate, input_index) = net.sinks[0][0];
        let exit_x = approach_column(plan.centre_x[gate], input_index);
        let distance = (exit_x - net.source_column).abs();
        if distance <= BYPASS_MAX_DISTANCE || distance > query_limit {
            continue;
        }

        let pin = source_pin_position(
            netlist,
            plan,
            &baseline_row_z,
            &cell_of_count,
            net.source,
            facing,
        );
        let row_z_gate = baseline_row_z[plan.row_of[gate]];
        let cell = &cell_of_count[&(netlist.gates[gate].inputs.len(), facing)];
        let (dx, dy, dz) = cell.input_offsets[input_index];
        let socket = Position::new(plan.centre_x[gate] + dx, GATE_Y + dy, row_z_gate + dz);

        if pin.x != exit_x {
            let self_zone = match net.source {
                Source::Lever(_) => (
                    net.source_column - COLUMN_CLEARANCE + 1,
                    net.source_column + COLUMN_CLEARANCE - 1,
                ),
                Source::Gate(_) => (
                    net.source_column - ENTRY_OFFSET - COLUMN_CLEARANCE + 1,
                    net.source_column + ENTRY_OFFSET + COLUMN_CLEARANCE - 1,
                ),
            };
            let (lo, hi) = (pin.x.min(exit_x), pin.x.max(exit_x));
            if jog_crosses_another_row_zone(&row_zones[net.channels[0]], self_zone, lo, hi) {
                continue;
            }
        }

        // See `bypass_source_start`'s own doc comment -- this candidate's
        // real geometry, once promoted, starts one hop further out than
        // `pin` whenever the source is a merge and a jog is actually
        // needed, so the safety check below has to examine that same
        // shifted start (and its own extra cell) rather than `pin` alone,
        // or a promoted candidate could pass this check against geometry
        // `emit` never actually builds.
        let start = bypass_source_start(netlist, net, pin, exit_x, facing);

        let mut waypoints: Vec<Position> = Vec::new();
        if start.x != exit_x {
            waypoints.push(Position::new(exit_x, GATE_Y, start.z));
        }
        if socket.x != exit_x {
            waypoints.push(Position::new(exit_x, GATE_Y, row_z_gate));
        }
        waypoints.push(socket);

        let mut cells = bent_path_cells(start, &waypoints);
        if start != pin {
            cells.push(start);
        }
        if cells
            .iter()
            .all(|&pos| cell_is_free_for(&probe_reservation, pos, n))
        {
            bypass_final[n] = true;
            // Fold this candidate's own cells into the reservation before
            // the next candidate is checked -- otherwise two candidates
            // decided in the same pass, whose jogs overlap each other
            // without overlapping anything in the baseline, would both look
            // clear. See this function's doc comment.
            for &pos in &cells {
                probe_reservation.entry(pos).or_insert(n);
            }
        }
    }

    let track_count = assign_tracks(plan, nets, channel_count, &bypass_final);
    let (row_z, track_z) = layout_z(row_count, channel_count, &track_count);
    (bypass_final, row_z, track_z)
}

/// Which net `nets[index]` is, by the name a person compiling the netlist
/// would recognise -- the lever's own input name, or the gate output the net
/// carries. Used for naming cells in a `ConnectivityViolation` or a
/// `TorchMergeViolation`.
fn net_name(netlist: &Netlist, nets: &[Net], index: usize) -> String {
    net_source_name(netlist, &nets[index]).to_string()
}

/// The same lookup `net_name` does, without the allocation `.clone()` would
/// force -- `MergeGroups::build` below calls this once per net just to find
/// *another* net's index by name, never to display it, so paying for a
/// `String` every time would be wasted work on every single `compile`.
fn net_source_name<'a>(netlist: &'a Netlist, net: &Net) -> &'a str {
    match net.source {
        Source::Lever(i) => netlist.inputs[i].as_str(),
        Source::Gate(g) => netlist.gates[g].output.as_str(),
    }
}

// ---------------------------------------------------------------------
// Declared wire merges
// ---------------------------------------------------------------------
//
// A net with several sources is exactly what a wire-merge OR is (see
// `docs/superpowers/specs/2026-08-08-gate-types-and-wired-or.md`, "The
// invariants have to allow multi-source nets, carefully") -- and exactly
// what two unrelated nets' dust touching by accident also looks like. The
// only thing that tells them apart is whether the netlist *asked* for the
// join, so that has to be checked, not assumed: `Gate::is_merge` is where
// the netlist says so, and `MergeGroups` is the one place both invariants
// below go to ask.
//
// A merge gate's "output" is not a separate pin the way a NOR's torch is --
// it is only a name for the point downstream of where its declared inputs'
// own dust runs are allowed to physically join. Electrically, the merge's
// output net and every one of its declared input nets are the *same* net,
// not several nets that happen to touch. `MergeGroups` computes exactly
// that: the union-find closure, over every `is_merge` gate in the netlist,
// of its output net with each of its declared input nets -- transitively,
// so a merge feeding another merge collapses into one group exactly as far
// as the netlist actually says it should, no further.
//
// A netlist with no `is_merge` gate at all gives every net its own
// singleton group, which makes `same_group` agree with plain `==`
// everywhere -- this is a strict generalisation of today's one-source-per-
// net check, not a separate code path next to it.
struct MergeGroups {
    parent: Vec<usize>,
}

impl MergeGroups {
    fn build(netlist: &Netlist, nets: &[Net]) -> Self {
        let mut parent: Vec<usize> = (0..nets.len()).collect();

        let index_of_signal: HashMap<&str, usize> = nets
            .iter()
            .enumerate()
            .map(|(i, net)| (net_source_name(netlist, net), i))
            .collect();

        for gate in &netlist.gates {
            if !gate.is_merge() {
                continue;
            }
            // Union every one of this merge's declared inputs together,
            // plus its own output net when it has one. The output can be
            // missing (`build_nets` drops a signal with no gate-input sink,
            // which is exactly what a merge feeding *only* a declared
            // circuit output looks like) -- but the inputs' own dust still
            // physically touches at the junction regardless of whether
            // anything reads the result any further, so they must still be
            // unioned with *each other* even then. Routing every union
            // through `output_index` alone (the previous version of this
            // loop) silently skipped exactly that case: with no output net
            // to hang the union on, two private branches sharing a junction
            // would have looked like two unrelated nets whose dust
            // happened to touch -- the very bug this whole relaxation
            // exists to keep catching everywhere else.
            let mut members: Vec<usize> = gate
                .inputs
                .iter()
                .filter_map(|input| index_of_signal.get(input.as_str()).copied())
                .collect();
            if let Some(&output_index) = index_of_signal.get(gate.output.as_str()) {
                members.push(output_index);
            }
            for pair in members.windows(2) {
                Self::union(&mut parent, pair[0], pair[1]);
            }
        }

        MergeGroups { parent }
    }

    fn find(parent: &mut [usize], x: usize) -> usize {
        if parent[x] != x {
            parent[x] = Self::find(parent, parent[x]);
        }
        parent[x]
    }

    fn union(parent: &mut [usize], a: usize, b: usize) {
        let root_a = Self::find(parent, a);
        let root_b = Self::find(parent, b);
        if root_a != root_b {
            parent[root_a] = root_b;
        }
    }

    /// Read-only root lookup -- no path compression, since both call sites
    /// below only ever hold a shared `&self`. `build`'s own unioning keeps
    /// every chain at most as long as the number of merges feeding one
    /// another, which nothing in this project constructs deep enough for
    /// the missing compression to cost anything observable.
    fn root(&self, mut index: usize) -> usize {
        while self.parent[index] != index {
            index = self.parent[index];
        }
        index
    }

    fn same_group(&self, a: usize, b: usize) -> bool {
        self.root(a) == self.root(b)
    }
}

/// Every merge gate's own gate-*body* cells -- its junction, and the
/// one-hop outbound pin `emit` starts its downstream route from -- mapped
/// to a representative net index from that gate's own declared-merge group
/// (any member works: callers only ever feed this into [`MergeGroups::
/// same_group`] or an equivalent root comparison, never read it as "the"
/// net).
///
/// These cells are dust (`RedstoneWire`), which is what makes them
/// different from a NOR's own support-and-torch body: a NOR's body is
/// stone and a torch, neither of which `verify_connectivity`'s wire walk or
/// `verify_signal_strength`'s decay walk ever visits, so it never had to
/// know about them. A merge's body is dust like everything around it, so
/// both walks *do* visit it -- but neither one ever finds it in
/// `Reservation`, because `place_merge_gate` writes it directly as gate
/// placement, the same way a NOR's support and torch are, and routing
/// (`Route::claim`) only ever claims *wire*. Without this lookup as a
/// fallback wherever `Reservation` comes up empty, that gap is a false
/// negative for connectivity (a foreign net's dust touching exactly at an
/// unclaimed junction cell has nothing to disagree with) and a dead end for
/// signal strength (a walk that cannot continue past an unclaimed cell
/// strands whatever sits beyond it -- see `emit`'s own doc comment on why
/// the outbound pin is one hop *past* the junction, not the junction
/// itself).
fn merge_gate_body_owners(
    netlist: &Netlist,
    nets: &[Net],
    gate_output_positions: &BTreeMap<String, (i32, i32, i32)>,
) -> HashMap<Position, usize> {
    let groups = MergeGroups::build(netlist, nets);
    let index_of_signal: HashMap<&str, usize> = nets
        .iter()
        .enumerate()
        .map(|(i, net)| (net_source_name(netlist, net), i))
        .collect();

    // The outbound pin sits one hop off the junction in the merge's own
    // output direction, and `place_merge_gate` -- the only thing that writes
    // these cells -- is only ever called by `emit`, which builds north.
    //
    // A literal rather than a parameter, deliberately, and for a reason that
    // is about shape rather than laziness: the loop below visits *every* merge
    // gate, so what it will eventually need is a facing per gate -- a
    // `&[CellFacing]` indexed by `g` -- not one facing for the whole call. A
    // single `CellFacing` parameter would type-check and be wrong the first
    // time two merges face different ways.
    //
    // Nor is there a slice to pass today, but the four callers do not divide
    // as neatly as "only `emit` knows". `emit` has its own binding.
    // `resolve_directed_dust_terminals` runs inside `compile` against `emit`'s
    // baseline world, before any `CompiledCircuit` -- and so any
    // `gate_facings` -- exists. `verify_connectivity` and
    // `verify_signal_strength` each run from two places: that same point
    // inside `compile`, and `verify_realised_world`, which the planner calls
    // on a world it realised itself. Only that second path has facings
    // anywhere near it -- `planner::realise_and_verify`'s own candidate, one
    // frame up, answering `facing_of` per node -- so threading this means
    // giving *both* paths a slice, not just the planner's, or the same
    // invariant would be checked against two different ideas of the geometry.
    // When `emit` starts turning gates, the per-gate choice it makes is what
    // has to arrive here, threaded from `emit` through all four.
    let facing = geometry::CellFacing::NORTH;

    let mut owners = HashMap::new();
    for (g, gate) in netlist.gates.iter().enumerate() {
        if !gate.is_merge() {
            continue;
        }
        let Some(root) = merge_output_group_root(netlist, g, &index_of_signal, &groups) else {
            continue;
        };
        let Some(&(jx, jy, jz)) = gate_output_positions.get(&gate.output) else {
            continue;
        };
        let junction = Position::new(jx, jy, jz);
        owners.insert(junction, root);
        owners.insert(junction.offset(geometry::output_direction(facing)), root);
    }
    owners
}

/// The connectivity invariant: every dust network the finished world
/// actually contains must belong to exactly one net -- or, when the
/// netlist declares a merge (`MergeGroups`), to one *declared group* of
/// nets, which a legitimate wire-merge OR's several sources are.
///
/// This does not know anything about tracks, columns or ramps -- it only
/// knows what `dust_connections` says is physically joined (the same rule
/// the simulator itself walks) and what `reservation` says every cell was
/// *for*, and it fails the moment those two disagree without the netlist
/// having asked for it. That independence is the point: it catches a
/// routing bug regardless of which pass caused it, including ones this
/// module's own keep-out logic has never heard of -- and it catches it
/// exactly as before for every netlist that declares no merge at all,
/// since `MergeGroups` gives every net its own singleton group in that
/// case.
///
/// `gate_output_positions` feeds [`merge_gate_body_owners`], the fallback
/// this now checks whenever `reservation` itself has nothing for a cell --
/// exactly the case for a merge's own junction and outbound pin (see that
/// function's own doc comment). Every hand-built test below that declares
/// no merge passes an empty map, which makes the fallback a no-op and this
/// exactly the check it always was.
///
/// # What it does **not** see, and where that is measured
///
/// `dust_connections` is one edge type out of four. `docs/derived/coupling-
/// mechanisms.md` measured the others by running the simulator: a component
/// drives adjacent dust; a component strongly powers a conductive block and
/// that block re-drives dust on every face it has left; and a block powered
/// even *weakly* is read by a torch attached to it or by a diode whose rear it
/// is. No dust-to-dust edge exists in any of those, so none of them can appear
/// in this walk -- which is structural, not an implementation slip. Both bugs
/// that shipped on this branch were the second of them.
///
/// The module `compile::coupling` (`#[cfg(test)]`) keeps this function's
/// granularity and widens the relation to all four, and
/// `docs/derived/realised-graph-extras.md` is what it finds on every circuit
/// this project builds, on both compile paths. Three tests in this file's own
/// test module record what this walk cannot see, and are written so that
/// closing a gap turns its test red.
fn verify_connectivity(
    world: &World,
    reservation: &Reservation,
    netlist: &Netlist,
    nets: &[Net],
    gate_output_positions: &BTreeMap<String, (i32, i32, i32)>,
) -> Result<(), CompileError> {
    let groups = MergeGroups::build(netlist, nets);
    let gate_body_owners = merge_gate_body_owners(netlist, nets, gate_output_positions);
    let owner_of = |pos: &Position| {
        reservation
            .get(pos)
            .or_else(|| gate_body_owners.get(pos))
            .copied()
    };
    let mut visited: HashSet<Position> = HashSet::new();

    for flat in world.positions_of(BlockKind::RedstoneWire) {
        let (x, y, z) = world.decode(flat);
        let start = Position::new(x, y, z);
        if !visited.insert(start) {
            continue;
        }

        let mut owner: Option<(usize, Position)> = owner_of(&start).map(|net| (net, start));
        let mut queue: VecDeque<Position> = VecDeque::new();
        queue.push_back(start);

        while let Some(pos) = queue.pop_front() {
            for direction in [Facing::North, Facing::South, Facing::East, Facing::West] {
                for next in dust_connections(world, pos, direction).iter() {
                    if !visited.insert(next) {
                        continue;
                    }
                    queue.push_back(next);

                    if let Some(found_net) = owner_of(&next) {
                        match owner {
                            None => owner = Some((found_net, next)),
                            Some((expected_net, expected_cell))
                                if expected_net != found_net
                                    && !groups.same_group(expected_net, found_net) =>
                            {
                                return Err(CompileError::ConnectivityViolation {
                                    cell: (next.x, next.y, next.z),
                                    found_net: net_name(netlist, nets, found_net),
                                    expected_cell: (
                                        expected_cell.x,
                                        expected_cell.y,
                                        expected_cell.z,
                                    ),
                                    expected_net: net_name(netlist, nets, expected_net),
                                });
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------
// The torch-merge invariant
// ---------------------------------------------------------------------
//
// `verify_connectivity` proves a net reaches exactly the cells it should.
// It says nothing about *function*: it never looks at a torch, so it
// cannot tell a working NOR from a torch standing on the wrong block, or
// an input wired straight into the torch instead of its support. This is
// the missing half -- see `docs/superpowers/specs/2026-08-08-3d-codesign.md`,
// "What this costs, honestly".
//
// Every condition below is derived from the simulator's own rules
// (`taxonomy.rs`, `component.rs`, `propagate.rs`), not from the shape
// `place_nor_gate` happens to produce -- so it accepts any geometry that
// implements a NOR, not just this module's own template.

/// Whether `state`, if it were actually driven, would deliver power toward
/// `direction` -- `(drives_dust, block_power)`, the same two independent
/// axes `taxonomy::PowerOutput` tracks, minus strength (this invariant only
/// ever asks "does *any* power arrive", never "how much").
///
/// This mirrors `taxonomy::power_emitted_by`/`power_emitted_toward`'s
/// per-kind directionality exactly, arm for arm, but drops every
/// activation gate (`state.lit`, `state.power > 0`, a comparator's stored
/// strength). That is deliberate, not an oversight: `verify_torch_merge`
/// runs on a freshly emitted, not-yet-settled world, where every active
/// component is hardcoded to a placeholder activation value --
/// `place_nor_gate`'s `wall_torch`/`repeater` helpers construct their
/// blocks pre-lit unconditionally, and dust is placed with `power == 0`
/// because nothing has run `recompute_dust_strengths` yet (`compile` never
/// calls it -- that is the simulator's job, once a `Simulator` is actually
/// constructed over the result). Asking "is it *currently* lit" would
/// therefore answer nothing about whether the *geometry* works once the
/// circuit is actually driven; what is left, once the activation gates are
/// dropped, is a purely geometric fact -- which directions a component of
/// this kind, in this orientation, is even *capable* of reaching.
fn structural_output(state: &BlockState, direction: Facing) -> (bool, BlockPower) {
    match state.kind {
        // Repeater/comparator: forward only (`power_emitted_toward`'s
        // `Repeater | Comparator` arm), strong when active.
        BlockKind::Repeater | BlockKind::Comparator => {
            let active = state.facing == Some(direction.opposite());
            (
                active,
                if active {
                    BlockPower::Strong
                } else {
                    BlockPower::None
                },
            )
        }
        // Standing torch: strong straight up, weak to the four sides,
        // inert straight down -- its own support
        // (`power_emitted_toward`'s `Torch` arm; the withheld direction
        // matches `component::torch_support_position`'s `Down` case).
        BlockKind::Torch => match direction {
            Facing::Down => (false, BlockPower::None),
            Facing::Up => (true, BlockPower::Strong),
            _ => (true, BlockPower::Weak),
        },
        // Wall torch: same shape, but the withheld direction is
        // `facing.opposite()` -- the wall it hangs on -- not a fixed
        // `Down`. Missing `facing` cannot be reasoned about at all,
        // matching `torch_support_position`'s own fail-safe `None`.
        BlockKind::WallTorch => match state.facing {
            None => (false, BlockPower::None),
            Some(facing) => {
                let attached_to = facing.opposite();
                if direction == attached_to {
                    (false, BlockPower::None)
                } else if direction == Facing::Up {
                    (true, BlockPower::Strong)
                } else {
                    (true, BlockPower::Weak)
                }
            }
        },
        // Redstone wire only ever weakly powers the block directly
        // beneath it (`power_emitted_toward`'s `RedstoneWire` arm) --
        // wire-to-wire connectivity is handled separately, by
        // `dust_connections`, not by this function.
        BlockKind::RedstoneWire => {
            if direction == Facing::Down {
                (true, BlockPower::Weak)
            } else {
                (false, BlockPower::None)
            }
        }
        // Lever, button, pressure plate, observer: isotropic and strong in
        // this model (`power_emitted_toward`'s `_ => full` fallback --
        // these components' directionality is not yet modelled).
        BlockKind::Lever | BlockKind::Button | BlockKind::PressurePlate | BlockKind::Observer => {
            (true, BlockPower::Strong)
        }
        // A redstone block drives adjacent dust but powers no block at all
        // (`power_emitted_by`'s `RedstoneBlock` arm) -- same for target,
        // daylight detector and weighted pressure plate.
        BlockKind::RedstoneBlock
        | BlockKind::Target
        | BlockKind::DaylightDetector
        | BlockKind::WeightedPressurePlate => (true, BlockPower::None),
        _ => (false, BlockPower::None),
    }
}

/// [`structural_output`] with the one half of it that depends on the *world*
/// filled in: which blocks a dust cell powers.
///
/// `structural_output` reads a `BlockState` and nothing else, so for
/// `RedstoneWire` it can only answer the vertical case -- which blocks a dust
/// cell powers horizontally depends on its connection shape, and that is a fact
/// about the world. `connectivity::dust_powers_block_toward` is the measured
/// rule (conformance category `dust-directionality`) and answers it in a
/// geometry-only form, so asking it here keeps both walks' refusal to trust a
/// freshly emitted world's placeholder `power` fields intact.
///
/// **Shared by [`net_reach`] and [`net_signal_strength`] on purpose.** Those two
/// walks had a copy of this each, and the copies disagreed: `net_reach`'s
/// covered `Down` -- the block a dust cell *stands on* -- and the strength
/// walk's looped `HORIZONTAL` only, so a run ending on top of a gate's support
/// was invisible to one of the two. One statement of the rule is what stops
/// that happening again; see `net_signal_strength`'s own doc comment for what
/// the divergence cost.
fn structural_output_in_world(
    world: &World,
    pos: Position,
    state: &BlockState,
    direction: Facing,
) -> (bool, BlockPower) {
    let (drives_dust, block_power) = structural_output(state, direction);
    if state.kind != BlockKind::RedstoneWire {
        return (drives_dust, block_power);
    }
    let powers_block = if dust_powers_block_toward(world, pos, direction) {
        BlockPower::Weak
    } else {
        BlockPower::None
    };
    (drives_dust, powers_block)
}

/// Add `pos` to the propagation frontier if it is not already in it.
fn enqueue(pos: Position, in_network: &mut HashSet<Position>, queue: &mut VecDeque<Position>) {
    if in_network.insert(pos) {
        queue.push_back(pos);
    }
}

/// Record that `pos` receives `power` (if any), honouring the same
/// conductivity gate `propagate::block_signal_at` enforces before it will
/// ever report a block as powered -- a non-conductive block (glass, a
/// fence, a honey block) never becomes "powered" in this model no matter
/// what touches it, and that is a real, load-bearing fact: it is exactly
/// what makes a torch standing on glass never invert (see
/// `TorchMergeFailure::SupportNotConductive`).
///
/// A block that ends up `Strong`ly powered can re-drive every redstone
/// wire adjacent to it, in all six directions -- not just the one
/// direction whatever powered it happens to face
/// (`BlockPower::can_repower_dust`) -- so those wires are queued too. A
/// `Weak`ly powered block is recorded (enough to satisfy "this net reaches
/// it") but propagates no further, matching `block_power_at`'s own
/// asymmetry between the two.
fn mark_powered(
    world: &World,
    powered: &mut HashSet<Position>,
    in_network: &mut HashSet<Position>,
    queue: &mut VecDeque<Position>,
    pos: Position,
    power: BlockPower,
) {
    if power == BlockPower::None {
        return;
    }
    if !flags_of(world.get(pos.x, pos.y, pos.z)).is_conductive() {
        return;
    }
    powered.insert(pos);
    if power == BlockPower::Strong {
        for direction in ALL_SIX {
            let wire = pos.offset(direction);
            if world.get(wire.x, wire.y, wire.z).kind == BlockKind::RedstoneWire {
                enqueue(wire, in_network, queue);
            }
        }
    }
}

/// Every block position that would receive *any* power (weak or strong) if
/// `cells` -- one net's own conductor path, as claimed in `reservation` --
/// were actually driven at its source. Computed structurally, walking the
/// same wire-connectivity (`dust_connections`) and block-power rules
/// (`structural_output`, `mark_powered`'s conductivity gate) the simulator
/// itself uses, but without trusting the freshly emitted world's
/// placeholder `lit`/`power` fields (see `structural_output`'s doc comment
/// for why).
///
/// This is `verify_torch_merge`'s core query: comparing this set, for
/// every net, against a gate's support block is "does every declared input
/// reach it" and "does nothing else" asked in the same breath -- a net
/// belongs in the answer if and only if it is one of the gate's own
/// inputs.
fn net_reach(world: &World, cells: &[Position]) -> HashSet<Position> {
    let mut in_network: HashSet<Position> = HashSet::new();
    let mut queue: VecDeque<Position> = VecDeque::new();
    let mut powered: HashSet<Position> = HashSet::new();

    for &pos in cells {
        enqueue(pos, &mut in_network, &mut queue);
    }

    while let Some(pos) = queue.pop_front() {
        let state = world.get(pos.x, pos.y, pos.z);

        if state.kind == BlockKind::RedstoneWire {
            // Wire-to-wire: the whole connected network is one electrical
            // node, so every cell `dust_connections` says joins it
            // (same-layer, climb, descend) becomes reachable too.
            for direction in HORIZONTAL {
                for neighbour in dust_connections(world, pos, direction).iter() {
                    enqueue(neighbour, &mut in_network, &mut queue);
                }
            }
        }

        for direction in ALL_SIX {
            // `structural_output_in_world` is `structural_output` with the
            // half of the dust rule that depends on the world already filled
            // in -- see its own doc comment, and note that
            // `net_signal_strength` asks the same function for the same
            // reason.
            //
            // This is what makes `ForeignNetReachesSupport` able to see a
            // foreign run that ends against a gate's support block. Nothing
            // else in the compiler models that adjacency: `dust_reach` and
            // `verify_connectivity` are both strictly dust-reaches-dust.
            let (drives_dust, block_power) =
                structural_output_in_world(world, pos, state, direction);
            let neighbour = pos.offset(direction);
            if drives_dust
                && world.get(neighbour.x, neighbour.y, neighbour.z).kind == BlockKind::RedstoneWire
            {
                enqueue(neighbour, &mut in_network, &mut queue);
            }
            mark_powered(
                world,
                &mut powered,
                &mut in_network,
                &mut queue,
                neighbour,
                block_power,
            );
        }
    }

    powered
}

/// The torch-merge invariant: every gate's output torch must genuinely
/// implement a NOR of exactly its declared inputs.
///
/// Precisely, for every gate:
///
/// 1. Its output position resolves to a torch with a support block
///    (`TorchMergeFailure::NoSupport` otherwise).
/// 2. That support block is conductive, so it can ever be observed as
///    powered at all (`SupportNotConductive` otherwise -- see
///    `propagate::block_signal_at`'s own conductivity gate).
/// 3. `net_reach` says the support block is reached by exactly the nets
///    feeding this gate's declared inputs -- no fewer
///    (`InputDoesNotReachSupport`) and no more
///    (`ForeignNetReachesSupport`). `verify_connectivity` already
///    guarantees each *routed* net's own dust stays a single, uncontaminated
///    network; what it does not check is whether that network actually
///    lands on *this* torch's support rather than merely near it, or
///    whether it lands on some *other* gate's support too -- that is
///    exactly what condition 3 adds.
/// 4. No conductor the torch itself directly powers -- any of its six
///    neighbours other than its own support -- belongs to a net other than
///    the one this gate's output actually drives
///    (`OutputLeaksIntoForeignNet`). A torch does not power the block it
///    is attached to; that asymmetry is what makes it invert, and it is
///    exactly the fact a freely-planned geometry could get wrong by
///    routing some other net's conductor onto one of the torch's other
///    five faces.
///
/// Nothing here duplicates `verify_connectivity`'s own check (it never
/// looks at a torch or a support block at all), but conditions 3 and 4
/// both lean on it having already run and passed: they trust that a net's
/// own claimed cells form one clean network, so `net_reach` only has to
/// ask where that network's power actually ends up, not re-derive that it
/// is one network in the first place.
///
/// A `Gate` with `is_merge` set has no torch and no support at all -- see
/// that field's doc comment -- so none of the four conditions above apply
/// to it, and it is skipped outright. Condition 3, for every *other* gate,
/// is widened by exactly the same `MergeGroups` `verify_connectivity` uses:
/// a declared input's whole merge group counts as declared, so a merge's
/// several branches are recognised as the one declared input they
/// electrically are, not flagged as foreign nets corrupting the gate that
/// actually consumes the merge's output. Nothing else changes -- a net
/// outside every declared input's group is still exactly as foreign as it
/// always was.
fn verify_torch_merge(
    world: &World,
    reservation: &Reservation,
    netlist: &Netlist,
    nets: &[Net],
    gate_output_positions: &BTreeMap<String, (i32, i32, i32)>,
) -> Result<(), CompileError> {
    let groups = MergeGroups::build(netlist, nets);

    // (gate, input_index) -> the net index that drives that input.
    let mut input_net: HashMap<(usize, usize), usize> = HashMap::new();
    // gate -> the net index this gate's own output feeds, if it feeds any
    // other gate at all. A gate whose output only reaches a circuit output
    // lamp has no `Net` of its own (`build_nets` drops nets with no
    // gate-input sinks) -- nothing claims its pin, so nothing can leak
    // into it either, and `None` here means exactly that.
    let mut output_net: HashMap<usize, usize> = HashMap::new();
    for (n, net) in nets.iter().enumerate() {
        if let Source::Gate(g) = net.source {
            output_net.insert(g, n);
        }
        for sinks in &net.sinks {
            for &(gate, input_index) in sinks {
                input_net.insert((gate, input_index), n);
            }
        }
    }

    let mut net_cells: Vec<Vec<Position>> = vec![Vec::new(); nets.len()];
    for (&pos, &owner) in reservation {
        net_cells[owner].push(pos);
    }

    // Reach is computed per *declared merge group*, not per raw net index.
    // `MergeGroups` says a declared merge's several nets are electrically
    // one, but `Reservation` still maps one physical cell to exactly one
    // raw index -- so only one of a merge's several branches can ever
    // literally own the mandatory repeater that terminates the join into a
    // downstream support (this module's own routing rule: dust never
    // charges a block sideways, only a repeater does). Asking `net_reach`
    // to prove each raw index reaches a support *from its own cells alone*
    // would therefore fail for every branch but the one that happens to own
    // that repeater, even though the whole group genuinely reaches it.
    // Grouping cells first is what makes the check match the physical fact
    // `MergeGroups` already declares -- and it reproduces today's exact
    // per-net reach whenever nothing is merged, since every net is then its
    // own singleton group and this is just `net_reach(net_cells[n])` again.
    let mut group_cells: HashMap<usize, Vec<Position>> = HashMap::new();
    for (n, cells) in net_cells.iter().enumerate() {
        group_cells
            .entry(groups.root(n))
            .or_default()
            .extend(cells.iter().copied());
    }
    let group_reach: HashMap<usize, HashSet<Position>> = group_cells
        .iter()
        .map(|(&root, cells)| (root, net_reach(world, cells)))
        .collect();
    let reach: Vec<HashSet<Position>> = (0..nets.len())
        .map(|n| {
            group_reach
                .get(&groups.root(n))
                .cloned()
                .unwrap_or_default()
        })
        .collect();

    for (g, gate) in netlist.gates.iter().enumerate() {
        if gate.is_merge() {
            // A declared merge is a bare wire join: no torch, no support,
            // nothing gate-shaped to check here at all. Whether the join
            // is legitimate -- no foreign net touching it -- is exactly
            // `verify_connectivity`'s job, generalised by the same
            // `MergeGroups` this function consults below for the gates
            // that actually consume a merge's output.
            continue;
        }

        let &(tx, ty, tz) = gate_output_positions
            .get(&gate.output)
            .expect("emit records a torch position for every gate");
        let torch_pos = Position::new(tx, ty, tz);
        let torch_state = world.get(tx, ty, tz);

        let Some(support) = torch_support_position(torch_state, torch_pos) else {
            return Err(CompileError::TorchMergeViolation {
                gate: gate.name.clone(),
                reason: TorchMergeFailure::NoSupport {
                    torch: (tx, ty, tz),
                },
            });
        };
        let support_tuple = (support.x, support.y, support.z);

        if !flags_of(world.get(support.x, support.y, support.z)).is_conductive() {
            return Err(CompileError::TorchMergeViolation {
                gate: gate.name.clone(),
                reason: TorchMergeFailure::SupportNotConductive {
                    torch: (tx, ty, tz),
                    support: support_tuple,
                },
            });
        }

        // Each declared input's whole merge group, not just its own net
        // index -- a wire-merge OR's several branches are, electrically,
        // the one declared input they together feed (see `MergeGroups`),
        // so any of them structurally reaching the support is exactly as
        // legitimate as the input net itself reaching it.
        let declared: HashSet<usize> = (0..gate.inputs.len())
            .map(|input_index| {
                groups.root(
                    *input_net
                        .get(&(g, input_index))
                        .expect("every gate input was assigned a net by build_nets"),
                )
            })
            .collect();

        for (n, reached) in reach.iter().enumerate() {
            match (
                declared.contains(&groups.root(n)),
                reached.contains(&support),
            ) {
                (true, false) => {
                    return Err(CompileError::TorchMergeViolation {
                        gate: gate.name.clone(),
                        reason: TorchMergeFailure::InputDoesNotReachSupport {
                            torch: (tx, ty, tz),
                            support: support_tuple,
                            input: net_name(netlist, nets, n),
                        },
                    });
                }
                (false, true) => {
                    return Err(CompileError::TorchMergeViolation {
                        gate: gate.name.clone(),
                        reason: TorchMergeFailure::ForeignNetReachesSupport {
                            torch: (tx, ty, tz),
                            support: support_tuple,
                            net: net_name(netlist, nets, n),
                        },
                    });
                }
                (true, true) | (false, false) => {}
            }
        }

        let legitimate_output = output_net.get(&g).copied();
        for direction in ALL_SIX {
            let neighbour = torch_pos.offset(direction);
            if neighbour == support {
                continue;
            }
            if world.get(neighbour.x, neighbour.y, neighbour.z).kind != BlockKind::RedstoneWire {
                continue;
            }
            if let Some(&owner) = reservation.get(&neighbour) {
                if Some(owner) != legitimate_output {
                    return Err(CompileError::TorchMergeViolation {
                        gate: gate.name.clone(),
                        reason: TorchMergeFailure::OutputLeaksIntoForeignNet {
                            torch: (tx, ty, tz),
                            leaked_cell: (neighbour.x, neighbour.y, neighbour.z),
                            net: net_name(netlist, nets, owner),
                        },
                    });
                }
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------
// The signal-strength invariant
// ---------------------------------------------------------------------
//
// Spacing proves nothing touches that shouldn't. `verify_connectivity`
// proves every dust network partitions into exactly the right nets.
// `verify_torch_merge` proves every torch genuinely inverts the nets that
// are structurally wired to its support. None of the three knows that
// redstone dust starts at 15 and loses one per step: a run one block too
// long, or a repeater whose output lands on a cell nothing continues from,
// leaves every one of those three invariants perfectly satisfied and the
// circuit silently wrong -- right on the input vectors that never need the
// far end of that run, wrong on the one that does.
//
// This is the fourth invariant: every net must deliver a non-zero signal to
// every one of its own declared sinks. Unlike the first three, it cannot be
// answered by "what touches what" -- it needs actual decay, so it derives
// its propagation from the same two places the simulator itself does,
// rather than restating them:
//
// - `connectivity::dust_connections` for which cells join, and in which
//   direction climbing/descending is even legal -- exactly what
//   `propagate::recompute_dust_strengths`'s own BFS walks.
// - The fact (`taxonomy::power_emitted_by`'s `Repeater` arm) that a
//   repeater's output is always exactly `MAX_SIGNAL_STRENGTH`, never a
//   partial value, when its input carries any non-zero signal at all -- so
//   a repeater is a step function on its own input, not a pass-through.
//
// What it must *not* do is ask the planner (`plan_straight_run`,
// `plan_track_run`, `ramp_reserve`, and friends) what strength it *meant* to
// deliver -- that would only prove the planner agrees with itself. It has
// to walk the emitted blocks, the same way `verify_connectivity` walks the
// world rather than trusting the router's intentions.

/// The real, decayed signal strength this net's own routed cells would
/// carry if its source were genuinely driven -- computed by walking
/// `own_cells` (this net's own reservation, so the walk can never wander
/// into a different net's dust even by accident) exactly the way
/// `propagate::recompute_dust_strengths` walks the whole world: one hop of
/// decay per `dust_connections` edge, with a repeater restoring to
/// `MAX_SIGNAL_STRENGTH` wherever its own declared input cell is found, by
/// this same walk, to carry a non-zero signal.
///
/// That last clause is the entire difference from `net_reach`
/// (`verify_torch_merge`'s tool): `net_reach` and `structural_output` ask
/// "could this component possibly reach that cell", assuming every active
/// component is already lit -- which is deliberately right for a freshly
/// emitted, not-yet-settled world when the question is "does this geometry
/// implement a NOR at all". This function asks a different question --
/// "given the actual dust run behind it, does this component ever receive
/// enough to fire" -- so a repeater here only restores the signal if the
/// cell that is genuinely, structurally its input (`structural_output`'s own
/// direction test, reused unchanged) was itself reached by this same walk
/// with a non-zero strength. A repeater whose own input never arrives stays
/// silent, and nothing downstream of it is ever recorded -- which is exactly
/// how a dust run one block too long, or a repeater refreshing into a cell
/// nothing continues from, ends up with an unreached sink instead of a
/// falsely "reachable" one.
///
/// `sources` are the group's true origins -- every lever or non-merge
/// gate's own output torch that genuinely drives something in `own_cells`,
/// read directly from the world, not from `own_cells` itself: unlike
/// `net_reach`, which can start from every reserved cell at once because it
/// never has to ask "how far", this function has to know exactly where the
/// signal actually starts so its decay count means anything. For an
/// ordinary (non-merge) net this is always exactly one origin -- its own
/// source -- so this generalises rather than changes that case. For a
/// declared wire merge's whole group (see `verify_signal_strength`), it is
/// every branch's own true origin seeded into *one* shared walk: `deliver`
/// below only ever keeps the larger of two arrivals at the same cell, so
/// seeding several origins at once computes exactly the max-of-sources a
/// real merge's dust does, not several independent answers that would need
/// combining afterwards. Each origin is assumed driven at
/// `MAX_SIGNAL_STRENGTH` regardless of its real, placeholder `lit`/`power`
/// value in the freshly emitted world -- the same assumption
/// `structural_output` documents for the same reason (nothing about a
/// fresh world's activation fields means anything yet). Only what happens
/// to that assumed-driven signal *after* it leaves its source is real
/// physics, never assumed.
///
/// The walk's **step relation is `net_reach`'s**, with a strength carried
/// along it. That is not a coincidence and it is not a restatement: the two
/// functions are asking different questions about the same physics, so the
/// only defensible arrangement is that they take the same steps and differ in
/// what they record. They did not, and the divergence is what
/// `planner::the_strength_verifier_follows_a_repeater_that_feeds_a_climb`
/// records -- see "The block-mediated step" below.
///
/// Applied by `pos`'s own kind once it is popped:
///
/// - **Dust** decays by one per `dust_connections` edge (same-layer, climb,
///   descend -- whichever the geometry actually allows), *and* separately
///   drives any horizontally adjacent repeater directly, since
///   `dust_connections` only ever returns other dust cells and a repeater is
///   never reached through it, *and* weakly powers the blocks
///   `structural_output_in_world` says it powers -- which includes the block
///   it **stands on**, `dust_powers_block_toward`'s always-true `Down` case.
/// - **A conductive block** this walk has already found *strongly* powered
///   re-drives the redstone wire touching any of its six faces, at the
///   block's own strength (`coupling-mechanisms.md` mechanism 3; the
///   simulator's own `recompute_dust_strengths` seeds a wire from a
///   neighbouring `BlockPower::Strong` block without decay).
/// - **A repeater** (the only kind this router ever places mid-route --
///   `lay_dust_run`, `lay_bent_path` and `move_between_layers`'s rest stops
///   all call `repeater(..)`, never a comparator or a torch) drives forward
///   from its own position along its own single fixed output direction,
///   whatever it lands on -- more dust, a gate's support block, or, in a
///   back-to-back placement, directly into another repeater's own input
///   side. This is what makes a chain of repeaters work regardless of
///   length: each one, once *it* is found to have a real signal, forwards
///   from itself exactly the way the first one did, rather than the first
///   repeater trying to reach all the way to the final destination in one
///   jump.
///
/// Either way, `deliver` is the single point that decides whether the
/// receiving cell can actually accept what is being sent its way, so a
/// repeater's own directionality is enforced identically regardless of
/// which of the three rules produced the delivery.
///
/// # The block-mediated step, and the defect that was in it
///
/// A signal leaves a conductor for a *block* twice in the physics, and the
/// two are not the same edge (`docs/derived/coupling-mechanisms.md`):
///
/// * **Strong** -- a component's output face landing on a conductive block.
///   That block then re-drives dust on **all six** of its faces (mechanism 3).
///   Every ramp this router builds is made of this: `realise_branch_from` puts
///   a mandatory refresh on the last flat cell before a climb, so the repeater
///   outputs into the *floor* of the climbing cell and the floor is what
///   lights the dust standing on it.
/// * **Weak** -- a dust run's own end, or the block a dust cell stands on.
///   That block powers a torch and feeds a diode's rear, and drives **no**
///   dust at all (mechanism 5 does not exist -- `recompute_dust_strengths`
///   seeds a wire from a block only on `BlockPower::Strong`).
///
/// So whether the walk may continue *from* a cell is decided by what stands
/// there and what class of power arrived, and by nothing else:
///
/// | target | continue? |
/// |---|---|
/// | dust, repeater, comparator | only inside `own_cells` |
/// | conductive block, strong power | yes |
/// | anything else | no -- recorded, never walked |
///
/// `own_cells` gates **conductors only**, which is the one thing it was ever
/// for: it stops this walk crossing into a different net's network where the
/// two happen to physically touch. It was applied to every delivery instead,
/// and a route reserves *anchors* -- a route anchor holds dust or a repeater,
/// never a block, and a route's floor is not an anchor at all. So no
/// conductive block was ever in `own_cells`, the strong arm below could never
/// fire, and every cell past a ramp read zero: measured on negotiated
/// `full_adder`, repeater `(55, 1, 108)` drives the floor `(55, 1, 107)`, the
/// dust standing on it at `(55, 2, 107)` reads 15 with `cin`'s lever the only
/// component emitting in the whole world, and the walk read 0 -- and so did
/// the 24 cells behind it, including the support `(57, 1, 91)` the refusal
/// named. Eight of eight vectors of that plan are a full adder in the real
/// `Simulator`.
///
/// What this does **not** widen: weak block power still propagates nowhere, a
/// conductor outside `own_cells` is still never walked from, and a repeater
/// still only fires when its own declared input cell was itself reached with a
/// non-zero strength. `strength_differential` measures all three.
fn net_signal_strength(
    world: &World,
    own_cells: &HashSet<Position>,
    sources: &[(Position, &BlockState)],
) -> HashMap<Position, u8> {
    let mut strength: HashMap<Position, u8> = HashMap::new();
    let mut queue: VecDeque<Position> = VecDeque::new();
    let mut ever_queued: HashSet<Position> = HashSet::new();

    /// Record `value` arriving at `target` from `from_direction`, if `target`
    /// is capable of receiving it at all, and enqueue `target` if the walk may
    /// continue from it.
    ///
    /// `drives_dust` and `block_power` are the two independent channels
    /// `taxonomy::PowerOutput` tracks and `structural_output_in_world` returns,
    /// kept apart here because **the receiver decides which of them applies**
    /// (`coupling-mechanisms.md` Table 5 is that fact, measured):
    ///
    /// | target | receives |
    /// |---|---|
    /// | dust, or a diode reached on its own input side | `drives_dust` only |
    /// | a **conductive** block | `block_power` only |
    /// | air, glass, anything non-conductive | nothing |
    ///
    /// That second row is `mark_powered`'s own conductivity gate, which is
    /// `propagate::block_signal_at`'s, and the third is why a repeater
    /// refreshing into thin air -- the descend rule's deliberately-open cell --
    /// records nothing and radiates nothing, which is exactly the failure mode
    /// this invariant exists to catch.
    ///
    /// A repeater or comparator only ever reads its one declared input side
    /// (`facing`) -- a signal arriving from any other side is exactly as inert
    /// as no signal at all, the same asymmetry `power_emitted_toward`'s own
    /// direction gate enforces for output.
    ///
    /// Whether the walk may continue **from** `target` is a second, separate
    /// question; see this function's own doc comment for the table and for
    /// what deciding it by `own_cells` alone cost.
    #[allow(clippy::too_many_arguments)]
    fn deliver(
        world: &World,
        own_cells: &HashSet<Position>,
        strength: &mut HashMap<Position, u8>,
        queue: &mut VecDeque<Position>,
        ever_queued: &mut HashSet<Position>,
        target: Position,
        from_direction: Facing,
        value: u8,
        drives_dust: bool,
        block_power: BlockPower,
    ) {
        let target_state = world.get(target.x, target.y, target.z);
        let conductor = matches!(
            target_state.kind,
            BlockKind::RedstoneWire | BlockKind::Repeater | BlockKind::Comparator
        );
        if matches!(
            target_state.kind,
            BlockKind::Repeater | BlockKind::Comparator
        ) && target_state.facing != Some(from_direction.opposite())
        {
            return;
        }

        let accepts = if conductor {
            drives_dust
        } else {
            block_power != BlockPower::None && flags_of(target_state).is_conductive()
        };
        if !accepts {
            return;
        }

        let onward = if conductor {
            // `own_cells` gates conductors, and nothing else. It is what stops
            // this walk crossing into a different net's network where the two
            // happen to physically touch.
            own_cells.contains(&target)
        } else {
            // A block re-drives dust exactly when the power that arrived is
            // strong -- `mark_powered`'s own `BlockPower::Strong` arm, and
            // `recompute_dust_strengths`' own seed condition. Weak power is
            // recorded (a torch support reads it, and that is what makes a
            // support a sink here) and walked from never.
            block_power == BlockPower::Strong
        };

        let improved = value > strength.get(&target).copied().unwrap_or(0);
        if improved {
            strength.insert(target, value);
        }
        if onward {
            // `improved` alone is not enough: a block can be recorded first by
            // a dust run's *weak* end at some value and reached afterwards by a
            // repeater's *strong* output at the same value, and only the second
            // of those may be walked from. `ever_queued` is what stops the
            // first arrival shadowing the second.
            let first = ever_queued.insert(target);
            if improved || first {
                queue.push_back(target);
            }
        }
    }

    for &(source, source_state) in sources {
        for direction in ALL_SIX {
            let (drives_dust, block_power) =
                structural_output_in_world(world, source, source_state, direction);
            if !drives_dust && block_power == BlockPower::None {
                continue;
            }
            deliver(
                world,
                own_cells,
                &mut strength,
                &mut queue,
                &mut ever_queued,
                source.offset(direction),
                direction,
                MAX_SIGNAL_STRENGTH,
                drives_dust,
                block_power,
            );
        }
    }

    while let Some(pos) = queue.pop_front() {
        let here = *strength
            .get(&pos)
            .expect("only positions already given a strength are ever queued");
        let state = world.get(pos.x, pos.y, pos.z);

        if state.kind == BlockKind::RedstoneWire {
            // Dust-to-dust decay, restricted (via `deliver`'s `own_cells`
            // gate) to this net's own reservation so the walk can never
            // cross into a different net's network even if the two happened
            // to physically touch (a bug `verify_connectivity` would already
            // have caught before this ever runs). Stops at `here == 1`:
            // decaying further would only ever produce a 0, which is the
            // same as never having recorded the cell at all.
            if here > 1 {
                let next = here - 1;
                for direction in HORIZONTAL {
                    for neighbour in dust_connections(world, pos, direction).iter() {
                        deliver(
                            world,
                            own_cells,
                            &mut strength,
                            &mut queue,
                            &mut ever_queued,
                            neighbour,
                            direction,
                            next,
                            true,
                            BlockPower::None,
                        );
                    }
                }
            }
            // Dust directly driving an adjacent repeater. This is *not* a
            // `dust_connections` edge (that only ever returns other dust
            // cells), and it fires on any non-zero `here` including 1 -- a
            // repeater only needs some signal to arrive, not two hops' worth
            // of it. `deliver`'s own facing check is what confirms `pos` is
            // genuinely this repeater's declared input side, not merely
            // adjacent to it.
            for direction in HORIZONTAL {
                let neighbour = pos.offset(direction);
                if world.get(neighbour.x, neighbour.y, neighbour.z).kind == BlockKind::Repeater {
                    deliver(
                        world,
                        own_cells,
                        &mut strength,
                        &mut queue,
                        &mut ever_queued,
                        neighbour,
                        direction,
                        MAX_SIGNAL_STRENGTH,
                        true,
                        BlockPower::None,
                    );
                }
            }
            // A dust run also *weakly* powers the blocks
            // `dust_powers_block_toward` names -- both ends of a straight
            // run's own axis, and, in every shape whatever, the block it
            // stands on. Weak power drives no dust (mechanism 5 does not
            // exist), so `deliver`'s own rule records these and walks from
            // none of them; that is exactly what makes a gate support a
            // *sink* here. `ALL_SIX` and not `HORIZONTAL`: the `Down` case is
            // a real coupling -- a torch whose support a route happens to
            // stand on goes out -- and looping horizontally was the second
            // way this walk and `net_reach` had drifted apart.
            for direction in ALL_SIX {
                let (_, block_power) = structural_output_in_world(world, pos, state, direction);
                if block_power == BlockPower::None {
                    continue;
                }
                deliver(
                    world,
                    own_cells,
                    &mut strength,
                    &mut queue,
                    &mut ever_queued,
                    pos.offset(direction),
                    direction,
                    here,
                    // Never `structural_output`'s own `drives_dust` for dust:
                    // that arm reports `Down`, and dust-to-dust is
                    // `dust_connections`' job, with a decay step this delivery
                    // does not carry. Handing it on here wrote a run's own
                    // undecayed strength into the next cell of the run and
                    // stopped it decaying at all -- measured on negotiated
                    // `segment_a`, where a run that dies at (120, 3, 85) in
                    // the simulator read a flat 5 all the way along and lit a
                    // repeater 2 cells past its own end.
                    false,
                    block_power,
                );
            }
        } else if flags_of(state).is_conductive() {
            // `pos` is a plain conductive block this walk has found
            // **strongly** powered -- `deliver`'s own rule is what
            // guarantees that, and it is the only way such a cell is ever
            // queued. A strongly powered conductive block re-drives *every*
            // redstone wire touching *any* of its six faces, not only the
            // one direction the repeater's own output happened to arrive
            // from -- this is `propagate::recompute_dust_strengths`'s own
            // "強充能的方塊也能驅動相鄰的紅石粉" rule, `mark_powered`'s own
            // `BlockPower::Strong` arm, and mechanism 3 of
            // `docs/derived/coupling-mechanisms.md`, where it is 31 measured
            // couplings rather than an argument. A repeater climbing a ramp
            // never touches the landing dust directly: it powers the riser
            // underneath it, and the riser is what lights the dust sitting
            // on its top face.
            for direction in ALL_SIX {
                let neighbour = pos.offset(direction);
                if world.get(neighbour.x, neighbour.y, neighbour.z).kind == BlockKind::RedstoneWire
                {
                    deliver(
                        world,
                        own_cells,
                        &mut strength,
                        &mut queue,
                        &mut ever_queued,
                        neighbour,
                        direction,
                        here,
                        true,
                        BlockPower::None,
                    );
                }
            }
        } else {
            // `pos` is an active component (in practice, always a repeater
            // -- see this function's own doc comment) this walk has already
            // established is fed (the only way its own cell -- as opposed to
            // whatever it drives -- ever ends up in `strength` at all): it
            // drives forward from itself along its one fixed output
            // direction, exactly as `structural_output` already models for
            // `verify_torch_merge`, reused here unchanged. Its output is
            // `BlockPower::Strong`, so a conductive block it lands on is
            // walked from and a run continues past a ramp; landing on thin
            // air (the descend rule's own deliberately-open cell) is still
            // recorded and still radiates nothing, which is exactly the
            // "repeater refreshing into a cell nothing continues from"
            // failure mode this invariant exists to catch.
            for direction in ALL_SIX {
                let (drives_dust, block_power) =
                    structural_output_in_world(world, pos, state, direction);
                if !drives_dust && block_power == BlockPower::None {
                    continue;
                }
                deliver(
                    world,
                    own_cells,
                    &mut strength,
                    &mut queue,
                    &mut ever_queued,
                    pos.offset(direction),
                    direction,
                    MAX_SIGNAL_STRENGTH,
                    drives_dust,
                    block_power,
                );
            }
        }
    }

    strength
}

/// The signal-strength invariant: every net must deliver a non-zero signal
/// to every one of its own declared gate-input sinks, and every declared
/// circuit output must receive a non-zero signal from its driving gate's
/// output torch. See this section's own doc comment for what makes this
/// different from `verify_connectivity`/`verify_torch_merge`, and
/// `net_signal_strength` for how the arriving strength is actually derived.
///
/// This assumes `verify_connectivity` and `verify_torch_merge` have already
/// run and passed: it trusts that a net's own reservation is a single clean
/// network (so `net_signal_strength` never has to re-derive that), and it
/// reuses `torch_support_position` exactly as `verify_torch_merge` does to
/// find each gate's support block, without re-checking that the torch
/// resolves to one at all.
///
/// # Declared wire merges
///
/// A merge gate has no torch or support of its own, and its own net (if it
/// even has one -- see below) has no independent source: electrically, its
/// output net and every one of its declared input nets are the *same* net
/// (`MergeGroups`, as `verify_connectivity`/`verify_torch_merge` already
/// treat it). So this checks strength per *declared merge group*, not per
/// raw net index -- mirroring `verify_torch_merge`'s own `group_cells`/
/// `group_reach` split, for the same reason: a repeater-isolated branch's
/// own reservation is disjoint from the junction it feeds (a repeater
/// breaks the wire-to-wire chain `net_signal_strength`'s walk otherwise
/// follows), so checking each raw net alone would show it reaching nothing
/// past its own final cell, even on a perfectly working merge. Uniting a
/// group's cells and seeding *every* branch's own true origin into one
/// shared `net_signal_strength` walk answers the real question instead:
/// does the max of every branch that is actually driven reach this group's
/// real, further-downstream sinks -- exactly what a real OR's dust does.
///
/// A net sourced from a merge gate (`Source::Gate(g)` where `g.is_merge()`)
/// contributes no origin of its own to `group_sources` -- its whole signal
/// is already the other branches sharing its group, seeding it too would
/// double up nothing (there is no active component there to seed from
/// anyway: `place_merge_gate` puts plain dust at that position, not a torch
/// or a lever). A merge gate is likewise never itself checked as a *sink*
/// in the loop below (`gate.is_merge()` skips it): it has nothing -- no torch,
/// no support -- for a signal to "arrive at", and whether its own output
/// eventually reaches a *real* sink is exactly what the same shared group
/// walk already answers for whatever net does declare that real sink.
fn verify_signal_strength(
    world: &World,
    reservation: &Reservation,
    netlist: &Netlist,
    nets: &[Net],
    gate_output_positions: &BTreeMap<String, (i32, i32, i32)>,
    input_positions: &BTreeMap<String, (i32, i32, i32)>,
    output_positions: &BTreeMap<String, (i32, i32, i32)>,
) -> Result<(), CompileError> {
    let groups = MergeGroups::build(netlist, nets);
    let index_of_signal: HashMap<&str, usize> = nets
        .iter()
        .enumerate()
        .map(|(i, net)| (net_source_name(netlist, net), i))
        .collect();

    let mut net_cells: Vec<HashSet<Position>> = vec![HashSet::new(); nets.len()];
    for (&pos, &owner) in reservation {
        net_cells[owner].insert(pos);
    }

    // Group cells: the union of every net's own cells within the same
    // declared-merge group (singleton groups, one per net, when nothing in
    // the netlist declares a merge at all -- same generalisation
    // `verify_torch_merge`'s `group_cells` already makes).
    let mut group_cells: HashMap<usize, HashSet<Position>> = HashMap::new();
    for (n, cells) in net_cells.iter().enumerate() {
        group_cells
            .entry(groups.root(n))
            .or_default()
            .extend(cells.iter().copied());
    }

    // Every merge's own junction and outbound-pin cells, too -- see
    // `merge_gate_body_owners`'s own doc comment for why they are never in
    // `reservation` (gate body, not routed wire) and why that is a real gap
    // here specifically: the walk below only ever continues past a cell it
    // considers "this group's own" (deliberately, to stop it crossing into
    // an unrelated net that happens to physically touch), so an unclaimed
    // junction cell would otherwise be a dead end it can record a value at
    // but never propagate *through*, silently stranding whatever sits past
    // it (its own outbound pin, and everything downstream of that).
    for (&position, &root) in &merge_gate_body_owners(netlist, nets, gate_output_positions) {
        group_cells.entry(root).or_default().insert(position);
    }

    // Group sources: one true origin per net in the group whose own source
    // is a genuine active component -- a lever, or a *non-merge* gate's
    // output torch. See this function's own doc comment for why a
    // merge-sourced net contributes none of its own.
    let mut group_sources: HashMap<usize, Vec<(Position, &BlockState)>> = HashMap::new();
    for (n, net) in nets.iter().enumerate() {
        let merge_sourced = matches!(net.source, Source::Gate(g) if netlist.gates[g].is_merge());
        if merge_sourced {
            continue;
        }
        let (source, source_state) = match net.source {
            Source::Lever(i) => {
                let &(x, y, z) = input_positions
                    .get(&netlist.inputs[i])
                    .expect("emit records a lever position for every input");
                (Position::new(x, y, z), world.get(x, y, z))
            }
            Source::Gate(g) => {
                let &(x, y, z) = gate_output_positions
                    .get(&netlist.gates[g].output)
                    .expect("emit records a torch position for every gate");
                (Position::new(x, y, z), world.get(x, y, z))
            }
        };
        group_sources
            .entry(groups.root(n))
            .or_default()
            .push((source, source_state));
    }

    let empty_sources: Vec<(Position, &BlockState)> = Vec::new();
    let group_strength: HashMap<usize, HashMap<Position, u8>> = group_cells
        .iter()
        .map(|(&root, cells)| {
            let sources = group_sources.get(&root).unwrap_or(&empty_sources);
            (root, net_signal_strength(world, cells, sources))
        })
        .collect();

    for (n, net) in nets.iter().enumerate() {
        let strength = &group_strength[&groups.root(n)];

        for &(gate, _input_index) in net.sinks.iter().flatten() {
            // A merge gate has no torch or support to check strength
            // against at all -- see this function's own doc comment.
            if netlist.gates[gate].is_merge() {
                continue;
            }
            let &(tx, ty, tz) = gate_output_positions
                .get(&netlist.gates[gate].output)
                .expect("emit records a torch position for every gate");
            let torch_pos = Position::new(tx, ty, tz);
            let torch_state = world.get(tx, ty, tz);
            let Some(support) = torch_support_position(torch_state, torch_pos) else {
                // Not this invariant's finding to make -- `verify_torch_merge`
                // already rejects a torch with no resolvable support, and it
                // runs before this does.
                continue;
            };

            if strength.get(&support).copied().unwrap_or(0) == 0 {
                return Err(CompileError::SignalStrengthViolation {
                    net: net_name(netlist, nets, n),
                    sink: SignalSink::GateInput {
                        gate: netlist.gates[gate].name.clone(),
                        support: (support.x, support.y, support.z),
                    },
                });
            }
        }
    }

    // Every declared circuit output, checked directly rather than through
    // any `Net`: a gate whose output drives *only* a lamp and no other
    // gate gets no `Net` of its own at all (`build_nets` drops a signal
    // with no gate-input sink), yet the lamp is still a real sink this
    // invariant has to answer for.
    for output_name in &netlist.outputs {
        let g = netlist
            .gates
            .iter()
            .position(|gate| &gate.output == output_name)
            .expect("every output was checked to be driven by a gate above");
        let &(tx, ty, tz) = gate_output_positions
            .get(output_name)
            .expect("every output was checked to be driven by a gate above");
        let torch_pos = Position::new(tx, ty, tz);
        let torch_state = world.get(tx, ty, tz);
        let &(lx, ly, lz) = output_positions
            .get(output_name)
            .expect("emit records a lamp position for every declared output");
        let lamp_pos = Position::new(lx, ly, lz);
        let pin = lamp_pos.up();

        let delivers = if netlist.gates[g].is_merge() {
            // A merge's "torch position" is plain dust (see
            // `place_merge_gate`), so the ordinary single-hop
            // `structural_output` check below cannot see it at all -- worse,
            // `torch_pos == pin` for a merge (see `emit`'s own `is_merge`
            // branch for `gate_pin`), so that check could never even ask the
            // right question. Ask the same shared group-strength map every
            // net in this merge's group already answers instead: does `pin`
            // (the merge's own junction, and this net's own reservation
            // whenever it has one) ever receive a non-zero signal.
            let root = merge_output_group_root(netlist, g, &index_of_signal, &groups)
                .expect("an undriven merge input would already have failed compile's own UndrivenSignal check");
            group_strength
                .get(&root)
                .and_then(|strength| strength.get(&pin))
                .copied()
                .unwrap_or(0)
                > 0
        } else {
            ALL_SIX.into_iter().any(|direction| {
                torch_pos.offset(direction) == pin && structural_output(torch_state, direction).0
            })
        };

        if !delivers {
            return Err(CompileError::SignalStrengthViolation {
                net: output_name.clone(),
                sink: SignalSink::OutputLamp {
                    output: output_name.clone(),
                    lamp: (lx, ly, lz),
                },
            });
        }
    }

    Ok(())
}

/// Re-run the physical invariant suite against a fresh legacy emission.
/// Its world and ownership reservation come from the new compilation;
/// ownership is never guessed by scanning blocks.
/// Run the three world-scanning invariants against a world the planner
/// realised itself, rather than against the legacy emitter's own output.
///
/// Spacing is not here: it is a property of the plan's reservation, which the
/// caller owns and checks before anything is written.
pub(crate) fn verify_realised_world(
    world: &World,
    reservation: &Reservation,
    netlist: &Netlist,
    nets: &[Net],
    gate_output_positions: &BTreeMap<String, (i32, i32, i32)>,
    input_positions: &BTreeMap<String, (i32, i32, i32)>,
    output_positions: &BTreeMap<String, (i32, i32, i32)>,
) -> Result<(), CompileError> {
    verify_connectivity(world, reservation, netlist, nets, gate_output_positions)?;
    verify_torch_merge(world, reservation, netlist, nets, gate_output_positions)?;
    verify_signal_strength(
        world,
        reservation,
        netlist,
        nets,
        gate_output_positions,
        input_positions,
        output_positions,
    )
}

/// Check that a route's recorded terminal describes the block realisation
/// actually put at its sink.
///
/// Terminal style is a planning decision -- dust or repeater into a support
/// -- and a plan that says one thing while its world holds the other is a
/// plan nobody can trust the cost of.
pub(crate) fn verify_route_terminal(
    world: &World,
    reservation: &Reservation,
    netlist: &Netlist,
    nets: &[Net],
    net: usize,
    route: &str,
    terminal: &RouteTerminal,
) -> Result<(), CompileError> {
    let gate = netlist
        .gates
        .iter()
        .position(|gate| gate.output == terminal.sink.gate)
        .ok_or_else(|| CompileError::CandidateMetadataViolation {
            item: route.to_string(),
            reason: format!("terminal names unknown gate `{}`", terminal.sink.gate),
        })?;
    if !nets[net]
        .sinks
        .iter()
        .flatten()
        .any(|&(sink_gate, sink_input)| {
            sink_gate == gate && sink_input == terminal.sink.input_index
        })
    {
        return Err(CompileError::CandidateMetadataViolation {
            item: route.to_string(),
            reason: "terminal sink is not an edge endpoint of this route".to_string(),
        });
    }
    let anchor = terminal.sink.anchor;
    let position = Position::new(anchor.x, anchor.y, anchor.z);
    if reservation.get(&position) != Some(&net) {
        return Err(CompileError::SpacingViolation {
            cell: (position.x, position.y, position.z),
            expected_net: route.to_string(),
            found_net: reservation
                .get(&position)
                .and_then(|owner| nets.get(*owner))
                .map(|owner| net_source_name(netlist, owner).to_string()),
        });
    }
    let actual = world.get(position.x, position.y, position.z).kind;
    // The four styles partition by two independent facts: which block landed,
    // and whether this is a bare merge branch. Not by whether the sink gate
    // is a merge -- a merge branch that is *not* bare lands through
    // `lay_bent_path` like any other, with an ordinary repeater.
    let bare = merge_branch_is_bare(netlist, &nets[net], gate);
    let matches = match terminal.kind {
        RouteTerminalKind::RepeaterIntoSupport => actual == BlockKind::Repeater && !bare,
        RouteTerminalKind::DirectedDustIntoSupport => actual == BlockKind::RedstoneWire && !bare,
        RouteTerminalKind::BareMergeDust => actual == BlockKind::RedstoneWire && bare,
        RouteTerminalKind::BareMergeRepeater => actual == BlockKind::Repeater && bare,
    };
    if !matches {
        return Err(CompileError::CandidateMetadataViolation {
            item: route.to_string(),
            reason: format!(
                "terminal style {:?} does not match its realised sink block {actual:?} at                  ({}, {}, {}) into {} input {} (merge: {})",
                terminal.kind,
                position.x,
                position.y,
                position.z,
                terminal.sink.gate,
                terminal.sink.input_index,
                netlist.gates[gate].is_merge(),
            ),
        });
    }
    Ok(())
}

/// The `MergeGroups` root that governs `gate`'s own output signal, whether
/// or not that signal has its own `Net` -- a merge whose output feeds
/// nothing but a declared circuit output has no `Net` of its own at all
/// (`build_nets` drops a signal with no gate-input sink), but every one of
/// its declared inputs still does (each was checked to be driven before
/// `compile` ever started placing anything), and `MergeGroups::build` unions
/// all of a merge's declared inputs together regardless of whether its own
/// output has a net -- so any one of them names the right group. `None`
/// only if `gate` (already known to be a merge) declares no input that is
/// driven by anything at all, which `compile`'s own `UndrivenSignal` check
/// rules out before this ever runs.
fn merge_output_group_root(
    netlist: &Netlist,
    gate: usize,
    index_of_signal: &HashMap<&str, usize>,
    groups: &MergeGroups,
) -> Option<usize> {
    if let Some(&output_index) = index_of_signal.get(netlist.gates[gate].output.as_str()) {
        return Some(groups.root(output_index));
    }
    netlist.gates[gate]
        .inputs
        .iter()
        .find_map(|input| index_of_signal.get(input.as_str()).map(|&i| groups.root(i)))
}

/// Compile a netlist into a redstone world.
///
/// Every gate must already be **realisable** -- a NOR or a wire merge (see
/// [`Netlist`]). A gate-level netlist, as the Verilog frontend produces, has
/// to go through [`lowering::lower`] first.
///
/// # Why this does not lower for you
///
/// Lowering it here would be one line and would work. It would also hand
/// every caller a trap: nearly everything that compiles a netlist keeps the
/// netlist afterwards and pairs it with the result -- `timing`'s
/// critical-path walk, `equivalence`'s structural check, `mc_dump`'s `GATE`
/// lines beside its `GATEOUT` positions, the viewer's per-gate metadata
/// beside its primitive graph. All of those correlate a gate in *the netlist
/// they hold* with something in *the circuit this compiled*, so a silent
/// lowering in here would mean those are two different netlists, silently.
/// (`summarize_worst_case` does not fail an assertion on that; it walks
/// forever.) Requiring the caller to lower makes holding the wrong netlist
/// impossible rather than merely discouraged.
pub fn compile(netlist: &Netlist) -> Result<CompiledCircuit, CompileError> {
    // Both compilers refuse the same netlists for the same reasons, and this
    // says so once rather than leaving it a property of the fallback happening
    // to duplicate it. It also keeps the trial below from being spent on a
    // netlist neither compiler can build.
    //
    // What it is *not*, measured rather than assumed: the thing that keeps
    // `CompileError::CyclicNetlist` reaching the caller. Deleting this line
    // leaves `an_unbuildable_netlist_is_refused_by_name_and_not_by_the_trial`
    // green, because `compile_legacy` runs the same checks and the fallback
    // reports the same error. What guards the caller's error is the fallback
    // itself; see that test.
    let _ = checked_topological_order(netlist)?;

    // The policy, and it is exactly this so it is predictable:
    //
    // 1. Try the planner: relaxation places, A* with a **bounded** rip-up
    //    budget routes, and `realise_and_verify` puts the four physical
    //    invariants on the world that would ship.
    // 2. If that succeeds, return it. `planner_kind()` says `Unified3d`.
    // 3. On any failure -- placement, routing, verification -- fall back to
    //    the row/channel/track emitter, which is what `compile` was before
    //    today and is unchanged. `planner_kind()` says `Legacy`.
    // 4. Same netlist, same path, every time. Nothing here reads a clock, a
    //    random number, or an environment variable.
    //
    // Why bounded: measured on 2026-08-16 (`planner::tests::
    // what_a_rip_up_budget_buys_and_what_it_costs`), the full 64-round budget
    // costs segment_a 36.7s and seven_segment 21.0s *to fail*, and neither
    // ever routes. That is time paid by every circuit that gains nothing.
    // `TRIAL_RIP_UP_ROUNDS` is what makes the trial affordable; the constant's
    // own doc carries the cost curve and why 8.
    //
    // The planner's error is discarded rather than reported, because a trial
    // that failed is not a compile that failed -- the circuit below is. What
    // is *not* discarded is the fact that it happened: `planner_kind` records
    // which path produced the world, so a circuit that quietly stopped taking
    // the planner shows up as a changed enum and a changed block count rather
    // than as nothing at all.
    //
    // The one thing that is refused *before* the trial rather than after it is
    // a netlist whose plan the planner cannot express at all. A failure it can
    // see becomes a fallback; a difference it cannot see would become a
    // silently wrong circuit, and no amount of trying harder finds it. See
    // `planner_can_express`.
    if planner_can_express(netlist) {
        if let Ok(planned) = compile_planned_within(
            netlist,
            &planner::PortPlacements::default(),
            planner::TRIAL_RIP_UP_ROUNDS,
        ) {
            return Ok(planned);
        }
    }
    compile_legacy(netlist)
}

/// Whether the planner's `PlanCandidate` can represent everything this netlist
/// needs built.
///
/// **This is a correctness gate, not a cost one.** Everything else `compile`
/// does about the planner is "try it and fall back if it says no"; this covers
/// the case where it would say yes and be wrong.
///
/// One condition today: a **merge branch whose producer signal has another
/// consumer**. `primitive_graph::expand` gives such a branch an
/// `IsolatingRepeater` node -- the one block standing between a shared producer
/// and the junction, without which the merge's other branches drive the
/// producer's other consumer backwards. But a `PlanCandidate` carries one
/// anchor per gate and per primary input, not one per primitive (the seam Task
/// 9 worked around and the spring-placement plan lists as out of scope), so
/// relaxation has nowhere to stand it and the branch realises as a bare join.
///
/// Measured 2026-08-16 on `tests/or_merge.rs`'s shared-branch fixture -- a
/// lever `a` feeding both a NOT and a two-input merge, which is the smallest
/// circuit with the shape:
///
/// | | junction | shared branch's socket | repeaters in the world |
/// |---|---|---|---|
/// | `compile_legacy` | (28,1,5) | **Repeater** | 4 |
/// | `compile_planned` | (33,1,11) | RedstoneWire | **0** |
///
/// Nothing catches it downstream: `verify_terminal_style` sorts a dust
/// terminal on a non-bare branch into `DirectedDustIntoSupport`, which is a
/// legal style, and `MergeGroups` unions all of a merge's declared inputs, so
/// the other branch reaching the shared consumer is same-group and permitted.
/// That fixture does come out computing the right function, and the reason is
/// distance rather than design: the junction is fourteen cells from the
/// sentinel's torch, so the backflow decays to nothing before it arrives.
/// `the_relaxation_path_cannot_isolate_a_shared_merge_branch` pins all of it.
///
/// **This gate is free on every circuit that matters, which is why it is a
/// gate and not a project.** Of the six circuits the Stage 3 condition names,
/// five contain no merge at all, and the sixth (`verilog:seven_segment`, 17
/// merges and 23 shared branches) already falls back because the projection
/// deadlocks on it. Not one block moves.
fn planner_can_express(netlist: &Netlist) -> bool {
    primitive_graph::shared_merge_branches(netlist).is_empty()
}

/// Every check `compile` made before it started building, and the topological
/// order that proves the last of them.
///
/// Shared rather than duplicated, because both compilers want it: the emitter
/// still has to refuse an unrealisable gate, and the planner path never had
/// these at all -- it relied on `plan_from_netlist` failing later, with a worse
/// message, and since today it relies on a failure being *swallowed*.
///
/// The order comes back rather than being recomputed by the caller. Proving one
/// exists **is** the acyclicity check, and `build_floorplan` needs the order
/// itself.
fn checked_topological_order(netlist: &Netlist) -> Result<Vec<usize>, CompileError> {
    for gate in &netlist.gates {
        let realisable = gate.kind.is_realisable() && gate.kind.accepts_arity(gate.inputs.len());
        if !realisable {
            return Err(CompileError::NotRealisable {
                gate: gate.output.clone(),
                kind: gate.kind,
            });
        }
    }
    for gate in &netlist.gates {
        for input in &gate.inputs {
            if !netlist.is_driven(input) {
                return Err(CompileError::UndrivenSignal(input.clone()));
            }
        }
    }
    for output in &netlist.outputs {
        if !netlist.gates.iter().any(|gate| &gate.output == output) {
            return Err(CompileError::UndrivenSignal(output.clone()));
        }
    }

    netlist
        .topological_order()
        .ok_or(CompileError::CyclicNetlist)
}

/// Compile by the row/channel/track emitter, which places the gates and hands
/// the planner a seed to realise.
///
/// This is what `compile` was before the hybrid landed, line for line, and it
/// is still what `compile` returns for every circuit the planner cannot handle.
/// It is kept **and public** for two reasons that are not the same reason:
///
/// * `compile` falls back to it, so it is production code, not a museum piece.
/// * "is relaxation better" is a question somebody will ask again, and a
///   comparison nobody can run is a claim nobody can check. `seed_from_legacy`
///   is `pub` and needs a circuit carrying a `LegacyEmission` to extract from;
///   since `compile` may not produce one, this is the only way to get one.
///
/// Stamps `PlannerKind::Legacy`, which nothing constructed before today.
pub fn compile_legacy(netlist: &Netlist) -> Result<CompiledCircuit, CompileError> {
    let order = checked_topological_order(netlist)?;

    let mut producer_of: HashMap<&str, usize> = HashMap::new();
    for (index, gate) in netlist.gates.iter().enumerate() {
        producer_of.insert(gate.output.as_str(), index);
    }

    let plan = build_floorplan(netlist, &order, &producer_of);
    let row_count = plan.rows.len();
    let channel_count = row_count.saturating_sub(1);
    let mut nets = build_nets(netlist, &order, &plan, &producer_of);

    reserve_columns(&plan, &mut nets, row_count, channel_count);
    let (bypass, row_z, track_z) = resolve_bypass_and_geometry(
        netlist,
        &plan,
        &mut nets,
        row_count,
        channel_count,
        BYPASS_QUERY_MAX_DISTANCE,
    );

    let (size_x, size_z) = world_size(&plan, &nets, &row_z);
    let track_count: Vec<usize> = track_z.iter().map(Vec::len).collect();
    let size_y = world_height(&track_count);

    // First build the conservative all-repeater geometry once.  This is not
    // an emitted result: it is the complete, live reservation and world shape
    // against which every candidate directed-dust terminal is judged.
    let baseline_terminals = default_terminal_kinds(&nets);
    let baseline_geometry = RoutingGeometry {
        plan: &plan,
        row_z: &row_z,
        nets: &nets,
        track_z: &track_z,
        track_count: &track_count,
        bypass: &bypass,
        terminals: &baseline_terminals,
    };
    let mut baseline_world = World::new(size_x.max(8), size_y, size_z.max(8));
    let mut baseline_footprint = Footprint::record();
    let baseline_result = emit(
        &mut baseline_world,
        netlist,
        &baseline_geometry,
        &mut baseline_footprint,
    );
    let terminals = resolve_directed_dust_terminals(
        &mut baseline_world,
        &baseline_footprint.reservation,
        netlist,
        &nets,
        &baseline_result.input_positions,
        &baseline_result.gate_output_positions,
    );
    drop(baseline_world);
    drop(baseline_footprint);

    // ---------------------------------------------------------------
    // Emission
    // ---------------------------------------------------------------
    //
    // Two passes over the exact same code (`emit`). The first, recording
    // pass runs against a throwaway world purely to learn where every net's
    // conductor cells end up -- ramps, columns and tracks alike, for every
    // net, not just the ones already written when a given ramp is placed.
    // Only once that whole-circuit picture exists does the second pass run
    // for real, consulting it so a keep-out cell is only ever sealed with
    // stone when nothing else in the circuit will ever legitimately use it.
    // See `Footprint` and `seal_cross_talk` for why that split is what makes
    // the spacing constraint free where nothing is near.

    let geometry = RoutingGeometry {
        plan: &plan,
        row_z: &row_z,
        nets: &nets,
        track_z: &track_z,
        track_count: &track_count,
        bypass: &bypass,
        terminals: &terminals,
    };

    let mut scratch = World::new(size_x.max(8), size_y, size_z.max(8));
    let mut footprint = Footprint::record();
    emit(&mut scratch, netlist, &geometry, &mut footprint);
    drop(scratch);

    // Ownership only exists in the recording pass -- `claim` is a no-op once
    // the reservation is complete -- so the recording footprint is kept alive
    // past the pass that produced it. The blocks it names are read out of the
    // world that actually ships, below.
    let recorded = footprint;
    let mut footprint = Footprint::enforce(recorded.reservation.clone());
    let mut world = World::new(size_x.max(8), size_y, size_z.max(8));
    let EmitResult {
        input_positions,
        output_positions,
        gate_output_positions,
        primitive_anchors,
    } = emit(&mut world, netlist, &geometry, &mut footprint);

    // The connectivity invariant: whatever the two passes above actually
    // wrote, it must partition into exactly the nets the netlist asked for.
    // Checked here, unconditionally, on every compile -- not just the ones a
    // test happens to exercise -- because a violation is a bug in *this*
    // router, not in the netlist it was given.
    verify_connectivity(
        &world,
        &footprint.reservation,
        netlist,
        &nets,
        &gate_output_positions,
    )?;

    // The torch-merge invariant: connectivity alone never looks at a torch,
    // so it cannot tell a working NOR from one whose input is wired into
    // the wrong block. Checked unconditionally too, for the same reason.
    verify_torch_merge(
        &world,
        &footprint.reservation,
        netlist,
        &nets,
        &gate_output_positions,
    )?;

    // The signal-strength invariant: connectivity and torch-merge both
    // reason about what touches what; neither knows redstone dust decays.
    // Checked last, unconditionally, and only after the first two have
    // already passed -- it trusts their guarantees (a net's own reservation
    // is one clean network, and every declared input structurally reaches
    // its gate's support) rather than re-deriving them. See this module's
    // "The signal-strength invariant" section for the full picture.
    verify_signal_strength(
        &world,
        &footprint.reservation,
        netlist,
        &nets,
        &gate_output_positions,
        &input_positions,
        &output_positions,
    )?;

    let legacy_routes = recorded.legacy_routes(netlist, &nets, &world);
    let primitive_nodes = legacy_primitive_nodes(netlist, &primitive_anchors);
    let legacy_emission = LegacyEmission {
        netlist: netlist.clone(),
        primitive_anchors,
        primitive_nodes,
        routes: legacy_routes,
    };

    // The legacy path stops here. Everything above exists to produce one
    // candidate; from here the circuit that ships is the one the planner
    // realises from it and the invariants pass on that. A plan the planner
    // cannot build, or builds into something illegal, is a compile error --
    // not a discrepancy between two worlds nobody compares.
    //
    // `world` is kept only to size the realisation identically, so nothing
    // downstream sees a different bounding box for the same circuit.
    let size = world.size();
    drop(world);

    let seed = planner::seed_from_legacy_parts(netlist, &legacy_emission).map_err(planner_error)?;
    let realised = planner::realise_and_verify(&seed, netlist, size).map_err(planner_error)?;

    Ok(CompiledCircuit {
        world: realised.world,
        input_positions: realised.ports.input_positions,
        output_positions: realised.ports.output_positions,
        gate_output_positions: realised.ports.gate_output_positions,
        gate_facings: vec![geometry::CellFacing::NORTH; netlist.gates.len()],
        planner_kind: PlannerKind::Legacy,
        legacy_emission: Some(legacy_emission),
    })
}

/// Compile without the legacy emitter at all: the planner places and routes.
///
/// `compile` tries this first and falls back to `compile_legacy` when it
/// fails. This is the same path with **no fallback and no budget cut**: it
/// gets the router's full `RIP_UP_ROUNDS`, and a failure is returned rather
/// than swallowed. That is what makes it the right entry point for a
/// measurement or a probe -- `compile` cannot tell you *why* the planner
/// declined a circuit, and this can.
///
/// It is also the only way to place from pinned ports, which `compile` has no
/// parameter for.
///
/// The result carries no `LegacyEmission`, because there was none.
pub fn compile_planned(
    netlist: &Netlist,
    placements: &planner::PortPlacements,
) -> Result<CompiledCircuit, CompileError> {
    compile_planned_within(netlist, placements, planner::RIP_UP_ROUNDS)
}

/// The failure-directed generation front door: place, negotiate, and when
/// routing fails, buy room exactly where the router fought, until the circuit
/// carries or the budget runs out. No legacy fallback anywhere behind it.
///
/// This is the night-of-2026-08-28 method
/// (`docs/superpowers/specs/2026-08-28-failure-directed-generation.md`) as a
/// compiler entry: [`planner::plan_from_netlist_with_growth`] under
/// [`planner::GROWN_SHIPPING_RULE`], then the same realisation and four
/// physical invariants every other path ends in. Its first measured plan is
/// the first `segment_a` this tree ever routed and verified outside the
/// legacy emitter: 47 nets, wire 4,530, delay term 74, in 315.3s.
///
/// **Deliberately not [`compile`]'s default.** That cost is real: `compile`
/// stays the fast trial-then-fallback it documents, the viewer stays
/// responsive, and a caller who wants the generated circuit -- and is willing
/// to pay minutes for it on a decoder-sized netlist -- asks for it by name.
pub fn compile_grown(netlist: &Netlist) -> Result<CompiledCircuit, CompileError> {
    let _ = checked_topological_order(netlist)?;
    let candidate = planner::plan_from_netlist_with_growth(
        netlist,
        &planner::PortPlacements::default(),
        planner::GROWN_SHIPPING_RULE,
    )
    .map_err(planner_error)?;
    let gate_facings: Vec<geometry::CellFacing> =
        (0..netlist.gates.len()).map(|g| candidate.facing_of(g)).collect();
    let size = planner::candidate_world_size(&candidate);
    let realised = planner::realise_and_verify(&candidate, netlist, size).map_err(planner_error)?;

    Ok(CompiledCircuit {
        world: realised.world,
        input_positions: realised.ports.input_positions,
        output_positions: realised.ports.output_positions,
        gate_output_positions: realised.ports.gate_output_positions,
        gate_facings,
        planner_kind: PlannerKind::Unified3d,
        legacy_emission: None,
    })
}

/// [`compile_planned`], with the router's rip-up budget as a parameter.
///
/// Private, and a parameter rather than a `pub` knob, for the reason Task 11
/// deleted `Shape` for: how hard to try is this compiler's decision, not a
/// caller's. `compile` spends [`planner::TRIAL_RIP_UP_ROUNDS`] because it has
/// somewhere to fall back to; `compile_planned` spends
/// [`planner::RIP_UP_ROUNDS`] because it does not.
fn compile_planned_within(
    netlist: &Netlist,
    placements: &planner::PortPlacements,
    rip_up_rounds: usize,
) -> Result<CompiledCircuit, CompileError> {
    let candidate =
        planner::plan_from_netlist_within(netlist, placements, rip_up_rounds).map_err(planner_error)?;
    // Read before `candidate` is moved into `realise_and_verify`, and read off
    // the candidate rather than assumed: since Task 10 `plan_from_netlist`
    // places by relaxation and relaxation turns gates, so a verifier handed
    // north would inspect the wrong cells -- and pass, because the cells it
    // inspects are empty rather than wrong.
    let gate_facings: Vec<geometry::CellFacing> =
        (0..netlist.gates.len()).map(|g| candidate.facing_of(g)).collect();
    let size = planner::candidate_world_size(&candidate);
    let realised = planner::realise_and_verify(&candidate, netlist, size).map_err(planner_error)?;

    Ok(CompiledCircuit {
        world: realised.world,
        input_positions: realised.ports.input_positions,
        output_positions: realised.ports.output_positions,
        gate_output_positions: realised.ports.gate_output_positions,
        gate_facings,
        planner_kind: PlannerKind::Unified3d,
        legacy_emission: None,
    })
}

/// A planner failure that is not already an invariant violation is still a
/// compile failure: the plan was unbuildable.
fn planner_error(error: planner::PlannerError) -> CompileError {
    match error {
        planner::PlannerError::PhysicalInvariant(inner) => inner,
        other => CompileError::CandidateMetadataViolation {
            item: "candidate".to_string(),
            reason: other.to_string(),
        },
    }
}

/// Which of `compile`'s two paths produced the world a `CompiledCircuit`
/// carries.
///
/// **This names the placer, not the realiser.** Both paths end in
/// `planner::realise_and_verify`, so the world is the planner's realisation
/// either way and the four physical invariants ran on it either way; what
/// differs is where the anchors came from and who routed between them.
///
/// It exists so a fallback is visible. `compile` swallows the planner's error
/// by design -- a trial that failed is not a compile that failed -- and
/// without a record of the choice, a circuit that quietly stopped taking the
/// planner would look exactly like a circuit that never took it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlannerKind {
    /// Placed and routed by the row/channel/track emitter, then realised and
    /// verified by `compile::planner` from that seed. `compile_legacy`, and
    /// `compile` for every circuit the planner declined.
    Legacy,
    /// Placed by spring relaxation and routed by A* with rip-up, both in
    /// `compile::planner`. `compile_planned`, and `compile` where it works.
    Unified3d,
}


/// Every cell a gate's own realisation occupies, found by realising it into a
/// scratch world rather than by re-deriving the cell geometry a second time.
pub(crate) fn gate_footprint(
    origin: (i32, i32, i32),
    gate: &Gate,
    facing: geometry::CellFacing,
) -> (Vec<Anchor>, Vec<Anchor>, Anchor) {
    let mut scratch = World::new(64, 8, 64);
    let shifted = (32, 1, 32);
    let cell = if gate.is_merge() {
        place_merge_gate(&mut scratch, shifted, gate.inputs.len(), facing)
    } else {
        place_nor_gate(&mut scratch, shifted, gate.inputs.len(), facing)
    };
    // The output pin belongs to the gate too: it is written outside the cell
    // and no route may pass through it.
    let torch = Position::new(
        shifted.0 + cell.output_offset.0,
        shifted.1 + cell.output_offset.1,
        shifted.2 + cell.output_offset.2,
    );
    let pin = torch.offset(geometry::output_direction(facing));
    scratch.set(pin.x, pin.y, pin.z, dust());

    // A gate's input sockets are left as air for the router to fill, which
    // does not make them free: a net wandering through one gives the gate's
    // support an extra connection and turns the terminal beside it from a
    // straight line into a corner, so it stops driving the support at all.
    // They belong to the gate; only the net terminating in one may enter it,
    // and that step is appended rather than searched.
    let mut sockets = Vec::new();
    for direction in geometry::input_directions(facing)
        .iter()
        .take(gate.inputs.len())
    {
        let socket = Position::new(shifted.0, shifted.1, shifted.2).offset(*direction);
        scratch.set(socket.x, socket.y, socket.z, stone());
        sockets.push(socket);
    }

    let mut cells = Vec::new();
    let mut conductors = Vec::new();
    for flat in 0..scratch.cells().len() {
        let (x, y, z) = scratch.decode(flat);
        let kind = scratch.get(x, y, z).kind;
        if kind == BlockKind::Air {
            continue;
        }
        let cell = Anchor {
            x: origin.0 + (x - shifted.0),
            y: origin.1 + (y - shifted.1),
            z: origin.2 + (z - shifted.2),
        };
        cells.push(cell);
        // Solid material is inert -- a net may run beside a floor -- with one
        // exception that is not material at all: a NOR's support block is the
        // gate's input node. Dust laid against it powers it and turns the
        // torch off, so a foreign net running past reads as a legal layout
        // that computes the wrong function. It keeps others out like any
        // other conductor.
        let is_support = cell.x == origin.0 && cell.y == origin.1 && cell.z == origin.2;
        // A socket is stone here only because the router has not filled it
        // yet; what ends up in it is dust or a repeater. Treated as inert, a
        // foreign net may sit diagonally above it, and dust joins diagonally
        // -- two nets in one network, structurally invisible.
        let is_socket = sockets
            .iter()
            .any(|socket| socket.x == x && socket.y == y && socket.z == z);
        if kind != BlockKind::Solid || is_socket || (is_support && !gate.is_merge()) {
            conductors.push(cell);
        }
    }
    // The cell directly above a torch, which the gate writes nothing into and
    // owns anyway.
    //
    // `rules::taxonomy` gives a lit torch `BlockPower::Strong` on the block
    // above it, and a strongly powered block drives every dust beside it. So a
    // route that merely *stands* on that cell -- realisation writes its floor
    // there as stone -- reads 15 out of a gate it never connected to, and the
    // gate's signal enters a net that was not routed to it. Nothing else keeps
    // it out: the cell is air in the scratch world above, so the scan cannot
    // see it, and `anchor_is_free_for`'s floor test asks for a *conductor*
    // below, which is what this makes it.
    //
    // Measured on 2026-08-14, before this existed: full_adder placed by
    // relaxation routed `g2` over `g5`'s torch at (39, 1, 125) with its floor
    // at (39, 2, 125), and `g2`'s dust at (39, 3, 125) read 15 while `g2`'s own
    // torch was off -- power rising from 9 at the pin to 15 at that cell, which
    // is what says which end was the source. Eight of full_adder's 22 gates came
    // out wrong and every invariant passed: cell exclusivity proves ownership of
    // *routed* cells, and this cell was owned by nobody.
    //
    // Only for a torch. A merge's anchor is dust, and dust powers the block
    // below it rather than the one above.
    if !gate.is_merge() {
        let above = Anchor {
            x: origin.0 + cell.output_offset.0,
            y: origin.1 + cell.output_offset.1 + 1,
            z: origin.2 + cell.output_offset.2,
        };
        cells.push(above);
        conductors.push(above);
    }

    let output_pin = Anchor {
        x: origin.0 + (pin.x - shifted.0),
        y: origin.1 + (pin.y - shifted.1),
        z: origin.2 + (pin.z - shifted.2),
    };
    (cells, conductors, output_pin)
}

/// Every cell a primary input's lever occupies -- the lever, the pin dust it
/// drives, and the cell directly above it -- with the pin returned separately.
/// All three conduct, so there is one list rather than the two `gate_footprint`
/// returns.
///
/// This exists because the same three lines had been written out at all three
/// places that build a lever node, and the third cell was missing from every
/// one of them.
///
/// **The cell above.** It is the hazard `gate_footprint` claims above a torch,
/// and it was found the same way: by measuring. A lit lever is
/// `BlockPower::Strong` on each neighbouring block (`rules::taxonomy`), a
/// strongly powered block drives every dust beside it, and realisation writes a
/// route's floor as stone. So a route flying one storey over a lever stands on
/// a cell nobody claims and reads 15 out of an input it never connected to.
/// Nothing else keeps it out: `verify_spacing` proves ownership of *routed*
/// cells, and a floor is not a route anchor.
///
/// Measured on 2026-08-14. Pinning `full_adder`'s `cin` at (37, 1, 126) put
/// `g12`'s route over a lever; every physical invariant passed and one of the
/// sixteen output readings was wrong. Swapping only that floor block from stone
/// to glass -- which carries dust but does not conduct -- made the same circuit
/// right, which is what says the lever was the cause rather than the placement.
/// It is not an artefact of pinning either: in one of the cases the leaking
/// lever was `cin`, which nothing had pinned, and relaxation had put it under a
/// raised route on its own.
///
/// **What this is not.** Vanilla Minecraft does not have this short. A floor
/// lever there strongly powers only the block it is attached to, the one
/// *below*. `taxonomy::power_emitted_toward` says in its own comment that these
/// components' directionality "is not yet modelled" and falls through to a
/// six-directional `full`, so the cell above is dangerous under this crate's
/// rules and inert under Minecraft's. Claimed anyway, because this simulator is
/// what every circuit here is verified against, and a circuit its own verifier
/// calls wrong is wrong. Teaching the taxonomy a lever's `face` would delete
/// this hazard and promote the one below the lever, which is the vanilla one;
/// that is a change to the oracle every reference circuit is measured by, and
/// it wants its own measurement rather than a rider on this.
///
/// The cell *below* is left unclaimed. Every lever this planner places sits at
/// `PLANNER_Y = 1` and relaxation never moves Y, so that cell is the world
/// floor and no dust can reach it -- but `PortPlacements::pin` accepts any `y`,
/// and a lever pinned off the ground plane has a hazard below it that this does
/// not close. Claiming it would not close it either: the dangerous conductor
/// would then be *above* the route's cell, and `anchor_is_free_for` never looks
/// up.
pub(crate) fn lever_footprint(
    anchor: Anchor,
    facing: geometry::CellFacing,
) -> (Vec<Anchor>, Anchor) {
    let stepped = Position::new(anchor.x, anchor.y, anchor.z)
        .offset(geometry::output_direction(facing));
    let pin = Anchor { x: stepped.x, y: stepped.y, z: stepped.z };
    let above = Anchor { y: anchor.y + 1, ..anchor };
    (vec![anchor, pin, above], pin)
}

fn legacy_primitive_nodes(netlist: &Netlist, anchors: &[Anchor]) -> Vec<PrimitiveNode> {
    let mut nodes = Vec::with_capacity(anchors.len());
    for (gate, anchor) in netlist.gates.iter().zip(anchors.iter().copied()) {
        // A merge is dust where nets join, so it reserves an anchor without
        // ever becoming a component; every other realisable gate is one NOR
        // cell, whose component is its output torch.
        let realisation = if gate.is_merge() {
            NodeRealisation::WireMerge
        } else {
            NodeRealisation::Primitive(Primitive::Torch)
        };
        // North, and a literal, because these nodes describe cells `emit`
        // already placed and `emit` places every one north. Like the other
        // per-gate readers, what this will need is a facing per gate rather
        // than one for the call -- `emit`'s own choice, carried out alongside
        // `primitive_anchors`, since `anchors` is exactly the record this
        // walks and the facings would travel with it.
        let (footprint, conductors, output_pin) = gate_footprint(
            (anchor.x, anchor.y, anchor.z),
            gate,
            geometry::CellFacing::NORTH,
        );
        nodes.push(PrimitiveNode {
            id: format!("gate:{}", gate.output),
            anchor,
            realisation,
            footprint,
            conductors,
            // The legacy emitter chose these, and nobody asked it to.
            pinned: false,
            output_pin: Some(output_pin),
        });
    }
    for (input, anchor) in netlist
        .inputs
        .iter()
        .zip(anchors.iter().skip(netlist.gates.len()).copied())
    {
        // North, and a literal, for the same reason the gates above are: these
        // nodes describe cells `emit` already placed, and `emit` places every
        // one north.
        let (cells, pin) = lever_footprint(anchor, geometry::CellFacing::NORTH);
        nodes.push(PrimitiveNode {
            id: format!("input:{input}"),
            anchor,
            realisation: NodeRealisation::Primitive(Primitive::Lever),
            footprint: cells.clone(),
            conductors: cells,
            pinned: false,
            output_pin: Some(pin),
        });
    }
    nodes
}

#[cfg(test)]
mod tests {
    use super::topology::GateKind;
    use super::*;
    use crate::redstone::simulator::Simulator;

    /// A cell built facing anywhere is the north cell turned, cell for cell.
    ///
    /// This is the whole of Stage 0's claim. Relaxation will choose facings and
    /// hand them here; if a turned cell were assembled from its own arithmetic
    /// rather than from north's, the two would drift apart exactly where nobody
    /// looks -- at the three facings no reference circuit uses yet.
    #[test]
    fn a_turned_gate_cell_is_the_north_one_turned() {
        use crate::compile::geometry::{self, CellFacing};

        let origin = (16, 1, 16);
        for arity in 1..=3usize {
            let mut north_world = World::new(40, 4, 40);
            let north = place_nor_gate(&mut north_world, origin, arity, CellFacing::NORTH);

            for index in 0..4u8 {
                let facing = CellFacing::from_index(index).expect("0..4 is horizontal");
                let mut turned_world = World::new(40, 4, 40);
                let turned = place_nor_gate(&mut turned_world, origin, arity, facing);

                for (input, &offset) in north.input_offsets.iter().enumerate() {
                    assert_eq!(
                        turned.input_offsets[input],
                        geometry::rotate(offset, facing),
                        "arity {arity} facing {index}: input {input}'s socket"
                    );
                }
                assert_eq!(
                    turned.output_offset,
                    geometry::rotate(north.output_offset, facing),
                    "arity {arity} facing {index}: output"
                );
                // `output_offset` only says where the torch *is*; it says nothing
                // about which way its own blockstate claims to hang. An
                // implementation that turned the position but left the wall
                // torch's `facing` hardcoded to North would still pass every
                // assertion above at three facings out of four -- this is the
                // one that catches it, by reading the block the turned cell
                // actually wrote.
                let torch = Position::new(
                    origin.0 + turned.output_offset.0,
                    origin.1 + turned.output_offset.1,
                    origin.2 + turned.output_offset.2,
                );
                assert_eq!(
                    turned_world.get(torch.x, torch.y, torch.z).facing,
                    Some(geometry::output_direction(facing)),
                    "arity {arity} facing {index}: torch blockstate facing"
                );
                // Turning a rectangle swaps its sides; it does not change its area.
                let north_area = north.size.0 * north.size.2;
                assert_eq!(
                    turned.size.0 * turned.size.2,
                    north_area,
                    "arity {arity} facing {index}: footprint area"
                );
            }
        }
    }

    /// A merge is built to the same footprint as a NOR of the same arity, and
    /// stays that way turned -- which is what lets `emit`'s geometry stay
    /// gate-kind-agnostic.
    #[test]
    fn a_turned_merge_keeps_a_nors_socket_faces() {
        use crate::compile::geometry::CellFacing;

        let origin = (16, 1, 16);
        for index in 0..4u8 {
            let facing = CellFacing::from_index(index).expect("0..4 is horizontal");
            let mut nor_world = World::new(40, 4, 40);
            let mut merge_world = World::new(40, 4, 40);
            let nor = place_nor_gate(&mut nor_world, origin, 3, facing);
            let merge = place_merge_gate(&mut merge_world, origin, 3, facing);
            assert_eq!(nor.input_offsets, merge.input_offsets, "facing {index}");
            assert_eq!(merge.output_offset, (0, 0, 0), "facing {index}");
        }
    }

    /// Every compiled circuit says which way it built each gate, and on the
    /// **emitter's** path the answer is still north for all of them.
    ///
    /// `compile_legacy` and not `compile`, and the change of name is the whole
    /// point: the emitter builds north and reports north, and it now has to
    /// keep doing that while the hybrid's other path turns gates freely. This
    /// was written against `compile` when `compile` *was* the emitter; pointing
    /// it at `compile` today would assert that and4 comes back north, which is
    /// false, and weakening it to "some facing" would assert nothing.
    /// `planner::a_planned_circuit_reports_the_facings_it_was_built_at` is the
    /// same assertion for the path that does turn gates.
    ///
    /// The value is dull; having somewhere to put it is not. Three modules verify
    /// a world by recomputing where a gate's sockets must be, and once relaxation
    /// turns gates they need to be told rather than to assume -- and a merge's
    /// junction is dust, which cannot be asked.
    #[test]
    fn a_legacy_compiled_circuit_records_a_facing_for_every_gate() {
        use crate::circuits::and4::build_and4_netlist;
        use crate::compile::geometry::CellFacing;

        let (netlist, _) = build_and4_netlist();
        let compiled = compile_legacy(&netlist).expect("and4 compiles");

        assert_eq!(compiled.gate_facings.len(), netlist.gates.len());
        assert!(
            compiled.gate_facings.iter().all(|&facing| facing == CellFacing::NORTH),
            "`compile_legacy` seeds from the legacy emitter, so every gate must still be north"
        );
    }

    /// A minimal `Net` for tests that only care about `net_name` / ownership
    /// lookups, not real routing geometry -- `verify_connectivity` never
    /// looks at anything but `source`.
    fn nameless_net(source: Source) -> Net {
        Net {
            source,
            source_column: 0,
            channels: Vec::new(),
            tracks: Vec::new(),
            sinks: Vec::new(),
            hops: Vec::new(),
        }
    }

    #[test]
    fn sequential_boundary_cuts_feedback_but_a_pure_combinational_loop_stays_cyclic() {
        let through_dff = Netlist {
            inputs: vec!["clk".to_string()],
            outputs: vec!["q".to_string()],
            gates: vec![
                Gate::nor("d", &["q"]),
                Gate {
                    name: "q".to_string(),
                    inputs: vec!["d".to_string(), "clk".to_string()],
                    output: "q".to_string(),
                    kind: GateKind::DffPosedge,
                },
            ],
        };

        assert_eq!(through_dff.combinational_order(), Some(vec![1, 0]));
        assert!(GateKind::DffPosedge.is_sequential());
        assert_eq!(GateKind::DffPosedge.fixed_arity(), Some(2));
        assert_eq!(GateKind::DffPosedge.wire_name(), "dff_p");
        assert!(GateKind::DffPosedge.accepts_arity(2));
        assert!(!GateKind::DffPosedge.accepts_arity(1));

        let pure_loop = Netlist {
            inputs: Vec::new(),
            outputs: vec!["a".to_string()],
            gates: vec![Gate::nor("a", &["b"]), Gate::nor("b", &["a"])],
        };

        assert_eq!(pure_loop.combinational_order(), None);
    }

    /// The connectivity invariant, built directly rather than hoped for:
    /// two separate dust cells, each reserved for a different net, placed
    /// next to each other so `dust_connections`' same-layer rule joins them
    /// into one electrical network. No router, no netlist compile -- this
    /// exercises `verify_connectivity` on a world constructed by hand,
    /// exactly the "two nets routed so their dust touches" case the spacing
    /// model spec asks for.
    #[test]
    fn verify_connectivity_rejects_two_nets_whose_dust_touches() {
        let netlist = Netlist {
            inputs: vec!["a".to_string(), "b".to_string()],
            outputs: Vec::new(),
            gates: Vec::new(),
        };
        let nets = vec![
            nameless_net(Source::Lever(0)),
            nameless_net(Source::Lever(1)),
        ];

        let mut world = World::new(5, 5, 5);
        let net_a_cell = Position::new(1, 1, 2);
        let net_b_cell = Position::new(2, 1, 2);
        world.set(net_a_cell.x, net_a_cell.y - 1, net_a_cell.z, stone());
        world.set(net_a_cell.x, net_a_cell.y, net_a_cell.z, dust());
        world.set(net_b_cell.x, net_b_cell.y - 1, net_b_cell.z, stone());
        world.set(net_b_cell.x, net_b_cell.y, net_b_cell.z, dust());

        let mut reservation = Reservation::new();
        reservation.insert(net_a_cell, 0);
        reservation.insert(net_b_cell, 1);

        let err = verify_connectivity(&world, &reservation, &netlist, &nets, &BTreeMap::new())
            .expect_err("adjacent dust reserved for two different nets must be rejected");

        assert_eq!(
            err,
            CompileError::ConnectivityViolation {
                cell: (net_b_cell.x, net_b_cell.y, net_b_cell.z),
                found_net: "b".to_string(),
                expected_cell: (net_a_cell.x, net_a_cell.y, net_a_cell.z),
                expected_net: "a".to_string(),
            }
        );

        let message = err.to_string();
        assert!(
            message.contains("(2, 1, 2)"),
            "message must name the offending cell: {message}"
        );
        assert!(
            message.contains('a') && message.contains('b'),
            "message must name both nets: {message}"
        );
    }

    /// The same two cells, but far enough apart that `dust_connections`
    /// never joins them -- the invariant must stay silent when nothing
    /// actually touches.
    #[test]
    fn verify_connectivity_accepts_two_nets_whose_dust_never_touches() {
        let netlist = Netlist {
            inputs: vec!["a".to_string(), "b".to_string()],
            outputs: Vec::new(),
            gates: Vec::new(),
        };
        let nets = vec![
            nameless_net(Source::Lever(0)),
            nameless_net(Source::Lever(1)),
        ];

        let mut world = World::new(6, 5, 6);
        let net_a_cell = Position::new(1, 1, 2);
        let net_b_cell = Position::new(4, 1, 2);
        world.set(net_a_cell.x, net_a_cell.y - 1, net_a_cell.z, stone());
        world.set(net_a_cell.x, net_a_cell.y, net_a_cell.z, dust());
        world.set(net_b_cell.x, net_b_cell.y - 1, net_b_cell.z, stone());
        world.set(net_b_cell.x, net_b_cell.y, net_b_cell.z, dust());

        let mut reservation = Reservation::new();
        reservation.insert(net_a_cell, 0);
        reservation.insert(net_b_cell, 1);

        assert_eq!(
            verify_connectivity(&world, &reservation, &netlist, &nets, &BTreeMap::new()),
            Ok(())
        );
    }

    // -----------------------------------------------------------------
    // What `verify_connectivity` does not see
    // -----------------------------------------------------------------
    //
    // The three tests below are **records of gaps, not endorsements**. Each
    // builds a world in which two nets really are one electrically -- proved
    // in the same test by running the `Simulator` and flipping one net's
    // source -- and then asserts that `verify_connectivity` returns `Ok`.
    //
    // They are written this way round on purpose. Closing any of these gaps
    // must turn its test red, and whoever closes one should delete the
    // recording and put the `expect_err` in its place; a gap nobody can
    // notice closing is how the first two shipped twice.
    //
    // Every coupling mechanism they use is derived by experiment in
    // `tests/coupling_mechanisms.rs` and tabulated in
    // `docs/derived/coupling-mechanisms.md`.

    /// Two nets, one net-a lever, one stone block, and net b's wire on top of
    /// it. **The first shipped bug's exact geometry**, reduced to two nets.
    ///
    /// `verify_connectivity` starts from every dust cell and follows
    /// `dust_connections`, which only ever returns dust neighbours. The two
    /// wires here are two cells apart with a block between, so no edge
    /// `dust_connections` can produce joins them -- and the invariant is
    /// silent while net b's wire tracks net a's lever exactly.
    #[test]
    fn verify_connectivity_cannot_see_two_nets_coupled_by_a_lever_through_a_block() {
        let netlist = Netlist {
            inputs: vec!["a".to_string(), "b".to_string()],
            outputs: Vec::new(),
            gates: Vec::new(),
        };
        let nets = vec![
            nameless_net(Source::Lever(0)),
            nameless_net(Source::Lever(1)),
        ];

        let mut world = World::new(8, 8, 8);
        let lever_cell = Position::new(3, 1, 3);
        // Net a: the lever on its floor, and net a's own wire beside it.
        world.set(lever_cell.x, lever_cell.y - 1, lever_cell.z, stone());
        world.set(lever_cell.x, lever_cell.y, lever_cell.z, lever(true));
        let net_a_wire = Position::new(3, 1, 2);
        world.set(net_a_wire.x, net_a_wire.y - 1, net_a_wire.z, stone());
        world.set(net_a_wire.x, net_a_wire.y, net_a_wire.z, dust());
        // Net b: a route whose floor happens to be the block above the lever.
        let net_b_floor = Position::new(3, 2, 3);
        let net_b_wire = Position::new(3, 3, 3);
        world.set(net_b_floor.x, net_b_floor.y, net_b_floor.z, stone());
        world.set(net_b_wire.x, net_b_wire.y, net_b_wire.z, dust());

        let mut reservation = Reservation::new();
        reservation.insert(lever_cell, 0);
        reservation.insert(net_a_wire, 0);
        reservation.insert(net_b_wire, 1);

        // The two nets are one node, proved by driving one and reading both.
        let mut simulator = Simulator::new(world.clone());
        simulator.run_until_stable(50).expect("settles");
        assert_eq!(
            simulator.world().get(net_a_wire.x, net_a_wire.y, net_a_wire.z).power,
            15
        );
        assert_eq!(
            simulator.world().get(net_b_wire.x, net_b_wire.y, net_b_wire.z).power,
            15,
            "net b's wire reads 15 from a lever it is not connected to"
        );
        simulator
            .world_mut()
            .set(lever_cell.x, lever_cell.y, lever_cell.z, lever(false));
        simulator.run_until_stable(50).expect("settles again");
        assert_eq!(
            simulator.world().get(net_b_wire.x, net_b_wire.y, net_b_wire.z).power,
            0,
            "and it follows that lever, which is what makes this a merge"
        );

        assert_eq!(
            verify_connectivity(&world, &reservation, &netlist, &nets, &BTreeMap::new()),
            Ok(()),
            "RECORDED GAP: block-mediated coupling is outside the relation this \
             invariant walks. See docs/derived/coupling-mechanisms.md, mechanism 3."
        );
    }

    /// The same gap with a lit torch in place of the lever -- **the second
    /// shipped bug's geometry**, in which `full_adder` routed over a gate's
    /// output torch and eight of its 22 gates came out wrong.
    ///
    /// Kept separate from the lever case rather than parameterised, because
    /// the two reach the block by different arms of
    /// `taxonomy::power_emitted_toward`: a lever is isotropic-strong through
    /// its `_ => full` fallback, a torch is strong upward only. A single test
    /// would pass with either arm broken.
    #[test]
    fn verify_connectivity_cannot_see_two_nets_coupled_by_a_torch_through_a_block() {
        let netlist = Netlist {
            inputs: vec!["a".to_string(), "b".to_string()],
            outputs: Vec::new(),
            gates: Vec::new(),
        };
        let nets = vec![
            nameless_net(Source::Lever(0)),
            nameless_net(Source::Lever(1)),
        ];

        let mut world = World::new(8, 8, 8);
        // Net a's gate: a support block with a wall torch on it, lit because
        // the support is unpowered.
        let support = Position::new(3, 1, 3);
        let torch_cell = Position::new(3, 1, 4);
        world.set(support.x, support.y, support.z, stone());
        world.set(torch_cell.x, torch_cell.y, torch_cell.z, wall_torch(Facing::South));
        // Net a's own wire, driven by that torch directly.
        let net_a_wire = Position::new(4, 1, 4);
        world.set(net_a_wire.x, net_a_wire.y - 1, net_a_wire.z, stone());
        world.set(net_a_wire.x, net_a_wire.y, net_a_wire.z, dust());
        // Net b: a route laid straight over the torch.
        let net_b_floor = Position::new(3, 2, 4);
        let net_b_wire = Position::new(3, 3, 4);
        world.set(net_b_floor.x, net_b_floor.y, net_b_floor.z, stone());
        world.set(net_b_wire.x, net_b_wire.y, net_b_wire.z, dust());

        let mut reservation = Reservation::new();
        reservation.insert(net_a_wire, 0);
        reservation.insert(net_b_wire, 1);

        let mut simulator = Simulator::new(world.clone());
        simulator.run_until_stable(50).expect("settles");
        assert!(
            simulator.world().get(torch_cell.x, torch_cell.y, torch_cell.z).lit,
            "the torch must be lit, or this test measures nothing"
        );
        assert_eq!(
            simulator.world().get(net_b_wire.x, net_b_wire.y, net_b_wire.z).power,
            15,
            "net b's wire reads 15 from a gate torch it is not connected to"
        );
        // Put the torch out by powering its support, and net b follows.
        simulator
            .world_mut()
            .set(support.x, support.y - 1, support.z, lever(true));
        simulator.run_until_stable(50).expect("settles again");
        assert!(!simulator.world().get(torch_cell.x, torch_cell.y, torch_cell.z).lit);
        assert_eq!(
            simulator.world().get(net_b_wire.x, net_b_wire.y, net_b_wire.z).power,
            0,
            "and it inverts with that gate, which is what makes this a merge"
        );

        assert_eq!(
            verify_connectivity(&world, &reservation, &netlist, &nets, &BTreeMap::new()),
            Ok(()),
            "RECORDED GAP: block-mediated coupling is outside the relation this \
             invariant walks. See docs/derived/coupling-mechanisms.md, mechanism 3."
        );
    }

    /// A gap inside the one relation this invariant *does* walk: a **one-way**
    /// `dust_connections` edge whose direction runs against the walk's seed
    /// order.
    ///
    /// Seeds arrive in `World::positions_of` order, which is flat-index order,
    /// which is lowest `y` first; and `visited` is shared across seeds, so a
    /// cell already claimed by an earlier component is skipped with `continue`
    /// before its owner is ever compared. When the only edge runs downward --
    /// the upper wire descends into the lower one and the lower one cannot
    /// climb back -- the lower cell is walked first, finds nothing, and the
    /// upper cell then forms a component of its own that never meets it.
    ///
    /// The climb comes back the moment the upper wire has a floor to be
    /// climbed onto, and the control at the end of this test is that same
    /// world with one stone added: the invariant then catches it. So this is a
    /// demonstration of the mechanism, not an assertion about a rule.
    ///
    /// **NOT MEASURED: whether any circuit this compiler builds contains such
    /// a pair.** The condition is a routed wire with no floor beneath it.
    #[test]
    fn verify_connectivity_misses_a_one_way_dust_edge_that_runs_against_its_seed_order() {
        let netlist = Netlist {
            inputs: vec!["a".to_string(), "b".to_string()],
            outputs: Vec::new(),
            gates: Vec::new(),
        };
        let nets = vec![
            nameless_net(Source::Lever(0)),
            nameless_net(Source::Lever(1)),
        ];

        let upper = Position::new(2, 2, 2);
        let lower = Position::new(3, 1, 2);
        let upper_floor = Position::new(2, 1, 2);

        let build = |floored: bool| {
            let mut world = World::new(8, 8, 8);
            world.set(lower.x, lower.y - 1, lower.z, stone());
            world.set(lower.x, lower.y, lower.z, dust());
            world.set(upper.x, upper.y, upper.z, dust());
            if floored {
                world.set(upper_floor.x, upper_floor.y, upper_floor.z, stone());
            }
            // Drive the upper wire from the side away from the lower one. A
            // redstone block, because it is the one source that drives dust
            // while powering no block at all -- nothing can leak from it into
            // the two cells whose relationship is under test.
            let mut source = BlockState::air();
            source.kind = BlockKind::RedstoneBlock;
            source.name = "minecraft:redstone_block".to_string();
            world.set(upper.x - 1, upper.y, upper.z, source);
            world
        };

        let mut reservation = Reservation::new();
        reservation.insert(upper, 0);
        reservation.insert(lower, 1);

        let world = build(false);

        // The edge is real, and it is one-way.
        assert!(
            [Facing::North, Facing::South, Facing::East, Facing::West]
                .iter()
                .any(|&d| dust_connections(&world, upper, d).iter().any(|p| p == lower)),
            "the upper wire must descend into the lower one"
        );
        assert!(
            [Facing::North, Facing::South, Facing::East, Facing::West]
                .iter()
                .all(|&d| dust_connections(&world, lower, d).iter().all(|p| p != upper)),
            "and the lower one must not climb back -- its step has no floor"
        );

        // And it carries a signal, so the two nets really are one node.
        let mut simulator = Simulator::new(world.clone());
        simulator.run_until_stable(50).expect("settles");
        assert_eq!(simulator.world().get(upper.x, upper.y, upper.z).power, 15);
        assert_eq!(
            simulator.world().get(lower.x, lower.y, lower.z).power,
            14,
            "net b's wire carries net a's signal, one step decayed"
        );

        assert_eq!(
            verify_connectivity(&world, &reservation, &netlist, &nets, &BTreeMap::new()),
            Ok(()),
            "RECORDED GAP: the walk shares one `visited` set across seeds, so a \
             one-way edge into an already-claimed component is never compared. \
             See docs/derived/coupling-mechanisms.md, Table 4."
        );

        // The control, one stone different: the climb returns, the edge becomes
        // two-way, and the very same invariant catches the very same merge.
        let floored = build(true);
        assert!(
            [Facing::North, Facing::South, Facing::East, Facing::West]
                .iter()
                .any(|&d| dust_connections(&floored, lower, d).iter().any(|p| p == upper)),
            "with a floor under the upper wire the climb must fire"
        );
        let err = verify_connectivity(&floored, &reservation, &netlist, &nets, &BTreeMap::new())
            .expect_err("a two-way edge between two nets must be rejected");
        assert!(
            matches!(err, CompileError::ConnectivityViolation { .. }),
            "got {err:?}"
        );
    }

    // -----------------------------------------------------------------
    // Declared wire merges: the same geometry must discriminate
    // -----------------------------------------------------------------
    //
    // A legitimate wire-merge OR and the bug this project has hunted the
    // most -- two unrelated nets' dust touching -- are geometrically
    // identical (see `docs/superpowers/specs/2026-08-08-gate-types-and-
    // wired-or.md`, "The invariants have to allow multi-source nets,
    // carefully"). So the only honest test is a pair built from the exact
    // same world, reservation and nets, differing in nothing but the
    // netlist's own `Gate::is_merge` flag -- if that single bit does not
    // flip the outcome, the relaxation is either useless (never permits the
    // merge) or dangerous (permits more than declared).

    /// Three dust cells in an L: `a` and `b` each touch a third cell `y` --
    /// exactly the shape a wire-merge OR (`y = a | b`) is built from, and
    /// exactly the shape the connectivity bug this invariant exists to
    /// catch also looks like. `declare_merge` selects which of those two
    /// this world is claiming to be.
    fn merge_touch_fixture(declare_merge: bool) -> (Netlist, Vec<Net>, World, Reservation) {
        let netlist = Netlist {
            inputs: vec!["a".to_string(), "b".to_string()],
            outputs: Vec::new(),
            gates: vec![Gate {
                name: "m".to_string(),
                inputs: vec!["a".to_string(), "b".to_string()],
                output: "y".to_string(),
                kind: if declare_merge {
                    GateKind::Or(2)
                } else {
                    GateKind::Nor(2)
                },
            }],
        };
        let nets = vec![
            nameless_net(Source::Lever(0)),
            nameless_net(Source::Lever(1)),
            nameless_net(Source::Gate(0)),
        ];

        let mut world = World::new(6, 5, 6);
        let a_cell = Position::new(1, 1, 2);
        let y_cell = Position::new(2, 1, 2);
        let b_cell = Position::new(2, 1, 3);
        for cell in [a_cell, y_cell, b_cell] {
            world.set(cell.x, cell.y - 1, cell.z, stone());
            world.set(cell.x, cell.y, cell.z, dust());
        }

        let mut reservation = Reservation::new();
        reservation.insert(a_cell, 0);
        reservation.insert(y_cell, 2);
        reservation.insert(b_cell, 1);

        (netlist, nets, world, reservation)
    }

    #[test]
    fn verify_connectivity_accepts_the_touch_when_the_netlist_declares_a_merge() {
        let (netlist, nets, world, reservation) = merge_touch_fixture(true);

        assert_eq!(
            verify_connectivity(&world, &reservation, &netlist, &nets, &BTreeMap::new()),
            Ok(()),
            "gate `m`'s `is_merge` declares nets `a`, `b` and `y` electrically one -- their \
             dust touching is exactly what was asked for, not the bug this invariant hunts"
        );
    }

    #[test]
    fn verify_connectivity_still_rejects_the_same_touch_without_a_declared_merge() {
        let (netlist, nets, world, reservation) = merge_touch_fixture(false);

        let err = verify_connectivity(&world, &reservation, &netlist, &nets, &BTreeMap::new())
            .expect_err(
                "the identical geometry, with `is_merge` false, is nothing but three nets whose \
             dust happens to touch -- undeclared, that must still be rejected",
            );
        assert!(
            matches!(err, CompileError::ConnectivityViolation { .. }),
            "expected a connectivity violation, got: {err}"
        );
    }

    /// The same touch as `merge_touch_fixture`, but built to exercise the
    /// case that fixture's own `nets` (built by hand with an explicit `Net`
    /// for `y`) never actually reaches: a merge whose output feeds *only* a
    /// declared circuit output has no `Net` of its own at all (`build_nets`
    /// drops a signal with no gate-input sink), so `y` is simply absent
    /// from `nets` here. `MergeGroups::build` still has to union `a` and
    /// `b` with each other directly in this case -- unioning only through
    /// `y`'s own (non-existent) net index, as an earlier version of that
    /// function did, would silently un-relax the check for exactly the
    /// circuits this task builds.
    fn merge_touch_fixture_with_no_net_for_the_output(
        declare_merge: bool,
    ) -> (Netlist, Vec<Net>, World, Reservation) {
        let netlist = Netlist {
            inputs: vec!["a".to_string(), "b".to_string()],
            outputs: vec!["y".to_string()],
            gates: vec![Gate {
                name: "m".to_string(),
                inputs: vec!["a".to_string(), "b".to_string()],
                output: "y".to_string(),
                kind: if declare_merge {
                    GateKind::Or(2)
                } else {
                    GateKind::Nor(2)
                },
            }],
        };
        // No net for `y` at all -- exactly what `build_nets` would produce
        // for a merge whose output drives nothing but a declared output.
        let nets = vec![
            nameless_net(Source::Lever(0)),
            nameless_net(Source::Lever(1)),
        ];

        let mut world = World::new(6, 5, 6);
        let a_cell = Position::new(1, 1, 2);
        let y_cell = Position::new(2, 1, 2);
        let b_cell = Position::new(2, 1, 3);
        for cell in [a_cell, y_cell, b_cell] {
            world.set(cell.x, cell.y - 1, cell.z, stone());
            world.set(cell.x, cell.y, cell.z, dust());
        }

        let mut reservation = Reservation::new();
        reservation.insert(a_cell, 0);
        reservation.insert(b_cell, 1);
        // `y_cell` is deliberately unclaimed too -- nothing claims it in a
        // real `compile()` run either, when the merge has no net of its own
        // (see `emit`'s "every net's own pin" loop, which only ever runs
        // over `nets`).

        (netlist, nets, world, reservation)
    }

    #[test]
    fn verify_connectivity_accepts_a_bare_merge_touch_even_when_its_output_has_no_net_of_its_own() {
        let (netlist, nets, world, reservation) =
            merge_touch_fixture_with_no_net_for_the_output(true);

        assert_eq!(
            verify_connectivity(&world, &reservation, &netlist, &nets, &BTreeMap::new()),
            Ok(()),
            "`a` and `b` must be unioned directly with each other, not only through `y`'s own net -- \
             `y` has none here, exactly as a merge feeding only a declared output produces"
        );
    }

    #[test]
    fn verify_connectivity_still_rejects_the_same_touch_without_a_declared_merge_even_with_no_net_for_the_output(
    ) {
        let (netlist, nets, world, reservation) =
            merge_touch_fixture_with_no_net_for_the_output(false);

        let err = verify_connectivity(&world, &reservation, &netlist, &nets, &BTreeMap::new())
            .expect_err(
                "undeclared, this is still nothing but two nets whose dust happens to touch",
            );
        assert!(
            matches!(err, CompileError::ConnectivityViolation { .. }),
            "expected a connectivity violation, got: {err}"
        );
    }

    // -----------------------------------------------------------------
    // `merge_branch_is_bare`: the isolation rule itself, tested directly
    // against hand-built `Net`/`Gate` fixtures rather than through a full
    // `compile()` run -- so the decision is checked independent of
    // whatever incidental, length-driven repeaters a specific circuit's
    // general row/channel placement happens to add elsewhere (see
    // `tests/or_merge.rs`'s own notes on exactly that noise).
    // -----------------------------------------------------------------

    fn merge_gate(inputs: &[&str], output: &str) -> Gate {
        Gate {
            name: output.to_string(),
            inputs: inputs.iter().map(|s| s.to_string()).collect(),
            output: output.to_string(),
            kind: GateKind::Or(inputs.len()),
        }
    }

    fn net_with_sinks(source: Source, sinks: Vec<(usize, usize)>) -> Net {
        Net {
            source,
            source_column: 0,
            channels: vec![0],
            tracks: vec![0],
            sinks: vec![sinks],
            hops: Vec::new(),
        }
    }

    #[test]
    fn merge_branch_is_bare_when_its_net_feeds_only_the_merge() {
        let netlist = Netlist {
            inputs: vec!["a".to_string()],
            outputs: Vec::new(),
            gates: vec![merge_gate(&["a", "b"], "y")],
        };
        // `a`'s only sink is the merge's own first input.
        let net = net_with_sinks(Source::Lever(0), vec![(0, 0)]);
        assert!(
            merge_branch_is_bare(&netlist, &net, 0),
            "a's only sink is the merge itself -- this branch is private"
        );
    }

    #[test]
    fn merge_branch_is_not_bare_when_its_net_also_feeds_a_different_gate() {
        let netlist = Netlist {
            inputs: vec!["a".to_string()],
            outputs: Vec::new(),
            gates: vec![
                Gate {
                    name: "s".to_string(),
                    inputs: vec!["a".to_string()],
                    output: "s".to_string(),
                    kind: GateKind::Nor(1),
                },
                merge_gate(&["a", "b"], "y"),
            ],
        };
        // `a` feeds both gate 0 (`s`, a real consumer) and gate 1 (the
        // merge's first input) -- the fanout rule says isolate.
        let net = net_with_sinks(Source::Lever(0), vec![(0, 0), (1, 0)]);
        assert!(
            !merge_branch_is_bare(&netlist, &net, 1),
            "a's source also feeds gate 0 besides the merge -- this branch must be isolated"
        );
    }

    #[test]
    fn merge_branch_is_never_bare_for_a_non_merge_gate() {
        // The exact same net shape as the private-branch case above, but
        // the target gate itself is an ordinary NOR, not a declared merge --
        // `merge_branch_is_bare` must never apply to it, regardless of how
        // few sinks its net has, since an ordinary NOR socket always needs
        // its usual mandatory repeater to drive a real support block.
        let netlist = Netlist {
            inputs: vec!["a".to_string()],
            outputs: Vec::new(),
            gates: vec![Gate {
                name: "g0".to_string(),
                inputs: vec!["a".to_string()],
                output: "g0".to_string(),
                kind: GateKind::Nor(1),
            }],
        };
        let net = net_with_sinks(Source::Lever(0), vec![(0, 0)]);
        assert!(
            !merge_branch_is_bare(&netlist, &net, 0),
            "a non-merge gate's socket is never a bare join"
        );
    }

    #[test]
    fn merge_branch_is_still_bare_when_it_feeds_the_same_merge_on_two_sockets() {
        // An unusual shape (the same net wired to two of a merge's own
        // inputs), but the fanout rule's own wording is exact about this:
        // isolate when the source fans out to anything *besides this
        // merge*. Both sockets here belong to the one merge gate itself, so
        // there is nothing besides it to protect against -- backflow
        // between the two sockets only ever circulates `a`'s own signal
        // back into a branch that was already carrying it, which corrupts
        // nothing.
        let netlist = Netlist {
            inputs: vec!["a".to_string()],
            outputs: Vec::new(),
            gates: vec![merge_gate(&["a", "a"], "y")],
        };
        let net = net_with_sinks(Source::Lever(0), vec![(0, 0), (0, 1)]);
        assert!(
            merge_branch_is_bare(&netlist, &net, 0),
            "both sinks are this same merge -- still nothing to isolate against"
        );
    }

    // -----------------------------------------------------------------
    // The torch-merge invariant
    // -----------------------------------------------------------------
    //
    // Each test below builds a world by hand that violates exactly one
    // condition of `verify_torch_merge` -- never a circuit that merely
    // happens to trigger it -- and confirms both the specific variant and
    // the message. A single-gate `Net`/`Netlist` pair, wired directly
    // rather than through `compile`'s own placement, is enough for every
    // one of them.

    fn glass() -> BlockState {
        let mut state = BlockState::air();
        state.kind = BlockKind::Glass;
        state.name = "minecraft:glass".to_string();
        state
    }

    /// A standing torch (as opposed to `wall_torch`, already defined above
    /// for the router's own use) -- attached to the block directly below
    /// it, per `component::torch_support_position`'s `Torch` arm.
    fn standing_torch() -> BlockState {
        let mut state = BlockState::air();
        state.kind = BlockKind::Torch;
        state.name = "minecraft:redstone_torch".to_string();
        state.lit = true;
        state
    }

    fn single_input_gate(gate_output: &str) -> Netlist {
        Netlist {
            inputs: vec!["a".to_string()],
            outputs: Vec::new(),
            gates: vec![Gate {
                name: "g0".to_string(),
                inputs: vec!["a".to_string()],
                output: gate_output.to_string(),
                kind: GateKind::Nor(1),
            }],
        }
    }

    fn single_input_net() -> Net {
        Net {
            source: Source::Lever(0),
            source_column: 0,
            channels: vec![0],
            tracks: vec![0],
            sinks: vec![vec![(0, 0)]],
            hops: Vec::new(),
        }
    }

    /// A short gate-to-gate edge that can exercise a gate-input terminal.
    /// `y = !!a`; the second gate's west socket must be driven from the west
    /// and can therefore weakly power its support eastward without a terminal
    /// repeater.
    fn directed_dust_terminal_netlist() -> Netlist {
        Netlist {
            inputs: vec!["a".to_string()],
            outputs: vec!["y".to_string()],
            gates: vec![Gate::nor("not_a", &["a"]), Gate::nor("y", &["not_a"])],
        }
    }

    /// A world in the exact local shape a direct-dust terminal needs:
    /// west-running dust, the proposed terminal repeater, then the NOR's
    /// conductive support.  The router normally provides the floor blocks;
    /// these tests state them explicitly so `dust_sides` sees real Minecraft
    /// geometry rather than an abstract edge.
    fn direct_terminal_world() -> (World, Reservation, Position, Position) {
        let mut world = World::new(7, 4, 7);
        let predecessor = Position::new(1, 1, 3);
        let socket = Position::new(2, 1, 3);
        let support = Position::new(3, 1, 3);
        world.set(1, 0, 3, stone());
        world.set(2, 0, 3, stone());
        world.set(predecessor.x, predecessor.y, predecessor.z, dust());
        world.set(socket.x, socket.y, socket.z, repeater(Facing::East));
        world.set(support.x, support.y, support.z, stone());

        let mut reservation = Reservation::new();
        reservation.insert(predecessor, 0);
        reservation.insert(socket, 0);
        (world, reservation, socket, support)
    }

    #[test]
    fn directed_dust_terminal_rejects_a_corner() {
        let (mut world, mut reservation, socket, support) = direct_terminal_world();
        let north = socket.offset(Facing::North);
        world.set(north.x, 0, north.z, stone());
        world.set(north.x, north.y, north.z, dust());
        reservation.insert(north, 0);

        assert!(
            !directed_dust_terminal_is_legal(&mut world, &reservation, 0, socket, support, 2),
            "a perpendicular attachment makes terminal dust a corner, which cannot weakly power east"
        );
        assert_eq!(
            world.get(socket.x, socket.y, socket.z).kind,
            BlockKind::Repeater,
            "the probe must restore the baseline world"
        );
    }

    #[test]
    fn directed_dust_terminal_rejects_a_dead_final_hop() {
        let (mut world, reservation, socket, support) = direct_terminal_world();

        assert!(
            !directed_dust_terminal_is_legal(&mut world, &reservation, 0, socket, support, 1),
            "one-strength predecessor would decay to zero at the proposed dust terminal"
        );
    }

    #[test]
    fn directed_dust_terminal_rejects_a_foreign_adjacent_route() {
        let (mut world, mut reservation, socket, support) = direct_terminal_world();
        let south = socket.offset(Facing::South);
        world.set(south.x, 0, south.z, stone());
        world.set(south.x, south.y, south.z, dust());
        reservation.insert(south, 1);

        assert!(
            !directed_dust_terminal_is_legal(&mut world, &reservation, 0, socket, support, 2),
            "a foreign lateral wire must preserve the isolating repeater and its keep-out"
        );
    }

    #[test]
    fn directed_dust_terminal_is_live_and_powers_a_nor_support() {
        let netlist = directed_dust_terminal_netlist();
        // `compile_legacy`, because `resolve_directed_dust_terminals` is the
        // emitter's own pass and the socket this reads is derived from a
        // north-facing gate. The hybrid `compile` would place this by
        // relaxation, which turns gates and chooses its terminals in
        // `realise_branch_from` instead -- a different mechanism, covered by
        // `planner::tests::a_planned_terminal_style_is_what_the_world_holds`.
        let compiled = compile_legacy(&netlist).expect("a one-input NOR must compile");

        let (torch_x, torch_y, torch_z) = *compiled
            .gate_output_positions
            .get("y")
            .expect("gate output must be recorded");
        let torch = Position::new(torch_x, torch_y, torch_z);
        let support = torch_support_position(compiled.world.get(torch.x, torch.y, torch.z), torch)
            .expect("the NOR output must be a supported torch");
        let socket = support.offset(Facing::West);

        assert_eq!(
            compiled.world.get(socket.x, socket.y, socket.z).kind,
            BlockKind::RedstoneWire,
            "a legal straight terminal must be dust, not the old mandatory repeater"
        );
        assert!(
            dust_powers_block_toward(&compiled.world, socket, Facing::East),
            "the terminal dust must be a straight live run pointing into the NOR support"
        );

        let input = *compiled
            .input_positions
            .get("a")
            .expect("input lever must be recorded");
        let output = *compiled
            .output_positions
            .get("y")
            .expect("output lamp must be recorded");
        let mut simulator = Simulator::new(compiled.world);
        simulator
            .run_until_stable(200)
            .expect("the compiled NOT must settle");
        for (on, expected) in [(false, false), (true, true)] {
            let mut lever_state = simulator.world().get(input.0, input.1, input.2).clone();
            lever_state.lit = on;
            simulator
                .world_mut()
                .set(input.0, input.1, input.2, lever_state);
            simulator
                .run_until_stable(200)
                .expect("the compiled NOT must settle after an input change");
            assert_eq!(
                simulator.world().get(output.0, output.1, output.2).lit,
                expected,
                "NOT(NOT({on}))"
            );
        }
    }

    #[test]
    fn directed_dust_terminals_cover_a_real_verilog_and4_merge_output() {
        let circuit =
            crate::circuits::verilog::find("verilog:and4").expect("the shipped circuit must exist");
        let (gate_level, _) = circuit.baked_netlist();
        let netlist = crate::compile::lowering::lower_optimised(&gate_level)
            .expect("the shipped netlist must lower");
        // `compile_legacy` for the same reason as the test above: the claim
        // being made is about `resolve_directed_dust_terminals` replacing the
        // emitter's mandatory terminal repeaters, and that pass runs only on
        // the emitter's path.
        let compiled = compile_legacy(&netlist).expect("the lowered circuit must compile");

        let direct_terminal_count = netlist
            .gates
            .iter()
            .enumerate()
            .filter(|(_, gate)| !gate.is_merge())
            .flat_map(|(g, gate)| {
                let &(x, y, z) = compiled
                    .gate_output_positions
                    .get(&gate.output)
                    .expect("every gate has a recorded output");
                let torch = Position::new(x, y, z);
                let support = torch_support_position(compiled.world.get(x, y, z), torch)
                    .expect("NOR has a support");
                geometry::gate_sockets(support, gate.inputs.len(), compiled.gate_facings[g])
                    .into_iter()
            })
            .filter(|socket| {
                compiled.world.get(socket.x, socket.y, socket.z).kind == BlockKind::RedstoneWire
            })
            .count();

        assert!(
            direct_terminal_count > 0,
            "verilog:and4 has merge-derived signals whose legal straight dust endpoints should replace at least one terminal repeater"
        );
    }

    #[test]
    fn torch_merge_rejects_an_output_that_is_not_a_torch_at_all() {
        let netlist = single_input_gate("out");
        let nets = vec![single_input_net()];

        let mut world = World::new(5, 5, 5);
        // Plain stone standing in for what should be the output torch --
        // `torch_support_position` returns `None` for every kind but
        // `Torch`/`WallTorch`.
        world.set(2, 1, 2, stone());

        let mut gate_output_positions = BTreeMap::new();
        gate_output_positions.insert("out".to_string(), (2, 1, 2));

        let err = verify_torch_merge(
            &world,
            &Reservation::new(),
            &netlist,
            &nets,
            &gate_output_positions,
        )
        .expect_err("a plain block standing in for the output torch must be rejected");

        assert_eq!(
            err,
            CompileError::TorchMergeViolation {
                gate: "g0".to_string(),
                reason: TorchMergeFailure::NoSupport { torch: (2, 1, 2) },
            }
        );
        assert!(
            err.to_string().contains("g0"),
            "message must name the gate: {err}"
        );
    }

    #[test]
    fn torch_merge_rejects_a_support_block_that_is_not_conductive() {
        let netlist = single_input_gate("out");
        let nets = vec![single_input_net()];

        let mut world = World::new(5, 5, 5);
        // Glass is a full cube -- it holds a standing torch just fine
        // (`SUPPORT_CENTER`) -- but `taxonomy::flags_of` marks it
        // explicitly non-conductive, so `propagate::block_signal_at` can
        // never observe it as powered no matter what surrounds it.
        world.set(2, 0, 2, glass());
        world.set(2, 1, 2, standing_torch());

        let mut gate_output_positions = BTreeMap::new();
        gate_output_positions.insert("out".to_string(), (2, 1, 2));

        let err = verify_torch_merge(
            &world,
            &Reservation::new(),
            &netlist,
            &nets,
            &gate_output_positions,
        )
        .expect_err("a non-conductive support must be rejected -- this torch could never invert");

        assert_eq!(
            err,
            CompileError::TorchMergeViolation {
                gate: "g0".to_string(),
                reason: TorchMergeFailure::SupportNotConductive {
                    torch: (2, 1, 2),
                    support: (2, 0, 2)
                },
            }
        );
        let message = err.to_string();
        assert!(
            message.contains("g0") && message.contains("(2, 0, 2)"),
            "message: {message}"
        );
    }

    #[test]
    fn torch_merge_rejects_a_declared_input_whose_net_never_reaches_the_support() {
        let netlist = single_input_gate("out");
        let nets = vec![single_input_net()];

        let mut world = World::new(5, 5, 5);
        world.set(2, 0, 2, stone()); // the torch's support -- conductive, correctly attached
        world.set(2, 1, 2, standing_torch());

        // The netlist declares input "a", but its route was never laid --
        // `reservation` has no cells for net 0 at all, so nothing can
        // structurally reach the support no matter how far the flood runs.
        let reservation = Reservation::new();

        let mut gate_output_positions = BTreeMap::new();
        gate_output_positions.insert("out".to_string(), (2, 1, 2));

        let err = verify_torch_merge(
            &world,
            &reservation,
            &netlist,
            &nets,
            &gate_output_positions,
        )
        .expect_err(
            "an input net with no conductor at all must not be accepted as reaching the support",
        );

        assert_eq!(
            err,
            CompileError::TorchMergeViolation {
                gate: "g0".to_string(),
                reason: TorchMergeFailure::InputDoesNotReachSupport {
                    torch: (2, 1, 2),
                    support: (2, 0, 2),
                    input: "a".to_string(),
                },
            }
        );
        let message = err.to_string();
        assert!(
            message.contains('a') && message.contains("g0"),
            "message: {message}"
        );
    }

    #[test]
    fn torch_merge_rejects_a_foreign_net_that_reaches_the_support() {
        // g0 declares *no* inputs -- so nothing may reach its support at
        // all -- but net "b" (undeclared: its `sinks` never mention g0)
        // has a repeater whose output lands directly on the support block,
        // strongly powering it regardless.
        let netlist = Netlist {
            inputs: vec!["b".to_string()],
            outputs: Vec::new(),
            gates: vec![Gate {
                name: "g0".to_string(),
                inputs: Vec::new(),
                output: "out".to_string(),
                kind: GateKind::Nor(0),
            }],
        };
        let nets = vec![Net {
            source: Source::Lever(0),
            source_column: 0,
            channels: Vec::new(),
            tracks: Vec::new(),
            sinks: vec![Vec::new()],
            hops: Vec::new(),
        }];

        let mut world = World::new(5, 5, 5);
        world.set(2, 0, 2, stone()); // g0's support
        world.set(2, 1, 2, standing_torch());
        // West of the support, outputting straight into it.
        world.set(1, 0, 2, repeater(Facing::East));

        let mut reservation = Reservation::new();
        reservation.insert(Position::new(1, 0, 2), 0);

        let mut gate_output_positions = BTreeMap::new();
        gate_output_positions.insert("out".to_string(), (2, 1, 2));

        let err = verify_torch_merge(
            &world,
            &reservation,
            &netlist,
            &nets,
            &gate_output_positions,
        )
        .expect_err("a repeater from an undeclared net feeding the support must be rejected");

        assert_eq!(
            err,
            CompileError::TorchMergeViolation {
                gate: "g0".to_string(),
                reason: TorchMergeFailure::ForeignNetReachesSupport {
                    torch: (2, 1, 2),
                    support: (2, 0, 2),
                    net: "b".to_string(),
                },
            }
        );
        let message = err.to_string();
        assert!(
            message.contains('b') && message.contains("g0"),
            "message: {message}"
        );
    }

    #[test]
    fn torch_merge_rejects_a_torch_that_leaks_its_output_into_a_foreign_net() {
        // g0 declares no inputs and feeds no other gate (`output_net` will
        // find no legitimate net at all for it), so the torch's own
        // structural power must reach *nothing* claimed by any net. A
        // stray wire sits directly above it -- one of the four directions
        // (plus the support itself) a standing torch always powers.
        let netlist = Netlist {
            inputs: vec!["leak".to_string()],
            outputs: Vec::new(),
            gates: vec![Gate {
                name: "g0".to_string(),
                inputs: Vec::new(),
                output: "out".to_string(),
                kind: GateKind::Nor(0),
            }],
        };
        let nets = vec![Net {
            source: Source::Lever(0),
            source_column: 0,
            channels: Vec::new(),
            tracks: Vec::new(),
            sinks: vec![Vec::new()],
            hops: Vec::new(),
        }];

        let mut world = World::new(5, 5, 5);
        world.set(2, 0, 2, stone());
        world.set(2, 1, 2, standing_torch());
        world.set(2, 2, 2, dust()); // directly above the torch

        let mut reservation = Reservation::new();
        reservation.insert(Position::new(2, 2, 2), 0);

        let mut gate_output_positions = BTreeMap::new();
        gate_output_positions.insert("out".to_string(), (2, 1, 2));

        let err = verify_torch_merge(
            &world,
            &reservation,
            &netlist,
            &nets,
            &gate_output_positions,
        )
        .expect_err("a torch powering a foreign net's wire must be rejected");

        assert_eq!(
            err,
            CompileError::TorchMergeViolation {
                gate: "g0".to_string(),
                reason: TorchMergeFailure::OutputLeaksIntoForeignNet {
                    torch: (2, 1, 2),
                    leaked_cell: (2, 2, 2),
                    net: "leak".to_string(),
                },
            }
        );
        let message = err.to_string();
        assert!(
            message.contains("leak") && message.contains("g0"),
            "message: {message}"
        );
    }

    /// The positive case, built with the same by-hand machinery as the
    /// five violations above: a correctly wired single-input gate must
    /// pass every condition. Without this, a check that only ever fails
    /// would not be distinguishable from one that works.
    #[test]
    fn torch_merge_accepts_a_correctly_wired_single_input_gate() {
        let netlist = single_input_gate("out");
        let nets = vec![single_input_net()];

        let mut world = World::new(5, 5, 5);
        world.set(2, 0, 2, stone());
        world.set(2, 1, 2, standing_torch());
        world.set(1, 0, 2, repeater(Facing::East)); // net "a"'s own conductor, facing straight into the support

        let mut reservation = Reservation::new();
        reservation.insert(Position::new(1, 0, 2), 0);

        let mut gate_output_positions = BTreeMap::new();
        gate_output_positions.insert("out".to_string(), (2, 1, 2));

        assert_eq!(
            verify_torch_merge(
                &world,
                &reservation,
                &netlist,
                &nets,
                &gate_output_positions
            ),
            Ok(())
        );
    }

    // -----------------------------------------------------------------
    // Declared wire merges: the same geometry must discriminate, here too
    // -----------------------------------------------------------------
    //
    // The connectivity pair above proves the touch itself is allowed only
    // when declared. This proves the other half: a gate that *consumes* a
    // merge's output must see the merge's own branches as its declared
    // input, not as foreign nets corrupting its support -- and must stop
    // seeing them that way the moment the declaration is removed, with
    // nothing else about the world changed.
    //
    // Two independent repeaters -- one per branch -- drive the same support
    // block directly, from different faces (exactly how any multi-input NOR
    // already merges its own inputs for free via block power; the only new
    // thing here is that `g1` declares a *single* input, `y`, standing for
    // both). `y` itself owns no physical cell at all: it is realised
    // entirely as "`a`'s repeater plus `b`'s repeater", so a correct
    // implementation has to resolve `y`'s own reach through its *group*
    // (`a` and `b`'s cells), not through an empty cell list of its own --
    // this is what actually exercises `verify_torch_merge`'s group-based
    // reach, not just the `declared`-set widening.
    struct MergeConsumerFixture {
        netlist: Netlist,
        nets: Vec<Net>,
        world: World,
        reservation: Reservation,
        gate_output_positions: BTreeMap<String, (i32, i32, i32)>,
    }

    fn merge_consumer_fixture(declare_merge: bool) -> MergeConsumerFixture {
        let netlist = Netlist {
            inputs: vec!["a".to_string(), "b".to_string()],
            outputs: Vec::new(),
            gates: vec![
                // Checked first (see below): with no merge declared, this
                // is where the rejection must fire.
                Gate {
                    name: "g1".to_string(),
                    inputs: vec!["y".to_string()],
                    output: "out".to_string(),
                    kind: GateKind::Nor(1),
                },
                // `m` is never actually reached by `verify_torch_merge`'s
                // own loop in either scenario: declared, it is skipped
                // outright (`is_merge`); undeclared, `g1` above already
                // returns an error before the loop gets this far. So it
                // needs no torch and no `gate_output_positions` entry at
                // all -- exactly the point `is_merge` exists to make.
                Gate {
                    name: "m".to_string(),
                    inputs: vec!["a".to_string(), "b".to_string()],
                    output: "y".to_string(),
                    kind: if declare_merge {
                        GateKind::Or(2)
                    } else {
                        GateKind::Nor(2)
                    },
                },
            ],
        };

        let nets = vec![
            nameless_net(Source::Lever(0)), // a
            nameless_net(Source::Lever(1)), // b
            Net {
                source: Source::Gate(1), // "m"
                source_column: 0,
                channels: Vec::new(),
                tracks: Vec::new(),
                sinks: vec![vec![(0, 0)]], // consumed by g1's own (only) input
                hops: Vec::new(),
            }, // y
        ];

        let mut world = World::new(6, 5, 6);
        let support = Position::new(4, 0, 2);
        let torch = Position::new(4, 1, 2);
        let repeater_a = Position::new(3, 0, 2); // west of the support, facing east into it
        let repeater_b = Position::new(4, 0, 1); // north of the support, facing south into it
        world.set(support.x, support.y, support.z, stone());
        world.set(torch.x, torch.y, torch.z, standing_torch());
        world.set(
            repeater_a.x,
            repeater_a.y,
            repeater_a.z,
            repeater(Facing::East),
        );
        world.set(
            repeater_b.x,
            repeater_b.y,
            repeater_b.z,
            repeater(Facing::South),
        );

        let mut reservation = Reservation::new();
        reservation.insert(repeater_a, 0);
        reservation.insert(repeater_b, 1);
        // Net `y` (index 2) is deliberately given no cell of its own.

        let mut gate_output_positions = BTreeMap::new();
        gate_output_positions.insert("out".to_string(), (torch.x, torch.y, torch.z));

        MergeConsumerFixture {
            netlist,
            nets,
            world,
            reservation,
            gate_output_positions,
        }
    }

    #[test]
    fn torch_merge_accepts_both_branches_of_a_declared_merge_as_g1s_input() {
        let MergeConsumerFixture {
            netlist,
            nets,
            world,
            reservation,
            gate_output_positions,
        } = merge_consumer_fixture(true);

        assert_eq!(
            verify_torch_merge(
                &world,
                &reservation,
                &netlist,
                &nets,
                &gate_output_positions
            ),
            Ok(()),
            "`m`'s `is_merge` declares `a` and `b` as the same net `y`, which `g1` declares as \
             its own input -- both branches reaching the support is exactly what was asked for"
        );
    }

    #[test]
    fn torch_merge_still_rejects_the_same_two_repeaters_without_a_declared_merge() {
        let MergeConsumerFixture {
            netlist,
            nets,
            world,
            reservation,
            gate_output_positions,
        } = merge_consumer_fixture(false);

        let err = verify_torch_merge(
            &world,
            &reservation,
            &netlist,
            &nets,
            &gate_output_positions,
        )
        .expect_err(
            "the identical world, with `is_merge` false on `m`, is nothing but an undeclared \
             net reaching g1's support -- `g1` only ever declared `y`, never `a`",
        );
        assert!(
            matches!(
                err,
                CompileError::TorchMergeViolation {
                    reason: TorchMergeFailure::ForeignNetReachesSupport { .. },
                    ..
                }
            ),
            "expected a foreign-net violation naming the undeclared branch, got: {err}"
        );
    }

    // -----------------------------------------------------------------
    // The signal-strength invariant
    // -----------------------------------------------------------------
    //
    // Every test below builds, by hand, a world that is perfectly clean by
    // the first two invariants' own standards -- one electrically coherent
    // network per net (`verify_connectivity`), every declared input
    // structurally reaching its gate's support and nothing else
    // (`verify_torch_merge`) -- and confirms both of them agree it is fine,
    // *before* checking that `verify_signal_strength` is the one that
    // catches it anyway. That double-check is the whole point: it proves
    // the failure mode this invariant exists for is invisible to the other
    // two, not merely untested by them.
    //
    // The reason `verify_connectivity`/`verify_torch_merge` cannot see any
    // of this is the same reason every time: both reason about a net's
    // reservation as a single, timelessly "in the network" set of cells
    // (`verify_torch_merge`'s `net_reach` literally seeds every one of a
    // net's own claimed cells into its flood at once), never about whether
    // a real signal, starting at the true source and losing strength one
    // hop at a time, would ever actually arrive there. A cell can be
    // legitimately claimed by a net and still be electrically orphaned from
    // everything upstream of it -- that gap is invisible to a check that
    // never asks "how did the signal get here", only "is this consistent
    // with everything else claimed for this net".

    /// Lay `len` cells of plain dust from `start` (inclusive) along `+x`,
    /// each claimed for `net`. Returns the position one past the last cell
    /// laid -- where a caller's next component goes.
    fn lay_test_dust_run(
        world: &mut World,
        reservation: &mut Reservation,
        start: Position,
        len: i32,
        net: usize,
    ) -> Position {
        for i in 0..len {
            let pos = Position::new(start.x + i, start.y, start.z);
            world.set(pos.x, pos.y, pos.z, dust());
            reservation.insert(pos, net);
        }
        Position::new(start.x + len, start.y, start.z)
    }

    /// A gate at `pos`: a stone support with a standing torch directly on
    /// top, exactly like every other torch-merge fixture above. Returns the
    /// torch's own position (what `gate_output_positions` records).
    fn place_test_gate(world: &mut World, support: Position) -> Position {
        world.set(support.x, support.y, support.z, stone());
        let torch = support.up();
        world.set(torch.x, torch.y, torch.z, standing_torch());
        torch
    }

    #[test]
    fn signal_strength_rejects_a_dust_run_one_block_too_long() {
        // Lever -> 16 plain dust cells -> mandatory terminating repeater ->
        // support. Sixteen cells is one too many: the pin itself (the first
        // cell, directly lit by the lever) carries 15, so the sixteenth
        // cell -- the sixteenth hop -- is the first one to actually reach
        // zero, and the repeater immediately after it reads a dead input.
        // No repeater anywhere in the run to refresh it -- the simplest
        // possible way to trigger this invariant.
        let netlist = single_input_gate("out");
        let nets = vec![single_input_net()];

        let mut world = World::new(25, 5, 5);
        let lever_pos = Position::new(0, 0, 2);
        world.set(lever_pos.x, lever_pos.y, lever_pos.z, lever(false));

        let mut reservation = Reservation::new();
        let after_run =
            lay_test_dust_run(&mut world, &mut reservation, Position::new(1, 0, 2), 16, 0);
        world.set(
            after_run.x,
            after_run.y,
            after_run.z,
            repeater(Facing::East),
        );
        reservation.insert(after_run, 0);

        let support = after_run.offset(Facing::East);
        let torch = place_test_gate(&mut world, support);

        let mut input_positions = BTreeMap::new();
        input_positions.insert("a".to_string(), (lever_pos.x, lever_pos.y, lever_pos.z));
        let mut gate_output_positions = BTreeMap::new();
        gate_output_positions.insert("out".to_string(), (torch.x, torch.y, torch.z));
        let output_positions = BTreeMap::new();

        assert_eq!(
            verify_connectivity(&world, &reservation, &netlist, &nets, &BTreeMap::new()),
            Ok(()),
            "one continuous, uncontested dust network must satisfy connectivity"
        );
        assert_eq!(
            verify_torch_merge(
                &world,
                &reservation,
                &netlist,
                &nets,
                &gate_output_positions
            ),
            Ok(()),
            "net_reach seeds every claimed cell at once, so it never notices the run is too long"
        );

        let err = verify_signal_strength(
            &world,
            &reservation,
            &netlist,
            &nets,
            &gate_output_positions,
            &input_positions,
            &output_positions,
        )
        .expect_err("a 16-cell run with no refresh must never deliver a signal to the support");

        assert_eq!(
            err,
            CompileError::SignalStrengthViolation {
                net: "a".to_string(),
                sink: SignalSink::GateInput {
                    gate: "g0".to_string(),
                    support: (support.x, support.y, support.z)
                },
            }
        );
        let message = err.to_string();
        assert!(
            message.contains("g0")
                && message.contains(&format!("{:?}", (support.x, support.y, support.z))),
            "message must name the gate and the unreached support: {message}"
        );
    }

    #[test]
    fn signal_strength_rejects_a_repeater_refreshing_into_an_invalid_cell() {
        // Exactly the failure the layering work hit: a repeater's own
        // designated output cell is deliberately left as air (the shape
        // `move_between_layers`'s descend rule requires -- see its own doc
        // comment), while the *real* continuation of the route is a
        // diagonal dust cell one level down that a repeater's straight-line
        // output can never actually reach. That diagonal cell is still
        // claimed by the net (the router laid it, meaning to connect it),
        // and a second repeater sits past it, correctly oriented, ready to
        // carry the signal the rest of the way to the support -- so
        // structurally the net still looks completely fine, right up until
        // a real signal tries to cross the gap.
        let netlist = single_input_gate("out");
        let nets = vec![single_input_net()];

        let mut world = World::new(10, 5, 5);
        let lever_pos = Position::new(0, 1, 2);
        world.set(lever_pos.x, lever_pos.y, lever_pos.z, lever(false));

        let mut reservation = Reservation::new();
        let pin = Position::new(1, 1, 2);
        world.set(pin.x, pin.y, pin.z, dust());
        reservation.insert(pin, 0);

        let repeater_a = Position::new(2, 1, 2);
        world.set(
            repeater_a.x,
            repeater_a.y,
            repeater_a.z,
            repeater(Facing::East),
        );
        reservation.insert(repeater_a, 0);

        // (3, 1, 2) -- repeater A's own designated output cell -- is left
        // as air on purpose. `landing`, the cell the real connection needed
        // to continue from, sits diagonally: one step further along and one
        // level down.
        let landing = Position::new(3, 0, 2);
        world.set(landing.x, landing.y, landing.z, dust());
        reservation.insert(landing, 0);

        let repeater_b = Position::new(4, 0, 2);
        world.set(
            repeater_b.x,
            repeater_b.y,
            repeater_b.z,
            repeater(Facing::East),
        );
        reservation.insert(repeater_b, 0);

        let support = Position::new(5, 0, 2);
        let torch = place_test_gate(&mut world, support);

        let mut input_positions = BTreeMap::new();
        input_positions.insert("a".to_string(), (lever_pos.x, lever_pos.y, lever_pos.z));
        let mut gate_output_positions = BTreeMap::new();
        gate_output_positions.insert("out".to_string(), (torch.x, torch.y, torch.z));
        let output_positions = BTreeMap::new();

        assert_eq!(
            verify_connectivity(&world, &reservation, &netlist, &nets, &BTreeMap::new()),
            Ok(()),
            "every claimed cell here is its own isolated island -- no cross-net merge for connectivity to catch"
        );
        assert_eq!(
            verify_torch_merge(&world, &reservation, &netlist, &nets, &gate_output_positions),
            Ok(()),
            "net_reach seeds `repeater_b` directly (it is one of the net's own claimed cells) and finds \
             it structurally drives the support, regardless of whether anything upstream ever reaches it"
        );

        let err = verify_signal_strength(
            &world,
            &reservation,
            &netlist,
            &nets,
            &gate_output_positions,
            &input_positions,
            &output_positions,
        )
        .expect_err(
            "a repeater whose output lands on air must never deliver a signal past the gap",
        );

        assert_eq!(
            err,
            CompileError::SignalStrengthViolation {
                net: "a".to_string(),
                sink: SignalSink::GateInput {
                    gate: "g0".to_string(),
                    support: (support.x, support.y, support.z)
                },
            }
        );
        let message = err.to_string();
        assert!(
            message.contains("g0")
                && message.contains(&format!("{:?}", (support.x, support.y, support.z))),
            "message must name the gate and the unreached support: {message}"
        );
    }

    #[test]
    fn signal_strength_rejects_a_sink_that_is_structurally_reachable_but_dead() {
        // Two segments, not one: the first repeater is real and genuinely
        // fires (fed by a run well inside budget), refreshing the signal
        // back to full strength -- proof this net is not simply
        // disconnected or trivially broken. The *second* segment, after
        // that perfectly good repeater, is on its own one block too long,
        // so the final mandatory repeater into the support never fires.
        // `verify_torch_merge` -- which never decays anything, and would
        // call this support "reached" even with *no* repeaters firing at
        // all -- has no way to tell this apart from a working net; only
        // walking the real, cumulative decay does.
        let netlist = single_input_gate("out");
        let nets = vec![single_input_net()];

        let mut world = World::new(45, 5, 5);
        let lever_pos = Position::new(0, 0, 2);
        world.set(lever_pos.x, lever_pos.y, lever_pos.z, lever(false));

        let mut reservation = Reservation::new();
        // Segment 1: pin + 12 more cells (13 cells total, hops 0..12), the
        // last one still carrying strength 3 -- comfortably non-zero.
        let after_first_run =
            lay_test_dust_run(&mut world, &mut reservation, Position::new(1, 0, 2), 13, 0);
        world.set(
            after_first_run.x,
            after_first_run.y,
            after_first_run.z,
            repeater(Facing::East),
        );
        reservation.insert(after_first_run, 0);

        // Segment 2: 16 plain cells after the (genuinely firing) repeater --
        // one too many again, exactly like the single-segment case, just
        // moved one refresh later.
        let segment_two_start = after_first_run.offset(Facing::East);
        let after_second_run =
            lay_test_dust_run(&mut world, &mut reservation, segment_two_start, 16, 0);
        world.set(
            after_second_run.x,
            after_second_run.y,
            after_second_run.z,
            repeater(Facing::East),
        );
        reservation.insert(after_second_run, 0);

        let support = after_second_run.offset(Facing::East);
        let torch = place_test_gate(&mut world, support);

        let mut input_positions = BTreeMap::new();
        input_positions.insert("a".to_string(), (lever_pos.x, lever_pos.y, lever_pos.z));
        let mut gate_output_positions = BTreeMap::new();
        gate_output_positions.insert("out".to_string(), (torch.x, torch.y, torch.z));
        let output_positions = BTreeMap::new();

        assert_eq!(
            verify_connectivity(&world, &reservation, &netlist, &nets, &BTreeMap::new()),
            Ok(())
        );
        assert_eq!(
            verify_torch_merge(
                &world,
                &reservation,
                &netlist,
                &nets,
                &gate_output_positions
            ),
            Ok(()),
            "a torch-merge check that never decays anything calls the support reached \
             regardless of how many segments, or how long, sit between the source and it"
        );

        let err = verify_signal_strength(
            &world,
            &reservation,
            &netlist,
            &nets,
            &gate_output_positions,
            &input_positions,
            &output_positions,
        )
        .expect_err("the second, independently too-long segment must still zero out the support");

        assert_eq!(
            err,
            CompileError::SignalStrengthViolation {
                net: "a".to_string(),
                sink: SignalSink::GateInput {
                    gate: "g0".to_string(),
                    support: (support.x, support.y, support.z)
                },
            }
        );
    }

    /// The positive case: the same two-segment shape as the test above, but
    /// with the second segment shortened to comfortably fit in one budget
    /// (10 cells instead of 16) -- so both repeaters genuinely fire and the
    /// support is genuinely reached. Without this, nothing distinguishes a
    /// signal-strength check that works from one that always fails, and
    /// this project has already shipped two bugs whose tests could not
    /// tell the difference (see this module's own "signal-strength
    /// invariant" section doc comment).
    #[test]
    fn signal_strength_accepts_a_correctly_refreshed_multi_segment_net() {
        let netlist = single_input_gate("out");
        let nets = vec![single_input_net()];

        let mut world = World::new(35, 5, 5);
        let lever_pos = Position::new(0, 0, 2);
        world.set(lever_pos.x, lever_pos.y, lever_pos.z, lever(false));

        let mut reservation = Reservation::new();
        let after_first_run =
            lay_test_dust_run(&mut world, &mut reservation, Position::new(1, 0, 2), 13, 0);
        world.set(
            after_first_run.x,
            after_first_run.y,
            after_first_run.z,
            repeater(Facing::East),
        );
        reservation.insert(after_first_run, 0);

        let segment_two_start = after_first_run.offset(Facing::East);
        let after_second_run =
            lay_test_dust_run(&mut world, &mut reservation, segment_two_start, 10, 0);
        world.set(
            after_second_run.x,
            after_second_run.y,
            after_second_run.z,
            repeater(Facing::East),
        );
        reservation.insert(after_second_run, 0);

        let support = after_second_run.offset(Facing::East);
        let torch = place_test_gate(&mut world, support);

        let mut input_positions = BTreeMap::new();
        input_positions.insert("a".to_string(), (lever_pos.x, lever_pos.y, lever_pos.z));
        let mut gate_output_positions = BTreeMap::new();
        gate_output_positions.insert("out".to_string(), (torch.x, torch.y, torch.z));
        let output_positions = BTreeMap::new();

        assert_eq!(
            verify_connectivity(&world, &reservation, &netlist, &nets, &BTreeMap::new()),
            Ok(())
        );
        assert_eq!(
            verify_torch_merge(
                &world,
                &reservation,
                &netlist,
                &nets,
                &gate_output_positions
            ),
            Ok(())
        );
        assert_eq!(
            verify_signal_strength(
                &world,
                &reservation,
                &netlist,
                &nets,
                &gate_output_positions,
                &input_positions,
                &output_positions,
            ),
            Ok(()),
            "a correctly refreshed multi-segment net must satisfy the signal-strength invariant"
        );
    }

    #[test]
    fn signal_strength_rejects_an_unreached_declared_output_lamp() {
        // The other kind of sink this invariant checks: a declared circuit
        // output's lamp, fed directly by its driving gate's own output
        // torch -- normally a single, unbreakable hop (`emit`'s own
        // "Every netlist output gets a lamp" doc comment), but checked
        // directly here rather than assumed, exactly like every other
        // condition in this module. A wall torch with no recorded `facing`
        // cannot resolve a direction to drive at all, so it cannot reach
        // the lamp's pin either -- the simplest way to construct the
        // failure without needing a real routing bug to produce it.
        let netlist = Netlist {
            inputs: Vec::new(),
            outputs: vec!["out".to_string()],
            gates: vec![Gate {
                name: "g0".to_string(),
                inputs: Vec::new(),
                output: "out".to_string(),
                kind: GateKind::Nor(0),
            }],
        };
        let nets: Vec<Net> = Vec::new();

        let mut world = World::new(5, 5, 5);
        let mut torch = wall_torch(Facing::North);
        torch.facing = None; // structurally incapable of driving any direction
        world.set(2, 1, 2, torch);
        world.set(2, 1, 1, dust()); // the pin a correctly-facing torch would drive
        world.set(2, 0, 1, lamp());

        let reservation = Reservation::new();
        let gate_output_positions = BTreeMap::from([("out".to_string(), (2, 1, 2))]);
        let input_positions = BTreeMap::new();
        let output_positions = BTreeMap::from([("out".to_string(), (2, 0, 1))]);

        let err = verify_signal_strength(
            &world,
            &reservation,
            &netlist,
            &nets,
            &gate_output_positions,
            &input_positions,
            &output_positions,
        )
        .expect_err("a torch that cannot resolve a drive direction must never be treated as lighting its lamp");

        assert_eq!(
            err,
            CompileError::SignalStrengthViolation {
                net: "out".to_string(),
                sink: SignalSink::OutputLamp {
                    output: "out".to_string(),
                    lamp: (2, 0, 1)
                },
            }
        );
        let message = err.to_string();
        assert!(
            message.contains("out") && message.contains("(2, 0, 1)"),
            "message: {message}"
        );
    }

    /// The six circuits the Stage 3 condition names, lowered exactly the way
    /// their own acceptance tests lower them.
    ///
    /// The four hand-written ones are pure NOR and need no lowering. The two
    /// Verilog ones arrive by `baked_netlist` rather than by synthesis, so this
    /// needs no Yosys and gives the same answer on every machine, and each is
    /// lowered by the function that ships it -- `lower` for `verilog:and4`,
    /// `lower_optimised` for `verilog:seven_segment`. Compiling through a
    /// lowering nothing ships would answer a question nobody asked.
    ///
    /// Shared by every measurement below rather than written out per test:
    /// `planner::tests::the_six_condition_circuits_stage_by_stage` carries the
    /// same list, and two lists that are meant to be the same list drift.
    pub(crate) fn the_six_condition_netlists() -> Vec<(&'static str, Netlist)> {
        use crate::circuits::and4::build_and4_netlist;
        use crate::circuits::full_adder::build_full_adder_netlist;
        use crate::circuits::seven_segment::{
            build_seven_segment_netlist, build_single_segment_netlist,
        };
        use crate::compile::lowering::{lower, lower_optimised};

        let lowered = |name: &str, optimised: bool| -> Netlist {
            let circuit = crate::circuits::verilog::find(name)
                .unwrap_or_else(|| panic!("{name} must be in the catalog"));
            let (netlist, _) = circuit.baked_netlist();
            if optimised { lower_optimised(&netlist) } else { lower(&netlist) }
                .unwrap_or_else(|error| panic!("{name} must lower: {error}"))
        };

        vec![
            ("and4", build_and4_netlist().0),
            ("full_adder", build_full_adder_netlist().0),
            ("segment_a", build_single_segment_netlist(0).0),
            ("seven_segment", build_seven_segment_netlist().0),
            ("verilog:and4", lowered("verilog:and4", false)),
            ("verilog:seven_segment", lowered("verilog:seven_segment", true)),
        ]
    }

    /// Which of `compile`'s two paths each of the six condition circuits
    /// takes, pinned.
    ///
    /// `tests/reference_circuits.rs` pins the same thing for the four
    /// hand-written circuits; this is here because the two Verilog ones need
    /// `lowering`, and because the third *kind* of fallback only appears among
    /// them. The three that fall back fail in three different places:
    ///
    /// | circuit | gates | path | why |
    /// |---|---|---|---|
    /// | and4 | 7 | `Unified3d` | routes on rip-up round 1 |
    /// | full_adder | 22 | `Unified3d` | routes on rip-up round 5 |
    /// | segment_a | 46 | `Legacy` | tried and failed: `no safe local route` |
    /// | seven_segment | 84 | `Legacy` | tried and failed: `no safe local route` |
    /// | verilog:and4 | 9 | `Unified3d` | routes on rip-up round 1 |
    /// | verilog:seven_segment | 47 | `Legacy` | **never tried**: 23 shared merge branches |
    ///
    /// The last row is the one worth having, and the reason it falls back is
    /// not the obvious one. It is the only circuit here with a merge at all,
    /// so `planner_can_express` refuses it **before** the trial -- it never
    /// reaches the projection deadlock that
    /// `planner::tests::the_smallest_netlist_that_deadlocks_the_projection`
    /// reduces to five gates, though `compile_planned` still shows that
    /// deadlock if asked. Two independent reasons to fall back, and this
    /// circuit has both; the gate is the one that fires.
    #[test]
    fn which_path_compile_takes_on_each_of_the_six_condition_circuits() {
        let expected = [
            ("and4", PlannerKind::Unified3d),
            ("full_adder", PlannerKind::Unified3d),
            ("segment_a", PlannerKind::Legacy),
            ("seven_segment", PlannerKind::Legacy),
            ("verilog:and4", PlannerKind::Unified3d),
            ("verilog:seven_segment", PlannerKind::Legacy),
        ];

        for ((name, netlist), (expected_name, expected_kind)) in
            the_six_condition_netlists().into_iter().zip(expected)
        {
            assert_eq!(name, expected_name, "the two lists must stay in the same order");
            let compiled = compile(&netlist)
                .unwrap_or_else(|error| panic!("{name} must compile by one path or the other: {error}"));
            assert_eq!(
                compiled.planner_kind(),
                expected_kind,
                "{name} took the other path"
            );
        }
    }

    /// A netlist the planner tries and cannot route still compiles, through
    /// the emitter. This is the policy's fallback clause, on the smallest
    /// circuit in the tree that actually exercises it.
    ///
    /// **`segment_a` and not the projection-deadlock netlist**, which is where
    /// this was pointed first and which turned out to prove something else:
    /// the deadlock shape is a merge with *both* branches isolated, so
    /// `planner_can_express` refuses it before the trial ever runs and the
    /// fallback is never reached. Verified by injection -- deleting the
    /// fallback entirely left that version **green**. `segment_a` has no merge
    /// at all, so the only way it reaches the emitter is by being tried and
    /// failing.
    ///
    /// **The pairing is the test.** That the trial really does fail is
    /// asserted here too, at the budget `compile` actually spends, because
    /// without it the other assertion could pass for the boring reason. If
    /// routing is ever fixed, this goes red on the first assertion and says
    /// so, which is the right way to find out.
    #[test]
    fn compile_falls_back_when_the_planner_cannot_route() {
        use crate::circuits::seven_segment::build_single_segment_netlist;

        let (netlist, _) = build_single_segment_netlist(0);
        assert!(
            planner_can_express(&netlist),
            "segment_a must be a netlist the planner is willing to try"
        );

        let refusal = compile_planned_within(
            &netlist,
            &planner::PortPlacements::default(),
            planner::TRIAL_RIP_UP_ROUNDS,
        )
        .err()
        .expect("the router must still fail on segment_a at the trial budget");
        // Either text is the router itself saying no: a dead-ended search, or
        // `lay_net`'s ring rule refusing a latch corridor. Which one segment_a
        // hits at the trial budget moved on 2026-08-28, when the bounded
        // own-stair reuse changed what the search explores -- the fallback
        // story this test pins did not.
        let message = refusal.to_string();
        assert!(
            message.contains("no safe local route") || message.contains("closes a ring"),
            "the planner must fail in the router, not somewhere else: {refusal}"
        );

        let compiled = compile(&netlist).expect("the emitter must pick this up");
        assert_eq!(compiled.planner_kind(), PlannerKind::Legacy);
    }

    /// The planner leaves a shared merge branch unisolated, `compile` refuses
    /// to use it for such a netlist, and every number here is measured.
    ///
    /// This is the whole of [`planner_can_express`]'s justification, pinned so
    /// the gate cannot become decorative. Three claims, each of which can fail
    /// independently:
    ///
    /// 1. **The planner really does lose the repeater.** `compile_planned` on
    ///    the smallest netlist with the shape -- a lever feeding both a NOT and
    ///    a two-input merge -- returns a world with *no repeater anywhere*, and
    ///    the shared branch's own socket holding plain dust. If somebody
    ///    teaches `PlanCandidate` one anchor per primitive, this goes red and
    ///    the gate can go with it.
    /// 2. **`compile` does not use it.** The circuit comes back `Legacy`, with
    ///    the isolating repeater in the shared branch's socket, which is what
    ///    `tests/or_merge.rs`'s
    ///    `compile_places_the_isolating_repeater_on_exactly_the_shared_branch`
    ///    asserts about the shipping path.
    /// 3. **The gate is about the shape and not about the circuit.** Dropping
    ///    the second consumer makes both branches bare, and the same netlist
    ///    then goes through the planner.
    ///
    /// Not asserted, and worth saying: the planner's world for (1) computes
    /// the right function anyway. The junction is fourteen cells from the
    /// sentinel's torch, so the backflow decays before it arrives. That is
    /// distance, not isolation, and it is exactly why this is gated rather
    /// than left to a truth table to notice.
    #[test]
    fn the_relaxation_path_isolates_a_shared_merge_branch_and_the_gate_awaits_its_battery() {
        use crate::compile::geometry::input_directions;

        // `a` drives the merge *and* the sentinel NOR, so the merge's first
        // branch is not bare. `b` drives only the merge, so its branch is.
        let shared = Netlist {
            inputs: vec!["a".to_string(), "b".to_string()],
            outputs: vec!["s".to_string(), "m".to_string()],
            gates: vec![
                Gate::nor("s", &["a"]),
                Gate {
                    name: "m".to_string(),
                    inputs: vec!["a".to_string(), "b".to_string()],
                    output: "m".to_string(),
                    kind: GateKind::Or(2),
                },
            ],
        };
        assert_eq!(
            primitive_graph::shared_merge_branches(&shared),
            vec![(1usize, 0usize)],
            "the merge's first branch shares its producer with the sentinel"
        );

        // (1) What the planner builds for it, on its own. UNTIL 2026-08-29
        // this pinned the opposite: a bare joint and zero repeaters anywhere,
        // which was `planner_can_express`'s whole measured justification.
        // The merge-terminal normalization (every terminal into a merge is a
        // repeater -- see `lay_net`) changed that fact deliberately: the
        // planner now isolates every inbound merge branch BY CONSTRUCTION,
        // shared ones included. The gate below still stands -- claim (2) --
        // because dropping it takes an end-to-end battery on shared-merge
        // netlists (truth table included), not one socket assertion; that
        // measurement is the follow-up this comment is the marker for.
        let planned = compile_planned(&shared, &planner::PortPlacements::default())
            .expect("the planner places and routes this fixture");
        let socket = |compiled: &CompiledCircuit| {
            let &(jx, jy, jz) = compiled.gate_output_positions.get("m").expect("the merge");
            let facing = compiled.gate_facings[1];
            Position::new(jx, jy, jz).offset(input_directions(facing)[0])
        };
        let planned_socket = socket(&planned);
        assert_eq!(
            planned
                .world
                .get(planned_socket.x, planned_socket.y, planned_socket.z)
                .kind,
            BlockKind::Repeater,
            "the planner now isolates the shared branch in its own socket"
        );
        assert!(
            count_kind(&planned.world, BlockKind::Repeater) >= 2,
            "one isolating repeater per inbound merge branch, by construction"
        );

        // (2) What `compile` therefore does about it.
        let shipped = compile(&shared).expect("the emitter picks this up");
        assert_eq!(shipped.planner_kind(), PlannerKind::Legacy);
        let shipped_socket = socket(&shipped);
        assert_eq!(
            shipped
                .world
                .get(shipped_socket.x, shipped_socket.y, shipped_socket.z)
                .kind,
            BlockKind::Repeater,
            "the emitter isolates the shared branch in its own socket"
        );

        // (3) The control: the same merge with nothing else consuming `a`.
        let private = Netlist {
            inputs: vec!["a".to_string(), "b".to_string()],
            outputs: vec!["m".to_string()],
            gates: vec![Gate {
                name: "m".to_string(),
                inputs: vec!["a".to_string(), "b".to_string()],
                output: "m".to_string(),
                kind: GateKind::Or(2),
            }],
        };
        assert!(primitive_graph::shared_merge_branches(&private).is_empty());
        assert_eq!(
            compile(&private).expect("a private merge compiles").planner_kind(),
            PlannerKind::Unified3d,
            "the gate is about the shape, not about merges as such"
        );
    }

    /// Blocks of one kind, counted the way every size measurement in this tree
    /// counts them.
    fn count_kind(world: &World, kind: BlockKind) -> usize {
        let (size_x, size_y, size_z) = world.size();
        let mut count = 0usize;
        for x in 0..size_x {
            for y in 0..size_y {
                for z in 0..size_z {
                    if world.get(x, y, z).kind == kind {
                        count += 1;
                    }
                }
            }
        }
        count
    }

    /// A netlist neither compiler can build is refused **by name**, not by
    /// whatever the planner made of it.
    ///
    /// The failure this guards against is the trial's error escaping: the
    /// planner wraps a bad netlist in `CandidateMetadataViolation { item:
    /// "candidate", reason: "cannot realise node netlist: signal `nobody` is
    /// never driven" }`, which names the planner's own bookkeeping where
    /// `UndrivenSignal("nobody")` names the netlist. Verified by injection:
    /// returning the trial's `Err` instead of falling back turns this red with
    /// exactly that message.
    ///
    /// **What it does not prove, and this is a correction of an earlier
    /// claim.** It was written asserting that the up-front
    /// `checked_topological_order` call is what keeps the right error
    /// reaching the caller. Measured: deleting that call alone leaves this
    /// test **green**, because `compile_legacy` runs the very same checks and
    /// the fallback therefore reports the very same error. So the up-front
    /// call buys two other things -- the trial is not run at all on a netlist
    /// neither compiler can build, and the shared contract is stated in one
    /// place instead of being a property of the fallback happening to
    /// duplicate it -- and this test cannot see either. It is named for what
    /// it can see.
    #[test]
    fn an_unbuildable_netlist_is_refused_by_name_and_not_by_the_trial() {
        let cyclic = Netlist {
            inputs: vec!["a".to_string()],
            outputs: vec!["x".to_string()],
            gates: vec![Gate::nor("x", &["y"]), Gate::nor("y", &["x"])],
        };
        assert_eq!(compile(&cyclic).err(), Some(CompileError::CyclicNetlist));

        let undriven = Netlist {
            inputs: vec!["a".to_string()],
            outputs: vec!["x".to_string()],
            gates: vec![Gate::nor("x", &["nobody"])],
        };
        assert_eq!(
            compile(&undriven).err(),
            Some(CompileError::UndrivenSignal("nobody".to_string()))
        );
    }

    /// What `compile` costs on every circuit the condition names, in seconds.
    ///
    /// ```bash
    /// cargo test --release --lib \
    ///   compile::tests::what_compile_costs_on_the_six_condition_circuits \
    ///   -- --ignored --nocapture
    /// ```
    ///
    /// This exists because the hybrid's one real risk is a time regression, not
    /// a correctness one: `compile` tries the planner first, so every circuit
    /// that falls back pays the trial's failure before the path that works even
    /// starts. A number nobody can reproduce is not a measurement, so the
    /// harness that produced the before/after table lives here rather than in a
    /// commit message.
    ///
    /// Asserts nothing. Wall clock is machine- and load-dependent, and a test
    /// that fails when the machine is busy is a test people learn to ignore.
    #[test]
    #[ignore = "measurement harness: asserts nothing, wall-clock timing of all six circuits"]
    fn what_compile_costs_on_the_six_condition_circuits() {
        use std::time::Instant;

        for (name, netlist) in the_six_condition_netlists() {
            let started = Instant::now();
            let result = compile(&netlist);
            let elapsed = started.elapsed().as_secs_f64();
            match result {
                Ok(compiled) => eprintln!(
                    "{name}: {} gates, {:.2}s, {:?}, {} blocks",
                    netlist.gates.len(),
                    elapsed,
                    compiled.planner_kind(),
                    occupied(&compiled.world),
                ),
                Err(error) => eprintln!(
                    "{name}: {} gates, {:.2}s, ERR {error}",
                    netlist.gates.len(),
                    elapsed
                ),
            }
        }
    }

    /// Non-air cells, counted the way every size measurement in this tree
    /// counts them.
    fn occupied(world: &World) -> usize {
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
}
