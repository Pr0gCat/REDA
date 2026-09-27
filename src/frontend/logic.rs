//! The canonical bit-level logic graph, and the bit-blasting that fills it.
//!
//! Nodes are interned: building the same operation over the same operands
//! twice returns the first node, and the second builder's origin is appended
//! to `extra_origins` instead of being lost. Origins never take part in the
//! hashing key, which is the whole point -- identical logic written in two
//! places should share one gate *and* keep both source spans.
//!
//! Each constructor also performs the cheap local identities the plan asks
//! for (constant folding, `x & x`, double negation, commutative operand
//! ordering, degenerate muxes) and records what it did. This is not an ABC
//! replacement and is not trying to be one: structural hashing plus local
//! identities is what establishes the cross-platform compiler, and measured
//! physical cost -- not guesswork -- is what will justify any rewrite beyond
//! it.
//!
use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::debug::{DebugNode, LogicOrigin, SyntheticOrigin, TransformReason, Transformation};
use super::elaborate::{bit_name, Design, ElabNodeId};
use super::rtl::{RtlBinaryOp, RtlExpr, RtlKind};
use super::source::Span;
use super::{Diagnostic, Severity};

/// Identity of one node in the logic arena.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct LogicNodeId(pub u32);

/// One bit-level operation. `Mux` follows the same meaning
/// `GateKind::Mux` already has downstream: `select ? when_true : when_false`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum LogicNode {
    Input(String),
    Const(bool),
    Not(LogicNodeId),
    And(LogicNodeId, LogicNodeId),
    Or(LogicNodeId, LogicNodeId),
    Xor(LogicNodeId, LogicNodeId),
    Mux {
        select: LogicNodeId,
        when_false: LogicNodeId,
        when_true: LogicNodeId,
    },
    /// A stateful positive-edge DFF. `key` is the elaborated register identity
    /// and intentionally prevents CSE from merging two registers that happen
    /// to have the same D and clock expressions.
    Dff {
        data: LogicNodeId,
        clock: LogicNodeId,
        key: u32,
    },
}

/// The arena plus its provenance side tables.
#[derive(Debug, Clone, Default)]
pub struct LogicGraph {
    pub nodes: Vec<LogicNode>,
    /// One embedded parent per node, indexed by [`LogicNodeId`].
    pub origins: Vec<LogicOrigin>,
    pub extra_origins: Vec<(LogicNodeId, LogicOrigin)>,
    pub transformations: Vec<Transformation>,
    /// Reachability from the output roots, filled in by [`LogicGraph::mark_live`].
    pub live: Vec<bool>,
    intern: HashMap<LogicNode, LogicNodeId>,
}

impl LogicGraph {
    pub fn node(&self, id: LogicNodeId) -> &LogicNode {
        &self.nodes[id.0 as usize]
    }

    /// The constant value of `id`, if it is one.
    pub fn const_value(&self, id: LogicNodeId) -> Option<bool> {
        match self.node(id) {
            LogicNode::Const(value) => Some(*value),
            _ => None,
        }
    }

    /// Intern `node`, attributing it to `origin`.
    fn add(&mut self, node: LogicNode, origin: LogicOrigin) -> LogicNodeId {
        if let Some(&existing) = self.intern.get(&node) {
            // A structural hit: the node keeps its first origin and gains
            // this one, so a shared gate can name every expression that
            // asked for it.
            self.extra_origins.push((existing, origin));
            self.transformations.push(Transformation {
                reason: TransformReason::Cse,
                inputs: vec![DebugNode::Logic(existing)],
                outputs: vec![DebugNode::Logic(existing)],
            });
            return existing;
        }
        let id = LogicNodeId(self.nodes.len() as u32);
        self.intern.insert(node.clone(), id);
        self.nodes.push(node);
        self.origins.push(origin);
        id
    }

    /// Record that `inputs` folded away into `output`.
    fn fold(&mut self, inputs: &[LogicNodeId], output: LogicNodeId) -> LogicNodeId {
        self.transformations.push(Transformation {
            reason: TransformReason::Fold,
            inputs: inputs.iter().copied().map(DebugNode::Logic).collect(),
            outputs: vec![DebugNode::Logic(output)],
        });
        output
    }

