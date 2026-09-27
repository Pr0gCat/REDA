//! Syntax-only data produced by the parser.
//!
//! Every node carries a [`SourceNodeId`] and a [`Span`]. The ID is assigned
//! in parse completion (postorder) sequence, so it depends only on token
//! order -- never on whitespace or comments -- while the span records the
//! bytes. The `SourceNode` table the parser builds alongside the tree is what
//! the debug database keeps; the tree itself is transient.

use serde::{Deserialize, Serialize};

use super::source::Span;

/// Identity of one syntax node within a compile call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SourceNodeId(pub u32);

/// What kind of syntax a [`SourceNodeId`] names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SourceKind {
    Module,
    Port,
    Declaration,
    Assign,
    Identifier,
    Literal,
    Unary,
    Binary,
    Ternary,
    BitSelect,
    PartSelect,
    Concat,
    AlwaysComb,
    AlwaysFf,
    Case,
    BlockingAssign,
    NonblockingAssign,
    /// The single supported enable-style `if (en) q <= d;` inside
    /// `always_ff`.
    If,
}

/// The persisted record for one syntax node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceNode {
    pub kind: SourceKind,
    pub span: Span,
    /// The declared or referenced name, for modules, ports, declarations,
    /// and identifier expressions.
    pub name: Option<String>,
}

/// One parsed source set: every module of every file, plus the node table.
#[derive(Debug, Clone)]
pub struct SourceSet {
    pub modules: Vec<Module>,
    pub nodes: Vec<SourceNode>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PortDirection {
    Input,
    Output,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetType {
    Wire,
    Logic,
}

/// A packed range `[msb:lsb]` as written. Elaboration insists on `[N-1:0]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub msb: u64,
    pub lsb: u64,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Ident {
    pub name: String,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Module {
    pub id: SourceNodeId,
    pub span: Span,
    pub name: Ident,
    pub ports: Vec<PortDecl>,
    pub items: Vec<Item>,
}

#[derive(Debug, Clone)]
pub struct PortDecl {
    pub id: SourceNodeId,
    pub span: Span,
    pub direction: PortDirection,
    pub net_type: Option<NetType>,
    pub range: Option<Range>,
    pub name: Ident,
}

#[derive(Debug, Clone)]
pub enum Item {
    /// `wire`/`logic` declaration of one internal net.
    Declaration(NetDecl),
    /// `assign lhs = rhs;`
    Assign(Assign),
    /// `always_comb begin ... end` procedural combinational logic.
    AlwaysComb(AlwaysComb),
    /// `always_ff @(posedge clock)` with the deliberately narrow sequential
    /// body supported by the compiler.
    AlwaysFf(AlwaysFf),
}

#[derive(Debug, Clone)]
pub struct AlwaysComb {
    pub id: SourceNodeId,
    pub span: Span,
    pub body: Vec<Stmt>,
}

#[derive(Debug, Clone)]
pub struct AlwaysFf {
    pub id: SourceNodeId,
    pub span: Span,
    pub clock: Expr,
    pub body: Vec<Stmt>,
}

#[derive(Debug, Clone)]
pub enum Stmt {
    BlockingAssign(BlockingAssign),
    NonblockingAssign(NonblockingAssign),
    Case(CaseStatement),
    /// `if (cond) lhs <= rhs;`, `always_ff`'s one supported enable form.
    /// There is deliberately no `else` field: version 1 rejects one instead
    /// of representing it.
    If(IfAssign),
}

#[derive(Debug, Clone)]
pub struct IfAssign {
    pub id: SourceNodeId,
    pub span: Span,
    pub cond: Expr,
    pub assign: NonblockingAssign,
}

#[derive(Debug, Clone)]
pub struct NonblockingAssign {
    pub span: Span,
    pub lhs: Expr,
    pub rhs: Expr,
}

#[derive(Debug, Clone)]
pub struct BlockingAssign {
    pub id: SourceNodeId,
    pub span: Span,
    pub lhs: Expr,
    pub rhs: Expr,
}

#[derive(Debug, Clone)]
pub struct CaseStatement {
    pub span: Span,
    pub expr: Expr,
    pub items: Vec<CaseItem>,
}

#[derive(Debug, Clone)]
pub struct CaseItem {
    pub id: SourceNodeId,
    pub span: Span,
    /// `None` denotes the single `default` arm.
    pub value: Option<Expr>,
    pub statement: BlockingAssign,
}

/// A `wire`/`logic` declaration. The declared net type is checked by the
/// parser but not carried: version 1 assigns every net continuously, where
/// a net and a variable behave identically. It comes back when procedural
/// assignment arrives and the distinction starts to mean something.
#[derive(Debug, Clone)]
pub struct NetDecl {
    pub id: SourceNodeId,
    pub span: Span,
    pub range: Option<Range>,
    pub name: Ident,
}

#[derive(Debug, Clone)]
pub struct Assign {
    pub id: SourceNodeId,
    pub span: Span,
    pub lhs: Expr,
    pub rhs: Expr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    /// `~`
    Not,
    /// `!`
    LogicalNot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    And,
    Or,
    Xor,
    LogicalAnd,
    LogicalOr,
    Equal,
    NotEqual,
}

impl BinaryOp {
    pub fn as_str(self) -> &'static str {
        match self {
            BinaryOp::And => "&",
            BinaryOp::Or => "|",
            BinaryOp::Xor => "^",
            BinaryOp::LogicalAnd => "&&",
            BinaryOp::LogicalOr => "||",
            BinaryOp::Equal => "==",
            BinaryOp::NotEqual => "!=",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Expr {
    pub id: SourceNodeId,
    pub span: Span,
    pub kind: ExprKind,
}

#[derive(Debug, Clone)]
pub enum ExprKind {
    Ident(String),
    /// A sized literal, bits LSB-first.
    Literal {
        width: u32,
        bits: Vec<bool>,
    },
    Unary(UnaryOp, Box<Expr>),
    Binary(BinaryOp, Box<Expr>, Box<Expr>),
    Ternary(Box<Expr>, Box<Expr>, Box<Expr>),
    BitSelect(Box<Expr>, u64),
    PartSelect(Box<Expr>, u64, u64),
    Concat(Vec<Expr>),
}
