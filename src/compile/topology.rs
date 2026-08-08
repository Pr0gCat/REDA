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
/// - `Repeater` -- restores strength, costs a tick, one-way. Used today by
///   the isolated-merge OR entry (`or_isolated_entry`): a branch whose
///   source fans out anywhere besides the merge it feeds gets one of these
///   in series, so backflow through the merge can never reach the rest of
///   that branch (see [`GateKind::Or`]'s doc comment). Every NOR entry still
///   has none of its own (a NOR's own input sockets are realisation, not
///   topology -- see this module's doc comment) -- this is a second
///   technique adding a use, not NOR growing one.
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
/// `Or(arity)` is the first `GateKind` this library realises with **no
/// primitive at all**: a wire-merge OR needs no torch, no support, no gate
/// body -- redstone dust joining is already the maximum-of-sources operation
/// an OR is (see
/// `docs/superpowers/specs/2026-08-08-gate-types-and-wired-or.md`, "In
/// redstone an OR is free"). It is also the first `GateKind` with **more
/// than one genuinely different entry** (`or_bare_entry`/`or_isolated_entry`
/// below): dust is bidirectional, so a branch whose source also feeds
/// something besides this merge needs a repeater to stop backflow, and one
/// that does not needs nothing at all. Which entry a *specific* branch of a
/// *specific* gate instance needs is a per-instance, per-input fact about
/// the netlist (whether that one input's own source fans out elsewhere),
/// not a fixed property of the technique the way NOR's arity is -- so
/// `compile`'s own emission code decides that directly from the netlist
/// (see `merge_branch_is_bare`), rather than through `Library::choose`,
/// which only ever picks one whole-gate technique at a time. The two
/// entries this module registers for each arity still matter as data: they
/// are what makes "no primitive at all" and "one isolating repeater per
/// branch" real, named, inspectable techniques instead of something only
/// `compile` privately knows how to build.
///
/// `OR2`/`OR3` are real `GATE` lines in `redstone_nor.genlib` now (see
/// [`gate_kind_for_yosys_cell`]), which raised the question the spec that
/// ordered this work flagged as genuinely open: bare and isolated cost
/// differently at the whole-circuit level, so which price does ABC get
/// told, and can one number honestly stand for both? [`genlib_cost`]'s own
/// doc comment works this out -- the short answer is that one number does
/// stand for both, and it is the ground plan `place_merge_gate` reserves
/// (`merge_footprint_area`), at zero torch delay. Bare and isolated differ
/// only in what terminates each branch's *route*, which is not either
/// entry's own "gate" -- the same way a NOR's mandatory input-route
/// repeater has never been part of *its* genlib price. Note that "no
/// primitive at all" is not "no cell at all": a merge still occupies a row
/// and a rectangle, which is why its area is not zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GateKind {
    Nor(usize),
    Buf,
    Or(usize),
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
///
/// `OR2`/`OR3` name `Or`'s *declared* arity the same way `Nor`'s entries do,
/// and fold the same way: `yosys_json::Context::build_or` re-derives the
/// real arity from however many of a cell's pins survive constant-0
/// folding, same as `build_nor`. Folding to exactly one real input is `Or`'s
/// own extra case NOR never has -- `OR(x) == x` is a wire, not a gate of any
/// kind, so `build_or` returns the surviving input's own signal name
/// directly rather than asking this library for a one-input `Or` entry that
/// does not exist (a bare wire is not a "technique" -- there is nothing for
/// a `Library` entry to name).
const YOSYS_CELL_KINDS: &[(&str, GateKind)] = &[
    ("NOR1", GateKind::Nor(1)),
    ("NOR2", GateKind::Nor(2)),
    ("NOR3", GateKind::Nor(3)),
    ("BUF", GateKind::Buf),
    ("OR2", GateKind::Or(2)),
    ("OR3", GateKind::Or(3)),
];

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
    /// One input branch's own isolating repeater, in `or_isolated_entry`'s
    /// per-arity template -- indexed (`0..arity`) because, unlike a NOR's
    /// interchangeable inputs sharing one `Torch`, each branch here is a
    /// physically distinct repeater: one input landing on the same node as
    /// another would mean two branches sharing one repeater's single input
    /// face, which is not a real technique.
    IsolatingRepeater(usize),
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
    /// in order (`inputs.len()` is this entry's arity, *except* for
    /// `or_bare_entry`, where it is empty -- see that function's own doc
    /// comment for why an entry with no nodes at all has nowhere for an
    /// input to land). Every NOR entry names the same node (`Torch`) for
    /// every index, because a NOR's inputs are interchangeable and land
    /// directly on the one functional element there is; `or_isolated_entry`
    /// names a *different* node per index, since each branch is its own
    /// physical repeater.
    pub inputs: Vec<TemplateNode>,
    /// Which node this gate's own outbound signal edge (to whatever it
    /// drives, or to a declared output's lamp) originates from -- `None`
    /// exactly when this entry realises to **no primitive at all**: both OR
    /// entries (`or_bare_entry`/`or_isolated_entry`) leave this `None`,
    /// because even isolated, a merge has no functional element of its own
    /// that a signal could be said to leave *from*. Electrically, this
    /// gate's own declared output net is simply the same net as its inputs
    /// (or, for the isolated entry, the same net as what its own repeaters
    /// forward into) -- see
    /// `docs/superpowers/specs/2026-08-08-gate-types-and-wired-or.md`, "An
    /// OR is a node, not a disappearing act". Every entry with a real
    /// primitive names `Some` node, exactly as before.
    pub output: Option<TemplateNode>,
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
    /// exceeds 3), `Buf`'s two-torch chain -- every `GateKind` named in
    /// [`gate_kind_for_yosys_cell`]'s table, so the Yosys frontend never
    /// meets a mapped cell type with nothing here to build it from -- plus
    /// `Or(2)` and `Or(3)`, each with two entries (bare, then isolated; see
    /// [`GateKind::Or`]'s doc comment for why both are registered even
    /// though `gate_kind_for_yosys_cell`'s table -- `OR2`/`OR3` -- only ever
    /// resolves to the whole `GateKind`, never to one specific entry:
    /// `compile`'s own emission code, not `Library::choose`, is what picks
    /// bare vs. isolated per branch, per gate instance).
    pub fn default_library() -> Self {
        let mut entries = BTreeMap::new();
        for arity in 1..=3 {
            entries.insert(GateKind::Nor(arity), vec![nor_entry(arity)]);
        }
        entries.insert(GateKind::Buf, vec![buf_entry()]);
        for arity in 2..=3 {
            entries.insert(GateKind::Or(arity), vec![or_bare_entry(arity), or_isolated_entry(arity)]);
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
            output: Some(TemplateNode::Torch),
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
            output: Some(TemplateNode::SecondTorch),
            embedding_hints: Vec::new(),
        },
    }
}

