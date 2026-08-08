//! The converse of `equivalence::verify_expansion_matches_compiled`.
//!
//! That check asks "is everything the graph claims present in the world".
//! It cannot, by itself, catch a graph that silently drops an edge or a
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
//!   `PrimitiveGraph` node's own realisation -- a gate's own torch, a
//!   primary input's lever, or a declared output's lamp. A gate's support
//!   block and its per-input repeaters are **not** graph nodes any more
//!   (see `topology::TemplateNode`'s doc comment: a NOR is one torch node,
//!   and the block it is mounted on plus the repeaters that feed it are how
//!   an input physically reaches that torch, not part of the signal graph
//!   itself), so both fall into the fill buckets below instead of this one.
//! - [`WorldPartition::routing_fill_dust`]: any non-air `RedstoneWire`
//!   block. The graph never models dust at all (dust is the *medium* a
//!   signal travels through, never a node -- see `topology::Primitive`'s
//!   doc comment), so every dust block in the world is, by construction of
//!   this compiler, routing fill and nothing else -- no further check is
//!   possible or needed.
//! - [`WorldPartition::routing_fill_repeater`]: every non-air `Repeater`
//!   block, unconditionally. Earlier versions of this module excluded a
//!   gate's own declared-input repeaters from this bucket, because those
//!   were graph nodes (`TemplateNode::Input`); now that a NOR entry is a
//!   single torch with no repeater node of its own ("the repeaters at gate
//!   input sockets are not part of a NOR" -- they exist only because of how
//!   the emitter terminates a route), *every* repeater the world contains is
//!   realisation, whether it faces a gate's support or sits mid-route. This
//!   is only safe to bucket unconditionally because
//!   [`check_gate_input_arity_agrees`] independently confirms, *before* a
//!   single block is bucketed, that `Netlist`'s declared arity, the graph's
//!   own edge count into each gate, and the world's own count of
//!   repeaters-facing-the-support all agree for every gate -- see that
//!   function's doc comment for why a graph that silently dropped one of a
//!   gate's inputs still cannot pass this check unnoticed.
//! - [`WorldPartition::routing_fill_support`]: two things, both narrowly
//!   identified rather than guessed from shape. First, a NOR gate's own
//!   support block -- found by walking from that gate's own (graph-
//!   explained) torch position via `torch_support_position`, the exact same
//!   walk `equivalence::verify_gate_structure` uses, never inferred from
//!   "something is on top of it" (a NOR's inputs and torch sit on its
//!   support's horizontal faces, not above it, so that heuristic would miss
//!   it entirely). Second, a `Solid` block, not any gate's support, with a
//!   redstone conductor (dust, a repeater, or a lever) directly on top of
//!   it -- `src/compile/mod.rs` places a bare solid block in exactly two
//!   situations, and both satisfy this: `ensure_floor` (every one of its
//!   call sites sets `pos` to a conductor in the same breath it floors
//!   `pos.down()`) and `move_between_layers`'s climbing riser (`riser.up()`
//!   is always the new landing dust it was placed to hold). No other call
//!   site in this compiler ever places a bare `Solid` block outside
//!   `place_nor_gate`'s own support -- checked directly against the source,
//!   not assumed, since this is exactly the rule a later change could
//!   quietly invalidate.
//!
//! Anything else -- including `seal_cross_talk`'s reactive keep-out stone,
//! which this compiler's own doc comment on that function says has never
//! actually fired on any of the five reference circuits -- is
//! [`UnexplainedBlock`], never guessed into fill.

use std::collections::HashSet;

