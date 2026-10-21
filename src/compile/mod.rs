//! Compile a lowered netlist into a redstone world that works in Minecraft.
//!
//! A netlist of NOR, OR and positive-edge DFF gates goes in ([`lowering`]
//! produces one from the frontend's gate level); a [`CompiledCircuit`] comes
//! out: the world, whose inputs and outputs are ports -- placed freely, on
//! an edge or at a cell the caller gives -- and where each is driven and
//! read.
//! [`construct`] is the one generator.

use std::collections::{BTreeMap, HashMap, VecDeque};

use crate::redstone::world::block::{BlockKind, BlockState, Face, Facing};
use crate::redstone::world::storage::World;
pub use construct::{PortPlace, Side};

pub mod construct;
pub mod geometry;
pub mod lowering;
pub mod metrics;
pub mod polarity;
pub mod topology;

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
    /// [`compile`] places three kinds: [`topology::GateKind::Nor`],
    /// [`topology::GateKind::Or`] and [`topology::GateKind::DffPosedge`].
    /// Every other kind is gate level, and [`lowering::lower`] rewrites it
    /// into those first; `compile` refuses it with
    /// [`CompileError::NotRealisable`] rather than lowering implicitly.
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

    /// Whether this gate is an OR rather than a NOR or a flip-flop.
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
}


// ---------------------------------------------------------------------
// Blocks
// ---------------------------------------------------------------------

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

/// A redstone lamp: the block a person reads an output from. It starts
/// unlit; the simulator's first settle gives it its value.
pub(crate) fn lamp() -> BlockState {
    let mut state = BlockState::air();
    state.kind = BlockKind::Lamp;
    state.name = "minecraft:redstone_lamp".to_string();
    state.lit = false;
    state
}

/// A redstone block: drives adjacent dust, powers no block, needs no support.
pub fn redstone_block() -> BlockState {
    let mut state = BlockState::air();
    state.kind = BlockKind::RedstoneBlock;
    state.name = "minecraft:redstone_block".to_string();
    state
}

/// A lever standing on a floor block. `face` must be `Floor` (Minecraft's
/// default is `Wall`, which pops a floor lever off as an item on paste);
/// `facing` only orients the handle and is fixed for determinism.
pub(crate) fn lever(on: bool) -> BlockState {
    let mut state = BlockState::air();
    state.kind = BlockKind::Lever;
    state.name = "minecraft:lever".to_string();
    state.lit = on;
    state.face = Some(Face::Floor);
    state.facing = Some(Facing::North);
    state
}

/// A repeater whose signal travels toward `direction`. Minecraft's `facing`
/// records the opposite (output side to input side), so that is what is
/// stored.
pub(crate) fn repeater(direction: Facing) -> BlockState {
    let mut state = BlockState::air();
    state.kind = BlockKind::Repeater;
    state.name = "minecraft:repeater".to_string();
    state.facing = Some(direction.opposite());
    state.delay = 1;
    state.lit = true;
    state
}

// ---------------------------------------------------------------------
// Compiling
// ---------------------------------------------------------------------

/// Why a netlist did not compile.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CompileError {
    /// A gate [`compile`] has no tile for: lower the netlist first.
    #[error("gate {gate} has kind {kind:?}, which is not realisable; lower the netlist first")]
    NotRealisable { gate: String, kind: topology::GateKind },
    #[error("the netlist has a combinational loop")]
    CyclicNetlist,
    #[error("signal {0} has no driver")]
    UndrivenSignal(String),
    /// The generator ran out of room or routes; see the reason.
    #[error("generation failed: {0}")]
    Unplaceable(String),
}

/// A compiled circuit: the world and where its ports are.
///
/// The circuit has no levers or lamps: every input and output is a port,
/// for whatever it is built into to drive and read.
#[derive(Debug, Clone)]
pub struct CompiledCircuit {
    pub world: World,
    /// [`Self::world`] with a test bench added: a lever at every input
    /// position and a lamp at every output position, in the same frame.
    pub bench: World,
    /// Where the world's corner goes in the coordinates ports were placed
    /// at cells in (see [`PortPlace::Cell`]), so each such port's position
    /// plus this is the cell it was given; zero when none was.
    pub origin: (i32, i32, i32),
    /// Where each primary input is driven from: the clear cell beside its
    /// port, which a lever or any other source there powers.
    pub input_positions: BTreeMap<String, (i32, i32, i32)>,
    /// Where each primary output is read: the clear cell beside its port,
    /// which a lamp there lights when the signal is high.
    pub output_positions: BTreeMap<String, (i32, i32, i32)>,
    /// Where every gate's own output value can be read (a NOR's torch, an
    /// OR's junction dust, a DFF's Q dust), keyed by its output signal.
    pub gate_output_positions: BTreeMap<String, (i32, i32, i32)>,
}

/// Compile a lowered netlist into a redstone world.
///
/// The netlist must already be realisable (see [`Gate::kind`]); this does
/// not lower it, because callers keep the netlist they passed and pair it
/// with the result, and a silent lowering would make those two different
/// netlists.
pub fn compile(netlist: &Netlist) -> Result<CompiledCircuit, CompileError> {
    compile_with_places(netlist, &BTreeMap::new())
}

/// [`compile`], with each port signal named in `places` where it says: on
/// an edge, or exactly at a cell (the rest wherever placement puts them).
pub fn compile_with_places(
    netlist: &Netlist,
    places: &BTreeMap<String, PortPlace>,
) -> Result<CompiledCircuit, CompileError> {
    circuit(construct::construct(netlist, places).map_err(construct_error)?)
}

fn circuit(construction: construct::Construction) -> Result<CompiledCircuit, CompileError> {
    let placed = construction.into_world();
    Ok(CompiledCircuit {
        world: placed.world,
        bench: placed.bench,
        origin: placed.origin,
        input_positions: placed.inputs,
        output_positions: placed.outputs,
        gate_output_positions: placed.gate_outputs,
    })
}

/// [`compile`] along a module tree: `owner[g]` is the instance that built
/// gate `g` and `parents[i]` instance `i`'s parent (the top is instance 0).
/// Instances whose subtree holds fewer than `grain` gates are laid out as
/// part of their parent.
pub fn compile_hierarchical(
    netlist: &Netlist,
    owner: &[usize],
    parents: &[Option<usize>],
    grain: usize,
) -> Result<CompiledCircuit, CompileError> {
    circuit(construct::construct_hierarchical(netlist, owner, parents, grain, &BTreeMap::new()).map_err(construct_error)?)
}

fn construct_error(error: construct::ConstructError) -> CompileError {
    match error {
        construct::ConstructError::NoTile { gate, kind } => CompileError::NotRealisable { gate, kind },
        construct::ConstructError::Undriven { signal } => CompileError::UndrivenSignal(signal),
        construct::ConstructError::CombinationalLoop => CompileError::CyclicNetlist,
        other @ construct::ConstructError::Unplaceable { .. } => CompileError::Unplaceable(other.to_string()),
    }
}

/// [`compile`], placing and routing by `method`.
pub fn compile_with(netlist: &Netlist, method: construct::Method) -> Result<CompiledCircuit, CompileError> {
    circuit(construct::construct_with(netlist, &BTreeMap::new(), method).map_err(construct_error)?)
}
