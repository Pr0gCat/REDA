//! Deterministic semantic revisions for baseline authorities.
//!
//! These descriptors deliberately contain no Git or wall-clock state. A
//! semantic change to the cell library, simulator, or physical verifier must
//! update the corresponding descriptor in the same commit.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::{Map, Value};

use crate::compile::metrics::{canonical_fingerprint, Fingerprint};
use crate::compile::topology::{
    EmbeddingHint, GateKind, Library, Primitive, StatefulPrimitiveRole, TemplateNode,
};
use crate::redstone::simulator::{component, schedule};

#[derive(Serialize)]
struct CellLibraryRevisionDescriptor {
    schema_version: u64,
    entries: Vec<CellLibraryKindDescriptor>,
    stateful_entries: Vec<StatefulLibraryKindDescriptor>,
}

#[derive(Serialize)]
struct CellLibraryKindDescriptor {
    gate_kind: String,
    entries: Vec<CellLibraryEntryDescriptor>,
}

#[derive(Serialize)]
struct CellLibraryEntryDescriptor {
    entry_ordinal: u64,
    nodes: Vec<NodePrimitiveDescriptor>,
    inputs: Vec<String>,
    internal_edges: Vec<EdgeDescriptor>,
    output: Option<String>,
    embedding_hints: Vec<EmbeddingHintDescriptor>,
}

#[derive(Serialize)]
struct NodePrimitiveDescriptor {
    node: String,
    primitive: &'static str,
}

#[derive(Serialize)]
struct EdgeDescriptor {
    from: String,
    to: String,
}

#[derive(Serialize)]
struct EmbeddingHintDescriptor {
    relation: &'static str,
    first: String,
    second: String,
}

#[derive(Serialize)]
struct StatefulLibraryKindDescriptor {
    gate_kind: String,
    nodes: Vec<StatefulNodeDescriptor>,
    signal_edges: Vec<StatefulEdgeDescriptor>,
    repeater_lock_sides: Vec<StatefulEdgeDescriptor>,
    d_landing: &'static str,
    clock_landing: &'static str,
    q_contributor: &'static str,
}

#[derive(Serialize)]
struct StatefulNodeDescriptor {
    role: &'static str,
    primitive: &'static str,
}

#[derive(Serialize)]
struct StatefulEdgeDescriptor {
    from: &'static str,
    to: &'static str,
}

#[derive(Serialize)]
struct SimulatorRevisionDescriptor {
    schema_version: u64,
    supported_component_kinds: Vec<&'static str>,
    unsupported_component_kinds: Vec<&'static str>,
    component_delay_game_ticks: BTreeMap<&'static str, u64>,
    burnout_window_game_ticks: u64,
    burnout_change_limit: u64,
    tick_priority_order: Vec<&'static str>,
    same_priority_order: &'static str,
    propagation_rule_version: &'static str,
    wire_observation_policy: &'static str,
}

#[derive(Serialize)]
struct PhysicalVerifierRevisionDescriptor {
    schema_version: u64,
    rules: Vec<VerifierRuleDescriptor>,
}

#[derive(Serialize)]
struct VerifierRuleDescriptor {
    id: &'static str,
    semantic_version: u64,
}

fn canonicalise_json(value: Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.into_iter().map(canonicalise_json).collect()),
        Value::Object(values) => {
            let mut entries: Vec<_> = values.into_iter().collect();
            entries.sort_by(|(left, _), (right, _)| left.cmp(right));
            let mut canonical = Map::new();
            for (key, value) in entries {
                canonical.insert(key, canonicalise_json(value));
            }
            Value::Object(canonical)
        }
        scalar => scalar,
    }
}

fn descriptor_fingerprint<T: Serialize>(descriptor: &T) -> Fingerprint {
    let value = serde_json::to_value(descriptor).expect("revision descriptors must serialize");
    let canonical = canonicalise_json(value);
    let bytes = serde_json::to_vec(&canonical).expect("canonical JSON values must serialize");
    canonical_fingerprint(&bytes)
}

