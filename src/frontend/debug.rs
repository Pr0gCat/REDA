//! The provenance graph: what every compiler entity came from, and the
//! bidirectional queries that answer "which gates is this expression?" and
//! "which source is this gate?".
//!
//! Source-to-cell debugging is a compiler *output*, not something to
//! reconstruct from the finished [`Netlist`]. Transformations here are
//! many-to-many: one expression splits into many gates, several expressions
//! share one interned gate, and a folded expression produces none at all.
//! So each persisted entity embeds exactly one origin pointing at its parent
//! layer, and only the genuinely many-to-many events -- interning hits,
//! folds, dead logic, and gate mapping that no single realisation records --
//! spend a row in a side table.
//!
//! The database is a sidecar. Nothing here is a field of [`Netlist`]:
//! `Netlist` is the frozen boundary the rest of REDA consumes, and the join
//! back to it is [`GateRef`] plus the canonical [`Fingerprint`] both this
//! sidecar and the future `PhysicalDebugArtifact` carry. A mismatched
//! fingerprint means the two halves describe different netlists and must be
//! rejected rather than joined.
//!
//! Everything is a `Vec`, and every query is a scan. That is deliberate: the
//! tables are small, the serialization is then deterministic by
//! construction (the M0 exit criterion asks for byte-identical repeat
//! rendering), and a map keyed by a hashed ID would add an ordering question
//! nothing yet needs to answer.

use serde::{Deserialize, Serialize};

use crate::compile::metrics::{canonical_fingerprint, Fingerprint};
use crate::compile::Netlist;

pub use super::ast::{SourceKind, SourceNode, SourceNodeId};
pub use super::elaborate::{ElabKind, ElabNode, ElabNodeId, InstancePathId};
pub use super::logic::LogicNodeId;
pub use super::source::{FileId, SourceFileInfo, Span};

/// The compiler-generator bridge: a gate's index in [`Netlist::gates`] plus
/// the signal it drives. A name alone is not enough -- the index is what
/// primitive generation, placement, and routes already key on, and the name
/// is what a human reads and what the baked text format carries.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct GateRef {
    pub index: u32,
    pub output: String,
}

/// Logic the compiler created that no expression spelled. Version 1's
/// continuous assignments have no control flow, so it produces none; the
/// variants are the ones `always_comb`/`always_ff` lowering will need, and
/// they exist now so those passes record a reason instead of borrowing an
/// unrelated source span.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SyntheticOrigin {
    HoldMux,
    BranchJoin,
    CaseFold,
    Passthrough,
}

/// Where one logic node came from: an elaborated node, which bit of it, and
/// whether the compiler invented it.
///
/// Bit expansion is represented right here by `bit` rather than by a
/// transformation row: a 7-bit signal becoming 7 nodes is the common case,
/// and paying a row for each would make the side tables larger than the
/// graph they annotate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogicOrigin {
    pub elab: ElabNodeId,
    pub bit: u32,
    pub synthetic: Option<SyntheticOrigin>,
}

/// What one bit of an elaborated signal actually is after compilation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BitBinding {
    Logic(LogicNodeId),
    Const(bool),
    Input(String),
}

/// Every bit of one elaborated signal, LSB-first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignalBinding {
    pub elab: ElabNodeId,
    pub bits: Vec<BitBinding>,
}

/// What became of one logic node. Indexed by [`LogicNodeId`], so every node
/// in the arena has exactly one -- including the dead ones the emitter
/// skipped, which stay queryable precisely because they are still here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Realisation {
    Gate(GateRef),
    Input(String),
    Dead,
    /// This node was replaced by another one; the payload is the survivor.
    Folded(LogicNodeId),
    Const(bool),
}

/// One endpoint of a [`Transformation`], in whichever layer it lives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DebugNode {
    Source(SourceNodeId),
    Elab(ElabNodeId),
    Logic(LogicNodeId),
    Gate(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransformReason {
    Fold,
    Cse,
    Dead,
    Map,
}

/// A many-to-many event: inputs became outputs for `reason`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transformation {
    pub reason: TransformReason,
    pub inputs: Vec<DebugNode>,
    pub outputs: Vec<DebugNode>,
}

/// One gate origin, resolved across every layer -- what
/// [`DebugDatabase::origins_for_gate`] answers with.
///
/// `extra` marks an origin that arrived through structural interning: the
/// node was built once for `source`, and a later identical expression was
/// given the same node instead of a duplicate. Both are real origins of the
/// gate; only the first one is the node's embedded parent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateOrigin {
    pub logic: LogicNodeId,
    pub elab: ElabNodeId,
    pub source: SourceNodeId,
    pub span: Span,
    pub bit: u32,
    pub synthetic: Option<SyntheticOrigin>,
    pub extra: bool,
}

