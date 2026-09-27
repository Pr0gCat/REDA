//! Declarations, widths, name resolution, and the typed IR the logic graph
//! is built from.
//!
//! Elaboration selects the top module, collects every declaration before
//! resolving any body, and turns each continuous assignment into a
//! transient [`RtlExpr`] with a checked width. It creates one [`ElabNode`]
//! per typed declaration and per typed expression, which is what gives the
//! debug database a layer between "the syntax that was written" and "the
//! bits that were built".
//!
//! Version 1 has one instance -- the top module -- but it still names it
//! [`ROOT_INSTANCE`] explicitly rather than pretending instance paths do not
//! exist. When hierarchy arrives, an elaborated identity becomes source node
//! plus instance path, and parameter values live on that path; nothing here
//! assumes the one-to-one relationship that would have to be unpicked.
//!
//! Width checking is a strict bottom-up pass implementing exactly the
//! unsigned table in the plan. It deliberately rejects legal SystemVerilog
//! that would need implicit extension, truncation, or signed conversion: a
//! rejection with a span is a better answer than a guess, and the guess is
//! what the plan's rejection matrix exists to prevent.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use super::ast::{
    AlwaysComb, AlwaysFf, Assign, BinaryOp, CaseStatement, Expr, ExprKind, IfAssign, Item, Module,
    NonblockingAssign, PortDirection, Range, SourceNodeId, SourceSet, Stmt, UnaryOp,
};
use super::debug::SyntheticOrigin;
use super::rtl::{RtlBinaryOp, RtlExpr, RtlKind};
use super::source::Span;
use super::{Diagnostic, Severity};

/// Identity of one elaborated node within a compile call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ElabNodeId(pub u32);

/// Identity of one instance path. Version 1 elaborates only the root.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct InstancePathId(pub u32);

/// The only instance path version 1 builds: the selected top module.
pub const ROOT_INSTANCE: InstancePathId = InstancePathId(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ElabKind {
    Module,
    Input,
    Output,
    /// An internal `wire`/`logic` net.
    Net,
    /// A typed expression.
    Expr,
    /// A continuous assignment.
    Assign,
    /// A positive-edge sequential assignment (`always_ff`).
    Dff,
}

/// One elaborated node: which syntax it came from, what it is, how wide it
/// is, and which instance owns it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ElabNode {
    pub source: SourceNodeId,
    pub kind: ElabKind,
    pub width: u32,
    pub instance: InstancePathId,
}

/// One declared signal of the top module, in declaration order.
#[derive(Debug, Clone)]
pub struct SignalInfo {
    pub name: String,
    pub elab: ElabNodeId,
    pub width: u32,
    /// `None` for an internal net.
    pub direction: Option<PortDirection>,
    pub span: Span,
}

/// `target = value`, with the target resolved to a signal and a bit range.
#[derive(Debug, Clone)]
pub struct ElabAssign {
    pub elab: ElabNodeId,
    pub span: Span,
    pub signal: usize,
    /// Lowest target bit, LSB-first.
    pub lsb: u32,
    pub width: u32,
    pub value: RtlExpr,
    /// `Some(signal)` for a positive-edge DFF, otherwise a combinational
    /// assignment. The clock signal is always a scalar top-level input.
    pub clock: Option<usize>,
}

/// The elaborated top module.
#[derive(Debug, Clone)]
pub struct Design {
    pub nodes: Vec<ElabNode>,
    pub signals: Vec<SignalInfo>,
    pub assigns: Vec<ElabAssign>,
}

impl Design {
    pub fn signal(&self, index: usize) -> &SignalInfo {
        &self.signals[index]
    }

    pub fn inputs(&self) -> impl Iterator<Item = (usize, &SignalInfo)> {
        self.signals
            .iter()
            .enumerate()
            .filter(|(_, signal)| signal.direction == Some(PortDirection::Input))
    }

    pub fn outputs(&self) -> impl Iterator<Item = (usize, &SignalInfo)> {
        self.signals
            .iter()
            .enumerate()
            .filter(|(_, signal)| signal.direction == Some(PortDirection::Output))
    }
}

