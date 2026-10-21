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

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};


// ---------------------------------------------------------------------
// The primitive vocabulary
// ---------------------------------------------------------------------


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
/// NOR's fan-in is 0..=3 (zero inputs give an isolated constant-one torch), a
/// merge's 2..=3 (`place_merge_gate` shares the same three input faces), so
/// for those two the arity is part of the kind: it selects the [`Template`].
/// `Nor(1)` is the same gate the spec calls `NOT` (a NOR gate with one input
/// inverts exactly like an inverter); [`GateKind::NOT`] names that case for
/// readability at call sites that mean it. Every gate-level variant has a
/// fixed arity instead (Yosys's simple cells are all fixed-arity), so it
/// carries none.
///
/// `Buf` is not logic at all -- it is what a Yosys `$_BUF_` cell, and a bare
/// `assign out = in;` with no cell in between, realises as two chained
/// `Nor(1)` torches, `NOT(NOT(x)) == x`. Its signed recipe asks for `!x` and
/// applies the second torch; lowering an all-positive source materialises
/// that requested input rail with the first torch. It is a gate-level kind
/// like the rest, with its own [`Expansion`] and [`Template`].
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
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
    /// `$_DFF_P_`. A positive-edge-triggered flip-flop with pin order
    /// `D, C` and stateful output `Q`.
    ///
    /// Unlike every combinational kind above, this is a dependency boundary:
    /// its output is available to the next combinational cycle before its D
    /// input is evaluated.  It deliberately has no boolean `evaluate`
    /// implementation; a DFF needs prior state and a clock transition.
    DffPosedge,
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
            GateKind::And
            | GateKind::Nand
            | GateKind::Xor
            | GateKind::Xnor
            | GateKind::AndNot
            | GateKind::OrNot
            | GateKind::DffPosedge => Some(2),
            GateKind::Aoi3 | GateKind::Oai3 | GateKind::Mux | GateKind::Nmux => Some(3),
            GateKind::Aoi4 | GateKind::Oai4 => Some(4),
        }
    }

    /// How many inputs a gate of this kind has.
    pub fn arity(self) -> usize {
        match self {
            GateKind::Nor(arity) | GateKind::Or(arity) => arity,
            other => other
                .fixed_arity()
                .expect("every non-Nor/Or kind is fixed-arity"),
        }
    }

    /// Whether this exact gate kind may be instantiated with `arity` input
    /// signals.  Keep this distinct from [`Self::arity`]: callers validating
    /// an untrusted netlist must compare a declared input count rather than
    /// assume it was already valid.
    pub fn accepts_arity(self, arity: usize) -> bool {
        match self {
            GateKind::Nor(expected) | GateKind::Or(expected) => expected == arity,
            other => other.fixed_arity() == Some(arity),
        }
    }

    /// Whether redstone builds this kind directly -- `Nor` (a torch) or `Or`
    /// (a wire merge). These are the only two kinds `compile` can place and
    /// `primitive_graph::expand` can turn into primitives; everything else
    /// has to go through `lowering::lower` first.
    pub fn is_realisable(self) -> bool {
        matches!(self, GateKind::Nor(_) | GateKind::Or(0..=3))
    }

    /// Whether this gate owns state and therefore cuts dependency edges for
    /// combinational ordering.  A cycle is legal only when it crosses one of
    /// these elements; a loop containing only combinational gates remains an
    /// error.
    pub fn is_sequential(self) -> bool {
        matches!(self, GateKind::DffPosedge)
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
            GateKind::DffPosedge => "dff_p",
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
            "dff_p" => GateKind::DffPosedge,
            _ => return None,
        })
    }

    /// Evaluate this kind's own boolean function on `inputs` (in pin order).
    /// The one place the *combinational meaning* of each gate-level kind is
    /// written down as executable truth rather than as a comment -- which is
    /// what lets `lowering`'s tests check an expansion against the gate it
    /// claims to realise, exhaustively, instead of against a second
    /// hand-derivation of the same formula. Stateful kinds intentionally
    /// panic: their meaning includes previous state and a transition.
    ///
    /// Panics if `inputs.len()` disagrees with [`GateKind::arity`].
    pub fn evaluate(self, inputs: &[bool]) -> bool {
        assert_eq!(
            inputs.len(),
            self.arity(),
            "{self:?} takes {} input(s)",
            self.arity()
        );
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
            GateKind::Nmux => !(if inputs[2] { inputs[1] } else { inputs[0] }),
            GateKind::DffPosedge => panic!("a DFF needs its previous state and a clock transition"),
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
/// - `$_DLATCH_*_`, `$_SR_*_` and every sequential cell other than
///   `$_DFF_P_`. A cell with state must have a named topology entry; it must
///   never silently enter the combinational library. `$_DFF_P_` is retained
///   as a state boundary and receives its physical topology in the sequential
///   compiler phase; it is not a combinational expansion.
/// - `$_TBUF_`: redstone does not represent a tri-state cell.
/// - `$__ZERO`/`$__ONE`: literal output bits are materialized by the JSON
///   reader as Nor(0), or its inverter, rather than as named Yosys cells.
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
    ("$_DFF_P_", GateKind::DffPosedge),
];

