//! Recursive-descent parser for declarations and statements, with a Pratt
//! parser for expressions.
//!
//! Version 1 has no syntax recovery: the first lexer or parser error is
//! returned as the single diagnostic, pointing at the offending token (for
//! a missing `;`, that is the token *after* the gap).
//!
//! The parser assigns every [`SourceNodeId`] in completion order, which
//! depends only on the token sequence. Comments and whitespace move spans
//! and nothing else.

use super::ast::{
    AlwaysComb, AlwaysFf, Assign, BinaryOp, BlockingAssign, CaseItem, CaseStatement, Expr,
    ExprKind, Ident, IfAssign, Item, Module, NetDecl, NetType, NonblockingAssign, PortDecl,
    PortDirection, Range, SourceKind, SourceNode, SourceNodeId, SourceSet, Stmt, UnaryOp,
};

use super::lexer::{self, Keyword, Number, Punct, Token, TokenKind};
use super::source::{FileId, Span};
use super::{Diagnostic, Severity};

/// Parse every file of a source set into modules plus the shared source
/// node table. Files are parsed in order; a failure in any file stops the
/// whole parse.
pub fn parse_sources(files: &[(FileId, &str)]) -> Result<SourceSet, Diagnostic> {
    let mut set = SourceSet {
        modules: Vec::new(),
        nodes: Vec::new(),
    };
    for &(file, text) in files {
        let tokens = lexer::tokenize(file, text)?;
        let mut parser = Parser {
            tokens,
            pos: 0,
            nodes: &mut set.nodes,
        };
        while !parser.at_eof() {
            let module = parser.module()?;
            set.modules.push(module);
        }
    }
    Ok(set)
}

struct Parser<'n> {
    tokens: Vec<Token>,
    pos: usize,
    nodes: &'n mut Vec<SourceNode>,
}

/// Binding powers for the Pratt loop, lowest first. Follows IEEE 1800's
/// table for the operators version 1 accepts: `?:` < `||` < `&&` < `|` <
/// `^` < `&` < `==`/`!=` < unary.
const BP_TERNARY: u8 = 1;
const BP_LOGICAL_OR: u8 = 2;
const BP_LOGICAL_AND: u8 = 3;
const BP_OR: u8 = 4;
const BP_XOR: u8 = 5;
const BP_AND: u8 = 6;
const BP_EQUALITY: u8 = 7;
const BP_UNARY: u8 = 9;

impl<'n> Parser<'n> {
    fn peek(&self) -> &Token {
        &self.tokens[self.pos]
    }

    fn at_eof(&self) -> bool {
        matches!(self.peek().kind, TokenKind::Eof)
    }

    fn advance(&mut self) -> Token {
        let token = self.tokens[self.pos].clone();
        if !matches!(token.kind, TokenKind::Eof) {
            self.pos += 1;
        }
        token
    }

    fn error_at(&self, span: Span, message: impl Into<String>) -> Diagnostic {
        Diagnostic {
            severity: Severity::Error,
            message: message.into(),
            span,
        }
    }

    fn error_here(&self, expected: &str) -> Diagnostic {
        let token = self.peek();
        self.error_at(
            token.span,
            format!("expected {expected}, found {}", token.kind.describe()),
        )
    }

    fn node(&mut self, kind: SourceKind, span: Span, name: Option<String>) -> SourceNodeId {
        let id = SourceNodeId(self.nodes.len() as u32);
        self.nodes.push(SourceNode { kind, span, name });
        id
    }

    fn is_punct(&self, punct: Punct) -> bool {
        self.peek().kind == TokenKind::Punct(punct)
    }

    fn is_keyword(&self, keyword: Keyword) -> bool {
        self.peek().kind == TokenKind::Keyword(keyword)
    }

