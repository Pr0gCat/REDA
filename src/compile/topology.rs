//! The topology library: gate type -> a graph of primitives, connectivity
//! only.
//!
//! See `docs/superpowers/specs/2026-08-08-primitive-level-flow.md`, "The
//! topology library" and "Topology carries no positions" for the design this
//! module implements. Two things live here:
//!
//! - [`Primitive`], the vocabulary every entry (and the flat graph
//!   `primitive_graph` expands into) is built from.
//! - [`Library`], a gate type -> [`Template`] table, written once and
//!   consulted by `primitive_graph::expand`, never derived per gate.
//!
//! # The governing rule: topology describes signal flow, and nothing else
//!
//! An earlier version of this module gave every NOR entry a `Support` node
//! (a block) and one `Repeater` node per input, reading `place_nor_gate`'s
//! own physical design back as a connectivity graph. That was a mistake this
//! module no longer makes, for reasons worth stating plainly since the fix
//! is a *smaller* graph, not a bigger one:
//!
//! - **A support block is not part of the signal.** It is how an input
//!   physically reaches a torch -- realisation, not topology. A torch and
//!   the block it is mounted on are one thing at this level.
//! - **The repeaters at gate input sockets are not part of a NOR.** A NOR
//!   does not need a repeater to be a NOR; those exist only because of how
//!   the current emitter terminates a route. They are realisation too.
//!
//! So a NOR of arity `n` is **one node** -- a torch -- with `n` inbound
//! signal edges and one outbound (`primitive_graph::expand` wires those
//! edges; `nor_entry` below just says "one torch, and every input lands on
//! it").
//!
//! One consequence follows directly, and is correct rather than a shortfall:
//! for a NOR-only netlist, the flat primitive graph is isomorphic to the
//! netlist itself -- this layer adds no information for the circuits this
//! compiler builds today. It earns its place the day the library holds an
//! entry that is *not* one-to-one with its gate: a deliberate repeater
//! delay, a repeater latch, a comparator-based subtractor, diode isolation
//! chosen as topology rather than falling out of routing. [`Template`]'s
//! shape (a small node list plus internal edges, an output node and a list
//! of input-landing nodes) is built to hold exactly those without changing
//! again -- see its own doc comment.

use std::collections::BTreeMap;

// ---------------------------------------------------------------------
// The primitive vocabulary
// ---------------------------------------------------------------------

/// The kind of thing a node in a [`Template`], or in the flat
/// `primitive_graph::PrimitiveGraph` a netlist expands into, physically is.
///
/// Every variant here **carries signal** -- has a function, a direction, or
/// is where a signal originates or terminates. That is the one test a
/// candidate primitive has to pass to belong in this enum, per the spec's
/// "topology describes signal flow, and nothing else":
///
/// - `Torch` -- the only element with a function: dark when its support is
///   powered. A NOR gate's realisation.
/// - `Repeater` -- restores strength, costs a tick, one-way. Not used by any
///   entry this module ships today (a NOR's own input sockets are
///   realisation, not topology -- see this module's doc comment), but it is
///   a functional, directional signal element in its own right, exactly the
///   kind of thing a deliberate-delay or latch entry would place as an
///   internal `Template` node, so it belongs to the vocabulary now rather
///   than being bolted on later.
/// - `Comparator` -- compares or subtracts two inputs, one-way. Also unused
///   today; also a real functional element a future entry (a
///   comparator-based subtractor, the spec's own example) would need, and
///   for the same reason as `Repeater` it belongs to the vocabulary rather
///   than to a later, larger change.
/// - `Lever` -- a primary input's switch: the source of a signal, never a
///   sink.
/// - `Lamp` -- a declared output's readable indicator: the sink of a signal,
///   never a source.
///
/// Two things this project's earlier draft of this enum had are deliberately
/// gone:
///
/// - **`Block`** (a support). A support is how an input physically reaches a
///   torch, not part of the signal itself -- realisation, out of scope here.
/// - **`Dust`**. Dust carries a strength that decays with distance, but it
///   has no function of its own -- it is the *medium* a signal travels
///   through, which makes it an edge's realisation, never a vertex. See the
///   spec: "Dust is not a node... it is the medium -- an edge, not a
///   vertex."
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Primitive {
    /// The only element with a function: dark when its support is powered.
    Torch,
    /// Restores strength, costs a tick, one-way. Unused by every entry this
    /// module ships (see this type's own doc comment) until a technique
    /// wants a repeater as *topology* rather than as routing.
    Repeater,
    /// Compares or subtracts two inputs, one-way. Unused today; reserved for
    /// a future comparator-based entry (see this type's own doc comment).
    Comparator,
    /// A primary input's switch -- the source of a signal, never the target
    /// of one.
    Lever,
    /// A declared output's readable indicator -- the sink of a signal, never
    /// its source.
    Lamp,
}

// ---------------------------------------------------------------------
// Gate kinds and templates
// ---------------------------------------------------------------------