/// Why a source node produced no gate of its own -- what
/// [`DebugDatabase::why_missing`] answers with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WhyMissing {
    pub reason: TransformReason,
    /// What survives instead, if anything: the node it was interned into,
    /// the constant it folded to, or nothing at all when it is dead.
    pub surviving: Vec<Realisation>,
}

/// The compact provenance sidecar for one [`CompileArtifact`](super::CompileArtifact).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DebugDatabase {
    pub files: Vec<SourceFileInfo>,
    pub source_nodes: Vec<SourceNode>,
    pub elab_nodes: Vec<ElabNode>,
    /// Every emitted gate, indexed by its position in `Netlist::gates`.
    ///
    /// The sidecar does not contain the netlist, so without this a
    /// `DebugNode::Gate(index)` could not be resolved to the `GateRef` the
    /// generator bridge is defined in terms of -- which is exactly the join
    /// a passthrough buffer needs, since the node it buffers is a primary
    /// input and already has a realisation of its own.
    pub gates: Vec<GateRef>,
    /// Indexed by [`LogicNodeId`]: every node's single embedded parent.
    pub logic_origins: Vec<LogicOrigin>,
    /// Origins that arrived after the fact, through structural interning.
    pub extra_origins: Vec<(LogicNodeId, LogicOrigin)>,
    /// Indexed by [`LogicNodeId`].
    pub realisations: Vec<Realisation>,
    pub transformations: Vec<Transformation>,
    pub signals: Vec<SignalBinding>,
    pub netlist_fingerprint: Fingerprint,
}

/// An empty database -- one that describes the empty netlist, and says so
/// with the same fingerprint an actually-empty compile would produce.
impl Default for DebugDatabase {
    fn default() -> Self {
        DebugDatabase {
            files: Vec::new(),
            source_nodes: Vec::new(),
            elab_nodes: Vec::new(),
            gates: Vec::new(),
            logic_origins: Vec::new(),
            extra_origins: Vec::new(),
            realisations: Vec::new(),
            transformations: Vec::new(),
            signals: Vec::new(),
            netlist_fingerprint: netlist_fingerprint(&Netlist {
                inputs: Vec::new(),
                outputs: Vec::new(),
                gates: Vec::new(),
            }),
        }
    }
}

impl DebugDatabase {
    /// Whether this sidecar describes `netlist`. The join to any other
    /// artifact -- the future `PhysicalDebugArtifact` above all -- must ask
    /// this first: a `GateRef` index against the wrong netlist is not a
    /// missing answer, it is a confidently wrong one.
    pub fn matches(&self, netlist: &Netlist) -> bool {
        self.netlist_fingerprint == netlist_fingerprint(netlist)
    }

