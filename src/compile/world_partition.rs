//! The converse of `equivalence::verify_expansion_matches_compiled`.
//!
//! That check asks "is everything the graph claims present in the world".
//! It cannot, by itself, catch a graph that silently drops a rigid edge or a
//! whole primitive: nothing in a forward-only check ever looks at a block
//! the graph never mentioned and asks what explains it. This module asks
//! the other direction: **partition every non-air block of the compiled
//! world**, and require the partition to be total. A block that fits
//! neither "explained by a graph node" nor a narrowly-defined "routing
//! fill" is reported by name, not folded into fill -- see
//! [`partition_world`]'s doc comment for exactly how narrow that definition
//! is and why it is safe.
//!
//! # The four categories
//!
//! - [`WorldPartition::graph_explained`]: a block that is exactly one
//!   `PrimitiveGraph` node's own realisation -- a gate's support or torch,
//!   one of its declared inputs' repeater, a primary input's lever, or a
//!   declared output's lamp.
//! - [`WorldPartition::routing_fill_dust`]: any non-air `RedstoneWire`
//!   block. The graph never models dust at all (see
//!   `topology::Primitive::Dust`'s doc comment: a routable edge becomes a
//!   dust/repeater chain only once a planner exists to decide its length
//!   and bends), so every dust block in the world is, by construction of
//!   this compiler, routing fill and nothing else -- no further check is
//!   possible or needed.
//! - [`WorldPartition::routing_fill_repeater`]: a repeater that is not one
//!   of a gate's declared input sockets. This is only safe to bucket as
//!   fill *after* [`partition_world`] has independently confirmed
//!   (`check_gate_input_arity_agrees`) that `Netlist`, `PrimitiveGraph` and
//!   the world's own repeaters-facing-a-support count all agree, for every
//!   gate -- otherwise a repeater the graph silently failed to claim as an
//!   input would land here instead of being reported. See that function's
//!   doc comment for the full argument.
//! - [`WorldPartition::routing_fill_support`]: a `Solid` block, not a
//!   gate's own support, with a redstone conductor (dust, a repeater, or a
//!   lever) directly on top of it. `src/compile/mod.rs` places a bare solid
//!   block in exactly two situations, and both satisfy this: `ensure_floor`
//!   (every one of its call sites sets `pos` to a conductor in the same
//!   breath it floors `pos.down()`) and `move_between_layers`'s climbing
//!   riser (`riser.up()` is always the new landing dust it was placed to
//!   hold). No other call site in this compiler ever places a bare `Solid`
//!   block outside `place_nor_gate`'s own support -- checked directly
//!   against the source, not assumed, since this is exactly the rule a
//!   later change could quietly invalidate.
//!
//! Anything else -- including `seal_cross_talk`'s reactive keep-out stone,
//! which this compiler's own doc comment on that function says has never
//! actually fired on any of the five reference circuits -- is
//! [`UnexplainedBlock`], never guessed into fill.

use std::collections::HashSet;

use super::primitive_graph::PrimitiveGraph;
use super::topology::TemplateNode;
use super::primitive_graph::Provenance;
use super::{CompiledCircuit, Netlist, INPUT_DIRECTIONS};
use crate::redstone::simulator::component::torch_support_position;
use crate::redstone::simulator::position::Position;
use crate::redstone::world::block::BlockKind;

/// A non-air block that [`partition_world`] could attribute to neither the
/// graph nor its narrow fill rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnexplainedBlock {
    pub position: (i32, i32, i32),
    pub kind: BlockKind,
}

/// The result of partitioning a compiled world's non-air blocks -- see this
/// module's doc comment for what each field means.
#[derive(Debug, Clone, Default)]
pub struct WorldPartition {
    pub graph_explained: usize,
    pub routing_fill_dust: usize,
    pub routing_fill_repeater: usize,
    pub routing_fill_support: usize,
    pub unexplained: Vec<UnexplainedBlock>,
}

impl WorldPartition {
    /// Every block placed by `lay_dust_run`/`lay_bent_path`/`move_between_layers`/
    /// `lay_track`/`ensure_floor` that the graph deliberately does not model.
    pub fn routing_fill(&self) -> usize {
        self.routing_fill_dust + self.routing_fill_repeater + self.routing_fill_support
    }

    /// `graph_explained + routing_fill() + unexplained.len()` -- every
    /// non-air block `partition_world` visited, however it was classified.
    pub fn total_non_air(&self) -> usize {
        self.graph_explained + self.routing_fill() + self.unexplained.len()
    }

