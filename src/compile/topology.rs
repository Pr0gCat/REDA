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

/// Which gate a netlist gate is, and -- for the two kinds redstone can build
/// directly -- which [`Library`] entry realises it.
///
/// # Two levels in one vocabulary
///
/// This enum spans the two levels the compiler now has, and the split is the
/// whole point of the pipeline `lowering::lower` sits in the middle of:
///
/// - **[`GateKind::Nor`] and [`GateKind::Or`] are what redstone builds.** A
///   NOR is a torch on a support block (`place_nor_gate`); an OR is dust
///   joining (`place_merge_gate`). These two, and only these two, are what
///   `compile` knows how to place, what [`Library`] ships a [`Template`]
///   for, and what `primitive_graph::expand` can turn into primitives.
/// - **Everything else is the gate level** -- the vocabulary Yosys's own
///   synthesis emits (`$_AND_`, `$_NAND_`, `$_XOR_`, `$_MUX_` ...). No such
///   gate has a redstone realisation of its own; each has an
///   [`Expansion`] into the two that do, and `lowering::lower` is the pass
///   that applies it.
///
/// Before this split, `abc -genlib redstone_nor.genlib` collapsed the gate
/// level into NOR inside Yosys, so the netlist reaching this crate was NOR
/// by construction and this library never got to decide anything. Now the
/// frontend reads Yosys's gate-level netlist as it stands and *this* module
/// decides how each gate becomes redstone -- see [`expansion_for`].
///
/// NOR's fan-in is 1..=3 (`place_nor_gate`'s own hardware ceiling), a
/// merge's 2..=3 (`place_merge_gate` shares the same three input faces), so
/// for those two the arity is part of the kind: it selects the [`Template`].
/// `Nor(1)` is the same gate the spec calls `NOT` (a NOR gate with one input
/// inverts exactly like an inverter); [`GateKind::NOT`] names that case for
/// readability at call sites that mean it. Every gate-level variant has a
/// fixed arity instead (Yosys's simple cells are all fixed-arity), so it
/// carries none.
///
/// `Buf` is not logic at all -- it is what a Yosys `$_BUF_` cell, and a bare
/// `assign out = in;` with no cell in between, realises as: two chained
/// `Nor(1)` torches, `NOT(NOT(x)) == x`. It is a gate-level kind like the
/// rest, with its own [`Expansion`]; it also still has a [`Template`], since
/// two chained torches are exactly the sort of not-one-to-one entry
/// [`Template`] exists to hold, and
/// `expansion_for_buf_agrees_with_the_two_torch_template` keeps the two
/// statements of that one fact from drifting.
///
/// `Or(arity)` is the `GateKind` this library realises with **no
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
/// Bare and isolated cost differently at the whole-circuit level, which
/// raises the question of whether one number can honestly stand for both.
/// [`realisation_cost`]'s own doc comment works this out -- the short answer
/// is that one number does stand for both, and it is the ground plan
/// `place_merge_gate` reserves (`merge_footprint_area`), at zero torch
/// delay. Bare and isolated differ only in what terminates each branch's
/// *route*, which is not either entry's own "gate" -- the same way a NOR's
/// mandatory input-route repeater has never been part of *its* price. Note
/// that "no primitive at all" is not "no cell at all": a merge still
/// occupies a row and a rectangle, which is why its area is not zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GateKind {
    // ---- realisable in redstone directly ----
    /// A NOR gate of `arity` inputs (1..=3): one torch on one support block.
    Nor(usize),
    /// A wire-merge OR of `arity` inputs (2..=3): dust joining, no torch.
    Or(usize),

    // ---- the gate level: Yosys's own simple cells ----
    /// `$_BUF_`, and a bare `assign out = in;`. `Y = A`.
    Buf,
    /// `$_AND_`. `Y = A & B`.
    And,
    /// `$_NAND_`. `Y = !(A & B)`.
    Nand,
    /// `$_XOR_`. `Y = A ^ B`.
    Xor,
    /// `$_XNOR_`. `Y = !(A ^ B)`.
    Xnor,
    /// `$_ANDNOT_`. `Y = A & !B`.
    AndNot,
    /// `$_ORNOT_`. `Y = A | !B`.
    OrNot,
    /// `$_AOI3_`. `Y = !((A & B) | C)`.
    Aoi3,
    /// `$_OAI3_`. `Y = !((A | B) & C)`.
    Oai3,
    /// `$_AOI4_`. `Y = !((A & B) | (C & D))`.
    Aoi4,
    /// `$_OAI4_`. `Y = !((A | B) & (C | D))`.
    Oai4,
    /// `$_MUX_`. `Y = S ? B : A`.
    Mux,
    /// `$_NMUX_`. `Y = !(S ? B : A)`.
    Nmux,
}

impl GateKind {
    /// `NOR` with one input -- an inverter. See the spec's "The topology
    /// library": "`NOT` maps to `input — torch — output`".
    pub const NOT: GateKind = GateKind::Nor(1);

    /// How many inputs a gate of this kind has. `None` for [`GateKind::Nor`]
    /// and [`GateKind::Or`], whose arity is variable and carried in the
    /// variant itself -- callers wanting "the arity of this kind, whatever
    /// it is" should use [`GateKind::arity`].
    ///
    /// Every gate-level kind is fixed-arity: Yosys's simple cells are, so
    /// there is nothing for a variant to carry and nothing for a netlist to
    /// disagree with.
    pub fn fixed_arity(self) -> Option<usize> {
        match self {
            GateKind::Nor(_) | GateKind::Or(_) => None,
            GateKind::Buf => Some(1),
            GateKind::And | GateKind::Nand | GateKind::Xor | GateKind::Xnor | GateKind::AndNot | GateKind::OrNot => {
                Some(2)
            }
            GateKind::Aoi3 | GateKind::Oai3 | GateKind::Mux | GateKind::Nmux => Some(3),
            GateKind::Aoi4 | GateKind::Oai4 => Some(4),
        }
    }

    /// How many inputs a gate of this kind has.
    pub fn arity(self) -> usize {
        match self {
            GateKind::Nor(arity) | GateKind::Or(arity) => arity,
            other => other.fixed_arity().expect("every non-Nor/Or kind is fixed-arity"),
        }
    }

