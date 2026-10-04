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
//! Hierarchy elaborates independent parameter/name scopes and links typed
//! ports before bit blasting. Repeated module syntax retains distinct instance
//! IDs, while qualified signal names identify the concrete instance paths.
//!
//! Width checking is a strict bottom-up pass implementing exactly the
//! unsigned table in the plan. It deliberately rejects legal SystemVerilog
//! that would need implicit extension, truncation, or signed conversion: a
//! rejection with a span is a better answer than a guess, and the guess is
//! what the plan's rejection matrix exists to prevent.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use super::ast::{
    AlwaysComb, AlwaysFf, Assign, BinaryOp, CaseStatement, ClockEdge, ConstExpr, ConstKind, ConstOp, Expr,
    ExprKind, Instance, Item, Module, Parameter, PortDirection, Range,
    SourceNodeId, SourceSet, Stmt, UnaryOp,
};
use super::debug::SyntheticOrigin;
use super::rtl::{RtlBinaryOp, RtlExpr, RtlKind};
use super::source::Span;
use super::{Diagnostic, Severity};

/// Identity of one elaborated node within a compile call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ElabNodeId(pub u32);

/// Identity of one concrete instance path, assigned in source traversal order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct InstancePathId(pub u32);

/// The selected top module is always instance zero.
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
    /// `Some(clock)` for a DFF, otherwise a combinational
    /// assignment. The clock signal is always a scalar top-level input.
    pub clock: Option<Clock>,
}

#[derive(Debug, Clone, Copy)]
pub struct Clock {
    pub signal: usize,
    pub edge: ClockEdge,
}

/// Two bits of one physical net, without inventing a directed assignment.
#[derive(Debug, Clone)]
pub struct SignalAlias {
    pub left: (usize, u32),
    pub right: (usize, u32),
}

/// An ordered, complete blocking process. Its sensitivity and per-bit
/// temporary values are resolved after hierarchy aliases become known.
#[derive(Debug, Clone)]
pub struct CombBlock {
    pub span: Span,
    pub assigns: Vec<ElabAssign>,
    pub sensitivity: Option<Vec<(usize, u32)>>,
}

/// The elaborated top module with hierarchy aliases still explicit.
#[derive(Debug, Clone)]
pub struct Design {
    pub comb_blocks: Vec<CombBlock>,
    pub aliases: Vec<SignalAlias>,
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
        kind: super::DiagnosticKind::Source,
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

    Hierarchy {
        set,
        active: Vec::new(),
        next_instance: ROOT_INSTANCE.0,
    }
    .build(module, HashMap::new())
}

/// Elaborate each scope independently, then link its typed RTL into its parent.
/// Instance IDs follow source order, so repeated definitions retain distinct origins.
struct Hierarchy<'a> {
    set: &'a SourceSet,
    active: Vec<String>,
    next_instance: u32,
}

impl Hierarchy<'_> {
    fn build(
        &mut self,
        module: &Module,
        parameters: HashMap<String, Constant>,
    ) -> Result<Design, Vec<Diagnostic>> {
        if self.active.is_empty() && module.ports.iter().any(|port| port.direction == PortDirection::Inout) {
            let mut diagnostic = error(module.name.span,
                "top-level inout needs an external read/drive/enable contract; only internal single-driver net aliases are supported");
            diagnostic.kind = super::DiagnosticKind::Unsupported;
            return Err(vec![diagnostic]);
        }
        if self.active.contains(&module.name.name) {
            return Err(vec![error(
                module.name.span,
                format!("recursive module instantiation of `{}`", module.name.name),
            )]);
        }
        if self.active.len() >= 128 {
            let mut diagnostic = error(
                module.name.span,
                "native module hierarchy exceeds 128 levels",
            );
            diagnostic.kind = super::DiagnosticKind::Unsupported;
            return Err(vec![diagnostic]);
        }
        let instance = InstancePathId(self.next_instance);
        self.next_instance += 1;
        self.active.push(module.name.name.clone());
        let mut elaborator = Elaborator {
            nodes: Vec::new(),
            signals: Vec::new(),
            by_name: HashMap::new(),
            errors: Vec::new(),
            parameters,
            aliases: Vec::new(),
            comb_blocks: Vec::new(),
            instance,
        };
        let design = elaborator.module(module, self);
        self.active.pop();
        if elaborator.errors.is_empty() {
            Ok(design)
        } else {
            elaborator
                .errors
                .sort_by_key(|d| (d.span.file, d.span.start));
            Err(elaborator.errors)
        }
    }
}

