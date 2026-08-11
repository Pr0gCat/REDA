//! The check `docs/superpowers/specs/2026-08-08-primitive-level-flow.md`'s
//! step 1 asks for: "expanding today's netlists and re-emitting today's
//! geometry must produce byte-identical worlds."
//!
//! # Which of the two approaches the spec allows this takes
//!
//! The spec leaves it open whether the existing emitter is refactored to go
//! through `primitive_graph::PrimitiveGraph`, or the graph is expanded and
//! compared against the existing output. This module takes the second path,
//! for two reasons:
//!
//! - `compile`'s row/channel/track router (`build_floorplan`, `emit`, and
//!   everything between them) is the placement/routing machinery the spec's
//!   own `Order` explicitly defers to steps 2 and 3 ("The planner",
//!   "Legalisation"). A `PrimitiveGraph` carries no positions at all (see
//!   `primitive_graph`'s doc comment), so making the *existing* emitter
//!   consume one instead of a `Netlist` directly would mean teaching it to
//!   invent positions for a graph that deliberately has none yet -- which is
//!   not a refactor, it is starting the planner early, exactly what this
//!   step is told not to do.
//! - The four invariants `compile` already runs unconditionally
//!   (`verify_connectivity`, `verify_torch_merge`, `verify_signal_strength`,
//!   and spacing) are, read structurally, precisely the correctness
//!   conditions this graph's own edges assert: a NOR gate's N-inputs-one-
//!   torch structure is `verify_torch_merge`'s "N sources, one sink"
//!   invariant; a net's edge delivering a real, undecayed signal from
//!   producer to every declared sink is exactly what `verify_connectivity`
//!   and `verify_signal_strength` already certify, on the *actual* emitted
//!   bytes. Reusing them here instead of re-implementing a second walk of
//!   the world is the same "don't re-derive, reuse" principle
//!   `routing_stats` already applies to the router's own geometry.
//!
//! # What "byte-identical" becomes, concretely
//!
//! `compile` itself is untouched by this step (see `topology` and
//! `primitive_graph`, neither of which `compile::compile` calls), so any
//! world it produces for a given `Netlist` is unaffected by this module's
//! existence -- there is no second emitter whose output could ever diverge
//! from today's. What has to be shown instead is that the graph has *lost
//! nothing*: that expanding a netlist and reading the already-compiled world
//! back out, primitive for primitive, finds exactly the graph's own nodes
//! and edges and nothing more.
//!
//! - **A `Nor`/`Buf` gate's own torch and its support** are checked directly
//!   against the compiled `World`'s own bytes -- the torch resolves to a
//!   real, resolvable, conductive support, and exactly `arity` of
//!   `INPUT_DIRECTIONS`'s sockets (no more, no fewer) actively feed it -- by
//!   either a repeater or a directed dust terminal.
//! - **A merge gate's own junction** is checked the same way, in its own
//!   shape: no torch, no support (`place_merge_gate` writes dust, not a
//!   support block) -- a bare branch's own socket must be plain dust
//!   (nothing terminates it, by design: see `GateKind::Or`'s doc comment),
//!   an isolated branch's own socket must be a repeater facing the
//!   junction, and which is which for each of a merge's own declared inputs
//!   is decided by the exact same fanout rule `compile::merge_branch_is_bare`
//!   uses, restated independently here over `Netlist` alone (see
//!   [`resolve_contributors`]).
//! - **The graph's own claimed structure** -- how many nodes each gate owns,
//!   and how many edges land on each of them from outside -- is checked
//!   against what [`resolve_contributors`] derives from `Netlist` alone,
//!   independent of `expand`'s own machinery entirely.
//! - **Edges** are checked two ways: independently, by comparing the
//!   graph's own edges into every real landing point (a `Nor`/`Buf` gate's
//!   torch, a merge's own isolating-repeater branch, or a declared output's
//!   lamp) against the net structure `Netlist` implies on its own, with no
//!   reference to `expand` at all; and by relying on `compile`'s own
//!   connectivity/torch-merge/signal-strength invariants, which have
//!   already read every byte of dust and every repeater the router placed
//!   for that same net and found it delivers exactly the declared producer
//!   to exactly the declared sinks. Together these two facts chain into
//!   "the graph's edges are what the compiled bytes actually implement"
//!   without this module re-walking dust itself.

use std::collections::HashMap;
use std::collections::HashSet;

use super::primitive_graph::{NodeId, PrimitiveGraph, Provenance};
use super::topology::TemplateNode;
use super::{input_socket_feeds_support, CompiledCircuit, Netlist, INPUT_DIRECTIONS, OUTPUT_DIRECTION};
use crate::redstone::rules::taxonomy::flags_of;
use crate::redstone::simulator::component::torch_support_position;
use crate::redstone::simulator::position::Position;
use crate::redstone::world::block::BlockKind;

