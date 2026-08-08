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

use std::collections::HashMap;

use super::topology::{GateKind, Library, LibraryEntry, Primitive, TemplateNode};
use super::Netlist;

/// Index into `PrimitiveGraph::nodes`. Stable for the lifetime of one
/// `PrimitiveGraph` -- nothing in this module ever removes a node, so an id
/// handed out earlier stays valid until the graph itself is dropped.
pub type NodeId = usize;

/// Which pair of primitives a graph edge connects, and why: "the planner
/// does need to know that some connections must become physical adjacency
/// while others become a dust path of any length... it follows from the
/// primitive types at each end" (spec, "Rigid and routable, at realisation
/// time"). Stored explicitly on every [`Edge`] rather than only left
/// implicit in its endpoints' `Primitive` kinds, because distinguishing the
/// two kinds is this step's own deliverable -- but every edge `expand`
/// produces is self-consistent with that rule (see this module's tests).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EdgeKind {
    /// Must become physical adjacency (or another fixed, un-routed
    /// relationship -- see `expand`'s own doc comment on the torch-to-lamp
    /// edge for the one case that is fixed without being single-cell
    /// adjacency). Never realised as a dust path of arbitrary length.
    Rigid,
    /// May become a dust path of any length, bent however the (not yet
    /// built) planner and router decide.
    Routable,
}

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

/// One edge of the flat graph. Directed (`from` -> `to`) for bookkeeping
/// convenience -- a rigid edge's direction records nothing about
/// realisation (adjacency has no direction); a routable edge's direction is
/// signal flow, producer to consumer, matching every other "source ->
/// sink" convention already used in `compile`.
#[derive(Debug)]
pub struct Edge {
    pub from: NodeId,
    pub to: NodeId,
    pub kind: EdgeKind,
}

