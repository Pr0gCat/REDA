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
//! # Signal edges and stateful control relations
//!
//! Ordinary [`EdgeKind::Signal`] edges are directed producer-to-consumer
//! signal flow.  The sole exception is a Design H DFF's
//! [`EdgeKind::RepeaterLockSide`] relation: it is still directed from its
//! controlling repeater to the locked data repeater, but it must be realised
//! at a side port, never as rear-data signal flow.  Rigidity -- physical
//! adjacency -- remains a planner concern; the typed relation only preserves
//! the electrical port contract that distinguishes a repeater lock from data.

use std::collections::{BTreeMap, HashMap};

use super::topology::{
    GateKind, Library, LibraryEntry, Primitive, StatefulPrimitiveRole, StatefulTopology,
    TemplateNode,
};
use super::Netlist;

/// Index into `PrimitiveGraph::nodes`. Stable for the lifetime of one
/// `PrimitiveGraph` -- nothing in this module ever removes a node, so an id
/// handed out earlier stays valid until the graph itself is dropped.
pub type NodeId = usize;

/// Index into [`PrimitiveGraph::edges`].
pub type EdgeId = usize;

/// The selected library entry for each gate index.  Absence means the
/// library's normal default policy remains in effect.
pub type EntrySelection = BTreeMap<usize, usize>;

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
    /// `Some` only for a node owned by a stateful macro.  `provenance` still
    /// gives every node its gate index; this field preserves the Design H
    /// role without overloading a combinational template role.
    pub stateful_role: Option<StatefulPrimitiveRole>,
}

/// How two primitives are related.  A lock-side relation is intentionally
/// not an ordinary signal input: a realiser must attach it to a repeater's
/// side port rather than its rear data port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeKind {
    Signal,
    RepeaterLockSide,
}

/// One relation of the flat primitive graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Edge {
    pub from: NodeId,
    pub to: NodeId,
    pub kind: EdgeKind,
}

/// One instantiated stateful macro.  Its node and edge ids refer directly
/// into the owning [`PrimitiveGraph`], so the graph has one source of truth
/// for every relationship while consumers can still recognise the DFF as a
/// single region.
#[derive(Debug)]
pub struct StatefulRegion {
    pub gate: usize,
    pub d_landing: NodeId,
    pub clock_landing: NodeId,
    pub q_contributor: NodeId,
    pub primitives: Vec<NodeId>,
    pub ordinary_edges: Vec<EdgeId>,
    pub lock_side_relations: Vec<EdgeId>,
    roles: Vec<(StatefulPrimitiveRole, NodeId)>,
}

impl StatefulRegion {
    pub fn node_for(&self, role: StatefulPrimitiveRole) -> Option<NodeId> {
        self.roles
            .iter()
            .find_map(|&(candidate, node)| (candidate == role).then_some(node))
    }
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
    ///
    /// **A merge's row can be empty**: a merge whose every branch is bare
    /// instantiates no primitive at all (see [`expand`]'s own doc comment),
    /// so it owns no nodes. That is not a missing entry -- it is the whole
    /// point of a wire merge -- which is exactly why
    /// [`PrimitiveGraph::output_nodes`] exists alongside this.
    pub gate_nodes: Vec<Vec<NodeId>>,
    /// The set of nodes a consumer of gate `g`'s declared output actually
    /// reads it from, indexed by `g` -- [`expand`]'s own `output_of`, kept
    /// rather than dropped on the floor.
    ///
    /// For a `Nor`/`Buf` gate this is the singleton `[its torch]`, the same
    /// node `gate_nodes[g]` holds. For a merge it is the set its branches
    /// resolve to: each bare branch's own producer's set spliced straight
    /// through, each isolated branch's own repeater. So a merge's row is
    /// *never* empty even when `gate_nodes[g]` is -- it names the nodes
    /// whose dust physically joins to form that merge's net, which is the
    /// only honest answer to "where is this gate" for a gate that is a
    /// junction rather than a body.
    ///
    /// [`expand`] fills this in for every gate it processes; the invariant a
    /// consumer may rely on is that every element indexes a real node, and
    /// that a gate driving a declared output has at least one.
    pub output_nodes: Vec<Vec<NodeId>>,
    /// Every stateful macro expanded from a sequential netlist gate.
    pub stateful_regions: Vec<StatefulRegion>,
}

impl PrimitiveGraph {
    fn empty(gate_count: usize) -> Self {
        PrimitiveGraph {
            nodes: Vec::new(),
            edges: Vec::new(),
            gate_nodes: vec![Vec::new(); gate_count],
            output_nodes: vec![Vec::new(); gate_count],
            stateful_regions: Vec::new(),
        }
    }

    fn push_node(&mut self, primitive: Primitive, provenance: Provenance) -> NodeId {
        let id = self.nodes.len();
        self.nodes.push(Node {
            primitive,
            provenance,
            stateful_role: None,
        });
        id
    }

    fn push_stateful_node(
        &mut self,
        primitive: Primitive,
        provenance: Provenance,
        role: StatefulPrimitiveRole,
    ) -> NodeId {
        let id = self.nodes.len();
        self.nodes.push(Node {
            primitive,
            provenance,
            stateful_role: Some(role),
        });
        id
    }