/// Why the compiled world does not match what `expand` says it should be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EquivalenceError {
    /// The graph's total node count does not match what [`resolve_
    /// contributors`] implies from `Netlist` alone (levers + lamps + one
    /// node per `Nor`/`Buf` gate + one `IsolatingRepeater` per non-bare
    /// merge branch).
    NodeCountMismatch { expected: usize, actual: usize },
    /// The graph's total edge count does not match what [`resolve_
    /// contributors`] implies.
    EdgeCountMismatch { expected: usize, actual: usize },
    /// A `Nor`/`Buf` gate's own instance is missing a node for the one
    /// fixed role (`TemplateNode::Torch`) every `topology::nor_entry` must
    /// provide.
    MissingRole { gate: String, role: TemplateNode },
    /// The number of edges landing on a gate's own node cluster from outside
    /// it does not match `Netlist`'s own declared arity for that gate --
    /// the graph-side half of "did `expand` wire up every declared input".
    GraphInputEdgeCountMismatch { gate: String, netlist_arity: usize, graph_edge_count: usize },
    /// `CompiledCircuit::gate_output_positions` has no entry for this gate's
    /// output at all.
    TorchNotPlaced { gate: String },
    /// The world cell at a gate's recorded torch position is not a torch, or
    /// is a wall torch with no recorded facing -- `torch_support_position`
    /// could not resolve a support for it.
    TorchHasNoResolvableSupport { gate: String, torch: (i32, i32, i32) },
    /// The block a gate's torch is attached to is not conductive --
    /// `verify_torch_merge` would already reject this, but this module
    /// checks it directly rather than only trusting that invariant ran.
    SupportNotConductive { gate: String, support: (i32, i32, i32) },
    /// One of this gate's declared input sockets (by `INPUT_DIRECTIONS`
    /// index, `< arity`) does not actively feed the support. The terminal
    /// may be a repeater or straight, directed dust.
    InputSocketDoesNotFeedSupport { gate: String, input_index: usize, socket: (i32, i32, i32) },
    /// A socket direction beyond this gate's declared arity nonetheless
    /// actively feeds the support -- more inputs than the netlist declares.
    UndeclaredInputFeedsSupport { gate: String, direction_index: usize, socket: (i32, i32, i32) },
    /// A merge gate's own junction (`CompiledCircuit::gate_output_positions`)
    /// is not plain dust -- `place_merge_gate` never writes anything else
    /// there.
    JunctionNotDust { gate: String, junction: (i32, i32, i32) },
    /// A merge's declared **bare** branch (`input_index < arity`) has a
    /// repeater at its own socket instead of plain dust -- bare means
    /// nothing terminates it (see `GateKind::Or`'s doc comment).
    BareBranchSocketNotDust { gate: String, input_index: usize, socket: (i32, i32, i32) },
    /// A merge's declared **isolated** branch has no repeater facing the
    /// junction at its own socket.
    IsolatedBranchSocketNotARepeaterFacingJunction { gate: String, input_index: usize, socket: (i32, i32, i32) },
    /// A socket direction beyond a merge's declared arity is nonetheless
    /// occupied by a repeater facing into the junction.
    UndeclaredMergeInputFeedsJunction { gate: String, direction_index: usize, socket: (i32, i32, i32) },
    /// `CompiledCircuit::input_positions` has no entry for this primary
    /// input.
    LeverNotPlaced { name: String },
    /// The world cell at a primary input's recorded lever position is not a
    /// lever.
    LeverWrongKind { name: String, position: (i32, i32, i32) },
    /// `CompiledCircuit::output_positions` has no entry for this declared
    /// output.
    LampNotPlaced { name: String },
    /// The world cell at a declared output's recorded lamp position is not
    /// a lamp.
    LampWrongKind { name: String, position: (i32, i32, i32) },
    /// A declared output's lamp is not at the fixed offset `emit` always
    /// places it at (one cell past its driving gate's torch, then straight
    /// down) -- the physical realisation of the torch-to-lamp edge.
    LampNotAtFixedOffsetFromTorch { name: String, expected: (i32, i32, i32), actual: (i32, i32, i32) },
    /// The graph's input edges, translated back to netlist-level identities,
    /// do not match the net structure `Netlist` implies on its own.
    InputEdgesDoNotMatchNetlist { only_in_graph: usize, only_in_netlist: usize },
}

impl std::fmt::Display for EquivalenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for EquivalenceError {}