    /// Note that `origin` also names an existing node. Reading a signal is
    /// the common case: `assign y = w;` builds no node, but the `w` in it is
    /// still an origin of whatever `w` already is, and a debugger that could
    /// not answer that would lose every identifier in the design.
    pub fn note_origin(&mut self, node: LogicNodeId, origin: LogicOrigin) {
        if self.origins[node.0 as usize] == origin {
            return;
        }
        if self
            .extra_origins
            .iter()
            .any(|(owner, existing)| *owner == node && *existing == origin)
        {
            return;
        }
        self.extra_origins.push((node, origin));
    }

    pub fn input(&mut self, name: String, origin: LogicOrigin) -> LogicNodeId {
        self.add(LogicNode::Input(name), origin)
    }

    pub fn constant(&mut self, value: bool, origin: LogicOrigin) -> LogicNodeId {
        self.add(LogicNode::Const(value), origin)
    }

    pub fn not(&mut self, a: LogicNodeId, origin: LogicOrigin) -> LogicNodeId {
        if let Some(value) = self.const_value(a) {
            let folded = self.constant(!value, origin);
            return self.fold(&[a], folded);
        }
        if let LogicNode::Not(inner) = *self.node(a) {
            return self.fold(&[a], inner);
        }
        self.add(LogicNode::Not(a), origin)
    }

    pub fn and(&mut self, a: LogicNodeId, b: LogicNodeId, origin: LogicOrigin) -> LogicNodeId {
        let (a, b) = order(a, b);
        if a == b {
            return self.fold(&[a, b], a);
        }
        match (self.const_value(a), self.const_value(b)) {
            (Some(false), _) | (_, Some(false)) => {
                let folded = self.constant(false, origin);
                return self.fold(&[a, b], folded);
            }
            (Some(true), _) => return self.fold(&[a, b], b),
            (_, Some(true)) => return self.fold(&[a, b], a),
            _ => {}
        }
        self.add(LogicNode::And(a, b), origin)
    }

    pub fn or(&mut self, a: LogicNodeId, b: LogicNodeId, origin: LogicOrigin) -> LogicNodeId {
        let (a, b) = order(a, b);
        if a == b {
            return self.fold(&[a, b], a);
        }
        match (self.const_value(a), self.const_value(b)) {
            (Some(true), _) | (_, Some(true)) => {
                let folded = self.constant(true, origin);
                return self.fold(&[a, b], folded);
            }
            (Some(false), _) => return self.fold(&[a, b], b),
            (_, Some(false)) => return self.fold(&[a, b], a),
            _ => {}
        }
        self.add(LogicNode::Or(a, b), origin)
    }

    pub fn xor(&mut self, a: LogicNodeId, b: LogicNodeId, origin: LogicOrigin) -> LogicNodeId {
        let (a, b) = order(a, b);
        if a == b {
            let folded = self.constant(false, origin.clone());
            return self.fold(&[a, b], folded);
        }
        match (self.const_value(a), self.const_value(b)) {
            (Some(x), Some(y)) => {
                let folded = self.constant(x ^ y, origin);
                return self.fold(&[a, b], folded);
            }
            (Some(false), _) => return self.fold(&[a, b], b),
            (_, Some(false)) => return self.fold(&[a, b], a),
            (Some(true), _) => {
                let inverted = self.not(b, origin);
                return self.fold(&[a, b], inverted);
            }
            (_, Some(true)) => {
                let inverted = self.not(a, origin);
                return self.fold(&[a, b], inverted);
            }
            _ => {}
        }
        self.add(LogicNode::Xor(a, b), origin)
    }

    pub fn mux(
        &mut self,
        select: LogicNodeId,
        when_false: LogicNodeId,
        when_true: LogicNodeId,
        origin: LogicOrigin,
    ) -> LogicNodeId {
        if when_false == when_true {
            return self.fold(&[select, when_false, when_true], when_false);
        }
        match self.const_value(select) {
            Some(true) => return self.fold(&[select, when_false, when_true], when_true),
            Some(false) => return self.fold(&[select, when_false, when_true], when_false),
            None => {}
        }
        // `Netlist` has no constant rail. Eliminate every mux arm constant
        // here, while the logic graph can still express and fold it, so the
        // emitter never has to invent a physical constant driver.
        match (self.const_value(when_false), self.const_value(when_true)) {
            (Some(false), Some(true)) => {
                return self.fold(&[select, when_false, when_true], select)
            }
            (Some(true), Some(false)) => {
                let inverted = self.not(select, origin);
                return self.fold(&[select, when_false, when_true], inverted);
            }
            (Some(false), None) => {
                let gated = self.and(select, when_true, origin);
                return self.fold(&[select, when_false, when_true], gated);
            }
            (Some(true), None) => {
                let not_select = self.not(select, origin.clone());
                let gated = self.or(not_select, when_true, origin);
                return self.fold(&[select, when_false, when_true], gated);
            }
            (None, Some(false)) => {
                let not_select = self.not(select, origin.clone());
                let gated = self.and(not_select, when_false, origin);
                return self.fold(&[select, when_false, when_true], gated);
            }
            (None, Some(true)) => {
                let gated = self.or(select, when_false, origin);
                return self.fold(&[select, when_false, when_true], gated);
            }
            (None, None) => {}
            // Equal constants were handled by `when_false == when_true`.
            (Some(_), Some(_)) => unreachable!("different constants handled above"),
        }
        self.add(
            LogicNode::Mux {
                select,
                when_false,
                when_true,
            },
            origin,
        )
    }