    fn push_edge(&mut self, from: NodeId, to: NodeId) -> EdgeId {
        self.push_edge_kind(from, to, EdgeKind::Signal)
    }

    fn push_edge_kind(&mut self, from: NodeId, to: NodeId, kind: EdgeKind) -> EdgeId {
        let id = self.edges.len();
        self.edges.push(Edge { from, to, kind });
        id
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
    /// A positive-edge DFF must have exactly its data and clock inputs.
    InvalidDffArity { gate: String, arity: usize },
    /// A gate input, or a declared output, names a signal nothing in the
    /// netlist drives. `compile` already rejects this before it ever builds
    /// a floorplan (`CompileError::UndrivenSignal`); `expand` re-checks it
    /// independently so it stays correct as a standalone entry point, not
    /// only when called from inside `compile`.
    UndrivenSignal(String),
    /// The netlist has a combinational cycle. `compile` already rejects this
    /// before it ever builds a floorplan (`CompileError::CyclicNetlist`);
    /// `expand` needs its own topological order to process a merge gate only
    /// after every gate it reads from (see this module's own doc comment on
    /// why a merge's own realisation, unlike a NOR's, actually depends on
    /// that order), so it re-checks this independently for the same reason
    /// `UndrivenSignal` does.
    CyclicNetlist,
    /// A gate is still at the **gate level** -- an `$_AND_`, `$_MUX_` and so
    /// on -- which has no redstone realisation and therefore no primitives
    /// to expand into. `compile::lowering::lower` is the pass that turns one
    /// into torches and merges, and it has to run first.
    ///
    /// This is deliberately an error rather than an implicit lowering: this
    /// module's `PrimitiveGraph::gate_nodes` and `Provenance::Gate` index
    /// the caller's own `Netlist::gates`, so silently expanding a *different*
    /// netlist than the caller handed over would make every one of those
    /// indices point at the wrong gate.
    NotRealisable { gate: String, kind: GateKind },
    /// A requested alternative is not registered for the gate's kind.
    UnknownLibraryEntry { gate: String, entry: usize },
    /// A registered alternative would violate the signal graph's hard
    /// fan-out constraint (for example, a bare merge on a shared signal).
    IllegalLibraryEntry {
        gate: String,
        entry: usize,
        reason: String,
    },
}

impl std::fmt::Display for ExpandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExpandError::NoLibraryEntry { gate, arity } => {
                write!(f, "no library entry for gate `{gate}` (fan-in {arity})")
            }
            ExpandError::InvalidDffArity { gate, arity } => write!(
                f,
                "DFF gate `{gate}` needs exactly 2 inputs (data and clock), got {arity}"
            ),
            ExpandError::UndrivenSignal(name) => write!(f, "signal `{name}` is never driven"),
            ExpandError::CyclicNetlist => write!(f, "netlist has a combinational cycle"),
            ExpandError::NotRealisable { gate, kind } => write!(
                f,
                "gate `{gate}` is a {kind:?}, which has no redstone realisation of its own -- run                  `compile::lowering::lower` on this netlist first"
            ),
            ExpandError::UnknownLibraryEntry { gate, entry } => {
                write!(f, "gate `{gate}` has no library entry {entry}")
            }
            ExpandError::IllegalLibraryEntry { gate, entry, reason } => {
                write!(f, "gate `{gate}` cannot use library entry {entry}: {reason}")
            }
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
///
/// Only ever called for a `GateKind::Nor`/`Buf` entry, both of which always
/// name a real output primitive -- a wire-merge `Or` entry (which may have
/// none, see `or_bare_entry`'s own doc comment) goes through
/// [`expand`]'s own merge-specific branch instead, never through here.
fn instantiate(
    graph: &mut PrimitiveGraph,
    gate: usize,
    entry: &LibraryEntry,
) -> (NodeId, Vec<NodeId>) {
    let mut id_of: HashMap<TemplateNode, NodeId> =
        HashMap::with_capacity(entry.template.nodes.len());
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

    let output = entry.template.output.map(|role| id_of[&role]).expect(
        "instantiate only ever runs on a Nor/Buf entry, which always names a real output primitive",
    );
    let inputs: Vec<NodeId> = entry
        .template
        .inputs
        .iter()
        .map(|role| id_of[role])
        .collect();
    (output, inputs)
}

/// The only reuse of [`TemplateNode`] inside a stateful macro is for the
/// stable, gate-indexed provenance identifier already understood by older
/// graph consumers.  The semantic role is carried separately in
/// [`Node::stateful_role`], so these identifiers never claim Design H is a
/// combinational template or an isolated merge.
fn stateful_provenance_role(role: StatefulPrimitiveRole) -> TemplateNode {
    match role {
        StatefulPrimitiveRole::MData => TemplateNode::Torch,
        StatefulPrimitiveRole::SData => TemplateNode::SecondTorch,
        StatefulPrimitiveRole::MLock => TemplateNode::IsolatingRepeater(0),
        StatefulPrimitiveRole::InvC => TemplateNode::IsolatingRepeater(1),
        StatefulPrimitiveRole::SLock => TemplateNode::IsolatingRepeater(2),
    }
}

/// Instantiate the fixed stateful portion of Design H.  External D/C edges
/// are attached afterwards, once every gate has published its Q contributor;
/// this is what permits feedback paths that cross a DFF boundary.
fn instantiate_stateful(
    graph: &mut PrimitiveGraph,
    gate: usize,
    entry: &StatefulTopology,
) -> StatefulRegion {
    let mut roles = Vec::with_capacity(entry.nodes.len());
    let mut primitives = Vec::with_capacity(entry.nodes.len());
    for &(role, primitive) in &entry.nodes {
        let node = graph.push_stateful_node(
            primitive,
            Provenance::Gate {
                gate,
                role: stateful_provenance_role(role),
            },
            role,
        );
        roles.push((role, node));
        primitives.push(node);
    }
    let node_for = |role| {
        roles
            .iter()
            .find_map(|&(candidate, node)| (candidate == role).then_some(node))
            .expect("a stateful topology relation names one of its nodes")
    };
    let mut ordinary_edges = Vec::new();
    for &(from, to) in &entry.signal_edges {
        ordinary_edges.push(graph.push_edge(node_for(from), node_for(to)));
    }
    let mut lock_side_relations = Vec::new();
    for &(from, to) in &entry.repeater_lock_sides {
        lock_side_relations.push(graph.push_edge_kind(
            node_for(from),
            node_for(to),
            EdgeKind::RepeaterLockSide,
        ));
    }

    StatefulRegion {
        gate,
        d_landing: node_for(entry.d_landing),
        clock_landing: node_for(entry.clock_landing),
        q_contributor: node_for(entry.q_contributor),
        primitives,
        ordinary_edges,
        lock_side_relations,
        roles,
    }
}

/// Substitute each gate in `netlist` for its `library` entry and stitch the
/// signals between them together, producing one flat [`PrimitiveGraph`] for
/// the whole circuit -- "a mechanical pass, not a decision" (spec, "The
/// topology library").
///
/// For a `Nor`/`Buf` gate this is exactly what it always was: one edge per
/// gate input, from whatever drives it (another gate's own contribution, or
/// a primary input's lever) to that input's own landing node (its single
/// Torch); one edge per declared output, from its driving gate's own output
/// node to a fresh `Lamp` node.
///
/// # Merges: a gate whose "output" is a *set* of nodes, not one
///
/// A `Nor`/`Buf` gate's own output is a single node (its own template's
/// `output` role) that every consumer reads from -- `instantiate` returns
/// exactly one `NodeId` for it, and that identity is what makes "one output
/// node feeds its consumers" true throughout the rest of this module.
///
/// A merge has no such node. `or_bare_entry`/`or_isolated_entry`
/// (`topology`) both leave `Template::output` `None`: a bare branch is nothing
/// but the point where its own producer's dust and the merge's other
/// branches touch, and an isolated branch's only real primitive is the
/// branch's own repeater, whose *input* faces the producer -- neither one
/// is a node a consumer could sensibly be said to "read the merge's output
/// from". So a merge's own contribution to whoever reads its declared
/// output is not one node, but the **set of nodes each of its own branches
/// actually resolves to**: a bare branch contributes its own producer's own
/// set of nodes directly (recursively -- if that producer is *itself* a
/// bare merge, its own set flows through unchanged, exactly mirroring how
/// `compile::place_merge_gate` really does let dust from arbitrarily many
/// chained bare merges touch as one physical net); an isolated branch
/// contributes exactly its own repeater's node (one physical, reconstituted
/// signal, however many nodes fed into it).
///
/// This is why `output_of` below is `Vec<Vec<NodeId>>`, not `Vec<NodeId>`:
/// every gate's own row is the *set* of nodes a consumer must draw an edge
/// from *each* of to correctly read that gate's output -- a singleton set
/// for `Nor`/`Buf`, `Or`'s own branch sets (concatenated in declaration
/// order) for a merge. Consuming it is uniform either way: wherever this
/// module used to draw one edge from `output_of[g]`, it now draws one edge
/// from *every* element of `output_of[g]` -- multiple edges landing on the
/// same consumer node is exactly how this graph already represents fan-out
/// in the other direction (`a_shared_producer_fans_out_to_two_edges_from_
/// one_torch_node`), so a bare-OR's own multi-source join needs no new
/// graph machinery, only a producer side that can hand back more than one
/// node.
///
/// # Why gates are processed in topological order now
///
/// A `Nor`/`Buf` gate's own instantiation never depended on any other
/// gate's own result (its one Torch node exists regardless of what feeds
/// it), so the old version of this function could instantiate every gate
/// first and wire all the edges in a second, order-independent pass. A
/// merge's own `output_of` entry is built *from* its branches' own already-
/// resolved contributions, so a merge cannot be processed before every gate
/// it (transitively, through bare chains) reads from has been. `topological_
/// order` is exactly the order that guarantees that, and it is also
/// `expand`'s own re-check that the netlist has no cycle -- `compile`
/// already rejects one before this ever runs from inside it, but `expand`
/// re-derives it independently for the same reason it re-checks
/// `UndrivenSignal` (see `ExpandError::CyclicNetlist`'s own doc comment).
///
/// # Bare branches: no landing node, and so no input-wiring edge at all
///
/// For a `Nor`/`Buf` gate, "one edge per gate input" and "instantiating the
/// gate" are two separate steps because every input lands on the same fixed
/// Torch node regardless of which branch is bare or isolated -- a concept
/// that does not even apply to `Nor`. For a merge, whether a branch is bare
/// or isolated *is* the whole decision, and it is made once, per branch, via
/// [`branch_is_bare`] -- the exact same fanout rule `compile::merge_branch_
/// is_bare` uses (a branch is bare iff its own producer signal drives
/// nothing besides this merge), restated over `Netlist` directly since this
/// module never builds `compile`'s own `Net` structures. A bare branch gets
/// no node and no edge of its own at all: its producer's own contribution
/// set is spliced directly into this merge's own `output_of` entry, which is
/// the entire realisation (see `or_bare_entry`'s own doc comment: "no
/// primitive at all"). An isolated branch gets exactly one new
/// `IsolatingRepeater` node (owned by this gate, so it appears in `gate_
/// nodes[g]`), with one edge into it from *each* of the branch's own
/// producer's contributions -- exactly mirroring `place_merge_gate`'s real
/// isolation, where an isolated branch's repeater is the one thing standing
/// between a shared producer and the junction.
pub fn expand(netlist: &Netlist, library: &Library) -> Result<PrimitiveGraph, ExpandError> {
    expand_with_selection(netlist, library, &EntrySelection::new())
}

/// Rebuild the graph after changing one gate's library entry.  The caller
/// keeps the prior selections; all other gates retain their chosen/default
/// entry.  This deliberately validates topology only and never invokes the
/// legacy physical router.
pub fn reexpand_gate(
    netlist: &Netlist,
    library: &Library,
    selected_entries: &EntrySelection,
    gate: usize,
    entry: usize,
) -> Result<PrimitiveGraph, ExpandError> {
    let mut selected_entries = selected_entries.clone();
    selected_entries.insert(gate, entry);
    expand_with_selection(netlist, library, &selected_entries)
}

fn expand_with_selection(
    netlist: &Netlist,
    library: &Library,
    selected_entries: &EntrySelection,
) -> Result<PrimitiveGraph, ExpandError> {
    let mut graph = PrimitiveGraph::empty(netlist.gates.len());

    // One Lever node per primary input, keyed by name so gate inputs below
    // can look it up.
    let mut lever_of: HashMap<&str, NodeId> = HashMap::with_capacity(netlist.inputs.len());
    for name in &netlist.inputs {
        let id = graph.push_node(
            Primitive::Lever,
            Provenance::PrimaryInput { name: name.clone() },
        );
        lever_of.insert(name.as_str(), id);
    }

    let mut producer_of: HashMap<&str, usize> = HashMap::with_capacity(netlist.gates.len());
    for (g, gate) in netlist.gates.iter().enumerate() {
        producer_of.insert(gate.output.as_str(), g);
    }

    // signal name -> every gate index it feeds an input of (repeated once
    // per input, if it feeds the same gate more than once) -- `branch_is_
    // bare`'s whole basis, and nothing else in this function needs it.
    let mut consumers_of: HashMap<&str, Vec<usize>> = HashMap::new();
    for (g, gate) in netlist.gates.iter().enumerate() {
        for input in &gate.inputs {
            consumers_of.entry(input.as_str()).or_default().push(g);
        }
    }
    let branch_is_bare = |signal: &str, into_gate: usize| {
        consumers_of
            .get(signal)
            .is_some_and(|gs| gs.iter().all(|&g| g == into_gate))
    };

    let order = netlist
        .combinational_order()
        .ok_or(ExpandError::CyclicNetlist)?;

    // Every gate's own contribution set -- see this function's own doc
    // comment for why this is `Vec<NodeId>` per gate rather than one
    // `NodeId`. Built locally rather than straight into
    // `graph.output_nodes` only because `resolve_producer` reads it while
    // `graph` is being mutated; it is moved into the graph verbatim once the
    // loop is done (see `PrimitiveGraph::output_nodes`).
    let mut output_of: Vec<Vec<NodeId>> = vec![Vec::new(); netlist.gates.len()];
    // A DFF publishes Q before its D cone is processed.  Defer only the
    // external D/C attachments, so a legal q -> combinational logic -> D
    // feedback path can still resolve after all contributors exist.
    let mut pending_stateful_inputs: Vec<(usize, String, String)> = Vec::new();

    for g in order {
        let gate = &netlist.gates[g];
        let requested_entry = selected_entries.get(&g).copied();
        if let Some(entry) = requested_entry {
            if gate.kind != GateKind::DffPosedge && library.entry_at(gate.kind, entry).is_none() {
                return Err(ExpandError::UnknownLibraryEntry {
                    gate: gate.output.clone(),
                    entry,
                });
            }
        }

        if gate.kind == GateKind::DffPosedge {
            if !gate.kind.accepts_arity(gate.inputs.len()) {
                return Err(ExpandError::InvalidDffArity {
                    gate: gate.output.clone(),
                    arity: gate.inputs.len(),
                });
            }
            let entry =
                library
                    .stateful_entry(gate.kind)
                    .ok_or_else(|| ExpandError::NoLibraryEntry {
                        gate: gate.output.clone(),
                        arity: gate.inputs.len(),
                    })?;
            let region = instantiate_stateful(&mut graph, g, entry);
            graph.gate_nodes[g] = region.primitives.clone();
            output_of[g] = vec![region.q_contributor];
            let region_index = graph.stateful_regions.len();
            graph.stateful_regions.push(region);
            pending_stateful_inputs.push((
                region_index,
                gate.inputs[0].clone(),
                gate.inputs[1].clone(),
            ));
        } else if gate.is_merge() {
            let selected = requested_entry.and_then(|entry| library.entry_at(gate.kind, entry));
            if let Some(entry) = requested_entry {
                if selected.is_none() {
                    return Err(ExpandError::UnknownLibraryEntry {
                        gate: gate.output.clone(),
                        entry,
                    });
                }
            }
            let force_bare = selected.is_some_and(|entry| entry.template.nodes.is_empty());
            let force_isolated = selected.is_some_and(|entry| !entry.template.nodes.is_empty());
            if force_bare && gate.inputs.iter().any(|input| !branch_is_bare(input, g)) {
                return Err(ExpandError::IllegalLibraryEntry {
                    gate: gate.output.clone(),
                    entry: requested_entry.expect("bare entry was requested"),
                    reason: "a bare merge may not consume a signal with another consumer"
                        .to_string(),
                });
            }
            let mut contributions: Vec<NodeId> = Vec::new();
            let mut owned_nodes: Vec<NodeId> = Vec::new();

            for (index, input_name) in gate.inputs.iter().enumerate() {
                let producer = resolve_producer(input_name, &lever_of, &producer_of, &output_of)
                    .ok_or_else(|| ExpandError::UndrivenSignal(input_name.clone()))?;

                if !force_isolated && branch_is_bare(input_name, g) {
                    contributions.extend(producer);
                } else {
                    let role = TemplateNode::IsolatingRepeater(index);
                    let repeater =
                        graph.push_node(Primitive::Repeater, Provenance::Gate { gate: g, role });
                    for from in producer {
                        graph.push_edge(from, repeater);
                    }
                    contributions.push(repeater);
                    owned_nodes.push(repeater);
                }
            }

            graph.gate_nodes[g] = owned_nodes;
            output_of[g] = contributions;
        } else {
            if gate.kind != GateKind::Nor(gate.inputs.len()) {
                return Err(ExpandError::NotRealisable {
                    gate: gate.output.clone(),
                    kind: gate.kind,
                });
            }
            let arity = gate.inputs.len();
            let kind = GateKind::Nor(arity);
            let entry = requested_entry
                .and_then(|entry| library.entry_at(kind, entry))
                .or_else(|| library.choose(kind))
                .ok_or_else(|| ExpandError::NoLibraryEntry {
                    gate: gate.output.clone(),
                    arity,
                })?;
            let (output, input_targets) = instantiate(&mut graph, g, entry);

            for (index, input_name) in gate.inputs.iter().enumerate() {
                let producer = resolve_producer(input_name, &lever_of, &producer_of, &output_of)
                    .ok_or_else(|| ExpandError::UndrivenSignal(input_name.clone()))?;
                for from in producer {
                    graph.push_edge(from, input_targets[index]);
                }
            }

            output_of[g] = vec![output];
        }
    }

    for (region_index, data_input, clock_input) in pending_stateful_inputs {
        let (d_landing, clock_landing, inverted_clock) = {
            let region = &graph.stateful_regions[region_index];
            (
                region.d_landing,
                region.clock_landing,
                region
                    .node_for(StatefulPrimitiveRole::InvC)
                    .expect("Design H includes INV_C"),
            )
        };
        let data_producers = resolve_producer(&data_input, &lever_of, &producer_of, &output_of)
            .ok_or_else(|| ExpandError::UndrivenSignal(data_input.clone()))?;
        let clock_producers = resolve_producer(&clock_input, &lever_of, &producer_of, &output_of)
            .ok_or_else(|| ExpandError::UndrivenSignal(clock_input.clone()))?;

        for from in data_producers {
            let edge = graph.push_edge(from, d_landing);
            graph.stateful_regions[region_index]
                .ordinary_edges
                .push(edge);
        }
        for from in clock_producers {
            let direct_clock_edge = graph.push_edge(from, clock_landing);
            let inverted_clock_edge = graph.push_edge(from, inverted_clock);
            graph.stateful_regions[region_index]
                .ordinary_edges
                .extend([direct_clock_edge, inverted_clock_edge]);
        }
    }

    graph.output_nodes = output_of.clone();

    // One edge per declared output, from *every* one of its driving gate's
    // own contributions to a fresh Lamp node.
    for output_name in &netlist.outputs {
        let &g = producer_of
            .get(output_name.as_str())
            .ok_or_else(|| ExpandError::UndrivenSignal(output_name.clone()))?;
        let lamp = graph.push_node(
            Primitive::Lamp,
            Provenance::PrimaryOutput {
                name: output_name.clone(),
            },
        );
        for &from in &output_of[g] {
            graph.push_edge(from, lamp);
        }
    }

    Ok(graph)
}

/// The set of nodes `signal` contributes -- a primary input's own lever (a
/// singleton), or a gate's own `output_of` entry (which may hold more than
/// one node; see [`expand`]'s own doc comment for why).
fn resolve_producer(
    signal: &str,
    lever_of: &HashMap<&str, NodeId>,
    producer_of: &HashMap<&str, usize>,
    output_of: &[Vec<NodeId>],
) -> Option<Vec<NodeId>> {
    if let Some(&lever) = lever_of.get(signal) {
        return Some(vec![lever]);
    }
    producer_of.get(signal).map(|&g| output_of[g].clone())
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
            kind: GateKind::Nor(inputs.len()),
        }
    }

    fn merge(output: &str, inputs: &[&str]) -> Gate {
        Gate {
            kind: GateKind::Or(inputs.len()),
            ..gate(output, inputs)
        }
    }

    fn dff(output: &str, data: &str, clock: &str) -> Gate {
        Gate {
            name: output.to_string(),
            inputs: vec![data.to_string(), clock.to_string()],
            output: output.to_string(),
            kind: GateKind::DffPosedge,
        }
    }

    #[test]
    fn reexpanding_a_shared_merge_as_bare_is_rejected() {
        let netlist = Netlist {
            inputs: vec!["a".to_string(), "b".to_string()],
            outputs: vec!["merged".to_string(), "other".to_string()],
            gates: vec![merge("merged", &["a", "b"]), gate("other", &["a"])],
        };
        let library = Library::default_library();

        assert!(matches!(
            reexpand_gate(&netlist, &library, &EntrySelection::new(), 0, 0),
            Err(ExpandError::IllegalLibraryEntry { .. })
        ));
    }

    #[test]
    fn reexpanding_a_nor_with_an_unknown_entry_is_rejected() {
        let netlist = Netlist {
            inputs: vec!["a".to_string()],
            outputs: vec!["y".to_string()],
            gates: vec![gate("y", &["a"])],
        };
        let library = Library::default_library();

        assert!(matches!(
            reexpand_gate(&netlist, &library, &EntrySelection::new(), 0, 1),
            Err(ExpandError::UnknownLibraryEntry { .. })
        ));
    }

    #[test]
    fn primitive_graph_accepts_design_h_dff() {
        // d = !q; q captures d on the clock edge.  The feedback is legal
        // because it crosses the DFF's state boundary, not a combinational
        // loop.
        let netlist = Netlist {
            inputs: vec!["clk".to_string()],
            outputs: vec!["q".to_string()],
            gates: vec![gate("d", &["q"]), dff("q", "d", "clk")],
        };
        let graph = expand(&netlist, &Library::default_library())
            .expect("a Design H DFF is a realisable stateful region");

        let region = graph
            .stateful_regions
            .iter()
            .find(|region| region.gate == 1)
            .expect("DFF region");
        assert_eq!(
            graph.nodes[region.d_landing].stateful_role,
            Some(StatefulPrimitiveRole::MData)
        );
        assert_eq!(
            graph.nodes[region.clock_landing].stateful_role,
            Some(StatefulPrimitiveRole::MLock)
        );
        assert_eq!(
            graph.nodes[region.q_contributor].stateful_role,
            Some(StatefulPrimitiveRole::SData)
        );
        assert_eq!(
            region.primitives.len(),
            5,
            "Design H owns four repeaters and INV_C"
        );

        for (role, primitive) in [
            (StatefulPrimitiveRole::MData, Primitive::Repeater),
            (StatefulPrimitiveRole::SData, Primitive::Repeater),
            (StatefulPrimitiveRole::MLock, Primitive::Repeater),
            (StatefulPrimitiveRole::InvC, Primitive::Torch),
            (StatefulPrimitiveRole::SLock, Primitive::Repeater),
        ] {
            let node = region
                .primitives
                .iter()
                .copied()
                .find(|&node| graph.nodes[node].stateful_role == Some(role))
                .unwrap_or_else(|| panic!("missing {role:?}"));
            assert_eq!(graph.nodes[node].primitive, primitive, "{role:?}");
        }

        let m_data = region
            .node_for(StatefulPrimitiveRole::MData)
            .expect("M_DATA");
        let s_data = region
            .node_for(StatefulPrimitiveRole::SData)
            .expect("S_DATA");
        let m_lock = region
            .node_for(StatefulPrimitiveRole::MLock)
            .expect("M_LOCK");
        let inv_c = region.node_for(StatefulPrimitiveRole::InvC).expect("INV_C");
        let s_lock = region
            .node_for(StatefulPrimitiveRole::SLock)
            .expect("S_LOCK");
        let d_source = graph.gate_nodes[0][0];
        let clock_source = graph
            .nodes
            .iter()
            .position(|node| matches!(&node.provenance, Provenance::PrimaryInput { name } if name == "clk"))
            .expect("clock lever");
        assert!(
            graph.edges.iter().any(|edge| edge.kind == EdgeKind::Signal
                && edge.from == d_source
                && edge.to == m_data),
            "D feeds M_DATA"
        );
        assert!(
            graph.edges.iter().any(|edge| edge.kind == EdgeKind::Signal
                && edge.from == m_data
                && edge.to == s_data),
            "M_DATA feeds S_DATA"
        );
        assert!(
            graph.edges.iter().any(|edge| edge.kind == EdgeKind::Signal
                && edge.from == region.q_contributor
                && edge.to == d_source),
            "Q feeds d"
        );
        assert!(
            graph.edges.iter().any(|edge| edge.kind == EdgeKind::Signal
                && edge.from == clock_source
                && edge.to == m_lock),
            "C feeds M_LOCK"
        );
        assert!(
            graph.edges.iter().any(|edge| edge.kind == EdgeKind::Signal
                && edge.from == clock_source
                && edge.to == inv_c),
            "C feeds INV_C"
        );
        assert!(
            graph.edges.iter().any(|edge| edge.kind == EdgeKind::Signal
                && edge.from == inv_c
                && edge.to == s_lock),
            "INV_C feeds S_LOCK"
        );
        assert_eq!(
            graph
                .edges
                .iter()
                .filter(|edge| edge.kind == EdgeKind::RepeaterLockSide)
                .map(|edge| (edge.from, edge.to))
                .collect::<Vec<_>>(),
            vec![(m_lock, m_data), (s_lock, s_data)],
            "only the two repeater side-lock relations are typed control edges"
        );
        assert_eq!(
            region.lock_side_relations.len(),
            2,
            "the region retains both typed control relations"
        );
        assert!(region
            .lock_side_relations
            .iter()
            .all(|&edge| graph.edges[edge].kind == EdgeKind::RepeaterLockSide));
        assert!(
            !graph.edges.iter().any(|edge| {
                edge.kind == EdgeKind::Signal
                    && ((edge.from == m_lock && edge.to == m_data)
                        || (edge.from == s_lock && edge.to == s_data))
            }),
            "lock controls must never masquerade as ordinary rear-data signal edges"
        );
    }

    #[test]
    fn expand_rejects_a_one_input_dff_instead_of_panicking() {
        let netlist = Netlist {
            inputs: vec!["data".to_string()],
            outputs: vec![],
            gates: vec![Gate {
                name: "q".to_string(),
                inputs: vec!["data".to_string()],
                output: "q".to_string(),
                kind: GateKind::DffPosedge,
            }],
        };

        assert_eq!(
            expand(&netlist, &Library::default_library())
                .expect_err("a DFF without a clock must be rejected"),
            ExpandError::InvalidDffArity {
                gate: "q".to_string(),
                arity: 1,
            }
        );
    }

    #[test]
    fn primitive_graph_rejects_combinational_cycle() {
        let netlist = Netlist {
            inputs: vec![],
            outputs: vec!["a".to_string()],
            gates: vec![gate("a", &["b"]), gate("b", &["a"])],
        };

        assert_eq!(
            expand(&netlist, &Library::default_library())
                .expect_err("pure combinational feedback is illegal"),
            ExpandError::CyclicNetlist
        );
    }

    /// For a NOR gate, "which nodes does this gate own" and "which nodes
    /// does a consumer read it from" are the same single torch -- so
    /// `output_nodes` adds nothing for a NOR-only netlist, and says so.
    #[test]
    fn a_nor_gates_output_nodes_row_is_exactly_its_own_torch() {
        let netlist = Netlist {
            inputs: vec!["a".to_string()],
            outputs: vec!["g0".to_string()],
            gates: vec![gate("g0", &["a"])],
        };
        let graph = expand(&netlist, &Library::default_library()).expect("valid netlist");
        assert_eq!(graph.output_nodes, graph.gate_nodes);
        assert_eq!(graph.output_nodes[0].len(), 1);
        assert_eq!(
            graph.nodes[graph.output_nodes[0][0]].primitive,
            Primitive::Torch
        );
    }

    /// A merge whose every branch is bare owns no primitive at all, so
    /// `gate_nodes` is empty for it -- and `output_nodes` is what still
    /// answers "where is this gate": the set of producer nodes whose dust
    /// joins to form its net. A viewer with only `gate_nodes` would have
    /// nothing whatsoever to draw for such a gate.
    #[test]
    fn an_all_bare_merge_owns_no_nodes_but_still_names_the_ones_its_dust_joins() {
        // g0 = NOR(a); g1 = NOR(b); m = g0 | g1 -- neither g0 nor g1 drives
        // anything besides `m`, so both branches are bare.
        let netlist = Netlist {
            inputs: vec!["a".to_string(), "b".to_string()],
            outputs: vec!["m".to_string()],
            gates: vec![
                gate("g0", &["a"]),
                gate("g1", &["b"]),
                merge("m", &["g0", "g1"]),
            ],
        };
        let graph = expand(&netlist, &Library::default_library()).expect("valid netlist");

        assert!(
            graph.gate_nodes[2].is_empty(),
            "an all-bare merge instantiates no primitive"
        );
        assert_eq!(
            graph.output_nodes[2],
            vec![graph.gate_nodes[0][0], graph.gate_nodes[1][0]],
            "the merge's net is exactly its two branches' own torches, in branch order"
        );
    }

    /// An isolated branch's repeater *is* owned by the merge, and is also
    /// the node that branch contributes -- so the two lists agree there
    /// while still disagreeing about the bare branch beside it.
    #[test]
    fn an_isolated_branch_contributes_the_repeater_the_merge_owns() {
        // g0 fans out to both `m` and `g2`, so `m`'s first branch is not
        // bare and gets an isolating repeater; g1 feeds only `m`, so its
        // branch is bare.
        let netlist = Netlist {
            inputs: vec!["a".to_string(), "b".to_string()],
            outputs: vec!["m".to_string(), "g2".to_string()],
            gates: vec![
                gate("g0", &["a"]),
                gate("g1", &["b"]),
                merge("m", &["g0", "g1"]),
                gate("g2", &["g0"]),
            ],
        };
        let graph = expand(&netlist, &Library::default_library()).expect("valid netlist");

        assert_eq!(
            graph.gate_nodes[2].len(),
            1,
            "only the isolated branch's repeater belongs to the merge"
        );
        let repeater = graph.gate_nodes[2][0];
        assert_eq!(graph.nodes[repeater].primitive, Primitive::Repeater);
        assert_eq!(
            graph.output_nodes[2],
            vec![repeater, graph.gate_nodes[1][0]],
            "the isolated branch contributes its repeater, the bare one its producer's torch"
        );
    }

    #[test]
    fn expanding_a_single_not_gate_yields_one_lever_one_torch_one_lamp_and_two_edges() {
        let netlist = Netlist {
            inputs: vec!["a".to_string()],
            outputs: vec!["g0".to_string()],
            gates: vec![gate("g0", &["a"])],
        };
        let library = Library::default_library();
        let graph = expand(&netlist, &library).expect("a 1-input NOR has a library entry");

        let primitives: Vec<Primitive> = graph.nodes.iter().map(|n| n.primitive).collect();
        assert_eq!(
            primitives
                .iter()
                .filter(|&&p| p == Primitive::Lever)
                .count(),
            1
        );
        assert_eq!(
            primitives
                .iter()
                .filter(|&&p| p == Primitive::Torch)
                .count(),
            1
        );
        assert_eq!(
            primitives.iter().filter(|&&p| p == Primitive::Lamp).count(),
            1
        );
        assert_eq!(
            graph.nodes.len(),
            3,
            "no Support, no per-input Repeater any more"
        );

        // Lever->Torch, Torch->Lamp: exactly the netlist's own two edges,
        // nothing else.
        assert_eq!(graph.edges.len(), 2);

        assert_eq!(graph.gate_nodes.len(), 1);
        assert_eq!(
            graph.gate_nodes[0].len(),
            1,
            "a NOR gate is one node: its torch"
        );
    }

    #[test]
    fn a_shared_producer_fans_out_to_two_edges_from_one_torch_node() {
        // g0 = NOR(a); g1 = NOR(g0); g2 = NOR(g0, a) -- g0's torch feeds two
        // consumers, so it should be exactly one node with two outgoing
        // edges, not two copies of it.
        let netlist = Netlist {
            inputs: vec!["a".to_string()],
            outputs: vec!["g1".to_string(), "g2".to_string()],
            gates: vec![
                gate("g0", &["a"]),
                gate("g1", &["g0"]),
                gate("g2", &["g0", "a"]),
            ],
        };
        let library = Library::default_library();
        let graph = expand(&netlist, &library).expect("every gate here has 1 or 2 inputs");

        let g0_torch = graph.gate_nodes[0][0];
        assert!(
            matches!(
                &graph.nodes[g0_torch].provenance,
                Provenance::Gate {
                    gate: 0,
                    role: TemplateNode::Torch
                }
            ),
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
        let netlist = Netlist {
            inputs: vec!["a".to_string()],
            outputs: vec![],
            gates: vec![gate("g0", &["a", "b", "c", "d"])],
        };
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
        assert_eq!(
            err,
            ExpandError::NoLibraryEntry {
                gate: "g0".to_string(),
                arity: 4
            }
        );
    }

    #[test]
    fn expand_rejects_an_undriven_input() {
        let netlist = Netlist {
            inputs: vec![],
            outputs: vec![],
            gates: vec![gate("g0", &["nowhere"])],
        };
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
            gates: vec![
                gate("g0", &["a"]),
                gate("g1", &["b"]),
                gate("g2", &["g0", "g1"]),
            ],
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