// ---------------------------------------------------------------------
// Independent restatement of `expand`'s own bare-chain resolution
// ---------------------------------------------------------------------
//
// Everything below derives, from `Netlist` alone, exactly what
// `primitive_graph::expand` is supposed to build -- restated independently
// (not by calling into `expand`'s own private helpers) so that a bug shared
// between the two would have to be typed twice, identically, to slip past
// this module. This mirrors `compile::merge_branch_is_bare`'s fanout rule
// and `expand`'s own bare-chain flattening (see that function's doc comment
// for the physical justification: a bare branch is nothing but the point
// where two nets' dust touch, so it contributes no node of its own).

/// One real, physical thing a signal can ultimately resolve to, once every
/// bare merge in its own chain has been flattened away -- exactly the three
/// kinds of node [`super::primitive_graph::expand`] ever draws a real edge
/// into or out of.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) enum Contributor {
    /// A `Netlist::inputs` entry's own lever.
    PrimaryInput(String),
    /// A `Nor`/`Buf` gate's own single Torch node, named by its declared
    /// output signal.
    GateTorch(String),
    /// One merge gate's own isolated branch -- its own dedicated
    /// `IsolatingRepeater` node. Identified by the merge's own output name
    /// and which declared input this branch is, since a merge can have more
    /// than one isolated branch, each its own distinct node.
    IsolatingRepeater(String, usize),
}

impl Contributor {
    /// This contributor's own identity as a netlist-level string -- what
    /// [`NetEdge`] actually compares, since a `HashMap` key needs `Eq`, not
    /// every field of this enum individually. Primary inputs and gate
    /// torches use their own real signal name (so they still line up with
    /// signal names read directly off `Netlist`); an isolating repeater
    /// uses a synthetic name (`#branch`) that cannot collide with a real
    /// Verilog identifier, which is never checked against a raw netlist
    /// name on its own -- only ever against another `Contributor`'s same
    /// synthetic name, on the graph side.
    fn label(&self) -> String {
        match self {
            Contributor::PrimaryInput(name) | Contributor::GateTorch(name) => name.clone(),
            Contributor::IsolatingRepeater(gate, index) => format!("{gate}#branch{index}"),
        }
    }
}

/// Everything [`verify_expansion_matches_compiled`] needs from `Netlist`
/// alone, computed once: every gate's own resolved contributor list (what a
/// consumer of its declared output must draw one edge from *each* of), and
/// every merge's own per-branch bareness and (for isolated branches) the
/// resolved contributors feeding that one branch's own repeater.
pub(super) struct NetlistResolution {
    /// gate index -> the contributors a consumer of this gate's own
    /// declared output must draw one edge from each of. A `Nor`/`Buf` gate's
    /// own entry is always exactly one `GateTorch`; a merge's is the
    /// concatenation, in declared-input order, of each branch's own
    /// contribution (a bare branch splices its own producer's contributors
    /// in directly; an isolated branch contributes exactly its own
    /// `IsolatingRepeater`).
    contributors_of_gate: Vec<Vec<Contributor>>,
    /// `(gate index, input index) -> whether that declared input is a bare
    /// branch` -- only meaningful for merge gates, but total over every
    /// `(gate, input_index)` pair that exists for simplicity. `pub(super)`
    /// so `world_partition` can independently know, per merge gate, how
    /// many of its own branches are isolated (its own version of "declared
    /// arity" for the world-side repeater-facing-junction count -- a bare
    /// branch is legitimately zero repeaters, not a missing one).
    pub(super) is_bare: HashMap<(usize, usize), bool>,
    /// `(gate index, input index) -> the resolved contributors feeding that
    /// one input` -- what an isolated branch's own repeater must have
    /// exactly these edges into it from, and what a `Nor`/`Buf` gate's own
    /// torch must have this many edges into it from, per declared input
    /// (summed across all of a `Nor`/`Buf` gate's own inputs, since they all
    /// land on the same one node).
    pub(super) contributors_of_input: HashMap<(usize, usize), Vec<Contributor>>,
}

