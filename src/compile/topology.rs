//! The topology library: gate type -> a graph of primitives, connectivity
//! only.
//!
//! See `docs/superpowers/specs/2026-08-08-primitive-level-flow.md`, "The
//! topology library" and "Topology carries no positions" for the design this
//! module implements. Two things live here:
//!
//! - [`Primitive`], the vocabulary every entry (and the flat graph
//!   `primitive_graph` expands into) is built from -- torch, dust, repeater,
//!   solid block, plus the two circuit-boundary elements (lever, lamp) a
//!   whole-circuit expansion needs.
//! - [`Library`], a gate type -> [`Template`] table, written once and
//!   consulted by `primitive_graph::expand`, never derived per gate.
//!
//! # Why every entry still has a `Support` node, even though "no support
//! blocks" is one of this step's own constraints
//!
//! The spec's own illustrative example for `NOT` (`input — torch — output`)
//! never mentions a support at all. What that elides -- and what "no support
//! blocks" actually rules out -- is which *block*, at which *coordinate*, on
//! which *face* realizes the support, not whether a support relationship
//! exists in the first place. It has to: `Primitive::Torch`'s own defining
//! behaviour ("dark when its support is powered") only makes sense if
//! something plays that role, and once more than one input can darken the
//! same torch, that something has to be a single shared node -- two inputs
//! that both merely "point at the torch" cannot express "either one is
//! enough" without a place for them to actually meet, since they generally
//! sit on unrelated cells that are not themselves adjacent to each other.
//! `Template::rigid_edges` records that merge point without ever naming its
//! coordinates or which of the support's faces a given input lands on --
//! both stay the planner's to choose, exactly as the spec requires.

use std::collections::BTreeMap;

// ---------------------------------------------------------------------
// The primitive vocabulary
// ---------------------------------------------------------------------

/// The kind of thing a node in a [`Template`], or in the flat
/// `primitive_graph::PrimitiveGraph` a netlist expands into, physically is.
///
/// `Torch`, `Dust`, `Repeater` and `Block` are "the things the simulator
/// actually models" (the spec's "The primitive level") -- everything a gate
/// topology is built from. `Dust` is part of that vocabulary but never
/// appears as a template node in this version of the library: a routable
/// edge (see `primitive_graph::EdgeKind::Routable`) *becomes* a dust/repeater
/// chain only once a planner decides how long that chain is and where it
/// bends, which is later work (`Order` step 2 onward in the spec) -- nothing
/// at this level ever needs to say "and here is a dust cell" itself.
///
/// `Lever` and `Lamp` are not part of any gate's topology -- no [`Library`]
/// entry ever mentions them -- but expanding a whole `Netlist` needs
/// something to stand for a primary input and a declared output, and those
/// are exactly what `compile` already places there today, unconditionally,
/// for every circuit. Naming them with the same enum keeps the flat graph in
/// one vocabulary instead of inventing a separate "boundary primitive" type
/// that would only ever hold two variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Primitive {
    /// The only element with a function: dark when its support is powered.
    Torch,
    /// Carries a strength that decays with distance. Not yet produced by
    /// this module's expansion -- see the note above.
    Dust,
    /// Restores strength, costs a tick, one-way.
    Repeater,
    /// Carries power to what touches it, supports what stands on it. Used
    /// here as a torch's shared support/merge point.
    Block,
    /// A primary input's switch -- the source of a routable edge, never the
    /// target of a rigid one.
    Lever,
    /// A declared output's readable indicator -- the target of a rigid edge
    /// from its driving gate's torch (see `primitive_graph::expand`'s
    /// doc comment for why that relationship is rigid, not routable).
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
/// `Torch` and `Input(i)` are fixed roles every entry must use for its
/// output and its `i`-th declared input respectively -- `expand` looks
/// specifically for these two roles by name. `Support` (or any other
/// internal node a future entry might add) is opaque to `expand`; it exists
/// purely to carry this entry's own rigid edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TemplateNode {
    /// The shared merge point every input drives and the torch depends on.
    Support,
    /// This gate's output. Reading its `lit` state is reading the gate's
    /// output, exactly as `place_nor_gate`'s doc comment already puts it.
    Torch,
    /// The `i`-th declared input's own terminal -- where a routable edge
    /// from whatever drives that input must land.
    Input(usize),
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

/// One gate technique's connectivity graph: which primitives it needs, and
/// which pairs of them must become physical adjacency (`rigid_edges`) rather
/// than a dust path of any length. Carries no positions, no faces, no
/// orientation -- see this module's doc comment, and the spec's "Topology
/// carries no positions" / "Nothing physical lives here either".
pub struct Template {
    /// Every node this entry's graph has, and the primitive kind it will be
    /// realised as. A `Vec`, not a set, so `expand` instantiates them in one
    /// deterministic order.
    pub nodes: Vec<(TemplateNode, Primitive)>,
    /// Adjacency constraints between two of this entry's own nodes. Always
    /// between two `nodes` of the *same* entry -- an entry describes one
    /// gate's own internal structure, never what it connects to outside
    /// itself (that is `primitive_graph::expand`'s job, via routable edges).
    pub rigid_edges: Vec<(TemplateNode, TemplateNode)>,
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

/// The one technique this library ships for an `arity`-input NOR: every
/// input, and the output torch, rigidly share one support node. This is
/// `place_nor_gate`'s own physical design (a support block plus its output
/// torch, every input's route terminating directly against a free face of
/// that same block) read back as a connectivity graph, with the block's
/// coordinates, faces and orientation -- everything `place_nor_gate` also
/// decides -- left out, because none of that is topology.
fn nor_entry(arity: usize) -> LibraryEntry {
    assert!((1..=3).contains(&arity), "a NOR gate's fan-in is 1..=3, got {arity}");

    let mut nodes = vec![(TemplateNode::Support, Primitive::Block), (TemplateNode::Torch, Primitive::Torch)];
    let mut rigid_edges = vec![(TemplateNode::Torch, TemplateNode::Support)];
    for i in 0..arity {
        nodes.push((TemplateNode::Input(i), Primitive::Repeater));
        rigid_edges.push((TemplateNode::Input(i), TemplateNode::Support));
    }

    let name = match arity {
        1 => "torch-nor1 (not)",
        2 => "torch-nor2",
        3 => "torch-nor3",
        _ => unreachable!("checked by the assert above"),
    };

    LibraryEntry { name, template: Template { nodes, rigid_edges, embedding_hints: Vec::new() } }
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
    fn every_nor_entry_has_one_support_one_torch_and_arity_many_inputs_all_rigidly_adjacent_to_the_support() {
        for arity in 1..=3 {
            let entry = nor_entry(arity);
            let supports = entry.template.nodes.iter().filter(|&&(role, _)| role == TemplateNode::Support).count();
            let torches = entry.template.nodes.iter().filter(|&&(role, _)| role == TemplateNode::Torch).count();
            let inputs = entry
                .template
                .nodes
                .iter()
                .filter(|&&(role, _)| matches!(role, TemplateNode::Input(_)))
                .count();
            assert_eq!(supports, 1, "arity {arity}");
            assert_eq!(torches, 1, "arity {arity}");
            assert_eq!(inputs, arity);
            // Torch and every input each have exactly one rigid edge, and it
            // goes to Support.
            assert_eq!(entry.template.rigid_edges.len(), arity + 1, "arity {arity}");
            for &(a, b) in &entry.template.rigid_edges {
                assert_eq!(b, TemplateNode::Support, "every rigid edge in a NOR entry ends at Support");
                assert_ne!(a, TemplateNode::Support, "Support has no rigid edge to itself");
            }
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
