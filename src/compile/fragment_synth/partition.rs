//! Minimal deterministic logical partitioner.
//!
//! Orders a combinational [`Netlist`] canonically (independent of gate
//! declaration order), slices that order into bounded chunks, and gives each
//! chunk a [`ChunkId`] derived only from the parent identity, the chunk's
//! interface, and its logical membership.  No physical placement or routing
//! decisions live here.
//!
//! Operand order is semantically significant: `Gate::inputs` is copied and
//! fingerprinted in declared order (a `MUX`'s `S` is not its `A`), so two
//! gates that differ only in operand order have different identities.

// Crate-private until the public synthesis API unfreezes at Gate 3; nothing
// outside the tests calls it yet.
#![cfg_attr(not(test), allow(dead_code))]

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde::Serialize;
use thiserror::Error;

use crate::compile::metrics::{canonical_fingerprint, Fingerprint};
use crate::compile::topology::GateKind;
use crate::compile::{Gate, Netlist};

const DESCRIPTOR_SCHEMA_VERSION: u32 = 1;

/// Stable identity of one chunk: parent identity, interface, and membership.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChunkId(Fingerprint);

/// One chunk: its cloned sub-netlist and canonical (sorted) boundary lists.
///
/// A boundary input is consumed inside but driven outside (or primary), or is
/// an unused root input carried down the first-child path to preserve the root
/// interface. A boundary output is produced inside and consumed outside or
/// declared as a root output. `netlist.inputs`/`netlist.outputs` mirror these
/// lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub id: ChunkId,
    pub netlist: Netlist,
    pub boundary_inputs: Vec<String>,
    pub boundary_outputs: Vec<String>,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum PartitionError {
    #[error("max gates per chunk must be at least one")]
    ZeroChunkSize,
    #[error("signal {signal} has more than one driver")]
    DuplicateDriver { signal: String },
    #[error("gate {name} is sequential; the partitioner accepts only combinational gates")]
    SequentialGate { name: String },
    #[error("gate {gate} reads {signal}, which is neither a primary input nor a gate output")]
    UndrivenSignal { gate: String, signal: String },
    #[error("declared output {signal} is not driven by any gate")]
    UndrivenOutput { signal: String },
    #[error("combinational cycle through {signals:?}")]
    Cycle { signals: Vec<String> },
}

/// Marker for a primary input in `producer_of`.
const PRIMARY: usize = usize::MAX;

/// Canonical topological order of `netlist.gates` (indices into the original
/// vector), after validating drivers, declared ports, and acyclicity.  Ties
/// are broken by output signal name, so the result does not depend on
/// declaration order.
pub fn canonical_order(netlist: &Netlist) -> Result<Vec<usize>, PartitionError> {
    let producer_of = producers(netlist)?;

    let gate_count = netlist.gates.len();
    let mut in_degree = vec![0usize; gate_count];
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); gate_count];
    for (index, gate) in netlist.gates.iter().enumerate() {
        for input in &gate.inputs {
            match producer_of.get(input.as_str()) {
                Some(&PRIMARY) => {}
                Some(&producer) => {
                    dependents[producer].push(index);
                    in_degree[index] += 1;
                }
                None => {
                    return Err(PartitionError::UndrivenSignal {
                        gate: gate.name.clone(),
                        signal: input.clone(),
                    })
                }
            }
        }
    }
    for output in &netlist.outputs {
        match producer_of.get(output.as_str()) {
            Some(&producer) if producer != PRIMARY => {}
            _ => {
                return Err(PartitionError::UndrivenOutput {
                    signal: output.clone(),
                })
            }
        }
    }
    let mut ready: BTreeMap<&str, usize> = netlist
        .gates
        .iter()
        .enumerate()
        .filter(|(index, _)| in_degree[*index] == 0)
        .map(|(index, gate)| (gate.output.as_str(), index))
        .collect();
    let mut order = Vec::with_capacity(gate_count);
    while let Some((_, index)) = ready.pop_first() {
        order.push(index);
        for &dependent in &dependents[index] {
            in_degree[dependent] -= 1;
            if in_degree[dependent] == 0 {
                ready.insert(netlist.gates[dependent].output.as_str(), dependent);
            }
        }
    }

    if order.len() == gate_count {
        Ok(order)
    } else {
        let mut signals: Vec<String> = (0..gate_count)
            .filter(|&index| in_degree[index] > 0)
            .map(|index| netlist.gates[index].output.clone())
            .collect();
        signals.sort();
        Err(PartitionError::Cycle { signals })
    }
}