/// The [`GateKind`] this library realises Yosys cell type `cell_type` as, or
/// `None` if this library has no realisation for a cell by that name --
/// which the frontend treats as a hard, loud error naming the cell rather
/// than a silent skip (a dropped cell is a netlist that still compiles, to
/// the wrong circuit).
pub fn gate_kind_for_yosys_cell(cell_type: &str) -> Option<GateKind> {
    YOSYS_CELL_KINDS
        .iter()
        .find(|&&(name, _)| name == cell_type)
        .map(|&(_, kind)| kind)
}

/// Every Yosys cell type [`gate_kind_for_yosys_cell`] knows, for tests and
/// diagnostics that want to say what *is* supported rather than only reject
/// what is not.
pub fn known_yosys_cell_types() -> impl Iterator<Item = (&'static str, GateKind)> {
    YOSYS_CELL_KINDS.iter().copied()
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
    Input {
        pin: usize,
        polarity: SignalPolarity,
    },
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
    /// Negative external rails this recipe's lowering must materialise before
    /// it builds any step. Most recipes resolve rails at their first use;
    /// XOR/XNOR need both input inverters first to retain the established
    /// lowered gate sequence and generated names.
    pub(crate) pre_materialize_negative_inputs: Vec<usize>,
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
    /// Called by `every_expansion_is_well_formed` over every combinational
    /// kind, so a recipe added later cannot quietly declare a 4-input torch.
    /// Stateful entries have a transition contract and are tested without
    /// calling this function.
    pub fn validate(&self, kind: GateKind) {
        assert!(!self.steps.is_empty(), "{kind:?}'s expansion has no steps");
        for (index, step) in self.steps.iter().enumerate() {
            let operands = Self::operands(step);
            match step {
                Step::Nor(_) => assert!(
                    (0..=3).contains(&operands.len()),
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
                        assert!(
                            s < index,
                            "{kind:?} step {index}: operand step {s} is not strictly earlier"
                        )
                    }
                }
            }
        }
        for (index, &pin) in self.pre_materialize_negative_inputs.iter().enumerate() {
            assert!(
                pin < kind.arity(),
                "{kind:?}: pre-materialized input {pin} is out of range"
            );
            assert!(
                !self.pre_materialize_negative_inputs[..index].contains(&pin),
                "{kind:?}: pre-materialized input {pin} appears more than once"
            );
            assert!(
                self.steps.iter().flat_map(Self::operands).any(|operand| {
                    matches!(
                        operand,
                        Operand::Input { pin: required_pin, polarity: SignalPolarity::Negative }
                            if *required_pin == pin
                    )
                }),
                "{kind:?}: pre-materialized input {pin} is not required as a negative rail"
            );
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
                Operand::Input {
                    pin,
                    polarity: SignalPolarity::Positive,
                } => inputs[pin],
                Operand::Input {
                    pin,
                    polarity: SignalPolarity::Negative,
                } => !inputs[pin],
                Operand::Step(s) => values[s],
            };
            values.push(match step {
                Step::Nor(ops) => !ops.iter().any(&read),
                Step::Merge(ops) => ops.iter().any(&read),
            });
        }
        *values
            .last()
            .expect("a validated expansion has at least one step")
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
    if let GateKind::Or(arity) = kind {
        if arity > 3 {
            return wide_or_expansion(arity, polarity);
        }
    }
    match polarity {
        SignalPolarity::Positive => positive_expansion_for(kind),
        SignalPolarity::Negative => negative_expansion_for(kind),
    }
}