/// Preserve all expression provenance while relocating a child scope's arenas.
fn relocate(expr: &mut RtlExpr, signals: usize, nodes: u32) {
    expr.elab.0 += nodes;
    match &mut expr.kind {
        RtlKind::Signal { signal, .. } => *signal += signals,
        RtlKind::Const(_) => {}
        RtlKind::Concat(parts) => {
            for part in parts {
                relocate(part, signals, nodes);
            }
        }
        RtlKind::Slice(value, _) | RtlKind::Not(value) | RtlKind::ReduceOr(value) => {
            relocate(value, signals, nodes)
        }
        RtlKind::Binary(_, left, right) | RtlKind::Equal(left, right) => {
            relocate(left, signals, nodes);
            relocate(right, signals, nodes);
        }
        RtlKind::Mux {
            select,
            when_true,
            when_false,
        } => {
            relocate(select, signals, nodes);
            relocate(when_true, signals, nodes);
            relocate(when_false, signals, nodes);
        }
        RtlKind::Table {
            selector,
            arms,
            default,
        } => {
            relocate(selector, signals, nodes);
            relocate(default, signals, nodes);
            for (_, condition, value) in arms {
                relocate(condition, signals, nodes);
                relocate(value, signals, nodes);
            }
        }
    }
}

/// An integral constant owns its width and signed interpretation. Values are
/// normalized at declaration/override boundaries, never at signal lookup.
#[derive(Clone, Copy, Debug)]
struct Constant {
    raw: u64,
    width: u32,
    signed: bool,
    checked_integer: bool,
}
impl Constant {
    fn number(self) -> i128 {
        if self.signed && self.raw & (1u64 << (self.width - 1)) != 0 {
            self.raw as i128 - (1i128 << self.width)
        } else { self.raw as i128 }
    }
    fn resized(self, width: u32, signed: bool, checked_integer: bool) -> Self {
        let mask = if width == 64 { u64::MAX } else { (1u64 << width) - 1 };
        Self { raw: self.number() as u64 & mask, width, signed, checked_integer }
    }
    fn bits(self) -> Vec<bool> {
        (0..self.width).map(|bit| self.raw & (1u64 << bit) != 0).collect()
    }
}

fn parameter_value(
    value: Constant,
    parameter: &Parameter,
    definitions: &HashMap<String, &Parameter>,
    values: &mut HashMap<String, Constant>,
    active: &mut HashSet<String>,
) -> Result<Constant, Diagnostic> {
    let Some(ty) = &parameter.ty else { return Ok(value); };
    let width = if let Some(range) = &ty.range {
        let msb = constant_value(&range.msb, definitions, values, active)?.number();
        let lsb = constant_value(&range.lsb, definitions, values, active)?.number();
        if lsb != 0 || msb < lsb || msb >= 64 {
            return Err(error(range.span, "parameter packed ranges must be descending [N-1:0], with 1..64 bits"));
        }
        (msb + 1) as u32
    } else if ty.integer { 32 } else if ty.packed { 1 } else { value.width };
    Ok(value.resized(width, ty.signed, false))
}

fn constant_value(
    expr: &ConstExpr,
    definitions: &HashMap<String, &Parameter>,
    values: &mut HashMap<String, Constant>,
    active: &mut HashSet<String>,
) -> Result<Constant, Diagnostic> {
    let overflow = || {
        let mut diagnostic = error(expr.span,
            "constant expression exceeds the native signed 32-bit integer subset");
        diagnostic.kind = super::DiagnosticKind::Unsupported;
        diagnostic
    };
    match &expr.kind {
        ConstKind::Integer(value) => {
            let raw = i32::try_from(*value).map_err(|_| overflow())? as u32 as u64;
            Ok(Constant { raw, width: 32, signed: true, checked_integer: true })
        }
        ConstKind::Sized { width, bits, signed } => Ok(Constant {
            raw: bits.iter().enumerate().fold(0, |raw, (bit, set)| raw | ((*set as u64) << bit)),
            width: *width, signed: *signed, checked_integer: false,
        }),
        ConstKind::Name(name) => {
            if !active.insert(name.clone()) {
                return Err(error(expr.span, format!("cyclic parameter dependency at `{name}`")));
            }
            let result = (|| {
                let value = if let Some(value) = values.get(name).copied() { value }
                    else if let Some(parameter) = definitions.get(name) {
                        constant_value(&parameter.value, definitions, values, active)?
                    } else { return Err(error(expr.span, format!("unknown parameter `{name}`"))); };
                if let Some(parameter) = definitions.get(name) {
                    parameter_value(value, parameter, definitions, values, active)
                } else { Ok(value) }
            })();
            active.remove(name);
            if let Ok(value) = result { values.insert(name.clone(), value); }
            result
        }
        ConstKind::Neg(operand) => {
            if matches!(operand.kind, ConstKind::Integer(value) if value == i32::MIN.unsigned_abs() as u64) {
                return Ok(Constant { raw: i32::MIN as u32 as u64, width:32, signed:true, checked_integer:true });
            }
            let value = constant_value(operand, definitions, values, active)?;
            let number = -value.number();
            if value.checked_integer && i32::try_from(number).is_err() { return Err(overflow()); }
            Ok(Constant { raw: number as u64, ..value }.resized(value.width, value.signed, value.checked_integer))
        }
        ConstKind::Binary(op, left, right) => {
            let left = constant_value(left, definitions, values, active)?;
            let right = constant_value(right, definitions, values, active)?;
            let width = left.width.max(right.width);
            let signed = left.signed && right.signed;
            let checked_integer = left.checked_integer && right.checked_integer;
            // Mixed signed/unsigned expressions extend each operand according
            // to its original signedness, then interpret at the common type.
            let left = Constant { signed, ..left }.resized(width, signed, checked_integer).number();
            let right = Constant { signed, ..right }.resized(width, signed, checked_integer).number();
            let number = match op { ConstOp::Add => left + right,
                ConstOp::Sub => left - right, ConstOp::Mul => left.wrapping_mul(right) };
            if checked_integer && i32::try_from(number).is_err() { return Err(overflow()); }
            Ok(Constant { raw: number as u64, width, signed, checked_integer }.resized(width, signed, checked_integer))
        }
    }
}

