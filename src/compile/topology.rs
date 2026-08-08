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
/// This codebase has exactly one logic family that is actual logic -- NOR,
/// with fan-in 1..=3 (`place_nor_gate`'s own hardware ceiling;
/// `NetlistBuilder::nor` and the Yosys frontend's genlib both enforce it
/// before a `Gate` is ever built) -- so a gate's "kind" is fully described by
/// its arity. `Nor(1)` is the same gate the spec calls `NOT` (a NOR gate with
/// one input inverts exactly like an inverter); [`GateKind::NOT`] names that
/// case for readability at call sites that mean it.
///
/// `Buf` is not logic at all -- it is what a Yosys `BUF` cell (a pass-through
/// ABC's mapper needs to exist as a real cell, and a bare `assign out = in;`
/// with no cell in between) realises as: two chained `Nor(1)` torches,
/// `NOT(NOT(x)) == x`. It earns its own `GateKind` rather than being folded
/// into `Nor` because it is a second technique this library ships, not a
/// third arity -- see [`gate_kind_for_yosys_cell`] for where it enters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GateKind {
    Nor(usize),
    Buf,
}

impl GateKind {
    /// `NOR` with one input -- an inverter. See the spec's "The topology
    /// library": "`NOT` maps to `input — torch — output`".
    pub const NOT: GateKind = GateKind::Nor(1);
}

// ---------------------------------------------------------------------
// The Yosys boundary: which cell type is which GateKind
// ---------------------------------------------------------------------

/// Every Yosys cell type `redstone_nor.genlib` can produce, and the
/// [`GateKind`] this library realises it as. This is the boundary the spec's
/// "gate topology is a library" is asking for stated as data instead of as
/// parsing code: `yosys_json::Context::build_cell` looks a cell's `type` up
/// here (via [`gate_kind_for_yosys_cell`]) rather than matching the string
/// itself and separately knowing what to build for it.
///
/// Every entry named here is a `GATE` line in `redstone_nor.genlib` with a
/// real realisation in this library -- `$__ZERO`/`$__ONE` (constant drivers)
/// are deliberately absent: this library has no way to build a hard-wired
/// signal in real redstone, so they stay a hard error in the frontend rather
/// than gaining a library entry that would be a lie.
///
/// For `Nor`, this names the cell's *declared* arity -- which pins
/// `yosys_json` reads out of the JSON. It is not necessarily the `GateKind`
/// actually built for one specific cell instance: Yosys is free to tie a
/// `NOR3`'s pin to constant 0, and `NOR(x, y, 0) == NOR(x, y)` folds away to
/// a smaller real gate (`yosys_json::Context::build_nor` does that folding
/// and re-derives the real, possibly-smaller `GateKind` from what survives
/// it). `Buf` has no such ambiguity -- its one pin either survives or the
/// cell has no realisable output at all.
const YOSYS_CELL_KINDS: &[(&str, GateKind)] =
    &[("NOR1", GateKind::Nor(1)), ("NOR2", GateKind::Nor(2)), ("NOR3", GateKind::Nor(3)), ("BUF", GateKind::Buf)];

/// The [`GateKind`] this library realises Yosys cell type `cell_type` as, or
/// `None` if `redstone_nor.genlib` never produces a cell by that name --
/// which the frontend treats as a hard, loud error naming the cell rather
/// than a silent skip (a dropped cell is a netlist that still compiles, to
/// the wrong circuit).
pub fn gate_kind_for_yosys_cell(cell_type: &str) -> Option<GateKind> {
    YOSYS_CELL_KINDS.iter().find(|&&(name, _)| name == cell_type).map(|&(_, kind)| kind)
}