fn gate_kind_name(kind: GateKind) -> String {
    match kind {
        GateKind::Nor(arity) => format!("nor/{arity}"),
        GateKind::Or(arity) => format!("or/{arity}"),
        GateKind::Buf => "buf".to_string(),
        GateKind::And => "and".to_string(),
        GateKind::Nand => "nand".to_string(),
        GateKind::Xor => "xor".to_string(),
        GateKind::Xnor => "xnor".to_string(),
        GateKind::AndNot => "and-not".to_string(),
        GateKind::OrNot => "or-not".to_string(),
        GateKind::Aoi3 => "aoi3".to_string(),
        GateKind::Oai3 => "oai3".to_string(),
        GateKind::Aoi4 => "aoi4".to_string(),
        GateKind::Oai4 => "oai4".to_string(),
        GateKind::Mux => "mux".to_string(),
        GateKind::Nmux => "nmux".to_string(),
        GateKind::DffPosedge => "dff-posedge".to_string(),
    }
}

fn primitive_name(primitive: Primitive) -> &'static str {
    match primitive {
        Primitive::Torch => "torch",
        Primitive::Repeater => "repeater",
        Primitive::Comparator => "comparator",
        Primitive::Lever => "lever",
        Primitive::Lamp => "lamp",
    }
}

fn template_node_name(node: TemplateNode) -> String {
    match node {
        TemplateNode::Torch => "torch".to_string(),
        TemplateNode::SecondTorch => "second-torch".to_string(),
        TemplateNode::IsolatingRepeater(index) => format!("isolating-repeater/{index}"),
    }
}

fn stateful_role_name(role: StatefulPrimitiveRole) -> &'static str {
    match role {
        StatefulPrimitiveRole::MData => "m-data",
        StatefulPrimitiveRole::SData => "s-data",
        StatefulPrimitiveRole::MLock => "m-lock",
        StatefulPrimitiveRole::InvC => "inv-c",
        StatefulPrimitiveRole::SLock => "s-lock",
    }
}

fn cell_library_descriptor(library: &Library) -> CellLibraryRevisionDescriptor {
    let mut entries: Vec<_> = library
        .revision_entries()
        .map(|(kind, registered)| CellLibraryKindDescriptor {
            gate_kind: gate_kind_name(kind),
            entries: registered
                .iter()
                .enumerate()
                .map(|(ordinal, entry)| CellLibraryEntryDescriptor {
                    entry_ordinal: ordinal as u64,
                    nodes: entry
                        .template
                        .nodes
                        .iter()
                        .map(|&(node, primitive)| NodePrimitiveDescriptor {
                            node: template_node_name(node),
                            primitive: primitive_name(primitive),
                        })
                        .collect(),
                    inputs: entry
                        .template
                        .inputs
                        .iter()
                        .copied()
                        .map(template_node_name)
                        .collect(),
                    internal_edges: entry
                        .template
                        .internal_edges
                        .iter()
                        .map(|&(from, to)| EdgeDescriptor {
                            from: template_node_name(from),
                            to: template_node_name(to),
                        })
                        .collect(),
                    output: entry.template.output.map(template_node_name),
                    embedding_hints: entry
                        .template
                        .embedding_hints
                        .iter()
                        .map(|&hint| match hint {
                            EmbeddingHint::OppositeSides(first, second) => {
                                EmbeddingHintDescriptor {
                                    relation: "opposite-sides",
                                    first: template_node_name(first),
                                    second: template_node_name(second),
                                }
                            }
                            EmbeddingHint::Coplanar(first, second) => EmbeddingHintDescriptor {
                                relation: "coplanar",
                                first: template_node_name(first),
                                second: template_node_name(second),
                            },
                        })
                        .collect(),
                })
                .collect(),
        })
        .collect();
    entries.sort_by(|left, right| left.gate_kind.cmp(&right.gate_kind));

    let mut stateful_entries: Vec<_> = library
        .revision_stateful_entries()
        .map(|(kind, topology)| StatefulLibraryKindDescriptor {
            gate_kind: gate_kind_name(kind),
            nodes: topology
                .nodes
                .iter()
                .map(|&(role, primitive)| StatefulNodeDescriptor {
                    role: stateful_role_name(role),
                    primitive: primitive_name(primitive),
                })
                .collect(),
            signal_edges: topology
                .signal_edges
                .iter()
                .map(|&(from, to)| StatefulEdgeDescriptor {
                    from: stateful_role_name(from),
                    to: stateful_role_name(to),
                })
                .collect(),
            repeater_lock_sides: topology
                .repeater_lock_sides
                .iter()
                .map(|&(from, to)| StatefulEdgeDescriptor {
                    from: stateful_role_name(from),
                    to: stateful_role_name(to),
                })
                .collect(),
            d_landing: stateful_role_name(topology.d_landing),
            clock_landing: stateful_role_name(topology.clock_landing),
            q_contributor: stateful_role_name(topology.q_contributor),
        })
        .collect();
    stateful_entries.sort_by(|left, right| left.gate_kind.cmp(&right.gate_kind));

    CellLibraryRevisionDescriptor {
        schema_version: 1,
        entries,
        stateful_entries,
    }
}