fn error(span: Span, message: impl Into<String>) -> Diagnostic {
    Diagnostic {
        severity: Severity::Error,
        message: message.into(),
        span,
    }
}

/// Elaborate `top` out of `set`. Item-level errors accumulate so one typo
/// does not hide the next one; they are sorted by position before returning.
pub fn elaborate(set: &SourceSet, top: &str) -> Result<Design, Vec<Diagnostic>> {
    let mut candidates = set.modules.iter().filter(|module| module.name.name == top);
    let Some(module) = candidates.next() else {
        // Nothing in the source set matches, so there is no right span:
        // point at the whole first module that does exist, or at the start
        // of the first file if the set is empty.
        let span = set
            .modules
            .first()
            .map(|module| module.span)
            .unwrap_or_else(|| Span::new(super::source::FileId(0), 0, 0));
        let available: Vec<&str> = set
            .modules
            .iter()
            .map(|module| module.name.name.as_str())
            .collect();
        return Err(vec![error(
            span,
            format!("no module named `{top}` in the source set; found {available:?}"),
        )]);
    };
    if let Some(duplicate) = candidates.next() {
        return Err(vec![error(
            duplicate.name.span,
            format!("module `{top}` is declared more than once"),
        )]);
    }

    let mut elaborator = Elaborator {
        nodes: Vec::new(),
        signals: Vec::new(),
        by_name: HashMap::new(),
        errors: Vec::new(),
        ff_clock: None,
    };
    let design = elaborator.module(module);
    if elaborator.errors.is_empty() {
        Ok(design)
    } else {
        let mut errors = elaborator.errors;
        errors.sort_by_key(|diagnostic| (diagnostic.span.file, diagnostic.span.start));
        Err(errors)
    }
}

struct Elaborator {
    nodes: Vec<ElabNode>,
    signals: Vec<SignalInfo>,
    by_name: HashMap<String, usize>,
    errors: Vec<Diagnostic>,
    ff_clock: Option<usize>,
}

impl Elaborator {
    fn node(&mut self, source: SourceNodeId, kind: ElabKind, width: u32) -> ElabNodeId {
        let id = ElabNodeId(self.nodes.len() as u32);
        self.nodes.push(ElabNode {
            source,
            kind,
            width,
            instance: ROOT_INSTANCE,
        });
        id
    }

    fn fail(&mut self, span: Span, message: impl Into<String>) {
        self.errors.push(error(span, message));
    }

    fn module(&mut self, module: &Module) -> Design {
        // The module's own node comes first, so elaborated IDs run in the
        // same order as the syntax they came from.
        let _module = self.node(module.id, ElabKind::Module, 0);

        // Pass one: collect every declaration and signature before
        // resolving any body, so an assignment may refer to a net declared
        // after it.
        for port in &module.ports {
            let width = self.width_of(port.range.as_ref(), &port.name.name, port.span);
            let kind = match port.direction {
                PortDirection::Input => ElabKind::Input,
                PortDirection::Output => ElabKind::Output,
            };
            let elab = self.node(port.id, kind, width);
            self.declare(SignalInfo {
                name: port.name.name.clone(),
                elab,
                width,
                direction: Some(port.direction),
                span: port.span,
            });
        }
        for item in &module.items {
            if let Item::Declaration(declaration) = item {
                let width = self.width_of(
                    declaration.range.as_ref(),
                    &declaration.name.name,
                    declaration.span,
                );
                let elab = self.node(declaration.id, ElabKind::Net, width);
                self.declare(SignalInfo {
                    name: declaration.name.name.clone(),
                    elab,
                    width,
                    direction: None,
                    span: declaration.span,
                });
            }
        }

        // Pass two: resolve bodies.
        let mut assigns = Vec::new();
        for item in &module.items {
            let elaborated = match item {
                Item::Assign(assign) => self.assign(assign),
                Item::AlwaysComb(always) => self.always_comb(always),
                Item::AlwaysFf(always) => self.always_ff(always),
                Item::Declaration(_) => None,
            };
            if let Some(elaborated) = elaborated {
                assigns.push(elaborated);
            }
        }

        if !self
            .signals
            .iter()
            .any(|signal| signal.direction == Some(PortDirection::Output))
        {
            self.fail(
                module.name.span,
                format!(
                    "unsupported: module `{}` declares no output ports, so it has nothing to \
                     compile into a netlist",
                    module.name.name
                ),
            );
        }

        Design {
            nodes: std::mem::take(&mut self.nodes),
            signals: std::mem::take(&mut self.signals),
            assigns,
        }
    }

