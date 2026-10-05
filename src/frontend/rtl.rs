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
    Add,
    Subtract,
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
    /// Assignment decomposition selects a validated range of a value.
    Slice(Box<RtlExpr>, u32),
    Not(Box<RtlExpr>),
    Binary(RtlBinaryOp, Box<RtlExpr>, Box<RtlExpr>),
    /// OR-reduce to one bit -- what `!a`, `a && b`, `a || b`, and a ternary
    /// selector each apply to their operand.
    ReduceOr(Box<RtlExpr>),
    /// Equality of two equal-width operands, one bit wide.
    Equal(Box<RtlExpr>, Box<RtlExpr>),
    /// Fully specified constant case table; support is bounded by elaboration.
    Table {
        selector: Box<RtlExpr>,
        arms: Vec<(usize, RtlExpr, RtlExpr)>,
        default: Box<RtlExpr>,
    },
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
    pub signed: bool,
    pub contextual: bool,
    pub elab: ElabNodeId,
    /// Lowest source-expression bit represented by a procedural projection.
    pub origin_lsb: u32,
    pub synthetic: Option<SyntheticOrigin>,
}

impl RtlExpr {
    /// A node spelled by the source expression `elab`.
    pub fn new(kind: RtlKind, width: u32, elab: ElabNodeId) -> RtlExpr {
        RtlExpr {
            kind,
            width,
            signed: false,
            contextual: true,
            elab,
            origin_lsb: 0,
            synthetic: None,
        }
    }
    pub fn self_determined(mut self) -> Self { self.contextual = false; self }

    pub fn with_signed(mut self, signed: bool) -> Self {
        self.signed = signed;
        self
    }

    /// Apply a context width before bitwise evaluation, retaining the RHS type.
    pub fn in_width(self, width: u32) -> Self {
        let signed = self.signed;
        self.in_integral_context(width, signed)
    }

    pub fn in_integral_context(mut self, width: u32, signed: bool) -> Self {
        let original = self.width;
        if width <= original {
            if width == original { self.signed = signed; return self; }
            let elab = self.elab;
            return Self::new(RtlKind::Slice(Box::new(self), 0), width, elab).with_signed(signed);
        }
        let kind = std::mem::replace(&mut self.kind, RtlKind::Const(Vec::new()));
        self.kind = match kind {
            RtlKind::Not(value) if self.contextual && value.width == original =>
                RtlKind::Not(Box::new(value.in_integral_context(width, signed))),
            RtlKind::Binary(op, left, right) if self.contextual && left.width == original && right.width == original =>
                RtlKind::Binary(op, Box::new(left.in_integral_context(width, signed)),
                    Box::new(right.in_integral_context(width, signed))),
            RtlKind::Mux { select, when_true, when_false } if self.contextual => RtlKind::Mux {
                select, when_true: Box::new(when_true.in_integral_context(width, signed)),
                when_false: Box::new(when_false.in_integral_context(width, signed)),
            },
            RtlKind::Const(mut bits) => {
                let extension = signed && bits.last().copied().unwrap_or(false);
                bits.resize(width as usize, extension);
                RtlKind::Const(bits)
            },
            kind => {
                let base = Self { kind, ..self.clone() };
                let mut parts = Vec::new();
                if signed {
                    for bit in original..width {
                        let mut extension = Self::new(RtlKind::Slice(Box::new(base.clone()), original - 1), 1, self.elab);
                        extension.origin_lsb = bit;
                        extension.synthetic = Some(SyntheticOrigin::Passthrough);
                        parts.push(extension);
                    }
                    parts.insert(0, base);
                } else {
                    parts.push(base);
                    let mut extension = Self::new(RtlKind::Const(vec![false; (width - original) as usize]), width - original, self.elab);
                    extension.origin_lsb = original;
                    extension.synthetic = Some(SyntheticOrigin::Passthrough);
                    parts.push(extension);
                }
                RtlKind::Concat(parts)
            }
        };
        self.width = width;
        self.signed = signed;
        self
    }

}
