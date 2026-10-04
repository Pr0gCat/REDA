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
//! identities establishes the cross-platform compiler. Small constant case
//! tables additionally use bounded implicant covers and native redstone cells;
//! physical quality is gated separately from logical equivalence.
//!
use std::collections::{BTreeMap, HashMap, HashSet};

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
    /// A redstone-native combinational cell used by bounded table mapping.
    Cell(crate::compile::topology::GateKind, Vec<LogicNodeId>),
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
    table_used: HashSet<LogicNodeId>,
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
    fn table_not(&mut self, node: LogicNodeId, origin: LogicOrigin) -> LogicNodeId {
        use crate::compile::topology::GateKind;
        match self.node(node).clone() {
            LogicNode::Cell(GateKind::Or(n), args) => {
                self.add(LogicNode::Cell(GateKind::Nor(n), args), origin)
            }
            LogicNode::Cell(GateKind::Nor(_), args) => self.table_or(args, origin),
            _ => self.not(node, origin),
        }
    }

    fn table_or(&mut self, args: Vec<LogicNodeId>, origin: LogicOrigin) -> LogicNodeId {
        use crate::compile::topology::GateKind;
        let mut flat = Vec::new();
        for arg in args {
            match self.node(arg) {
                LogicNode::Const(true) => return self.constant(true, origin),
                LogicNode::Const(false) => {}
                LogicNode::Cell(GateKind::Or(_), children) => flat.extend(children),
                _ => flat.push(arg),
            }
        }
        flat.sort();
        flat.dedup();
        if flat.is_empty() {
            return self.constant(false, origin);
        }
        while flat.len() > 3 {
            flat = flat
                .chunks(3)
                .map(|chunk| {
                    if chunk.len() == 1 {
                        chunk[0]
                    } else {
                        self.add(
                            LogicNode::Cell(GateKind::Or(chunk.len()), chunk.to_vec()),
                            origin.clone(),
                        )
                    }
                })
                .collect();
        }
        if flat.len() == 1 {
            flat[0]
        } else {
            self.add(LogicNode::Cell(GateKind::Or(flat.len()), flat), origin)
        }
    }

    fn table_and(&mut self, args: Vec<LogicNodeId>, origin: LogicOrigin) -> LogicNodeId {
        let inverted = args
            .into_iter()
            .map(|arg| self.table_not(arg, origin.clone()))
            .collect();
        let joined = self.table_or(inverted, origin.clone());
        self.table_not(joined, origin)
    }

    /// Map each implicant to native NOR/merge cells and share identical terms
    /// through the interner. Keep the cover shallow instead of introducing
    /// extra inversions by factoring through a binary AND/OR network.
    fn sum_of_products(
        &mut self,
        cubes: &[super::cover::Cube],
        inputs: &[LogicNodeId],
        origin: LogicOrigin,
    ) -> LogicNodeId {
        let mut terms = Vec::new();
        for cube in cubes {
            let literals = inputs
                .iter()
                .enumerate()
                .filter_map(|(bit, &input)| {
                    if cube.mask & (1 << bit) == 0 {
                        None
                    } else if cube.bits & (1 << bit) != 0 {
                        Some(input)
                    } else {
                        Some(self.table_not(input, origin.clone()))
                    }
                })
                .collect();
            terms.push(self.table_and(literals, origin.clone()));
        }
        self.table_or(terms, origin)
    }

    fn table_cone(&self, root: LogicNodeId) -> HashSet<LogicNodeId> {
        let mut seen = HashSet::new();
        let mut pending = vec![root];
        while let Some(id) = pending.pop() {
            if !seen.insert(id) {
                continue;
            }
            match self.node(id) {
                LogicNode::Cell(_, args) => pending.extend(args),
                LogicNode::Not(a) => pending.push(*a),
                LogicNode::Input(_) | LogicNode::Const(_) => {}
                _ => {} // selector logic is shared by both polarities
            }
        }
        seen
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
                LogicNode::Cell(_, operands) => stack.extend(operands),
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
        aliases: AliasClasses::new(design),
        arithmetic_carries: HashMap::new(),
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
            let (canonical, canonical_bit) = builder.aliases.canonical(index, bit);
            if matches!(builder.bits[canonical][canonical_bit as usize], BitState::Ready(_)) {
                builder.fail(signal.span, "aliased net is driven by more than one primary input");
            } else {
                builder.bits[canonical][canonical_bit as usize] = BitState::Ready(node);
            }
        }
    }

    // One driver per target bit; a second one is a dual-writer error.
    for (index, assign) in design.assigns.iter().enumerate() {
        for bit in assign.lsb..assign.lsb + assign.width {
            let key = builder.aliases.canonical(assign.signal, bit);
            if matches!(builder.bits[key.0][key.1 as usize], BitState::Ready(_)) {
                builder.fail(assign.span, "aliased net is driven by both a primary input and an assignment");
                continue;
            }
            if let Some(&(previous, _)) = builder.drivers.get(&key) {
                let earlier: &super::elaborate::ElabAssign = &design.assigns[previous];
                builder.errors.push(Diagnostic {
                    kind: super::DiagnosticKind::Source,
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

/// Deterministic connected components for undirected hierarchy net aliases.
/// The smallest flattened bit is the representative; path compression keeps
/// deeply nested aliases independent of hierarchy traversal depth.
struct AliasClasses {
    bits: Vec<Vec<(usize, u32)>>,
}
impl AliasClasses {
    fn new(design: &Design) -> Self {
        if design.aliases.is_empty() { return Self { bits: Vec::new() }; }
        let mut offsets = vec![0];
        let mut identities = Vec::new();
        for (signal, info) in design.signals.iter().enumerate() {
            identities.extend((0..info.width).map(|bit| (signal, bit)));
            offsets.push(identities.len());
        }
        let mut roots: Vec<_> = (0..identities.len()).collect();
        fn find(roots: &mut [usize], mut bit: usize) -> usize {
            while roots[bit] != bit {
                roots[bit] = roots[roots[bit]];
                bit = roots[bit];
            }
            bit
        }
        for alias in &design.aliases {
            let left = find(&mut roots, offsets[alias.left.0] + alias.left.1 as usize);
            let right = find(&mut roots, offsets[alias.right.0] + alias.right.1 as usize);
            roots[left.max(right)] = left.min(right);
        }
        let bits = design.signals.iter().enumerate().map(|(signal, info)| {
            (0..info.width).map(|bit| identities[find(&mut roots, offsets[signal] + bit as usize)]).collect()
        }).collect();
        Self { bits }
    }
    fn canonical(&self, signal: usize, bit: u32) -> (usize, u32) {
        if self.bits.is_empty() { (signal, bit) } else { self.bits[signal][bit as usize] }
    }
}

/// Resolve blocking values after all hierarchical net aliases are known.
/// The ordinary single-statement/wildcard path never enters this pass.
pub fn resolve_comb_blocks(design: &mut Design) -> Result<(), Vec<Diagnostic>> {
    if design.comb_blocks.is_empty() {
        return Ok(());
    }
    let aliases = AliasClasses::new(design);
    let mut errors = Vec::new();
    let mut resolved = Vec::new();
    for block in &design.comb_blocks {
        let written = block.assigns.iter().flat_map(|assign| {
            (assign.lsb..assign.lsb + assign.width)
                .map(|bit| aliases.canonical(assign.signal, bit))
        }).collect();
        let mut environment = BlockingValues {
            aliases: &aliases,
            written,
            values: HashMap::new(),
            reads: HashSet::new(),
            span: block.span,
        };
        let mut writers = BTreeMap::new();
        let mut index = 0;
        while index < block.assigns.len() {
            let source = design.nodes[block.assigns[index].elab.0 as usize].source;
            let mut end = index + 1;
            while end < block.assigns.len()
                && design.nodes[block.assigns[end].elab.0 as usize].source == source {
                end += 1;
            }
            // One source assignment may have several concatenated targets.
            // Evaluate all of their RHS slices before updating any target.
            let mut values = Vec::new();
            for assign in &block.assigns[index..end] {
                environment.span = assign.span;
                match environment.substitute(&assign.value) {
                    Ok(value) => values.push(value),
                    Err(error) => { errors.push(error); break; }
                }
            }
            if values.len() != end - index { break; }
            for (offset, value) in values.into_iter().enumerate() {
                let assign = &block.assigns[index + offset];
                for bit in 0..assign.width {
                    let key = aliases.canonical(assign.signal, assign.lsb + bit);
                    let mut value_bit = RtlExpr::new(
                        RtlKind::Slice(Box::new(value.clone()), bit), 1, assign.elab,
                    );
                    value_bit.origin_lsb = bit;
                    environment.values.insert(key, value_bit);
                    writers.insert(key, index + offset);
                }
            }
            index = end;
        }
        if let Some(sensitivity) = &block.sensitivity {
            let sensitive: HashSet<_> = sensitivity.iter()
                .map(|&(signal, bit)| aliases.canonical(signal, bit)).collect();
            if let Some(&(signal, bit)) = environment.reads.iter()
                .filter(|read| !sensitive.contains(read)).min() {
                let info = &design.signals[signal];
                errors.push(Diagnostic {
                    kind: super::DiagnosticKind::Unsupported,
                    severity: Severity::Error,
                    message: format!("incomplete combinational sensitivity list: `{}` is read without a corresponding net event", bit_name(&info.name, info.width, bit)),
                    span: block.span,
                });
            }
        }
        for ((signal, bit), index) in writers {
            let assign = &block.assigns[index];
            resolved.push(super::elaborate::ElabAssign {
                elab: assign.elab,
                span: assign.span,
                signal,
                lsb: bit,
                width: 1,
                value: environment.values.remove(&(signal, bit)).expect("last write owns its value"),
                clock: None,
            });
        }
    }
    if !errors.is_empty() {
        errors.sort_by_key(|error| (error.span.file, error.span.start));
        return Err(errors);
    }
    design.assigns.extend(resolved);
    design.comb_blocks.clear();
    Ok(())
}

/// Per-process procedural state. Values are expressions, not stored bits:
/// a read before this process writes a target would require real storage.
struct BlockingValues<'a> {
    aliases: &'a AliasClasses,
    written: HashSet<(usize, u32)>,
    values: HashMap<(usize, u32), RtlExpr>,
    reads: HashSet<(usize, u32)>,
    span: Span,
}

impl BlockingValues<'_> {
    fn substitute(&mut self, expr: &RtlExpr) -> Result<RtlExpr, Diagnostic> {
        let mut result = expr.clone();
        result.kind = match &expr.kind {
            RtlKind::Signal { signal, lsb } => {
                let mut bits = Vec::new();
                for bit in *lsb..*lsb + expr.width {
                    let key = self.aliases.canonical(*signal, bit);
                    let value = if let Some(value) = self.values.get(&key) {
                        value.clone()
                    } else {
                        if self.written.contains(&key) {
                            return Err(Diagnostic {
                                kind: super::DiagnosticKind::Unsupported,
                                severity: Severity::Error,
                                message: "read-before-write in ordered combinational block would infer storage".into(),
                                span: self.span,
                            });
                        }
                        self.reads.insert(key);
                        let mut value = RtlExpr::new(RtlKind::Signal { signal: *signal, lsb: bit }, 1, expr.elab);
                        value.origin_lsb = expr.origin_lsb + bit - *lsb;
                        value
                    };
                    bits.push(value);
                }
                RtlKind::Slice(Box::new(RtlExpr::new(RtlKind::Concat(bits), expr.width, expr.elab)), 0)
            }
            RtlKind::Const(bits) => RtlKind::Const(bits.clone()),
            RtlKind::Concat(parts) => RtlKind::Concat(parts.iter()
                .map(|part| self.substitute(part)).collect::<Result<_, _>>()?),
            RtlKind::Slice(value, bit) => RtlKind::Slice(Box::new(self.substitute(value)?), *bit),
            RtlKind::Not(value) => RtlKind::Not(Box::new(self.substitute(value)?)),
            RtlKind::ReduceOr(value) => RtlKind::ReduceOr(Box::new(self.substitute(value)?)),
            RtlKind::Binary(op, left, right) => RtlKind::Binary(*op,
                Box::new(self.substitute(left)?), Box::new(self.substitute(right)?)),
            RtlKind::Equal(left, right) => RtlKind::Equal(
                Box::new(self.substitute(left)?), Box::new(self.substitute(right)?)),
            RtlKind::Mux { select, when_true, when_false } => RtlKind::Mux {
                select: Box::new(self.substitute(select)?),
                when_true: Box::new(self.substitute(when_true)?),
                when_false: Box::new(self.substitute(when_false)?),
            },
            RtlKind::Table { selector, arms, default } => RtlKind::Table {
                selector: Box::new(self.substitute(selector)?),
                arms: arms.iter().map(|(index, condition, value)| Ok((
                    *index, self.substitute(condition)?, self.substitute(value)?,
                ))).collect::<Result<_, Diagnostic>>()?,
                default: Box::new(self.substitute(default)?),
            },
        };
        Ok(result)
    }
}

struct Builder<'d> {
    design: &'d Design,
    aliases: AliasClasses,
    graph: LogicGraph,
    bits: Vec<Vec<BitState>>,
    /// `(signal, bit) -> (assignment index, bit within that assignment)`.
    drivers: HashMap<(usize, u32), (usize, u32)>,
    /// Carry into a bit of one typed arithmetic expression. Shared slices
    /// and arbitrary bit requests reuse the same prefix without resolving
    /// future register bits before their DFF identity exists.
    arithmetic_carries: HashMap<(ElabNodeId, u32), LogicNodeId>,
    errors: Vec<Diagnostic>,
}

impl Builder<'_> {
    fn fail(&mut self, span: Span, message: impl Into<String>) -> LogicNodeId {
        self.errors.push(Diagnostic {
            kind: super::DiagnosticKind::Source,
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
        let (canonical, canonical_bit) = self.aliases.canonical(signal, bit);
        if (canonical, canonical_bit) != (signal, bit) {
            let node = self.resolve(canonical, canonical_bit);
            self.bits[signal][bit as usize] = BitState::Ready(node);
            self.graph.note_origin(node, LogicOrigin { elab: self.design.signal(signal).elab, bit, synthetic: None });
            return node;
        }
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
            let mut clock_node = self.resolve(clock.signal, 0);
            if clock.edge == super::ast::ClockEdge::Negedge {
                clock_node = self.graph.not(clock_node, LogicOrigin {
                    elab: assignment.elab,
                    bit: 0,
                    synthetic: Some(SyntheticOrigin::ClockInversion),
                });
            }
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

    /// Fixed-width unsigned addition, with subtraction as A + !B + 1.
    /// Only the required carry prefix is built; overflow is discarded by
    /// the typed result width, as in an equal-width HDL expression.
    fn arithmetic_bit(
        &mut self,
        expr: &RtlExpr,
        left: &RtlExpr,
        right: &RtlExpr,
        subtract: bool,
        bit: u32,
    ) -> LogicNodeId {
        let mut start = bit;
        while start > 0 && !self.arithmetic_carries.contains_key(&(expr.elab, start)) {
            start -= 1;
        }
        let mut carry = self
            .arithmetic_carries
            .get(&(expr.elab, start))
            .copied()
            .unwrap_or_else(|| self.graph.constant(subtract, self.origin(expr, 0)));
        for index in start..=bit {
            let origin = self.origin(expr, index);
            let a = self.blast_bit(left, index);
            let mut b = self.blast_bit(right, index);
            if subtract {
                b = self.graph.not(b, origin.clone());
            }
            let propagate = self.graph.xor(a, b, origin.clone());
            if index == bit {
                return self.graph.xor(propagate, carry, origin);
            }
            let generated = self.graph.and(a, b, origin.clone());
            let propagated = self.graph.and(propagate, carry, origin.clone());
            carry = self.graph.or(generated, propagated, origin);
            self.arithmetic_carries
                .insert((expr.elab, index + 1), carry);
        }
        unreachable!("a carry prefix ends at its requested sum bit")
    }

    fn origin(&self, expr: &RtlExpr, bit: u32) -> LogicOrigin {
        LogicOrigin {
            elab: expr.elab,
            bit: expr.origin_lsb + bit,
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
            RtlKind::Slice(value, lsb) => {
                let node = self.blast_bit(value, lsb + bit);
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
                if matches!(op, RtlBinaryOp::Add | RtlBinaryOp::Subtract) {
                    return self.arithmetic_bit(expr, left, right, *op == RtlBinaryOp::Subtract, bit);
                }
                let a = self.blast_bit(left, bit);
                let b = self.blast_bit(right, bit);
                match op {
                    RtlBinaryOp::And => self.graph.and(a, b, origin),
                    RtlBinaryOp::Or => self.graph.or(a, b, origin),
                    RtlBinaryOp::Xor => self.graph.xor(a, b, origin),
                    RtlBinaryOp::Add | RtlBinaryOp::Subtract => unreachable!("handled above"),
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
            RtlKind::Table {
                selector,
                arms,
                default,
            } => {
                let RtlKind::Const(default_bits) = &default.kind else {
                    unreachable!()
                };
                let mut table = vec![default_bits[bit as usize]; 1 << selector.width];
                for (index, _, value) in arms {
                    let RtlKind::Const(bits) = &value.kind else {
                        unreachable!()
                    };
                    table[*index] = bits[bit as usize];
                }
                let positive = super::cover::cover(&table, true);
                let negative = super::cover::cover(&table, false);
                let inputs: Vec<_> = (0..selector.width)
                    .map(|i| self.blast_bit(selector, i))
                    .collect();
                let positive = self
                    .graph
                    .sum_of_products(&positive, &inputs, origin.clone());
                let negative = self
                    .graph
                    .sum_of_products(&negative, &inputs, origin.clone());
                let negative = self.graph.table_not(negative, origin.clone());
                // Prefer a driven output to a passive wire merge: electrical
                // isolation matters more than a smaller Boolean node count.
                // Among equally driven forms, reuse already selected table logic.
                // Only membership/counts are read from HashSets, never their order.
                let candidates = [positive, negative];
                let result = *candidates
                    .iter()
                    .min_by_key(|&&root| {
                        let cells = self
                            .graph
                            .table_cone(root)
                            .iter()
                            .filter(|id| {
                                !self.graph.table_used.contains(id)
                                    && !matches!(
                                        self.graph.node(**id),
                                        LogicNode::Input(_) | LogicNode::Const(_)
                                    )
                            })
                            .count();
                        let passive_output = matches!(
                            self.graph.node(root),
                            LogicNode::Cell(crate::compile::topology::GateKind::Or(_), _)
                        );
                        (passive_output, cells)
                    })
                    .unwrap();
                self.graph.table_used.extend(self.graph.table_cone(result));
                // A minimized output represents several original case arms.
                // Keep each source value reachable through the debug sidecar.
                for (_, condition, value) in arms {
                    self.graph.note_origin(result, self.origin(condition, 0));
                    if let RtlKind::Equal(_, label) = &condition.kind {
                        for label_bit in 0..label.width {
                            self.graph
                                .note_origin(result, self.origin(label, label_bit));
                        }
                    }
                    self.graph.note_origin(result, self.origin(value, bit));
                }
                self.graph.note_origin(result, self.origin(default, bit));
                result
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
