//! One flat graph of redstone primitives for a whole `Netlist` -- the
//! expansion described in
//! `docs/superpowers/specs/2026-08-08-primitive-level-flow.md`, "The flow"
//! and "The topology library".
//!
//! [`expand`] substitutes each gate in a [`Netlist`] for its
//! `topology::Library` entry and stitches the signals between gates (and the
//! circuit's own primary inputs/outputs) together, producing one graph with
//! no gate boundaries left in it -- "the analogue of expanding a CMOS gate
//! into its P and N transistors" for redstone. No coordinates, no faces, no
//! support-block placement: every node here is a primitive *kind*, not a
//! placed block, exactly as `topology::Template` describes one gate's own
//! internal structure.
//!
//! # Every edge is signal flow
//!
//! There is no `EdgeKind` here, and deliberately so: "rigidity" -- the idea
//! that two primitives must become physical adjacency -- is a realisation
//! concept, decided by a planner that does not exist yet, never a property
//! of the signal graph itself (spec: "There are no rigid edges here"). Every
//! [`Edge`] this module produces is directed the same way, producer to
//! consumer, and means the same thing: a signal goes from `from` to `to`.
//! For today's NOR-only library that makes the flat graph isomorphic to the
//! netlist it expands -- see `topology`'s own doc comment for why that is
//! the correct, unsurprising state of this layer rather than a sign it is
//! missing something.

use std::collections::HashMap;

use super::topology::{GateKind, Library, LibraryEntry, Primitive, TemplateNode};
use super::Netlist;

/// Index into `PrimitiveGraph::nodes`. Stable for the lifetime of one
/// `PrimitiveGraph` -- nothing in this module ever removes a node, so an id
/// handed out earlier stays valid until the graph itself is dropped.
pub type NodeId = usize;

/// Where one node of the flat graph came from -- what lets a region be
/// re-expanded later without rebuilding the rest of the graph (spec: "The
/// flat graph must remember which primitives came from which gate").
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Provenance {
    /// One primitive of `gate`'s own library-entry instance (`gate` indexes
    /// `Netlist::gates`, exactly as everywhere else in `compile`). `role`
    /// says which node of that entry's `Template` this is, so re-expanding
    /// gate `gate` with a different entry means: find every node whose
    /// provenance is `Gate { gate, .. }` (`PrimitiveGraph::gate_nodes[gate]`
    /// answers this in O(1)), drop them and the edges touching them, and
    /// instantiate the replacement in their place -- everything else in the
    /// graph is untouched.
    Gate { gate: usize, role: TemplateNode },
    /// One `Netlist::inputs` entry's lever. One node per name, shared by
    /// every gate that reads it -- a primary input's fan-out is edges out of
    /// this one node, not one node per reader.
    PrimaryInput { name: String },
    /// One `Netlist::outputs` entry's lamp.
    PrimaryOutput { name: String },
}

/// One node of the flat graph: a primitive kind, and where it came from.
#[derive(Debug)]
pub struct Node {
    pub primitive: Primitive,
    pub provenance: Provenance,
}

/// One edge of the flat graph: a signal from `from` to `to`, and nothing
/// else -- see this module's doc comment for why there is no `kind` field
/// any more.
#[derive(Debug)]
pub struct Edge {
    pub from: NodeId,
    pub to: NodeId,
}

/// One flat graph of primitives for a whole circuit -- the output of
/// [`expand`]. No gate boundaries: a NOR gate's torch sits in exactly the
/// same `nodes`/`edges` as every other gate's, tied back to their gate only
/// through `Node::provenance` / [`PrimitiveGraph::gate_nodes`].
#[derive(Debug)]
pub struct PrimitiveGraph {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    /// Every node instantiated for gate `g`'s own library-entry instance,
    /// indexed by `g` (an index into the `Netlist::gates` `expand` was
    /// given). See [`Provenance::Gate`]'s doc comment for what this is for.
    pub gate_nodes: Vec<Vec<NodeId>>,
}

impl PrimitiveGraph {
    fn empty(gate_count: usize) -> Self {
        PrimitiveGraph { nodes: Vec::new(), edges: Vec::new(), gate_nodes: vec![Vec::new(); gate_count] }
    }