pub(super) fn resolve_contributors(netlist: &Netlist) -> NetlistResolution {
    let order = netlist
        .topological_order()
        .expect("compile() already rejected a cyclic netlist before this ever runs, and expand() re-checks it too");
    let producer_of: HashMap<&str, usize> =
        netlist.gates.iter().enumerate().map(|(i, gate)| (gate.output.as_str(), i)).collect();
    let input_names: HashSet<&str> = netlist.inputs.iter().map(|s| s.as_str()).collect();

    let mut consumers_of: HashMap<&str, Vec<usize>> = HashMap::new();
    for (g, gate) in netlist.gates.iter().enumerate() {
        for input in &gate.inputs {
            consumers_of.entry(input.as_str()).or_default().push(g);
        }
    }
    let branch_is_bare =
        |signal: &str, into_gate: usize| consumers_of.get(signal).is_some_and(|gs| gs.iter().all(|&g| g == into_gate));

    let resolve_signal = |signal: &str, contributors_of_gate: &[Vec<Contributor>]| -> Vec<Contributor> {
        if input_names.contains(signal) {
            vec![Contributor::PrimaryInput(signal.to_string())]
        } else {
            contributors_of_gate[producer_of[signal]].clone()
        }
    };

    let mut contributors_of_gate: Vec<Vec<Contributor>> = vec![Vec::new(); netlist.gates.len()];
    let mut is_bare: HashMap<(usize, usize), bool> = HashMap::new();
    let mut contributors_of_input: HashMap<(usize, usize), Vec<Contributor>> = HashMap::new();

    for g in order {
        let gate = &netlist.gates[g];
        if gate.is_merge() {
            let mut acc = Vec::new();
            for (index, input) in gate.inputs.iter().enumerate() {
                let feeding = resolve_signal(input, &contributors_of_gate);
                contributors_of_input.insert((g, index), feeding.clone());
                let bare = branch_is_bare(input, g);
                is_bare.insert((g, index), bare);
                if bare {
                    acc.extend(feeding);
                } else {
                    acc.push(Contributor::IsolatingRepeater(gate.output.clone(), index));
                }
            }
            contributors_of_gate[g] = acc;
        } else {
            for (index, input) in gate.inputs.iter().enumerate() {
                let feeding = resolve_signal(input, &contributors_of_gate);
                contributors_of_input.insert((g, index), feeding);
                is_bare.insert((g, index), false);
            }
            contributors_of_gate[g] = vec![Contributor::GateTorch(gate.output.clone())];
        }
    }

    NetlistResolution { contributors_of_gate, is_bare, contributors_of_input }
}

/// One (producer, consuming landing point) pair, in netlist-level terms
/// rather than `NodeId`s -- what both sides of the input-edge cross-check in
/// [`verify_expansion_matches_compiled`] are reduced to before comparison.
///
/// Deliberately carries no input *index* for the `Nor`/`Buf` case: a NOR's
/// inputs are interchangeable, so the flat graph never claimed an order
/// beyond "this many edges land here" -- see `topology`'s doc comment.
/// Comparison is therefore a multiset-of-pairs equality, not a
/// set-of-triples one, so a landing point fed the same producer twice is
/// still counted correctly on both sides.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct NetEdge {
    producer: String,
    consumer: String,
}

/// Check that `graph` (built by `primitive_graph::expand(netlist, ..)`)
/// accounts for exactly what `compiled` (built by `compile(netlist)`, the
/// same `netlist`) actually contains -- see this module's doc comment for
/// what "accounts for" means precisely and why.
pub fn verify_expansion_matches_compiled(
    netlist: &Netlist,
    graph: &PrimitiveGraph,
    compiled: &CompiledCircuit,
) -> Result<(), EquivalenceError> {
    let resolution = resolve_contributors(netlist);

    verify_counts(netlist, graph, &resolution)?;

    for (g, gate) in netlist.gates.iter().enumerate() {
        if gate.is_merge() {
            verify_merge_gate_structure(netlist, graph, compiled, g, &resolution)?;
        } else {
            verify_gate_structure(netlist, graph, compiled, g, gate.inputs.len(), &resolution)?;
        }
    }

    for name in &netlist.inputs {
        verify_lever(compiled, name)?;
    }

    for output_name in &netlist.outputs {
        verify_lamp(netlist, compiled, output_name)?;
    }

    verify_input_edges_match_netlist(netlist, graph, &resolution)?;

    Ok(())
}

/// Node/edge totals [`resolve_contributors`] implies, independent of
/// `graph` -- the first, cheapest cross-check that `expand` built neither
/// too much nor too little.
fn verify_counts(netlist: &Netlist, graph: &PrimitiveGraph, resolution: &NetlistResolution) -> Result<(), EquivalenceError> {
    let mut expected_nodes = netlist.inputs.len() + netlist.outputs.len();
    let mut expected_edges = 0usize;

    for (g, gate) in netlist.gates.iter().enumerate() {
        if gate.is_merge() {
            for index in 0..gate.inputs.len() {
                if !resolution.is_bare[&(g, index)] {
                    expected_nodes += 1; // this branch's own IsolatingRepeater
                    expected_edges += resolution.contributors_of_input[&(g, index)].len();
                }
            }
        } else {
            expected_nodes += 1; // this gate's own Torch
            for index in 0..gate.inputs.len() {
                expected_edges += resolution.contributors_of_input[&(g, index)].len();
            }
        }
    }
    for output_name in &netlist.outputs {
        let driving_gate = netlist
            .gates
            .iter()
            .position(|gate| &gate.output == output_name)
            .expect("every declared output is driven by a gate -- checked by `compile` before this ever runs");
        expected_edges += resolution.contributors_of_gate[driving_gate].len();
    }

    if graph.nodes.len() != expected_nodes {
        return Err(EquivalenceError::NodeCountMismatch { expected: expected_nodes, actual: graph.nodes.len() });
    }

    if graph.edges.len() != expected_edges {
        return Err(EquivalenceError::EdgeCountMismatch { expected: expected_edges, actual: graph.edges.len() });
    }

    Ok(())
}