/// One flat graph of primitives for a whole circuit -- the output of
/// [`expand`]. No gate boundaries: a NOR gate's support, torch and input
/// pins sit in exactly the same `nodes`/`edges` as every other gate's, tied
/// back to their gate only through `Node::provenance` /
/// [`PrimitiveGraph::gate_nodes`].
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

    fn push_edge(&mut self, from: NodeId, to: NodeId, kind: EdgeKind) {
        self.edges.push(Edge { from, to, kind });
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
/// edges belonging to `gate`, and return the ids of its `Torch` node and its
/// `Input(0..arity)` nodes in order -- the two roles `expand` needs to wire
/// up afterwards.
fn instantiate(graph: &mut PrimitiveGraph, gate: usize, entry: &LibraryEntry) -> (NodeId, Vec<NodeId>) {
    let mut id_of: HashMap<TemplateNode, NodeId> = HashMap::with_capacity(entry.template.nodes.len());
    let mut instance_nodes = Vec::with_capacity(entry.template.nodes.len());

    for &(role, primitive) in &entry.template.nodes {
        let id = graph.push_node(primitive, Provenance::Gate { gate, role });
        id_of.insert(role, id);
        instance_nodes.push(id);
    }
    for &(a, b) in &entry.template.rigid_edges {
        graph.push_edge(id_of[&a], id_of[&b], EdgeKind::Rigid);
    }

    graph.gate_nodes[gate] = instance_nodes;

    let torch = id_of[&TemplateNode::Torch];
    let input_count = entry.template.nodes.iter().filter(|&&(role, _)| matches!(role, TemplateNode::Input(_))).count();
    let inputs: Vec<NodeId> = (0..input_count).map(|i| id_of[&TemplateNode::Input(i)]).collect();
    (torch, inputs)
}

/// Substitute each gate in `netlist` for its `library` entry and stitch the
/// signals between them together, producing one flat [`PrimitiveGraph`] for
/// the whole circuit -- "a mechanical pass, not a decision" (spec, "The
/// topology library").
///
/// # The two kinds of edge this produces
///
/// - **Rigid**: every gate's own internal structure (its entry's
///   `rigid_edges`, instantiated once per gate), plus one edge per declared
///   output, from its driving gate's torch to a new `Lamp` node.
/// - **Routable**: one edge per gate input, from whatever drives it (another
///   gate's torch, or a primary input's lever) to that input's own node.
///
/// # Why a declared output's lamp is rigid, not routable
///
/// `compile`'s `emit` places a declared output's lamp at a fixed offset from
/// its driving gate's own output pin (`gate_pin[g].down()`), decided before
/// any routing happens and never touched by where the net's dust
/// subsequently travels -- see `emit`'s "Every netlist output gets a lamp"
/// comment. That is a fixed relationship, not "a dust path of any length":
/// the router never gets to choose how far the lamp sits from the pin, so it
/// belongs on the rigid side of the distinction even though today's
/// placement happens to insert one intermediate dust cell (the pin itself)
/// between the torch and the lamp -- exactly the kind of "how it is built
/// out of blocks" detail the spec assigns to the planner, not to this graph.
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

    // One gate cluster (support + torch + input pins) per gate, from the
    // library entry `Library::choose` picks for its arity.
    let mut torch_of: Vec<NodeId> = Vec::with_capacity(netlist.gates.len());
    let mut input_pin_of: Vec<Vec<NodeId>> = Vec::with_capacity(netlist.gates.len());
    for (g, gate) in netlist.gates.iter().enumerate() {
        let arity = gate.inputs.len();
        let kind = GateKind::Nor(arity);
        let entry = library
            .choose(kind)
            .ok_or_else(|| ExpandError::NoLibraryEntry { gate: gate.output.clone(), arity })?;
        let (torch, inputs) = instantiate(&mut graph, g, entry);
        torch_of.push(torch);
        input_pin_of.push(inputs);
    }

    // Routable edges: one per gate input, from its producer to that input's
    // own node.
    for (g, gate) in netlist.gates.iter().enumerate() {
        for (i, input_name) in gate.inputs.iter().enumerate() {
            let producer = resolve_producer(input_name, &lever_of, &producer_of, &torch_of)
                .ok_or_else(|| ExpandError::UndrivenSignal(input_name.clone()))?;
            graph.push_edge(producer, input_pin_of[g][i], EdgeKind::Routable);
        }
    }

    // Rigid edges: one per declared output, from its driving gate's torch to
    // a fresh Lamp node.
    for output_name in &netlist.outputs {
        let &g = producer_of
            .get(output_name.as_str())
            .ok_or_else(|| ExpandError::UndrivenSignal(output_name.clone()))?;
        let lamp = graph.push_node(Primitive::Lamp, Provenance::PrimaryOutput { name: output_name.clone() });
        graph.push_edge(torch_of[g], lamp, EdgeKind::Rigid);
    }

    Ok(graph)
}