/// Signal name -> gate index (or [`PRIMARY`]); rejects duplicate drivers and
/// sequential gates.
fn producers(netlist: &Netlist) -> Result<HashMap<&str, usize>, PartitionError> {
    let mut producer_of: HashMap<&str, usize> = HashMap::new();
    for name in &netlist.inputs {
        if producer_of.insert(name, PRIMARY).is_some() {
            return Err(PartitionError::DuplicateDriver {
                signal: name.clone(),
            });
        }
    }
    for (index, gate) in netlist.gates.iter().enumerate() {
        if gate.kind.is_sequential() {
            return Err(PartitionError::SequentialGate {
                name: gate.name.clone(),
            });
        }
        if producer_of.insert(&gate.output, index).is_some() {
            return Err(PartitionError::DuplicateDriver {
                signal: gate.output.clone(),
            });
        }
    }
    Ok(producer_of)
}

/// Identity of a whole netlist as the root of a partition tree: its exact
/// declared interface plus its gates as a set, independent of declaration
/// order.  Validates the netlist first.
pub fn root_chunk_id(netlist: &Netlist) -> Result<ChunkId, PartitionError> {
    canonical_order(netlist)?;
    Ok(chunk_id(
        None,
        &netlist.inputs,
        &netlist.outputs,
        &netlist.gates,
    ))
}

/// Split `netlist` into chunks of at most `max_gates` gates each, in
/// canonical order.  `parent` is the identity of the enclosing root/parent
/// chunk and is folded into every child [`ChunkId`].
///
/// A declared primary input no gate reads is kept, deterministically, on the
/// first chunk's boundary inputs, so re-partitioning that chunk keeps it
/// again.  A netlist with no gates has no chunk to carry anything and
/// explicitly partitions to zero chunks.
pub fn partition(
    netlist: &Netlist,
    parent: &ChunkId,
    max_gates: usize,
) -> Result<Vec<Chunk>, PartitionError> {
    if max_gates == 0 {
        return Err(PartitionError::ZeroChunkSize);
    }
    let order = canonical_order(netlist)?;
    if order.is_empty() {
        return Ok(Vec::new());
    }
    let producer_of = producers(netlist)?;

    let chunk_count = order.len().div_ceil(max_gates);
    let mut chunk_of = vec![0usize; netlist.gates.len()];
    for (chunk, members) in order.chunks(max_gates).enumerate() {
        for &gate in members {
            chunk_of[gate] = chunk;
        }
    }

    // One pass over every edge: an edge whose producer lives in another chunk
    // (or is primary) is a boundary input of the consumer's chunk and, when
    // gate-driven, a boundary output of the producer's chunk.
    let mut inputs: Vec<BTreeSet<&str>> = vec![BTreeSet::new(); chunk_count];
    let mut outputs: Vec<BTreeSet<&str>> = vec![BTreeSet::new(); chunk_count];
    let mut used_primary: HashSet<&str> = HashSet::new();
    for (gate, consumer) in netlist.gates.iter().enumerate() {
        for signal in &consumer.inputs {
            match producer_of[signal.as_str()] {
                PRIMARY => {
                    inputs[chunk_of[gate]].insert(signal);
                    used_primary.insert(signal);
                }
                producer if chunk_of[producer] != chunk_of[gate] => {
                    inputs[chunk_of[gate]].insert(signal);
                    outputs[chunk_of[producer]].insert(signal);
                }
                _ => {}
            }
        }
    }
    for signal in netlist
        .inputs
        .iter()
        .filter(|s| !used_primary.contains(s.as_str()))
    {
        inputs[0].insert(signal);
    }
    // Invariant: canonical_order already proved every declared output is
    // gate-driven, so this indexing cannot hit PRIMARY or miss.
    for signal in &netlist.outputs {
        outputs[chunk_of[producer_of[signal.as_str()]]].insert(signal);
    }

    Ok(order
        .chunks(max_gates)
        .zip(inputs)
        .zip(outputs)
        .map(|((members, inputs), outputs)| {
            let gates: Vec<Gate> = members.iter().map(|&i| netlist.gates[i].clone()).collect();
            let boundary_inputs: Vec<String> = inputs.into_iter().map(str::to_owned).collect();
            let boundary_outputs: Vec<String> = outputs.into_iter().map(str::to_owned).collect();
            Chunk {
                id: chunk_id(Some(parent), &boundary_inputs, &boundary_outputs, &gates),
                netlist: Netlist {
                    inputs: boundary_inputs.clone(),
                    outputs: boundary_outputs.clone(),
                    gates,
                },
                boundary_inputs,
                boundary_outputs,
            }
        })
        .collect())
}