    fn declare(&mut self, signal: SignalInfo) {
        if let Some(&previous) = self.by_name.get(&signal.name) {
            let first = self.signals[previous].span;
            self.fail(
                signal.span,
                format!(
                    "`{}` is declared more than once (first at byte {})",
                    signal.name, first.start
                ),
            );
            return;
        }
        self.by_name.insert(signal.name.clone(), self.signals.len());
        self.signals.push(signal);
    }

    /// The declared width of `[N-1:0]`, or 1 when there is no range.
    fn width_of(&mut self, range: Option<&Range>, name: &str, span: Span) -> u32 {
        let Some(range) = range else { return 1 };
        if range.lsb != 0 || range.msb < range.lsb {
            self.fail(
                range.span,
                format!(
                    "`{name}` must be declared as `[N-1:0]`; version 1 supports only descending \
                     ranges based at zero"
                ),
            );
            return 1;
        }
        match u32::try_from(range.msb + 1) {
            Ok(width) if width <= 4096 => width,
            _ => {
                self.fail(
                    span,
                    format!("`{name}` is wider than the supported maximum"),
                );
                1
            }
        }
    }

    fn assign(&mut self, assign: &Assign) -> Option<ElabAssign> {
        let (signal, lsb, width) = self.target(&assign.lhs)?;
        let value = self.expr(&assign.rhs)?;
        if value.width != width {
            self.fail(
                assign.span,
                format!(
                    "assignment width mismatch: the target is {width} bit(s) and the value is {} \
                     bit(s); version 1 never extends or truncates",
                    value.width
                ),
            );
            return None;
        }
        let elab = self.node(assign.id, ElabKind::Assign, width);
        Some(ElabAssign {
            elab,
            span: assign.span,
            signal,
            lsb,
            width,
            value,
            clock: None,
        })
    }

    /// Lower the deliberately narrow Milestone 2 procedural form into the
    /// same single-assignment RTL used by continuous assignments.
    fn always_comb(&mut self, always: &AlwaysComb) -> Option<ElabAssign> {
        let [Stmt::Case(case)] = always.body.as_slice() else {
            self.fail(
                always.span,
                "`always_comb` must contain exactly one `case` statement in this milestone",
            );
            return None;
        };
        self.case_assignment(always, case)
    }