/// Check gate `g`'s structure: the graph's own claim (a Torch node, fed by
/// edges from outside its cluster matching `resolution`'s own resolved
/// contributor counts) directly against the compiled world's own bytes (a
/// resolvable, conductive support with exactly `arity` input sockets
/// holding a repeater).
fn verify_gate_structure(
    netlist: &Netlist,
    graph: &PrimitiveGraph,
    compiled: &CompiledCircuit,
    g: usize,
    arity: usize,
    resolution: &NetlistResolution,
) -> Result<(), EquivalenceError> {
    let gate_name = netlist.gates[g].output.clone();

    // The graph must have the one role a NOR entry provides today: a Torch.
    let has_torch = graph.gate_nodes[g]
        .iter()
        .any(|&id| matches!(&graph.nodes[id].provenance, Provenance::Gate { gate, role: TemplateNode::Torch } if *gate == g));
    if !has_torch {
        return Err(EquivalenceError::MissingRole { gate: gate_name, role: TemplateNode::Torch });
    }

    // The graph's own claimed edge count: edges landing on this gate's node
    // cluster from outside it -- there is no `TemplateNode::Input` role left
    // to count directly (see `topology`'s doc comment on why), so this is
    // the structural replacement: an edge counts as one of gate `g`'s
    // declared inputs iff it targets a node gate `g` owns and originates
    // from a node gate `g` does not. Compared against the *resolved*
    // contributor count, not `arity` directly -- one declared input can
    // land more than one edge on this same Torch node, if its own producer
    // is a bare-merge chain with more than one ultimate contributor.
    let own: HashSet<NodeId> = graph.gate_nodes[g].iter().copied().collect();
    let graph_edge_count = graph.edges.iter().filter(|e| own.contains(&e.to) && !own.contains(&e.from)).count();
    let expected_edge_count: usize = (0..arity).map(|index| resolution.contributors_of_input[&(g, index)].len()).sum();
    if graph_edge_count != expected_edge_count {
        return Err(EquivalenceError::GraphInputEdgeCountMismatch {
            gate: gate_name,
            netlist_arity: expected_edge_count,
            graph_edge_count,
        });
    }

    // Now walk the actual world.
    let (tx, ty, tz) =
        *compiled.gate_output_positions.get(&gate_name).ok_or_else(|| EquivalenceError::TorchNotPlaced {
            gate: gate_name.clone(),
        })?;
    let torch_pos = Position::new(tx, ty, tz);
    let torch_state = compiled.world.get(tx, ty, tz);
    let support = torch_support_position(torch_state, torch_pos).ok_or_else(|| {
        EquivalenceError::TorchHasNoResolvableSupport { gate: gate_name.clone(), torch: (tx, ty, tz) }
    })?;

    let support_state = compiled.world.get(support.x, support.y, support.z);
    if !flags_of(support_state).is_conductive() {
        return Err(EquivalenceError::SupportNotConductive {
            gate: gate_name.clone(),
            support: (support.x, support.y, support.z),
        });
    }

    // Every direction `place_nor_gate` could ever use for an input: the
    // declared ones must actively feed the support; the rest must not (or
    // the world has more inputs than the netlist declares).
    for (index, &direction) in INPUT_DIRECTIONS.iter().enumerate() {
        let socket = support.offset(direction);
        let feeds_support = input_socket_feeds_support(&compiled.world, socket, support);

        if index < arity {
            if !feeds_support {
                return Err(EquivalenceError::InputSocketDoesNotFeedSupport {
                    gate: gate_name.clone(),
                    input_index: index,
                    socket: (socket.x, socket.y, socket.z),
                });
            }
        } else if feeds_support {
            return Err(EquivalenceError::UndeclaredInputFeedsSupport {
                gate: gate_name.clone(),
                direction_index: index,
                socket: (socket.x, socket.y, socket.z),
            });
        }
    }

    Ok(())
}