    /// `graph_explained` as a fraction of `total_non_air()` -- how much of
    /// the emitted world the topology layer actually describes, at this
    /// circuit's size. `None` for an empty world (never happens for a real
    /// circuit, but avoids a division by zero for a degenerate one).
    pub fn graph_explained_ratio(&self) -> Option<f64> {
        let total = self.total_non_air();
        (total > 0).then(|| self.graph_explained as f64 / total as f64)
    }
}

/// Why [`partition_world`] could not even attempt the partition -- a
/// structural disagreement between `Netlist`, `PrimitiveGraph` and the
/// compiled `World` serious enough that no partition would mean anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PartitionError {
    /// `Netlist`'s own declared arity, `PrimitiveGraph::gate_nodes`' input
    /// node count, and the compiled world's own independently-counted
    /// repeaters-facing-the-support disagree for this gate. This is
    /// precisely the failure this module exists to catch: a graph that
    /// silently dropped (or duplicated) one of a gate's rigid inputs while
    /// the world -- built by `compile` straight from `Netlist`, entirely
    /// independent of the graph -- still has exactly what the netlist
    /// declared.
    GateInputArityDisagreement { gate: String, netlist_arity: usize, graph_arity: usize, world_arity: usize },
    /// Two different graph nodes resolved to the same world position -- the
    /// graph cannot be a lossless account of the world if two of its own
    /// primitives collide.
    DuplicateExplainedPosition { position: (i32, i32, i32) },
    /// A graph node's own resolved position holds nothing (`Air`) in the
    /// compiled world -- the graph claims a primitive the world never
    /// actually built.
    ExplainedPositionIsAir { position: (i32, i32, i32) },
    /// A graph node could not be resolved to a world position at all --
    /// some `CompiledCircuit` lookup it depends on came back empty.
    CannotResolveNodePosition { detail: String },
}

impl std::fmt::Display for PartitionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for PartitionError {}

/// Partition every non-air block of `compiled.world` into
/// [`WorldPartition::graph_explained`], the three routing-fill buckets, or
/// [`UnexplainedBlock`] -- see this module's doc comment for the exact rule
/// each category uses. `graph` must be `primitive_graph::expand(netlist,
/// ..)`'s output for this same `netlist`, and `compiled` must be
/// `compile(netlist)`'s output for the same `netlist`.
///
/// This does not call `equivalence::verify_expansion_matches_compiled` --
/// the two checks are deliberately independent implementations of
/// overlapping guarantees, not one built on top of the other, so a bug in
/// one is unlikely to also be a bug in the other.
pub fn partition_world(
    netlist: &Netlist,
    graph: &PrimitiveGraph,
    compiled: &CompiledCircuit,
) -> Result<WorldPartition, PartitionError> {
    // Checked first, and separately from the position-by-position walk
    // below: this is the one fact that makes bucketing an unclaimed
    // repeater as "routing fill" safe at all. See this function's own doc
    // comment on `WorldPartition::routing_fill_repeater`.
    check_gate_input_arity_agrees(netlist, graph, compiled)?;

    let explained = explained_positions(netlist, graph, compiled)?;

    let (size_x, size_y, size_z) = compiled.world.size();
    let mut found: HashSet<Position> = HashSet::with_capacity(explained.len());
    let mut partition = WorldPartition::default();

    for x in 0..size_x {
        for y in 0..size_y {
            for z in 0..size_z {
                let state = compiled.world.get(x, y, z);
                if state.kind == BlockKind::Air {
                    continue;
                }
                let pos = Position::new(x, y, z);
                if explained.contains(&pos) {
                    partition.graph_explained += 1;
                    found.insert(pos);
                    continue;
                }
                match state.kind {
                    BlockKind::RedstoneWire => partition.routing_fill_dust += 1,
                    BlockKind::Repeater => partition.routing_fill_repeater += 1,
                    BlockKind::Solid => {
                        let above = compiled.world.get(x, y + 1, z);
                        if matches!(above.kind, BlockKind::RedstoneWire | BlockKind::Repeater | BlockKind::Lever) {
                            partition.routing_fill_support += 1;
                        } else {
                            partition.unexplained.push(UnexplainedBlock { position: (x, y, z), kind: state.kind });
                        }
                    }
                    other => partition.unexplained.push(UnexplainedBlock { position: (x, y, z), kind: other }),
                }
            }
        }
    }

    if found.len() != explained.len() {
        let missing = *explained
            .iter()
            .find(|p| !found.contains(p))
            .expect("found is a subset of explained with a smaller length, so a missing element exists");
        return Err(PartitionError::ExplainedPositionIsAir { position: (missing.x, missing.y, missing.z) });
    }

    Ok(partition)
}