    /// Whether redstone builds this kind directly -- `Nor` (a torch) or `Or`
    /// (a wire merge). These are the only two kinds `compile` can place and
    /// `primitive_graph::expand` can turn into primitives; everything else
    /// has to go through `lowering::lower` first.
    pub fn is_realisable(self) -> bool {
        matches!(self, GateKind::Nor(_) | GateKind::Or(_))
    }

    /// The name this kind goes by in the baked-netlist text format, in
    /// `mc_dump`'s `GATE` lines, and in the viewer -- the one spelling, so
    /// three readers cannot disagree about what a gate is. The inverse is
    /// [`GateKind::from_wire_name`].
    ///
    /// `Nor`/`Or` deliberately drop their arity here: a gate's input list is
    /// right there beside the name in every one of those formats, so
    /// spelling the arity again would be a second copy of it that a
    /// hand-edited file could contradict.
    pub fn wire_name(self) -> &'static str {
        match self {
            GateKind::Nor(_) => "nor",
            GateKind::Or(_) => "merge",
            GateKind::Buf => "buf",
            GateKind::And => "and",
            GateKind::Nand => "nand",
            GateKind::Xor => "xor",
            GateKind::Xnor => "xnor",
            GateKind::AndNot => "andnot",
            GateKind::OrNot => "ornot",
            GateKind::Aoi3 => "aoi3",
            GateKind::Oai3 => "oai3",
            GateKind::Aoi4 => "aoi4",
            GateKind::Oai4 => "oai4",
            GateKind::Mux => "mux",
            GateKind::Nmux => "nmux",
        }
    }

    /// The inverse of [`GateKind::wire_name`]. `arity` is the gate's own
    /// input count, which is what supplies `Nor`/`Or`'s missing parameter;
    /// for every other kind it is ignored (the reader checks it against
    /// [`GateKind::arity`] separately, so a malformed line is rejected
    /// rather than silently reinterpreted).
    pub fn from_wire_name(name: &str, arity: usize) -> Option<GateKind> {
        Some(match name {
            "nor" => GateKind::Nor(arity),
            "merge" => GateKind::Or(arity),
            "buf" => GateKind::Buf,
            "and" => GateKind::And,
            "nand" => GateKind::Nand,
            "xor" => GateKind::Xor,
            "xnor" => GateKind::Xnor,
            "andnot" => GateKind::AndNot,
            "ornot" => GateKind::OrNot,
            "aoi3" => GateKind::Aoi3,
            "oai3" => GateKind::Oai3,
            "aoi4" => GateKind::Aoi4,
            "oai4" => GateKind::Oai4,
            "mux" => GateKind::Mux,
            "nmux" => GateKind::Nmux,
            _ => return None,
        })
    }

    /// Evaluate this kind's own boolean function on `inputs` (in pin order).
    /// The one place the *meaning* of each gate-level kind is written down
    /// as executable truth rather than as a comment -- which is what lets
    /// `lowering`'s tests check an expansion against the gate it claims to
    /// realise, exhaustively, instead of against a second hand-derivation
    /// of the same formula.
    ///
    /// Panics if `inputs.len()` disagrees with [`GateKind::arity`].
    pub fn evaluate(self, inputs: &[bool]) -> bool {
        assert_eq!(inputs.len(), self.arity(), "{self:?} takes {} input(s)", self.arity());
        match self {
            GateKind::Nor(_) => !inputs.iter().any(|&b| b),
            GateKind::Or(_) => inputs.iter().any(|&b| b),
            GateKind::Buf => inputs[0],
            GateKind::And => inputs[0] && inputs[1],
            GateKind::Nand => !(inputs[0] && inputs[1]),
            GateKind::Xor => inputs[0] ^ inputs[1],
            GateKind::Xnor => !(inputs[0] ^ inputs[1]),
            GateKind::AndNot => inputs[0] && !inputs[1],
            GateKind::OrNot => inputs[0] || !inputs[1],
            GateKind::Aoi3 => !((inputs[0] && inputs[1]) || inputs[2]),
            GateKind::Oai3 => !((inputs[0] || inputs[1]) && inputs[2]),
            GateKind::Aoi4 => !((inputs[0] && inputs[1]) || (inputs[2] && inputs[3])),
            GateKind::Oai4 => !((inputs[0] || inputs[1]) && (inputs[2] || inputs[3])),
            // Yosys: `Y = S ? B : A`, pins in declaration order A, B, S.
            GateKind::Mux => {
                if inputs[2] {
                    inputs[1]
                } else {
                    inputs[0]
                }
            }
            GateKind::Nmux => {
                !(if inputs[2] {
                    inputs[1]
                } else {
                    inputs[0]
                })
            }
        }
    }
}

// ---------------------------------------------------------------------
// The Yosys boundary: which cell type is which GateKind
// ---------------------------------------------------------------------