fn simulator_descriptor() -> SimulatorRevisionDescriptor {
    let mut component_delay_game_ticks = BTreeMap::new();
    component_delay_game_ticks.insert("torch", component::TORCH_DELAY_GAME_TICKS);
    component_delay_game_ticks.insert("repeater-per-redstone-tick", 2);
    component_delay_game_ticks.insert("comparator", component::COMPARATOR_DELAY_GAME_TICKS);
    component_delay_game_ticks.insert("lamp-turn-on", component::LAMP_TURN_ON_DELAY_GAME_TICKS);
    component_delay_game_ticks.insert("lamp-turn-off", component::LAMP_TURN_OFF_DELAY_GAME_TICKS);
    component_delay_game_ticks.insert("minimum-schedule", schedule::MIN_SCHEDULE_DELAY_GAME_TICKS);

    SimulatorRevisionDescriptor {
        schema_version: 1,
        supported_component_kinds: vec![
            "air",
            "solid",
            "glass",
            "slab",
            "redstone-wire",
            "repeater",
            "comparator",
            "torch",
            "wall-torch",
            "lever",
            "redstone-block",
            "lamp",
            "other",
        ],
        unsupported_component_kinds: vec![
            "piston",
            "observer",
            "button",
            "pressure-plate",
            "weighted-pressure-plate",
            "target",
            "daylight-detector",
        ],
        component_delay_game_ticks,
        burnout_window_game_ticks: component::BURNOUT_WINDOW_GAME_TICKS,
        burnout_change_limit: component::BURNOUT_CHANGE_LIMIT as u64,
        tick_priority_order: vec!["highest", "higher", "high", "normal"],
        same_priority_order: "stable-insertion-order",
        propagation_rule_version: "directed-dust-component-propagation-v1",
        wire_observation_policy: "redstone-wire-is-high-iff-power-greater-than-zero",
    }
}

fn physical_verifier_descriptor() -> PhysicalVerifierRevisionDescriptor {
    PhysicalVerifierRevisionDescriptor {
        schema_version: 1,
        rules: vec![
            VerifierRuleDescriptor {
                id: "collision",
                semantic_version: 1,
            },
            VerifierRuleDescriptor {
                id: "coupling",
                semantic_version: 1,
            },
            VerifierRuleDescriptor {
                id: "connectivity",
                semantic_version: 1,
            },
            VerifierRuleDescriptor {
                id: "torch-merge-structure",
                semantic_version: 1,
            },
            VerifierRuleDescriptor {
                id: "signal-strength",
                semantic_version: 1,
            },
            VerifierRuleDescriptor {
                id: "repeater-direction",
                semantic_version: 1,
            },
            VerifierRuleDescriptor {
                id: "terminal-style",
                semantic_version: 1,
            },
            VerifierRuleDescriptor {
                id: "pin-handover-halo",
                semantic_version: 1,
            },
        ],
    }
}

/// Semantic revision of one concrete topology library.
pub fn cell_library_revision(library: &Library) -> Fingerprint {
    descriptor_fingerprint(&cell_library_descriptor(library))
}

