//! Transient word-level RTL expressions.
//!
//! This is the seam between elaboration (which knows names, widths, and
//! types) and the bit-level logic graph (which knows only booleans). It is
//! deliberately *transient*: an `RtlExpr` is an owned tree that lives from
//! the moment [`super::elaborate`] type-checks an expression until
//! [`super::logic`] bit-blasts it, and it has **no ID arena of its own**.
//! The plan asks for one only when word-level optimisation or memories
//! arrive; until then a second arena would be a table nothing reads.
//!
//! What every node does carry is its provenance: the [`ElabNodeId`] it was
//! built for, plus an optional [`SyntheticOrigin`] for nodes no expression
//! spelled (hold muxes, branch joins, case folds, passthroughs). Version 1
//! creates none of those -- continuous assignment has no control flow -- but
//! the field is where `always_ff`'s hold mux will record itself without
//! inventing a source span.

use super::debug::SyntheticOrigin;
use super::elaborate::ElabNodeId;

/// The word-level operations version 1 lowers to. Wider vocabulary
/// (registers, arithmetic, memories) is added when a fixture needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RtlBinaryOp {
    And,
    Or,
    Xor,
}

#[derive(Debug, Clone)]
pub enum RtlKind {
    /// `width` bits of `signal`, starting at bit `lsb` (LSB-first, which is
    /// the convention the Yosys bridge and `Netlist`'s port map already
    /// use).
    Signal {
        signal: usize,
        lsb: u32,
    },
    /// A sized constant, LSB-first.
    Const(Vec<bool>),
    /// Concatenation, **LSB-first**: `parts[0]` supplies the low bits.
    /// The parser's source order is the reverse of this.
    Concat(Vec<RtlExpr>),
    Not(Box<RtlExpr>),
    Binary(RtlBinaryOp, Box<RtlExpr>, Box<RtlExpr>),
    /// OR-reduce to one bit -- what `!a`, `a && b`, `a || b`, and a ternary
    /// selector each apply to their operand.
    ReduceOr(Box<RtlExpr>),
    /// Equality of two equal-width operands, one bit wide.
    Equal(Box<RtlExpr>, Box<RtlExpr>),
    Mux {
        select: Box<RtlExpr>,
        when_true: Box<RtlExpr>,
        when_false: Box<RtlExpr>,
    },
}

#[derive(Debug, Clone)]
pub struct RtlExpr {
    pub kind: RtlKind,
    pub width: u32,
    pub elab: ElabNodeId,
    pub synthetic: Option<SyntheticOrigin>,
}

impl RtlExpr {
    /// A node spelled by the source expression `elab`.
    pub fn new(kind: RtlKind, width: u32, elab: ElabNodeId) -> RtlExpr {
        RtlExpr {
            kind,
            width,
            elab,
            synthetic: None,
        }
    }
}