/// Every combinational cell type Yosys's `abc` pass can emit, and the
/// [`GateKind`] this library realises it as. This is the boundary the spec's
/// "gate topology is a library" is asking for stated as data instead of as
/// parsing code: `yosys_json::Context::build_cell` looks a cell's `type` up
/// here (via [`gate_kind_for_yosys_cell`]) rather than matching the string
/// itself and separately knowing what to build for it.
///
/// This table used to hold `NOR1`/`NOR2`/`NOR3`/`BUF`/`OR2`/`OR3` -- the
/// `GATE` lines of a genlib the frontend asked ABC to technology-map onto.
/// Mapping was the mistake: it decided the redstone realisation of every
/// gate inside Yosys, before this library ever saw the design. ABC still
/// runs (its logic optimisation is the genuinely valuable half; it is what
/// takes the hand-written decoder's 84 gates to 31), but it now leaves the
/// design in Yosys's own gate-level vocabulary and *this* module decides the
/// realisation -- see [`expansion_for`], and `frontend/synth.py` for the
/// script change that is the whole difference.
///
/// Names are Yosys's `$_*_` "simple cell" set (see Yosys's
/// `docs/source/cell/word_*.rst` and `techlibs/common/simcells.v`), which is
/// exactly the set its `abc` pass's default `-g` gate list can produce.
/// Deliberately absent, and staying absent:
///
/// - `$_DFF_*_`, `$_DLATCH_*_`, `$_SR_*_` and every other sequential cell.
///   Sequential logic is a later task; a cell with state has no
///   realisation here, so it stays a hard error naming the cell.
/// - `$_TBUF_`, and Yosys's `$__ZERO`/`$__ONE` constant drivers. There is
///   no tri-state and no "always on" cell in real redstone, so an entry for
///   either would be a lie.
///
/// Two Yosys cells need no expansion at all, because redstone builds them
/// directly: `$_NOR_` is [`GateKind::Nor`] (a torch) and `$_OR_` is
/// [`GateKind::Or`] (a wire merge). `$_NOT_` is `Nor(1)` for the same
/// reason -- a one-input NOR *is* an inverter, the identical cell, so
/// giving it a separate kind would be two names for one torch.
const YOSYS_CELL_KINDS: &[(&str, GateKind)] = &[
    ("$_NOT_", GateKind::Nor(1)),
    ("$_NOR_", GateKind::Nor(2)),
    ("$_OR_", GateKind::Or(2)),
    ("$_BUF_", GateKind::Buf),
    ("$_AND_", GateKind::And),
    ("$_NAND_", GateKind::Nand),
    ("$_XOR_", GateKind::Xor),
    ("$_XNOR_", GateKind::Xnor),
    ("$_ANDNOT_", GateKind::AndNot),
    ("$_ORNOT_", GateKind::OrNot),
    ("$_AOI3_", GateKind::Aoi3),
    ("$_OAI3_", GateKind::Oai3),
    ("$_AOI4_", GateKind::Aoi4),
    ("$_OAI4_", GateKind::Oai4),
    ("$_MUX_", GateKind::Mux),
    ("$_NMUX_", GateKind::Nmux),
];

/// The [`GateKind`] this library realises Yosys cell type `cell_type` as, or
/// `None` if this library has no realisation for a cell by that name --
/// which the frontend treats as a hard, loud error naming the cell rather
/// than a silent skip (a dropped cell is a netlist that still compiles, to
/// the wrong circuit).
pub fn gate_kind_for_yosys_cell(cell_type: &str) -> Option<GateKind> {
    YOSYS_CELL_KINDS.iter().find(|&&(name, _)| name == cell_type).map(|&(_, kind)| kind)
}

/// Every Yosys cell type [`gate_kind_for_yosys_cell`] knows, for tests and
/// diagnostics that want to say what *is* supported rather than only reject
/// what is not.
pub fn known_yosys_cell_types() -> impl Iterator<Item = (&'static str, GateKind)> {
    YOSYS_CELL_KINDS.iter().copied()
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
// Expansions: how a gate-level gate becomes redstone
// ---------------------------------------------------------------------

/// Which logical rail of a declared gate input an [`Operand::Input`] reads.
///
/// Recipes describe logical topology and can therefore ask for either the
/// declared signal or its complement. `lowering` decides how the latter rail
/// is materialised and shared in a concrete netlist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SignalPolarity {
    Positive,
    Negative,
}

/// One input of an [`Expansion`] step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operand {
    /// One logical rail of the gate's declared input `pin`, in pin order.
    Input { pin: usize, polarity: SignalPolarity },
    /// The result of the recipe's `i`-th earlier step. Always refers
    /// backwards, so a recipe is a DAG by construction and needs no cycle
    /// check ([`Expansion::validate`] enforces it).
    Step(usize),
}

/// One step of an [`Expansion`]: one of the exactly two things redstone
/// builds. There is deliberately no third variant -- if a step could be
/// anything else, this type would no longer be a statement about hardware.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// A NOR gate: one torch on one support block (`place_nor_gate`). 1..=3
    /// operands. A one-operand `Nor` is an inverter, and `lowering` routes
    /// it through `NetlistBuilder::not`, so the same inverted signal is
    /// built once and shared across every gate in the circuit that wants it.
    Nor(Vec<Operand>),
    /// A wire merge (`place_merge_gate`): dust joining, no torch, no
    /// support, no gate body. 2..=3 operands.
    Merge(Vec<Operand>),
}

/// How one [`GateKind`] becomes redstone: a short, straight-line list of
/// [`Step`]s over the gate's own inputs, whose **last step drives the
/// gate's declared output**.
///
/// This is the thing `abc -genlib redstone_nor.genlib` used to decide for
/// us, moved to where it belongs. It is data, not code, for the same reason
/// [`Library`] is: a better realisation of `$_XOR_` should be a different
/// list here, inspectable and diffable, not a different branch inside a
/// lowering function.
///
/// # Why NOR-of-inverted-inputs, and why merges are worth reaching for
///
/// Two facts about this hardware drive every recipe below:
///
/// - `NOR(x, y) = !x & !y`. So an **AND-shaped** output is one torch over
///   inverted inputs, while an **OR-shaped** output would need a second
///   torch to un-invert -- unless it can be a merge.
/// - A merge is free of torches entirely: dust joining *is* the
///   maximum-of-sources operation an OR is. So an OR-shaped output costs
///   **no** gate delay at all, which inverts the usual CMOS instinct.
///
/// Together those say: invert what you must (once, shared), then finish
/// with a torch for an AND-shaped output or a merge for an OR-shaped one.
/// `docs/superpowers/specs/2026-08-08-cell-type-costs.md` measured this the
/// long way round and reached the same conclusion -- its own table costs
/// `$_NAND_` at 4 gates because it had no merge to finish with; the recipe
/// below spends 2 torches and a merge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expansion {
    pub steps: Vec<Step>,
}

impl Expansion {
    /// The step index whose result is the gate's declared output -- always
    /// the last one.
    pub fn output_step(&self) -> usize {
        self.steps.len() - 1
    }

    /// Every operand of `step`, in order.
    fn operands(step: &Step) -> &[Operand] {
        match step {
            Step::Nor(ops) | Step::Merge(ops) => ops,
        }
    }