    fn case_assignment(&mut self, always: &AlwaysComb, case: &CaseStatement) -> Option<ElabAssign> {
        let selector = self.expr(&case.expr)?;
        let mut target: Option<(usize, u32, u32)> = None;
        let mut default = None;
        let mut arms = Vec::new();
        let mut labels = HashSet::new();
        let mut valid = true;

        for item in &case.items {
            let arm_target = self.target(&item.statement.lhs);
            let arm_value = self.expr(&item.statement.rhs);
            let (Some(arm_target), Some(arm_value)) = (arm_target, arm_value) else {
                valid = false;
                continue;
            };

            if arm_value.width != arm_target.2 {
                self.fail(
                    item.statement.span,
                    format!(
                        "assignment width mismatch: the target is {} bit(s) and the value is {} bit(s); version 1 never extends or truncates",
                        arm_target.2, arm_value.width
                    ),
                );
                valid = false;
            }

            if let Some(expected) = target {
                if arm_target != expected {
                    self.fail(
                        item.statement.lhs.span,
                        "every arm of a `case` in `always_comb` must assign the same signal slice",
                    );
                    valid = false;
                }
            } else {
                target = Some(arm_target);
            }

            let Some(label) = &item.value else {
                if default.replace(arm_value).is_some() {
                    self.fail(item.span, "a `case` may have only one `default` arm");
                    valid = false;
                }
                continue;
            };

            let ExprKind::Literal { bits, .. } = &label.kind else {
                self.fail(label.span, "case labels must be sized two-state literals");
                valid = false;
                continue;
            };
            if !labels.insert(bits.clone()) {
                self.fail(label.span, "duplicate case label");
                valid = false;
                continue;
            }

            let Some(label) = self.expr(label) else {
                valid = false;
                continue;
            };
            if label.width != selector.width {
                self.fail(
                    item.span,
                    format!(
                        "case label is {} bit(s) wide but the selector is {} bit(s)",
                        label.width, selector.width
                    ),
                );
                valid = false;
                continue;
            }

            let condition_elab = self.node(item.id, ElabKind::Expr, 1);
            let condition = RtlExpr {
                kind: RtlKind::Equal(Box::new(selector.clone()), Box::new(label)),
                width: 1,
                elab: condition_elab,
                synthetic: Some(SyntheticOrigin::CaseFold),
            };
            arms.push((item.statement.id, condition, arm_value));
        }

        let Some(default) = default else {
            self.fail(
                case.span,
                "`always_comb` case needs exactly one `default` arm to avoid a latch",
            );
            return None;
        };
        let Some((signal, lsb, width)) = target else {
            self.fail(case.span, "a `case` must contain at least one assignment");
            return None;
        };
        let target_name = self.signals[signal].name.clone();
        if let Some(span) = referenced_signal_span(&case.expr, &target_name) {
            self.fail(
                span,
                format!(
                    "unsupported: read-before-write in `always_comb`; `{target_name}` is read before it is assigned, which would infer storage"
                ),
            );
            valid = false;
        }
        for item in &case.items {
            if let Some(span) = referenced_signal_span(&item.statement.rhs, &target_name) {
                self.fail(
                    span,
                    format!(
                        "unsupported: read-before-write in `always_comb`; `{target_name}` is read before it is assigned, which would infer storage"
                    ),
                );
                valid = false;
            }
        }
        if !valid {
            return None;
        }

        let value =
            arms.into_iter()
                .rev()
                .fold(default, |when_false, (source, select, when_true)| {
                    let elab = self.node(source, ElabKind::Expr, width);
                    RtlExpr {
                        kind: RtlKind::Mux {
                            select: Box::new(select),
                            when_true: Box::new(when_true),
                            when_false: Box::new(when_false),
                        },
                        width,
                        elab,
                        synthetic: Some(SyntheticOrigin::BranchJoin),
                    }
                });
        let elab = self.node(always.id, ElabKind::Assign, width);
        Some(ElabAssign {
            elab,
            span: always.span,
            signal,
            lsb,
            width,
            value,
            clock: None,
        })
    }

    fn always_ff(&mut self, always: &AlwaysFf) -> Option<ElabAssign> {
        let clock = match &always.clock.kind {
            ExprKind::Ident(name) => self.lookup(name, always.clock.span),
            _ => {
                self.fail(
                    always.clock.span,
                    "unsupported: `always_ff` clock must be one top-level input identifier",
                );
                None
            }
        }?;
        if self.signals[clock].direction != Some(PortDirection::Input) {
            self.fail(
                always.clock.span,
                format!(
                    "unsupported: `always_ff` clock `{}` must be a top-level input port",
                    self.signals[clock].name
                ),
            );
            return None;
        }
        if self.signals[clock].width != 1 {
            self.fail(
                always.clock.span,
                format!(
                    "unsupported: `always_ff` clock `{}` must be one bit wide",
                    self.signals[clock].name
                ),
            );
            return None;
        }
        if let Some(previous) = self.ff_clock {
            if previous != clock {
                self.fail(
                    always.clock.span,
                    "unsupported: all `always_ff` blocks must use the same clock; multiple clocks are not supported",
                );
                return None;
            }
        } else {
            self.ff_clock = Some(clock);
        }

        match always.body.as_slice() {
            [Stmt::NonblockingAssign(assign)] => {
                self.always_ff_assignment(always, assign, clock, None)
            }
            [Stmt::If(if_assign)] => {
                self.always_ff_assignment(always, &if_assign.assign, clock, Some(if_assign))
            }
            _ => {
                self.fail(
                    always.span,
                    "unsupported: `always_ff` must contain exactly one nonblocking assignment, \
                     optionally guarded by one enable-style `if`; reset, `else`, and multiple \
                     writes are not supported",
                );
                None
            }
        }
    }