    fn push_node(&mut self, primitive: Primitive, provenance: Provenance) -> NodeId {
        let id = self.nodes.len();
        self.nodes.push(Node { primitive, provenance });
        id
    }

    fn push_edge(&mut self, from: NodeId, to: NodeId) {
        self.edges.push(Edge { from, to });
    }

    /// Every edge whose `from` is `node`.
    pub fn edges_from(&self, node: NodeId) -> impl Iterator<Item = &Edge> {
        self.edges.iter().filter(move |edge| edge.from == node)
    }

    /// Every edge whose `to` is `node`.
    pub fn edges_to(&self, node: NodeId) -> impl Iterator<Item = &Edge> {
        self.edges.iter().filter(move |edge| edge.to == node)
    }
}

/// Why [`expand`] could not build a graph for a `Netlist`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExpandError {
    /// A gate's arity has no entry in the library at all -- mirrors
    /// `place_nor_gate`'s own `assert!`, but as a recoverable error, since a
    /// library (unlike the hard-coded router) is exactly the place new gate
    /// kinds are meant to be added.
    NoLibraryEntry { gate: String, arity: usize },
    /// A gate input, or a declared output, names a signal nothing in the
    /// netlist drives. `compile` already rejects this before it ever builds
    /// a floorplan (`CompileError::UndrivenSignal`); `expand` re-checks it
    /// independently so it stays correct as a standalone entry point, not
    /// only when called from inside `compile`.
    UndrivenSignal(String),
}

impl std::fmt::Display for ExpandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExpandError::NoLibraryEntry { gate, arity } => {
                write!(f, "no library entry for gate `{gate}` (fan-in {arity})")
            }
            ExpandError::UndrivenSignal(name) => write!(f, "signal `{name}` is never driven"),
        }
    }
}

impl std::error::Error for ExpandError {}

/// Instantiate `entry`'s template as a fresh, numbered set of nodes and
/// edges belonging to `gate`, and return the id of its `Template::output`
/// node and the ids its `Template::inputs` name, in order -- the two things
/// `expand` needs to wire up afterwards. Works the same way regardless of
/// how many nodes the entry has, or whether two of its `inputs` happen to
/// name the same node (every entry this module ships does, today).
fn instantiate(graph: &mut PrimitiveGraph, gate: usize, entry: &LibraryEntry) -> (NodeId, Vec<NodeId>) {
    let mut id_of: HashMap<TemplateNode, NodeId> = HashMap::with_capacity(entry.template.nodes.len());
    let mut instance_nodes = Vec::with_capacity(entry.template.nodes.len());

    for &(role, primitive) in &entry.template.nodes {
        let id = graph.push_node(primitive, Provenance::Gate { gate, role });
        id_of.insert(role, id);
        instance_nodes.push(id);
    }
    for &(a, b) in &entry.template.internal_edges {
        graph.push_edge(id_of[&a], id_of[&b]);
    }

    graph.gate_nodes[gate] = instance_nodes;

    // `expand` only ever asks the library for `GateKind::Nor`/`Buf` entries
    // today (see below), and every one of those names a real output
    // primitive -- an entry with none (a wire-merge OR) has no node for
    // this to look up, and `expand` does not yet know how to wire one in
    // (its whole model is "one output node feeds its consumers"; a merge's
    // consumers are fed by its own *inputs* instead, which is a different
    // wiring shape this module has not been taught yet).
    let output = entry.template.output.map(|role| id_of[&role]).expect(
        "instantiate only ever runs on an entry with a real output primitive -- a wire-merge OR \
         entry is not reachable through `expand` yet",
    );
    let inputs: Vec<NodeId> = entry.template.inputs.iter().map(|role| id_of[role]).collect();
    (output, inputs)
}