/// Check merge gate `g`'s structure: the graph's own claim (one
/// `IsolatingRepeater` node per non-bare declared input, each fed by exactly
/// its own resolved contributors) against the compiled world's own bytes
/// (a plain-dust junction with, per declared input, either plain dust -- a
/// bare branch -- or a repeater facing the junction -- an isolated one, per
/// [`resolve_contributors`]'s own answer for that branch).
fn verify_merge_gate_structure(
    netlist: &Netlist,
    graph: &PrimitiveGraph,
    compiled: &CompiledCircuit,
    g: usize,
    resolution: &NetlistResolution,
) -> Result<(), EquivalenceError> {
    let gate_name = netlist.gates[g].output.clone();
    let arity = netlist.gates[g].inputs.len();

    // Graph side: `gate_nodes[g]` must be exactly one `IsolatingRepeater`
    // per non-bare declared input, no more, no fewer, and each one's own
    // edges must come from exactly its own resolved contributors.
    let mut owned_by_index: HashMap<usize, NodeId> = HashMap::new();
    for &id in &graph.gate_nodes[g] {
        match &graph.nodes[id].provenance {
            Provenance::Gate { gate, role: TemplateNode::IsolatingRepeater(index) } if *gate == g => {
                owned_by_index.insert(*index, id);
            }
            other => panic!("merge gate `{gate_name}` owns an unexpected node: {other:?}"),
        }
    }
    for index in 0..arity {
        let bare = resolution.is_bare[&(g, index)];
        match (bare, owned_by_index.get(&index)) {
            (true, Some(_)) => panic!(
                "merge gate `{gate_name}`'s own branch {index} is bare, but the graph still owns an \
                 IsolatingRepeater node for it"
            ),
            (false, None) => {
                return Err(EquivalenceError::MissingRole {
                    gate: gate_name.clone(),
                    role: TemplateNode::IsolatingRepeater(index),
                })
            }
            (true, None) => {}
            (false, Some(&repeater)) => {
                let expected: HashSet<String> =
                    resolution.contributors_of_input[&(g, index)].iter().map(Contributor::label).collect();
                let actual: HashSet<String> = graph
                    .edges_to(repeater)
                    .map(|edge| contributor_label(netlist, graph, edge.from))
                    .collect();
                let graph_edge_count = graph.edges_to(repeater).count();
                if actual != expected || graph_edge_count != resolution.contributors_of_input[&(g, index)].len() {
                    return Err(EquivalenceError::GraphInputEdgeCountMismatch {
                        gate: format!("{gate_name}#branch{index}"),
                        netlist_arity: resolution.contributors_of_input[&(g, index)].len(),
                        graph_edge_count,
                    });
                }
            }
        }
    }

    // World side: the junction itself is dust, and every declared input's
    // own socket matches whichever realisation `resolution` says it is.
    let (jx, jy, jz) = *compiled.gate_output_positions.get(&gate_name).ok_or_else(|| EquivalenceError::TorchNotPlaced {
        gate: gate_name.clone(),
    })?;
    let junction = Position::new(jx, jy, jz);
    let junction_state = compiled.world.get(jx, jy, jz);
    if junction_state.kind != BlockKind::RedstoneWire {
        return Err(EquivalenceError::JunctionNotDust { gate: gate_name.clone(), junction: (jx, jy, jz) });
    }

    for (index, &direction) in INPUT_DIRECTIONS.iter().enumerate() {
        let socket = junction.offset(direction);
        let socket_state = compiled.world.get(socket.x, socket.y, socket.z);
        let feeds_junction = socket_state.kind == BlockKind::Repeater && socket_state.facing == Some(direction);

        if index < arity {
            let bare = resolution.is_bare[&(g, index)];
            if bare {
                if socket_state.kind != BlockKind::RedstoneWire {
                    return Err(EquivalenceError::BareBranchSocketNotDust {
                        gate: gate_name.clone(),
                        input_index: index,
                        socket: (socket.x, socket.y, socket.z),
                    });
                }
            } else if !feeds_junction {
                return Err(EquivalenceError::IsolatedBranchSocketNotARepeaterFacingJunction {
                    gate: gate_name.clone(),
                    input_index: index,
                    socket: (socket.x, socket.y, socket.z),
                });
            }
        } else if feeds_junction {
            return Err(EquivalenceError::UndeclaredMergeInputFeedsJunction {
                gate: gate_name.clone(),
                direction_index: index,
                socket: (socket.x, socket.y, socket.z),
            });
        }
    }

    Ok(())
}