    /// Lower the plain feed-forward form (`enable` is `None`) and the one
    /// supported enable-style form (`if (en) q <= d;`) through the same
    /// path: both produce one [`ElabAssign`] whose `value` a positive-edge
    /// DFF captures every cycle. An incomplete assignment means hold, so the
    /// enable form's `value` becomes `en ? d : q` -- a synthetic
    /// [`SyntheticOrigin::HoldMux`] reading the target's own current value,
    /// not a second write. That self-read is exactly the state feedback the
    /// DFF is meant to allow: [`super::logic::LogicGraph::reserve_dff`]
    /// gives the register its identity before blasting `d`, so the read
    /// resolves to the register itself instead of tripping the
    /// combinational-loop check that guards every other signal.
    fn always_ff_assignment(
        &mut self,
        always: &AlwaysFf,
        assign: &NonblockingAssign,
        clock: usize,
        enable: Option<&IfAssign>,
    ) -> Option<ElabAssign> {
        let (signal, lsb, width) = self.target(&assign.lhs)?;
        if references_signal(&assign.rhs, &self.signals[signal].name) {
            self.fail(
                assign.rhs.span,
                format!(
                    "unsupported: feedback in `always_ff` is not supported; `{}` must be driven from a feed-forward value",
                    self.signals[signal].name
                ),
            );
            return None;
        }
        let value = self.expr(&assign.rhs)?;
        if value.width != width {
            self.fail(
                assign.span,
                format!(
                    "assignment width mismatch: the DFF target is {width} bit(s) and the value is {} bit(s); version 1 never extends or truncates",
                    value.width
                ),
            );
            return None;
        }

        let value = match enable {
            None => value,
            Some(if_assign) => {
                if references_signal(&if_assign.cond, &self.signals[signal].name) {
                    self.fail(
                        if_assign.cond.span,
                        format!(
                            "unsupported: feedback in `always_ff` is not supported; `{}` must not appear in its own enable condition",
                            self.signals[signal].name
                        ),
                    );
                    return None;
                }
                let condition = self.expr(&if_assign.cond)?;
                let condition = self.reduce_to_truth(condition);
                let elab = self.node(if_assign.id, ElabKind::Expr, width);
                let hold = RtlExpr {
                    kind: RtlKind::Signal { signal, lsb },
                    width,
                    elab,
                    synthetic: Some(SyntheticOrigin::HoldMux),
                };
                RtlExpr {
                    kind: RtlKind::Mux {
                        select: Box::new(condition),
                        when_true: Box::new(value),
                        when_false: Box::new(hold),
                    },
                    width,
                    elab,
                    synthetic: Some(SyntheticOrigin::HoldMux),
                }
            }
        };

        let elab = self.node(always.id, ElabKind::Dff, width);
        Some(ElabAssign {
            elab,
            span: always.span,
            signal,
            lsb,
            width,
            value,
            clock: Some(clock),
        })
    }