/// Which gate a [`Library`] entry realises.
///
/// This codebase has exactly one logic family -- NOR, with fan-in 1..=3
/// (`place_nor_gate`'s own hardware ceiling; `NetlistBuilder::nor` and the
/// Yosys frontend's genlib both enforce it before a `Gate` is ever built) --
/// so a gate's "kind" is fully described by its arity. `Nor(1)` is the same
/// gate the spec calls `NOT` (a NOR gate with one input inverts exactly like
/// an inverter); [`GateKind::NOT`] names that case for readability at call
/// sites that mean it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GateKind {
    Nor(usize),
}

impl GateKind {
    /// `NOR` with one input -- an inverter. See the spec's "The topology
    /// library": "`NOT` maps to `input — torch — output`".
    pub const NOT: GateKind = GateKind::Nor(1);
}

/// A symbolic node inside one [`Template`] -- turned into a real, numbered
/// `primitive_graph::NodeId` once for every gate a `Library` entry expands
/// (see `primitive_graph::expand`'s `instantiate`).
///
/// One variant today: every entry this module ships needs exactly one node,
/// so `Torch` is the only role there is anything to name. A future entry
/// that needs more than one of its own internal nodes (a delay repeater in
/// series with its torch, say) adds a variant here for that node -- a change
/// of library data and this enum's own size, never of `Template`'s shape or
/// of `primitive_graph::expand`, which only ever looks up whatever
/// `Template::output` and `Template::inputs` name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TemplateNode {
    /// This gate's output. Reading its `lit` state is reading the gate's
    /// output, exactly as `place_nor_gate`'s doc comment already puts it.
    Torch,
}

/// A relational preference between two of a template's own nodes --
/// "these two want to be on opposite sides", "this one wants to be coplanar
/// with that one" -- read by the planner (not built yet) as a soft term,
/// never a coordinate. See the spec's "Choosing topologies, and the loop
/// that closes".
///
/// No entry in [`nor_entry`] populates this yet -- a NOR gate's inputs are
/// interchangeable, so today's only technique has no preference to express.
/// The type exists so a future entry (a different technique for the same
/// [`GateKind`], added purely as library data) can carry one without
/// `Template`'s shape changing to accommodate it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbeddingHint {
    /// The two nodes should end up on opposite sides of whatever they are
    /// both connected to.
    OppositeSides(TemplateNode, TemplateNode),
    /// The two nodes should end up coplanar (sharing one axis) with each
    /// other.
    Coplanar(TemplateNode, TemplateNode),
}

/// One gate technique's connectivity graph: which primitives it needs, how
/// they connect to each other, and where its own inputs and output attach.
/// Carries no positions, no faces, no orientation, and -- since "there are
/// no rigid edges here" (the spec's own correction: rigidity is a
/// realisation-time concept, not a topology one) -- no distinction between
/// edge kinds either. See this module's doc comment, and the spec's
/// "Topology carries no positions" / "Nothing physical lives here either".
///
/// `nodes` and `internal_edges` are what let this shape hold a future
/// technique that is *not* one-to-one with its gate (a delay repeater in
/// series, say: two nodes, one internal edge from the input-landing node to
/// the torch). Every entry this module ships today has one node and no
/// internal edges -- see this module's doc comment for why that is the
/// correct state of the library now, not a placeholder for something
/// missing.
pub struct Template {
    /// Every node this entry's graph has, and the primitive kind it will be
    /// realised as. A `Vec`, not a set, so `expand` instantiates them in one
    /// deterministic order.
    pub nodes: Vec<(TemplateNode, Primitive)>,
    /// Directed signal-flow edges between two of this entry's own nodes --
    /// always between two `nodes` of the *same* entry. Empty for every entry
    /// this module ships (a single-node entry has nothing to connect
    /// internally); a multi-node technique (a delay repeater in series with
    /// its torch) would use this for the edge between them.
    pub internal_edges: Vec<(TemplateNode, TemplateNode)>,
    /// Which node this entry's `i`-th declared input's signal edge lands on,
    /// in order (`inputs.len()` is this entry's arity). Every entry this
    /// module ships names the same node (`Torch`) for every index, because a
    /// NOR's inputs are interchangeable and land directly on the one
    /// functional element there is.
    pub inputs: Vec<TemplateNode>,
    /// Which node this gate's own outbound signal edge (to whatever it
    /// drives, or to a declared output's lamp) originates from.
    pub output: TemplateNode,
    /// See [`EmbeddingHint`]. Empty for every entry this module ships.
    pub embedding_hints: Vec<EmbeddingHint>,
}

/// One named technique for realising a [`GateKind`] in primitives.
pub struct LibraryEntry {
    /// Identifies the technique, for diagnostics -- not consulted by
    /// `expand`, which only ever looks at `template`.
    pub name: &'static str,
    pub template: Template,
}