// A wide Boolean OR needs isolated, bounded-fan-in stages. Preserve the
// reference builders and the existing two-/three-input merge recipes.
fn wide_or_expansion(arity: usize, polarity: SignalPolarity) -> Expansion {
    let mut steps = Vec::new();
    let mut level: Vec<_> = (0..arity)
        .map(|pin| Operand::Input {
            pin,
            polarity: SignalPolarity::Positive,
        })
        .collect();
    while level.len() > 3 {
        level = level
            .chunks(3)
            .map(|group| {
                if group.len() == 1 {
                    return group[0];
                }
                let nor = steps.len();
                steps.push(Step::Nor(group.to_vec()));
                steps.push(Step::Nor(vec![Operand::Step(nor)]));
                Operand::Step(nor + 1)
            })
            .collect();
    }
    let nor = steps.len();
    steps.push(Step::Nor(level));
    if polarity == SignalPolarity::Positive {
        steps.push(Step::Nor(vec![Operand::Step(nor)]));
    }
    Expansion {
        steps,
        pre_materialize_negative_inputs: Vec::new(),
    }
}

fn positive_expansion_for(kind: GateKind) -> Expansion {
    use Operand::Step as S;
    use SignalPolarity::{Negative, Positive};

    let positive = |pin| Operand::Input {
        pin,
        polarity: Positive,
    };
    let negative = |pin| Operand::Input {
        pin,
        polarity: Negative,
    };

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
            Step::Merge(vec![S(0), S(1)]),             // their OR
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
            Step::Nor(vec![S(0), positive(2)]),        // !((a & b) | c)
        ],
        // OAI3: `!((a | b) & c) == !(a | b) | !c`. De Morgan turns it into a
        // merge of two torches -- no product term is ever built.
        GateKind::Oai3 => vec![
            Step::Nor(vec![positive(0), positive(1)]), // !(a | b)
            Step::Merge(vec![S(0), negative(2)]),      // their OR
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
            Step::Merge(vec![S(0), S(1)]),             // their OR
        ],
        // NMUX: the same two products joined by a torch, which inverts.
        GateKind::Nmux => vec![
            Step::Nor(vec![negative(1), negative(2)]),
            Step::Nor(vec![negative(0), positive(2)]),
            Step::Nor(vec![S(0), S(1)]),
        ],
        GateKind::DffPosedge => {
            panic!("DFFs are stateful topology entries, not combinational expansions")
        }
    };

    let pre_materialize_negative_inputs = match kind {
        GateKind::Xor | GateKind::Xnor => vec![0, 1],
        _ => Vec::new(),
    };
    Expansion {
        steps,
        pre_materialize_negative_inputs,
    }
}

fn negative_expansion_for(kind: GateKind) -> Expansion {
    use Operand::Step as S;
    use SignalPolarity::{Negative, Positive};

    let positive = |pin| Operand::Input {
        pin,
        polarity: Positive,
    };
    let negative = |pin| Operand::Input {
        pin,
        polarity: Negative,
    };

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
        GateKind::DffPosedge => {
            panic!("DFFs are stateful topology entries, not combinational expansions")
        }
    };

    let pre_materialize_negative_inputs = match kind {
        GateKind::Xor | GateKind::Xnor => vec![0, 1],
        _ => Vec::new(),
    };
    Expansion {
        steps,
        pre_materialize_negative_inputs,
    }
}

// ---------------------------------------------------------------------
// Realisation cost, derived from the template
// ---------------------------------------------------------------------



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

/// The ground plan a realisable gate's tile reserves (`compile::construct`):
/// the body, and four cells -- repeater, approach and their floors -- per
/// input stub.
/// `None` for a kind or arity no tile exists for.
pub fn realisable_area(kind: GateKind) -> Option<u32> {
    match kind {
        GateKind::Nor(arity) if arity <= 3 => Some(5 + 4 * arity as u32),
        GateKind::Or(arity) if (2..=3).contains(&arity) => Some(4 + 4 * arity as u32),
        _ => None,
    }
}

fn nor_footprint_area(arity: usize) -> u32 {
    realisable_area(GateKind::Nor(arity)).expect("a NOR expansion step has at most three inputs")
}