fn resolve_producer(
    signal: &str,
    lever_of: &HashMap<&str, NodeId>,
    producer_of: &HashMap<&str, usize>,
    torch_of: &[NodeId],
) -> Option<NodeId> {
    if let Some(&lever) = lever_of.get(signal) {
        return Some(lever);
    }
    producer_of.get(signal).map(|&g| torch_of[g])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::topology::Library;
    use crate::compile::Gate;

    fn gate(output: &str, inputs: &[&str]) -> Gate {
        Gate { name: output.to_string(), inputs: inputs.iter().map(|s| s.to_string()).collect(), output: output.to_string() }
    }

    #[test]
    fn expanding_a_single_not_gate_yields_one_lever_one_input_one_torch_one_lamp() {
        let netlist =
            Netlist { inputs: vec!["a".to_string()], outputs: vec!["g0".to_string()], gates: vec![gate("g0", &["a"])] };
        let library = Library::default_library();
        let graph = expand(&netlist, &library).expect("a 1-input NOR has a library entry");

        let primitives: Vec<Primitive> = graph.nodes.iter().map(|n| n.primitive).collect();
        assert_eq!(primitives.iter().filter(|&&p| p == Primitive::Lever).count(), 1);
        assert_eq!(primitives.iter().filter(|&&p| p == Primitive::Repeater).count(), 1);
        assert_eq!(primitives.iter().filter(|&&p| p == Primitive::Block).count(), 1);
        assert_eq!(primitives.iter().filter(|&&p| p == Primitive::Torch).count(), 1);
        assert_eq!(primitives.iter().filter(|&&p| p == Primitive::Lamp).count(), 1);

        let rigid = graph.edges.iter().filter(|e| e.kind == EdgeKind::Rigid).count();
        let routable = graph.edges.iter().filter(|e| e.kind == EdgeKind::Routable).count();
        // Rigid: Input->Support, Torch->Support, Torch->Lamp.
        assert_eq!(rigid, 3);
        // Routable: Lever->Input.
        assert_eq!(routable, 1);

        assert_eq!(graph.gate_nodes.len(), 1);
        assert_eq!(graph.gate_nodes[0].len(), 3, "support + torch + one input pin");
    }

    #[test]
    fn a_shared_producer_fans_out_to_two_routable_edges_from_one_torch_node() {
        // g0 = NOR(a); g1 = NOR(g0); g2 = NOR(g0, a) -- g0's torch feeds two
        // consumers, so it should be exactly one node with two outgoing
        // routable edges, not two copies of it.
        let netlist = Netlist {
            inputs: vec!["a".to_string()],
            outputs: vec!["g1".to_string(), "g2".to_string()],
            gates: vec![gate("g0", &["a"]), gate("g1", &["g0"]), gate("g2", &["g0", "a"])],
        };
        let library = Library::default_library();
        let graph = expand(&netlist, &library).expect("every gate here has 1 or 2 inputs");

        let g0_torch = graph.gate_nodes[0]
            .iter()
            .find(|&&id| matches!(&graph.nodes[id].provenance, Provenance::Gate { role: TemplateNode::Torch, .. }))
            .copied()
            .expect("g0 has a torch node");
        // `edges_from` also includes g0's own rigid Torch->Support edge, so
        // filter down to the routable ones -- g1's input pin and g2's first
        // input pin, and nothing else: g0 is not itself a declared output,
        // so its torch gets no lamp edge (unlike g1's own torch, a different
        // node, which does).
        let outgoing_routable: Vec<&Edge> =
            graph.edges_from(g0_torch).filter(|e| e.kind == EdgeKind::Routable).collect();
        assert_eq!(outgoing_routable.len(), 2);
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

    #[test]
    fn every_rigid_edge_endpoint_pair_never_also_appears_as_a_routable_pair() {
        // Cross-check for `EdgeKind`'s own doc comment: the (primitive kind
        // of `from`, primitive kind of `to`) pair should determine the edge
        // kind on its own, for every edge `expand` ever produces on a
        // reasonably rich netlist.
        let netlist = Netlist {
            inputs: vec!["a".to_string(), "b".to_string()],
            outputs: vec!["g2".to_string()],
            gates: vec![gate("g0", &["a"]), gate("g1", &["b"]), gate("g2", &["g0", "g1"])],
        };
        let library = Library::default_library();
        let graph = expand(&netlist, &library).expect("valid netlist");

        let mut rigid_pairs = std::collections::HashSet::new();
        let mut routable_pairs = std::collections::HashSet::new();
        for edge in &graph.edges {
            let pair = (graph.nodes[edge.from].primitive, graph.nodes[edge.to].primitive);
            match edge.kind {
                EdgeKind::Rigid => rigid_pairs.insert(pair),
                EdgeKind::Routable => routable_pairs.insert(pair),
            };
        }
        assert!(
            rigid_pairs.is_disjoint(&routable_pairs),
            "rigid pairs {rigid_pairs:?} overlap routable pairs {routable_pairs:?}"
        );
    }
}