    /// Check the two things a recipe has to get right, which no type in this
    /// module can enforce on its own: every operand refers to a real input
    /// or a *strictly earlier* step, and every step's fan-in is one the
    /// placer can actually build (1..=3 for a torch, 2..=3 for a merge --
    /// `place_nor_gate`/`place_merge_gate` have three free input faces, the
    /// fourth being the output).
    ///
    /// Called by `every_expansion_is_well_formed` over every kind, so a
    /// recipe added later cannot quietly declare a 4-input torch.
    pub fn validate(&self, kind: GateKind) {
        assert!(!self.steps.is_empty(), "{kind:?}'s expansion has no steps");
        for (index, step) in self.steps.iter().enumerate() {
            let operands = Self::operands(step);
            match step {
                Step::Nor(_) => assert!(
                    (1..=3).contains(&operands.len()),
                    "{kind:?} step {index}: a torch has 1..=3 inputs, got {}",
                    operands.len()
                ),
                Step::Merge(_) => assert!(
                    (2..=3).contains(&operands.len()),
                    "{kind:?} step {index}: a merge has 2..=3 inputs, got {}",
                    operands.len()
                ),
            }
            for &operand in operands {
                match operand {
                    Operand::Input { pin, .. } => {
                        assert!(pin < kind.arity(), "{kind:?} step {index}: no input {pin}")
                    }
                    Operand::Step(s) => {
                        assert!(s < index, "{kind:?} step {index}: operand step {s} is not strictly earlier")
                    }
                }
            }
        }
    }

    /// Evaluate this recipe on `inputs`, treating a `Nor` step as a NOR and
    /// a `Merge` step as an OR -- which is what those two really compute in
    /// redstone. `lowering`'s tests compare this against
    /// [`GateKind::evaluate`] over every input combination, so a recipe that
    /// does not compute its own gate is caught by construction rather than
    /// by a decoder that silently lights the wrong segment.
    pub fn evaluate(&self, inputs: &[bool]) -> bool {
        let mut values: Vec<bool> = Vec::with_capacity(self.steps.len());
        for step in &self.steps {
            let read = |operand: &Operand| match *operand {
                Operand::Input { pin, polarity: SignalPolarity::Positive } => inputs[pin],
                Operand::Input { pin, polarity: SignalPolarity::Negative } => !inputs[pin],
                Operand::Step(s) => values[s],
            };
            values.push(match step {
                Step::Nor(ops) => !ops.iter().any(&read),
                Step::Merge(ops) => ops.iter().any(&read),
            });
        }
        *values.last().expect("a validated expansion has at least one step")
    }
}

/// The positive-polarity compatibility wrapper for
/// [`expansion_for_polarity`].
pub fn expansion_for(kind: GateKind) -> Expansion {
    expansion_for_polarity(kind, SignalPolarity::Positive)
}

/// How `kind` becomes redstone when its declared output is required on the
/// selected logical rail.
///
/// A recipe can require either rail of each external input. It does not add a
/// generic final inverter: each branch names the NOR/merge topology that
/// actually computes the selected output polarity.
pub fn expansion_for_polarity(kind: GateKind, polarity: SignalPolarity) -> Expansion {
    match polarity {
        SignalPolarity::Positive => positive_expansion_for(kind),
        SignalPolarity::Negative => negative_expansion_for(kind),
    }
}

fn positive_expansion_for(kind: GateKind) -> Expansion {
    use Operand::Step as S;
    use SignalPolarity::{Negative, Positive};

    let positive = |pin| Operand::Input { pin, polarity: Positive };
    let negative = |pin| Operand::Input { pin, polarity: Negative };

    let steps = match kind {
        // The two kinds redstone builds directly: one step, itself.
        GateKind::Nor(arity) => vec![Step::Nor((0..arity).map(positive).collect())],
        GateKind::Or(arity) => vec![Step::Merge((0..arity).map(positive).collect())],

        // BUF: `NOT(!a) == a`.
        GateKind::Buf => vec![Step::Nor(vec![negative(0)])],

        // AND: `a & b == !(!a | !b)`. One torch over the two inverted
        // inputs -- the cheap, AND-shaped case.
        GateKind::And => vec![Step::Nor(vec![negative(0), negative(1)])],

        // NAND: `!(a & b) == !a | !b`. Same two inverters, but the output is
        // OR-shaped, so it finishes in a merge instead of a torch -- and
        // therefore costs no more torch delay than the AND does.
        GateKind::Nand => vec![Step::Merge(vec![negative(0), negative(1)])],

        // ANDNOT: `a & !b == !(!a | b)`. One inverter, one torch -- `b`
        // arrives already in the polarity the torch wants.
        GateKind::AndNot => vec![Step::Nor(vec![negative(0), positive(1)])],

        // ORNOT: `a | !b`. One inverter and a merge: no torch stands on the
        // path from `a` at all.
        GateKind::OrNot => vec![Step::Merge(vec![positive(0), negative(1)])],

        // XOR: `(a & !b) | (!a & b)` -- two ANDNOTs joined by a merge. Each
        // ANDNOT reuses the inverter the other one needs, so the whole thing
        // is two product torches and a merge.
        GateKind::Xor => vec![
            Step::Nor(vec![negative(0), positive(1)]), // a & !b
            Step::Nor(vec![negative(1), positive(0)]), // b & !a
            Step::Merge(vec![S(0), S(1)]),              // their OR
        ],
        // XNOR: the same two products, joined by a torch instead of a merge,
        // which inverts as it joins.
        GateKind::Xnor => vec![
            Step::Nor(vec![negative(0), positive(1)]),
            Step::Nor(vec![negative(1), positive(0)]),
            Step::Nor(vec![S(0), S(1)]),
        ],

        // AOI3: `!((a & b) | c)`. Build `a & b` as an AND, then NOR it with
        // `c` -- the OR and the inversion are the same torch.
        GateKind::Aoi3 => vec![
            Step::Nor(vec![negative(0), negative(1)]), // a & b
            Step::Nor(vec![S(0), positive(2)]),         // !((a & b) | c)
        ],
        // OAI3: `!((a | b) & c) == !(a | b) | !c`. De Morgan turns it into a
        // merge of two torches -- no product term is ever built.
        GateKind::Oai3 => vec![
            Step::Nor(vec![positive(0), positive(1)]), // !(a | b)
            Step::Merge(vec![S(0), negative(2)]),       // their OR
        ],
        // AOI4: `!((a & b) | (c & d))` -- two ANDs, then one torch that ORs
        // and inverts them together.
        GateKind::Aoi4 => vec![
            Step::Nor(vec![negative(0), negative(1)]), // a & b
            Step::Nor(vec![negative(2), negative(3)]), // c & d
            Step::Nor(vec![S(0), S(1)]),
        ],
        // OAI4: `!((a | b) & (c | d)) == !(a | b) | !(c | d)`. Two torches
        // and a merge -- the cheapest four-input cell in this library.
        GateKind::Oai4 => vec![
            Step::Nor(vec![positive(0), positive(1)]),
            Step::Nor(vec![positive(2), positive(3)]),
            Step::Merge(vec![S(0), S(1)]),
        ],

        // MUX (`Y = s ? b : a`): `(b & s) | (a & !s)`, i.e. two ANDNOT-shaped
        // products joined by a merge. `b & s == !(!b | !s)` and
        // `a & !s == !(!a | s)`, so three inverters and two product torches.
        GateKind::Mux => vec![
            Step::Nor(vec![negative(1), negative(2)]), // b & s
            Step::Nor(vec![negative(0), positive(2)]), // a & !s
            Step::Merge(vec![S(0), S(1)]),              // their OR
        ],
        // NMUX: the same two products joined by a torch, which inverts.
        GateKind::Nmux => vec![
            Step::Nor(vec![negative(1), negative(2)]),
            Step::Nor(vec![negative(0), positive(2)]),
            Step::Nor(vec![S(0), S(1)]),
        ],
    };

    Expansion { steps }
}