    /// This database as JSON.
    ///
    /// Deterministic by construction: every table is a `Vec` in a fixed
    /// order, so the same compile renders the same bytes -- which is what
    /// lets a host cache, diff, or transport a sidecar and lets a test
    /// check that a compile is reproducible at all.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("a debug database contains no unserialisable value")
    }

    /// The inverse of [`DebugDatabase::to_json`].
    pub fn from_json(text: &str) -> Result<DebugDatabase, serde_json::Error> {
        serde_json::from_str(text)
    }

    pub fn source_node(&self, id: SourceNodeId) -> Option<&SourceNode> {
        self.source_nodes.get(id.0 as usize)
    }

    pub fn elab_node(&self, id: ElabNodeId) -> Option<&ElabNode> {
        self.elab_nodes.get(id.0 as usize)
    }

    /// The span of the syntax `elab` was elaborated from.
    pub fn span_of_elab(&self, elab: ElabNodeId) -> Option<Span> {
        let node = self.elab_node(elab)?;
        Some(self.source_node(node.source)?.span)
    }

    // ---- forward: source -> gates -------------------------------------

    /// Every gate any syntax **inside** `span` produced, deduplicated and
    /// ordered by gate index.
    ///
    /// Containment rather than equality is what makes this useful at two
    /// scales at once: the span of a whole `assign` finds the gates of its
    /// entire right-hand side, and the span of one nested subexpression
    /// finds only that subexpression's own gates.
    pub fn gates_for_span(&self, span: Span) -> Vec<GateRef> {
        let mut gates = Vec::new();
        for (index, node) in self.source_nodes.iter().enumerate() {
            if span.contains(node.span) {
                self.collect_gates_of_source(SourceNodeId(index as u32), &mut gates);
            }
        }
        sort_dedup_gates(&mut gates);
        gates
    }

    /// Every gate one elaborated signal's bit is realised by. A signal bit
    /// usually has exactly one; it has none when the bit is a primary input
    /// or a folded constant.
    pub fn gates_for_signal_bit(&self, name: &str, bit: u32) -> Vec<GateRef> {
        let mut gates = Vec::new();
        if let Some(binding) = self.signal_bits(name) {
            if let Some(BitBinding::Logic(node)) = binding.bits.get(bit as usize) {
                self.collect_gates_of_logic(*node, &mut gates);
            }
        }
        sort_dedup_gates(&mut gates);
        gates
    }

    // ---- reverse: gate -> source --------------------------------------

    /// Every direct and extra origin of one gate, ordered by logic node and
    /// then by arrival. A gate several source expressions share reports all
    /// of them; that is the interning hit made visible rather than lost.
    pub fn origins_for_gate(&self, gate: &GateRef) -> Vec<GateOrigin> {
        let mut origins = Vec::new();
        for node in self.logic_nodes_of_gate(gate) {
            let index = node.0 as usize;
            if let Some(origin) = self.logic_origins.get(index) {
                if let Some(resolved) = self.resolve_origin(node, origin, false) {
                    origins.push(resolved);
                }
            }
            for (owner, origin) in &self.extra_origins {
                if *owner == node {
                    if let Some(resolved) = self.resolve_origin(node, origin, true) {
                        origins.push(resolved);
                    }
                }
            }
        }
        origins
    }

    /// The bits of the signal named `name` -- `"y"`, or the declared name of
    /// a vector, whose bits are listed LSB-first. Distinguishes a bit driven
    /// by logic from a folded constant and from a primary input.
    pub fn signal_bits(&self, name: &str) -> Option<&SignalBinding> {
        self.signals.iter().find(|binding| {
            self.elab_node(binding.elab)
                .and_then(|node| self.source_node(node.source))
                .and_then(|source| source.name.as_deref())
                == Some(name)
        })
    }

    /// Why `source` has no gate of its own: it was interned into another
    /// node, folded to a constant, or left dead. `None` means the question
    /// does not apply -- the node did produce gates.
    pub fn why_missing(&self, source: SourceNodeId) -> Option<WhyMissing> {
        let mut gates = Vec::new();
        self.collect_gates_of_source(source, &mut gates);
        if !gates.is_empty() {
            return None;
        }

        let nodes: Vec<LogicNodeId> = self.logic_nodes_of_source(source);
        if nodes.is_empty() {
            // No logic node was ever built for it: every operand folded
            // away before this expression could become a node.
            let surviving = self.folded_survivors(source);
            return Some(WhyMissing {
                reason: TransformReason::Fold,
                surviving,
            });
        }

        let mut surviving = Vec::new();
        let mut reason = TransformReason::Dead;
        for node in nodes {
            match self.realisations.get(node.0 as usize) {
                Some(Realisation::Dead) => surviving.push(Realisation::Dead),
                Some(Realisation::Const(value)) => {
                    reason = TransformReason::Fold;
                    surviving.push(Realisation::Const(*value));
                }
                Some(Realisation::Folded(into)) => {
                    reason = TransformReason::Fold;
                    surviving.push(Realisation::Folded(*into));
                }
                Some(Realisation::Input(name)) => {
                    // Shared with a primary input: the expression is real,
                    // it simply needs no gate of its own.
                    reason = TransformReason::Cse;
                    surviving.push(Realisation::Input(name.clone()));
                }
                Some(Realisation::Gate(gate)) => surviving.push(Realisation::Gate(gate.clone())),
                None => {}
            }
        }
        Some(WhyMissing { reason, surviving })
    }

    // ---- internals ----------------------------------------------------

    fn resolve_origin(
        &self,
        logic: LogicNodeId,
        origin: &LogicOrigin,
        extra: bool,
    ) -> Option<GateOrigin> {
        let elab = self.elab_node(origin.elab)?;
        let span = self.source_node(elab.source)?.span;
        Some(GateOrigin {
            logic,
            elab: origin.elab,
            source: elab.source,
            span,
            bit: origin.bit,
            synthetic: origin.synthetic,
            extra,
        })
    }

    /// Every logic node that names `source` as a direct or extra origin.
    fn logic_nodes_of_source(&self, source: SourceNodeId) -> Vec<LogicNodeId> {
        let owns = |origin: &LogicOrigin| {
            self.elab_node(origin.elab)
                .map(|elab| elab.source == source)
                .unwrap_or(false)
        };
        let mut nodes: Vec<LogicNodeId> = self
            .logic_origins
            .iter()
            .enumerate()
            .filter(|(_, origin)| owns(origin))
            .map(|(index, _)| LogicNodeId(index as u32))
            .collect();
        for (node, origin) in &self.extra_origins {
            if owns(origin) {
                nodes.push(*node);
            }
        }
        nodes.sort_unstable();
        nodes.dedup();
        nodes
    }

    fn logic_nodes_of_gate(&self, gate: &GateRef) -> Vec<LogicNodeId> {
        let mut nodes: Vec<LogicNodeId> = self
            .realisations
            .iter()
            .enumerate()
            .filter(
                |(_, realisation)| matches!(realisation, Realisation::Gate(this) if this == gate),
            )
            .map(|(index, _)| LogicNodeId(index as u32))
            .collect();
        // A gate that no single node's realisation claims -- the buffer an
        // output passthrough needs -- is recorded as a Map transformation
        // instead, because the node it buffers already has a realisation of
        // its own.
        for transformation in &self.transformations {
            if transformation.reason != TransformReason::Map
                || !transformation
                    .outputs
                    .contains(&DebugNode::Gate(gate.index))
            {
                continue;
            }
            for input in &transformation.inputs {
                if let DebugNode::Logic(node) = input {
                    nodes.push(*node);
                }
            }
        }
        nodes.sort_unstable();
        nodes.dedup();
        nodes
    }

    fn collect_gates_of_source(&self, source: SourceNodeId, gates: &mut Vec<GateRef>) {
        for node in self.logic_nodes_of_source(source) {
            self.collect_gates_of_logic(node, gates);
        }
    }

    fn collect_gates_of_logic(&self, node: LogicNodeId, gates: &mut Vec<GateRef>) {
        if let Some(Realisation::Gate(gate)) = self.realisations.get(node.0 as usize) {
            gates.push(gate.clone());
        }
        for transformation in &self.transformations {
            if transformation.reason != TransformReason::Map
                || !transformation.inputs.contains(&DebugNode::Logic(node))
            {
                continue;
            }
            for output in &transformation.outputs {
                if let DebugNode::Gate(index) = output {
                    if let Some(gate) = self.gate_ref_of_index(*index) {
                        gates.push(gate);
                    }
                }
            }
        }
    }

    /// The `GateRef` of an index mentioned by a transformation.
    fn gate_ref_of_index(&self, index: u32) -> Option<GateRef> {
        self.gates.get(index as usize).cloned()
    }

    /// The survivors of the folds that consumed a source node which never
    /// reached the graph at all.
    fn folded_survivors(&self, source: SourceNodeId) -> Vec<Realisation> {
        let mut surviving = Vec::new();
        for transformation in &self.transformations {
            if transformation.reason != TransformReason::Fold {
                continue;
            }
            let mentions = transformation.inputs.contains(&DebugNode::Source(source));
            if !mentions {
                continue;
            }
            for output in &transformation.outputs {
                if let DebugNode::Logic(node) = output {
                    if let Some(realisation) = self.realisations.get(node.0 as usize) {
                        surviving.push(realisation.clone());
                    }
                }
            }
        }
        surviving
    }
}