    fn eat_punct(&mut self, punct: Punct) -> bool {
        if self.is_punct(punct) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn expect_punct(&mut self, punct: Punct) -> Result<Span, Diagnostic> {
        if self.is_punct(punct) {
            Ok(self.advance().span)
        } else {
            Err(self.error_here(&format!("`{}`", punct.as_str())))
        }
    }

    fn expect_keyword(&mut self, keyword: Keyword) -> Result<Span, Diagnostic> {
        if self.is_keyword(keyword) {
            Ok(self.advance().span)
        } else {
            Err(self.error_here(&format!("`{}`", keyword.as_str())))
        }
    }

    fn expect_ident(&mut self) -> Result<Ident, Diagnostic> {
        match &self.peek().kind {
            TokenKind::Ident(name) => {
                let name = name.clone();
                let span = self.advance().span;
                Ok(Ident { name, span })
            }
            _ => Err(self.error_here("an identifier")),
        }
    }

    fn expect_unsized_number(&mut self) -> Result<(u64, Span), Diagnostic> {
        match &self.peek().kind {
            TokenKind::Number(Number::Unsized(text)) => {
                let cleaned: String = text.chars().filter(|&c| c != '_').collect();
                let span = self.peek().span;
                match cleaned.parse::<u64>() {
                    Ok(value) => {
                        self.advance();
                        Ok((value, span))
                    }
                    Err(_) => Err(self.error_at(span, format!("integer `{text}` is out of range"))),
                }
            }
            _ => Err(self.error_here("an integer")),
        }
    }

    // ---- declarations -------------------------------------------------

    fn module(&mut self) -> Result<Module, Diagnostic> {
        let start = self.expect_keyword(Keyword::Module)?;
        let name = self.expect_ident()?;

        if self.is_punct(Punct::Hash) {
            return Err(self.error_here("`(` or `;`: parameter ports are not supported"));
        }

        let mut ports = Vec::new();
        if self.eat_punct(Punct::LParen) {
            if !self.is_punct(Punct::RParen) {
                let mut previous: Option<(PortDirection, Option<NetType>, Option<Range>)> = None;
                loop {
                    let port = self.port(previous.as_ref())?;
                    previous = Some((port.direction, port.net_type, port.range));
                    ports.push(port);
                    if !self.eat_punct(Punct::Comma) {
                        break;
                    }
                }
            }
            self.expect_punct(Punct::RParen)?;
        }
        self.expect_punct(Punct::Semicolon)?;

        let mut items = Vec::new();
        while !self.is_keyword(Keyword::Endmodule) {
            if self.at_eof() {
                return Err(self.error_here("`endmodule`"));
            }
            self.item(&mut items)?;
        }
        let end = self.expect_keyword(Keyword::Endmodule)?;
        let span = start.join(end);
        let id = self.node(SourceKind::Module, span, Some(name.name.clone()));
        Ok(Module {
            id,
            span,
            name,
            ports,
            items,
        })
    }

    fn port(
        &mut self,
        previous: Option<&(PortDirection, Option<NetType>, Option<Range>)>,
    ) -> Result<PortDecl, Diagnostic> {
        let start = self.peek().span;
        let direction = match self.peek().kind {
            TokenKind::Keyword(Keyword::Input) => {
                self.advance();
                Some(PortDirection::Input)
            }
            TokenKind::Keyword(Keyword::Output) => {
                self.advance();
                Some(PortDirection::Output)
            }
            TokenKind::Keyword(Keyword::Inout) => {
                return Err(self.error_here("`input` or `output`: `inout` ports are not supported"));
            }
            _ => None,
        };

        let (direction, net_type, range) = match direction {
            Some(direction) => {
                let net_type = self.net_type()?;
                let range = self.range()?;
                (direction, net_type, range)
            }
            None => match previous {
                // `input a, b`: a bare identifier inherits the previous
                // port's direction, type, and range.
                Some(&(direction, net_type, range)) => (direction, net_type, range),
                None => return Err(self.error_here("`input` or `output`")),
            },
        };

        let name = self.expect_ident()?;
        let span = start.join(name.span);
        let id = self.node(SourceKind::Port, span, Some(name.name.clone()));
        Ok(PortDecl {
            id,
            span,
            direction,
            net_type,
            range,
            name,
        })
    }

    fn net_type(&mut self) -> Result<Option<NetType>, Diagnostic> {
        Ok(match self.peek().kind {
            TokenKind::Keyword(Keyword::Wire) => {
                self.advance();
                Some(NetType::Wire)
            }
            TokenKind::Keyword(Keyword::Logic) => {
                self.advance();
                Some(NetType::Logic)
            }
            TokenKind::Keyword(Keyword::Reg) => {
                return Err(self.error_here("`logic`: legacy `reg` is not supported"));
            }
            _ => None,
        })
    }

    fn range(&mut self) -> Result<Option<Range>, Diagnostic> {
        if !self.is_punct(Punct::LBracket) {
            return Ok(None);
        }
        let start = self.advance().span;
        let (msb, _) = self.expect_unsized_number()?;
        self.expect_punct(Punct::Colon)?;
        let (lsb, _) = self.expect_unsized_number()?;
        let end = self.expect_punct(Punct::RBracket)?;
        Ok(Some(Range {
            msb,
            lsb,
            span: start.join(end),
        }))
    }

    fn item(&mut self, items: &mut Vec<Item>) -> Result<(), Diagnostic> {
        match self.peek().kind {
            TokenKind::Keyword(Keyword::Assign) => {
                let start = self.advance().span;
                let lhs = self.expr(0)?;
                self.expect_punct(Punct::Assign)?;
                let rhs = self.expr(0)?;
                let end = self.expect_punct(Punct::Semicolon)?;
                let span = start.join(end);
                let id = self.node(SourceKind::Assign, span, None);
                items.push(Item::Assign(Assign { id, span, lhs, rhs }));
                Ok(())
            }
            TokenKind::Keyword(Keyword::Wire) | TokenKind::Keyword(Keyword::Logic) => {
                let start = self.peek().span;
                self.net_type()?.expect("checked above");
                let range = self.range()?;
                loop {
                    let name = self.expect_ident()?;
                    let span = start.join(name.span);
                    let id = self.node(SourceKind::Declaration, span, Some(name.name.clone()));
                    items.push(Item::Declaration(NetDecl {
                        id,
                        span,
                        range,
                        name,
                    }));
                    if !self.eat_punct(Punct::Comma) {
                        break;
                    }
                }
                self.expect_punct(Punct::Semicolon)?;
                Ok(())
            }
            TokenKind::Keyword(Keyword::Reg) => {
                Err(self.error_here("`logic`: legacy `reg` is not supported"))
            }
            TokenKind::Keyword(Keyword::Always) => Err(self.error_here(
                "`always_comb` or `always_ff`: legacy `always` is not supported",
            )),
            TokenKind::Keyword(Keyword::AlwaysComb) => {
                let start = self.advance().span;
                self.expect_keyword(Keyword::Begin)?;
                let mut body = Vec::new();
                while !self.is_keyword(Keyword::End) {
                    if self.at_eof() {
                        return Err(self.error_here("`end`"));
                    }
                    body.push(self.statement()?);
                }
                let end = self.expect_keyword(Keyword::End)?;
                let span = start.join(end);
                let id = self.node(SourceKind::AlwaysComb, span, None);
                items.push(Item::AlwaysComb(AlwaysComb { id, span, body }));
                Ok(())
            }
            TokenKind::Keyword(Keyword::AlwaysFf) => {
                self.always_ff(items)
            }
            TokenKind::Keyword(Keyword::Parameter) | TokenKind::Keyword(Keyword::Localparam) => {
                Err(self.error_here("a module item: parameters are not supported"))
            }
            TokenKind::Keyword(Keyword::Input) | TokenKind::Keyword(Keyword::Output) => Err(self
                .error_here(
                    "a module item: non-ANSI port declarations are not supported; declare ports in the module header",
                )),
            TokenKind::Ident(_) => Err(self.error_here(
                "a module item: module instances are not supported",
            )),
            _ => Err(self.error_here("a module item")),
        }
    }

    fn always_ff(&mut self, items: &mut Vec<Item>) -> Result<(), Diagnostic> {
        let start = self.expect_keyword(Keyword::AlwaysFf)?;
        self.expect_punct(Punct::At)?;
        self.expect_punct(Punct::LParen)?;
        if self.is_keyword(Keyword::Negedge) {
            let span = self.advance().span;
            return Err(self.error_at(
                span,
                "unsupported: `always_ff` only supports `posedge`; negative-edge clocks are not supported",
            ));
        }
        self.expect_keyword(Keyword::Posedge)?;
        let clock = self.expr(0)?;
        if self.is_keyword(Keyword::Or) {
            let span = self.advance().span;
            return Err(self.error_at(
                span,
                "unsupported: `always_ff` allows exactly one clock; multiple clocks and asynchronous resets are not supported",
            ));
        }
        self.expect_punct(Punct::RParen)?;

        let mut body = Vec::new();
        let end = if self.eat_keyword(Keyword::Begin) {
            while !self.is_keyword(Keyword::End) {
                if self.at_eof() {
                    return Err(self.error_here("`end`"));
                }
                body.push(self.nonblocking_statement()?);
            }
            self.expect_keyword(Keyword::End)?
        } else {
            body.push(self.nonblocking_statement()?);
            match body.last().expect("just pushed") {
                Stmt::NonblockingAssign(assign) => assign.span,
                Stmt::If(if_assign) => if_assign.span,
                Stmt::Case(_) | Stmt::BlockingAssign(_) => {
                    unreachable!("nonblocking_statement never returns a case or blocking form")
                }
            }
        };
        let span = start.join(end);
        let id = self.node(SourceKind::AlwaysFf, span, None);
        items.push(Item::AlwaysFf(AlwaysFf {
            id,
            span,
            clock,
            body,
        }));
        Ok(())
    }

    fn eat_keyword(&mut self, keyword: Keyword) -> bool {
        if self.is_keyword(keyword) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn nonblocking_statement(&mut self) -> Result<Stmt, Diagnostic> {
        if self.is_keyword(Keyword::If) {
            return Ok(Stmt::If(self.if_assign_statement()?));
        }
        if self.is_keyword(Keyword::Case) {
            return Err(self.error_at(
                self.peek().span,
                "unsupported: `case` logic in `always_ff` is not supported; use one feed-forward `q <= d` assignment",
            ));
        }
        self.nonblocking_assign_statement()
            .map(Stmt::NonblockingAssign)
    }

    /// `lhs <= rhs;`, on its own -- the body of the plain feed-forward form
    /// and of the one supported enable-style `if`.
    fn nonblocking_assign_statement(&mut self) -> Result<NonblockingAssign, Diagnostic> {
        let start = self.peek().span;
        let lhs = self.expr(0)?;
        if self.is_punct(Punct::Assign) {
            let span = self.advance().span;
            return Err(self.error_at(
                span,
                "unsupported: `always_ff` requires a nonblocking `<=` assignment",
            ));
        }
        self.expect_punct(Punct::LessEqual)?;
        let rhs = self.expr(0)?;
        let end = self.expect_punct(Punct::Semicolon)?;
        let span = start.join(end);
        self.node(SourceKind::NonblockingAssign, span, None);
        Ok(NonblockingAssign { span, lhs, rhs })
    }

    /// `if (cond) lhs <= rhs;`, with an optional `begin`/`end` around the
    /// single nonblocking assignment. A nested `if`, `case`, or `else` is
    /// rejected here with a span, rather than being accepted and rejected
    /// later by elaboration.
    fn if_assign_statement(&mut self) -> Result<IfAssign, Diagnostic> {
        let start = self.expect_keyword(Keyword::If)?;
        self.expect_punct(Punct::LParen)?;
        let cond = self.expr(0)?;
        self.expect_punct(Punct::RParen)?;
        let assign = if self.eat_keyword(Keyword::Begin) {
            let assign = self.nonblocking_assign_statement()?;
            self.expect_keyword(Keyword::End)?;
            assign
        } else {
            self.nonblocking_assign_statement()?
        };
        if self.is_keyword(Keyword::Else) {
            let span = self.advance().span;
            return Err(self.error_at(
                span,
                "unsupported: `else` in `always_ff` is not supported; only one enable-style \
                 `if (en) q <= d;` is supported",
            ));
        }
        let span = start.join(assign.span);
        let id = self.node(SourceKind::If, span, None);
        Ok(IfAssign {
            id,
            span,
            cond,
            assign,
        })
    }

    fn statement(&mut self) -> Result<Stmt, Diagnostic> {
        match self.peek().kind {
            TokenKind::Keyword(Keyword::Case) => Ok(Stmt::Case(self.case_statement()?)),
            TokenKind::Ident(_) => {
                let start = self.peek().span;
                let lhs = self.expr(0)?;
                self.expect_punct(Punct::Assign)?;
                let rhs = self.expr(0)?;
                let end = self.expect_punct(Punct::Semicolon)?;
                let span = start.join(end);
                let id = self.node(SourceKind::BlockingAssign, span, None);
                Ok(Stmt::BlockingAssign(BlockingAssign { id, span, lhs, rhs }))
            }
            _ => Err(self.error_here("a blocking assignment or `case` statement")),
        }
    }

    fn case_statement(&mut self) -> Result<CaseStatement, Diagnostic> {
        let start = self.expect_keyword(Keyword::Case)?;
        self.expect_punct(Punct::LParen)?;
        let expr = self.expr(0)?;
        self.expect_punct(Punct::RParen)?;
        let mut items = Vec::new();
        while !self.is_keyword(Keyword::Endcase) {
            if self.at_eof() {
                return Err(self.error_here("`endcase`"));
            }
            let item_start = self.peek().span;
            let value = if self.is_keyword(Keyword::Default) {
                self.advance();
                None
            } else {
                Some(self.expr(0)?)
            };
            self.expect_punct(Punct::Colon)?;
            let statement = match self.statement()? {
                Stmt::BlockingAssign(assign) => assign,
                Stmt::Case(_) => {
                    return Err(self.error_here(
                        "a blocking assignment in a case arm; nested case is not supported",
                    ))
                }
                Stmt::NonblockingAssign(_) => {
                    return Err(self.error_here(
                        "a blocking assignment in a case arm; nonblocking assignment is not supported here",
                    ))
                }
                Stmt::If(_) => unreachable!("statement() never parses `if`; only nonblocking_statement does"),
            };
            let span = item_start.join(statement.span);
            let id = self.node(SourceKind::Case, span, None);
            items.push(CaseItem {
                id,
                span,
                value,
                statement,
            });
        }
        let end = self.expect_keyword(Keyword::Endcase)?;
        let span = start.join(end);
        self.node(SourceKind::Case, span, None);
        Ok(CaseStatement { span, expr, items })
    }

    // ---- expressions --------------------------------------------------

    fn expr(&mut self, min_bp: u8) -> Result<Expr, Diagnostic> {
        let mut lhs = self.unary()?;

        loop {
            let (op, bp) = match self.peek().kind {
                TokenKind::Punct(Punct::OrOr) => (Some(BinaryOp::LogicalOr), BP_LOGICAL_OR),
                TokenKind::Punct(Punct::AndAnd) => (Some(BinaryOp::LogicalAnd), BP_LOGICAL_AND),
                TokenKind::Punct(Punct::Pipe) => (Some(BinaryOp::Or), BP_OR),
                TokenKind::Punct(Punct::Caret) => (Some(BinaryOp::Xor), BP_XOR),
                TokenKind::Punct(Punct::Amp) => (Some(BinaryOp::And), BP_AND),
                TokenKind::Punct(Punct::EqualEqual) => (Some(BinaryOp::Equal), BP_EQUALITY),
                TokenKind::Punct(Punct::BangEqual) => (Some(BinaryOp::NotEqual), BP_EQUALITY),
                TokenKind::Punct(Punct::Question) => (None, BP_TERNARY),
                _ => break,
            };
            if bp < min_bp {
                break;
            }

            match op {
                Some(op) => {
                    // Left associative: the right operand binds strictly
                    // tighter.
                    self.advance();
                    let rhs = self.expr(bp + 1)?;
                    let span = lhs.span.join(rhs.span);
                    let id = self.node(SourceKind::Binary, span, None);
                    lhs = Expr {
                        id,
                        span,
                        kind: ExprKind::Binary(op, Box::new(lhs), Box::new(rhs)),
                    };
                }
                None => {
                    // Right associative ternary: `a ? b : c ? d : e`.
                    self.advance();
                    let when_true = self.expr(BP_TERNARY)?;
                    self.expect_punct(Punct::Colon)?;
                    let when_false = self.expr(BP_TERNARY)?;
                    let span = lhs.span.join(when_false.span);
                    let id = self.node(SourceKind::Ternary, span, None);
                    lhs = Expr {
                        id,
                        span,
                        kind: ExprKind::Ternary(
                            Box::new(lhs),
                            Box::new(when_true),
                            Box::new(when_false),
                        ),
                    };
                }
            }
        }
        Ok(lhs)
    }

    fn unary(&mut self) -> Result<Expr, Diagnostic> {
        let op = match self.peek().kind {
            TokenKind::Punct(Punct::Tilde) => Some(UnaryOp::Not),
            TokenKind::Punct(Punct::Bang) => Some(UnaryOp::LogicalNot),
            _ => None,
        };
        if let Some(op) = op {
            let start = self.advance().span;
            let operand = self.expr(BP_UNARY)?;
            let span = start.join(operand.span);
            let id = self.node(SourceKind::Unary, span, None);
            return Ok(Expr {
                id,
                span,
                kind: ExprKind::Unary(op, Box::new(operand)),
            });
        }
        self.postfix()
    }

    fn postfix(&mut self) -> Result<Expr, Diagnostic> {
        let mut expr = self.primary()?;
        while self.is_punct(Punct::LBracket) {
            self.advance();
            let (first, _) = self.expect_unsized_number()?;
            if self.eat_punct(Punct::Colon) {
                let (second, _) = self.expect_unsized_number()?;
                let end = self.expect_punct(Punct::RBracket)?;
                let span = expr.span.join(end);
                let id = self.node(SourceKind::PartSelect, span, None);
                expr = Expr {
                    id,
                    span,
                    kind: ExprKind::PartSelect(Box::new(expr), first, second),
                };
            } else {
                let end = self.expect_punct(Punct::RBracket)?;
                let span = expr.span.join(end);
                let id = self.node(SourceKind::BitSelect, span, None);
                expr = Expr {
                    id,
                    span,
                    kind: ExprKind::BitSelect(Box::new(expr), first),
                };
            }
        }
        Ok(expr)
    }

    fn primary(&mut self) -> Result<Expr, Diagnostic> {
        let token = self.peek().clone();
        match token.kind {
            TokenKind::Ident(name) => {
                self.advance();
                let id = self.node(SourceKind::Identifier, token.span, Some(name.clone()));
                Ok(Expr {
                    id,
                    span: token.span,
                    kind: ExprKind::Ident(name),
                })
            }
            TokenKind::Number(Number::Sized {
                width,
                base,
                digits,
            }) => {
                self.advance();
                let (width, bits) = parse_sized_literal(&width, base, &digits)
                    .map_err(|message| self.error_at(token.span, message))?;
                let id = self.node(SourceKind::Literal, token.span, None);
                Ok(Expr {
                    id,
                    span: token.span,
                    kind: ExprKind::Literal { width, bits },
                })
            }
            TokenKind::Number(Number::Unsized(_)) => Err(self.error_at(
                token.span,
                "unsized literal: every literal needs an explicit width such as `1'b1`",
            )),
            TokenKind::Punct(Punct::LParen) => {
                self.advance();
                let inner = self.expr(0)?;
                self.expect_punct(Punct::RParen)?;
                // Parentheses only group; the inner expression keeps its
                // own span so `gates_for_span` finds it either way.
                Ok(inner)
            }
            TokenKind::Punct(Punct::LBrace) => {
                let start = self.advance().span;
                let mut parts = vec![self.expr(0)?];
                while self.eat_punct(Punct::Comma) {
                    parts.push(self.expr(0)?);
                }
                let end = self.expect_punct(Punct::RBrace)?;
                let span = start.join(end);
                let id = self.node(SourceKind::Concat, span, None);
                Ok(Expr {
                    id,
                    span,
                    kind: ExprKind::Concat(parts),
                })
            }
            _ => Err(self.error_here("an expression")),
        }
    }
}

/// Interpret `width'<base><digits>` into LSB-first bits, applying the
/// version 1 rules: explicit width, binary or decimal, `_` allowed, no
/// `x`/`z`/`?`, and the value must fit without truncation.
fn parse_sized_literal(width: &str, base: char, digits: &str) -> Result<(u32, Vec<bool>), String> {
    let width_text: String = width.chars().filter(|&c| c != '_').collect();
    let width: u32 = width_text
        .parse()
        .map_err(|_| format!("literal width `{width}` is out of range"))?;
    if width == 0 {
        return Err("literal width must be at least 1".to_string());
    }
    if width > 4096 {
        return Err(format!(
            "literal width {width} is larger than the supported maximum of 4096"
        ));
    }
    let mut bits = vec![false; width as usize];
    match base {
        'b' => {
            let mut significant: Vec<bool> = Vec::new();
            for c in digits.chars() {
                match c {
                    '_' => {}
                    '0' => significant.push(false),
                    '1' => significant.push(true),
                    'x' | 'X' | 'z' | 'Z' | '?' => {
                        return Err(format!(
                            "literal digit `{c}`: four-state values are not supported"
                        ))
                    }
                    _ => return Err(format!("`{c}` is not a binary digit")),
                }
            }
            if significant.is_empty() {
                return Err("literal has no digits".to_string());
            }
            // MSB first as written; the leading digits must be zero if
            // there are more digits than bits.
            let extra = significant.len().saturating_sub(width as usize);
            if significant[..extra].iter().any(|&b| b) {
                return Err(format!(
                    "literal value does not fit in {width} bit(s); truncation is not performed"
                ));
            }
            for (i, &bit) in significant[extra..].iter().rev().enumerate() {
                bits[i] = bit;
            }
        }
        'd' => {
            let mut value: u128 = 0;
            let mut any = false;
            for c in digits.chars() {
                match c {
                    '_' => {}
                    '0'..='9' => {
                        any = true;
                        value = value
                            .checked_mul(10)
                            .and_then(|v| v.checked_add(c as u128 - '0' as u128))
                            .ok_or_else(|| "decimal literal is too large".to_string())?;
                    }
                    'x' | 'X' | 'z' | 'Z' | '?' => {
                        return Err(format!(
                            "literal digit `{c}`: four-state values are not supported"
                        ))
                    }
                    _ => return Err(format!("`{c}` is not a decimal digit")),
                }
            }
            if !any {
                return Err("literal has no digits".to_string());
            }
            if width < 128 && value >> width != 0 {
                return Err(format!(
                    "literal value {value} does not fit in {width} bit(s); truncation is not performed"
                ));
            }
            for (i, bit) in bits.iter_mut().enumerate().take(128.min(width as usize)) {
                *bit = (value >> i) & 1 == 1;
            }
        }
        'h' | 'o' => {
            return Err(format!(
                "literal base `'{base}` is not supported; use binary (`'b`) or decimal (`'d`)"
            ))
        }
        other => return Err(format!("`'{other}` is not a literal base")),
    }
    Ok((width, bits))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_one(text: &str) -> Result<SourceSet, Diagnostic> {
        parse_sources(&[(FileId(0), text)])
    }

    #[test]
    fn parses_the_and4_shape() {
        let set = parse_one(
            "module and4(input logic a, input logic b, input logic c, input logic d, output logic y);\n  assign y = a & b & c & d;\nendmodule\n",
        )
        .expect("parses");
        assert_eq!(set.modules.len(), 1);
        let module = &set.modules[0];
        assert_eq!(module.name.name, "and4");
        assert_eq!(module.ports.len(), 5);
        assert_eq!(module.items.len(), 1);
        let Item::Assign(assign) = &module.items[0] else {
            panic!("expected an assign");
        };
        // `a & b & c & d` is left associative: ((a & b) & c) & d.
        let ExprKind::Binary(BinaryOp::And, left, _) = &assign.rhs.kind else {
            panic!("expected a binary and");
        };
        assert!(matches!(left.kind, ExprKind::Binary(BinaryOp::And, _, _)));
    }

    #[test]
    fn missing_semicolon_points_at_the_following_token() {
        let text = "module m(input logic a, output logic y);\n  assign y = a\nendmodule\n";
        let err = parse_one(text).unwrap_err();
        let endmodule = text.find("endmodule").unwrap() as u32;
        assert_eq!(err.span.start, endmodule);
        assert_eq!(err.message, "expected `;`, found keyword `endmodule`");
    }

    /// `a | b & c ^ d == e ? f : g` is
    /// `a | ((b & c) ^ (d == e)) ? f : g`: `?:` is the loosest operator,
    /// then `|`, then `^`, then `&`, and `==` binds tighter than all of
    /// them.
    #[test]
    fn precedence_follows_the_standard() {
        let set = parse_one("module m; assign y = a | b & c ^ d == e ? f : g; endmodule").unwrap();
        let Item::Assign(assign) = &set.modules[0].items[0] else {
            panic!()
        };
        let ExprKind::Ternary(cond, _, _) = &assign.rhs.kind else {
            panic!("expected ternary")
        };
        let ExprKind::Binary(BinaryOp::Or, _, right) = &cond.kind else {
            panic!("expected or")
        };
        let ExprKind::Binary(BinaryOp::Xor, left, right) = &right.kind else {
            panic!("expected xor")
        };
        assert!(matches!(left.kind, ExprKind::Binary(BinaryOp::And, _, _)));
        assert!(matches!(
            right.kind,
            ExprKind::Binary(BinaryOp::Equal, _, _)
        ));
    }

    /// Same-precedence binary operators are left associative, and `?:` is
    /// right associative.
    #[test]
    fn associativity_follows_the_standard() {
        let set = parse_one("module m; assign y = a ^ b ^ c; endmodule").unwrap();
        let Item::Assign(assign) = &set.modules[0].items[0] else {
            panic!()
        };
        let ExprKind::Binary(BinaryOp::Xor, left, right) = &assign.rhs.kind else {
            panic!("expected xor")
        };
        assert!(matches!(left.kind, ExprKind::Binary(BinaryOp::Xor, _, _)));
        assert!(matches!(right.kind, ExprKind::Ident(_)));

        let set = parse_one("module m; assign y = a ? b : c ? d : e; endmodule").unwrap();
        let Item::Assign(assign) = &set.modules[0].items[0] else {
            panic!()
        };
        let ExprKind::Ternary(_, _, when_false) = &assign.rhs.kind else {
            panic!("expected ternary")
        };
        assert!(matches!(when_false.kind, ExprKind::Ternary(_, _, _)));
    }

    #[test]
    fn literals_are_sized_two_state_and_must_fit() {
        assert_eq!(
            parse_sized_literal("4", 'b', "01_10"),
            Ok((4, vec![false, true, true, false]))
        );
        assert_eq!(
            parse_sized_literal("3", 'd', "5"),
            Ok((3, vec![true, false, true]))
        );
        assert!(parse_sized_literal("2", 'd', "4").is_err());
        assert!(parse_sized_literal("2", 'b', "1x").is_err());
        assert!(parse_sized_literal("0", 'b', "0").is_err());
        assert!(parse_sized_literal("8", 'h', "ff").is_err());
        assert_eq!(
            parse_sized_literal("2", 'b', "0011"),
            Ok((2, vec![true, true]))
        );
        assert!(parse_sized_literal("2", 'b', "0111").is_err());
    }

    #[test]
    fn bare_identifier_ports_inherit_direction() {
        let set = parse_one("module m(input logic [1:0] a, b, output y); endmodule").unwrap();
        let ports = &set.modules[0].ports;
        assert_eq!(ports[1].direction, PortDirection::Input);
        assert_eq!(ports[1].range.map(|r| (r.msb, r.lsb)), Some((1, 0)));
        assert_eq!(ports[2].direction, PortDirection::Output);
        assert_eq!(ports[2].range, None);
    }
}