    /// Resolve an assignment target to `(signal, lsb, width)`.
    fn target(&mut self, expr: &Expr) -> Option<(usize, u32, u32)> {
        let (base, lsb, width) = match &expr.kind {
            ExprKind::Ident(name) => {
                let index = self.lookup(name, expr.span)?;
                (index, 0, self.signals[index].width)
            }
            ExprKind::BitSelect(base, index) => {
                let ExprKind::Ident(name) = &base.kind else {
                    self.fail(
                        expr.span,
                        "an assignment target must be a signal or a selection of one",
                    );
                    return None;
                };
                let signal = self.lookup(name, base.span)?;
                let bit = self.check_index(signal, *index, expr.span)?;
                (signal, bit, 1)
            }
            ExprKind::PartSelect(base, msb, lsb) => {
                let ExprKind::Ident(name) = &base.kind else {
                    self.fail(
                        expr.span,
                        "an assignment target must be a signal or a selection of one",
                    );
                    return None;
                };
                let signal = self.lookup(name, base.span)?;
                let (lsb, width) = self.check_range(signal, *msb, *lsb, expr.span)?;
                (signal, lsb, width)
            }
            _ => {
                self.fail(
                    expr.span,
                    "an assignment target must be a signal, a bit selection, or a part selection",
                );
                return None;
            }
        };

        if self.signals[base].direction == Some(PortDirection::Input) {
            self.fail(
                expr.span,
                format!(
                    "`{}` is an input port and cannot be assigned",
                    self.signals[base].name
                ),
            );
            return None;
        }
        Some((base, lsb, width))
    }

    fn lookup(&mut self, name: &str, span: Span) -> Option<usize> {
        match self.by_name.get(name) {
            Some(&index) => Some(index),
            None => {
                self.fail(span, format!("unknown signal `{name}`"));
                None
            }
        }
    }

    fn check_index(&mut self, signal: usize, index: u64, span: Span) -> Option<u32> {
        let width = self.signals[signal].width as u64;
        if index >= width {
            self.fail(
                span,
                format!(
                    "bit {index} is out of range for `{}`, which is {width} bit(s) wide",
                    self.signals[signal].name
                ),
            );
            return None;
        }
        Some(index as u32)
    }

    fn check_range(&mut self, signal: usize, msb: u64, lsb: u64, span: Span) -> Option<(u32, u32)> {
        let width = self.signals[signal].width as u64;
        if msb < lsb {
            self.fail(
                span,
                "a part selection must be descending, as `[msb:lsb]`".to_string(),
            );
            return None;
        }
        if msb >= width {
            self.fail(
                span,
                format!(
                    "bits [{msb}:{lsb}] are out of range for `{}`, which is {width} bit(s) wide",
                    self.signals[signal].name
                ),
            );
            return None;
        }
        Some((lsb as u32, (msb - lsb + 1) as u32))
    }