struct Elaborator {
    comb_blocks: Vec<CombBlock>,
    aliases: Vec<SignalAlias>,
    instance: InstancePathId,
    parameters: HashMap<String, Constant>,
    nodes: Vec<ElabNode>,
    signals: Vec<SignalInfo>,
    by_name: HashMap<String, usize>,
    errors: Vec<Diagnostic>,
}

impl Elaborator {
    fn node(&mut self, source: SourceNodeId, kind: ElabKind, width: u32) -> ElabNodeId {
        let id = ElabNodeId(self.nodes.len() as u32);
        self.nodes.push(ElabNode {
            source,
            kind,
            width,
            instance: self.instance,
        });
        id
    }

    fn fail(&mut self, span: Span, message: impl Into<String>) {
        self.errors.push(error(span, message));
    }

    fn unsupported(&mut self, span: Span, message: impl Into<String>) {
        let mut diagnostic = error(span, message);
        diagnostic.kind = super::DiagnosticKind::Unsupported;
        self.errors.push(diagnostic);
    }

    fn module(&mut self, module: &Module, hierarchy: &mut Hierarchy<'_>) -> Design {
        // The module's own node comes first, so elaborated IDs run in the
        // same order as the syntax they came from.
        let _module = self.node(module.id, ElabKind::Module, 0);
        let mut definitions = HashMap::new();
        for parameter in &module.parameters {
            if definitions
                .insert(parameter.name.name.clone(), parameter)
                .is_some()
            {
                self.fail(
                    parameter.name.span,
                    format!(
                        "parameter `{}` is declared more than once",
                        parameter.name.name
                    ),
                );
            }
        }
        for parameter in &module.parameters {
            match constant_value(
                &ConstExpr {
                    kind: ConstKind::Name(parameter.name.name.clone()),
                    span: parameter.name.span,
                },
                &definitions,
                &mut self.parameters,
                &mut HashSet::new(),
            ) {
                Ok(value) => {
                    self.parameters.insert(parameter.name.name.clone(), value);
                }
                Err(diagnostic) => self.errors.push(diagnostic),
            }
            let width = self.parameters.get(&parameter.name.name).map_or(32, |value| value.width);
            self.node(parameter.id, ElabKind::Expr, width);
        }

        // Constant failures prevent meaningful width checking; preserve the
        // original error classification instead of inventing unknown-name errors.
        if !self.errors.is_empty() {
            return Design {
                nodes: std::mem::take(&mut self.nodes),
                signals: Vec::new(),
                assigns: Vec::new(),
                aliases: Vec::new(),
                comb_blocks: Vec::new(),
            };
        }

        // Pass one: collect every declaration and signature before
        // resolving any body, so an assignment may refer to a net declared
        // after it.
        for port in &module.ports {
            let width = self.width_of(port.range.as_ref(), &port.name.name, port.span);
            let kind = match port.direction {
                PortDirection::Input => ElabKind::Input,
                PortDirection::Output => ElabKind::Output,
                PortDirection::Inout => ElabKind::Net,
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

        // Instance names share their scope's declaration namespace.
        let mut instances = HashSet::new();
        for item in &module.items {
            if let Item::Instance(instance) = item {
                if !instances.insert(instance.name.name.clone())
                    || self.by_name.contains_key(&instance.name.name)
                    || self.parameters.contains_key(&instance.name.name)
                {
                    self.fail(
                        instance.name.span,
                        format!("duplicate declaration `{}`", instance.name.name),
                    );
                }
            }
        }

        // Pass two: resolve bodies and recursively link instance ports.
        let mut assigns = Vec::new();
        for item in &module.items {
            let elaborated = match item {
                Item::Assign(assign) => {
                    assigns.extend(self.assignments(assign));
                    continue;
                }
                Item::AlwaysComb(always) if always.sensitivity.is_some() || always.body.len() > 1
                    || matches!(always.body.as_slice(), [Stmt::BlockingAssign(assign)] if matches!(assign.lhs.kind, ExprKind::Concat(_))) => {
                    self.procedural_comb(always);
                    None
                }
                Item::AlwaysComb(always) => self.always_comb(always),
                Item::AlwaysFf(always) => self.always_ff(always),
                Item::Declaration(_) => None,
                Item::Instance(instance) => {
                    assigns.extend(self.instance(instance, hierarchy));
                    None
                }
            };
            if let Some(elaborated) = elaborated {
                assigns.push(elaborated);
            }
        }

        if !self
            .signals
            .iter()
            .any(|signal| signal.direction == Some(PortDirection::Output)
                || (self.instance != ROOT_INSTANCE && signal.direction == Some(PortDirection::Inout)))
        {
            self.unsupported(
                module.name.span,
                format!(
                    "unsupported: module `{}` declares no output ports, so it has nothing to \
                     compile into a netlist",
                    module.name.name
                ),
            );
        }

        Design {
            aliases: std::mem::take(&mut self.aliases),
            comb_blocks: std::mem::take(&mut self.comb_blocks),
            nodes: std::mem::take(&mut self.nodes),
            signals: std::mem::take(&mut self.signals),
            assigns,
        }
    }

    fn instance(&mut self, instance: &Instance, hierarchy: &mut Hierarchy<'_>) -> Vec<ElabAssign> {
        let matches: Vec<_> = hierarchy
            .set
            .modules
            .iter()
            .filter(|m| m.name.name == instance.module.name)
            .collect();
        let module = match matches.as_slice() {
            [module] => *module,
            [] => {
                self.fail(
                    instance.module.span,
                    format!("unknown module `{}`", instance.module.name),
                );
                return Vec::new();
            }
            _ => {
                self.fail(
                    instance.module.span,
                    format!(
                        "module `{}` is declared more than once",
                        instance.module.name
                    ),
                );
                return Vec::new();
            }
        };
        let formal_parameters: Vec<_> = module.parameters.iter().filter(|p| !p.local).collect();
        let mut parameters = HashMap::new();
        for (index, connection) in instance.parameters.iter().enumerate() {
            let parameter = match &connection.name {
                Some(name) => formal_parameters
                    .iter()
                    .find(|p| p.name.name == name.name)
                    .copied(),
                None => formal_parameters.get(index).copied(),
            };
            let Some(parameter) = parameter else {
                self.fail(
                    connection.value.span,
                    "unknown, local or excess parameter override",
                );
                continue;
            };
            if parameters.contains_key(&parameter.name.name) {
                self.fail(
                    connection.value.span,
                    format!("duplicate parameter override `{}`", parameter.name.name),
                );
                continue;
            }
            match constant_value(
                &connection.value,
                &HashMap::new(),
                &mut self.parameters,
                &mut HashSet::new(),
            ) {
                Ok(value) => {
                    parameters.insert(parameter.name.name.clone(), value);
                }
                Err(diagnostic) => self.errors.push(diagnostic),
            }
        }
        let mut connections = vec![None; module.ports.len()];
        for (index, connection) in instance.ports.iter().enumerate() {
            let index = match &connection.name {
                Some(name) => module.ports.iter().position(|p| p.name.name == name.name),
                None => (index < module.ports.len()).then_some(index),
            };
            match index {
                Some(index) if connections[index].is_none() => {
                    connections[index] = Some(&connection.value)
                }
                Some(_) => self.fail(instance.span, "duplicate port connection"),
                None => self.fail(instance.span, "unknown or excess port connection"),
            }
        }
        if self.errors.is_empty() && connections.iter().any(Option::is_none) {
            self.unsupported(
                instance.span,
                "unconnected instance ports are outside the native two-state subset",
            );
        }
        if !self.errors.is_empty() {
            return Vec::new();
        }
        let mut child = match hierarchy.build(module, parameters) {
            Ok(child) => child,
            Err(errors) => {
                self.errors.extend(errors);
                return Vec::new();
            }
        };
        let signal_offset = self.signals.len();
        let node_offset = self.nodes.len() as u32;
        // Clocks cross scopes only as direct aliases of scalar input ports.
        // Each register retains its own clock alias across the hierarchy.
        let mut clocks = HashMap::new();
        for assignment in &child.assigns {
            if let Some(clock) = assignment.clock {
                let clock = clock.signal;
                if clocks.contains_key(&clock) {
                    continue;
                }
                let Some(connection) = connections.get(clock).copied().flatten() else {
                    self.unsupported(
                        instance.span,
                        "instance clock must connect directly to an input port",
                    );
                    return Vec::new();
                };
                let ExprKind::Ident(name) = &connection.kind else {
                    self.unsupported(
                        connection.span,
                        "instance clock must connect directly to an input port",
                    );
                    return Vec::new();
                };
                let Some(parent_clock) = self.lookup(name, connection.span) else {
                    return Vec::new();
                };
                let signal = &self.signals[parent_clock];
                if signal.direction != Some(PortDirection::Input) || signal.width != 1 {
                    self.unsupported(
                        connection.span,
                        "instance clock must connect to a scalar input port",
                    );
                    return Vec::new();
                }
                clocks.insert(clock, parent_clock);
            }
        }
        for signal in &mut child.signals {
            signal.name = format!("{}.{}", instance.name.name, signal.name);
            signal.elab.0 += node_offset;
            signal.direction = None;
        }
        self.aliases.extend(child.aliases.into_iter().map(|alias| SignalAlias {
            left: (alias.left.0 + signal_offset, alias.left.1),
            right: (alias.right.0 + signal_offset, alias.right.1),
        }));
        for mut block in child.comb_blocks {
            for assignment in &mut block.assigns {
                assignment.elab.0 += node_offset;
                assignment.signal += signal_offset;
                relocate(&mut assignment.value, signal_offset, node_offset);
            }
            if let Some(sensitivity) = &mut block.sensitivity {
                for (signal, _) in sensitivity { *signal += signal_offset; }
            }
            self.comb_blocks.push(block);
        }
        self.nodes.extend(child.nodes);
        self.signals.extend(child.signals);
        for assignment in &mut child.assigns {
            assignment.elab.0 += node_offset;
            assignment.signal += signal_offset;
            assignment.clock = assignment.clock.map(|clock| Clock {
                signal: clocks[&clock.signal],
                ..clock
            });
            relocate(&mut assignment.value, signal_offset, node_offset);
        }
        let mut assigns = child.assigns;
        for (index, port) in module.ports.iter().enumerate() {
            let connection = connections[index].unwrap();
            let signal = signal_offset + index;
            let width = self.signals[signal].width;
            match port.direction {
                PortDirection::Input => {
                    if let Some(value) = self.expr(connection) {
                        if value.width != width {
                            self.fail(
                                connection.span,
                                format!(
                                    "instance input `{}` width mismatch: expected {width}, got {}",
                                    port.name.name, value.width
                                ),
                            );
                            continue;
                        }
                        let elab = self.node(instance.id, ElabKind::Assign, width);
                        assigns.push(ElabAssign {
                            elab,
                            span: connection.span,
                            signal,
                            lsb: 0,
                            width,
                            value,
                            clock: None,
                        });
                    }
                }
                PortDirection::Inout => {
                    if let Some(targets) = self.net_targets(connection) {
                        if targets.iter().map(|target| target.2).sum::<u32>() != width {
                            self.fail(connection.span, format!("instance inout `{}` width mismatch", port.name.name));
                            continue;
                        }
                        let mut offset = width;
                        for (target, lsb, target_width) in targets {
                            offset -= target_width;
                            for bit in 0..target_width {
                                self.aliases.push(SignalAlias {
                                    left: (signal, offset + bit), right: (target, lsb + bit),
                                });
                            }
                        }
                    }
                }
                PortDirection::Output => {
                    if let Some(targets) = self.assignment_targets(connection) {
                        if targets.iter().map(|t| t.2).sum::<u32>() != width {
                            self.fail(
                                connection.span,
                                format!("instance output `{}` width mismatch", port.name.name),
                            );
                            continue;
                        }
                        let mut offset = width;
                        for (target, lsb, target_width) in targets {
                            offset -= target_width;
                            let elab = self.node(instance.id, ElabKind::Assign, target_width);
                            assigns.push(ElabAssign {
                                elab,
                                span: connection.span,
                                signal: target,
                                lsb,
                                width: target_width,
                                value: RtlExpr::new(
                                    RtlKind::Signal {
                                        signal,
                                        lsb: offset,
                                    },
                                    target_width,
                                    elab,
                                ),
                                clock: None,
                            });
                        }
                    }
                }
            }
        }
        assigns
    }

    fn declare(&mut self, signal: SignalInfo) {
        if self.parameters.contains_key(&signal.name) {
            self.fail(
                signal.span,
                format!("`{}` is already declared as a parameter", signal.name),
            );
            return;
        }
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
        let mut resolve = |expr| {
            constant_value(
                expr,
                &HashMap::new(),
                &mut self.parameters,
                &mut HashSet::new(),
            )
        };
        let msb = match resolve(&range.msb) {
            Ok(value) => value.number(),
            Err(error) => {
                self.errors.push(error);
                return 1;
            }
        };
        let lsb = match resolve(&range.lsb) {
            Ok(value) => value.number(),
            Err(error) => {
                self.errors.push(error);
                return 1;
            }
        };
        if lsb != 0 || msb < lsb {
            self.fail(range.span, format!("`{name}` must be declared as `[N-1:0]`; version 1 supports only descending ranges based at zero"));
            return 1;
        }
        match u32::try_from(msb + 1) {
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

    fn assignment_targets(&mut self, expr: &Expr) -> Option<Vec<(usize, u32, u32)>> {
        if let ExprKind::Concat(parts) = &expr.kind {
            let mut targets = Vec::new();
            for part in parts {
                targets.extend(self.assignment_targets(part)?);
            }
            Some(targets)
        } else {
            Some(vec![self.target(expr)?])
        }
    }

    fn assignments(&mut self, assign: &Assign) -> Vec<ElabAssign> {
        if !matches!(assign.lhs.kind, ExprKind::Concat(_)) {
            return self.assign(assign).into_iter().collect();
        }
        let Some(targets) = self.assignment_targets(&assign.lhs) else {
            return Vec::new();
        };
        let Some(value) = self.expr(&assign.rhs) else {
            return Vec::new();
        };
        let width = targets.iter().try_fold(0u32, |sum, t| sum.checked_add(t.2));
        if width != Some(value.width) {
            self.fail(assign.span, "assignment width mismatch: concatenated targets must exactly match the value width");
            return Vec::new();
        }
        let mut offset = value.width;
        targets
            .into_iter()
            .map(|(signal, lsb, width)| {
                offset -= width;
                let elab = self.node(assign.id, ElabKind::Assign, width);
                ElabAssign {
                    elab,
                    span: assign.span,
                    signal,
                    lsb,
                    width,
                    clock: None,
                    value: RtlExpr {
                        kind: RtlKind::Slice(Box::new(value.clone()), offset),
                        width,
                        elab,
                        origin_lsb: 0,
                        synthetic: Some(SyntheticOrigin::Passthrough),
                    },
                }
            })
            .collect()
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

    fn procedural_comb(&mut self, always: &AlwaysComb) {
        let mut assigns = Vec::new();
        if let [Stmt::Case(case)] = always.body.as_slice() {
            if let Some(assign) = self.case_assignment(always, case) { assigns.push(assign); }
        } else {
            for statement in &always.body {
                let Stmt::BlockingAssign(assign) = statement else {
                    self.unsupported(always.span, "ordered combinational blocks support blocking assignments or one complete case, not mixed/nested control flow");
                    return;
                };
                assigns.extend(self.assignments(&Assign {
                    id: assign.id, span: assign.span, lhs: assign.lhs.clone(), rhs: assign.rhs.clone(),
                }));
            }
        }
        let sensitivity = if let Some(events) = &always.sensitivity {
            let mut bits = Vec::new();
            for event in events {
                let Some(targets) = self.net_targets(event) else { return; };
                for (signal, lsb, width) in targets {
                    bits.extend((lsb..lsb+width).map(|bit| (signal, bit)));
                }
            }
            Some(bits)
        } else { None };
        self.comb_blocks.push(CombBlock { span: always.span, assigns, sensitivity });
    }

    /// Lower a complete blocking assignment or case into continuous RTL.
    fn always_comb(&mut self, always: &AlwaysComb) -> Option<ElabAssign> {
        if let [Stmt::BlockingAssign(assign)] = always.body.as_slice() {
            let (signal, _, _) = self.target(&assign.lhs)?;
            let name = self.signals[signal].name.clone();
            if let Some(span) = referenced_signal_span(&assign.rhs, &name) {
                self.unsupported(span, format!(
                    "unsupported: read-before-write in combinational block; `{name}` would infer storage",
                ));
                return None;
            }
            return self.assign(&Assign {
                id: assign.id,
                span: assign.span,
                lhs: assign.lhs.clone(),
                rhs: assign.rhs.clone(),
            });
        }
        let [Stmt::Case(case)] = always.body.as_slice() else {
            self.fail(
                always.span,
                "combinational block must contain exactly one blocking assignment or `case` statement",
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
                origin_lsb: 0,
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
            self.unsupported(
                span,
                format!(
                    "unsupported: read-before-write in `always_comb`; `{target_name}` is read before it is assigned, which would infer storage"
                ),
            );
            valid = false;
        }
        for item in &case.items {
            if let Some(span) = referenced_signal_span(&item.statement.rhs, &target_name) {
                self.unsupported(
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

        // Small constant tables retain their mutually exclusive labels until
        // bit blasting, where implicants can be minimized per output bit.
        // Exact, distinct case labels are mutually exclusive. With a zero
        // default, each arm can be gated independently and the results ORed;
        // carrying the other arms' priority conditions only adds logic.
        let value = if selector.width <= 6
            && matches!(&default.kind, RtlKind::Const(_))
            && arms
                .iter()
                .all(|(_, _, value)| matches!(value.kind, RtlKind::Const(_)))
        {
            let elab = self.node(always.id, ElabKind::Expr, width);
            RtlExpr {
                kind: RtlKind::Table {
                    selector: Box::new(selector),
                    arms: arms
                        .into_iter()
                        .map(|(_, condition, value)| {
                            let RtlKind::Equal(_, label) = &condition.kind else {
                                unreachable!()
                            };
                            let RtlKind::Const(bits) = &label.kind else {
                                unreachable!()
                            };
                            let index = bits
                                .iter()
                                .enumerate()
                                .fold(0, |acc, (i, bit)| acc | (usize::from(*bit) << i));
                            (index, condition, value)
                        })
                        .collect(),
                    default: Box::new(default),
                },
                width,
                elab,
                origin_lsb: 0,
                synthetic: Some(SyntheticOrigin::CaseFold),
            }
        } else if matches!(&default.kind, RtlKind::Const(bits) if bits.iter().all(|bit| !bit)) {
            arms.into_iter()
                .fold(default.clone(), |acc, (source, select, when_true)| {
                    let elab = self.node(source, ElabKind::Expr, width);
                    let gated = RtlExpr {
                        kind: RtlKind::Mux {
                            select: Box::new(select),
                            when_true: Box::new(when_true),
                            when_false: Box::new(default.clone()),
                        },
                        width,
                        elab,
                        origin_lsb: 0,
                        synthetic: Some(SyntheticOrigin::BranchJoin),
                    };
                    RtlExpr {
                        kind: RtlKind::Binary(RtlBinaryOp::Or, Box::new(acc), Box::new(gated)),
                        width,
                        elab,
                        origin_lsb: 0,
                        synthetic: Some(SyntheticOrigin::BranchJoin),
                    }
                })
        } else {
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
                        origin_lsb: 0,
                        synthetic: Some(SyntheticOrigin::BranchJoin),
                    }
                })
        };
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
                self.unsupported(
                    always.clock.span,
                    "unsupported: `always_ff` clock must be one top-level input identifier",
                );
                None
            }
        }?;
        if self.signals[clock].direction != Some(PortDirection::Input) {
            self.unsupported(
                always.clock.span,
                format!(
                    "unsupported: `always_ff` clock `{}` must be a top-level input port",
                    self.signals[clock].name
                ),
            );
            return None;
        }
        if self.signals[clock].width != 1 {
            self.unsupported(
                always.clock.span,
                format!(
                    "unsupported: `always_ff` clock `{}` must be one bit wide",
                    self.signals[clock].name
                ),
            );
            return None;
        }

        let [statement] = always.body.as_slice() else {
            self.unsupported(
                always.span,
                "unsupported: `always_ff` must contain exactly one assignment or conditional tree; multiple writes are not supported",
            );
            return None;
        };
        let mut first = statement;
        while let Stmt::If(branch) = first {
            first = &branch.when_true;
        }
        let Stmt::NonblockingAssign(assign) = first else {
            unreachable!("the parser accepts only clocked assignment trees")
        };
        let target = self.target(&assign.lhs)?;
        let value = self.clocked_value(statement, target)?;
        let (signal, lsb, width) = target;
        let elab = self.node(always.id, ElabKind::Dff, width);
        Some(ElabAssign {
            elab,
            span: always.span,
            signal,
            lsb,
            width,
            value,
            clock: Some(Clock { signal: clock, edge: always.edge }),
        })
    }

    /// A conditional tree chooses one next-state value for one register
    /// slice. Missing branches read that register's old value. The logic
    /// graph reserves its DFF identity before resolving this feedback.
    fn clocked_value(&mut self, statement: &Stmt, target: (usize, u32, u32)) -> Option<RtlExpr> {
        let (signal, lsb, width) = target;
        match statement {
            Stmt::NonblockingAssign(assign) => {
                if self.target(&assign.lhs)? != target {
                    self.fail(
                        assign.lhs.span,
                        "all branches of a clocked conditional must write the same register slice",
                    );
                    return None;
                }
                let value = self.expr(&assign.rhs)?;
                if value.width != width {
                    self.fail(assign.span, format!(
                        "assignment width mismatch: the DFF target is {width} bit(s) and the value is {} bit(s); version 1 never extends or truncates", value.width));
                    return None;
                }
                Some(value)
            }
            Stmt::If(branch) => {
                let when_true = self.clocked_value(&branch.when_true, target)?;
                let condition = self.expr(&branch.cond)?;
                let condition = self.reduce_to_truth(condition);
                let elab = self.node(branch.id, ElabKind::Expr, width);
                let when_false = match &branch.when_false {
                    Some(statement) => self.clocked_value(statement, target)?,
                    None => RtlExpr {
                        kind: RtlKind::Signal { signal, lsb },
                        width,
                        elab,
                        origin_lsb: 0,
                        synthetic: Some(SyntheticOrigin::HoldMux),
                    },
                };
                Some(RtlExpr {
                    kind: RtlKind::Mux {
                        select: Box::new(condition),
                        when_true: Box::new(when_true),
                        when_false: Box::new(when_false),
                    },
                    width,
                    elab,
                    origin_lsb: 0,
                    synthetic: Some(if branch.when_false.is_some() {
                        SyntheticOrigin::BranchJoin
                    } else {
                        SyntheticOrigin::HoldMux
                    }),
                })
            }
            _ => unreachable!("the parser accepts only clocked assignment trees"),
        }
    }

    fn constant_index(&mut self, expr: &ConstExpr) -> Option<u64> {
        match constant_value(
            expr,
            &HashMap::new(),
            &mut self.parameters,
            &mut HashSet::new(),
        ) {
            Ok(value) if value.number() >= 0 && value.number() <= u64::MAX as i128 => Some(value.number() as u64),
            Ok(_) => {
                self.fail(expr.span, "negative selection index is out of range");
                None
            }
            Err(error) => {
                self.errors.push(error);
                None
            }
        }
    }

    /// Resolve an assignment target to `(signal, lsb, width)`.
    fn target(&mut self, expr: &Expr) -> Option<(usize, u32, u32)> {
        let (base, lsb, width) = self.net_target(expr)?;
        if self.signals[base].direction == Some(PortDirection::Input) {
            self.fail(expr.span, format!("`{}` is an input port and cannot be assigned", self.signals[base].name));
            return None;
        }
        Some((base, lsb, width))
    }

    /// Alias connections may reference primary inputs, but never constants or expressions.
    fn net_targets(&mut self, expr: &Expr) -> Option<Vec<(usize, u32, u32)>> {
        if let ExprKind::Concat(parts) = &expr.kind {
            let mut targets = Vec::new();
            for part in parts { targets.extend(self.net_targets(part)?); }
            Some(targets)
        } else { Some(vec![self.net_target(expr)?]) }
    }

    fn net_target(&mut self, expr: &Expr) -> Option<(usize, u32, u32)> {
        if let ExprKind::Ident(name) = &expr.kind {
            if self.parameters.contains_key(name) {
                self.fail(expr.span, format!("parameter `{name}` cannot be assigned"));
                return None;
            }
        }
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
                let index = self.constant_index(index)?;
                let bit = self.check_index(signal, index, expr.span)?;
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
                let msb = self.constant_index(msb)?;
                let lsb = self.constant_index(lsb)?;
                let (lsb, width) = self.check_range(signal, msb, lsb, expr.span)?;
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
                if let Some(value) = self.parameters.get(name).copied() {
                    let elab = self.node(expr.id, ElabKind::Expr, value.width);
                    return Some(RtlExpr::new(RtlKind::Const(value.bits()), value.width, elab));
                }
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
                let index = self.constant_index(index)?;
                if let Some(value) = self.parameters.get(name).copied() {
                    if index >= value.width as u64 {
                        self.fail(expr.span, "parameter selection is out of range");
                        return None;
                    }
                    let elab = self.node(expr.id, ElabKind::Expr, 1);
                    return Some(RtlExpr::new(
                        RtlKind::Const(vec![value.raw & (1u64 << index) != 0]),
                        1,
                        elab,
                    ));
                }
                let signal = self.lookup(name, base.span)?;
                let bit = self.check_index(signal, index, expr.span)?;
                let elab = self.node(expr.id, ElabKind::Expr, 1);
                Some(RtlExpr::new(RtlKind::Signal { signal, lsb: bit }, 1, elab))
            }
            ExprKind::PartSelect(base, msb, lsb) => {
                let ExprKind::Ident(name) = &base.kind else {
                    self.fail(expr.span, "only a signal can be part-selected in version 1");
                    return None;
                };
                let msb = self.constant_index(msb)?;
                let lsb = self.constant_index(lsb)?;
                if let Some(value) = self.parameters.get(name).copied() {
                    if msb < lsb || msb >= value.width as u64 {
                        self.fail(expr.span, "parameter selection is out of range");
                        return None;
                    }
                    let width = (msb - lsb + 1) as u32;
                    let elab = self.node(expr.id, ElabKind::Expr, width);
                    let bits = (lsb..=msb)
                        .map(|bit| value.raw & (1u64 << bit) != 0)
                        .collect();
                    return Some(RtlExpr::new(RtlKind::Const(bits), width, elab));
                }
                let signal = self.lookup(name, base.span)?;
                let (lsb, width) = self.check_range(signal, msb, lsb, expr.span)?;
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
            ExprKind::Unary(op @ (UnaryOp::Positive | UnaryOp::Negate), operand) => {
                let operand = self.expr(operand)?;
                let width = operand.width;
                let elab = self.node(expr.id, ElabKind::Expr, width);
                let kind = if *op == UnaryOp::Positive {
                    RtlKind::Slice(Box::new(operand), 0)
                } else {
                    let zero = RtlExpr::new(RtlKind::Const(vec![false; width as usize]), width, elab);
                    RtlKind::Binary(RtlBinaryOp::Subtract, Box::new(zero), Box::new(operand))
                };
                Some(RtlExpr::new(kind, width, elab))
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

        let word_operation = |op| match op {
            BinaryOp::Add => Some(RtlBinaryOp::Add),
            BinaryOp::Subtract => Some(RtlBinaryOp::Subtract),
            BinaryOp::And => Some(RtlBinaryOp::And),
            BinaryOp::Or => Some(RtlBinaryOp::Or),
            BinaryOp::Xor => Some(RtlBinaryOp::Xor),
            _ => None,
        };

        match op {
            BinaryOp::And | BinaryOp::Or | BinaryOp::Xor | BinaryOp::Add | BinaryOp::Subtract => {
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
                        word_operation(op).expect("checked above"),
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