/// Semantic revision of the simulator's supported components and rules.
pub fn simulator_revision() -> Fingerprint {
    descriptor_fingerprint(&simulator_descriptor())
}

/// Semantic revision of the ordered physical-verification rule set.
pub fn physical_verifier_revision() -> Fingerprint {
    descriptor_fingerprint(&physical_verifier_descriptor())
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};

    use serde::Serialize;

    use super::{
        cell_library_descriptor, cell_library_revision, descriptor_fingerprint,
        physical_verifier_descriptor, physical_verifier_revision, simulator_descriptor,
        simulator_revision,
    };
    use crate::compile::topology::{
        GateKind, Library, LibraryEntry, Primitive, Template, TemplateNode,
    };

    fn one_node_entry(name: &'static str, primitive: Primitive) -> LibraryEntry {
        LibraryEntry {
            name,
            template: Template {
                nodes: vec![(TemplateNode::Torch, primitive)],
                internal_edges: Vec::new(),
                inputs: vec![TemplateNode::Torch],
                output: Some(TemplateNode::Torch),
                embedding_hints: Vec::new(),
            },
        }
    }

    fn two_kind_library(reverse_insertion: bool) -> Library {
        let mut entries = BTreeMap::new();
        let first = (
            GateKind::Nor(1),
            vec![one_node_entry("nor", Primitive::Torch)],
        );
        let second = (
            GateKind::Or(2),
            vec![one_node_entry("or", Primitive::Repeater)],
        );
        if reverse_insertion {
            entries.insert(second.0, second.1);
            entries.insert(first.0, first.1);
        } else {
            entries.insert(first.0, first.1);
            entries.insert(second.0, second.1);
        }
        Library::new(entries)
    }

    #[test]
    fn production_revision_providers_are_repeatable_and_non_empty() {
        let library = Library::default_library();
        let revisions = [
            cell_library_revision(&library),
            simulator_revision(),
            physical_verifier_revision(),
        ];

        assert_eq!(revisions[0], cell_library_revision(&library));
        assert_eq!(revisions[1], simulator_revision());
        assert_eq!(revisions[2], physical_verifier_revision());
        assert!(revisions
            .iter()
            .all(|revision| !revision.as_str().is_empty()));
    }

    #[test]
    fn cell_library_revision_ignores_map_insertion_order() {
        assert_eq!(
            cell_library_revision(&two_kind_library(false)),
            cell_library_revision(&two_kind_library(true))
        );
    }

    #[derive(Serialize)]
    struct MapDescriptor {
        values: HashMap<&'static str, u64>,
    }

    #[test]
    fn descriptor_maps_are_canonical_regardless_of_insertion_order() {
        let mut forward = HashMap::new();
        forward.insert("alpha", 1);
        forward.insert("beta", 2);
        let mut reverse = HashMap::new();
        reverse.insert("beta", 2);
        reverse.insert("alpha", 1);

        assert_eq!(
            descriptor_fingerprint(&MapDescriptor { values: forward }),
            descriptor_fingerprint(&MapDescriptor { values: reverse })
        );
    }

    #[test]
    fn changing_one_cell_library_descriptor_field_changes_its_revision() {
        let mut changed = cell_library_descriptor(&Library::default_library());
        let original = descriptor_fingerprint(&changed);
        changed.schema_version += 1;

        assert_ne!(original, descriptor_fingerprint(&changed));
    }

    #[test]
    fn changing_one_simulator_descriptor_field_changes_its_revision() {
        let mut changed = simulator_descriptor();
        let original = descriptor_fingerprint(&changed);
        changed.propagation_rule_version = "test-only-mutated-rule";

        assert_ne!(original, descriptor_fingerprint(&changed));
    }

    #[test]
    fn changing_one_verifier_descriptor_field_changes_its_revision() {
        let mut changed = physical_verifier_descriptor();
        let original = descriptor_fingerprint(&changed);
        changed.rules[0].semantic_version += 1;

        assert_ne!(original, descriptor_fingerprint(&changed));
    }
}
