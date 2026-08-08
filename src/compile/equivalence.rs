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
//!   conditions this graph's own rigid and routable edges assert: a NOR
//!   gate's rigid inputs-converge-on-one-support-and-torch structure is
//!   `verify_torch_merge`'s "N sources, one sink" invariant; a net's
//!   routable edge delivering a real, undecayed signal from producer to
//!   every declared sink is exactly what `verify_connectivity` and
//!   `verify_signal_strength` already certify, on the *actual* emitted
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
//! and edges and nothing more. [`verify_expansion_matches_compiled`] is that
//! check:
//!
//! - **Rigid edges** are checked directly against the compiled `World`'s own
//!   bytes -- every gate's support block, its torch, and each of its input
//!   sockets' repeater, read back cell by cell and confirmed to match
//!   exactly the graph's own node count and rigid-edge structure for that
//!   gate (no more, no fewer inputs than the netlist declares); likewise a
//!   declared output's lamp, at the exact fixed offset `emit` places it.
//! - **Routable edges** are checked two ways: independently, by comparing
//!   the graph's own routable edges (translated back to netlist-level
//!   signal names, not internal node ids) against the net structure
//!   `Netlist` implies on its own, with no reference to `expand` at all; and
//!   by relying on `compile`'s own connectivity/torch-merge/signal-strength
//!   invariants, which have already read every byte of dust and every
//!   repeater the router placed for that same net and found it delivers
//!   exactly the declared producer to exactly the declared sinks. Together
//!   these two facts chain into "the graph's routable edges are what the
//!   compiled bytes actually implement" without this module re-walking dust
//!   itself.

use std::collections::HashSet;

use super::primitive_graph::{EdgeKind, PrimitiveGraph, Provenance};
use super::topology::TemplateNode;
use super::{CompiledCircuit, Netlist, INPUT_DIRECTIONS, OUTPUT_DIRECTION};
use crate::redstone::rules::taxonomy::flags_of;
use crate::redstone::simulator::component::torch_support_position;
use crate::redstone::simulator::position::Position;
use crate::redstone::world::block::BlockKind;

/// Why the compiled world does not match what `expand` says it should be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EquivalenceError {
    /// The graph's total node count does not match what `Netlist` alone
    /// implies (levers + lamps + `2 + arity` per gate).
    NodeCountMismatch { expected: usize, actual: usize },
    /// The graph's rigid or routable edge count does not match what
    /// `Netlist` alone implies.
    EdgeCountMismatch { expected_rigid: usize, actual_rigid: usize, expected_routable: usize, actual_routable: usize },
    /// A gate's own instance is missing a node for one of the fixed roles
    /// (`TemplateNode::Torch`, `TemplateNode::Support`, or one of its
    /// `Input`s) that every `topology::nor_entry` must provide.
    MissingRole { gate: String, role: TemplateNode },
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
    /// One of the graph's declared `Input` nodes for this gate has no
    /// matching repeater, facing into the support, at the corresponding
    /// `INPUT_DIRECTIONS` socket.
    InputSocketNotARepeaterFacingSupport { gate: String, input_index: usize, socket: (i32, i32, i32) },
    /// A socket direction the graph does *not* declare as one of this
    /// gate's inputs is nonetheless occupied by a repeater facing into the
    /// support -- an undeclared rigid input the graph is missing.
    UndeclaredInputFeedsSupport { gate: String, direction_index: usize, socket: (i32, i32, i32) },
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
    /// down) -- the physical realisation of the rigid torch-to-lamp edge.
    LampNotAtRigidOffsetFromTorch { name: String, expected: (i32, i32, i32), actual: (i32, i32, i32) },
    /// The graph's routable edges, translated back to netlist-level signal
    /// names, do not match the net structure `Netlist` implies on its own.
    RoutableEdgesDoNotMatchNetlist { only_in_graph: usize, only_in_netlist: usize },
}

impl std::fmt::Display for EquivalenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for EquivalenceError {}

/// One (producer, consuming gate, input index) triple, in netlist-level
/// terms rather than `NodeId`s -- what both sides of the routable-edge
/// cross-check in [`verify_expansion_matches_compiled`] are reduced to
/// before comparison.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum NetSink {
    /// `producer` is `Some(gate output name)` for a gate-driven signal, or
    /// `None` for a primary input.
    Edge { producer_is_primary_input: bool, producer_name: String, consumer_gate: String, input_index: usize },
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
    verify_counts(netlist, graph)?;

    for (g, gate) in netlist.gates.iter().enumerate() {
        verify_gate_rigid_structure(netlist, graph, compiled, g, gate.inputs.len())?;
    }

    for name in &netlist.inputs {
        verify_lever(compiled, name)?;
    }

    for output_name in &netlist.outputs {
        verify_lamp(netlist, compiled, output_name)?;
    }

    verify_routable_edges_match_netlist(netlist, graph)?;

    Ok(())
}