/// Cross-check, for every gate, that `Netlist`'s declared arity, the
/// graph's own `Input` node count, and the world's own independently
/// counted repeaters-facing-the-support all agree.
///
/// This is what stops [`WorldPartition::routing_fill_repeater`] from being
/// the "shrug" a bare `BlockKind::Repeater => fill` rule would otherwise be:
/// without it, a graph that quietly failed to instantiate one of a gate's
/// `Input` nodes would still find that socket occupied by a real repeater
/// in the world (`compile` builds the world straight from `Netlist`,
/// entirely independent of this graph), and a rule that buckets "any
/// repeater not already explained" as fill would absorb exactly the defect
/// this whole module exists to surface. Checking the three counts against
/// each other -- not just "does the graph's own claimed structure look
/// internally consistent" -- closes that gap before a single block is ever
/// bucketed.
fn check_gate_input_arity_agrees(
    netlist: &Netlist,
    graph: &PrimitiveGraph,
    compiled: &CompiledCircuit,
) -> Result<(), PartitionError> {
    for (g, gate) in netlist.gates.iter().enumerate() {
        let netlist_arity = gate.inputs.len();

        let graph_arity = graph
            .gate_nodes
            .get(g)
            .map(|nodes| {
                nodes
                    .iter()
                    .filter(|&&id| {
                        matches!(&graph.nodes[id].provenance, Provenance::Gate { gate: gg, role: TemplateNode::Input(_) } if *gg == g)
                    })
                    .count()
            })
            .unwrap_or(0);

        let &(tx, ty, tz) = compiled.gate_output_positions.get(&gate.output).ok_or_else(|| {
            PartitionError::CannotResolveNodePosition { detail: format!("gate `{}` has no recorded torch position", gate.output) }
        })?;
        let torch_pos = Position::new(tx, ty, tz);
        let torch_state = compiled.world.get(tx, ty, tz);
        let world_arity = match torch_support_position(torch_state, torch_pos) {
            Some(support) => INPUT_DIRECTIONS
                .iter()
                .filter(|&&direction| {
                    let socket = support.offset(direction);
                    let socket_state = compiled.world.get(socket.x, socket.y, socket.z);
                    socket_state.kind == BlockKind::Repeater && socket_state.facing == Some(direction)
                })
                .count(),
            // No resolvable support at all: as far as the world is
            // concerned, this gate has zero working inputs, whatever the
            // netlist or graph claim.
            None => 0,
        };

        if netlist_arity != graph_arity || netlist_arity != world_arity {
            return Err(PartitionError::GateInputArityDisagreement {
                gate: gate.output.clone(),
                netlist_arity,
                graph_arity,
                world_arity,
            });
        }
    }
    Ok(())
}

/// Every position the graph's own nodes resolve to, given `compiled`'s
/// recorded torch/lever/lamp positions -- the authoritative "explained by
/// the graph" set `partition_world` checks every non-air block against
/// first.
fn explained_positions(
    netlist: &Netlist,
    graph: &PrimitiveGraph,
    compiled: &CompiledCircuit,
) -> Result<HashSet<Position>, PartitionError> {
    let mut explained = HashSet::with_capacity(graph.nodes.len());
    for node in &graph.nodes {
        let pos = resolve_node_position(netlist, compiled, &node.provenance)?;
        if !explained.insert(pos) {
            return Err(PartitionError::DuplicateExplainedPosition { position: (pos.x, pos.y, pos.z) });
        }
    }
    Ok(explained)
}

/// Where one graph node's own `Provenance` says it must physically be, read
/// off `compiled`'s recorded torch/lever/lamp positions (never off the
/// graph's own say-so alone -- `TemplateNode::Support`/`Input` positions are
/// derived by resolving the *real* torch's *real* support, exactly as
/// `equivalence::verify_gate_rigid_structure` does).
fn resolve_node_position(
    netlist: &Netlist,
    compiled: &CompiledCircuit,
    provenance: &Provenance,
) -> Result<Position, PartitionError> {
    match provenance {
        Provenance::PrimaryInput { name } => {
            let &(x, y, z) = compiled.input_positions.get(name).ok_or_else(|| {
                PartitionError::CannotResolveNodePosition { detail: format!("primary input `{name}` has no recorded lever position") }
            })?;
            Ok(Position::new(x, y, z))
        }
        Provenance::PrimaryOutput { name } => {
            let &(x, y, z) = compiled.output_positions.get(name).ok_or_else(|| {
                PartitionError::CannotResolveNodePosition { detail: format!("declared output `{name}` has no recorded lamp position") }
            })?;
            Ok(Position::new(x, y, z))
        }
        Provenance::Gate { gate, role } => {
            let gate_name = &netlist.gates[*gate].output;
            let &(tx, ty, tz) = compiled.gate_output_positions.get(gate_name).ok_or_else(|| {
                PartitionError::CannotResolveNodePosition { detail: format!("gate `{gate_name}` has no recorded torch position") }
            })?;
            let torch_pos = Position::new(tx, ty, tz);
            match role {
                TemplateNode::Torch => Ok(torch_pos),
                TemplateNode::Support => resolve_support(compiled, gate_name, torch_pos),
                TemplateNode::Input(i) => {
                    let support = resolve_support(compiled, gate_name, torch_pos)?;
                    let &direction = INPUT_DIRECTIONS.get(*i).ok_or_else(|| PartitionError::CannotResolveNodePosition {
                        detail: format!("gate `{gate_name}` declares input index {i}, outside INPUT_DIRECTIONS' range"),
                    })?;
                    Ok(support.offset(direction))
                }
            }
        }
    }
}