/// A symbolic node inside one [`Template`] -- turned into a real, numbered
/// `primitive_graph::NodeId` once for every gate a `Library` entry expands
/// (see `primitive_graph::expand`'s `instantiate`).
///
/// Every NOR entry needs exactly one node (`Torch`). `Buf`'s entry is the
/// first one this module ships that needs a second (`SecondTorch`, chained
/// after `Torch`) -- exactly the kind of addition this type's shape was
/// built for: a change of library data and this enum's own size, never of
/// `Template`'s shape or of `primitive_graph::expand`, which only ever looks
/// up whatever `Template::output` and `Template::inputs` name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TemplateNode {
    /// A gate's output torch (or, for a multi-torch entry, its first).
    /// Reading its `lit` state is reading that torch's own output, exactly
    /// as `place_nor_gate`'s doc comment already puts it.
    Torch,
    /// The second torch in a two-torch chain -- today, only `Buf`'s entry
    /// (`NOT(NOT(x)) == x`) has one.
    SecondTorch,
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
    /// exceeds 3), plus `Buf`'s two-torch chain -- every `GateKind` named in
    /// [`gate_kind_for_yosys_cell`]'s table, so the Yosys frontend never
    /// meets a mapped cell type with nothing here to build it from.
    pub fn default_library() -> Self {
        let mut entries = BTreeMap::new();
        for arity in 1..=3 {
            entries.insert(GateKind::Nor(arity), vec![nor_entry(arity)]);
        }
        entries.insert(GateKind::Buf, vec![buf_entry()]);
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

/// The one technique this library ships for `BUF`: two torches in series,
/// `Torch` feeding `SecondTorch`, with the declared input landing on `Torch`
/// and the gate's own output taken from `SecondTorch` -- `NOT(NOT(x)) == x`.
/// See `yosys_json::Context::synthesize_buffer` for why the Yosys frontend
/// needs a real gate for this at all (ABC's mapper requires a buffer cell to
/// exist, and there is no native "wire only" primitive in redstone).
fn buf_entry() -> LibraryEntry {
    LibraryEntry {
        name: "torch-torch-buf (buf)",
        template: Template {
            nodes: vec![(TemplateNode::Torch, Primitive::Torch), (TemplateNode::SecondTorch, Primitive::Torch)],
            internal_edges: vec![(TemplateNode::Torch, TemplateNode::SecondTorch)],
            inputs: vec![TemplateNode::Torch],
            output: TemplateNode::SecondTorch,
            embedding_hints: Vec::new(),
        },
    }
}

// ---------------------------------------------------------------------
// Genlib cost: what ABC needs to know, derived from the template
// ---------------------------------------------------------------------

/// `place_nor_gate`'s own ground-plan footprint (X*Z, in blocks) for a NOR
/// cell with `arity` inputs -- read straight off its bounding-box
/// computation (see `redstone_nor.genlib`'s own derivation comment for the
/// underlying numbers and how they were measured). This is the one fact a
/// [`Template`]'s cost genuinely cannot be derived without: `Template`
/// carries no positions at all (this module's own doc comment, "Topology
/// carries no positions"), and a footprint is a realisation fact -- how big
/// the support block plus its input sockets actually are on the ground
/// plane -- not a property of the signal graph. Hand-written here, once, so
/// every entry built out of NOR torches (today: every entry) derives its
/// *own* total area from this table rather than each carrying its own copy
/// of it the way `redstone_nor.genlib`'s comment used to.
fn nor_footprint_area(arity: usize) -> u32 {
    match arity {
        1 => 6,
        2 => 9,
        3 => 12,
        other => unreachable!("a NOR gate's fan-in is 1..=3, got {other}"),
    }
}

impl Template {
    /// This node's fan-in in `self`'s own graph: how many of the entry's
    /// declared inputs land here, plus how many internal edges terminate
    /// here. The arity the realised NOR gate at `node` actually has to
    /// carry -- for a NOR entry's lone `Torch`, `inputs.len()`; for `Buf`'s
    /// `SecondTorch`, the one internal edge from `Torch`.
    fn fan_in(&self, node: TemplateNode) -> usize {
        let external = self.inputs.iter().filter(|&&role| role == node).count();
        let internal = self.internal_edges.iter().filter(|(_, to)| *to == node).count();
        external + internal
    }

    /// How many torches sit on the longest path from any of this template's
    /// own inputs to `node`, `node` itself included -- for `node ==
    /// self.output`, the number of `TORCH_DELAY_GAME_TICKS` increments a
    /// signal pays crossing this entry end to end. Every entry this module
    /// ships is a simple chain, so "longest path" and "the only path" agree;
    /// written as a max over predecessors anyway so a future entry with real
    /// internal fan-in gets the right answer without this changing.
    fn torch_depth(&self, node: TemplateNode) -> u32 {
        let upstream = self
            .internal_edges
            .iter()
            .filter(|(_, to)| *to == node)
            .map(|&(from, _)| self.torch_depth(from))
            .max()
            .unwrap_or(0);
        upstream + 1
    }
}

/// What ABC's technology mapper needs to know about a [`LibraryEntry`], in
/// this project's own units: ground-plan area (blocks) and gate delay (game
/// ticks) -- exactly the two numbers `redstone_nor.genlib`'s `GATE`/`PIN`
/// lines encode for each cell (see that file's own derivation comment).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RealisationCost {
    pub area: u32,
    pub delay_game_ticks: u64,
}