    /// Reserve a DFF's identity before its `data` operand exists. A hold
    /// mux's `en ? d : q` reads the register's own current value, so `q`
    /// must already resolve to *this* node while `d` is still being built --
    /// otherwise the read would look like the register depending on itself
    /// before it exists, which is exactly the combinational-loop shape
    /// [`super::logic`]'s bit-blaster rejects for every other signal.
    /// `data` is a harmless placeholder until [`LogicGraph::finalize_dff`]
    /// overwrites it; a DFF's `key` is unique per register, so unlike every
    /// other constructor this one is deliberately not interned; nothing
    /// else can ever build an identical node to collide with.
    pub fn reserve_dff(
        &mut self,
        clock: LogicNodeId,
        key: u32,
        origin: LogicOrigin,
    ) -> LogicNodeId {
        let id = LogicNodeId(self.nodes.len() as u32);
        self.nodes.push(LogicNode::Dff {
            data: clock,
            clock,
            key,
        });
        self.origins.push(origin);
        id
    }

    /// Fill in the `data` operand [`LogicGraph::reserve_dff`] deferred.
    pub fn finalize_dff(&mut self, id: LogicNodeId, data: LogicNodeId) {
        let index = id.0 as usize;
        let LogicNode::Dff { clock, key, .. } = self.nodes[index] else {
            unreachable!("finalize_dff called on a non-Dff node");
        };
        self.nodes[index] = LogicNode::Dff { data, clock, key };
    }

    /// Mark every node reachable from `roots`. Unreachable nodes stay in the
    /// arena -- a debugger still has to be able to explain them -- and are
    /// recorded as `Dead` so the emitter knows to skip them.
    pub fn mark_live(&mut self, roots: &[LogicNodeId]) {
        self.live = vec![false; self.nodes.len()];
        let mut stack: Vec<LogicNodeId> = roots.to_vec();
        while let Some(id) = stack.pop() {
            let index = id.0 as usize;
            if std::mem::replace(&mut self.live[index], true) {
                continue;
            }
            match self.nodes[index].clone() {
                LogicNode::Input(_) | LogicNode::Const(_) => {}
                LogicNode::Not(a) => stack.push(a),
                LogicNode::And(a, b) | LogicNode::Or(a, b) | LogicNode::Xor(a, b) => {
                    stack.push(a);
                    stack.push(b);
                }
                LogicNode::Mux {
                    select,
                    when_false,
                    when_true,
                } => {
                    stack.push(select);
                    stack.push(when_false);
                    stack.push(when_true);
                }
                LogicNode::Dff { data, clock, .. } => {
                    stack.push(data);
                    stack.push(clock);
                }
            }
        }
        for (index, live) in self.live.iter().enumerate() {
            if !live {
                self.transformations.push(Transformation {
                    reason: TransformReason::Dead,
                    inputs: vec![DebugNode::Logic(LogicNodeId(index as u32))],
                    outputs: Vec::new(),
                });
            }
        }
    }
}