fn merge_footprint_area(arity: usize) -> u32 {
    realisable_area(GateKind::Or(arity)).expect("a merge expansion step has two or three inputs")
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
///
/// The positive-polarity compatibility wrapper for
/// [`expansion_cost_for_polarity`].
pub fn expansion_cost(kind: GateKind) -> RealisationCost {
    expansion_cost_for_polarity(kind, SignalPolarity::Positive)
}

/// What `kind` costs when its selected output polarity is realised through
/// its own signed [`Expansion`].
pub fn expansion_cost_for_polarity(kind: GateKind, polarity: SignalPolarity) -> RealisationCost {
    let expansion = expansion_for_polarity(kind, polarity);

    // The source representation carries only positive physical rails today,
    // so each distinct negative external operand requires the same cached
    // NOR1 `lowering` materialises. Count it once per pin for this gate; a
    // whole circuit can share that inverter across gates, as documented
    // above.
    let negative_inputs: BTreeSet<usize> = expansion
        .steps
        .iter()
        .flat_map(Expansion::operands)
        .filter_map(|operand| match *operand {
            Operand::Input {
                pin,
                polarity: SignalPolarity::Negative,
            } => Some(pin),
            Operand::Input { .. } | Operand::Step(_) => None,
        })
        .collect();
    let mut area = negative_inputs.len() as u32 * nor_footprint_area(1);
    // Torch depth of each step's result: how many torches stand on the
    // longest path from any input to it. A merge adds none.
    let mut depth: Vec<u32> = Vec::with_capacity(expansion.steps.len());

    for step in &expansion.steps {
        let operands = Expansion::operands(step);
        let upstream = operands
            .iter()
            .map(|operand| match *operand {
                Operand::Input {
                    polarity: SignalPolarity::Positive,
                    ..
                } => 0,
                Operand::Input {
                    polarity: SignalPolarity::Negative,
                    ..
                } => 1,
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

    let torches = *depth
        .last()
        .expect("a validated expansion has at least one step");
    RealisationCost {
        area,
        delay_game_ticks: crate::redstone::simulator::component::TORCH_DELAY_GAME_TICKS
            * torches as u64,
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_is_nor_of_one() {
        assert_eq!(GateKind::NOT, GateKind::Nor(1));
    }

    #[test]
    fn gate_kind_for_yosys_cell_maps_the_simple_cells_abc_actually_emits() {
        // The seven types `abc` emitted on the seven-segment decoder, plus
        // the two redstone builds directly.
        assert_eq!(
            gate_kind_for_yosys_cell("$_NOT_"),
            Some(GateKind::Nor(1)),
            "a 1-input NOR *is* an inverter"
        );
        assert_eq!(gate_kind_for_yosys_cell("$_NOR_"), Some(GateKind::Nor(2)));
        assert_eq!(
            gate_kind_for_yosys_cell("$_OR_"),
            Some(GateKind::Or(2)),
            "an OR is a wire merge, not a gate"
        );
        assert_eq!(gate_kind_for_yosys_cell("$_AND_"), Some(GateKind::And));
        assert_eq!(gate_kind_for_yosys_cell("$_NAND_"), Some(GateKind::Nand));
        assert_eq!(
            gate_kind_for_yosys_cell("$_ANDNOT_"),
            Some(GateKind::AndNot)
        );
        assert_eq!(gate_kind_for_yosys_cell("$_ORNOT_"), Some(GateKind::OrNot));
        assert_eq!(gate_kind_for_yosys_cell("$_MUX_"), Some(GateKind::Mux));
        assert_eq!(gate_kind_for_yosys_cell("$_BUF_"), Some(GateKind::Buf));
        assert_eq!(
            gate_kind_for_yosys_cell("$_DFF_P_"),
            Some(GateKind::DffPosedge)
        );
    }

    /// Every combinational cell type this table names must have an
    /// [`Expansion`] whose arity matches. Stateful entries are represented
    /// as named boundaries, not passed to a combinational recipe.
    #[test]
    fn every_known_yosys_cell_expands_and_its_arity_is_the_kinds_own() {
        for (cell_type, kind) in known_yosys_cell_types() {
            if kind.is_sequential() {
                assert_eq!(
                    kind,
                    GateKind::DffPosedge,
                    "only the named DFF is stateful today"
                );
                continue;
            }
            let expansion = expansion_for(kind);
            expansion.validate(kind);
            assert!(
                !expansion.steps.is_empty(),
                "{cell_type} ({kind:?}) expands to nothing"
            );
        }
    }

    #[test]
    fn gate_kind_for_yosys_cell_has_no_mapping_for_constants_state_or_tri_state() {
        // Yosys's constant drivers: this library has no way to realize a
        // hard-wired signal in real redstone, so they get no GateKind on
        // purpose -- see YOSYS_CELL_KINDS's own doc comment.
        assert_eq!(gate_kind_for_yosys_cell("$__ZERO"), None);
        assert_eq!(gate_kind_for_yosys_cell("$__ONE"), None);
        assert_eq!(gate_kind_for_yosys_cell("$_DLATCH_P_"), None);
        assert_eq!(
            gate_kind_for_yosys_cell("$_TBUF_"),
            None,
            "there is no tri-state in redstone"
        );
        assert_eq!(
            gate_kind_for_yosys_cell("$_and_"),
            None,
            "the table is exact-match, not case-insensitive"
        );
        // The genlib-era names. `abc -genlib redstone_nor.genlib` is gone,
        // so a netlist can never contain these again; nothing should
        // silently accept one.
        assert_eq!(gate_kind_for_yosys_cell("NOR2"), None);
        assert_eq!(gate_kind_for_yosys_cell("OR2"), None);
        assert_eq!(gate_kind_for_yosys_cell("BUF"), None);
    }

    // -----------------------------------------------------------------
    // OR: the first gate type that realises to no primitive at all, and the
    // first with genuinely alternative entries.
    // -----------------------------------------------------------------

    // -----------------------------------------------------------------
    // Expansions: the decision `abc -genlib` used to make for us
    // -----------------------------------------------------------------

    /// Every kind this module can name. Written out rather than derived so
    /// that adding a variant to `GateKind` without deciding how it becomes
    /// redstone fails to compile here (the `match` below is exhaustive).
    /// Stateful kinds are deliberately excluded: they have no
    /// combinational expansion.
    fn every_gate_kind() -> Vec<GateKind> {
        let mut kinds: Vec<GateKind> = (1..=3)
            .map(GateKind::Nor)
            .chain((2..=3).map(GateKind::Or))
            .collect();
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
                GateKind::Nor(_) | GateKind::Or(_) => {
                    unreachable!("listed separately, with their arities")
                }
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
                GateKind::DffPosedge => {
                    unreachable!("stateful kinds do not have combinational expansions")
                }
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
            Operand::Input {
                pin: 0,
                polarity: SignalPolarity::Negative,
            },
            Operand::Input {
                pin: 1,
                polarity: SignalPolarity::Negative,
            },
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
                        .map(|pin| Operand::Input {
                            pin,
                            polarity: SignalPolarity::Positive
                        })
                        .collect()
                )]
            );
        }
        for arity in 2..=3 {
            assert_eq!(
                expansion_for(GateKind::Or(arity)).steps,
                vec![Step::Merge(
                    (0..arity)
                        .map(|pin| Operand::Input {
                            pin,
                            polarity: SignalPolarity::Positive
                        })
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

    #[test]
    fn positive_buf_cost_includes_its_required_negative_input_rail() {
        let torch = crate::redstone::simulator::component::TORCH_DELAY_GAME_TICKS;
        assert_eq!(
            expansion_cost(GateKind::Buf),
            RealisationCost {
                area: 2 * nor_footprint_area(1),
                delay_game_ticks: 2 * torch,
            }
        );
    }

    #[test]
    fn positive_and_cost_counts_each_required_negative_input_rail() {
        let torch = crate::redstone::simulator::component::TORCH_DELAY_GAME_TICKS;
        assert_eq!(
            expansion_cost(GateKind::And),
            RealisationCost {
                area: 2 * nor_footprint_area(1) + nor_footprint_area(2),
                delay_game_ticks: 2 * torch,
            }
        );
    }

    /// The asymmetry every recipe is built around, stated as a test rather
    /// than only as prose: an OR-shaped output finished in a merge costs no
    /// torch delay at all, while the same shape finished in a torch costs
    /// one more level than its AND-shaped sibling.
    #[test]
    fn finishing_in_a_merge_costs_no_gate_delay_and_finishing_in_a_torch_costs_one() {
        let torch = crate::redstone::simulator::component::TORCH_DELAY_GAME_TICKS;

        // NAND and AND share their two input inverters; NAND finishes in a
        // merge while AND finishes in a torch.
        assert_eq!(expansion_cost(GateKind::Nand).delay_game_ticks, torch);
        assert_eq!(expansion_cost(GateKind::And).delay_game_ticks, 2 * torch);

        // XOR and XNOR are the same four torches; XOR joins them with a
        // merge, XNOR with a torch.
        assert_eq!(expansion_cost(GateKind::Xor).delay_game_ticks, 2 * torch);
        assert_eq!(expansion_cost(GateKind::Xnor).delay_game_ticks, 3 * torch);

        // OAI4 is four inputs for two torch levels, because De Morgan turns
        // its whole output stage into a merge.
        assert_eq!(expansion_cost(GateKind::Oai4).delay_game_ticks, torch);
        assert_eq!(expansion_cost(GateKind::Aoi4).delay_game_ticks, 3 * torch);
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