/// This node's own netlist-level identity, for comparison against a
/// [`Contributor::label`] -- the graph-side half of that same reduction.
/// Only ever called on a node that is actually somebody's producer (a
/// `Lever`, a `Nor`/`Buf` gate's own `Torch`, or a merge's own
/// `IsolatingRepeater`), which every edge source in this graph always is.
fn contributor_label(netlist: &Netlist, graph: &PrimitiveGraph, node: NodeId) -> String {
    match &graph.nodes[node].provenance {
        Provenance::PrimaryInput { name } => name.clone(),
        Provenance::PrimaryOutput { name } => {
            panic!("a Lamp node (`{name}`) is never anyone's producer, only a consumer")
        }
        Provenance::Gate { gate, role: TemplateNode::Torch } => netlist.gates[*gate].output.clone(),
        Provenance::Gate { gate, role: TemplateNode::IsolatingRepeater(index) } => {
            Contributor::IsolatingRepeater(netlist.gates[*gate].output.clone(), *index).label()
        }
        other => panic!("node with provenance {other:?} is never anyone's producer"),
    }
}

fn verify_lever(compiled: &CompiledCircuit, name: &str) -> Result<(), EquivalenceError> {
    let &(x, y, z) = compiled
        .input_positions
        .get(name)
        .ok_or_else(|| EquivalenceError::LeverNotPlaced { name: name.to_string() })?;
    let state = compiled.world.get(x, y, z);
    if state.kind != BlockKind::Lever {
        return Err(EquivalenceError::LeverWrongKind { name: name.to_string(), position: (x, y, z) });
    }
    Ok(())
}

fn verify_lamp(netlist: &Netlist, compiled: &CompiledCircuit, output_name: &str) -> Result<(), EquivalenceError> {
    let &(lx, ly, lz) = compiled
        .output_positions
        .get(output_name)
        .ok_or_else(|| EquivalenceError::LampNotPlaced { name: output_name.to_string() })?;
    let lamp_state = compiled.world.get(lx, ly, lz);
    if lamp_state.kind != BlockKind::Lamp {
        return Err(EquivalenceError::LampWrongKind { name: output_name.to_string(), position: (lx, ly, lz) });
    }

    let driving_gate = netlist
        .gates
        .iter()
        .find(|gate| gate.output == output_name)
        .expect("every declared output is driven by a gate -- checked by `compile` before this ever runs");
    let &(tx, ty, tz) = compiled
        .gate_output_positions
        .get(&driving_gate.output)
        .ok_or_else(|| EquivalenceError::TorchNotPlaced { gate: driving_gate.output.clone() })?;

    // The fixed relationship `emit` actually builds: one cell in the
    // torch's own output direction (its net's own source pin), then
    // straight down. Holds for a merge's own junction exactly the same way
    // (`place_merge_gate`'s own `output_offset` is `(0, 0, 0)`, so its own
    // `gate_output_positions` entry already *is* what a NOR's own torch
    // position would be for this same formula).
    let expected = Position::new(tx, ty, tz).offset(OUTPUT_DIRECTION).down();
    let expected = (expected.x, expected.y, expected.z);
    if expected != (lx, ly, lz) {
        return Err(EquivalenceError::LampNotAtFixedOffsetFromTorch {
            name: output_name.to_string(),
            expected,
            actual: (lx, ly, lz),
        });
    }

    Ok(())
}