/// The gate-topology library: [`GateKind`] -> every [`LibraryEntry`] known
/// for it. Written once here and consulted by `primitive_graph::expand`, per
/// the spec: "It is written once and consulted, not derived per gate."
///
/// Every `GateKind` this module populates maps to exactly one entry today --
/// but the table's *shape* (`GateKind -> Vec<LibraryEntry>`) already admits
/// more, because "the library admits alternatives" is the one property the
/// spec asks this step to guarantee architecturally, independent of how many
/// alternatives actually exist yet.
pub struct Library {
    entries: BTreeMap<GateKind, Vec<LibraryEntry>>,
}

impl Library {
    /// Build a library from an explicit `GateKind -> Vec<LibraryEntry>`
    /// table. Exposed mainly so tests can build a deliberately incomplete
    /// library (e.g. to exercise `primitive_graph::ExpandError`); real
    /// callers want [`Library::default_library`].
    pub fn new(entries: BTreeMap<GateKind, Vec<LibraryEntry>>) -> Self {
        Library { entries }
    }

    /// Today's library: one entry per NOR arity this compiler ever places
    /// (1..=3 -- see [`GateKind`]'s doc comment for why fan-in never
    /// exceeds 3).
    pub fn default_library() -> Self {
        let mut entries = BTreeMap::new();
        for arity in 1..=3 {
            entries.insert(GateKind::Nor(arity), vec![nor_entry(arity)]);
        }
        Library { entries }
    }

    /// Every technique known for `kind`, in the order they were registered.
    /// Empty (not absent) for a `kind` this library has never heard of.
    pub fn entries_for(&self, kind: GateKind) -> &[LibraryEntry] {
        self.entries.get(&kind).map(Vec::as_slice).unwrap_or(&[])
    }

    /// The technique `primitive_graph::expand` should use for `kind` today.
    ///
    /// Picks the first registered entry, unconditionally. The spec is
    /// explicit that this is allowed to be simplistic at first ("the first
    /// version may pick by rule") as long as choosing between alternatives
    /// later is a change of *policy* -- swapping this method's body for one
    /// that consults where a gate's neighbours actually landed -- not a
    /// change of `Library`'s shape. `None` iff `kind` has no entry at all.
    pub fn choose(&self, kind: GateKind) -> Option<&LibraryEntry> {
        self.entries_for(kind).first()
    }
}

/// The one technique this library ships for an `arity`-input NOR: a single
/// torch, with every one of its `arity` inputs landing directly on it and
/// its own output originating from it too. This is deliberately *not* a
/// reading-back of `place_nor_gate`'s physical design (that design's support
/// block and its per-input repeaters are realisation -- see this module's
/// doc comment) -- it is the gate's signal flow and nothing else: `n`
/// inbound edges, one outbound.
fn nor_entry(arity: usize) -> LibraryEntry {
    assert!((1..=3).contains(&arity), "a NOR gate's fan-in is 1..=3, got {arity}");

    let name = match arity {
        1 => "torch-nor1 (not)",
        2 => "torch-nor2",
        3 => "torch-nor3",
        _ => unreachable!("checked by the assert above"),
    };

    LibraryEntry {
        name,
        template: Template {
            nodes: vec![(TemplateNode::Torch, Primitive::Torch)],
            internal_edges: Vec::new(),
            inputs: vec![TemplateNode::Torch; arity],
            output: TemplateNode::Torch,
            embedding_hints: Vec::new(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_library_has_exactly_one_entry_per_nor_arity_one_to_three() {
        let library = Library::default_library();
        for arity in 1..=3 {
            let entries = library.entries_for(GateKind::Nor(arity));
            assert_eq!(entries.len(), 1, "arity {arity} should have exactly one entry");
        }
        assert!(library.entries_for(GateKind::Nor(4)).is_empty(), "fan-in 4 has no entry -- it is not hardware");
    }

    #[test]
    fn not_is_nor_of_one() {
        assert_eq!(GateKind::NOT, GateKind::Nor(1));
    }

    #[test]
    fn every_nor_entry_is_a_single_torch_node_with_no_internal_edges() {
        for arity in 1..=3 {
            let entry = nor_entry(arity);
            assert_eq!(entry.template.nodes, vec![(TemplateNode::Torch, Primitive::Torch)], "arity {arity}");
            assert!(entry.template.internal_edges.is_empty(), "arity {arity}: a single-node entry has nothing to connect internally");
            assert_eq!(entry.template.output, TemplateNode::Torch, "arity {arity}");
        }
    }

    #[test]
    fn every_nor_entry_lands_all_of_its_arity_many_inputs_on_the_torch() {
        for arity in 1..=3 {
            let entry = nor_entry(arity);
            assert_eq!(entry.template.inputs.len(), arity, "arity {arity}");
            assert!(entry.template.inputs.iter().all(|&role| role == TemplateNode::Torch), "arity {arity}");
        }
    }

    #[test]
    fn choose_picks_the_first_registered_entry() {
        let library = Library::default_library();
        let chosen = library.choose(GateKind::Nor(2)).expect("NOR2 has an entry");
        assert_eq!(chosen.name, "torch-nor2");
    }

    #[test]
    fn choose_returns_none_for_an_unregistered_gate_kind() {
        let library = Library::default_library();
        assert!(library.choose(GateKind::Nor(99)).is_none());
    }
}
