//! Typed logical-to-physical instance ownership for fragment synthesis.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::Serialize;
use thiserror::Error;

use crate::compile::fragment_synth::identity::{
    GateIndex, ImplementationKey, InstanceId, PortId, PrimitiveId, TopologyNodeId,
};
use crate::compile::fragment_synth::topology::{
    instantiate, merge_isolation_mask, ContributorSpec, ExpandedInstance, OutputSpec, TopologyError,
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
    /// Block instances stamped into this module's graph (Task 8). Empty for
    /// every flat design, and deliberately not serialised when empty so the
    /// on-disk / fingerprinted shape of a flat `InstanceGraph` is unchanged.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<BlockInstance>,
}

/// A compiled block ([`crate::compile::fragment_synth::blocks::CompiledBlock`])
/// stamped once into the parent's instance graph under its own [`InstanceId`].
/// A block reuses existing physical identities rather than adding a new
/// `PhysicalEndpointId` variant: block output `k` is
/// `PhysicalEndpointId::PrimitiveOutput(PrimitiveId { instance: id, node: TopologyNodeId(k) })`,
/// and block input `k` lands at
/// `PhysicalSink::InstanceInput { instance: id, input_index: k }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BlockInstance {
    pub id: InstanceId,
    /// Index into the parent's compiled block list.
    pub block: u32,
    /// Instance path segment(s) inside this module (one name).
    pub path: Vec<String>,
    /// Block input `k` <- parent signal.
    pub inputs: Vec<LogicalSignalId>,
    /// Block output `k` = synthetic gate row in the planning netlist.
    pub output_gates: Vec<GateIndex>,
}