fn negative_expansion_for(kind: GateKind) -> Expansion {
    use Operand::Step as S;
    use SignalPolarity::{Negative, Positive};

    let positive = |pin| Operand::Input { pin, polarity: Positive };
    let negative = |pin| Operand::Input { pin, polarity: Negative };

    let steps = match kind {
        // The complement of a one-input NOR is a buffer, which still needs a
        // valid hardware step because a merge has at least two inputs.
        GateKind::Nor(1) => vec![Step::Nor(vec![negative(0)])],
        GateKind::Nor(arity) => vec![Step::Merge((0..arity).map(positive).collect())],
        GateKind::Or(arity) => vec![Step::Nor((0..arity).map(positive).collect())],
        GateKind::Buf => vec![Step::Nor(vec![positive(0)])],

        GateKind::And => vec![Step::Merge(vec![negative(0), negative(1)])],
        GateKind::Nand => vec![Step::Nor(vec![negative(0), negative(1)])],
        GateKind::AndNot => vec![Step::Merge(vec![negative(0), positive(1)])],
        GateKind::OrNot => vec![Step::Nor(vec![positive(0), negative(1)])],

        GateKind::Xor => vec![
            Step::Nor(vec![negative(0), positive(1)]),
            Step::Nor(vec![negative(1), positive(0)]),
            Step::Nor(vec![S(0), S(1)]),
        ],
        GateKind::Xnor => vec![
            Step::Nor(vec![negative(0), positive(1)]),
            Step::Nor(vec![negative(1), positive(0)]),
            Step::Merge(vec![S(0), S(1)]),
        ],

        GateKind::Aoi3 => vec![
            Step::Nor(vec![negative(0), negative(1)]),
            Step::Merge(vec![S(0), positive(2)]),
        ],
        GateKind::Oai3 => vec![
            Step::Nor(vec![positive(0), positive(1)]),
            Step::Nor(vec![S(0), negative(2)]),
        ],
        GateKind::Aoi4 => vec![
            Step::Nor(vec![negative(0), negative(1)]),
            Step::Nor(vec![negative(2), negative(3)]),
            Step::Merge(vec![S(0), S(1)]),
        ],
        GateKind::Oai4 => vec![
            Step::Nor(vec![positive(0), positive(1)]),
            Step::Nor(vec![positive(2), positive(3)]),
            Step::Nor(vec![S(0), S(1)]),
        ],

        GateKind::Mux => vec![
            Step::Nor(vec![negative(1), negative(2)]),
            Step::Nor(vec![negative(0), positive(2)]),
            Step::Nor(vec![S(0), S(1)]),
        ],
        GateKind::Nmux => vec![
            Step::Nor(vec![negative(1), negative(2)]),
            Step::Nor(vec![negative(0), positive(2)]),
            Step::Merge(vec![S(0), S(1)]),
        ],
    };

    Expansion { steps }
}

// ---------------------------------------------------------------------
// Realisation cost, derived from the template
// ---------------------------------------------------------------------

/// `place_nor_gate`'s own ground-plan footprint (X*Z, in blocks) for a NOR
/// cell with `arity` inputs -- read straight off its bounding-box
/// computation. This is the one fact a [`Template`]'s cost genuinely cannot
/// be derived without: `Template` carries no positions at all (this
/// module's own doc comment, "Topology carries no positions"), and a
/// footprint is a realisation fact -- how big the support block plus its
/// input sockets actually are on the ground plane -- not a property of the
/// signal graph. Hand-written here, once, so every entry and every
/// [`Expansion`] step derives its *own* area from this one table:
///
/// ```text
///   inputs   NorCell.size (x,y,z)   ground footprint (x * z)
///   1        (2, 1, 3)              6
///   2        (3, 1, 3)              9
///   3        (3, 1, 4)              12
/// ```
///
/// `every_cell_footprint_matches_what_the_placer_actually_reserves` checks
/// each row against a cell really placed into a scratch `World`.
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

/// What one gate costs, in this project's own units: ground-plan area
/// (blocks reserved on the floor) and gate delay (game ticks of torch
/// propagation).
///
/// These two numbers used to exist to be written into
/// `redstone_nor.genlib`'s `GATE`/`PIN` lines, so ABC's technology mapper
/// could choose realisations for us. It no longer chooses (see
/// [`YOSYS_CELL_KINDS`]), and that file is gone -- but the *derivation* is
/// exactly what this library needs for its own choosing, so it stayed and
/// the file did not. [`expansion_cost`] is what makes it live: it prices
/// every gate-level kind straight off the recipe [`expansion_for`] returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RealisationCost {
    pub area: u32,
    pub delay_game_ticks: u64,
}