/// Commutative operands are ordered by ID, so `a & b` and `b & a` intern to
/// one node.
fn order(a: LogicNodeId, b: LogicNodeId) -> (LogicNodeId, LogicNodeId) {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

/// Every bit of every declared signal, LSB-first, once resolved.
#[derive(Debug, Clone)]
pub struct Blasted {
    pub graph: LogicGraph,
    /// Indexed the same way as `Design::signals`.
    pub signal_bits: Vec<Vec<LogicNodeId>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BitState {
    Undriven,
    InProgress,
    Ready(LogicNodeId),
}

/// Bit-blast every assignment of `design` into one logic graph.
///
/// Signals resolve on demand rather than in source order, so an assignment
/// may read a net that a later statement drives; a signal that reaches
/// itself is a combinational loop and is reported as one.
pub fn build(design: &Design) -> Result<Blasted, Vec<Diagnostic>> {
    let mut builder = Builder {
        design,
        graph: LogicGraph::default(),
        bits: design
            .signals
            .iter()
            .map(|signal| vec![BitState::Undriven; signal.width as usize])
            .collect(),
        drivers: HashMap::new(),
        errors: Vec::new(),
    };

    // Primary inputs are the graph's leaves, in declaration order.
    for (index, signal) in design.inputs() {
        for bit in 0..signal.width {
            let name = bit_name(&signal.name, signal.width, bit);
            let origin = LogicOrigin {
                elab: signal.elab,
                bit,
                synthetic: None,
            };
            let node = builder.graph.input(name, origin);
            builder.bits[index][bit as usize] = BitState::Ready(node);
        }
    }

    // One driver per target bit; a second one is a dual-writer error.
    for (index, assign) in design.assigns.iter().enumerate() {
        for bit in assign.lsb..assign.lsb + assign.width {
            let key = (assign.signal, bit);
            if let Some(&(previous, _)) = builder.drivers.get(&key) {
                let earlier: &super::elaborate::ElabAssign = &design.assigns[previous];
                builder.errors.push(Diagnostic {
                    severity: Severity::Error,
                    message: format!(
                        "`{}` is driven by more than one assignment (the first is at \
                         byte {})",
                        bit_name(
                            &design.signal(assign.signal).name,
                            design.signal(assign.signal).width,
                            bit
                        ),
                        earlier.span.start
                    ),
                    span: assign.span,
                });
                continue;
            }
            builder.drivers.insert(key, (index, bit - assign.lsb));
        }
    }

    for index in 0..design.signals.len() {
        for bit in 0..design.signals[index].width {
            builder.resolve(index, bit);
        }
    }

    if !builder.errors.is_empty() {
        let mut errors = builder.errors;
        errors.sort_by_key(|diagnostic| (diagnostic.span.file, diagnostic.span.start));
        return Err(errors);
    }

    let signal_bits: Vec<Vec<LogicNodeId>> = builder
        .bits
        .iter()
        .map(|bits| {
            bits.iter()
                .map(|state| match state {
                    BitState::Ready(node) => *node,
                    _ => unreachable!("every bit resolved or reported an error"),
                })
                .collect()
        })
        .collect();

    let mut graph = builder.graph;
    let roots: Vec<LogicNodeId> = design
        .outputs()
        .flat_map(|(index, _)| signal_bits[index].iter().copied())
        .collect();
    graph.mark_live(&roots);

    Ok(Blasted { graph, signal_bits })
}

struct Builder<'d> {
    design: &'d Design,
    graph: LogicGraph,
    bits: Vec<Vec<BitState>>,
    /// `(signal, bit) -> (assignment index, bit within that assignment)`.
    drivers: HashMap<(usize, u32), (usize, u32)>,
    errors: Vec<Diagnostic>,
}

impl Builder<'_> {
    fn fail(&mut self, span: Span, message: impl Into<String>) -> LogicNodeId {
        self.errors.push(Diagnostic {
            severity: Severity::Error,
            message: message.into(),
            span,
        });
        // Keep going with a placeholder so one undriven bit does not hide
        // the rest of the design's errors. The compile has already failed.
        self.graph.constant(
            false,
            LogicOrigin {
                elab: ElabNodeId(0),
                bit: 0,
                synthetic: Some(SyntheticOrigin::Passthrough),
            },
        )
    }

    fn resolve(&mut self, signal: usize, bit: u32) -> LogicNodeId {
        match self.bits[signal][bit as usize] {
            BitState::Ready(node) => return node,
            BitState::InProgress => {
                let info = self.design.signal(signal);
                let (span, name) = (info.span, info.name.clone());
                let node = self.fail(
                    span,
                    format!(
                        "`{}` depends on itself: a combinational loop cannot be built",
                        bit_name(&name, self.design.signal(signal).width, bit)
                    ),
                );
                self.bits[signal][bit as usize] = BitState::Ready(node);
                return node;
            }
            BitState::Undriven => {}
        }

        let Some(&(assign_index, value_bit)) = self.drivers.get(&(signal, bit)) else {
            let info = self.design.signal(signal);
            let (span, name, width) = (info.span, info.name.clone(), info.width);
            let node = self.fail(
                span,
                format!(
                    "`{}` has no driver; version 1 needs one continuous assignment for every bit",
                    bit_name(&name, width, bit)
                ),
            );
            self.bits[signal][bit as usize] = BitState::Ready(node);
            return node;
        };

        // `design` is a copy of the borrow, not a borrow of `self`, so the
        // expression stays readable while the graph is mutated.
        let design = self.design;
        let assignment = &design.assigns[assign_index];
        let node = if let Some(clock) = assignment.clock {
            // Reserve the register's own identity before blasting `d`: a
            // hold mux's `en ? d : q` reads this exact signal, and it must
            // already resolve to the DFF rather than trip the
            // self-dependency check below, which exists for every other
            // (combinational) signal.
            let clock_node = self.resolve(clock, 0);
            let dff = self.graph.reserve_dff(
                clock_node,
                assignment.elab.0,
                LogicOrigin {
                    elab: assignment.elab,
                    bit: value_bit,
                    synthetic: None,
                },
            );
            self.bits[signal][bit as usize] = BitState::Ready(dff);
            let data = self.blast_bit(&assignment.value, value_bit);
            self.graph.finalize_dff(dff, data);
            dff
        } else {
            self.bits[signal][bit as usize] = BitState::InProgress;
            self.blast_bit(&assignment.value, value_bit)
        };
        self.bits[signal][bit as usize] = BitState::Ready(node);
        node
    }

    fn origin(&self, expr: &RtlExpr, bit: u32) -> LogicOrigin {
        LogicOrigin {
            elab: expr.elab,
            bit,
            synthetic: expr.synthetic,
        }
    }

    /// One bit of an RTL expression. Bit blasting is on demand: only the
    /// bits an output actually needs are built.
    fn blast_bit(&mut self, expr: &RtlExpr, bit: u32) -> LogicNodeId {
        let origin = self.origin(expr, bit);
        match &expr.kind {
            RtlKind::Signal { signal, lsb } => {
                let node = self.resolve(*signal, lsb + bit);
                self.graph.note_origin(node, origin);
                node
            }
            RtlKind::Const(bits) => {
                let value = bits.get(bit as usize).copied().unwrap_or(false);
                self.graph.constant(value, origin)
            }
            RtlKind::Concat(parts) => {
                let mut offset = 0u32;
                for part in parts {
                    if bit < offset + part.width {
                        return self.blast_bit(part, bit - offset);
                    }
                    offset += part.width;
                }
                unreachable!("a concatenation covers every bit of its own width")
            }
            RtlKind::Not(operand) => {
                let inner = self.blast_bit(operand, bit);
                self.graph.not(inner, origin)
            }
            RtlKind::Binary(op, left, right) => {
                let a = self.blast_bit(left, bit);
                let b = self.blast_bit(right, bit);
                match op {
                    RtlBinaryOp::And => self.graph.and(a, b, origin),
                    RtlBinaryOp::Or => self.graph.or(a, b, origin),
                    RtlBinaryOp::Xor => self.graph.xor(a, b, origin),
                }
            }
            RtlKind::ReduceOr(operand) => {
                let mut accumulated = self.blast_bit(operand, 0);
                for index in 1..operand.width {
                    let next = self.blast_bit(operand, index);
                    accumulated = self.graph.or(accumulated, next, origin.clone());
                }
                accumulated
            }
            RtlKind::Equal(left, right) => {
                // Equality is the NOR of the operands' bitwise difference.
                let mut different: Option<LogicNodeId> = None;
                for index in 0..left.width {
                    let a = self.blast_bit(left, index);
                    let b = self.blast_bit(right, index);
                    let bit_differs = self.graph.xor(a, b, origin.clone());
                    different = Some(match different {
                        Some(previous) => self.graph.or(previous, bit_differs, origin.clone()),
                        None => bit_differs,
                    });
                }
                match different {
                    Some(different) => self.graph.not(different, origin),
                    None => self.graph.constant(true, origin),
                }
            }
            RtlKind::Mux {
                select,
                when_true,
                when_false,
            } => {
                let select = self.blast_bit(select, 0);
                let when_false = self.blast_bit(when_false, bit);
                let when_true = self.blast_bit(when_true, bit);
                self.graph.mux(select, when_false, when_true, origin)
            }
        }
    }
}