    /// Type and width check one expression, producing transient RTL.
    fn expr(&mut self, expr: &Expr) -> Option<RtlExpr> {
        match &expr.kind {
            ExprKind::Ident(name) => {
                let signal = self.lookup(name, expr.span)?;
                let width = self.signals[signal].width;
                let elab = self.node(expr.id, ElabKind::Expr, width);
                Some(RtlExpr::new(
                    RtlKind::Signal { signal, lsb: 0 },
                    width,
                    elab,
                ))
            }
            ExprKind::Literal { width, bits } => {
                let elab = self.node(expr.id, ElabKind::Expr, *width);
                Some(RtlExpr::new(RtlKind::Const(bits.clone()), *width, elab))
            }
            ExprKind::BitSelect(base, index) => {
                let ExprKind::Ident(name) = &base.kind else {
                    self.fail(expr.span, "only a signal can be bit-selected in version 1");
                    return None;
                };
                let signal = self.lookup(name, base.span)?;
                let bit = self.check_index(signal, *index, expr.span)?;
                let elab = self.node(expr.id, ElabKind::Expr, 1);
                Some(RtlExpr::new(RtlKind::Signal { signal, lsb: bit }, 1, elab))
            }
            ExprKind::PartSelect(base, msb, lsb) => {
                let ExprKind::Ident(name) = &base.kind else {
                    self.fail(expr.span, "only a signal can be part-selected in version 1");
                    return None;
                };
                let signal = self.lookup(name, base.span)?;
                let (lsb, width) = self.check_range(signal, *msb, *lsb, expr.span)?;
                let elab = self.node(expr.id, ElabKind::Expr, width);
                Some(RtlExpr::new(RtlKind::Signal { signal, lsb }, width, elab))
            }
            ExprKind::Concat(parts) => {
                // Source order is MSB-first; RTL keeps parts LSB-first.
                let mut lowered = Vec::with_capacity(parts.len());
                let mut width = 0u32;
                for part in parts.iter().rev() {
                    let part = self.expr(part)?;
                    width = width.saturating_add(part.width);
                    lowered.push(part);
                }
                if width > 4096 {
                    self.fail(
                        expr.span,
                        format!("concatenation is {width} bit(s) wide, over the supported maximum"),
                    );
                    return None;
                }
                let elab = self.node(expr.id, ElabKind::Expr, width);
                Some(RtlExpr::new(RtlKind::Concat(lowered), width, elab))
            }
            ExprKind::Unary(UnaryOp::Not, operand) => {
                let operand = self.expr(operand)?;
                let width = operand.width;
                let elab = self.node(expr.id, ElabKind::Expr, width);
                Some(RtlExpr::new(RtlKind::Not(Box::new(operand)), width, elab))
            }
            ExprKind::Unary(UnaryOp::LogicalNot, operand) => {
                let operand = self.expr(operand)?;
                let truth = self.reduce_to_truth(operand);
                let elab = self.node(expr.id, ElabKind::Expr, 1);
                Some(RtlExpr::new(RtlKind::Not(Box::new(truth)), 1, elab))
            }
            ExprKind::Binary(op, left, right) => self.binary(expr, *op, left, right),
            ExprKind::Ternary(select, when_true, when_false) => {
                let select = self.expr(select)?;
                let select = self.reduce_to_truth(select);
                let when_true = self.expr(when_true)?;
                let when_false = self.expr(when_false)?;
                if when_true.width != when_false.width {
                    self.fail(
                        expr.span,
                        format!(
                            "the branches of `?:` are {} and {} bit(s) wide; version 1 requires \
                             them to match exactly",
                            when_true.width, when_false.width
                        ),
                    );
                    return None;
                }
                let width = when_true.width;
                let elab = self.node(expr.id, ElabKind::Expr, width);
                Some(RtlExpr::new(
                    RtlKind::Mux {
                        select: Box::new(select),
                        when_true: Box::new(when_true),
                        when_false: Box::new(when_false),
                    },
                    width,
                    elab,
                ))
            }
        }
    }

    fn binary(&mut self, expr: &Expr, op: BinaryOp, left: &Expr, right: &Expr) -> Option<RtlExpr> {
        let left = self.expr(left)?;
        let right = self.expr(right)?;

        let bitwise = |op| match op {
            BinaryOp::And => Some(RtlBinaryOp::And),
            BinaryOp::Or => Some(RtlBinaryOp::Or),
            BinaryOp::Xor => Some(RtlBinaryOp::Xor),
            _ => None,
        };

        match op {
            BinaryOp::And | BinaryOp::Or | BinaryOp::Xor => {
                if left.width != right.width {
                    self.fail(
                        expr.span,
                        format!(
                            "the operands of `{}` are {} and {} bit(s) wide; version 1 requires \
                             them to match exactly",
                            op.as_str(),
                            left.width,
                            right.width
                        ),
                    );
                    return None;
                }
                let width = left.width;
                let elab = self.node(expr.id, ElabKind::Expr, width);
                Some(RtlExpr::new(
                    RtlKind::Binary(
                        bitwise(op).expect("checked above"),
                        Box::new(left),
                        Box::new(right),
                    ),
                    width,
                    elab,
                ))
            }
            BinaryOp::Equal | BinaryOp::NotEqual => {
                if left.width != right.width {
                    self.fail(
                        expr.span,
                        format!(
                            "the operands of `{}` are {} and {} bit(s) wide; version 1 requires \
                             them to match exactly",
                            op.as_str(),
                            left.width,
                            right.width
                        ),
                    );
                    return None;
                }
                let elab = self.node(expr.id, ElabKind::Expr, 1);
                let equal = RtlExpr::new(RtlKind::Equal(Box::new(left), Box::new(right)), 1, elab);
                Some(match op {
                    BinaryOp::Equal => equal,
                    // `!=` is the same comparison, inverted; both nodes
                    // belong to the one `!=` expression.
                    _ => RtlExpr::new(RtlKind::Not(Box::new(equal)), 1, elab),
                })
            }
            BinaryOp::LogicalAnd | BinaryOp::LogicalOr => {
                let left = self.reduce_to_truth(left);
                let right = self.reduce_to_truth(right);
                let elab = self.node(expr.id, ElabKind::Expr, 1);
                let op = if op == BinaryOp::LogicalAnd {
                    RtlBinaryOp::And
                } else {
                    RtlBinaryOp::Or
                };
                Some(RtlExpr::new(
                    RtlKind::Binary(op, Box::new(left), Box::new(right)),
                    1,
                    elab,
                ))
            }
        }
    }