/// What `kind` costs when realised through its own [`Expansion`]: the sum of
/// every step's reserved ground plan, and the longest torch chain from any
/// input to the output step, in game ticks.
///
/// This is the library's own price list, and unlike the genlib it replaced,
/// nothing outside this crate has to agree with it -- it exists so that
/// choosing between realisations (a thing this library will do, and ABC no
/// longer does for it) has a number to choose on. A merge contributes its
/// reserved rectangle and **no delay at all**, which is the asymmetry every
/// recipe in [`expansion_for`] is built around.
///
/// Two honest limitations, stated rather than hidden:
///
/// - It counts a shared inverter once **per gate**, not once per circuit.
///   `lowering` routes single-input `Nor` steps through
///   `NetlistBuilder::not`, so in a real netlist one `!a` serves every gate
///   that wants it; this number is therefore an upper bound on what a gate
///   adds, not what it always adds.
/// - It prices no wire. Routing is where this compiler's blocks and ticks
///   actually go (`compile`'s own module doc comment), and a positionless
///   library cannot see it. The measured whole-circuit numbers in
///   `tests/verilog_frontend.rs` are the honest total; this is the part a
///   library can know on its own.
/// The positive-polarity compatibility wrapper for
/// [`expansion_cost_for_polarity`].
pub fn expansion_cost(kind: GateKind) -> RealisationCost {
    expansion_cost_for_polarity(kind, SignalPolarity::Positive)
}

/// What `kind` costs when its selected output polarity is realised through
/// its own signed [`Expansion`].
pub fn expansion_cost_for_polarity(kind: GateKind, polarity: SignalPolarity) -> RealisationCost {
    let expansion = expansion_for_polarity(kind, polarity);

    let mut area = 0u32;
    // Torch depth of each step's result: how many torches stand on the
    // longest path from any input to it. A merge adds none.
    let mut depth: Vec<u32> = Vec::with_capacity(expansion.steps.len());

    for step in &expansion.steps {
        let operands = Expansion::operands(step);
        let upstream = operands
            .iter()
            .map(|operand| match *operand {
                Operand::Input { .. } => 0,
                Operand::Step(s) => depth[s],
            })
            .max()
            .expect("a validated step has at least one operand");
        match step {
            Step::Nor(_) => {
                area += nor_footprint_area(operands.len());
                depth.push(upstream + 1);
            }
            Step::Merge(_) => {
                area += merge_footprint_area(operands.len());
                depth.push(upstream);
            }
        }
    }

    let torches = *depth.last().expect("a validated expansion has at least one step");
    RealisationCost {
        area,
        delay_game_ticks: crate::redstone::simulator::component::TORCH_DELAY_GAME_TICKS * torches as u64,
    }
}