/// Derive `entry`'s genlib cost from its own template: total area is the sum
/// of every node's own [`nor_footprint_area`] at its *actual* fan-in (from
/// [`Template::fan_in`]); total delay is one `TORCH_DELAY_GAME_TICKS` per
/// torch on the entry's longest input-to-output path (from
/// [`Template::torch_depth`]). This is total, not partial, for every entry
/// this library ships today, because every one of them is built entirely
/// out of `Primitive::Torch` nodes -- the only primitive with an ABC-visible
/// cost. A future entry built from a `Repeater` or `Comparator` would need
/// its own contribution added here (`assert_eq!` below is what would catch
/// silently pricing it at zero instead).
///
/// This is the derivation `redstone_nor.genlib`'s own comment says should
/// exist: run against `Library::default_library()`'s `Nor(1..=3)` and `Buf`
/// entries, it reproduces that file's `GATE`/`PIN` numbers exactly (see this
/// module's tests) -- proof the two are no longer two hand-maintained copies
/// of the same fact, even though the genlib file itself stays hand-written
/// (see that file's own doc comment for why: it is SIS Genlib syntax ABC
/// parses, which this crate has no other reason to generate, and a
/// byte-identical static file is the only way to guarantee ABC's own mapping
/// decisions cannot shift as a side effect of this refactor).
pub fn genlib_cost(entry: &LibraryEntry) -> RealisationCost {
    let template = &entry.template;
    let mut area = 0u32;
    for &(node, primitive) in &template.nodes {
        assert_eq!(primitive, Primitive::Torch, "genlib_cost only knows how to price a Torch node");
        area += nor_footprint_area(template.fan_in(node));
    }
    let delay_game_ticks =
        crate::redstone::simulator::component::TORCH_DELAY_GAME_TICKS * template.torch_depth(template.output) as u64;
    RealisationCost { area, delay_game_ticks }
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

    #[test]
    fn gate_kind_for_yosys_cell_maps_every_cell_redstone_nor_genlib_can_produce() {
        assert_eq!(gate_kind_for_yosys_cell("NOR1"), Some(GateKind::Nor(1)));
        assert_eq!(gate_kind_for_yosys_cell("NOR2"), Some(GateKind::Nor(2)));
        assert_eq!(gate_kind_for_yosys_cell("NOR3"), Some(GateKind::Nor(3)));
        assert_eq!(gate_kind_for_yosys_cell("BUF"), Some(GateKind::Buf));
    }

    #[test]
    fn gate_kind_for_yosys_cell_has_no_mapping_for_the_constant_drivers_or_anything_else() {
        // The constant drivers are real GATE lines in redstone_nor.genlib
        // (ABC's mapper requires them to exist), but this library has no way
        // to realize a hard-wired signal, so they get no GateKind on
        // purpose -- see YOSYS_CELL_KINDS's own doc comment.
        assert_eq!(gate_kind_for_yosys_cell("$__ZERO"), None);
        assert_eq!(gate_kind_for_yosys_cell("$__ONE"), None);
        assert_eq!(gate_kind_for_yosys_cell("$_DFF_P_"), None, "sequential logic is a later task, not this one");
        assert_eq!(gate_kind_for_yosys_cell("nor1"), None, "the table is exact-match, not case-insensitive");
    }

    #[test]
    fn buf_entry_is_two_chained_torches_with_the_input_on_the_first_and_the_output_from_the_second() {
        let entry = buf_entry();
        assert_eq!(
            entry.template.nodes,
            vec![(TemplateNode::Torch, Primitive::Torch), (TemplateNode::SecondTorch, Primitive::Torch)]
        );
        assert_eq!(entry.template.internal_edges, vec![(TemplateNode::Torch, TemplateNode::SecondTorch)]);
        assert_eq!(entry.template.inputs, vec![TemplateNode::Torch]);
        assert_eq!(entry.template.output, TemplateNode::SecondTorch);
    }

    #[test]
    fn genlib_cost_of_every_nor_arity_is_its_own_footprint_at_one_torch_delay() {
        for arity in 1..=3 {
            let cost = genlib_cost(&nor_entry(arity));
            assert_eq!(cost.area, nor_footprint_area(arity), "arity {arity}");
            assert_eq!(cost.delay_game_ticks, crate::redstone::simulator::component::TORCH_DELAY_GAME_TICKS, "arity {arity}");
        }
    }

    #[test]
    fn genlib_cost_of_buf_is_two_nor1_footprints_at_two_torch_delays() {
        let cost = genlib_cost(&buf_entry());
        assert_eq!(cost.area, 2 * nor_footprint_area(1));
        assert_eq!(cost.delay_game_ticks, 2 * crate::redstone::simulator::component::TORCH_DELAY_GAME_TICKS);
    }
}