use super::primitive_graph::{NodeId, PrimitiveGraph, Provenance};
use super::topology::TemplateNode;
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
    /// `lay_track`/`ensure_floor`/`place_nor_gate` that the graph deliberately
    /// does not model.
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
    /// `Netlist`'s own declared arity, the number of edges the graph lands
    /// on this gate's own node cluster from outside it, and the compiled
    /// world's own independently-counted repeaters-facing-the-support
    /// disagree for this gate. This is precisely the failure this module
    /// exists to catch: a graph that silently dropped (or duplicated) one of
    /// a gate's inputs while the world -- built by `compile` straight from
    /// `Netlist`, entirely independent of the graph -- still has exactly
    /// what the netlist declared.
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
    let supports = known_support_positions(netlist, compiled)?;

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
                        if supports.contains(&pos) {
                            partition.routing_fill_support += 1;
                            continue;
                        }
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
/// graph's own edge count into that gate's node cluster, and the world's
/// own independently counted repeaters-facing-the-support all agree.
///
/// This is what stops [`WorldPartition::routing_fill_repeater`] from being
/// the "shrug" a bare `BlockKind::Repeater => fill` rule would otherwise be:
/// without it, a graph that quietly failed to wire one of a gate's inputs
/// would still find that socket occupied by a real repeater in the world
/// (`compile` builds the world straight from `Netlist`, entirely independent
/// of this graph), and a rule that buckets "any repeater not already
/// explained" as fill would absorb exactly the defect this whole module
/// exists to surface.
///
/// `graph_arity` here is *not* a count of `TemplateNode::Input` nodes --
/// that role no longer exists (see `topology`'s doc comment: a NOR is one
/// torch node, and its inputs are edges landing on it, not nodes of their
/// own). The independent third source this check needs is instead: the
/// number of edges the graph itself lands on gate `g`'s own node cluster
/// from *outside* it. That is computed purely from `graph.edges` and
/// `graph.gate_nodes` -- never from `Netlist`, never from the world -- so it
/// remains a genuinely separate source from the other two: an `expand` bug
/// that dropped one of a gate's producer edges shows up here as a lower
/// edge count, exactly as a dropped `Input` node used to show up as a lower
/// node count.
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
                let own: HashSet<NodeId> = nodes.iter().copied().collect();
                graph.edges.iter().filter(|e| own.contains(&e.to) && !own.contains(&e.from)).count()
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
/// off `compiled`'s recorded torch/lever/lamp positions.
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
        Provenance::Gate { gate, role: TemplateNode::Torch } => {
            let gate_name = &netlist.gates[*gate].output;
            let &(tx, ty, tz) = compiled.gate_output_positions.get(gate_name).ok_or_else(|| {
                PartitionError::CannotResolveNodePosition { detail: format!("gate `{gate_name}` has no recorded torch position") }
            })?;
            Ok(Position::new(tx, ty, tz))
        }
        // `SecondTorch` only exists in `topology::GateKind::Buf`'s entry, and
        // `primitive_graph::expand` never chooses it for a `Netlist::Gate`
        // (it always looks up `GateKind::Nor(arity)` -- see `expand`'s own
        // body): the Yosys frontend realizes `BUF` as two already-separate
        // NOR gates before a `Netlist` ever exists, so a compiled
        // `PrimitiveGraph`'s gate nodes are always plain `Torch`. Reachable
        // only if that stops being true, so this is a real gap to close
        // then, not dead code to delete now.
        Provenance::Gate { gate, role: TemplateNode::SecondTorch } => Err(PartitionError::CannotResolveNodePosition {
            detail: format!(
                "gate `{}`'s primitive graph has a `SecondTorch` node, which `compiled.gate_output_positions` \
                 has no recorded position for -- no multi-torch library entry is ever chosen for a real \
                 `Netlist::Gate` today",
                netlist.gates[*gate].output
            ),
        }),
        // Same situation as `SecondTorch` above: `primitive_graph::expand`
        // never chooses a `GateKind::Or` entry for a real `Netlist::Gate`
        // today (it always looks up `GateKind::Nor(arity)`), so this node
        // kind cannot appear in a `PrimitiveGraph` built from a real
        // `compile()` run yet. Reachable only once the frontend/genlib work
        // that maps `$_OR_` onto the library lands.
        Provenance::Gate { gate, role: TemplateNode::IsolatingRepeater(index) } => Err(PartitionError::CannotResolveNodePosition {
            detail: format!(
                "gate `{}`'s primitive graph has an `IsolatingRepeater({index})` node, which \
                 `compiled` has no recorded position for -- no OR library entry is ever chosen for a \
                 real `Netlist::Gate` today",
                netlist.gates[*gate].output
            ),
        }),
    }
}

/// Every position that is a NOR gate's own support block -- fill, not a
/// graph node (see this module's doc comment on `routing_fill_support`).
/// Found the same way `equivalence::verify_gate_structure` finds it: by
/// walking from the gate's own (already resolved, graph-explained) torch
/// position via `torch_support_position`, never guessed from the block's
/// shape or its neighbours' arrangement.
fn known_support_positions(netlist: &Netlist, compiled: &CompiledCircuit) -> Result<HashSet<Position>, PartitionError> {
    let mut supports = HashSet::with_capacity(netlist.gates.len());
    for gate in &netlist.gates {
        let &(tx, ty, tz) = compiled.gate_output_positions.get(&gate.output).ok_or_else(|| {
            PartitionError::CannotResolveNodePosition { detail: format!("gate `{}` has no recorded torch position", gate.output) }
        })?;
        let torch_pos = Position::new(tx, ty, tz);
        supports.insert(resolve_support(compiled, &gate.output, torch_pos)?);
    }
    Ok(supports)
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
    ///
    /// Sabotage here means removing one of the *edges* that lands on the
    /// gate's torch node from outside it -- a NOR gate is a single node now,
    /// so there is no separate `Input` node left to drop; the graph's own
    /// claim about its arity lives entirely in how many such edges point at
    /// it (see `check_gate_input_arity_agrees`'s doc comment).
    #[test]
    fn a_graph_missing_one_declared_input_is_reported_not_absorbed() {
        use crate::compile::Gate;

        let netlist = Netlist {
            inputs: vec!["a".to_string(), "b".to_string()],
            outputs: vec![],
            gates: vec![Gate {
                name: "g0".to_string(),
                inputs: vec!["a".to_string(), "b".to_string()],
                output: "g0".to_string(),
                is_merge: false,
            }],
        };
        let compiled = compile(&netlist).expect("a 2-input NOR gate compiles");

        let library = Library::default_library();
        let mut graph = expand(&netlist, &library).expect("a 2-input NOR gate has a library entry");

        // Sabotage: drop one of the two edges landing on gate 0's torch node
        // (from lever "b"), exactly what a buggy `expand` might do -- the
        // world itself is untouched, so it still has both repeaters.
        let torch = graph.gate_nodes[0][0];
        let lever_b = graph
            .nodes
            .iter()
            .position(|n| matches!(&n.provenance, Provenance::PrimaryInput { name } if name == "b"))
            .expect("lever \"b\" exists");
        let before = graph.edges.len();
        graph.edges.retain(|e| !(e.from == lever_b && e.to == torch));
        assert_eq!(graph.edges.len(), before - 1, "sabotage must remove exactly one edge");

        let err = partition_world(&netlist, &graph, &compiled).expect_err("the dropped input must be reported");
        assert_eq!(
            err,
            PartitionError::GateInputArityDisagreement { gate: "g0".to_string(), netlist_arity: 2, graph_arity: 1, world_arity: 2 }
        );
    }
}