fn sort_dedup_gates(gates: &mut Vec<GateRef>) {
    gates.sort_unstable_by(|a, b| a.index.cmp(&b.index).then_with(|| a.output.cmp(&b.output)));
    gates.dedup();
}

/// A canonical, line-oriented rendering of `netlist` -- the exact bytes the
/// fingerprint is taken over.
///
/// It records only what makes a netlist *that* netlist: the inputs in
/// declaration order, each gate's kind, arity, output, and input list in
/// gate-index order, and the outputs. Nothing about provenance, spans, or
/// options appears, so the same circuit compiled from differently formatted
/// (or differently commented) source fingerprints identically -- while any
/// change a `GateRef` index could point at changes it.
pub fn canonical_netlist_render(netlist: &Netlist) -> String {
    let mut out = String::new();
    out.push_str("netlist 1\n");
    for name in &netlist.inputs {
        out.push_str("input ");
        out.push_str(name);
        out.push('\n');
    }
    for gate in &netlist.gates {
        out.push_str("gate ");
        out.push_str(gate.kind.wire_name());
        out.push_str(&format!(" {} {} <-", gate.kind.arity(), gate.output));
        for input in &gate.inputs {
            out.push(' ');
            out.push_str(input);
        }
        out.push('\n');
    }
    for name in &netlist.outputs {
        out.push_str("output ");
        out.push_str(name);
        out.push('\n');
    }
    out
}

/// The canonical fingerprint of `netlist`: SHA-256 over
/// [`canonical_netlist_render`].
pub fn netlist_fingerprint(netlist: &Netlist) -> Fingerprint {
    canonical_fingerprint(canonical_netlist_render(netlist).as_bytes())
}