/// The bare-merge technique for an `arity`-input OR: **no nodes at all**.
///
/// This is the entry the spec's "An OR is a node, not a disappearing act"
/// and this module's own doc comment ("the first entry that maps a gate
/// onto no primitive at all") are about. A wire-merge OR has nothing to
/// place -- no torch, no support, not even a repeater -- so `nodes` is
/// empty, `inputs` is empty (there is no node for a declared input's signal
/// edge to land on -- the input *is* the output, electrically), and
/// `output` is `None`. `compile`'s own emission code is what actually knows
/// how to build this (a join at the point downstream of where the declared
/// inputs' own routes are allowed to touch -- see `place_merge_gate`); this
/// entry exists so the technique is named and inspectable in the library
/// even though nothing here needs instantiating.
///
/// Correct only when none of the gate's declared inputs fans out to
/// anything besides this merge -- see [`GateKind::Or`]'s doc comment and
/// `or_isolated_entry` for the alternative when that does not hold.
fn or_bare_entry(arity: usize) -> LibraryEntry {
    assert!((2..=3).contains(&arity), "an OR entry's fan-in is 2..=3, got {arity}");
    LibraryEntry {
        name: match arity {
            2 => "wire-merge-or2 (bare)",
            3 => "wire-merge-or3 (bare)",
            _ => unreachable!("checked by the assert above"),
        },
        template: Template { nodes: Vec::new(), internal_edges: Vec::new(), inputs: Vec::new(), output: None, embedding_hints: Vec::new() },
    }
}