#[derive(Serialize)]
struct GateDescriptor<'a> {
    name: &'a str,
    output: &'a str,
    kind: GateKind,
    /// Declared operand order; significant.
    inputs: &'a [String],
}

#[derive(Serialize)]
struct ChunkDescriptor<'a> {
    schema_version: u32,
    parent: Option<&'a str>,
    /// Exact ordered interface: declared ports for a root, canonical
    /// boundary lists for a child.
    inputs: &'a [String],
    outputs: &'a [String],
    /// Membership as a set: sorted by output signal.
    members: Vec<GateDescriptor<'a>>,
}

fn chunk_id(
    parent: Option<&ChunkId>,
    inputs: &[String],
    outputs: &[String],
    gates: &[Gate],
) -> ChunkId {
    let mut members: Vec<GateDescriptor<'_>> = gates
        .iter()
        .map(|gate| GateDescriptor {
            name: &gate.name,
            output: &gate.output,
            kind: gate.kind,
            inputs: &gate.inputs,
        })
        .collect();
    members.sort_by(|a, b| a.output.cmp(b.output));
    let descriptor = ChunkDescriptor {
        schema_version: DESCRIPTOR_SCHEMA_VERSION,
        parent: parent.map(|id| id.0.as_str()),
        inputs,
        outputs,
        members,
    };
    ChunkId(canonical_fingerprint(
        &serde_json::to_vec(&descriptor).expect("chunk descriptor must serialize"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parent() -> ChunkId {
        ChunkId(canonical_fingerprint(b"partition-test-root"))
    }

    fn netlist(inputs: &[&str], outputs: &[&str], gates: Vec<Gate>) -> Netlist {
        Netlist {
            inputs: inputs.iter().map(|s| s.to_string()).collect(),
            outputs: outputs.iter().map(|s| s.to_string()).collect(),
            gates,
        }
    }

    fn diamond() -> Vec<Gate> {
        vec![
            Gate::nor("a", &["x"]),
            Gate::nor("b", &["y"]),
            Gate::nor("c", &["a", "b"]),
            Gate::nor("d", &["z"]),
        ]
    }

    #[test]
    fn shuffled_declarations_yield_identical_chunks() {
        let ordered = netlist(&["x", "y", "z"], &["c", "d"], diamond());
        let mut shuffled_gates = diamond();
        shuffled_gates.reverse();
        shuffled_gates.swap(0, 2);
        let shuffled = netlist(&["x", "y", "z"], &["c", "d"], shuffled_gates);

        let lhs_root = root_chunk_id(&ordered).unwrap();
        let rhs_root = root_chunk_id(&shuffled).unwrap();
        assert_eq!(lhs_root, rhs_root);
        let lhs = partition(&ordered, &lhs_root, 2).unwrap();
        let rhs = partition(&shuffled, &rhs_root, 2).unwrap();
        assert_eq!(lhs, rhs);
        assert_eq!(lhs.len(), 2);
        assert_ne!(lhs[0].id, lhs[1].id);
        assert_eq!(lhs[0].boundary_inputs, ["x", "y"]);
        assert_eq!(lhs[0].boundary_outputs, ["a", "b"]);
    }

    #[test]
    fn interface_and_gate_name_change_identity() {
        let base = netlist(&["x", "y", "z"], &["c", "d"], diamond());
        let more_outputs = netlist(&["x", "y", "z"], &["c", "d", "a"], diamond());
        let mut renamed_gates = diamond();
        renamed_gates[0].name = "a_renamed".into();
        let renamed = netlist(&["x", "y", "z"], &["c", "d"], renamed_gates);

        let base_id = root_chunk_id(&base).unwrap();
        assert_ne!(base_id, root_chunk_id(&more_outputs).unwrap());
        assert_ne!(base_id, root_chunk_id(&renamed).unwrap());
        assert_ne!(
            partition(&base, &base_id, 2).unwrap()[0].id,
            partition(&renamed, &base_id, 2).unwrap()[0].id
        );

        let gates = diamond();
        let narrow = chunk_id(Some(&base_id), &["x".into()], &["c".into()], &gates);
        let wide = chunk_id(
            Some(&base_id),
            &["x".into(), "y".into()],
            &["c".into()],
            &gates,
        );
        assert_ne!(narrow, wide);
    }

    #[test]
    fn unused_primary_input_stays_on_first_chunk_recursively() {
        let net = netlist(
            &["x", "unused"],
            &["b"],
            vec![Gate::nor("a", &["x"]), Gate::nor("b", &["a"])],
        );
        let root = root_chunk_id(&net).unwrap();
        let chunks = partition(&net, &root, 1).unwrap();
        assert_eq!(chunks[0].boundary_inputs, ["unused", "x"]);
        assert_eq!(chunks[1].boundary_inputs, ["a"]);

        let again = partition(&chunks[0].netlist, &chunks[0].id, 1).unwrap();
        assert_eq!(again.len(), 1);
        assert_eq!(again[0].boundary_inputs, ["unused", "x"]);

        let empty = netlist(&["x"], &[], vec![]);
        assert_eq!(partition(&empty, &parent(), 1), Ok(Vec::new()));
    }

    #[test]
    fn cross_chunk_chain_exposes_matching_boundary() {
        let chain = netlist(
            &["x"],
            &["b"],
            vec![Gate::nor("b", &["a"]), Gate::nor("a", &["x"])],
        );
        let chunks = partition(&chain, &parent(), 1).unwrap();
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].boundary_inputs, ["x"]);
        assert_eq!(chunks[0].boundary_outputs, ["a"]);
        assert_eq!(chunks[1].boundary_inputs, ["a"]);
        assert_eq!(chunks[1].boundary_outputs, ["b"]);
        assert_eq!(chunks[0].netlist.gates, [Gate::nor("a", &["x"])]);
        assert_eq!(chunks[1].netlist.inputs, ["a"]);
    }

    #[test]
    fn invalid_netlists_are_typed_errors() {
        let simple = netlist(&["x"], &["a"], vec![Gate::nor("a", &["x"])]);
        assert_eq!(
            partition(&simple, &parent(), 0),
            Err(PartitionError::ZeroChunkSize)
        );

        let cyclic = netlist(
            &[],
            &["a"],
            vec![Gate::nor("a", &["b"]), Gate::nor("b", &["a"])],
        );
        assert_eq!(
            partition(&cyclic, &parent(), 4),
            Err(PartitionError::Cycle {
                signals: vec!["a".into(), "b".into()]
            })
        );

        let duplicate = netlist(
            &["x"],
            &["a"],
            vec![Gate::nor("a", &["x"]), Gate::nor("a", &["x"])],
        );
        assert_eq!(
            partition(&duplicate, &parent(), 4),
            Err(PartitionError::DuplicateDriver { signal: "a".into() })
        );

        let undriven = netlist(&[], &["a"], vec![Gate::nor("a", &["x"])]);
        assert_eq!(
            partition(&undriven, &parent(), 4),
            Err(PartitionError::UndrivenSignal {
                gate: "a".into(),
                signal: "x".into()
            })
        );

        let undriven_output = netlist(&["x"], &["x"], vec![Gate::nor("a", &["x"])]);
        assert_eq!(
            root_chunk_id(&undriven_output),
            Err(PartitionError::UndrivenOutput { signal: "x".into() })
        );
    }
}