/// Describes one block instantiation for [`InstanceGraph::with_blocks`]: a
/// name, an index into the parent's compiled block list, and the parent
/// signals wired to each input/output in the block's declared port order.
#[derive(Debug, Clone, Copy)]
pub struct BlockSpec<'a> {
    pub name: &'a str,
    pub block: u32,
    /// Parent signals, in block input order.
    pub inputs: &'a [String],
    /// Parent signals, in block output order.
    pub outputs: &'a [String],
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
        let mut instances =
            instantiate_gates(netlist, library, implementations, netlist.gates.len())?;
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
            blocks: Vec::new(),
        };
        graph.validate(netlist)?;
        Ok(graph)
    }

    /// Builds an `InstanceGraph` for a module that stamps one or more
    /// compiled blocks. `planning` is the parent's lowered own gates
    /// followed by one synthetic `GateKind::Buf` gate per block output
    /// (`"<inst>.<port>"`, driven by the block input signal(s), producing
    /// the parent signal of that output) -- synthetic gates exist only so
    /// `signal_table` assigns every block output a `LogicalSignalId::GateOutput`
    /// and are never instantiated as real instances. `blocks` describes each
    /// block instantiation in the same order its synthetic output gates
    /// appear at the tail of `planning.gates`.
    pub(crate) fn with_blocks(
        planning: &Netlist,
        library: &Library,
        blocks: &[BlockSpec<'_>],
    ) -> Result<Self, SynthesisError> {
        let synthetic_outputs: usize = blocks.iter().map(|spec| spec.outputs.len()).sum();
        let real_gates = planning
            .gates
            .len()
            .checked_sub(synthetic_outputs)
            .ok_or(SynthesisError::IdentityOverflow)?;
        let instances = instantiate_gates(planning, library, &BTreeMap::new(), real_gates)?;

        let (signals, primary_inputs) = signal_table(planning)?;

        let base_id = u32::try_from(planning.gates.len()).map_err(|_| SynthesisError::IdentityOverflow)?;
        let mut block_instances = Vec::with_capacity(blocks.len());
        for (k, spec) in blocks.iter().enumerate() {
            let offset = u32::try_from(k).map_err(|_| SynthesisError::IdentityOverflow)?;
            let id = InstanceId(
                base_id
                    .checked_add(offset)
                    .ok_or(SynthesisError::IdentityOverflow)?,
            );
            let inputs = spec
                .inputs
                .iter()
                .map(|name| {
                    signals
                        .get(name.as_str())
                        .copied()
                        .ok_or_else(|| SynthesisError::UndrivenSignal {
                            signal: name.clone(),
                        })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let output_gates = spec
                .outputs
                .iter()
                .map(|name| match signals.get(name.as_str()) {
                    Some(LogicalSignalId::GateOutput(gate)) => Ok(*gate),
                    _ => Err(SynthesisError::UndrivenSignal {
                        signal: name.clone(),
                    }),
                })
                .collect::<Result<Vec<_>, _>>()?;
            block_instances.push(BlockInstance {
                id,
                block: spec.block,
                path: vec![spec.name.to_string()],
                inputs,
                output_gates,
            });
        }
        block_instances.sort_by_key(|block| block.id);

        let mut assignments = Vec::new();
        for instance in &instances {
            let gate = &planning.gates[usize::try_from(instance.logical_gate.0).unwrap()];
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
                    driver: driver_for_signal_with_blocks(signal, &instances, &block_instances)?,
                });
            }
        }

        for block in &block_instances {
            for (input_index, &signal) in block.inputs.iter().enumerate() {
                assignments.push(SinkAssignment {
                    sink: PhysicalSink::InstanceInput {
                        instance: block.id,
                        input_index: u16::try_from(input_index)
                            .map_err(|_| SynthesisError::IdentityOverflow)?,
                    },
                    signal,
                    driver: driver_for_signal_with_blocks(signal, &instances, &block_instances)?,
                });
            }
        }

        let mut declared_outputs = Vec::with_capacity(planning.outputs.len());
        for (index, name) in planning.outputs.iter().enumerate() {
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
                driver: driver_for_signal_with_blocks(signal, &instances, &block_instances)?,
            });
        }

        assignments.sort_by_key(|assignment| assignment.sink);

        let graph = InstanceGraph {
            instances,
            assignments,
            primary_inputs,
            declared_outputs,
            blocks: block_instances,
        };
        graph.validate(planning)?;
        Ok(graph)
    }

    /// Whether `id` names a block instance rather than a gate instance.
    pub fn is_block(&self, id: InstanceId) -> bool {
        self.blocks.iter().any(|block| block.id == id)
    }

    /// The block instance named `id`, if any.
    pub fn block(&self, id: InstanceId) -> Option<&BlockInstance> {
        self.blocks.iter().find(|block| block.id == id)
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

        // Synthetic block-output gates (the tail of `netlist.gates` for a
        // module compiled with `with_blocks`) have no canonical instance of
        // their own -- they are represented by `self.blocks` instead.
        let block_output_gates: BTreeSet<GateIndex> = self
            .blocks
            .iter()
            .flat_map(|block| block.output_gates.iter().copied())
            .collect();

        let mut instance_by_id = BTreeMap::new();
        let mut roles = BTreeSet::new();
        let mut canonical_counts: BTreeMap<GateIndex, usize> = (0..netlist.gates.len())
            .map(gate_index)
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .filter(|gate| !block_output_gates.contains(gate))
            .map(|gate| (gate, 0usize))
            .collect();
        for instance in &self.instances {
            if instance_by_id.insert(instance.id, instance).is_some() {
                return Err(SynthesisError::DuplicateInstanceId {
                    instance: instance.id,
                });
            }
            if usize::try_from(instance.logical_gate.0)
                .ok()
                .filter(|&gate| gate < netlist.gates.len())
                .is_none()
            {
                return Err(SynthesisError::UnknownLogicalGate {
                    instance: instance.id,
                    gate: instance.logical_gate,
                });
            }
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
                if let Some(count) = canonical_counts.get_mut(&instance.logical_gate) {
                    *count += 1;
                }
            }
        }
        for (gate, count) in canonical_counts {
            match count {
                0 => return Err(SynthesisError::MissingCanonicalInstance { gate }),
                1 => {}
                _ => return Err(SynthesisError::DuplicateCanonicalInstance { gate }),
            }
        }
        let mut block_by_id = BTreeMap::new();
        for block in &self.blocks {
            if instance_by_id.contains_key(&block.id) || block_by_id.insert(block.id, block).is_some() {
                return Err(SynthesisError::DuplicateInstanceId { instance: block.id });
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
                validate_driver(assignment, &instance_by_id, &block_by_id)?;
            }
        }
        for block in &self.blocks {
            for (input_index, &signal) in block.inputs.iter().enumerate() {
                let input_index =
                    u16::try_from(input_index).map_err(|_| SynthesisError::IdentityOverflow)?;
                let sink = PhysicalSink::InstanceInput {
                    instance: block.id,
                    input_index,
                };
                expected_sinks.insert(sink);
                let assignment = assignment_by_sink
                    .get(&sink)
                    .ok_or(SynthesisError::MissingAssignment { sink })?;
                if assignment.signal != signal {
                    return Err(SynthesisError::WrongLogicalSignal {
                        sink,
                        expected: signal,
                        actual: assignment.signal,
                    });
                }
                validate_driver(assignment, &instance_by_id, &block_by_id)?;
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
            validate_driver(assignment, &instance_by_id, &block_by_id)?;
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

/// Instantiates `netlist.gates[..upto]` exactly as `with_variants` always
/// has -- `netlist` itself is the full (untruncated) netlist so
/// `merge_isolation_mask` still sees every gate; `upto` only bounds which
/// gates get an `Instance`. `with_variants` calls this with
/// `upto = netlist.gates.len()` (its previous, unchanged behaviour);
/// `with_blocks` calls it with `upto` stopping before the synthetic
/// block-output gates.
fn instantiate_gates(
    netlist: &Netlist,
    library: &Library,
    implementations: &BTreeMap<InstanceId, ImplementationKey>,
    upto: usize,
) -> Result<Vec<Instance>, SynthesisError> {
    let mut instances = Vec::with_capacity(upto);
    for (index, gate) in netlist.gates.iter().take(upto).enumerate() {
        let gate_index = gate_index(index)?;
        if gate.kind.is_sequential() {
            return Err(SynthesisError::UnsupportedStatefulTopology { gate: gate_index });
        }
        let instance = InstanceId(gate_index.0);
        let default_implementation = if matches!(gate.kind, GateKind::Or(_)) {
            ImplementationKey::Merge {
                isolation_mask: merge_isolation_mask(netlist, gate_index)
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
    Ok(instances)
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

/// The driver for block output `k`: a single-terminal primitive driver
/// naming the block's own instance and topology node `k`, so its endpoint
/// resolves (via `endpoint_for_driver`) to
/// `PhysicalEndpointId::PrimitiveOutput(PrimitiveId { instance: block.id, node: TopologyNodeId(k) })`.
fn block_output_driver(block: &BlockInstance, node: u16) -> InstanceDriver {
    InstanceDriver::Primitive {
        logical_owner: block.id,
        terminals: vec![PrimitiveId {
            instance: block.id,
            node: TopologyNodeId(node),
        }],
    }
}

/// Like [`driver_for_signal`], but a `GateOutput` naming one of `blocks`'
/// synthetic output gates resolves to that block's own driver instead of
/// requiring a canonical `Instance` (blocks have neither).
fn driver_for_signal_with_blocks(
    signal: LogicalSignalId,
    instances: &[Instance],
    blocks: &[BlockInstance],
) -> Result<PhysicalDriver, SynthesisError> {
    if let LogicalSignalId::GateOutput(gate) = signal {
        if let Some((block, position)) = blocks.iter().find_map(|block| {
            block
                .output_gates
                .iter()
                .position(|&owned| owned == gate)
                .map(|position| (block, position))
        }) {
            let node = u16::try_from(position).map_err(|_| SynthesisError::IdentityOverflow)?;
            return Ok(PhysicalDriver::Instance(block_output_driver(block, node)));
        }
    }
    driver_for_signal(signal, instances)
}

/// Reads the single terminal's node out of a driver that claims to be owned
/// by `owner`, or `None` if it is not of the canonical single-terminal
/// primitive shape a block output driver always has.
fn block_driver_node(driver: &InstanceDriver, owner: InstanceId) -> Option<usize> {
    match driver {
        InstanceDriver::Primitive { terminals, .. } => match terminals.as_slice() {
            [PrimitiveId { instance, node }] if *instance == owner => Some(node.0 as usize),
            _ => None,
        },
        InstanceDriver::Junction { .. } => None,
    }
}

fn validate_driver(
    assignment: &SinkAssignment,
    instance_by_id: &BTreeMap<InstanceId, &Instance>,
    block_by_id: &BTreeMap<InstanceId, &BlockInstance>,
) -> Result<(), SynthesisError> {
    let actual = match &assignment.driver {
        PhysicalDriver::PrimaryInput(port) => LogicalSignalId::PrimaryInput(*port),
        PhysicalDriver::Instance(driver) => {
            let owner = driver.logical_owner();
            if let Some(instance) = instance_by_id.get(&owner) {
                LogicalSignalId::GateOutput(instance.logical_gate)
            } else if let Some(block) = block_by_id.get(&owner) {
                let node = block_driver_node(driver, owner).ok_or(
                    SynthesisError::WrongPhysicalDriver {
                        sink: assignment.sink,
                    },
                )?;
                let gate = block.output_gates.get(node).copied().ok_or(
                    SynthesisError::WrongPhysicalDriver {
                        sink: assignment.sink,
                    },
                )?;
                LogicalSignalId::GateOutput(gate)
            } else {
                return Err(SynthesisError::UnknownDriverInstance { instance: owner });
            }
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
            if let Some(instance) = instance_by_id.get(&owner) {
                PhysicalDriver::Instance(instance_driver(instance))
            } else {
                // `owner` was already resolved to a known block above, or
                // this function would have returned `UnknownDriverInstance`.
                let block = block_by_id
                    .get(&owner)
                    .expect("owner resolved against instance_by_id or block_by_id above");
                let gate = match assignment.signal {
                    LogicalSignalId::GateOutput(gate) => gate,
                    LogicalSignalId::PrimaryInput(_) => {
                        return Err(SynthesisError::WrongPhysicalDriver {
                            sink: assignment.sink,
                        })
                    }
                };
                let node = block
                    .output_gates
                    .iter()
                    .position(|&owned| owned == gate)
                    .and_then(|position| u16::try_from(position).ok())
                    .ok_or(SynthesisError::WrongPhysicalDriver {
                        sink: assignment.sink,
                    })?;
                PhysicalDriver::Instance(block_output_driver(block, node))
            }
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
pub(crate) mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use crate::compile::fragment_synth::candidate::endpoint_for_driver;
    use crate::compile::fragment_synth::identity::{
        GateIndex, ImplementationKey, InputMask, InstanceId, PhysicalEndpointId, PortId,
        PrimitiveId, TopologyNodeId,
    };
    use crate::compile::topology::{GateKind, Library};
    use crate::compile::{Gate, Netlist};

    use super::{
        BlockSpec, InstanceDriver, InstanceGraph, InstanceRole, LogicalSignalId, PhysicalDriver,
        PhysicalSink, SynthesisError,
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

    /// top: x -> [block u0: inputs a; outputs y, w] ; y -> nor g0 -> z ; w declared output.
    /// Shared with the placement tests (Task 9).
    pub(crate) fn planning_with_one_block() -> (Netlist, Vec<(String, u32, Vec<String>, Vec<String>)>)
    {
        let planning = Netlist {
            inputs: vec!["x".into()],
            outputs: vec!["z".into(), "w".into()],
            gates: vec![
                Gate {
                    name: "g0".into(),
                    inputs: vec!["y".into()],
                    output: "z".into(),
                    kind: GateKind::Nor(1),
                },
                Gate {
                    name: "u0.y".into(),
                    inputs: vec!["x".into()],
                    output: "y".into(),
                    kind: GateKind::Buf,
                },
                Gate {
                    name: "u0.w".into(),
                    inputs: vec!["x".into()],
                    output: "w".into(),
                    kind: GateKind::Buf,
                },
            ],
        };
        (
            planning,
            vec![(
                "u0".into(),
                0,
                vec!["x".into()],
                vec!["y".into(), "w".into()],
            )],
        )
    }

    pub(crate) fn specs_of(
        owned: &[(String, u32, Vec<String>, Vec<String>)],
    ) -> Vec<BlockSpec<'_>> {
        owned
            .iter()
            .map(|(name, block, inputs, outputs)| BlockSpec {
                name,
                block: *block,
                inputs,
                outputs,
            })
            .collect()
    }

    #[test]
    fn a_block_joins_the_graph_with_one_driver_per_output_and_one_sink_per_input() {
        let (planning, owned) = planning_with_one_block();
        let library = Library::default_library();
        let graph = InstanceGraph::with_blocks(&planning, &library, &specs_of(&owned)).expect("builds");
        assert_eq!(graph.instances.len(), 1, "only the real gate is instantiated");
        assert_eq!(graph.blocks.len(), 1);
        let block = &graph.blocks[0];
        assert_eq!(block.id, InstanceId(3));
        assert_eq!(block.inputs, vec![LogicalSignalId::PrimaryInput(PortId(0))]);
        assert_eq!(block.output_gates, vec![GateIndex(1), GateIndex(2)]);
        let to_block = graph
            .assignments
            .iter()
            .find(|a| {
                a.sink
                    == PhysicalSink::InstanceInput {
                        instance: block.id,
                        input_index: 0,
                    }
            })
            .unwrap();
        assert_eq!(to_block.driver, PhysicalDriver::PrimaryInput(PortId(0)));
        let from_block = graph
            .assignments
            .iter()
            .find(|a| {
                a.sink
                    == PhysicalSink::InstanceInput {
                        instance: InstanceId(0),
                        input_index: 0,
                    }
            })
            .unwrap();
        assert_eq!(
            endpoint_for_driver(&from_block.driver),
            Some(PhysicalEndpointId::PrimitiveOutput(PrimitiveId {
                instance: block.id,
                node: TopologyNodeId(0)
            }))
        );
        let w = graph
            .assignments
            .iter()
            .find(|a| a.sink == PhysicalSink::DeclaredOutput(PortId(1)))
            .unwrap();
        assert_eq!(
            endpoint_for_driver(&w.driver),
            Some(PhysicalEndpointId::PrimitiveOutput(PrimitiveId {
                instance: block.id,
                node: TopologyNodeId(1)
            }))
        );
        assert!(graph.is_block(block.id));
    }

    #[test]
    fn a_graph_without_blocks_serialises_exactly_as_before() {
        let netlist = crate::circuits::and4::build_and4_netlist().0; // any existing fixture
        let lowered = crate::compile::lowering::lower_optimised(&netlist).unwrap();
        let graph = InstanceGraph::one_to_one(&lowered, &Library::default_library()).unwrap();
        let json = serde_json::to_string(&graph).unwrap();
        assert!(!json.contains("\"blocks\""));
    }

    /// top: x0, x1 -> [block u0: inputs a, b; outputs p, q, r] ; p, q, r all declared outputs.
    /// Proves every input and every output of a multi-port block is wired, not just the first.
    fn planning_with_wide_block() -> (Netlist, Vec<(String, u32, Vec<String>, Vec<String>)>) {
        let planning = Netlist {
            inputs: vec!["x0".into(), "x1".into()],
            outputs: vec!["p".into(), "q".into(), "r".into()],
            gates: vec![
                Gate {
                    name: "u0.p".into(),
                    inputs: vec!["x0".into()],
                    output: "p".into(),
                    kind: GateKind::Buf,
                },
                Gate {
                    name: "u0.q".into(),
                    inputs: vec!["x1".into()],
                    output: "q".into(),
                    kind: GateKind::Buf,
                },
                Gate {
                    name: "u0.r".into(),
                    inputs: vec!["x0".into()],
                    output: "r".into(),
                    kind: GateKind::Buf,
                },
            ],
        };
        (
            planning,
            vec![(
                "u0".into(),
                0,
                vec!["x0".into(), "x1".into()],
                vec!["p".into(), "q".into(), "r".into()],
            )],
        )
    }

    #[test]
    fn a_wide_block_wires_every_input_and_every_output() {
        let (planning, owned) = planning_with_wide_block();
        let library = Library::default_library();
        let graph = InstanceGraph::with_blocks(&planning, &library, &specs_of(&owned)).expect("builds");
        assert_eq!(graph.instances.len(), 0, "no real gates besides the block");
        assert_eq!(graph.blocks.len(), 1);
        let block = &graph.blocks[0];
        assert_eq!(
            block.inputs,
            vec![
                LogicalSignalId::PrimaryInput(PortId(0)),
                LogicalSignalId::PrimaryInput(PortId(1)),
            ]
        );
        assert_eq!(
            block.output_gates,
            vec![GateIndex(0), GateIndex(1), GateIndex(2)]
        );
        for input_index in 0..2u16 {
            let sink = PhysicalSink::InstanceInput {
                instance: block.id,
                input_index,
            };
            let assignment = graph
                .assignments
                .iter()
                .find(|a| a.sink == sink)
                .unwrap_or_else(|| panic!("missing sink for block input {input_index}"));
            assert_eq!(
                assignment.driver,
                PhysicalDriver::PrimaryInput(PortId(input_index as u32))
            );
        }
        for (k, name) in ["p", "q", "r"].iter().enumerate() {
            let index = planning.outputs.iter().position(|o| o == name).unwrap();
            let sink = PhysicalSink::DeclaredOutput(PortId(index as u32));
            let assignment = graph
                .assignments
                .iter()
                .find(|a| a.sink == sink)
                .unwrap_or_else(|| panic!("missing declared output {name}"));
            assert_eq!(
                endpoint_for_driver(&assignment.driver),
                Some(PhysicalEndpointId::PrimitiveOutput(PrimitiveId {
                    instance: block.id,
                    node: TopologyNodeId(k as u16),
                }))
            );
        }
    }

    #[test]
    fn validate_accepts_a_graph_containing_blocks() {
        let (planning, owned) = planning_with_one_block();
        let library = Library::default_library();
        let graph = InstanceGraph::with_blocks(&planning, &library, &specs_of(&owned)).expect("builds");
        assert_eq!(graph.validate(&planning), Ok(()));
    }

    #[test]
    fn a_graph_with_blocks_keeps_assignments_sorted_by_sink() {
        let (planning, owned) = planning_with_wide_block();
        let library = Library::default_library();
        let graph = InstanceGraph::with_blocks(&planning, &library, &specs_of(&owned)).expect("builds");
        assert!(!graph.blocks.is_empty());
        assert!(graph
            .assignments
            .windows(2)
            .all(|pair| pair[0].sink < pair[1].sink));
    }
}