/// Substitute each gate in `netlist` for its `library` entry and stitch the
/// signals between them together, producing one flat [`PrimitiveGraph`] for
/// the whole circuit -- "a mechanical pass, not a decision" (spec, "The
/// topology library").
///
/// Two shapes of edge come out of this, both the same [`Edge`] kind: one per
/// gate input, from whatever drives it (another gate's output node, or a
/// primary input's lever) to that input's own landing node; and one per
/// declared output, from its driving gate's own output node to a fresh
/// `Lamp` node.
pub fn expand(netlist: &Netlist, library: &Library) -> Result<PrimitiveGraph, ExpandError> {
    let mut graph = PrimitiveGraph::empty(netlist.gates.len());

    // One Lever node per primary input, keyed by name so gate inputs below
    // can look it up.
    let mut lever_of: HashMap<&str, NodeId> = HashMap::with_capacity(netlist.inputs.len());
    for name in &netlist.inputs {
        let id = graph.push_node(Primitive::Lever, Provenance::PrimaryInput { name: name.clone() });
        lever_of.insert(name.as_str(), id);
    }

    let mut producer_of: HashMap<&str, usize> = HashMap::with_capacity(netlist.gates.len());
    for (g, gate) in netlist.gates.iter().enumerate() {
        producer_of.insert(gate.output.as_str(), g);
    }

    // One node cluster per gate, from the library entry `Library::choose`
    // picks for its arity.
    let mut output_of: Vec<NodeId> = Vec::with_capacity(netlist.gates.len());
    let mut input_targets_of: Vec<Vec<NodeId>> = Vec::with_capacity(netlist.gates.len());
    for (g, gate) in netlist.gates.iter().enumerate() {
        let arity = gate.inputs.len();
        let kind = GateKind::Nor(arity);
        let entry = library
            .choose(kind)
            .ok_or_else(|| ExpandError::NoLibraryEntry { gate: gate.output.clone(), arity })?;
        let (output, inputs) = instantiate(&mut graph, g, entry);
        output_of.push(output);
        input_targets_of.push(inputs);
    }

    // One edge per gate input, from its producer to that input's own landing
    // node.
    for (g, gate) in netlist.gates.iter().enumerate() {
        for (i, input_name) in gate.inputs.iter().enumerate() {
            let producer = resolve_producer(input_name, &lever_of, &producer_of, &output_of)
                .ok_or_else(|| ExpandError::UndrivenSignal(input_name.clone()))?;
            graph.push_edge(producer, input_targets_of[g][i]);
        }
    }

    // One edge per declared output, from its driving gate's own output node
    // to a fresh Lamp node.
    for output_name in &netlist.outputs {
        let &g = producer_of
            .get(output_name.as_str())
            .ok_or_else(|| ExpandError::UndrivenSignal(output_name.clone()))?;
        let lamp = graph.push_node(Primitive::Lamp, Provenance::PrimaryOutput { name: output_name.clone() });
        graph.push_edge(output_of[g], lamp);
    }

    Ok(graph)
}