/// Reduce `graph`'s edges into every real landing point, and `netlist`'s own
/// structure via [`resolve_contributors`], to the same netlist-level
/// vocabulary and compare them as multisets -- see this module's doc
/// comment for why this, combined with `compile`'s own invariants, is
/// enough to trust the compiled bytes match.
fn verify_input_edges_match_netlist(
    netlist: &Netlist,
    graph: &PrimitiveGraph,
    resolution: &NetlistResolution,
) -> Result<(), EquivalenceError> {
    // netlist-level -> gate-index name map, needed because
    // `Provenance::Gate` only ever carries a gate *index*, not its own
    // declared output name.
    let gate_name = |g: usize| netlist.gates[g].output.clone();

    let mut from_netlist: Vec<NetEdge> = Vec::new();
    for (g, gate) in netlist.gates.iter().enumerate() {
        if gate.is_merge() {
            for index in 0..gate.inputs.len() {
                if resolution.is_bare[&(g, index)] {
                    continue; // no node, no edge -- see `resolve_contributors`'s own doc comment.
                }
                let consumer = Contributor::IsolatingRepeater(gate.output.clone(), index).label();
                for producer in &resolution.contributors_of_input[&(g, index)] {
                    from_netlist.push(NetEdge { producer: producer.label(), consumer: consumer.clone() });
                }
            }
        } else {
            let consumer = gate.output.clone();
            for index in 0..gate.inputs.len() {
                for producer in &resolution.contributors_of_input[&(g, index)] {
                    from_netlist.push(NetEdge { producer: producer.label(), consumer: consumer.clone() });
                }
            }
        }
    }
    for output_name in &netlist.outputs {
        let driving_gate = netlist
            .gates
            .iter()
            .position(|gate| &gate.output == output_name)
            .expect("every declared output is driven by a gate -- checked by `compile` before this ever runs");
        let consumer = format!("OUTPUT:{output_name}");
        for producer in &resolution.contributors_of_gate[driving_gate] {
            from_netlist.push(NetEdge { producer: producer.label(), consumer: consumer.clone() });
        }
    }

    // Which real landing point (if any) owns a given node, as a
    // netlist-level string identity -- the graph-side half of the same
    // reduction. A `Nor`/`Buf` gate's own Torch and a merge's own
    // `IsolatingRepeater` are both landing points; a Lamp is handled
    // separately below since `Provenance::PrimaryOutput` already names it
    // directly.
    let landing_point_of: HashMap<NodeId, String> = graph
        .nodes
        .iter()
        .enumerate()
        .filter_map(|(id, node)| match &node.provenance {
            Provenance::Gate { gate, role: TemplateNode::Torch } => Some((id, gate_name(*gate))),
            Provenance::Gate { gate, role: TemplateNode::IsolatingRepeater(index) } => {
                Some((id, Contributor::IsolatingRepeater(gate_name(*gate), *index).label()))
            }
            _ => None,
        })
        .collect();

    let producer_label = |node: NodeId| -> String { contributor_label(netlist, graph, node) };

    let mut from_graph: Vec<NetEdge> = Vec::new();
    for edge in &graph.edges {
        let consumer = match &graph.nodes[edge.to].provenance {
            Provenance::PrimaryOutput { name } => format!("OUTPUT:{name}"),
            _ => match landing_point_of.get(&edge.to) {
                Some(consumer) => consumer.clone(),
                // An edge into a node this gate owns from a node the *same*
                // gate owns is an internal edge of the consumer's own
                // template (a `Buf`'s Torch->SecondTorch edge, say) --
                // never reachable here since `Nor`/`Buf` gates never survive
                // into `Netlist::gates` with more than one owned node (see
                // `primitive_graph::instantiate`'s own doc comment), but
                // kept as a skip rather than a panic in case that ever
                // changes.
                None => continue,
            },
        };
        from_graph.push(NetEdge { producer: producer_label(edge.from), consumer });
    }

    let mut tally: HashMap<NetEdge, i64> = HashMap::new();
    for edge in from_netlist {
        *tally.entry(edge).or_insert(0) += 1;
    }
    for edge in from_graph {
        *tally.entry(edge).or_insert(0) -= 1;
    }
    let only_in_netlist: i64 = tally.values().filter(|&&count| count > 0).sum();
    let only_in_graph: i64 = tally.values().filter(|&&count| count < 0).map(|count| -count).sum();

    if only_in_netlist != 0 || only_in_graph != 0 {
        return Err(EquivalenceError::InputEdgesDoNotMatchNetlist {
            only_in_graph: only_in_graph as usize,
            only_in_netlist: only_in_netlist as usize,
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::circuits::and4::build_and4_netlist;
    use crate::circuits::full_adder::build_full_adder_netlist;
    use crate::circuits::seven_segment::{build_seven_segment_netlist, build_single_segment_netlist};
    use crate::compile::compile;
    use crate::compile::primitive_graph::expand;
    use crate::compile::topology::Library;

    fn check(netlist: &Netlist) {
        let compiled = compile(netlist).expect("reference circuits compile");
        let library = Library::default_library();
        let graph = expand(netlist, &library).expect("reference circuits only use NOR gates of fan-in 1..=3");
        verify_expansion_matches_compiled(netlist, &graph, &compiled)
            .expect("the expanded graph must account for exactly what compile() built");
    }

    #[test]
    fn and4_expansion_matches_its_compiled_world() {
        let (netlist, _output) = build_and4_netlist();
        check(&netlist);
    }

    #[test]
    fn full_adder_expansion_matches_its_compiled_world() {
        let (netlist, _outputs) = build_full_adder_netlist();
        check(&netlist);
    }

    #[test]
    fn segment_a_expansion_matches_its_compiled_world() {
        let (netlist, _output) = build_single_segment_netlist(0);
        check(&netlist);
    }

    #[test]
    fn seven_segment_expansion_matches_its_compiled_world() {
        let (netlist, _outputs) = build_seven_segment_netlist();
        check(&netlist);
    }
}