/// Derive `entry`'s cost from its own template: total area is the sum
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
/// This prices a *technique* (a [`LibraryEntry`]); [`expansion_cost`] prices
/// a *gate kind* (an [`Expansion`]). They agree wherever both apply --
/// `entry_cost_and_expansion_cost_agree_for_every_realisable_kind` holds
/// them to it -- which is the check that used to be
/// `genlib_numbers_match_what_the_topology_library_derives`, now between two
/// things inside this library rather than between this library and a file
/// ABC read.
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
/// wrong.** The reasoning behind it went: this model prices gate bodies
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
/// Zero was not merely imprecise, it was unbounded: a cell priced at zero is
/// one whoever is choosing will buy any number of, and every one bought
/// costs real blocks. Measured through the real compiler back when ABC was
/// the one choosing, that is exactly what happened -- the Verilog-derived
/// seven-segment decoder went from 37 gates / 8130 blocks to 50 / 10668 when
/// `OR2`/`OR3` entered the price list at area 0. ABC no longer chooses, but
/// this library now does (see [`expansion_for`]), so the lesson survives the
/// mapper that taught it.
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
/// in the whole compiled circuit already has. This model has never priced
/// that repeater for `Nor`/`Buf` either, so not pricing it here is the rule
/// being applied, not suspended.
///
/// That is also why **one number still honestly prices both entries.** Bare
/// and isolated differ only in what terminates each branch's route, which
/// is route cost on both sides of the comparison; the cell itself --
/// `place_merge_gate`'s reserved rectangle -- is byte-for-byte the same
/// either way, and neither ever places a torch. So the promise stays one
/// this library can keep: choosing an OR never costs more reserved ground
/// or more torch delay than this says, whichever realisation each branch
/// ends up with.
pub fn entry_cost(kind: GateKind, entry: &LibraryEntry) -> RealisationCost {
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
        assert_eq!(primitive, Primitive::Torch, "entry_cost only knows how to price a Torch node");
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
    fn gate_kind_for_yosys_cell_maps_the_simple_cells_abc_actually_emits() {
        // The seven types `abc` emitted on the seven-segment decoder, plus
        // the two redstone builds directly.
        assert_eq!(gate_kind_for_yosys_cell("$_NOT_"), Some(GateKind::Nor(1)), "a 1-input NOR *is* an inverter");
        assert_eq!(gate_kind_for_yosys_cell("$_NOR_"), Some(GateKind::Nor(2)));
        assert_eq!(gate_kind_for_yosys_cell("$_OR_"), Some(GateKind::Or(2)), "an OR is a wire merge, not a gate");
        assert_eq!(gate_kind_for_yosys_cell("$_AND_"), Some(GateKind::And));
        assert_eq!(gate_kind_for_yosys_cell("$_NAND_"), Some(GateKind::Nand));
        assert_eq!(gate_kind_for_yosys_cell("$_ANDNOT_"), Some(GateKind::AndNot));
        assert_eq!(gate_kind_for_yosys_cell("$_ORNOT_"), Some(GateKind::OrNot));
        assert_eq!(gate_kind_for_yosys_cell("$_MUX_"), Some(GateKind::Mux));
        assert_eq!(gate_kind_for_yosys_cell("$_BUF_"), Some(GateKind::Buf));
    }

    /// Every cell type this table names must have an [`Expansion`] whose
    /// arity matches, or the frontend would map a cell to a kind and then
    /// find nothing to build it from.
    #[test]
    fn every_known_yosys_cell_expands_and_its_arity_is_the_kinds_own() {
        for (cell_type, kind) in known_yosys_cell_types() {
            let expansion = expansion_for(kind);
            expansion.validate(kind);
            assert!(!expansion.steps.is_empty(), "{cell_type} ({kind:?}) expands to nothing");
        }
    }

    #[test]
    fn gate_kind_for_yosys_cell_has_no_mapping_for_constants_state_or_tri_state() {
        // Yosys's constant drivers: this library has no way to realize a
        // hard-wired signal in real redstone, so they get no GateKind on
        // purpose -- see YOSYS_CELL_KINDS's own doc comment.
        assert_eq!(gate_kind_for_yosys_cell("$__ZERO"), None);
        assert_eq!(gate_kind_for_yosys_cell("$__ONE"), None);
        assert_eq!(gate_kind_for_yosys_cell("$_DFF_P_"), None, "sequential logic is a later task, not this one");
        assert_eq!(gate_kind_for_yosys_cell("$_DLATCH_P_"), None);
        assert_eq!(gate_kind_for_yosys_cell("$_TBUF_"), None, "there is no tri-state in redstone");
        assert_eq!(gate_kind_for_yosys_cell("$_and_"), None, "the table is exact-match, not case-insensitive");
        // The genlib-era names. `abc -genlib redstone_nor.genlib` is gone,
        // so a netlist can never contain these again; nothing should
        // silently accept one.
        assert_eq!(gate_kind_for_yosys_cell("NOR2"), None);
        assert_eq!(gate_kind_for_yosys_cell("OR2"), None);
        assert_eq!(gate_kind_for_yosys_cell("BUF"), None);
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
    fn entry_cost_of_every_nor_arity_is_its_own_footprint_at_one_torch_delay() {
        for arity in 1..=3 {
            let cost = entry_cost(GateKind::Nor(arity), &nor_entry(arity));
            assert_eq!(cost.area, nor_footprint_area(arity), "arity {arity}");
            assert_eq!(cost.delay_game_ticks, crate::redstone::simulator::component::TORCH_DELAY_GAME_TICKS, "arity {arity}");
        }
    }

    #[test]
    fn entry_cost_of_buf_is_two_nor1_footprints_at_two_torch_delays() {
        let cost = entry_cost(GateKind::Buf, &buf_entry());
        assert_eq!(cost.area, 2 * nor_footprint_area(1));
        assert_eq!(cost.delay_game_ticks, 2 * crate::redstone::simulator::component::TORCH_DELAY_GAME_TICKS);
    }

    /// Both footprint tables are the one realisation fact `entry_cost`
    /// cannot derive from a positionless `Template`, so both are checked
    /// against what the placer actually reserves -- by placing one cell of
    /// each kind and arity into a scratch `World` and reading `NorCell.size`
    /// back -- the measurement the deleted genlib's derivation comment
    /// claimed had been done by hand, now done by the test suite. Without this, `nor_footprint_area`
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
    fn entry_cost_of_a_bare_or_merge_is_its_reserved_ground_plan_at_no_torch_delay() {
        // Not zero area: a merge places no primitive, but `place_merge_gate`
        // still reserves a rectangle for it (see `merge_footprint_area`).
        // Zero *delay* is the real, measured figure -- a bare merge settles
        // instantaneously, there being no active component in it at all (see
        // the referenced cost-table spec, and `tests/cell_type_costs.rs`'s
        // `or_is_a_free_wire_merge_when_nothing_else_shares_the_branch`).
        for arity in 2..=3 {
            let cost = entry_cost(GateKind::Or(arity), &or_bare_entry(arity));
            assert_eq!(cost.area, merge_footprint_area(arity), "arity {arity}");
            assert_eq!(cost.delay_game_ticks, 0, "arity {arity}");
        }
    }

    #[test]
    fn entry_cost_of_an_isolated_or_merge_is_exactly_the_bare_entrys() {
        // Not a placeholder, and not a different (understated) number from
        // the bare entry's -- see `entry_cost`'s own doc comment, "OR: one
        // number, honestly standing for two realisations": the two entries
        // reserve the identical ground plan (`place_merge_gate` does not
        // know which one it is building), and the isolating repeater is the
        // same mandatory route-terminating repeater every socket in this
        // compiler already pays, never a cost this entry's own cell adds on
        // top.
        for arity in 2..=3 {
            let bare = entry_cost(GateKind::Or(arity), &or_bare_entry(arity));
            let isolated = entry_cost(GateKind::Or(arity), &or_isolated_entry(arity));
            assert_eq!(isolated, bare, "arity {arity}: one number has to price both realisations");
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
                let cost = entry_cost(kind, entry);
                assert!(cost.area > 0, "{kind:?}'s entry `{}` is priced at zero area", entry.name);
            }
        }
    }

    // -----------------------------------------------------------------
    // Expansions: the decision `abc -genlib` used to make for us
    // -----------------------------------------------------------------

    /// Every kind this module can name. Written out rather than derived so
    /// that adding a variant to `GateKind` without deciding how it becomes
    /// redstone fails to compile here (the `match` below is exhaustive).
    fn every_gate_kind() -> Vec<GateKind> {
        let mut kinds: Vec<GateKind> = (1..=3).map(GateKind::Nor).chain((2..=3).map(GateKind::Or)).collect();
        // Exhaustive on purpose: a new variant must be added here too.
        for kind in [
            GateKind::Buf,
            GateKind::And,
            GateKind::Nand,
            GateKind::Xor,
            GateKind::Xnor,
            GateKind::AndNot,
            GateKind::OrNot,
            GateKind::Aoi3,
            GateKind::Oai3,
            GateKind::Aoi4,
            GateKind::Oai4,
            GateKind::Mux,
            GateKind::Nmux,
        ] {
            match kind {
                GateKind::Nor(_) | GateKind::Or(_) => unreachable!("listed separately, with their arities"),
                GateKind::Buf
                | GateKind::And
                | GateKind::Nand
                | GateKind::Xor
                | GateKind::Xnor
                | GateKind::AndNot
                | GateKind::OrNot
                | GateKind::Aoi3
                | GateKind::Oai3
                | GateKind::Aoi4
                | GateKind::Oai4
                | GateKind::Mux
                | GateKind::Nmux => kinds.push(kind),
            }
        }
        kinds
    }

    #[test]
    fn every_expansion_is_well_formed() {
        for kind in every_gate_kind() {
            for polarity in [SignalPolarity::Positive, SignalPolarity::Negative] {
                expansion_for_polarity(kind, polarity).validate(kind);
            }
        }
    }

    /// The property that makes a recipe *right*: expanded, it computes the
    /// gate it claims to. Checked exhaustively over every input combination
    /// of every kind, against [`GateKind::evaluate`] -- an independent
    /// statement of each gate's boolean function, not a second copy of the
    /// recipe.
    #[test]
    fn every_positive_expansion_computes_its_own_gates_truth_table() {
        for kind in every_gate_kind() {
            let arity = kind.arity();
            let expansion = expansion_for_polarity(kind, SignalPolarity::Positive);
            for bits in 0..(1u32 << arity) {
                let inputs: Vec<bool> = (0..arity).map(|i| (bits >> i) & 1 == 1).collect();
                assert_eq!(
                    expansion.evaluate(&inputs),
                    kind.evaluate(&inputs),
                    "{kind:?} on {inputs:?}: the expansion does not compute the gate"
                );
            }
        }
    }

    /// Negative recipes are first-class topologies, not a positive recipe
    /// with a final inverter appended by a caller.
    #[test]
    fn every_negative_expansion_computes_the_complement_of_its_gate() {
        for kind in every_gate_kind() {
            let arity = kind.arity();
            for bits in 0..(1u32 << arity) {
                let inputs: Vec<bool> = (0..arity).map(|i| (bits >> i) & 1 == 1).collect();
                assert_eq!(
                    expansion_for_polarity(kind, SignalPolarity::Negative).evaluate(&inputs),
                    !kind.evaluate(&inputs),
                    "{kind:?} on {inputs:?}: the negative expansion does not compute the complement"
                );
            }
        }
    }

    #[test]
    fn nand_is_the_negative_realisation_of_and() {
        assert_eq!(
            expansion_cost_for_polarity(GateKind::And, SignalPolarity::Negative),
            expansion_cost(GateKind::Nand),
        );
    }

    #[test]
    fn and_selects_signed_torch_and_merge_topologies() {
        let negative_inputs = vec![
            Operand::Input { pin: 0, polarity: SignalPolarity::Negative },
            Operand::Input { pin: 1, polarity: SignalPolarity::Negative },
        ];
        assert_eq!(
            expansion_for_polarity(GateKind::And, SignalPolarity::Positive).steps,
            vec![Step::Nor(negative_inputs.clone())],
        );
        assert_eq!(
            expansion_for_polarity(GateKind::And, SignalPolarity::Negative).steps,
            vec![Step::Merge(negative_inputs)],
        );
    }

    /// The two kinds redstone builds directly expand to exactly themselves,
    /// one step -- which is what makes `lowering::lower` the identity on a
    /// netlist that is already realisable (every hand-written circuit in
    /// this project), and therefore what keeps the four reference circuits
    /// byte-for-byte where they were.
    #[test]
    fn a_realisable_kinds_expansion_is_one_step_of_itself() {
        for arity in 1..=3 {
            assert_eq!(
                expansion_for(GateKind::Nor(arity)).steps,
                vec![Step::Nor(
                    (0..arity)
                        .map(|pin| Operand::Input { pin, polarity: SignalPolarity::Positive })
                        .collect()
                )]
            );
        }
        for arity in 2..=3 {
            assert_eq!(
                expansion_for(GateKind::Or(arity)).steps,
                vec![Step::Merge(
                    (0..arity)
                        .map(|pin| Operand::Input { pin, polarity: SignalPolarity::Positive })
                        .collect()
                )]
            );
        }
    }

    /// A signed recipe can consume the opposite rail directly; lowering
    /// materialises that rail through its shared inverter cache.
    #[test]
    fn positive_buf_reads_the_negative_input_rail() {
        let expansion = expansion_for(GateKind::Buf);
        assert_eq!(
            expansion.steps,
            vec![Step::Nor(vec![Operand::Input {
                pin: 0,
                polarity: SignalPolarity::Negative,
            }])]
        );
    }

    /// Wherever both cost models apply -- the kinds with a `Library` entry
    /// *and* an expansion -- they must agree. This is the check that used to
    /// hold `redstone_nor.genlib`'s hand-written `GATE` numbers to
    /// `genlib_cost`'s derivation, now entirely inside this library.
    #[test]
    fn entry_cost_and_expansion_cost_agree_for_every_realisable_kind() {
        let library = Library::default_library();
        for arity in 1..=3 {
            let kind = GateKind::Nor(arity);
            let entry = library.choose(kind).expect("registered");
            assert_eq!(entry_cost(kind, entry), expansion_cost(kind), "{kind:?}");
        }
        for arity in 2..=3 {
            let kind = GateKind::Or(arity);
            for entry in library.entries_for(kind) {
                assert_eq!(entry_cost(kind, entry), expansion_cost(kind), "{kind:?} / `{}`", entry.name);
            }
        }
    }

    /// The asymmetry every recipe is built around, stated as a test rather
    /// than only as prose: an OR-shaped output finished in a merge costs no
    /// torch delay at all, while the same shape finished in a torch costs
    /// one more level than its AND-shaped sibling.
    #[test]
    fn finishing_in_a_merge_costs_no_gate_delay_and_finishing_in_a_torch_costs_one() {
        let torch = crate::redstone::simulator::component::TORCH_DELAY_GAME_TICKS;

        // NAND and AND consume the same negative rails; NAND finishes in a
        // merge while AND finishes in a torch.
        assert_eq!(expansion_cost(GateKind::Nand).delay_game_ticks, 0);
        assert_eq!(expansion_cost(GateKind::And).delay_game_ticks, torch);

        // XOR and XNOR are the same two product torches; XOR joins them with
        // a merge, XNOR with a torch.
        assert_eq!(expansion_cost(GateKind::Xor).delay_game_ticks, torch);
        assert_eq!(expansion_cost(GateKind::Xnor).delay_game_ticks, 2 * torch);

        // OAI4 is four inputs for two torch levels, because De Morgan turns
        // its whole output stage into a merge.
        assert_eq!(expansion_cost(GateKind::Oai4).delay_game_ticks, torch);
        assert_eq!(expansion_cost(GateKind::Aoi4).delay_game_ticks, 2 * torch);
    }

    #[test]
    fn wire_names_round_trip_for_every_kind() {
        for kind in every_gate_kind() {
            let name = kind.wire_name();
            assert_eq!(
                GateKind::from_wire_name(name, kind.arity()),
                Some(kind),
                "`{name}` must read back as the kind that wrote it"
            );
        }
        assert_eq!(GateKind::from_wire_name("nonsense", 2), None);
    }
}
