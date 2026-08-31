//! Pure expansion of one selected logical-gate implementation.

use std::collections::{BTreeMap, HashMap};

use serde::Serialize;
use thiserror::Error;

use crate::compile::fragment_synth::identity::{
    ConnectionId, GateIndex, ImplementationKey, InputMask, InstanceId, LibraryEntryId, PrimitiveId,
    TopologyNodeId,
};
use crate::compile::metrics::{canonical_fingerprint, Fingerprint};
use crate::compile::topology::{EmbeddingHint, GateKind, Library, Primitive, TemplateNode};
use crate::compile::{Gate, Netlist};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PrimitiveSpec {
    pub id: PrimitiveId,
    pub role: TemplateNode,
    pub primitive: Primitive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ConnectionSource {
    ExternalInput { input_index: u16 },
    Primitive(PrimitiveId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ConnectionTarget {
    Primitive(PrimitiveId),
    Junction(InstanceId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ConnectionSpec {
    pub id: ConnectionId,
    pub source: ConnectionSource,
    pub target: ConnectionTarget,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ContributorSpec {
    Landing(ConnectionId),
    Primitive(PrimitiveId),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum OutputSpec {
    Primitive(PrimitiveId),
    Junction {
        logical_owner: InstanceId,
        contributors: Vec<ContributorSpec>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ValidatedTopology {
    pub fingerprint: Fingerprint,
    pub primitives: Vec<PrimitiveSpec>,
    pub connections: Vec<ConnectionSpec>,
    pub output: OutputSpec,
    pub embedding_hints: Vec<EmbeddingHint>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExpandedInstance {
    pub instance: InstanceId,
    pub implementation: ImplementationKey,
    pub topology: ValidatedTopology,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TopologyError {
    #[error("library entry {entry:?} is not registered")]
    UnknownLibraryEntry { entry: LibraryEntryId },
    #[error("implementation kind {implementation:?} does not match gate kind {gate:?}")]
    ImplementationKindMismatch {
        gate: GateKind,
        implementation: GateKind,
    },
    #[error("gate kind {kind:?} does not accept {arity} inputs")]
    InvalidGateArity { kind: GateKind, arity: usize },
    #[error("merge implementation is required for {kind:?}")]
    MergeImplementationRequired { kind: GateKind },
    #[error("merge isolation mask {bits:#b} has bits outside arity {arity}")]
    IllegalIsolationMask { bits: u64, arity: usize },
    #[error("template role {role:?} is declared more than once")]
    DuplicateTemplateRole { role: TemplateNode },
    #[error("template input {input_index} names missing role {role:?}")]
    UnresolvedTemplateInput {
        input_index: usize,
        role: TemplateNode,
    },
    #[error("template output names missing role {role:?}")]
    UnresolvedTemplateOutput { role: TemplateNode },
    #[error("template edge {edge_index} source names missing role {role:?}")]
    UnresolvedInternalSource {
        edge_index: usize,
        role: TemplateNode,
    },
    #[error("template edge {edge_index} target names missing role {role:?}")]
    UnresolvedInternalTarget {
        edge_index: usize,
        role: TemplateNode,
    },
    #[error("template exposes {template_inputs} inputs for a {gate_inputs}-input gate")]
    TemplateInputCountMismatch {
        template_inputs: usize,
        gate_inputs: usize,
    },
    #[error("stateful topology {kind:?} is not supported by fragment synthesis")]
    UnsupportedStatefulTopology { kind: GateKind },
    #[error("topology has too many {what} to assign a u16 identity")]
    IdentityOverflow { what: &'static str },
    #[error("merge arity {arity} exceeds the 64-bit isolation mask")]
    MergeArityExceedsInputMask { arity: usize },
}

#[derive(Serialize)]
struct FingerprintPayload<'a> {
    primitives: &'a [PrimitiveSpec],
    connections: &'a [ConnectionSpec],
    output: &'a OutputSpec,
    embedding_hints: &'a [EmbeddingHint],
}

fn finish_topology(
    primitives: Vec<PrimitiveSpec>,
    connections: Vec<ConnectionSpec>,
    output: OutputSpec,
    embedding_hints: Vec<EmbeddingHint>,
) -> ValidatedTopology {
    let payload = FingerprintPayload {
        primitives: &primitives,
        connections: &connections,
        output: &output,
        embedding_hints: &embedding_hints,
    };
    let bytes = serde_json::to_vec(&payload).expect("validated topology must serialize");
    ValidatedTopology {
        fingerprint: canonical_fingerprint(&bytes),
        primitives,
        connections,
        output,
        embedding_hints,
    }
}

/// Expand one implementation without placing, routing, emitting, or mutating
/// any compiler state.
pub fn instantiate(
    library: &Library,
    gate: &Gate,
    instance: InstanceId,
    implementation: &ImplementationKey,
) -> Result<ExpandedInstance, TopologyError> {
    if gate.kind.is_sequential() {
        return Err(TopologyError::UnsupportedStatefulTopology { kind: gate.kind });
    }
    if !gate.kind.accepts_arity(gate.inputs.len()) {
        return Err(TopologyError::InvalidGateArity {
            kind: gate.kind,
            arity: gate.inputs.len(),
        });
    }

    let topology = match *implementation {
        ImplementationKey::Library(entry_id) => {
            if entry_id.kind != gate.kind {
                return Err(TopologyError::ImplementationKindMismatch {
                    gate: gate.kind,
                    implementation: entry_id.kind,
                });
            }
            if matches!(gate.kind, GateKind::Or(_)) {
                return Err(TopologyError::MergeImplementationRequired { kind: gate.kind });
            }
            instantiate_library_entry(library, gate, instance, entry_id)?
        }
        ImplementationKey::Merge { isolation_mask } => {
            instantiate_merge(gate, instance, isolation_mask)?
        }
    };

    Ok(ExpandedInstance {
        instance,
        implementation: *implementation,
        topology,
    })
}

fn instantiate_library_entry(
    library: &Library,
    gate: &Gate,
    instance: InstanceId,
    entry_id: LibraryEntryId,
) -> Result<ValidatedTopology, TopologyError> {
    let entry = library
        .entry(entry_id)
        .ok_or(TopologyError::UnknownLibraryEntry { entry: entry_id })?;
    let template = &entry.template;
    if template.inputs.len() != gate.inputs.len() {
        return Err(TopologyError::TemplateInputCountMismatch {
            template_inputs: template.inputs.len(),
            gate_inputs: gate.inputs.len(),
        });
    }

    let mut role_ids = BTreeMap::new();
    let mut primitives = Vec::with_capacity(template.nodes.len());
    for (index, &(role, primitive)) in template.nodes.iter().enumerate() {
        let node = TopologyNodeId(
            u16::try_from(index).map_err(|_| TopologyError::IdentityOverflow { what: "nodes" })?,
        );
        let id = PrimitiveId { instance, node };
        if role_ids.insert(role, id).is_some() {
            return Err(TopologyError::DuplicateTemplateRole { role });
        }
        primitives.push(PrimitiveSpec {
            id,
            role,
            primitive,
        });
    }

    let mut connections = Vec::with_capacity(template.inputs.len() + template.internal_edges.len());
    for (index, &role) in template.inputs.iter().enumerate() {
        let target =
            role_ids
                .get(&role)
                .copied()
                .ok_or(TopologyError::UnresolvedTemplateInput {
                    input_index: index,
                    role,
                })?;
        let input_index =
            u16::try_from(index).map_err(|_| TopologyError::IdentityOverflow { what: "inputs" })?;
        connections.push(ConnectionSpec {
            id: ConnectionId::External {
                instance,
                input_index,
            },
            source: ConnectionSource::ExternalInput { input_index },
            target: ConnectionTarget::Primitive(target),
        });
    }
    for (index, &(from_role, to_role)) in template.internal_edges.iter().enumerate() {
        let source =
            role_ids
                .get(&from_role)
                .copied()
                .ok_or(TopologyError::UnresolvedInternalSource {
                    edge_index: index,
                    role: from_role,
                })?;
        let target =
            role_ids
                .get(&to_role)
                .copied()
                .ok_or(TopologyError::UnresolvedInternalTarget {
                    edge_index: index,
                    role: to_role,
                })?;
        let edge_index = u16::try_from(index).map_err(|_| TopologyError::IdentityOverflow {
            what: "internal edges",
        })?;
        connections.push(ConnectionSpec {
            id: ConnectionId::Internal {
                instance,
                edge_index,
            },
            source: ConnectionSource::Primitive(source),
            target: ConnectionTarget::Primitive(target),
        });
    }

    let output_role = template
        .output
        .ok_or(TopologyError::UnresolvedTemplateOutput {
            role: TemplateNode::Torch,
        })?;
    let output = role_ids
        .get(&output_role)
        .copied()
        .ok_or(TopologyError::UnresolvedTemplateOutput { role: output_role })?;

    Ok(finish_topology(
        primitives,
        connections,
        OutputSpec::Primitive(output),
        template.embedding_hints.clone(),
    ))
}

fn instantiate_merge(
    gate: &Gate,
    instance: InstanceId,
    isolation_mask: InputMask,
) -> Result<ValidatedTopology, TopologyError> {
    let arity = match gate.kind {
        GateKind::Or(arity) if arity == gate.inputs.len() => arity,
        GateKind::Or(_) => {
            return Err(TopologyError::InvalidGateArity {
                kind: gate.kind,
                arity: gate.inputs.len(),
            })
        }
        kind => {
            return Err(TopologyError::ImplementationKindMismatch {
                gate: kind,
                implementation: GateKind::Or(gate.inputs.len()),
            })
        }
    };
    if arity > u64::BITS as usize {
        return Err(TopologyError::MergeArityExceedsInputMask { arity });
    }
    let legal_bits = if arity == u64::BITS as usize {
        u64::MAX
    } else {
        (1u64 << arity) - 1
    };
    if isolation_mask.bits() & !legal_bits != 0 {
        return Err(TopologyError::IllegalIsolationMask {
            bits: isolation_mask.bits(),
            arity,
        });
    }

    let mut primitives = Vec::new();
    let mut connections = Vec::with_capacity(arity);
    let mut contributors = Vec::with_capacity(arity);
    for input_index in 0..arity {
        let input_index_u16 = u16::try_from(input_index)
            .map_err(|_| TopologyError::IdentityOverflow { what: "inputs" })?;
        let connection = ConnectionId::External {
            instance,
            input_index: input_index_u16,
        };
        if isolation_mask.contains(input_index) {
            let node = TopologyNodeId(
                u16::try_from(primitives.len())
                    .map_err(|_| TopologyError::IdentityOverflow { what: "nodes" })?,
            );
            let primitive = PrimitiveId { instance, node };
            primitives.push(PrimitiveSpec {
                id: primitive,
                role: TemplateNode::IsolatingRepeater(input_index),
                primitive: Primitive::Repeater,
            });
            connections.push(ConnectionSpec {
                id: connection,
                source: ConnectionSource::ExternalInput {
                    input_index: input_index_u16,
                },
                target: ConnectionTarget::Primitive(primitive),
            });
            contributors.push(ContributorSpec::Primitive(primitive));
        } else {
            connections.push(ConnectionSpec {
                id: connection,
                source: ConnectionSource::ExternalInput {
                    input_index: input_index_u16,
                },
                target: ConnectionTarget::Junction(instance),
            });
            contributors.push(ContributorSpec::Landing(connection));
        }
    }

    Ok(finish_topology(
        primitives,
        connections,
        OutputSpec::Junction {
            logical_owner: instance,
            contributors,
        },
        Vec::new(),
    ))
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum MergeMaskError {
    #[error("gate index {gate_index:?} does not exist")]
    UnknownGate { gate_index: GateIndex },
    #[error("gate index {gate_index:?} is not a merge")]
    NotMerge { gate_index: GateIndex },
    #[error("merge has too many inputs for an InputMask")]
    TooManyInputs,
}

/// Return the exact per-branch isolation decision used by both generators.
pub fn merge_isolation_mask(
    lowered: &Netlist,
    gate_index: GateIndex,
) -> Result<InputMask, MergeMaskError> {
    let index =
        usize::try_from(gate_index.0).map_err(|_| MergeMaskError::UnknownGate { gate_index })?;
    let gate = lowered
        .gates
        .get(index)
        .ok_or(MergeMaskError::UnknownGate { gate_index })?;
    if !gate.is_merge() {
        return Err(MergeMaskError::NotMerge { gate_index });
    }
    if gate.inputs.len() > u64::BITS as usize {
        return Err(MergeMaskError::TooManyInputs);
    }

    let mut consumers: HashMap<&str, Vec<usize>> = HashMap::new();
    for (consumer, candidate) in lowered.gates.iter().enumerate() {
        for input in &candidate.inputs {
            consumers.entry(input.as_str()).or_default().push(consumer);
        }
    }
    let mut bits = 0u64;
    for (input_index, input) in gate.inputs.iter().enumerate() {
        let is_bare = consumers
            .get(input.as_str())
            .is_some_and(|users| users.iter().all(|&consumer| consumer == index));
        if !is_bare {
            bits |= 1u64 << input_index;
        }
    }
    Ok(InputMask::new(bits))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::compile::fragment_synth::identity::{
        ConnectionId, ImplementationKey, InputMask, InstanceId, LibraryEntryId, PrimitiveId,
        TopologyNodeId,
    };
    use crate::compile::topology::{
        GateKind, Library, LibraryEntry, Primitive, Template, TemplateNode,
    };
    use crate::compile::Gate;

    use super::{instantiate, ConnectionTarget, ContributorSpec, OutputSpec, TopologyError};

    fn gate(kind: GateKind, inputs: &[&str]) -> Gate {
        Gate {
            name: "g".to_string(),
            inputs: inputs.iter().map(|input| (*input).to_string()).collect(),
            output: "y".to_string(),
            kind,
        }
    }

    fn implementation(kind: GateKind, ordinal: u16) -> ImplementationKey {
        ImplementationKey::Library(LibraryEntryId { kind, ordinal })
    }

    #[test]
    fn typed_library_ids_round_trip_and_revision_fingerprint_stays_authoritative() {
        let library = Library::default_library();
        let id = library.entry_id_at(GateKind::Buf, 0).unwrap();

        assert_eq!(
            id,
            LibraryEntryId {
                kind: GateKind::Buf,
                ordinal: 0
            }
        );
        assert_eq!(library.entry(id).unwrap().name, "torch-torch-buf (buf)");
        assert_eq!(
            library.revision_fingerprint(),
            crate::compile::revisions::cell_library_revision(&library)
        );
    }

    #[test]
    fn expands_one_node_nor_with_dense_identity_and_external_connections() {
        let library = Library::default_library();
        let instance = InstanceId(7);
        let expanded = instantiate(
            &library,
            &gate(GateKind::Nor(2), &["a", "b"]),
            instance,
            &implementation(GateKind::Nor(2), 0),
        )
        .unwrap();

        assert_eq!(expanded.topology.primitives.len(), 1);
        assert_eq!(
            expanded.topology.primitives[0].id,
            PrimitiveId {
                instance,
                node: TopologyNodeId(0),
            }
        );
        assert_eq!(expanded.topology.primitives[0].primitive, Primitive::Torch);
        assert_eq!(
            expanded
                .topology
                .connections
                .iter()
                .map(|connection| connection.id)
                .collect::<Vec<_>>(),
            vec![
                ConnectionId::External {
                    instance,
                    input_index: 0
                },
                ConnectionId::External {
                    instance,
                    input_index: 1
                },
            ]
        );
        assert!(expanded.topology.connections.iter().all(|connection| {
            connection.target == ConnectionTarget::Primitive(expanded.topology.primitives[0].id)
        }));
        assert_eq!(
            expanded.topology.output,
            OutputSpec::Primitive(expanded.topology.primitives[0].id)
        );
    }

    #[test]
    fn expands_two_node_buf_with_stable_external_then_internal_order() {
        let library = Library::default_library();
        let instance = InstanceId(3);
        let first = instantiate(
            &library,
            &gate(GateKind::Buf, &["a"]),
            instance,
            &implementation(GateKind::Buf, 0),
        )
        .unwrap();
        let second = instantiate(
            &library,
            &gate(GateKind::Buf, &["a"]),
            instance,
            &implementation(GateKind::Buf, 0),
        )
        .unwrap();

        assert_eq!(
            first
                .topology
                .primitives
                .iter()
                .map(|primitive| primitive.id.node)
                .collect::<Vec<_>>(),
            vec![TopologyNodeId(0), TopologyNodeId(1)]
        );
        assert_eq!(
            first
                .topology
                .connections
                .iter()
                .map(|connection| connection.id)
                .collect::<Vec<_>>(),
            vec![
                ConnectionId::External {
                    instance,
                    input_index: 0
                },
                ConnectionId::Internal {
                    instance,
                    edge_index: 0
                },
            ]
        );
        assert_eq!(
            first.topology.output,
            OutputSpec::Primitive(PrimitiveId {
                instance,
                node: TopologyNodeId(1),
            })
        );
        assert_eq!(first.topology.fingerprint, second.topology.fingerprint);
    }

    #[test]
    fn expands_bare_mixed_and_fully_isolated_merges_without_fake_output_primitive() {
        let library = Library::default_library();
        let instance = InstanceId(11);
        let merge = gate(GateKind::Or(2), &["a", "b"]);

        let bare = instantiate(
            &library,
            &merge,
            instance,
            &ImplementationKey::Merge {
                isolation_mask: InputMask::new(0),
            },
        )
        .unwrap();
        assert!(bare.topology.primitives.is_empty());
        assert_eq!(bare.topology.connections.len(), 2);
        assert_eq!(
            bare.topology.output,
            OutputSpec::Junction {
                logical_owner: instance,
                contributors: vec![
                    ContributorSpec::Landing(ConnectionId::External {
                        instance,
                        input_index: 0,
                    }),
                    ContributorSpec::Landing(ConnectionId::External {
                        instance,
                        input_index: 1,
                    }),
                ],
            }
        );

        let mixed = instantiate(
            &library,
            &merge,
            instance,
            &ImplementationKey::Merge {
                isolation_mask: InputMask::new(0b01),
            },
        )
        .unwrap();
        assert_eq!(mixed.topology.primitives.len(), 1);
        assert_eq!(
            mixed.topology.primitives[0].role,
            TemplateNode::IsolatingRepeater(0)
        );
        assert_eq!(
            mixed.topology.output,
            OutputSpec::Junction {
                logical_owner: instance,
                contributors: vec![
                    ContributorSpec::Primitive(mixed.topology.primitives[0].id),
                    ContributorSpec::Landing(ConnectionId::External {
                        instance,
                        input_index: 1,
                    }),
                ],
            }
        );

        let isolated = instantiate(
            &library,
            &merge,
            instance,
            &ImplementationKey::Merge {
                isolation_mask: InputMask::new(0b11),
            },
        )
        .unwrap();
        assert_eq!(isolated.topology.primitives.len(), 2);
        assert!(matches!(
            &isolated.topology.output,
            OutputSpec::Junction { contributors, .. }
                if contributors.iter().all(|contributor| matches!(contributor, ContributorSpec::Primitive(_)))
        ));
    }

    fn library_with(template: Template) -> Library {
        Library::new(BTreeMap::from([(
            GateKind::Nor(1),
            vec![LibraryEntry {
                name: "test",
                template,
            }],
        )]))
    }

    fn template(nodes: Vec<(TemplateNode, Primitive)>) -> Template {
        Template {
            nodes,
            internal_edges: Vec::new(),
            inputs: vec![TemplateNode::Torch],
            output: Some(TemplateNode::Torch),
            embedding_hints: Vec::new(),
        }
    }

    #[test]
    fn rejects_unknown_entries_illegal_masks_and_stateful_gates_by_name() {
        let library = Library::default_library();
        assert!(matches!(
            instantiate(
                &library,
                &gate(GateKind::Nor(1), &["a"]),
                InstanceId(0),
                &implementation(GateKind::Nor(1), 99),
            ),
            Err(TopologyError::UnknownLibraryEntry { .. })
        ));
        assert!(matches!(
            instantiate(
                &library,
                &gate(GateKind::Or(2), &["a", "b"]),
                InstanceId(0),
                &ImplementationKey::Merge {
                    isolation_mask: InputMask::new(0b100)
                },
            ),
            Err(TopologyError::IllegalIsolationMask { .. })
        ));
        assert!(matches!(
            instantiate(
                &library,
                &gate(GateKind::DffPosedge, &["d", "c"]),
                InstanceId(0),
                &implementation(GateKind::DffPosedge, 0),
            ),
            Err(TopologyError::UnsupportedStatefulTopology { .. })
        ));
    }

    #[test]
    fn a_64_input_merge_uses_the_full_mask_without_shift_overflow() {
        let inputs = (0..64).map(|index| format!("i{index}")).collect::<Vec<_>>();
        let merge = Gate {
            name: "merge64".to_string(),
            inputs,
            output: "out".to_string(),
            kind: GateKind::Or(64),
        };
        let expanded = instantiate(
            &Library::default_library(),
            &merge,
            InstanceId(0),
            &ImplementationKey::Merge {
                isolation_mask: InputMask::new(u64::MAX),
            },
        )
        .unwrap();

        assert_eq!(expanded.topology.primitives.len(), 64);
        assert_eq!(expanded.topology.connections.len(), 64);
    }

    #[test]
    fn a_65_input_merge_is_named_instead_of_shifting_past_the_mask() {
        let inputs = (0..65).map(|index| format!("i{index}")).collect::<Vec<_>>();
        let merge = Gate {
            name: "merge65".to_string(),
            inputs,
            output: "out".to_string(),
            kind: GateKind::Or(65),
        };

        assert_eq!(
            instantiate(
                &Library::default_library(),
                &merge,
                InstanceId(0),
                &ImplementationKey::Merge {
                    isolation_mask: InputMask::new(u64::MAX),
                },
            ),
            Err(TopologyError::MergeArityExceedsInputMask { arity: 65 })
        );
    }

    #[test]
    fn rejects_unresolved_roles_duplicate_roles_and_gate_key_mismatch() {
        let instance = InstanceId(0);
        let gate = gate(GateKind::Nor(1), &["a"]);

        let mut unresolved_input = template(vec![(TemplateNode::SecondTorch, Primitive::Torch)]);
        unresolved_input.output = Some(TemplateNode::SecondTorch);
        assert!(matches!(
            instantiate(
                &library_with(unresolved_input),
                &gate,
                instance,
                &implementation(GateKind::Nor(1), 0),
            ),
            Err(TopologyError::UnresolvedTemplateInput { .. })
        ));

        let mut unresolved_output = template(vec![(TemplateNode::SecondTorch, Primitive::Torch)]);
        unresolved_output.inputs = vec![TemplateNode::SecondTorch];
        assert!(matches!(
            instantiate(
                &library_with(unresolved_output),
                &gate,
                instance,
                &implementation(GateKind::Nor(1), 0),
            ),
            Err(TopologyError::UnresolvedTemplateOutput { .. })
        ));

        let duplicate = template(vec![
            (TemplateNode::Torch, Primitive::Torch),
            (TemplateNode::Torch, Primitive::Repeater),
        ]);
        assert!(matches!(
            instantiate(
                &library_with(duplicate),
                &gate,
                instance,
                &implementation(GateKind::Nor(1), 0),
            ),
            Err(TopologyError::DuplicateTemplateRole { .. })
        ));

        assert!(matches!(
            instantiate(
                &Library::default_library(),
                &gate,
                instance,
                &implementation(GateKind::Buf, 0),
            ),
            Err(TopologyError::ImplementationKindMismatch { .. })
        ));
    }
}
