//! Pure topology analysis for topology-aware seed placement.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use thiserror::Error;

use crate::compile::fragment_synth::identity::{InstanceId, PrimitiveId};
use crate::compile::fragment_synth::instance_graph::{
    InstanceDriver, InstanceGraph, PhysicalDriver, PhysicalSink,
};
use crate::compile::fragment_synth::topology::{
    ConnectionSource, ConnectionTarget, ContributorSpec, OutputSpec, ValidatedTopology,
};
use crate::compile::topology::Primitive;
use crate::redstone::simulator::component::{
    COMPARATOR_DELAY_GAME_TICKS, REPEATER_GAME_TICKS_PER_REDSTONE_TICK, TORCH_DELAY_GAME_TICKS,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct NodeFacts {
    pub predecessors: Vec<InstanceId>,
    pub successors: Vec<InstanceId>,
    pub forward_level: u64,
    pub reverse_level: u64,
    pub head_ticks: u64,
    pub tail_ticks: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct EdgeFacts {
    pub source: InstanceId,
    pub sink: InstanceId,
    pub structural_slack_ticks: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct SeedPlacementAnalysis {
    pub order: Vec<InstanceId>,
    pub nodes: BTreeMap<InstanceId, NodeFacts>,
    pub edges: Vec<EdgeFacts>,
    pub critical_delay_ticks: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum SeedPlacementError {
    #[error("instance identity {instance:?} appears more than once")]
    DuplicateInstance { instance: InstanceId },
    #[error("dependency names missing instance {instance:?}")]
    UnknownInstance { instance: InstanceId },
    #[error("instance dependency graph contains a cycle among {instances:?}")]
    DependencyCycle { instances: Vec<InstanceId> },
    #[error("selected topology for {instance:?} contains an unresolved primitive dependency")]
    UnresolvedTopology { instance: InstanceId },
    #[error("structural timing delay overflowed u64 game ticks")]
    TimingOverflow,
}

pub(crate) fn analyse_instance_dag(
    graph: &InstanceGraph,
) -> Result<SeedPlacementAnalysis, SeedPlacementError> {
    let ids = graph
        .instances
        .iter()
        .map(|instance| instance.id)
        .collect::<BTreeSet<_>>();
    if ids.len() != graph.instances.len() {
        let mut seen = BTreeSet::new();
        let instance = graph
            .instances
            .iter()
            .map(|instance| instance.id)
            .find(|instance| !seen.insert(*instance))
            .expect("different instance and identity counts imply a duplicate");
        return Err(SeedPlacementError::DuplicateInstance { instance });
    }

    let mut predecessors = ids
        .iter()
        .copied()
        .map(|id| (id, BTreeSet::new()))
        .collect::<BTreeMap<_, _>>();
    let mut successors = predecessors.clone();
    let mut structural_edges = BTreeSet::new();
    let mut declared_output_drivers = BTreeSet::new();

    for assignment in &graph.assignments {
        let PhysicalDriver::Instance(driver) = &assignment.driver else {
            continue;
        };
        let source = instance_driver_owner(driver);
        require_instance(&ids, source)?;
        match assignment.sink {
            PhysicalSink::InstanceInput { instance: sink, .. } => {
                require_instance(&ids, sink)?;
                if structural_edges.insert((source, sink)) {
                    predecessors
                        .get_mut(&sink)
                        .expect("known sink has predecessor storage")
                        .insert(source);
                    successors
                        .get_mut(&source)
                        .expect("known source has successor storage")
                        .insert(sink);
                }
            }
            PhysicalSink::DeclaredOutput(_) => {
                declared_output_drivers.insert(source);
            }
        }
    }

    let mut indegree = predecessors
        .iter()
        .map(|(&id, incoming)| (id, incoming.len()))
        .collect::<BTreeMap<_, _>>();
    let mut ready = indegree
        .iter()
        .filter_map(|(&id, &degree)| (degree == 0).then_some(id))
        .collect::<BTreeSet<_>>();
    let mut order = Vec::with_capacity(ids.len());
    while let Some(&next) = ready.iter().next() {
        ready.remove(&next);
        order.push(next);
        for &successor in &successors[&next] {
            let remaining = indegree
                .get_mut(&successor)
                .expect("known successor has an indegree");
            *remaining -= 1;
            if *remaining == 0 {
                ready.insert(successor);
            }
        }
    }
    if order.len() != ids.len() {
        let instances = indegree
            .into_iter()
            .filter_map(|(id, degree)| (degree > 0).then_some(id))
            .collect();
        return Err(SeedPlacementError::DependencyCycle { instances });
    }

    let instance_delays = graph
        .instances
        .iter()
        .map(|instance| {
            topology_delay_ticks(instance.id, &instance.expanded.topology)
                .map(|delay| (instance.id, delay))
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;

    let mut forward_levels = BTreeMap::<InstanceId, u64>::new();
    let mut head_ticks = BTreeMap::<InstanceId, u64>::new();
    for &id in &order {
        let forward_level = predecessors[&id]
            .iter()
            .map(|predecessor| forward_levels[predecessor] + 1)
            .max()
            .unwrap_or(0);
        let upstream_ticks = predecessors[&id]
            .iter()
            .map(|predecessor| head_ticks[predecessor])
            .max()
            .unwrap_or(0);
        let head = upstream_ticks
            .checked_add(instance_delays[&id])
            .ok_or(SeedPlacementError::TimingOverflow)?;
        forward_levels.insert(id, forward_level);
        head_ticks.insert(id, head);
    }

    let critical_delay_ticks = declared_output_drivers
        .iter()
        .map(|driver| head_ticks[driver])
        .max()
        .unwrap_or(0);
    let mut reverse_levels = BTreeMap::<InstanceId, u64>::new();
    let mut tail_ticks = BTreeMap::<InstanceId, u64>::new();
    let mut reaches_declared_output = BTreeSet::new();
    for &id in order.iter().rev() {
        let mut reverse_level = declared_output_drivers.contains(&id).then_some(0);
        let mut downstream_ticks = declared_output_drivers.contains(&id).then_some(0);
        for successor in &successors[&id] {
            if !reaches_declared_output.contains(successor) {
                continue;
            }
            reverse_level = Some(
                reverse_level.unwrap_or(0).max(
                    reverse_levels[successor]
                        .checked_add(1)
                        .ok_or(SeedPlacementError::TimingOverflow)?,
                ),
            );
            downstream_ticks = Some(downstream_ticks.unwrap_or(0).max(tail_ticks[successor]));
        }
        let reaches_output = reverse_level.is_some();
        let tail = instance_delays[&id]
            .checked_add(downstream_ticks.unwrap_or(0))
            .ok_or(SeedPlacementError::TimingOverflow)?;
        reverse_levels.insert(id, reverse_level.unwrap_or(0));
        tail_ticks.insert(id, tail);
        if reaches_output {
            reaches_declared_output.insert(id);
        }
    }

    let nodes = ids
        .iter()
        .copied()
        .map(|id| {
            (
                id,
                NodeFacts {
                    predecessors: predecessors[&id].iter().copied().collect(),
                    successors: successors[&id].iter().copied().collect(),
                    forward_level: forward_levels[&id],
                    reverse_level: reverse_levels[&id],
                    head_ticks: head_ticks[&id],
                    tail_ticks: tail_ticks[&id],
                },
            )
        })
        .collect();
    let edges = structural_edges
        .into_iter()
        .map(|(source, sink)| {
            let path_ticks = head_ticks[&source]
                .checked_add(tail_ticks[&sink])
                .ok_or(SeedPlacementError::TimingOverflow)?;
            Ok(EdgeFacts {
                source,
                sink,
                structural_slack_ticks: critical_delay_ticks.saturating_sub(path_ticks),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(SeedPlacementAnalysis {
        order,
        nodes,
        edges,
        critical_delay_ticks,
    })
}

fn instance_driver_owner(driver: &InstanceDriver) -> InstanceId {
    match driver {
        InstanceDriver::Primitive { logical_owner, .. }
        | InstanceDriver::Junction { logical_owner, .. } => *logical_owner,
    }
}

fn require_instance(
    ids: &BTreeSet<InstanceId>,
    instance: InstanceId,
) -> Result<(), SeedPlacementError> {
    if ids.contains(&instance) {
        Ok(())
    } else {
        Err(SeedPlacementError::UnknownInstance { instance })
    }
}

fn topology_delay_ticks(
    instance: InstanceId,
    topology: &ValidatedTopology,
) -> Result<u64, SeedPlacementError> {
    let mut delays = BTreeMap::<PrimitiveId, u64>::new();
    while delays.len() < topology.primitives.len() {
        let mut progressed = false;
        for specification in &topology.primitives {
            if delays.contains_key(&specification.id) {
                continue;
            }
            let mut upstream = 0;
            let mut unresolved = false;
            for connection in topology.connections.iter().filter(|connection| {
                connection.target == ConnectionTarget::Primitive(specification.id)
            }) {
                match connection.source {
                    ConnectionSource::ExternalInput { .. } => {}
                    ConnectionSource::Primitive(source) => {
                        let Some(&delay) = delays.get(&source) else {
                            unresolved = true;
                            break;
                        };
                        upstream = upstream.max(delay);
                    }
                }
            }
            if unresolved {
                continue;
            }
            let delay = upstream
                .checked_add(primitive_delay_ticks(specification.primitive))
                .ok_or(SeedPlacementError::TimingOverflow)?;
            delays.insert(specification.id, delay);
            progressed = true;
        }
        if !progressed {
            return Err(SeedPlacementError::UnresolvedTopology { instance });
        }
    }

    match &topology.output {
        OutputSpec::Primitive(primitive) => delays
            .get(primitive)
            .copied()
            .ok_or(SeedPlacementError::UnresolvedTopology { instance }),
        OutputSpec::Junction { contributors, .. } => contributors
            .iter()
            .map(|contributor| match *contributor {
                ContributorSpec::Primitive(primitive) => delays.get(&primitive).copied(),
                ContributorSpec::Landing(connection) => topology
                    .connections
                    .iter()
                    .find(|candidate| candidate.id == connection)
                    .and_then(|connection| match connection.source {
                        ConnectionSource::ExternalInput { .. } => Some(0),
                        ConnectionSource::Primitive(primitive) => delays.get(&primitive).copied(),
                    }),
            })
            .collect::<Option<Vec<_>>>()
            .map(|delays| delays.into_iter().max().unwrap_or(0))
            .ok_or(SeedPlacementError::UnresolvedTopology { instance }),
    }
}

const fn primitive_delay_ticks(primitive: Primitive) -> u64 {
    match primitive {
        Primitive::Torch => TORCH_DELAY_GAME_TICKS,
        Primitive::Repeater => REPEATER_GAME_TICKS_PER_REDSTONE_TICK,
        Primitive::Comparator => COMPARATOR_DELAY_GAME_TICKS,
        Primitive::Lever | Primitive::Lamp => 0,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use crate::compile::fragment_synth::identity::{GateIndex, InstanceId, PortId};
    use crate::compile::fragment_synth::instance_graph::{
        DuplicateRequest, InstanceGraph, LogicalSignalId, PhysicalSink,
    };
    use crate::compile::topology::{GateKind, Library};
    use crate::compile::{Gate, Netlist};

    use super::{analyse_instance_dag, EdgeFacts, SeedPlacementError};

    fn nor(output: &str, inputs: &[&str]) -> Gate {
        Gate::nor(output, inputs)
    }

    #[test]
    fn dependency_order_ignores_reversed_gate_declaration_order() {
        let netlist = Netlist {
            inputs: vec!["a".into()],
            outputs: vec!["y".into()],
            gates: vec![nor("y", &["produced_later"]), nor("produced_later", &["a"])],
        };
        let graph = InstanceGraph::one_to_one(&netlist, &Library::default_library()).unwrap();

        let facts = analyse_instance_dag(&graph).unwrap();

        assert_eq!(facts.nodes[&InstanceId(1)].forward_level, 0);
        assert_eq!(facts.nodes[&InstanceId(0)].forward_level, 1);
        assert_eq!(facts.order, [InstanceId(1), InstanceId(0)]);
    }

    #[test]
    fn concrete_instance_fanout_populates_ordered_successors() {
        let netlist = Netlist {
            inputs: vec!["a".into()],
            outputs: vec!["left".into(), "right".into()],
            gates: vec![
                nor("shared", &["a"]),
                nor("right", &["shared"]),
                nor("left", &["shared"]),
            ],
        };
        let graph = InstanceGraph::one_to_one(&netlist, &Library::default_library()).unwrap();

        let facts = analyse_instance_dag(&graph).unwrap();

        assert_eq!(
            facts.nodes[&InstanceId(0)].successors,
            [InstanceId(1), InstanceId(2)]
        );
        assert_eq!(facts.nodes[&InstanceId(1)].predecessors, [InstanceId(0)]);
        assert_eq!(facts.nodes[&InstanceId(2)].predecessors, [InstanceId(0)]);
    }

    #[test]
    fn fanout_levels_and_structural_slack_use_literal_longest_paths() {
        let netlist = Netlist {
            inputs: vec!["a".into()],
            outputs: vec!["y".into()],
            gates: vec![
                nor("long_0", &["a"]),
                nor("long_1", &["long_0"]),
                nor("short", &["a"]),
                nor("y", &["long_1", "short"]),
            ],
        };
        let graph = InstanceGraph::one_to_one(&netlist, &Library::default_library()).unwrap();

        let facts = analyse_instance_dag(&graph).unwrap();

        assert_eq!(
            facts.order,
            [InstanceId(0), InstanceId(1), InstanceId(2), InstanceId(3)]
        );
        assert_eq!(facts.nodes[&InstanceId(0)].predecessors, []);
        assert_eq!(facts.nodes[&InstanceId(0)].successors, [InstanceId(1)]);
        assert_eq!(
            facts.nodes[&InstanceId(3)].predecessors,
            [InstanceId(1), InstanceId(2)]
        );
        assert_eq!(facts.nodes[&InstanceId(3)].successors, []);
        assert_eq!(facts.nodes[&InstanceId(0)].forward_level, 0);
        assert_eq!(facts.nodes[&InstanceId(1)].forward_level, 1);
        assert_eq!(facts.nodes[&InstanceId(2)].forward_level, 0);
        assert_eq!(facts.nodes[&InstanceId(3)].forward_level, 2);
        assert_eq!(facts.nodes[&InstanceId(0)].reverse_level, 2);
        assert_eq!(facts.nodes[&InstanceId(1)].reverse_level, 1);
        assert_eq!(facts.nodes[&InstanceId(2)].reverse_level, 1);
        assert_eq!(facts.nodes[&InstanceId(3)].reverse_level, 0);
        assert_eq!(facts.nodes[&InstanceId(0)].head_ticks, 2);
        assert_eq!(facts.nodes[&InstanceId(1)].head_ticks, 4);
        assert_eq!(facts.nodes[&InstanceId(2)].head_ticks, 2);
        assert_eq!(facts.nodes[&InstanceId(3)].head_ticks, 6);
        assert_eq!(facts.nodes[&InstanceId(0)].tail_ticks, 6);
        assert_eq!(facts.nodes[&InstanceId(1)].tail_ticks, 4);
        assert_eq!(facts.nodes[&InstanceId(2)].tail_ticks, 4);
        assert_eq!(facts.nodes[&InstanceId(3)].tail_ticks, 2);
        assert_eq!(facts.critical_delay_ticks, 6);
        assert_eq!(
            facts.edges,
            [
                EdgeFacts {
                    source: InstanceId(0),
                    sink: InstanceId(1),
                    structural_slack_ticks: 0,
                },
                EdgeFacts {
                    source: InstanceId(1),
                    sink: InstanceId(3),
                    structural_slack_ticks: 0,
                },
                EdgeFacts {
                    source: InstanceId(2),
                    sink: InstanceId(3),
                    structural_slack_ticks: 2,
                },
            ]
        );
    }

    #[test]
    fn selected_topology_primitives_supply_cell_only_delay() {
        let netlist = Netlist {
            inputs: vec!["a".into()],
            outputs: vec!["y".into()],
            gates: vec![Gate {
                name: "y".into(),
                inputs: vec!["a".into()],
                output: "y".into(),
                kind: GateKind::Buf,
            }],
        };
        let graph = InstanceGraph::one_to_one(&netlist, &Library::default_library()).unwrap();

        let facts = analyse_instance_dag(&graph).unwrap();

        assert_eq!(facts.nodes[&InstanceId(0)].head_ticks, 4);
        assert_eq!(facts.nodes[&InstanceId(0)].tail_ticks, 4);
        assert_eq!(facts.critical_delay_ticks, 4);
    }

    #[test]
    fn duplicate_instances_remain_independent_dependency_nodes() {
        let netlist = Netlist {
            inputs: vec!["a".into()],
            outputs: vec!["left".into(), "right".into()],
            gates: vec![
                nor("shared", &["a"]),
                nor("left", &["shared"]),
                nor("right", &["shared"]),
            ],
        };
        let graph = InstanceGraph::with_variants(
            &netlist,
            &Library::default_library(),
            &BTreeMap::new(),
            &[DuplicateRequest {
                canonical: InstanceId(0),
                ordinal: 1,
                sinks: BTreeSet::from([PhysicalSink::InstanceInput {
                    instance: InstanceId(2),
                    input_index: 0,
                }]),
            }],
        )
        .unwrap();

        let facts = analyse_instance_dag(&graph).unwrap();

        assert_eq!(
            facts.order,
            [InstanceId(0), InstanceId(1), InstanceId(3), InstanceId(2)]
        );
        assert_eq!(facts.nodes[&InstanceId(0)].successors, [InstanceId(1)]);
        assert_eq!(facts.nodes[&InstanceId(3)].successors, [InstanceId(2)]);
        assert_eq!(facts.nodes[&InstanceId(0)].forward_level, 0);
        assert_eq!(facts.nodes[&InstanceId(3)].forward_level, 0);
        assert_eq!(facts.nodes[&InstanceId(0)].reverse_level, 1);
        assert_eq!(facts.nodes[&InstanceId(3)].reverse_level, 1);
    }

    #[test]
    fn malformed_instance_dependency_cycle_is_rejected() {
        let netlist = Netlist {
            inputs: vec!["a".into()],
            outputs: vec!["y".into()],
            gates: vec![nor("middle", &["a"]), nor("y", &["middle"])],
        };
        let mut cycle = InstanceGraph::one_to_one(&netlist, &Library::default_library()).unwrap();
        let back_edge_driver = cycle
            .assignments
            .iter()
            .find(|assignment| assignment.sink == PhysicalSink::DeclaredOutput(PortId(0)))
            .unwrap()
            .driver
            .clone();
        let first_input = cycle
            .assignments
            .iter_mut()
            .find(|assignment| {
                assignment.sink
                    == PhysicalSink::InstanceInput {
                        instance: InstanceId(0),
                        input_index: 0,
                    }
            })
            .unwrap();
        first_input.signal = LogicalSignalId::GateOutput(GateIndex(1));
        first_input.driver = back_edge_driver;

        assert!(matches!(
            analyse_instance_dag(&cycle),
            Err(SeedPlacementError::DependencyCycle { .. })
        ));
    }
}