    /// OR-reduce `value` to one bit, which is what `!`, `&&`, `||`, and a
    /// ternary selector each do to their operand. A one-bit value is
    /// already its own truth value, so no node is created for it.
    fn reduce_to_truth(&mut self, value: RtlExpr) -> RtlExpr {
        if value.width == 1 {
            return value;
        }
        let elab = value.elab;
        RtlExpr::new(RtlKind::ReduceOr(Box::new(value)), 1, elab)
    }
}

fn references_signal(expr: &Expr, name: &str) -> bool {
    match &expr.kind {
        ExprKind::Ident(value) => value == name,
        ExprKind::Literal { .. } => false,
        ExprKind::Unary(_, operand) => references_signal(operand, name),
        ExprKind::Binary(_, left, right) => {
            references_signal(left, name) || references_signal(right, name)
        }
        ExprKind::Ternary(select, when_true, when_false) => {
            references_signal(select, name)
                || references_signal(when_true, name)
                || references_signal(when_false, name)
        }
        ExprKind::BitSelect(base, _) | ExprKind::PartSelect(base, _, _) => {
            references_signal(base, name)
        }
        ExprKind::Concat(parts) => parts.iter().any(|part| references_signal(part, name)),
    }
}

/// Return the first source span at which `name` is read in `expr`.
///
/// `always_comb` has no state to read before its assignment. Catching that
/// here gives the editor the actual identifier span instead of making the
/// bit-blaster report a less useful declaration-level combinational loop.
fn referenced_signal_span(expr: &Expr, name: &str) -> Option<Span> {
    match &expr.kind {
        ExprKind::Ident(value) if value == name => Some(expr.span),
        ExprKind::Ident(_) | ExprKind::Literal { .. } => None,
        ExprKind::Unary(_, operand) => referenced_signal_span(operand, name),
        ExprKind::Binary(_, left, right) => {
            referenced_signal_span(left, name).or_else(|| referenced_signal_span(right, name))
        }
        ExprKind::Ternary(select, when_true, when_false) => referenced_signal_span(select, name)
            .or_else(|| referenced_signal_span(when_true, name))
            .or_else(|| referenced_signal_span(when_false, name)),
        ExprKind::BitSelect(base, _) | ExprKind::PartSelect(base, _, _) => {
            referenced_signal_span(base, name)
        }
        ExprKind::Concat(parts) => parts
            .iter()
            .find_map(|part| referenced_signal_span(part, name)),
    }
}

/// The name one bit of a signal goes by in a [`Netlist`](crate::compile::Netlist):
/// the bare name for a scalar, `name[i]` LSB-first for a vector -- the same
/// convention the Yosys bridge already produces, so both frontends label a
/// port the same way.
pub fn bit_name(name: &str, width: u32, bit: u32) -> String {
    if width == 1 {
        name.to_string()
    } else {
        format!("{name}[{bit}]")
    }
}
