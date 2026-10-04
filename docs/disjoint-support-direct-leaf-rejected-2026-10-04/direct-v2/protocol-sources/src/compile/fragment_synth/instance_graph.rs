//! Typed logical-to-physical instance ownership for fragment synthesis.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::Serialize;
use thiserror::Error;

use crate::compile::fragment_synth::identity::{
    GateIndex, ImplementationKey, InstanceId, PortId, PrimitiveId,
};
use crate::compile::fragment_synth::topology::{
    instantiate, merge_isolation_mask, seed_merge_isolation_mask, ContributorSpec, ExpandedInstance, OutputSpec, TopologyError,
};
use crate::compile::topology::{GateKind, Library};
use crate::compile::Netlist;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub enum InstanceRole {
    Canonical,
    Duplicate { ordinal: u16 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub enum LogicalSignalId {
    PrimaryInput(PortId),
    GateOutput(GateIndex),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Instance {
    pub id: InstanceId,
    pub logical_gate: GateIndex,
    pub role: InstanceRole,
    pub implementation: ImplementationKey,
    pub expanded: ExpandedInstance,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum InstanceDriver {
    Primitive {
        logical_owner: InstanceId,
        terminals: Vec<PrimitiveId>,
    },
    Junction {
        logical_owner: InstanceId,
        contributors: Vec<ContributorSpec>,
    },
}

impl InstanceDriver {
    fn logical_owner(&self) -> InstanceId {
        match self {
            InstanceDriver::Primitive { logical_owner, .. }
            | InstanceDriver::Junction { logical_owner, .. } => *logical_owner,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum PhysicalDriver {
    PrimaryInput(PortId),
    Instance(InstanceDriver),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub enum PhysicalSink {
    InstanceInput {
        instance: InstanceId,
        input_index: u16,
    },
    DeclaredOutput(PortId),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SinkAssignment {
    pub sink: PhysicalSink,
    pub signal: LogicalSignalId,
    pub driver: PhysicalDriver,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InstanceGraph {
    pub instances: Vec<Instance>,
    pub assignments: Vec<SinkAssignment>,
    pub primary_inputs: Vec<PortId>,
    pub declared_outputs: Vec<PortId>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub(crate) struct DuplicateRequest {
    pub canonical: InstanceId,
    pub ordinal: u16,
    pub sinks: BTreeSet<PhysicalSink>,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SynthesisError {
    #[error("stateful gate {gate:?} is not supported")]
    UnsupportedStatefulTopology { gate: GateIndex },
    #[error("gate {gate:?} has no registered implementation")]
    NoLibraryEntry { gate: GateIndex },
    #[error("implementation override names unknown instance {instance:?}")]
    UnknownImplementationOverride { instance: InstanceId },
    #[error("duplicate request names missing canonical instance {canonical:?}")]
    MissingDuplicateCanonical { canonical: InstanceId },
    #[error("duplicate request repeats logical gate {gate:?} ordinal {ordinal}")]
    RepeatedDuplicateRequest { gate: GateIndex, ordinal: u16 },
    #[error("canonical instance {canonical:?} has no duplicable concrete output primitive")]
    UnsupportedDuplicateTopology { canonical: InstanceId },
    #[error("sink {sink:?} does not carry canonical {canonical:?}'s output signal")]
    DuplicateSinkSignalMismatch {
        canonical: InstanceId,
        sink: PhysicalSink,
    },
    #[error("gate or port count exceeds typed identity width")]
    IdentityOverflow,
    #[error("signal `{signal}` has no primary-input or gate driver")]
    UndrivenSignal { signal: String },
    #[error("signal `{signal}` has more than one gate driver")]
    DuplicateSignalDriver { signal: String },
    #[error("topology expansion failed for gate {gate:?}: {source}")]
    Topology {
        gate: GateIndex,
        #[source]
        source: TopologyError,
    },
    #[error("missing assignment for {sink:?}")]
    MissingAssignment { sink: PhysicalSink },
    #[error("duplicate assignment for {sink:?}")]
    DuplicateAssignment { sink: PhysicalSink },
    #[error("unexpected assignment for {sink:?}")]
    UnexpectedAssignment { sink: PhysicalSink },
    #[error("assignment for {sink:?} carries {actual:?}, expected {expected:?}")]
    WrongLogicalSignal {
        sink: PhysicalSink,
        expected: LogicalSignalId,
        actual: LogicalSignalId,
    },
    #[error(
        "duplicate {instance:?} input {input_index} carries {actual:?}, expected {expected:?}"
    )]
    DuplicateInputMismatch {
        instance: InstanceId,
        input_index: u16,
        expected: LogicalSignalId,
        actual: LogicalSignalId,
    },
    #[error("driver for {sink:?} carries {actual:?}, assignment claims {expected:?}")]
    DriverSignalMismatch {
        sink: PhysicalSink,
        expected: LogicalSignalId,
        actual: LogicalSignalId,
    },
    #[error("driver for {sink:?} does not match the selected instance topology")]
    WrongPhysicalDriver { sink: PhysicalSink },
    #[error("instance identity {instance:?} appears more than once")]
    DuplicateInstanceId { instance: InstanceId },
    #[error("instance {instance:?} does not agree with its expanded topology identity")]
    ExpandedInstanceMismatch { instance: InstanceId },
    #[error("logical gate {gate:?} uses role {role:?} more than once")]
    DuplicateInstanceRole { gate: GateIndex, role: InstanceRole },
    #[error("instance {instance:?} names missing logical gate {gate:?}")]
    UnknownLogicalGate {
        instance: InstanceId,
        gate: GateIndex,
    },
    #[error("logical gate {gate:?} has no canonical instance")]
    MissingCanonicalInstance { gate: GateIndex },
    #[error("logical gate {gate:?} has more than one canonical instance")]
    DuplicateCanonicalInstance { gate: GateIndex },
    #[error("driver names missing instance {instance:?}")]
    UnknownDriverInstance { instance: InstanceId },
    #[error("instances are not in canonical logical-gate and role order")]
    NonCanonicalInstanceOrder,
    #[error("sink assignments are not in canonical sink order")]
    NonCanonicalAssignmentOrder,
}

impl InstanceGraph {
    pub fn one_to_one(netlist: &Netlist, library: &Library) -> Result<Self, SynthesisError> {
        Self::one_to_one_with_implementations(netlist, library, &BTreeMap::new())
    }

    /// [`one_to_one`](Self::one_to_one) with every merge isolated the way
    /// the legacy emitter isolates it ([`merge_isolation_mask`]), for
    /// adapting a circuit that emitter built. The seed generator's own graph
    /// also isolates a merge input that is a declared output
    /// ([`seed_merge_isolation_mask`]); the legacy one does not.
    pub(crate) fn one_to_one_legacy(netlist: &Netlist, library: &Library) -> Result<Self, SynthesisError> {
        let mut implementations = BTreeMap::new();
        for (index, gate) in netlist.gates.iter().enumerate() {
            if matches!(gate.kind, GateKind::Or(_)) {
                let gate_index = gate_index(index)?;
                let isolation_mask = merge_isolation_mask(netlist, gate_index)
                    .map_err(|_| SynthesisError::NoLibraryEntry { gate: gate_index })?;
                implementations.insert(
                    InstanceId(gate_index.0),
                    ImplementationKey::Merge { isolation_mask },
                );
            }
        }
        Self::one_to_one_with_implementations(netlist, library, &implementations)
    }

    pub(crate) fn one_to_one_with_implementations(
        netlist: &Netlist,
        library: &Library,
        implementations: &BTreeMap<InstanceId, ImplementationKey>,
    ) -> Result<Self, SynthesisError> {
        Self::with_variants(netlist, library, implementations, &[])
    }

    pub(crate) fn with_variants(
        netlist: &Netlist,
        library: &Library,
        implementations: &BTreeMap<InstanceId, ImplementationKey>,
        duplicates: &[DuplicateRequest],
    ) -> Result<Self, SynthesisError> {
        let mut instances = Vec::with_capacity(netlist.gates.len());
        for (index, gate) in netlist.gates.iter().enumerate() {
            let gate_index = gate_index(index)?;
            if gate.kind.is_sequential() {
                return Err(SynthesisError::UnsupportedStatefulTopology { gate: gate_index });
            }
            let instance = InstanceId(gate_index.0);
            let default_implementation = if matches!(gate.kind, GateKind::Or(_)) {
                ImplementationKey::Merge {
                    isolation_mask: seed_merge_isolation_mask(netlist, gate_index)
                        .map_err(|_| SynthesisError::NoLibraryEntry { gate: gate_index })?,
                }
            } else {
                ImplementationKey::Library(
                    library
                        .entry_id_at(gate.kind, 0)
                        .ok_or(SynthesisError::NoLibraryEntry { gate: gate_index })?,
                )
            };
            let implementation = implementations
                .get(&instance)
                .copied()
                .unwrap_or(default_implementation);
            let expanded =
                instantiate(library, gate, instance, &implementation).map_err(|source| {
                    SynthesisError::Topology {
                        gate: gate_index,
                        source,
                    }
                })?;
            instances.push(Instance {
                id: instance,
                logical_gate: gate_index,
                role: InstanceRole::Canonical,
                implementation,
                expanded,
            });
        }
        if let Some(&instance) = implementations
            .keys()
            .find(|instance| !instances.iter().any(|item| item.id == **instance))
        {
            return Err(SynthesisError::UnknownImplementationOverride { instance });
        }

        let mut ordered_duplicates = duplicates.to_vec();
        ordered_duplicates.sort();
        let mut duplicate_ids = BTreeMap::new();
        for request in &ordered_duplicates {
            let canonical = instances
                .iter()
                .find(|instance| {
                    instance.id == request.canonical && instance.role == InstanceRole::Canonical
                })
                .cloned()
                .ok_or(SynthesisError::MissingDuplicateCanonical {
                    canonical: request.canonical,
                })?;
            if !matches!(canonical.expanded.topology.output, OutputSpec::Primitive(_)) {
                return Err(SynthesisError::UnsupportedDuplicateTopology {
                    canonical: request.canonical,
                });
            }
            let role = InstanceRole::Duplicate {
                ordinal: request.ordinal,
            };
            if duplicate_ids.contains_key(&(canonical.logical_gate, request.ordinal)) {
                return Err(SynthesisError::RepeatedDuplicateRequest {
                    gate: canonical.logical_gate,
                    ordinal: request.ordinal,
                });
            }
            let id = InstanceId(
                u32::try_from(netlist.gates.len())
                    .map_err(|_| SynthesisError::IdentityOverflow)?
                    .checked_add(
                        u32::try_from(duplicate_ids.len())
                            .map_err(|_| SynthesisError::IdentityOverflow)?,
                    )
                    .ok_or(SynthesisError::IdentityOverflow)?,
            );
            let gate = &netlist.gates[usize::try_from(canonical.logical_gate.0).unwrap()];
            let expanded =
                instantiate(library, gate, id, &canonical.implementation).map_err(|source| {
                    SynthesisError::Topology {
                        gate: canonical.logical_gate,
                        source,
                    }
                })?;
            duplicate_ids.insert((canonical.logical_gate, request.ordinal), id);
            instances.push(Instance {
                id,
                logical_gate: canonical.logical_gate,
                role,
                implementation: canonical.implementation,
                expanded,
            });
        }
        instances.sort_by_key(|instance| (instance.logical_gate, instance.role, instance.id));

        let (signals, primary_inputs) = signal_table(netlist)?;
        let mut assignments = Vec::new();
        for instance in &instances {
            let gate = &netlist.gates[usize::try_from(instance.logical_gate.0).unwrap()];
            for (input_index, name) in gate.inputs.iter().enumerate() {
                let signal =
                    *signals
                        .get(name.as_str())
                        .ok_or_else(|| SynthesisError::UndrivenSignal {
                            signal: name.clone(),
                        })?;
                assignments.push(SinkAssignment {
                    sink: PhysicalSink::InstanceInput {
                        instance: instance.id,
                        input_index: u16::try_from(input_index)
                            .map_err(|_| SynthesisError::IdentityOverflow)?,
                    },
                    signal,
                    driver: driver_for_signal(signal, &instances)?,
                });
            }
        }

        let mut declared_outputs = Vec::with_capacity(netlist.outputs.len());
        for (index, name) in netlist.outputs.iter().enumerate() {
            let port = PortId(u32::try_from(index).map_err(|_| SynthesisError::IdentityOverflow)?);
            let signal =
                *signals
                    .get(name.as_str())
                    .ok_or_else(|| SynthesisError::UndrivenSignal {
                        signal: name.clone(),
                    })?;
            declared_outputs.push(port);
            assignments.push(SinkAssignment {
                sink: PhysicalSink::DeclaredOutput(port),
                signal,
                driver: driver_for_signal(signal, &instances)?,
            });
        }

        for request in &ordered_duplicates {
            let canonical = instances
                .iter()
                .find(|instance| instance.id == request.canonical)
                .ok_or(SynthesisError::MissingDuplicateCanonical {
                    canonical: request.canonical,
                })?;
            let duplicate_id = duplicate_ids[&(canonical.logical_gate, request.ordinal)];
            let duplicate = instances
                .iter()
                .find(|instance| instance.id == duplicate_id)
                .expect("derived duplicate identity must exist");
            let expected_signal = LogicalSignalId::GateOutput(canonical.logical_gate);
            for &sink in &request.sinks {
                let assignment = assignments
                    .iter_mut()
                    .find(|assignment| assignment.sink == sink)
                    .ok_or(SynthesisError::DuplicateSinkSignalMismatch {
                        canonical: request.canonical,
                        sink,
                    })?;
                if assignment.signal != expected_signal {
                    return Err(SynthesisError::DuplicateSinkSignalMismatch {
                        canonical: request.canonical,
                        sink,
                    });
                }
                assignment.driver = PhysicalDriver::Instance(instance_driver(duplicate));
            }
        }
        assignments.sort_by_key(|assignment| assignment.sink);

        let graph = InstanceGraph {
            instances,
            assignments,
            primary_inputs,
            declared_outputs,
        };
        graph.validate(netlist)?;
        Ok(graph)
    }

    pub fn validate(&self, netlist: &Netlist) -> Result<(), SynthesisError> {
        for (index, gate) in netlist.gates.iter().enumerate() {
            if gate.kind.is_sequential() {
                return Err(SynthesisError::UnsupportedStatefulTopology {
                    gate: gate_index(index)?,
                });
            }
        }
        let (signals, expected_inputs) = signal_table(netlist)?;
        if self.primary_inputs != expected_inputs {
            return Err(SynthesisError::IdentityOverflow);
        }
        let expected_outputs = (0..netlist.outputs.len())
            .map(|index| {
                u32::try_from(index)
                    .map(PortId)
                    .map_err(|_| SynthesisError::IdentityOverflow)
            })
            .collect::<Result<Vec<_>, _>>()?;
        if self.declared_outputs != expected_outputs {
            return Err(SynthesisError::IdentityOverflow);
        }

        let mut instance_by_id = BTreeMap::new();
        let mut roles = BTreeSet::new();
        let mut canonical_counts = vec![0usize; netlist.gates.len()];
        for instance in &self.instances {
            if instance_by_id.insert(instance.id, instance).is_some() {
                return Err(SynthesisError::DuplicateInstanceId {
                    instance: instance.id,
                });
            }
            let logical = usize::try_from(instance.logical_gate.0)
                .ok()
                .filter(|&gate| gate < netlist.gates.len())
                .ok_or(SynthesisError::UnknownLogicalGate {
                    instance: instance.id,
                    gate: instance.logical_gate,
                })?;
            if instance.expanded.instance != instance.id
                || instance.expanded.implementation != instance.implementation
            {
                return Err(SynthesisError::ExpandedInstanceMismatch {
                    instance: instance.id,
                });
            }
            if !roles.insert((instance.logical_gate, instance.role)) {
                return Err(SynthesisError::DuplicateInstanceRole {
                    gate: instance.logical_gate,
                    role: instance.role,
                });
            }
            if instance.role == InstanceRole::Canonical {
                canonical_counts[logical] += 1;
            }
        }
        for (index, count) in canonical_counts.into_iter().enumerate() {
            let gate = gate_index(index)?;
            match count {
                0 => return Err(SynthesisError::MissingCanonicalInstance { gate }),
                1 => {}
                _ => return Err(SynthesisError::DuplicateCanonicalInstance { gate }),
            }
        }
        let mut assignment_by_sink = BTreeMap::new();
        for assignment in &self.assignments {
            if assignment_by_sink
                .insert(assignment.sink, assignment)
                .is_some()
            {
                return Err(SynthesisError::DuplicateAssignment {
                    sink: assignment.sink,
                });
            }
        }
        let mut expected_sinks = BTreeSet::new();
        for instance in &self.instances {
            let gate = &netlist.gates[usize::try_from(instance.logical_gate.0).unwrap()];
            for (input_index, name) in gate.inputs.iter().enumerate() {
                let input_index =
                    u16::try_from(input_index).map_err(|_| SynthesisError::IdentityOverflow)?;
                let sink = PhysicalSink::InstanceInput {
                    instance: instance.id,
                    input_index,
                };
                expected_sinks.insert(sink);
                let expected =
                    *signals
                        .get(name.as_str())
                        .ok_or_else(|| SynthesisError::UndrivenSignal {
                            signal: name.clone(),
                        })?;
                let assignment = assignment_by_sink
                    .get(&sink)
                    .ok_or(SynthesisError::MissingAssignment { sink })?;
                if assignment.signal != expected {
                    return if matches!(instance.role, InstanceRole::Duplicate { .. }) {
                        Err(SynthesisError::DuplicateInputMismatch {
                            instance: instance.id,
                            input_index,
                            expected,
                            actual: assignment.signal,
                        })
                    } else {
                        Err(SynthesisError::WrongLogicalSignal {
                            sink,
                            expected,
                            actual: assignment.signal,
                        })
                    };
                }
                validate_driver(assignment, &instance_by_id)?;
            }
        }
        for (index, name) in netlist.outputs.iter().enumerate() {
            let sink = PhysicalSink::DeclaredOutput(PortId(
                u32::try_from(index).map_err(|_| SynthesisError::IdentityOverflow)?,
            ));
            expected_sinks.insert(sink);
            let expected =
                *signals
                    .get(name.as_str())
                    .ok_or_else(|| SynthesisError::UndrivenSignal {
                        signal: name.clone(),
                    })?;
            let assignment = assignment_by_sink
                .get(&sink)
                .ok_or(SynthesisError::MissingAssignment { sink })?;
            if assignment.signal != expected {
                return Err(SynthesisError::WrongLogicalSignal {
                    sink,
                    expected,
                    actual: assignment.signal,
                });
            }
            validate_driver(assignment, &instance_by_id)?;
        }
        if let Some((&sink, _)) = assignment_by_sink
            .iter()
            .find(|(sink, _)| !expected_sinks.contains(sink))
        {
            return Err(SynthesisError::UnexpectedAssignment { sink });
        }
        if self.instances.windows(2).any(|pair| {
            (pair[0].logical_gate, pair[0].role, pair[0].id)
                >= (pair[1].logical_gate, pair[1].role, pair[1].id)
        }) {
            return Err(SynthesisError::NonCanonicalInstanceOrder);
        }
        if self
            .assignments
            .windows(2)
            .any(|pair| pair[0].sink >= pair[1].sink)
        {
            return Err(SynthesisError::NonCanonicalAssignmentOrder);
        }
        Ok(())
    }
}

fn gate_index(index: usize) -> Result<GateIndex, SynthesisError> {
    u32::try_from(index)
        .map(GateIndex)
        .map_err(|_| SynthesisError::IdentityOverflow)
}

fn signal_table(
    netlist: &Netlist,
) -> Result<(HashMap<&str, LogicalSignalId>, Vec<PortId>), SynthesisError> {
    let mut signals = HashMap::new();
    let mut primary_inputs = Vec::with_capacity(netlist.inputs.len());
    for (index, name) in netlist.inputs.iter().enumerate() {
        let port = PortId(u32::try_from(index).map_err(|_| SynthesisError::IdentityOverflow)?);
        if signals
            .insert(name.as_str(), LogicalSignalId::PrimaryInput(port))
            .is_some()
        {
            return Err(SynthesisError::DuplicateSignalDriver {
                signal: name.clone(),
            });
        }
        primary_inputs.push(port);
    }
    for (index, gate) in netlist.gates.iter().enumerate() {
        let signal = LogicalSignalId::GateOutput(gate_index(index)?);
        if signals.insert(gate.output.as_str(), signal).is_some() {
            return Err(SynthesisError::DuplicateSignalDriver {
                signal: gate.output.clone(),
            });
        }
    }
    Ok((signals, primary_inputs))
}

fn instance_driver(instance: &Instance) -> InstanceDriver {
    match &instance.expanded.topology.output {
        OutputSpec::Primitive(primitive) => InstanceDriver::Primitive {
            logical_owner: instance.id,
            terminals: vec![*primitive],
        },
        OutputSpec::Junction { contributors, .. } => InstanceDriver::Junction {
            logical_owner: instance.id,
            contributors: contributors.clone(),
        },
    }
}

fn driver_for_signal(
    signal: LogicalSignalId,
    instances: &[Instance],
) -> Result<PhysicalDriver, SynthesisError> {
    match signal {
        LogicalSignalId::PrimaryInput(port) => Ok(PhysicalDriver::PrimaryInput(port)),
        LogicalSignalId::GateOutput(gate) => {
            let instance = instances
                .iter()
                .find(|instance| {
                    instance.logical_gate == gate && instance.role == InstanceRole::Canonical
                })
                .ok_or(SynthesisError::MissingCanonicalInstance { gate })?;
            Ok(PhysicalDriver::Instance(instance_driver(instance)))
        }
    }
}

fn validate_driver(
    assignment: &SinkAssignment,
    instance_by_id: &BTreeMap<InstanceId, &Instance>,
) -> Result<(), SynthesisError> {
    let actual = match &assignment.driver {
        PhysicalDriver::PrimaryInput(port) => LogicalSignalId::PrimaryInput(*port),
        PhysicalDriver::Instance(driver) => {
            let owner = driver.logical_owner();
            let instance = instance_by_id
                .get(&owner)
                .ok_or(SynthesisError::UnknownDriverInstance { instance: owner })?;
            LogicalSignalId::GateOutput(instance.logical_gate)
        }
    };
    if actual != assignment.signal {
        return Err(SynthesisError::DriverSignalMismatch {
            sink: assignment.sink,
            expected: assignment.signal,
            actual,
        });
    }
    let expected = match &assignment.driver {
        PhysicalDriver::PrimaryInput(port) => PhysicalDriver::PrimaryInput(*port),
        PhysicalDriver::Instance(driver) => {
            let owner = driver.logical_owner();
            let instance = instance_by_id
                .get(&owner)
                .ok_or(SynthesisError::UnknownDriverInstance { instance: owner })?;
            PhysicalDriver::Instance(instance_driver(instance))
        }
    };
    if assignment.driver != expected {
        return Err(SynthesisError::WrongPhysicalDriver {
            sink: assignment.sink,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use crate::compile::fragment_synth::identity::{
        GateIndex, ImplementationKey, InputMask, InstanceId, PortId,
    };
    use crate::compile::topology::{GateKind, Library};
    use crate::compile::{Gate, Netlist};

    use super::{
        InstanceDriver, InstanceGraph, InstanceRole, LogicalSignalId, PhysicalDriver, PhysicalSink,
        SynthesisError,
    };

    fn nor(output: &str, inputs: &[&str]) -> Gate {
        Gate::nor(output, inputs)
    }

    fn fanout_netlist() -> Netlist {
        Netlist {
            inputs: vec!["a".to_string(), "b".to_string()],
            outputs: vec!["y".to_string(), "z".to_string()],
            gates: vec![
                nor("shared", &["a"]),
                nor("y", &["shared", "b"]),
                nor("z", &["shared"]),
            ],
        }
    }

    #[test]
    fn one_to_one_graph_has_dense_canonical_instances_and_every_explicit_assignment() {
        let netlist = fanout_netlist();
        let graph = InstanceGraph::one_to_one(&netlist, &Library::default_library()).unwrap();

        assert_eq!(
            graph
                .instances
                .iter()
                .map(|instance| (instance.id, instance.logical_gate, instance.role))
                .collect::<Vec<_>>(),
            vec![
                (InstanceId(0), GateIndex(0), InstanceRole::Canonical),
                (InstanceId(1), GateIndex(1), InstanceRole::Canonical),
                (InstanceId(2), GateIndex(2), InstanceRole::Canonical),
            ]
        );
        assert_eq!(graph.primary_inputs, vec![PortId(0), PortId(1)]);
        assert_eq!(graph.declared_outputs, vec![PortId(0), PortId(1)]);

        assert_eq!(graph.assignments.len(), 6);
        assert_eq!(
            graph.assignments[0].sink,
            PhysicalSink::InstanceInput {
                instance: InstanceId(0),
                input_index: 0,
            }
        );
        assert_eq!(
            graph.assignments[0].driver,
            PhysicalDriver::PrimaryInput(PortId(0))
        );
        assert_eq!(
            graph.assignments[1].signal,
            LogicalSignalId::GateOutput(GateIndex(0))
        );
        assert_eq!(
            graph.assignments[4].sink,
            PhysicalSink::DeclaredOutput(PortId(0))
        );
        assert_eq!(
            graph.assignments[5].sink,
            PhysicalSink::DeclaredOutput(PortId(1))
        );
        graph.validate(&netlist).unwrap();
    }

    #[test]
    fn an_explicit_implementation_override_rebuilds_the_selected_instance_before_assignments() {
        let netlist = Netlist {
            inputs: vec!["a".into(), "b".into()],
            outputs: vec!["y".into()],
            gates: vec![Gate::merge("y", &["a", "b"])],
        };
        let implementation = ImplementationKey::Merge {
            isolation_mask: InputMask::new(0b11),
        };
        let graph = InstanceGraph::one_to_one_with_implementations(
            &netlist,
            &Library::default_library(),
            &BTreeMap::from([(InstanceId(0), implementation)]),
        )
        .unwrap();

        assert_eq!(graph.instances[0].implementation, implementation);
        assert_eq!(graph.instances[0].expanded.topology.primitives.len(), 2);
        assert_eq!(graph.assignments.len(), 3);
        graph.validate(&netlist).unwrap();
    }

    #[test]
    fn a_duplicate_request_derives_fresh_ids_and_partitions_sinks_deterministically() {
        let netlist = Netlist {
            inputs: vec!["a".into(), "b".into()],
            outputs: vec!["left".into(), "right".into()],
            gates: vec![
                Gate::nor("shared", &["a", "b"]),
                Gate::nor("left", &["shared"]),
                Gate::nor("right", &["shared"]),
            ],
        };
        let partition = BTreeSet::from([PhysicalSink::InstanceInput {
            instance: InstanceId(2),
            input_index: 0,
        }]);
        let graph = InstanceGraph::with_variants(
            &netlist,
            &Library::default_library(),
            &BTreeMap::new(),
            &[super::DuplicateRequest {
                canonical: InstanceId(0),
                ordinal: 1,
                sinks: partition.clone(),
            }],
        )
        .unwrap();
        let duplicate = graph
            .instances
            .iter()
            .find(|instance| instance.role == super::InstanceRole::Duplicate { ordinal: 1 })
            .unwrap();
        let assignment = graph
            .assignments
            .iter()
            .find(|assignment| partition.contains(&assignment.sink))
            .unwrap();
        let super::PhysicalDriver::Instance(super::InstanceDriver::Primitive {
            logical_owner,
            terminals,
        }) = &assignment.driver
        else {
            panic!("partitioned sink must use the duplicate primitive")
        };

        assert_eq!(duplicate.id, InstanceId(3));
        assert_eq!(duplicate.logical_gate, GateIndex(0));
        assert_eq!(duplicate.expanded.instance, duplicate.id);
        assert!(duplicate
            .expanded
            .topology
            .connections
            .iter()
            .all(|connection| {
                matches!(
                    connection.id,
                    crate::compile::fragment_synth::identity::ConnectionId::External {
                        instance,
                        ..
                    } | crate::compile::fragment_synth::identity::ConnectionId::Internal {
                        instance,
                        ..
                    } if instance == duplicate.id
                )
            }));
        assert_eq!(*logical_owner, duplicate.id);
        let crate::compile::fragment_synth::topology::OutputSpec::Primitive(output) =
            duplicate.expanded.topology.output
        else {
            panic!("duplicate must retain one concrete output primitive")
        };
        assert_eq!(terminals, &vec![output]);
        graph.validate(&netlist).unwrap();
    }

    #[test]
    fn duplication_rejects_junction_outputs_and_non_owned_sink_partitions() {
        let merge = Netlist {
            inputs: vec!["a".into(), "b".into()],
            outputs: vec!["y".into()],
            gates: vec![Gate::merge("y", &["a", "b"])],
        };
        let request = super::DuplicateRequest {
            canonical: InstanceId(0),
            ordinal: 1,
            sinks: BTreeSet::from([PhysicalSink::DeclaredOutput(PortId(0))]),
        };
        assert!(matches!(
            InstanceGraph::with_variants(
                &merge,
                &Library::default_library(),
                &BTreeMap::new(),
                &[request]
            ),
            Err(SynthesisError::UnsupportedDuplicateTopology {
                canonical: InstanceId(0)
            })
        ));

        let netlist = fanout_netlist();
        let wrong_partition = super::DuplicateRequest {
            canonical: InstanceId(1),
            ordinal: 1,
            sinks: BTreeSet::from([PhysicalSink::DeclaredOutput(PortId(1))]),
        };
        assert!(matches!(
            InstanceGraph::with_variants(
                &netlist,
                &Library::default_library(),
                &BTreeMap::new(),
                &[wrong_partition]
            ),
            Err(SynthesisError::DuplicateSinkSignalMismatch { .. })
        ));
    }

    #[test]
    fn graph_validation_rejects_missing_duplicate_and_wrong_signal_assignments() {
        let netlist = fanout_netlist();
        let valid = InstanceGraph::one_to_one(&netlist, &Library::default_library()).unwrap();

        let mut missing = valid.clone();
        let removed = missing.assignments.remove(0);
        assert_eq!(
            missing.validate(&netlist),
            Err(SynthesisError::MissingAssignment { sink: removed.sink })
        );

        let mut duplicate = valid.clone();
        let repeated = duplicate.assignments[0].clone();
        duplicate.assignments.push(repeated.clone());
        assert_eq!(
            duplicate.validate(&netlist),
            Err(SynthesisError::DuplicateAssignment {
                sink: repeated.sink
            })
        );

        let mut wrong = valid.clone();
        wrong.assignments[0].signal = LogicalSignalId::PrimaryInput(PortId(1));
        assert!(matches!(
            wrong.validate(&netlist),
            Err(SynthesisError::WrongLogicalSignal { .. })
        ));
    }

    #[test]
    fn a_duplicate_cannot_read_different_logical_inputs_from_its_canonical_gate() {
        let netlist = fanout_netlist();
        let library = Library::default_library();
        let mut graph = InstanceGraph::one_to_one(&netlist, &library).unwrap();
        let mut duplicate = graph.instances[1].clone();
        duplicate.id = InstanceId(3);
        duplicate.role = InstanceRole::Duplicate { ordinal: 0 };
        duplicate.expanded = crate::compile::fragment_synth::topology::instantiate(
            &library,
            &netlist.gates[1],
            duplicate.id,
            &duplicate.implementation,
        )
        .unwrap();
        graph.instances.push(duplicate);
        graph.assignments.push(super::SinkAssignment {
            sink: PhysicalSink::InstanceInput {
                instance: InstanceId(3),
                input_index: 0,
            },
            signal: LogicalSignalId::PrimaryInput(PortId(0)),
            driver: PhysicalDriver::PrimaryInput(PortId(0)),
        });
        graph.assignments.push(super::SinkAssignment {
            sink: PhysicalSink::InstanceInput {
                instance: InstanceId(3),
                input_index: 1,
            },
            signal: LogicalSignalId::PrimaryInput(PortId(1)),
            driver: PhysicalDriver::PrimaryInput(PortId(1)),
        });

        assert_eq!(
            graph.validate(&netlist),
            Err(SynthesisError::DuplicateInputMismatch {
                instance: InstanceId(3),
                input_index: 0,
                expected: LogicalSignalId::GateOutput(GateIndex(0)),
                actual: LogicalSignalId::PrimaryInput(PortId(0)),
            })
        );
    }

    #[test]
    fn one_to_one_graph_rejects_stateful_topology_by_gate_identity() {
        let netlist = Netlist {
            inputs: vec!["d".to_string(), "c".to_string()],
            outputs: vec!["q".to_string()],
            gates: vec![Gate {
                name: "q".to_string(),
                inputs: vec!["d".to_string(), "c".to_string()],
                output: "q".to_string(),
                kind: GateKind::DffPosedge,
            }],
        };

        assert_eq!(
            InstanceGraph::one_to_one(&netlist, &Library::default_library()).unwrap_err(),
            SynthesisError::UnsupportedStatefulTopology { gate: GateIndex(0) },
        );
    }

    #[test]
    fn validation_rejects_a_merge_driver_with_altered_contributors() {
        let netlist = Netlist {
            inputs: vec!["a".to_string(), "b".to_string()],
            outputs: vec!["m".to_string()],
            gates: vec![Gate::merge("m", &["a", "b"])],
        };
        let mut graph = InstanceGraph::one_to_one(&netlist, &Library::default_library()).unwrap();
        let assignment = graph.assignments.last_mut().unwrap();
        let PhysicalDriver::Instance(InstanceDriver::Junction { contributors, .. }) =
            &mut assignment.driver
        else {
            panic!("merge output must be a junction driver")
        };
        contributors.pop();

        assert_eq!(
            graph.validate(&netlist),
            Err(SynthesisError::WrongPhysicalDriver {
                sink: PhysicalSink::DeclaredOutput(PortId(0)),
            })
        );
    }

    #[test]
    fn one_to_one_rejects_duplicate_primary_input_names() {
        let netlist = Netlist {
            inputs: vec!["a".to_string(), "a".to_string()],
            outputs: vec!["y".to_string()],
            gates: vec![nor("y", &["a"])],
        };

        assert_eq!(
            InstanceGraph::one_to_one(&netlist, &Library::default_library()).unwrap_err(),
            SynthesisError::DuplicateSignalDriver {
                signal: "a".to_string(),
            }
        );
    }

    #[test]
    fn validation_rejects_noncanonical_order_and_stateful_netlists() {
        let netlist = fanout_netlist();
        let graph = InstanceGraph::one_to_one(&netlist, &Library::default_library()).unwrap();

        let mut reordered_instances = graph.clone();
        reordered_instances.instances.reverse();
        assert!(matches!(
            reordered_instances.validate(&netlist),
            Err(SynthesisError::NonCanonicalInstanceOrder)
        ));

        let mut reordered_assignments = graph.clone();
        reordered_assignments.assignments.reverse();
        assert!(matches!(
            reordered_assignments.validate(&netlist),
            Err(SynthesisError::NonCanonicalAssignmentOrder)
        ));

        let mut stateful = netlist;
        stateful.gates[0].kind = GateKind::DffPosedge;
        assert_eq!(
            graph.validate(&stateful),
            Err(SynthesisError::UnsupportedStatefulTopology { gate: GateIndex(0) })
        );
    }
}
