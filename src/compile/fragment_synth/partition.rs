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
    #[error("declared output {signal} has no primary-input or gate driver")]
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
            Some(_) => {}
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

/// The identity a whole netlist has as one child of `parent`.
///
/// The same descriptor [`root_chunk_id`] builds, with the enclosing identity
/// folded in: a node that is packed into a parent is that parent's child, and
/// two structurally identical nodes under different parents are not the same
/// child.  It is derived from the declared interface and the membership only,
/// so nothing a placer or router later decides can move it.
pub fn node_chunk_id(netlist: &Netlist, parent: &ChunkId) -> Result<ChunkId, PartitionError> {
    canonical_order(netlist)?;
    Ok(chunk_id(
        Some(parent),
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
/// again. A netlist with no gates but declared aliases has one pass-through
/// chunk; with no gates or declared outputs it partitions to zero chunks.
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
        if netlist.outputs.is_empty() {
            return Ok(Vec::new());
        }
        let boundary_inputs: Vec<String> = netlist.inputs.iter().cloned()
            .collect::<BTreeSet<_>>().into_iter().collect();
        let boundary_outputs: Vec<String> = netlist.outputs.iter().cloned()
            .collect::<BTreeSet<_>>().into_iter().collect();
        return Ok(vec![Chunk {
            id: chunk_id(Some(parent), &boundary_inputs, &boundary_outputs, &[]),
            netlist: Netlist {
                inputs: boundary_inputs.clone(), outputs: boundary_outputs.clone(), gates: vec![],
            },
            boundary_inputs,
            boundary_outputs,
        }]);
    }
    let groups = order.chunks(max_gates).map(<[usize]>::to_vec).collect::<Vec<_>>();
    partition_groups(netlist, parent, &groups)
}

fn partition_groups(netlist: &Netlist, parent: &ChunkId, groups: &[Vec<usize>]) -> Result<Vec<Chunk>, PartitionError> {
    let producer_of = producers(netlist)?;
    let chunk_count = groups.len();
    let mut chunk_of = vec![0usize; netlist.gates.len()];
    for (chunk, members) in groups.iter().enumerate() {
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
    // A primary alias is carried through the first child, including its input
    // boundary. Repartitioning that child preserves the same pass-through.
    for signal in &netlist.outputs {
        let producer = producer_of[signal.as_str()];
        if producer == PRIMARY {
            inputs[0].insert(signal);
            outputs[0].insert(signal);
        } else {
            outputs[chunk_of[producer]].insert(signal);
        }
    }

    Ok(groups
        .iter()
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

/// Generate bounded topological cuts with the fewest crossing signal edges.
/// This is only a candidate construction heuristic; runtime quality admission
/// still compares every public and actual private site against the incumbent.
pub(crate) fn partition_minimum_crossing(
    netlist: &Netlist,
    parent: &ChunkId,
    max_gates: usize,
) -> Result<Vec<Chunk>, PartitionError> {
    if max_gates == 0 { return Err(PartitionError::ZeroChunkSize); }
    let order = canonical_order(netlist)?;
    if order.is_empty() { return partition(netlist, parent, max_gates); }
    let mut position = vec![0; order.len()];
    for (at, &gate) in order.iter().enumerate() { position[gate] = at; }
    let producer = producers(netlist)?;
    let mut last_use = position.clone();
    for (consumer, gate) in netlist.gates.iter().enumerate() {
        for signal in &gate.inputs {
            let source = producer[signal.as_str()];
            if source != PRIMARY { last_use[source] = last_use[source].max(position[consumer]); }
        }
    }
    let mut changes = vec![0isize; order.len()+2];
    for gate in 0..order.len() {
        if last_use[gate] > position[gate] {
            changes[position[gate]+1] += 1;
            changes[last_use[gate]+1] -= 1;
        }
    }
    let mut active = 0isize;
    let crossing = changes.iter().take(order.len()+1).map(|delta| {
        active += delta; active as usize
    }).collect::<Vec<_>>();
    let mut best = vec![(usize::MAX, usize::MAX); order.len()+1];
    let mut previous = vec![0; order.len()+1];
    best[0] = (0, 0);
    for end in 1..=order.len() {
        for start in end.saturating_sub(max_gates)..end {
            let score = (best[start].0 + crossing[start], best[start].1 + 1);
            if score < best[end] { best[end] = score; previous[end] = start; }
        }
    }
    let mut groups = Vec::new();
    let mut end = order.len();
    while end > 0 { let start = previous[end]; groups.push(order[start..end].to_vec()); end = start; }
    groups.reverse();
    partition_groups(netlist, parent, &groups)
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
    #[test]
    fn minimum_crossing_cuts_follow_dependencies_and_preserve_all_interfaces() {
        let mut gates = Vec::new();
        let mut driver = "input".to_owned();
        for index in 0..8 {
            let name = format!("c{index:02}"); gates.push(Gate::nor(&name, &[driver.as_str()])); driver = name;
        }
        for index in 0..16 { gates.push(Gate::nor(&format!("f{index:02}"), &[driver.as_str()])); }
        for index in 0..16 { gates.push(Gate::nor(&format!("h{index:02}"), &[&format!("f{index:02}")])); }
        let net = Netlist { inputs: vec!["input".into(), "unused".into()],
            outputs: (0..16).map(|i| format!("h{i:02}")).collect(), gates };
        let root = root_chunk_id(&net).unwrap();
        let chunks = partition_minimum_crossing(&net, &root, 32).unwrap();
        assert_eq!(chunks.iter().map(|c| c.netlist.gates.len()).collect::<Vec<_>>(), vec![8, 32]);
        assert_eq!(chunks[0].boundary_outputs, vec!["c07"]);
        assert!(chunks[0].boundary_inputs.contains(&"unused".to_owned()));
        assert_eq!(chunks[1].boundary_inputs, vec!["c07"]);
        assert_eq!(chunks[1].boundary_outputs, net.outputs);
        let mut reverse = net.clone(); reverse.gates.reverse();
        assert_eq!(partition_minimum_crossing(&reverse, &root, 32).unwrap(), chunks);
        for chunk in chunks { canonical_order(&chunk.netlist).unwrap(); }
    }

    #[test]
    fn minimum_crossing_cuts_match_exhaustive_cut_search() {
        let gates = vec![Gate::nor("a", &["x"]), Gate::nor("b", &["a"]),
            Gate::nor("c", &["a"]), Gate::nor("d", &["b", "c"]),
            Gate::nor("e", &["d"]), Gate::nor("f", &["c"]),
            Gate::nor("g", &["e", "f"]), Gate::nor("h", &["g"])];
        let net = Netlist { inputs: vec!["x".into()], outputs: vec!["h".into()], gates };
        let order = canonical_order(&net).unwrap();
        let cut_cost = |cut: usize| {
            let before = order[..cut].iter().map(|&i| net.gates[i].output.as_str()).collect::<BTreeSet<_>>();
            order[cut..].iter().flat_map(|&i| &net.gates[i].inputs)
                .filter(|s| before.contains(s.as_str())).collect::<BTreeSet<_>>().len()
        };
        let mut optimum = (usize::MAX, usize::MAX);
        for mask in 0..(1usize << (order.len()-1)) {
            let mut cuts = vec![0];
            cuts.extend((1..order.len()).filter(|cut| mask & (1 << (cut-1)) != 0)); cuts.push(order.len());
            if cuts.windows(2).any(|pair| pair[1]-pair[0]>3) { continue; }
            optimum = optimum.min((cuts[1..cuts.len()-1].iter().map(|&cut| cut_cost(cut)).sum(), cuts.len()-1));
        }
        let root = root_chunk_id(&net).unwrap();
        let chunks = partition_minimum_crossing(&net, &root, 3).unwrap();
        let mut end = 0; let mut edges = 0;
        for chunk in chunks.iter().take(chunks.len()-1) { end += chunk.netlist.gates.len(); edges += cut_cost(end); }
        assert_eq!((edges, chunks.len()), optimum);
        assert!(partition_minimum_crossing(&net, &root, 0).is_err());
    }

    #[test]
    fn primary_aliases_keep_a_driver_through_repartitioning() {
        for net in [
            netlist(&["a", "unused"], &["a", "a"], vec![]),
            netlist(&["a", "b"], &["a", "y", "a"], vec![Gate::nor("y", &["a", "b"])]),
        ] {
            let root = root_chunk_id(&net).unwrap();
            let children = partition(&net, &root, 1).unwrap();
            assert_eq!(children.len(), 1);
            assert!(children[0].boundary_inputs.contains(&"a".into()));
            assert!(children[0].boundary_outputs.contains(&"a".into()));
            canonical_order(&children[0].netlist).unwrap();
            let grandchildren = partition(&children[0].netlist, &children[0].id, 1).unwrap();
            assert_eq!(grandchildren.len(), 1);
            assert_eq!(grandchildren[0].boundary_inputs, children[0].boundary_inputs);
            assert_eq!(grandchildren[0].boundary_outputs, children[0].boundary_outputs);
        }
    }

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

        let undriven_output = netlist(&["x"], &["missing"], vec![Gate::nor("a", &["x"])]);
        assert_eq!(
            root_chunk_id(&undriven_output),
            Err(PartitionError::UndrivenOutput { signal: "missing".into() })
        );
    }
}