fn resolve_support(compiled: &CompiledCircuit, gate_name: &str, torch_pos: Position) -> Result<Position, PartitionError> {
    let torch_state = compiled.world.get(torch_pos.x, torch_pos.y, torch_pos.z);
    torch_support_position(torch_state, torch_pos).ok_or_else(|| PartitionError::CannotResolveNodePosition {
        detail: format!("gate `{gate_name}`'s torch has no resolvable support"),
    })
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

    fn check(label: &str, netlist: &Netlist) -> WorldPartition {
        let compiled = compile(netlist).expect("reference circuits compile");
        let library = Library::default_library();
        let graph = expand(netlist, &library).expect("reference circuits only use NOR gates of fan-in 1..=3");
        let partition = partition_world(netlist, &graph, &compiled)
            .unwrap_or_else(|e| panic!("{label}: could not partition the world: {e}"));

        assert!(
            partition.unexplained.is_empty(),
            "{label}: {} block(s) neither explained by the graph nor narrowly-defined routing fill: {:?}",
            partition.unexplained.len(),
            partition.unexplained
        );

        eprintln!(
            "{label}: {} blocks total -- {} graph-explained, {} routing fill (dust {}, repeater {}, support {}), \
             {} unexplained ({:.1}% explained by the graph)",
            partition.total_non_air(),
            partition.graph_explained,
            partition.routing_fill(),
            partition.routing_fill_dust,
            partition.routing_fill_repeater,
            partition.routing_fill_support,
            partition.unexplained.len(),
            partition.graph_explained_ratio().unwrap_or(0.0) * 100.0,
        );

        partition
    }

    #[test]
    fn and4_world_is_fully_explained() {
        let (netlist, _output) = build_and4_netlist();
        check("and4", &netlist);
    }

    #[test]
    fn full_adder_world_is_fully_explained() {
        let (netlist, _outputs) = build_full_adder_netlist();
        check("full_adder", &netlist);
    }

    #[test]
    fn segment_a_world_is_fully_explained() {
        let (netlist, _output) = build_single_segment_netlist(0);
        check("segment_a", &netlist);
    }

    #[test]
    fn seven_segment_world_is_fully_explained() {
        let (netlist, _outputs) = build_seven_segment_netlist();
        check("seven_segment", &netlist);
    }

    /// A graph missing one of a gate's declared inputs must be caught as a
    /// named arity disagreement, never silently absorbed into "routing
    /// fill" -- the exact failure mode this module exists to prevent. Built
    /// directly (not via `expand`, which never produces this shape) so the
    /// test does not depend on being able to sabotage `expand` itself.
    #[test]
    fn a_graph_missing_one_declared_input_is_reported_not_absorbed() {
        use crate::compile::Gate;

        let netlist = Netlist {
            inputs: vec!["a".to_string(), "b".to_string()],
            outputs: vec![],
            gates: vec![Gate { name: "g0".to_string(), inputs: vec!["a".to_string(), "b".to_string()], output: "g0".to_string() }],
        };
        let compiled = compile(&netlist).expect("a 2-input NOR gate compiles");

        let library = Library::default_library();
        let mut graph = expand(&netlist, &library).expect("a 2-input NOR gate has a library entry");

        // Sabotage: drop gate 0's second Input node and the routable edge
        // that fed it, exactly what a buggy `expand` might do -- the world
        // itself is untouched, so it still has both repeaters.
        let dropped = graph
            .gate_nodes[0]
            .iter()
            .position(|&id| matches!(&graph.nodes[id].provenance, Provenance::Gate { role: TemplateNode::Input(1), .. }))
            .expect("gate 0 has an Input(1) node before sabotage");
        let dropped_id = graph.gate_nodes[0].remove(dropped);
        graph.edges.retain(|e| e.to != dropped_id);

        let err = partition_world(&netlist, &graph, &compiled).expect_err("the dropped input must be reported");
        assert_eq!(
            err,
            PartitionError::GateInputArityDisagreement { gate: "g0".to_string(), netlist_arity: 2, graph_arity: 1, world_arity: 2 }
        );
    }
}