fn resolve_producer(
    signal: &str,
    lever_of: &HashMap<&str, NodeId>,
    producer_of: &HashMap<&str, usize>,
    output_of: &[NodeId],
) -> Option<NodeId> {
    if let Some(&lever) = lever_of.get(signal) {
        return Some(lever);
    }
    producer_of.get(signal).map(|&g| output_of[g])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::topology::Library;
    use crate::compile::Gate;

    fn gate(output: &str, inputs: &[&str]) -> Gate {
        Gate {
            name: output.to_string(),
            inputs: inputs.iter().map(|s| s.to_string()).collect(),
            output: output.to_string(),
            is_merge: false,
        }
    }

    #[test]
    fn expanding_a_single_not_gate_yields_one_lever_one_torch_one_lamp_and_two_edges() {
        let netlist =
            Netlist { inputs: vec!["a".to_string()], outputs: vec!["g0".to_string()], gates: vec![gate("g0", &["a"])] };
        let library = Library::default_library();
        let graph = expand(&netlist, &library).expect("a 1-input NOR has a library entry");

        let primitives: Vec<Primitive> = graph.nodes.iter().map(|n| n.primitive).collect();
        assert_eq!(primitives.iter().filter(|&&p| p == Primitive::Lever).count(), 1);
        assert_eq!(primitives.iter().filter(|&&p| p == Primitive::Torch).count(), 1);
        assert_eq!(primitives.iter().filter(|&&p| p == Primitive::Lamp).count(), 1);
        assert_eq!(graph.nodes.len(), 3, "no Support, no per-input Repeater any more");

        // Lever->Torch, Torch->Lamp: exactly the netlist's own two edges,
        // nothing else.
        assert_eq!(graph.edges.len(), 2);

        assert_eq!(graph.gate_nodes.len(), 1);
        assert_eq!(graph.gate_nodes[0].len(), 1, "a NOR gate is one node: its torch");
    }

    #[test]
    fn a_shared_producer_fans_out_to_two_edges_from_one_torch_node() {
        // g0 = NOR(a); g1 = NOR(g0); g2 = NOR(g0, a) -- g0's torch feeds two
        // consumers, so it should be exactly one node with two outgoing
        // edges, not two copies of it.
        let netlist = Netlist {
            inputs: vec!["a".to_string()],
            outputs: vec!["g1".to_string(), "g2".to_string()],
            gates: vec![gate("g0", &["a"]), gate("g1", &["g0"]), gate("g2", &["g0", "a"])],
        };
        let library = Library::default_library();
        let graph = expand(&netlist, &library).expect("every gate here has 1 or 2 inputs");

        let g0_torch = graph.gate_nodes[0][0];
        assert!(
            matches!(&graph.nodes[g0_torch].provenance, Provenance::Gate { gate: 0, role: TemplateNode::Torch }),
            "gate 0 is a single Torch node"
        );
        // g0 is not itself a declared output, so its torch gets no lamp
        // edge -- just its two fan-out edges (g1's landing node and g2's
        // first input's landing node).
        let outgoing: Vec<&Edge> = graph.edges_from(g0_torch).collect();
        assert_eq!(outgoing.len(), 2);
    }

    #[test]
    fn expand_rejects_a_gate_with_no_library_entry_for_its_arity() {
        use std::collections::BTreeMap;
        let netlist =
            Netlist { inputs: vec!["a".to_string()], outputs: vec![], gates: vec![gate("g0", &["a", "b", "c", "d"])] };
        // A library that only knows about arity 1..=3, same shape as
        // `Library::default_library` but built directly so this test does
        // not depend on that constructor's own arity range.
        let mut entries = BTreeMap::new();
        for arity in 1..=3 {
            entries.insert(GateKind::Nor(arity), vec![]);
        }
        let library = Library::new(entries);

        // "b", "c", "d" are undriven too, but arity is checked first.
        let err = expand(&netlist, &library).expect_err("fan-in 4 has no entry");
        assert_eq!(err, ExpandError::NoLibraryEntry { gate: "g0".to_string(), arity: 4 });
    }

    #[test]
    fn expand_rejects_an_undriven_input() {
        let netlist =
            Netlist { inputs: vec![], outputs: vec![], gates: vec![gate("g0", &["nowhere"])] };
        let library = Library::default_library();
        let err = expand(&netlist, &library).expect_err("`nowhere` drives nothing");
        assert_eq!(err, ExpandError::UndrivenSignal("nowhere".to_string()));
    }

    /// The consequence the spec calls out explicitly: for a NOR-only
    /// netlist, the flat graph is isomorphic to the netlist -- one node per
    /// gate (plus one per primary input/output), one edge per net endpoint.
    /// Not a sign this layer is unfinished; the layer earns its keep the day
    /// an entry stops being one-to-one (see `topology`'s doc comment).
    #[test]
    fn the_graph_is_isomorphic_to_a_nor_only_netlist() {
        let netlist = Netlist {
            inputs: vec!["a".to_string(), "b".to_string()],
            outputs: vec!["g2".to_string()],
            gates: vec![gate("g0", &["a"]), gate("g1", &["b"]), gate("g2", &["g0", "g1"])],
        };
        let library = Library::default_library();
        let graph = expand(&netlist, &library).expect("valid netlist");

        // One node per primary input/output/gate.
        let expected_nodes = netlist.inputs.len() + netlist.outputs.len() + netlist.gates.len();
        assert_eq!(graph.nodes.len(), expected_nodes);

        // One edge per gate input, plus one per declared output -- exactly
        // what the netlist itself declares, no more.
        let expected_edges: usize =
            netlist.gates.iter().map(|g| g.inputs.len()).sum::<usize>() + netlist.outputs.len();
        assert_eq!(graph.edges.len(), expected_edges);
    }
}