/// The fully-isolated technique for an `arity`-input OR: one
/// [`TemplateNode::IsolatingRepeater`] per branch, and still **no output
/// primitive** -- even isolated, a merge has no functional element of its
/// own; only its inputs gain one apiece. `output` stays `None` for the same
/// reason `or_bare_entry`'s does (see that function's doc comment and
/// `Template::output`'s own).
///
/// This is the safe-for-any-instance technique: isolating a branch that did
/// not actually need it (a private one) costs an unnecessary repeater, never
/// correctness, so registering "every branch isolated" as one named entry is
/// sound even though `compile`'s own emission code, per gate instance,
/// mixes bare and isolated branches individually according to the fanout
/// rule (see [`GateKind::Or`]'s doc comment for why that per-branch mixing
/// is not itself expressed as further library entries).
fn or_isolated_entry(arity: usize) -> LibraryEntry {
    assert!((2..=3).contains(&arity), "an OR entry's fan-in is 2..=3, got {arity}");
    let nodes: Vec<(TemplateNode, Primitive)> =
        (0..arity).map(|i| (TemplateNode::IsolatingRepeater(i), Primitive::Repeater)).collect();
    let inputs: Vec<TemplateNode> = (0..arity).map(TemplateNode::IsolatingRepeater).collect();
    LibraryEntry {
        name: match arity {
            2 => "wire-merge-or2 (isolated)",
            3 => "wire-merge-or3 (isolated)",
            _ => unreachable!("checked by the assert above"),
        },
        template: Template { nodes, internal_edges: Vec::new(), inputs, output: None, embedding_hints: Vec::new() },
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

/// `place_merge_gate`'s own ground-plan footprint (X*Z, in blocks) for a
/// wire-merge cell with `arity` inputs -- read off its bounding-box
/// computation exactly the way [`nor_footprint_area`] is read off
/// `place_nor_gate`'s, and for the same reason (a footprint is a
/// realisation fact a positionless [`Template`] cannot derive).
///
/// A merge places no support block and no torch. It is still a *cell* the
/// floorplanner has to hand a row, an X and a Z to, though:
/// `place_merge_gate` returns a real `NorCell` with a real `size`, computed
/// by the same bounding-box walk `place_nor_gate` uses, over the same
/// `INPUT_DIRECTIONS` sockets. The one thing it does not reserve is the
/// cell the absent output torch would have stood in, so its output socket
/// sits one hop north of the junction where a NOR's sits two hops north of
/// its support -- which is exactly, and only, one row of Z cheaper:
///
/// ```text
///   inputs   NorCell.size (x,y,z)   ground footprint (x * z)   NOR's, for comparison
///   2        (3, 1, 2)              6                          9
///   3        (3, 1, 3)              9                          12
/// ```
///
/// `every_cell_footprint_matches_what_the_placer_actually_reserves` (below)
/// checks both this table and [`nor_footprint_area`] against what
/// `place_merge_gate`/`place_nor_gate` really write into a scratch `World`,
/// so neither can drift from the placer the way this one silently could
/// while it did not exist at all.
fn merge_footprint_area(arity: usize) -> u32 {
    match arity {
        2 => 6,
        3 => 9,
        other => unreachable!("a wire-merge OR's fan-in is 2..=3, got {other}"),
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
///
/// # OR: one number, honestly standing for two realisations
///
/// An OR is the one [`GateKind`] here whose entries realise to no output
/// primitive at all, so its cost cannot come from counting torches. It
/// comes from [`merge_footprint_area`] instead -- the same ground plan
/// `place_merge_gate` actually reserves -- and its delay is zero, because
/// zero torches stand on the path through it.
///
/// **This function used to return `(0, 0)` for an OR, and that area was
/// wrong.** The reasoning behind it went: this genlib prices gate bodies
/// and has never priced wire for any cell, a merge has no body, therefore a
/// merge costs nothing. Every step of that is true except the conclusion,
/// and the step it skips is the one that matters -- *a merge is still a
/// cell*. `place_merge_gate` returns a real `NorCell` with a real `size`,
/// computed by the same bounding-box walk over the same `INPUT_DIRECTIONS`
/// sockets `place_nor_gate` uses; the floorplanner gives it a row and a
/// slot exactly as it gives one to a NOR (`compile::compute_asap_levels`
/// levelises merge and non-merge gates identically). "Area" in this cost
/// model has only ever meant that reserved ground plan, never the torch
/// standing on it, so a merge's area is its own rectangle -- 6 for `Or(2)`,
/// 9 for `Or(3)` -- and not zero.
///
/// Zero was not merely imprecise, it was unbounded: an OR priced at zero is
/// an OR ABC will buy any number of, and every one it buys costs real
/// blocks. Measured through the real compiler, that is what happened -- the
/// Verilog-derived seven-segment decoder went from 37 gates / 8130 blocks
/// to 50 / 10668 when `OR2`/`OR3` were added to the genlib at area 0. See
/// `redstone_nor.genlib`'s own "OR2 / OR3" section for the price sweep that
/// re-measured it.
///
/// The **delay** half of the old derivation was right, and survives
/// unchanged at zero. A merge places no torch, so nothing on the path
/// through it costs `TORCH_DELAY_GAME_TICKS`; and the repeater an isolated
/// branch gets is not this gate's own cost either. Trace where
/// `compile::emit` puts it (`compile::merge_branch_is_bare` choosing
/// between `lay_bent_path_bare` and `lay_bent_path`): it is the exact same
/// mandatory route-terminating repeater *every* socket in this compiler
/// already pays unconditionally -- a plain `Nor` gate's input sockets
/// included (`compile`'s own module doc comment: "a route always ends in a
/// repeater facing the next gate's support block"). Choosing
/// `or_isolated_entry` over `or_bare_entry` does not add a repeater on top
/// of some cheaper baseline route; it is simply *not* the one special case
/// (`lay_bent_path_bare`) that gets to omit the repeater every other route
/// in the whole compiled circuit already has. This genlib has never priced
/// that repeater for `Nor`/`Buf` either (see `redstone_nor.genlib`'s own
/// "the 'fanout delay' fields are 0" note), so not pricing it here is the
/// rule being applied, not suspended.
///
/// That is also why **one number still honestly prices both entries.** Bare
/// and isolated differ only in what terminates each branch's route, which
/// is route cost on both sides of the comparison; the cell itself --
/// `place_merge_gate`'s reserved rectangle -- is byte-for-byte the same
/// either way, and neither ever places a torch. So the promise stays one
/// ABC's mapper can keep: choosing an OR never costs more reserved ground
/// or more torch delay than this says, whichever realisation each branch
/// ends up with.
pub fn genlib_cost(kind: GateKind, entry: &LibraryEntry) -> RealisationCost {
    let template = &entry.template;

    // An OR realises to no primitive, so there is nothing in `nodes` to
    // price -- but there is still a reserved ground plan, and its size
    // depends on the gate's declared arity, which only `kind` carries (an
    // entry that realises to nothing cannot record its arity in `inputs`:
    // there is no node for a declared input to land on -- see
    // `or_bare_entry`).
    if let GateKind::Or(arity) = kind {
        assert!(
            template.output.is_none(),
            "`{}` realises an OR, which has no output primitive to take a signal from",
            entry.name
        );
        return RealisationCost { area: merge_footprint_area(arity), delay_game_ticks: 0 };
    }

    let output = template
        .output
        .unwrap_or_else(|| panic!("`{}` ({kind:?}) has no output node to measure a delay to", entry.name));
    let mut area = 0u32;
    for &(node, primitive) in &template.nodes {
        assert_eq!(primitive, Primitive::Torch, "genlib_cost only knows how to price a Torch node");
        area += nor_footprint_area(template.fan_in(node));
    }
    let delay_game_ticks = crate::redstone::simulator::component::TORCH_DELAY_GAME_TICKS * template.torch_depth(output) as u64;
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
            assert_eq!(entry.template.output, Some(TemplateNode::Torch), "arity {arity}");
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
        assert_eq!(entry.template.output, Some(TemplateNode::SecondTorch));
    }

    #[test]
    fn genlib_cost_of_every_nor_arity_is_its_own_footprint_at_one_torch_delay() {
        for arity in 1..=3 {
            let cost = genlib_cost(GateKind::Nor(arity), &nor_entry(arity));
            assert_eq!(cost.area, nor_footprint_area(arity), "arity {arity}");
            assert_eq!(cost.delay_game_ticks, crate::redstone::simulator::component::TORCH_DELAY_GAME_TICKS, "arity {arity}");
        }
    }

    #[test]
    fn genlib_cost_of_buf_is_two_nor1_footprints_at_two_torch_delays() {
        let cost = genlib_cost(GateKind::Buf, &buf_entry());
        assert_eq!(cost.area, 2 * nor_footprint_area(1));
        assert_eq!(cost.delay_game_ticks, 2 * crate::redstone::simulator::component::TORCH_DELAY_GAME_TICKS);
    }

    /// Both footprint tables are the one realisation fact `genlib_cost`
    /// cannot derive from a positionless `Template`, so both are checked
    /// against what the placer actually reserves -- by placing one cell of
    /// each kind and arity into a scratch `World` and reading `NorCell.size`
    /// back, exactly the measurement `redstone_nor.genlib`'s derivation
    /// comment claims was done by hand. Without this, `nor_footprint_area`
    /// and `merge_footprint_area` are two numbers nothing stops from
    /// drifting away from `place_nor_gate`/`place_merge_gate`.
    #[test]
    fn every_cell_footprint_matches_what_the_placer_actually_reserves() {
        use crate::redstone::world::storage::World;

        // Big enough that a cell placed in the middle cannot be clipped by
        // the world bounds in any direction.
        const ORIGIN: (i32, i32, i32) = (8, 1, 8);

        for arity in 1..=3 {
            let mut world = World::new(20, 6, 20);
            let cell = super::super::place_nor_gate(&mut world, ORIGIN, arity);
            let (x, _, z) = cell.size;
            assert_eq!(
                (x * z) as u32,
                nor_footprint_area(arity),
                "NOR arity {arity}: place_nor_gate reserves {x}x{z}, nor_footprint_area says {}",
                nor_footprint_area(arity)
            );
        }

        for arity in 2..=3 {
            let mut world = World::new(20, 6, 20);
            let cell = super::super::place_merge_gate(&mut world, ORIGIN, arity);
            let (x, _, z) = cell.size;
            assert_eq!(
                (x * z) as u32,
                merge_footprint_area(arity),
                "OR arity {arity}: place_merge_gate reserves {x}x{z}, merge_footprint_area says {}",
                merge_footprint_area(arity)
            );
        }
    }

    // -----------------------------------------------------------------
    // OR: the first gate type that realises to no primitive at all, and the
    // first with genuinely alternative entries.
    // -----------------------------------------------------------------

    #[test]
    fn default_library_registers_a_bare_and_an_isolated_entry_for_or_two_and_three() {
        let library = Library::default_library();
        for arity in 2..=3 {
            let entries = library.entries_for(GateKind::Or(arity));
            assert_eq!(entries.len(), 2, "arity {arity} should have exactly the bare and isolated entries");
            assert!(entries[0].name.contains("bare"), "arity {arity}: bare entry should be registered first");
            assert!(entries[1].name.contains("isolated"), "arity {arity}");
        }
        assert!(library.entries_for(GateKind::Or(4)).is_empty(), "fan-in 4 has no entry -- it is not hardware");
        assert!(library.entries_for(GateKind::Or(1)).is_empty(), "a 1-input OR is not registered");
    }

    #[test]
    fn or_bare_entry_has_no_nodes_no_inputs_and_no_output_primitive() {
        for arity in 2..=3 {
            let entry = or_bare_entry(arity);
            assert!(entry.template.nodes.is_empty(), "arity {arity}: a bare merge places nothing");
            assert!(entry.template.internal_edges.is_empty(), "arity {arity}");
            assert!(
                entry.template.inputs.is_empty(),
                "arity {arity}: there is no node for a declared input to land on"
            );
            assert_eq!(entry.template.output, None, "arity {arity}: no primitive realises this gate's output");
        }
    }

    #[test]
    fn or_isolated_entry_has_one_distinct_repeater_per_branch_and_still_no_output_primitive() {
        for arity in 2..=3 {
            let entry = or_isolated_entry(arity);
            assert_eq!(entry.template.nodes.len(), arity, "arity {arity}: one repeater per branch");
            for &(role, primitive) in &entry.template.nodes {
                assert_eq!(primitive, Primitive::Repeater, "arity {arity}: every node here is a repeater");
                assert!(matches!(role, TemplateNode::IsolatingRepeater(_)), "arity {arity}");
            }
            // Every branch lands on its *own* node -- no two inputs share a
            // repeater's single input face.
            let distinct: std::collections::BTreeSet<TemplateNode> = entry.template.inputs.iter().copied().collect();
            assert_eq!(distinct.len(), arity, "arity {arity}: every input must name a distinct node");
            assert!(entry.template.internal_edges.is_empty(), "arity {arity}: branches do not feed each other");
            assert_eq!(entry.template.output, None, "arity {arity}: even isolated, a merge has no output primitive");
        }
    }

    #[test]
    fn genlib_cost_of_a_bare_or_merge_is_its_reserved_ground_plan_at_no_torch_delay() {
        // Not zero area: a merge places no primitive, but `place_merge_gate`
        // still reserves a rectangle for it (see `merge_footprint_area`).
        // Zero *delay* is the real, measured figure -- a bare merge settles
        // instantaneously, there being no active component in it at all (see
        // the referenced cost-table spec, and `tests/cell_type_costs.rs`'s
        // `or_is_a_free_wire_merge_when_nothing_else_shares_the_branch`).
        for arity in 2..=3 {
            let cost = genlib_cost(GateKind::Or(arity), &or_bare_entry(arity));
            assert_eq!(cost.area, merge_footprint_area(arity), "arity {arity}");
            assert_eq!(cost.delay_game_ticks, 0, "arity {arity}");
        }
    }

    #[test]
    fn genlib_cost_of_an_isolated_or_merge_is_exactly_the_bare_entrys() {
        // Not a placeholder, and not a different (understated) number from
        // the bare entry's -- see `genlib_cost`'s own doc comment, "OR: one
        // number, honestly standing for two realisations": the two entries
        // reserve the identical ground plan (`place_merge_gate` does not
        // know which one it is building), and the isolating repeater is the
        // same mandatory route-terminating repeater every socket in this
        // compiler already pays, never a cost this entry's own cell adds on
        // top.
        for arity in 2..=3 {
            let bare = genlib_cost(GateKind::Or(arity), &or_bare_entry(arity));
            let isolated = genlib_cost(GateKind::Or(arity), &or_isolated_entry(arity));
            assert_eq!(isolated, bare, "arity {arity}: one genlib number has to price both realisations");
            assert_eq!(isolated.area, merge_footprint_area(arity), "arity {arity}");
            assert_eq!(isolated.delay_game_ticks, 0, "arity {arity}");
        }
    }

    /// The regression that made this price list worth re-deriving at all,
    /// stated as an invariant rather than as a number: no cell in this
    /// library may cost ABC *nothing*. A cell priced at zero is one the
    /// mapper will buy without limit, and every cell here -- merge included
    /// -- occupies real ground when `compile` places it.
    #[test]
    fn no_library_entry_is_priced_at_zero_area() {
        let library = Library::default_library();
        for (&kind, entries) in &library.entries {
            for entry in entries {
                let cost = genlib_cost(kind, entry);
                assert!(cost.area > 0, "{kind:?}'s entry `{}` is priced at zero area", entry.name);
            }
        }
    }

    #[test]
    fn gate_kind_for_yosys_cell_maps_or2_and_or3() {
        // Step 4 of the referenced spec's Order: the genlib gains OR at its
        // real cost, and the frontend maps it onto the library's wire-merge
        // entries.
        assert_eq!(gate_kind_for_yosys_cell("OR2"), Some(GateKind::Or(2)));
        assert_eq!(gate_kind_for_yosys_cell("OR3"), Some(GateKind::Or(3)));
    }

    #[test]
    fn gate_kind_for_yosys_cell_never_maps_yosys_native_or_names() {
        // `$_OR_` is Yosys's own pre-`abc` cell name (the one a plain
        // `synth`, with no technology mapping, would leave behind -- see the
        // referenced cost-table spec's cell histogram). `abc -genlib`
        // renames every cell it maps to the GATE line it chose, so a
        // *mapped* netlist's OR cells are always literally `OR2`/`OR3`, the
        // same way a mapped NOR cell is `NOR2`/`NOR3`, never `$_NOR_`. This
        // table is exact-match against genlib's own names, so the
        // pre-mapping name correctly has, and always will have, no entry.
        assert_eq!(gate_kind_for_yosys_cell("$_OR_"), None);
    }
}