/// Node/edge totals `Netlist` alone implies, independent of `graph` -- the
/// first, cheapest cross-check that `expand` built neither too much nor too
/// little.
fn verify_counts(netlist: &Netlist, graph: &PrimitiveGraph) -> Result<(), EquivalenceError> {
    let mut expected_nodes = netlist.inputs.len() + netlist.outputs.len();
    let mut expected_rigid = netlist.outputs.len(); // one Torch->Lamp edge each
    let mut expected_routable = 0usize;
    for gate in &netlist.gates {
        let arity = gate.inputs.len();
        expected_nodes += 2 + arity; // Support + Torch + one node per input
        expected_rigid += arity + 1; // each Input->Support, plus Torch->Support
        expected_routable += arity; // each input is driven by exactly one routable edge
    }

    if graph.nodes.len() != expected_nodes {
        return Err(EquivalenceError::NodeCountMismatch { expected: expected_nodes, actual: graph.nodes.len() });
    }

    let actual_rigid = graph.edges.iter().filter(|e| e.kind == EdgeKind::Rigid).count();
    let actual_routable = graph.edges.iter().filter(|e| e.kind == EdgeKind::Routable).count();
    if actual_rigid != expected_rigid || actual_routable != expected_routable {
        return Err(EquivalenceError::EdgeCountMismatch {
            expected_rigid,
            actual_rigid,
            expected_routable,
            actual_routable,
        });
    }

    Ok(())
}

/// Check gate `g`'s rigid substructure (support, torch, `arity` input
/// sockets) directly against the compiled world's own bytes.
fn verify_gate_rigid_structure(
    netlist: &Netlist,
    graph: &PrimitiveGraph,
    compiled: &CompiledCircuit,
    g: usize,
    arity: usize,
) -> Result<(), EquivalenceError> {
    let gate_name = netlist.gates[g].output.clone();

    // The graph must have exactly the roles a NOR entry provides: one
    // Support, one Torch, and `arity` Input nodes, for this gate.
    let has_role = |role: TemplateNode| {
        graph.gate_nodes[g]
            .iter()
            .any(|&id| matches!(&graph.nodes[id].provenance, Provenance::Gate { gate, role: r } if *gate == g && *r == role))
    };
    if !has_role(TemplateNode::Support) {
        return Err(EquivalenceError::MissingRole { gate: gate_name, role: TemplateNode::Support });
    }
    if !has_role(TemplateNode::Torch) {
        return Err(EquivalenceError::MissingRole { gate: gate_name, role: TemplateNode::Torch });
    }
    for i in 0..arity {
        if !has_role(TemplateNode::Input(i)) {
            return Err(EquivalenceError::MissingRole { gate: gate_name, role: TemplateNode::Input(i) });
        }
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
    // declared ones must hold a repeater facing into the support; the rest
    // must not (or the graph is missing a rigid input the world actually
    // has).
    for (index, &direction) in INPUT_DIRECTIONS.iter().enumerate() {
        let socket = support.offset(direction);
        let socket_state = compiled.world.get(socket.x, socket.y, socket.z);
        let feeds_support = socket_state.kind == BlockKind::Repeater && socket_state.facing == Some(direction);

        if index < arity {
            if !feeds_support {
                return Err(EquivalenceError::InputSocketNotARepeaterFacingSupport {
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
    // straight down.
    let expected = Position::new(tx, ty, tz).offset(OUTPUT_DIRECTION).down();
    let expected = (expected.x, expected.y, expected.z);
    if expected != (lx, ly, lz) {
        return Err(EquivalenceError::LampNotAtRigidOffsetFromTorch {
            name: output_name.to_string(),
            expected,
            actual: (lx, ly, lz),
        });
    }

    Ok(())
}

/// Reduce `graph`'s routable edges, and `netlist`'s own gate-input
/// structure, to the same netlist-level vocabulary and compare them as
/// sets -- see this module's doc comment for why this, combined with
/// `compile`'s own invariants, is enough to trust the compiled bytes match.
fn verify_routable_edges_match_netlist(netlist: &Netlist, graph: &PrimitiveGraph) -> Result<(), EquivalenceError> {
    let from_netlist: HashSet<NetSink> = netlist
        .gates
        .iter()
        .flat_map(|gate| {
            let consumer_gate = gate.output.clone();
            gate.inputs.iter().enumerate().map(move |(input_index, producer_name)| NetSink::Edge {
                producer_is_primary_input: netlist.inputs.iter().any(|i| i == producer_name),
                producer_name: producer_name.clone(),
                consumer_gate: consumer_gate.clone(),
                input_index,
            })
        })
        .collect();

    let from_graph: HashSet<NetSink> = graph
        .edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::Routable)
        .map(|edge| {
            let producer = &graph.nodes[edge.from].provenance;
            let (producer_is_primary_input, producer_name) = match producer {
                Provenance::PrimaryInput { name } => (true, name.clone()),
                Provenance::Gate { role: TemplateNode::Torch, .. } => {
                    let gate = gate_output_name(netlist, producer);
                    (false, gate)
                }
                other => panic!("a routable edge's source is never {other:?}"),
            };
            let consumer = &graph.nodes[edge.to].provenance;
            let (consumer_gate, input_index) = match consumer {
                Provenance::Gate { role: TemplateNode::Input(i), gate } => (netlist.gates[*gate].output.clone(), *i),
                other => panic!("a routable edge's target is never {other:?}"),
            };
            NetSink::Edge { producer_is_primary_input, producer_name, consumer_gate, input_index }
        })
        .collect();

    if from_netlist != from_graph {
        let only_in_graph = from_graph.difference(&from_netlist).count();
        let only_in_netlist = from_netlist.difference(&from_graph).count();
        return Err(EquivalenceError::RoutableEdgesDoNotMatchNetlist { only_in_graph, only_in_netlist });
    }

    Ok(())
}

fn gate_output_name(netlist: &Netlist, provenance: &Provenance) -> String {
    match provenance {
        Provenance::Gate { gate, .. } => netlist.gates[*gate].output.clone(),
        _ => panic!("gate_output_name called on a non-gate provenance"),
    }
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
